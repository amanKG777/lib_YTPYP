pub mod browse;
pub mod innertube;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub type SearchCallback = extern "C" fn(
    id: *const c_char,
    title: *const c_char,
    channel: *const c_char,
    duration_str: *const c_char,
    duration_sec: i32,
    user_data: *mut c_void,
);

/// Callback for search pages and URL expansion.
/// kind: 1 = video, 2 = playlist, 3 = channel.
/// info: duration text (video), "N videos" (playlist) or subscriber text (channel).
pub type ItemCallback = extern "C" fn(
    kind: i32,
    id: *const c_char,
    title: *const c_char,
    channel: *const c_char,
    info: *const c_char,
    duration_sec: i32,
    user_data: *mut c_void,
);

/// Return code from `ytpipe_fetch_range` when the stream URL has expired or been rejected.
pub const YTPIPE_FETCH_EXPIRED: i64 = -2;

/// Run `f`, converting any panic into `fallback`. Unwinding across `extern "C"` aborts the host.
fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("[ytpipe_core] internal panic caught at FFI boundary");
            fallback
        }
    }
}

/// Copy `src` into a NUL-terminated C buffer, truncating on a UTF-8 character boundary.
fn copy_to_c_buf(src: &str, dest: *mut c_char, max_len: usize) {
    if dest.is_null() || max_len == 0 {
        return;
    }
    let mut copy_len = src.len().min(max_len - 1);
    while copy_len > 0 && !src.is_char_boundary(copy_len) {
        copy_len -= 1;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), dest as *mut u8, copy_len);
        *dest.add(copy_len) = 0;
    }
}

unsafe fn c_str_arg<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

/// Extracts an 11-char YouTube video ID from a URL, URI, or ID string.
/// Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn ytpipe_extract_video_id(input: *const c_char, out_id: *mut c_char, out_max: usize) -> i32 {
    guard(0, || {
        let Some(input_str) = (unsafe { c_str_arg(input) }) else { return 0 };
        match innertube::extract_video_id(input_str) {
            Some(id) => {
                copy_to_c_buf(&id, out_id, out_max);
                1
            }
            None => 0,
        }
    })
}

/// Retrieves metadata and direct playable stream URL for a video ID.
/// Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn ytpipe_get_video_info(
    video_id: *const c_char,
    out_title: *mut c_char,
    out_title_max: usize,
    out_channel: *mut c_char,
    out_channel_max: usize,
    out_duration_seconds: *mut i32,
    out_stream_url: *mut c_char,
    out_url_max: usize,
) -> i32 {
    ytpipe_get_video_info_ex(
        video_id,
        out_title,
        out_title_max,
        out_channel,
        out_channel_max,
        out_duration_seconds,
        out_stream_url,
        out_url_max,
        std::ptr::null_mut(),
        0,
    )
}

/// Retrieves metadata, stream URL, and MIME content type for a video ID.
/// Returns 1 on success, 0 on failure (including a stream URL that does not fit in `out_url_max`).
#[no_mangle]
pub extern "C" fn ytpipe_get_video_info_ex(
    video_id: *const c_char,
    out_title: *mut c_char,
    out_title_max: usize,
    out_channel: *mut c_char,
    out_channel_max: usize,
    out_duration_seconds: *mut i32,
    out_stream_url: *mut c_char,
    out_url_max: usize,
    out_mime_type: *mut c_char,
    out_mime_max: usize,
) -> i32 {
    guard(0, || {
        let Some(id_str) = (unsafe { c_str_arg(video_id) }) else { return 0 };
        let Some(clean_id) = innertube::extract_video_id(id_str) else {
            eprintln!("[ytpipe_core] Not a valid YouTube video ID or URL: {}", id_str);
            return 0;
        };

        match innertube::get_video_info(&clean_id) {
            Ok(info) => {
                // A truncated URL would silently fail later; report it as an error now.
                if !out_stream_url.is_null() && info.stream_url.len() >= out_url_max {
                    eprintln!("[ytpipe_core] Stream URL too long for caller buffer ({} bytes)", info.stream_url.len());
                    return 0;
                }
                copy_to_c_buf(&info.title, out_title, out_title_max);
                copy_to_c_buf(&info.channel, out_channel, out_channel_max);
                if !out_duration_seconds.is_null() {
                    unsafe {
                        *out_duration_seconds = info.duration_seconds.min(i32::MAX as u32) as i32;
                    }
                }
                copy_to_c_buf(&info.stream_url, out_stream_url, out_url_max);
                copy_to_c_buf(&info.mime_type, out_mime_type, out_mime_max);
                1
            }
            Err(e) => {
                eprintln!("[ytpipe_core] Error fetching video info: {}", e);
                0
            }
        }
    })
}

