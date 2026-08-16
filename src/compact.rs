//! Compaction: o que sai do histórico vira resumo, não silêncio.
//!
//! `history_cap` sempre **descartou** mensagem antiga. Numa tarefa longa isso
//! apaga a decisão tomada na mensagem 3 e o agente refaz o que já tinha feito.
//! Aqui o trecho descartado vira uma nota de sistema curta — e o texto integral
//! vai para o disco (spill), então nada some de verdade.
//!
//! O resumo é **extrativo e local**: sem chamada de LLM, sem custo, sem
//! latência. cyrix: piora em texto muito narrativo — a versão com LLM entra se
//! alguém reclamar da qualidade, não antes.

use crate::llm::ChatMessage;

/// Nota que substitui o trecho removido. Vazia quando não há o que resumir.
pub fn condense(dropped: &[ChatMessage], max_chars: usize) -> String {
    if dropped.is_empty() {
        return String::new();
    }
    let mut asks: Vec<String> = Vec::new();
    let mut did: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();

    for m in dropped {
        match m.role.as_str() {
            "user" => {
                if let Some(c) = &m.content {
                    let line = first_line(c, 120);
                    if !line.is_empty() && !asks.contains(&line) {
                        asks.push(line);
                    }
                }
            }
            "assistant" => {
                if let Some(calls) = &m.tool_calls {
                    for c in calls {
                        let name = c.function.name.clone();
                        if let Some(p) = path_of(&c.function.arguments) {
                            if !files.contains(&p) {
                                files.push(p);
                            }
                        }
                        if !did.contains(&name) {
                            did.push(name);
                        }
                    }
                } else if let Some(c) = &m.content {
                    let line = first_line(c, 100);
                    if !line.is_empty() && !did.contains(&line) {
                        did.push(line);
                    }
                }
            }
            _ => {}
        }
    }

    let mut out = String::from("[compacted history] earlier in this chat:\n");
    if !asks.is_empty() {
        out.push_str("- asked: ");
        out.push_str(&asks.join(" | "));
        out.push('\n');
    }
    if !did.is_empty() {
        out.push_str("- did: ");
        out.push_str(&join_capped(&did, 12));
        out.push('\n');
    }
    if !files.is_empty() {
        out.push_str("- touched: ");
        out.push_str(&join_capped(&files, 10));
        out.push('\n');
    }
    out.push_str("(full text kept in .harness_spill.jsonl)");
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect::<String>() + "…";
    }
    out
}

fn join_capped(v: &[String], max: usize) -> String {
    if v.len() <= max {
        return v.join(", ");
    }
    format!("{}, +{} more", v[..max].join(", "), v.len() - max)
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    line.chars().take(max).collect()
}

/// `{"path":"web/index.html", …}` → `web/index.html`, sem parser de JSON.
fn path_of(args: &str) -> Option<String> {
    let key = "\"path\"";
    let at = args.find(key)? + key.len();
    let rest = args[at..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let p = &rest[..end];
    if p.is_empty() {
        None
    } else {
        Some(p.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn user(t: &str) -> ChatMessage {
        ChatMessage {
            role: "user".into(),
            content: Some(t.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        }
    }

    fn call(name: &str, args: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: args.into(),
                },
            }]),
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn resume_guarda_o_pedido_as_tools_e_os_arquivos() {
        let dropped = vec![
            user("criar o jogo em html"),
            call("write_file", r#"{"path":"web/index.html","content":"..."}"#),
            call("run_command", r#"{"command":"ls"}"#),
        ];
        let s = condense(&dropped, 2_000);
        assert!(s.contains("criar o jogo em html"), "{s}");
        assert!(s.contains("write_file") && s.contains("run_command"), "{s}");
        assert!(s.contains("web/index.html"), "{s}");
        assert!(s.contains("spill"), "tem que dizer onde está o texto inteiro");
    }

    #[test]
    fn nada_descartado_nao_vira_nota() {
        assert!(condense(&[], 500).is_empty());
    }

    #[test]
    fn nota_respeita_o_teto_de_tamanho() {
        let many: Vec<ChatMessage> = (0..80)
            .map(|i| user(&format!("pedido numero {i} com bastante texto para encher")))
            .collect();
        let s = condense(&many, 300);
        assert!(s.chars().count() <= 301, "len={}", s.chars().count());
    }
}
