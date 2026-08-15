//! Convenções do projeto apontado: `AGENTS.md`, `CLAUDE.md`, `HARNESS.md`.
//!
//! Entra no **system prompt**, não numa mensagem volátil: o conteúdo é estável
//! durante o chat inteiro, então ele ajuda o cache de prefixo em vez de quebrá-lo.

use std::path::Path;

/// Ordem de procura. O primeiro que existir manda.
pub const NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md", "HARNESS.md", ".cursorrules"];

/// `(nome, conteúdo)` do arquivo de convenções na raiz, se houver.
pub fn find(root: &Path, max_chars: usize) -> Option<(String, String)> {
    for name in NAMES {
        let p = root.join(name);
        if !p.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&p).ok()?;
        if raw.trim().is_empty() {
            continue;
        }
        let text: String = raw.chars().take(max_chars).collect();
        let text = if raw.chars().count() > max_chars {
            format!("{text}\n[truncated]")
        } else {
            text
        };
        return Some((name.to_string(), text));
    }
    None
}

/// Bloco pronto para o system prompt. Vazio quando não há arquivo.
pub fn block(root: &Path, on: bool, max_chars: usize) -> String {
    if !on {
        return String::new();
    }
    match find(root, max_chars) {
        Some((name, text)) => format!(
            "\n\nPROJECT CONVENTIONS (from {name} — follow them over your defaults):\n{text}"
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("harness_instr_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn acha_agents_md_e_respeita_a_ordem() {
        let root = tmp();
        std::fs::write(root.join("CLAUDE.md"), "regra do claude").unwrap();
        assert_eq!(find(&root, 999).unwrap().0, "CLAUDE.md");
        std::fs::write(root.join("AGENTS.md"), "regra do agents").unwrap();
        let (name, text) = find(&root, 999).unwrap();
        assert_eq!(name, "AGENTS.md", "AGENTS.md tem precedência");
        assert!(text.contains("regra do agents"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sem_arquivo_ou_desligado_nao_injeta_nada() {
        let root = tmp();
        assert!(find(&root, 999).is_none());
        assert!(block(&root, true, 999).is_empty());
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        assert!(block(&root, false, 999).is_empty(), "desligado não lê");
        assert!(block(&root, true, 999).contains("PROJECT CONVENTIONS"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn arquivo_gigante_e_cortado_e_avisa() {
        let root = tmp();
        std::fs::write(root.join("AGENTS.md"), "a".repeat(5_000)).unwrap();
        let (_, text) = find(&root, 100).unwrap();
        assert!(text.ends_with("[truncated]"));
        assert!(text.chars().count() < 200);
        std::fs::remove_dir_all(&root).ok();
    }
}
