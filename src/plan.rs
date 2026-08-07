//! Session plan / todo list (jcode-inspired, local JSON).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: u32,
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    pub items: Vec<PlanItem>,
    next_id: u32,
}

static PLAN: Mutex<Plan> = Mutex::new(Plan {
    items: Vec::new(),
    next_id: 1,
});

fn plan_path(chat_dir: &Path) -> PathBuf {
    chat_dir.join(".harness_plan.json")
}

pub fn load(chat_dir: &Path) -> Result<()> {
    let p = plan_path(chat_dir);
    if p.exists() {
        let raw = fs::read_to_string(p)?;
        let plan: Plan = serde_json::from_str(&raw)?;
        if let Ok(mut g) = PLAN.lock() {
            *g = plan;
        }
    } else if let Ok(mut g) = PLAN.lock() {
        *g = Plan::default();
        g.next_id = 1;
    }
    Ok(())
}

pub fn save(chat_dir: &Path) -> Result<()> {
    let g = PLAN.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    fs::write(plan_path(chat_dir), serde_json::to_string_pretty(&*g)?)?;
    Ok(())
}

pub fn add(text: &str) -> Result<PlanItem> {
    let mut g = PLAN.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    let id = g.next_id;
    g.next_id += 1;
    let item = PlanItem {
        id,
        text: text.to_string(),
        done: false,
    };
    g.items.push(item.clone());
    Ok(item)
}

pub fn set_done(id: u32, done: bool) -> Result<String> {
    let mut g = PLAN.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(it) = g.items.iter_mut().find(|i| i.id == id) {
        it.done = done;
        Ok(format!("#{} done={}", id, done))
    } else {
        anyhow::bail!("unknown plan item {id}")
    }
}

pub fn format() -> String {
    let g = PLAN.lock().ok();
    let Some(g) = g else {
        return "(plan unavailable)".into();
    };
    if g.items.is_empty() {
        return "(empty plan)".into();
    }
    g.items
        .iter()
        .map(|i| {
            format!(
                "[{}] #{} {}",
                if i.done { "x" } else { " " },
                i.id,
                i.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn sync_side_panel() {
    crate::side_panel::set_plan(format());
}
