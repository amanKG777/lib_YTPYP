use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const IOS_PLAYER_URL: &str = "https://youtubei.googleapis.com/youtubei/v1/player?prettyPrint=false";
const ANDROID_VR_PLAYER_URL: &str = "https://www.youtube.com/youtubei/v1/player?key=AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const SEARCH_URL: &str = "https://www.youtube.com/youtubei/v1/search?key=AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub duration_seconds: u32,
    pub stream_url: String,
    pub mime_type: String,
}

#[derive(Debug, Clone)]
pub struct SearchItem {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub duration_str: String,
    pub duration_seconds: u32,
}

struct CacheEntry {
    info: VideoInfo,
    fetched_at: Instant,
}

static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();

fn get_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn extract_video_id(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    // Direct 11-char ID
    if s.len() == 11 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Some(s.to_string());
    }

    // Scheme handling (youtube:// or yt://)
    if let Some(rest) = s.strip_prefix("youtube://") {
        let clean = rest.trim_start_matches('/');
        let id = clean.split('?').next().unwrap_or(clean).split('&').next().unwrap_or(clean);
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    if let Some(rest) = s.strip_prefix("yt://") {
        let clean = rest.trim_start_matches('/');
        let id = clean.split('?').next().unwrap_or(clean).split('&').next().unwrap_or(clean);
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }

    // Standard URLs
    if s.contains("youtu.be/") {
        if let Some(part) = s.split("youtu.be/").nth(1) {
            let id = part.split('?').next().unwrap_or(part).split('&').next().unwrap_or(part).split('/').next().unwrap_or(part);
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }

    if s.contains("watch?v=") {
        if let Some(part) = s.split("watch?v=").nth(1) {
            let id = part.split('&').next().unwrap_or(part).split('#').next().unwrap_or(part);
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }

    if s.contains("/shorts/") {
        if let Some(part) = s.split("/shorts/").nth(1) {
            let id = part.split('?').next().unwrap_or(part).split('&').next().unwrap_or(part).split('/').next().unwrap_or(part);
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }

    if s.contains("/embed/") {
        if let Some(part) = s.split("/embed/").nth(1) {
            let id = part.split('?').next().unwrap_or(part).split('&').next().unwrap_or(part).split('/').next().unwrap_or(part);
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }

    None
}

pub fn get_video_info(video_id: &str) -> Result<VideoInfo, String> {
    // Check in-memory cache (valid for 3 hours, since googlevideo URLs typically expire in 6 hours)
    {
        let cache = get_cache().lock().unwrap();
        if let Some(entry) = cache.get(video_id) {
            if entry.fetched_at.elapsed() < Duration::from_secs(3 * 3600) {
                return Ok(entry.info.clone());
            }
        }
    }

    // 1. Try Primary: VISIONOS client with session visitorData and CPN (FukeX / NewPipe method)
    match fetch_via_visionos(video_id) {
        Ok(info) => {
            let mut cache = get_cache().lock().unwrap();
            cache.insert(
                video_id.to_string(),
                CacheEntry {
                    info: info.clone(),
                    fetched_at: Instant::now(),
                },
            );
            return Ok(info);
        }
        Err(e) => {
            eprintln!("[ytpipe_core] VISIONOS client failed for {}: {}", video_id, e);
        }
    }

    // 2. Fallback: IOS client
    match fetch_via_ios(video_id) {
        Ok(info) => {
            let mut cache = get_cache().lock().unwrap();
            cache.insert(
                video_id.to_string(),
                CacheEntry {
                    info: info.clone(),
                    fetched_at: Instant::now(),
                },
            );
            return Ok(info);
        }
        Err(e) => {
            eprintln!("[ytpipe_core] IOS client failed for {}: {}", video_id, e);
        }
    }

    // 3. Fallback: ANDROID_VR client
    match fetch_via_android_vr(video_id) {
        Ok(info) => {
            let mut cache = get_cache().lock().unwrap();
            cache.insert(
                video_id.to_string(),
                CacheEntry {
                    info: info.clone(),
                    fetched_at: Instant::now(),
                },
            );
            return Ok(info);
        }
        Err(e) => {
            eprintln!("[ytpipe_core] ANDROID_VR fallback failed for {}: {}", video_id, e);
            Err(format!("Failed to retrieve playable audio for video {}: {}", video_id, e))
        }
    }
}

fn random_string(len: usize) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let mut state = (seed ^ 0x5DEECE66D) as u64;
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let idx = ((state >> 33) as usize) % ALPHABET.len();
        out.push(ALPHABET[idx] as char);
    }
    out
}

static VISITOR_DATA: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();

fn get_visitor_data() -> Result<String, String> {
    let mutex = VISITOR_DATA.get_or_init(|| Mutex::new(None));
    {
        let guard = mutex.lock().unwrap();
        if let Some((ref vd, fetched_at)) = *guard {
            if fetched_at.elapsed() < Duration::from_secs(24 * 3600) {
                return Ok(vd.clone());
            }
        }
    }

    let payload = serde_json::json!({
        "context": {
            "client": {
                "clientName": "VISIONOS",
                "clientVersion": "1.02",
                "clientScreen": "WATCH",
                "platform": "MOBILE",
                "deviceMake": "Apple",
                "deviceModel": "RealityDevice14,1",
                "osName": "visionOS",
                "osVersion": "25.6.0.23O471",
                "hl": "en-GB",
                "gl": "GB",
                "utcOffsetMinutes": 0
            }
        }
    });

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();

    let resp = agent
        .post("https://www.youtube.com/youtubei/v1/visitor_id?prettyPrint=false")
        .set("Content-Type", "application/json")
        .set("User-Agent", "com.google.visionos.youtube/1.02(RealityDevice14,1; U; CPU visionOS 25_6_0 like Mac OS X; GB)")
        .send_json(payload)
        .map_err(|e| format!("Failed to fetch visitor_id: {}", e))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("Failed to parse visitor_id response: {}", e))?;

    let vd = json["responseContext"]["visitorData"]
        .as_str()
        .ok_or_else(|| "visitorData missing in response".to_string())?
        .to_string();

    let mut guard = mutex.lock().unwrap();
    *guard = Some((vd.clone(), Instant::now()));
    Ok(vd)
}

fn fetch_via_visionos(video_id: &str) -> Result<VideoInfo, String> {
    let visitor_data = get_visitor_data().unwrap_or_default();
    let t_param = random_string(12);
    let cpn = random_string(16);

    let mut client = serde_json::json!({
        "clientName": "VISIONOS",
        "clientVersion": "1.02",
        "clientScreen": "WATCH",
        "platform": "MOBILE",
        "deviceMake": "Apple",
        "deviceModel": "RealityDevice14,1",
        "osName": "visionOS",
        "osVersion": "25.6.0.23O471",
        "hl": "en-GB",
        "gl": "GB",
        "utcOffsetMinutes": 0
    });

    if !visitor_data.is_empty() {
        client["visitorData"] = serde_json::Value::String(visitor_data);
    }

    let payload = serde_json::json!({
        "context": {
            "client": client,
            "request": {
                "internalExperimentFlags": [],
                "useSsl": true
            },
            "user": {
                "lockedSafetyMode": false
            }
        },
        "videoId": video_id,
        "cpn": cpn,
        "contentCheckOk": true,
        "racyCheckOk": true
    });

    let url = format!(
        "https://youtubei.googleapis.com/youtubei/v1/player?prettyPrint=false&t={}&id={}",
        t_param, video_id
    );

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .build();

    let resp = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .set("User-Agent", "com.google.visionos.youtube/1.02(RealityDevice14,1; U; CPU visionOS 25_6_0 like Mac OS X; GB)")
        .set("X-Goog-Api-Format-Version", "2")
        .send_json(payload)
        .map_err(|e| format!("VISIONOS player request failed: {}", e))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("Failed to parse VISIONOS response JSON: {}", e))?;

    parse_player_response(video_id, &cpn, &json)
}

