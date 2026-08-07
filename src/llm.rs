use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::config::Config;
use crate::modes::AppMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct LlmReply {
    pub message: ChatMessage,
    #[allow(dead_code)]
    pub finish_reason: String,
}

pub fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .pool_max_idle_per_host(1)
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()
            .expect("http client")
    })
}

/// Callback for streamed assistant text deltas (not tool JSON).
pub type StreamCb = Box<dyn FnMut(&str) + Send>;

pub fn chat(
    cfg: &Config,
    messages: &[ChatMessage],
    tools: &[Value],
    cancel: &AtomicBool,
    mut on_delta: Option<StreamCb>,
) -> Result<LlmReply> {
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }

    let order = if cfg.llm_auto_failover {
        crate::llm_pool::failover_order(cfg)
    } else {
        crate::llm_pool::resolve_endpoint(cfg, None)
            .into_iter()
            .collect()
    };

    if order.is_empty() {
        bail!("No LLM configured. Add endpoints in Settings or set API key.");
    }

    let mut last_err = String::new();
    for (i, ep) in order.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let mut try_cfg = cfg.clone();
        ep.apply_to(&mut try_cfg);
        if try_cfg.api_key.trim().is_empty() {
            continue;
        }

        // Responses API tem corpo e eventos próprios; adaptador separado.
        let is_responses = crate::llm_pool::wire_of(&ep.wire, &try_cfg.api_base)
            == crate::llm_pool::Wire::Responses;

        let result = if is_responses {
            crate::llm_responses::chat(&try_cfg, messages, tools, cancel, on_delta.as_mut())
        } else if try_cfg.stream {
            match chat_stream(&try_cfg, messages, tools, cancel, on_delta.as_mut()) {
                Ok(r) => Ok(r),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("cancelled") {
                        return Err(e);
                    }
                    // non-stream fallback for this endpoint
                    chat_blocking(&try_cfg, messages, tools, cancel)
                }
            }
        } else {
            chat_blocking(&try_cfg, messages, tools, cancel)
        };

        match result {
            Ok(r) => {
                if i > 0 {
                    let note = format!(
                        "failover OK → {} ({}) after: {}",
                        ep.name,
                        ep.model,
                        last_err.chars().take(120).collect::<String>()
                    );
                    crate::llm_pool::set_failover_note(&note);
                    crate::llm_pool::set_runtime_active(&ep.name);
                    if cfg.llm_failover_persist {
                        let mut disk = Config::load();
                        disk.active_llm = ep.name.clone();
                        ep.apply_to(&mut disk);
                        let _ = disk.save();
                    }
                }
                return Ok(r);
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("cancelled") {
                    return Err(e);
                }
                last_err = msg.clone();
                if !cfg.llm_auto_failover || !crate::llm_pool::is_failover_error(&msg) {
                    // non-failover error on primary: still try next only if failover on and looks like limit
                    if !cfg.llm_auto_failover {
                        return Err(e);
                    }
                    if !crate::llm_pool::is_failover_error(&msg) && i == 0 {
                        // try next only for failover-class; otherwise fail fast unless multiple and connection error
                        if !(msg.contains("error sending")
                            || msg.contains("connection")
                            || msg.contains("timed out")
                            || msg.contains("dns"))
                        {
                            return Err(e);
                        }
                    }
                }
                crate::llm_pool::set_failover_note(&format!(
                    "tried {} failed: {}",
                    ep.name,
                    msg.chars().take(100).collect::<String>()
                ));
                continue;
            }
        }
    }
    bail!(
        "All LLMs failed. Last error: {last_err}. Chat/memory kept — fix keys or /llm use <name>."
    )
}

// tiny helper removed — use explicit save above

