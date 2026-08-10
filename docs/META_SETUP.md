# Running harness with the Meta Model API (Muse)

`api.meta.ai/v1` speaks the **Responses API** (`POST /v1/responses` with
`input[]`, SSE event streaming) — it is **not** Chat Completions. harness detects
it by host and switches to the `llm_responses` adapter automatically.

## Facts

| Field | Value |
|--------|--------|
| Base URL | `https://api.meta.ai/v1` |
| Model | `muse-spark-1.2` (1M context / 131072 output, reasoning) |
| Other ids | `muse-spark-1.2-contributor`, `muse-spark-1.1` |
| API key | dev.meta.ai portal ("Meta Model API") |
| Env | `MODEL_API_KEY` (or `META_API_KEY`) |

## Option A — environment variable (recommended)

```bash
export MODEL_API_KEY="..."   # your key
cd /path/to/harness
cargo run --release
```

With `MODEL_API_KEY` set, harness seeds the `meta` endpoint into the pool
(`muse-spark-1.2`, wire=responses by detection) and leaves it enabled.

## Option B — Settings in the app

1. Open **Settings**
2. Base URL: `https://api.meta.ai/v1`
3. API key: paste the key
4. Model: `muse-spark-1.2`
5. Leave Wire on `auto` (meta.ai → responses); if you change the base URL, force `responses`
6. **Save**

## Option C — config.toml

macOS: `~/Library/Application Support/sh.harness.harness/config.toml`

```toml
api_base = "https://api.meta.ai/v1"
api_key = "..."
model = "muse-spark-1.2"

[[llm_pool]]
name = "meta"
api_base = "https://api.meta.ai/v1"
api_key = "..."
model = "muse-spark-1.2"
wire = "responses"   # optional; "auto" infers it from the host
```

## API smoke test (without the app)

```bash
curl -s https://api.meta.ai/v1/responses \
  -H "Authorization: Bearer $MODEL_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "muse-spark-1.2",
    "input": [{"role":"user","content":[{"type":"input_text","text":"Reply with exactly: HARNESS_OK"}]}],
    "max_output_tokens": 64
  }' | head -40
```

The reply is an object `{"id":"resp_...","object":"response","output":[...]}`.
If `output` carries the text `HARNESS_OK`, the key works.

> Note: the Responses API has no reliable `/models` endpoint, so the pool's
> **Models…** button may fail here — that is expected. The key is only really
> exercised on the first message (Setup warns when you save without testing).

## Wire

The endpoint also registers a `/chat/completions` route (probes return 401, not
404), but the official integration (Codex, `wire_api = "responses"`) and the
community examples use `/responses`. Use `responses` (or `auto`, which infers it
from the meta.ai host). Only force `chat` if Meta ships full compatibility.
