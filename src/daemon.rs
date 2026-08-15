//! Single-server multi-client multi-session daemon.
//! Sessions survive client disconnect; optional restore from disk on boot.

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::agent::{self, AgentEvent, ApprovalDecision};
use crate::config::Config;
use crate::llm::ChatMessage;
use crate::modes::AppMode;
use crate::protocol::{self, ClientMsg, ServerMsg, SessionSummary};
use crate::session::{self, Session};

struct LiveSession {
    session: Session,
    history: Vec<ChatMessage>,
    mode: AppMode,
    busy: bool,
    cancel: Option<Arc<AtomicBool>>,
    approval_tx: Option<Sender<ApprovalDecision>>,
    subscribers: Vec<u64>,
    /// Override de token_less vindo do cliente (por sessão/aba).
    token_less: Option<crate::tokenless::TokenLessLevel>,
    /// Gauntlet Loop ligado neste chat (o cliente manda a cada mensagem).
    gauntlet: bool,
    /// Esforço de raciocínio deste chat.
    effort: Option<String>,
    /// Objetivo do chat.
    goal: String,
}

struct DaemonState {
    cfg: Config,
    sessions: HashMap<String, LiveSession>,
    next_client: u64,
    /// client_id → outbound queue
    clients: HashMap<u64, Sender<ServerMsg>>,
}

pub fn run_server(cfg: Config, tcp: bool) -> Result<()> {
    let max = cfg.max_sessions;
    let mut state = DaemonState {
        cfg,
        sessions: HashMap::new(),
        next_client: 1,
        clients: HashMap::new(),
    };
    restore_sessions_from_disk(&mut state);
    eprintln!(
        "harness daemon: restored {} session(s), max_sessions={max}",
        state.sessions.len()
    );
    let state = Arc::new(Mutex::new(state));

    if tcp || cfg!(windows) {
        let addr = crate::protocol::default_tcp_addr();
        let listener = TcpListener::bind(&addr).with_context(|| format!("bind {addr}"))?;
        eprintln!("harness daemon listening on tcp://{addr}");
        for stream in listener.incoming() {
            let stream = stream?;
            let st = Arc::clone(&state);
            thread::spawn(move || {
                if let Err(e) = handle_tcp_client(st, stream) {
                    eprintln!("client error: {e:#}");
                }
            });
        }
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::net::UnixListener;
            let path = crate::protocol::default_socket_path();
            let _ = std::fs::remove_file(&path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let listener =
                UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
            eprintln!("harness daemon listening on unix:{}", path.display());
            for stream in listener.incoming() {
                let stream = stream?;
                let st = Arc::clone(&state);
                thread::spawn(move || {
                    if let Err(e) = handle_unix_client(st, stream) {
                        eprintln!("client error: {e:#}");
                    }
                });
            }
        }
        #[cfg(not(unix))]
        {
            bail!("unix sockets unsupported; use TCP");
        }
    }
    Ok(())
}

fn restore_sessions_from_disk(state: &mut DaemonState) {
    let Ok(list) = session::list_sessions() else {
        return;
    };
    // Deixa folga: sessões restauradas são só cache quente, e as demais
    // reabrem sob demanda a partir do disco.
    let cap = (state.cfg.max_sessions * 3 / 4).max(1);
    for meta in list.into_iter().take(cap) {
        if state.sessions.contains_key(&meta.id) {
            continue;
        }
        let Ok(sess) = session::load_session(&meta.id) else {
            continue;
        };
        let history = sess.messages.clone();
        let mode = sess.meta.mode;
        state.sessions.insert(
            sess.meta.id.clone(),
            LiveSession {
                session: sess,
                history,
                mode,
                busy: false,
                cancel: None,
                approval_tx: None,
                subscribers: Vec::new(),
                token_less: None,
                gauntlet: false,
                effort: None,
                goal: String::new(),
            },
        );
    }
}

/// Libera espaço tirando da memória a sessão ociosa mais antiga. O JSON em
/// disco continua lá — ela reabre ao ser clicada. Sem isto um daemon que
/// restaurou `max_sessions` do disco no boot nunca aceita um chat novo.
fn evict_idle_session(g: &mut DaemonState) -> bool {
    let victim = g
        .sessions
        .values()
        .filter(|s| !s.busy && s.subscribers.is_empty())
        .min_by(|a, b| a.session.meta.updated_at.cmp(&b.session.meta.updated_at))
        .map(|s| s.session.meta.id.clone());
    match victim {
        Some(id) => {
            g.sessions.remove(&id);
            true
        }
        None => false,
    }
}

