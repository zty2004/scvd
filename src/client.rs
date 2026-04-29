use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::cookie::Jar;

use crate::config;
use crate::vdebug;

/// A serializable cookie entry
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CookieEntry {
    pub domain: String,
    pub name: String,
    pub value: String,
    pub path: String,
}

/// Create a new HTTP client with cookie support, loading saved cookies if available.
pub fn create_client() -> Result<reqwest::Client> {
    let cookie_jar = Arc::new(Jar::default());

    // Restore saved cookies
    if let Ok(cookies) = load_cookies_from_file() {
        for entry in &cookies {
            let cookie_str = format!("{}={}", entry.name, entry.value);
            if let Ok(url) = format!("https://{}", entry.domain).parse::<url::Url>() {
                cookie_jar.add_cookie_str(&cookie_str, &url);
            }
        }
        if !cookies.is_empty() {
            vdebug!("Loaded {} saved cookies.", cookies.len());
        }
    }

    let client = reqwest::Client::builder()
        .cookie_provider(cookie_jar)
        .redirect(reqwest::redirect::Policy::limited(20))
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .build()?;

    Ok(client)
}

/// Create a client that does NOT follow redirects, for capturing Set-Cookie headers.
pub fn _create_no_redirect_client() -> Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .build()?;

    Ok(client)
}

/// Capture Set-Cookie headers from a response and save them to file.
pub fn capture_and_save_cookies(resp: &reqwest::Response, domain: &str) -> Result<()> {
    let mut cookies = load_cookies_from_file().unwrap_or_default();

    for val in resp.headers().get_all("set-cookie") {
        if let Ok(s) = val.to_str() {
            if let Some((nv, _rest)) = s.split_once(';') {
                if let Some((name, value)) = nv.trim().split_once('=') {
                    // Remove existing cookie with same domain+name
                    cookies.retain(|c: &CookieEntry| !(c.domain == domain && c.name == name));
                    cookies.push(CookieEntry {
                        domain: domain.to_string(),
                        name: name.to_string(),
                        value: value.to_string(),
                        path: "/".to_string(),
                    });
                }
            }
        }
    }

    if !cookies.is_empty() {
        save_cookies_to_file(&cookies)?;
    }

    Ok(())
}

fn load_cookies_from_file() -> Result<Vec<CookieEntry>> {
    let path = config::config_dir().join("cookies.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path).context("Failed to read cookies")?;
    let cookies: Vec<CookieEntry> =
        serde_json::from_str(&content).context("Failed to parse cookies")?;
    Ok(cookies)
}

fn save_cookies_to_file(cookies: &[CookieEntry]) -> Result<()> {
    config::ensure_config_dir()?;
    let path = config::config_dir().join("cookies.json");
    let content = serde_json::to_string_pretty(cookies).context("Failed to serialize cookies")?;
    std::fs::write(&path, content).context("Failed to write cookies")?;
    Ok(())
}

pub fn _clear_cookies() -> Result<()> {
    let path = config::config_dir().join("cookies.json");
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}
