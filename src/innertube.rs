use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const API_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const PLAYER_URL: &str = "https://www.youtube.com/youtubei/v1/player?key=AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
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
        .timeout(Duration::from_secs(15))
        .build();

    let resp = agent
        .post(PLAYER_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .send_json(payload)
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("Failed to parse response JSON: {}", e))?;

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
    // 1. itag 140 (m4a aac ~128kbps) or itag 251 (webm opus ~160kbps)
    // 2. Highest bitrate audio with direct url
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

        let itag = f["itag"].as_u64().unwrap_or(0);
        let bitrate = f["bitrate"].as_u64().unwrap_or(0);

        // Preference for itag 140 (AAC) or itag 251 (Opus)
        if itag == 140 {
            selected_format = Some(f);
            break;
        } else if itag == 251 && (selected_format.is_none() || best_bitrate < bitrate) {
            selected_format = Some(f);
            best_bitrate = bitrate;
        } else if selected_format.is_none() || bitrate > best_bitrate {
            selected_format = Some(f);
            best_bitrate = bitrate;
        }
    }

    let fmt = selected_format.ok_or_else(|| "No direct playable audio streams found in response".to_string())?;
    let stream_url = fmt["url"].as_str().unwrap().to_string();
    let mime_type = fmt["mimeType"].as_str().unwrap_or("audio/mp4").to_string();

    let info = VideoInfo {
        id: video_id.to_string(),
        title,
        channel,
        duration_seconds: duration_sec,
        stream_url,
        mime_type,
    };

    // Store in cache
    {
        let mut cache = get_cache().lock().unwrap();
        cache.insert(
            video_id.to_string(),
            CacheEntry {
                info: info.clone(),
                fetched_at: Instant::now(),
            },
        );
    }

    Ok(info)
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
