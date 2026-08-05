//! Browser helpers: internal harness WebView (preferred) + text fetch preview.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserState {
    pub url: String,
    pub title: String,
    pub preview_text: String,
    pub status_code: u16,
    pub history: Vec<String>,
    pub last_error: String,
}

impl Default for BrowserState {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8765/".into(),
            title: String::new(),
            preview_text: String::new(),
            status_code: 0,
            history: Vec::new(),
            last_error: String::new(),
        }
    }
}

pub static BROWSER: Mutex<BrowserState> = Mutex::new(BrowserState {
    url: String::new(),
    title: String::new(),
    preview_text: String::new(),
    status_code: 0,
    history: Vec::new(),
    last_error: String::new(),
});

pub fn get() -> BrowserState {
    BROWSER.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_url(url: &str) {
    if let Ok(mut g) = BROWSER.lock() {
        g.url = url.to_string();
    }
}

/// Open URL inside harness (native WebView window). Preferred for web projects.
pub fn open_in_app(url: &str) -> Result<()> {
    let url = url.trim();
    if url.is_empty() {
        anyhow::bail!("empty url");
    }
    crate::webview_win::open_in_app(url)?;
    if let Ok(mut g) = BROWSER.lock() {
        push_history(&mut g, url);
        g.url = url.to_string();
        g.last_error.clear();
    }
    Ok(())
}

/// Fallback: system browser (only if explicitly requested).
pub fn open_external(url: &str) -> Result<()> {
    let url = url.trim();
    if url.is_empty() {
        anyhow::bail!("empty url");
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .context("open")?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .context("start")?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .context("xdg-open")?;
    }
    if let Ok(mut g) = BROWSER.lock() {
        g.url = url.to_string();
        push_history(&mut g, url);
        g.last_error.clear();
    }
    Ok(())
}

pub fn fetch_preview(url: &str) -> Result<BrowserState> {
    let url = url.trim();
    if url.is_empty() {
        anyhow::bail!("empty url");
    }
    let client = crate::llm::http_client();
    let resp = client
        .get(url)
        .header("User-Agent", "harness-browser/0.3")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .context("fetch")?;
    let status = resp.status().as_u16();
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().unwrap_or_default();
    let take = bytes.len().min(256 * 1024);
    let body = String::from_utf8_lossy(&bytes[..take]);

    let title = extract_title(&body).unwrap_or_else(|| url.to_string());
    let preview = if ctype.contains("html") || body.trim_start().starts_with('<') {
        html_to_text(&body)
    } else {
        body.chars().take(12_000).collect()
    };

    let st = BrowserState {
        url: url.to_string(),
        title,
        preview_text: preview.chars().take(16_000).collect(),
        status_code: status,
        history: Vec::new(),
        last_error: String::new(),
    };
    if let Ok(mut g) = BROWSER.lock() {
        push_history(&mut g, url);
        g.url = st.url.clone();
        g.title = st.title.clone();
        g.preview_text = st.preview_text.clone();
        g.status_code = status;
        g.last_error.clear();
        return Ok(g.clone());
    }
    Ok(st)
}

fn push_history(g: &mut BrowserState, url: &str) {
    if g.history.last().map(|s| s.as_str()) != Some(url) {
        g.history.push(url.to_string());
    }
    if g.history.len() > 30 {
        let drain = g.history.len() - 25;
        g.history.drain(0..drain);
    }
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after = html[start..].find('>')? + start + 1;
    let end_rel = lower[after..].find("</title>")?;
    let t = html[after..after + end_rel].trim();
    if t.is_empty() {
        None
    } else {
        Some(html_entities(t).chars().take(120).collect())
    }
}

fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut in_script = false;
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !in_tag && bytes[i..].len() >= 7 {
            let slice = html[i..].to_ascii_lowercase();
            if slice.starts_with("<script") {
                in_script = true;
            } else if slice.starts_with("</script") {
                in_script = false;
            } else if slice.starts_with("<style") {
                in_script = true;
            } else if slice.starts_with("</style") {
                in_script = false;
            }
        }
        let c = bytes[i] as char;
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !in_script {
                    out.push(' ');
                }
            }
            _ if !in_tag && !in_script => out.push(c),
            _ => {}
        }
        i += 1;
        if out.len() > 20_000 {
            break;
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    html_entities(&collapsed)
}

fn html_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}
