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

/// Uma requisição: devolve (status, content-type, corpo).
fn fetch_raw(url: &str) -> Result<(u16, String, String)> {
    let client = crate::llm::http_client();
    let resp = client
        .get(url)
        .header("User-Agent", "harness-browser/0.5")
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
    let take = bytes.len().min(512 * 1024);
    Ok((status, ctype, String::from_utf8_lossy(&bytes[..take]).into_owned()))
}

/// `as_markdown` liga o extrator (título/links/código preservados); desligado,
/// volta o texto corrido de antes.
pub fn fetch_preview(url: &str, as_markdown: bool) -> Result<BrowserState> {
    let url = url.trim();
    if url.is_empty() {
        anyhow::bail!("empty url");
    }
    let (status, ctype, body) = fetch_raw(url)?;
    let is_html = ctype.contains("html") || body.trim_start().starts_with('<');

    let (title, preview) = if is_html {
        let page = crate::web_extract::extract(&body, url);
        let title = if page.title.is_empty() {
            url.to_string()
        } else {
            page.title
        };
        if as_markdown {
            (title, page.markdown)
        } else {
            // sem markdown: o mesmo miolo, só que achatado
            let flat = page
                .markdown
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ");
            (title, flat)
        }
    } else {
        (url.to_string(), body.chars().take(12_000).collect())
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

/// Tetos do crawl. Sem eles um site grande vira fatura.
#[derive(Debug, Clone)]
pub struct CrawlOpts {
    pub max_pages: usize,
    pub max_depth: usize,
    pub same_domain: bool,
    pub respect_robots: bool,
    /// Corte por página, em chars.
    pub per_page: usize,
}

/// Percorre a partir de `start` em largura e devolve markdown concatenado.
/// Corte por teto é sempre **dito** no resultado — nada de truncar calado.
pub fn crawl(
    start: &str,
    opts: &CrawlOpts,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String> {
    use std::sync::atomic::Ordering;
    let start = start.trim();
    if start.is_empty() {
        anyhow::bail!("empty url");
    }
    let origin = crate::web_extract::origin_of(start);
    let disallow = if opts.respect_robots {
        fetch_raw(&format!("{origin}/robots.txt"))
            .ok()
            .filter(|(s, _, _)| *s == 200)
            .map(|(_, _, txt)| crate::web_extract::robots_disallow(&txt))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut queue: Vec<(String, usize)> = vec![(start.to_string(), 0)];
    let mut seen: Vec<String> = vec![start.to_string()];
    let mut out = String::new();
    let mut pages = 0usize;
    let mut skipped_robots = 0usize;

    while let Some((url, depth)) = if queue.is_empty() {
        None
    } else {
        Some(queue.remove(0))
    } {
        if cancel.load(Ordering::Relaxed) {
            out.push_str("

_(cancelled)_");
            break;
        }
        if pages >= opts.max_pages {
            out.push_str(&format!(
                "

---
_stopped at max_pages={} — {} more URL(s) were queued._",
                opts.max_pages,
                queue.len() + 1
            ));
            break;
        }
        if !crate::web_extract::robots_allows(&disallow, &url) {
            skipped_robots += 1;
            continue;
        }
        let (status, ctype, body) = match fetch_raw(&url) {
            Ok(v) => v,
            Err(e) => {
                out.push_str(&format!("

---
## {url}
_fetch error: {e}_
"));
                continue;
            }
        };
        if status >= 400 || !(ctype.contains("html") || body.trim_start().starts_with('<')) {
            continue;
        }
        let page = crate::web_extract::extract(&body, &url);
        pages += 1;
        let body_md: String = page.markdown.chars().take(opts.per_page).collect();
        let cut = page.markdown.chars().count() > opts.per_page;
        out.push_str(&format!(
            "

---
## {}
{}

{}{}
",
            if page.title.is_empty() { url.clone() } else { page.title },
            url,
            body_md,
            if cut { "

_(page truncated)_" } else { "" }
        ));

        if depth < opts.max_depth {
            // fronteira vem do documento inteiro; o texto limpo é só o que sai
            for l in crate::web_extract::all_links(&body, &url) {
                if seen.len() >= opts.max_pages * 4 {
                    break;
                }
                if opts.same_domain && !l.starts_with(&origin) {
                    continue;
                }
                if seen.iter().any(|s| s == &l) {
                    continue;
                }
                seen.push(l.clone());
                queue.push((l, depth + 1));
            }
        }
        // civilidade: não martelar o servidor
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    if skipped_robots > 0 {
        out.push_str(&format!(
            "

_{skipped_robots} URL(s) skipped by robots.txt._"
        ));
    }
    Ok(format!("crawled {pages} page(s) from {start}{out}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Página real, para medir o que os testes puros não medem: quanto ruído
    /// sobra e quanto token o markdown economiza.
    /// `cargo test -- --ignored fetch_pagina_real --nocapture`
    #[test]
    #[ignore]
    fn fetch_pagina_real() {
        let url = "https://doc.rust-lang.org/book/ch01-01-installation.html";
        let (_, _, body) = fetch_raw(url).expect("fetch");
        let md = fetch_preview(url, true).expect("preview");
        let flat = fetch_preview(url, false).expect("flat");
        println!(
            "html {} chars → markdown {} chars ({}%) · flat {} chars\ntitle: {}\n--- primeiras linhas ---\n{}",
            body.len(),
            md.preview_text.len(),
            md.preview_text.len() * 100 / body.len().max(1),
            flat.preview_text.len(),
            md.title,
            md.preview_text.lines().take(12).collect::<Vec<_>>().join("\n")
        );
        assert!(md.preview_text.contains('#'), "perdeu os títulos");
        assert!(md.preview_text.len() < body.len() / 3, "não comprimiu");
    }

    /// `cargo test -- --ignored crawl_real --nocapture`
    #[test]
    #[ignore]
    fn crawl_real() {
        let opts = CrawlOpts {
            max_pages: 3,
            max_depth: 1,
            same_domain: true,
            respect_robots: true,
            per_page: 1200,
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let out = crawl(
            "https://doc.rust-lang.org/book/ch01-01-installation.html",
            &opts,
            &cancel,
        )
        .expect("crawl");
        println!("{}", out.chars().take(1400).collect::<String>());
        assert!(out.starts_with("crawled 3 page(s)"), "{}", &out[..60.min(out.len())]);
        assert!(out.contains("stopped at max_pages=3"), "teto tem que ser dito");
    }
}
