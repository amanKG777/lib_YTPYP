use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IOS_PLAYER_URL: &str = "https://youtubei.googleapis.com/youtubei/v1/player?prettyPrint=false";
const ANDROID_VR_PLAYER_URL: &str = "https://www.youtube.com/youtubei/v1/player?key=AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";

const VISIONOS_UA: &str = "com.google.visionos.youtube/1.02(RealityDevice14,1; U; CPU visionOS 25_6_0 like Mac OS X; GB)";

// Never trust a cached stream URL longer than this, even if `expire=` says otherwise.
const MAX_CACHE_AGE: Duration = Duration::from_secs(3 * 3600);
// Treat a URL as expired this long before its `expire=` timestamp.
const EXPIRY_MARGIN_SECS: u64 = 10 * 60;
const MAX_CACHE_ENTRIES: usize = 256;

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

#[derive(Debug)]
pub enum FetchError {
    /// The stream URL is no longer valid (expired / forbidden). Caller should re-resolve.
    Expired(String),
    Other(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Expired(s) | FetchError::Other(s) => f.write_str(s),
        }
    }
}

struct CacheEntry {
    info: VideoInfo,
    fetched_at: Instant,
    expires_unix: Option<u64>,
}

impl CacheEntry {
    fn is_fresh(&self) -> bool {
        if self.fetched_at.elapsed() >= MAX_CACHE_AGE {
            return false;
        }
        match self.expires_unix {
            Some(exp) => unix_now() + EXPIRY_MARGIN_SECS < exp,
            None => true,
        }
    }
}

static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();

/// Lock a mutex, recovering from poisoning instead of panicking (panics must never cross the C ABI).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn get_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let query = url.split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then_some(v)
    })
}

fn is_valid_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Take the leading path segment (up to `?`, `&`, `#`, `/`) and validate it as a video ID.
fn leading_id(s: &str) -> Option<String> {
    let end = s.find(['?', '&', '#', '/']).unwrap_or(s.len());
    let id = &s[..end];
    is_valid_video_id(id).then(|| id.to_string())
}

pub fn extract_video_id(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    if is_valid_video_id(s) {
        return Some(s.to_string());
    }

    for scheme in ["youtube://", "yt://"] {
        if s.len() >= scheme.len() && s[..scheme.len()].eq_ignore_ascii_case(scheme) {
            return leading_id(s[scheme.len()..].trim_start_matches('/'));
        }
    }

    if let Some(v) = query_param(s, "v") {
        if is_valid_video_id(v) {
            return Some(v.to_string());
        }
    }

    for marker in ["youtu.be/", "/shorts/", "/embed/", "/live/", "/v/", "/e/"] {
        if let Some(idx) = s.find(marker) {
            if let Some(id) = leading_id(&s[idx + marker.len()..]) {
                return Some(id);
            }
        }
    }

    None
}

pub fn invalidate(video_id: &str) {
    lock(get_cache()).remove(video_id);
}

fn cache_insert(info: &VideoInfo) {
    let expires_unix = query_param(&info.stream_url, "expire").and_then(|e| e.parse::<u64>().ok());
    let mut cache = lock(get_cache());
    cache.retain(|_, e| e.is_fresh());
    if cache.len() >= MAX_CACHE_ENTRIES {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, e)| e.fetched_at)
            .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(
        info.id.clone(),
        CacheEntry {
            info: info.clone(),
            fetched_at: Instant::now(),
            expires_unix,
        },
    );
}

pub fn get_video_info(video_id: &str) -> Result<VideoInfo, String> {
    {
        let cache = lock(get_cache());
        if let Some(entry) = cache.get(video_id) {
            if entry.is_fresh() {
                return Ok(entry.info.clone());
            }
        }
    }

    let clients: [(&str, fn(&str) -> Result<VideoInfo, String>); 3] = [
        ("VISIONOS", fetch_via_visionos),
        ("IOS", fetch_via_ios),
        ("ANDROID_VR", fetch_via_android_vr),
    ];

    let mut errors = Vec::new();
    for (name, fetch) in clients {
        match fetch(video_id) {
            Ok(info) => {
                cache_insert(&info);
                return Ok(info);
            }
            Err(e) => {
                eprintln!("[ytpipe_core] {} client failed for {}: {}", name, video_id, e);
                errors.push(format!("{}: {}", name, e));
            }
        }
    }

    Err(format!(
        "Failed to retrieve playable audio for video {} ({})",
        video_id,
        errors.join("; ")
    ))
}

fn random_string(len: usize) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // splitmix64-style mixing so consecutive calls produce unrelated strings
    let mut state = nanos ^ n.wrapping_mul(0x9E3779B97F4A7C15);
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        out.push(ALPHABET[(z % ALPHABET.len() as u64) as usize] as char);
    }
    out
}

fn api_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(8))
            .timeout(Duration::from_secs(15))
            .build()
    })
}

static VISITOR_DATA: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();

