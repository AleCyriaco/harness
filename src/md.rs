//! Lightweight markdown-ish rendering for chat (no browser).

use egui::{RichText, Ui};

use crate::theme::pal;

const BODY: f32 = 14.5;

/// Ação acionada por um clique no texto renderizado.
#[derive(Debug, Clone, PartialEq)]
pub enum MdAction {
    /// Copiar o texto (bloco/link) para o clipboard.
    CopyText(String),
    /// Executar o comando (bloco ```sh/bash) e mostrar a saída no chat.
    RunCommand(String),
}

/// Render a subset of markdown: headings, bold, bullets, code fences, bare lines.
pub fn render_markdown(ui: &mut Ui, text: &str) -> Option<MdAction> {
    let text = crate::mermaid_lite::expand_in_markdown(text);
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut lang = String::new();
    let mut action: Option<MdAction> = None;

    let mut take = |a: Option<MdAction>| {
        if action.is_none() {
            action = a;
        }
    };

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                take(code_block(ui, &lang, code_buf.trim_end()));
                code_buf.clear();
                in_code = false;
                ui.add_space(6.0);
            } else {
                lang = line.trim_start().trim_start_matches('`').trim().to_string();
                in_code = true;
            }
            continue;
        }
        if in_code {
            code_buf.push_str(line);
            code_buf.push('\n');
            continue;
        }

        let t = line.trim_end();
        if t.is_empty() {
            ui.add_space(5.0);
            continue;
        }
        // cyrix: um arm só para os 3 níveis de heading — só muda o size
        let heading = t.strip_prefix("### ").map(|r| (r, 14.5))
            .or_else(|| t.strip_prefix("## ").map(|r| (r, 15.5)))
            .or_else(|| t.strip_prefix("# ").map(|r| (r, 17.0)));
        if let Some((rest, size)) = heading {
            let owned = rest.to_string();
            take(text_block(ui, &owned, |ui| {
                ui.label(RichText::new(&owned).strong().size(size).color(pal().text));
                None
            }));
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            let owned = rest.to_string();
            take(text_block(ui, &owned, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("•").color(pal().muted));
                });
                render_inline(ui, &owned)
            }));
        } else {
            let owned = t.to_string();
            take(text_block(ui, &owned, |ui| render_inline(ui, &owned)));
        }
    }
    if in_code && !code_buf.is_empty() {
        take(code_block(ui, &lang, code_buf.trim_end()));
    }
    action
}

/// Um bloco de texto com um pequeno ícone ⧉ à direita que copia o bloco
/// inteiro (devolve a ação para o app dar feedback). O texto quebra
/// normalmente; o botão fica no canto da 1ª linha.
fn text_block(
    ui: &mut Ui,
    copy_text: &str,
    content: impl FnOnce(&mut Ui) -> Option<MdAction>,
) -> Option<MdAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 24.0).max(120.0);
        ui.allocate_ui_with_layout(
            egui::vec2(w, 0.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                if action.is_none() {
                    action = content(ui);
                } else {
                    content(ui);
                }
            },
        );
        let btn = ui
            .add(
                egui::Button::new(RichText::new("⧉").size(11.0).color(pal().muted))
                    .frame(false),
            )
            .on_hover_text("Copy this block");
        if btn.clicked() {
            action = Some(MdAction::CopyText(copy_text.to_string()));
        }
    });
    action
}

/// Bloco de código: cabeçalho com a linguagem, botão de copiar e — para
/// comandos shell — botão ▶ Executar.
fn code_block(ui: &mut Ui, lang: &str, code: &str) -> Option<MdAction> {
    let p = pal();
    let mut action = None;
    egui::Frame::new()
        .fill(p.code_bg)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if lang.is_empty() { "code" } else { lang })
                        .monospace()
                        .size(9.5)
                        .color(p.muted),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let runnable = is_shell_lang(lang);
                    if runnable {
                        let btn = ui
                            .add(
                                egui::Button::new(
                                    RichText::new("▶ run").monospace().size(10.0).color(p.ok),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Run this command and show the output");
                        if btn.clicked() {
                            action = Some(MdAction::RunCommand(code.to_string()));
                        }
                        ui.label(
                            RichText::new("·")
                                .monospace()
                                .size(10.0)
                                .color(p.muted),
                        );
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("copy").monospace().size(10.0).color(p.muted),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        action = Some(MdAction::CopyText(code.to_string()));
                    }
                });
            });
            // linha de código é linha, não parágrafo: sem o respiro padrão
            ui.spacing_mut().item_spacing.y = 1.0;
            for line in code.lines() {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (text, color) in highlight(lang, line) {
                        ui.label(RichText::new(text).monospace().size(12.5).color(color));
                    }
                });
            }
        });
    action
}

