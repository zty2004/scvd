use std::collections::HashMap;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::Rng;
use scraper::{Html, Selector};
use md5::{Md5, Digest};
use url::Url;
use std::sync::Arc;
use reqwest::cookie::Jar;

use crate::types::{Course, VideoInfo};

// ============================================================
// Helper: JSON value navigation
// ============================================================

fn get_nested<'a>(val: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut cur = val;
    for &key in path {
        cur = cur.get(key)?;
    }
    Some(cur)
}

fn extract_records(payload: &serde_json::Value) -> Option<Vec<&serde_json::Value>> {
    if payload.is_array() {
        return Some(payload.as_array().unwrap().iter().collect());
    }
    let candidates: Vec<Vec<&str>> = vec![
        vec!["data", "records"],
        vec!["data", "list"],
        vec!["data", "rows"],
        vec!["data", "items"],
        vec!["data", "page", "records"],
        vec!["data", "page", "list"],
        vec!["body", "list"],
        vec!["body"],
        vec!["data"],
    ];
    for path in candidates {
        if let Some(arr) = get_nested(payload, &path).and_then(|v| v.as_array()) {
            return Some(arr.iter().collect());
        }
    }
    None
}

fn extract_detail(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    for path in [vec!["data"], vec!["body"]] {
        if let Some(obj) = get_nested(payload, &path) {
            if obj.is_object() {
                return Some(obj);
            }
        }
    }
    None
}

// ============================================================
// JWT helpers
// ============================================================

fn decode_jwt_payload(token: &str) -> serde_json::Value {
    if token.is_empty() || token.matches('.').count() < 2 {
        return serde_json::Value::Null;
    }
    let parts: Vec<&str> = token.split('.').collect();
    let payload = parts[1];
    let padding = "====".chars().take((4 - payload.len() % 4) % 4).collect::<String>();
    let padded = format!("{}{}", payload, padding);
    if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&padded)
        .or_else(|_| BASE64.decode(&padded))
    {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    }
}

fn parse_redirect_params(url: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    if let Ok(parsed) = Url::parse(url) {
        for (k, v) in parsed.query_pairs() {
            params.insert(k.to_string(), v.to_string());
        }
        // Also parse query params after #/path?...
        if let Some(frag) = parsed.fragment() {
            if let Some(q) = frag.find('?') {
                let query = &frag[q + 1..];
                for pair in query.split('&') {
                    if let Some((k, v)) = pair.split_once('=') {
                        params.insert(
                            urlencoding::decode(k).ok().map(|s| s.into_owned()).unwrap_or(k.to_string()),
                            urlencoding::decode(v).ok().map(|s| s.into_owned()).unwrap_or(v.to_string()),
                        );
                    }
                }
            }
        }
    }
    params
}

