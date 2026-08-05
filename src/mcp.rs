//! Minimal MCP client (JSON-RPC over stdio) — jcode-inspired tool bridge.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub server: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

struct LiveServer {
    name: String,
    child: Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

static LIVE: Mutex<Vec<LiveServer>> = Mutex::new(Vec::new());
static TOOLS: Mutex<Vec<McpToolInfo>> = Mutex::new(Vec::new());

pub fn list_tools() -> Vec<McpToolInfo> {
    TOOLS.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn connected_servers() -> Vec<String> {
    LIVE.lock()
        .map(|g| g.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default()
}

pub fn connect(cfg: &McpServerConfig) -> Result<String> {
    disconnect(&cfg.name);
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().with_context(|| format!("spawn {}", cfg.command))?;
    let stdin = child.stdin.take().context("stdin")?;
    let stdout = child.stdout.take().context("stdout")?;
    let mut live = LiveServer {
        name: cfg.name.clone(),
        child,
        stdin,
        reader: BufReader::new(stdout),
        next_id: 1,
    };

    // initialize
    let init = live.request(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "harness", "version": env!("CARGO_PKG_VERSION") }
        }),
    )?;
    let _ = init;
    live.notify("notifications/initialized", json!({}))?;

    // tools/list
    let listed = live.request("tools/list", json!({}))?;
    let tools = listed
        .pointer("/result/tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    let mut infos = Vec::new();
    for t in tools {
        let name = t
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        infos.push(McpToolInfo {
            server: cfg.name.clone(),
            name,
            description: t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string(),
            input_schema: t.get("inputSchema").cloned().unwrap_or(json!({})),
        });
    }

    if let Ok(mut g) = TOOLS.lock() {
        g.retain(|t| t.server != cfg.name);
        g.extend(infos.iter().cloned());
    }
    if let Ok(mut g) = LIVE.lock() {
        g.push(live);
    }
    Ok(format!(
        "MCP {} connected · {} tool(s)",
        cfg.name,
        infos.len()
    ))
}

pub fn disconnect(name: &str) {
    if let Ok(mut g) = LIVE.lock() {
        let mut kept = Vec::new();
        for mut s in g.drain(..) {
            if s.name == name {
                let _ = s.child.kill();
                let _ = s.child.wait();
            } else {
                kept.push(s);
            }
        }
        *g = kept;
    }
    if let Ok(mut g) = TOOLS.lock() {
        g.retain(|t| t.server != name);
    }
}

pub fn disconnect_all() {
    if let Ok(mut g) = LIVE.lock() {
        for s in g.iter_mut() {
            let _ = s.child.kill();
            let _ = s.child.wait();
        }
        g.clear();
    }
    if let Ok(mut g) = TOOLS.lock() {
        g.clear();
    }
}

pub fn call_tool(server: &str, tool: &str, arguments: Value) -> Result<String> {
    let mut g = LIVE.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    let live = g
        .iter_mut()
        .find(|s| s.name == server)
        .ok_or_else(|| anyhow::anyhow!("MCP server '{server}' not connected"))?;
    let resp = live.request(
        "tools/call",
        json!({
            "name": tool,
            "arguments": arguments
        }),
    )?;
    // content array text
    if let Some(arr) = resp.pointer("/result/content").and_then(|c| c.as_array()) {
        let mut out = String::new();
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    out.push_str(t);
                    out.push('\n');
                }
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    Ok(resp.to_string())
}

pub fn summary() -> String {
    let servers = connected_servers();
    let tools = list_tools();
    if servers.is_empty() {
        return "no MCP servers connected".into();
    }
    let mut lines = vec![format!("{} server(s), {} tool(s)", servers.len(), tools.len())];
    for s in servers {
        lines.push(format!("- {s}"));
    }
    for t in tools.iter().take(40) {
        lines.push(format!("  · {}.{} — {}", t.server, t.name, t.description.chars().take(80).collect::<String>()));
    }
    lines.join("\n")
}

impl LiveServer {
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.write_msg(&body)?;
        // read until matching id
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let msg = self.read_msg()?;
            if msg.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    bail!("MCP error: {err}");
                }
                return Ok(msg);
            }
            // ignore notifications
        }
        bail!("MCP timeout on {method}")
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.write_msg(&body)
    }

    fn write_msg(&mut self, body: &Value) -> Result<()> {
        let s = body.to_string();
        write!(
            self.stdin,
            "Content-Length: {}\r\n\r\n{}",
            s.len(),
            s
        )?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_msg(&mut self) -> Result<Value> {
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                bail!("MCP EOF");
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            headers.push_str(&line);
        }
        let mut len = 0usize;
        for h in headers.lines() {
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        if len == 0 || len > 8 * 1024 * 1024 {
            bail!("bad MCP content-length");
        }
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf)?;
        Ok(serde_json::from_slice(&buf)?)
    }
}