fn summary_of(s: &LiveSession) -> SessionSummary {
    let approx = crate::mem_stats::estimate_history_bytes(&s.history) as u64
        + crate::mem_stats::estimate_history_bytes(&s.session.messages) as u64
        + 2048;
    SessionSummary {
        id: s.session.meta.id.clone(),
        // resumo derivado do log quando o chat nunca ganhou título
        title: session::display_title(&s.session),
        mode: s.mode.label().to_string(),
        chat_dir: s.session.meta.chat_dir.clone(),
        folder: s.session.meta.chat_folder_name.clone(),
        busy: s.busy,
        subscribers: s.subscribers.len(),
        updated_at: s.session.meta.updated_at.clone(),
        short_id: protocol::short_id(&s.session.meta.id),
        approx_bytes: approx,
        history_msgs: s.history.len(),
        pinned: s.session.meta.pinned,
    }
}

#[cfg(unix)]
fn handle_unix_client(
    state: Arc<Mutex<DaemonState>>,
    stream: std::os::unix::net::UnixStream,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    let reader = BufReader::new(stream.try_clone()?);
    let writer = stream;
    serve_connection(state, reader, writer)
}

fn handle_tcp_client(state: Arc<Mutex<DaemonState>>, stream: TcpStream) -> Result<()> {
    stream.set_nodelay(true)?;
    let reader = BufReader::new(stream.try_clone()?);
    serve_connection(state, reader, stream)
}

fn serve_connection<R: BufRead, W: Write + Send + 'static>(
    state: Arc<Mutex<DaemonState>>,
    mut reader: R,
    mut writer: W,
) -> Result<()> {
    let (out_tx, out_rx) = mpsc::channel::<ServerMsg>();
    let client_id = {
        let mut g = state.lock().unwrap();
        let id = g.next_client;
        g.next_client += 1;
        g.clients.insert(id, out_tx.clone());
        id
    };

    thread::spawn(move || {
        while let Ok(msg) = out_rx.recv() {
            if serde_json::to_writer(&mut writer, &msg).is_err() {
                break;
            }
            if writer.write_all(b"\n").is_err() || writer.flush().is_err() {
                break;
            }
        }
    });

    let _ = out_tx.send(ServerMsg::Hello {
        server: "harness-daemon".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    });

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: ClientMsg = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => {
                let _ = out_tx.send(ServerMsg::Error {
                    message: format!("bad json: {e}"),
                });
                continue;
            }
        };
        if matches!(msg, ClientMsg::Shutdown) {
            let _ = out_tx.send(ServerMsg::Ok {
                message: "bye".into(),
            });
            break;
        }
        if let Err(e) = handle_msg(&state, client_id, msg, &out_tx) {
            let _ = out_tx.send(ServerMsg::Error {
                message: e.to_string(),
            });
        }
    }

    if let Ok(mut g) = state.lock() {
        g.clients.remove(&client_id);
        for s in g.sessions.values_mut() {
            s.subscribers.retain(|id| *id != client_id);
        }
    }
    Ok(())
}

fn broadcast_session(state: &Arc<Mutex<DaemonState>>, session_id: &str, msg: ServerMsg) {
    let (subs, clients) = {
        let g = state.lock().unwrap();
        let subs = g
            .sessions
            .get(session_id)
            .map(|s| s.subscribers.clone())
            .unwrap_or_default();
        let clients = g.clients.clone();
        (subs, clients)
    };
    for id in subs {
        if let Some(tx) = clients.get(&id) {
            let _ = tx.send(msg.clone());
        }
    }
}

