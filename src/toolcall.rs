//! Tool calls estruturados dentro do `ui_log` (role = "tool").
//!
//! O log de sessão em disco continua sendo `{role, text}`; a estrutura viaja
//! codificada no próprio `text` com um marcador de controle, então sessões
//! antigas (texto solto) continuam abrindo — só caem no render legado.

const MARK: char = '\u{1}';
const SEP: char = '\u{1f}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Ok,
    Err,
    NeedsApproval,
}

impl ToolState {
    fn tag(self) -> &'static str {
        match self {
            ToolState::Running => "run",
            ToolState::Ok => "ok",
            ToolState::Err => "err",
            ToolState::NeedsApproval => "ask",
        }
    }

    fn from_tag(t: &str) -> Self {
        match t {
            "ok" => ToolState::Ok,
            "err" => ToolState::Err,
            "ask" => ToolState::NeedsApproval,
            _ => ToolState::Running,
        }
    }

    pub fn is_final(self) -> bool {
        matches!(self, ToolState::Ok | ToolState::Err)
    }
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    /// "write_file"
    pub name: String,
    /// "src/lexer/token.rs" — o argumento que interessa
    pub target: String,
    /// "412 linhas", "48 passed", "erro"
    pub metric: String,
    /// corpo completo (resultado / preview dos args enquanto roda)
    pub body: String,
    pub state: ToolState,
}

impl ToolCall {
    pub fn start(name: &str, args: &str) -> Self {
        Self {
            name: name.to_string(),
            target: target_from_args(args),
            metric: String::new(),
            body: args.to_string(),
            state: ToolState::Running,
        }
    }

    pub fn finish(&mut self, result: &str) {
        self.state = if looks_like_error(result) {
            ToolState::Err
        } else {
            ToolState::Ok
        };
        self.metric = metric_from_result(result);
        self.body = result.to_string();
    }

    pub fn encode(&self) -> String {
        format!(
            "{MARK}{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}",
            self.state.tag(),
            self.name,
            self.target,
            self.metric,
            self.body
        )
    }

    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(MARK)?;
        let mut it = rest.splitn(5, SEP);
        let state = ToolState::from_tag(it.next()?);
        Some(Self {
            state,
            name: it.next()?.to_string(),
            target: it.next()?.to_string(),
            metric: it.next()?.to_string(),
            body: it.next().unwrap_or("").to_string(),
        })
    }
}

fn looks_like_error(result: &str) -> bool {
    let head = result.trim_start();
    let lower: String = head.chars().take(60).collect::<String>().to_lowercase();
    lower.starts_with("error")
        || lower.starts_with("erro")
        || lower.starts_with("failed")
        || lower.starts_with("falha")
        || lower.starts_with("denied")
        || lower.starts_with("panic")
}

/// Um número honesto: linhas quando é multi-linha, caracteres quando não é.
fn metric_from_result(result: &str) -> String {
    if looks_like_error(result) {
        return "error".into();
    }
    let trimmed = result.trim_end();
    if trimmed.is_empty() {
        return "ok".into();
    }
    let lines = trimmed.lines().count();
    if lines > 1 {
        format!("{lines} lines")
    } else {
        format!("{} chars", trimmed.chars().count())
    }
}

/// Chaves que costumam carregar "o que" a chamada tocou.
const TARGET_KEYS: &[&str] = &[
    "path",
    "file",
    "file_path",
    "command",
    "cmd",
    "query",
    "pattern",
    "url",
    "name",
    "title",
    "task",
    "text",
];

fn target_from_args(args: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
        for k in TARGET_KEYS {
            if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
                if !s.trim().is_empty() {
                    return one_line(s, 72);
                }
            }
        }
    }
    one_line(args, 72)
}

pub fn one_line(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_state_and_body() {
        let mut tc = ToolCall::start("write_file", r#"{"path":"src/a.rs"}"#);
        assert_eq!(tc.target, "src/a.rs");
        assert_eq!(tc.state, ToolState::Running);
        tc.finish("a\nb\nc");
        let back = ToolCall::parse(&tc.encode()).expect("parse");
        assert_eq!(back.state, ToolState::Ok);
        assert_eq!(back.metric, "3 lines");
        assert_eq!(back.body, "a\nb\nc");
        assert_eq!(back.name, "write_file");
    }

    #[test]
    fn errors_are_flagged() {
        let mut tc = ToolCall::start("shell", r#"{"command":"false"}"#);
        tc.finish("error: exit 1");
        assert_eq!(tc.state, ToolState::Err);
        assert_eq!(tc.metric, "error");
    }

    #[test]
    fn legacy_plain_text_is_not_a_toolcall() {
        // sessões antigas gravaram "▶ nome(args)" — devem cair no render legado
        assert!(ToolCall::parse("▶ read_file({})").is_none());
    }
}
