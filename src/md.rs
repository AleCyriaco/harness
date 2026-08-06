//! Lightweight markdown-ish rendering for chat (no browser).

use egui::{RichText, Ui};

use crate::theme::pal;

const BODY: f32 = 14.5;

/// Render a subset of markdown: headings, bold, bullets, code fences, bare lines.
pub fn render_markdown(ui: &mut Ui, text: &str) {
    let text = crate::mermaid_lite::expand_in_markdown(text);
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut lang = String::new();

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                code_block(ui, &lang, code_buf.trim_end());
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
        if let Some(rest) = t.strip_prefix("### ") {
            ui.label(RichText::new(rest).strong().size(14.5).color(pal().text));
        } else if let Some(rest) = t.strip_prefix("## ") {
            ui.label(RichText::new(rest).strong().size(15.5).color(pal().text));
        } else if let Some(rest) = t.strip_prefix("# ") {
            ui.label(RichText::new(rest).strong().size(17.0).color(pal().text));
        } else if let Some(rest) = t.strip_prefix("- ") {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("•").color(pal().muted));
                render_inline(ui, rest);
            });
        } else if let Some(rest) = t.strip_prefix("* ") {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("•").color(pal().muted));
                render_inline(ui, rest);
            });
        } else {
            render_inline(ui, t);
        }
    }
    if in_code && !code_buf.is_empty() {
        code_block(ui, &lang, code_buf.trim_end());
    }
}

/// Bloco de código: cabeçalho com a linguagem e botão de copiar, corpo realçado.
fn code_block(ui: &mut Ui, lang: &str, code: &str) {
    let p = pal();
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
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("copy").monospace().size(10.0).color(p.muted),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        ui.ctx().copy_text(code.to_string());
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

fn render_inline(ui: &mut Ui, text: &str) {
    let mut rest = text;
    ui.horizontal_wrapped(|ui| {
        while !rest.is_empty() {
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
}
