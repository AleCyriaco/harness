//! Native diagnostics: cargo / compilers + optional rust-analyzer LSP.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub severity: String,
    pub message: String,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct DiagnosticsSnapshot {
    pub items: Vec<Diagnostic>,
    pub summary: String,
}

/// Run best-effort native diagnostics for the workspace (no long-lived index).
pub fn run_workspace_diagnostics(root: &Path, path_filter: Option<&str>) -> DiagnosticsSnapshot {
    let mut items = Vec::new();

    // Rust: cargo check JSON
    if root.join("Cargo.toml").exists() {
        items.extend(cargo_check(root));
    }

    // Python: compile selected / all .py (capped)
    items.extend(python_compile(root, path_filter));

    // TypeScript/JS: tsc --noEmit when tsconfig exists
    if root.join("tsconfig.json").exists() {
        items.extend(tsc_check(root));
    }

    // Optional rust-analyzer for a single file (richer), short timeout
    if let Some(p) = path_filter {
        if p.ends_with(".rs") {
            if let Ok(extra) = rust_analyzer_file(root, p) {
                // Prefer RA if it returns anything for that file
                if !extra.is_empty() {
                    items.retain(|d| !d.path.ends_with(p));
                    items.extend(extra);
                }
            }
        }
    }

    if let Some(f) = path_filter {
        items.retain(|d| d.path.contains(f));
    }

    items.truncate(200);
    let errors = items.iter().filter(|d| d.severity == "error").count();
    let warnings = items.iter().filter(|d| d.severity == "warning").count();
    let summary = if items.is_empty() {
        "no diagnostics".into()
    } else {
        format!(
            "{} issue(s): {errors} error(s), {warnings} warning(s)",
            items.len()
        )
    };
    DiagnosticsSnapshot { items, summary }
}

fn cargo_check(root: &Path) -> Vec<Diagnostic> {
    let output = Command::new("cargo")
        .args([
            "check",
            "--message-format=json",
            "--quiet",
            "--all-targets",
        ])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let Some(msg) = v.get("message") else {
            continue;
        };
        let level = msg
            .get("level")
            .and_then(|l| l.as_str())
            .unwrap_or("warning");
        let message = msg
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let spans = msg
            .get("spans")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();
        let primary = spans
            .iter()
            .find(|s| s.get("is_primary").and_then(|p| p.as_bool()) == Some(true))
            .or_else(|| spans.first());
        let (path, line, col) = if let Some(s) = primary {
            (
                s.get("file_name")
                    .and_then(|f| f.as_str())
                    .unwrap_or("")
                    .to_string(),
                s.get("line_start").and_then(|l| l.as_u64()).unwrap_or(1) as u32,
                s.get("column_start").and_then(|c| c.as_u64()).unwrap_or(1) as u32,
            )
        } else {
            (String::new(), 1, 1)
        };
        if message.is_empty() {
            continue;
        }
        out.push(Diagnostic {
            path,
            line,
            col,
            severity: level.to_string(),
            message,
            source: "cargo".into(),
        });
        if out.len() >= 100 {
            break;
        }
    }
    out
}

fn python_compile(root: &Path, path_filter: Option<&str>) -> Vec<Diagnostic> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if matches!(name, ".git" | "venv" | ".venv" | "node_modules" | "target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("py") {
                let rel = p
                    .strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned();
                if let Some(f) = path_filter {
                    if !rel.contains(f) {
                        continue;
                    }
                }
                files.push(p);
                if files.len() >= 40 {
                    break;
                }
            }
        }
        if files.len() >= 40 {
            break;
        }
    }

    let mut out = Vec::new();
    for file in files {
        let output = Command::new("python3")
            .args(["-m", "py_compile"])
            .arg(&file)
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        let Ok(output) = output else {
            continue;
        };
        if output.status.success() {
            continue;
        }
        let err = String::from_utf8_lossy(&output.stderr);
        // File "x.py", line N
        let mut line = 1u32;
        for l in err.lines() {
            if let Some(rest) = l.trim().strip_prefix("File ") {
                if let Some(idx) = rest.rfind("line ") {
                    if let Ok(n) = rest[idx + 5..].trim_matches(|c: char| !c.is_ascii_digit()).parse()
                    {
                        line = n;
                    }
                }
            }
        }
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        out.push(Diagnostic {
            path: rel,
            line,
            col: 1,
            severity: "error".into(),
            message: err.lines().last().unwrap_or("python error").to_string(),
            source: "py_compile".into(),
        });
    }
    out
}

