use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::provider::Provider;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub port: u16,
    #[serde(default)]
    pub provider: Provider,
    pub model: String,
    /// Vertex AI (Google Cloud) project for `provider = "vertex"`. ADC auth, so
    /// it bills GCP credit instead of a metered API key. Falls back to the
    /// IRONMEM_VERTEX_PROJECT env var when unset.
    #[serde(default)]
    pub vertex_project: Option<String>,
    /// Vertex region (or "global"). Override with IRONMEM_VERTEX_LOCATION.
    #[serde(default = "default_vertex_location")]
    pub vertex_location: String,
    pub inject_limit: usize,
    pub max_observation_bytes: usize,
    pub db_path: String,
    #[serde(default)]
    pub database_url: Option<String>,
    #[serde(default = "default_mcp_transport")]
    pub mcp_transport: String,
    #[serde(default = "default_mcp_sse_port")]
    pub mcp_sse_port: u16,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub rerank: RerankConfig,
    #[serde(default)]
    pub llm_retry: LlmRetryConfig,
    #[serde(default)]
    pub temporal_trust: TemporalTrustConfig,
    #[serde(default)]
    pub governance_router: GovernanceRouterConfig,
    #[serde(default)]
    pub multi_hop: MultiHopConfig,
}

/// (W3.1) Iterative multi-hop retrieval. For questions that chain facts across
/// turns, the retriever runs extra retrieve→reason→re-query hops. Gated to
/// multi-hop-looking queries only (see `retrieval::is_multi_hop_query`), so
/// single-hop recall pays no extra latency. `enabled` is overridable at runtime
/// via `IRONMEM_MULTI_HOP_ENABLED` (0/1) when latency needs to be cut fast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiHopConfig {
    #[serde(default = "default_multi_hop_enabled")]
    pub enabled: bool,
    /// Total retrieval passes (>=1). 2 = one bridge hop after the first search.
    #[serde(default = "default_multi_hop_max_hops")]
    pub max_hops: usize,
}

impl Default for MultiHopConfig {
    fn default() -> Self {
        Self {
            enabled: default_multi_hop_enabled(),
            max_hops: default_multi_hop_max_hops(),
        }
    }
}

fn default_multi_hop_enabled() -> bool {
    true
}

fn default_multi_hop_max_hops() -> usize {
    2
}

/// (#1) Governed retrieval router (paper M3): the writer trust-tier recorded on
/// every memory becomes a query-time ranking signal, so user-explicit (`High`)
/// facts outrank machine-derived (`Medium`) ones and `Low`/`Untrusted` writers
/// are demoted. Additive and symmetric around `Medium`, so it only reorders
/// near-ties. See `governance::tier_authority_boost`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceRouterConfig {
    /// Authority weight added to a candidate's retrieval score by writer tier.
    /// 0.0 = off. Defaults to a conservative on-value (matches `temporal_trust`).
    #[serde(default = "default_router_weight")]
    pub weight: f64,
}

impl Default for GovernanceRouterConfig {
    fn default() -> Self {
        Self {
            weight: default_router_weight(),
        }
    }
}

fn default_router_weight() -> f64 {
    0.05
}

