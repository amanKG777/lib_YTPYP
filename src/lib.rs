pub mod innertube;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

pub type SearchCallback = extern "C" fn(
    id: *const c_char,
    title: *const c_char,
    channel: *const c_char,
    duration_str: *const c_char,
    duration_sec: i32,
    user_data: *mut c_void,
);

fn copy_to_c_buf(src: &str, dest: *mut c_char, max_len: usize) {
    if dest.is_null() || max_len == 0 {
        return;
    }
    let bytes = src.as_bytes();
    let copy_len = bytes.len().min(max_len - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dest as *mut u8, copy_len);
        *dest.add(copy_len) = 0;
    }
}

/// Extracts an 11-char YouTube video ID from a URL, URI, or ID string.
/// Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn ytpipe_extract_video_id(
    input: *const c_char,
    out_id: *mut c_char,
    out_max: usize,
) -> i32 {
    if input.is_null() {
        return 0;
    }
    let c_str = unsafe { CStr::from_ptr(input) };
    let input_str = match c_str.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };

    if let Some(id) = innertube::extract_video_id(input_str) {
        copy_to_c_buf(&id, out_id, out_max);
        1
    } else {
        0
    }
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
/// Returns 1 on success, 0 on failure.
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
    if video_id.is_null() {
        return 0;
    }
    let c_str = unsafe { CStr::from_ptr(video_id) };
    let id_str = match c_str.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };

    // If input is a URL, extract the ID first
    let clean_id = match innertube::extract_video_id(id_str) {
        Some(id) => id,
        None => id_str.to_string(),
    };

    match innertube::get_video_info(&clean_id) {
        Ok(info) => {
            copy_to_c_buf(&info.title, out_title, out_title_max);
            copy_to_c_buf(&info.channel, out_channel, out_channel_max);
            if !out_duration_seconds.is_null() {
                unsafe {
                    *out_duration_seconds = info.duration_seconds as i32;
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
}

/// Fetches a byte range chunk from a stream URL.
/// Returns bytes read into out_buf, or -1 on error.
#[no_mangle]
pub extern "C" fn ytpipe_fetch_range(
    stream_url: *const c_char,
    start_byte: u64,
    end_byte: u64,
    out_buf: *mut u8,
    buf_capacity: usize,
    out_total_size: *mut u64,
) -> i64 {
    if stream_url.is_null() || out_buf.is_null() || buf_capacity == 0 {
        return -1;
    }
    let c_str = unsafe { CStr::from_ptr(stream_url) };
    let url_str = match c_str.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };

    match innertube::fetch_range(url_str, start_byte, end_byte) {
        Ok((bytes, total_size)) => {
            let to_copy = bytes.len().min(buf_capacity);
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, to_copy);
                if !out_total_size.is_null() && total_size > 0 {
                    *out_total_size = total_size;
                }
            }
            to_copy as i64
        }
        Err(e) => {
            eprintln!("[ytpipe_core] fetch_range error: {}", e);
            -1
        }
    }
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
    if query.is_null() {
        return -1;
    }
    let c_str = unsafe { CStr::from_ptr(query) };
    let query_str = match c_str.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };

    let limit = if max_results <= 0 { 20 } else { max_results as usize };
    let results = match innertube::search(query_str, limit) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[ytpipe_core] Error searching YouTube: {}", e);
            return -1;
        }
    };

    if let Some(cb) = callback {
        for item in &results {
            let id_c = CString::new(item.id.as_str()).unwrap_or_default();
            let title_c = CString::new(item.title.as_str()).unwrap_or_default();
            let chan_c = CString::new(item.channel.as_str()).unwrap_or_default();
            let dur_c = CString::new(item.duration_str.as_str()).unwrap_or_default();

            cb(
                id_c.as_ptr(),
                title_c.as_ptr(),
                chan_c.as_ptr(),
                dur_c.as_ptr(),
                item.duration_seconds as i32,
                user_data,
            );
        }
    }

    results.len() as i32
}