fn fetch_via_ios(video_id: &str) -> Result<VideoInfo, String> {
    let cpn = random_string(16);
    let payload = serde_json::json!({
        "context": {
            "client": {
                "clientName": "IOS",
                "clientVersion": "21.03.2",
                "deviceMake": "Apple",
                "deviceModel": "iPhone16,2",
                "osName": "iOS",
                "osVersion": "18.7.2.22H124",
                "hl": "en",
                "gl": "US"
            }
        },
        "videoId": video_id,
        "cpn": cpn
    });

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .build();

    let resp = agent
        .post(IOS_PLAYER_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", "com.google.ios.youtube/21.03.2 (iPhone16,2; U; CPU iOS 18_7_2 like Mac OS X; en_US)")
        .set("X-Goog-Api-Format-Version", "2")
        .send_json(payload)
        .map_err(|e| format!("IOS HTTP request failed: {}", e))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("Failed to parse response JSON: {}", e))?;

    parse_player_response(video_id, &cpn, &json)
}

fn fetch_via_android_vr(video_id: &str) -> Result<VideoInfo, String> {
    let payload = serde_json::json!({
        "context": {
            "client": {
                "clientName": "ANDROID_VR",
                "clientVersion": "1.56.21",
                "hl": "en",
                "gl": "US"
            }
        },
        "videoId": video_id
    });

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .build();

    let resp = agent
        .post(ANDROID_VR_PLAYER_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .send_json(payload)
        .map_err(|e| format!("ANDROID_VR HTTP request failed: {}", e))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("Failed to parse response JSON: {}", e))?;

    parse_player_response(video_id, "", &json)
}