/// Temporal trust trajectory as a retrieval signal (paper Finding 4: "standard
/// semantic consolidation often destroys crucial chronological cues"). Each
/// memory accrues a trajectory — first_seen / last_validated / receipt-confirmed
/// ref_count — and this controls how much that trajectory boosts retrieval rank.
/// `weight = 0.0` (the default) is a pure no-op: the trajectory is still recorded
/// and exposed, but ranking is unchanged, so the lever can be A/B-tuned against
/// the funnel without a rebuild.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalTrustConfig {
    /// Multiplier on the trajectory boost added to a candidate's retrieval score.
    /// 0.0 = off. Small values (≈0.05–0.15) nudge recently-validated, frequently-
    /// referenced memories up without overriding semantic relevance. Defaults to a
    /// conservative on-value so the trust trajectory actually shapes ranking; the
    /// base reciprocal-rank term stays dominant, so it only reorders near-ties.
    #[serde(default = "default_trust_weight")]
    pub weight: f64,
    /// Half-life (days) for the recency term: a memory last validated this many
    /// days ago contributes half the recency boost of one validated just now.
    #[serde(default = "default_trust_halflife_days")]
    pub recency_halflife_days: f64,
    /// Saturating scale on the reference-count term, so a heavily-referenced
    /// memory can't dominate purely on popularity.
    #[serde(default = "default_trust_ref_saturation")]
    pub ref_saturation: f64,
    /// (B) Times the date-bearing temporal-event id-list is pushed into RRF
    /// fusion (>=1). 1 = unchanged; higher lifts exact dated facts that semantic
    /// and keyword channels rank low (LoCoMo temporal questions). Gated, A/B-able.
    #[serde(default = "default_temporal_event_fusion_weight")]
    pub temporal_event_fusion_weight: usize,
}

impl Default for TemporalTrustConfig {
    fn default() -> Self {
        Self {
            weight: default_trust_weight(),
            recency_halflife_days: default_trust_halflife_days(),
            ref_saturation: default_trust_ref_saturation(),
            temporal_event_fusion_weight: default_temporal_event_fusion_weight(),
        }
    }
}

fn default_trust_weight() -> f64 {
    0.05
}

fn default_trust_halflife_days() -> f64 {
    30.0
}

fn default_trust_ref_saturation() -> f64 {
    5.0
}

fn default_temporal_event_fusion_weight() -> usize {
    1
}

/// LLM reranking of retrieval candidates. Off by default: it adds one provider
/// call (and its latency) per query. When enabled, retrieval pulls a `pool`-sized
/// candidate set that a fast model reranks down to the requested limit — the
/// precision lever for date- and answer-specific questions where the answer
/// memory is in the pool but not yet in the top few.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankConfig {
    /// Default on/off when a request doesn't specify `?rerank=`.
    #[serde(default)]
    pub enabled: bool,
    /// Model for the rerank scoring call. Empty (the default) means "use the
    /// compression `model`" — always available and as capable as the model the
    /// user already trusts. Set a cheaper/faster model id here to override.
    #[serde(default = "default_rerank_model")]
    pub model: String,
    /// Minimum candidate-pool size to rerank from (the effective pool is at least
    /// twice the requested limit, so there is always headroom to promote a buried
    /// answer memory into the top results).
    #[serde(default = "default_rerank_pool")]
    pub pool: usize,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_rerank_model(),
            pool: default_rerank_pool(),
        }
    }
}

fn default_rerank_model() -> String {
    // Empty ⇒ fall back to the compression `model` at call time (see retrieval).
    String::new()
}

fn default_rerank_pool() -> usize {
    50
}

/// Retry policy for provider calls that are safe to repeat: compression,
/// profile/reflection completions, and retrieval reranking. The defaults are
/// intentionally conservative for Vertex quota bursts while still surfacing
/// hard failures quickly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRetryConfig {
    #[serde(default = "default_llm_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_llm_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
}

impl Default for LlmRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_llm_max_attempts(),
            initial_backoff_ms: default_llm_initial_backoff_ms(),
        }
    }
}

fn default_llm_max_attempts() -> u32 {
    3
}

fn default_llm_initial_backoff_ms() -> u64 {
    500
}

/// Semantic-retrieval configuration. Local-first / no-egress by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// auto | ollama | openai | google | onnx | none
    #[serde(default = "default_embed_provider")]
    pub provider: String,
    /// Override the per-provider default model.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,
    #[serde(default)]
    pub weights: Weights,
    #[serde(default = "default_half_life")]
    pub recency_half_life_days: f64,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: default_embed_provider(),
            model: None,
            ollama_url: default_ollama_url(),
            weights: Weights::default(),
            recency_half_life_days: default_half_life(),
        }
    }
}

