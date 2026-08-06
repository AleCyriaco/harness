//! Contadores de trabalho e economia — medidos, não estimados de brochura.
//!
//! Vivem no processo do daemon (é lá que o agente e os workers rodam) e viajam
//! para a GUI junto do estado do swarm.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::sync::Mutex;

use crate::tokenless::TokenLessLevel;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LevelUsage {
    pub tag: String,
    pub replies: u64,
    #[serde(default)]
    pub completion_tokens: u64,
}

impl LevelUsage {
    pub fn avg(&self) -> f32 {
        if self.replies == 0 {
            0.0
        } else {
            self.completion_tokens as f32 / self.replies as f32
        }
    }
}

/// Consumo por origem: agente principal e cada worker do swarm.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceUsage {
    pub name: String,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    /// Entrada servida pelo cache do provedor (0 quando ele não informa).
    #[serde(default)]
    pub cached_tokens: u64,
}

impl SourceUsage {
    pub fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    /// Fatia da entrada que veio do cache do provedor.
    pub fn hit_rate(&self) -> f32 {
        if self.prompt_tokens == 0 {
            0.0
        } else {
            self.cached_tokens as f32 / self.prompt_tokens as f32
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    /// Uma entrada por nível de token_less já usado.
    #[serde(default)]
    pub token_less: Vec<LevelUsage>,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    /// Entrada servida pelo cache do provedor (0 quando ele não informa).
    #[serde(default)]
    pub cached_tokens: u64,
    /// Custo acumulado em USD, só quando há preço configurado no pool.
    #[serde(default)]
    pub cost_usd: f64,
    /// Uma entrada por origem: "main" e cada worker.
    #[serde(default)]
    pub by_source: Vec<SourceUsage>,
    /// Consultas ao grafo e tokens de leitura evitados (estimativa, ~4 chars/token).
    #[serde(default)]
    pub graph_queries: u64,
    #[serde(default)]
    pub graph_saved_tokens: i64,
    #[serde(default)]
    pub graph_builds: u64,
    /// Última build: quanto durou e quantos arquivos entraram.
    #[serde(default)]
    pub graph_last_build_ms: u64,
    #[serde(default)]
    pub graph_last_build_files: usize,
}

impl Metrics {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    /// Fatia da entrada servida por cache — comparável ao "avg hit".
    pub fn hit_rate(&self) -> f32 {
        if self.prompt_tokens == 0 {
            0.0
        } else {
            self.cached_tokens as f32 / self.prompt_tokens as f32
        }
    }

    pub fn level(&self, tag: &str) -> Option<&LevelUsage> {
        self.token_less.iter().find(|l| l.tag == tag)
    }

    /// Economia do token_less: média de tokens de saída com o nível `tag`
    /// comparada com a média medida em `off`. `None` enquanto faltar amostra
    /// dos dois lados — é comparação observada, não promessa.
    pub fn token_less_delta(&self, tag: &str) -> Option<f32> {
        let base = self.level("off").filter(|l| l.replies >= 3)?;
        let cur = self.level(tag).filter(|l| l.replies >= 3)?;
        if base.avg() <= 0.0 {
            return None;
        }
        Some((cur.avg() - base.avg()) / base.avg())
    }
}

static METRICS: Mutex<Option<Metrics>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Metrics) -> T) -> T {
    let mut g = METRICS.lock().expect("metrics");
    if g.is_none() {
        *g = Some(Metrics::default());
    }
    f(g.as_mut().unwrap())
}

pub fn snapshot() -> Metrics {
    with(|m| m.clone())
}

thread_local! {
    /// Nível de token_less do turno que roda nesta thread.
    static CURRENT: RefCell<TokenLessLevel> = const { RefCell::new(TokenLessLevel::Off) };
}

pub fn set_current_level(level: TokenLessLevel) {
    CURRENT.with(|c| *c.borrow_mut() = level);
}

pub fn current_level() -> TokenLessLevel {
    CURRENT.with(|c| *c.borrow())
}

/// Chamado a cada resposta do LLM, junto do `record_usage`.
///
/// `cached` = entrada servida pelo cache do provedor (0 quando ele não conta).
/// `cost` = USD, só quando o endpoint tem preço configurado.
pub fn record_call(prompt: u64, completion: u64, cached: u64, cost: f64) {
    let tag = current_level().tag();
    // worker do swarm se identifica na thread; vazio = agente principal
    let src = {
        let a = crate::swarm::current_agent();
        if a.is_empty() { "main".to_string() } else { a }
    };
    with(|m| {
        m.calls += 1;
        m.prompt_tokens += prompt;
        m.completion_tokens += completion;
        m.cached_tokens += cached;
        m.cost_usd += cost;
        match m.by_source.iter_mut().find(|s| s.name == src) {
            Some(s) => {
                s.calls += 1;
                s.prompt_tokens += prompt;
                s.completion_tokens += completion;
                s.cached_tokens += cached;
            }
            None => m.by_source.push(SourceUsage {
                name: src,
                calls: 1,
                prompt_tokens: prompt,
                completion_tokens: completion,
                cached_tokens: cached,
            }),
        }
        if completion == 0 {
            return;
        }
        match m.token_less.iter_mut().find(|l| l.tag == tag) {
            Some(l) => {
                l.replies += 1;
                l.completion_tokens += completion;
            }
            None => m.token_less.push(LevelUsage {
                tag: tag.into(),
                replies: 1,
                completion_tokens: completion,
            }),
        }
    });
}

pub fn record_graph_query(saved_tokens: i64) {
    with(|m| {
        m.graph_queries += 1;
        m.graph_saved_tokens += saved_tokens.max(0);
    });
}

pub fn record_graph_build(files: usize, ms: u64) {
    with(|m| {
        m.graph_builds += 1;
        m.graph_last_build_files = files;
        m.graph_last_build_ms = ms;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_needs_both_sides() {
        let mut m = Metrics::default();
        m.token_less.push(LevelUsage {
            tag: "off".into(),
            replies: 4,
            completion_tokens: 400,
        });
        // sem amostra de "full" ainda
        assert!(m.token_less_delta("full").is_none());
        m.token_less.push(LevelUsage {
            tag: "full".into(),
            replies: 2,
            completion_tokens: 100,
        });
        // 2 respostas < mínimo de 3
        assert!(m.token_less_delta("full").is_none());
        m.token_less[1].replies = 4;
        m.token_less[1].completion_tokens = 200;
        // 50/resposta contra 100/resposta = -50%
        let d = m.token_less_delta("full").unwrap();
        assert!((d + 0.5).abs() < 1e-6, "veio {d}");
    }
}