fn tsc_check(root: &Path) -> Vec<Diagnostic> {
    let output = Command::new("npx")
        .args(["--yes", "tsc", "--noEmit", "--pretty", "false"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut out = Vec::new();
    // path(line,col): error TS123: msg
    for line in text.lines() {
        let Some((loc, rest)) = line.split_once(": ") else {
            continue;
        };
        let severity = if rest.contains("error") {
            "error"
        } else if rest.contains("warning") {
            "warning"
        } else {
            continue;
        };
        let (path, line_n, col_n) = parse_ts_loc(loc);
        out.push(Diagnostic {
            path,
            line: line_n,
            col: col_n,
            severity: severity.into(),
            message: rest.to_string(),
            source: "tsc".into(),
        });
        if out.len() >= 80 {
            break;
        }
    }
    out
}

fn parse_ts_loc(loc: &str) -> (String, u32, u32) {
    // file.ts(10,5)
    if let Some((path, rest)) = loc.split_once('(') {
        let rest = rest.trim_end_matches(')');
        let mut parts = rest.split(',');
        let line = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        let col = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        return (path.to_string(), line, col);
    }
    (loc.to_string(), 1, 1)
}

/// Short-lived rust-analyzer session for one file (if installed).
fn rust_analyzer_file(root: &Path, rel: &str) -> Result<Vec<Diagnostic>> {
    let path = root.join(rel);
    if !path.exists() {
        bail!("file missing");
    }
    let text = std::fs::read_to_string(&path)?;
    if text.len() > 512 * 1024 {
        bail!("file too large for RA one-shot");
    }

    let mut child = Command::new("rust-analyzer")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("rust-analyzer not available")?;

    let mut stdin = child.stdin.take().context("stdin")?;
    let stdout = child.stdout.take().context("stdout")?;
    let mut reader = BufReader::new(stdout);

    let root_uri = path_to_uri(root);
    let file_uri = path_to_uri(&path);

    lsp_write(
        &mut stdin,
        1,
        "initialize",
        json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "publishDiagnostics": { "relatedInformation": false }
                }
            }
        }),
    )?;
    let _ = lsp_read_response(&mut reader, Duration::from_secs(8))?;
    lsp_notify(&mut stdin, "initialized", json!({}))?;

    lsp_notify(
        &mut stdin,
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": file_uri,
                "languageId": "rust",
                "version": 1,
                "text": text
            }
        }),
    )?;

    // Collect publishDiagnostics for a short window
    let mut diags = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        match lsp_try_read(&mut reader, Duration::from_millis(400)) {
            Ok(msg) => {
                if msg.get("method").and_then(|m| m.as_str())
                    == Some("textDocument/publishDiagnostics")
                {
                    if let Some(params) = msg.get("params") {
                        diags.extend(parse_publish_diagnostics(params, root));
                    }
                }
            }
            Err(_) => break,
        }
        if !diags.is_empty() {
            // wait a bit more for full set
            std::thread::sleep(Duration::from_millis(200));
            while let Ok(msg) = lsp_try_read(&mut reader, Duration::from_millis(100)) {
                if msg.get("method").and_then(|m| m.as_str())
                    == Some("textDocument/publishDiagnostics")
                {
                    if let Some(params) = msg.get("params") {
                        diags.extend(parse_publish_diagnostics(params, root));
                    }
                }
            }
            break;
        }
    }

    let _ = lsp_write(&mut stdin, 2, "shutdown", json!(null));
    let _ = lsp_notify(&mut stdin, "exit", json!(null));
    let _ = child.kill();
    let _ = child.wait();

    // de-dupe
    diags.sort_by(|a, b| {
        (&a.path, a.line, &a.message).cmp(&(&b.path, b.line, &b.message))
    });
    diags.dedup_by(|a, b| a.path == b.path && a.line == b.line && a.message == b.message);
    Ok(diags)
}

