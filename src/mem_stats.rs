//! Process RSS + per-session approximate memory accounting.

use crate::llm::ChatMessage;
use crate::session::{Session, UiLogLine};

/// Resident set size of a process in KiB, if available.
pub fn process_rss_kb(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    // Portable-ish: `ps -o rss=` prints KiB on macOS and Linux.
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let n: u64 = s.trim().parse().ok()?;
    if n == 0 {
        None
    } else {
        Some(n)
    }
}

pub fn self_rss_kb() -> Option<u64> {
    process_rss_kb(std::process::id())
}

/// Rough heap bytes attributed to one session (history + UI log + overhead).
pub fn estimate_session_bytes(
    session: &Session,
    history: &[ChatMessage],
    ui_messages_role_text: impl Iterator<Item = (usize, usize)>,
) -> usize {
    let mut n: usize = 8 * 1024; // base struct / maps / bookkeeping
    n += session.meta.title.len()
        + session.meta.chat_dir.len()
        + session.meta.workspace.len()
        + session.meta.id.len()
        + session.meta.chat_folder_name.len();
    for m in history {
        n += chat_message_bytes(m);
    }
    for m in &session.messages {
        n += chat_message_bytes(m);
    }
    for m in &session.ui_log {
        n += ui_log_bytes(m);
    }
    for (role_len, text_len) in ui_messages_role_text {
        n += role_len + text_len + 64;
    }
    n
}

fn chat_message_bytes(m: &ChatMessage) -> usize {
    let mut n = m.role.len() + 48;
    if let Some(c) = &m.content {
        n += c.len();
    }
    if let Some(id) = &m.tool_call_id {
        n += id.len();
    }
    if let Some(name) = &m.name {
        n += name.len();
    }
    if let Some(calls) = &m.tool_calls {
        for c in calls {
            n += c.id.len() + c.function.name.len() + c.function.arguments.len() + 64;
        }
    }
    n
}

fn ui_log_bytes(m: &UiLogLine) -> usize {
    m.role.len() + m.text.len() + 32
}

/// Estimate from daemon live history only (no UI).
pub fn estimate_history_bytes(history: &[ChatMessage]) -> usize {
    let mut n = 4 * 1024;
    for m in history {
        n += chat_message_bytes(m);
    }
    n
}

pub fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn format_kb(kb: u64) -> String {
    if kb < 1024 {
        format!("{kb} KB")
    } else {
        format!("{:.1} MB", kb as f64 / 1024.0)
    }
}

/// Human line for GUI / CLI.
pub fn summary_line(
    gui_rss_kb: Option<u64>,
    daemon_rss_kb: Option<u64>,
    tabs_bytes: usize,
    live: usize,
    max: usize,
) -> String {
    let gui = gui_rss_kb
        .map(format_kb)
        .unwrap_or_else(|| "?".into());
    let daemon = daemon_rss_kb
        .map(format_kb)
        .unwrap_or_else(|| "?".into());
    format!(
        "GUI {gui} · daemon {daemon} · tabs ~{} · live {live}/{max}",
        format_bytes(tabs_bytes)
    )
}
