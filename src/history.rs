use std::sync::Mutex;

use anyhow::{Context, Result};
use crate::types::HistoryEntry;
use crate::config::history_path;

static HISTORY: Mutex<Vec<HistoryEntry>> = Mutex::new(Vec::new());
static LOADED: Mutex<bool> = Mutex::new(false);

fn ensure_loaded() {
    let mut loaded = LOADED.lock().unwrap();
    if !*loaded {
        *loaded = true;
        if let Ok(entries) = load_history_from_file() {
            let mut history = HISTORY.lock().unwrap();
            *history = entries;
        }
    }
}

fn load_history_from_file() -> Result<Vec<HistoryEntry>> {
    let path = history_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .context("Failed to read history.json")?;
    let entries: Vec<HistoryEntry> = serde_json::from_str(&content)
        .context("Failed to parse history.json")?;
    Ok(entries)
}

pub fn get_history() -> Vec<HistoryEntry> {
    ensure_loaded();
    HISTORY.lock().unwrap().clone()
}

pub fn add_history(entry: HistoryEntry) {
    ensure_loaded();
    HISTORY.lock().unwrap().push(entry);
    let _ = save_to_file();
}

pub fn _save_history() -> Result<()> {
    ensure_loaded();
    save_to_file()
}

fn save_to_file() -> Result<()> {
    let path = history_path();
    let entries = HISTORY.lock().unwrap();
    let content = serde_json::to_string_pretty(&*entries)
        .context("Failed to serialize history")?;
    std::fs::write(&path, content)
        .context("Failed to write history.json")?;
    Ok(())
}

pub fn clear_history() -> Result<()> {
    ensure_loaded();
    HISTORY.lock().unwrap().clear();
    save_to_file()
}