fn chat_blocking(
    cfg: &Config,
    messages: &[ChatMessage],
    tools: &[Value],
    cancel: &AtomicBool,
) -> Result<LlmReply> {
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    let url = format!("{}/chat/completions", cfg.api_base.trim_end_matches('/'));
    let body = completion_body(cfg, messages, tools, false);
    let resp = http_client()
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .context("request to LLM failed")?;

    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("LLM HTTP {status}: {text}");
    }

    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        let pt = v
            .pointer("/usage/prompt_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let ct = v
            .pointer("/usage/completion_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        // Cache de prompt: OpenAI usa prompt_tokens_details.cached_tokens,
        // Anthropic usa cache_read_input_tokens. Fica 0 se o provedor não conta.
        let cached = v
            .pointer("/usage/prompt_tokens_details/cached_tokens")
            .or_else(|| v.pointer("/usage/cache_read_input_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        if pt > 0 || ct > 0 {
            let (pi, po) = crate::llm_pool::active_price(cfg);
            let cost = (pt as f64 / 1e6) * pi + (ct as f64 / 1e6) * po;
            crate::provider_doctor::record_usage(pt, ct);
            crate::metrics::record_call(pt, ct, cached.min(pt), cost);
        }
    }

    #[derive(Deserialize)]
    struct Resp {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: ChatMessage,
        finish_reason: Option<String>,
    }

    let parsed: Resp = serde_json::from_str(&text).with_context(|| format!("bad LLM json: {text}"))?;
    let choice = parsed.choices.into_iter().next().context("empty choices")?;
    Ok(LlmReply {
        finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".into()),
        message: choice.message,
    })
}

fn chat_stream(
    cfg: &Config,
    messages: &[ChatMessage],
    tools: &[Value],
    cancel: &AtomicBool,
    mut on_delta: Option<&mut StreamCb>,
) -> Result<LlmReply> {
    let url = format!("{}/chat/completions", cfg.api_base.trim_end_matches('/'));
    let body = completion_body(cfg, messages, tools, true);
    let resp = http_client()
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .context("stream request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        bail!("LLM stream HTTP {status}: {text}");
    }

    let mut reader = BufReader::new(resp);
    let mut content = String::new();
    // index -> (id, name, arguments)
    let mut tool_acc: Vec<(String, String, String)> = Vec::new();
    let mut finish = String::from("stop");
    let mut line = String::new();
    let mut data_buf = String::new();

    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if data_buf.is_empty() {
                continue;
            }
            let payload = std::mem::take(&mut data_buf);
            if payload == "[DONE]" {
                break;
            }
            if let Ok(v) = serde_json::from_str::<Value>(&payload) {
                apply_stream_chunk(
                    &v,
                    &mut content,
                    &mut tool_acc,
                    &mut finish,
                    on_delta.as_deref_mut(),
                );
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("data:") {
            let rest = rest.trim_start();
            if !data_buf.is_empty() {
                data_buf.push('\n');
            }
            data_buf.push_str(rest);
        }
    }

    // Flush last event without blank line
    if !data_buf.is_empty() && data_buf != "[DONE]" {
        if let Ok(v) = serde_json::from_str::<Value>(&data_buf) {
            apply_stream_chunk(
                &v,
                &mut content,
                &mut tool_acc,
                &mut finish,
                on_delta.as_deref_mut(),
            );
        }
    }

    let tool_calls = if tool_acc.is_empty() {
        None
    } else {
        Some(
            tool_acc
                .into_iter()
                .map(|(id, name, arguments)| ToolCall {
                    id: if id.is_empty() {
                        uuid::Uuid::new_v4().to_string()
                    } else {
                        id
                    },
                    kind: "function".into(),
                    function: FunctionCall { name, arguments },
                })
                .collect(),
        )
    };

    Ok(LlmReply {
        finish_reason: finish,
        message: ChatMessage {
            role: "assistant".into(),
            content: if content.is_empty() {
                None
            } else {
                Some(content)
            },
            tool_calls,
            tool_call_id: None,
            name: None,
        },
    })
}

fn apply_stream_chunk(
    v: &Value,
    content: &mut String,
    tool_acc: &mut Vec<(String, String, String)>,
    finish: &mut String,
    on_delta: Option<&mut StreamCb>,
) {
    let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
        return;
    };
    let Some(choice) = choices.first() else {
        return;
    };
    if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
        if !fr.is_empty() && fr != "null" {
            *finish = fr.to_string();
        }
    }
    let Some(delta) = choice.get("delta") else {
        return;
    };
    if let Some(c) = delta.get("content").and_then(|x| x.as_str()) {
        if !c.is_empty() {
            content.push_str(c);
            if let Some(cb) = on_delta {
                cb(c);
            }
        }
    }
    if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
        for tc in tcs {
            let idx = tc.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            while tool_acc.len() <= idx {
                tool_acc.push((String::new(), String::new(), String::new()));
            }
            if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                if !id.is_empty() {
                    tool_acc[idx].0 = id.to_string();
                }
            }
            if let Some(func) = tc.get("function") {
                if let Some(name) = func.get("name").and_then(|x| x.as_str()) {
                    tool_acc[idx].1.push_str(name);
                }
                if let Some(args) = func.get("arguments").and_then(|x| x.as_str()) {
                    tool_acc[idx].2.push_str(args);
                }
            }
        }
    }
}

fn completion_body(cfg: &Config, messages: &[ChatMessage], tools: &[Value], stream: bool) -> Value {
    if tools.is_empty() {
        json!({
            "model": cfg.model,
            "messages": messages,
            "temperature": 0.15,
            "stream": stream,
        })
    } else {
        json!({
            "model": cfg.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "temperature": 0.15,
            "stream": stream,
        })
    }
}

