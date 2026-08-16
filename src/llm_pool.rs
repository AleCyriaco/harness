//! Multi-LLM pool: several endpoints, concurrent roles, auto-failover on limits.
//! Memory (chat history + SQLite) is independent of which LLM answers.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmEndpoint {
    pub name: String,
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Order for failover (0 = try first among fallbacks after active).
    #[serde(default)]
    pub priority: u32,
    /// Relative traffic share when rotation is on (e.g. 70 + 30 = 70%/30%).
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// Eligible for Code mode rotation / use.
    #[serde(default = "default_true")]
    pub use_for_code: bool,
    /// Eligible for Office mode rotation / use.
    #[serde(default = "default_true")]
    pub use_for_office: bool,
    /// If true, swarm workers prefer this when set as worker_llm.
    #[serde(default)]
    pub use_for_workers: bool,
    /// USD por 1M tokens de entrada. 0 = sem preço → custo aparece como "—".
    #[serde(default)]
    pub price_in: f64,
    /// USD por 1M tokens de saída.
    #[serde(default)]
    pub price_out: f64,
    /// Token Less Cost deste provedor. `None` = usa o padrão do config.
    /// Vive aqui, não no chat: compressão é característica de quanto o
    /// provedor cobra, não da conversa.
    #[serde(default)]
    pub token_less: Option<crate::tokenless::TokenLessLevel>,
    /// Protocolo do endpoint: "chat" (padrão) ou "responses".
    /// Vazio = deduzido do host, para não quebrar config já gravado.
    #[serde(default)]
    pub wire: String,
}

/// Servidor na própria máquina ou na LAN: a chave é opcional, então um
/// endpoint local sem key precisa continuar valendo.
/// cyrix: cobre localhost/127./192.168./10./.local — 172.16-31 fica de fora até
/// alguém precisar.
pub fn is_local(api_base: &str) -> bool {
    let h = api_base.to_ascii_lowercase();
    ["localhost", "127.0.0.1", "://192.168.", "://10.", "0.0.0.0", ".local"]
        .iter()
        .any(|m| h.contains(m))
}