fn handle_msg(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    msg: ClientMsg,
    out_tx: &Sender<ServerMsg>,
) -> Result<()> {
    match msg {
        ClientMsg::Hello { .. } => {
            out_tx.send(ServerMsg::Ok {
                message: "hello".into(),
            })?;
        }
        ClientMsg::Ping => out_tx.send(ServerMsg::Pong)?,
        ClientMsg::DaemonInfo => {
            let g = state.lock().unwrap();
            let socket = if cfg!(windows) {
                protocol::default_tcp_addr()
            } else {
                protocol::default_socket_path().display().to_string()
            };
            let sessions_bytes: u64 = g.sessions.values().map(|s| summary_of(s).approx_bytes).sum();
            let rss_kb = crate::mem_stats::self_rss_kb().unwrap_or(0);
            out_tx.send(ServerMsg::DaemonInfo {
                live_sessions: g.sessions.len(),
                max_sessions: g.cfg.max_sessions,
                clients: g.clients.len(),
                socket,
                rss_kb,
                sessions_bytes,
                pid: std::process::id(),
            })?;
        }
        ClientMsg::RuntimeInfo => {
            // Workers e métricas vivem neste processo; a GUI não tem como
            // enxergá-los sozinha. O grafo não vem daqui: a raiz é por sessão
            // (o projeto apontado), e o daemon só conhece o workspace global.
            out_tx.send(ServerMsg::RuntimeInfo {
                swarm: crate::swarm::snapshot(),
                metrics: crate::metrics::snapshot(),
            })?;
        }
        ClientMsg::SwarmStop { id } => {
            let swarm = crate::swarm::global_swarm();
            let mut g = swarm.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            let message = if id == "all" || id == "*" {
                g.stop_all();
                "swarm stopped".to_string()
            } else {
                g.stop(&id)
            };
            out_tx.send(ServerMsg::Ok { message })?;
        }
        ClientMsg::UpdateSession {
            session_id,
            title,
            pinned,
            project_dir,
        } => {
            let mut done = false;
            {
                let mut g = state.lock().unwrap();
                if let Some(s) = g.sessions.get_mut(&session_id) {
                    if let Some(t) = title.as_deref() {
                        let t = t.trim();
                        if !t.is_empty() {
                            s.session.meta.title = t.chars().take(80).collect();
                            s.session.meta.title_locked = true;
                        }
                    }
                    if let Some(p) = pinned {
                        s.session.meta.pinned = p;
                    }
                    if let Some(p) = project_dir.clone() {
                        s.session.meta.project_dir = p.filter(|v| !v.trim().is_empty());
                    }
                    let _ = session::save_session(&s.session);
                    done = true;
                }
            }
            if !done {
                // sessão não está viva: mexe direto no disco
                let _ = session::update_meta(&session_id, title.as_deref(), pinned, project_dir);
            }
            out_tx.send(ServerMsg::Ok {
                message: "session updated".into(),
            })?;
        }
        ClientMsg::ListSessions => {
            let g = state.lock().unwrap();
            let mut sessions: Vec<_> = g.sessions.values().map(summary_of).collect();
            sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            out_tx.send(ServerMsg::SessionList { sessions })?;
        }
        ClientMsg::CreateSession {
            mode,
            title,
            chat_dir: reuse_dir,
            session_id: prefer_id,
        } => {
            create_or_restore(
                state,
                client_id,
                mode,
                title,
                reuse_dir,
                prefer_id,
                out_tx,
            )?;
        }
        ClientMsg::Attach { session_id } => {
            attach_session(state, client_id, &session_id, out_tx)?;
        }
        ClientMsg::Detach { session_id } => {
            let mut g = state.lock().unwrap();
            if let Some(s) = g.sessions.get_mut(&session_id) {
                s.subscribers.retain(|id| *id != client_id);
                out_tx.send(ServerMsg::Ok {
                    message: format!("detached {session_id}"),
                })?;
            } else {
                out_tx.send(ServerMsg::Error {
                    message: format!("unknown session {session_id}"),
                })?;
            }
        }
        ClientMsg::KillSession {
            session_id,
            delete_disk,
        } => {
            kill_session(state, &session_id, delete_disk, out_tx)?;
        }
        ClientMsg::UserMessage {
            session_id,
            text,
            token_less,
            gauntlet,
            effort,
            goal,
        } => {
            if token_less.is_some() || gauntlet.is_some() || effort.is_some() || goal.is_some() {
                let mut g = state.lock().unwrap();
                if let Some(s) = g.sessions.get_mut(&session_id) {
                    if let Some(level) = token_less
                        .as_deref()
                        .and_then(crate::tokenless::TokenLessLevel::parse)
                    {
                        s.token_less = Some(level);
                    }
                    if let Some(on) = gauntlet {
                        s.gauntlet = on;
                    }
                    if let Some(e) = effort {
                        s.effort = Some(e);
                    }
                    if let Some(g) = goal {
                        s.goal = g;
                    }
                }
            }
            start_turn(state, &session_id, text, out_tx)?;
        }
        ClientMsg::Cancel { session_id } => {
            let g = state.lock().unwrap();
            if let Some(s) = g.sessions.get(&session_id) {
                if let Some(c) = &s.cancel {
                    c.store(true, Ordering::Relaxed);
                }
            }
            out_tx.send(ServerMsg::Ok {
                message: "cancel signaled".into(),
            })?;
        }
        ClientMsg::Approval {
            session_id,
            allow,
            always_shell,
        } => {
            let g = state.lock().unwrap();
            if let Some(s) = g.sessions.get(&session_id) {
                if let Some(tx) = &s.approval_tx {
                    let d = if always_shell {
                        ApprovalDecision::AllowAlwaysShell
                    } else if allow {
                        ApprovalDecision::Allow
                    } else {
                        ApprovalDecision::Deny
                    };
                    let _ = tx.send(d);
                }
            }
            out_tx.send(ServerMsg::Ok {
                message: "approval sent".into(),
            })?;
        }
        ClientMsg::Bus {
            from_session,
            to_session,
            body,
        } => {
            handle_bus(state, &from_session, to_session.as_deref(), &body, out_tx)?;
        }
        ClientMsg::Shutdown => {}
    }
    Ok(())
}