fn get_canvas_course_id(
    params_dict: &HashMap<String, String>,
    payloads: &[serde_json::Value],
) -> Option<String> {
    for key in ["courId", "canvasCourseId", "courseId", "ltiCourseId"] {
        if let Some(v) = params_dict.get(key) {
            if !v.is_empty() {
                return Some(v.clone());
            }
        }
    }
    for source in payloads {
        for key in ["lti_message_hint", "id_token", "state"] {
            if let Some(val) = source.get(key).and_then(|v| v.as_str()) {
                let payload = decode_jwt_payload(val);
                if let Some(ctx_id) = payload.get("context_id").and_then(|v| v.as_str()) {
                    return Some(ctx_id.to_string());
                }
                let lti_ctx = payload.get("https://purl.imsglobal.org/spec/lti/claim/context");
                if let Some(ctx) = lti_ctx {
                    if let Some(id) = ctx.get("id").and_then(|v| v.as_str()) {
                        return Some(id.to_string());
                    }
                }
                for fb_key in ["courId", "canvasCourseId", "courseId", "ltiCourseId"] {
                    if let Some(v) = payload.get(fb_key).and_then(|v| v.as_str()) {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

fn iter_course_id_candidates(course_id: &str, canvas_course_id: Option<&str>) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut add = |v: &str| {
        if !v.is_empty() && !candidates.contains(&v.to_string()) {
            candidates.push(v.to_string());
        }
    };

    if let Some(v) = canvas_course_id {
        add(v);
        if v.chars().all(|c| c.is_ascii_digit()) {
            let trimmed = v.trim_start_matches('0');
            if !trimmed.is_empty() {
                add(trimmed);
            }
        }
        let encoded = urlencoding::encode(v);
        if encoded != v {
            add(&encoded);
        }
    }

    add(course_id);
    if course_id.chars().all(|c| c.is_ascii_digit()) {
        let trimmed = course_id.trim_start_matches('0');
        if !trimmed.is_empty() {
            add(trimmed);
        }
    }
    let encoded = urlencoding::encode(course_id);
    if encoded != course_id {
        add(&encoded);
    }

    candidates
}

// ============================================================
// Default mode: courses.sjtu.edu.cn VOD API (OAuth)
// ============================================================

const OAUTH_HREF: &str = "https://courses.sjtu.edu.cn/app/vodvideo/vodVideoPlay.d2j";
const OAUTH_PATH: &str = "aHR0cHM6Ly9jb3Vyc2VzLnNqdHUuZWR1LmNuL2FwcC92b2R2aWRlby92b2RWaWRlb1BsYXkuZDJq";

const OAUTH_RANDOM_P1: &str = "oauth_ABCDE";
const OAUTH_RANDOM_P1_VAL: &str = "ABCDEFGH";
const OAUTH_RANDOM_P2: &str = "oauth_VWXYZ";
const OAUTH_RANDOM_P2_VAL: &str = "STUVWXYZ";

/// Extract OAuth consumer key from the VOD page.
pub async fn get_oauth_consumer_key(client: &reqwest::Client) -> Result<Option<String>> {
    let resp = client
        .get(OAUTH_HREF)
        .query(&[("ssoCheckToken", "ssoCheckToken"), ("refreshToken", ""), ("accessToken", ""), ("userId", "")])
        .send()
        .await
        .context("Failed to fetch VOD page")?;
    let body = resp.text().await.context("Failed to read VOD page")?;

    let document = Html::parse_document(&body);
    // The Python code looks for meta#xForSecName with attribute "vaule" (typo in the original)
    let meta_sel = Selector::parse("meta#xForSecName").unwrap();
    for meta in document.select(&meta_sel) {
        if let Some(val) = meta.value().attr("vaule").or_else(|| meta.value().attr("value")) {
            if let Ok(decoded) = BASE64.decode(val) {
                if let Ok(s) = String::from_utf8(decoded) {
                    return Ok(Some(s));
                }
            }
        }
    }
    Ok(None)
}

fn _random_uuid(len: usize) -> String {
    let chars = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..chars.len());
            chars.chars().nth(idx).unwrap()
        })
        .collect()
}

fn get_oauth_signature(course_id: &str, oauth_nonce: &str, oauth_consumer_key: &str) -> String {
    let source = format!(
        "/app/system/resource/vodVideo/getvideoinfos?id={}&oauth-consumer-key={}&oauth-nonce={}&oauth-path={}&{}={}&{}={}&playTypeHls=true",
        course_id, oauth_consumer_key, oauth_nonce, OAUTH_PATH,
        OAUTH_RANDOM_P1, OAUTH_RANDOM_P1_VAL,
        OAUTH_RANDOM_P2, OAUTH_RANDOM_P2_VAL,
    );
    let mut hasher = Md5::new();
    hasher.update(source.as_bytes());
    hex::encode(hasher.finalize())
}

/// Fetch enrolled subject IDs.
pub async fn get_subject_ids(client: &reqwest::Client) -> Result<Vec<(String, String)>> {
    let resp = client
        .get("https://courses.sjtu.edu.cn/app/system/course/subject/findSubjectVodList")
        .query(&[("pageIndex", "1"), ("pageSize", "128")])
        .header("accept", "application/json")
        .send()
        .await
        .context("Failed to fetch subject list")?;

    let body: serde_json::Value = resp.json().await
        .context("Failed to parse subject list")?;

    let mut ids = Vec::new();
    if let Some(list) = body.get("list").and_then(|v| v.as_array()) {
        for subj in list {
            if let (Some(subject_id), Some(tecl_id)) = (
                subj.get("subjectId").and_then(|v| v.as_str()),
                subj.get("teclId").and_then(|v| v.as_str()),
            ) {
                ids.push((subject_id.to_string(), tecl_id.to_string()));
            }
        }
    }
    Ok(ids)
}

/// Fetch course IDs for a subject.
pub async fn get_course_ids(
    client: &reqwest::Client,
    subject_id: &str,
    tecl_id: &str,
) -> Result<Option<Vec<String>>> {
    let resp = client
        .get("https://courses.sjtu.edu.cn/app/system/resource/vodVideo/getCourseListBySubject")
        .query(&[("orderField", "courTimes"), ("subjectId", subject_id), ("teclId", tecl_id)])
        .header("accept", "application/json")
        .send()
        .await
        .context("Failed to fetch course IDs")?;

    let body: serde_json::Value = resp.json().await
        .context("Failed to parse course IDs")?;

    let list = match body.get("list").and_then(|v| v.as_array()) {
        Some(l) => l,
        None => return Ok(None),
    };
    let first = match list.first() {
        Some(f) => f,
        None => return Ok(None),
    };
    let response_list = match first.get("responseVoList").and_then(|v| v.as_array()) {
        Some(l) => l,
        None => return Ok(None),
    };

    let ids: Vec<String> = response_list
        .iter()
        .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();

    Ok(Some(ids))
}

/// Fetch course/video details with OAuth headers.
pub async fn get_course(
    client: &reqwest::Client,
    course_id: &str,
    consumer_key: &str,
) -> Result<Option<serde_json::Value>> {
    let oauth_nonce = chrono::Utc::now().timestamp_millis().to_string();
    let signature = get_oauth_signature(course_id, &oauth_nonce, consumer_key);

    let mut form = HashMap::new();
    form.insert("playTypeHls", "true");
    form.insert("id", course_id);
    form.insert(OAUTH_RANDOM_P1, OAUTH_RANDOM_P1_VAL);
    form.insert(OAUTH_RANDOM_P2, OAUTH_RANDOM_P2_VAL);

    let resp = client
        .post("https://courses.sjtu.edu.cn/app/system/resource/vodVideo/getvideoinfos")
        .form(&form)
        .header("accept", "application/json")
        .header("oauth-consumer-key", consumer_key)
        .header("oauth-nonce", &oauth_nonce)
        .header("oauth-path", OAUTH_PATH)
        .header("oauth-signature", &signature)
        .send()
        .await
        .context("Failed to fetch course info")?;

    let mut body: serde_json::Value = resp.json().await
        .context("Failed to parse course info")?;

    // Remove loginUserId like the Python code does
    if let Some(obj) = body.as_object_mut() {
        obj.remove("loginUserId");
    }

    Ok(Some(body))
}

/// Top-level: fetch all enrolled courses via the default VOD API.
pub async fn get_all_courses(client: &reqwest::Client) -> Result<Vec<Course>> {
    let consumer_key = match get_oauth_consumer_key(client).await? {
        Some(k) => k,
        None => return Ok(Vec::new()),
    };

    let subjects = get_subject_ids(client).await?;
    let mut all_courses = Vec::new();

    for (subject_id, tecl_id) in subjects {
        let course_ids = match get_course_ids(client, &subject_id, &tecl_id).await? {
            Some(ids) => ids,
            None => continue,
        };

        let mut courses = Vec::new();
        for cid in course_ids {
            if let Ok(Some(course_json)) = get_course(client, &cid, &consumer_key).await {
                if let Some(course) = parse_default_course(&course_json, &subject_id) {
                    courses.push(course);
                }
            }
        }
        if !courses.is_empty() {
            all_courses.push(Course {
                id: String::new(),
                name: format!("Subject {}", subject_id),
                teacher: String::new(),
                subject_name: String::new(),
                videos: Vec::new(),
                // Use cdviList to populate
            });
            // Replace last entry with actual course data
            if let Some(last) = all_courses.last_mut() {
                last.videos = courses.into_iter().flat_map(|c| c.videos).collect();
            }
        }
    }

    Ok(all_courses)
}

fn parse_default_course(val: &serde_json::Value, _subject_id: &str) -> Option<Course> {
    let name = val.get("vodCourseName")
        .or_else(|| val.get("courseName"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let teacher = val.get("teacherName")
        .or_else(|| val.get("teacher"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let videos = if let Some(cdvi_list) = val.get("cdviList").and_then(|v| v.as_array()) {
        cdvi_list.iter().filter_map(parse_video_from_json).collect()
    } else {
        Vec::new()
    };

    Some(Course {
        id: val.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        name,
        teacher,
        subject_name: String::new(),
        videos,
    })
}

fn parse_video_from_json(val: &serde_json::Value) -> Option<VideoInfo> {
    let url = val.get("url")
        .or_else(|| val.get("downloadUrl"))
        .or_else(|| val.get("vodVideoUrl"))
        .and_then(|v| v.as_str())?
        .to_string();

    let name = val.get("vodVideoName")
        .or_else(|| val.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let id = val.get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let view_num = val.get("cdviViewNum")
        .or_else(|| val.get("viewNum"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let file_ext = val.get("fileExt")
        .and_then(|v| v.as_str())
        .unwrap_or("mp4")
        .to_string();

    Some(VideoInfo {
        id,
        name,
        url,
        view_num,
        is_recording: view_num == 0,
        file_ext,
    })
}

// ============================================================
// Course ID mode: v.sjtu.edu.cn OIDC/LTI3 flow (v2)
// ============================================================

/// Scrape the external tool ID from oc.sjtu.edu.cn course page.
pub async fn get_external_tool_id(client: &reqwest::Client, course_id: &str) -> String {
    let default = "8329".to_string();
    let url = format!("https://oc.sjtu.edu.cn/courses/{}", course_id);

    eprintln!("[DEBUG] get_external_tool_id: GET {}", url);
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[DEBUG] Failed to fetch course page: {}", e);
            return default;
        }
    };

    eprintln!("[DEBUG] get_external_tool_id: final URL = {}", resp.url());

    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to read course page: {}", e);
            return default;
        }
    };

    let document = Html::parse_document(&body);
    // Find div#main, then a link whose text starts with "课堂视频" and doesn't end with "旧版"
    let main_sel = match Selector::parse("div#main") {
        Ok(s) => s,
        Err(_) => return default,
    };
    let a_sel = match Selector::parse("a") {
        Ok(s) => s,
        Err(_) => return default,
    };

    if let Some(main_div) = document.select(&main_sel).next() {
        for a in main_div.select(&a_sel) {
            let text: String = a.text().collect();
            if text.starts_with("课堂视频") && !text.ends_with("旧版") {
                if let Some(href) = a.value().attr("href") {
                    if let Some(tool_id) = href.rsplit('/').next() {
                        if !tool_id.is_empty() {
                            return tool_id.to_string();
                        }
                    }
                }
            }
        }
    }

    default
}

/// Full OIDC/LTI3 authentication flow.
/// Returns (canvas_course_id, token_header_map).
pub async fn get_sub_cookies_v2(
    client: &reqwest::Client,
    course_id: &str,
) -> Result<(String, HashMap<String, String>)> {
    let external_tool_id = get_external_tool_id(client, course_id).await;
    eprintln!("[DEBUG] external_tool_id = {}", external_tool_id);

    // Step 1: GET external tool URL to get the launch form
    let ext_url = format!(
        "https://oc.sjtu.edu.cn/courses/{}/external_tools/{}",
        course_id, external_tool_id
    );
    eprintln!("[DEBUG] Step 1: GET {}", ext_url);
    let resp = client.get(&ext_url).send().await
        .context("Failed to visit external tool")?;
    eprintln!("[DEBUG] Step 1: final URL = {}", resp.url());
    let body = resp.text().await
        .context("Failed to read external tool page")?;

    // Step 2: Extract OIDC login initiation form
    let document = Html::parse_document(&body);
    let form_sel = Selector::parse("form").unwrap();
    let input_sel = Selector::parse("input").unwrap();

    let mut launch_action = String::new();
    let mut launch_data: HashMap<String, String> = HashMap::new();

    for form in document.select(&form_sel) {
        if let Some(action) = form.value().attr("action") {
            if action.contains("oidc/login_initiations") {
                launch_action = action.to_string();
                for input in form.select(&input_sel) {
                    if let (Some(name), Some(value)) =
                        (input.value().attr("name"), input.value().attr("value"))
                    {
                        launch_data.insert(name.to_string(), value.to_string());
                    }
                }
                break;
            }
        }
    }

    if launch_action.is_empty() {
        eprintln!("[DEBUG] Step 2: No OIDC form found. Page forms:");
        for form in document.select(&form_sel) {
            if let Some(action) = form.value().attr("action") {
                eprintln!("  form action = {}", action);
            }
        }
        // Dump first 500 chars of the page for debugging
        eprintln!("[DEBUG] Page preview: {}...", &body[..body.len().min(500)]);
        return Err(anyhow::anyhow!("未找到视频平台登录表单，可能是 Cookie 已失效，或课程页面结构已变化。"));
    }
    eprintln!("[DEBUG] Step 2: launch_action = {}, fields = {}", launch_action, launch_data.len());

    // Step 3: POST to OIDC login initiation (with cookies, allow redirects)
    eprintln!("[DEBUG] Step 3: POST to {}", launch_action);
    let resp2 = client
        .post(&launch_action)
        .form(&launch_data)
        .send()
        .await
        .context("Failed to POST OIDC login initiation")?;

    eprintln!("[DEBUG] Step 3: final URL = {}, status = {}", resp2.url(), resp2.status());
    let body2 = resp2.text().await
        .context("Failed to read OIDC response")?;

    // Step 4: Extract LTI3 auth form
    let document2 = Html::parse_document(&body2);
    let mut auth_action = String::new();
    let mut auth_data: HashMap<String, String> = HashMap::new();

    for form in document2.select(&form_sel) {
        if let Some(action) = form.value().attr("action") {
            if action.contains("lti3/lti3Auth/ivs") {
                auth_action = action.to_string();
                for input in form.select(&input_sel) {
                    if let (Some(name), Some(value)) =
                        (input.value().attr("name"), input.value().attr("value"))
                    {
                        auth_data.insert(name.to_string(), value.to_string());
                    }
                }
                break;
            }
        }
    }

    if auth_action.is_empty() {
        eprintln!("[DEBUG] Step 4: No LTI3 auth form found. Page forms:");
        for form in document2.select(&form_sel) {
            if let Some(action) = form.value().attr("action") {
                eprintln!("  form action = {}", action);
            }
        }
        eprintln!("[DEBUG] Page preview: {}...", &body2[..body2.len().min(500)]);
        return Err(anyhow::anyhow!("未找到 LTI 鉴权表单，可能是登录状态失效，或学校视频平台返回流程已变化。"));
    }
    eprintln!("[DEBUG] Step 4: auth_action = {}, fields = {}", auth_action, auth_data.len());

    // Step 5: POST to LTI3 auth (no redirect following, but WITH cookies)
    // Build a new client that shares the same cookie jar but doesn't follow redirects
    let _cookie_jar = Arc::new(Jar::default());
    // We can't extract the jar from the existing client, so instead we'll
    // use the existing client and manually handle the redirect.
    // The trick: reqwest with Policy::none() on a cloned client builder.
    // But we need cookies. The simplest approach: just POST and check if
    // the response is a redirect, then read the Location header.
    // Actually, reqwest doesn't let us switch redirect policy per-request.
    // Workaround: build a separate no-redirect client, but manually copy
    // cookies from the main client by reading the Cookie header it would send.

    // Get cookies for v.sjtu.edu.cn from the main client
    let _v_url = "https://v.sjtu.edu.cn".parse::<url::Url>().unwrap();

    let no_redirect_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .cookie_provider(Arc::new(Jar::default()))
        .build()?;

    // We need to extract cookies from the main client's jar for v.sjtu.edu.cn
    // Unfortunately we can't. Let's try a different approach: just use the
    // main client and parse the final URL after redirect.
    // Python uses allow_redirects=False but Python's requests also sends cookies.
    // Let's POST with the main client (which has cookies) but catch the redirect.
    // Actually, reqwest doesn't support per-request redirect policy.
    // The only option is a separate client. But we need to manually pass cookies.

    // Since we can't extract cookies from the jar, let's try just using the
    // main client. If it follows the redirect, we can check the final URL
    // which should contain the tokenId.
    eprintln!("[DEBUG] Step 5: POST to {} (will follow redirects)", auth_action);
    let resp3 = client
        .post(&auth_action)
        .form(&auth_data)
        .send()
        .await
        .context("Failed to POST LTI3 auth")?;

    let final_url3 = resp3.url().to_string();
    eprintln!("[DEBUG] Step 5: final URL = {}", final_url3);
    eprintln!("[DEBUG] Step 5: status = {}", resp3.status());

    // Parse params from the final URL (which may contain tokenId after redirect)
    let params_dict = parse_redirect_params(&final_url3);
    eprintln!("[DEBUG] Step 5: params_dict = {:?}", params_dict);

    // If we didn't get tokenId from the final URL, try reading the Location header
    // from a no-redirect request
    let token_id = if let Some(tid) = params_dict.get("tokenId").cloned() {
        tid
    } else {
        eprintln!("[DEBUG] Step 5: tokenId not in final URL, trying no-redirect approach...");
        // Fallback: use no-redirect client, but we need to pass cookies manually
        // We can't get them from the jar, so this may fail.
        // As a last resort, try the no-redirect client without cookies:
        let resp3b = no_redirect_client
            .post(&auth_action)
            .form(&auth_data)
            .send()
            .await
            .context("Failed to POST LTI3 auth (no-redirect)")?;

        let loc = resp3b.headers().get("location")
            .map(|h| h.to_str().unwrap_or(""))
            .unwrap_or("")
            .to_string();
        eprintln!("[DEBUG] Step 5b: Location = {}", loc);

        let params2 = parse_redirect_params(&loc);
        eprintln!("[DEBUG] Step 5b: params = {:?}", params2);
        params2.get("tokenId").cloned()
            .ok_or_else(|| anyhow::anyhow!("未能从视频平台跳转中解析 tokenId，final_url={}, params={:?}", final_url3, params_dict))?
    };

    let canvas_course_id = get_canvas_course_id(&params_dict, &[
        serde_json::Value::Object(launch_data.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect()),
        serde_json::Value::Object(auth_data.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect()),
    ]);
    eprintln!("[DEBUG] canvas_course_id from redirect = {:?}", canvas_course_id);

    // Step 6: Exchange tokenId for access token
    eprintln!("[DEBUG] Step 6: Exchanging token for tokenId = {}", token_id);
    let token_payload = exchange_token(client, &token_id).await?;
    eprintln!("[DEBUG] Step 6: token_payload = {:?}", token_payload);

    let access_token = token_payload.get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Access token not found in response"))?
        .to_string();

    // Use params.courId as canonical course id
    let access_params = token_payload.get("params")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let final_course_id = access_params.get("courId")
        .or_else(|| access_params.get("canvasCourseId"))
        .or_else(|| access_params.get("courseId"))
        .or_else(|| access_params.get("ltiCourseId"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or(canvas_course_id)
        .unwrap_or_default();

    eprintln!("[DEBUG] final_course_id = {}, access_token length = {}", final_course_id, access_token.len());

    let mut v_header = HashMap::new();
    v_header.insert("token".to_string(), access_token);

    Ok((final_course_id, v_header))
}

/// Exchange tokenId for access token (GET request, not POST).
async fn exchange_token(
    client: &reqwest::Client,
    token_id: &str,
) -> Result<serde_json::Value> {
    let resp = client
        .get("https://v.sjtu.edu.cn/jy-application-canvas-sjtu/lti3/getAccessTokenByTokenId")
        .query(&[("tokenId", token_id)])
        .send()
        .await
        .context("Failed to exchange token")?;

    let json: serde_json::Value = resp.json().await
        .context("Failed to parse token response")?;

    json.get("data").cloned()
        .ok_or_else(|| anyhow::anyhow!("data not found in token response"))
}

/// Fetch video detail for a single video.
pub async fn get_real_canvas_video_single_v2(
    client: &reqwest::Client,
    video_id: &str,
    v_header: &HashMap<String, String>,
) -> Result<serde_json::Value> {
    let mut form = HashMap::new();
    form.insert("playTypeHls", "true");
    form.insert("id", video_id);
    form.insert("isAudit", "true");

    eprintln!("[DEBUG] get_real_canvas_video_single_v2: videoId = {}", video_id);

    let resp = client
        .post("https://v.sjtu.edu.cn/jy-application-canvas-sjtu/directOnDemandPlay/getVodVideoInfos")
        .form(&form)
        .headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert("Referer", "https://v.sjtu.edu.cn/".parse().unwrap());
            if let Some(token) = v_header.get("token") {
                h.insert("token", token.parse().unwrap());
            }
            h
        })
        .send()
        .await
        .context("Failed to fetch video detail")?;

    eprintln!("[DEBUG] get_real_canvas_video_single_v2: status = {}", resp.status());
    let json: serde_json::Value = resp.json().await
        .context("Failed to parse video detail")?;

    eprintln!("[DEBUG] get_real_canvas_video_single_v2: response keys = {:?}", json.as_object().map(|o| o.keys().collect::<Vec<_>>()));
    let detail = extract_detail(&json)
        .ok_or_else(|| anyhow::anyhow!("视频详情接口未返回可识别的数据。返回字段: {:?}", json.as_object().map(|o| o.keys().collect::<Vec<_>>())))?;

    Ok(detail.clone())
}

/// Fetch video list, trying multiple body formats.
async fn request_video_list(
    client: &reqwest::Client,
    v_header: &HashMap<String, String>,
    course_id: &str,
    canvas_course_id: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let candidates = iter_course_id_candidates(course_id, canvas_course_id);
    eprintln!("[DEBUG] request_video_list: candidate IDs = {:?}", candidates);

    let mut bodies = Vec::new();
    for cid in &candidates {
        bodies.push(serde_json::json!({"canvasCourseId": cid}));
        bodies.push(serde_json::json!({"canvasCourseId": cid, "pageIndex": 1, "pageSize": 1000}));
        bodies.push(serde_json::json!({"courId": cid}));
        bodies.push(serde_json::json!({"courId": cid, "pageIndex": 1, "pageSize": 1000}));
        bodies.push(serde_json::json!({"courseId": cid}));
        bodies.push(serde_json::json!({"ltiCourseId": cid}));
    }

    let mut last_payload = serde_json::Value::Null;

    for body in bodies {
        let resp = client
            .post("https://v.sjtu.edu.cn/jy-application-canvas-sjtu/directOnDemandPlay/findVodVideoList")
            .json(&body)
            .headers({
                let mut h = reqwest::header::HeaderMap::new();
                h.insert("Referer", "https://v.sjtu.edu.cn/".parse().unwrap());
                if let Some(token) = v_header.get("token") {
                    h.insert("token", token.parse().unwrap());
                }
                h
            })
            .send()
            .await;

        if let Ok(r) = resp {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                eprintln!("[DEBUG] request_video_list body={:?} -> code={:?}, data_type={:?}",
                    body,
                    json.get("code"),
                    json.get("data").map(|d| if d.is_array() { "array" } else if d.is_object() { "object" } else if d.is_null() { "null" } else { "other" })
                );
                last_payload = json.clone();
                if let Some(records) = extract_records(&json) {
                    eprintln!("[DEBUG] request_video_list: found {} records", records.len());
                    return Ok(records.into_iter().cloned().collect());
                }
            }
        }
    }

    let summary = if last_payload.is_object() {
        format!(
            "code={:?}, message={:?}, data_type={:?}",
            last_payload.get("code"),
            last_payload.get("message").or_else(|| last_payload.get("msg")),
            last_payload.get("data").map(|v| match v { serde_json::Value::Null => "null", serde_json::Value::Array(_) => "array", serde_json::Value::Object(_) => "object", _ => "other" }),
        )
    } else {
        String::new()
    };

    Err(anyhow::anyhow!(
        "视频列表接口未返回可识别的数据。尝试的课程ID: {:?}，最后一次返回: {}",
        candidates, summary
    ))
}

/// Top-level: fetch all courses/videos via the v2 OIDC/LTI3 flow.
pub async fn get_real_canvas_videos_v2(
    client: &reqwest::Client,
    course_id: &str,
) -> Result<Vec<Course>> {
    let (canvas_course_id, v_header) = get_sub_cookies_v2(client, course_id).await?;

    let records = request_video_list(
        client,
        &v_header,
        course_id,
        Some(&canvas_course_id),
    ).await?;

    eprintln!("[DEBUG] get_real_canvas_videos_v2: got {} records", records.len());

    // Each record represents one lecture. For each, fetch the detail to get
    // the actual download URL and course metadata (subjName, userName, courName).
    // One detail call → one Course entry with multiple videos in videoPlayResponseVoList.
    let mut courses = Vec::new();

    for record in &records {
        let video_id = record.get("videoId")
            .or_else(|| record.get("id"))
            .map(|v| {
                if v.is_string() { v.as_str().unwrap_or("").to_string() }
                else { v.to_string() }
            })
            .unwrap_or_default();

        if video_id.is_empty() {
            eprintln!("[DEBUG] Skipping record with no videoId");
            continue;
        }

        // Fetch full video detail
        let detail = match get_real_canvas_video_single_v2(client, &video_id, &v_header).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[DEBUG] Failed to get detail for videoId={}: {}", video_id, e);
                continue;
            }
        };

        // Extract course-level metadata from the detail response
        let subj_name = detail.get("subjName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let user_name = detail.get("userName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cour_name = detail.get("courName")
            .or_else(|| detail.get("courseName"))
            .and_then(|v| v.as_str())
            .unwrap_or("Course")
            .to_string();

        // Extract videos from videoPlayResponseVoList
        let videos = if let Some(vlist) = detail.get("videoPlayResponseVoList").and_then(|v| v.as_array()) {
            vlist.iter().filter_map(|v| parse_v2_video_detail(v)).collect::<Vec<_>>()
        } else {
            eprintln!("[DEBUG] Detail for videoId={} has no videoPlayResponseVoList, keys: {:?}",
                video_id, detail.as_object().map(|o| o.keys().collect::<Vec<_>>()));
            Vec::new()
        };

        if !videos.is_empty() {
            courses.push(Course {
                id: canvas_course_id.clone(),
                name: cour_name,
                teacher: user_name,
                subject_name: subj_name,
                videos,
            });
        }
    }

    eprintln!("[DEBUG] Built {} courses from {} records", courses.len(), records.len());
    Ok(courses)
}

/// Parse a single video entry from the videoPlayResponseVoList array
/// in the detail response. The actual download URL is in "rtmpUrlHdv".
fn parse_v2_video_detail(val: &serde_json::Value) -> Option<VideoInfo> {
    let url = val.get("rtmpUrlHdv")
        .or_else(|| val.get("downloadUrl"))
        .or_else(|| val.get("url"))
        .and_then(|v| v.as_str())?
        .to_string();

    let name = val.get("vodVideoName")
        .or_else(|| val.get("videoName"))
        .or_else(|| val.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let id = val.get("id")
        .or_else(|| val.get("vodVideoId"))
        .or_else(|| val.get("videoId"))
        .and_then(|v| {
            if v.is_string() { v.as_str().map(String::from) }
            else if v.is_number() { Some(v.to_string()) }
            else { None }
        })
        .unwrap_or_default();

    let view_num = val.get("cdviViewNum")
        .or_else(|| val.get("viewNum"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Determine file extension from the URL
    let file_ext = url.rsplit('.').next()
        .unwrap_or("mp4")
        .split('?').next()
        .unwrap_or("mp4")
        .to_string();

    Some(VideoInfo {
        id,
        name,
        url,
        view_num,
        is_recording: view_num == 0,
        file_ext,
    })
}

fn _parse_v2_video_from_record(val: &serde_json::Value) -> Option<VideoInfo> {
    // Field names from the actual v2 API response:
    // videoId, videoName, userName, classroomName, courId, courseBeginTime, courseEndTime, ...
    let name = val.get("videoName")
        .or_else(|| val.get("vodVideoName"))
        .or_else(|| val.get("name"))
        .and_then(|v| v.as_str())?
        .to_string();

    let view_num = val.get("cdviViewNum")
        .or_else(|| val.get("viewNum"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let file_ext = val.get("fileExt")
        .or_else(|| val.get("ext"))
        .and_then(|v| v.as_str())
        .unwrap_or("mp4")
        .to_string();

    let id = val.get("videoId")
        .or_else(|| val.get("id"))
        .or_else(|| val.get("vodVideoId"))
        .and_then(|v| {
            // videoId might be a number, convert to string
            if v.is_string() { v.as_str().map(String::from) }
            else if v.is_number() { Some(v.to_string()) }
            else { None }
        })
        .unwrap_or_default();

    // URL not available in list response, will be filled by detail call
    // URL not available in list response, will be filled by detail call
    let _url = String::new();

    Some(VideoInfo {
        id,
        name,
        url: String::new(), // URL comes from detail call
        view_num,
        is_recording: view_num == 0,
        file_ext,
    })
}
