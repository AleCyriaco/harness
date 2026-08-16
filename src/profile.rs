//! Modelo do usuário: um markdown que o agente atualiza e consulta entre
//! sessões. Fatos duráveis sobre quem você é e como trabalha — não o histórico
//! de um chat. Vive fora do workspace (data dir), então acompanha você em
//! qualquer projeto.
//!
//! Vai no **system prompt** (é estável durante o chat, então ajuda o cache).
//! O agente escreve nele com a tool `profile_note`.

use anyhow::Result;
use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    directories::ProjectDirs::from("sh", "harness", "harness")
        .map(|d| d.data_dir().join("profile.md"))
}

pub fn read() -> String {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// Junta um fato ao perfil. Dedup por linha (case-insensitive), teto de linhas
/// para não virar despejo. Retorna `true` se algo mudou.
pub fn add(fact: &str) -> Result<bool> {
    let fact = fact.trim();
    if fact.is_empty() {
        return Ok(false);
    }
    let (Some(p),) = (path(),) else {
        return Ok(false);
    };
    let cur = read();
    let bullet = normalize(fact);
    if cur.lines().any(|l| normalize(l) == bullet) {
        return Ok(false); // já sabemos disso
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lines: Vec<String> = cur.lines().map(str::to_string).collect();
    lines.push(format!("- {fact}"));
    // teto: mantém as últimas 60 (as mais recentes ganham)
    let start = lines.len().saturating_sub(60);
    let kept = lines[start..].join("\n");
    std::fs::write(&p, format!("{}\n", kept.trim()))?;
    Ok(true)
}

fn normalize(s: &str) -> String {
    s.trim().trim_start_matches('-').trim().to_lowercase()
}

/// Bloco para o system prompt. Vazio quando desligado ou sem perfil.
pub fn block(on: bool) -> String {
    if !on {
        return String::new();
    }
    let p = read();
    let p = p.trim();
    if p.is_empty() {
        return String::new();
    }
    format!(
        "\n\nABOUT THE USER (learned across sessions — use it, keep it current \
         with `profile_note` when you learn something durable):\n{p}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desligado_ou_vazio_nao_injeta() {
        // não escreve nada em disco; só o caminho puro
        assert!(block(false).is_empty());
    }

    #[test]
    fn normalize_ignora_marcador_e_caixa() {
        assert_eq!(normalize("- Prefere Rust"), normalize("prefere rust"));
        assert_eq!(normalize("  Trabalha com Oracle "), "trabalha com oracle");
    }
}
