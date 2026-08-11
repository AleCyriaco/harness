//! Embedded office previews — text/table extract without external apps.
//! Caps size for low RAM.

use anyhow::{Context, Result, bail};
use calamine::{Reader, Xlsx, open_workbook_from_rs};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use zip::ZipArchive;

const MAX_PREVIEW_CHARS: usize = 48_000;
const MAX_SHEET_ROWS: usize = 80;
const MAX_SHEET_COLS: usize = 20;

#[derive(Debug, Clone)]
pub enum PreviewContent {
    Text {
        title: String,
        body: String,
    },
    Table {
        title: String,
        sheet: String,
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        note: String,
    },
    /// HTML / web project — rendered in harness WebView.
    WebPage {
        title: String,
        path: String,
        url: String,
        source_preview: String,
    },
    Error {
        title: String,
        message: String,
    },
}

/// `open_window = false` só serve a pasta: o painel se prepara sozinho no fim
/// de um turno sem roubar o foco do usuário.
pub fn preview_path(path: &Path, open_window: bool) -> PreviewContent {
    let title = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => preview_html(path, &title, open_window),
        "docx" => match preview_docx(path) {
            Ok(body) => PreviewContent::Text {
                title,
                body: truncate(&body, MAX_PREVIEW_CHARS),
            },
            Err(e) => PreviewContent::Error {
                title,
                message: e.to_string(),
            },
        },
        "xlsx" | "xlsm" => match preview_xlsx(path) {
            Ok((sheet, headers, rows, note)) => PreviewContent::Table {
                title,
                sheet,
                headers,
                rows,
                note,
            },
            Err(e) => PreviewContent::Error {
                title,
                message: e.to_string(),
            },
        },
        "pdf" => match preview_pdf(path) {
            Ok(body) => PreviewContent::Text {
                title,
                body: truncate(&body, MAX_PREVIEW_CHARS),
            },
            Err(e) => PreviewContent::Error {
                title,
                message: e.to_string(),
            },
        },
        "md" | "txt" | "rs" | "py" | "toml" | "json" | "csv" | "css" | "js" | "ts" => {
            match std::fs::read_to_string(path) {
                Ok(s) => PreviewContent::Text {
                    title,
                    body: truncate(&s, MAX_PREVIEW_CHARS),
                },
                Err(e) => PreviewContent::Error {
                    title,
                    message: e.to_string(),
                },
            }
        }
        other => PreviewContent::Error {
            title,
            message: format!("no embedded preview for .{other} — open externally"),
        },
    }
}

/// Serve the HTML's directory (or nearest `web/` parent) and open in harness WebView.
fn preview_html(path: &Path, title: &str, open_window: bool) -> PreviewContent {
    let abs = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    let source_preview = std::fs::read_to_string(&abs)
        .map(|s| truncate(&s, 8_000))
        .unwrap_or_default();

    let served = if open_window {
        open_html_as_web_preview(&abs)
    } else {
        serve_html(&abs)
    };
    match served {
        Ok(url) => PreviewContent::WebPage {
            title: title.to_string(),
            path: abs.display().to_string(),
            url,
            source_preview,
        },
        Err(e) => {
            // Fallback: file:// still opens local HTML
            let url = path_to_file_url(&abs);
            match crate::browser::open_in_app(&url) {
                Ok(()) => PreviewContent::WebPage {
                    title: title.to_string(),
                    path: abs.display().to_string(),
                    url,
                    source_preview,
                },
                Err(e2) => PreviewContent::Error {
                    title: title.to_string(),
                    message: format!("html preview failed: {e}; file url: {e2}"),
                },
            }
        }
    }
}