fn parse_player_response(video_id: &str, cpn: &str, json: &serde_json::Value) -> Result<VideoInfo, String> {
    let status = json["playabilityStatus"]["status"]
        .as_str()
        .unwrap_or("UNKNOWN");

    if status != "OK" {
        let reason = json["playabilityStatus"]["reason"]
            .as_str()
            .unwrap_or("Video unplayable");
        return Err(format!("Playability status: {} ({})", status, reason));
    }

    let details = &json["videoDetails"];
    let title = details["title"].as_str().unwrap_or("Unknown Title").to_string();
    let channel = details["author"].as_str().unwrap_or("Unknown Channel").to_string();
    let duration_sec = details["lengthSeconds"]
        .as_str()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    let empty_vec = Vec::new();
    let adaptive = json["streamingData"]["adaptiveFormats"]
        .as_array()
        .unwrap_or(&empty_vec);

    // Pick best audio format:
    // Prioritize high-bitrate Opus (itag 251 ~160-174kbps) or AAC (itag 140 ~128kbps)
    let mut selected_format: Option<&serde_json::Value> = None;
    let mut best_bitrate = 0u64;

    for f in adaptive {
        let mime = f["mimeType"].as_str().unwrap_or("");
        if !mime.contains("audio") {
            continue;
        }
        if f["url"].as_str().is_none() {
            continue;
        }

        let bitrate = f["bitrate"].as_u64().unwrap_or(0);
        if selected_format.is_none() || bitrate > best_bitrate {
            selected_format = Some(f);
            best_bitrate = bitrate;
        }
    }

    let fmt = selected_format.ok_or_else(|| "No direct playable audio streams found in response".to_string())?;
    let mut raw_url = fmt["url"].as_str().unwrap().to_string();
    let mime_type = fmt["mimeType"].as_str().unwrap_or("audio/webm").to_string();

    // Append client playback nonce (cpn) to bypass YouTube CDN chunk throttling
    if !cpn.is_empty() && !raw_url.contains("&cpn=") && !raw_url.contains("?cpn=") {
        if raw_url.contains('?') {
            raw_url.push_str(&format!("&cpn={}", cpn));
        } else {
            raw_url.push_str(&format!("?cpn={}", cpn));
        }
    }

    // Append extension query parameter so foobar2000's input manager instantly recognizes the format
    if mime_type.contains("webm") || mime_type.contains("opus") {
        if !raw_url.contains("&file=") {
            raw_url.push_str("&file=audio.webm");
        }
    } else if mime_type.contains("mp4") || mime_type.contains("m4a") || mime_type.contains("aac") {
        if !raw_url.contains("&file=") {
            raw_url.push_str("&file=audio.m4a");
        }
    }

    Ok(VideoInfo {
        id: video_id.to_string(),
        title,
        channel,
        duration_seconds: duration_sec,
        stream_url: raw_url,
        mime_type,
    })
}