fn create_or_restore(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    mode: String,
    title: Option<String>,
    reuse_dir: Option<String>,
    prefer_id: Option<String>,
    out_tx: &Sender<ServerMsg>,
) -> Result<()> {
    let mode = if mode.eq_ignore_ascii_case("office") {
        AppMode::Office
    } else {
        AppMode::Code
    };

    // Restore by id if already live
    if let Some(ref id) = prefer_id {
        let mut g = state.lock().unwrap();
        if let Some(s) = g.sessions.get_mut(id) {
            if !s.subscribers.contains(&client_id) {
                s.subscribers.push(client_id);
            }
            let session_id = s.session.meta.id.clone();
            let chat_dir = s.session.meta.chat_dir.clone();
            let title = s.session.meta.title.clone();
            drop(g);
            out_tx.send(ServerMsg::SessionCreated {
                session_id,
                chat_dir,
                title,
            })?;
            return Ok(());
        }
        // try load from disk into live map
        if let Ok(sess) = session::load_session(id) {
            if g.sessions.len() >= g.cfg.max_sessions && !evict_idle_session(&mut g) {
                out_tx.send(ServerMsg::Error {
                    message: format!(
                        "max_sessions={} atingido e todas ocupadas — pare um turno ou aumente o limite",
                        g.cfg.max_sessions
                    ),
                })?;
                return Ok(());
            }
            let history = sess.messages.clone();
            let session_id = sess.meta.id.clone();
            let chat_dir = sess.meta.chat_dir.clone();
            let title = sess.meta.title.clone();
            let mode_s = sess.meta.mode;
            g.sessions.insert(
                session_id.clone(),
                LiveSession {
                    session: sess,
                    history,
                    mode: mode_s,
                    busy: false,
                    cancel: None,
                    approval_tx: None,
                    subscribers: vec![client_id],
                    token_less: None,
                    gauntlet: false,
                    effort: None,
                    goal: String::new(),
                },
            );
            drop(g);
            out_tx.send(ServerMsg::SessionCreated {
                session_id,
                chat_dir,
                title,
            })?;
            return Ok(());
        }
    }

    // Reuse chat_dir: attach existing live session with same folder
    if let Some(ref dir) = reuse_dir {
        if !dir.is_empty() {
            let mut g = state.lock().unwrap();
            if let Some((id, _)) = g
                .sessions
                .iter()
                .find(|(_, s)| s.session.meta.chat_dir == *dir)
                .map(|(id, _)| (id.clone(), ()))
            {
                if let Some(s) = g.sessions.get_mut(&id) {
                    if !s.subscribers.contains(&client_id) {
                        s.subscribers.push(client_id);
                    }
                    let session_id = s.session.meta.id.clone();
                    let chat_dir = s.session.meta.chat_dir.clone();
                    let title = s.session.meta.title.clone();
                    drop(g);
                    out_tx.send(ServerMsg::SessionCreated {
                        session_id,
                        chat_dir,
                        title,
                    })?;
                    return Ok(());
                }
            }
        }
    }

    let mut g = state.lock().unwrap();
    if g.sessions.len() >= g.cfg.max_sessions && !evict_idle_session(&mut g) {
        out_tx.send(ServerMsg::Error {
            message: format!(
                "max_sessions={} atingido e todas ocupadas — pare um turno ou aumente o limite",
                g.cfg.max_sessions
            ),
        })?;
        return Ok(());
    }
    if !g.cfg.workspace_ready {
        g.cfg.workspace = crate::config::suggested_workspace();
        g.cfg.workspace_ready = true;
        let _ = crate::config::ensure_workspace_layout(&g.cfg.workspace);
    }
    let mut sess = if let Some(dir) = reuse_dir.filter(|d| !d.is_empty()) {
        let p = PathBuf::from(&dir);
        let _ = crate::config::ensure_workspace_layout(&p);
        let folder = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "chat".into());
        let id = uuid::Uuid::new_v4().to_string();
        Session {
            meta: crate::session::SessionMeta {
                id: id.clone(),
                title: title.clone().unwrap_or_else(|| folder.clone()),
                mode,
                updated_at: chrono::Utc::now().to_rfc3339(),
                workspace: g.cfg.workspace.display().to_string(),
                chat_dir: dir,
                chat_folder_name: folder,
                daemon_session_id: id,
                token_less: None,
                gauntlet: false,
                effort: None,
                goal: String::new(),
                pinned: false,
                project_dir: None,
                title_locked: false,
            },
            messages: Vec::new(),
            ui_log: Vec::new(),
        }
    } else {
        Session::new(mode, &g.cfg.workspace)
    };
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        sess.meta.title = t;
    }
    sess.meta.daemon_session_id = sess.meta.id.clone();
    let id = sess.meta.id.clone();
    let chat_dir = sess.meta.chat_dir.clone();
    let title = sess.meta.title.clone();
    let _ = session::save_session(&sess);
    g.sessions.insert(
        id.clone(),
        LiveSession {
            session: sess,
            history: Vec::new(),
            mode,
            busy: false,
            cancel: None,
            approval_tx: None,
            subscribers: vec![client_id],
            token_less: None,
            gauntlet: false,
            effort: None,
            goal: String::new(),
        },
    );
    drop(g);
    out_tx.send(ServerMsg::SessionCreated {
        session_id: id,
        chat_dir,
        title,
    })?;
    Ok(())
}