/// Prefer HTTP via tiny server so relative CSS/JS work; root = parent dir of the html
/// (or the `web/` folder if the file lives under it).
/// Sobe o servidor estático na pasta certa e devolve a URL — **sem** abrir
/// janela. Auto-preview usa esta; clique do usuário usa a versão que abre.
pub fn serve_html(html_path: &Path) -> anyhow::Result<String> {
    let abs = html_path
        .canonicalize()
        .with_context(|| format!("resolve {}", html_path.display()))?;
    let (serve_root, url_path) = resolve_web_serve_root(&abs)?;
    let port = {
        let p = crate::webserver::status().port;
        if p == 0 {
            8765
        } else {
            p
        }
    };
    // (Re)bind server to this project folder so assets resolve.
    let st = crate::webserver::start(serve_root, port)?;
    let rel = url_path.trim_start_matches('/');
    let url = if rel.is_empty() || rel == "index.html" {
        st.url.clone()
    } else {
        format!("{}{rel}", st.url)
    };
    crate::browser::set_url(&url);
    Ok(url)
}

pub fn open_html_as_web_preview(html_path: &Path) -> anyhow::Result<String> {
    let url = serve_html(html_path)?;
    crate::browser::open_in_app(&url)?;
    Ok(url)
}

/// Returns (directory to serve, path under that dir for the HTML file).
fn resolve_web_serve_root(abs_html: &Path) -> anyhow::Result<(std::path::PathBuf, String)> {
    let parent = abs_html
        .parent()
        .context("html has no parent dir")?
        .to_path_buf();

    // Walk up for a folder named "web" — serve that root so /style.css works as web/style.css
    let mut cur = parent.clone();
    let mut rel_parts: Vec<String> = vec![abs_html
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()];
    loop {
        if cur
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("web"))
        {
            let rel = rel_parts
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("/");
            return Ok((cur, rel));
        }
        match cur.parent() {
            Some(p) if p != cur => {
                if let Some(name) = cur.file_name().map(|s| s.to_string_lossy().into_owned()) {
                    rel_parts.push(name);
                }
                cur = p.to_path_buf();
            }
            _ => break,
        }
        // Don't walk past chat folder-ish depth
        if rel_parts.len() > 8 {
            break;
        }
    }

    // Default: serve the directory containing the HTML
    let name = abs_html
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    Ok((parent, name))
}

pub fn path_to_file_url(path: &Path) -> String {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        let s = abs.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            format!("file:///{s}")
        }
    }
    #[cfg(not(windows))]
    {
        format!("file://{}", abs.display())
    }
}

fn preview_docx(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut zip = ZipArchive::new(file).context("docx zip")?;
    let mut doc = zip
        .by_name("word/document.xml")
        .context("missing word/document.xml")?;
    let mut xml = String::new();
    doc.read_to_string(&mut xml)?;
    Ok(extract_xml_text(&xml))
}

/// Pull text nodes; keep paragraph breaks on w:p.
fn extract_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let bytes = xml.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // paragraph boundary
        if bytes[i..].starts_with(b"<w:p") || bytes[i..].starts_with(b"<w:p>") {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
        }
        // <w:t ...>text</w:t>
        if bytes[i..].starts_with(b"<w:t") {
            if let Some(gt) = bytes[i..].iter().position(|&b| b == b'>') {
                let start = i + gt + 1;
                if let Some(end_rel) = bytes[start..].windows(6).position(|w| w == b"</w:t>") {
                    let text = String::from_utf8_lossy(&bytes[start..start + end_rel]);
                    out.push_str(&decode_xml_entities(&text));
                    i = start + end_rel + 6;
                    continue;
                }
            }
        }
        i += 1;
        if out.len() > MAX_PREVIEW_CHARS {
            break;
        }
    }
    out.trim().to_string()
}

fn decode_xml_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn preview_xlsx(path: &Path) -> Result<(String, Vec<String>, Vec<Vec<String>>, String)> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > 12 * 1024 * 1024 {
        bail!("xlsx too large for in-app preview (>12MB)");
    }
    let mut workbook: Xlsx<_> =
        open_workbook_from_rs(Cursor::new(bytes)).context("open xlsx")?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .unwrap_or_else(|| "Sheet1".into());
    let range = workbook
        .worksheet_range(&sheet_name)
        .with_context(|| format!("sheet {sheet_name}"))?;

    let mut headers = Vec::new();
    let mut rows = Vec::new();
    for (r_idx, row) in range.rows().enumerate() {
        if r_idx >= MAX_SHEET_ROWS + 1 {
            break;
        }
        let cells: Vec<String> = row
            .iter()
            .take(MAX_SHEET_COLS)
            .map(|c| format!("{c}"))
            .collect();
        if r_idx == 0 {
            headers = cells;
        } else {
            rows.push(cells);
        }
    }
    let total_rows = range.rows().count().saturating_sub(1);
    let note = if total_rows > MAX_SHEET_ROWS {
        format!("showing {MAX_SHEET_ROWS} of ~{total_rows} rows")
    } else {
        format!("{} rows", rows.len())
    };
    Ok((sheet_name, headers, rows, note))
}

