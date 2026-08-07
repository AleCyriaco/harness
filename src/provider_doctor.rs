//! Provider doctor + named profiles + cache/usage helpers.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::llm;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub name: String,
    pub api_base: String,
    pub api_key_env: String,
    pub model: String,
    #[serde(default)]
    pub kind: String, // openai | anthropic | openai-compatible
}

pub fn builtin_profiles() -> Vec<ProviderProfile> {
    vec![
        ProviderProfile {
            name: "grok".into(),
            api_base: "https://api.x.ai/v1".into(),
            api_key_env: "XAI_API_KEY".into(),
            model: "grok-4.5".into(),
            kind: "openai-compatible".into(),
        },
        ProviderProfile {
            name: "openai".into(),
            api_base: "https://api.openai.com/v1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            model: "gpt-4.1-mini".into(),
            kind: "openai".into(),
        },
        ProviderProfile {
            name: "meta".into(),
            api_base: "https://api.meta.ai/v1".into(),
            api_key_env: "MODEL_API_KEY".into(),
            model: "muse-spark-1.2".into(),
            kind: "responses".into(),
        },
        ProviderProfile {
            name: "openrouter".into(),
            api_base: "https://openrouter.ai/api/v1".into(),
            api_key_env: "OPENROUTER_API_KEY".into(),
            model: "openai/gpt-4.1-mini".into(),
            kind: "openai-compatible".into(),
        },
        ProviderProfile {
            name: "deepseek".into(),
            api_base: "https://api.deepseek.com/v1".into(),
            api_key_env: "DEEPSEEK_API_KEY".into(),
            model: "deepseek-chat".into(),
            kind: "openai-compatible".into(),
        },
        ProviderProfile {
            name: "ollama".into(),
            api_base: "http://localhost:11434/v1".into(),
            api_key_env: "OLLAMA_API_KEY".into(),
            model: "llama3.2".into(),
            kind: "openai-compatible".into(),
        },
        ProviderProfile {
            name: "anthropic-proxy".into(),
            api_base: "https://api.anthropic.com/v1".into(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
            model: "claude-sonnet-4-5".into(),
            kind: "anthropic".into(),
        },
    ]
}

pub fn apply_profile(cfg: &mut Config, name: &str) -> Result<String> {
    let p = builtin_profiles()
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown profile {name}"))?;
    cfg.api_base = p.api_base;
    cfg.model = p.model;
    if let Ok(k) = std::env::var(&p.api_key_env) {
        if !k.is_empty() {
            cfg.api_key = k;
        }
    }
    // mark anthropic-ish bases for cache warnings
    if p.kind == "anthropic" || cfg.api_base.contains("anthropic") {
        note_provider_kind("anthropic");
    } else {
        note_provider_kind("openai-compatible");
    }
    Ok(format!(
        "profile {} → base={} model={}",
        p.name, cfg.api_base, cfg.model
    ))
}

static LAST_ANTHROPIC_TOUCH: Mutex<Option<Instant>> = Mutex::new(None);
static PROVIDER_KIND: Mutex<String> = Mutex::new(String::new());
static USAGE: Mutex<UsageStats> = Mutex::new(UsageStats {
    prompt_tokens: 0,
    completion_tokens: 0,
    calls: 0,
});

#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub calls: u64,
}

fn note_provider_kind(kind: &str) {
    if let Ok(mut g) = PROVIDER_KIND.lock() {
        *g = kind.into();
    }
}

pub fn touch_anthropic_cache() {
    if let Ok(mut g) = LAST_ANTHROPIC_TOUCH.lock() {
        *g = Some(Instant::now());
    }
}

/// Warn if Anthropic prompt cache likely cold (>5 min).
pub fn cache_warning() -> Option<String> {
    let kind = PROVIDER_KIND.lock().ok()?.clone();
    if kind != "anthropic" && !kind.contains("anthropic") {
        // also detect base
    }
    let last = LAST_ANTHROPIC_TOUCH.lock().ok()?.clone()?;
    let idle = last.elapsed();
    if idle > Duration::from_secs(5 * 60) {
        Some(format!(
            "⚠ Anthropic-style cache likely cold ({:.0}s idle > 300s) — next call may be a cache miss",
            idle.as_secs_f32()
        ))
    } else {
        None
    }
}

pub fn record_usage(prompt: u64, completion: u64) {
    if let Ok(mut g) = USAGE.lock() {
        g.prompt_tokens += prompt;
        g.completion_tokens += completion;
        g.calls += 1;
    }
    touch_anthropic_cache();
}

pub fn usage_summary() -> String {
    let g = USAGE.lock().ok();
    let Some(u) = g else {
        return "usage n/a".into();
    };
    format!(
        "calls={} prompt_tokens={} completion_tokens={} total={}",
        u.calls,
        u.prompt_tokens,
        u.completion_tokens,
        u.prompt_tokens + u.completion_tokens
    )
}

pub fn run() -> Result<()> {
    let cfg = Config::load();
    println!("harness doctor v{}", env!("CARGO_PKG_VERSION"));
    println!("workspace: {} ready={}", cfg.workspace.display(), cfg.workspace_ready);
    println!("api_base: {}", cfg.api_base);
    println!("model: {}", cfg.model);
    println!(
        "api_key: {}",
        if cfg.api_key.is_empty() {
            "MISSING"
        } else {
            "set"
        }
    );
    println!("profiles:");
    for p in builtin_profiles() {
        let has = std::env::var(&p.api_key_env).map(|k| !k.is_empty()).unwrap_or(false);
        println!(
            "  - {} ({}) key_env={} {}",
            p.name,
            p.model,
            p.api_key_env,
            if has { "OK" } else { "unset" }
        );
    }
    println!("usage: {}", usage_summary());
    if let Some(w) = cache_warning() {
        println!("{w}");
    }
    // quick ping if key present
    if !cfg.api_key.is_empty() {
        print!("llm ping… ");
        let msgs = vec![crate::llm::ChatMessage {
            role: "user".into(),
            content: Some("Reply with PONG only".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        match llm::chat(&cfg, &msgs, &[], &std::sync::atomic::AtomicBool::new(false), None) {
            Ok(r) => println!(
                "OK ({})",
                r.message
                    .content
                    .unwrap_or_default()
                    .chars()
                    .take(40)
                    .collect::<String>()
            ),
            Err(e) => println!("FAIL: {e}"),
        }
    }
    Ok(())
}

pub fn list_profiles_text() -> String {
    builtin_profiles()
        .iter()
        .map(|p| format!("{} → {} ({})", p.name, p.model, p.api_base))
        .collect::<Vec<_>>()
        .join("\n")
}
