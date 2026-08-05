# Testar harness com Grok (xAI)

A API do Grok é **compatível com OpenAI** (`/v1/chat/completions`).

## Dados

| Campo | Valor |
|--------|--------|
| Base URL | `https://api.x.ai/v1` |
| Model (ex.) | `grok-4.5` (ou o id listado no console) |
| API key | em [console.x.ai](https://console.x.ai) |
| Env | `XAI_API_KEY` |

## Opção A — variável de ambiente (recomendado)

```bash
export XAI_API_KEY="xai-..."   # sua chave
cd /caminho/para/harness
cargo run --release
```

Com `XAI_API_KEY` definida, o harness preenche base/model se a key do config estiver vazia.

## Opção B — Settings no app

1. Abra **Settings**
2. Base URL: `https://api.x.ai/v1`
3. API key: cole a chave xAI
4. Model: `grok-4.5` (ou outro disponível na sua conta)
5. **Save**

## Opção C — config.toml

macOS:

`~/Library/Application Support/sh.harness.harness/config.toml`

```toml
api_base = "https://api.x.ai/v1"
api_key = "xai-..."
model = "grok-4.5"
```

## Smoke test da API (sem o app)

```bash
export XAI_API_KEY="xai-..."
curl -s https://api.x.ai/v1/chat/completions \
  -H "Authorization: Bearer $XAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "grok-4.5",
    "messages": [{"role":"user","content":"Reply with exactly: HARNESS_OK"}],
    "temperature": 0
  }'
```

Se a resposta contiver `HARNESS_OK`, a key está ok.

## Modelos

Os ids mudam com o tempo. Confira em:

- https://docs.x.ai  
- https://console.x.ai  

Se `grok-4.5` der 404, liste modelos:

```bash
curl -s https://api.x.ai/v1/models \
  -H "Authorization: Bearer $XAI_API_KEY" | head
```

E ajuste `model` no Settings.

## Tools / function calling

O harness usa tools no estilo OpenAI. A maioria dos modelos Grok recentes suporta; se o agent “só conversar” sem criar arquivos, troque o model no console (preferir flagship/coding).