/// Blend weights for session-start injection ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Weights {
    #[serde(default = "default_w_relevance")]
    pub relevance: f64,
    #[serde(default = "default_w_recency")]
    pub recency: f64,
    #[serde(default = "default_w_importance")]
    pub importance: f64,
    /// Optional per-`kind` score multipliers that override the built-in priors
    /// in [`Weights::kind_multiplier`]. Absent in legacy settings → empty map →
    /// built-in defaults apply. Keys are memory kinds (e.g. `"preference"`).
    #[serde(default)]
    pub kind_boosts: std::collections::HashMap<String, f64>,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            relevance: default_w_relevance(),
            recency: default_w_recency(),
            importance: default_w_importance(),
            kind_boosts: std::collections::HashMap::new(),
        }
    }
}

impl Weights {
    /// Score multiplier for a memory `kind`. A configured `kind_boosts` entry
    /// wins; otherwise built-in priors gently lift durable, high-signal kinds
    /// (profile/error_solution/preference) over plain session summaries. Unknown
    /// kinds are neutral (1.0).
    pub fn kind_multiplier(&self, kind: &str) -> f64 {
        if let Some(&m) = self.kind_boosts.get(kind) {
            return m;
        }
        match kind {
            "profile" => 1.4,
            "error_solution" => 1.3,
            "procedural" => 1.28,
            "preference" => 1.25,
            "architecture" | "learned_pattern" => 1.15,
            "project_config" => 1.1,
            _ => 1.0,
        }
    }
}

fn default_embed_provider() -> String {
    "auto".to_string()
}
fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_half_life() -> f64 {
    30.0
}
fn default_w_relevance() -> f64 {
    0.5
}
fn default_w_recency() -> f64 {
    0.3
}
fn default_w_importance() -> f64 {
    0.2
}

fn default_mcp_transport() -> String {
    "stdio".to_string()
}

fn default_mcp_sse_port() -> u16 {
    37779
}

fn default_vertex_location() -> String {
    "us-central1".to_string()
}

impl Default for Config {
    fn default() -> Self {
        let provider = Provider::default();
        Self {
            port: 37778,
            provider,
            model: provider.default_model().to_string(),
            vertex_project: None,
            vertex_location: default_vertex_location(),
            inject_limit: 5,
            max_observation_bytes: 2048,
            db_path: ironmem_dir().join("mem.db").to_string_lossy().to_string(),
            database_url: None,
            mcp_transport: default_mcp_transport(),
            mcp_sse_port: default_mcp_sse_port(),
            auth_token: None,
            embedding: EmbeddingConfig::default(),
            rerank: RerankConfig::default(),
            llm_retry: LlmRetryConfig::default(),
            temporal_trust: TemporalTrustConfig::default(),
            governance_router: GovernanceRouterConfig::default(),
            multi_hop: MultiHopConfig::default(),
        }
    }
}

impl Config {
    /// Whether iterative multi-hop retrieval is active. `IRONMEM_MULTI_HOP_ENABLED`
    /// (0/1/true/false/on/off) overrides the configured default at runtime.
    pub fn multi_hop_enabled(&self) -> bool {
        match std::env::var("IRONMEM_MULTI_HOP_ENABLED") {
            Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
            Err(_) => self.multi_hop.enabled,
        }
    }

    pub fn effective_database_url(&self) -> String {
        std::env::var("DATABASE_URL")
            .ok()
            .or_else(|| self.database_url.clone())
            .unwrap_or_else(|| format!("sqlite://{}?mode=rwc", self.db_path))
    }

    pub fn effective_mcp_transport(&self) -> String {
        std::env::var("IRONMEM_MCP_TRANSPORT").unwrap_or_else(|_| self.mcp_transport.clone())
    }

