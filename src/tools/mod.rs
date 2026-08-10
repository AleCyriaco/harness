mod agentgrep;
mod code;
mod doc;
mod pdf;
mod sheet;

use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::modes::AppMode;

pub fn tool_schemas(mode: AppMode) -> Vec<Value> {
    match mode {
        AppMode::Code => code_tools(),
        AppMode::Office => office_tools(),
    }
}

fn code_tools() -> Vec<Value> {
    vec![
        fn_tool(
            "workspace_tree",
            "Show a shallow directory tree (skips target/.git/node_modules). Low RAM.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "default '.'"},
                    "max_depth": {"type": "integer", "description": "1-6, default 3"}
                }
            }),
        ),
        fn_tool(
            "list_dir",
            "List files in a directory (capped).",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            }),
        ),
        fn_tool(
            "glob_files",
            "Find files by simple pattern (e.g. *.rs, src/*).",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"}
                },
                "required": ["pattern"]
            }),
        ),
        fn_tool(
            "search",
            "Search text; returns matches + nearby symbol outline. Streams files.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "path_contains": {"type": "string"}
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "agentgrep",
            "Structure-aware grep with adaptive truncation (jcode-style).",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "path_contains": {"type": "string"},
                    "max_hits": {"type": "integer"}
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "multiedit",
            "Apply multiple unique replacements across one or more files.",
            json!({
                "type": "object",
                "properties": {
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string"},
                                "old_string": {"type": "string"},
                                "new_string": {"type": "string"}
                            },
                            "required": ["path", "old_string", "new_string"]
                        }
                    }
                },
                "required": ["edits"]
            }),
        ),
        fn_tool(
            "resume_import",
            "Import transcript from Claude/Codex/OpenCode/pi export (json/jsonl).",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        ),
        fn_tool(
            "selfdev",
            "Self-dev: status|build|reload of harness source.",
            json!({
                "type": "object",
                "properties": {"action": {"type": "string"}},
                "required": ["action"]
            }),
        ),
        fn_tool(
            "memory_graph_add",
            "Add a node to the memory graph (links related).",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "kind": {"type": "string"}
                },
                "required": ["text"]
            }),
        ),
        fn_tool(
            "memory_consolidate",
            "Consolidate memory graph (dedupe stale).",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "memory_graph_status",
            "Memory graph summary.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "ambient_start",
            "Start ambient background consolidation loop.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "ambient_stop",
            "Stop ambient loop.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "swarm_plan_propose",
            "Propose shared DAG plan for swarm_id.",
            json!({
                "type": "object",
                "properties": {
                    "swarm_id": {"type": "string"},
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "title": {"type": "string"},
                                "depends_on": {"type": "array", "items": {"type": "string"}}
                            },
                            "required": ["id", "title"]
                        }
                    }
                },
                "required": ["swarm_id", "tasks"]
            }),
        ),
        fn_tool(
            "swarm_plan_show",
            "Show shared swarm plan.",
            json!({
                "type": "object",
                "properties": {"swarm_id": {"type": "string"}},
                "required": ["swarm_id"]
            }),
        ),
        fn_tool(
            "git_worktree_add",
            "Create a git worktree for isolated multi-agent work.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "branch": {"type": "string"}
                },
                "required": ["path", "branch"]
            }),
        ),
        fn_tool(
            "provider_profile",
            "Switch named provider profile (grok, openai, openrouter, deepseek, ollama…).",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        ),
        fn_tool(
            "usage",
            "Show token usage counters.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "read_file",
            "Read text with optional start_line/end_line windows.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer"},
                    "end_line": {"type": "integer"}
                },
                "required": ["path"]
            }),
        ),
        fn_tool(
            "write_file",
            "Create or overwrite a file.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        ),
        fn_tool(
            "replace_in_file",
            "Surgical single unique string replace; returns mini-diff.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        ),
        fn_tool(
            "apply_patch",
            "Apply multiple unique replacements to one file.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": {"type": "string"},
                                "new_string": {"type": "string"}
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
        ),
        fn_tool(
            "git_status",
            "git status -sb in workspace.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "git_diff",
            "git diff (optional path, optional staged). Output capped.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "staged": {"type": "boolean"}
                }
            }),
        ),
        fn_tool(
            "git_log",
            "Recent commits (oneline).",
            json!({
                "type": "object",
                "properties": {
                    "n": {"type": "integer", "description": "1-30, default 12"}
                }
            }),
        ),
        fn_tool(
            "run_command",
            "Shell in workspace (90s timeout, output capped). Prefer smallest verify command.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        ),
        fn_tool(
            "get_diagnostics",
            "Native diagnostics: cargo check, py_compile, tsc; optional rust-analyzer for a .rs path.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Optional path filter / rust file for RA"}
                }
            }),
        ),
        fn_tool(
            "preview_file",
            "Extract embedded text/table preview for docx/xlsx/pdf/text (for verification).",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        ),
        fn_tool(
            "graph_build",
            "Index the workspace into a structural graph (files, symbols, imports, call refs, \
             clusters). Costs no LLM tokens. Run once, then use graph_query instead of \
             repeated search+read_file.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "subfolder; default = workspace root"},
                    "full": {"type": "boolean", "description": "reindex everything (default: incremental)"}
                }
            }),
        ),
        fn_tool(
            "graph_query",
            "Ask the structural graph where something lives and what touches it. Returns a small \
             subgraph (symbol, file:line, who references it, cluster neighbours) instead of file \
             dumps. Prefer this over `search` for 'where is X' / 'what connects X to Y'.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "description": "max symbols, default 12"}
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "graph_impact",
            "Blast radius of a symbol: which files break if you change it, grouped by how \
             many reference hops away they are. Run this BEFORE editing a shared symbol.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": {"type": "string"},
                    "depth": {"type": "integer", "description": "reference hops, 1..6 (default 2)"}
                },
                "required": ["symbol"]
            }),
        ),
        fn_tool(
            "graph_stats",
            "Graph coverage: files, symbols, clusters, how many files drifted since the build.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "swarm_spawn",
            "Spawn a parallel worker agent on a subtask (max concurrent workers). Returns agent id.",
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string"},
                    "name": {"type": "string"}
                },
                "required": ["task"]
            }),
        ),
        fn_tool(
            "swarm_list",
            "List swarm agents and recent bus messages.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "swarm_message",
            "Post a message to a worker or broadcast (*).",
            json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "agent name/id or *"},
                    "message": {"type": "string"}
                },
                "required": ["to", "message"]
            }),
        ),
        fn_tool(
            "swarm_wait",
            "Block until spawned workers finish (or timeout) and return their summaries. \
             Use after swarm_spawn instead of guessing when they are done.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "agent id/name, or 'all' (default)"},
                    "timeout_s": {"type": "integer", "description": "1..600, default 120"}
                }
            }),
        ),
        fn_tool(
            "swarm_plan_assign",
            "Assign a plan task to an agent (marks it running).",
            json!({
                "type": "object",
                "properties": {
                    "swarm_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "agent": {"type": "string"}
                },
                "required": ["swarm_id", "task_id", "agent"]
            }),
        ),
        fn_tool(
            "swarm_plan_complete",
            "Mark a plan task done (unblocks its dependents).",
            json!({
                "type": "object",
                "properties": {
                    "swarm_id": {"type": "string"},
                    "task_id": {"type": "string"}
                },
                "required": ["swarm_id", "task_id"]
            }),
        ),
        fn_tool(
            "swarm_plan_next",
            "List plan tasks whose dependencies are satisfied — what can be spawned now.",
            json!({
                "type": "object",
                "properties": {"swarm_id": {"type": "string"}},
                "required": ["swarm_id"]
            }),
        ),
        fn_tool(
            "swarm_stop",
            "Stop a worker by id (or 'all').",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"}
                },
                "required": ["id"]
            }),
        ),
        fn_tool(
            "memory_store",
            "Store a durable memory in the local vector DB (SQLite + embeddings).",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "tags": {"type": "string", "description": "optional tags"}
                },
                "required": ["text"]
            }),
        ),
        fn_tool(
            "memory_search",
            "Search stored memories. Matching is lexical (hashed words + character \
             trigrams), not semantic: it finds wording you used before, not \
             paraphrases of it. Prefer the terms the note itself would use.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "memory_list",
            "List recent memories.",
            json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer"}
                }
            }),
        ),
        fn_tool(
            "memory_delete",
            "Delete a memory by id.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer"}
                },
                "required": ["id"]
            }),
        ),
        fn_tool(
            "web_server_start",
            "Start a tiny static HTTP server for testing web apps (serves a directory).",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory relative to workspace (default '.')"},
                    "port": {"type": "integer", "description": "default 8765"}
                }
            }),
        ),
        fn_tool(
            "web_server_stop",
            "Stop the local static web server.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "web_server_status",
            "Status of the local web server (url/port/root).",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "browser_open",
            "Open a URL in the harness internal WebView window (not external Safari/Chrome). Use for local server apps.",
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"}
                },
                "required": ["url"]
            }),
        ),
        fn_tool(
            "browser_fetch",
            "Fetch a URL and return it as clean markdown (headings, links and code kept; nav/cookie/footer chrome dropped).",
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"}
                },
                "required": ["url"]
            }),
        ),
        fn_tool(
            "web_crawl",
            "Crawl from a URL and return the pages as markdown. Breadth-first, capped by config (max pages/depth, same-domain, robots.txt).",
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "max_pages": {"type": "integer", "description": "optional, capped by config"},
                    "max_depth": {"type": "integer", "description": "optional, capped by config"}
                },
                "required": ["url"]
            }),
        ),
        // --- jcode-inspired extensions ---
        fn_tool(
            "side_panel",
            "Show content in the harness side panel (file path, note text, or clear).",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "description": "show_file | note | clear"},
                    "path": {"type": "string"},
                    "title": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["action"]
            }),
        ),
        fn_tool(
            "plan_add",
            "Add a todo/plan item for this chat.",
            json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"]
            }),
        ),
        fn_tool(
            "plan_list",
            "List plan/todo items.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "plan_set",
            "Mark plan item done/undone.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer"},
                    "done": {"type": "boolean"}
                },
                "required": ["id", "done"]
            }),
        ),
        fn_tool(
            "bg_start",
            "Start a background shell job (non-blocking).",
            json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
        ),
        fn_tool(
            "bg_poll",
            "Poll background job by id.",
            json!({
                "type": "object",
                "properties": {"id": {"type": "integer"}},
                "required": ["id"]
            }),
        ),
        fn_tool(
            "bg_list",
            "List background jobs.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "bg_kill",
            "Kill a background job.",
            json!({
                "type": "object",
                "properties": {"id": {"type": "integer"}},
                "required": ["id"]
            }),
        ),
        fn_tool(
            "skill_list",
            "List available skills under .harness/skills/.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "skill_save",
            "Create or update a skill. The previous body is archived as a version, so edits \
             are reversible. Frontmatter: description, triggers (when it applies), \
             not_when (when it must not), validate (how to know it worked).",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "markdown": {"type": "string", "description": "full file, frontmatter + body"}
                },
                "required": ["name", "markdown"]
            }),
        ),
        fn_tool(
            "skill_versions",
            "List archived versions of a skill, and restore one when `restore` is given. \
             Restoring makes the old body the newest version; nothing is overwritten.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "restore": {"type": "integer", "description": "version to bring back"}
                },
                "required": ["name"]
            }),
        ),
        fn_tool(
            "skill_load",
            "Load a skill markdown into context (returns body).",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        ),
        fn_tool(
            "session_search",
            "Search past chat transcripts (session RAG).",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "mcp_connect",
            "Connect an MCP server via stdio command.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "command": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["name", "command"]
            }),
        ),
        fn_tool(
            "mcp_list",
            "List MCP servers/tools.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "mcp_call",
            "Call an MCP tool.",
            json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "tool": {"type": "string"},
                    "arguments": {"type": "object"}
                },
                "required": ["server", "tool"]
            }),
        ),
        fn_tool(
            "mcp_disconnect",
            "Disconnect MCP server (or all).",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}}
            }),
        ),
    ]
}

