use anyhow::{Context, Result};
use chrono::{Local, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

use crate::config;
use crate::llm::ChatMessage;
use crate::modes::AppMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub mode: AppMode,
    pub updated_at: String,
    /// User-chosen root (Documents/Harness, etc.).
    pub workspace: String,
    /// Per-chat folder: `{workspace}/{YYYYMMDD_HHMMSS}/` — all generated files go here.
    #[serde(default)]
    pub chat_dir: String,
    /// Folder name only (timestamp label).
    #[serde(default)]
    pub chat_folder_name: String,
    /// Session id on the multi-client daemon (same as `id` when created via daemon).
    #[serde(default)]
    pub daemon_session_id: String,
    /// Token Less Cost deste chat. `None` = segue o padrão do Config.
    /// `alias`: sessões gravadas quando isto se chamava caveman seguem abrindo.
    #[serde(default, alias = "caveman")]
    pub token_less: Option<crate::tokenless::TokenLessLevel>,
    /// Favorito: vai para o topo da lista e não some no meio dos antigos.
    #[serde(default)]
    pub pinned: bool,
    /// Projeto que este chat edita. `None` = o agente fica na pasta do chat.
    #[serde(default)]
    pub project_dir: Option<String>,
    /// Gauntlet Loop ligado neste chat.
    #[serde(default)]
    pub gauntlet: bool,
    /// Título posto à mão — o auto-título da primeira mensagem não o sobrescreve.
    #[serde(default)]
    pub title_locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<ChatMessage>,
    pub ui_log: Vec<UiLogLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiLogLine {
    pub role: String,
    pub text: String,
}

impl Session {
    /// Create a new chat bound to a fresh timestamp folder under `workspace_root`.
    pub fn new(mode: AppMode, workspace_root: &Path) -> Self {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let (folder_name, chat_dir) = create_chat_folder(workspace_root, mode);
        let chat_dir_s = chat_dir.display().to_string();

        Self {
            meta: SessionMeta {
                id: id.clone(),
                title: "New session".into(),
                mode,
                updated_at: now,
                workspace: workspace_root.display().to_string(),
                chat_dir: chat_dir_s.clone(),
                chat_folder_name: folder_name.clone(),
                daemon_session_id: id.clone(),
                token_less: None,
                pinned: false,
                project_dir: None,
                gauntlet: false,
                title_locked: false,
            },
            messages: Vec::new(),
            ui_log: vec![UiLogLine {
                role: "system".into(),
                text: format!(
                    "{}\nChat folder: {}\n({})\nOpen this folder anytime from the session panel.",
                    crate::app_text::welcome(mode),
                    folder_name,
                    chat_dir_s
                ),
            }],
        }
    }

    /// Bind GUI session to a daemon-created session (same id + chat_dir).
    pub fn from_daemon(
        session_id: String,
        chat_dir: String,
        title: String,
        mode: AppMode,
        workspace_root: &Path,
    ) -> Self {
        let folder_name = std::path::Path::new(&chat_dir)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| session_id[..8.min(session_id.len())].to_string());
        let _ = config::ensure_workspace_layout(std::path::Path::new(&chat_dir));
        Self {
            meta: SessionMeta {
                id: session_id.clone(),
                title,
                mode,
                updated_at: Utc::now().to_rfc3339(),
                workspace: workspace_root.display().to_string(),
                chat_dir: chat_dir.clone(),
                chat_folder_name: folder_name.clone(),
                daemon_session_id: session_id,
                token_less: None,
                pinned: false,
                project_dir: None,
                gauntlet: false,
                title_locked: false,
            },
            messages: Vec::new(),
            ui_log: vec![UiLogLine {
                role: "system".into(),
                text: format!(
                    "{}\nDaemon session · folder {} · {}\nGUI is attached to the multi-client daemon.",
                    crate::app_text::welcome(mode),
                    folder_name,
                    chat_dir
                ),
            }],
        }
    }

    pub fn chat_path(&self) -> PathBuf {
        if !self.meta.chat_dir.is_empty() {
            PathBuf::from(&self.meta.chat_dir)
        } else {
            PathBuf::from(&self.meta.workspace)
        }
    }

    pub fn ensure_chat_dir(&mut self, workspace_root: &Path) {
        if self.meta.chat_dir.is_empty() || !Path::new(&self.meta.chat_dir).is_dir() {
            let (folder_name, chat_dir) = create_chat_folder(workspace_root, self.meta.mode);
            self.meta.chat_dir = chat_dir.display().to_string();
            self.meta.chat_folder_name = folder_name;
            self.meta.workspace = workspace_root.display().to_string();
        } else {
            let _ = config::ensure_workspace_layout(Path::new(&self.meta.chat_dir));
        }
    }

    pub fn touch_title_from_user(&mut self, user_text: &str) {
        if !self.meta.title_locked
            && (self.meta.title == "New session" || self.meta.title.starts_with("Session "))
        {
            let t = summarize_words(user_text, 5);
            self.meta.title = if t.is_empty() {
                format!(
                    "{} · {}",
                    self.meta.chat_folder_name,
                    &self.meta.id[..8.min(self.meta.id.len())]
                )
            } else {
                t
            };
        }
        self.meta.updated_at = Utc::now().to_rfc3339();
    }
}

