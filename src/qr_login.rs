use anyhow::{Context, Result};
use tungstenite::Message;

/// Fetch QR code image bytes.
pub async fn get_qr_code_bytes(
    client: &reqwest::Client,
    uuid: &str,
    ts: &str,
    sig: &str,
) -> Result<Vec<u8>> {
    let resp = client
        .get("https://jaccount.sjtu.edu.cn/jaccount/qrcode")
        .query(&[("uuid", uuid), ("ts", ts), ("sig", sig)])
        .send()
        .await
        .context("Failed to fetch QR code")?;

    resp.bytes().await
        .context("Failed to read QR code")
        .map(|b| b.to_vec())
}

/// Display QR code in terminal.
pub fn display_qr(bytes: &[u8]) {
    if let Ok(img) = image::load_from_memory(bytes) {
        if let Err(e) = viuer::print(&img, &viuer::Config::default()) {
            eprintln!("Warning: failed to display QR code: {}", e);
        }
    }
}

/// Open WebSocket and monitor for QR updates / login event.
///
/// Calls `on_update(ts, sig)` when a new QR code is available (caller should
/// fetch the new QR image and re-display it).
/// Returns Ok(()) when the LOGIN event is received.
///
/// This function blocks until login or WS close.
pub fn monitor_qr_ws(
    uuid: &str,
    cookie_header: &str,
    on_update: impl Fn(String, String) -> Result<()>,
) -> Result<()> {
    let ws_url = format!("wss://jaccount.sjtu.edu.cn/jaccount/sub/{}", uuid);
    let uri: tungstenite::http::Uri = ws_url.parse().unwrap();
    let mut request = tungstenite::client::IntoClientRequest::into_client_request(uri).unwrap();
    request.headers_mut().insert(
        "cookie",
        tungstenite::http::HeaderValue::from_str(cookie_header).unwrap(),
    );

    let (mut socket, _) = tungstenite::connect(request)
        .context("Failed to connect WebSocket")?;

    // Request initial QR code
    let _ = socket.send(Message::Text(r#"{"type": "UPDATE_QR_CODE"}"#.into()));

    loop {
        let msg = match socket.read() {
            Ok(m) => m,
            Err(_) => break,
        };

        let Message::Text(text) = msg else { continue };

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            let t = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match t {
                "UPDATE_QR_CODE" => {
                    if let Some(payload) = json.get("payload") {
                        let ts = payload.get("ts").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let sig = payload.get("sig").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        on_update(ts, sig)?;
                    }
                }
                "LOGIN" => {
                    println!("QR code scanned! Logging in...");
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    Err(anyhow::anyhow!("WebSocket closed without login event"))
}

/// Complete QR login by calling express login endpoint.
/// Returns true on success (redirected away from expresslogin page).
pub async fn express_login(
    client: &reqwest::Client,
    uuid: &str,
) -> Result<bool> {
    let resp = client
        .get("https://jaccount.sjtu.edu.cn/jaccount/expresslogin")
        .query(&[("uuid", uuid)])
        .send()
        .await
        .context("Failed to perform express login")?;

    // Failed if still on expresslogin page
    let failed = resp.url().to_string().starts_with("https://jaccount.sjtu.edu.cn/jaccount/expresslogin");

    Ok(!failed)
}
