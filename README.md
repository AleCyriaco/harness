# harness v0.3

Professional low-RAM multiplatform desktop AI (Linux · macOS · Windows).

## New in 0.3

| Feature | Detail |
|---------|--------|
| **Browser (Web)** | URL bar, open system browser, HTML text preview |
| **Web server** | Tiny static HTTP server (`127.0.0.1:8765`) for local web apps |
| **Vector memory** | SQLite + hashing embeddings (no ONNX) — store / search / auto-recall |

## Run

```bash
cd harness
export OPENAI_API_KEY=sk-...
cargo run --release
```

### Test a web app

1. Put files in workspace (e.g. `web/index.html`)
2. Panel **Server** → path `web` → **Start**
3. Panel **Web** → **Open browser** (or Fetch preview)

Agent can also: `web_server_start` + `browser_open`.

### Memory

- Panel **Memory** or tools `memory_store` / `memory_search`
- Auto-recall injects top hits into the agent turn
- DB: `~/Library/Application Support/sh.harness.harness/memory.sqlite3` (macOS)

## Right panels

Files · Preview · **Web** · **Server** · **Memory** · Diag · Swarm

## Config

```toml
web_server_port = 8765
memory_auto_recall = true
```
