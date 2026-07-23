// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Media-capture verbs: `image` (terminal-content framebuffer PNG), `image read`
//! (structured inline-image payloads, headless/cross-session-safe), `window`
//! (whole-window composited PNG), and `chrome` (native macOS UI readout). Moved
//! verbatim from `control.rs` (behavior-preserving). The `ImageReq`/`ImageQueue`
//! types and the `AUDIT_SUBSYSTEM` name stay in `control.rs`, reached via `super::`.

use std::sync::{Arc, Mutex};

use aterm_containment::log_denial;
use aterm_core::grid::extra::{ImageData, ImageFormat};
use aterm_core::terminal::Terminal;
use winit::event_loop::EventLoopProxy;

use super::{AUDIT_SUBSYSTEM, ImageQueue, ImageReq};
use crate::control_auth;
use crate::{Wake, term_lock};

/// Main-loop turns are normally sub-millisecond, but a debug build's first native
/// document raster can legitimately spend well over two seconds initializing and
/// painting its retained tray. Keep the historical 30-second wire allowance: it
/// remains a hard bound (unlike the former raw `recv`) without misreporting cold
/// work as a wedged event loop. Lane-exact socket admission keeps other peers from
/// waiting invisibly behind every occupied worker.
const MAIN_THREAD_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The ONE main-thread RPC. Every UI-mutating / aux-window verb (controls, open,
/// settings, invoke, spawn, close, chrome) needs the same hop: build a one-shot
/// reply channel, post a reply-bearing `Wake` to the event loop, and BLOCK on the
/// answer (the control thread cannot touch `App`/AppKit). This collapses that
/// channel + event-gone guard + recv — previously copy-pasted into ~8 verbs — into
/// one call, so the surface never grows a bespoke RPC ceremony per verb. `Err` is a
/// wire-ready reason; callers format the `Ok(T)` into their own reply shape.
pub(crate) fn call_main<T>(
    proxy: &EventLoopProxy<Wake>,
    make: impl FnOnce(std::sync::mpsc::Sender<T>) -> Wake,
) -> Result<T, &'static str> {
    let (tx, rx) = std::sync::mpsc::channel();
    if proxy.send_event(make(tx)).is_err() {
        return Err("event loop gone");
    }
    // These RPCs answer within one event-loop turn; the deadline only fires when
    // that turn cannot finish within the generous cold-render allowance. Timing
    // out frees THIS fixed worker lane instead of retaining it forever.
    match rx.recv_timeout(MAIN_THREAD_REPLY_TIMEOUT) {
        Ok(v) => Ok(v),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err("main thread did not answer within 30s (event loop wedged?)")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("main-thread reply dropped"),
    }
}

/// One `image read` result line:
/// `<row> <col> <img_cols> <img_rows> <cell_row> <cell_col> <format> <nbytes> <base64>`.
/// `row`/`col` are the image's TOP-LEFT anchor on the grid; `cell_row`/`cell_col`
/// are the tile of interest (0/0 for a whole-image report, the queried tile in
/// cell mode); `nbytes` is the raw (pre-base64) length; the trailing base64 is the
/// image's full raw payload (PNG bytes etc.), independent of the GUI framebuffer.
/// Per-image payload cap for the line + JSON image channels (audit finding F4). An
/// inline image is USER-supplied (the inner TUI emits OSC 1337), so a hostile or
/// careless inner could embed a multi-megabyte image and force a large base64
/// allocation on every `image read` AND every styled `cells`/`screen` frame. Above
/// this raw-byte cap the payload is OMITTED and the image marked `truncated` — the
/// metadata + real `nbytes` still report it, so a consumer learns an image is there
/// and how big it is, then fetches it deliberately, without the per-frame blowup.
pub(crate) const MAX_IMAGE_PAYLOAD_BYTES: usize = 4 * 1024 * 1024; // 4 MiB raw (~5.3 MiB base64)

/// `(format, base64)` for an image, applying the F4 cap: oversized images report
/// `("truncated", "")` instead of encoding their bytes.
pub(crate) fn image_payload(img: &ImageData) -> (&'static str, String) {
    let fmt = match img.format {
        ImageFormat::Png => "png",
        ImageFormat::RawRgba8 { .. } => "rgba",
        _ => "unknown",
    };
    if img.bytes.len() > MAX_IMAGE_PAYLOAD_BYTES {
        ("truncated", String::new())
    } else {
        // `base64::encode` is fallible only above `aterm_codec::MAX_INPUT_LEN`
        // (64 MiB); bytes are already capped at MAX_IMAGE_PAYLOAD_BYTES (4 MiB)
        // above, so this branch never errors in practice. Report it like the cap
        // path (omit the payload, mark truncated) rather than panic on oversized
        // input.
        match aterm_codec::base64::encode(&img.bytes) {
            Ok(b64) => (fmt, b64),
            Err(_) => ("truncated", String::new()),
        }
    }
}

pub(crate) fn image_read_line(
    anchor_r: usize,
    anchor_c: usize,
    tile_row: u16,
    tile_col: u16,
    img: &ImageData,
) -> String {
    let (fmt, b64) = image_payload(img);
    format!(
        "{anchor_r} {anchor_c} {} {} {tile_row} {tile_col} {fmt} {} {b64}",
        img.cols,
        img.rows,
        img.bytes.len(),
    )
}

