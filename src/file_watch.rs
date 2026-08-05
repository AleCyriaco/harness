//! Track file reads per agent; notify peers on overlapping writes (jcode swarm).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct FileNotify {
    pub path: String,
    pub writer: String,
    pub readers: Vec<String>,
}

static READS: Mutex<Option<HashMap<String, HashSet<String>>>> = Mutex::new(None);
static NOTICES: Mutex<Option<Vec<FileNotify>>> = Mutex::new(None);

fn with_reads<T>(f: impl FnOnce(&mut HashMap<String, HashSet<String>>) -> T) -> T {
    let mut g = READS.lock().expect("reads");
    if g.is_none() {
        *g = Some(HashMap::new());
    }
    f(g.as_mut().unwrap())
}

fn with_notices<T>(f: impl FnOnce(&mut Vec<FileNotify>) -> T) -> T {
    let mut g = NOTICES.lock().expect("notices");
    if g.is_none() {
        *g = Some(Vec::new());
    }
    f(g.as_mut().unwrap())
}

pub fn note_read(agent: &str, path: &str) {
    with_reads(|g| {
        g.entry(path.to_string())
            .or_default()
            .insert(agent.to_string());
    });
}

pub fn note_write(agent: &str, path: &str) -> Option<FileNotify> {
    let others: Vec<String> = with_reads(|g| {
        g.get(path)
            .map(|set| set.iter().filter(|a| *a != agent).cloned().collect())
            .unwrap_or_default()
    });
    if others.is_empty() {
        return None;
    }
    let n = FileNotify {
        path: path.into(),
        writer: agent.into(),
        readers: others,
    };
    with_notices(|g| {
        g.push(n.clone());
        while g.len() > 50 {
            g.remove(0);
        }
    });
    Some(n)
}

pub fn drain_notices_for(agent: &str) -> Vec<FileNotify> {
    with_notices(|g| {
        let mut out = Vec::new();
        let mut keep = Vec::new();
        for n in g.drain(..) {
            if n.readers.iter().any(|r| r == agent) {
                out.push(n);
            } else {
                keep.push(n);
            }
        }
        *g = keep;
        out
    })
}

pub fn format_notices(notices: &[FileNotify]) -> String {
    if notices.is_empty() {
        return String::new();
    }
    let mut lines = vec!["⚠ Files you read were edited by peers:".to_string()];
    for n in notices {
        lines.push(format!(
            "- {} written by {} (you were a reader)",
            n.path, n.writer
        ));
    }
    lines.join("\n")
}
