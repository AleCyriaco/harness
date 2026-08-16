//! Resume sessions from other harnesses (Claude Code, Codex, OpenCode, pi) — best-effort.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

use crate::llm::ChatMessage;

#[derive(Debug, Clone)]
pub struct ImportedSession {
    pub source: String,
    pub messages: Vec<ChatMessage>,
    pub note: String,
}

pub fn import_path(path: &Path) -> Result<ImportedSession> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if path.extension().and_then(|s| s.to_str()) == Some("jsonl")
        || name.contains("claude")
        || raw.lines().next().map(|l| l.contains("\"type\"")).unwrap_or(false)
    {
        return import_claude_jsonl(&raw);
    }
    if raw.trim_start().starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if v.get("messages").is_some() || v.get("items").is_some() {
                return import_generic_json(&v);
            }
        }
    }
    // plain transcript fallback
    Ok(ImportedSession {
        source: "plain".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(format!("Resumed transcript from {}:\n\n{raw}", path.display())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        }],
        note: "imported as single user blob".into(),
    })
}

fn import_claude_jsonl(raw: &str) -> Result<ImportedSession> {
    let mut messages = Vec::new();
    for line in raw.lines().take(5000) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        // common shapes: type=user/assistant, message.role, content string or array
        let role = v
            .pointer("/message/role")
            .or_else(|| v.get("role"))
            .and_then(|r| r.as_str())
            .or_else(|| {
                v.get("type").and_then(|t| t.as_str()).map(|t| match t {
                    "user" | "human" => "user",
                    "assistant" | "ai" => "assistant",
                    _ => "",
                })
            })
            .unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let content = extract_content(
            v.pointer("/message/content")
                .or_else(|| v.get("content"))
                .or_else(|| v.get("text")),
        );
        if content.trim().is_empty() {
            continue;
        }
        messages.push(ChatMessage {
            role: role.into(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        });
        if messages.len() >= 200 {
            break;
        }
    }
    if messages.is_empty() {
        bail!("no messages parsed from jsonl");
    }
    let n = messages.len();
    Ok(ImportedSession {
        source: "claude-jsonl".into(),
        messages,
        note: format!("imported {n} messages"),
    })
}

fn import_generic_json(v: &serde_json::Value) -> Result<ImportedSession> {
    let arr = v
        .get("messages")
        .or_else(|| v.get("items"))
        .and_then(|a| a.as_array())
        .ok_or_else(|| anyhow::anyhow!("no messages array"))?;
    let mut messages = Vec::new();
    for m in arr.iter().take(200) {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let content = extract_content(m.get("content"));
        if content.is_empty() {
            continue;
        }
        messages.push(ChatMessage {
            role: role.into(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        });
    }
    if messages.is_empty() {
        bail!("empty import");
    }
    let n = messages.len();
    Ok(ImportedSession {
        source: "json".into(),
        messages,
        note: format!("imported {n} messages"),
    })
}

fn extract_content(v: Option<&serde_json::Value>) -> String {
    let Some(v) = v else {
        return String::new();
    };
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(arr) = v.as_array() {
        let mut out = String::new();
        for item in arr {
            if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                out.push_str(t);
                out.push('\n');
            } else if let Some(s) = item.as_str() {
                out.push_str(s);
                out.push('\n');
            }
        }
        return out;
    }
    v.to_string()
}
