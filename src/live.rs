//! Visão "Live": o turno inteiro como grafo — você → harness → agente → tools
//! → resultado. Módulo puro: monta nós e arestas a partir do estado que a GUI
//! já tem (eventos de tool que ela recebe, flags do config, snapshot do swarm).
//! Quem desenha, arrasta e clica é o `app.rs`; aqui só a topologia, testável.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    You,
    Harness,
    /// checkpoint / guard / compaction — o que o harness faz calado.
    Infra,
    Agent,
    Llm,
    Task,
    Tool,
    Result,
    Swarm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Done,
    Pending,
    Error,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Linha cheia (contém / gerou).
    Solid,
    /// Fluxo animado (agente ↔ LLM ativo).
    Flow,
    /// Tracejada (rota, dependência, contribuição).
    Dashed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub kind: Kind,
    pub state: State,
    pub label: String,
    /// Camada da esquerda p/ direita (0 = você, 4 = resultado). Vira X no layout.
    pub col: u8,
    pub detail: Vec<(String, String)>,
    pub last: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Uma chamada de tool observada no turno (a GUI já recebe estes eventos).
#[derive(Debug, Clone)]
pub struct ToolEvt {
    pub name: String,
    pub arg: String,
    pub result: String,
    pub done: bool,
}

pub struct Input<'a> {
    pub chat_label: &'a str,
    pub busy: bool,
    pub llm_name: &'a str,
    pub llm_model: &'a str,
    pub checkpoint: bool,
    pub guard: bool,
    pub compaction: bool,
    pub tools: &'a [ToolEvt],
    /// (id, título, status) — vazio em chat sem plano.
    pub tasks: &'a [(String, String, String)],
    /// (nome, estado, tarefa) — vazio sem swarm.
    pub swarm: &'a [(String, String, String)],
}

fn task_state(status: &str) -> State {
    match status {
        "done" => State::Done,
        "running" => State::Running,
        "blocked" => State::Error,
        _ => State::Pending,
    }
}

/// Tools que produzem um artefato — dão origem ao nó de resultado.
fn produces(name: &str, arg: &str) -> bool {
    matches!(
        name,
        "write_file" | "replace_in_file" | "multiedit" | "apply_patch"
            | "create_doc" | "create_sheet" | "create_pdf"
    ) || (name == "run_command" && (arg.contains("mkdir") || arg.contains(" > ")))
}

