use anyhow::{Context, Result};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::llm_pool::LlmEndpoint;
use crate::modes::AppMode;
use crate::update;

/// Standard subfolders under the user workspace.
pub const WS_SUBDIRS: &[&str] = &["code", "docs", "sheets", "pdfs", "web"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    /// Active LLM name in `llm_pool` (memory survives switches).
    #[serde(default)]
    pub active_llm: String,
    /// Multiple OpenAI-compatible endpoints (Grok, OpenAI, Ollama, …).
    #[serde(default)]
    pub llm_pool: Vec<LlmEndpoint>,
    /// On 429/quota/overload, try next enabled LLM automatically.
    #[serde(default = "default_true")]
    pub llm_auto_failover: bool,
    /// Persist active_llm to disk after successful failover.
    #[serde(default = "default_true")]
    pub llm_failover_persist: bool,
    /// Swarm workers may use a different endpoint (`use_for_workers`).
    #[serde(default = "default_true")]
    pub llm_multi_worker: bool,
    /// Rotate active LLM by weight on a timer (Code/Office pick separately).
    #[serde(default)]
    pub llm_rotate_enabled: bool,
    /// Minutes between weighted re-picks (60 = hourly, 120 = every 2h, …).
    #[serde(default = "default_rotate_minutes")]
    pub llm_rotate_minutes: u32,
    /// Last rotation slot id (unix_minutes / rotate_minutes).
    #[serde(default)]
    pub llm_rotate_slot: String,
    /// Last rotation wall time (RFC3339), for UI.
    #[serde(default)]
    pub llm_rotate_last: String,
    /// Root folder where all generated code, docs, sheets, PDFs, web apps go.
    pub workspace: PathBuf,
    /// User confirmed the default generation folder (setup wizard).
    #[serde(default)]
    pub workspace_ready: bool,
    #[serde(default)]
    pub mode: AppMode,
    /// Aparência: Paper (claro) ou Ember (escuro). Alterna com ⇧⌘D.
    #[serde(default)]
    pub theme: crate::theme::ThemeMode,
    /// Web: converter página em markdown (títulos/links/código) em vez de
    /// texto corrido. Desligue para voltar ao comportamento antigo.
    #[serde(default = "default_true")]
    pub web_markdown: bool,
    /// Teto de páginas por `web_crawl`.
    #[serde(default = "default_crawl_pages")]
    pub web_crawl_max_pages: u32,
    /// Profundidade máxima a partir da URL inicial.
    #[serde(default = "default_crawl_depth")]
    pub web_crawl_max_depth: u32,
    /// Só seguir links do mesmo domínio.
    #[serde(default = "default_true")]
    pub web_crawl_same_domain: bool,
    /// Obedecer robots.txt do site.
    #[serde(default = "default_true")]
    pub web_respect_robots: bool,
    /// Interromper a mesma tool chamada com os mesmos argumentos N vezes.
    #[serde(default = "default_true")]
    pub stuck_detect: bool,
    /// N do detector acima.
    #[serde(default = "default_stuck_threshold")]
    pub stuck_threshold: u32,
    /// Loop de aprendizado: no fim de um turno de trabalho, cria rascunho de skill.
    #[serde(default = "default_true")]
    pub learning_loop: bool,
    /// Mínimo de tools de trabalho para um turno virar skill.
    #[serde(default = "default_learn_min")]
    pub learn_min_steps: u32,
    /// Modelo do usuário: injeta o perfil no system prompt e deixa o agente atualizá-lo.
    #[serde(default = "default_true")]
    pub user_model: bool,
    /// Mostra o destino "Live" no rail (grafo do turno). Opcional.
    #[serde(default = "default_true")]
    pub live_view: bool,
    /// Checkpoint: copia o arquivo antes da primeira alteração de cada turno.
    #[serde(default = "default_true")]
    pub checkpoint: bool,
    /// Lê AGENTS.md/CLAUDE.md do projeto apontado para o system prompt.
    #[serde(default = "default_true")]
    pub project_instructions: bool,
    /// Avisa quando um turno passa deste tempo. 0 = desligado.
    #[serde(default = "default_notify_after")]
    pub notify_after_secs: u64,
    /// Compaction: o trecho cortado do histórico vira resumo em vez de sumir.
    #[serde(default = "default_true")]
    pub compaction: bool,
    /// Spill: o texto integral do trecho cortado vai para
    /// `{chat}/.harness_spill.jsonl`.
    #[serde(default = "default_true")]
    pub spill: bool,
    /// Guard: regras que barram comando destrutivo antes da aprovação.
    #[serde(default = "default_true")]
    pub guard: bool,
    /// Modo somente-leitura: nenhuma tool que escreve ou executa roda.
    #[serde(default)]
    pub guard_read_only: bool,
    /// Substitui a lista padrão de padrões barrados. Vazia = usa a padrão.
    #[serde(default)]
    pub guard_deny: Vec<String>,
    /// Objetivo do chat injetado no system prompt entre turnos.
    #[serde(default = "default_true")]
    pub goal_track: bool,
    /// Objetivo deste turno (vem do chat, não do disco).
    #[serde(default, skip)]
    pub goal: String,
    /// Esforço de raciocínio pedido ao modelo: "low" | "medium" | "high".
    /// Padrão para chats novos; cada chat pode sobrescrever.
    #[serde(default = "default_effort")]
    pub reasoning_effort: String,
    /// Gauntlet Loop ligado neste turno. Vive no chat, não no config.toml —
    /// o daemon copia do `LiveSession` para o `cfg` do turno.
    #[serde(skip)]
    pub gauntlet: bool,
    /// Teto de auto-continues do Gauntlet Loop por objetivo.
    #[serde(default = "default_gauntlet_max")]
    pub gauntlet_max_iterations: u32,
    /// Painel de uso fixo (abre junto com o app).
    #[serde(default)]
    pub usage_pinned: bool,
    /// Token Less Cost: padrão para chats novos (cada chat pode sobrescrever).
    /// `alias` mantém legível o config.toml gravado quando isto se chamava caveman.
    #[serde(default, alias = "caveman")]
    pub token_less: crate::tokenless::TokenLessLevel,
    #[serde(default = "default_history_cap")]
    pub history_cap: usize,
    #[serde(default = "default_tool_result_cap")]
    pub tool_result_cap: usize,
    #[serde(default = "default_true")]
    pub auto_approve_safe: bool,
    #[serde(default)]
    pub auto_approve_shell: bool,
    #[serde(default = "default_true")]
    pub stream: bool,
    #[serde(default = "default_update_repo")]
    pub update_repo: String,
    #[serde(default = "default_true")]
    pub check_updates_on_start: bool,
    #[serde(default = "default_swarm_max")]
    pub swarm_max_workers: usize,
    #[serde(default = "default_web_port")]
    pub web_server_port: u16,
    #[serde(default = "default_true")]
    pub memory_auto_recall: bool,
    /// Max concurrent live sessions in the multi-client daemon.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
}

