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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 37778,
            model: "claude-sonnet-4-20250514".to_string(),
            inject_limit: 5,
            max_observation_bytes: 2048,
            db_path: ironmem_dir()
                .join("mem.db")
                .to_string_lossy()
                .to_string(),
        }
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
