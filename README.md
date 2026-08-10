# harness v0.5

Desktop AI agent for code, docs, spreadsheets and PDFs. **One Rust binary**, GUI
in eframe/egui. No external runtime, no Python, no background service to install.

Linux · macOS · Windows.

```bash
export XAI_API_KEY=...        # or OPENAI_API_KEY / MODEL_API_KEY
cargo run --release           # GUI
./scripts/bundle-macos.sh     # release + target/harness.app (macOS)
```

---

## Architecture

**Two processes.** The GUI (`app.rs`) talks to a **daemon** (`harness serve`)
over a unix/TCP socket in NDJSON. The daemon is spawned on demand and outlives
the GUI, so sessions keep running when you close the window — reattach later and
the stream is still there.

**The agent runs in the daemon**, not in the GUI: turns, tool rounds, approvals
and swarm workers all live there. Anything the UI shows about a turn crossed the
protocol to get there.

```
GUI (app.rs) ──NDJSON socket──> daemon.rs ──> agent.rs ──> tools/ · swarm.rs · llm.rs
```

| File | Role |
|---|---|
| `app.rs` | Whole GUI: rail, chat list, chat, panels, ⌘K, Settings |
| `daemon.rs` · `daemon_client.rs` · `protocol.rs` | Multi-session, multi-client |
| `agent.rs` | Turn loop, tool rounds, approvals |
| `llm.rs` · `llm_responses.rs` · `llm_pool.rs` | Chat Completions, Responses API, pool/failover |
| `graph.rs` | Structural project index (symbols/imports/refs/clusters) + impact |
| `gauntlet.rs` · `tokenless.rs` | Gauntlet Loop, Token Less Cost |
| `session.rs` · `memory.rs` · `skills.rs` | Sessions, lexical vector memory, versioned skills |
| `theme.rs` · `ui.rs` · `icon.rs` · `md.rs` | Paper/Ember palettes, widgets, mark, chat markdown |

---

## CLI

```
harness                  GUI
harness serve [--tcp]    start the multi-client daemon
harness connect          attach a REPL to the daemon
harness run <prompt>     one-shot through the daemon (starts it if needed)
harness doctor           provider / env check
harness self-dev build|status|reload
harness session list | info | create [title] | attach <id> | kill <id> [--delete]
```

After changing the protocol, kill the old daemon or the new GUI will not talk to
it: `pkill -f "harness serve"`.

---

## The window

A 68px **rail** on the left switches destinations: **USAGE · CHAT · FILES ·
GRAPH · MEM · SWARM · DIAG · WEB**, plus Settings at the foot. A dot on an item
means there is work or something stale there.

| Shortcut | Action |
|---|---|
| `⌘Enter` | Send |
| `⌘K` | Command palette (chats, files, actions, LLM switch) |
| `⌘N` | New chat |
| `⇧⌘D` | Toggle theme — Paper (light) / Ember (dark) |
| `Esc` | Close palette / panel / usage |

**Modes.** *Code* and *Office* expose different tool sets and prompts.

**Usage panel.** Live token send/receive charts (separate Sent and Received),
cache hits, cost by origin. Pin it to keep it open across chats.

**Status bar.** Chat folder · daemon `live/max` · Gauntlet counter · graph
coverage · Token Less level · memory footprint. The graph and Token Less chips
are clickable.

---

## Chats and projects

Every chat gets its own sandbox folder `{workspace}/{timestamp}/`. Point a chat
at a real codebase with `/project <path>` (no argument opens a picker) and the
agent works there instead.

**Pointing at a project turns on write approval.** Inside the chat folder the
agent writes freely; inside your project every write asks first.

Chats can be renamed (`/rename`), pinned (`/pin`), deleted (`/delete` — the
generated files stay) and searched from ⌘K. Titles are summarized from your
first message unless you lock one in.

---

## Features

### Token Less Cost
Compresses the **output**: a directive in the system prompt at four levels —
`off · lite · full · ultra` — set per chat from the composer pill or
`/tokenless`. Code and commands are left intact; prose gets shorter. Measured
savings show in the status bar and usage panel.

### Structural graph
`graph.rs` indexes the project into SQLite: symbols, imports, references and
label-propagation clusters over weighted edges. It compresses the **input** — the
agent reads a subgraph instead of whole files.

- `/graph build` — index the current project
- `/graph <query>` — look up symbols
- `/graph impact <symbol>` — what breaks if this changes

The graph is **per session**: its root is the chat's project, not the global
workspace.

Token Less shrinks the output, the graph shrinks the input. They are orthogonal.

### Gauntlet Loop
A composer toggle that makes the model split a goal into separately-judgeable
parts, critique each one as a severe critic, and redo what fails.

When it is on, a directive is injected into the system prompt and, at the end of
each turn, harness auto-sends `continue o loop` while the reply does not carry
the `[GAUNTLET:DONE]` marker — up to `gauntlet_max_iterations` (default 10).
Turning the toggle off stops the loop immediately; the status bar shows
`gauntlet <n>/<max>`. Typing your own message starts a new goal and resets the
counter.

No sub-agents, no queues, no extra processes: it is prompt injection +
auto-continue + a ceiling.

### Swarm
Parallel workers inside the daemon (`swarm_max_workers`, 1–3) with a shared
plan: propose, assign, claim, complete. Workers can use a different endpoint from
the main chat (`llm_multi_worker`). Live state reaches the GUI through
`ServerMsg::RuntimeInfo`.

