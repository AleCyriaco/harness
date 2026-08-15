//! Checkpoint: cópia do arquivo **antes** da primeira alteração do turno.
//!
//! Diferença para snapshot de workspace: aqui só entra arquivo que o agente vai
//! mesmo tocar, no momento em que ele vai tocar. Um turno que edita 3 arquivos
//! custa 3 cópias, não uma cópia do projeto — então dá para ligar por padrão
//! mesmo em repositório grande.
//!
//! Arquivo que **não existia** também é registrado (`existed: false`): desfazer
//! precisa apagá-lo, senão sobra lixo que o usuário não pediu.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DIR: &str = ".harness_checkpoints";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Caminho relativo à raiz do agente.
    pub path: String,
    /// Existia antes? Falso = desfazer significa apagar.
    pub existed: bool,
}

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub id: String,
    pub entries: Vec<Entry>,
}

fn ckpt_dir(root: &Path, id: &str) -> PathBuf {
    root.join(DIR).join(id)
}

fn manifest_path(root: &Path, id: &str) -> PathBuf {
    ckpt_dir(root, id).join("manifest.jsonl")
}

/// Guarda o estado atual de `rel` sob o checkpoint `id`. Repetir para o mesmo
/// arquivo no mesmo checkpoint é no-op — o que importa é o estado do começo.
pub fn snapshot_file(root: &Path, id: &str, rel: &str) -> Result<()> {
    let rel = rel.trim_start_matches("./");
    if rel.is_empty() || rel.starts_with(DIR) {
        return Ok(());
    }
    let already = read_manifest(root, id).iter().any(|e| e.path == rel);
    if already {
        return Ok(());
    }
    let src = root.join(rel);
    let existed = src.is_file();
    if existed {
        let dst = ckpt_dir(root, id).join("files").join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dst).with_context(|| format!("snapshot {rel}"))?;
    }
    let entry = Entry {
        path: rel.to_string(),
        existed,
    };
    let mpath = manifest_path(root, id);
    if let Some(parent) = mpath.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&mpath)?;
    writeln!(f, "{}", serde_json::to_string(&entry)?)?;
    Ok(())
}

pub fn read_manifest(root: &Path, id: &str) -> Vec<Entry> {
    let Ok(raw) = std::fs::read_to_string(manifest_path(root, id)) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect()
}

/// Checkpoints do mais novo para o mais antigo (o id é o carimbo de tempo).
pub fn list(root: &Path) -> Vec<Checkpoint> {
    let Ok(rd) = std::fs::read_dir(root.join(DIR)) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    ids.sort();
    ids.reverse();
    ids.into_iter()
        .map(|id| {
            let entries = read_manifest(root, &id);
            Checkpoint { id, entries }
        })
        .filter(|c| !c.entries.is_empty())
        .collect()
}

/// Devolve os arquivos ao estado do checkpoint. Retorna quantos mudaram.
pub fn rollback(root: &Path, id: &str) -> Result<usize> {
    let entries = read_manifest(root, id);
    if entries.is_empty() {
        anyhow::bail!("checkpoint {id} not found or empty");
    }
    let mut n = 0;
    for e in entries {
        let live = root.join(&e.path);
        if e.existed {
            let saved = ckpt_dir(root, id).join("files").join(&e.path);
            if saved.is_file() {
                if let Some(parent) = live.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&saved, &live)
                    .with_context(|| format!("restore {}", e.path))?;
                n += 1;
            }
        } else if live.exists() {
            // não existia antes do turno: desfazer é apagar
            std::fs::remove_file(&live).with_context(|| format!("remove {}", e.path))?;
            n += 1;
        }
    }
    Ok(n)
}

pub fn describe(list: &[Checkpoint]) -> String {
    if list.is_empty() {
        return "no checkpoints in this chat yet".into();
    }
    let mut out = String::from("checkpoints (newest first):\n");
    for c in list.iter().take(20) {
        let files: Vec<&str> = c.entries.iter().map(|e| e.path.as_str()).take(4).collect();
        out.push_str(&format!(
            "{} · {} file(s): {}{}\n",
            c.id,
            c.entries.len(),
            files.join(", "),
            if c.entries.len() > 4 { ", …" } else { "" }
        ));
    }
    out.push_str("\n/rollback <id> — or /rollback for the newest");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("harness_ckpt_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn desfaz_edicao_e_apaga_o_que_nasceu_no_turno() {
        let root = tmp();
        std::fs::write(root.join("a.txt"), "original").unwrap();

        snapshot_file(&root, "t1", "a.txt").unwrap();
        snapshot_file(&root, "t1", "novo.txt").unwrap(); // ainda não existe

        std::fs::write(root.join("a.txt"), "estragado pelo agente").unwrap();
        std::fs::write(root.join("novo.txt"), "criado pelo agente").unwrap();

        let n = rollback(&root, "t1").unwrap();
        assert_eq!(n, 2);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "original");
        assert!(!root.join("novo.txt").exists(), "o que nasceu no turno some");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn segundo_snapshot_do_mesmo_arquivo_nao_sobrescreve_o_estado_inicial() {
        let root = tmp();
        std::fs::write(root.join("a.txt"), "v1").unwrap();
        snapshot_file(&root, "t1", "a.txt").unwrap();
        std::fs::write(root.join("a.txt"), "v2").unwrap();
        // o agente mexe de novo no mesmo arquivo, no mesmo turno
        snapshot_file(&root, "t1", "a.txt").unwrap();
        std::fs::write(root.join("a.txt"), "v3").unwrap();

        rollback(&root, "t1").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "v1",
            "desfazer volta ao começo do turno, não ao passo anterior"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn subpasta_e_listagem() {
        let root = tmp();
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(root.join("web/index.html"), "<h1>ok</h1>").unwrap();
        snapshot_file(&root, "20260101_000001", "web/index.html").unwrap();
        std::fs::write(root.join("web/index.html"), "quebrado").unwrap();

        let l = list(&root);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].entries[0].path, "web/index.html");
        assert!(describe(&l).contains("web/index.html"));

        rollback(&root, "20260101_000001").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("web/index.html")).unwrap(),
            "<h1>ok</h1>"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn o_proprio_diretorio_de_checkpoint_nunca_entra() {
        let root = tmp();
        snapshot_file(&root, "t1", ".harness_checkpoints/x").unwrap();
        assert!(read_manifest(&root, "t1").is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
}