/// `{root}/{YYYYMMDD_HHMMSS}/` with code/docs/sheets/pdfs/web.
pub fn create_chat_folder(workspace_root: &Path, mode: AppMode) -> (String, PathBuf) {
    let _ = fs::create_dir_all(workspace_root);
    let base = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let mut name = base.clone();
    let mut path = workspace_root.join(&name);
    let mut n = 1u32;
    while path.exists() {
        name = format!("{base}_{n}");
        path = workspace_root.join(&name);
        n += 1;
        if n > 99 {
            name = format!("{base}_{}", &Uuid::new_v4().to_string()[..8]);
            path = workspace_root.join(&name);
            break;
        }
    }
    let _ = config::ensure_workspace_layout(&path);
    // Marker linking folder ↔ chat
    let mode_s = mode.label();
    let _ = fs::write(
        path.join(".harness_chat.txt"),
        format!(
            "harness chat folder\nmode={mode_s}\ncreated={}\n",
            Local::now().to_rfc3339()
        ),
    );
    (name, path)
}

pub fn sessions_dir() -> Result<PathBuf> {
    let dirs =
        ProjectDirs::from("sh", "harness", "harness").context("sessions dir")?;
    let p = dirs.data_dir().join("sessions");
    fs::create_dir_all(&p)?;
    Ok(p)
}

/// Título vazio/padrão? A lista mostraria só o carimbo de data. Derivamos um
/// resumo do log — o arquivo já foi lido aqui, então não custa I/O extra.
pub fn is_placeholder_title(t: &str) -> bool {
    let t = t.trim();
    t.is_empty()
        || t == "New session"
        || t.starts_with("Session ")
        // carimbo de pasta: 20260804_153021
        || (t.len() >= 15
            && t.as_bytes()[8] == b'_'
            && t.chars().take(8).all(|c| c.is_ascii_digit()))
}

/// Primeira linha sem marcadores, limitada a `max_words` palavras —
/// o título curto que aparece na lista de chats.
pub fn summarize_words(text: &str, max_words: usize) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let clean = first
        .trim_start_matches(['#', '*', '-', '>', '`', ' '])
        .trim();
    let words: Vec<&str> = clean.split_whitespace().collect();
    if words.len() <= max_words {
        return clean.to_string();
    }
    format!("{}…", words[..max_words].join(" "))
}

/// Título para exibir: o do usuário quando existe, senão um resumo do log
/// (primeira linha, máx. 5 palavras). Usado na lista salva e também no
/// daemon (sessões vivas passam por aqui).
pub fn display_title(s: &Session) -> String {
    if !is_placeholder_title(&s.meta.title) {
        return s.meta.title.clone();
    }
    derive_title(s).unwrap_or_else(|| {
        if s.meta.chat_folder_name.is_empty() {
            s.meta.id.chars().take(8).collect()
        } else {
            s.meta.chat_folder_name.clone()
        }
    })
}