fn office_tools() -> Vec<Value> {
    vec![
        fn_tool(
            "list_dir",
            "List files.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        ),
        fn_tool(
            "read_file",
            "Read text file.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer"},
                    "end_line": {"type": "integer"}
                },
                "required": ["path"]
            }),
        ),
        fn_tool(
            "write_file",
            "Write text file.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        ),
        fn_tool(
            "run_command",
            "Shell command.",
            json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
        ),
        fn_tool(
            "create_doc",
            "Create Word .docx",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "title": {"type": "string"},
                    "paragraphs": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["path", "title", "paragraphs"]
            }),
        ),
        fn_tool(
            "create_sheet",
            "Create Excel .xlsx",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "headers": {"type": "array", "items": {"type": "string"}},
                    "rows": {
                        "type": "array",
                        "items": {"type": "array", "items": {"type": "string"}}
                    },
                    "sheet_name": {"type": "string"}
                },
                "required": ["path", "headers", "rows"]
            }),
        ),
        fn_tool(
            "create_pdf",
            "Create PDF",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "title": {"type": "string"},
                    "paragraphs": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["path", "title", "paragraphs"]
            }),
        ),
        fn_tool(
            "preview_file",
            "Preview docx/xlsx/pdf/text content inside harness.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        ),
        fn_tool(
            "memory_store",
            "Store a durable memory in the local vector DB.",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "tags": {"type": "string"}
                },
                "required": ["text"]
            }),
        ),
        fn_tool(
            "memory_search",
            "Search local vector memories.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "web_server_start",
            "Start static server for web app folder.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "port": {"type": "integer"}
                }
            }),
        ),
        fn_tool(
            "web_server_stop",
            "Stop static web server.",
            json!({"type": "object", "properties": {}}),
        ),
        fn_tool(
            "browser_open",
            "Open URL in harness internal WebView (not external browser).",
            json!({
                "type": "object",
                "properties": {"url": {"type": "string"}},
                "required": ["url"]
            }),
        ),
    ]
}

