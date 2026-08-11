//! Headless CLI: serve / connect / run / self-dev — jcode-style entrypoints.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::config::Config;
use crate::daemon;
use crate::protocol::ClientMsg;
use crate::selfdev;

pub fn dispatch(args: &[String]) -> Result<bool> {
    if args.is_empty() {
        return Ok(false);
    }
    match args[0].as_str() {
        "serve" | "daemon" => {
            let mut cfg = Config::load();
            if !cfg.workspace_ready {
                cfg.workspace = crate::config::suggested_workspace();
                cfg.workspace_ready = true;
                let _ = crate::config::ensure_workspace_layout(&cfg.workspace);
                let _ = cfg.save();
            }
            let tcp = args.iter().any(|a| a == "--tcp");
            daemon::run_server(cfg, tcp)?;
            Ok(true)
        }
        "run" => {
            let prompt = args[1..].join(" ");
            if prompt.trim().is_empty() {
                bail!("usage: harness run <prompt>");
            }
            cli_run(&prompt, false)?;
            Ok(true)
        }
        "connect" => {
            cli_connect_repl()?;
            Ok(true)
        }
        "self-dev" | "selfdev" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("build");
            match sub {
                "build" => {
                    let msg = selfdev::build_release()?;
                    println!("{msg}");
                }
                "status" => {
                    println!("{}", selfdev::status());
                }
                "reload" => {
                    selfdev::reload()?;
                }
                _ => bail!("usage: harness self-dev [build|status|reload]"),
            }
            Ok(true)
        }
        "doctor" => {
            crate::provider_doctor::run()?;
            Ok(true)
        }
        "session" | "sessions" => {
            cli_session(&args[1..])?;
            Ok(true)
        }
        "version" | "--version" | "-V" => {
            println!("harness {}", env!("CARGO_PKG_VERSION"));
            Ok(true)
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn print_help() {
    println!(
        r#"harness {} — desktop agent + multi-session daemon

GUI:
  harness                  open desktop app

Daemon / headless:
  harness serve [--tcp]    start multi-client daemon
  harness connect          attach REPL to daemon
  harness run <prompt>     one-shot via daemon (starts daemon if needed)

Multi-session:
  harness session list              live sessions on daemon
  harness session info              capacity (live/max/clients)
  harness session create [title]    new live session
  harness session attach <id>       REPL on existing session
  harness session kill <id>         drop live session (keeps disk)
  harness session kill <id> --delete  also delete saved JSON

Self-dev:
  harness self-dev build|status|reload

Other:
  harness doctor           provider / env check
  harness --webview <url>  internal WebView
  harness help
"#,
        env!("CARGO_PKG_VERSION")
    );
}

fn cli_session(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "ls" => {
            ensure_daemon()?;
            let (mut r, mut w) = daemon::connect_stream(cfg!(windows))?;
            let _ = daemon::read_msg(&mut r)?;
            daemon::write_msg(&mut w, &ClientMsg::ListSessions)?;
            loop {
                match daemon::read_msg(&mut r)? {
                    crate::protocol::ServerMsg::SessionList { sessions } => {
                        if sessions.is_empty() {
                            println!("(no live sessions)");
                        }
                        for s in sessions {
                            let busy = if s.busy { "BUSY" } else { "idle" };
                            let mem = crate::mem_stats::format_bytes(s.approx_bytes as usize);
                            println!(
                                "{}  {}  [{busy}]  sub={}  msgs={}  ~{mem}  {}  {}",
                                s.short_id,
                                s.mode,
                                s.subscribers,
                                s.history_msgs,
                                s.folder,
                                s.title
                            );
                            println!("    id={}  {}", s.id, s.chat_dir);
                        }
                        return Ok(());
                    }
                    crate::protocol::ServerMsg::Error { message } => bail!(message),
                    crate::protocol::ServerMsg::Hello { .. }
                    | crate::protocol::ServerMsg::Ok { .. } => continue,
                    other => bail!("unexpected: {other:?}"),
                }
            }
        }
        "info" | "status" => {
            ensure_daemon()?;
            let (mut r, mut w) = daemon::connect_stream(cfg!(windows))?;
            let _ = daemon::read_msg(&mut r)?;
            daemon::write_msg(&mut w, &ClientMsg::DaemonInfo)?;
            loop {
                match daemon::read_msg(&mut r)? {
                    crate::protocol::ServerMsg::DaemonInfo {
                        live_sessions,
                        max_sessions,
                        clients,
                        socket,
                        rss_kb,
                        sessions_bytes,
                        pid,
                    } => {
                        println!("live_sessions={live_sessions}/{max_sessions}");
                        println!("clients={clients}");
                        println!("socket={socket}");
                        println!("pid={pid}");
                        if rss_kb > 0 {
                            println!(
                                "daemon_rss={}",
                                crate::mem_stats::format_kb(rss_kb)
                            );
                        }
                        if sessions_bytes > 0 {
                            println!(
                                "sessions_approx={}",
                                crate::mem_stats::format_bytes(sessions_bytes as usize)
                            );
                        }
                        return Ok(());
                    }
                    crate::protocol::ServerMsg::Error { message } => bail!(message),
                    crate::protocol::ServerMsg::Hello { .. }
                    | crate::protocol::ServerMsg::Ok { .. } => continue,
                    other => bail!("unexpected: {other:?}"),
                }
            }
        }
        "create" | "new" => {
            ensure_daemon()?;
            let title = args.get(1).cloned();
            let mode = if args.iter().any(|a| a == "--office") {
                "office"
            } else {
                "code"
            };
            let (mut r, mut w) = daemon::connect_stream(cfg!(windows))?;
            let _ = daemon::read_msg(&mut r)?;
            daemon::write_msg(
                &mut w,
                &ClientMsg::CreateSession {
                    mode: mode.into(),
                    title,
                    chat_dir: None,
                    session_id: None,
                },
            )?;
            loop {
                match daemon::read_msg(&mut r)? {
                    crate::protocol::ServerMsg::SessionCreated {
                        session_id,
                        chat_dir,
                        title,
                    } => {
                        println!("session_id={session_id}");
                        println!("title={title}");
                        println!("chat_dir={chat_dir}");
                        return Ok(());
                    }
                    crate::protocol::ServerMsg::Error { message } => bail!(message),
                    crate::protocol::ServerMsg::Hello { .. }
                    | crate::protocol::ServerMsg::Ok { .. } => continue,
                    other => bail!("unexpected: {other:?}"),
                }
            }
        }
        "attach" => {
            let id = args
                .get(1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("usage: harness session attach <id>"))?;
            cli_attach_session(&id)?;
            Ok(())
        }
        "kill" | "rm" => {
            let id = args
                .get(1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("usage: harness session kill <id> [--delete]"))?;
            let delete_disk = args.iter().any(|a| a == "--delete" || a == "--disk");
            ensure_daemon()?;
            let (mut r, mut w) = daemon::connect_stream(cfg!(windows))?;
            let _ = daemon::read_msg(&mut r)?;
            // resolve short id
            let full = resolve_session_id(&mut r, &mut w, &id)?;
            daemon::write_msg(
                &mut w,
                &ClientMsg::KillSession {
                    session_id: full.clone(),
                    delete_disk,
                },
            )?;
            loop {
                match daemon::read_msg(&mut r)? {
                    crate::protocol::ServerMsg::SessionKilled { session_id } => {
                        println!("killed {session_id}");
                        return Ok(());
                    }
                    crate::protocol::ServerMsg::Ok { message } => {
                        println!("{message}");
                        return Ok(());
                    }
                    crate::protocol::ServerMsg::Error { message } => bail!(message),
                    crate::protocol::ServerMsg::Hello { .. } => continue,
                    other => bail!("unexpected: {other:?}"),
                }
            }
        }
        "help" | "-h" => {
            println!(
                "harness session list|info|create|attach <id>|kill <id> [--delete]"
            );
            Ok(())
        }
        other => bail!("unknown session subcommand '{other}' — try: list|info|create|attach|kill"),
    }
}

fn resolve_session_id(
    r: &mut dyn BufRead,
    w: &mut dyn Write,
    id_or_prefix: &str,
) -> Result<String> {
    if id_or_prefix.len() >= 32 || id_or_prefix.contains('-') && id_or_prefix.len() > 20 {
        return Ok(id_or_prefix.to_string());
    }
    daemon::write_msg(w, &ClientMsg::ListSessions)?;
    loop {
        match daemon::read_msg(r)? {
            crate::protocol::ServerMsg::SessionList { sessions } => {
                let matches: Vec<_> = sessions
                    .into_iter()
                    .filter(|s| s.id.starts_with(id_or_prefix) || s.short_id == id_or_prefix)
                    .collect();
                if matches.is_empty() {
                    bail!("no session matching '{id_or_prefix}'");
                }
                if matches.len() > 1 {
                    bail!(
                        "ambiguous prefix '{id_or_prefix}' — matches {}",
                        matches
                            .iter()
                            .map(|s| s.short_id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                return Ok(matches[0].id.clone());
            }
            crate::protocol::ServerMsg::Error { message } => bail!(message),
            crate::protocol::ServerMsg::Hello { .. }
            | crate::protocol::ServerMsg::Ok { .. } => continue,
            other => bail!("unexpected: {other:?}"),
        }
    }
}

fn cli_attach_session(id_or_prefix: &str) -> Result<()> {
    ensure_daemon()?;
    let (mut r, mut w) = daemon::connect_stream(cfg!(windows))?;
    let _ = daemon::read_msg(&mut r)?;
    let session_id = resolve_session_id(&mut r, &mut w, id_or_prefix)?;
    daemon::write_msg(
        &mut w,
        &ClientMsg::Attach {
            session_id: session_id.clone(),
        },
    )?;
    // drain attach
    loop {
        match daemon::read_msg(&mut r)? {
            crate::protocol::ServerMsg::Event { event, .. } if event == "attached" => break,
            crate::protocol::ServerMsg::Ok { message } if message.starts_with("attached") => {
                break;
            }
            crate::protocol::ServerMsg::Error { message } => bail!(message),
            _ => continue,
        }
    }
    println!("attached {session_id}");
    println!("Type messages; /quit to exit; /detach to leave session running");
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let t = line.trim();
        if t == "/quit" || t == "/exit" {
            break;
        }
        if t == "/detach" {
            daemon::write_msg(
                &mut w,
                &ClientMsg::Detach {
                    session_id: session_id.clone(),
                },
            )?;
            println!("detached — session keeps running on daemon");
            break;
        }
        if t.is_empty() {
            continue;
        }
        daemon::write_msg(
            &mut w,
            &ClientMsg::UserMessage {
                session_id: session_id.clone(),
                text: line,
                token_less: None,
                gauntlet: None,
                effort: None,
            },
        )?;
        loop {
            let msg = daemon::read_msg(&mut r)?;
            match msg {
                crate::protocol::ServerMsg::Event {
                    event, payload, ..
                } => match event.as_str() {
                    "stream" => {
                        if let Some(txt) = payload.get("text").and_then(|v| v.as_str()) {
                            print!("{txt}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                    "done" => {
                        println!();
                        if let Some(txt) = payload.get("reply").and_then(|v| v.as_str()) {
                            if !txt.is_empty() {
                                println!("{txt}");
                            }
                        }
                        break;
                    }
                    "error" => {
                        eprintln!(
                            "error: {}",
                            payload.get("message").and_then(|v| v.as_str()).unwrap_or("?")
                        );
                        break;
                    }
                    "need_approval" => {
                        eprint!("Allow tool? [Y/n] ");
                        let _ = std::io::stdout().flush();
                        let mut ans = String::new();
                        let _ = stdin.lock().read_line(&mut ans);
                        let allow = !ans.trim().eq_ignore_ascii_case("n");
                        daemon::write_msg(
                            &mut w,
                            &ClientMsg::Approval {
                                session_id: session_id.clone(),
                                allow,
                                always_shell: false,
                            },
                        )?;
                    }
                    "tool_start" => {
                        eprintln!(
                            "\n▶ {}",
                            payload.get("name").and_then(|v| v.as_str()).unwrap_or("?")
                        );
                    }
                    _ => {}
                },
                crate::protocol::ServerMsg::Ok { .. } => {}
                crate::protocol::ServerMsg::Error { message } => {
                    eprintln!("error: {message}");
                    break;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn ensure_daemon() -> Result<()> {
    if daemon_alive() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve");
    #[cfg(windows)]
    cmd.arg("--tcp");
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
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if daemon_alive() {
            return Ok(());
        }
    }
    bail!("daemon did not start in time")
}

fn daemon_alive() -> bool {
    daemon::connect_stream(cfg!(windows)).is_ok()
}

fn cli_run(prompt: &str, tcp: bool) -> Result<()> {
    let _ = tcp;
    ensure_daemon()?;
    let (mut r, mut w) = daemon::connect_stream(cfg!(windows))?;
    let _hello = daemon::read_msg(&mut r)?;
    daemon::write_msg(
        &mut w,
        &ClientMsg::CreateSession {
            mode: "code".into(),
            title: Some("cli-run".into()),
            chat_dir: None,
            session_id: None,
        },
    )?;
    let session_id = loop {
        match daemon::read_msg(&mut r)? {
            crate::protocol::ServerMsg::SessionCreated { session_id, .. } => break session_id,
            crate::protocol::ServerMsg::Error { message } => bail!(message),
            crate::protocol::ServerMsg::Hello { .. }
            | crate::protocol::ServerMsg::Ok { .. } => continue,
            other => bail!("unexpected: {other:?}"),
        }
    };
    daemon::write_msg(
        &mut w,
        &ClientMsg::UserMessage {
            session_id: session_id.clone(),
            text: prompt.into(),
            token_less: None,
            gauntlet: None,
            effort: None,
        },
    )?;
    loop {
        let msg = daemon::read_msg(&mut r)?;
        match msg {
            crate::protocol::ServerMsg::Event {
                event, payload, ..
            } => {
                match event.as_str() {
                    "stream" => {
                        if let Some(t) = payload.get("text").and_then(|v| v.as_str()) {
                            print!("{t}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                    "tool_start" => {
                        eprintln!(
                            "\n▶ {} {}",
                            payload.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                            payload.get("args").and_then(|v| v.as_str()).unwrap_or("")
                        );
                    }
                    "tool_result" => {
                        eprintln!(
                            "✓ {}",
                            payload
                                .get("result")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .chars()
                                .take(200)
                                .collect::<String>()
                        );
                    }
                    "done" => {
                        if let Some(t) = payload.get("reply").and_then(|v| v.as_str()) {
                            if !t.is_empty() {
                                println!("\n{t}");
                            } else {
                                println!();
                            }
                        }
                        break;
                    }
                    "error" => {
                        bail!(
                            "{}",
                            payload
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("error")
                        );
                    }
                    "cancelled" => {
                        eprintln!("cancelled");
                        break;
                    }
                    "need_approval" => {
                        // auto-allow in CLI for non-interactive run
                        daemon::write_msg(
                            &mut w,
                            &ClientMsg::Approval {
                                session_id: session_id.clone(),
                                allow: true,
                                always_shell: true,
                            },
                        )?;
                    }
                    _ => {}
                }
            }
            crate::protocol::ServerMsg::Ok { .. } => {}
            crate::protocol::ServerMsg::Error { message } => bail!(message),
            _ => {}
        }
    }
    Ok(())
}

fn cli_connect_repl() -> Result<()> {
    ensure_daemon()?;
    let (mut r, mut w) = daemon::connect_stream(cfg!(windows))?;
    let _ = daemon::read_msg(&mut r)?;
    daemon::write_msg(
        &mut w,
        &ClientMsg::CreateSession {
            mode: "code".into(),
            title: Some("cli-repl".into()),
            chat_dir: None,
            session_id: None,
        },
    )?;
    let session_id = loop {
        match daemon::read_msg(&mut r)? {
            crate::protocol::ServerMsg::SessionCreated {
                session_id,
                chat_dir,
                ..
            } => {
                println!("session {session_id}");
                println!("chat_dir {chat_dir}");
                break session_id;
            }
            crate::protocol::ServerMsg::Error { message } => bail!(message),
            crate::protocol::ServerMsg::Hello { .. }
            | crate::protocol::ServerMsg::Ok { .. } => continue,
            other => bail!("unexpected {other:?}"),
        }
    };
    println!("Type messages; /quit to exit");
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim() == "/quit" || line.trim() == "/exit" {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        daemon::write_msg(
            &mut w,
            &ClientMsg::UserMessage {
                session_id: session_id.clone(),
                text: line,
                token_less: None,
                gauntlet: None,
                effort: None,
            },
        )?;
        // drain until done
        loop {
            let msg = daemon::read_msg(&mut r)?;
            match msg {
                crate::protocol::ServerMsg::Event {
                    event, payload, ..
                } => match event.as_str() {
                    "stream" => {
                        if let Some(t) = payload.get("text").and_then(|v| v.as_str()) {
                            print!("{t}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                    "done" => {
                        println!();
                        if let Some(t) = payload.get("reply").and_then(|v| v.as_str()) {
                            if !t.is_empty() {
                                println!("{t}");
                            }
                        }
                        break;
                    }
                    "error" => {
                        eprintln!(
                            "error: {}",
                            payload.get("message").and_then(|v| v.as_str()).unwrap_or("?")
                        );
                        break;
                    }
                    "need_approval" => {
                        eprint!("Allow tool? [Y/n] ");
                        let _ = std::io::stdout().flush();
                        let mut ans = String::new();
                        let _ = stdin.lock().read_line(&mut ans);
                        let allow = !ans.trim().eq_ignore_ascii_case("n");
                        daemon::write_msg(
                            &mut w,
                            &ClientMsg::Approval {
                                session_id: session_id.clone(),
                                allow,
                                always_shell: false,
                            },
                        )?;
                    }
                    "tool_start" => eprintln!(
                        "▶ {}",
                        payload.get("name").and_then(|v| v.as_str()).unwrap_or("?")
                    ),
                    _ => {}
                },
                crate::protocol::ServerMsg::Ok { .. } => {}
                _ => {}
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub fn workspace_flag(args: &[String]) -> Option<PathBuf> {
    args.windows(2).find_map(|w| {
        if w[0] == "--workspace" {
            Some(PathBuf::from(&w[1]))
        } else {
            None
        }
    })
}
