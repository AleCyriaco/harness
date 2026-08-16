//! Loop de aprendizado: um turno que fez algo não-trivial vira rascunho de
//! skill, para o próximo turno igual já começar sabendo o caminho.
//!
//! Módulo puro: decide se vale e monta o markdown. Quem grava e avisa é o
//! `app.rs`. cyrix: **rascunho** — o harness cria, o usuário mantém ou apaga.
//! Não reescreve skill sozinho (isso é chamada de LLM); a versão nova sai do
//! `skill_save` que já versiona.

use crate::live::ToolEvt;

/// Tools que representam trabalho de verdade (não só olhar).
fn is_work(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "replace_in_file" | "multiedit" | "apply_patch"
            | "run_command" | "create_doc" | "create_sheet" | "create_pdf"
            | "graph_build" | "web_crawl" | "bg_start"
    )
}

/// Vale virar skill? Turno que falhou não ensina; turno raso também não.
pub fn is_worthy(tools: &[ToolEvt], failed: bool, min_steps: u32) -> bool {
    if failed || min_steps == 0 {
        return false;
    }
    let work = tools.iter().filter(|t| is_work(&t.name)).count();
    work as u32 >= min_steps
}

/// `Título Legível` → `titulo-legivel`, sem depender de crate.
pub fn slug(goal: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in goal.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if (c.is_whitespace() || c == '-' || c == '_') && !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
        // demais (acento, pontuação) são descartados, não viram separador
    }
    let s = out.trim_matches('-');
    s.chars().take(40).collect::<String>().trim_matches('-').to_string()
}

/// Rascunho de skill a partir do objetivo e da sequência de tools do turno.
/// Devolve `(nome, markdown)` — o markdown já sai no formato que `save_skill`
/// entende (frontmatter + corpo).
pub fn draft_skill(goal: &str, tools: &[ToolEvt]) -> (String, String) {
    let name = {
        let s = slug(goal);
        if s.is_empty() { "learned-skill".into() } else { s }
    };
    // passos = tools de trabalho, na ordem, com o alvo
    let mut steps = String::new();
    let mut n = 0;
    for t in tools.iter().filter(|t| is_work(&t.name)) {
        n += 1;
        let target = t.arg.lines().next().unwrap_or("").chars().take(60).collect::<String>();
        steps.push_str(&format!("{n}. `{}` {}\n", t.name, target.trim()));
    }
    let triggers = slug(goal).replace('-', ", ");
    let body = format!(
        "# {goal}\n\n\
         DRAFT — harness wrote this from a turn that worked. Keep it, edit it, \
         or delete the file. Improve it with `skill_save` (it versions).\n\n\
         ## Steps that worked\n{steps}\n\
         ## When to use\nSame goal as above.\n",
        goal = goal.trim(),
    );
    let md = format!(
        "---\nname: {name}\nversion: 1\ndescription: learned from a turn — {}\n\
         triggers: {triggers}\n---\n{body}",
        goal.trim().chars().take(80).collect::<String>(),
    );
    (name, md)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str, arg: &str) -> ToolEvt {
        ToolEvt {
            name: name.into(),
            arg: arg.into(),
            result: "ok".into(),
            done: true,
        }
    }

    #[test]
    fn so_vale_com_trabalho_suficiente_e_sem_falha() {
        let tools = vec![ev("read_file", "a"), ev("write_file", "b"), ev("run_command", "c")];
        assert!(is_worthy(&tools, false, 2), "2 tools de trabalho >= 2");
        assert!(!is_worthy(&tools, true, 2), "turno que falhou não ensina");
        assert!(!is_worthy(&tools, false, 5), "raso demais para o teto 5");
        // só leitura não conta como trabalho
        let reads = vec![ev("read_file", "a"), ev("list_dir", "b")];
        assert!(!is_worthy(&reads, false, 1));
    }

    #[test]
    fn slug_vira_nome_de_arquivo() {
        assert_eq!(slug("Criar jogo Pac-Man!"), "criar-jogo-pac-man");
        assert_eq!(slug("  espaços   e --- traços "), "espaos-e-traos");
        assert!(slug("").is_empty());
    }

    #[test]
    fn rascunho_salva_e_recarrega_como_skill_de_verdade() {
        let root = std::env::temp_dir().join(format!("harness_learn_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let tools = vec![ev("write_file", "a.rs"), ev("run_command", "cargo test")];
        assert!(is_worthy(&tools, false, 2));
        let (name, md) = draft_skill("configurar CI", &tools);
        crate::skills::save_skill(&root, &name, &md).unwrap();
        let back = crate::skills::load_skill(&root, &name).expect("skill volta do disco");
        assert_eq!(back.name, "configurar-ci");
        assert_eq!(back.version, 1);
        assert!(back.body.contains("write_file"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rascunho_tem_frontmatter_e_os_passos_de_trabalho() {
        let tools = vec![
            ev("read_file", "mole/README.md"),
            ev("write_file", "gui/App.swift"),
            ev("run_command", "mkdir -p gui"),
        ];
        let (name, md) = draft_skill("Portar CLI para GUI", &tools);
        assert_eq!(name, "portar-cli-para-gui");
        assert!(md.starts_with("---\nname: portar-cli-para-gui"));
        assert!(md.contains("write_file"), "passo de trabalho entra");
        assert!(md.contains("mkdir -p gui"));
        assert!(!md.contains("read_file"), "leitura pura não vira passo");
        assert!(md.contains("DRAFT"), "deixa claro que é rascunho");
    }
}
