use std::path::PathBuf;

use crate::types::SavedConfig;
use anyhow::{Context, Result};

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sjtu-canvas")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn history_path() -> PathBuf {
    config_dir().join("history.json")
}

fn _cookies_path() -> PathBuf {
    config_dir().join("cookies.json")
}

pub fn ensure_config_dir() -> Result<()> {
    std::fs::create_dir_all(config_dir()).context("Failed to create config directory")
}

pub fn load_config() -> Result<SavedConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(SavedConfig::default());
    }
    let content = std::fs::read_to_string(&path).context("Failed to read config.json")?;
    let config: SavedConfig =
        serde_json::from_str(&content).context("Failed to parse config.json")?;
    Ok(config)
}

pub fn _save_config(config: &SavedConfig) -> Result<()> {
    ensure_config_dir()?;
    let content = serde_json::to_string_pretty(config).context("Failed to serialize config")?;
    std::fs::write(config_path(), content).context("Failed to write config.json")?;
    Ok(())
}

pub fn _load_cookie_strings() -> Result<Vec<String>> {
    let path = _cookies_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path).context("Failed to read cookies")?;
    let cookies: Vec<String> = serde_json::from_str(&content).context("Failed to parse cookies")?;
    Ok(cookies)
}

pub fn _save_cookie_strings(cookie_strs: &[String]) -> Result<()> {
    ensure_config_dir()?;
    let content =
        serde_json::to_string_pretty(cookie_strs).context("Failed to serialize cookies")?;
    std::fs::write(_cookies_path(), content).context("Failed to write cookies")?;
    Ok(())
}

pub fn _clear_cookies() -> Result<()> {
    _save_cookie_strings(&[])
}