/// `image read [<r> [<c>]]` -> the structured inline-image payloads (iTerm2 OSC
/// 1337) as base64, readable HEADLESS and CROSS-SESSION (unlike the framebuffer
/// `image` rasterize verb). With no args it reports every DISTINCT image on the
/// grid (deduplicated by payload identity), one line per image at its top-left
/// anchor; `image read <r>` restricts to images intersecting row `r`; `image read
/// <r> <c>` returns the single image tile covering that exact cell (`ERR none` if
/// the cell has no image). Framed `OK <nlines>\n` + one line per image.
pub(crate) fn cmd_image_read(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let t = term_lock(term);
    let rows = t.rows() as usize;
    let cols = t.cols() as usize;
    let mut it = rest.split_whitespace();
    let r_tok = it.next();
    let c_tok = it.next();

    // Cell mode: the image covering exactly (r, c).
    if let (Some(rs), Some(cs)) = (r_tok, c_tok) {
        let (Ok(r), Ok(c)) = (rs.parse::<usize>(), cs.parse::<usize>()) else {
            return "ERR bad args\n".to_string();
        };
        if r >= rows || c >= cols {
            return "ERR out of range\n".to_string();
        }
        for (col, iref) in t.images_row(r) {
            if col == c {
                let anchor_r = r.saturating_sub(iref.cell_row as usize);
                let anchor_c = col.saturating_sub(iref.cell_col as usize);
                return format!(
                    "OK 1\n{}\n",
                    image_read_line(
                        anchor_r,
                        anchor_c,
                        iref.cell_row,
                        iref.cell_col,
                        &iref.image
                    )
                );
            }
        }
        return "ERR none\n".to_string();
    }

    // Row mode (one row) or screen mode (all rows): distinct images, anchored.
    let row_range: Vec<usize> = match r_tok {
        Some(rs) => match rs.parse::<usize>() {
            Ok(r) if r < rows => vec![r],
            Ok(_) => return "ERR out of range\n".to_string(),
            Err(_) => return "ERR bad args\n".to_string(),
        },
        None => (0..rows).collect(),
    };
    let mut seen: Vec<*const ImageData> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for r in row_range {
        for (col, iref) in t.images_row(r) {
            let ptr = std::sync::Arc::as_ptr(&iref.image);
            if seen.contains(&ptr) {
                continue;
            }
            seen.push(ptr);
            let anchor_r = r.saturating_sub(iref.cell_row as usize);
            let anchor_c = col.saturating_sub(iref.cell_col as usize);
            // Whole-image report: anchor + tile 0/0 (the full payload is carried).
            lines.push(image_read_line(anchor_r, anchor_c, 0, 0, &iref.image));
        }
    }
    let mut out = format!("OK {}\n", lines.len());
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    out
}

/// `image [path]` -> hand the render to the MAIN thread (it owns the renderer),
/// block on the reply, and report `OK <w> <h> <path>\n`. The reply is sent by
/// the GUI's encode worker only AFTER the PNG is fully written, so `OK` still
/// means the file at `<path>` is complete and readable.
///
/// PATH SAFETY: the PNG is confined to the `images/` subdir of the per-user
/// socket directory. A bare name (`image shot.png`) lands there; an empty
/// request defaults to `images/aterm-control.png`. A path that would escape the
/// subdir (`../`, an absolute path elsewhere, a symlink out) is refused with
/// `ERR path\n` and audited — the socket can no longer be used to overwrite an
/// arbitrary file via a caller-supplied path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ImageOptions {
    want_bytes: bool,
    want_metadata: bool,
    rest: String,
}

fn parse_image_options(rest: &str) -> ImageOptions {
    let tokens = rest.split_whitespace().collect::<Vec<_>>();
    ImageOptions {
        want_bytes: tokens
            .iter()
            .any(|token| *token == "--bytes" || *token == "bytes"),
        want_metadata: tokens.contains(&"--meta"),
        rest: tokens
            .into_iter()
            .filter(|token| *token != "--bytes" && *token != "bytes" && *token != "--meta")
            .collect::<Vec<_>>()
            .join(" "),
    }
}

pub(crate) fn cmd_image(
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    rest: &str,
    sock_dir: &std::path::Path,
    session: Option<u64>,
) -> String {
    // `--bytes` / `bytes`: return the PNG OVER THE WIRE — the only capture form a
    // REMOTE driver (dial/TLS) can use, since the file path names the SERVER's
    // filesystem. LINES-framed as `OK 1\n<w> <h> <nbytes> <base64>` (see the return
    // site below + `framing_of`'s image-bytes flip), NOT a raw byte body. Strip the
    // flag from anywhere in the tail; the remainder parses as usual.
    let options = parse_image_options(rest);
    let want_bytes = options.want_bytes;
    let want_metadata = options.want_metadata;
    let rest = options.rest.as_str();
    // Optional leading `plain`/`clean` keyword → a CLEAN capture with all visual bling
    // (cursor trail/glow, sparkle words, the Scene) suppressed, so an AI reads the bare
    // terminal. The rest (if any) is the filename. `image plain [name]`.
    let (clean, rest) = {
        let t = rest.trim();
        match t.split_once(char::is_whitespace) {
            Some((kw, r)) if kw == "plain" || kw == "clean" => (true, r.trim()),
            None if t == "plain" || t == "clean" => (true, ""),
            _ => (false, t),
        }
    };
    let requested = if rest.is_empty() {
        if clean {
            "aterm-clean.png"
        } else {
            "aterm-control.png"
        }
    } else {
        rest
    };
    let Some(target) = control_auth::confine_image_path(sock_dir, requested) else {
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("image write '{requested}'"),
            aterm_containment::mode_or_containment(),
            "path escapes images/ subdir or names a nested target",
        );
        return "ERR path: give a bare filename (no '/'); captures are confined to the \
                app's Application Support images/ dir. Omit the path to auto-name one — \
                the OK reply prints the full written path.\n"
            .to_string();
    };
    // For the reply only — the writer re-opens via the dir fd, not this string.
    let path = target.display_path().to_string_lossy().into_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    queue.lock().unwrap().push_back(ImageReq {
        target,
        clean,
        session,
        want_bytes,
        want_metadata,
        frame_metadata: std::sync::Arc::clone(&frame_metadata),
        reply: tx,
    });
    if proxy.send_event(Wake::Control).is_err() {
        return "ERR event loop gone\n".to_string();
    }
    // Generous ceiling: the render + encode is tens of ms; the deadline only fires
    // when the event loop or worker is wedged, where blocking the control thread for
    // the client's whole 900 s exchange window would wedge the verb too.
    match rx.recv_timeout(std::time::Duration::from_secs(120)) {
        // (0,0) is the render's honest failure reply — no window shows the target
        // (a background tab, or no window at all) and NO file was written. Report
        // it as an error instead of an `OK 0 0 <path>` pointing at nothing.
        Ok(Ok((0, 0, _))) => {
            "ERR no window displays the target session (background tab?)\n".to_string()
        }
        // `--bytes`: Lines-framed `OK 1\n<w> <h> <nbytes> <base64-png>` — the control
        // dispatch replies TEXT (written as UTF-8), so the binary PNG is base64'd
        // (the same wire-safe form `image read` uses for inline images). A REMOTE
        // driver base64-decodes to get the exact pixels, with no server-local file
        // path. Dims are on the line so the driver need not parse the PNG header.
        Ok(Ok((w, h, Some(png)))) => {
            image_bytes_reply(w, h, &png, want_metadata, frame_metadata.get())
        }
        Ok(Ok((w, h, None))) => image_file_reply(w, h, &path, want_metadata, frame_metadata.get()),
        // The encode/write failed AFTER a successful render: no file on disk.
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            "ERR image: timed out waiting for the render/encode\n".to_string()
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => "ERR render failed\n".to_string(),
    }
}

