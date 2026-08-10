# harness

Desktop AI agent for code, docs, spreadsheets and PDFs. **One Rust binary**, GUI
in eframe/egui. No external runtime, no Python, no service.

## Commands

```bash
cargo test                      # 55 tests, 0 warnings — keep it that way
cargo build                     # debug (slow UI; iteration only)
./scripts/bundle-macos.sh       # release + target/harness.app (use this to test)
./scripts/bundle-macos.sh debug # bundle from the debug binary
pkill -f "harness serve"        # kill the daemon (required after protocol changes)
```

CLI: `harness serve | run | connect | doctor | session | self-dev`.

## Architecture

**Two processes.** The GUI (`app.rs`) talks to a **daemon** (`daemon.rs`,
`harness serve`) over a unix/TCP socket in NDJSON (`protocol.rs`, client in
`daemon_client.rs`). The daemon is spawned on demand and outlives the GUI, so
sessions keep running.

**The agent runs in the daemon**, not in the GUI — `agent.rs` (turn +
approvals), `swarm.rs` (parallel workers), `tools/`, `llm.rs`. The consequence
that has bitten repeatedly: **any agent state is invisible to the GUI unless it
crosses the protocol**. Swarm and metrics travel in `ServerMsg::RuntimeInfo`.

**Every new field on a struct that crosses the protocol needs
`#[serde(default)]`.** Without it, an old daemon kills the new GUI's connection
with "missing field X".

## Modules

| File | Role |
|---|---|
| `app.rs` (6.8k lines) | Whole GUI: rail, chat list, chat, panels, ⌘K, Settings |
| `daemon.rs` / `daemon_client.rs` / `protocol.rs` | multi-session, multi-client |
| `agent.rs` | turn, tool rounds, approval |
| `llm.rs` | Chat Completions + pool/failover |
| `llm_responses.rs` | **Responses API** adapter (Meta Muse) |
| `llm_pool.rs` | endpoints, weights, rotation, `Wire::{Chat,Responses}` |
| `graph.rs` | structural project index (symbols/imports/refs/clusters) + impact |
| `session.rs` | session, title, pointed project, list cache |
| `md.rs` | chat markdown: highlight, copy, links, run shell (`MdAction`) |
| `theme.rs` / `ui.rs` / `icon.rs` | Paper/Ember palettes, primitives, brand |
| `metrics.rs` | tokens, cache, cost, by origin |
| `tokenless.rs` | Token Less Cost — output compression per chat |
| `gauntlet.rs` | Gauntlet Loop — directive + done marker + continue rule |

## Concepts the code does not make obvious

**Chat folder vs project.** Every chat has `{workspace}/{timestamp}/` as its
sandbox. `SessionMeta.project_dir` points the chat at a real project;
`effective_root()` picks between the two. With a project pointed, **writes start
asking for approval** (inside the chat folder they are free) —
`needs_approval(guard_writes)`.

**The graph is per session.** Its root is the chat's `project_root()`, not the
global workspace. That is why the GUI computes the stats directly instead of
going through the daemon.

**Token Less Cost** shrinks the **output** (a directive in the system prompt).
The **graph** shrinks the **input** (a subgraph instead of reading files). They
are orthogonal.

**Gauntlet Loop** is deliberately dumb: inject a directive, and at the end of
each turn auto-send `continue o loop` while the reply lacks `[GAUNTLET:DONE]`,
up to `cfg.gauntlet_max_iterations`. The whole rule lives in `gauntlet.rs` and is
tested in isolation; the rest is wiring. Reading the toggle at decision time is
what makes turning it off interrupt the loop. It rides the exact same path as
`token_less`: `ClientMsg::UserMessage` → `LiveSession` → turn `cfg` →
`apply_to_system` in `agent.rs`. **No sub-agents, no queues, no new processes.**

**Vector memory is lexical**, not semantic: `memory::embed` is bag-of-words with
hashing plus trigrams. Swapping in real embeddings requires a migration (`DIM` is
fixed at compile time).

## Conventions

- **UI strings in en-US. Docs in English. Code comments in Portuguese.** PT
  command aliases stay valid (`/apagar`, `/renomear`, `/grafo`).
- **No new dependency** without a strong reason — 32 crates today. Syntax
  highlighting, the graph, the icon and the fonts are all hand-rolled because of
  it.
- **Tests assert intent, not coordinates.** `icon::shape_is_sane` checks color
  proportion, not a fixed pixel; the highlighter tests that it reconstructs the
  text.
- Brand: terracotta `#d97757`, **geometric H** (the cursive version was
  rejected). `icon.rs` draws the app icon and the in-app mark from the same
  geometry.
