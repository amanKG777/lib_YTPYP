//! Search pages, playlists, mixes and channel uploads via the InnerTube WEB client.

use crate::innertube::{extract_video_id, query_param};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

const API_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const WEB_CLIENT_VERSION: &str = "2.20250101.01.00";
const MAX_PAGES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ItemKind {
    Video = 1,
    Playlist = 2,
    Channel = 3,
}

impl ItemKind {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            1 => Some(ItemKind::Video),
            2 => Some(ItemKind::Playlist),
            3 => Some(ItemKind::Channel),
            _ => None,
        }
    }

    fn search_params(self) -> &'static str {
        match self {
            ItemKind::Video => "EgIQAQ==",
            ItemKind::Playlist => "EgIQAw==",
            ItemKind::Channel => "EgIQAg==",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub kind: ItemKind,
    pub id: String,
    pub title: String,
    pub channel: String,
    /// Duration text for videos, "N videos" for playlists, subscriber text for channels.
    pub info: String,
    pub duration_seconds: u32,
}

#[derive(Debug, Default)]
pub struct Page {
    pub items: Vec<Item>,
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlTarget {
    Video(String),
    Playlist(String),
    /// A `UC…` channel ID, or a URL (handle, /c/, /user/) that still needs resolving.
    Channel(String),
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(8))
            .timeout(Duration::from_secs(20))
            .build()
    })
}

fn web_post(endpoint: &str, mut body: Value) -> Result<Value, String> {
    body["context"] = serde_json::json!({
        "client": { "clientName": "WEB", "clientVersion": WEB_CLIENT_VERSION, "hl": "en", "gl": "US" }
    });
    let url = format!("https://www.youtube.com/youtubei/v1/{}?key={}&prettyPrint=false", endpoint, API_KEY);
    agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .send_json(body)
        .map_err(|e| format!("{} request failed: {}", endpoint, e))?
        .into_json()
        .map_err(|e| format!("Failed to parse {} response: {}", endpoint, e))
}

fn is_channel_id(s: &str) -> bool {
    s.len() == 24 && s.starts_with("UC") && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn is_playlist_id(s: &str) -> bool {
    s.len() >= 12 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Path of a youtube.com URL (no scheme/host/query), or None if it isn't a YouTube page URL.
fn youtube_path(s: &str) -> Option<&str> {
    let s = s.strip_prefix("fy+").unwrap_or(s);
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let host = host.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
    if host != "youtube.com" {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    Some(path)
}

pub fn classify_url(input: &str) -> Option<UrlTarget> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    if let Some(path) = youtube_path(s) {
        let list = query_param(s, "list").filter(|l| is_playlist_id(l));
        // A bare playlist page expands the playlist; watch?v=…&list=… plays just that video.
        if path == "playlist" {
            if let Some(list) = list {
                return Some(UrlTarget::Playlist(list.to_string()));
            }
        }
        if let Some(id) = path.strip_prefix("browse/VL") {
            if is_playlist_id(id) {
                return Some(UrlTarget::Playlist(id.to_string()));
            }
        }

        let first = path.split('/').next().unwrap_or("");
        if first == "channel" {
            if let Some(id) = path.split('/').nth(1).filter(|id| is_channel_id(id)) {
                return Some(UrlTarget::Channel(id.to_string()));
            }
        }
        if first.starts_with('@') && first.len() > 1 {
            return Some(UrlTarget::Channel(format!("https://www.youtube.com/{}", first)));
        }
        if (first == "c" || first == "user") && path.split('/').nth(1).map_or(false, |n| !n.is_empty()) {
            let name = path.split('/').nth(1).unwrap_or("");
            return Some(UrlTarget::Channel(format!("https://www.youtube.com/{}/{}", first, name)));
        }
    }

    extract_video_id(s).map(UrlTarget::Video)
}

fn text_of(v: &Value) -> String {
    if let Some(s) = v["simpleText"].as_str() {
        return s.to_string();
    }
    if let Some(s) = v["content"].as_str() {
        return s.to_string();
    }
    v["runs"]
        .as_array()
        .map(|a| a.iter().filter_map(|r| r["text"].as_str()).collect())
        .unwrap_or_default()
}

fn parse_duration(s: &str) -> u32 {
    let mut secs = 0u32;
    for part in s.split(':') {
        match part.trim().parse::<u32>() {
            Ok(v) => secs = secs.saturating_mul(60).saturating_add(v),
            Err(_) => return 0,
        }
    }
    secs
}

fn find_first<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => {
            if let Some(x) = m.get(key) {
                return Some(x);
            }
            m.values().find_map(|x| find_first(x, key))
        }
        Value::Array(a) => a.iter().find_map(|x| find_first(x, key)),
        _ => None,
    }
}