    pub fn effective_llm_max_attempts(&self) -> u32 {
        std::env::var("IRONMEM_LLM_MAX_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(self.llm_retry.max_attempts)
            .clamp(1, 10)
    }

    pub fn effective_llm_initial_backoff_ms(&self) -> u64 {
        std::env::var("IRONMEM_LLM_INITIAL_BACKOFF_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(self.llm_retry.initial_backoff_ms)
            .clamp(50, 30_000)
    }

    /// Get or generate the auth token for SSE. Persists to settings on first generation.
    #[allow(dead_code)]
    pub fn ensure_auth_token(&mut self) -> String {
        if let Some(ref token) = self.auth_token {
            return token.clone();
        }
        let token = uuid::Uuid::new_v4().to_string();
        self.auth_token = Some(token.clone());
        let _ = save(self);
        token
    }
}

pub fn ironmem_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| {
            // Fallback for environments where home dir detection fails
            #[cfg(windows)]
            {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".to_string()))
            }
            #[cfg(not(windows))]
            {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
            }
        })
        .join(".ironmem")
}

pub fn settings_path() -> PathBuf {
    ironmem_dir().join("settings.json")
}

pub fn load() -> Result<Config> {
    let path = settings_path();
    if !path.exists() {
        let config = Config::default();
        save(&config)?;
        return Ok(config);
    }
    let raw = std::fs::read_to_string(&path)?;
    let config: Config = serde_json::from_str(&raw)?;
    Ok(config)
}

pub fn save(config: &Config) -> Result<()> {
    let dir = ironmem_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(settings_path(), json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{
        "port": 37778,
        "model": "claude-sonnet-4-6",
        "inject_limit": 5,
        "max_observation_bytes": 2048,
        "db_path": "/tmp/mem.db"
    }"#;

    #[test]
    fn missing_embedding_key_yields_defaults() {
        let cfg: Config = serde_json::from_str(BASE).unwrap();
        assert_eq!(cfg.embedding.provider, "auto");
        assert_eq!(cfg.embedding.weights.relevance, 0.5);
        assert_eq!(cfg.embedding.weights.recency, 0.3);
        assert_eq!(cfg.embedding.weights.importance, 0.2);
        assert_eq!(cfg.embedding.recency_half_life_days, 30.0);
        assert_eq!(cfg.embedding.ollama_url, "http://localhost:11434");
    }

    #[test]
    fn missing_rerank_key_yields_defaults() {
        let cfg: Config = serde_json::from_str(BASE).unwrap();
        assert!(
            !cfg.rerank.enabled,
            "rerank is off unless explicitly enabled"
        );
        assert_eq!(cfg.rerank.pool, 50);
        // Empty default ⇒ rerank falls back to the compression model.
        assert!(
            cfg.rerank.model.is_empty(),
            "default rerank model defers to compression model"
        );
    }

    #[test]
    fn missing_retry_key_yields_defaults() {
        let cfg: Config = serde_json::from_str(BASE).unwrap();
        assert_eq!(cfg.llm_retry.max_attempts, 3);
        assert_eq!(cfg.llm_retry.initial_backoff_ms, 500);
        assert_eq!(cfg.effective_llm_max_attempts(), 3);
    }

    #[test]
    fn provider_none_round_trips() {
        let raw = BASE.replace(
            "\"db_path\": \"/tmp/mem.db\"",
            "\"db_path\": \"/tmp/mem.db\", \"embedding\": { \"provider\": \"none\" }",
        );
        let cfg: Config = serde_json::from_str(&raw).unwrap();
        assert_eq!(cfg.embedding.provider, "none");
        // Round-trip through JSON preserves the explicit provider.
        let back = serde_json::to_string(&cfg).unwrap();
        let cfg2: Config = serde_json::from_str(&back).unwrap();
        assert_eq!(cfg2.embedding.provider, "none");
    }
}
