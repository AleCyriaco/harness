//! Skills — markdown playbooks injected by name or keyword (jcode-inspired, light).

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub path: PathBuf,
    pub body: String,
    pub description: String,
}

pub fn skills_dir(workspace: &Path) -> PathBuf {
    workspace.join(".harness").join("skills")
}

pub fn ensure_default_skills(workspace: &Path) -> Result<()> {
    let dir = skills_dir(workspace);
    fs::create_dir_all(&dir)?;
    let def = dir.join("web-app.md");
    if !def.exists() {
        fs::write(
            def,
            r#"---
name: web-app
description: Build static web apps under web/ and preview in harness
---
# Web app skill
1. Put HTML/CSS/JS under `web/`
2. Prefer `web/index.html` as entry
3. Start server with path `web` and open in harness WebView
4. Keep assets relative so local server works
"#,
        )?;
    }
    let code = dir.join("rust-project.md");
    if !code.exists() {
        fs::write(
            code,
            r#"---
name: rust-project
description: Scaffold and verify small Rust projects
---
# Rust skill
1. Put sources under `code/`
2. Prefer `cargo check` / `cargo test` for verification
3. Use replace_in_file for small edits
4. Run get_diagnostics after changes
"#,
        )?;
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
        if let Ok(body) = fs::read_to_string(&p) {
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "skill".into());
            let description = body
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("description:"))
                .map(|l| l.splitn(2, ':').nth(1).unwrap_or("").trim().to_string())
                .unwrap_or_default();
            out.push(Skill {
                name,
                path: p,
                body,
                description,
            });
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

pub fn match_skills(workspace: &Path, query: &str, limit: usize) -> Vec<Skill> {
    let q = query.to_ascii_lowercase();
    let mut scored: Vec<(i32, Skill)> = list_skills(workspace)
        .into_iter()
        .filter_map(|s| {
            let mut score = 0i32;
            if q.contains(&s.name) {
                score += 5;
            }
            if !s.description.is_empty() && q.contains(&s.description.to_ascii_lowercase()) {
                score += 3;
            }
            for w in s.name.split(|c: char| !c.is_alphanumeric()) {
                if w.len() > 2 && q.contains(w) {
                    score += 1;
                }
            }
            if score > 0 {
                Some((score, s))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(limit).map(|(_, s)| s).collect()
}

pub fn format_skills(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return "(no skills — put .md under .harness/skills/)".into();
    }
    skills
        .iter()
        .map(|s| format!("- {} — {}", s.name, s.description))
        .collect::<Vec<_>>()
        .join("\n")
}
