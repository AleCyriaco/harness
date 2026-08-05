//! Minimal Rust client for harness daemon (TCP NDJSON).
//!
//! ```ignore
//! let mut c = harness_sdk::Client::connect("127.0.0.1:19876")?;
//! let sid = c.create_session("code")?;
//! c.run(&sid, "hello", |ev| println!("{ev:?}"))?;
//! ```

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

pub struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Client {
    pub fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr).context("connect daemon")?;
        stream.set_nodelay(true)?;
        let reader = BufReader::new(stream.try_clone()?);
        let mut c = Self {
            reader,
            writer: stream,
        };
        let _ = c.read()?;
        Ok(c)
    }

    fn write_json(&mut self, v: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.writer, v)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        if line.is_empty() {
            bail!("eof");
        }
        Ok(serde_json::from_str(line.trim())?)
    }

    pub fn create_session(&mut self, mode: &str) -> Result<String> {
        self.write_json(&serde_json::json!({
            "type": "create_session",
            "mode": mode
        }))?;
        loop {
            let m = self.read()?;
            if m.get("type").and_then(|t| t.as_str()) == Some("session_created") {
                return Ok(m
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string());
            }
            if m.get("type").and_then(|t| t.as_str()) == Some("error") {
                bail!("{}", m.get("message").and_then(|x| x.as_str()).unwrap_or("err"));
            }
        }
    }

    pub fn run(
        &mut self,
        session_id: &str,
        text: &str,
        mut on_event: impl FnMut(&Value),
    ) -> Result<String> {
        self.write_json(&serde_json::json!({
            "type": "user_message",
            "session_id": session_id,
            "text": text
        }))?;
        loop {
            let m = self.read()?;
            on_event(&m);
            if m.get("type").and_then(|t| t.as_str()) == Some("event")
                && m.get("event").and_then(|e| e.as_str()) == Some("done")
            {
                return Ok(m
                    .pointer("/payload/reply")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string());
            }
            if m.get("type").and_then(|t| t.as_str()) == Some("error") {
                bail!("{}", m.get("message").and_then(|x| x.as_str()).unwrap_or("err"));
            }
        }
    }
}
