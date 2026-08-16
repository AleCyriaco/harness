//! GUI/CLI long-lived client to the multi-client daemon.

use anyhow::{Context, Result, bail};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::agent::{AgentEvent, ApprovalDecision};
use crate::daemon;
use crate::modes::AppMode;
use crate::protocol::{ClientMsg, ServerMsg, SessionSummary};

pub enum Incoming {
    /// Correlated request/response (non-event)
    Reply(ServerMsg),
    /// Agent stream for a session
    Event {
        session_id: String,
        event: AgentEvent,
    },
    /// Raw attach history etc.
    RawEvent {
        session_id: String,
        event: String,
        payload: serde_json::Value,
    },
    Disconnected(String),
}

pub struct DaemonGuiClient {
    write: Arc<Mutex<Box<dyn Write + Send>>>,
    incoming: Receiver<Incoming>,
    /// Eventos que chegaram no meio de uma chamada request/response.
    /// Sem isto um delta de stream some quando a GUI pede daemon_info.
    stash: Stash,
}

type Stash = Arc<Mutex<VecDeque<Incoming>>>;

impl DaemonGuiClient {
    /// Ensure daemon is up and connect. Reader thread feeds `incoming`.
    pub fn connect() -> Result<Self> {
        daemon::ensure_daemon_running()?;
        let use_tcp = cfg!(windows)
            || std::env::var("HARNESS_DAEMON_TCP").ok().as_deref() == Some("1");
        let (r, w) = match daemon::connect_stream(use_tcp) {
            Ok(x) => (x.0, x.1),
            Err(_) => daemon::connect_stream(true)?, // force TCP fallback
        };
        let write = Arc::new(Mutex::new(w));
        let stash: Stash = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, incoming) = mpsc::channel();

        // reader thread owns reader
        thread::Builder::new()
            .name("harness-daemon-reader".into())
            .spawn(move || {
                let mut reader = r;
                loop {
                    match daemon::read_msg(&mut reader) {
                        Ok(msg) => {
                            if let Err(_) = dispatch_incoming(&tx, msg) {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Incoming::Disconnected(e.to_string()));
                            break;
                        }
                    }
                }
            })
            .context("spawn reader")?;

        // drain hello
        let _ = wait_reply_filter(&incoming, &stash, |m| {
            matches!(m, ServerMsg::Hello { .. } | ServerMsg::Ok { .. })
        }, 2000);

        let client = Self {
            write,
            incoming,
            stash,
        };
        // announce
        client.send(&ClientMsg::Hello {
            client: "harness-gui".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })?;
        let _ = wait_reply_filter(&client.incoming, &client.stash, |_| true, 1000);

