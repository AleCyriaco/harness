# Running harness with Grok (xAI)

The Grok API is **OpenAI-compatible** (`/v1/chat/completions`).

## Facts

| Field | Value |
|--------|--------|
| Base URL | `https://api.x.ai/v1` |
| Model (e.g.) | `grok-4.5` (or whatever id your console lists) |
| API key | [console.x.ai](https://console.x.ai) |
| Env | `XAI_API_KEY` |

## Option A — environment variable (recommended)

```bash
export XAI_API_KEY="xai-..."   # your key
cd /path/to/harness
cargo run --release
```

With `XAI_API_KEY` set, harness fills in base/model when the config key is empty.

## Option B — Settings in the app

1. Open **Settings**
2. Base URL: `https://api.x.ai/v1`
3. API key: paste the xAI key
4. Model: `grok-4.5` (or another one available to your account)
5. **Save**

## Option C — config.toml

macOS:

`~/Library/Application Support/sh.harness.harness/config.toml`

```toml
api_base = "https://api.x.ai/v1"
api_key = "xai-..."
model = "grok-4.5"
```

## API smoke test (without the app)

```bash
curl -s https://api.x.ai/v1/chat/completions \
  -H "Authorization: Bearer $XAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "grok-4.5",
    "messages": [{"role":"user","content":"Reply with exactly: HARNESS_OK"}],
    "temperature": 0
  }'
```

If the reply contains `HARNESS_OK`, the key works.

## Models

Ids change over time. Check:

- https://docs.x.ai
- https://console.x.ai

If `grok-4.5` returns 404, list the models:

```bash
curl -s https://api.x.ai/v1/models -H "Authorization: Bearer $XAI_API_KEY" | head
```

and adjust `model` in Settings.

## Tools / function calling

harness uses OpenAI-style tools. Most recent Grok models support them; if the
agent only chats and never creates files, switch to a flagship/coding model in
the console.