fn lockup_metadata_rows(lk: &Value) -> Vec<Vec<String>> {
    lk["metadata"]["lockupMetadataViewModel"]["metadata"]["contentMetadataViewModel"]["metadataRows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    r["metadataParts"]
                        .as_array()
                        .map(|ps| ps.iter().map(|p| text_of(&p["text"])).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn badge_text(lk: &Value) -> String {
    find_first(lk, "thumbnailBadgeViewModel")
        .and_then(|b| b["text"].as_str())
        .unwrap_or("")
        .to_string()
}

fn parse_item(el: &Value) -> Option<Item> {
    if let Some(vr) = el.get("videoRenderer").or_else(|| el.get("playlistVideoRenderer")) {
        let id = vr["videoId"].as_str()?;
        let info = text_of(&vr["lengthText"]);
        let channel = {
            let o = text_of(&vr["ownerText"]);
            if o.is_empty() { text_of(&vr["shortBylineText"]) } else { o }
        };
        let duration_seconds = vr["lengthSeconds"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| parse_duration(&info));
        return Some(Item {
            kind: ItemKind::Video,
            id: id.to_string(),
            title: text_of(&vr["title"]),
            channel,
            info,
            duration_seconds,
        });
    }

    if let Some(pr) = el.get("playlistPanelVideoRenderer") {
        let id = pr["videoId"].as_str()?;
        let info = text_of(&pr["lengthText"]);
        return Some(Item {
            kind: ItemKind::Video,
            id: id.to_string(),
            title: text_of(&pr["title"]),
            channel: text_of(&pr["longBylineText"]),
            duration_seconds: parse_duration(&info),
            info,
        });
    }

    if let Some(cr) = el.get("channelRenderer") {
        let id = cr["channelId"].as_str()?;
        return Some(Item {
            kind: ItemKind::Channel,
            id: id.to_string(),
            title: text_of(&cr["title"]),
            channel: String::new(),
            info: text_of(&cr["videoCountText"]),
            duration_seconds: 0,
        });
    }

    if let Some(lk) = el.get("lockupViewModel") {
        let id = lk["contentId"].as_str()?;
        let title = lk["metadata"]["lockupMetadataViewModel"]["title"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let rows = lockup_metadata_rows(lk);
        let channel = rows.first().and_then(|r| r.first()).cloned().unwrap_or_default();
        let badge = badge_text(lk);
        return match lk["contentType"].as_str().unwrap_or("") {
            "LOCKUP_CONTENT_TYPE_VIDEO" => Some(Item {
                kind: ItemKind::Video,
                id: id.to_string(),
                title,
                channel,
                duration_seconds: parse_duration(&badge),
                info: badge,
            }),
            "LOCKUP_CONTENT_TYPE_PLAYLIST" | "LOCKUP_CONTENT_TYPE_PODCAST" | "LOCKUP_CONTENT_TYPE_ALBUM" => Some(Item {
                kind: ItemKind::Playlist,
                id: id.to_string(),
                title,
                channel,
                info: badge,
                duration_seconds: 0,
            }),
            _ => None,
        };
    }

    None
}

fn continuation_token(el: &Value) -> Option<String> {
    let c = el.get("continuationItemRenderer").or_else(|| el.get("continuationItemViewModel"))?;
    let cmd = find_first(c, "continuationCommand")?;
    cmd["token"]
        .as_str()
        .or_else(|| find_first(cmd, "token").and_then(|t| t.as_str()))
        .map(str::to_string)
}

/// Collect items and the "next page" token. A token is only taken from an array whose subtree
/// holds result items (so unrelated continuations are ignored); the innermost such array wins.
/// Returns whether any result items were found under `v`.
fn collect(v: &Value, want: Option<ItemKind>, page: &mut Page, seen: &mut HashSet<String>) -> bool {
    match v {
        Value::Array(arr) => {
            let mut had_items = false;
            let mut token = None;
            for el in arr {
                if let Some(item) = parse_item(el) {
                    had_items = true;
                    if want.map_or(true, |k| k == item.kind) && seen.insert(item.id.clone()) {
                        page.items.push(item);
                    }
                    continue;
                }
                if let Some(t) = continuation_token(el) {
                    token = Some(t);
                    continue;
                }
                had_items |= collect(el, want, page, seen);
            }
            if had_items && page.continuation.is_none() {
                page.continuation = token;
            }
            had_items
        }
        Value::Object(m) => {
            let mut had_items = false;
            for x in m.values() {
                had_items |= collect(x, want, page, seen);
            }
            had_items
        }
        _ => false,
    }
}

pub fn search_page(query: &str, kind: ItemKind, continuation: Option<&str>) -> Result<Page, String> {
    let body = match continuation {
        Some(tok) if !tok.is_empty() => serde_json::json!({ "continuation": tok }),
        _ => serde_json::json!({ "query": query, "params": kind.search_params() }),
    };
    let json = web_post("search", body)?;
    let mut page = Page::default();
    let mut seen = HashSet::new();
    collect(&json, Some(kind), &mut page, &mut seen);
    if page.items.is_empty() {
        page.continuation = None;
    }
    Ok(page)
}

fn resolve_channel_id(target: &str) -> Result<String, String> {
    if is_channel_id(target) {
        return Ok(target.to_string());
    }
    let json = web_post("navigation/resolve_url", serde_json::json!({ "url": target }))?;
    json["endpoint"]["browseEndpoint"]["browseId"]
        .as_str()
        .filter(|id| is_channel_id(id))
        .map(str::to_string)
        .ok_or_else(|| format!("Could not find a channel for {}", target))
}

fn expand_mix(list_id: &str, max_items: usize) -> Result<(String, Vec<Item>), String> {
    let mut body = serde_json::json!({ "playlistId": list_id });
    // RD<videoId> mixes need the seed video.
    if let Some(seed) = list_id.strip_prefix("RD").filter(|s| s.len() == 11) {
        body["videoId"] = Value::String(seed.to_string());
    }
    let json = web_post("next", body)?;
    let pl = &json["contents"]["twoColumnWatchNextResults"]["playlist"]["playlist"];
    let title = pl["title"].as_str().unwrap_or("YouTube Mix").to_string();
    let mut page = Page::default();
    let mut seen = HashSet::new();
    collect(&pl["contents"], Some(ItemKind::Video), &mut page, &mut seen);
    page.items.truncate(max_items);
    if page.items.is_empty() {
        return Err("Mix is empty or unavailable".to_string());
    }
    Ok((title, page.items))
}

fn expand_playlist(list_id: &str, max_items: usize) -> Result<(String, Vec<Item>), String> {
    if list_id.starts_with("RD") {
        return expand_mix(list_id, max_items);
    }

    let first = web_post("browse", serde_json::json!({ "browseId": format!("VL{}", list_id) }))?;
    if let Some(alert) = find_first(&first, "alertRenderer") {
        let msg = text_of(&alert["text"]);
        if first["contents"].is_null() {
            return Err(if msg.is_empty() { "Playlist unavailable".to_string() } else { msg });
        }
    }
    let title = first["metadata"]["playlistMetadataRenderer"]["title"]
        .as_str()
        .map(str::to_string)
        .or_else(|| find_first(&first, "pageHeaderRenderer").and_then(|h| h["pageTitle"].as_str()).map(str::to_string))
        .unwrap_or_else(|| "YouTube Playlist".to_string());

    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut page = Page::default();
    collect(&first, Some(ItemKind::Video), &mut page, &mut seen);

    for _ in 0..MAX_PAGES {
        items.append(&mut page.items);
        if items.len() >= max_items {
            break;
        }
        let Some(tok) = page.continuation.take() else { break };
        let json = web_post("browse", serde_json::json!({ "continuation": tok }))?;
        collect(&json, Some(ItemKind::Video), &mut page, &mut seen);
        if page.items.is_empty() {
            break;
        }
    }
    items.truncate(max_items);
    if items.is_empty() {
        return Err("Playlist is empty or private".to_string());
    }
    Ok((title, items))
}

fn expand_channel(target: &str, max_items: usize) -> Result<(String, Vec<Item>), String> {
    let id = resolve_channel_id(target)?;
    let uploads = format!("UU{}", &id[2..]);
    let (title, items) = expand_playlist(&uploads, max_items)?;
    let title = title.strip_prefix("Uploads from ").map(str::to_string).unwrap_or(title);
    Ok((title, items))
}

/// Expand a URL into its videos. Returns (display name, items).
pub fn expand(target: &UrlTarget, max_items: usize) -> Result<(String, Vec<Item>), String> {
    let max_items = max_items.max(1);
    match target {
        UrlTarget::Video(id) => Ok((
            String::new(),
            vec![Item {
                kind: ItemKind::Video,
                id: id.clone(),
                title: String::new(),
                channel: String::new(),
                info: String::new(),
                duration_seconds: 0,
            }],
        )),
        UrlTarget::Playlist(id) => expand_playlist(id, max_items),
        UrlTarget::Channel(t) => expand_channel(t, max_items),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify() {
        use UrlTarget::*;
        let cases = [
            ("https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI", Some(Playlist("PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI".into()))),
            ("https://music.youtube.com/playlist?list=OLAK5uy_abcdefghijklmnop", Some(Playlist("OLAK5uy_abcdefghijklmnop".into()))),
            ("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDdQw4w9WgXcQ", Some(Video("dQw4w9WgXcQ".into()))),
            ("https://www.youtube.com/channel/UCtFOW7jJXChfFNoucRFqRmw/videos", Some(Channel("UCtFOW7jJXChfFNoucRFqRmw".into()))),
            ("https://www.youtube.com/@Official_ArijitSingh/videos", Some(Channel("https://www.youtube.com/@Official_ArijitSingh".into()))),
            ("youtube.com/c/TechnoGamerzOfficial", Some(Channel("https://www.youtube.com/c/TechnoGamerzOfficial".into()))),
            ("fy+https://www.youtube.com/watch?v=lPoozXQ3XsM", Some(Video("lPoozXQ3XsM".into()))),
            ("https://youtu.be/dQw4w9WgXcQ", Some(Video("dQw4w9WgXcQ".into()))),
            ("https://example.com/@someone", None),
            ("hello world", None),
        ];
        for (input, want) in cases {
            assert_eq!(classify_url(input), want, "{}", input);
        }
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("3:05"), 185);
        assert_eq!(parse_duration("1:02:03"), 3723);
        assert_eq!(parse_duration(""), 0);
        assert_eq!(parse_duration("LIVE"), 0);
    }

    #[test]
    #[ignore = "hits the network; run with --ignored"]
    fn live_browse() {
        let p1 = search_page("lofi hip hop", ItemKind::Video, None).expect("search");
        assert!(p1.items.len() >= 10, "page1 {}", p1.items.len());
        let tok = p1.continuation.clone().expect("search continuation");
        let p2 = search_page("", ItemKind::Video, Some(&tok)).expect("search page 2");
        assert!(!p2.items.is_empty());
        assert!(p2.items.iter().all(|i| !p1.items.iter().any(|j| j.id == i.id)));

        let pls = search_page("lofi hip hop", ItemKind::Playlist, None).expect("playlist search");
        assert!(pls.items.iter().all(|i| i.kind == ItemKind::Playlist) && !pls.items.is_empty());

        let chs = search_page("arijit singh", ItemKind::Channel, None).expect("channel search");
        assert!(chs.items.iter().all(|i| i.id.starts_with("UC")) && !chs.items.is_empty());

        let (title, items) = expand(&UrlTarget::Playlist("PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI".into()), 1000).expect("playlist");
        println!("playlist '{}' -> {} items, first '{}' by '{}' ({})", title, items.len(), items[0].title, items[0].channel, items[0].duration_seconds);
        assert!(items.len() > 100, "expected continuation to load more than one page");

        let (title, items) = expand(&classify_url("https://www.youtube.com/@Official_ArijitSingh").unwrap(), 150).expect("channel");
        println!("channel '{}' -> {} items", title, items.len());
        assert_eq!(items.len(), 150);

        let (title, items) = expand(&UrlTarget::Playlist("RDdQw4w9WgXcQ".into()), 50).expect("mix");
        println!("mix '{}' -> {} items", title, items.len());
        assert!(items.len() >= 10);
    }
}