fn attach_session(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    session_id: &str,
    out_tx: &Sender<ServerMsg>,
) -> Result<()> {
    // hydrate from disk if not live
    {
        let mut g = state.lock().unwrap();
        if !g.sessions.contains_key(session_id) {
            if let Ok(sess) = session::load_session(session_id) {
                if g.sessions.len() >= g.cfg.max_sessions && !evict_idle_session(&mut g) {
                    out_tx.send(ServerMsg::Error {
                        message: format!(
                            "max_sessions={} — cannot hydrate session",
                            g.cfg.max_sessions
                        ),
                    })?;
                    return Ok(());
                }
                let history = sess.messages.clone();
                let mode = sess.meta.mode;
                g.sessions.insert(
                    sess.meta.id.clone(),
                    LiveSession {
                        session: sess,
                        history,
                        mode,
                        busy: false,
                        cancel: None,
                        approval_tx: None,
                        subscribers: Vec::new(),
                        token_less: None,
                        gauntlet: false,
                        effort: None,
                        goal: String::new(),
                    },
                );
            }
        }
    }

    let mut g = state.lock().unwrap();
    if let Some(s) = g.sessions.get_mut(session_id) {
        if !s.subscribers.contains(&client_id) {
            s.subscribers.push(client_id);
        }
        let hist: Vec<_> = s
            .history
            .iter()
            .filter_map(|m| {
                Some(json!({
                    "role": m.role,
                    "content": m.content,
                }))
            })
            .collect();
        let chat_dir = s.session.meta.chat_dir.clone();
        let title = s.session.meta.title.clone();
        let mode = s.mode.label().to_string();
        let folder = s.session.meta.chat_folder_name.clone();
        let busy = s.busy;
        let updated_at = s.session.meta.updated_at.clone();
        drop(g);
        out_tx.send(ServerMsg::Event {
            session_id: session_id.into(),
            event: "attached".into(),
            payload: json!({
                "title": title,
                "chat_dir": chat_dir,
                "folder": folder,
                "mode": mode,
                "busy": busy,
                "updated_at": updated_at,
                "history": hist,
            }),
        })?;
        out_tx.send(ServerMsg::Ok {
            message: format!("attached {session_id}"),
        })?;
    } else {
        out_tx.send(ServerMsg::Error {
            message: format!("unknown session {session_id}"),
        })?;
    }
    Ok(())
}