/// Drops any cached stream URL for the video so the next lookup re-resolves it.
#[no_mangle]
pub extern "C" fn ytpipe_invalidate_video(video_id: *const c_char) {
    guard((), || {
        if let Some(s) = unsafe { c_str_arg(video_id) } {
            if let Some(id) = innertube::extract_video_id(s) {
                innertube::invalidate(&id);
            }
        }
    })
}

/// Fetches a byte range chunk from a stream URL.
/// Returns bytes read into out_buf, -2 if the URL has expired/been rejected, or -1 on other errors.
#[no_mangle]
pub extern "C" fn ytpipe_fetch_range(
    stream_url: *const c_char,
    start_byte: u64,
    end_byte: u64,
    out_buf: *mut u8,
    buf_capacity: usize,
    out_total_size: *mut u64,
) -> i64 {
    guard(-1, || {
        if out_buf.is_null() || buf_capacity == 0 {
            return -1;
        }
        let Some(url_str) = (unsafe { c_str_arg(stream_url) }) else { return -1 };
        let out = unsafe { std::slice::from_raw_parts_mut(out_buf, buf_capacity) };

        match innertube::fetch_range(url_str, start_byte, end_byte, out) {
            Ok((n, total_size)) => {
                if !out_total_size.is_null() && total_size > 0 {
                    unsafe { *out_total_size = total_size };
                }
                n as i64
            }
            Err(innertube::FetchError::Expired(e)) => {
                eprintln!("[ytpipe_core] fetch_range: {}", e);
                YTPIPE_FETCH_EXPIRED
            }
            Err(e) => {
                eprintln!("[ytpipe_core] fetch_range error: {}", e);
                -1
            }
        }
    })
}

/// Performs a YouTube search and passes results to the callback.
/// Returns the number of results found, or -1 on error.
#[no_mangle]
pub extern "C" fn ytpipe_search(
    query: *const c_char,
    max_results: i32,
    callback: Option<SearchCallback>,
    user_data: *mut c_void,
) -> i32 {
    guard(-1, || {
        let Some(query_str) = (unsafe { c_str_arg(query) }) else { return -1 };

        let limit = if max_results <= 0 { 20 } else { max_results as usize };
        let results = match innertube::search(query_str, limit) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[ytpipe_core] Error searching YouTube: {}", e);
                return -1;
            }
        };

        let to_c = |s: &str| CString::new(s.replace('\0', "")).unwrap_or_default();

        if let Some(cb) = callback {
            for item in &results {
                let id_c = to_c(&item.id);
                let title_c = to_c(&item.title);
                let chan_c = to_c(&item.channel);
                let dur_c = to_c(&item.duration_str);

                cb(
                    id_c.as_ptr(),
                    title_c.as_ptr(),
                    chan_c.as_ptr(),
                    dur_c.as_ptr(),
                    item.duration_seconds.min(i32::MAX as u32) as i32,
                    user_data,
                );
            }
        }

        results.len() as i32
    })
}

