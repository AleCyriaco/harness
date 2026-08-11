//! Adaptador para a **Responses API** (`POST {base}/responses`), usada pelos
//! modelos Muse da Meta.
//!
//! O resto do harness fala Chat Completions: `messages[]`, `tool_calls`,
//! `max_tokens`. A Responses API fala `input[]`, itens `function_call` /
//! `function_call_output` e `max_output_tokens`, e responde por eventos SSE.
//! Este módulo traduz nos dois sentidos, então o agente não precisa saber.
//!
//! **Ressalva honesta:** o formato do *pedido* veio do snippet do usuário e
//! está fiel. Os nomes dos *eventos* de resposta são inferidos da Responses
//! API; por isso o leitor aceita também um caminho genérico (qualquer evento
//! com `delta` textual, e o objeto final em `response.output`), e guarda o
//! último evento desconhecido para aparecer no erro em vez de falhar mudo.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Config;
use crate::llm::{ChatMessage, FunctionCall, LlmReply, StreamCb, ToolCall};

/// Valores do snippet do usuário.
const TEMPERATURE: f32 = 0.6;
const TOP_P: f32 = 0.9;
const REASONING_EFFORT: &str = "medium";

/// Limite de saída por modelo. muse-spark-1.2 aceita 131072; os demais ficam
/// em 32768, valor seguro para a maioria dos modelos reasoning. Teto baixo
/// aqui truncava respostas longas de agente (código, diffs).
fn max_output_tokens(model: &str) -> u32 {
    if model.starts_with("muse-spark-1.2") {
        131_072
    } else {
        32_768
    }
}

/// Converte o histórico do harness para `input[]` da Responses API.
pub fn build_input(messages: &[ChatMessage]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        match m.role.as_str() {
            // resultado de ferramenta é item próprio, não uma "mensagem"
            "tool" => out.push(json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.clone().unwrap_or_default(),
                "output": m.content.clone().unwrap_or_default(),
            })),
            "assistant" => {
                if let Some(text) = m.content.as_ref().filter(|t| !t.is_empty()) {
                    out.push(json!({
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    }));
                }
                for tc in m.tool_calls.iter().flatten() {
                    out.push(json!({
                        "type": "function_call",
                        "call_id": tc.id,
                        "name": tc.function.name,
                        "arguments": tc.function.arguments,
                    }));
                }
            }
            role => out.push(json!({
                "role": role,
                "content": [{"type": "input_text", "text": m.content.clone().unwrap_or_default()}],
            })),
        }
    }
    out
}

/// Chat Completions aninha a função em `function`; a Responses API quer plano.
pub fn flatten_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| match t.get("function") {
            Some(f) => json!({
                "type": "function",
                "name": f.get("name").cloned().unwrap_or(Value::Null),
                "description": f.get("description").cloned().unwrap_or(Value::Null),
                "parameters": f.get("parameters").cloned().unwrap_or(json!({})),
            }),
            None => t.clone(),
        })
        .collect()
}