/// Lightweight PDF text pull via stream tokens (no full render).
fn preview_pdf(path: &Path) -> Result<String> {
    let data = std::fs::read(path)?;
    if data.len() > 15 * 1024 * 1024 {
        bail!("pdf too large for in-app preview (>15MB)");
    }
    // Extract (....) Tj and (...)' string literals — good enough for many PDFs.
    let mut out = String::new();
    let mut i = 0;
    let b = &data;
    while i + 1 < b.len() && out.len() < MAX_PREVIEW_CHARS {
        if b[i] == b'(' {
            i += 1;
            let mut s = String::new();
            while i < b.len() {
                match b[i] {
                    b')' => {
                        i += 1;
                        break;
                    }
                    b'\\' if i + 1 < b.len() => {
                        i += 1;
                        match b[i] {
                            b'n' => s.push('\n'),
                            b'r' => s.push('\r'),
                            b't' => s.push('\t'),
                            b'(' | b')' | b'\\' => s.push(b[i] as char),
                            _ => s.push(b[i] as char),
                        }
                        i += 1;
                    }
                    c => {
                        if c.is_ascii_graphic() || c == b' ' {
                            s.push(c as char);
                        }
                        i += 1;
                    }
                }
            }
            if s.len() > 1 {
                if !out.is_empty() && !out.ends_with(|c: char| c.is_whitespace()) {
                    out.push(' ');
                }
                out.push_str(&s);
            }
            continue;
        }
        i += 1;
    }
    let cleaned = out
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.len() < 8 {
        bail!("could not extract text (image-only or compressed streams)");
    }
    Ok(cleaned)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub(20);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…\n[preview truncated]", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Faz o que o botão **Run** faz: serve a pasta e abre a janela.
    /// `cargo test -- --ignored abre_html_como_o_botao_run --nocapture`
    #[test]
    #[ignore]
    fn abre_html_como_o_botao_run() {
        let dir = std::env::temp_dir().join("harness_preview_e2e/web");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("index.html");
        std::fs::write(&file, "<!doctype html><title>e2e</title><h1>HARNESS_PREVIEW_OK</h1>").unwrap();

        match preview_path(&file, true) {
            PreviewContent::WebPage { url, .. } => {
                println!("url = {url}");
                let body = crate::llm::http_client()
                    .get(&url)
                    .send()
                    .expect("servidor no ar")
                    .text()
                    .unwrap_or_default();
                assert!(
                    body.contains("HARNESS_PREVIEW_OK"),
                    "o servidor tem que entregar o arquivo, veio: {}",
                    body.chars().take(200).collect::<String>()
                );
                println!("servidor OK — janela do WebView foi pedida");
            }
            other => panic!("html devia virar WebPage, veio {other:?}"),
        }
        std::thread::sleep(std::time::Duration::from_secs(6));
    }
    use crate::config::Config;
    use crate::modes::AppMode;
    use crate::tools;

    #[test]
    fn docx_preview_roundtrip() {
        let root = std::env::temp_dir().join(format!("h-prev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = Config {
            workspace: root.clone(),
            ..Config::default()
        };
        tools::dispatch_no_cancel(
            &cfg,
            AppMode::Office,
            "create_doc",
            r#"{"path":"p.docx","title":"Hello","paragraphs":["World line"]}"#,
        )
        .unwrap();
        let p = root.join("p.docx");
        match preview_path(&p, true) {
            PreviewContent::Text { body, .. } => {
                assert!(body.to_lowercase().contains("hello") || body.contains("World"));
            }
            other => panic!("unexpected {other:?}"),
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