fn fn_tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters
        }
    })
}

/// Atalho para chamadas fora de um turno do agente (testes, preview).
#[allow(dead_code)]
pub fn dispatch_no_cancel(
    cfg: &Config,
    mode: AppMode,
    name: &str,
    args_json: &str,
) -> Result<String> {
    static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    dispatch(cfg, mode, name, args_json, &NEVER)
}

pub fn dispatch(
    cfg: &Config,
    mode: AppMode,
    name: &str,
    args_json: &str,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String> {
    let args: Value = serde_json::from_str(args_json).unwrap_or_else(|_| json!({}));
    let root = &cfg.workspace;

    // Um worker só escreve num arquivo que ninguém vivo esteja escrevendo.
    if matches!(name, "write_file" | "replace_in_file" | "apply_patch") {
        if let Some(rel) = args.get("path").and_then(|v| v.as_str()) {
            if let Err(e) = crate::swarm::claim_path(rel) {
                bail!("{e}");
            }
        }
    }

    if mode == AppMode::Code
        && matches!(name, "create_doc" | "create_sheet" | "create_pdf")
    {
        bail!("tool '{name}' only in Office mode");
    }
    if mode == AppMode::Office
        && matches!(
            name,
            "search"
                | "replace_in_file"
                | "apply_patch"
                | "glob_files"
                | "workspace_tree"
                | "git_status"
                | "git_diff"
                | "git_log"
                | "get_diagnostics"
                | "swarm_spawn"
                | "swarm_list"
                | "swarm_message"
                | "swarm_stop"
                | "swarm_wait"
                | "swarm_plan_assign"
                | "swarm_plan_complete"
                | "swarm_plan_next"
                | "graph_build"
                | "graph_query"
                | "graph_stats"
                | "graph_impact"
        )
    {
        bail!("tool '{name}' only in Code mode");
    }

    let raw = match name {
        "list_dir" => {
            let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            code::list_dir(root, rel)
        }
        "workspace_tree" => {
            let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            code::workspace_tree(root, rel, depth)
        }
        "glob_files" => {
            let pat = require_str(&args, "pattern")?;
            code::glob_files(root, pat)
        }
        "search" => {
            let q = require_str(&args, "query")?;
            let filter = args.get("path_contains").and_then(|v| v.as_str());
            code::search(root, q, filter)
        }
        "agentgrep" => {
            let q = require_str(&args, "query")?;
            let filter = args.get("path_contains").and_then(|v| v.as_str());
            let max = args.get("max_hits").and_then(|v| v.as_u64()).unwrap_or(40) as usize;
            agentgrep::agentgrep(root, q, filter, max)
        }
        "read_file" => {
            let rel = require_str(&args, "path")?;
            let start = args.get("start_line").and_then(|v| v.as_u64()).map(|n| n as usize);
            let end = args.get("end_line").and_then(|v| v.as_u64()).map(|n| n as usize);
            crate::file_watch::note_read("main", rel);
            agentgrep::note_read_path(root, rel);
            code::read_file(root, rel, start, end)
        }
        "write_file" => {
            let rel = require_str(&args, "path")?;
            let content = require_str(&args, "content")?;
            let r = code::write_file(root, rel, content)?;
            if let Some(n) = crate::file_watch::note_write("main", rel) {
                crate::swarm::global_swarm()
                    .lock()
                    .ok()
                    .map(|mut g| g.post("system", "*", &format!("file {} edited; readers: {:?}", n.path, n.readers)));
            }
            if let Ok(path) = safe_join(root, rel) {
                crate::side_panel::set_file(path.clone(), content.to_string());
                if rel.ends_with(".html") || rel.ends_with(".htm") {
                    let _ = crate::preview::open_html_as_web_preview(&path);
                }
            }
            Ok(r)
        }
        "replace_in_file" => {
            let rel = require_str(&args, "path")?;
            let old = require_str(&args, "old_string")?;
            let new = require_str(&args, "new_string")?;
            let r = code::replace_in_file(root, rel, old, new)?;
            let _ = crate::file_watch::note_write("main", rel);
            if let Ok(path) = safe_join(root, rel) {
                if let Ok(body) = std::fs::read_to_string(&path) {
                    crate::side_panel::set_file(path.clone(), body);
                }
                crate::side_panel::set_diff(rel, format!("replaced in {rel}\n- {old}\n+ {new}"));
                if rel.ends_with(".html") || rel.ends_with(".htm") {
                    let _ = crate::preview::open_html_as_web_preview(&path);
                }
            }
            Ok(r)
        }
        "multiedit" => {
            let edits = args
                .get("edits")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow::anyhow!("edits required"))?;
            let mut reports = Vec::new();
            for e in edits {
                let rel = e.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("path"))?;
                let old = e.get("old_string").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("old"))?;
                let new = e.get("new_string").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("new"))?;
                let r = code::replace_in_file(root, rel, old, new)?;
                let _ = crate::file_watch::note_write("main", rel);
                reports.push(r);
            }
            Ok(reports.join("\n"))
        }
        "resume_import" => {
            let rel = require_str(&args, "path")?;
            let path = safe_join(root, rel)?;
            let imp = crate::resume_import::import_path(&path)?;
            // stash into side panel for user review
            let preview = imp
                .messages
                .iter()
                .filter_map(|m| m.content.as_ref().map(|c| format!("{}: {}", m.role, c.chars().take(120).collect::<String>())))
                .take(30)
                .collect::<Vec<_>>()
                .join("\n");
            crate::side_panel::set_note(
                &format!("import {}", imp.source),
                format!("{}\n\n{preview}", imp.note),
            );
            Ok(format!(
                "imported {} ({} msgs). Review side panel; messages not auto-merged — use as context.",
                imp.source,
                imp.messages.len()
            ))
        }
        "selfdev" => {
            let action = require_str(&args, "action")?;
            crate::selfdev::tool_selfdev(action, root)
        }
        "memory_graph_add" => {
            let text = require_str(&args, "text")?;
            let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("fact");
            let id = crate::memory_graph::add_node(text, kind)?;
            Ok(format!("graph node #{id}"))
        }
        "memory_consolidate" => crate::memory_graph::consolidate(),
        "memory_graph_status" => crate::memory_graph::graph_summary(),
        "ambient_start" => {
            crate::memory_graph::ambient_start();
            Ok(crate::memory_graph::ambient_status())
        }
        "ambient_stop" => {
            crate::memory_graph::ambient_stop();
            Ok("ambient stopped".into())
        }
        "swarm_plan_propose" => {
            let sid = require_str(&args, "swarm_id")?;
            let tasks_v = args
                .get("tasks")
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();
            let mut tasks = Vec::new();
            for t in tasks_v {
                let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let deps = t
                    .get("depends_on")
                    .and_then(|d| d.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                tasks.push(crate::swarm_plan::PlanTask {
                    id,
                    title,
                    status: "pending".into(),
                    assignee: None,
                    depends_on: deps,
                });
            }
            let plan = crate::swarm_plan::propose(sid, tasks);
            Ok(format!("plan v{} with {} tasks", plan.version, plan.tasks.len()))
        }
        "swarm_plan_show" => {
            let sid = require_str(&args, "swarm_id")?;
            Ok(crate::swarm_plan::format(sid))
        }
        "git_worktree_add" => {
            let path = require_str(&args, "path")?;
            let branch = require_str(&args, "branch")?;
            let out = std::process::Command::new("git")
                .args(["worktree", "add", "-b", branch, path])
                .current_dir(root)
                .output()?;
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            if !out.status.success() {
                bail!("git worktree failed: {s}");
            }
            Ok(s)
        }
        "provider_profile" => {
            let name = require_str(&args, "name")?;
            Ok(format!(
                "Switch in chat with /profile {name} or /llm use {name}. Pool:\n{}",
                crate::llm_pool::list_text(cfg)
            ))
        }
        "usage" => Ok(format!(
            "{}\n{}",
            crate::provider_doctor::usage_summary(),
            crate::llm_pool::list_text(cfg)
        )),
        "apply_patch" => {
            let rel = require_str(&args, "path")?;
            let edits = parse_edits(&args)?;
            code::apply_patch(root, rel, &edits)
        }
        "git_status" => code::git_status(root),
        "git_diff" => {
            let path = args.get("path").and_then(|v| v.as_str());
            let staged = args
                .get("staged")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            code::git_diff(root, path, staged)
        }
        "git_log" => {
            let n = args.get("n").and_then(|v| v.as_u64()).unwrap_or(12) as usize;
            code::git_log(root, n)
        }
        "run_command" => {
            let cmd = require_str(&args, "command")?;
            code::run_command(root, cmd)
        }
        "get_diagnostics" => {
            let filter = args.get("path").and_then(|v| v.as_str());
            let snap = crate::diagnostics::run_workspace_diagnostics(root, filter);
            crate::diagnostics::store_snapshot(snap.clone());
            Ok(crate::diagnostics::format_snapshot(&snap))
        }
        "preview_file" => {
            let rel = require_str(&args, "path")?;
            let path = safe_join(root, rel)?;
            Ok(format_preview(&crate::preview::preview_path(&path)))
        }
        "swarm_spawn" => {
            let task = require_str(&args, "task")?;
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let swarm = crate::swarm::global_swarm();
            // enforce config max
            {
                let g = swarm.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
                let running = g.running_count();
                if running >= cfg.swarm_max_workers {
                    bail!(
                        "swarm at max running workers ({})",
                        cfg.swarm_max_workers
                    );
                }
            }
            let mut worker_cfg = cfg.clone();
            worker_cfg.auto_approve_shell = true; // unattended workers
            match crate::swarm::Swarm::spawn_worker(swarm, worker_cfg, name, task.to_string()) {
                Ok(info) => Ok(format!(
                    "spawned {} id={} task={}",
                    info.name, info.id, info.task
                )),
                Err(e) => bail!("{e}"),
            }
        }
        "graph_build" => {
            let sub = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let target = if sub.is_empty() {
                root.clone()
            } else {
                safe_join(root, sub)?
            };
            let t0 = std::time::Instant::now();
            let st = crate::graph::build(&target, !full)?;
            crate::metrics::record_graph_build(st.files, t0.elapsed().as_millis() as u64);
            Ok(format!(
                "graph ready in {} ms: {} files, {} symbols, {} refs, {} clusters ({} KB indexed)",
                t0.elapsed().as_millis(),
                st.files,
                st.symbols,
                st.edges,
                st.clusters,
                st.indexed_bytes / 1024
            ))
        }
        "graph_query" => {
            let q = require_str(&args, "query")?;
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(12)
                .clamp(1, 50) as usize;
            let res = crate::graph::query(root, q, limit, cfg.tool_result_cap as u64)?;
            crate::metrics::record_graph_query(res.saved_tokens());
            Ok(res.render())
        }
        "graph_impact" => {
            let symbol = require_str(&args, "symbol")?;
            let depth = args
                .get("depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(2)
                .clamp(1, 6) as usize;
            Ok(crate::graph::impact(root, symbol, depth)?.render())
        }
        "graph_stats" => {
            let st = crate::graph::stats(root, true)?;
            Ok(format!(
                "graph of {}: {} files, {} symbols, {} refs, {} clusters\nbuild: {}\n{} file(s) changed since then{}",
                st.root,
                st.files,
                st.symbols,
                st.edges,
                st.clusters,
                if st.built_at.is_empty() { "never" } else { &st.built_at },
                st.stale_files,
                if st.stale_files > 0 { " — run graph_build" } else { "" }
            ))
        }
        "swarm_list" => Ok(crate::swarm::summary_text()),
        "swarm_wait" => {
            let who = args.get("id").and_then(|v| v.as_str()).unwrap_or("all");
            let secs = args
                .get("timeout_s")
                .and_then(|v| v.as_u64())
                .unwrap_or(120)
                .clamp(1, 600);
            Ok(crate::swarm::wait_for(
                who,
                std::time::Duration::from_secs(secs),
                cancel,
            ))
        }
        "swarm_plan_assign" => {
            let sid = require_str(&args, "swarm_id")?;
            let task = require_str(&args, "task_id")?;
            let agent = require_str(&args, "agent")?;
            crate::swarm_plan::assign(sid, task, agent).map_err(|e| anyhow::anyhow!(e))
        }
        "swarm_plan_complete" => {
            let sid = require_str(&args, "swarm_id")?;
            let task = require_str(&args, "task_id")?;
            crate::swarm_plan::complete(sid, task).map_err(|e| anyhow::anyhow!(e))
        }
        "swarm_plan_next" => {
            let sid = require_str(&args, "swarm_id")?;
            let next = crate::swarm_plan::next_runnable(sid);
            if next.is_empty() {
                return Ok(format!("plan {sid}: nothing runnable"));
            }
            Ok(next
                .iter()
                .map(|t| format!("- {} {} deps={:?}", t.id, t.title, t.depends_on))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "swarm_message" => {
            let to = require_str(&args, "to")?;
            let message = require_str(&args, "message")?;
            let swarm = crate::swarm::global_swarm();
            let mut g = swarm.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            g.post("coordinator", to, message);
            Ok(format!("posted to {to}"))
        }
        "swarm_stop" => {
            let id = require_str(&args, "id")?;
            let swarm = crate::swarm::global_swarm();
            let mut g = swarm.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            if id == "all" {
                g.stop_all();
                Ok("stopped all agents".into())
            } else {
                let target = g
                    .list()
                    .into_iter()
                    .find(|a| a.id == id || a.id.starts_with(id) || a.name == id);
                if let Some(a) = target {
                    Ok(g.stop(&a.id))
                } else {
                    Ok(g.stop(id))
                }
            }
        }
        "memory_store" => {
            let text = require_str(&args, "text")?;
            let tags = args.get("tags").and_then(|v| v.as_str()).unwrap_or("");
            let id = crate::memory::with_store(|s| s.store(text, tags))?;
            Ok(format!("stored memory #{id}"))
        }
        "memory_search" => {
            let q = require_str(&args, "query")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
            let hits = crate::memory::with_store(|s| s.search(q, limit))?;
            Ok(crate::memory::format_hits(&hits))
        }
        "memory_list" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let hits = crate::memory::with_store(|s| s.list_recent(limit))?;
            Ok(crate::memory::format_hits(&hits))
        }
        "memory_delete" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("missing id"))?;
            let ok = crate::memory::with_store(|s| s.delete(id))?;
            Ok(if ok {
                format!("deleted #{id}")
            } else {
                format!("memory #{id} not found")
            })
        }
        "web_server_start" => {
            let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let port = args
                .get("port")
                .and_then(|v| v.as_u64())
                .unwrap_or(cfg.web_server_port as u64) as u16;
            let dir = safe_join(root, rel)?;
            let st = crate::webserver::start(dir, port)?;
            crate::browser::set_url(&st.url);
            // Open project in harness WebView (not external browser)
            let view = match crate::browser::open_in_app(&st.url) {
                Ok(()) => "opened in harness WebView".to_string(),
                Err(e) => format!("webview note: {e}"),
            };
            Ok(format!(
                "server running at {} root={} · {}",
                st.url,
                st.root.display(),
                view
            ))
        }
        "web_server_stop" => {
            crate::webserver::stop();
            Ok("web server stopped".into())
        }
        "web_server_status" => {
            let st = crate::webserver::status();
            Ok(format!(
                "running={} port={} url={} root={} err={}",
                st.running,
                st.port,
                st.url,
                st.root.display(),
                st.last_error
            ))
        }
        "browser_open" => {
            let url = require_str(&args, "url")?;
            crate::browser::open_in_app(url)?;
            Ok(format!("opened {url} in harness WebView"))
        }
        "web_crawl" => {
            let url = require_str(&args, "url")?;
            let cap_pages = cfg.web_crawl_max_pages as usize;
            let cap_depth = cfg.web_crawl_max_depth as usize;
            let opts = crate::browser::CrawlOpts {
                max_pages: args
                    .get("max_pages")
                    .and_then(|v| v.as_u64())
                    .map(|v| (v as usize).min(cap_pages))
                    .unwrap_or(cap_pages)
                    .max(1),
                max_depth: args
                    .get("max_depth")
                    .and_then(|v| v.as_u64())
                    .map(|v| (v as usize).min(cap_depth))
                    .unwrap_or(cap_depth),
                same_domain: cfg.web_crawl_same_domain,
                respect_robots: cfg.web_respect_robots,
                per_page: (cfg.tool_result_cap / 3).max(1_000),
            };
            crate::browser::crawl(url, &opts, cancel)
        }
        "browser_fetch" => {
            let url = require_str(&args, "url")?;
            let st = crate::browser::fetch_preview(url, cfg.web_markdown)?;
            Ok(format!(
                "HTTP {} · {}\n{}\n\n{}",
                st.status_code,
                st.title,
                st.url,
                st.preview_text.chars().take(6000).collect::<String>()
            ))
        }
        "side_panel" => {
            let action = require_str(&args, "action")?;
            match action {
                "clear" => {
                    crate::side_panel::clear();
                    Ok("side panel cleared".into())
                }
                "note" => {
                    let title = args
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Note");
                    let content = require_str(&args, "content")?;
                    crate::side_panel::set_note(title, content.to_string());
                    Ok("side panel note set".into())
                }
                "show_file" => {
                    let rel = require_str(&args, "path")?;
                    let path = safe_join(root, rel)?;
                    let body = std::fs::read_to_string(&path)?;
                    let body = if body.len() > 80_000 {
                        format!("{}…\n[truncated]", &body[..80_000])
                    } else {
                        body
                    };
                    crate::side_panel::set_file(path, body);
                    Ok(format!("side panel showing {rel}"))
                }
                other => bail!("unknown side_panel action: {other}"),
            }
        }
        "plan_add" => {
            let text = require_str(&args, "text")?;
            let item = crate::plan::add(text)?;
            let _ = crate::plan::save(root);
            crate::plan::sync_side_panel();
            Ok(format!("added plan #{}: {}", item.id, item.text))
        }
        "plan_list" => {
            let _ = crate::plan::load(root);
            crate::plan::sync_side_panel();
            Ok(crate::plan::format())
        }
        "plan_set" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("id"))? as u32;
            let done = args
                .get("done")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let r = crate::plan::set_done(id, done)?;
            let _ = crate::plan::save(root);
            crate::plan::sync_side_panel();
            Ok(r)
        }
        "bg_start" => {
            let cmd = require_str(&args, "command")?;
            let j = crate::bg::start(root, cmd)?;
            Ok(format!("bg job #{} started: {}", j.id, j.command))
        }
        "bg_poll" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("id"))?;
            let j = crate::bg::poll(id)?;
            Ok(format!(
                "#{} [{}]\n{}",
                j.id, j.status, j.output_preview
            ))
        }
        "bg_list" => Ok(crate::bg::format_list()),
        "bg_kill" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("id"))?;
            crate::bg::kill(id)
        }
        "skill_list" => {
            let _ = crate::skills::ensure_default_skills(root);
            Ok(crate::skills::format_skills(&crate::skills::list_skills(root)))
        }
        "skill_save" => {
            let name = require_str(&args, "name")?;
            let markdown = require_str(&args, "markdown")?;
            let v = crate::skills::save_skill(root, name, markdown)?;
            Ok(format!(
                "skill {name} saved as v{v}{}",
                if v > 1 { " (previous archived)" } else { "" }
            ))
        }
        "skill_versions" => {
            let name = require_str(&args, "name")?;
            if let Some(v) = args.get("restore").and_then(|v| v.as_u64()) {
                let now = crate::skills::restore_skill(root, name, v as u32)?;
                return Ok(format!("{name}: v{v} restored as v{now}"));
            }
            let versions = crate::skills::skill_versions(root, name);
            let cur = crate::skills::load_skill(root, name)
                .map(|s| s.version)
                .unwrap_or(0);
            if versions.is_empty() {
                return Ok(format!("{name}: v{cur} (no earlier versions)"));
            }
            Ok(format!(
                "{name}: current v{cur} · archived {}",
                versions
                    .iter()
                    .map(|v| format!("v{v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
        "skill_load" => {
            let name = require_str(&args, "name")?;
            let _ = crate::skills::ensure_default_skills(root);
            match crate::skills::load_skill(root, name) {
                Some(s) => Ok(crate::skills::format_for_prompt(&s)),
                None => bail!("skill not found: {name}"),
            }
        }
        "session_search" => {
            let q = require_str(&args, "query")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let hits = crate::session_search::search(q, limit)?;
            Ok(crate::session_search::format_hits(&hits))
        }
        "mcp_connect" => {
            let name = require_str(&args, "name")?;
            let command = require_str(&args, "command")?;
            let args_v: Vec<String> = args
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let cfg = crate::mcp::McpServerConfig {
                name: name.to_string(),
                command: command.to_string(),
                args: args_v,
                env: Default::default(),
            };
            crate::mcp::connect(&cfg)
        }
        "mcp_list" => Ok(crate::mcp::summary()),
        "mcp_call" => {
            let server = require_str(&args, "server")?;
            let tool = require_str(&args, "tool")?;
            let arguments = args.get("arguments").cloned().unwrap_or(json!({}));
            crate::mcp::call_tool(server, tool, arguments)
        }
        "mcp_disconnect" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("all");
            if name == "all" {
                crate::mcp::disconnect_all();
                Ok("disconnected all MCP servers".into())
            } else {
                crate::mcp::disconnect(name);
                Ok(format!("disconnected {name}"))
            }
        }
        "create_doc" => {
            let rel = require_str(&args, "path")?;
            let title = require_str(&args, "title")?;
            let paragraphs = string_array(&args, "paragraphs");
            let path = resolve_write_path(root, rel)?;
            doc::create_docx(&path, title, &paragraphs)
        }
        "create_sheet" => {
            let rel = require_str(&args, "path")?;
            let headers = string_array(&args, "headers");
            let rows = rows_array(&args, "rows");
            let sheet_name = args
                .get("sheet_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Sheet1");
            let path = resolve_write_path(root, rel)?;
            sheet::create_xlsx(&path, sheet_name, &headers, &rows)
        }
        "create_pdf" => {
            let rel = require_str(&args, "path")?;
            let title = require_str(&args, "title")?;
            let paragraphs = string_array(&args, "paragraphs");
            let path = resolve_write_path(root, rel)?;
            pdf::create_pdf(&path, title, &paragraphs)
        }
        other => bail!("unknown tool: {other}"),
    }?;

    Ok(truncate_for_model(&raw, cfg.tool_result_cap))
}

fn parse_edits(args: &Value) -> Result<Vec<(String, String)>> {
    let arr = args
        .get("edits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing edits array"))?;
    let mut out = Vec::new();
    for (i, e) in arr.iter().enumerate() {
        let old = e
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edits[{i}].old_string"))?
            .to_string();
        let new = e
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edits[{i}].new_string"))?
            .to_string();
        out.push((old, new));
    }
    Ok(out)
}

fn truncate_for_model(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub(40);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…\n[tool result truncated to {max} chars]", &s[..end])
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing string arg '{key}'"))
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn rows_array(args: &Value, key: &str) -> Vec<Vec<String>> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| match row {
                    Value::Array(cells) => cells
                        .iter()
                        .map(|c| match c {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect(),
                    Value::String(s) => vec![s.clone()],
                    other => vec![other.to_string()],
                })
                .collect()
        })
        .unwrap_or_default()
}

