//! Local vector memory — SQLite + hashing embeddings (no ONNX, low RAM).

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const DIM: usize = 192;
const MAX_MEMORIES: usize = 5_000;

#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub id: i64,
    pub text: String,
    pub tags: String,
    pub score: f32,
    pub created_at: String,
}

pub struct MemoryStore {
    conn: Connection,
}

impl MemoryStore {
    pub fn open_default() -> Result<Self> {
        let path = db_path()?;
        Self::open(&path)
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("open memory db")?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS memories (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               text TEXT NOT NULL,
               tags TEXT NOT NULL DEFAULT '',
               embedding BLOB NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_mem_created ON memories(created_at);",
        )?;
        Ok(Self { conn })
    }

    pub fn store(&self, text: &str, tags: &str) -> Result<i64> {
        let text = text.trim();
        if text.is_empty() {
            bail!("empty memory text");
        }
        if text.len() > 8_000 {
            bail!("memory text too long (max 8000 chars)");
        }
        // Cap total rows — drop oldest.
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        if count >= MAX_MEMORIES as i64 {
            self.conn.execute(
                "DELETE FROM memories WHERE id IN (
                   SELECT id FROM memories ORDER BY id ASC LIMIT ?1
                 )",
                params![count - MAX_MEMORIES as i64 + 50],
            )?;
        }
        let emb = embed(text);
        let blob = emb_to_bytes(&emb);
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO memories (text, tags, embedding, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![text, tags, blob, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
        let q = embed(query);
        let limit = limit.clamp(1, 32);
        let mut stmt = self.conn.prepare(
            "SELECT id, text, tags, embedding, created_at FROM memories ORDER BY id DESC LIMIT 2000",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let text: String = row.get(1)?;
            let tags: String = row.get(2)?;
            let blob: Vec<u8> = row.get(3)?;
            let created_at: String = row.get(4)?;
            Ok((id, text, tags, blob, created_at))
        })?;

        let mut hits = Vec::new();
        for r in rows.flatten() {
            let (id, text, tags, blob, created_at) = r;
            let emb = bytes_to_emb(&blob);
            let score = cosine(&q, &emb);
            hits.push(MemoryHit {
                id,
                text,
                tags,
                score,
                created_at,
            });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn list_recent(&self, limit: usize) -> Result<Vec<MemoryHit>> {
        let limit = limit.clamp(1, 50);
        let mut stmt = self.conn.prepare(
            "SELECT id, text, tags, created_at FROM memories ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(MemoryHit {
                id: row.get(0)?,
                text: row.get(1)?,
                tags: row.get(2)?,
                score: 0.0,
                created_at: row.get(3)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}

fn db_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("sh", "harness", "harness").context("dirs")?;
    Ok(dirs.data_dir().join("memory.sqlite3"))
}

/// Feature-hashing embedding: stable, model-free, tiny RAM.
/// Vetor **lexical**, não semântico: palavras e trigramas de caracteres jogados
/// num espaço fixo por hash, normalizado L2. Acha o que você escreveu de novo;
/// não liga "como faço login" a "fluxo de autenticação".
///
/// Trocar por embedding de modelo exigiria migração: `DIM` é fixo em tempo de
/// compilação e os blobs já gravados ficariam incompatíveis.
pub fn embed(text: &str) -> [f32; DIM] {
    let mut v = [0.0f32; DIM];
    let lower = text.to_ascii_lowercase();
    // tokens + char n-grams
    for token in lower.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        add_feature(&mut v, token, 1.0);
        let bytes = token.as_bytes();
        if bytes.len() >= 3 {
            for w in bytes.windows(3) {
                add_feature(&mut v, &format!("3g:{}", String::from_utf8_lossy(w)), 0.5);
            }
        }
    }
    // L2 normalize
    let mut norm = 0.0f32;
    for x in &v {
        norm += x * x;
    }
    norm = norm.sqrt();
    if norm > 1e-8 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn add_feature(v: &mut [f32; DIM], feature: &str, weight: f32) {
    let h = hash64(feature);
    let idx = (h as usize) % DIM;
    let sign = if h & 1 == 0 { 1.0 } else { -1.0 };
    v[idx] += sign * weight;
}

fn hash64(s: &str) -> u64 {
    // FNV-1a 64
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn emb_to_bytes(e: &[f32; DIM]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DIM * 4);
    for x in e {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn bytes_to_emb(b: &[u8]) -> [f32; DIM] {
    let mut e = [0.0f32; DIM];
    for i in 0..DIM {
        let start = i * 4;
        if start + 4 <= b.len() {
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&b[start..start + 4]);
            e[i] = f32::from_le_bytes(buf);
        }
    }
    e
}

fn cosine(a: &[f32; DIM], b: &[f32; DIM]) -> f32 {
    let mut dot = 0.0f32;
    for i in 0..DIM {
        dot += a[i] * b[i];
    }
    // vectors already normalized at insert/query time for query; stored may be normalized too
    dot.clamp(-1.0, 1.0)
}

pub fn format_hits(hits: &[MemoryHit]) -> String {
    if hits.is_empty() {
        return "(no memories)".into();
    }
    let mut lines = Vec::new();
    for h in hits {
        lines.push(format!(
            "#{} score={:.3} tags={} {}\n  {}",
            h.id,
            h.score,
            h.tags,
            h.created_at,
            h.text.chars().take(400).collect::<String>()
        ));
    }
    lines.join("\n")
}

/// Process-wide store (lazy open).
pub static GLOBAL_MEMORY: Mutex<Option<MemoryStore>> = Mutex::new(None);

pub fn with_store<T>(f: impl FnOnce(&MemoryStore) -> Result<T>) -> Result<T> {
    let mut g = GLOBAL_MEMORY.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    if g.is_none() {
        *g = Some(MemoryStore::open_default()?);
    }
    f(g.as_ref().unwrap())
}

/// Heuristic memory extraction from a finished assistant reply (jcode-like light).
pub fn maybe_extract_from_turn(user: &str, assistant: &str) -> usize {
    let mut n = 0;
    let combined = format!("{user}\n{assistant}");
    // Prefer explicit remember cues / durable facts
    let candidates: Vec<String> = assistant
        .lines()
        .filter(|l| {
            let l = l.trim();
            l.len() > 24
                && l.len() < 400
                && (l.contains("prefer")
                    || l.contains("always")
                    || l.contains("never")
                    || l.contains("decided")
                    || l.contains("remember")
                    || l.starts_with("- "))
        })
        .take(3)
        .map(|s| s.trim().trim_start_matches('-').trim().to_string())
        .collect();
    for c in candidates {
        if with_store(|s| s.store(&c, "auto")).is_ok() {
            n += 1;
        }
    }
    // Also store a compressed user intent if looks like a preference
    let u = user.trim();
    if n == 0 && u.len() > 20 && u.len() < 280 {
        let lower = u.to_ascii_lowercase();
        if lower.contains("prefer") || lower.contains("always") || lower.contains("don't") {
            if with_store(|s| s.store(u, "user")).is_ok() {
                n += 1;
            }
        }
    }
    let _ = combined;
    n
}

/// Inject top memories for a user prompt (agent context).
pub fn recall_for_prompt(query: &str, k: usize) -> String {
    match with_store(|s| s.search(query, k)) {
        Ok(hits) if !hits.is_empty() => {
            // Rótulo explícito: sem isto o modelo lê um pedido antigo como se
            // fosse o atual.
            let mut out = String::from(
                "Background from earlier sessions — context only. \
                 The current request is the user's last message, not any of these:\n",
            );
            for h in hits {
                if h.score < 0.08 {
                    continue;
                }
                out.push_str(&format!("- ({:.2}) {}\n", h.score, h.text.chars().take(280).collect::<String>()));
            }
            if out.len() < 24 {
                String::new()
            } else {
                out
            }
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_search() {
        let dir = std::env::temp_dir().join(format!("hmem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sqlite3");
        let store = MemoryStore::open(&path).unwrap();
        store
            .store("User prefers dark mode UI and rustc 1.93", "prefs")
            .unwrap();
        store
            .store("Deploy target is linux aarch64", "infra")
            .unwrap();
        let hits = store.search("dark theme preference", 5).unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].text.to_lowercase().contains("dark") || hits[0].score > 0.0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
