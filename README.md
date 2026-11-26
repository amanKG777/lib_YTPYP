# lib_YTPYP (`ytpipe_core`)

`ytpipe_core` is a small Rust library that talks to YouTube without yt-dlp, Python, Node.js or a browser. It:

- resolves a video to a direct, playable audio stream link,
- fetches that audio in byte ranges, so a player can seek instantly without downloading the whole file,
- searches YouTube for videos, playlists and channels, with paging,
- expands playlists, mixes and channel uploads into lists of videos,
- recognises almost any form of YouTube link.

It exposes a plain C interface and builds as a static library (`ytpipe_core.lib`) and a DLL, so any native program can use it. It was written for [foo_ytpyp](https://github.com/amanKG777/foo_ytpyp), the foobar2000 YouTube component, which includes this repository as a git submodule.

---

## Contents

- [How it works](#how-it-works-and-how-its-different-from-yt-dlp)
- [Link resolution in detail](#link-resolution-in-detail)
- [Streaming in detail](#streaming-in-detail)
- [Search, playlists, mixes and channels in detail](#search-playlists-mixes-and-channels-in-detail)
- [Supported links](#supported-links)
- [C interface reference](#c-interface-reference)
- [Threading, errors and logging](#threading-errors-and-logging)
- [Building](#building)
- [Tests](#tests)
- [Source layout](#source-layout)
- [When YouTube breaks something](#when-youtube-breaks-something)
- [The honest trade-off](#the-honest-trade-off)
- [Privacy](#privacy)
- [Credits](#credits)

---

## How it works (and how it's different from yt-dlp)

Most YouTube players for desktop apps run **yt-dlp** behind the scenes. yt-dlp is great, but it's a big separate program. It needs regular updates, and because of the way YouTube scrambles its links, it now needs a JavaScript runtime too. This library takes a much shorter path.

### Getting a playable link

When you play a video, YouTube has to hand over a link to the actual audio file on its servers (googlevideo.com). How that link comes back depends on who's asking:

- **The YouTube website** gets links that are scrambled. Unscrambling them means downloading YouTube's player JavaScript and running parts of it. That's most of what yt-dlp does, and it's why yt-dlp breaks whenever YouTube changes that code.
- **YouTube's own apps** (iPhone, Vision Pro, Android VR headsets) talk to YouTube through its internal app API, called InnerTube. For many of these apps, YouTube sends back plain, ready-to-use links.

`ytpipe_core` asks the way those apps do. It first gets a normal anonymous visitor session from YouTube, just like a freshly installed app would. Then it requests the video as the Vision Pro app. If that doesn't work, it tries the iPhone app and then the Android VR app. Whichever answers first wins. No JavaScript is run and nothing is unscrambled. It's one small web request, usually well under a second.

### Streaming instead of downloading

yt-dlp-based players usually download the whole audio file, or pipe it through another program, before or while it plays. This library never downloads the whole thing. It reads the audio in pieces, asking YouTube's servers for "bytes 5,000,000 to 6,000,000" and so on (this is called an HTTP range request). The caller sees what looks like an ordinary file and decodes it itself.

That's why seeking is instant. Jumping to 45:00 in a mix just means asking for the piece at that spot, not waiting for everything before it to arrive.

### Search, playlists and channels

These use the same InnerTube API, this time asking the way the website does. Search results, playlist contents and channel uploads aren't scrambled, so no extra work is needed. Long playlists and channels come in pages of about 100, and the library keeps asking for the next page until it has the lot (up to the caller's limit).

---

## Link resolution in detail

`ytpipe_get_video_info` / `ytpipe_get_video_info_ex` (Rust: `innertube::get_video_info`) work like this:

1. **Normalise the input.** Any supported link or URI is reduced to the 11-character video ID (see [Supported links](#supported-links)).
2. **Check the cache.** If a fresh link for that ID is cached, it is returned immediately with no network request.
3. **Try each app client in order**, stopping at the first that returns a playable audio stream:

   | Order | Client | Endpoint | Notes |
   | --- | --- | --- | --- |
   | 1 | `VISIONOS` 1.02 (Apple Vision Pro) | `youtubei.googleapis.com/youtubei/v1/player` | Sends `visitorData` and a `cpn`. |
   | 2 | `IOS` 21.03.2 (iPhone16,2, iOS 18.7.2) | `youtubei.googleapis.com/youtubei/v1/player` | Sends a `cpn`. |
   | 3 | `ANDROID_VR` 1.56.21 | `www.youtube.com/youtubei/v1/player` | No session data. |

   Every request sets `contentCheckOk` and `racyCheckOk`, so videos behind a "this may be inappropriate" interstitial still resolve. If all three fail, the error lists each client's reason.

4. **Check the answer.** The response must have `playabilityStatus.status == "OK"`, and the returned `videoId` must match the one asked for. Title, channel (`author`) and length (`lengthSeconds`) come from `videoDetails`.
5. **Pick the audio format.** From `streamingData.adaptiveFormats`, only `audio/*` formats that carry a plain `url` are considered. They are ranked by, in order:
   1. original-language track over AI-dubbed ones (`audioTrack.audioIsDefault`, or a track name containing "original"; formats with no `audioTrack` count as original),
   2. non-DRC (normal dynamics) over DRC,
   3. higher bitrate.

   In practice this is usually Opus in WebM at around 160 kbps, or AAC in MP4.
6. **Decorate the URL.**
   - `cpn=<16 random chars>` (the client playback nonce) is appended for the VISIONOS and IOS clients. Without it, googlevideo throttles or cuts off the stream partway through.
   - `file=audio.webm` or `file=audio.m4a` is appended as a harmless extension hint, so hosts that pick a decoder by file extension (such as foobar2000) recognise the container.
7. **Cache** the result and return it.

### Visitor session

Before the VISIONOS request, the library calls `youtubei/v1/visitor_id` as the VISIONOS client to get an anonymous `visitorData` token, the same thing a freshly installed app does. The token is reused for 24 hours. If fetching it fails, the VISIONOS request is still tried without it.

### Cache rules

- Keyed by video ID, held in memory for the life of the process.
- An entry is used only while it is **less than 3 hours old** and **more than 10 minutes before** the `expire=` timestamp in the stream URL.
- At most **256** entries; stale entries are pruned on insert, then the oldest is evicted if still full.
- `ytpipe_invalidate_video` drops an entry, so the next lookup resolves a fresh link. Call it when `ytpipe_fetch_range` returns `-2`.

---

## Streaming in detail

`ytpipe_fetch_range` (Rust: `innertube::fetch_range`) sends a `GET` with `Range: bytes=<start>-<end>` (inclusive) and reads at most `min(end - start + 1, buf_capacity)` bytes.

- **206 Partial Content** is the normal case. The total file size is taken from `Content-Range` and written to `out_total_size`. If the server's range starts somewhere other than `start`, the call fails rather than returning misplaced data.
- **200 OK** (the server ignored `Range`) is accepted only when `start == 0`; the size comes from `Content-Length`.
- **401, 403, 404, 410** mean the link has expired or been rejected. The call returns `-2` (`YTPIPE_FETCH_EXPIRED`), so the caller can invalidate, re-resolve and retry at the same offset.
- Any other status, a network failure, or an empty body returns `-1`.
- If the connection drops after some bytes have arrived, the bytes received so far are returned (a short read) instead of an error. The caller simply asks for the rest.

The caller chooses the piece size. foo_ytpyp uses 1 MB pieces.

---

## Search, playlists, mixes and channels in detail

All browsing goes through the InnerTube **WEB** client (`clientVersion 2.20250101.01.00`, `hl=en`, `gl=US`) at `www.youtube.com/youtubei/v1/<endpoint>`. Responses are plain JSON, with nothing scrambled.

| Task | Endpoint | How |
| --- | --- | --- |
| Search | `search` | `query` plus a type filter (`params`): `EgIQAQ==` videos, `EgIQAw==` playlists, `EgIQAg==` channels. Later pages send only the `continuation` token. |
| Playlist | `browse` | `browseId = VL<playlistId>`, then `continuation` requests until the limit is hit, there are no more pages, or 200 pages have been read. |
| Mix (`RD…`) | `next` | Mixes are only reachable through the watch-next panel. For `RD<videoId>` mixes the seed video ID is sent too. A mix is a single page. |
| Channel uploads | `navigation/resolve_url`, then `browse` | `@handle`, `/c/` and `/user/` links are resolved to a `UC…` channel ID. The uploads playlist is `UU` + the rest of that ID, which is then expanded like a playlist. A leading "Uploads from " is stripped from the title. |

### How results are read

YouTube's response layout changes often and differs between pages, so the parser doesn't depend on fixed paths. It walks the whole JSON tree and recognises result items by their renderer type wherever they appear:

- `videoRenderer`, `playlistVideoRenderer`, `playlistPanelVideoRenderer`: videos
- `channelRenderer`: channels
- `lockupViewModel`: the newer layout, used for videos, playlists, podcasts and albums

The "next page" token is taken from a `continuationItemRenderer` / `continuationItemViewModel` sitting in the same list as real results (the innermost such list wins), so unrelated continuations elsewhere in the page are ignored. Duplicate IDs are dropped, and results of the wrong type are filtered out.

Each item carries:

| Field | Video | Playlist | Channel |
| --- | --- | --- | --- |
| `kind` | 1 | 2 | 3 |
| `id` | 11-char video ID | playlist ID | `UC…` channel ID |
| `title` | video title | playlist title | channel name |
| `channel` | uploader | owner | (empty) |
| `info` | duration text, e.g. `3:45` | e.g. `25 videos` | subscriber text |
| `duration_sec` | parsed duration, 0 if unknown or live | 0 | 0 |

---

## Supported links

`ytpipe_extract_video_id` accepts:

- a bare 11-character video ID (surrounding spaces are trimmed),
- `youtube://<id>` and `yt://<id>` (any case),
- any URL with a `v=` query parameter: `watch?v=`, YouTube Music, mobile (`m.`), and links with extra parameters such as `&t=30s`, `&list=…`, `?si=…` or `#t=3`,
- `youtu.be/<id>`, `/shorts/<id>`, `/embed/<id>`, `/live/<id>`, `/v/<id>`, `/e/<id>`,
- legacy foo_youtube URIs (`fy+https://www.youtube.com/watch?v=…`).

`ytpipe_classify_url` / `ytpipe_expand_url` also understand:

| Link | Result |
| --- | --- |
| `youtube.com/playlist?list=<id>` (including YouTube Music albums and playlists) | playlist |
| `youtube.com/browse/VL<id>` | playlist |
| a playlist ID starting `RD` | mix |
| `youtube.com/channel/UC…` (any sub-page, e.g. `/videos`) | channel |
| `youtube.com/@handle`, `/c/<name>`, `/user/<name>` | channel |
| `watch?v=<id>&list=<id>` | **video** (just that video, not the list) |

Hosts `www.`, `m.` and `music.` are accepted, with or without `http(s)://`.

---

## C interface reference

All functions are `extern "C"` with unmangled names, defined in `src/lib.rs`. There is no shipped header; the declarations below match the Rust signatures:

```c
#include <stdint.h>
#include <stddef.h>

#define YTPIPE_FETCH_EXPIRED (-2)

typedef void (*ytpipe_search_cb)(
    const char* id, const char* title, const char* channel,
    const char* duration_str, int32_t duration_sec, void* user_data);

/* kind: 1 = video, 2 = playlist, 3 = channel */
typedef void (*ytpipe_item_cb)(
    int32_t kind, const char* id, const char* title, const char* channel,
    const char* info, int32_t duration_sec, void* user_data);

int32_t ytpipe_extract_video_id(const char* input, char* out_id, size_t out_max);

int32_t ytpipe_classify_url(const char* input);

int32_t ytpipe_get_video_info(
    const char* video_id,
    char* out_title, size_t out_title_max,
    char* out_channel, size_t out_channel_max,
    int32_t* out_duration_seconds,
    char* out_stream_url, size_t out_url_max);

int32_t ytpipe_get_video_info_ex(
    const char* video_id,
    char* out_title, size_t out_title_max,
    char* out_channel, size_t out_channel_max,
    int32_t* out_duration_seconds,
    char* out_stream_url, size_t out_url_max,
    char* out_mime_type, size_t out_mime_max);

void ytpipe_invalidate_video(const char* video_id);

int64_t ytpipe_fetch_range(
    const char* stream_url, uint64_t start_byte, uint64_t end_byte,
    uint8_t* out_buf, size_t buf_capacity, uint64_t* out_total_size);

int32_t ytpipe_search(
    const char* query, int32_t max_results,
    ytpipe_search_cb callback, void* user_data);

int32_t ytpipe_search_page(
    const char* query, int32_t kind, const char* continuation,
    ytpipe_item_cb callback, void* user_data,
    char* out_continuation, size_t out_continuation_max);

int32_t ytpipe_expand_url(
    const char* input, int32_t max_items,
    ytpipe_item_cb callback, void* user_data,
    char* out_title, size_t out_title_max,
    char* out_error, size_t out_error_max);
```

### Conventions

- **Strings** are UTF-8 and NUL-terminated. Input strings that are NULL or not valid UTF-8 make the call fail.
- **Output buffers** are owned by the caller. Each takes a pointer and its size in bytes (including the terminator). Text that doesn't fit is truncated on a UTF-8 character boundary and always NUL-terminated. Any output pointer may be NULL to skip that value.
- **Callbacks** are called synchronously, on the calling thread, before the function returns. The string pointers passed to a callback are valid only during that call; copy anything you want to keep. A NULL callback is allowed (you just get the count).
- **Nothing is allocated for the caller**, so there is nothing to free.

### Functions

#### `ytpipe_extract_video_id`
Writes the 11-character video ID found in `input` to `out_id` (needs at least 12 bytes). Returns `1` on success, `0` if no ID was found. No network access.

#### `ytpipe_classify_url`
Returns `0` not a YouTube link, `1` video, `2` playlist or mix, `3` channel. No network access.

#### `ytpipe_get_video_info` / `ytpipe_get_video_info_ex`
Resolves `video_id` (any supported link is accepted) to its title, channel, length in seconds and a direct stream URL; the `_ex` version also returns the MIME type, e.g. `audio/webm; codecs="opus"`. Returns `1` on success, `0` on failure. **If the stream URL would not fit in `out_url_max`, the call fails** rather than returning a truncated, unusable URL. Size the URL buffer generously; foo_ytpyp uses 16 KB. Uses the [cache](#cache-rules), so repeated calls are cheap.

#### `ytpipe_invalidate_video`
Drops the cached stream URL for that video (any supported link is accepted). Safe to call for videos that aren't cached.

#### `ytpipe_fetch_range`
Reads bytes `start_byte`..`end_byte` (inclusive) of `stream_url` into `out_buf`, up to `buf_capacity` bytes. Returns the number of bytes read (which may be fewer than asked for), `-2` if the URL has expired or been rejected, or `-1` on any other error. Writes the total file size to `out_total_size` when the server reports it (left untouched otherwise). See [Streaming in detail](#streaming-in-detail).

#### `ytpipe_search`
Simple video search. Calls `callback` once per result, up to `max_results` (`<= 0` means 20), from the first results page only. `duration_str` is the display text, e.g. `3:45`. Returns the number of results, or `-1` on error. New code should prefer `ytpipe_search_page`.

#### `ytpipe_search_page`
Fetches one page of a search for `kind` (1 video, 2 playlist, 3 channel). For the first page, pass `continuation` as NULL or empty. For the next page, pass the token the previous call wrote to `out_continuation`; `query` is then ignored, but `kind` still filters the results. `out_continuation` is set to an empty string when there are no more pages (or the token didn't fit; 4 KB is plenty). Returns the number of items on this page, or `-1` on error (including an empty query with no continuation, or an invalid `kind`).

#### `ytpipe_expand_url`
Expands a link into its videos, calling `callback` once per video (`kind` is always 1), up to `max_items` (`<= 0` means 5000). Writes the playlist, mix or channel name to `out_title`. For a single-video link it returns 1 and calls the callback once with only the `id` filled in and an empty title; resolve the details with `ytpipe_get_video_info`. Returns the number of videos, or `-1` on error, with a readable message in `out_error` (for example "Not a YouTube link", "Playlist is empty or private", or YouTube's own alert text). This can take several seconds for large playlists, since each page of about 100 is a separate request.

### Minimal example

```c
char title[1024], channel[512], url[16384], mime[128];
int32_t secs;
if (ytpipe_get_video_info_ex("https://youtu.be/dQw4w9WgXcQ",
        title, sizeof title, channel, sizeof channel, &secs,
        url, sizeof url, mime, sizeof mime)) {
    static uint8_t buf[1 << 20];
    uint64_t total = 0;
    int64_t n = ytpipe_fetch_range(url, 0, sizeof buf - 1, buf, sizeof buf, &total);
    if (n == YTPIPE_FETCH_EXPIRED) {
        ytpipe_invalidate_video("dQw4w9WgXcQ");
        /* resolve again and retry from the same offset */
    }
}
```

---

## Threading, errors and logging

- **Blocking.** Every function except `ytpipe_extract_video_id` and `ytpipe_classify_url` makes network requests and blocks until done. Don't call them on a UI thread.
- **Thread-safe.** Calls can be made from any number of threads at once. The stream-URL cache and visitor session are shared and protected by locks; HTTP connections are pooled.
- **Timeouts.** 8 s to connect everywhere. API requests time out after 15 s in total (20 s for browsing); stream reads time out after 15 s without data.
- **No panics cross into the host.** Every exported function catches Rust panics and returns its normal failure value instead, so a bug in the library can't crash the host program. Lock poisoning is recovered rather than propagated.
- **Logging.** Failures are written to stderr with the prefix `[ytpipe_core]`, including which app client failed and why. In a GUI process stderr usually goes nowhere, so hosts that want these messages should redirect stderr or surface the return values themselves.

---

## Building

You'll need the Rust toolchain (2021 edition). For use from MSVC programs such as foobar2000 components, use the `x86_64-pc-windows-msvc` target.

```powershell
cargo build --release
```

This produces, in `target/release/`:

- `ytpipe_core.lib`: static library (`staticlib`)
- `ytpipe_core.dll` + `ytpipe_core.dll.lib`: dynamic library (`cdylib`)

When used from foo_ytpyp, build it from the parent repository:

```powershell
cargo build --release --manifest-path "ytpipe_core\Cargo.toml"
```

### Linking the static library on Windows

Also link these system libraries, which the Rust standard library and TLS stack need:

```
ws2_32.lib crypt32.lib userenv.lib bcrypt.lib ntdll.lib synchronization.lib
```

### Dependencies

| Crate | Used for |
| --- | --- |
| `ureq` 2.x (with `json`) | Blocking HTTPS client (rustls TLS, so no OpenSSL needed) |
| `serde` / `serde_json` | Building requests and reading InnerTube's JSON responses |

---

## Tests

```powershell
cargo test --release                         # offline tests
cargo test --release -- --include-ignored    # also the live tests against YouTube
```

**Offline tests** cover video ID extraction from every supported link form, link classification, duration parsing, audio format ranking (original language over dubs, non-DRC over DRC), and the random nonce generator.

**Live tests** (marked `#[ignore]`, they hit the network) check the whole pipeline end to end:

- search, then resolve the first result, then fetch two byte ranges and check their sizes,
- video, playlist and channel search, plus paging to a second, non-overlapping page,
- a playlist with more than 100 videos (so continuation paging is exercised),
- a channel's uploads (`@handle` resolution, limited to exactly 150),
- a mix.

Running the live tests is the quickest way to find out whether YouTube has changed something.

---

## Source layout

- **`src/lib.rs`**: the C interface. Argument checking, buffer copying, the panic guard, and the callback types.
- **`src/innertube.rs`**: playback. Video ID extraction, the visitor session, the three app clients, format ranking, URL decoration, the stream-URL cache, and range fetching. Also the simple `search` wrapper.
- **`src/browse.rs`**: browsing. Link classification, the WEB client, search pages, the result parser, continuation paging, and playlist, mix and channel expansion.

---

## When YouTube breaks something

Typical symptoms and where to look:

- **Nothing plays; logs show every client failing** (`VISIONOS client failed …`, `IOS client failed …`): an app client has been blocked or its version retired. Update the client name, version, device model or OS version and User-Agent in `fetch_via_visionos` / `fetch_via_ios` / `fetch_via_android_vr`, or swap in a different client. The order of the `clients` array in `get_video_info` decides which is tried first.
- **"No direct playable audio streams found"**: YouTube is returning scrambled links (`signatureCipher`) to that client. Switch to a client that still gets plain links.
- **Playback cuts off after a minute or so**: googlevideo is throttling. Check that `cpn` (and, for VISIONOS, `visitorData`) are still being sent.
- **Search, playlists or channels return nothing**: the WEB layout has changed. Add any new renderer types to `parse_item` and `continuation_token` in `browse.rs`, or bump `WEB_CLIENT_VERSION`.

Run the live tests after any change.

---

## The honest trade-off

This only works as long as YouTube keeps handing those app clients plain links. yt-dlp has a big team chasing every YouTube change. This is one small project. If YouTube locks things down, link resolution may stop working until the library is updated. To soften that, there are three different app clients to fall back on, and they can be swapped out quickly when one stops working.

---

## Privacy

- No sign-in. The library never uses a Google account, cookies or passwords.
- It only talks to YouTube's own servers: `www.youtube.com`, `youtubei.googleapis.com` and `*.googlevideo.com`. There is no server of ours in between, and no tracking.
- YouTube sees an anonymous app session, nothing tied to the user. The visitor token is kept only in memory and never written to disk.

---

## Credits

The idea of talking to YouTube's app API directly, instead of shelling out to yt-dlp, comes from [**FukeX**](https://github.com/amankg777/fukex), and the approach to YouTube's app clients follows ideas from **NewPipe**.

---

## License

lib_YTPYP is free and open-source software, released under the **MIT License**. You are free to use, copy, modify and redistribute it, including in commercial projects, as long as the copyright notice and license text are kept. See [`LICENSE`](LICENSE) for the full text.

Copyright © 2026 Aman Kumar Gupta.

The Rust crates it depends on (`serde`, `serde_json`, `ureq` and their dependencies) are distributed under their own permissive licenses, mostly MIT and/or Apache-2.0.

lib_YTPYP is not affiliated with or endorsed by YouTube or Google.
