//! Memory graph + consolidation + side-judge + ambient (jcode-inspired, local).

use anyhow::Result;
use rusqlite::{Connection, params};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::memory::{self, embed};

fn db() -> Result<Connection> {
    let dirs = directories::ProjectDirs::from("sh", "harness", "harness")
        .ok_or_else(|| anyhow::anyhow!("dirs"))?;
    let path = dirs.data_dir().join("memory_graph.sqlite3");
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS nodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'fact',
            embedding BLOB,
            created_at TEXT NOT NULL,
            stale INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS edges (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            src INTEGER NOT NULL,
            dst INTEGER NOT NULL,
            rel TEXT NOT NULL,
            weight REAL NOT NULL DEFAULT 1.0
         );",
    )?;
    Ok(conn)
}

pub fn add_node(text: &str, kind: &str) -> Result<i64> {
    let conn = db()?;
    let emb = embed(text);
    let mut blob = Vec::with_capacity(emb.len() * 4);
    for x in &emb {
        blob.extend_from_slice(&x.to_le_bytes());
    }
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO nodes (text, kind, embedding, created_at) VALUES (?1,?2,?3,?4)",
        params![text, kind, blob, now],
    )?;
    let id = conn.last_insert_rowid();
    // Also mirror into flat memory store for search compatibility
    let _ = memory::with_store(|s| s.store(text, kind));
    // Link to similar existing nodes
    link_similar(&conn, id, &emb)?;
    Ok(id)
}

fn link_similar(conn: &Connection, id: i64, emb: &[f32; 192]) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id, embedding FROM nodes WHERE id != ?1 AND stale=0 ORDER BY id DESC LIMIT 200")?;
    let rows = stmt.query_map(params![id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    for row in rows.flatten() {
        let (oid, blob) = row;
        let o = bytes_to_emb(&blob);
        let score = cosine(emb, &o);
        if score > 0.55 {
            conn.execute(
                "INSERT INTO edges (src, dst, rel, weight) VALUES (?1,?2,'related',?3)",
                params![id, oid, score as f64],
            )?;
        }
    }
    Ok(())
}

fn bytes_to_emb(b: &[u8]) -> [f32; 192] {
    let mut e = [0.0f32; 192];
    for i in 0..192 {
        let s = i * 4;
        if s + 4 <= b.len() {
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&b[s..s + 4]);
            e[i] = f32::from_le_bytes(buf);
        }
    }
    e
}

fn cosine(a: &[f32; 192], b: &[f32; 192]) -> f32 {
    let mut d = 0.0;
    for i in 0..192 {
        d += a[i] * b[i];
    }
    d
}

/// Side-agent style relevance judge (local score, no extra LLM cost by default).
pub fn judge_relevance(query: &str, candidate: &str) -> f32 {
    let q = embed(query);
    let c = embed(candidate);
    let sim = cosine(&q, &c);
    // length penalty for tiny/huge
    let len = candidate.len() as f32;
    let pen = if len < 20.0 {
        0.5
    } else if len > 800.0 {
        0.85
    } else {
        1.0
    };
    (sim * pen).clamp(0.0, 1.0)
}

pub fn consolidate() -> Result<String> {
    let conn = db()?;
    // Mark near-duplicates stale
    let mut stmt = conn.prepare("SELECT id, text, embedding FROM nodes WHERE stale=0 ORDER BY id")?;
    let rows: Vec<(i64, String, Vec<u8>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .flatten()
        .collect();
    let mut marked = 0;
    for i in 0..rows.len() {
        for j in (i + 1)..rows.len() {
            let a = bytes_to_emb(&rows[i].2);
            let b = bytes_to_emb(&rows[j].2);
            if cosine(&a, &b) > 0.92 {
                conn.execute("UPDATE nodes SET stale=1 WHERE id=?1", params![rows[j].0])?;
                marked += 1;
            }
        }
    }
    // Drop old edges to stale
    conn.execute(
        "DELETE FROM edges WHERE src IN (SELECT id FROM nodes WHERE stale=1)
         OR dst IN (SELECT id FROM nodes WHERE stale=1)",
        [],
    )?;
    Ok(format!("consolidated: marked {marked} duplicate node(s) stale"))
}

pub fn graph_summary() -> Result<String> {
    let conn = db()?;
    let nodes: i64 = conn.query_row("SELECT COUNT(*) FROM nodes WHERE stale=0", [], |r| r.get(0))?;
    let edges: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let stale: i64 = conn.query_row("SELECT COUNT(*) FROM nodes WHERE stale=1", [], |r| r.get(0))?;
    Ok(format!("nodes={nodes} edges={edges} stale={stale}"))
}

// --- Ambient mode ---
static AMBIENT_ON: AtomicBool = AtomicBool::new(false);
static LAST_DRIFT: Mutex<Option<Instant>> = Mutex::new(None);
static LAST_USER: Mutex<String> = Mutex::new(String::new());

pub fn ambient_start() {
    if AMBIENT_ON.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::Builder::new()
        .name("harness-ambient".into())
        .spawn(|| {
            while AMBIENT_ON.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(120));
                let _ = consolidate();
            }
        })
        .ok();
}

pub fn ambient_stop() {
    AMBIENT_ON.store(false, Ordering::SeqCst);
}

pub fn ambient_status() -> String {
    format!(
        "ambient={} {}",
        AMBIENT_ON.load(Ordering::SeqCst),
        graph_summary().unwrap_or_default()
    )
}

/// Call when a user message arrives — extract on semantic drift.
pub fn on_user_message(text: &str) {
    let mut last = LAST_USER.lock().ok();
    let drift = if let Some(ref mut g) = last {
        let prev = g.clone();
        **g = text.to_string();
        if prev.is_empty() {
            false
        } else {
            judge_relevance(&prev, text) < 0.25
        }
    } else {
        false
    };
    if drift {
        if let Ok(mut t) = LAST_DRIFT.lock() {
            *t = Some(Instant::now());
        }
        // store a drift marker
        let _ = add_node(&format!("topic shift: {}", text.chars().take(160).collect::<String>()), "drift");
    }
}