fn kill_session(
    state: &Arc<Mutex<DaemonState>>,
    session_id: &str,
    delete_disk: bool,
    out_tx: &Sender<ServerMsg>,
) -> Result<()> {
    let mut g = state.lock().unwrap();
    if let Some(s) = g.sessions.remove(session_id) {
        if let Some(c) = &s.cancel {
            c.store(true, Ordering::Relaxed);
        }
        if delete_disk {
            let _ = session::delete_session(session_id);
        } else {
            let _ = session::save_session(&s.session);
        }
        drop(g);
        out_tx.send(ServerMsg::SessionKilled {
            session_id: session_id.into(),
        })?;
        out_tx.send(ServerMsg::Ok {
            message: format!("killed {session_id}"),
        })?;
    } else if delete_disk {
        // não estava viva, mas apagar do disco é exatamente o pedido
        let _ = session::delete_session(session_id);
        out_tx.send(ServerMsg::SessionKilled {
            session_id: session_id.into(),
        })?;
        out_tx.send(ServerMsg::Ok {
            message: format!("deleted {session_id}"),
        })?;
    } else {
        out_tx.send(ServerMsg::Error {
            message: format!("unknown session {session_id}"),
        })?;
    }
    Ok(())
}

fn handle_bus(
    state: &Arc<Mutex<DaemonState>>,
    from: &str,
    to: Option<&str>,
    body: &str,
    out_tx: &Sender<ServerMsg>,
) -> Result<()> {
    let targets: Vec<String> = {
        let g = state.lock().unwrap();
        match to {
            Some(t) if !t.is_empty() && t != "*" => {
                if g.sessions.contains_key(t) {
                    vec![t.to_string()]
                } else {
                    Vec::new()
                }
            }
            _ => g
                .sessions
                .keys()
                .filter(|id| id.as_str() != from)
                .cloned()
                .collect(),
        }
    };
    if targets.is_empty() {
        out_tx.send(ServerMsg::Error {
            message: "bus: no target sessions".into(),
        })?;
        return Ok(());
    }
    let payload = json!({ "from": from, "body": body });
    for sid in targets {
        broadcast_session(
            state,
            &sid,
            ServerMsg::Event {
                session_id: sid.clone(),
                event: "bus".into(),
                payload: payload.clone(),
            },
        );
    }
    out_tx.send(ServerMsg::Ok {
        message: format!("bus delivered from {from}"),
    })?;
    Ok(())
}

