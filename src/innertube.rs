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

    // 1. Try Primary: IOS client (returns unthrottled direct audio URLs without signature cipher)
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

    // 2. Fallback: ANDROID_VR client
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

fn fetch_via_ios(video_id: &str) -> Result<VideoInfo, String> {
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
        "videoId": video_id
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

    parse_player_response(video_id, &json)
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

    parse_player_response(video_id, &json)
}

fn parse_player_response(video_id: &str, json: &serde_json::Value) -> Result<VideoInfo, String> {
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
        .set("User-Agent", "com.google.ios.youtube/21.03.2 (iPhone16,2; U; CPU iOS 18_7_2 like Mac OS X; en_US)")
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
