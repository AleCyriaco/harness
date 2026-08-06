//! Skills — playbooks em markdown com frontmatter, versionados em disco.
//!
//! A ideia de "skill de verdade" (versão, fronteira de disparo, validação) vem
//! do TencentDB Agent Memory. Aqui ela cabe em arquivos: nada de serviço, nada
//! de banco. Cada `save` arquiva o corpo anterior em `.versions/{nome}/v{n}.md`,
//! então dá para ver o que mudou e voltar atrás.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Skill {
    pub name: String,
    /// Corpo sem o frontmatter.
    pub body: String,
    pub description: String,
    /// Sobe a cada `save`; 1 = primeira versão.
    pub version: u32,
    /// Palavras que ligam a skill.
    pub triggers: Vec<String>,
    /// Fronteira: se a pergunta casa aqui, a skill **não** se aplica.
    pub not_when: Vec<String>,
    /// Como saber que deu certo (texto livre, vai junto no prompt).
    pub validate: String,
}

impl Skill {
    /// Reconstrói o arquivo inteiro: frontmatter + corpo.
    pub fn to_markdown(&self) -> String {
        let mut fm = format!(
            "---\nname: {}\nversion: {}\ndescription: {}\n",
            self.name, self.version, self.description
        );
        if !self.triggers.is_empty() {
            fm.push_str(&format!("triggers: {}\n", self.triggers.join(", ")));
        }
        if !self.not_when.is_empty() {
            fm.push_str(&format!("not_when: {}\n", self.not_when.join(", ")));
        }
        if !self.validate.is_empty() {
            fm.push_str(&format!("validate: {}\n", self.validate));
        }
        fm.push_str("---\n");
        fm.push_str(self.body.trim_start_matches('\n'));
        fm
    }
}

pub fn skills_dir(workspace: &Path) -> PathBuf {
    workspace.join(".harness").join("skills")
}

fn versions_dir(workspace: &Path, name: &str) -> PathBuf {
    skills_dir(workspace).join(".versions").join(name)
}

/// Separa o frontmatter (entre `---`) do corpo. Sem fence, tudo é corpo.
fn split_frontmatter(raw: &str) -> (Vec<(String, String)>, String) {
    let t = raw.trim_start_matches('\u{feff}');
    let Some(rest) = t.strip_prefix("---") else {
        return (Vec::new(), raw.to_string());
    };
    let rest = rest.trim_start_matches('\n');
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), raw.to_string());
    };
    let (head, tail) = rest.split_at(end);
    let body = tail.trim_start_matches("\n---").trim_start_matches('\n');
    let pairs = head
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    (pairs, body.to_string())
}

fn csv(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_skill(path: &Path, raw: &str) -> Skill {
    let (fm, body) = split_frontmatter(raw);
    let get = |k: &str| {
        fm.iter()
            .find(|(a, _)| a == k)
            .map(|(_, b)| b.clone())
            .unwrap_or_default()
    };
    let file_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skill".into());
    let name = {
        let n = get("name");
        if n.is_empty() { file_name } else { n }
    };
    Skill {
        name,
        body,
        description: get("description"),
        version: get("version").parse().unwrap_or(1),
        triggers: csv(&get("triggers")),
        not_when: csv(&get("not_when")),
        validate: get("validate"),
    }
}

pub fn ensure_default_skills(workspace: &Path) -> Result<()> {
    let dir = skills_dir(workspace);
    fs::create_dir_all(&dir)?;
    let defaults = [
        (
            "web-app.md",
            r#"---
name: web-app
version: 1
description: Build static web apps under web/ and preview inside harness
triggers: web, html, css, static site, landing page, front-end
not_when: backend, api, server-side, database
validate: web/index.html exists and the static server serves it
---
# Web app skill
1. Put HTML/CSS/JS under `web/`
2. Prefer `web/index.html` as entry
3. Start the server with path `web` and open it in the harness WebView
4. Keep asset paths relative so the local server works
"#,
        ),
        (
            "rust-project.md",
            r#"---
name: rust-project
version: 1
description: Scaffold and verify small Rust projects
triggers: rust, cargo, crate, .rs
not_when: python, javascript, typescript, go
validate: cargo check passes and get_diagnostics is clean
---
# Rust skill
1. Put sources under `code/`
2. Verify with `cargo check` / `cargo test`
3. Use replace_in_file for small edits
4. Run get_diagnostics after changes
"#,
        ),
    ];
    for (file, content) in defaults {
        let p = dir.join(file);
        if !p.exists() {
            fs::write(p, content)?;
        }
    }
    Ok(())
}

pub fn list_skills(workspace: &Path) -> Vec<Skill> {
    let dir = skills_dir(workspace);
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&p) {
            out.push(parse_skill(&p, &raw));
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn load_skill(workspace: &Path, name: &str) -> Option<Skill> {
    list_skills(workspace)
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
}

/// Grava a skill arquivando a versão anterior. Devolve o número novo.
pub fn save_skill(workspace: &Path, name: &str, markdown: &str) -> Result<u32> {
    let dir = skills_dir(workspace);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.md"));

    let previous = fs::read_to_string(&path).ok();
    let next_version = match &previous {
        Some(old) => {
            let old_skill = parse_skill(&path, old);
            let vdir = versions_dir(workspace, name);
            fs::create_dir_all(&vdir)?;
            fs::write(vdir.join(format!("v{}.md", old_skill.version)), old)
                .with_context(|| format!("archive {name} v{}", old_skill.version))?;
            old_skill.version + 1
        }
        None => 1,
    };

    // A versão gravada manda: o `version` do texto recebido é ignorado.
    let mut skill = parse_skill(&path, markdown);
    skill.name = name.to_string();
    skill.version = next_version;
    fs::write(&path, skill.to_markdown())?;
    Ok(next_version)
}

/// Versões arquivadas, da mais antiga para a mais nova.
pub fn skill_versions(workspace: &Path, name: &str) -> Vec<u32> {
    let mut out: Vec<u32> = fs::read_dir(versions_dir(workspace, name))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.path()
                .file_stem()?
                .to_string_lossy()
                .strip_prefix('v')?
                .parse()
                .ok()
        })
        .collect();
    out.sort_unstable();
    out
}