pub fn build_body(cfg: &Config, messages: &[ChatMessage], tools: &[Value], stream: bool) -> Value {
    let mut body = json!({
        "model": cfg.model,
        "input": build_input(messages),
        "stream": stream,
        "temperature": TEMPERATURE,
        "max_output_tokens": max_output_tokens(&cfg.model),
        "top_p": TOP_P,
        "reasoning": {"effort": REASONING_EFFORT},
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(flatten_tools(tools));
    }
    body
}

/// Estado acumulado enquanto os eventos chegam.
#[derive(Default)]
pub struct Acc {
    pub text: String,
    /// call_id → (nome, argumentos)
    pub calls: Vec<(String, String, String)>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub last_unknown: Option<String>,
    pub done: bool,
}

fn as_u64(v: Option<&Value>) -> u64 {
    v.and_then(|x| x.as_u64()).unwrap_or(0)
}

/// Aplica um evento SSE já desserializado. Tolerante de propósito: nome de
/// evento desconhecido não derruba a resposta, só é registrado.
pub fn apply_event(v: &Value, acc: &mut Acc, mut on_delta: Option<&mut StreamCb>) {
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    // texto incremental
    if kind.ends_with("output_text.delta") || kind == "response.text.delta" {
        if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
            acc.text.push_str(d);
            if let Some(cb) = on_delta.as_deref_mut() {
                cb(d);
            }
        }
        return;
    }
    // argumentos de tool chegando em pedaços
    if kind.ends_with("function_call_arguments.delta") {
        let id = v
            .get("call_id")
            .or_else(|| v.get("item_id"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let d = v.get("delta").and_then(|d| d.as_str()).unwrap_or("");
        match acc.calls.iter_mut().find(|(cid, _, _)| *cid == id) {
            Some((_, _, args)) => args.push_str(d),
            None => acc.calls.push((id, String::new(), d.to_string())),
        }
        return;
    }
    // item completo (é aqui que costuma vir o nome da função)
    if kind.ends_with("output_item.added") || kind.ends_with("output_item.done") {
        if let Some(item) = v.get("item") {
            absorb_item(item, acc);
        }
        return;
    }
    if kind == "error" || v.get("error").is_some() {
        acc.last_unknown = Some(v.to_string().chars().take(400).collect());
        return;
    }
    // Terminais além do `completed`: falha (erro do servidor) e incompleta
    // (estourou max_output_tokens ou filtro de conteúdo). Os dois podem vir
    // com texto parcial em `response.output` — absorve e registra o motivo.
    if kind == "response.failed" || kind == "response.incomplete" {
        if let Some(resp) = v.get("response") {
            absorb_final(resp, acc);
            if let Some(msg) = resp.pointer("/error/message").and_then(|m| m.as_str()) {
                acc.last_unknown = Some(format!("{kind}: {msg}"));
            } else if let Some(reason) =
                resp.pointer("/incomplete_details/reason").and_then(|r| r.as_str())
            {
                acc.last_unknown = Some(format!("response.incomplete: {reason}"));
            }
        }
        acc.done = true;
        return;
    }
    if kind.ends_with("completed") || kind.ends_with("response.done") {
        if let Some(resp) = v.get("response") {
            absorb_final(resp, acc);
        }
        acc.done = true;
        return;
    }

    // caminho genérico: se o evento traz um `delta` textual, é texto
    if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
        acc.text.push_str(d);
        if let Some(cb) = on_delta.as_deref_mut() {
            cb(d);
        }
        return;
    }
    // Objeto final **sem** `type`: é a resposta não-stream inteira.
    //
    // Este ramo não pode olhar só para `response.output`: eventos de ciclo de
    // vida (`response.created`, `response.in_progress`) carregam a mesma chave
    // com `output: []`, e tratá-los como final encerrava o stream no primeiro
    // evento — o servidor mandava o texto e o harness dizia "nothing usable".
    if kind.is_empty() && v.get("response").and_then(|r| r.get("output")).is_some() {
        absorb_final(v.get("response").unwrap(), acc);
        acc.done = true;
        return;
    }
    if !kind.is_empty() {
        acc.last_unknown = Some(kind.to_string());
    }
}

fn absorb_item(item: &Value, acc: &mut Acc) {
    match item.get("type").and_then(|t| t.as_str()) {
        Some("function_call") => {
            let id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let name = item
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let args = item
                .get("arguments")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            match acc.calls.iter_mut().find(|(cid, _, _)| *cid == id) {
                Some(slot) => {
                    if !name.is_empty() {
                        slot.1 = name;
                    }
                    if !args.is_empty() {
                        slot.2 = args;
                    }
                }
                None => acc.calls.push((id, name, args)),
            }
        }
        Some("message") => {
            if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                        // só entra se o streaming não trouxe (evita duplicar)
                        if acc.text.is_empty() {
                            acc.text.push_str(t);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn absorb_final(resp: &Value, acc: &mut Acc) {
    if let Some(items) = resp.get("output").and_then(|o| o.as_array()) {
        for item in items {
            absorb_item(item, acc);
        }
    }
    if let Some(u) = resp.get("usage") {
        // Responses usa input/output_tokens; aceita o nome antigo também
        acc.prompt_tokens = as_u64(u.get("input_tokens")).max(as_u64(u.get("prompt_tokens")));
        acc.completion_tokens =
            as_u64(u.get("output_tokens")).max(as_u64(u.get("completion_tokens")));
        acc.cached_tokens = as_u64(u.pointer("/input_tokens_details/cached_tokens"))
            .max(as_u64(u.get("cached_tokens")));
    }
}

/// Lê o SSE linha a linha e alimenta o acumulador. Separado de `chat` para
/// poder ser testado com os bytes reais de uma captura, sem rede.
pub fn parse_stream<R: BufRead>(
    mut reader: R,
    acc: &mut Acc,
    mut on_delta: Option<&mut StreamCb>,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut line = String::new();
    let mut data_buf = String::new();
    loop {
        // sem isto o Stop não interrompe um stream em andamento
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if let Some(rest) = trimmed.strip_prefix("data:") {
            data_buf.push_str(rest.trim_start());
            continue;
        }
        if !trimmed.is_empty() {
            // linhas `event:` / `id:` não carregam corpo
            continue;
        }
        if data_buf.is_empty() {
            continue;
        }
        let payload = std::mem::take(&mut data_buf);
        if payload == "[DONE]" {
            break;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&payload) {
            apply_event(&v, acc, on_delta.as_deref_mut());
        }
        if acc.done {
            break;
        }
    }
    if !data_buf.is_empty() {
        if let Ok(v) = serde_json::from_str::<Value>(&data_buf) {
            apply_event(&v, acc, on_delta.as_deref_mut());
        }
    }
    Ok(())
}

/// Vira a resposta que o resto do harness espera.
pub fn into_reply(acc: Acc) -> Result<LlmReply> {
    if acc.text.is_empty() && acc.calls.is_empty() {
        let hint = acc
            .last_unknown
            .unwrap_or_else(|| "no events parsed".into());
        bail!("Responses API returned nothing usable ({hint})");
    }
    let tool_calls: Vec<ToolCall> = acc
        .calls
        .into_iter()
        .filter(|(_, name, _)| !name.is_empty())
        .map(|(id, name, arguments)| ToolCall {
            id,
            kind: "function".into(),
            function: FunctionCall { name, arguments },
        })
        .collect();
    Ok(LlmReply {
        finish_reason: if tool_calls.is_empty() {
            "stop".into()
        } else {
            "tool_calls".into()
        },
        message: ChatMessage {
            role: "assistant".into(),
            content: (!acc.text.is_empty()).then_some(acc.text),
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            tool_call_id: None,
            name: None,
        },
    })
}

pub fn chat(
    cfg: &Config,
    messages: &[ChatMessage],
    tools: &[Value],
    cancel: &AtomicBool,
    mut on_delta: Option<&mut StreamCb>,
) -> Result<LlmReply> {
    let url = format!("{}/responses", cfg.api_base.trim_end_matches('/'));
    let body = build_body(cfg, messages, tools, true);
    let resp = crate::llm::http_client()
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .context("responses request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        bail!("LLM HTTP {status}: {text}");
    }

    let mut acc = Acc::default();
    parse_stream(BufReader::new(resp), &mut acc, on_delta.as_deref_mut(), cancel)?;

    if acc.prompt_tokens > 0 || acc.completion_tokens > 0 {
        let (pi, po) = crate::llm_pool::active_price(cfg);
        let cost = (acc.prompt_tokens as f64 / 1e6) * pi
            + (acc.completion_tokens as f64 / 1e6) * po;
        crate::provider_doctor::record_usage(acc.prompt_tokens, acc.completion_tokens);
        crate::metrics::record_call(
            acc.prompt_tokens,
            acc.completion_tokens,
            acc.cached_tokens.min(acc.prompt_tokens),
            cost,
        );
    }
    into_reply(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes reais de uma resposta do muse. O `response.created` traz
    /// `output: []`, que antes era confundido com o objeto final e cortava o
    /// stream no primeiro evento.
    const SSE: &str = concat!(
        "event: response.created\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"output\":[],\"status\":\"in_progress\"},\"sequence_number\":0,\"type\":\"response.created\"}\n",
        "\n",
        "event: response.in_progress\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"output\":[],\"status\":\"in_progress\"},\"sequence_number\":1,\"type\":\"response.in_progress\"}\n",
        "\n",
        "event: response.output_text.delta\n",
        "data: {\"content_index\":0,\"delta\":\"HARNESS\",\"item_id\":\"msg_1\",\"output_index\":1,\"type\":\"response.output_text.delta\"}\n",
        "\n",
        "event: response.output_text.delta\n",
        "data: {\"content_index\":0,\"delta\":\"_OK\",\"item_id\":\"msg_1\",\"output_index\":1,\"type\":\"response.output_text.delta\"}\n",
        "\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"output\":[{\"content\":[{\"text\":\"HARNESS_OK\",\"type\":\"output_text\"}],\"role\":\"assistant\",\"type\":\"message\"}],\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":3}},\"type\":\"response.completed\"}\n",
        "\n",
        "data: [DONE]\n\n",
    );

    #[test]
    fn evento_de_criacao_nao_encerra_o_stream() {
        let mut acc = Acc::default();
        parse_stream(
            std::io::Cursor::new(SSE),
            &mut acc,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(acc.text, "HARNESS_OK", "o texto do stream tem que chegar inteiro");
        assert!(acc.done, "o `response.completed` fecha");
        assert_eq!(acc.prompt_tokens, 10);
        assert_eq!(acc.completion_tokens, 3);
        let reply = into_reply(acc).expect("não pode dizer que não veio nada");
        assert_eq!(reply.message.content.as_deref(), Some("HARNESS_OK"));
    }

    #[test]
    fn cancelar_interrompe_o_stream() {
        let mut acc = Acc::default();
        let err = parse_stream(
            std::io::Cursor::new(SSE),
            &mut acc,
            None,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(acc.text.is_empty());
    }

    /// O corpo não-stream (sem `type`) continua sendo aceito inteiro.
    #[test]
    fn resposta_nao_stream_ainda_e_absorvida() {
        let body = serde_json::json!({
            "response": {
                "output": [{"content": [{"text": "OK", "type": "output_text"}],
                            "role": "assistant", "type": "message"}],
                "status": "completed"
            }
        });
        let mut acc = Acc::default();
        apply_event(&body, &mut acc, None);
        assert_eq!(acc.text, "OK");
        assert!(acc.done);
    }

    /// Reproduz o corpo **real** do app (system prompt + todas as tools) contra
    /// o endpoint meta do config do usuário, para ver o que volta de fato.
    /// `cargo test -- --ignored muse_com_o_corpo_do_app --nocapture`
    #[test]
    #[ignore]
    fn muse_com_o_corpo_do_app() {
        let disk = Config::load();
        let Some(ep) = disk
            .llm_pool
            .iter()
            .find(|e| e.api_base.contains("meta.ai") && !e.api_key.trim().is_empty())
        else {
            println!("sem endpoint meta com key — nada a testar");
            return;
        };
        let mut cfg = disk.clone();
        ep.apply_to(&mut cfg);

        let mode = crate::modes::AppMode::Code;
        let tools = crate::tools::tool_schemas(mode);
        let sys = crate::llm::system_prompt(mode, "/tmp");
        let messages = vec![
            crate::llm::ChatMessage {
                role: "system".into(),
                content: Some(sys.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            crate::llm::ChatMessage {
                role: "user".into(),
                content: Some("Reply with exactly: HARNESS_OK".into()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];
        let body = build_body(&cfg, &messages, &tools, true);
        println!(
            "model={} tools={} system={} chars · body={} KB",
            cfg.model,
            tools.len(),
            sys.len(),
            serde_json::to_string(&body).unwrap().len() / 1024
        );

        let cancel = AtomicBool::new(false);
        match chat(&cfg, &messages, &tools, &cancel, None) {
            Ok(r) => println!(
                "OK · content={:?} · tool_calls={}",
                r.message.content.as_deref().map(|s| s.chars().take(120).collect::<String>()),
                r.message.tool_calls.map(|c| c.len()).unwrap_or(0)
            ),
            Err(e) => println!("FALHOU: {e}"),
        }
    }

    fn msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn historico_vira_input_da_responses_api() {
        let mut assistant = msg("assistant", "vou olhar");
        assistant.tool_calls = Some(vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
            },
        }]);
        let mut tool = msg("tool", "conteúdo");
        tool.tool_call_id = Some("call_1".into());

        let input = build_input(&[msg("system", "sys"), msg("user", "oi"), assistant, tool]);
        assert_eq!(input.len(), 5, "texto e tool_call viram itens separados");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[2]["content"][0]["type"], "output_text");
        assert_eq!(input[3]["type"], "function_call");
        assert_eq!(input[3]["call_id"], "call_1");
        assert_eq!(input[4]["type"], "function_call_output");
        assert_eq!(input[4]["call_id"], "call_1");
    }

    #[test]
    fn tools_ficam_planas() {
        let t = json!({"type":"function","function":{"name":"f","description":"d","parameters":{"a":1}}});
        let flat = flatten_tools(&[t]);
        assert_eq!(flat[0]["name"], "f");
        assert_eq!(flat[0]["parameters"]["a"], 1);
        assert!(flat[0].get("function").is_none());
    }

    #[test]
    fn le_texto_tool_call_e_usage_do_stream() {
        let mut acc = Acc::default();
        for ev in [
            json!({"type":"response.output_text.delta","delta":"Oi"}),
            json!({"type":"response.output_text.delta","delta":" mundo"}),
            json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"c1","name":"grep","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","call_id":"c1","delta":"{\"q\":1}"}),
            json!({"type":"response.completed","response":{"output":[],"usage":{"input_tokens":120,"output_tokens":8,"input_tokens_details":{"cached_tokens":100}}}}),
        ] {
            apply_event(&ev, &mut acc, None);
        }
        assert_eq!(acc.text, "Oi mundo");
        assert_eq!(acc.prompt_tokens, 120);
        assert_eq!(acc.completion_tokens, 8);
        assert_eq!(acc.cached_tokens, 100);
        assert!(acc.done);

        let reply = into_reply(acc).unwrap();
        assert_eq!(reply.finish_reason, "tool_calls");
        let tc = &reply.message.tool_calls.unwrap()[0];
        assert_eq!(tc.function.name, "grep");
        assert_eq!(tc.function.arguments, r#"{"q":1}"#);
    }

    #[test]
    fn evento_desconhecido_nao_derruba_mas_aparece_no_erro() {
        let mut acc = Acc::default();
        apply_event(&json!({"type":"response.some_future_thing"}), &mut acc, None);
        assert_eq!(acc.last_unknown.as_deref(), Some("response.some_future_thing"));
        let err = into_reply(acc).unwrap_err().to_string();
        assert!(err.contains("response.some_future_thing"), "{err}");
    }

    #[test]
    fn resposta_sem_streaming_e_lida_do_objeto_final() {
        let mut acc = Acc::default();
        apply_event(
            &json!({"type":"response.completed","response":{
                "output":[{"type":"message","content":[{"type":"output_text","text":"pronto"}]}],
                "usage":{"input_tokens":5,"output_tokens":2}
            }}),
            &mut acc,
            None,
        );
        assert_eq!(into_reply(acc).unwrap().message.content.as_deref(), Some("pronto"));
    }

    #[test]
    fn resposta_incompleta_guarda_o_texto_parcial_e_o_motivo() {
        let mut acc = Acc::default();
        apply_event(
            &json!({"type":"response.incomplete","response":{
                "status":"incomplete",
                "incomplete_details":{"reason":"max_output_tokens"},
                "output":[{"type":"message","content":[{"type":"output_text","text":"parcial"}]}]
            }}),
            &mut acc,
            None,
        );
        assert!(acc.done);
        assert!(acc.last_unknown.as_deref().unwrap().contains("max_output_tokens"));
        let reply = into_reply(acc).unwrap();
        assert_eq!(reply.message.content.as_deref(), Some("parcial"));
    }

    #[test]
    fn resposta_com_falha_vira_erro_com_mensagem_do_servidor() {
        let mut acc = Acc::default();
        apply_event(
            &json!({"type":"response.failed","response":{
                "status":"failed",
                "error":{"message":"server overloaded"},
                "output":[]
            }}),
            &mut acc,
            None,
        );
        assert!(acc.done);
        let err = into_reply(acc).unwrap_err().to_string();
        assert!(err.contains("server overloaded"), "{err}");
    }

    #[test]
    fn teto_de_saida_segue_o_modelo() {
        assert_eq!(max_output_tokens("muse-spark-1.2"), 131_072);
        assert_eq!(max_output_tokens("muse-spark-1.2-contributor"), 131_072);
        assert_eq!(max_output_tokens("muse-spark-1.1"), 32_768);
        assert_eq!(max_output_tokens("gpt-4.1-mini"), 32_768);
    }
}
