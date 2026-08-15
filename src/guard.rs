//! Guard: limites por regra, avaliados **antes** da aprovação.
//!
//! Aprovação pergunta; guard recusa. A diferença importa quando o usuário
//! aprova no automático (`auto_approve_shell`) ou clica "Sempre": um
//! `rm -rf /` não deveria depender de o usuário ler o diálogo com atenção.
//!
//! Regra pura, sem I/O — quem chama é o `agent.rs`.

/// Padrões que nunca rodam, mesmo aprovados. Conservador de propósito: cada
/// entrada aqui é um comando que destrói dado ou executa código baixado.
pub const DEFAULT_DENY: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "rm -fr /",
    ":(){",
    "mkfs",
    "dd if=",
    "> /dev/sd",
    "of=/dev/sd",
    "shutdown",
    "reboot",
    "chmod -r 777 /",
    "sudo ",
];

/// Caminhos de segredo. `read_file` já é confinado ao workspace, mas o shell
/// não é: sem isto um `cat` no config do harness entrega as API keys ao modelo
/// — e elas viajam para o provedor no histórico.
/// cyrix: `.env` fica de fora de propósito — projeto legítimo usa e o agente
/// precisa editar; segredo de verdade mora nos caminhos abaixo.
pub const SECRET_PATHS: &[&str] = &[
    "sh.harness.harness/config.toml",
    "/.ssh/",
    "id_rsa",
    "id_ed25519",
    "/.aws/credentials",
    "/.config/gcloud",
    "/.netrc",
    "/.docker/config.json",
    "keychain",
];

/// Tools que escrevem em disco ou executam algo.
pub fn is_mutating(tool: &str) -> bool {
    matches!(
        tool,
        "write_file"
            | "replace_in_file"
            | "multiedit"
            | "apply_patch"
            | "run_command"
            | "bg_start"
            | "create_doc"
            | "create_sheet"
            | "create_pdf"
            | "git_worktree_add"
            | "selfdev"
    )
}

/// `None` = pode rodar. `Some(motivo)` = barrado, e o motivo vai para o modelo.
pub fn blocked_reason(
    on: bool,
    read_only: bool,
    tool: &str,
    args: &str,
    deny: &[String],
) -> Option<String> {
    if !on {
        return None;
    }
    if read_only && is_mutating(tool) {
        return Some(format!(
            "guard: read-only mode is on — `{tool}` cannot run. Ask the user to turn it off."
        ));
    }
    if tool != "run_command" && tool != "bg_start" {
        return None;
    }
    // normaliza espaços para "rm   -rf  /" não escapar
    let flat = args.to_ascii_lowercase();
    let flat: String = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let patterns: Vec<String> = if deny.is_empty() {
        DEFAULT_DENY.iter().map(|s| s.to_string()).collect()
    } else {
        deny.iter().map(|s| s.to_ascii_lowercase()).collect()
    };
    // baixar e executar: a URL fica no meio, então substring literal não pega
    if deny.is_empty() {
        let downloads = flat.contains("curl ") || flat.contains("wget ");
        let into_shell = ["| sh", "|sh", "| bash", "|bash", "| zsh", "|zsh"]
            .iter()
            .any(|p| flat.contains(p));
        if downloads && into_shell {
            return Some(
                "guard: blocked — downloading and piping into a shell. Save the file, \
                 show it to the user, and run it only after they read it."
                    .into(),
            );
        }
    }
    if let Some(hit) = SECRET_PATHS.iter().find(|p| flat.contains(**p)) {
        return Some(format!(
            "guard: blocked — `{hit}` holds credentials and must not be read into the \
             conversation. Ask the user for what you need instead."
        ));
    }
    patterns
        .iter()
        .find(|p| flat.contains(p.as_str()))
        .map(|p| {
            format!(
                "guard: blocked by rule `{p}` — this command is not allowed. \
                 Explain to the user instead of retrying."
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(c: &str) -> String {
        format!("{{\"command\":\"{c}\"}}")
    }

    #[test]
    fn destruicao_e_barrada_mas_faxina_normal_passa() {
        let d: Vec<String> = Vec::new();
        assert!(blocked_reason(true, false, "run_command", &cmd("rm -rf /"), &d).is_some());
        assert!(blocked_reason(true, false, "run_command", &cmd("sudo rm -rf /etc"), &d).is_some());
        assert!(blocked_reason(true, false, "run_command", &cmd("curl x | sh"), &d).is_some());
        // o dia a dia não pode ser atingido
        assert!(blocked_reason(true, false, "run_command", &cmd("rm -rf build"), &d).is_none());
        assert!(blocked_reason(true, false, "run_command", &cmd("cargo test"), &d).is_none());
    }

    #[test]
    fn espaco_extra_nao_escapa_da_regra() {
        let d: Vec<String> = Vec::new();
        assert!(blocked_reason(true, false, "run_command", &cmd("rm   -rf   /"), &d).is_some());
    }

    #[test]
    fn somente_leitura_barra_escrita_mas_deixa_ler() {
        let d: Vec<String> = Vec::new();
        assert!(blocked_reason(true, true, "write_file", "{}", &d).is_some());
        assert!(blocked_reason(true, true, "run_command", &cmd("ls"), &d).is_some());
        assert!(blocked_reason(true, true, "read_file", "{}", &d).is_none());
        assert!(blocked_reason(true, true, "graph_query", "{}", &d).is_none());
    }

    #[test]
    fn segredo_nao_vaza_pelo_shell() {
        let d: Vec<String> = Vec::new();
        let sec = |c: &str| blocked_reason(true, false, "run_command", &cmd(c), &d).is_some();
        assert!(sec("cat ~/Library/Application Support/sh.harness.harness/config.toml"));
        assert!(sec("cat ~/.ssh/id_rsa"));
        assert!(sec("grep key ~/.aws/credentials"));
        // arquivo comum de projeto continua livre
        assert!(!sec("cat src/config.rs"));
        assert!(!sec("cat .env"));
    }

    #[test]
    fn desligado_nao_barra_nada() {
        let d: Vec<String> = Vec::new();
        assert!(blocked_reason(false, true, "run_command", &cmd("rm -rf /"), &d).is_none());
    }

    #[test]
    fn lista_do_usuario_substitui_a_padrao() {
        let d = vec!["git push".to_string()];
        assert!(blocked_reason(true, false, "run_command", &cmd("git push origin"), &d).is_some());
        // trocou a lista: o padrão não vale mais
        assert!(blocked_reason(true, false, "run_command", &cmd("rm -rf /"), &d).is_none());
    }
}