fn default_history_cap() -> usize {
    28
}
fn default_tool_result_cap() -> usize {
    12_000
}
fn default_true() -> bool {
    true
}
fn default_update_repo() -> String {
    update::default_repo()
}
fn default_swarm_max() -> usize {
    3
}
fn default_web_port() -> u16 {
    8765
}
fn default_max_sessions() -> usize {
    32
}
fn default_learn_min() -> u32 {
    4
}
fn default_notify_after() -> u64 {
    60
}
fn default_effort() -> String {
    "medium".into()
}
fn default_crawl_pages() -> u32 {
    20
}
fn default_crawl_depth() -> u32 {
    2
}
fn default_stuck_threshold() -> u32 {
    3
}
fn default_gauntlet_max() -> u32 {
    crate::gauntlet::DEFAULT_MAX_ITERATIONS
}
fn default_rotate_minutes() -> u32 {
    60
}

impl Default for Config {
    fn default() -> Self {
        let xai = env_first(&["XAI_API_KEY", "GROK_API_KEY"]);
        let openai = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        let (api_base, api_key, model): (String, String, String) = if !xai.is_empty() {
            (
                "https://api.x.ai/v1".to_string(),
                xai,
                std::env::var("HARNESS_MODEL").unwrap_or_else(|_| "grok-4.5".into()),
            )
        } else {
            (
                "https://api.openai.com/v1".to_string(),
                openai,
                std::env::var("HARNESS_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".into()),
            )
        };
        let mut cfg = Self {
            api_base: api_base.clone(),
            api_key: api_key.clone(),
            model: model.clone(),
            active_llm: String::new(),
            llm_pool: Vec::new(),
            llm_auto_failover: true,
            llm_failover_persist: true,
            llm_multi_worker: true,
            llm_rotate_enabled: false,
            llm_rotate_minutes: default_rotate_minutes(),
            llm_rotate_slot: String::new(),
            llm_rotate_last: String::new(),
            workspace: suggested_workspace(),
            workspace_ready: false,
            mode: AppMode::Code,
            theme: crate::theme::ThemeMode::default(),
            usage_pinned: false,
            learning_loop: true,
            learn_min_steps: default_learn_min(),
            user_model: true,
            live_view: true,
            checkpoint: true,
            project_instructions: true,
            notify_after_secs: default_notify_after(),
            compaction: true,
            spill: true,
            guard: true,
            guard_read_only: false,
            guard_deny: Vec::new(),
            goal_track: true,
            goal: String::new(),
            reasoning_effort: default_effort(),
            web_markdown: true,
            web_crawl_max_pages: default_crawl_pages(),
            web_crawl_max_depth: default_crawl_depth(),
            web_crawl_same_domain: true,
            web_respect_robots: true,
            stuck_detect: true,
            stuck_threshold: default_stuck_threshold(),
            gauntlet: false,
            gauntlet_max_iterations: default_gauntlet_max(),
            token_less: crate::tokenless::TokenLessLevel::default(),
            history_cap: default_history_cap(),
            tool_result_cap: default_tool_result_cap(),
            auto_approve_safe: true,
            auto_approve_shell: false,
            stream: true,
            update_repo: default_update_repo(),
            check_updates_on_start: true,
            swarm_max_workers: default_swarm_max(),
            web_server_port: default_web_port(),
            memory_auto_recall: true,
            max_sessions: default_max_sessions(),
        };
        crate::llm_pool::ensure_pool(&mut cfg);
        cfg
    }
}