        Ok(client)
    }

    pub fn send(&self, msg: &ClientMsg) -> Result<()> {
        let mut w = self.write.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        daemon::write_msg(&mut **w, msg)
    }

    pub fn try_recv(&self) -> Option<Incoming> {
        if let Ok(mut q) = self.stash.lock() {
            if let Some(m) = q.pop_front() {
                return Some(m);
            }
        }
        self.incoming.try_recv().ok()
    }

    /// Espera uma resposta guardando (em vez de descartar) o que chegar junto.
    fn wait_reply(
        &self,
        mut pred: impl FnMut(&ServerMsg) -> bool,
        timeout_ms: u64,
    ) -> Result<ServerMsg> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            if std::time::Instant::now() > deadline {
                bail!("timeout waiting for daemon reply");
            }
            match self
                .incoming
                .recv_timeout(std::time::Duration::from_millis(100))
            {
                Ok(Incoming::Reply(m)) if pred(&m) => return Ok(m),
                Ok(Incoming::Reply(ServerMsg::Error { message })) => bail!(message),
                Ok(Incoming::Disconnected(e)) => {
                    self.push_stash(Incoming::Disconnected(e.clone()));
                    bail!("{e}");
                }
                Ok(other) => self.push_stash(other),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(e) => bail!("{e}"),
            }
        }
    }

    fn push_stash(&self, m: Incoming) {
        if let Ok(mut q) = self.stash.lock() {
            if q.len() < 4096 {
                q.push_back(m);
            }
        }
    }

    /// Renomeia e/ou fixa um chat (o daemon repassa para o disco).
    pub fn update_session(
        &self,
        session_id: &str,
        title: Option<&str>,
        pinned: Option<bool>,
        project_dir: Option<Option<String>>,
    ) -> Result<()> {
        self.send(&ClientMsg::UpdateSession {
            session_id: session_id.into(),
            title: title.map(|s| s.to_string()),
            pinned,
            project_dir,
        })
    }

    pub fn swarm_stop(&self, id: &str) -> Result<()> {
        self.send(&ClientMsg::SwarmStop { id: id.into() })?;
        Ok(())
    }

    /// Swarm + métricas — vivem no daemon, não aqui.
    pub fn runtime_info(&self) -> Result<(crate::swarm::SwarmSnapshot, crate::metrics::Metrics)> {
        self.send(&ClientMsg::RuntimeInfo)?;
        // socket local: se não responder rápido, tenta de novo no próximo ciclo
        // (nunca vale travar o frame da GUI por causa disto)
        match self.wait_reply(|m| matches!(m, ServerMsg::RuntimeInfo { .. }), 800)? {
            ServerMsg::RuntimeInfo { swarm, metrics } => Ok((swarm, metrics)),
            _ => bail!("unexpected reply"),
        }
    }

    pub fn create_session(
        &self,
        mode: AppMode,
        title: Option<String>,
        chat_dir: Option<String>,
    ) -> Result<(String, String, String)> {
        self.create_session_ex(mode, title, chat_dir, None)
    }

    pub fn create_session_ex(
        &self,
        mode: AppMode,
        title: Option<String>,
        chat_dir: Option<String>,
        session_id: Option<String>,
    ) -> Result<(String, String, String)> {
        // session_id, chat_dir, title
        self.send(&ClientMsg::CreateSession {
            mode: mode.label().to_ascii_lowercase(),
            title,
            chat_dir,
            session_id,
        })?;
        // drain until SessionCreated
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                bail!("timeout waiting for session_created");
            }
            match self.incoming.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Incoming::Reply(ServerMsg::SessionCreated {
                    session_id,
                    chat_dir,
                    title,
                })) => return Ok((session_id, chat_dir, title)),
                Ok(Incoming::Reply(ServerMsg::Error { message })) => bail!(message),
                Ok(Incoming::Disconnected(e)) => bail!("disconnected: {e}"),
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(e) => bail!("{e}"),
            }
        }
    }

    pub fn attach(&self, session_id: &str) -> Result<serde_json::Value> {
        self.send(&ClientMsg::Attach {
            session_id: session_id.into(),
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                bail!("timeout attach");
            }
            match self.incoming.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Incoming::RawEvent {
                    event,
                    payload,
                    ..
                }) if event == "attached" => return Ok(payload),
                Ok(Incoming::Reply(ServerMsg::Error { message })) => bail!(message),
                Ok(Incoming::Reply(ServerMsg::Ok { .. })) => continue,
                Ok(Incoming::Disconnected(e)) => bail!("{e}"),
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(e) => bail!("{e}"),
            }
        }
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        self.send(&ClientMsg::ListSessions)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                bail!("timeout list");
            }
            match self.incoming.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Incoming::Reply(ServerMsg::SessionList { sessions })) => return Ok(sessions),
                Ok(Incoming::Reply(ServerMsg::Error { message })) => bail!(message),
                Ok(Incoming::Disconnected(e)) => bail!("{e}"),
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(e) => bail!("{e}"),
            }
        }
    }

    /// Estado por chat que o daemon precisa saber a cada mensagem.
    pub fn user_message(
        &self,
        session_id: &str,
        text: &str,
        gauntlet: Option<bool>,
        effort: Option<String>,
        goal: Option<String>,
        get_it_done: Option<bool>,
    ) -> Result<()> {
        self.send(&ClientMsg::UserMessage {
            session_id: session_id.into(),
            text: text.into(),
            gauntlet,
            effort,
            goal,
            get_it_done,
        })?;
        // wait Ok turn started (optional)
        let _ = wait_reply_filter(&self.incoming, &self.stash, |m| {
            matches!(m, ServerMsg::Ok { .. } | ServerMsg::Error { .. })
        }, 3000);
        Ok(())
    }

    /// `every_secs = 0` + prompt vazio cancela; prompt "?" lista.
    pub fn schedule(&self, session_id: &str, every_secs: u64, prompt: &str) -> Result<String> {
        self.send(&ClientMsg::Schedule {
            session_id: session_id.into(),
            every_secs,
            prompt: if prompt == "?" { String::new() } else { prompt.into() },
        })?;
        match wait_reply_filter(
            &self.incoming,
            &self.stash,
            |m| matches!(m, ServerMsg::Ok { .. } | ServerMsg::Error { .. }),
            3000,
        ) {
            Some(ServerMsg::Ok { message }) => Ok(message),
            Some(ServerMsg::Error { message }) => Err(anyhow::anyhow!(message)),
            _ => Err(anyhow::anyhow!("daemon did not answer")),
        }
    }

    pub fn cancel(&self, session_id: &str) -> Result<()> {
        self.send(&ClientMsg::Cancel {
            session_id: session_id.into(),
        })
    }

    pub fn approve(&self, session_id: &str, d: ApprovalDecision) -> Result<()> {
        let (allow, always_shell) = match d {
            ApprovalDecision::Allow => (true, false),
            ApprovalDecision::Deny => (false, false),
            ApprovalDecision::AllowAlwaysShell => (true, true),
        };
        self.send(&ClientMsg::Approval {
            session_id: session_id.into(),
            allow,
            always_shell,
        })
    }

    pub fn detach(&self, session_id: &str) -> Result<()> {
        self.send(&ClientMsg::Detach {
            session_id: session_id.into(),
        })?;
        let _ = wait_reply_filter(
            &self.incoming,
            &self.stash,
            |m| matches!(m, ServerMsg::Ok { .. } | ServerMsg::Error { .. }),
            2000,
        );
        Ok(())
    }

    pub fn kill_session(&self, session_id: &str, delete_disk: bool) -> Result<()> {
        self.send(&ClientMsg::KillSession {
            session_id: session_id.into(),
            delete_disk,
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                bail!("timeout kill");
            }
            match self
                .incoming
                .recv_timeout(std::time::Duration::from_millis(200))
            {
                Ok(Incoming::Reply(ServerMsg::SessionKilled { .. })) => return Ok(()),
                Ok(Incoming::Reply(ServerMsg::Ok { .. })) => return Ok(()),
                Ok(Incoming::Reply(ServerMsg::Error { message })) => bail!(message),
                Ok(Incoming::Disconnected(e)) => bail!("{e}"),
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(e) => bail!("{e}"),
            }
        }
    }

    /// live, max, clients, socket, rss_kb, sessions_bytes, pid
    pub fn daemon_info(&self) -> Result<(usize, usize, usize, String, u64, u64, u32)> {
        self.send(&ClientMsg::DaemonInfo)?;
        match self.wait_reply(|m| matches!(m, ServerMsg::DaemonInfo { .. }), 5000)? {
            ServerMsg::DaemonInfo {
                live_sessions,
                max_sessions,
                clients,
                socket,
                rss_kb,
                sessions_bytes,
                pid,
            } => Ok((
                live_sessions,
                max_sessions,
                clients,
                socket,
                rss_kb,
                sessions_bytes,
                pid,
            )),
            _ => bail!("unexpected reply"),
        }
    }

}