fn parse_publish_diagnostics(params: &Value, root: &Path) -> Vec<Diagnostic> {
    let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
    let path = uri_to_rel(uri, root);
    let arr = params
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for d in arr {
        let severity = match d.get("severity").and_then(|s| s.as_u64()).unwrap_or(2) {
            1 => "error",
            2 => "warning",
            3 => "info",
            _ => "hint",
        };
        let message = d
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let line = d
            .pointer("/range/start/line")
            .and_then(|l| l.as_u64())
            .unwrap_or(0) as u32
            + 1;
        let col = d
            .pointer("/range/start/character")
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32
            + 1;
        out.push(Diagnostic {
            path: path.clone(),
            line,
            col,
            severity: severity.into(),
            message,
            source: "rust-analyzer".into(),
        });
    }
    out
}

fn path_to_uri(path: &Path) -> String {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        let s = p.to_string_lossy().replace('\\', "/");
        format!("file:///{}", s)
    }
    #[cfg(not(windows))]
    {
        format!("file://{}", p.display())
    }
}

fn uri_to_rel(uri: &str, root: &Path) -> String {
    let path = uri
        .strip_prefix("file://")
        .unwrap_or(uri)
        .to_string();
    #[cfg(windows)]
    let path = path.trim_start_matches('/').replace('/', "\\");
    let p = PathBuf::from(&path);
    p.strip_prefix(root)
        .unwrap_or(&p)
        .to_string_lossy()
        .into_owned()
}

fn lsp_write(w: &mut impl Write, id: u64, method: &str, params: Value) -> Result<()> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    })
    .to_string();
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()?;
    Ok(())
}

fn lsp_notify(w: &mut impl Write, method: &str, params: Value) -> Result<()> {
    let body = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    })
    .to_string();
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()?;
    Ok(())
}

fn lsp_read_response(r: &mut impl BufRead, timeout: Duration) -> Result<Value> {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            bail!("lsp timeout");
        }
        let msg = lsp_try_read(r, Duration::from_millis(200))?;
        if msg.get("id").is_some() {
            return Ok(msg);
        }
    }
}

fn lsp_try_read(r: &mut impl BufRead, _wait: Duration) -> Result<Value> {
    // Non-blocking-ish: read headers with a deadline via set_nonblocking not available on BufReader easily.
    // Blocking read with caller-level timeout loop.
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            bail!("eof");
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        headers.push_str(&line);
    }
    let mut len = 0usize;
    for h in headers.lines() {
        if let Some(v) = h
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
        {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    if len == 0 || len > 4 * 1024 * 1024 {
        bail!("bad content-length");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(serde_json::from_slice(&buf)?)
}

/// Format diagnostics for agent / UI.
pub fn format_snapshot(snap: &DiagnosticsSnapshot) -> String {
    if snap.items.is_empty() {
        return snap.summary.clone();
    }
    let mut lines = vec![snap.summary.clone()];
    for d in snap.items.iter().take(80) {
        lines.push(format!(
            "{}:{}:{}: [{}] {} ({})",
            d.path, d.line, d.col, d.severity, d.message, d.source
        ));
    }
    lines.join("\n")
}

/// Shared last snapshot for UI (updated by tools / Refresh).
pub static LAST_DIAGNOSTICS: Mutex<Option<DiagnosticsSnapshot>> = Mutex::new(None);

pub fn store_snapshot(snap: DiagnosticsSnapshot) {
    if let Ok(mut g) = LAST_DIAGNOSTICS.lock() {
        *g = Some(snap);
    }
}

pub fn load_snapshot() -> DiagnosticsSnapshot {
    LAST_DIAGNOSTICS
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}