pub fn search(query: &str, max_results: usize) -> Result<Vec<SearchItem>, String> {
    let payload = serde_json::json!({
        "context": {
            "client": {
                "clientName": "WEB",
                "clientVersion": "2.20240101.01.00",
                "hl": "en",
                "gl": "US"
            }
        },
        "query": query
    });

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();

    let resp = agent
        .post(SEARCH_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .send_json(payload)
        .map_err(|e| format!("Search HTTP request failed: {}", e))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("Failed to parse search response JSON: {}", e))?;

    let mut results = Vec::new();

    let sections = json["contents"]["twoColumnSearchResultsRenderer"]["primaryContents"]["sectionListRenderer"]["contents"]
        .as_array();

    if let Some(sections) = sections {
        for section in sections {
            if let Some(items) = section["itemSectionRenderer"]["contents"].as_array() {
                for item in items {
                    if let Some(vr) = item.get("videoRenderer") {
                        let id = vr["videoId"].as_str().unwrap_or("").to_string();
                        if id.is_empty() {
                            continue;
                        }

                        let title = extract_runs_text(&vr["title"]["runs"]);
                        let channel = extract_runs_text(&vr["ownerText"]["runs"]);
                        let duration_str = vr["lengthText"]["simpleText"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        let duration_seconds = parse_duration_seconds(&duration_str);

                        results.push(SearchItem {
                            id,
                            title,
                            channel,
                            duration_str,
                            duration_seconds,
                        });

                        if results.len() >= max_results {
                            return Ok(results);
                        }
                    }
                }
            }
        }
    }

    Ok(results)
}

fn extract_runs_text(runs: &serde_json::Value) -> String {
    if let Some(arr) = runs.as_array() {
        let mut text = String::new();
        for r in arr {
            if let Some(t) = r["text"].as_str() {
                text.push_str(t);
            }
        }
        text
    } else {
        String::new()
    }
}

fn parse_duration_seconds(dur_str: &str) -> u32 {
    let parts: Vec<&str> = dur_str.split(':').collect();
    let mut secs = 0u32;
    for part in parts {
        if let Ok(val) = part.parse::<u32>() {
            secs = secs * 60 + val;
        }
    }
    secs
}

static STREAM_AGENT: OnceLock<ureq::Agent> = OnceLock::new();

fn get_stream_agent() -> &'static ureq::Agent {
    STREAM_AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(12))
            .build()
    })
}

pub fn fetch_range(url: &str, start: u64, end: u64) -> Result<(Vec<u8>, u64), String> {
    use std::io::Read;

    let range_header = format!("bytes={}-{}", start, end);
    let agent = get_stream_agent();

    let resp = agent
        .get(url)
        .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .set("Range", &range_header)
        .call()
        .map_err(|e| format!("HTTP Range request failed: {}", e))?;

    let mut total_size = 0u64;
    if let Some(cr) = resp.header("Content-Range") {
        if let Some(slash_pos) = cr.rfind('/') {
            let total_str = &cr[slash_pos + 1..];
            total_size = total_str.trim().parse::<u64>().unwrap_or(0);
        }
    }

    let mut buf = Vec::new();
    let mut reader = resp.into_reader();
    reader
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read stream response bytes: {}", e))?;

    Ok((buf, total_size))
}