fn image_metadata_fields(metadata: Option<&crate::control::ImageFrameMetadata>) -> String {
    metadata.map_or_else(
        || {
            "image-meta-version=2 frame-kind=unknown frame-phase=rendered identity=unavailable"
                .to_string()
        },
        crate::control::ImageFrameMetadata::wire_fields,
    )
}

fn image_bytes_reply(
    width: u32,
    height: u32,
    png: &[u8],
    want_metadata: bool,
    metadata: Option<&crate::control::ImageFrameMetadata>,
) -> String {
    match aterm_codec::base64::encode(png) {
        Ok(base64) if want_metadata => format!(
            "OK 2\nimage-meta {}\n{width} {height} {} {base64}\n",
            image_metadata_fields(metadata),
            png.len(),
        ),
        Ok(base64) => format!("OK 1\n{width} {height} {} {base64}\n", png.len()),
        Err(_) => "ERR image: PNG too large to return as bytes\n".to_string(),
    }
}

fn image_file_reply(
    width: u32,
    height: u32,
    path: &str,
    want_metadata: bool,
    metadata: Option<&crate::control::ImageFrameMetadata>,
) -> String {
    if want_metadata {
        format!(
            "OK {width} {height} image-meta {} path={path:?}\n",
            image_metadata_fields(metadata),
        )
    } else {
        format!("OK {width} {height} {path}\n")
    }
}

/// `window [<target>] [path]` -> capture a window's ENTIRE on-screen pixels to a PNG,
/// replying `OK <w> <h> <path>` (the SAME wire shape as `image`). `<target>` is an
/// optional leading keyword selecting WHICH window:
///   * (omitted) / `front` — the frontmost TERMINAL window: native macOS chrome
///     (titlebar, traffic lights, unified toolbar, full-width tab strip) AND the
///     terminal content. This is the original behavior and closes the gap `image`
///     leaves (`image` rasterizes only the content framebuffer, no OS chrome).
///   * `prefs` / `settings` — the Preferences / settings window.
///   * `perf` / `performance` — the Performance control panel.
///
/// The aux targets (`prefs`/`perf`) are directly-owned `NSWindow`s that the front-window
/// path is structurally blind to; they are captured by their own window number. A first
/// token that is NOT a known keyword is treated as the PATH (so the original
/// `window [path]` wire shape still works); a literal filename `prefs`/`perf`/`front`
/// must therefore be given a target first (e.g. `window front prefs`).
///
/// PATH CONFINEMENT (mirrors [`cmd_image`]): the `path` is validated by
/// `confine_image_path` to a single filename inside the socket dir's `images/` subdir,
/// so the socket can never overwrite an arbitrary file. The default name varies by
/// target (`aterm-window.png` / `aterm-prefs.png` / `aterm-perf.png`).
///
/// MAIN-THREAD HOP (mirrors [`cmd_chrome`]): reaching a window's `NSWindow` + reading its
/// window number + calling `CGWindowListCreateImage` may ONLY happen on the main thread,
/// but this runs on a background control thread. So we post [`Wake::CaptureWindow`]
/// (front) or [`Wake::CaptureAuxWindow`] (prefs/perf) with the confined target + a
/// one-shot reply channel and BLOCK; the main thread captures and replies `Ok((w, h))`
/// or an `Err(msg)` surfaced verbatim as `ERR <msg>` (missing Screen Recording grant /
/// window not open / off-macOS).
pub(crate) fn cmd_window(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
) -> String {
    use crate::app_introspect::AuxTarget;
    // Optional leading target keyword: `window [front|prefs|perf] [path]`. A first token
    // that is not a known keyword is the PATH (default front), preserving `window [path]`.
    let mut it = rest.split_whitespace();
    let first = it.next().unwrap_or("");
    let (aux, path_arg) = match AuxTarget::parse(first) {
        Some(t) if !first.is_empty() => (t, it.next().unwrap_or("")),
        _ => (AuxTarget::Front, rest.trim()),
    };
    let default_name = match aux {
        AuxTarget::Front => "aterm-window.png",
        AuxTarget::Prefs => "aterm-prefs.png",
        AuxTarget::Perf => "aterm-perf.png",
        AuxTarget::About => "aterm-about.png",
        AuxTarget::Menu => "aterm-menu.png",
        AuxTarget::Update => "aterm-update.png",
    };
    let requested = {
        let p = path_arg.trim();
        if p.is_empty() { default_name } else { p }
    };
    let Some(confined) = control_auth::confine_image_path(sock_dir, requested) else {
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("window write '{requested}'"),
            aterm_containment::mode_or_containment(),
            "path escapes images/ subdir or names a nested target",
        );
        return "ERR path: give a bare filename (no '/'); captures are confined to the \
                app's Application Support images/ dir. Omit the path to auto-name one — \
                the OK reply prints the full written path.\n"
            .to_string();
    };
    // For the reply only — the writer re-opens via the dir fd, not this string.
    let path = confined.display_path().to_string_lossy().into_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    // Front uses the unchanged `CaptureWindow` (sacred path); aux windows use the new
    // `CaptureAuxWindow` (resolved by their own window number on the main thread).
    let wake = match aux {
        AuxTarget::Front => Wake::CaptureWindow {
            path: confined,
            reply: tx,
        },
        _ => Wake::CaptureAuxWindow {
            target: aux,
            path: confined,
            reply: tx,
        },
    };
    if proxy.send_event(wake).is_err() {
        return "ERR event loop gone\n".to_string();
    }
    // Same wedge guard as `cmd_image`: the photograph + encode is fast; only a
    // stuck main thread or dead worker reaches the deadline.
    match rx.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok(Ok((w, h))) => format!("OK {w} {h} {path}\n"),
        // The main thread's clear, actionable message (missing permission / headless /
        // window not open / off-macOS / capture failure) is surfaced as a single `ERR`.
        Ok(Err(msg)) => format!("ERR {msg}\n"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            "ERR window: timed out waiting for the capture/encode\n".to_string()
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            "ERR window capture failed\n".to_string()
        }
    }
}

/// Parsed `video` verb arguments, every numeric already CLAMPED to its lawful
/// range (secs 0.5..=60, fps 1..=120, budget 64..=4096 MiB) so the consumer
/// never re-validates. Split from [`cmd_video`] so the parse laws are pure
/// unit-testable (no event loop needed).
#[derive(Debug)]
struct VideoArgs {
    secs: f64,
    full_res: bool,
    keys: bool,
    pace: bool,
    /// `fps=<n>` capture cap; `None` captures every present.
    fps: Option<u32>,
    /// Frame-store RAM budget (from `budget=<MiB>`; default 512 MiB).
    budget_bytes: usize,
}

