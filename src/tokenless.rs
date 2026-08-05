//! Token Less Cost — comprime a *prosa* da resposta para gastar menos tokens
//! de saída, sem tocar em código, comandos, caminhos ou mensagens de erro.
//!
//! Ideia emprestada do projeto "caveman"
//! (https://github.com/juliusbrussee/caveman), uma skill de compressão de
//! tokens para agentes. Aqui vira uma diretiva no system prompt, ligável por
//! sessão/chat/aba (`SessionMeta::token_less`) com padrão global em
//! `Config::token_less`. Na UI a feature se chama **Token Less Cost**.
//!
//! Ressalva honesta, a mesma do projeto original: isto encolhe **só a saída**.
//! Entrada, histórico e tokens de raciocínio seguem iguais, então a economia
//! numa sessão real de código é bem menor que a de uma resposta em prosa.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenLessLevel {
    #[default]
    Off,
    Lite,
    Full,
    Ultra,
}

impl TokenLessLevel {
    pub const ALL: [TokenLessLevel; 4] = [
        TokenLessLevel::Off,
        TokenLessLevel::Lite,
        TokenLessLevel::Full,
        TokenLessLevel::Ultra,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            TokenLessLevel::Off => "off",
            TokenLessLevel::Lite => "lite",
            TokenLessLevel::Full => "full",
            TokenLessLevel::Ultra => "ultra",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TokenLessLevel::Off => "off",
            TokenLessLevel::Lite => "lite — trims filler",
            TokenLessLevel::Full => "full — telegraphic",
            TokenLessLevel::Ultra => "ultra — fragments only",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "no" | "nao" | "não" | "desligado" | "0" => Some(TokenLessLevel::Off),
            "lite" | "leve" | "1" => Some(TokenLessLevel::Lite),
            "full" | "on" | "sim" | "2" => Some(TokenLessLevel::Full),
            "ultra" | "max" | "3" => Some(TokenLessLevel::Ultra),
            _ => None,
        }
    }

    pub fn next(self) -> Self {
        match self {
            TokenLessLevel::Off => TokenLessLevel::Lite,
            TokenLessLevel::Lite => TokenLessLevel::Full,
            TokenLessLevel::Full => TokenLessLevel::Ultra,
            TokenLessLevel::Ultra => TokenLessLevel::Off,
        }
    }

    pub fn is_on(self) -> bool {
        self != TokenLessLevel::Off
    }
}

const PRESERVE: &str = "NEVER compress or paraphrase: code, diffs, shell commands, file paths, \
identifiers, error strings, URLs and quoted text — reproduce them byte-for-byte. \
Never drop substance: risks, caveats and the actual answer stay, just shorter. \
Keep the user's language.";

/// Trecho a acrescentar ao system prompt. `None` quando desligado.
pub fn directive(level: TokenLessLevel) -> Option<String> {
    let body = match level {
        TokenLessLevel::Off => return None,
        TokenLessLevel::Lite => {
            "Trim filler: no greetings, no restating the question, no \"I will now…\", \
             no summary of what you just did, no hedging. Normal sentences, fewer of them."
        }
        TokenLessLevel::Full => {
            "Talk terse. Drop filler, greetings, restatements, hedging and closing summaries. \
             Prefer fragments to full sentences; cut articles and copulas when meaning survives. \
             Lead with the finding, not the narration. One line per idea."
        }
        TokenLessLevel::Ultra => {
            "Maximum compression. Fragments only, no connective prose, no transitions. \
             Aim for ≤10 words per line. Bullet lists over paragraphs. \
             State cause and fix; skip everything else."
        }
    };
    Some(format!(
        "OUTPUT STYLE — TOKEN LESS COST ({})\n{body}\n{PRESERVE}",
        level.tag().to_uppercase()
    ))
}

/// Aplica no primeiro `system` do histórico.
pub fn apply_to_system(content: &mut String, level: TokenLessLevel) {
    if let Some(d) = directive(level) {
        content.push_str("\n\n");
        content.push_str(&d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_has_no_directive() {
        assert!(directive(TokenLessLevel::Off).is_none());
        let mut sys = "base".to_string();
        apply_to_system(&mut sys, TokenLessLevel::Off);
        assert_eq!(sys, "base");
    }

    #[test]
    fn levels_append_and_preserve_code_rule() {
        for l in [TokenLessLevel::Lite, TokenLessLevel::Full, TokenLessLevel::Ultra] {
            let mut sys = "base".to_string();
            apply_to_system(&mut sys, l);
            assert!(sys.starts_with("base"));
            assert!(sys.contains("TOKEN LESS COST"));
            assert!(sys.contains("byte-for-byte"), "{l:?} must protect code");
        }
    }

    #[test]
    fn parse_and_cycle() {
        assert_eq!(TokenLessLevel::parse("ULTRA"), Some(TokenLessLevel::Ultra));
        assert_eq!(TokenLessLevel::parse("desligado"), Some(TokenLessLevel::Off));
        assert_eq!(TokenLessLevel::parse("banana"), None);
        // o chip do composer cicla e volta ao começo
        let mut l = TokenLessLevel::Off;
        for _ in 0..4 {
            l = l.next();
        }
        assert_eq!(l, TokenLessLevel::Off);
    }
}
