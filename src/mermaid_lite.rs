//! Minimal mermaid-ish diagram rendering as ASCII/unicode for chat (no browser).

/// Convert a tiny subset of mermaid flowchart to text art.
pub fn render_mermaid_block(src: &str) -> String {
    let mut lines = Vec::new();
    lines.push("┌─ diagram ─────────────────────".into());
    for raw in src.lines() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with("graph") || l.starts_with("flowchart") {
            continue;
        }
        // A-->B / A-->|label|B / A---B
        if let Some((left, right)) = l.split_once("-->").or_else(|| l.split_once("---")) {
            let a = clean_node(left);
            let b = clean_node(right);
            lines.push(format!("  ({a}) ──▶ ({b})"));
        } else if l.contains('[') {
            lines.push(format!("  • {}", clean_node(l)));
        } else {
            lines.push(format!("  {l}"));
        }
    }
    lines.push("└───────────────────────────────".into());
    lines.join("\n")
}

fn clean_node(s: &str) -> String {
    s.replace(['[', ']', '(', ')', '{', '}', '|', '"', '\''], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Expand ```mermaid blocks in markdown text to ascii diagrams.
pub fn expand_in_markdown(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("```mermaid") {
            let mut body = String::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                body.push_str(l);
                body.push('\n');
            }
            out.push_str(&render_mermaid_block(&body));
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
