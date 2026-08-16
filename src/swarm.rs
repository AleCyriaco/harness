//! Lightweight multi-agent swarm — bounded concurrency, shared file notify bus.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::config::Config;
use crate::llm::{self, ChatMessage};
use crate::modes::AppMode;
use crate::tools;

const MAX_AGENTS: usize = 4;
const MAX_BUS: usize = 80;
const MAX_WORKER_ROUNDS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Coordinator,
    Worker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Running,
    Done,
    Error,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub role: AgentRole,
    pub state: AgentState,
    pub task: String,
    pub last_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub ts: u64,
    pub from: String,
    pub to: String, // "*" for broadcast
    pub body: String,
}

/// Estado do swarm de um processo, pronto para viajar no protocolo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwarmSnapshot {
    pub agents: Vec<AgentInfo>,
    pub bus: Vec<BusMessage>,
    pub file_events: Vec<String>,
    /// (caminho, agente que reservou)
    pub claims: Vec<(String, String)>,
    pub plans: Vec<(String, crate::swarm_plan::VersionedPlan)>,
    pub max_workers: usize,
}

struct LiveAgent {
    info: AgentInfo,
    cancel: Arc<AtomicBool>,
}

pub struct Swarm {
    agents: HashMap<String, LiveAgent>,
    bus: VecDeque<BusMessage>,
    file_events: VecDeque<String>,
    /// caminho → nome do agente que está escrevendo nele
    claims: HashMap<String, String>,
    running: AtomicUsize,
}

impl Default for Swarm {
    fn default() -> Self {
        Self::new()
    }
}