fn derive_title(s: &Session) -> Option<String> {
    let pick = s
        .ui_log
        .iter()
        .find(|l| l.role == "user")
        .or_else(|| s.ui_log.iter().find(|l| l.role == "assistant"))?;
    let t = summarize_words(&pick.text, 5);
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Lista em cache. `list_sessions` desserializa a sessão **inteira** (histórico
/// do LLM + ui_log) só para extrair o meta, e é chamada no fim de cada turno —
/// sem cache isso relê todos os chats a cada resposta.
static LIST_CACHE: Mutex<Option<(u64, Vec<SessionMeta>)>> = Mutex::new(None);

/// Impressão digital barata do diretório: quantidade + mtime mais recente.
/// Um `save_session` muda o mtime do arquivo, então o cache cai sozinho.
fn dir_fingerprint(dir: &Path) -> u64 {
    let mut count: u64 = 0;
    let mut newest: u128 = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            count += 1;
            if let Some(t) = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            {
                newest = newest.max(t.as_nanos());
            }
        }
    }
    count.wrapping_mul(0x9e37_79b9).wrapping_add(newest as u64)
}

pub fn list_sessions() -> Result<Vec<SessionMeta>> {
    let dir = sessions_dir()?;
    let fp = dir_fingerprint(&dir);
    if let Ok(g) = LIST_CACHE.lock() {
        if let Some((cached_fp, list)) = g.as_ref() {
            if *cached_fp == fp {
                return Ok(list.clone());
            }
        }
    }
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(s) = serde_json::from_str::<Session>(&raw) {
                let mut meta = s.meta.clone();
                meta.title = display_title(&s);
                out.push(meta);
            }
        }
    }
    // fixados primeiro; dentro de cada grupo, mais recente antes
    out.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(b.updated_at.cmp(&a.updated_at))
    });
    out.truncate(80);
    if let Ok(mut g) = LIST_CACHE.lock() {
        *g = Some((fp, out.clone()));
    }
    Ok(out)
}

/// Chat sem pergunta/resposta ainda não gera arquivo — não há o que gravar.
pub fn has_content(session: &Session) -> bool {
    !session.messages.is_empty()
        || session
            .ui_log
            .iter()
            .any(|l| l.role == "user" || l.role == "assistant")
}

pub fn save_session(session: &Session) -> Result<()> {
    if !has_content(session) {
        // chat recém-criado / vazio: sem log no disco até haver conteúdo
        return Ok(());
    }
    let dir = sessions_dir()?;
    let path = dir.join(format!("{}.json", session.meta.id));
    let raw = serde_json::to_string_pretty(session)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, raw)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn load_session(id: &str) -> Result<Session> {
    let path = sessions_dir()?.join(format!("{id}.json"));
    let raw = fs::read_to_string(&path).with_context(|| format!("load {id}"))?;
    Ok(serde_json::from_str(&raw)?)
}

/// Onde o agente deste chat trabalha: o projeto apontado, ou a pasta do chat.
/// Caminho vazio ou relativo é ignorado — root do agente tem que ser absoluto.
pub fn effective_root(project_dir: Option<&str>, chat_dir: &str) -> PathBuf {
    match project_dir.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
        _ => PathBuf::from(chat_dir),
    }
}

/// Renomeia e/ou fixa uma sessão no disco. Renomear trava o auto-título.
pub fn update_meta(
    id: &str,
    title: Option<&str>,
    pinned: Option<bool>,
    project_dir: Option<Option<String>>,
) -> Result<SessionMeta> {
    let mut s = load_session(id)?;
    if let Some(p) = project_dir {
        s.meta.project_dir = p.filter(|v| !v.trim().is_empty());
    }
    if let Some(t) = title {
        let t = t.trim();
        if !t.is_empty() {
            s.meta.title = t.chars().take(80).collect();
            s.meta.title_locked = true;
        }
    }
    if let Some(p) = pinned {
        s.meta.pinned = p;
    }
    save_session(&s)?;
    Ok(s.meta)
}

