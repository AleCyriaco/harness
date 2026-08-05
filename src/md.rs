//! Lightweight markdown-ish rendering for chat (no browser).

use egui::{RichText, Ui};

use crate::theme::pal;

const BODY: f32 = 14.5;

/// Render a subset of markdown: headings, bold, bullets, code fences, bare lines.
pub fn render_markdown(ui: &mut Ui, text: &str) {
    let text = crate::mermaid_lite::expand_in_markdown(text);
    let mut in_code = false;
    let mut code_buf = String::new();

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                egui::Frame::new()
                    .fill(pal().code_bg)
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(code_buf.trim_end())
                                    .monospace()
                                    .size(12.5)
                                    .color(pal().text_dim),
                            )
                            .wrap(),
                        );
                    });
                code_buf.clear();
                in_code = false;
                ui.add_space(6.0);
            } else {
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
        ui.add(
            egui::Label::new(
                RichText::new(code_buf.trim_end())
                    .monospace()
                    .size(12.5)
                    .color(pal().text_dim),
            )
            .wrap(),
        );
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