fn dispatch_incoming(tx: &Sender<Incoming>, msg: ServerMsg) -> Result<(), ()> {
    match msg {
        ServerMsg::Event {
            session_id,
            event,
            payload,
        } => {
            if event == "attached" {
                let _ = tx.send(Incoming::RawEvent {
                    session_id,
                    event,
                    payload,
                });
            } else if let Some(ev) = map_event(&event, &payload) {
                let _ = tx.send(Incoming::Event {
                    session_id,
                    event: ev,
                });
            } else {
                let _ = tx.send(Incoming::RawEvent {
                    session_id,
                    event,
                    payload,
                });
            }
        }
        other => {
            let _ = tx.send(Incoming::Reply(other));
        }
    }
    Ok(())
}

fn map_event(event: &str, payload: &serde_json::Value) -> Option<AgentEvent> {
    match event {
        "status" => Some(AgentEvent::Status(
            payload
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
        )),
        "stream" => Some(AgentEvent::StreamDelta(
            payload
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
        )),
        "tool_start" => Some(AgentEvent::ToolStart {
            name: payload
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .into(),
            args: payload
                .get("args")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
        }),
        "tool_result" => Some(AgentEvent::ToolResult {
            name: payload
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .into(),
            result: payload
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
        }),
        "need_approval" => Some(AgentEvent::NeedApproval {
            name: payload
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .into(),
            args_preview: payload
                .get("args")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
        }),
        "round" => Some(AgentEvent::Round {
            n: payload.get("n").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            max: payload.get("max").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        }),
        "done" => Some(AgentEvent::Done {
            reply: payload
                .get("reply")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
            // daemon antigo não manda o campo — ausente = não travou
            stuck: payload
                .get("stuck")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }),
        "error" => Some(AgentEvent::Error(
            payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error")
                .into(),
        )),
        "cancelled" => Some(AgentEvent::Cancelled),
        _ => None,
    }
}

fn wait_reply_filter(
    rx: &Receiver<Incoming>,
    stash: &Stash,
    mut pred: impl FnMut(&ServerMsg) -> bool,
    timeout_ms: u64,
) -> Option<ServerMsg> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(Incoming::Reply(m)) if pred(&m) => return Some(m),
            Ok(other) => {
                if let Ok(mut q) = stash.lock() {
                    q.push_back(other);
                }
            }
            Err(_) => continue,
        }
    }
    None
}
