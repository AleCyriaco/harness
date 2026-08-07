use serde::{Deserialize, Serialize};

/// Interaction mode — changes tools, prompt, and memory policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppMode {
    /// Coding agent: lean tools, compact history, low RAM.
    #[default]
    Code,
    /// Office + general chat: docs, sheets, PDFs, light code tools.
    Office,
}

impl AppMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Code => "Code",
            Self::Office => "Office",
        }
    }
}
