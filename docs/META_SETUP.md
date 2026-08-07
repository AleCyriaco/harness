# Testar harness com Meta Model API (Muse)

A API `api.meta.ai/v1` fala a **Responses API** (`POST /v1/responses` com
`input[]`, streaming por eventos SSE) — **não** é Chat Completions. O harness
detecta pelo host e usa o adaptador `llm_responses` automaticamente.

## Dados

| Campo | Valor |
|--------|--------|
| Base URL | `https://api.meta.ai/v1` |
| Model | `muse-spark-1.2` (1M contexto / 131072 saída, reasoning) |
| Outros ids | `muse-spark-1.2-contributor`, `muse-spark-1.1` |
| API key | portal dev.meta.ai ("Meta Model API") |
| Env | `MODEL_API_KEY` (ou `META_API_KEY`) |

## Opção A — variável de ambiente (recomendado)

```bash
export MODEL_API_KEY="..."   # sua chave
cd /caminho/para/harness
cargo run --release
```

Com `MODEL_API_KEY` definida, o harness semeia o endpoint `meta` no pool
(`muse-spark-1.2`, wire=responses auto) e já o deixa habilitado.

## Opção B — Settings no app

1. Abra **Settings**
2. Base URL: `https://api.meta.ai/v1`
3. API key: cole a chave
4. Model: `muse-spark-1.2`
5. Wire fica em `auto` (meta.ai → responses); se trocar a base, force `responses`
6. **Save**

## Opção C — config.toml

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
wire = "responses"   # opcional; "auto" deduz pelo host
```

## Smoke test da API (sem o app)

```bash
export MODEL_API_KEY="..."
curl -s https://api.meta.ai/v1/responses \
  -H "Authorization: Bearer $MODEL_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "muse-spark-1.2",
    "input": [{"role":"user","content":[{"type":"input_text","text":"Reply with exactly: HARNESS_OK"}]}],
    "max_output_tokens": 64
  }' | head -40
```

A resposta volta como objeto `{"id":"resp_...","object":"response","output":[...]}`.
Se `output` contiver o texto `HARNESS_OK`, a key está ok.

> Nota: a Responses API não expõe `/models` confiável; o botão **Modelos…**
> do pool pode falhar para este endpoint — é esperado. A key só é testada de
> verdade na primeira mensagem (o Setup avisa quando salva sem testar).

## Wire

O endpoint também registra rota `/chat/completions` (probes retornam 401, não
404), mas a integração oficial (Codex, `wire_api = "responses"`) e os exemplos
da comunidade usam `/responses`. Use `responses` (ou `auto`, que deduz pelo
host meta.ai). Só force `chat` se a Meta publicar compatibilidade completa.