fn visionos_client_context() -> serde_json::Value {
    serde_json::json!({
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
    })
}

fn get_visitor_data() -> Result<String, String> {
    let mutex = VISITOR_DATA.get_or_init(|| Mutex::new(None));
    {
        let guard = lock(mutex);
        if let Some((ref vd, fetched_at)) = *guard {
            if fetched_at.elapsed() < Duration::from_secs(24 * 3600) {
                return Ok(vd.clone());
            }
        }
    }

    let payload = serde_json::json!({ "context": { "client": visionos_client_context() } });

    let resp = api_agent()
        .post("https://www.youtube.com/youtubei/v1/visitor_id?prettyPrint=false")
        .set("Content-Type", "application/json")
        .set("User-Agent", VISIONOS_UA)
        .send_json(payload)
        .map_err(|e| format!("Failed to fetch visitor_id: {}", e))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("Failed to parse visitor_id response: {}", e))?;

    let vd = json["responseContext"]["visitorData"]
        .as_str()
        .ok_or_else(|| "visitorData missing in response".to_string())?
        .to_string();

    *lock(mutex) = Some((vd.clone(), Instant::now()));
    Ok(vd)
}

fn fetch_via_visionos(video_id: &str) -> Result<VideoInfo, String> {
    let visitor_data = get_visitor_data().unwrap_or_default();
    let t_param = random_string(12);
    let cpn = random_string(16);

    let mut client = visionos_client_context();
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

    let resp = api_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set("User-Agent", VISIONOS_UA)
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
        "cpn": cpn,
        "contentCheckOk": true,
        "racyCheckOk": true
    });

    let resp = api_agent()
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
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true
    });

    let resp = api_agent()
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

/// Ranking key for an audio format; higher is better.
/// Original-language track beats auto-dubbed ones, non-DRC beats DRC, then bitrate.
fn format_rank(f: &serde_json::Value) -> (bool, bool, u64) {
    let track = &f["audioTrack"];
    let is_default = if track.is_object() {
        track["audioIsDefault"].as_bool().unwrap_or(false)
            || track["displayName"]
                .as_str()
                .map(|n| n.to_ascii_lowercase().contains("original"))
                .unwrap_or(false)
    } else {
        true
    };
    let is_drc = f["isDrc"].as_bool().unwrap_or(false);
    let bitrate = f["bitrate"].as_u64().unwrap_or(0);
    (is_default, !is_drc, bitrate)
}

fn parse_player_response(video_id: &str, cpn: &str, json: &serde_json::Value) -> Result<VideoInfo, String> {
    let status = json["playabilityStatus"]["status"].as_str().unwrap_or("UNKNOWN");

    if status != "OK" {
        let reason = json["playabilityStatus"]["reason"]
            .as_str()
            .unwrap_or("Video unplayable");
        return Err(format!("Playability status: {} ({})", status, reason));
    }

    let details = &json["videoDetails"];
    if let Some(returned_id) = details["videoId"].as_str() {
        if returned_id != video_id {
            return Err(format!("Player returned a different video ({})", returned_id));
        }
    }
    let title = details["title"].as_str().unwrap_or("Unknown Title").to_string();
    let channel = details["author"].as_str().unwrap_or("Unknown Channel").to_string();
    let duration_sec = details["lengthSeconds"]
        .as_str()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    let fmt = json["streamingData"]["adaptiveFormats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["mimeType"].as_str().unwrap_or("").starts_with("audio/"))
        .filter(|f| f["url"].as_str().is_some())
        .max_by_key(|f| format_rank(f))
        .ok_or_else(|| "No direct playable audio streams found in response".to_string())?;

    let mut raw_url = fmt["url"].as_str().unwrap_or_default().to_string();
    let mime_type = fmt["mimeType"].as_str().unwrap_or("audio/webm").to_string();

    let sep = |u: &str| if u.contains('?') { '&' } else { '?' };

    // Append client playback nonce (cpn) to avoid CDN chunk throttling
    if !cpn.is_empty() && query_param(&raw_url, "cpn").is_none() {
        let s = sep(&raw_url);
        raw_url.push_str(&format!("{}cpn={}", s, cpn));
    }

    // Extension hint so foobar2000's input manager recognizes the container
    if query_param(&raw_url, "file").is_none() {
        let ext = if mime_type.contains("webm") || mime_type.contains("opus") {
            Some("webm")
        } else if mime_type.contains("mp4") || mime_type.contains("m4a") || mime_type.contains("aac") {
            Some("m4a")
        } else {
            None
        };
        if let Some(ext) = ext {
            let s = sep(&raw_url);
            raw_url.push_str(&format!("{}file=audio.{}", s, ext));
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
    let page = crate::browse::search_page(query, crate::browse::ItemKind::Video, None)?;
    Ok(page
        .items
        .into_iter()
        .take(max_results)
        .map(|i| SearchItem {
            id: i.id,
            title: i.title,
            channel: i.channel,
            duration_str: i.info,
            duration_seconds: i.duration_seconds,
        })
        .collect())
}

fn get_stream_agent() -> &'static ureq::Agent {
    static STREAM_AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    STREAM_AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(8))
            .timeout_read(Duration::from_secs(15))
            .build()
    })
}

