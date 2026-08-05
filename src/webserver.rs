//! Tiny static file server for testing web apps (low footprint).

use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub running: bool,
    pub port: u16,
    pub root: PathBuf,
    pub url: String,
    pub last_error: String,
}

impl Default for ServerStatus {
    fn default() -> Self {
        Self {
            running: false,
            port: 8765,
            root: PathBuf::from("."),
            url: String::new(),
            last_error: String::new(),
        }
    }
}

struct LiveServer {
    stop: Arc<AtomicBool>,
    port: u16,
    #[allow(dead_code)]
    root: PathBuf,
}

static LIVE: Mutex<Option<LiveServer>> = Mutex::new(None);
static STATUS: Mutex<ServerStatus> = Mutex::new(ServerStatus {
    running: false,
    port: 8765,
    root: PathBuf::new(),
    url: String::new(),
    last_error: String::new(),
});

pub fn status() -> ServerStatus {
    STATUS.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn start(root: PathBuf, port: u16) -> Result<ServerStatus> {
    stop();
    if !root.is_dir() {
        bail!("root is not a directory: {}", root.display());
    }
    let port = if port == 0 { 8765 } else { port };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
    listener
        .set_nonblocking(true)
        .context("set_nonblocking")?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let root_t = root.clone();

    thread::Builder::new()
        .name("harness-web".into())
        .spawn(move || serve_loop(listener, root_t, stop_t))
        .context("spawn web server")?;

    let st = ServerStatus {
        running: true,
        port,
        root: root.clone(),
        url: format!("http://127.0.0.1:{port}/"),
        last_error: String::new(),
    };
    if let Ok(mut g) = LIVE.lock() {
        *g = Some(LiveServer {
            stop,
            port,
            root,
        });
    }
    if let Ok(mut g) = STATUS.lock() {
        *g = st.clone();
    }
    Ok(st)
}

pub fn stop() {
    if let Ok(mut g) = LIVE.lock() {
        if let Some(live) = g.take() {
            live.stop.store(true, Ordering::Relaxed);
            // nudge accept loop
            let _ = TcpStream::connect(format!("127.0.0.1:{}", live.port));
            thread::sleep(Duration::from_millis(50));
        }
    }
    if let Ok(mut g) = STATUS.lock() {
        g.running = false;
        g.url.clear();
    }
}

fn serve_loop(listener: TcpListener, root: PathBuf, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = handle_client(&mut stream, &root);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(15));
            }
            Err(e) => {
                if let Ok(mut g) = STATUS.lock() {
                    g.last_error = e.to_string();
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn handle_client(stream: &mut TcpStream, root: &Path) -> Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut lines = req.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");

    if method != "GET" && method != "HEAD" {
        return write_response(stream, 405, "text/plain; charset=utf-8", b"Method Not Allowed");
    }

    let rel = sanitize_url_path(path);
    let mut file_path = root.join(&rel);
    if file_path.is_dir() {
        file_path = file_path.join("index.html");
    }
    // SPA-ish fallback: if missing and looks like a route, serve index.html
    if !file_path.exists() {
        let index = root.join("index.html");
        if index.exists() && !rel.to_string_lossy().contains('.') {
            file_path = index;
        }
    }

    if !file_path.exists() || !file_path.starts_with(root) {
        return write_response(stream, 404, "text/plain; charset=utf-8", b"Not Found");
    }

    let data = fs::read(&file_path).unwrap_or_default();
    let ctype = content_type(&file_path);
    if method == "HEAD" {
        write_headers(stream, 200, ctype, data.len())?;
        return Ok(());
    }
    write_response(stream, 200, ctype, &data)
}

fn sanitize_url_path(path: &str) -> PathBuf {
    let path = path.split('?').next().unwrap_or(path);
    let path = path.trim_start_matches('/');
    let mut out = PathBuf::new();
    for comp in Path::new(path).components() {
        match comp {
            std::path::Component::Normal(s) => out.push(s),
            std::path::Component::CurDir => {}
            _ => {} // skip ParentDir / roots — path traversal guard
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from("index.html")
    } else {
        out
    }
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "wasm" => "application/wasm",
        "txt" | "md" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

fn write_headers(stream: &mut TcpStream, status: u16, ctype: &str, len: usize) -> Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n"
    );
    stream.write_all(headers.as_bytes())?;
    Ok(())
}

fn write_response(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) -> Result<()> {
    write_headers(stream, status, ctype, body.len())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}