fn is_shell_lang(lang: &str) -> bool {
    matches!(
        lang.trim().to_ascii_lowercase().as_str(),
        "sh" | "bash" | "zsh" | "shell" | "console" | "terminal"
    )
}

/// Realce por linha, sem dependência: comentário, string, número, palavra-chave.
/// Não é parser — é o suficiente para o olho achar a estrutura.
fn highlight(lang: &str, line: &str) -> Vec<(String, egui::Color32)> {
    let p = pal();
    let comment = comment_prefix(lang);
    if let Some(c) = comment {
        if line.trim_start().starts_with(c) {
            return vec![(line.to_string(), p.syn_com)];
        }
    }
    let kws = keywords(lang);
    let mut out: Vec<(String, egui::Color32)> = Vec::new();
    let mut buf = String::new();
    let mut chars = line.chars().peekable();

    let flush = |buf: &mut String, out: &mut Vec<(String, egui::Color32)>| {
        if buf.is_empty() {
            return;
        }
        let color = if kws.contains(&buf.as_str()) {
            p.syn_kw
        } else if buf.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            p.syn_num
        } else {
            p.text_dim
        };
        out.push((std::mem::take(buf), color));
    };

    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            flush(&mut buf, &mut out);
            let mut lit = String::from(c);
            for n in chars.by_ref() {
                lit.push(n);
                if n == c {
                    break;
                }
            }
            out.push((lit, p.syn_str));
        } else if c.is_alphanumeric() || c == '_' {
            buf.push(c);
        } else {
            flush(&mut buf, &mut out);
            out.push((c.to_string(), p.text_dim));
        }
    }
    flush(&mut buf, &mut out);
    out
}

fn comment_prefix(lang: &str) -> Option<&'static str> {
    match lang {
        "rs" | "rust" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "cpp" | "cs"
        | "swift" | "kt" => Some("//"),
        "py" | "python" | "sh" | "bash" | "rb" | "ruby" | "toml" | "yaml" | "yml" => Some("#"),
        "sql" | "lua" => Some("--"),
        _ => None,
    }
}

fn keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "rs" | "rust" => &[
            "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "use", "mod", "match",
            "if", "else", "for", "while", "loop", "return", "self", "Self", "const", "static",
            "async", "await", "move", "ref", "where", "type", "dyn", "crate", "as", "in",
        ],
        "py" | "python" => &[
            "def", "class", "import", "from", "return", "if", "elif", "else", "for", "while",
            "try", "except", "finally", "with", "as", "lambda", "yield", "async", "await",
            "pass", "raise", "in", "not", "and", "or", "None", "True", "False", "self",
        ],
        "js" | "ts" | "jsx" | "tsx" | "javascript" | "typescript" => &[
            "function", "const", "let", "var", "class", "return", "if", "else", "for", "while",
            "import", "export", "from", "async", "await", "new", "this", "type", "interface",
            "extends", "implements", "try", "catch", "finally", "throw", "null", "undefined",
        ],
        "go" => &[
            "func", "package", "import", "var", "const", "type", "struct", "interface", "return",
            "if", "else", "for", "range", "go", "defer", "chan", "select", "switch", "case",
        ],
        "sh" | "bash" => &[
            "if", "then", "else", "fi", "for", "in", "do", "done", "while", "case", "esac",
            "function", "return", "export", "local", "echo", "cd",
        ],
        "sql" => &[
            "SELECT", "FROM", "WHERE", "INSERT", "UPDATE", "DELETE", "JOIN", "LEFT", "INNER",
            "GROUP", "ORDER", "BY", "LIMIT", "CREATE", "TABLE", "INDEX", "ON", "AS", "AND", "OR",
        ],
        _ => &[],
    }
}

