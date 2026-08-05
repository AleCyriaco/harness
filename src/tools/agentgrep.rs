//! agentgrep — structure-aware search with adaptive truncation (jcode-inspired).

use anyhow::{Result, bail};
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::safe_join;

/// Paths already shown to the agent this process (adaptive truncation).
static SEEN: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);

fn with_seen<T>(f: impl FnOnce(&mut HashSet<String>) -> T) -> T {
    let mut g = SEEN.lock().expect("seen");
    if g.is_none() {
        *g = Some(HashSet::new());
    }
    f(g.as_mut().unwrap())
}

pub fn agentgrep(root: &Path, query: &str, path_contains: Option<&str>, max_hits: usize) -> Result<String> {
    if query.is_empty() {
        bail!("empty query");
    }
    let q = query.to_ascii_lowercase();
    let max_hits = max_hits.clamp(1, 60);
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    with_seen(|seen| {

    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if matches!(
                    name,
                    "target" | ".git" | "node_modules" | "dist" | ".venv" | "venv"
                ) {
                    continue;
                }
                stack.push(p);
                continue;
            }
            if !is_code(&p) {
                continue;
            }
            if let Some(f) = path_contains {
                if !p.to_string_lossy().contains(f) {
                    continue;
                }
            }
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            let already = seen.contains(&rel);
            if let Ok(file) = std::fs::File::open(&p) {
                let reader = BufReader::new(file);
                let mut symbols = Vec::new();
                let mut file_hits = Vec::new();
                for (i, line) in reader.lines().enumerate() {
                    let Ok(line) = line else { continue };
                    let ln = i + 1;
                    if let Some(sym) = detect_symbol(&line) {
                        symbols.push((ln, sym));
                    }
                    if line.to_ascii_lowercase().contains(&q) {
                        let ctx = if already {
                            line.chars().take(100).collect::<String>()
                        } else {
                            line.chars().take(220).collect::<String>()
                        };
                        file_hits.push((ln, ctx));
                        if file_hits.len() >= if already { 3 } else { 8 } {
                            break;
                        }
                    }
                }
                if !file_hits.is_empty() {
                    seen.insert(rel.clone());
                    for (ln, ctx) in file_hits {
                        let outline = nearby(&symbols, ln);
                        let struct_info = if already {
                            String::new()
                        } else {
                            format!("\n  structure: {outline}")
                        };
                        hits.push(format!("{rel}:{ln}:{struct_info}\n  {ctx}"));
                        if hits.len() >= max_hits {
                            return;
                        }
                    }
                }
            }
        }
    }
    });
    if hits.is_empty() {
        Ok("(no matches)".into())
    } else {
        Ok(hits.join("\n\n"))
    }
}

pub fn note_read_path(root: &Path, rel: &str) {
    if let Ok(p) = safe_join(root, rel) {
        let s = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .into_owned();
        with_seen(|g| {
            g.insert(s);
        });
    }
}

fn is_code(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).unwrap_or(""),
        "rs" | "py" | "js" | "ts" | "tsx" | "go" | "java" | "c" | "h" | "cpp" | "md" | "toml"
            | "json" | "swift" | "kt" | "css" | "html"
    )
}

fn detect_symbol(line: &str) -> Option<String> {
    let t = line.trim();
    for p in [
        "fn ", "pub fn ", "def ", "class ", "struct ", "pub struct ", "impl ", "function ",
        "export function ", "interface ", "type ",
    ] {
        if let Some(rest) = t.strip_prefix(p) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(format!("{p}{name}"));
            }
        }
    }
    None
}

fn nearby(symbols: &[(usize, String)], line: usize) -> String {
    let mut v = Vec::new();
    for (ln, name) in symbols.iter().rev() {
        if *ln <= line {
            v.push(name.as_str());
            if v.len() >= 4 {
                break;
            }
        }
    }
    if v.is_empty() {
        "—".into()
    } else {
        v.join(" · ")
    }
}
