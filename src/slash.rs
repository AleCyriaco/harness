//! Slash commands — jcode-style /model /clear /folder etc.

use crate::config::Config;
use crate::modes::AppMode;

#[derive(Debug, Clone)]
pub enum SlashAction {
    NotSlash,
    Help(String),
    ClearChat,
    OpenFolder,
    OpenRoot,
    SetMode(AppMode),
    SetModel(String),
    MemorySearch(String),
    MemoryStore(String),
    Diagnostics,
    /// `/checkpoints` lista os snapshots do chat.
    Checkpoints,
    /// `/rollback [id]` — sem id, desfaz o turno mais recente.
    Rollback(String),
    /// `/schedule 30m <prompt>` · `/schedule` lista · `/schedule off` cancela.
    Schedule { every_secs: u64, prompt: String },
    SwarmList,
    ServerStart { path: String, port: Option<u16> },
    ServerStop,
    WebOpen(String),
    SideClear,
    /// Modo token_less deste chat (None = mostra o estado / ajuda).
    TokenLess(Option<crate::tokenless::TokenLessLevel>),
    /// Grafo estrutural: sem arg mostra estado, `build` reindexa, resto consulta.
    Graph(String),
    /// Renomeia o chat atual.
    Rename(String),
    /// Fixa/desafixa o chat atual no topo da lista.
    Pin(Option<bool>),
    /// Abre a confirmação de apagar o chat atual.
    Delete,
    /// Mostra/esconde o painel de uso.
    ToggleUsage,
    /// Aponta o chat para uma pasta de projeto. Vazio = abre o seletor;
    /// "off" desaponta.
    Project(String),
    Compact,
    Status,
    Unknown(String),
}

pub fn parse(input: &str) -> SlashAction {
    let t = input.trim();
    if !t.starts_with('/') {
        return SlashAction::NotSlash;
    }
    let mut parts = t[1..].splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();

    match cmd.as_str() {
        "help" | "?" => SlashAction::Help(help_text()),
        "clear" => SlashAction::ClearChat,
        "folder" | "open" => SlashAction::OpenFolder,
        "root" => SlashAction::OpenRoot,
        "code" => SlashAction::SetMode(AppMode::Code),
        "office" | "doc" => SlashAction::SetMode(AppMode::Office),
        "model" if !rest.is_empty() => SlashAction::SetModel(rest.to_string()),
        "model" => SlashAction::Help("Usage: /model <name>".into()),
        "mem" | "memory" if !rest.is_empty() => SlashAction::MemorySearch(rest.to_string()),
        "mem" | "memory" => SlashAction::Help("Usage: /mem <query> or /remember <text>".into()),
        "remember" if !rest.is_empty() => SlashAction::MemoryStore(rest.to_string()),
        "remember" => SlashAction::Help("Usage: /remember <text>".into()),
        "diag" | "diagnostics" => SlashAction::Diagnostics,
        "checkpoints" | "checkpoint" => SlashAction::Checkpoints,
        "rollback" | "undo" | "desfazer" => SlashAction::Rollback(rest.to_string()),
        "schedule" | "agendar" => {
            let (first, prompt) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            match first.trim() {
                "" => SlashAction::Schedule { every_secs: 0, prompt: "?".into() },
                "off" | "clear" | "cancelar" => {
                    SlashAction::Schedule { every_secs: 0, prompt: String::new() }
                }
                every => match crate::schedule::parse_every(every) {
                    Some(d) if !prompt.trim().is_empty() => SlashAction::Schedule {
                        every_secs: d.as_secs(),
                        prompt: prompt.trim().to_string(),
                    },
                    Some(_) => SlashAction::Help(
                        "Usage: /schedule <30s|30m|2h|1d> <prompt>".into(),
                    ),
                    None => SlashAction::Help(
                        "Interval must be 30s–30d, e.g. /schedule 2h run the tests".into(),
                    ),
                },
            }
        }
        "swarm" => SlashAction::SwarmList,
        "serve" => {
            let mut sp = rest.split_whitespace();
            let path = sp.next().unwrap_or("web").to_string();
            let port = sp.next().and_then(|p| p.parse().ok());
            SlashAction::ServerStart { path, port }
        }
        "stopserve" | "unserve" => SlashAction::ServerStop,
        "web" if !rest.is_empty() => SlashAction::WebOpen(rest.to_string()),
        "web" => SlashAction::WebOpen("http://127.0.0.1:8765/".into()),
        "side" | "sideclear" => SlashAction::SideClear,
        "graph" | "grafo" => SlashAction::Graph(rest.to_string()),
        "rename" | "renomear" | "title" if !rest.is_empty() => {
            SlashAction::Rename(rest.to_string())
        }
        "rename" | "renomear" | "title" => {
            SlashAction::Help("Usage: /rename <new title>".into())
        }
        "delete" | "apagar" | "excluir" => SlashAction::Delete,
        "project" | "projeto" | "cwd" => SlashAction::Project(rest.to_string()),
        "pin" | "fixar" => SlashAction::Pin(match rest {
            "" => None,
            "off" | "no" | "nao" | "não" => Some(false),
            _ => Some(true),
        }),
        // `caveman` fica como alias: era o nome antigo da feature
        "tokenless" | "tlc" | "token" | "caveman" | "cave" => {
            if rest.is_empty() {
                SlashAction::TokenLess(None)
            } else {
                match crate::tokenless::TokenLessLevel::parse(rest) {
                    Some(l) => SlashAction::TokenLess(Some(l)),
                    None => SlashAction::Help(
                        "Usage: /tokenless off|lite|full|ultra  (applies to this chat only)".into(),
                    ),
                }
            }
        }
        "compact" => SlashAction::Compact,
        "status" => SlashAction::Status,
        "profile" if !rest.is_empty() => SlashAction::SetModel(format!("__profile__:{rest}")),
        "profile" => SlashAction::Help(crate::provider_doctor::list_profiles_text()),
        "usage" | "uso" => SlashAction::ToggleUsage,
        "resume" if !rest.is_empty() => SlashAction::Help(format!(
            "Ask agent: resume_import path={rest}"
        )),
        "ambient" => SlashAction::Help(crate::memory_graph::ambient_status()),
        "sessions" | "session" => SlashAction::Help("__sessions__".into()),
        "llm" if rest.is_empty() || rest == "list" => {
            // filled in handler with live config
            SlashAction::Help("__llm_list__".into())
        }
        "llm" => {
            let mut sp = rest.splitn(2, char::is_whitespace);
            let sub = sp.next().unwrap_or("");
            let arg = sp.next().unwrap_or("").trim();
            match sub {
                "use" | "switch" if !arg.is_empty() => {
                    SlashAction::SetModel(format!("__llm__:{arg}"))
                }
                "failover" => SlashAction::Help("__llm_failover_toggle__".into()),
                "weights" | "rotate" | "pool" => SlashAction::Help("__llm_weights__".into()),
                "rotate_on" => SlashAction::Help("__llm_rotate_on__".into()),
                "rotate_off" => SlashAction::Help("__llm_rotate_off__".into()),
                "every" if !arg.is_empty() => {
                    SlashAction::Help(format!("__llm_rotate_mins__:{arg}"))
                }
                "every" => SlashAction::Help(
                    "Usage: /llm every <minutes>  e.g. /llm every 60".into(),
                ),
                _ => SlashAction::Help(
                    "Usage: /llm list | /llm use <name> | /llm failover | /llm weights\n\
                     /llm rotate_on|rotate_off | /llm every <minutes>\n\
                     Memory is kept when switching."
                        .into(),
                ),
            }
        }
        other => SlashAction::Unknown(other.into()),
    }
}