pub fn build(input: &Input) -> Graph {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let solid = |a: &str, b: &str| Edge {
        from: a.into(),
        to: b.into(),
        kind: EdgeKind::Solid,
    };

    // você → harness
    nodes.push(Node {
        id: "you".into(),
        kind: Kind::You,
        state: State::Idle,
        label: input.chat_label.to_string(),
        col: 0,
        detail: vec![("request".into(), input.chat_label.to_string())],
        last: "where this turn started".into(),
    });
    nodes.push(Node {
        id: "harness".into(),
        kind: Kind::Harness,
        state: if input.busy { State::Running } else { State::Idle },
        label: "harness".into(),
        col: 1,
        detail: vec![
            ("role".into(), "runs the agent behind the GUI".into()),
            ("silent work".into(), "checkpoint · guard · compaction".into()),
        ],
        last: "holds the session even if you close the window".into(),
    });
    edges.push(solid("you", "harness"));

    // infra: o que o harness faz por trás
    for (on, id, label, what) in [
        (input.checkpoint, "ck", "checkpoint", "snapshots a file before an edit"),
        (input.guard, "gd", "guard", "blocks destructive commands + secret reads"),
        (input.compaction, "cp", "compaction", "summarizes old history instead of dropping it"),
    ] {
        if !on {
            continue;
        }
        nodes.push(Node {
            id: id.into(),
            kind: Kind::Infra,
            state: State::Idle,
            label: label.into(),
            col: 1,
            detail: vec![("role".into(), what.into())],
            last: "on".into(),
        });
        edges.push(Edge {
            from: "harness".into(),
            to: id.into(),
            kind: EdgeKind::Dashed,
        });
    }

    // agente + LLM
    nodes.push(Node {
        id: "agent".into(),
        kind: Kind::Agent,
        state: if input.busy { State::Running } else { State::Idle },
        label: "agent".into(),
        col: 2,
        detail: vec![
            ("doing".into(), if input.busy { "working" } else { "idle" }.into()),
            ("calls".into(), input.tools.len().to_string()),
        ],
        last: input
            .tools
            .last()
            .map(|t| format!("{} {}", t.name, t.arg.chars().take(40).collect::<String>()))
            .unwrap_or_else(|| "waiting for you".into()),
    });
    edges.push(solid("harness", "agent"));
    nodes.push(Node {
        id: "llm".into(),
        kind: Kind::Llm,
        state: if input.busy { State::Running } else { State::Idle },
        label: input.llm_model.to_string(),
        col: 2,
        detail: vec![
            ("endpoint".into(), input.llm_name.to_string()),
            ("model".into(), input.llm_model.to_string()),
        ],
        last: "answering the agent's calls".into(),
    });
    edges.push(Edge {
        from: "agent".into(),
        to: "llm".into(),
        kind: EdgeKind::Flow,
    });

    // tarefas do plano
    for (id, title, status) in input.tasks {
        let nid = format!("task_{id}");
        nodes.push(Node {
            id: nid.clone(),
            kind: Kind::Task,
            state: task_state(status),
            label: title.chars().take(28).collect(),
            col: 3,
            detail: vec![
                ("task".into(), title.clone()),
                ("status".into(), status.clone()),
            ],
            last: status.clone(),
        });
        edges.push(solid("agent", &nid));
    }

    // tools do turno, agrupadas na tarefa aberta (inferência por ordem)
    let mut task_idx = 0usize;
    let mut result_from: Option<String> = None;
    for (i, t) in input.tools.iter().enumerate() {
        let tid = format!("tool_{i}");
        let parent = if input.tasks.is_empty() {
            "agent".to_string()
        } else {
            let idx = task_idx.min(input.tasks.len() - 1);
            format!("task_{}", input.tasks[idx].0)
        };
        let val = if t.done { &t.result } else { &t.arg };
        nodes.push(Node {
            id: tid.clone(),
            kind: Kind::Tool,
            state: if t.done { State::Done } else { State::Running },
            label: t.name.clone(),
            col: 3,
            detail: vec![
                (if t.done { "result" } else { "args" }.into(),
                 val.chars().take(160).collect()),
            ],
            last: t.name.clone(),
        });
        edges.push(solid(&parent, &tid));
        if produces(&t.name, &t.arg) {
            result_from = Some(tid.clone());
        }
        if t.name == "plan_set" {
            task_idx += 1;
        }
    }

    // nó de resultado: só quando algo foi produzido
    if let Some(src) = result_from {
        nodes.push(Node {
            id: "result".into(),
            kind: Kind::Result,
            state: if input.busy { State::Running } else { State::Done },
            label: "result".into(),
            col: 4,
            detail: vec![("is".into(), "what the turn is building".into())],
            last: "the turn flows toward this".into(),
        });
        edges.push(Edge {
            from: src,
            to: "result".into(),
            kind: EdgeKind::Solid,
        });
    }

    // agentes do swarm, quando houver
    for (name, state, task) in input.swarm {
        let nid = format!("sw_{name}");
        let st = match state.as_str() {
            "running" => State::Running,
            "done" => State::Done,
            "error" => State::Error,
            "stopped" => State::Idle,
            _ => State::Idle,
        };
        nodes.push(Node {
            id: nid.clone(),
            kind: Kind::Swarm,
            state: st,
            label: name.clone(),
            col: 2,
            detail: vec![("worker".into(), name.clone()), ("task".into(), task.clone())],
            last: task.clone(),
        });
        edges.push(solid("agent", &nid));
    }

    Graph { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str, arg: &str, done: bool) -> ToolEvt {
        ToolEvt {
            name: name.into(),
            arg: arg.into(),
            result: if done { "ok".into() } else { String::new() },
            done,
        }
    }

    fn base<'a>(tools: &'a [ToolEvt], tasks: &'a [(String, String, String)]) -> Input<'a> {
        Input {
            chat_label: "mole GUI",
            busy: true,
            llm_name: "grok",
            llm_model: "grok-4.5",
            checkpoint: true,
            guard: true,
            compaction: true,
            tools,
            tasks,
            swarm: &[],
        }
    }

    #[test]
    fn nucleo_sempre_existe_com_a_infra_ligada() {
        let g = build(&base(&[], &[]));
        for id in ["you", "harness", "agent", "llm", "ck", "gd", "cp"] {
            assert!(g.nodes.iter().any(|n| n.id == id), "faltou {id}");
        }
        // harness → agente é o caminho de trás
        assert!(g.edges.iter().any(|e| e.from == "harness" && e.to == "agent"));
        // agente ↔ LLM é fluxo animado
        assert!(g
            .edges
            .iter()
            .any(|e| e.to == "llm" && e.kind == EdgeKind::Flow));
    }

    #[test]
    fn infra_desligada_some_do_grafo() {
        let mut i = base(&[], &[]);
        i.guard = false;
        let g = build(&i);
        assert!(!g.nodes.iter().any(|n| n.id == "gd"), "guard off não entra");
        assert!(g.nodes.iter().any(|n| n.id == "ck"));
    }

    #[test]
    fn tool_que_escreve_cria_o_no_de_resultado() {
        let tools = vec![ev("read_file", "a.rs", true), ev("write_file", "b.rs", true)];
        let g = build(&base(&tools, &[]));
        assert!(g.nodes.iter().any(|n| n.id == "result"));
        // read puro não produz
        let g2 = build(&base(&[ev("read_file", "a.rs", true)], &[]));
        assert!(!g2.nodes.iter().any(|n| n.id == "result"));
    }

    #[test]
    fn tools_penduram_na_tarefa_aberta_ate_o_plan_set() {
        let tasks = vec![
            ("1".into(), "analyze".into(), "done".into()),
            ("2".into(), "build".into(), "running".into()),
        ];
        let tools = vec![
            ev("read_file", "x", true),   // tool_0 → task_1
            ev("plan_set", "{id:1}", true), // tool_1 → task_1, depois avança
            ev("run_command", "mkdir y", true), // tool_2 → task_2
        ];
        let g = build(&base(&tools, &tasks));
        let edge_of = |tid: &str| {
            g.edges
                .iter()
                .find(|e| e.to == tid)
                .map(|e| e.from.clone())
                .unwrap()
        };
        assert_eq!(edge_of("tool_0"), "task_1");
        assert_eq!(edge_of("tool_2"), "task_2", "depois do plan_set cai na task 2");
    }

    #[test]
    fn sem_plano_as_tools_penduram_no_agente() {
        let g = build(&base(&[ev("read_file", "x", true)], &[]));
        assert!(g.edges.iter().any(|e| e.from == "agent" && e.to == "tool_0"));
    }
}