impl Swarm {
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            bus: VecDeque::new(),
            file_events: VecDeque::new(),
            claims: HashMap::new(),
            running: AtomicUsize::new(0),
        }
    }

    pub fn list(&self) -> Vec<AgentInfo> {
        let mut v: Vec<_> = self.agents.values().map(|a| a.info.clone()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn bus_tail(&self, n: usize) -> Vec<BusMessage> {
        self.bus.iter().rev().take(n).cloned().rev().collect()
    }

    pub fn file_events_tail(&self, n: usize) -> Vec<String> {
        self.file_events.iter().rev().take(n).cloned().rev().collect()
    }

    pub fn push_file_event(&mut self, path: &str) {
        self.file_events
            .push_back(format!("{} edited", path));
        while self.file_events.len() > 40 {
            self.file_events.pop_front();
        }
    }

    pub fn post(&mut self, from: &str, to: &str, body: &str) {
        self.bus.push_back(BusMessage {
            ts: now_secs(),
            from: from.into(),
            to: to.into(),
            body: body.into(),
        });
        while self.bus.len() > MAX_BUS {
            self.bus.pop_front();
        }
    }

    pub fn running_count(&self) -> usize {
        self.agents
            .values()
            .filter(|a| a.info.state == AgentState::Running)
            .count()
    }

    /// Reserva um arquivo para `agent`. Erra se outro worker vivo já o tem.
    pub fn claim_file(&mut self, path: &str, agent: &str) -> Result<(), String> {
        if let Some(owner) = self.claims.get(path) {
            if owner != agent {
                let owner_busy = self
                    .agents
                    .values()
                    .any(|a| a.info.name == *owner && a.info.state == AgentState::Running);
                if owner_busy {
                    return Err(format!(
                        "{path} is claimed by {owner} — coordinate via swarm_message, or wait with swarm_wait"
                    ));
                }
            }
        }
        self.claims.insert(path.to_string(), agent.to_string());
        Ok(())
    }

    fn release_agent_files(&mut self, agent: &str) {
        self.claims.retain(|_, owner| owner != agent);
    }

    pub fn snapshot(&self) -> SwarmSnapshot {
        SwarmSnapshot {
            agents: self.list(),
            bus: self.bus_tail(20),
            file_events: self.file_events_tail(12),
            claims: self
                .claims
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            plans: crate::swarm_plan::all(),
            max_workers: MAX_AGENTS.saturating_sub(1),
        }
    }

    pub fn stop(&mut self, id: &str) -> String {
        if let Some(a) = self.agents.get_mut(id) {
            a.cancel.store(true, Ordering::Relaxed);
            a.info.state = AgentState::Stopped;
            a.info.last_message = "stopped".into();
            format!("stopped {}", a.info.name)
        } else {
            format!("unknown agent {id}")
        }
    }

    pub fn stop_all(&mut self) {
        let ids: Vec<_> = self.agents.keys().cloned().collect();
        for id in ids {
            let _ = self.stop(&id);
        }
    }

    pub fn spawn_worker(
        swarm: Arc<Mutex<Swarm>>,
        cfg: Config,
        name: Option<String>,
        task: String,
    ) -> Result<AgentInfo, String> {
        let mut g = swarm.lock().map_err(|e| e.to_string())?;
        let running = g.running.load(Ordering::Relaxed);
        if g.agents.len() >= MAX_AGENTS {
            return Err(format!("max agents ({MAX_AGENTS}) reached"));
        }
        if running >= MAX_AGENTS.saturating_sub(1) {
            return Err("too many running workers".into());
        }

        let id = Uuid::new_v4().to_string();
        let short = id[..8].to_string();
        let name = name.unwrap_or_else(|| format!("worker-{short}"));
        let cancel = Arc::new(AtomicBool::new(false));
        let info = AgentInfo {
            id: id.clone(),
            name: name.clone(),
            role: AgentRole::Worker,
            state: AgentState::Running,
            task: task.clone(),
            last_message: "starting…".into(),
        };
        g.agents.insert(
            id.clone(),
            LiveAgent {
                info: info.clone(),
                cancel: Arc::clone(&cancel),
            },
        );
        g.post("system", &name, &format!("spawned: {task}"));
        g.running.fetch_add(1, Ordering::Relaxed);
        drop(g);

        let swarm_bg = Arc::clone(&swarm);
        let agent_id = id.clone();
        let agent_name = name.clone();
        thread::Builder::new()
            .name(format!("swarm-{short}"))
            .spawn(move || {
                let result = run_worker(&cfg, &task, &cancel, &swarm_bg, &agent_name);
                if let Ok(mut g) = swarm_bg.lock() {
                    let bus_body = match &result {
                        Ok(reply) => {
                            let msg: String = reply.chars().take(240).collect();
                            if let Some(a) = g.agents.get_mut(&agent_id) {
                                a.info.state = AgentState::Done;
                                a.info.last_message = msg.clone();
                            }
                            msg
                        }
                        Err(e) => {
                            if let Some(a) = g.agents.get_mut(&agent_id) {
                                a.info.state = AgentState::Error;
                                a.info.last_message = e.clone();
                            }
                            e.clone()
                        }
                    };
                    g.post(&agent_name, "coordinator", &bus_body);
                    g.release_agent_files(&agent_name);
                    g.running.fetch_sub(1, Ordering::Relaxed);
                }
            })
            .map_err(|e| e.to_string())?;

        Ok(info)
    }
}

fn run_worker(
    cfg: &Config,
    task: &str,
    cancel: &AtomicBool,
    swarm: &Arc<Mutex<Swarm>>,
    name: &str,
) -> Result<String, String> {
    // Optional second LLM for workers (same memory/files; different quota)
    let mut cfg = cfg.clone();
    if cfg.llm_multi_worker {
        if let Some(ep) = crate::llm_pool::worker_endpoint(&cfg) {
            ep.apply_to(&mut cfg);
        }
    }
    set_current_agent(name);
    let bus_ctx = swarm
        .lock()
        .map(|g| {
            g.bus_tail(6)
                .into_iter()
                .map(|m| format!("{}→{}: {}", m.from, m.to, m.body))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let files = swarm
        .lock()
        .map(|g| g.file_events_tail(8).join("\n"))
        .unwrap_or_default();

    let mut system = format!(
        r#"You are harness swarm worker "{name}".
Workspace: {}
Complete ONLY your assigned task. Prefer search + partial reads + replace_in_file.
Report a short summary when done. No destructive shell.

Recent bus:
{bus_ctx}

Recent file events:
{files}"#,
        cfg.workspace.display()
    );
    crate::tokenless::apply_to_system(&mut system, cfg.token_less);
    crate::metrics::set_current_level(cfg.token_less);

    let mut history = vec![
        ChatMessage {
            role: "system".into(),
            content: Some(system),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        },
        ChatMessage {
            role: "user".into(),
            content: Some(task.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        },
    ];

    let tools = tools::tool_schemas(AppMode::Code);
    // Workers do not get swarm spawn tools — strip by mode only (code tools).
    // Further strip swarm_* in dispatch if called.

    let mut final_text = String::new();
    for _ in 0..MAX_WORKER_ROUNDS {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        history = llm::compact_history(&history, cfg.history_cap.min(16), cfg.tool_result_cap);
        let reply = llm::chat(&cfg, &history, &tools, cancel, None).map_err(|e| e.to_string())?;
        let msg = reply.message;
        if let Some(calls) = msg.tool_calls.clone() {
            if !calls.is_empty() {
                history.push(ChatMessage {
                    role: "assistant".into(),
                    content: msg.content.clone(),
                    tool_calls: Some(calls.clone()),
                    tool_call_id: None,
                    name: None,
            images: Vec::new(),
                });
                for call in calls {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("cancelled".into());
                    }
                    let name_t = call.function.name.clone();
                    // Workers cannot spawn more agents.
                    if name_t.starts_with("swarm_") {
                        history.push(ChatMessage {
                            role: "tool".into(),
                            content: Some("error: workers cannot use swarm tools".into()),
                            tool_calls: None,
                            tool_call_id: Some(call.id),
                            name: Some(name_t),
            images: Vec::new(),
                        });
                        continue;
                    }
                    let args = call.function.arguments.clone();
                    let result = match tools::dispatch(&cfg, AppMode::Code, &name_t, &args, cancel) {
                        Ok(s) => {
                            if matches!(
                                name_t.as_str(),
                                "write_file" | "replace_in_file" | "apply_patch"
                            ) {
                                if let Ok(mut g) = swarm.lock() {
                                    // best-effort path from args
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&args)
                                    {
                                        if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                                            g.push_file_event(p);
                                        }
                                    }
                                }
                            }
                            s
                        }
                        Err(e) => format!("error: {e}"),
                    };
                    history.push(ChatMessage {
                        role: "tool".into(),
                        content: Some(result),
                        tool_calls: None,
                        tool_call_id: Some(call.id),
                        name: Some(name_t),
            images: Vec::new(),
                    });
                }
                continue;
            }
        }
        final_text = msg.content.unwrap_or_default();
        break;
    }
    if final_text.is_empty() {
        final_text = "(worker done)".into();
    }
    Ok(final_text)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Global swarm for the process (UI + tools).
pub static GLOBAL_SWARM: Mutex<Option<Arc<Mutex<Swarm>>>> = Mutex::new(None);

pub fn global_swarm() -> Arc<Mutex<Swarm>> {
    let mut g = GLOBAL_SWARM.lock().unwrap();
    if g.is_none() {
        *g = Some(Arc::new(Mutex::new(Swarm::new())));
    }
    Arc::clone(g.as_ref().unwrap())
}

thread_local! {
    /// Nome do worker rodando nesta thread ("" = agente principal / coordenador).
    static CURRENT_AGENT: RefCell<String> = const { RefCell::new(String::new()) };
}

pub fn set_current_agent(name: &str) {
    CURRENT_AGENT.with(|c| *c.borrow_mut() = name.to_string());
}

pub fn current_agent() -> String {
    CURRENT_AGENT.with(|c| c.borrow().clone())
}

/// Reserva o arquivo para o worker corrente. Sem worker, não faz nada
/// (o coordenador só escreve quando os workers já reportaram).
pub fn claim_path(path: &str) -> Result<(), String> {
    let agent = current_agent();
    if agent.is_empty() {
        return Ok(());
    }
    let s = global_swarm();
    let mut g = s.lock().map_err(|e| e.to_string())?;
    g.claim_file(path, &agent)
}

pub fn snapshot() -> SwarmSnapshot {
    global_swarm()
        .lock()
        .map(|g| g.snapshot())
        .unwrap_or_default()
}

/// Bloqueia até os workers pedidos saírem de `Running` (ou estourar o tempo).
/// `who` = "all" / "*" ou um id/nome de agente.
pub fn wait_for(who: &str, timeout: Duration, cancel: &AtomicBool) -> String {
    let deadline = Instant::now() + timeout;
    let matches = |a: &AgentInfo| {
        who.is_empty() || who == "all" || who == "*" || a.id.starts_with(who) || a.name == who
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return "cancelled".into();
        }
        let agents = global_swarm().lock().map(|g| g.list()).unwrap_or_default();
        let watched: Vec<_> = agents.iter().filter(|a| matches(a)).collect();
        if watched.is_empty() {
            return format!("no agents matching '{who}'");
        }
        let still_running = watched
            .iter()
            .filter(|a| a.state == AgentState::Running)
            .count();
        if still_running == 0 {
            let mut lines = vec![format!("{} agent(s) finished", watched.len())];
            for a in watched {
                lines.push(format!(
                    "- {} [{:?}] {}",
                    a.name,
                    a.state,
                    a.last_message.chars().take(400).collect::<String>()
                ));
            }
            return lines.join("\n");
        }
        if Instant::now() >= deadline {
            return format!(
                "timeout — {still_running} agent(s) still running:\n{}",
                watched
                    .iter()
                    .filter(|a| a.state == AgentState::Running)
                    .map(|a| format!("- {}: {}", a.name, a.task))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub fn summary_text() -> String {
    let s = global_swarm();
    let Ok(g) = s.lock() else {
        return "swarm unavailable".into();
    };
    let agents = g.list();
    if agents.is_empty() {
        return "no swarm agents".into();
    }
    let mut lines = vec![format!("{} agent(s)", agents.len())];
    for a in agents {
        lines.push(format!(
            "- {} [{}] {:?}: {} | {}",
            a.name,
            &a.id[..8.min(a.id.len())],
            a.state,
            a.task.chars().take(60).collect::<String>(),
            a.last_message.chars().take(80).collect::<String>()
        ));
    }
    let bus = g.bus_tail(8);
    if !bus.is_empty() {
        lines.push("bus:".into());
        for m in bus {
            lines.push(format!("  {}→{}: {}", m.from, m.to, m.body));
        }
    }
    lines.join("\n")
}

