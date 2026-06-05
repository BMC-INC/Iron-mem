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
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            relevance: default_w_relevance(),
            recency: default_w_recency(),
            importance: default_w_importance(),
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

impl Default for Config {
    fn default() -> Self {
        let provider = Provider::default();
        Self {
            port: 37778,
            provider,
            model: provider.default_model().to_string(),
            inject_limit: 5,
            max_observation_bytes: 2048,
            db_path: ironmem_dir().join("mem.db").to_string_lossy().to_string(),
            database_url: None,
            mcp_transport: default_mcp_transport(),
            mcp_sse_port: default_mcp_sse_port(),
            auth_token: None,
            embedding: EmbeddingConfig::default(),
        }
    }
}

impl Config {
    pub fn effective_database_url(&self) -> String {
        std::env::var("DATABASE_URL")
            .ok()
            .or_else(|| self.database_url.clone())
            .unwrap_or_else(|| format!("sqlite://{}?mode=rwc", self.db_path))
    }

    pub fn effective_mcp_transport(&self) -> String {
        std::env::var("IRONMEM_MCP_TRANSPORT").unwrap_or_else(|_| self.mcp_transport.clone())
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