fn env_first(keys: &[&str]) -> String {
    for k in keys {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    String::new()
}

impl Config {
    pub fn config_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("sh", "harness", "harness")
            .context("could not resolve config directory")?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    pub fn load() -> Self {
        let path = match Self::config_path() {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        let mut cfg = Self::default();
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(file) = toml::from_str::<Config>(&raw) {
                cfg = file;
            }
        }
        let placeholder = cfg.api_key.is_empty() || cfg.api_key == "sk-...";
        if placeholder {
            let xai = env_first(&["XAI_API_KEY", "GROK_API_KEY"]);
            if !xai.is_empty() {
                cfg.api_key = xai;
                cfg.api_base = "https://api.x.ai/v1".into();
                if cfg.model.starts_with("gpt-") || cfg.model.is_empty() {
                    cfg.model =
                        std::env::var("HARNESS_MODEL").unwrap_or_else(|_| "grok-4.5".into());
                }
            } else {
                let openai = env_first(&["OPENAI_API_KEY"]);
                if !openai.is_empty() {
                    cfg.api_key = openai;
                }
            }
        }
        if cfg.history_cap == 0 {
            cfg.history_cap = default_history_cap();
        }
        if cfg.tool_result_cap == 0 {
            cfg.tool_result_cap = default_tool_result_cap();
        }
        if cfg.swarm_max_workers == 0 {
            cfg.swarm_max_workers = default_swarm_max();
        }
        cfg.swarm_max_workers = cfg.swarm_max_workers.clamp(1, 3);
        if cfg.web_server_port == 0 {
            cfg.web_server_port = default_web_port();
        }
        if cfg.max_sessions == 0 {
            cfg.max_sessions = default_max_sessions();
        }
        cfg.max_sessions = cfg.max_sessions.clamp(1, 256);
        if cfg.gauntlet_max_iterations == 0 {
            cfg.gauntlet_max_iterations = default_gauntlet_max();
        }
        cfg.gauntlet_max_iterations = cfg.gauntlet_max_iterations.clamp(1, 100);
        cfg.web_crawl_max_pages = cfg.web_crawl_max_pages.clamp(1, 200);
        cfg.web_crawl_max_depth = cfg.web_crawl_max_depth.clamp(0, 5);
        cfg.stuck_threshold = cfg.stuck_threshold.clamp(2, 20);
        cfg.learn_min_steps = cfg.learn_min_steps.clamp(1, 30);
        if !matches!(cfg.reasoning_effort.as_str(), "low" | "medium" | "high") {
            cfg.reasoning_effort = default_effort();
        }
        if cfg.llm_rotate_minutes == 0 {
            cfg.llm_rotate_minutes = default_rotate_minutes();
        }
        cfg.llm_rotate_minutes = cfg.llm_rotate_minutes.clamp(1, 60 * 24 * 7);
        if cfg.update_repo.is_empty() {
            cfg.update_repo = default_update_repo();
        }
        if cfg.workspace.as_os_str().is_empty() {
            cfg.workspace = suggested_workspace();
            cfg.workspace_ready = false;
        }
        let pool_was_empty = cfg.llm_pool.is_empty();
        crate::llm_pool::ensure_pool(&mut cfg);
        // Persist first-time pool seed so multi-LLM is on disk next launch.
        if pool_was_empty && !cfg.llm_pool.is_empty() {
            let _ = cfg.save();
        }
        // Do not auto-create until user confirms (unless already ready).
        if cfg.workspace_ready {
            let _ = ensure_workspace_layout(&cfg.workspace);
        }
        cfg
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, toml::to_string_pretty(self)?)?;
        if self.workspace_ready {
            let _ = ensure_workspace_layout(&self.workspace);
        }
        Ok(())
    }

    pub fn needs_setup(&self) -> bool {
        self.api_key.trim().is_empty() || !self.workspace_ready || !workspace_path_ok(&self.workspace)
    }

    pub fn needs_workspace(&self) -> bool {
        !self.workspace_ready || !workspace_path_ok(&self.workspace)
    }

    pub fn needs_api(&self) -> bool {
        if !self.api_key.trim().is_empty() {
            return false;
        }
        !self
            .llm_pool
            .iter()
            .any(|e| e.enabled && !e.api_key.trim().is_empty())
    }
}