fn start_turn(
    state: &Arc<Mutex<DaemonState>>,
    session_id: &str,
    text: String,
    out_tx: &Sender<ServerMsg>,
) -> Result<()> {
    let (cfg, mode, history, guard_writes) = {
        let mut g = state.lock().unwrap();
        let cfg_base = g.cfg.clone();
        let s = g
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("unknown session"))?;
        if s.busy {
            bail!("session busy");
        }
        s.busy = true;
        s.session.touch_title_from_user(&text);
        s.history.push(ChatMessage {
            role: "user".into(),
            content: Some(text.clone()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
        let mut cfg = cfg_base;
        // Projeto apontado manda; sem ele o agente fica na pasta do chat.
        cfg.workspace = session::effective_root(
            s.session.meta.project_dir.as_deref(),
            &s.session.meta.chat_dir,
        );
        // Override por sessão vence o padrão global; os workers do swarm
        // herdam pelo mesmo `cfg`.
        if let Some(level) = s.token_less {
            cfg.token_less = level;
        }
        cfg.gauntlet = s.gauntlet;
        if let Some(e) = &s.effort {
            cfg.reasoning_effort = e.clone();
        }
        cfg.goal = s.goal.clone();
        let guard = s.session.meta.project_dir.is_some();
        (cfg, s.mode, s.history.clone(), guard)
    };

    let handle = agent::spawn_turn(cfg, mode, history, guard_writes);
    {
        let mut g = state.lock().unwrap();
        if let Some(s) = g.sessions.get_mut(session_id) {
            s.cancel = Some(Arc::clone(&handle.cancel));
            s.approval_tx = Some(handle.approval_tx.clone());
        }
    }

    let sid = session_id.to_string();
    let state_bg = Arc::clone(state);
    thread::spawn(move || {
        while let Ok(ev) = handle.event_rx.recv() {
            let (kind, payload) = event_to_json(&ev);
            broadcast_session(
                &state_bg,
                &sid,
                ServerMsg::Event {
                    session_id: sid.clone(),
                    event: kind,
                    payload,
                },
            );
            match &ev {
                AgentEvent::Done { reply, .. } => {
                    let mut g = state_bg.lock().unwrap();
                    if let Some(s) = g.sessions.get_mut(&sid) {
                        s.history.push(ChatMessage {
                            role: "assistant".into(),
                            content: Some(reply.clone()),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                        s.session.messages = s.history.clone();
                        s.busy = false;
                        s.cancel = None;
                        s.approval_tx = None;
                        let _ = session::save_session(&s.session);
                    }
                    break;
                }
                AgentEvent::Error(_) | AgentEvent::Cancelled => {
                    let mut g = state_bg.lock().unwrap();
                    if let Some(s) = g.sessions.get_mut(&sid) {
                        s.session.messages = s.history.clone();
                        s.busy = false;
                        s.cancel = None;
                        s.approval_tx = None;
                        let _ = session::save_session(&s.session);
                    }
                    break;
                }
                _ => {}
            }
        }
    });

    out_tx.send(ServerMsg::Ok {
        message: "turn started".into(),
    })?;
    Ok(())
}

fn event_to_json(ev: &AgentEvent) -> (String, serde_json::Value) {
    match ev {
        AgentEvent::Status(s) => ("status".into(), json!({ "text": s })),
        AgentEvent::StreamDelta(s) => ("stream".into(), json!({ "text": s })),
        AgentEvent::ToolStart { name, args } => (
            "tool_start".into(),
            json!({ "name": name, "args": args }),
        ),
        AgentEvent::ToolResult { name, result } => (
            "tool_result".into(),
            json!({ "name": name, "result": result }),
        ),
        AgentEvent::NeedApproval { name, args_preview } => (
            "need_approval".into(),
            json!({ "name": name, "args": args_preview }),
        ),
        AgentEvent::Round { n, max } => ("round".into(), json!({ "n": n, "max": max })),
        AgentEvent::Done { reply, stuck } => {
            ("done".into(), json!({ "reply": reply, "stuck": stuck }))
        }
        AgentEvent::Error(e) => ("error".into(), json!({ "message": e })),
        AgentEvent::Cancelled => ("cancelled".into(), json!({})),
    }
}

/// Thin client helpers for CLI / GUI.
pub fn connect_stream(tcp: bool) -> Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
    if tcp || cfg!(windows) {
        let addr = crate::protocol::default_tcp_addr();
        let stream = TcpStream::connect(&addr).with_context(|| format!("connect {addr}"))?;
        stream.set_nodelay(true)?;
        let r = BufReader::new(stream.try_clone()?);
        Ok((Box::new(r), Box::new(stream)))
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::net::UnixStream;
            let path = crate::protocol::default_socket_path();
            let stream = UnixStream::connect(&path)
                .with_context(|| format!("connect {}", path.display()))?;
            let r = BufReader::new(stream.try_clone()?);
            Ok((Box::new(r), Box::new(stream)))
        }
        #[cfg(not(unix))]
        {
            bail!("unix sockets unsupported")
        }
    }
}

pub fn write_msg(w: &mut dyn Write, msg: &ClientMsg) -> Result<()> {
    serde_json::to_writer(&mut *w, msg)?;
    w.write_all(b"\n")?;
    w.flush()?;
    Ok(())
}

pub fn read_msg(r: &mut dyn BufRead) -> Result<ServerMsg> {
    let mut line = String::new();
    r.read_line(&mut line)?;
    if line.is_empty() {
        bail!("daemon closed connection");
    }
    Ok(serde_json::from_str(line.trim())?)
}

pub fn ensure_daemon_running() -> Result<()> {
    if connect_stream(cfg!(windows)).is_ok() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve");
    #[cfg(windows)]
    cmd.arg("--tcp");
    if std::env::var("HARNESS_DAEMON_TCP").ok().as_deref() == Some("1") {
        cmd.arg("--tcp");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd.spawn().context("spawn daemon")?;
    for _ in 0..80 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if connect_stream(cfg!(windows)).is_ok() {
            return Ok(());
        }
    }
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if connect_stream(true).is_ok() {
            return Ok(());
        }
    }
    bail!("daemon did not start")
}
