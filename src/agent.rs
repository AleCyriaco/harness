use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::llm::{self, ChatMessage};
use crate::modes::AppMode;
use crate::tools;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Status(String),
    StreamDelta(String),
    ToolStart { name: String, args: String },
    ToolResult { name: String, result: String },
    /// Shell / write tools may wait for user decision.
    NeedApproval {
        name: String,
        args_preview: String,
    },
    Done { reply: String },
    Error(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Allow,
    Deny,
    AllowAlwaysShell,
}

const MAX_TOOL_ROUNDS_CODE: usize = 20;
const MAX_TOOL_ROUNDS_OFFICE: usize = 10;

pub struct AgentHandle {
    pub event_rx: Receiver<AgentEvent>,
    pub approval_tx: Sender<ApprovalDecision>,
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn_turn(
    cfg: Config,
    mode: AppMode,
    history: Vec<ChatMessage>,
) -> AgentHandle {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    let cancel = llm::new_cancel();
    let cancel_thread = Arc::clone(&cancel);

    std::thread::Builder::new()
        .name("harness-agent".into())
        .spawn(move || {
            if let Err(e) =
                run_turn_inner(cfg, mode, history, &event_tx, &approval_rx, &cancel_thread)
            {
                let msg = e.to_string();
                if msg.contains("cancelled") {
                    let _ = event_tx.send(AgentEvent::Cancelled);
                } else {
                    let _ = event_tx.send(AgentEvent::Error(msg));
                }
            }
        })
        .expect("spawn agent");

    AgentHandle {
        event_rx,
        approval_tx,
        cancel,
    }
}

fn run_turn_inner(
    mut cfg: Config,
    mode: AppMode,
    mut history: Vec<ChatMessage>,
    tx: &Sender<AgentEvent>,
    approval_rx: &Receiver<ApprovalDecision>,
    cancel: &AtomicBool,
) -> Result<()> {
    // Weighted multi-provider rotation (Code/Office separate pools)
    if let Some(note) = crate::llm_pool::maybe_rotate(&mut cfg, mode) {
        let _ = tx.send(AgentEvent::Status(note));
    }
    let tools = tools::tool_schemas(mode);
    let max_rounds = match mode {
        AppMode::Code => MAX_TOOL_ROUNDS_CODE,
        AppMode::Office => MAX_TOOL_ROUNDS_OFFICE,
    };

    let _ = tx.send(AgentEvent::Status(format!(
        "{} · {} · thinking…",
        mode.label(),
        cfg.model
    )));

    let mut sys_content = llm::system_prompt(mode, &cfg.workspace.display().to_string());
    crate::tokenless::apply_to_system(&mut sys_content, cfg.token_less);
    crate::metrics::set_current_level(cfg.token_less);
    let sys = ChatMessage {
        role: "system".into(),
        content: Some(sys_content),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    };
    if history.first().map(|m| m.role == "system").unwrap_or(false) {
        history[0] = sys;
    } else {
        history.insert(0, sys);
    }

    history = llm::compact_history(&history, cfg.history_cap, cfg.tool_result_cap);

    let user_q = history
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.clone())
        .unwrap_or_default();

    // pre_turn hook
    if let Some(out) = crate::hooks::run_hook(&cfg.workspace, "pre_turn", &user_q) {
        if let Some(sys) = history.first_mut() {
            if let Some(ref mut c) = sys.content {
                c.push_str("\n\nHook pre_turn:\n");
                c.push_str(&out);
            }
        }
    }

    // Auto-recall vector memories + skill hints
    if cfg.memory_auto_recall && !user_q.is_empty() {
        let recall = crate::memory::recall_for_prompt(&user_q, 5);
        if !recall.is_empty() {
            if let Some(sys) = history.first_mut() {
                if let Some(ref mut c) = sys.content {
                    c.push_str("\n\n");
                    c.push_str(&recall);
                }
            }
            let _ = tx.send(AgentEvent::Status("memory recall…".into()));
        }
        let skills = crate::skills::match_skills(&cfg.workspace, &user_q, 2);
        if !skills.is_empty() {
            if let Some(sys) = history.first_mut() {
                if let Some(ref mut c) = sys.content {
                    c.push_str("\n\nMatched skills (use skill_load if needed):\n");
                    c.push_str(&crate::skills::format_skills(&skills));
                }
            }
        }
    }

    // File-watch context for swarm + peer edit notices
    let file_events = crate::swarm::global_swarm()
        .lock()
        .map(|g| g.file_events_tail(6).join("\n"))
        .unwrap_or_default();
    if !file_events.is_empty() {
        if let Some(sys) = history.first_mut() {
            if let Some(ref mut c) = sys.content {
                c.push_str("\n\nRecent file events (swarm):\n");
                c.push_str(&file_events);
            }
        }
    }
    let notices = crate::file_watch::drain_notices_for("main");
    let notice_txt = crate::file_watch::format_notices(&notices);
    if !notice_txt.is_empty() {
        if let Some(sys) = history.first_mut() {
            if let Some(ref mut c) = sys.content {
                c.push_str("\n\n");
                c.push_str(&notice_txt);
            }
        }
    }
    crate::memory_graph::on_user_message(&user_q);
    if let Some(w) = crate::provider_doctor::cache_warning() {
        let _ = tx.send(AgentEvent::Status(w));
    }
    if let Some(ep) = crate::llm_pool::resolve_endpoint(&cfg, None) {
        let _ = tx.send(AgentEvent::Status(format!(
            "llm · {} · {}",
            ep.name, ep.model
        )));
    }
    let note = crate::llm_pool::last_failover_note();
    if !note.is_empty() {
        let _ = tx.send(AgentEvent::Status(note));
    }