/// O que `harness reset` apaga. **Nunca** inclui o workspace: os arquivos
/// gerados são do usuário, não estado do app.
/// `keys = true` também joga fora o config.toml (chaves de API).
pub fn reset_targets(keys: bool) -> Vec<PathBuf> {
    let Some(dirs) = ProjectDirs::from("sh", "harness", "harness") else {
        return Vec::new();
    };
    let data = dirs.data_dir();
    let mut v = vec![
        data.join("sessions"),
        data.join("memory.sqlite3"),
        data.join("memory_graph.sqlite3"),
        data.join("graph"),
    ];
    if keys {
        v.push(dirs.config_dir().join("config.toml"));
    }
    v
}

/// Suggested default: ~/Documents/Harness (visible, easy to find).
pub fn suggested_workspace() -> PathBuf {
    if let Some(user) = UserDirs::new() {
        if let Some(docs) = user.document_dir() {
            return docs.join("Harness");
        }
        if let Some(home) = user.home_dir().to_str() {
            return PathBuf::from(home).join("Documents").join("Harness");
        }
    }
    if let Some(dirs) = ProjectDirs::from("sh", "harness", "harness") {
        return dirs.data_dir().join("workspace");
    }
    PathBuf::from("Harness")
}

pub fn workspace_path_ok(path: &Path) -> bool {
    let s = path.to_string_lossy();
    !s.trim().is_empty() && path.is_absolute()
}

/// Create workspace root + code/docs/sheets/pdfs/web.
pub fn ensure_workspace_layout(root: &Path) -> Result<()> {
    fs::create_dir_all(root).with_context(|| format!("create {}", root.display()))?;
    for sub in WS_SUBDIRS {
        fs::create_dir_all(root.join(sub))?;
    }
    // Small README so the folder is self-explanatory
    let readme = root.join("README.txt");
    if !readme.exists() {
        let _ = fs::write(
            readme,
            "Harness default workspace\n\
             \n\
             code/    — source files generated by Code mode\n\
             docs/    — Word (.docx) documents\n\
             sheets/  — Excel (.xlsx) spreadsheets\n\
             pdfs/    — PDF files\n\
             web/     — static web apps (use Server panel)\n\
             \n\
             Everything the agent creates goes under this folder (unless you ask otherwise).\n",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn reset_nunca_mira_no_workspace_e_so_toca_as_chaves_se_pedir() {
        let sem = reset_targets(false);
        let com = reset_targets(true);
        assert!(sem.iter().any(|p| p.ends_with("sessions")));
        assert!(sem.iter().any(|p| p.ends_with("memory.sqlite3")));
        assert!(!sem.iter().any(|p| p.ends_with("config.toml")), "chave só com --keys");
        assert!(com.iter().any(|p| p.ends_with("config.toml")));
        let ws = suggested_workspace();
        assert!(
            !com.iter().any(|p| *p == ws || ws.starts_with(p)),
            "o workspace do usuário nunca entra"
        );
    }

    use super::*;

    #[test]
    fn esforco_nasce_medio_e_recusa_valor_invalido() {
        assert_eq!(Config::default().reasoning_effort, "medium");
        let mut cfg = Config::default();
        cfg.reasoning_effort = "turbo".into();
        // mesma validação de `load`
        if !matches!(cfg.reasoning_effort.as_str(), "low" | "medium" | "high") {
            cfg.reasoning_effort = default_effort();
        }
        assert_eq!(cfg.reasoning_effort, "medium");
    }
}
