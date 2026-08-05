//! Versioned shared plan DAG for multi-agent swarms.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTask {
    pub id: String,
    pub title: String,
    pub status: String, // pending | running | done | blocked
    pub assignee: Option<String>,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VersionedPlan {
    pub version: u32,
    pub tasks: Vec<PlanTask>,
}

static PLANS: Mutex<Option<HashMap<String, VersionedPlan>>> = Mutex::new(None);

fn with_plans<T>(f: impl FnOnce(&mut HashMap<String, VersionedPlan>) -> T) -> T {
    let mut g = PLANS.lock().expect("plans");
    if g.is_none() {
        *g = Some(HashMap::new());
    }
    f(g.as_mut().unwrap())
}

pub fn get(swarm_id: &str) -> VersionedPlan {
    with_plans(|g| g.get(swarm_id).cloned().unwrap_or_default())
}

/// Todos os planos vivos (para o snapshot que a GUI consome).
pub fn all() -> Vec<(String, VersionedPlan)> {
    with_plans(|g| {
        let mut v: Vec<_> = g.iter().map(|(k, p)| (k.clone(), p.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    })
}

pub fn propose(swarm_id: &str, tasks: Vec<PlanTask>) -> VersionedPlan {
    with_plans(|g| {
        let plan = g.entry(swarm_id.to_string()).or_default();
        plan.version += 1;
        plan.tasks = tasks;
        plan.clone()
    })
}

pub fn assign(swarm_id: &str, task_id: &str, agent: &str) -> Result<String, String> {
    with_plans(|g| {
        let plan = g.entry(swarm_id.to_string()).or_default();
        if let Some(t) = plan.tasks.iter_mut().find(|t| t.id == task_id) {
            t.assignee = Some(agent.into());
            t.status = "running".into();
            plan.version += 1;
            Ok(format!("assigned {task_id} → {agent} (v{})", plan.version))
        } else {
            Err(format!("unknown task {task_id}"))
        }
    })
}

pub fn complete(swarm_id: &str, task_id: &str) -> Result<String, String> {
    with_plans(|g| {
        let plan = g.entry(swarm_id.to_string()).or_default();
        if let Some(t) = plan.tasks.iter_mut().find(|t| t.id == task_id) {
            t.status = "done".into();
            plan.version += 1;
            Ok(format!("done {task_id} (v{})", plan.version))
        } else {
            Err(format!("unknown task {task_id}"))
        }
    })
}

pub fn next_runnable(swarm_id: &str) -> Vec<PlanTask> {
    let plan = get(swarm_id);
    let done: std::collections::HashSet<_> = plan
        .tasks
        .iter()
        .filter(|t| t.status == "done")
        .map(|t| t.id.clone())
        .collect();
    plan.tasks
        .into_iter()
        .filter(|t| {
            t.status == "pending"
                && t.depends_on.iter().all(|d| done.contains(d))
        })
        .collect()
}

pub fn format(swarm_id: &str) -> String {
    let plan = get(swarm_id);
    if plan.tasks.is_empty() {
        return format!("plan {swarm_id} empty");
    }
    let mut lines = vec![format!("plan {swarm_id} v{}", plan.version)];
    for t in &plan.tasks {
        lines.push(format!(
            "- [{}] {} {} ass={} deps={:?}",
            t.status,
            t.id,
            t.title,
            t.assignee.clone().unwrap_or_else(|| "-".into()),
            t.depends_on
        ));
    }
    lines.join("\n")
}