/// Protocolo que um endpoint fala. `api.meta.ai` usa a Responses API, que tem
/// corpo e eventos diferentes de Chat Completions.
pub fn wire_of(wire: &str, api_base: &str) -> Wire {
    match wire.trim().to_ascii_lowercase().as_str() {
        "responses" => Wire::Responses,
        "chat" => Wire::Chat,
        _ if api_base.contains("meta.ai") => Wire::Responses,
        _ => Wire::Chat,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    Chat,
    Responses,
}

/// Preço do endpoint ativo, ou (0,0) quando não configurado.
pub fn active_price(cfg: &crate::config::Config) -> (f64, f64) {
    cfg.llm_pool
        .iter()
        .find(|e| e.model == cfg.model && e.api_base == cfg.api_base)
        .map(|e| (e.price_in, e.price_out))
        .unwrap_or((0.0, 0.0))
}

fn default_true() -> bool {
    true
}
fn default_weight() -> u32 {
    1
}

impl LlmEndpoint {
    pub fn from_primary(cfg: &Config) -> Self {
        Self {
            name: "primary".into(),
            api_base: cfg.api_base.clone(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            enabled: true,
            priority: 0,
            weight: 1,
            use_for_code: true,
            use_for_office: true,
            use_for_workers: false,
            price_in: 0.0,
            price_out: 0.0,
            token_less: None,
            wire: String::new(),
        }
    }

    pub fn apply_to(&self, cfg: &mut Config) {
        cfg.api_base = self.api_base.clone();
        cfg.api_key = self.api_key.clone();
        cfg.model = self.model.clone();
        cfg.active_llm = self.name.clone();
    }

    pub fn display(&self) -> String {
        format!(
            "{} · {} w={} @ {}{}",
            self.name,
            self.model,
            self.weight.max(1),
            self.api_base,
            if self.enabled { "" } else { " [off]" }
        )
    }

    pub fn has_key(&self) -> bool {
        !self.api_key.trim().is_empty()
            || self.api_base.contains("11434")
            || self.api_base.contains("1234")
    }

    pub fn for_mode(&self, mode: crate::modes::AppMode) -> bool {
        match mode {
            crate::modes::AppMode::Code => self.use_for_code,
            crate::modes::AppMode::Office => self.use_for_office,
        }
    }
}

/// Runtime override (after auto-failover mid-session) without full config reload.
static RUNTIME_ACTIVE: Mutex<Option<String>> = Mutex::new(None);
static LAST_FAILOVER: Mutex<String> = Mutex::new(String::new());

pub fn runtime_active() -> Option<String> {
    RUNTIME_ACTIVE.lock().ok().and_then(|g| g.clone())
}

pub fn set_runtime_active(name: &str) {
    if let Ok(mut g) = RUNTIME_ACTIVE.lock() {
        *g = Some(name.to_string());
    }
}

pub fn last_failover_note() -> String {
    LAST_FAILOVER.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_failover_note(s: &str) {
    if let Ok(mut g) = LAST_FAILOVER.lock() {
        *g = s.to_string();
    }
}

/// Ensure pool has at least the primary endpoint; fill keys from env for known names.
pub fn ensure_pool(cfg: &mut Config) {
    if cfg.llm_pool.is_empty() {
        let mut primary = LlmEndpoint::from_primary(cfg);
        primary.name = if cfg.api_base.contains("x.ai") {
            "grok".into()
        } else if cfg.api_base.contains("11434") {
            "ollama".into()
        } else {
            "primary".into()
        };
        cfg.llm_pool.push(primary);
        // seed common locals / secondaries with empty keys (user fills in Settings)
        seed_defaults(cfg);
    }
    // Meta (Muse) aparece para configs antigas também, sem duplicar
    seed_meta(cfg);
    if cfg.active_llm.is_empty() {
        cfg.active_llm = cfg
            .llm_pool
            .first()
            .map(|e| e.name.clone())
            .unwrap_or_else(|| "primary".into());
    }
    // Sync flat fields from active endpoint
    if let Some(ep) = resolve_endpoint(cfg, None) {
        cfg.api_base = ep.api_base;
        cfg.api_key = ep.api_key;
        cfg.model = ep.model;
    }
}

fn seed_defaults(cfg: &mut Config) {
    // (nome, base, env1, env2, modelo, prioridade, workers, preço in, preço out)
    let defaults = [
        (
            "grok",
            "https://api.x.ai/v1",
            "XAI_API_KEY",
            "GROK_API_KEY",
            "grok-4.6",
            0u32,
            false,
            0.0,
            0.0,
        ),
        (
            "openai",
            "https://api.openai.com/v1",
            "OPENAI_API_KEY",
            "",
            "gpt-4.1-mini",
            10,
            false,
            0.0,
            0.0,
        ),
        (
            "openrouter",
            "https://openrouter.ai/api/v1",
            "OPENROUTER_API_KEY",
            "",
            "openai/gpt-4.1-mini",
            20,
            false,
            0.0,
            0.0,
        ),
        (
            "ollama",
            "http://localhost:11434/v1",
            "OLLAMA_API_KEY",
            "",
            "llama3.2",
            30,
            true,
            0.0,
            0.0,
        ),
        (
            "lmstudio",
            "http://localhost:1234/v1",
            "LMSTUDIO_API_KEY",
            "",
            "local-model",
            40,
            true,
            0.0,
            0.0,
        ),
    ];
    for (name, base, env1, env2, model, prio, workers, price_in, price_out) in defaults {
        if cfg.llm_pool.iter().any(|e| e.name == name) {
            continue;
        }
        // Don't duplicate if primary already is this base
        if cfg.llm_pool.iter().any(|e| e.api_base.contains(
            base.trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or(""),
        ) && e.name != name)
        {
            // still add if different name? skip if same host as existing with key
        }
        let mut key = std::env::var(env1).unwrap_or_default();
        if key.is_empty() && !env2.is_empty() {
            key = std::env::var(env2).unwrap_or_default();
        }
        if key.is_empty() && is_local(base) {
            key = "local".into();
        }
        let enabled = !key.is_empty();
        // Don't add disabled cloud without key to reduce noise — add but disabled
        cfg.llm_pool.push(LlmEndpoint {
            price_in,
            price_out,
            token_less: None,
            wire: String::new(),
            name: name.into(),
            api_base: base.into(),
            api_key: key,
            model: model.into(),
            enabled,
            priority: prio,
            weight: if workers { 20 } else { 50 },
            use_for_code: true,
            use_for_office: true,
            use_for_workers: workers,
        });
    }
}

/// Endpoint Meta (Muse) mesmo em configs antigas. Não duplica se já existir
/// algum na base meta.ai; só habilita quando há key (env ou já digitada).
pub fn seed_meta(cfg: &mut Config) {
    if cfg.llm_pool.iter().any(|e| e.api_base.contains("meta.ai")) {
        return;
    }
    let mut key = std::env::var("MODEL_API_KEY").unwrap_or_default();
    if key.trim().is_empty() {
        key = std::env::var("META_API_KEY").unwrap_or_default();
    }
    let has_key = !key.trim().is_empty();
    cfg.llm_pool.push(LlmEndpoint {
        price_in: 1.25,
        price_out: 4.25,
        // vazio = auto; wire_of deduz "responses" pelo host meta.ai
        token_less: None,
        wire: String::new(),
        name: "meta".into(),
        api_base: "https://api.meta.ai/v1".into(),
        api_key: key,
        model: "muse-spark-1.2".into(),
        enabled: has_key,
        priority: 5,
        weight: 50,
        use_for_code: true,
        use_for_office: true,
        use_for_workers: false,
    });
}

/// Endpoints usable for a mode (enabled + key + mode flag).
pub fn candidates_for_mode(cfg: &Config, mode: crate::modes::AppMode) -> Vec<LlmEndpoint> {
    cfg.llm_pool
        .iter()
        .filter(|e| e.enabled && e.has_key() && e.for_mode(mode))
        .cloned()
        .map(|mut e| {
            if e.api_key.is_empty() {
                e.api_key = "local".into();
            }
            if e.weight == 0 {
                e.weight = 1;
            }
            e
        })
        .collect()
}

/// Deterministic weighted pick for a time slot (stable within rotation window).
pub fn pick_weighted(candidates: &[LlmEndpoint], slot: u64) -> Option<LlmEndpoint> {
    if candidates.is_empty() {
        return None;
    }
    let total: u64 = candidates.iter().map(|e| e.weight.max(1) as u64).sum();
    if total == 0 {
        return candidates.first().cloned();
    }
    // Mix slot so different hours don't always pick first heavy weight the same way
    let mut r = slot
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0xBF58_476D_1CE4_E5B9)
        % total;
    for e in candidates {
        let w = e.weight.max(1) as u64;
        if r < w {
            return Some(e.clone());
        }
        r -= w;
    }
    candidates.last().cloned()
}

/// If rotation is enabled, switch active LLM by weights + mode every N minutes.
/// Returns a short status line if the active model changed.
pub fn maybe_rotate(cfg: &mut Config, mode: crate::modes::AppMode) -> Option<String> {
    if !cfg.llm_rotate_enabled {
        return None;
    }
    let mins = cfg.llm_rotate_minutes.max(1) as i64;
    let now = chrono::Utc::now().timestamp();
    let slot = (now / (mins * 60)) as u64;

    // Same slot as last time → keep current pick (unless invalid)
    if let Ok(prev_slot) = cfg.llm_rotate_slot.parse::<u64>() {
        if prev_slot == slot {
            // Ensure active is still a valid candidate
            let ok = candidates_for_mode(cfg, mode)
                .iter()
                .any(|e| e.name == cfg.active_llm);
            if ok {
                return None;
            }
        }
    }

    let cands = candidates_for_mode(cfg, mode);
    if cands.is_empty() {
        return None;
    }
    let prev = cfg.active_llm.clone();
    let Some(ep) = pick_weighted(&cands, slot) else {
        return None;
    };
    ep.apply_to(cfg);
    set_runtime_active(&ep.name);
    cfg.llm_rotate_slot = slot.to_string();
    cfg.llm_rotate_last = chrono::Utc::now().to_rfc3339();
    let _ = cfg.save();

    let total_w: u32 = cands.iter().map(|e| e.weight.max(1)).sum();
    let pct = if total_w > 0 {
        (ep.weight.max(1) * 100) / total_w
    } else {
        0
    };
    if ep.name != prev {
        let note = format!(
            "rotate → {} ({}) weight~{}% every {}m",
            ep.name, ep.model, pct, mins
        );
        set_failover_note(&note);
        Some(note)
    } else {
        None
    }
}

/// Human table of weights / rotation for UI and /llm list.
pub fn weights_text(cfg: &Config, mode: crate::modes::AppMode) -> String {
    let cands = candidates_for_mode(cfg, mode);
    let total: u32 = cands.iter().map(|e| e.weight.max(1)).sum::<u32>().max(1);
    let mut lines = vec![format!(
        "rotate={} every {}m  slot={}  mode={}",
        cfg.llm_rotate_enabled,
        cfg.llm_rotate_minutes.max(1),
        if cfg.llm_rotate_slot.is_empty() {
            "—"
        } else {
            &cfg.llm_rotate_slot
        },
        mode.label()
    )];
    if !cfg.llm_rotate_last.is_empty() {
        lines.push(format!("last_rotate={}", cfg.llm_rotate_last));
    }
    for e in &cfg.llm_pool {
        let in_mode = e.for_mode(mode) && e.enabled && e.has_key();
        let w = e.weight.max(1);
        let pct = if in_mode {
            (w * 100) / total
        } else {
            0
        };
        let mark = if e.name == cfg.active_llm { "→" } else { " " };
        lines.push(format!(
            "{mark} {} model={} w={w} (~{pct}%) code={} office={} on={} key={}",
            e.name,
            e.model,
            e.use_for_code,
            e.use_for_office,
            e.enabled,
            if e.has_key() { "yes" } else { "no" }
        ));
    }
    lines.join("\n")
}

/// GET /v1/models (OpenAI-compatible) for an endpoint.
/// Nome do endpoint ativo (o que `resolve_endpoint` escolheria agora).
pub fn active_name(cfg: &Config) -> String {
    resolve_endpoint(cfg, None)
        .map(|e| e.name)
        .unwrap_or_else(|| cfg.active_llm.clone())
}

/// Fixa o modelo no endpoint ativo, para a escolha sobreviver à troca de chat.
pub fn set_model_of_active(cfg: &mut Config, model: &str) {
    let name = active_name(cfg);
    if let Some(ep) = cfg.llm_pool.iter_mut().find(|e| e.name == name) {
        ep.model = model.to_string();
    }
}

pub fn fetch_remote_models(api_base: &str, api_key: &str) -> Result<Vec<String>> {
    let base = api_base.trim_end_matches('/');
    let url = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    let client = crate::llm::http_client();
    let mut req = client.get(&url);
    if !api_key.is_empty() && api_key != "local" {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().map_err(|e| anyhow::anyhow!("models request: {e}"))?;
    if !resp.status().is_success() {
        bail!("models HTTP {}", resp.status());
    }
    let v: serde_json::Value = resp.json().map_err(|e| anyhow::anyhow!("models json: {e}"))?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                out.push(id.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn resolve_endpoint(cfg: &Config, prefer: Option<&str>) -> Option<LlmEndpoint> {
    let name = prefer
        .map(|s| s.to_string())
        .or_else(runtime_active)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cfg.active_llm.clone());
    cfg.llm_pool
        .iter()
        .find(|e| e.name == name && e.enabled && !e.api_key.is_empty())
        .cloned()
        .or_else(|| {
            cfg.llm_pool
                .iter()
                .find(|e| e.enabled && !e.api_key.is_empty())
                .cloned()
        })
        .or_else(|| {
            // last resort: flat config fields
            if !cfg.api_key.is_empty() {
                Some(LlmEndpoint::from_primary(cfg))
            } else {
                None
            }
        })
}

/// Ordered try list: active first, then by priority.
pub fn failover_order(cfg: &Config) -> Vec<LlmEndpoint> {
    let active_name = runtime_active()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cfg.active_llm.clone());
    let mut list: Vec<_> = cfg
        .llm_pool
        .iter()
        .filter(|e| e.enabled && !e.api_key.trim().is_empty())
        .cloned()
        .collect();
    // ollama / lmstudio: allow empty key as "local"
    let local_eps: Vec<_> = cfg
        .llm_pool
        .iter()
        .filter(|e| {
            e.enabled
                && (e.api_base.contains("11434") || e.api_base.contains("1234"))
                && !list.iter().any(|x| x.name == e.name)
        })
        .cloned()
        .collect();
    for mut c in local_eps {
        if c.api_key.is_empty() {
            c.api_key = "local".into();
        }
        list.push(c);
    }
    list.sort_by_key(|e| {
        let primary = if e.name == active_name { 0u32 } else { 1 };
        (primary, e.priority, e.name.clone())
    });
    // dedupe by name
    let mut seen = std::collections::HashSet::new();
    list.retain(|e| seen.insert(e.name.clone()));
    list
}

pub fn worker_endpoint(cfg: &Config) -> Option<LlmEndpoint> {
    cfg.llm_pool
        .iter()
        .find(|e| e.use_for_workers && e.enabled && !e.api_key.is_empty())
        .cloned()
        .or_else(|| resolve_endpoint(cfg, None))
}

pub fn is_failover_error(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("429")
        || e.contains("rate limit")
        || e.contains("rate_limit")
        || e.contains("quota")
        || e.contains("insufficient_quota")
        || e.contains("402")
        || e.contains("credit")
        || e.contains("billing")
        || e.contains("overloaded")
        || e.contains("529")
        || e.contains("503")
        || e.contains("capacity")
        || e.contains("too many requests")
        || e.contains("resource_exhausted")
        || e.contains("tokens per day")
        || e.contains("budget")
}

pub fn list_text(cfg: &Config) -> String {
    let active = runtime_active().unwrap_or_else(|| cfg.active_llm.clone());
    let mut lines = vec![format!(
        "active={} failover={} rotate={} every {}m multi_worker={}",
        active,
        cfg.llm_auto_failover,
        cfg.llm_rotate_enabled,
        cfg.llm_rotate_minutes.max(1),
        cfg.llm_multi_worker
    )];
    let mut pool = cfg.llm_pool.clone();
    pool.sort_by_key(|e| e.priority);
    for e in pool {
        let mark = if e.name == active { "→" } else { " " };
        let key = if e.has_key() { "key" } else { "no-key" };
        lines.push(format!(
            "{mark} [{}] {} model={} w={} prio={} {} code={} office={} worker={}",
            if e.enabled { "on" } else { "off" },
            e.name,
            e.model,
            e.weight.max(1),
            e.priority,
            key,
            e.use_for_code,
            e.use_for_office,
            e.use_for_workers
        ));
    }
    let note = last_failover_note();
    if !note.is_empty() {
        lines.push(format!("last: {note}"));
    }
    lines.join("\n")
}

pub fn set_active(cfg: &mut Config, name: &str) -> Result<String> {
    let ep = cfg
        .llm_pool
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown LLM '{name}' — /llm list"))?
        .clone();
    if !ep.enabled {
        bail!("LLM '{name}' is disabled");
    }
    if ep.api_key.is_empty() && !ep.api_base.contains("11434") && !ep.api_base.contains("1234") {
        bail!("LLM '{name}' has empty api_key");
    }
    ep.apply_to(cfg);
    set_runtime_active(&ep.name);
    set_failover_note("");
    Ok(format!("active LLM → {}", ep.display()))
}

pub fn upsert_endpoint(cfg: &mut Config, ep: LlmEndpoint) {
    if let Some(slot) = cfg.llm_pool.iter_mut().find(|e| e.name == ep.name) {
        *slot = ep;
    } else {
        cfg.llm_pool.push(ep);
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn meta_e_detectada_pelo_host_sem_config() {
        // config antigo não tem o campo; o host decide
        assert!(is_local("http://192.168.0.50:8080/v1"));
        assert!(is_local("http://localhost:11434/v1"));
        assert!(!is_local("https://api.x.ai/v1"));
        assert_eq!(wire_of("", "https://api.meta.ai/v1"), Wire::Responses);
        assert_eq!(wire_of("", "https://api.x.ai/v1"), Wire::Chat);
        assert_eq!(wire_of("", "https://api.openai.com/v1"), Wire::Chat);
    }

    #[test]
    fn campo_explicito_vence_o_host() {
        assert_eq!(wire_of("chat", "https://api.meta.ai/v1"), Wire::Chat);
        assert_eq!(wire_of("Responses", "https://qualquer.host/v1"), Wire::Responses);
    }

    #[test]
    fn seed_meta_adiciona_endpoint_muse_sem_duplicar() {
        let mut cfg = crate::config::Config::default();
        cfg.llm_pool.clear();
        seed_meta(&mut cfg);
        let n = cfg.llm_pool.iter().filter(|e| e.name == "meta").count();
        assert_eq!(n, 1, "primeira chamada cria o endpoint");
        seed_meta(&mut cfg);
        let n = cfg.llm_pool.iter().filter(|e| e.api_base.contains("meta.ai")).count();
        assert_eq!(n, 1, "segunda chamada não duplica");
        let ep = cfg.llm_pool.iter().find(|e| e.name == "meta").unwrap();
        assert_eq!(ep.model, "muse-spark-1.2");
        assert_eq!(wire_of(&ep.wire, &ep.api_base), Wire::Responses);
    }
}
