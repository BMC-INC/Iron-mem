use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub port: u16,
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
}

fn default_mcp_transport() -> String {
    "stdio".to_string()
}

fn default_mcp_sse_port() -> u16 {
    37779
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 37778,
            model: "claude-sonnet-4-6-20250627".to_string(),
            inject_limit: 5,
            max_observation_bytes: 2048,
            db_path: ironmem_dir().join("mem.db").to_string_lossy().to_string(),
            database_url: None,
            mcp_transport: default_mcp_transport(),
            mcp_sse_port: default_mcp_sse_port(),
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
}

pub fn ironmem_dir() -> PathBuf {
    dirs::home_dir()
        .expect("Could not find home directory")
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