    let mut final_text = String::new();
    let mut shell_always = cfg.auto_approve_shell;

    for round in 0..max_rounds {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(AgentEvent::Cancelled);
            return Ok(());
        }
        history = llm::compact_history(&history, cfg.history_cap, cfg.tool_result_cap);
        let _ = tx.send(AgentEvent::Status(format!(
            "{} · round {}…",
            mode.label(),
            round + 1
        )));

        let event_tx = tx.clone();
        let on_delta: llm::StreamCb = Box::new(move |d| {
            let _ = event_tx.send(AgentEvent::StreamDelta(d.to_string()));
        });

        // Multi-LLM + auto-failover (history/memory unchanged across providers)
        let reply = llm::chat(&cfg, &history, &tools, cancel, Some(on_delta))?;
        let fo = crate::llm_pool::last_failover_note();
        if fo.contains("failover OK") {
            let _ = tx.send(AgentEvent::Status(fo));
        }
        let msg = reply.message;

        if let Some(calls) = msg.tool_calls.clone() {
            if !calls.is_empty() {
                history.push(ChatMessage {
                    role: "assistant".into(),
                    content: msg.content.clone(),
                    tool_calls: Some(calls.clone()),
                    tool_call_id: None,
                    name: None,
                });

                for call in calls {
                    if cancel.load(Ordering::Relaxed) {
                        let _ = tx.send(AgentEvent::Cancelled);
                        return Ok(());
                    }
                    let name = call.function.name.clone();
                    let args = call.function.arguments.clone();
                    let short_args = preview_args(&args, 180);

                    if needs_approval(&cfg, &name, shell_always) {
                        let _ = tx.send(AgentEvent::NeedApproval {
                            name: name.clone(),
                            args_preview: short_args.clone(),
                        });
                        let _ = tx.send(AgentEvent::Status(format!(
                            "awaiting approval: {name}"
                        )));
                        match wait_approval(approval_rx, cancel) {
                            ApprovalWait::Allow => {}
                            ApprovalWait::AllowShellAlways => {
                                shell_always = true;
                            }
                            ApprovalWait::Deny => {
                                history.push(ChatMessage {
                                    role: "tool".into(),
                                    content: Some(
                                        "error: user denied tool execution".into(),
                                    ),
                                    tool_calls: None,
                                    tool_call_id: Some(call.id),
                                    name: Some(name),
                                });
                                continue;
                            }
                            ApprovalWait::Cancelled => {
                                let _ = tx.send(AgentEvent::Cancelled);
                                return Ok(());
                            }
                        }
                    }

                    let _ = tx.send(AgentEvent::ToolStart {
                        name: name.clone(),
                        args: short_args,
                    });
                    let result = match tools::dispatch(&cfg, mode, &name, &args, cancel) {
                        Ok(s) => s,
                        Err(e) => format!("error: {e}"),
                    };
                    if let Some(hook) =
                        crate::hooks::run_hook(&cfg.workspace, "post_tool", &format!("{name}:{args}"))
                    {
                        let _ = tx.send(AgentEvent::Status(format!("hook: {}", hook.chars().take(80).collect::<String>())));
                    }
                    let preview = if result.len() > 360 {
                        format!("{}…", &result[..360])
                    } else {
                        result.clone()
                    };
                    let _ = tx.send(AgentEvent::ToolResult {
                        name: name.clone(),
                        result: preview,
                    });
                    history.push(ChatMessage {
                        role: "tool".into(),
                        content: Some(result),
                        tool_calls: None,
                        tool_call_id: Some(call.id),
                        name: Some(name),
                    });
                }
                continue;
            }
        }

        final_text = msg.content.clone().unwrap_or_default();
        break;
    }

    if final_text.is_empty() {
        final_text = "(done)".into();
    }

    // Auto memory extract + post_turn hook
    let extracted = crate::memory::maybe_extract_from_turn(&user_q, &final_text);
    if extracted > 0 {
        let _ = tx.send(AgentEvent::Status(format!(
            "stored {extracted} auto-memor(ies)"
        )));
    }
    if let Some(out) = crate::hooks::run_hook(&cfg.workspace, "post_turn", &final_text) {
        let _ = tx.send(AgentEvent::Status(format!(
            "hook post_turn: {}",
            out.chars().take(100).collect::<String>()
        )));
    }

    let _ = tx.send(AgentEvent::Done {
        reply: final_text,
    });
    Ok(())
}

enum ApprovalWait {
    Allow,
    AllowShellAlways,
    Deny,
    Cancelled,
}

fn wait_approval(rx: &Receiver<ApprovalDecision>, cancel: &AtomicBool) -> ApprovalWait {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return ApprovalWait::Cancelled;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ApprovalDecision::Allow) => return ApprovalWait::Allow,
            Ok(ApprovalDecision::Deny) => return ApprovalWait::Deny,
            Ok(ApprovalDecision::AllowAlwaysShell) => return ApprovalWait::AllowShellAlways,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return ApprovalWait::Deny,
        }
    }
}

fn needs_approval(cfg: &Config, name: &str, shell_always: bool) -> bool {
    if llm::is_safe_tool(name) && cfg.auto_approve_safe {
        return false;
    }
    if name == "run_command" {
        return !shell_always;
    }
    // writes: auto if safe mode treats them as auto when auto_approve_safe is used only for reads
    // Professional default: auto-approve writes (agent is local), only shell prompts.
    matches!(name, "run_command")
}

fn preview_args(args: &str, max: usize) -> String {
    let compact: String = args.chars().filter(|c| *c != '\n').collect();
    if compact.len() > max {
        format!("{}…", &compact[..max])
    } else {
        compact
    }
}