/// Restaura uma versão arquivada — que vira a versão mais nova, sem apagar nada.
pub fn restore_skill(workspace: &Path, name: &str, version: u32) -> Result<u32> {
    let file = versions_dir(workspace, name).join(format!("v{version}.md"));
    let raw = fs::read_to_string(&file)
        .with_context(|| format!("{name} has no archived v{version}"))?;
    save_skill(workspace, name, &raw)
}

/// Casa a pergunta com `triggers`, e a fronteira `not_when` veta.
pub fn match_skills(workspace: &Path, query: &str, limit: usize) -> Vec<Skill> {
    let q = query.to_ascii_lowercase();
    let mut scored: Vec<(i32, Skill)> = list_skills(workspace)
        .into_iter()
        .filter_map(|s| {
            // fronteira antes de pontuar: uma skill de front-end não deve
            // aparecer numa pergunta de banco só porque a palavra "app" bateu
            if s.not_when.iter().any(|n| q.contains(n)) {
                return None;
            }
            let mut score = 0i32;
            if q.contains(&s.name.to_ascii_lowercase()) {
                score += 5;
            }
            for t in &s.triggers {
                if q.contains(t) {
                    score += 3;
                }
            }
            for w in s.name.split(|c: char| !c.is_alphanumeric()) {
                if w.len() > 2 && q.contains(w) {
                    score += 1;
                }
            }
            if score > 0 { Some((score, s)) } else { None }
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    scored.into_iter().take(limit).map(|(_, s)| s).collect()
}

pub fn format_skills(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return "(no skills — put .md under .harness/skills/)".into();
    }
    skills
        .iter()
        .map(|s| {
            let mut line = format!("- {} v{} — {}", s.name, s.version, s.description);
            if !s.triggers.is_empty() {
                line.push_str(&format!(" [on: {}]", s.triggers.join(", ")));
            }
            if !s.not_when.is_empty() {
                line.push_str(&format!(" [not: {}]", s.not_when.join(", ")));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Corpo pronto para injetar, com a regra de validação junto.
pub fn format_for_prompt(s: &Skill) -> String {
    let mut out = format!("# skill {} (v{})\n{}", s.name, s.version, s.body);
    if !s.validate.is_empty() {
        out.push_str(&format!("\n\nDone when: {}", s.validate));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("h-skills-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn frontmatter_nao_vaza_para_o_corpo() {
        let raw = "---\nname: x\nversion: 3\ndescription: faz coisa\ntriggers: a, b\n---\n# Corpo\ndescription: isto é texto, não campo\n";
        let s = parse_skill(Path::new("/tmp/x.md"), raw);
        assert_eq!(s.version, 3);
        assert_eq!(s.description, "faz coisa");
        assert_eq!(s.triggers, vec!["a", "b"]);
        assert!(s.body.starts_with("# Corpo"));
        // o bug antigo: pegava o `description:` de dentro do corpo
        assert_ne!(s.description, "isto é texto, não campo");
    }

    #[test]
    fn salvar_versiona_e_restaura() {
        let ws = tmp();
        let v1 = save_skill(&ws, "t", "---\ndescription: um\n---\nprimeiro\n").unwrap();
        assert_eq!(v1, 1);
        assert!(skill_versions(&ws, "t").is_empty(), "v1 ainda não arquivou nada");

        let v2 = save_skill(&ws, "t", "---\ndescription: dois\n---\nsegundo\n").unwrap();
        assert_eq!(v2, 2);
        assert_eq!(skill_versions(&ws, "t"), vec![1]);
        assert!(load_skill(&ws, "t").unwrap().body.contains("segundo"));

        // voltar para a v1 cria a v3 — nada é sobrescrito
        let v3 = restore_skill(&ws, "t", 1).unwrap();
        assert_eq!(v3, 3);
        let cur = load_skill(&ws, "t").unwrap();
        assert!(cur.body.contains("primeiro"));
        assert_eq!(cur.version, 3);
        assert_eq!(skill_versions(&ws, "t"), vec![1, 2]);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn not_when_veta_a_skill() {
        let ws = tmp();
        save_skill(
            &ws,
            "front",
            "---\ndescription: web\ntriggers: app, html\nnot_when: database, sql\n---\ncorpo\n",
        )
        .unwrap();
        assert_eq!(match_skills(&ws, "criar um app html", 5).len(), 1);
        // mesma palavra-gatilho, mas a fronteira veta
        assert!(match_skills(&ws, "criar um app com database sql", 5).is_empty());
        let _ = fs::remove_dir_all(&ws);
    }
}