const VIDEO_USAGE: &str = "usage: video <seconds> [full] [keys] [pace] [fps=<n>] [budget=<MiB>] | video status|stop | video frames [count=N]";

/// Default / max frames returned by `video frames` (the top-delta key frames).
const VIDEO_FRAMES_DEFAULT: usize = 8;
const VIDEO_FRAMES_MAX: usize = 64;

/// The newest recording dir under `root` with a VALID `index.json`. A still-encoding
/// recording has no index yet, and a TORN index (a short write / crash / ENOSPC
/// mid-write leaves the file present but not valid JSON) is SKIPPED — so a poisoned
/// newest artifact does not shadow a good prior recording; the reader falls back to
/// the next-newest that parses. Server-named `rec-<epoch>-<nnn>` stamps sort
/// oldest-first, so this walks newest→oldest and returns the first that parses.
fn newest_recording_with_index(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut recs: Vec<std::path::PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("rec-"))
        })
        .collect();
    recs.sort();
    recs.into_iter().rev().find(|p| index_json_valid(p))
}

/// Whether `<rec>/index.json` exists AND parses as JSON with a `frames` array — the
/// completion marker's integrity check (a torn/partial write is present but invalid).
fn index_json_valid(rec: &std::path::Path) -> bool {
    std::fs::read_to_string(rec.join("index.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|v| v.get("frames").is_some_and(serde_json::Value::is_array))
}

/// `video frames [count=N]` — read the newest finished recording's `index.json`
/// and emit its N highest-`delta` frames as `OK <n>` + n `frame …` rows. The
/// `delta` (fingerprint movement from the previous captured frame) is already in
/// the index, so this is a cheap read that hands an AI just the eventful frames
/// (the visual key moments) instead of the whole PNG sequence.
fn video_frames<'a>(sock_dir: &std::path::Path, args: impl Iterator<Item = &'a str>) -> String {
    let mut count = VIDEO_FRAMES_DEFAULT;
    for t in args {
        if let Some(v) = t.strip_prefix("count=") {
            match v.parse::<usize>() {
                Ok(n) if n >= 1 => count = n.min(VIDEO_FRAMES_MAX),
                _ => {
                    return format!("ERR video frames: bad count '{t}' (1..={VIDEO_FRAMES_MAX})\n");
                }
            }
        } else {
            return format!(
                "ERR video frames: unknown arg '{t}' (usage: video frames [count=N])\n"
            );
        }
    }
    let root = sock_dir.join(control_auth::VIDEO_DIR);
    let Some(rec) = newest_recording_with_index(&root) else {
        return "ERR video frames: no finished recording found (run `video <seconds>` first)\n"
            .to_string();
    };
    let Ok(text) = std::fs::read_to_string(rec.join("index.json")) else {
        return "ERR video frames: could not read the recording index\n".to_string();
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else {
        return "ERR video frames: recording index is not valid JSON\n".to_string();
    };
    let Some(frames) = val.get("frames").and_then(|f| f.as_array()) else {
        return "ERR video frames: recording index has no frames\n".to_string();
    };
    // (delta, n, seq, t_us, file) — sort by delta DESC, ties by capture order (n ASC).
    let mut rows: Vec<(u64, u64, u64, u64, String)> = frames
        .iter()
        .filter_map(|f| {
            Some((
                f.get("delta")?.as_u64()?,
                f.get("n")?.as_u64()?,
                f.get("seq")?.as_u64()?,
                f.get("t_us")?.as_u64()?,
                f.get("file")?.as_str()?.to_string(),
            ))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    rows.truncate(count);
    let mut out = format!("OK {}\n", rows.len());
    for (delta, n, seq, t_us, file) in rows {
        // Absolute path so an AI can open the PNG directly (same server-local
        // convention as `image <path>`; a remote driver reads over `dial`).
        out.push_str(&format!(
            "frame n={n} delta={delta} t_us={t_us} seq={seq} {}\n",
            rec.join(&file).display()
        ));
    }
    out
}

/// Parse the `video` verb's argument tail. Unknown tokens and malformed
/// `fps=`/`budget=` values are REJECTED with the usage line (never silently
/// defaulted); out-of-range numbers are clamped, not rejected.
fn parse_video_args(rest: &str) -> Result<VideoArgs, String> {
    let mut args = VideoArgs {
        secs: 3.0,
        full_res: false,
        keys: false,
        pace: false,
        fps: None,
        budget_bytes: aterm_gpu::video_tap::DEFAULT_BUDGET,
    };
    for tok in rest.split_whitespace() {
        match tok {
            "full" => args.full_res = true,
            "half" => args.full_res = false,
            "keys" => args.keys = true,
            "pace" => args.pace = true,
            t => {
                if let Some(v) = t.strip_prefix("fps=") {
                    match v.parse::<u32>() {
                        Ok(n) => args.fps = Some(n.clamp(1, 120)),
                        Err(_) => {
                            return Err(format!("ERR video: bad fps '{t}' ({VIDEO_USAGE})\n"));
                        }
                    }
                } else if let Some(v) = t.strip_prefix("budget=") {
                    match v.parse::<u64>() {
                        Ok(mib) => args.budget_bytes = (mib.clamp(64, 4096) as usize) << 20,
                        Err(_) => {
                            return Err(format!("ERR video: bad budget '{t}' ({VIDEO_USAGE})\n"));
                        }
                    }
                } else {
                    match t.parse::<f64>() {
                        Ok(s) => args.secs = s,
                        Err(_) => {
                            return Err(format!("ERR video: unknown arg '{t}' ({VIDEO_USAGE})\n"));
                        }
                    }
                }
            }
        }
    }
    args.secs = args.secs.clamp(0.5, 60.0);
    Ok(args)
}

/// `video <seconds> [full] [keys] [pace] [fps=<n>] [budget=<MiB>]` — record the
/// front window's PRESENTED frames (the exact bytes handed to present,
/// including the swapchain-only glow/chrome layers every single-frame tool
/// misses) and dump a PNG sequence + index.json. One-shot: blocks until the
/// dump's completion marker is on disk (the encode of a multi-second recording
/// can take a while — documented in the verb help). `keys` (the same-clock
/// keystroke log) is OWNER-only: recording someone's keystrokes is not a
/// screen-read.
pub(crate) fn cmd_video(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
    owner: bool,
) -> String {
    // Observability + cancel for the blocking one-shot: `video status` reads the
    // in-flight recording (or reports none), `video stop` finalizes it now — the
    // dump still answers the ORIGINAL client. Both are cheap main-thread reads.
    // `stop` TRUNCATES a recording someone else started, so it is OWNER-only —
    // mirroring the `keys` gate below; a scoped edge must not be able to cut short
    // (or, via the mode readout, probe) an owner's in-flight capture. `status` is
    // a benign read and stays any-scope.
    match rest.trim() {
        "status" => {
            return match call_main(proxy, |tx| Wake::VideoStatus { reply: tx }) {
                Ok(line) => line,
                Err(e) => format!("ERR video: {e}\n"),
            };
        }
        "stop" => {
            if !owner {
                return "ERR video: stop is owner-only (it truncates a recording)\n".to_string();
            }
            return match call_main(proxy, |tx| Wake::VideoStop { reply: tx }) {
                Ok(line) => line,
                Err(e) => format!("ERR video: {e}\n"),
            };
        }
        _ => {}
    }
    // `video frames [count=N]`: NO capture — read the newest finished recording's
    // index.json and return its N highest-`delta` frames (the visually eventful key
    // moments). A pure main-thread filesystem read, so it never touches the event
    // loop. Detect the sub-verb by first token (not exact match) so `count=` rides.
    {
        let mut toks = rest.split_whitespace();
        if toks.next() == Some("frames") {
            return video_frames(sock_dir, toks);
        }
    }
    let args = match parse_video_args(rest) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if args.keys && !owner {
        return "ERR video: keys requires owner scope (a keystroke log is not a screen-read)\n"
            .to_string();
    }
    let Some(dir) = control_auth::confine_video_dir(sock_dir) else {
        return "ERR video: could not create the recording dir\n".to_string();
    };
    let (tx, rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(Wake::Video {
            dur_ms: (args.secs * 1000.0) as u64,
            full_res: args.full_res,
            keys: args.keys,
            pace: args.pace,
            fps: args.fps,
            budget_bytes: args.budget_bytes,
            dir,
            reply: tx,
        })
        .is_err()
    {
        return "ERR event loop gone\n".to_string();
    }
    // The reply arrives only after index.json is on disk (recording duration +
    // the PNG encode burst) — wait generously (scaled with the recording so a
    // long take cannot falsely time out mid-encode), but never forever.
    match rx.recv_timeout(std::time::Duration::from_secs_f64(args.secs + 120.0)) {
        Ok(reply) => reply,
        Err(_) => "ERR video: timed out waiting for the recording dump\n".to_string(),
    }
}

/// `controls <target>` -> dump a compatibility GUI target's controls as text: the
/// Settings preference rows (`field key=… label=… value=… effective=…`) or the
/// Performance control panel's toggles (`toggle key=… label=… enabled=…`). The analogue
/// of `chrome` for the settings/perf GUIs — so an AI can SEE what those screens show and
/// their current values WITHOUT a screenshot. `<target>` is `prefs`/`settings` or
/// `perf`/`performance` (an unknown target is rejected with a clear `ERR`).
///
/// Unlike the pixel `window` capture, this works HEADLESS and needs no Screen Recording
/// grant: the main thread builds the lines from the PURE config/panel model
/// (`App::read_aux_controls`), not by walking AppKit views, so the window need not even
/// be open. Framed `OK <n>\n` + `<n>` rows, the SAME multi-line shape as `chrome`/`text`.
pub(crate) fn cmd_controls(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    use crate::app_introspect::AuxTarget;
    let trimmed = rest.trim();
    // The aux windows AND the front window's modal overlay have a controls surface.
    // `front` (and a bare/empty arg, which `parse` maps to Front) reports the front
    // window's open overlay slot (open/closed + kind + fp + scroll extent) — headless-safe.
    let target = match AuxTarget::parse(trimmed) {
        Some(
            t @ (AuxTarget::Front
            | AuxTarget::Prefs
            | AuxTarget::Perf
            | AuxTarget::About
            | AuxTarget::Menu
            | AuxTarget::Update),
        ) => t,
        _ => {
            return format!(
                "ERR unsupported target {trimmed:?} (use: front | prefs | perf | about | menu | update)\n"
            );
        }
    };
    let lines = match call_main(proxy, |tx| Wake::ReadAuxControls { target, reply: tx }) {
        Ok(lines) => lines,
        Err(e) => return format!("ERR {e}\n"),
    };
    // Same `OK <n>\n` + n-rows framing `chrome`/`text` use, so the aterm-ctl client
    // prints the rows verbatim (it lists `controls` among the multi-line verbs).
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Read the versioned semantic surface of native tab applications. Parsing is
/// completed on the control thread, but stable-view resolution and compilation
/// happen atomically on the main thread against the canonical live state.
pub(crate) fn cmd_inspect(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let request = match crate::app_control::parse_inspect(rest) {
        Ok(request) => request,
        Err(error) => return format!("ERR {error}\n"),
    };
    let lines = match call_main(proxy, |reply| Wake::InspectApp { request, reply }) {
        Ok(Ok(lines)) => lines,
        Ok(Err(error)) => return format!("ERR {error}\n"),
        Err(error) => return format!("ERR {error}\n"),
    };
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Dispatch one exact semantic action to a stable native view. The main-thread
/// handler revalidates view identity, UI key, action, enabled state, and value
/// type in one event-loop turn before entering the reducer/effect host.
pub(crate) fn cmd_act(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let request = match crate::app_control::parse_act(rest) {
        Ok(request) => request,
        Err(error) => return format!("ERR {error}\n"),
    };
    match call_main(proxy, |reply| Wake::ActApp { request, reply }) {
        Ok(Ok(message)) => format!("OK {message}\n"),
        Ok(Err(error)) => format!("ERR {error}\n"),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `open <target> [close]` -> open or close a compatibility GUI target. `prefs`,
/// `about`, and `update` resolve to routes in the native Settings tab; `menu` is a
/// transient palette and `perf`/`performance` is the Performance control panel.
/// The versioned, explicit forms are `open app settings [/route]` and
/// `inspect app/v1 ...`. Reuses the SAME open paths as human menu items.
///
/// MAIN-THREAD HOP: touching `App` state (and building the perf `NSWindow`) may ONLY
/// happen on the main thread, but this runs on a background control thread — so we post
/// [`Wake::OpenAuxWindow`] + a one-shot reply and BLOCK; the main thread opens the
/// surface and replies `Ok(())` (now open) or `Err(msg)` (no front window; for `perf`:
/// headless / off-macOS). Single-line `OK opened <target>` / `ERR`.
pub(crate) fn cmd_open(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    use crate::app_introspect::AuxTarget;
    let trimmed = rest.trim();
    if trimmed.split_whitespace().next() == Some("app") {
        let request = match crate::app_control::parse_open(trimmed) {
            Ok(request) => request,
            Err(error) => return format!("ERR {error}\n"),
        };
        return match call_main(proxy, |reply| Wake::OpenApp { request, reply }) {
            Ok(Ok(message)) => format!("OK {message}\n"),
            Ok(Err(error)) => format!("ERR {error}\n"),
            Err(error) => format!("ERR {error}\n"),
        };
    }
    // `open <target> close` is the symmetric half: native Settings targets close
    // through the tab lifecycle and the transient palette uses its overlay exit.
    let (target_tok, close) = match trimmed.split_once(char::is_whitespace) {
        Some((t, r)) if r.trim() == "close" => (t, true),
        Some(_) => {
            return "ERR usage: open <prefs|perf|about|menu|update> [close]\n".to_string();
        }
        None => (trimmed, false),
    };
    // Only the aux windows can be opened; `front` is always open (and a bare/empty arg
    // maps to Front) — reject with the verb's advertised `prefs | perf` contract.
    let target = match AuxTarget::parse(target_tok) {
        Some(
            t @ (AuxTarget::Prefs
            | AuxTarget::Perf
            | AuxTarget::About
            | AuxTarget::Menu
            | AuxTarget::Update),
        ) => t,
        _ => {
            return format!(
                "ERR unsupported target {target_tok:?} (use: prefs | perf | about | menu | update)\n"
            );
        }
    };
    match call_main(proxy, |tx| Wake::OpenAuxWindow {
        target,
        close,
        reply: tx,
    }) {
        Ok(Ok(())) => format!(
            "OK {} {}\n",
            if close { "closed" } else { "opened" },
            target.keyword()
        ),
        Ok(Err(msg)) => format!("ERR {msg}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `settings [open|close|toggle]` -> control the process-singleton native Settings tab
/// (`open settings` opens the same app; this verb adds close/toggle and replies with
/// the resulting state). Bare arg / `toggle` toggles; `open`/`close` force the state.
/// `inspect app/v1 tabs` discovers its stable view and `image` captures it headlessly.
/// Posts to the main
/// thread (the sole `App` mutator) and BLOCKS on a one-shot reply carrying the resulting
/// open state. Single-line `OK settings open` / `OK settings closed` / `ERR …`.
pub(crate) fn cmd_settings_overlay(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    // `settings set <key> <value…>` / `settings unset <key>`: commit ONE field by
    // key through the validated config seam (works with the Settings tab closed and
    // headless). Keys are what `controls prefs` prints; a value may contain spaces.
    let t = rest.trim();
    let (word, tail) = match t.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim_start()),
        None => (t, ""),
    };
    if word == "set" || word == "unset" {
        let (key, value) = if word == "set" {
            match tail.split_once(char::is_whitespace) {
                Some((k, v)) if !v.trim().is_empty() => (k, Some(v.trim().to_string())),
                _ => {
                    return "ERR usage: settings set <key> <value…> | settings unset <key>\n"
                        .to_string();
                }
            }
        } else {
            if tail.is_empty() || tail.split_whitespace().count() != 1 {
                return "ERR usage: settings unset <key>\n".to_string();
            }
            (tail, None)
        };
        let key = key.to_string();
        return match call_main(proxy, |tx| Wake::SetSettingsField {
            key,
            value,
            reply: tx,
        }) {
            Ok(Ok(status)) => format!("OK {status}\n"),
            Ok(Err(e)) => format!("ERR {e}\n"),
            Err(e) => format!("ERR {e}\n"),
        };
    }
    // `settings section <name>`: land the OPEN surface on a category (skipping the
    // §L landing hero) — the driver's Get-started + sidebar click in one hop.
    if word == "section" {
        let want = tail.trim().to_lowercase();
        let Some(section) = crate::prefs::Section::ORDER
            .iter()
            .copied()
            .find(|s| s.label().to_lowercase() == want)
        else {
            let names: Vec<String> = crate::prefs::Section::ORDER
                .iter()
                .map(|s| s.label().to_lowercase())
                .collect();
            return format!(
                "ERR unknown section {:?} (use: {})\n",
                tail.trim(),
                names.join(" | ")
            );
        };
        return match call_main(proxy, |tx| Wake::SettingsShowSection { section, reply: tx }) {
            Ok(Ok(())) => format!("OK settings section {}\n", section.label().to_lowercase()),
            Ok(Err(e)) => format!("ERR {e}\n"),
            Err(e) => format!("ERR {e}\n"),
        };
    }
    let open = match rest.trim() {
        "" | "toggle" => None,
        "open" | "on" | "show" => Some(true),
        "close" | "off" | "hide" => Some(false),
        other => {
            return format!(
                "ERR unsupported {other:?} (use: open | close | toggle | section <name>)\n"
            );
        }
    };
    match call_main(proxy, |tx| Wake::SettingsOverlay { open, reply: tx }) {
        Ok(Some(true)) => "OK settings open\n".to_string(),
        Ok(Some(false)) => "OK settings closed\n".to_string(),
        Ok(None) => "ERR no front window\n".to_string(),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `invoke <action>` -> fire a menu action BY NAME — the `action=` tokens `controls
/// menu` prints (the `MenuAction` Debug names, e.g. `NewTab`) — through the SAME
/// single dispatch sink the native menu bar and the ⌘K palette use. The live
/// palette row's `enabled` (the `validateMenuItem:` conditions) gates it: a
/// disabled action is a named ERR, never a silent no-op. Main-thread hop like
/// `open`/`controls` (one-shot reply channel); works headless.
pub(crate) fn cmd_invoke(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let name = rest.trim();
    if name.is_empty() || name.split_whitespace().count() != 1 {
        return "ERR usage: invoke <action>   (list actions with `controls menu`)\n".to_string();
    }
    let name = name.to_string();
    match call_main(proxy, |tx| Wake::InvokeMenuAction { name, reply: tx }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `rain [status|on|off|toggle]` -> the PHOSPHOR per-session matrix-rain
/// override on the focused window's FRONT session (the same per-session state
/// View ▸ Matrix Rain and the `toggle_matrix_rain` keybinding flip). `status`
/// is the observability face scripts/tests read:
/// `OK config_enabled=<bool> session_override=<none|on|off> effective=<bool>`.
/// Main-thread hop like `open`/`controls` (one-shot reply channel); works
/// headless. Runtime-only — nothing durable is written (the Settings switch /
/// `settings set` own the `[matrix_rain]` config bit).
pub(crate) fn cmd_rain(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let op = match rest.trim() {
        "" | "status" => crate::RainCtlOp::Status,
        "on" => crate::RainCtlOp::On,
        "off" => crate::RainCtlOp::Off,
        "toggle" => crate::RainCtlOp::Toggle,
        other => {
            return format!("ERR usage: rain [status|on|off|toggle] (got {other:?})\n");
        }
    };
    match call_main(proxy, |tx| Wake::RainControl { op, reply: tx }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `spawn [cwd=<path>]` -> mint ONE new tab session in the frontmost window and
/// reply `OK <sid>` — birth as a socket primitive. The sid is live in the registry
/// before the reply is sent, so `@<sid> …` works immediately: an orchestrator
/// stands up a fleet with a loop of spawn calls and drives each newborn with
/// `turn`/`send`/`subscribe`, no process management. `cwd=<path>` sets the
/// newborn's working directory (default: inherit the focused pane's cwd, like
/// Cmd-T). The newborn runs the default shell; give it a command with
/// `@<sid> turn '<cmd>'`. Main-thread hop like `open`/`controls`.
pub(crate) fn cmd_spawn(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let mut cwd: Option<String> = None;
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix("cwd=") {
            cwd = Some(v.to_string());
        } else {
            return "ERR usage: spawn [cwd=<path>]\n".to_string();
        }
    }
    match call_main(proxy, |tx| Wake::SpawnSession { cwd, reply: tx }) {
        Ok(Ok(sid)) => format!("OK {sid}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `@<sid> close` -> retire the resolved session by id (the death half of `spawn`):
/// close the tab hosting it through the same teardown the ✕ uses. Reply
/// `OK closed <sid>` on success, `ERR <why>` if unknown or the close was refused
/// (a running job armed the last-tab quit-confirm). Main-thread hop like `spawn`.
pub(crate) fn cmd_close(proxy: &EventLoopProxy<Wake>, session: u64, sid: &str) -> String {
    match call_main(proxy, |tx| Wake::CloseSession { session, reply: tx }) {
        Ok(Ok(())) => format!("OK closed {sid}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `chrome` -> dump the frontmost window's NATIVE macOS UI: its `NSToolbar` items
/// (each `id=<identifier> label="<label>"`, e.g. the "+" New Tab button) and the
/// app menu bar (`menu "File": New Window, New Tab, ...`). A read-only
/// introspection verb so an AI driving aterm can SEE and verify the native chrome
/// — which `image`/`text` CANNOT capture, as they render only the terminal content
/// view, never the OS toolbar/menu bar.
///
/// MAIN-THREAD HOP (mirrors [`cmd_image`]): AppKit objects (`NSToolbar`/`NSMenu`/
/// `NSWindow`) may ONLY be touched on the main thread, but this runs on a
/// background control thread. So we build a one-shot reply channel, post
/// [`Wake::ReadChrome`] to wake the event loop, and BLOCK on the reply; the main
/// thread reads the chrome (`App::read_native_chrome`) and sends back the text
/// lines. The lines are returned in the SAME multi-line shape as `text`:
/// `OK <n>\n` followed by `<n>` data rows.
///
/// Off macOS the main thread replies with one explanatory line (no native chrome),
/// so the wire shape (`OK 1` + one row) is identical on every platform.
pub(crate) fn cmd_chrome(proxy: &EventLoopProxy<Wake>) -> String {
    let lines = match call_main(proxy, |tx| Wake::ReadChrome { reply: tx }) {
        Ok(lines) => lines,
        Err(e) => return format!("ERR {e}\n"),
    };
    // Same `OK <n>\n` + n-rows framing the `text` verb uses, so the aterm-ctl client
    // prints the rows verbatim (it lists `chrome` among the multi-line verbs).
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod video_parse_tests {
    use super::*;

    fn native_image_metadata() -> crate::control::ImageFrameMetadata {
        crate::control::ImageFrameMetadata {
            frame_kind: "native",
            phase: "presented",
            window: 3,
            view: Some(42),
            generation: Some(7),
            config_revision: Some(11),
            update_revision: Some(13),
            document_seq: Some(17),
            presentation_revision: Some(19),
            paint_revision: Some(21),
            capture_serial: 23,
            width: 800,
            height: 600,
            pixel_fingerprint: 0x1111_2222,
            compiled_fingerprint: Some(0x1234_abcd),
            raster_fingerprint: 0x3333_4444,
            raster_model_fingerprint: 0x5555_6666,
            raster_geometry: 0x7777_8888,
            overlay_fingerprint: 0x9999_aaaa,
            theme_fingerprint: 0xbbbb_cccc,
            leaves: vec![crate::control::ImageLeafFrameMetadata {
                kind: "native",
                view: 42,
                session: None,
                focused: true,
                width: 800,
                height: 600,
                snapshot_seq: None,
                instance: Some(5),
                generation: Some(7),
                geometry: Some(9),
                config_revision: Some(11),
                update_revision: Some(13),
                document_seq: Some(17),
                presentation_revision: Some(19),
                paint_revision: Some(21),
                compiled_fingerprint: Some(0x1234_abcd),
                raster_fingerprint: Some(0x3333_4444),
            }],
        }
    }

    #[test]
    fn image_meta_is_an_opt_in_flag_and_preserves_legacy_names() {
        assert_eq!(
            parse_image_options("plain --meta --bytes shot.png"),
            ImageOptions {
                want_bytes: true,
                want_metadata: true,
                rest: "plain shot.png".to_string(),
            }
        );
        assert_eq!(
            parse_image_options("meta"),
            ImageOptions {
                want_bytes: false,
                want_metadata: false,
                rest: "meta".to_string(),
            },
            "only --meta is reserved; an existing file literally named meta still works",
        );
    }

    #[test]
    fn image_meta_protocol_is_additive_and_keeps_legacy_replies_exact() {
        let metadata = native_image_metadata();
        assert_eq!(
            image_file_reply(800, 600, "/tmp/legacy.png", false, Some(&metadata)),
            "OK 800 600 /tmp/legacy.png\n",
        );
        let file = image_file_reply(800, 600, "/tmp/frame.png", true, Some(&metadata));
        assert!(file.starts_with("OK 800 600 image-meta image-meta-version=2 frame-kind=native "));
        for field in [
            "image-meta-version=2",
            "frame-phase=presented",
            "view=42",
            "generation=7",
            "config-revision=11",
            "update-revision=13",
            "document-seq=17",
            "presentation-revision=19",
            "capture-serial=23",
            "dimensions=800x600",
            "pixel-fingerprint=0000000011112222",
            "compiled-fingerprint=000000001234abcd",
            "leaf-count=1",
            "leaves=v42:native:s-:f1:",
            "path=\"/tmp/frame.png\"",
        ] {
            assert!(file.contains(field), "missing {field}: {file}");
        }

        let png = [0x89, b'P', b'N', b'G'];
        assert_eq!(
            image_bytes_reply(2, 3, &png, false, Some(&metadata)),
            "OK 1\n2 3 4 iVBORw==\n",
        );
        let bytes = image_bytes_reply(2, 3, &png, true, Some(&metadata));
        let lines = bytes.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "OK 2");
        assert!(lines[1].starts_with("image-meta image-meta-version=2 frame-kind=native "));
        assert!(lines[1].contains("capture-serial=23"));
        assert_eq!(lines[2], "2 3 4 iVBORw==");

        let unavailable = image_file_reply(4, 5, "/tmp/unknown.png", true, None);
        assert!(unavailable.contains(
            "image-meta-version=2 frame-kind=unknown frame-phase=rendered identity=unavailable"
        ));
    }

    /// Bare `video` keeps the historical defaults: 3s, half-res, no keys/pace,
    /// no fps cap, the 512 MiB DEFAULT_BUDGET.
    #[test]
    fn video_args_defaults() {
        let a = parse_video_args("").expect("defaults parse");
        assert_eq!(a.secs, 3.0);
        assert!(!a.full_res && !a.keys && !a.pace);
        assert_eq!(a.fps, None);
        assert_eq!(a.budget_bytes, aterm_gpu::video_tap::DEFAULT_BUDGET);
    }

    /// PHASE-2 LAW: `budget=<MiB>` and `fps=<n>` are accepted alongside the
    /// existing flags and carried through exactly.
    #[test]
    fn video_args_budget_and_fps_accepted() {
        let a = parse_video_args("5 full keys pace fps=10 budget=1024").expect("parses");
        assert_eq!(a.secs, 5.0);
        assert!(a.full_res && a.keys && a.pace);
        assert_eq!(a.fps, Some(10));
        assert_eq!(a.budget_bytes, 1024 << 20);
    }

    /// Out-of-range numbers CLAMP (secs 0.5..=60, fps 1..=120,
    /// budget 64..=4096 MiB) — never reject, never pass through raw.
    #[test]
    fn video_args_clamped() {
        assert_eq!(parse_video_args("0.1").unwrap().secs, 0.5);
        assert_eq!(parse_video_args("999").unwrap().secs, 60.0);
        assert_eq!(parse_video_args("fps=0").unwrap().fps, Some(1));
        assert_eq!(parse_video_args("fps=500").unwrap().fps, Some(120));
        assert_eq!(parse_video_args("budget=8").unwrap().budget_bytes, 64 << 20);
        assert_eq!(
            parse_video_args("budget=999999").unwrap().budget_bytes,
            4096 << 20
        );
    }

    /// Unknown tokens and malformed key=value forms are still REJECTED with
    /// the usage error — the new args must not turn typos into defaults.
    #[test]
    fn video_args_rejects_unknown_and_malformed() {
        for bad in [
            "bogus",
            "fps=abc",
            "fps=",
            "budget=abc",
            "budget=",
            "fps=-1",
        ] {
            let err = parse_video_args(bad).expect_err(bad);
            assert!(err.starts_with("ERR video:"), "{bad}: {err}");
            assert!(err.contains("usage: video"), "{bad}: {err}");
        }
    }

    /// A minimal fake recording under `<sock_dir>/video/<rec>/index.json` with the
    /// exact frame-object shape `app_introspect` writes, for the `video frames` read.
    fn write_fake_recording(sock_dir: &std::path::Path, rec: &str, frames: &[(u64, u64)]) {
        let dir = sock_dir.join(super::control_auth::VIDEO_DIR).join(rec);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        for (i, (delta, t_us)) in frames.iter().enumerate() {
            if i > 0 {
                lines.push_str(",\n");
            }
            let n = i + 1;
            lines.push_str(&format!(
                "    {{\"n\":{n},\"seq\":{n},\"t_us\":{t_us},\"fp\":0,\"delta\":{delta},\"file\":\"frame_{n:04}.png\"}}"
            ));
        }
        std::fs::write(
            dir.join("index.json"),
            format!("{{\"frames\":[\n{lines}\n]}}"),
        )
        .unwrap();
    }

    /// `video frames` returns the highest-`delta` frames of the NEWEST finished
    /// recording, ordered most-changed first, framed `OK <n>` + n rows — and honors
    /// `count=`. A recording DIR with no index.json (still encoding) is skipped.
    #[test]
    fn video_frames_ranks_by_delta_and_picks_newest() {
        let tmp = std::env::temp_dir().join(format!("aterm-vf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Older recording (should be ignored once a newer complete one exists).
        write_fake_recording(&tmp, "rec-1000000000-000", &[(5, 10), (1, 20)]);
        // A still-encoding dir: newest name, but NO index.json -> must be skipped.
        std::fs::create_dir_all(
            tmp.join(super::control_auth::VIDEO_DIR)
                .join("rec-1000000002-000"),
        )
        .unwrap();
        // Newest COMPLETE recording: deltas 3, 40, 7, 2 across four frames.
        write_fake_recording(
            &tmp,
            "rec-1000000001-000",
            &[(3, 0), (40, 16), (7, 33), (2, 50)],
        );

        // count=2 -> the two highest deltas (40 then 7), most-changed first.
        let out = video_frames(&tmp, "count=2".split_whitespace());
        assert!(out.starts_with("OK 2\n"), "framed header: {out:?}");
        let body: Vec<&str> = out.lines().skip(1).collect();
        assert!(
            body[0].contains("delta=40") && body[0].contains("n=2"),
            "top: {out:?}"
        );
        assert!(
            body[1].contains("delta=7") && body[1].contains("n=3"),
            "second: {out:?}"
        );
        // It read the NEWEST complete recording, not the older one (delta=5 max there).
        assert!(
            !out.contains("delta=5"),
            "ignored the older recording: {out:?}"
        );

        // No count -> default cap, all four frames, still delta-ordered.
        let all = video_frames(&tmp, std::iter::empty());
        assert!(all.starts_with("OK 4\n"), "all frames: {all:?}");

        // Bad count / unknown arg are rejected.
        assert!(video_frames(&tmp, "count=0".split_whitespace()).starts_with("ERR "));
        assert!(video_frames(&tmp, "bogus".split_whitespace()).starts_with("ERR "));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// With no recordings at all, `video frames` is a clean `ERR`, not a panic.
    #[test]
    fn video_frames_errors_when_no_recording() {
        let tmp = std::env::temp_dir().join(format!("aterm-vf-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = video_frames(&tmp, std::iter::empty());
        assert!(out.starts_with("ERR video frames:"), "{out:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