pub fn delete_session(id: &str) -> Result<()> {
    let path = sessions_dir()?.join(format!("{id}.json"));
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauntlet_sobrevive_ao_disco_e_a_chat_antigo() {
        let mut s = sess("t", &[("user", "oi")]);
        s.meta.gauntlet = true;
        // mesmo caminho de serialização de save_session/load_session
        let raw = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&raw).unwrap();
        assert!(back.meta.gauntlet, "toggle tem que voltar ligado do disco");

        // chat gravado antes deste campo existir não pode falhar ao carregar
        let old = raw.replace(r#""gauntlet":true,"#, "");
        assert!(!old.contains("gauntlet"));
        let legacy: Session = serde_json::from_str(&old).unwrap();
        assert!(!legacy.meta.gauntlet);
    }

    fn sess(title: &str, log: &[(&str, &str)]) -> Session {
        Session {
            meta: SessionMeta {
                id: "abcd1234efgh".into(),
                title: title.into(),
                mode: AppMode::Code,
                updated_at: String::new(),
                workspace: String::new(),
                chat_dir: String::new(),
                chat_folder_name: "20260804_153021".into(),
                daemon_session_id: String::new(),
                token_less: None,
                pinned: false,
                project_dir: None,
                gauntlet: false,
                title_locked: false,
            },
            messages: Vec::new(),
            ui_log: log
                .iter()
                .map(|(r, t)| UiLogLine {
                    role: (*r).into(),
                    text: (*t).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn carimbo_de_data_conta_como_placeholder() {
        assert!(is_placeholder_title("20260804_153021"));
        assert!(is_placeholder_title("New session"));
        assert!(is_placeholder_title("  "));
        assert!(!is_placeholder_title("refatorar o tokenizer"));
    }

    #[test]
    fn resume_a_primeira_mensagem_do_usuario() {
        let s = sess(
            "New session",
            &[
                ("system", "boas-vindas"),
                ("user", "refatora o tokenizer pra aceitar unicode"),
                ("assistant", "vou olhar"),
            ],
        );
        // resumo limitado a 5 palavras
        assert_eq!(display_title(&s), "refatora o tokenizer pra aceitar…");
    }

    #[test]
    fn titulo_curto_nao_passa_de_cinco_palavras() {
        let s = sess(
            "New session",
            &[("user", "um dois três quatro cinco seis sete")],
        );
        assert_eq!(display_title(&s), "um dois três quatro cinco…");

        // menos de 5 palavras fica inteiro
        let curta = sess("New session", &[("user", "apenas três palavras")]);
        assert_eq!(display_title(&curta), "apenas três palavras");

        // marcadores de markdown saem antes do resumo
        let md = sess("New session", &[("user", "# conserta o build quebrado")]);
        assert_eq!(display_title(&md), "conserta o build quebrado");
    }

    #[test]
    fn chat_vazio_nao_conta_como_conteudo() {
        // só boas-vindas (system) → nada para gravar
        let vazio = sess("New session", &[("system", "welcome")]);
        assert!(!has_content(&vazio));

        let com_pergunta = sess("t", &[("system", "welcome"), ("user", "oi")]);
        assert!(has_content(&com_pergunta));

        let com_resposta = sess("t", &[("system", "welcome"), ("assistant", "olá")]);
        assert!(has_content(&com_resposta));

        // histórico do LLM sozinho também conta
        let mut so_historico = sess("t", &[("system", "welcome")]);
        so_historico.messages.push(ChatMessage {
            role: "user".into(),
            content: Some("oi".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
        assert!(has_content(&so_historico));
    }

    /// Chat vazio grava nada no disco. A parte que "com conteúdo grava" é
    /// coberta por `cache_da_lista_invalida_ao_gravar`; aqui só verificamos o
    /// skip (que retorna antes de qualquer I/O, então roda até sem permissão).
    #[test]
    fn chat_sem_conteudo_nao_gera_arquivo() {
        let id = format!("test-empty-{}", uuid::Uuid::new_v4());
        let mut s = sess("New session", &[("system", "welcome")]);
        s.meta.id = id.clone();
        // sem pergunta/resposta → save_session retorna sem tocar no disco
        assert!(save_session(&s).is_ok());
        if let Ok(dir) = sessions_dir() {
            // (em sistemas reais; na sandbox o create_dir_all pode falhar)
            let json = dir.join(format!("{id}.json"));
            assert!(!json.exists(), "chat vazio não devia gerar arquivo");
        }
        // agora ganha conteúdo → vira um save comum (gravado em sistemas reais)
        s.ui_log.push(UiLogLine {
            role: "user".into(),
            text: "primeira pergunta".into(),
        });
        assert!(has_content(&s));
        let _ = delete_session(&id);
    }

    #[test]
    fn sem_mensagem_cai_na_pasta_e_titulo_do_usuario_vence() {
        let vazio = sess("New session", &[("system", "boas-vindas")]);
        assert_eq!(display_title(&vazio), "20260804_153021");

        let nomeado = sess("meu chat", &[("user", "oi")]);
        assert_eq!(display_title(&nomeado), "meu chat");
    }

    /// A promessa do diálogo: apagar o chat tira a conversa, não os arquivos.
    #[test]
    fn apagar_chat_nao_toca_na_pasta_gerada() {
        let id = format!("test-del-{}", uuid::Uuid::new_v4());
        let dir = std::env::temp_dir().join(&id);
        fs::create_dir_all(&dir).unwrap();
        let gerado = dir.join("main.rs");
        fs::write(&gerado, "fn main() {}").unwrap();

        let mut s = sess("chat de teste", &[("user", "oi")]);
        s.meta.id = id.clone();
        s.meta.chat_dir = dir.display().to_string();
        save_session(&s).unwrap();
        let json = sessions_dir().unwrap().join(format!("{id}.json"));
        assert!(json.exists());

        delete_session(&id).unwrap();
        assert!(!json.exists(), "a conversa devia sumir");
        assert!(gerado.exists(), "o arquivo gerado devia ficar");
        let _ = fs::remove_dir_all(&dir);
    }

    /// O cache tem que cair quando uma sessão é gravada, senão a lista congela.
    #[test]
    fn cache_da_lista_invalida_ao_gravar() {
        // afirma a presença do *nosso* id, não a contagem da pasta: outros
        // testes gravam na mesma pasta em paralelo e a contagem oscila
        let id = format!("test-cache-{}", uuid::Uuid::new_v4());
        let tem = |id: &str| list_sessions().unwrap().iter().any(|m| m.id == id);
        assert!(!tem(&id));
        assert!(!tem(&id), "segunda chamada vem do cache e tem que bater");

        let mut s = sess("cache", &[("user", "oi")]);
        s.meta.id = id.clone();
        save_session(&s).unwrap();
        assert!(tem(&id), "gravar tem que invalidar o cache");

        delete_session(&id).unwrap();
        assert!(!tem(&id), "apagar também");
    }

    #[test]
    fn root_do_agente_segue_o_projeto_quando_absoluto() {
        assert_eq!(
            effective_root(Some("/tmp/proj"), "/chats/abc"),
            PathBuf::from("/tmp/proj")
        );
        // sem projeto, ou com valor inútil, fica na pasta do chat
        for bad in [None, Some(""), Some("   "), Some("relativo/nao/vale")] {
            assert_eq!(
                effective_root(bad, "/chats/abc"),
                PathBuf::from("/chats/abc"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn projeto_persiste_e_pode_ser_removido() {
        let id = format!("test-proj-{}", uuid::Uuid::new_v4());
        let mut s = sess("p", &[("user", "oi")]);
        s.meta.id = id.clone();
        save_session(&s).unwrap();

        let m = update_meta(&id, None, None, Some(Some("/tmp/x".into()))).unwrap();
        assert_eq!(m.project_dir.as_deref(), Some("/tmp/x"));
        assert_eq!(
            load_session(&id).unwrap().meta.project_dir.as_deref(),
            Some("/tmp/x")
        );
        // string vazia desaponta
        let m = update_meta(&id, None, None, Some(Some("  ".into()))).unwrap();
        assert!(m.project_dir.is_none());
        delete_session(&id).unwrap();
    }

    #[test]
    fn renomear_trava_o_auto_titulo() {
        let mut s = sess("New session", &[]);
        s.meta.title = "nome à mão".into();
        s.meta.title_locked = true;
        s.touch_title_from_user("primeira mensagem do usuário");
        assert_eq!(s.meta.title, "nome à mão");
    }
}