fn format_preview(p: &crate::preview::PreviewContent) -> String {
    use crate::preview::PreviewContent;
    match p {
        PreviewContent::Text { title, body } => format!("# {title}\n\n{body}"),
        PreviewContent::Table {
            title,
            sheet,
            headers,
            rows,
            note,
        } => {
            let mut s = format!("# {title} ({sheet}) — {note}\n");
            s.push_str(&headers.join(" | "));
            s.push('\n');
            for r in rows.iter().take(40) {
                s.push_str(&r.join(" | "));
                s.push('\n');
            }
            s
        }
        PreviewContent::WebPage {
            title,
            path,
            url,
            source_preview,
        } => format!("# {title}\npath: {path}\nurl: {url}\n\n{source_preview}"),
        PreviewContent::Error { title, message } => format!("{title}: {message}"),
    }
}

pub fn resolve_write_path(root: &Path, rel: &str) -> Result<PathBuf> {
    let path = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        root.join(rel)
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}

pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let lexical = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        root.join(rel)
    };
    let lex = normalize_path(&lexical);
    let root_norm = normalize_path(root);
    if !lex.starts_with(&root_norm) {
        bail!("path escapes workspace: {rel}");
    }
    Ok(lexical)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod smoke {
    use super::*;
    use crate::modes::AppMode;

    #[test]
    fn code_and_office_tools() {
        let root = std::env::temp_dir().join(format!("harness-smoke-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = Config {
            workspace: root.clone(),
            mode: AppMode::Code,
            ..Config::default()
        };

        dispatch_no_cancel(
            &cfg,
            AppMode::Code,
            "write_file",
            r#"{"path":"a.rs","content":"fn main() {\n  println!(\"hi\");\n}\n"}"#,
        )
        .unwrap();

        let tree = dispatch_no_cancel(
            &cfg,
            AppMode::Code,
            "workspace_tree",
            r#"{"path":".","max_depth":2}"#,
        )
        .unwrap();
        assert!(tree.contains("a.rs"));

        let g = dispatch_no_cancel(&cfg, AppMode::Code, "glob_files", r#"{"pattern":"*.rs"}"#).unwrap();
        assert!(g.contains("a.rs"));

        let s = dispatch_no_cancel(&cfg, AppMode::Code, "search", r#"{"query":"main"}"#).unwrap();
        assert!(s.contains("a.rs"));

        dispatch_no_cancel(
            &cfg,
            AppMode::Code,
            "replace_in_file",
            r#"{"path":"a.rs","old_string":"hi","new_string":"hello"}"#,
        )
        .unwrap();

        dispatch_no_cancel(
            &cfg,
            AppMode::Code,
            "apply_patch",
            r#"{"path":"a.rs","edits":[{"old_string":"hello","new_string":"world"}]}"#,
        )
        .unwrap();

        let doc = dispatch_no_cancel(
            &cfg,
            AppMode::Office,
            "create_doc",
            r#"{"path":"t.docx","title":"T","paragraphs":["a"]}"#,
        )
        .unwrap();
        assert!(doc.contains("created docx"));

        let _ = std::fs::remove_dir_all(root);
    }
}
