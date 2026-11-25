# lib_YTPYP (`ytpipe_core`)

`ytpipe_core` is a small Rust library that talks to YouTube without yt-dlp, Python, Node.js or a browser. It finds playable audio stream links, searches YouTube, expands playlists, mixes and channels, and fetches audio in pieces so playback can seek instantly.

It exposes a plain C interface and builds as a static library (`ytpipe_core.lib`), so any native program can use it. It was written for [foo_ytpyp](https://github.com/amanKG777/foo_ytpyp), the foobar2000 YouTube component, which includes this repository as a git submodule.

---

## How it works (and how it's different from yt-dlp)

Most YouTube players for desktop apps run **yt-dlp** behind the scenes. yt-dlp is great, but it's a big separate program. It needs regular updates, and because of the way YouTube scrambles its links, it now needs a JavaScript runtime too. This library takes a much shorter path.

### Getting a playable link

When you play a video, YouTube has to hand over a link to the actual audio file on its servers (googlevideo.com). How that link comes back depends on who's asking:

- **The YouTube website** gets links that are scrambled. Unscrambling them means downloading YouTube's player JavaScript and running parts of it. That's most of what yt-dlp does, and it's why yt-dlp breaks whenever YouTube changes that code.
- **YouTube's own apps** (iPhone, Vision Pro, Android VR headsets) talk to YouTube through its internal app API, called InnerTube. For many of these apps, YouTube sends back plain, ready-to-use links.

`ytpipe_core` asks the way those apps do. It first gets a normal anonymous visitor session from YouTube, just like a freshly installed app would. Then it requests the video as the Vision Pro app. If that doesn't work, it tries the iPhone app and then the Android VR app. Whichever answers first wins. No JavaScript is run and nothing is unscrambled. It's one small web request, usually well under a second.

From the formats returned it picks the best audio-only stream (usually Opus around 160 kbps, or AAC), preferring the original-language track over AI-dubbed ones. Resolved links are cached, and a caller can drop a cached link (`ytpipe_invalidate_video`) when it expires so the next lookup gets a fresh one.

### Streaming instead of downloading

yt-dlp-based players usually download the whole audio file, or pipe it through another program, before or while it plays. This library never downloads the whole thing. It reads the audio in pieces, asking YouTube's servers for "bytes 5,000,000 to 6,000,000" and so on (this is called an HTTP range request). The caller sees what looks like an ordinary file and decodes it itself.

That's why seeking is instant. Jumping to 45:00 in a mix just means asking for the piece at that spot, not waiting for everything before it to arrive. If a link has expired or been rejected, `ytpipe_fetch_range` reports it separately (`-2`) so the caller can re-resolve and carry on.

### Search, playlists and channels

These use the same InnerTube API, this time asking the way the website does. Search results, playlist contents and channel uploads aren't scrambled, so no extra work is needed. Long playlists and channels come in pages of about 100, and the library keeps asking for the next page until it has the lot (up to the caller's limit).

### The honest trade-off

This only works as long as YouTube keeps handing those app clients plain links. yt-dlp has a big team chasing every YouTube change. This is one small project. If YouTube locks things down, link resolution may stop working until the library is updated. To soften that, there are three different app clients to fall back on, and they can be swapped out quickly when one stops working.

### Privacy

No sign-in, cookies or Google account are used. The library only talks to YouTube's own servers (youtube.com, youtubei.googleapis.com and googlevideo.com), as an anonymous app session.

---

## C interface

All functions are declared in `src/lib.rs`. Strings are UTF-8, null-terminated; output buffers are caller-owned.

| Function | What it does |
| --- | --- |
| `ytpipe_extract_video_id` | Pulls the 11-character video ID out of a URL, `youtube://` URI or bare ID. |
| `ytpipe_classify_url` | Classifies a link: 0 = not YouTube, 1 = video, 2 = playlist/mix, 3 = channel. |
| `ytpipe_get_video_info` / `_ex` | Resolves title, channel, length and a direct stream URL (plus MIME type in `_ex`). |
| `ytpipe_invalidate_video` | Drops the cached stream URL for a video. |
| `ytpipe_fetch_range` | Fetches a byte range from a stream URL. Returns bytes read, `-2` if the URL expired, `-1` on other errors. |
| `ytpipe_search` / `ytpipe_search_page` | Searches videos, playlists or channels; `_page` supports continuation tokens for paging. |
| `ytpipe_expand_url` | Expands a video, playlist, mix or channel link into its videos via a callback. |

Supported links include `watch?v=`, `youtu.be`, Shorts, live, embeds, YouTube Music, mobile links, playlists, mixes, `@handle`, `/channel/UC...`, `/c/...` and `/user/...`.

---

## Building

You'll need the Rust toolchain. For use from MSVC programs such as foobar2000 components, use the `x86_64-pc-windows-msvc` target.

```powershell
cargo build --release
```

This produces `target/release/ytpipe_core.lib` (static) and `ytpipe_core.dll` (dynamic).

When used from foo_ytpyp, run it from the parent repository instead:

```powershell
cargo build --release --manifest-path "ytpipe_core\Cargo.toml"
```

When linking the static library on Windows, also link `ws2_32.lib`, `crypt32.lib`, `userenv.lib`, `bcrypt.lib`, `ntdll.lib` and `synchronization.lib`.

### Tests

- `cargo test --release` runs the offline tests, such as link parsing.
- `cargo test --release -- --include-ignored` also runs live tests against YouTube: search, paging, playlists, channels, mixes and actual audio fetching. It's a quick way to check whether YouTube has changed something.

### Where things are

- `src/innertube.rs` finds stream links, picks formats, caches links and fetches audio pieces.
- `src/browse.rs` handles search, playlists, mixes and channels.
- `src/lib.rs` is the C interface.

---

## Credits

The idea of talking to YouTube's app API directly, instead of shelling out to yt-dlp, comes from [**FukeX**](https://github.com/amankg777/fukex), and the approach to YouTube's app clients follows ideas from **NewPipe**.