pub fn help_text() -> String {
    r#"/help — this list
/clear — clear chat (keeps folder)
/folder — open chat folder
/root — open workspace root
/code · /office — switch mode
/model <name> — set model
/profile <name> — grok|openai|openrouter|deepseek|ollama
/llm list | /llm use <name> | /llm failover — multi-LLM (memory kept)
/llm weights | /llm rotate_on|off | /llm every <min> — weighted rotation
/sessions — live multi-session list on daemon
/mem <query> — search memories
/remember <text> — store memory
/schedule [30m <prompt>|off] — repeat a prompt in this chat (daemon keeps it)
/checkpoints — snapshots taken before the agent edited files
/rollback [id] — undo those edits (newest checkpoint when no id)
/diag — run diagnostics
/swarm — list swarm agents
/serve [path] [port] — start static server
/stopserve — stop server
/web [url] — open harness WebView
/side — clear side panel
/tokenless [off|lite|full|ultra] — Token Less Cost: compressed replies in this chat
/graph [build|impact <symbol>|<query>] — structural graph of the workspace
/rename <title> — rename this chat
/pin [off] — pin this chat to the top of the list
/delete — delete this chat (asks first; generated files stay)
/project [path|off] — point this chat at a project folder (no arg opens a picker)
/compact — compact LLM history
/usage — show/hide the usage panel (live tokens, cache, cost)
/ambient — memory ambient status
/status — show config snapshot

CLI: harness serve | run | connect | self-dev | doctor"#
        .into()
}

pub fn status_line(cfg: &Config, chat_dir: &str, mode: AppMode) -> String {
    format!(
        "mode={} model={} base={} chat={} root={} stream={} mem={} usage={}",
        mode.label(),
        cfg.model,
        cfg.api_base,
        chat_dir,
        cfg.workspace.display(),
        cfg.stream,
        cfg.memory_auto_recall,
        crate::provider_doctor::usage_summary()
    )
}
