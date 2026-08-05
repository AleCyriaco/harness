# jcode ↔ harness parity (v0.4+)

## Now implemented (or substantially improved)

### Architecture / runtime
| Feature | Status |
|---------|--------|
| Daemon multi-client (NDJSON, reattach sessions) | ✅ `harness serve` / `connect` |
| **Multi-session scale** (N live sessions, max cap, restore, kill/detach, bus) | ✅ `harness session *` + GUI Live panel |
| **GUI on daemon** (Stop/approve/stream via socket) | ✅ auto-starts daemon on GUI launch |
| Headless CLI | ✅ `harness run "<prompt>"` |
| Self-dev build/reload | ✅ `harness self-dev` + tool `selfdev` |
| SDK | ✅ `sdk/rust` + `sdk/typescript` |
| TCP always on Windows; unix socket on Unix | ✅ |

### Memory
| Feature | Status |
|---------|--------|
| Vector memory + auto-recall | ✅ |
| Memory **graph** + related edges | ✅ `memory_graph_*` |
| Consolidation / dedupe | ✅ |
| Side **judge** (local relevance) | ✅ |
| Drift markers on topic shift | ✅ |
| Ambient consolidation loop | ✅ `ambient_start/stop` |
| Session search RAG | ✅ `session_search` |
| Session-end extract helper | ✅ |

### Swarm
| Feature | Status |
|---------|--------|
| Workers + bus | ✅ |
| File-read tracking + peer edit notify | ✅ |
| Shared **versioned plan DAG** | ✅ `swarm_plan_*` |
| Git worktrees | ✅ `git_worktree_add` |

### Tools
| Feature | Status |
|---------|--------|
| agentgrep adaptive | ✅ |
| MCP pool | ✅ |
| multiedit | ✅ |
| bg jobs | ✅ |
| skills + hooks | ✅ |
| resume import Claude/json | ✅ |
| plan/todo | ✅ |
| side_panel live | ✅ |
| provider profiles | ✅ `/profile` + doctor |
| usage counters | ✅ |
| Anthropic cache warning | ✅ |

### UI
| Feature | Status |
|---------|--------|
| Markdown chat | ✅ |
| Mermaid → ASCII | ✅ |
| Slash commands | ✅ |
| Side panel tab | ✅ |
| Diff in side panel | ✅ |
| WebView internal | ✅ |

## Still lighter than jcode (honest)

- No full jemalloc multi-session 10MB profile (GUI+WebView costs more)
- No OAuth device flows (Claude/Copilot login UI) — profiles + API keys only
- No neural ONNX embeddings (hash embeddings + graph; faster/lighter)
- No Firefox computer-use automation (WebView preview, not agent bridge)
- No soft KV-cache interleave mid-stream
- No iOS app / installer channels / full telemetry pipeline
- Binary hot-reload with zero disconnect still lighter than jcode `/reload` (use `self-dev reload` / restart GUI; daemon can stay up)
- Soft-interrupt mid-stream still cancel-only

## CLI quick ref

```bash
harness                  # GUI
harness serve [--tcp]    # daemon
harness run "fix foo"
harness connect          # REPL
harness self-dev build
harness doctor
harness --webview URL
```