fn render_inline(ui: &mut Ui, text: &str) -> Option<MdAction> {
    let mut rest = text;
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        while !rest.is_empty() {
            // link http(s)://… — botão clicável, copia para o clipboard.
            // Button em vez de Label: labels selecionáveis engolem o clique.
            if let Some(i) = find_url(rest) {
                if i > 0 {
                    ui.label(RichText::new(&rest[..i]).size(BODY).color(pal().text));
                }
                let (url, consumed) = take_url(&rest[i..]);
                let url_owned = url.to_string();
                let resp = ui
                    .add(
                        egui::Button::new(
                            RichText::new(url)
                                .size(BODY)
                                .underline()
                                .color(pal().accent),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Click to copy");
                if resp.clicked() {
                    action = Some(MdAction::CopyText(url_owned));
                }
                rest = &rest[i + consumed..];
                continue;
            }
            if let Some(i) = rest.find("**") {
                if i > 0 {
                    ui.label(RichText::new(&rest[..i]).size(BODY).color(pal().text));
                }
                rest = &rest[i + 2..];
                if let Some(j) = rest.find("**") {
                    ui.label(RichText::new(&rest[..j]).strong().size(BODY).color(pal().text));
                    rest = &rest[j + 2..];
                } else {
                    ui.label(RichText::new(format!("**{rest}")).size(BODY));
                    break;
                }
            } else if let Some(i) = rest.find('`') {
                if i > 0 {
                    ui.label(RichText::new(&rest[..i]).size(BODY).color(pal().text));
                }
                rest = &rest[i + 1..];
                if let Some(j) = rest.find('`') {
                    ui.label(
                        RichText::new(&rest[..j])
                            .monospace()
                            .size(13.0)
                            .color(pal().text_dim),
                    );
                    rest = &rest[j + 1..];
                } else {
                    ui.label(RichText::new(format!("`{rest}")).size(BODY));
                    break;
                }
            } else {
                ui.label(RichText::new(rest).size(BODY).color(pal().text));
                break;
            }
        }
    });
    action
}

/// Índice do início do primeiro URL `http://` ou `https://` na string.
fn find_url(s: &str) -> Option<usize> {
    let mut idx = 0;
    let bytes = s.as_bytes();
    while idx + 8 <= bytes.len() {
        if bytes[idx..].starts_with(b"http://") || bytes[idx..].starts_with(b"https://") {
            return Some(idx);
        }
        idx += 1;
    }
    None
}

/// Consome um URL a partir do início; devolve (fatia, bytes consumidos).
fn take_url(s: &str) -> (&str, usize) {
    let end = s
        .char_indices()
        .find(|(i, c)| {
            *i > 0 && matches!(c, ' ' | '\t' | '\n' | ')' | ']' | '}' | '"' | '\'' | ',')
        })
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    (&s[..end], end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comentario_inteiro_vira_uma_peca() {
        let out = highlight("rust", "  // isto é comentário com \"aspas\"");
        assert_eq!(out.len(), 1, "comentário não deve ser tokenizado");
    }

    #[test]
    fn palavra_chave_string_e_numero_separam() {
        let out = highlight("rust", "let x = \"oi\" + 42;");
        let kw = out.iter().find(|(t, _)| t == "let").expect("let");
        let st = out.iter().find(|(t, _)| t == "\"oi\"").expect("string");
        let nm = out.iter().find(|(t, _)| t == "42").expect("número");
        assert_ne!(kw.1, st.1);
        assert_ne!(st.1, nm.1);
        // o texto reconstruído tem que ser idêntico ao original
        let joined: String = out.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, "let x = \"oi\" + 42;");
    }

    #[test]
    fn linguagem_desconhecida_nao_quebra() {
        let line = "algo :: qualquer 'coisa'";
        let out = highlight("brainfuck", line);
        let joined: String = out.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, line);
    }

    #[test]
    fn acha_url_no_meio_do_texto() {
        assert_eq!(find_url("veja https://example.com/x agora"), Some(5));
        assert_eq!(find_url("sem link aqui"), None);
        assert_eq!(find_url("http://a.io"), Some(0));
    }

    #[test]
    fn take_url_para_antes_do_espaco_ou_pontuacao() {
        let (u, n) = take_url("https://example.com/x) resto");
        assert_eq!(u, "https://example.com/x");
        assert_eq!(n, "https://example.com/x".len());
        let (u2, _) = take_url("https://a.io");
        assert_eq!(u2, "https://a.io");
    }

    #[test]
    fn bloco_shell_e_reconhecido() {
        assert!(is_shell_lang("sh"));
        assert!(is_shell_lang("bash"));
        assert!(is_shell_lang(" shell "));
        assert!(!is_shell_lang("rust"));
    }
}