### Memory
SQLite store with **lexical** embeddings — bag-of-words with hashing plus
trigrams, no ONNX, no download. Store, search, auto-recall into the turn, plus a
memory graph with ambient consolidation.

`/remember <text>` · `/mem <query>` · `/ambient`

### Skills
Reusable instruction files the agent can load, save and roll back —
`skill_list`, `skill_load`, `skill_save`, `skill_versions`. Saving versions the
previous copy instead of overwriting it.

### Multi-LLM
An endpoint pool with weights, auto-failover on 429/quota/overload, and optional
weighted rotation on a timer. Two wire formats are supported and picked
automatically: **Chat Completions** and the **Responses API** (Meta Muse).

`/llm list | use <name> | failover | weights | rotate_on|off | every <min>`

See [docs/META_SETUP.md](docs/META_SETUP.md) and
[docs/GROK_SETUP.md](docs/GROK_SETUP.md).

### Web and server
Built-in static HTTP server (`127.0.0.1:8765`) for local web apps, a WebView
window, and page fetch/preview — `/serve [path] [port]`, `/stopserve`,
`/web [url]`, or the `web_server_*` and `browser_*` tools.

### Also
MCP client (`mcp_connect` / `mcp_call`), background jobs (`bg_start`, `bg_poll`),
git introspection and worktrees, patch application, agentic grep, diagnostics,
plan tracking, session search, transcript import, and `self-dev` — harness
rebuilding and reloading itself.

---

## Slash commands

```
/help /clear /folder /root /status
/code /office
/model <name>  /profile <name>  /llm ...
/tokenless [off|lite|full|ultra]
/graph [build|impact <symbol>|<query>]
/project [path|off]  /rename <title>  /pin [off]  /delete
/mem <query>  /remember <text>  /ambient
/usage  /compact  /sessions  /swarm  /diag
/serve [path] [port]  /stopserve  /web [url]  /side
```

Portuguese aliases stay valid (`/apagar`, `/renomear`, `/grafo`).

---

## Agent tools

| Group | Tools |
|---|---|
| Files | `read_file` `write_file` `replace_in_file` `multiedit` `apply_patch` `list_dir` `glob_files` `search` `agentgrep` `workspace_tree` `show_file` `preview_file` |
| Shell / jobs | `run_command` `bg_start` `bg_poll` `bg_list` `bg_kill` |
| Git | `git_status` `git_diff` `git_log` `git_worktree_add` |
| Graph | `graph_build` `graph_query` `graph_stats` `graph_impact` |
| Office | `create_doc` `create_sheet` `create_pdf` |
| Memory | `memory_store` `memory_search` `memory_list` `memory_delete` `memory_consolidate` `memory_graph_add` `memory_graph_status` `ambient_start` `ambient_stop` |
| Skills | `skill_list` `skill_load` `skill_save` `skill_versions` |
| Swarm | `swarm_spawn` `swarm_list` `swarm_message` `swarm_wait` `swarm_stop` `swarm_plan_propose` `swarm_plan_assign` `swarm_plan_next` `swarm_plan_complete` `swarm_plan_show` |
| Plan | `plan_add` `plan_list` `plan_set` `note` |
| Web | `web_server_start` `web_server_status` `web_server_stop` `browser_open` `browser_fetch` |
| MCP | `mcp_connect` `mcp_list` `mcp_call` `mcp_disconnect` |
| Meta | `usage` `get_diagnostics` `provider_profile` `session_search` `resume_import` `selfdev` `side_panel` `clear` |

---

## Config

`~/Library/Application Support/sh.harness.harness/config.toml` (macOS).

```toml
api_base = "https://api.x.ai/v1"
api_key = "..."
model = "grok-4.5"
workspace = "/Users/you/Documents/Harness"
mode = "Code"                    # Code | Office
theme = "Paper"                  # Paper | Ember

token_less = "lite"              # default for new chats
gauntlet_max_iterations = 10     # auto-continue ceiling (1–100)
usage_pinned = false

history_cap = 28                 # LLM messages kept
tool_result_cap = 12000          # chars per tool result
auto_approve_safe = true
auto_approve_shell = false
stream = true

swarm_max_workers = 3            # 1–3
max_sessions = 32                # live sessions in the daemon
web_server_port = 8765
memory_auto_recall = true

llm_auto_failover = true
llm_multi_worker = true
llm_rotate_enabled = false
llm_rotate_minutes = 60
```

Environment: `XAI_API_KEY` / `GROK_API_KEY`, `OPENAI_API_KEY`, `MODEL_API_KEY`
(Meta), `HARNESS_MODEL`.

**Data.** `~/Library/Application Support/sh.harness.harness/` holds
`config.toml`, `memory.sqlite3`, the graph index and saved sessions. Generated
files go to your workspace, never next to the binary.

---

## Development

```bash
cargo test                      # 55 tests, 0 warnings — keep it that way
cargo build                     # debug (slow UI; iteration only)
./scripts/bundle-macos.sh       # release + target/harness.app
pkill -f "harness serve"        # required after protocol changes
```

Conventions: UI strings in en-US, code comments in Portuguese. No new
dependency without a strong reason — 32 crates today; syntax highlighting, the
graph, the icon and the fonts are all hand-rolled because of it. Every new field
on a struct crossing the protocol needs `#[serde(default)]`, or an old daemon
kills the new GUI's connection with `missing field X`. Tests assert intent, not
coordinates.

Brand: terracotta `#d97757`, geometric **H**. `icon.rs` draws both the app icon
and the in-app mark from the same geometry.