fn emit_items(items: &[browse::Item], callback: Option<ItemCallback>, user_data: *mut c_void) {
    let Some(cb) = callback else { return };
    let to_c = |s: &str| CString::new(s.replace(' ', "")).unwrap_or_default();
    for item in items {
        let id = to_c(&item.id);
        let title = to_c(&item.title);
        let channel = to_c(&item.channel);
        let info = to_c(&item.info);
        cb(
            item.kind as i32,
            id.as_ptr(),
            title.as_ptr(),
            channel.as_ptr(),
            info.as_ptr(),
            item.duration_seconds.min(i32::MAX as u32) as i32,
            user_data,
        );
    }
}

/// Fetches one page of search results of the given kind (1 video, 2 playlist, 3 channel).
/// Pass a NULL/empty `continuation` for the first page, or the token written to
/// `out_continuation` by the previous call for the next page. `out_continuation` is set to an
/// empty string when there are no more pages. Returns the number of items, or -1 on error.
#[no_mangle]
pub extern "C" fn ytpipe_search_page(
    query: *const c_char,
    kind: i32,
    continuation: *const c_char,
    callback: Option<ItemCallback>,
    user_data: *mut c_void,
    out_continuation: *mut c_char,
    out_continuation_max: usize,
) -> i32 {
    guard(-1, || {
        copy_to_c_buf("", out_continuation, out_continuation_max);
        let query_str = unsafe { c_str_arg(query) }.unwrap_or("");
        let cont = unsafe { c_str_arg(continuation) }.filter(|c| !c.is_empty());
        if query_str.trim().is_empty() && cont.is_none() {
            return -1;
        }
        let Some(kind) = browse::ItemKind::from_i32(kind) else { return -1 };

        match browse::search_page(query_str, kind, cont) {
            Ok(page) => {
                emit_items(&page.items, callback, user_data);
                if let Some(tok) = page.continuation {
                    if tok.len() < out_continuation_max {
                        copy_to_c_buf(&tok, out_continuation, out_continuation_max);
                    }
                }
                page.items.len() as i32
            }
            Err(e) => {
                eprintln!("[ytpipe_core] search page failed: {}", e);
                -1
            }
        }
    })
}

/// Classifies a link: 0 = not YouTube, 1 = video, 2 = playlist/mix, 3 = channel.
#[no_mangle]
pub extern "C" fn ytpipe_classify_url(input: *const c_char) -> i32 {
    guard(0, || match unsafe { c_str_arg(input) }.and_then(browse::classify_url) {
        Some(browse::UrlTarget::Video(_)) => 1,
        Some(browse::UrlTarget::Playlist(_)) => 2,
        Some(browse::UrlTarget::Channel(_)) => 3,
        None => 0,
    })
}

/// Expands a video, playlist, mix or channel link into its videos (up to `max_items`), calling
/// `callback` for each. Writes the playlist/channel name to `out_title` (empty for a single
/// video). Returns the number of videos, or -1 on error (message in `out_error`).
#[no_mangle]
pub extern "C" fn ytpipe_expand_url(
    input: *const c_char,
    max_items: i32,
    callback: Option<ItemCallback>,
    user_data: *mut c_void,
    out_title: *mut c_char,
    out_title_max: usize,
    out_error: *mut c_char,
    out_error_max: usize,
) -> i32 {
    guard(-1, || {
        copy_to_c_buf("", out_title, out_title_max);
        copy_to_c_buf("", out_error, out_error_max);
        let Some(target) = unsafe { c_str_arg(input) }.and_then(browse::classify_url) else {
            copy_to_c_buf("Not a YouTube link", out_error, out_error_max);
            return -1;
        };
        let max = if max_items <= 0 { 5000 } else { max_items as usize };
        match browse::expand(&target, max) {
            Ok((title, items)) => {
                copy_to_c_buf(&title, out_title, out_title_max);
                emit_items(&items, callback, user_data);
                items.len() as i32
            }
            Err(e) => {
                copy_to_c_buf(&e, out_error, out_error_max);
                -1
            }
        }
    })
}