/// Fetch bytes [start, end] into `out`. Returns (bytes written, total resource size or 0 if unknown).
pub fn fetch_range(url: &str, start: u64, end: u64, out: &mut [u8]) -> Result<(usize, u64), FetchError> {
    if end < start {
        return Err(FetchError::Other(format!("Invalid range {}-{}", start, end)));
    }
    let range_header = format!("bytes={}-{}", start, end);

    let resp = match get_stream_agent()
        .get(url)
        .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .set("Range", &range_header)
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code @ (401 | 403 | 404 | 410), _)) => {
            return Err(FetchError::Expired(format!("Stream URL rejected with HTTP {}", code)));
        }
        Err(e) => return Err(FetchError::Other(format!("HTTP Range request failed: {}", e))),
    };

    let status = resp.status();
    let total_size;
    match status {
        206 => {
            // Content-Range: bytes <first>-<last>/<total>
            let cr = resp.header("Content-Range").unwrap_or("");
            let (range_part, total_part) = cr.rsplit_once('/').unwrap_or(("", ""));
            total_size = total_part.trim().parse::<u64>().unwrap_or(0);
            let first = range_part
                .trim()
                .trim_start_matches("bytes")
                .trim()
                .split('-')
                .next()
                .and_then(|s| s.trim().parse::<u64>().ok());
            if let Some(first) = first {
                if first != start {
                    return Err(FetchError::Other(format!(
                        "Server returned range starting at {} instead of {}",
                        first, start
                    )));
                }
            }
        }
        200 => {
            // Server ignored Range. Only usable when we asked from the beginning.
            if start != 0 {
                return Err(FetchError::Other("Server ignored Range header".to_string()));
            }
            total_size = resp
                .header("Content-Length")
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
        }
        _ => {
            return Err(FetchError::Other(format!("Unexpected HTTP status {}", status)));
        }
    }

    let wanted = ((end - start + 1).min(out.len() as u64)) as usize;
    let mut reader = resp.into_reader().take(wanted as u64);
    let mut filled = 0usize;
    while filled < wanted {
        match reader.read(&mut out[filled..wanted]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                if filled == 0 {
                    return Err(FetchError::Other(format!("Failed to read stream bytes: {}", e)));
                }
                break;
            }
        }
    }

    if filled == 0 {
        return Err(FetchError::Other("Empty response body".to_string()));
    }
    Ok((filled, total_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let id = "dQw4w9WgXcQ";
        for s in [
            "dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ&t=10",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RD",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ#t=3",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ?autoplay=1",
            "https://www.youtube.com/live/dQw4w9WgXcQ?si=x",
            "youtube://dQw4w9WgXcQ",
            "yt://dQw4w9WgXcQ",
            "YouTube://dQw4w9WgXcQ",
            "  dQw4w9WgXcQ  ",
        ] {
            assert_eq!(extract_video_id(s).as_deref(), Some(id), "{}", s);
        }
        for s in ["", "hello", "youtube://short", "https://example.com/watch?v=bad", "https://youtu.be/"] {
            assert_eq!(extract_video_id(s), None, "{}", s);
        }
    }

    #[test]
    #[ignore = "hits the network; run with --ignored"]
    fn live_pipeline() {
        let results = search("rick astley never gonna give you up", 5).expect("search");
        assert!(!results.is_empty());
        let info = get_video_info(&results[0].id).expect("player");
        assert!(info.mime_type.starts_with("audio/"));
        let mut buf = vec![0u8; 64 * 1024];
        let (n, total) = fetch_range(&info.stream_url, 0, buf.len() as u64 - 1, &mut buf).expect("range0");
        assert_eq!(n, buf.len());
        assert!(total > buf.len() as u64);
        let (n2, _) = fetch_range(&info.stream_url, 1000, 1999, &mut buf).expect("range1000");
        assert_eq!(n2, 1000);
        println!("{} | {} | {} | {} bytes", info.title, info.channel, info.mime_type, total);
    }

    #[test]
    fn randoms_differ() {
        assert_ne!(random_string(16), random_string(16));
        assert_eq!(random_string(12).len(), 12);
    }

    #[test]
    fn format_ranking_prefers_original_track() {
        let dubbed = serde_json::json!({"bitrate": 200000, "audioTrack": {"audioIsDefault": false, "displayName": "French"}});
        let orig = serde_json::json!({"bitrate": 100000, "audioTrack": {"audioIsDefault": true, "displayName": "English original"}});
        let drc = serde_json::json!({"bitrate": 300000, "isDrc": true});
        let plain = serde_json::json!({"bitrate": 130000});
        assert!(format_rank(&orig) > format_rank(&dubbed));
        assert!(format_rank(&plain) > format_rank(&drc));
    }
}
