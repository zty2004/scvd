use std::collections::HashMap;

use anyhow::{Context, Result};
use scraper::{Html, Selector};

use crate::client;

/// Parsed OAuth params from login redirect
#[derive(Debug, Clone)]
pub struct LoginParams {
    pub params: HashMap<String, String>,
    pub uuid: String,
    pub final_url: String,
}

/// Parse query params from a URL string (mimicking Python's parse_params).
fn parse_params(url: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    if let Some(q_start) = url.find('?') {
        let query = &url[q_start + 1..];
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                let k = urlencoding::decode(k).ok().map(|s| s.into_owned()).unwrap_or(k.to_string());
                let v = urlencoding::decode(v).ok().map(|s| s.into_owned()).unwrap_or(v.to_string());
                params.insert(k, v);
            }
        }
    }
    params
}

/// Visit the initial login URL, extract OAuth params, UUID, and cookies.
/// Uses a no-redirect client to capture Set-Cookie headers along the way.
pub async fn get_params_uuid_cookies(
    client: &reqwest::Client,
    login_url: &str,
) -> Result<LoginParams> {
    let resp = client
        .get(login_url)
        .header("accept-language", "zh-CN")
        .send()
        .await
        .context("Failed to visit login URL")?;

    // Capture Set-Cookie headers
    {
        let domain = resp.url().host_str().unwrap_or("");
        client::capture_and_save_cookies(&resp, domain).ok();
    }

    let final_url = resp.url().to_string();
    let body = resp.text().await.context("Failed to read login page")?;

    let params = parse_params(&final_url);

    // Extract UUID from <a id="firefox_link" href="?uuid=XXXX">
    let document = Html::parse_document(&body);
    let a_sel = Selector::parse("a#firefox_link").unwrap();
    let uuid = document
        .select(&a_sel)
        .next()
        .and_then(|a| a.value().attr("href"))
        .and_then(|href| href.split('=').nth(1))
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("Could not find UUID (firefox_link) on login page"))?;

    Ok(LoginParams {
        params,
        uuid,
        final_url,
    })
}

/// Fetch captcha image bytes. Caller should display it.
pub async fn get_captcha_bytes(
    client: &reqwest::Client,
    uuid: &str,
    referer_url: &str,
) -> Result<Vec<u8>> {
    let resp = client
        .get("https://jaccount.sjtu.edu.cn/jaccount/captcha")
        .query(&[("uuid", uuid), ("t", &format!("{}", chrono::Utc::now().timestamp_millis()))])
        .header("Referer", referer_url)
        .send()
        .await
        .context("Failed to fetch captcha")?;

    // Capture cookies from captcha response
    client::capture_and_save_cookies(&resp, "jaccount.sjtu.edu.cn").ok();

    resp.bytes().await
        .context("Failed to read captcha")
        .map(|b| b.to_vec())
}

/// POST login credentials + captcha to complete authentication.
/// Returns true on success (redirected away from jalogin page).
pub async fn login(
    client: &reqwest::Client,
    username: &str,
    password: &str,
    uuid: &str,
    captcha: &str,
    params: &HashMap<String, String>,
) -> Result<bool> {
    let mut form = HashMap::new();
    form.insert("user".to_string(), username.to_string());
    form.insert("pass".to_string(), password.to_string());
    form.insert("uuid".to_string(), uuid.to_string());
    form.insert("captcha".to_string(), captcha.to_string());

    // Include all OAuth params
    for (k, v) in params {
        form.insert(k.clone(), v.clone());
    }

    let resp = client
        .post("https://jaccount.sjtu.edu.cn/jaccount/ulogin")
        .form(&form)
        .header("accept-language", "zh-CN")
        .send()
        .await
        .context("Failed to submit login form")?;

    // Capture cookies from login response
    client::capture_and_save_cookies(&resp, "jaccount.sjtu.edu.cn").ok();

    // Login failed if redirected to jalogin page
    let failed = resp.url().to_string().starts_with("https://jaccount.sjtu.edu.cn/jaccount/jalogin");

    Ok(!failed)
}

/// Replay a GET request with existing cookies to re-establish session.
pub async fn login_using_cookies(
    client: &reqwest::Client,
    url: &str,
) -> Result<()> {
    let resp = client
        .get(url)
        .header("accept-language", "zh-CN")
        .send()
        .await
        .context("Failed to restore session")?;

    // Capture cookies from OC session establishment
    if let Some(domain) = resp.url().host_str() {
        client::capture_and_save_cookies(&resp, domain).ok();
    }

    Ok(())
}

/// Visit the Canvas login URL to get initial cookies for the jAccount flow.
pub const CANVAS_LOGIN_URL: &str =
    "https://courses.sjtu.edu.cn/app/oauth/2.0/login?login_type=outer";

/// Visit the OpenCourse login URL to establish OC session after jAccount login.
pub const OC_LOGIN_URL: &str = "https://oc.sjtu.edu.cn/login/openid_connect";
