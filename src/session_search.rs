//! Search across past chat sessions (lightweight RAG over transcripts).

use anyhow::Result;
use std::fs;

use crate::session::{self, Session};

#[derive(Debug, Clone)]
pub struct Hit {
    pub session_id: String,
    pub title: String,
    pub folder: String,
    pub snippet: String,
    pub score: i32,
}

pub fn search(query: &str, limit: usize) -> Result<Vec<Hit>> {
    let q = query.to_ascii_lowercase();
    let terms: Vec<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let dir = session::sessions_dir()?;
    let mut hits = Vec::new();
    let rd = fs::read_dir(dir)?;
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(sess) = serde_json::from_str::<Session>(&raw) else {
            continue;
        };
        let mut score = 0i32;
        let mut snippet = String::new();
        let title_l = sess.meta.title.to_ascii_lowercase();
        for t in &terms {
            if title_l.contains(t) {
                score += 3;
            }
        }
        for msg in &sess.ui_log {
            let body = msg.text.to_ascii_lowercase();
            let mut local = 0;
            for t in &terms {
                if body.contains(t) {
                    local += 1;
                }
            }
            if local > 0 {
                score += local;
                if snippet.is_empty() {
                    snippet = msg.text.chars().take(200).collect();
                }
            }
        }
        for m in &sess.messages {
            if let Some(c) = &m.content {
                let body = c.to_ascii_lowercase();
                let mut local = 0;
                for t in &terms {
                    if body.contains(t) {
                        local += 1;
                    }
                }
                if local > 0 {
                    score += local;
                    if snippet.is_empty() {
                        snippet = c.chars().take(200).collect();
                    }
                }
            }
        }
        if score > 0 {
            hits.push(Hit {
                session_id: sess.meta.id,
                title: sess.meta.title,
                folder: sess.meta.chat_folder_name,
                snippet,
                score,
            });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score));
    hits.truncate(limit.max(1).min(30));
    Ok(hits)
}

pub fn format_hits(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return "(no session matches)".into();
    }
    hits.iter()
        .map(|h| {
            format!(
                "[{}] {} · {} (score {})\n  {}",
                &h.session_id[..8.min(h.session_id.len())],
                h.folder,
                h.title,
                h.score,
                h.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
