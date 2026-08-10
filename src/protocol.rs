//! Shared client↔daemon protocol (NDJSON lines) — multi-client multi-session.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        client: String,
        version: String,
    },
    CreateSession {
        mode: String,
        title: Option<String>,
        /// Reuse existing chat folder (reattach after GUI restart).
        #[serde(default)]
        chat_dir: Option<String>,
        /// Prefer restoring this session id from disk if present.
        #[serde(default)]
        session_id: Option<String>,
    },
    Attach {
        session_id: String,
    },
    /// Stop receiving events for a session (session keeps running headless).
    Detach {
        session_id: String,
    },
    /// Cancel turn (if any) and drop live session from daemon memory.
    /// Disk JSON is kept unless `delete_disk` is true.
    KillSession {
        session_id: String,
        #[serde(default)]
        delete_disk: bool,
    },
    ListSessions,
    /// Daemon capacity / live counts.
    DaemonInfo,
    /// Estado vivo do processo do daemon: swarm, métricas e grafo.
    RuntimeInfo,
    /// Para um worker por id/nome, ou "all".
    SwarmStop {
        id: String,
    },
    /// Renomear e/ou fixar um chat. Passa pelo daemon porque a sessão viva
    /// tem a própria cópia do título — mexer só no disco seria sobrescrito.
    UpdateSession {
        session_id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        pinned: Option<bool>,
        /// `Some(None)` desaponta o projeto; ausente não mexe.
        #[serde(default)]
        project_dir: Option<Option<String>>,
    },
    UserMessage {
        session_id: String,
        text: String,
        /// Nível do Token Less Cost ("off"/"lite"/"full"/"ultra").
        /// Ausente = daemon mantém o que já sabia da sessão.
        #[serde(default, alias = "caveman")]
        token_less: Option<String>,
        /// Gauntlet Loop deste chat. Ausente = daemon mantém o que sabia.
        #[serde(default)]
        gauntlet: Option<bool>,
    },
    Cancel {
        session_id: String,
    },
    Approval {
        session_id: String,
        allow: bool,
        always_shell: bool,
    },
    /// Cross-session bus (to = empty or "*" → all live sessions except from).
    Bus {
        from_session: String,
        #[serde(default)]
        to_session: Option<String>,
        body: String,
    },
    Ping,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        server: String,
        version: String,
    },
    SessionCreated {
        session_id: String,
        chat_dir: String,
        title: String,
    },
    SessionList {
        sessions: Vec<SessionSummary>,
    },
    DaemonInfo {
        live_sessions: usize,
        max_sessions: usize,
        clients: usize,
        socket: String,
        /// Daemon process RSS in KiB (0 if unknown).
        #[serde(default)]
        rss_kb: u64,
        /// Sum of approx_bytes across live sessions.
        #[serde(default)]
        sessions_bytes: u64,
        #[serde(default)]
        pid: u32,
    },
    SessionKilled {
        session_id: String,
    },
    RuntimeInfo {
        swarm: crate::swarm::SwarmSnapshot,
        metrics: crate::metrics::Metrics,
    },
    Event {
        session_id: String,
        event: String,
        payload: serde_json::Value,
    },
    Error {
        message: String,
    },
    Pong,
    Ok {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub mode: String,
    pub chat_dir: String,
    pub folder: String,
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub subscribers: usize,
    #[serde(default)]
    pub updated_at: String,
    /// Short id prefix for UI.
    #[serde(default)]
    pub short_id: String,
    /// Approximate memory attributed to this session (history + meta), bytes.
    #[serde(default)]
    pub approx_bytes: u64,
    /// Number of chat messages in live history.
    #[serde(default)]
    pub history_msgs: usize,
    #[serde(default)]
    pub pinned: bool,
}

pub fn default_socket_path() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
            return std::path::PathBuf::from(runtime).join("harness.sock");
        }
        std::env::temp_dir().join(format!("harness-{}.sock", whoami_fallback()))
    }
    #[cfg(not(unix))]
    {
        std::path::PathBuf::from(r"\\.\pipe\harness-agent")
    }
}

fn whoami_fallback() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
}

pub fn default_tcp_addr() -> String {
    std::env::var("HARNESS_DAEMON_ADDR").unwrap_or_else(|_| "127.0.0.1:19876".into())
}

pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