pub fn system_prompt(mode: AppMode, workspace: &str) -> String {
    match mode {
        AppMode::Code => format!(
            r#"You are harness — a professional, efficient coding agent (desktop).

THIS CHAT FOLDER (write everything here only):
{workspace}

All paths are relative to this chat folder unless absolute.
Preferred layout:
- code/     source code, scripts, projects
- docs/     Word documents (.docx) if any
- sheets/   spreadsheets (.xlsx)
- pdfs/     PDF files
- web/      static web apps for the local server
Do not write outside this chat folder.

Tools:
- workspace_tree, list_dir, glob_files, search
- read_file (prefer start_line/end_line), write_file, replace_in_file, apply_patch
- git_status, git_diff, git_log
- run_command, get_diagnostics (cargo/tsc/py + rust-analyzer when available)
- preview_file (docx/xlsx/pdf/text extract)
- swarm_spawn / swarm_list / swarm_message / swarm_stop (parallel workers)
- memory_store / memory_search / memory_list / memory_delete (local vector SQLite)
- web_server_start / web_server_stop / web_server_status (static server for web apps)
- browser_open / browser_fetch (internal harness WebView + text preview; not external browser)
- side_panel (show_file / note / clear) — live side panel
- plan_add / plan_list / plan_set — session todos
- bg_start / bg_poll / bg_list / bg_kill — background jobs
- skill_list / skill_load — playbooks under .harness/skills/
- session_search — search past chats
- mcp_connect / mcp_list / mcp_call / mcp_disconnect — MCP tools

Professional workflow:
1) Orient with git_status / workspace_tree / search before large reads.
2) Prefer partial reads and replace_in_file / apply_patch over full rewrites.
3) After edits, get_diagnostics or the smallest verify command.
4) For web apps: web_server_start on the app folder, then browser_open the local URL (opens inside harness WebView).
5) Persist lasting facts with memory_store; recall with memory_search.
6) Use swarm_spawn for independent parallel subtasks; synthesize results yourself.
7) Keep chat concise: what changed, paths, how to verify.
8) Never destructive shell. Never exfiltrate secrets.
9) Do not invent file contents you did not read or write.

Efficiency:
- Cap mental context: search + outlines beat dumping whole files.
- Batch related edits; avoid redundant tool calls."#
        ),
        AppMode::Office => format!(
            r#"You are harness in OFFICE mode.

THIS CHAT FOLDER (write everything here only):
{workspace}

Prefer writing under:
- docs/    → .docx  (e.g. docs/report.docx)
- sheets/  → .xlsx  (e.g. sheets/budget.xlsx)
- pdfs/    → .pdf   (e.g. pdfs/status.pdf)
- code/    → scripts if needed
- web/     → static sites
Do not write outside this chat folder.

Tools: list_dir, read_file, write_file, run_command, create_doc, create_sheet, create_pdf, preview_file,
memory_store/search, web_server_*, browser_open/fetch.

After creating a file, call preview_file to verify content. Be concise; report absolute paths. No destructive shell."#
        ),
    }
}

pub fn compact_history(messages: &[ChatMessage], cap: usize, tool_cap: usize) -> Vec<ChatMessage> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<ChatMessage> = messages
        .iter()
        .map(|m| {
            let mut c = m.clone();
            if c.role == "tool" {
                if let Some(ref content) = c.content {
                    c.content = Some(truncate_chars(content, tool_cap));
                }
            } else if c.role == "assistant" {
                if let Some(ref content) = c.content {
                    if content.len() > tool_cap {
                        c.content = Some(truncate_chars(content, tool_cap / 2));
                    }
                }
            }
            c
        })
        .collect();

    let sys = if out.first().map(|m| m.role == "system").unwrap_or(false) {
        Some(out.remove(0))
    } else {
        None
    };

    if out.len() > cap {
        let drop_n = out.len() - cap;
        out.drain(0..drop_n);
        while out
            .first()
            .map(|m| m.role == "tool" || (m.role == "assistant" && m.tool_calls.is_some()))
            .unwrap_or(false)
        {
            out.remove(0);
        }
    }

    if let Some(s) = sys {
        out.insert(0, s);
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub(32);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…\n[truncated {} chars]", &s[..end], s.len() - end)
}

/// Drain residual body on cancel — keeps connection reusable.
#[allow(dead_code)]
fn drain_reader(mut r: impl Read) {
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    while total < 256 * 1024 {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
}

pub fn is_safe_tool(name: &str) -> bool {
    matches!(
        name,
        "list_dir"
            | "read_file"
            | "search"
            | "glob_files"
            | "workspace_tree"
            | "git_status"
            | "git_diff"
            | "git_log"
            | "get_diagnostics"
            | "preview_file"
            | "swarm_list"
            | "memory_search"
            | "memory_list"
            | "web_server_status"
            | "browser_fetch"
            | "plan_list"
            | "skill_list"
            | "session_search"
            | "mcp_list"
            | "bg_list"
            | "bg_poll"
            | "side_panel"
    )
}

pub type CancelFlag = Arc<AtomicBool>;

pub fn new_cancel() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}
