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
            // State only what expiring this deadline actually proves: no reply
            // arrived in time. The former text guessed "(event loop wedged?)",
            // and that guess was WRONG for the first real report it produced --
            // a request abandoned in the config queue by a loop that was turning
            // tens of thousands of times a second and answering every other verb
            // instantly -- while costing an hour of hunting for a stall that did
            // not exist. A timeout cannot distinguish a wedged loop from a slow
            // turn from a request nobody ever settles, so it must not name one.
            Err("main-thread reply did not arrive within 30s")
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
///
/// GATHER-THEN-SERIALIZE, like the styled-frame path: everything the reply needs
/// (anchors, tile indices, and an `Arc` clone of each payload) is collected under
/// ONE lock hold, then the guard is dropped and the (up to multi-MiB per image)
/// base64 encode runs with the lock RELEASED. That mutex is the same one the PTY
/// reader's `process()` and the renderer's frame snapshot take — holds there are
/// budgeted in tens of microseconds, and an on-lock encode of a 4 MiB image is
/// milliseconds. The bytes are `Arc`-shared and immutable once placed, so encoding
/// after the drop yields byte-identical output.
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
        // Gather only: the FIRST tile at that column (what the old early `return`
        // picked), carried out of the lock scope as an `Arc` clone.
        let hit = t
            .images_row(r)
            .into_iter()
            .find(|(col, _)| *col == c)
            .map(|(col, iref)| {
                let anchor_r = r.saturating_sub(iref.cell_row as usize);
                let anchor_c = col.saturating_sub(iref.cell_col as usize);
                (anchor_r, anchor_c, iref.cell_row, iref.cell_col, iref.image)
            });
        drop(t);
        let Some((anchor_r, anchor_c, cell_row, cell_col, img)) = hit else {
            return "ERR none\n".to_string();
        };
        return format!(
            "OK 1\n{}\n",
            image_read_line(anchor_r, anchor_c, cell_row, cell_col, &img)
        );
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
    // Anchor + payload handle per distinct image, in the same walk order the reply
    // uses. Holding the `Arc` keeps each payload alive (and keeps the `seen`
    // pointer-identity dedup valid) after the guard drops.
    let mut hits: Vec<(usize, usize, Arc<ImageData>)> = Vec::new();
    for r in row_range {
        for (col, iref) in t.images_row(r) {
            let ptr = std::sync::Arc::as_ptr(&iref.image);
            if seen.contains(&ptr) {
                continue;
            }
            seen.push(ptr);
            let anchor_r = r.saturating_sub(iref.cell_row as usize);
            let anchor_c = col.saturating_sub(iref.cell_col as usize);
            hits.push((anchor_r, anchor_c, iref.image));
        }
    }
    drop(t);
    let mut out = format!("OK {}\n", hits.len());
    for (anchor_r, anchor_c, img) in hits {
        // Whole-image report: anchor + tile 0/0 (the full payload is carried).
        out.push_str(&image_read_line(anchor_r, anchor_c, 0, 0, &img));
        out.push('\n');
    }
    out
}

/// `image [path]` -> hand the render to the MAIN thread (it owns the renderer),
/// block on the guarded result, and report `OK <w> <h> <path>\n`. The encode
/// worker transfers that result only after the PNG is fully written; the socket
/// writer revalidates it and retains its identity through the complete response
/// challenge and causal client ACK (or the failed-handoff quarantine).
///
/// PATH SAFETY: the PNG is confined to the `images/` subdir of the per-user
/// socket directory. A bare name (`image shot.png`) lands there; an empty
/// request gets a server-unique path below `images/auto/<process-instance>/`.
/// A path that would escape the subdir (`../`, an absolute path elsewhere, a symlink out) is refused with
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

struct CancelCaptureRequestOnDrop(Option<crate::control::CaptureCancellation>);

impl CancelCaptureRequestOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }

    fn cancel_won(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(crate::control::CaptureCancellation::cancel)
    }
}

impl Drop for CancelCaptureRequestOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            let _ = cancel.cancel();
        }
    }
}

fn recv_capture_reply<T>(
    rx: &std::sync::mpsc::Receiver<Result<crate::control::Retained<T>, String>>,
    cancel_on_drop: &mut CancelCaptureRequestOnDrop,
    timeout: std::time::Duration,
    context: &str,
) -> Result<crate::control::Retained<T>, String> {
    match rx.recv_timeout(timeout) {
        Ok(result) => {
            cancel_on_drop.disarm();
            result
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) if cancel_on_drop.cancel_won() => {
            cancel_on_drop.disarm();
            Err(format!("{context}: timed out; publication cancelled"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // The worker won the one-shot election immediately before the final
            // name operation. Cancellation can no longer make an ERR truthful:
            // wait for the committed result or an actual worker disconnect.
            match rx.recv() {
                Ok(result) => {
                    cancel_on_drop.disarm();
                    result
                }
                Err(_) => {
                    cancel_on_drop.disarm();
                    Err(format!(
                        "{context}: capture worker disconnected after commit authorization"
                    ))
                }
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(format!("{context}: capture worker disconnected"))
        }
    }
}

pub(crate) fn cmd_image(
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    rest: &str,
    sock_dir: &std::path::Path,
    session: Option<u64>,
) -> crate::control::ControlReply {
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
    let target = if rest.is_empty() {
        let stem = if clean {
            "aterm-clean"
        } else {
            "aterm-control"
        };
        let Some(target) = control_auth::confine_automatic_image_path(sock_dir, stem) else {
            return "ERR image: could not create the automatic capture path\n".into();
        };
        target
    } else {
        let requested = rest.to_string();
        let Some(target) = control_auth::confine_image_path(sock_dir, &requested) else {
            log_denial(
                AUDIT_SUBSYSTEM,
                &format!("image write '{requested}'"),
                aterm_containment::mode_or_containment(),
                "path escapes images/ subdir or names a nested target",
            );
            return "ERR path: give a bare filename (no '/'); captures are confined to the \
                app's Application Support images/ dir. Omit the path to auto-name one — \
                the OK reply prints the full written path.\n"
                .into();
        };
        target
    };
    // For the reply only — the writer re-opens via the dir fd, not this string.
    let path = target.display_path().to_string_lossy().into_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = crate::control::CaptureCancellation::new();
    let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
    let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    queue.lock().unwrap().push_back(ImageReq {
        target,
        clean,
        session,
        want_bytes,
        want_metadata,
        frame_metadata: std::sync::Arc::clone(&frame_metadata),
        cancel,
        reply: tx,
    });
    if proxy.send_event(Wake::Control).is_err() {
        return "ERR event loop gone\n".into();
    }
    // Generous ceiling: the render + encode is tens of ms; the deadline only fires
    // when the event loop or worker is wedged, where blocking the control thread for
    // the client's whole 900 s exchange window would wedge the verb too.
    match recv_capture_reply(
        &rx,
        &mut cancel_on_drop,
        std::time::Duration::from_secs(120),
        "image",
    ) {
        // (0,0) is the render's honest failure reply — no window shows the target
        // (a background tab, or no window at all) and NO file was written. Report
        // it as an error instead of an `OK 0 0 <path>` pointing at nothing.
        Ok(retained) if matches!(retained.value, (0, 0, _)) => {
            "ERR no window displays the target session (background tab?)\n".into()
        }
        // `--bytes`: Lines-framed `OK 1\n<w> <h> <nbytes> <base64-png>` — the control
        // dispatch replies TEXT (written as UTF-8), so the binary PNG is base64'd
        // (the same wire-safe form `image read` uses for inline images). A REMOTE
        // driver base64-decodes to get the exact pixels, with no server-local file
        // path. Dims are on the line so the driver need not parse the PNG header.
        Ok(retained) => {
            let ((w, h, png), retention) = retained.into_parts();
            let body = match png {
                Some(png) => image_bytes_reply(w, h, &png, want_metadata, frame_metadata.get()),
                None => image_file_reply(w, h, &path, want_metadata, frame_metadata.get()),
            };
            crate::control::ControlReply::guarded(body, retention)
        }
        // The encode/write failed AFTER a successful render: no file on disk.
        Err(e) => format!("ERR {e}\n").into(),
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

/// `window [<target>] [path]` -> capture a full-window artifact to a PNG,
/// replying `OK <w> <h> <path>` (the SAME wire shape as `image`). For the front
/// terminal window this stitches platform chrome around the exact submitted
/// client destination. It does not claim compositor visibility or scanout, and
/// refuses capture when translucent/material pixels cannot be represented
/// honestly. `<target>` is an optional leading keyword selecting WHICH window:
///   * (omitted) / `front` — the frontmost TERMINAL window: native macOS chrome
///     (titlebar, traffic lights, unified toolbar, full-width tab strip) AND the
///     terminal content. This is the original behavior and closes the gap `image`
///     leaves (`image` rasterizes only the content framebuffer, no OS chrome).
///   * `prefs` / `settings` — the Settings surface.
///
/// A first token that is not a known keyword is treated as the path (so the original
/// `window [path]` wire shape still works); a literal filename `prefs`/`front`
/// must therefore be given a target first (e.g. `window front prefs`).
///
/// PATH CONFINEMENT (mirrors [`cmd_image`]): the `path` is validated by
/// `confine_image_path` to a single filename inside the socket dir's `images/` subdir,
/// so the socket can never overwrite an arbitrary file. Omitted names are unique
/// inside this process instance's bounded automatic-output namespace.
///
/// MAIN-THREAD HOP (mirrors [`cmd_chrome`]): reaching a window's `NSWindow` + reading its
/// window number + calling `CGWindowListCreateImage` may ONLY happen on the main thread,
/// but this runs on a background control thread. So we post [`Wake::CaptureWindow`]
/// (front) or [`Wake::CaptureAuxWindow`] (Settings routes) with the confined target + a
/// one-shot result channel and BLOCK. The main thread captures, the encode worker writes
/// and transfers a guarded `Ok((w, h))`, and the socket server retains it through the
/// client's explicit complete-response ACK; an `Err(msg)` is surfaced verbatim (missing
/// Screen Recording grant / window not open / off-macOS).
pub(crate) fn cmd_window(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
) -> crate::control::ControlReply {
    use crate::app_introspect::AuxTarget;
    // Optional leading target keyword: `window [front|prefs] [path]`. A first token
    // that is not a known keyword is the PATH (default front), preserving `window [path]`.
    let mut it = rest.split_whitespace();
    let first = it.next().unwrap_or("");
    if matches!(first.to_ascii_lowercase().as_str(), "perf" | "performance") {
        return "ERR target perf was removed with the bottom HUD\n".into();
    }
    let (aux, path_arg) = match AuxTarget::parse(first) {
        Some(t) if !first.is_empty() => (t, it.next().unwrap_or("")),
        _ => (AuxTarget::Front, rest.trim()),
    };
    let default_stem = match aux {
        AuxTarget::Front => "aterm-window",
        AuxTarget::Prefs => "aterm-prefs",
        AuxTarget::About => "aterm-about",
        AuxTarget::Menu => "aterm-menu",
        AuxTarget::TabMenu => "aterm-tab-menu",
        AuxTarget::ConnCard => "aterm-conn-card",
        AuxTarget::SessionPicker => "aterm-session-picker",
        AuxTarget::Connections => "aterm-connections",
        AuxTarget::Update => "aterm-update",
    };
    let p = path_arg.trim();
    let confined = if p.is_empty() {
        let Some(target) = control_auth::confine_automatic_image_path(sock_dir, default_stem)
        else {
            return "ERR window: could not create the automatic capture path\n".into();
        };
        target
    } else {
        let requested = p.to_string();
        let Some(target) = control_auth::confine_image_path(sock_dir, &requested) else {
            log_denial(
                AUDIT_SUBSYSTEM,
                &format!("window write '{requested}'"),
                aterm_containment::mode_or_containment(),
                "path escapes images/ subdir or names a nested target",
            );
            return "ERR path: give a bare filename (no '/'); captures are confined to the \
                app's Application Support images/ dir. Omit the path to auto-name one — \
                the OK reply prints the full written path.\n"
                .into();
        };
        target
    };
    // For the reply only — the writer re-opens via the dir fd, not this string.
    let path = confined.display_path().to_string_lossy().into_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = crate::control::CaptureCancellation::new();
    let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
    // Front uses the unchanged `CaptureWindow` (sacred path); aux windows use the new
    // `CaptureAuxWindow` (resolved by their own window number on the main thread).
    let wake = match aux {
        AuxTarget::Front => Wake::CaptureWindow {
            path: confined,
            cancel,
            reply: tx,
        },
        _ => Wake::CaptureAuxWindow {
            target: aux,
            path: confined,
            cancel,
            reply: tx,
        },
    };
    if proxy.send_event(wake).is_err() {
        return "ERR event loop gone\n".into();
    }
    // Same wedge guard as `cmd_image`: the photograph + encode is fast; only a
    // stuck main thread or dead worker reaches the deadline.
    match recv_capture_reply(
        &rx,
        &mut cancel_on_drop,
        std::time::Duration::from_secs(120),
        "window",
    ) {
        Ok(retained) => {
            let ((w, h), retention) = retained.into_parts();
            crate::control::ControlReply::guarded(format!("OK {w} {h} {path}\n"), retention)
        }
        // The main thread's clear, actionable message (missing permission / headless /
        // window not open / off-macOS / capture failure) is surfaced as a single `ERR`.
        Err(msg) => format!("ERR {msg}\n").into(),
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

/// A healthy per-instance namespace converges to eight completed recordings.
/// Bound the directory walk well above that so a planted or crash-filled
/// namespace cannot make one control request scan an unbounded number of names.
const VIDEO_RECORDING_SCAN_MAX: usize = 128;

/// A 60 s / 120 fps recording plus the bounded 1024-input ledger is comfortably
/// below this. Read `MAX + 1` so an oversized regular file fails closed without
/// allocating its attacker-controlled length.
const VIDEO_INDEX_MAX_BYTES: usize = 4 * 1024 * 1024;

fn valid_recording_name(name: &str) -> bool {
    let Some(stamp) = name.strip_prefix("rec-") else {
        return false;
    };
    !stamp.is_empty()
        && stamp
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
}

fn valid_frame_file_name(file: &str) -> bool {
    let path = std::path::Path::new(file);
    if path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return false;
    }
    let Some(number) = file
        .strip_prefix("frame_")
        .and_then(|tail| tail.strip_suffix(".png"))
    else {
        return false;
    };
    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Debug)]
struct VideoFrameCandidate {
    delta: u64,
    n: u64,
    seq: u64,
    t_us: u64,
    file: String,
}

fn video_frame_candidates(index: &serde_json::Value) -> Vec<VideoFrameCandidate> {
    let Some(frames) = index.get("frames").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut candidates = frames
        .iter()
        .filter_map(|frame| {
            let file = frame.get("file")?.as_str()?;
            if !valid_frame_file_name(file) {
                return None;
            }
            Some(VideoFrameCandidate {
                delta: frame.get("delta")?.as_u64()?,
                n: frame.get("n")?.as_u64()?,
                seq: frame.get("seq")?.as_u64()?,
                t_us: frame.get("t_us")?.as_u64()?,
                file: file.to_string(),
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.delta.cmp(&a.delta).then(a.n.cmp(&b.n)));
    candidates
}

struct PinnedVideoFrame<P> {
    candidate: VideoFrameCandidate,
    path: std::path::PathBuf,
    _pin: P,
}

fn format_video_frame_rows<P>(rows: &[PinnedVideoFrame<P>]) -> String {
    let mut out = format!("OK {}\n", rows.len());
    for row in rows {
        let VideoFrameCandidate {
            delta,
            n,
            seq,
            t_us,
            ..
        } = &row.candidate;
        // Absolute path so a local AI can open the PNG directly (same server-local
        // convention as `image <path>`). A dial/TLS reply still names the remote
        // host's filesystem; use `image --bytes` when pixels must cross the relay.
        //
        // The retained handles below make this path name the validated inode at
        // the response commit point. As with every filesystem-path API, a
        // same-user process can mutate it after the reply reaches the caller.
        out.push_str(&format!(
            "frame n={n} delta={delta} t_us={t_us} seq={seq} {}\n",
            row.path.display()
        ));
    }
    out
}

#[cfg(unix)]
mod confined_video_reader {
    use std::io::Read as _;

    use rustix::fd::OwnedFd;
    use rustix::fs::{CWD, Dir, FileType, Mode, OFlags, fstat, openat};

    use super::{
        PinnedVideoFrame, VIDEO_INDEX_MAX_BYTES, VIDEO_RECORDING_SCAN_MAX, control_auth,
        format_video_frame_rows, valid_recording_name, video_frame_candidates,
    };

    const INDEX_NAME: &str = "index.json";

    #[cfg(test)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum ReadStage {
        SocketPathResolved,
        NamespacePinned,
        RecordingPinned,
        IndexPinned,
        FramePinned,
    }

    #[cfg(not(test))]
    #[derive(Clone, Copy)]
    enum ReadStage {
        SocketPathResolved,
        NamespacePinned,
        RecordingPinned,
        IndexPinned,
        FramePinned,
    }

    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    fn file_flags() -> OFlags {
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    fn open_directory_at<Fd: rustix::fd::AsFd>(
        parent: Fd,
        name: impl rustix::path::Arg,
    ) -> Option<OwnedFd> {
        openat(parent, name, directory_flags(), Mode::empty()).ok()
    }

    fn open_file_at<Fd: rustix::fd::AsFd>(
        parent: Fd,
        name: impl rustix::path::Arg,
    ) -> Option<OwnedFd> {
        openat(parent, name, file_flags(), Mode::empty()).ok()
    }

    fn is_directory(fd: &impl rustix::fd::AsFd) -> bool {
        fstat(fd)
            .ok()
            .is_some_and(|stat| FileType::from_raw_mode(stat.st_mode).is_dir())
    }

    fn is_regular_file(fd: &impl rustix::fd::AsFd) -> bool {
        fstat(fd)
            .ok()
            .is_some_and(|stat| FileType::from_raw_mode(stat.st_mode).is_file())
    }

    fn same_identity(left: &impl rustix::fd::AsFd, right: &impl rustix::fd::AsFd) -> bool {
        let (Ok(left), Ok(right)) = (fstat(left), fstat(right)) else {
            return false;
        };
        left.st_dev == right.st_dev && left.st_ino == right.st_ino
    }

    fn current_directory_matches<Fd: rustix::fd::AsFd>(
        parent: Fd,
        name: impl rustix::path::Arg,
        expected: &impl rustix::fd::AsFd,
    ) -> bool {
        open_directory_at(parent, name).is_some_and(|current| same_identity(&current, expected))
    }

    fn current_file_matches<Fd: rustix::fd::AsFd>(
        parent: Fd,
        name: impl rustix::path::Arg,
        expected: &impl rustix::fd::AsFd,
    ) -> bool {
        open_file_at(parent, name)
            .is_some_and(|current| is_regular_file(&current) && same_identity(&current, expected))
    }

    fn read_index(
        recording: &OwnedFd,
        hook: &mut impl FnMut(ReadStage),
    ) -> Option<(std::fs::File, serde_json::Value)> {
        let index_fd = open_file_at(recording, INDEX_NAME)?;
        hook(ReadStage::IndexPinned);
        if !is_regular_file(&index_fd) {
            return None;
        }
        let mut file = std::fs::File::from(index_fd);
        let mut bytes = Vec::new();
        (&mut file)
            .take((VIDEO_INDEX_MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > VIDEO_INDEX_MAX_BYTES {
            return None;
        }
        let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
        value
            .get("frames")
            .is_some_and(serde_json::Value::is_array)
            .then_some((file, value))
    }

    fn recording_names(instance: &OwnedFd) -> Result<Vec<String>, &'static str> {
        let mut directory =
            Dir::read_from(instance).map_err(|_| "could not read recording namespace")?;
        let mut scanned = 0_usize;
        let mut names = Vec::new();
        for result in &mut directory {
            let entry = result.map_err(|_| "could not inspect recording namespace")?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if scanned >= VIDEO_RECORDING_SCAN_MAX {
                return Err("recording namespace has too many entries");
            }
            scanned += 1;
            let Ok(name) = std::str::from_utf8(bytes) else {
                continue;
            };
            if valid_recording_name(name) {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// One canonical absolute directory path, retained component-by-component
    /// from `/`. `O_NOFOLLOW` only applies to the final component of one open;
    /// this chain makes it apply to every ancestor and keeps each exact inode
    /// alive until the response is committed.
    struct AbsoluteDirectory {
        root: OwnedFd,
        components: Vec<(std::ffi::OsString, OwnedFd)>,
    }

    impl AbsoluteDirectory {
        fn leaf(&self) -> &OwnedFd {
            self.components
                .last()
                .map_or(&self.root, |(_, directory)| directory)
        }

        fn still_matches(&self) -> bool {
            let Some(mut current) =
                open_directory_at(CWD, std::path::Path::new("/")).filter(is_directory)
            else {
                return false;
            };
            if !same_identity(&current, &self.root) {
                return false;
            }
            for (name, expected) in &self.components {
                let Some(next) = open_directory_at(&current, name.as_os_str()).filter(is_directory)
                else {
                    return false;
                };
                if !same_identity(&next, expected) {
                    return false;
                }
                current = next;
            }
            true
        }
    }

    fn open_absolute_directory(path: std::path::PathBuf) -> Option<AbsoluteDirectory> {
        if !path.is_absolute() {
            return None;
        }
        let root = open_directory_at(CWD, std::path::Path::new("/")).filter(is_directory)?;
        let mut components = Vec::new();
        for component in path.components() {
            let name = match component {
                std::path::Component::RootDir => continue,
                std::path::Component::Normal(name) => name,
                // `canonicalize` must eliminate `.`/`..`; Unix has no prefix.
                _ => return None,
            };
            let parent = components.last().map_or(&root, |(_, directory)| directory);
            let directory = open_directory_at(parent, name).filter(is_directory)?;
            components.push((name.to_os_string(), directory));
        }
        Some(AbsoluteDirectory { root, components })
    }

    struct Namespace {
        socket: AbsoluteDirectory,
        video: OwnedFd,
        instance: OwnedFd,
        instance_path: std::path::PathBuf,
    }

    fn open_namespace(
        sock_dir: &std::path::Path,
        instance: &str,
        hook: &mut impl FnMut(ReadStage),
    ) -> Result<Namespace, &'static str> {
        if control_auth::video_instance_root_for(sock_dir, instance).is_none() {
            return Err("recording namespace is not confined");
        }
        let socket_path =
            std::fs::canonicalize(sock_dir).map_err(|_| "recording namespace is not confined")?;
        hook(ReadStage::SocketPathResolved);
        let socket = open_absolute_directory(socket_path.clone())
            .ok_or("recording namespace is not confined")?;
        let video = open_directory_at(socket.leaf(), control_auth::VIDEO_DIR)
            .filter(is_directory)
            .ok_or("recording namespace is not confined")?;
        let instance_fd = open_directory_at(&video, instance)
            .filter(is_directory)
            .ok_or("recording namespace is not confined")?;
        let instance_path = socket_path.join(control_auth::VIDEO_DIR).join(instance);
        Ok(Namespace {
            socket,
            video,
            instance: instance_fd,
            instance_path,
        })
    }

    fn namespace_still_matches(
        namespace: &Namespace,
        recording_name: &str,
        recording: &OwnedFd,
        published_marker: &std::fs::File,
        index: &std::fs::File,
        rows: &[PinnedVideoFrame<OwnedFd>],
    ) -> bool {
        namespace.socket.still_matches()
            && current_directory_matches(
                namespace.socket.leaf(),
                control_auth::VIDEO_DIR,
                &namespace.video,
            )
            && current_directory_matches(
                &namespace.video,
                namespace
                    .instance_path
                    .file_name()
                    .expect("validated instance component"),
                &namespace.instance,
            )
            && current_directory_matches(&namespace.instance, recording_name, recording)
            && current_file_matches(
                recording,
                control_auth::VIDEO_PUBLISHED_FILE,
                published_marker,
            )
            && current_file_matches(recording, INDEX_NAME, index)
            && rows
                .iter()
                .all(|row| current_file_matches(recording, row.candidate.file.as_str(), &row._pin))
    }

    fn read_with_hook(
        sock_dir: &std::path::Path,
        instance: &str,
        count: usize,
        mut hook: impl FnMut(ReadStage),
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        let namespace = open_namespace(sock_dir, instance, &mut hook)?;
        hook(ReadStage::NamespacePinned);
        let names = recording_names(&namespace.instance)?;

        // A still-encoding or torn newest recording is skipped. Walk newest to
        // oldest until a completion index parses from the exact retained handle.
        let mut selected = None;
        for name in names.into_iter().rev() {
            let Some(recording) =
                open_directory_at(&namespace.instance, name.as_str()).filter(is_directory)
            else {
                continue;
            };
            hook(ReadStage::RecordingPinned);
            let Some(published_marker) =
                open_file_at(&recording, control_auth::VIDEO_PUBLISHED_FILE)
                    .filter(is_regular_file)
                    .map(std::fs::File::from)
            else {
                continue;
            };
            let Some((index_file, index)) = read_index(&recording, &mut hook) else {
                continue;
            };
            selected = Some((name, recording, published_marker, index_file, index));
            break;
        }
        let Some((recording_name, recording, published_marker, index_file, index)) = selected
        else {
            return Ok(None);
        };

        // Sort manifest metadata first, then retain only the selected safe frame
        // handles. This preserves valid-frame backfill without holding thousands
        // of descriptors for a 60 s / 120 fps recording.
        let recording_path = namespace.instance_path.join(&recording_name);
        let Some(lease) = control_auth::retain_video_artifact_path(&recording_path) else {
            drop(index_file);
            drop(published_marker);
            drop(recording);
            drop(namespace);
            return Err("recording retention sweep is in progress");
        };
        let sweep_root = match crate::pinned_dir::PinnedDir::open_resolved(&namespace.instance_path)
        {
            Ok(root) => root,
            Err(_) => {
                drop(index_file);
                drop(published_marker);
                drop(recording);
                drop(namespace);
                drop(lease);
                return Err("recording namespace changed during read");
            }
        };
        let mut rows = Vec::new();
        for candidate in video_frame_candidates(&index) {
            let Some(frame) = open_file_at(&recording, candidate.file.as_str()) else {
                continue;
            };
            hook(ReadStage::FramePinned);
            if !is_regular_file(&frame) {
                continue;
            }
            let path = recording_path.join(&candidate.file);
            rows.push(PinnedVideoFrame {
                candidate,
                path,
                _pin: frame,
            });
            if rows.len() == count {
                break;
            }
        }

        if !namespace_still_matches(
            &namespace,
            &recording_name,
            &recording,
            &published_marker,
            &index_file,
            &rows,
        ) {
            // Lease release may trigger retention. Drop every exact reader
            // handle first (especially Windows deny-delete handles), then let
            // the final lease run its callback.
            drop(rows);
            drop(index_file);
            drop(published_marker);
            drop(recording);
            drop(namespace);
            drop(sweep_root);
            drop(lease);
            return Err("recording namespace changed during read");
        }
        if sweep_root.validate_path_identity().is_err()
            || !sweep_root.same_directory_fd(&namespace.instance)
            || lease
                .arm_video_reader_sweep(sweep_root, std::ffi::OsString::from(&recording_name))
                .is_err()
        {
            drop(rows);
            drop(index_file);
            drop(published_marker);
            drop(recording);
            drop(namespace);
            drop(lease);
            return Err("recording namespace changed while arming retention");
        }
        let output = format_video_frame_rows(&rows);
        let retention = crate::control::ReplyRetention::new(VideoFramesRetention {
            namespace,
            recording_name,
            recording,
            published_marker,
            index_file,
            rows,
            _lease: lease,
        });
        Ok(Some(crate::control::Retained::guarded(output, retention)))
    }

    struct VideoFramesRetention {
        namespace: Namespace,
        recording_name: String,
        recording: OwnedFd,
        published_marker: std::fs::File,
        index_file: std::fs::File,
        rows: Vec<PinnedVideoFrame<OwnedFd>>,
        _lease: control_auth::ArtifactPathLease,
    }

    impl crate::control::WireRetention for VideoFramesRetention {
        fn prepare_write(&mut self) -> Result<(), String> {
            namespace_still_matches(
                &self.namespace,
                &self.recording_name,
                &self.recording,
                &self.published_marker,
                &self.index_file,
                &self.rows,
            )
            .then_some(())
            .ok_or_else(|| "video frame identity changed before wire reply".to_string())
        }
    }

    pub(super) fn read(
        sock_dir: &std::path::Path,
        instance: &str,
        count: usize,
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        read_with_hook(sock_dir, instance, count, |_| {})
    }

    #[cfg(test)]
    pub(super) fn read_for_test(
        sock_dir: &std::path::Path,
        instance: &str,
        count: usize,
        hook: impl FnMut(ReadStage),
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        read_with_hook(sock_dir, instance, count, hook)
    }
}

#[cfg(windows)]
mod confined_video_reader {
    use std::io::Read as _;
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    use super::{
        PinnedVideoFrame, VIDEO_INDEX_MAX_BYTES, VIDEO_RECORDING_SCAN_MAX, control_auth,
        format_video_frame_rows, valid_recording_name, video_frame_candidates,
    };

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const INDEX_NAME: &str = "index.json";

    struct PinnedDirectory {
        _file: std::fs::File,
        path: std::path::PathBuf,
    }

    struct PinnedFile {
        file: std::fs::File,
        path: std::path::PathBuf,
    }

    fn is_reparse(metadata: &std::fs::Metadata) -> bool {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    fn open_directory_path(
        path: &std::path::Path,
        expected_parent: Option<&std::path::Path>,
    ) -> Option<PinnedDirectory> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_dir() || is_reparse(&metadata) {
            return None;
        }
        // This handle denies FILE_SHARE_DELETE, so after open succeeds neither
        // this component nor any pinned ancestor can be renamed/replaced while
        // the request is in flight.
        let canonical = std::fs::canonicalize(path).ok()?;
        if expected_parent.is_some_and(|parent| canonical.parent() != Some(parent)) {
            return None;
        }
        Some(PinnedDirectory {
            _file: file,
            path: canonical,
        })
    }

    fn open_directory_child(
        parent: &PinnedDirectory,
        name: impl AsRef<std::path::Path>,
    ) -> Option<PinnedDirectory> {
        open_directory_path(&parent.path.join(name), Some(&parent.path))
    }

    fn open_file_child(parent: &PinnedDirectory, name: &str) -> Option<PinnedFile> {
        let path = parent.path.join(name);
        let file = std::fs::OpenOptions::new()
            .read(true)
            // A completion index/frame has no live writer. Denying WRITE as well
            // as DELETE makes the bytes stable for the whole bounded read.
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
            .ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || is_reparse(&metadata) {
            return None;
        }
        let canonical = std::fs::canonicalize(&path).ok()?;
        if canonical.parent() != Some(parent.path.as_path()) {
            return None;
        }
        Some(PinnedFile {
            file,
            path: canonical,
        })
    }

    fn path_still_pinned(path: &std::path::Path, directory: bool) -> bool {
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return false;
        };
        !is_reparse(&metadata)
            && (if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            })
            && std::fs::canonicalize(path).is_ok_and(|current| current == path)
    }

    /// Retain the volume root and every canonical absolute path component. A
    /// single Windows open with OPEN_REPARSE_POINT protects only its leaf; this
    /// chain rejects a reparse at every level and the no-DELETE share mode keeps
    /// all opened ancestors from being renamed or replaced.
    struct AbsoluteDirectory {
        root: PinnedDirectory,
        components: Vec<(std::ffi::OsString, PinnedDirectory)>,
    }

    impl AbsoluteDirectory {
        fn leaf(&self) -> &PinnedDirectory {
            self.components
                .last()
                .map_or(&self.root, |(_, directory)| directory)
        }

        fn still_matches(&self) -> bool {
            let Some(mut current) = open_directory_path(&self.root.path, None) else {
                return false;
            };
            if current.path != self.root.path {
                return false;
            }
            for (name, expected) in &self.components {
                let Some(next) = open_directory_child(&current, name) else {
                    return false;
                };
                if next.path != expected.path {
                    return false;
                }
                current = next;
            }
            true
        }
    }

    fn open_absolute_directory(path: &std::path::Path) -> Option<AbsoluteDirectory> {
        let mut parts = path.components();
        let std::path::Component::Prefix(prefix) = parts.next()? else {
            return None;
        };
        if !matches!(parts.next(), Some(std::path::Component::RootDir)) {
            return None;
        }
        let mut volume_root = std::path::PathBuf::from(prefix.as_os_str());
        volume_root.push("\\");
        let root = open_directory_path(&volume_root, None)?;
        let mut components = Vec::new();
        for component in parts {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            let parent = components.last().map_or(&root, |(_, directory)| directory);
            let directory = open_directory_child(parent, name)?;
            components.push((name.to_os_string(), directory));
        }
        Some(AbsoluteDirectory { root, components })
    }

    fn read_index(recording: &PinnedDirectory) -> Option<(PinnedFile, serde_json::Value)> {
        let mut pinned = open_file_child(recording, INDEX_NAME)?;
        let mut bytes = Vec::new();
        (&mut pinned.file)
            .take((VIDEO_INDEX_MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > VIDEO_INDEX_MAX_BYTES {
            return None;
        }
        let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
        value
            .get("frames")
            .is_some_and(serde_json::Value::is_array)
            .then_some((pinned, value))
    }

    fn recording_names(instance: &PinnedDirectory) -> Result<Vec<String>, &'static str> {
        let entries =
            std::fs::read_dir(&instance.path).map_err(|_| "could not read recording namespace")?;
        let mut names = Vec::new();
        for (scanned, result) in entries.enumerate() {
            if scanned >= VIDEO_RECORDING_SCAN_MAX {
                return Err("recording namespace has too many entries");
            }
            let entry = result.map_err(|_| "could not inspect recording namespace")?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if valid_recording_name(&name) {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    fn selection_still_matches(
        socket: &AbsoluteDirectory,
        video: &PinnedDirectory,
        instance: &PinnedDirectory,
        recording: &PinnedDirectory,
        published_marker: &PinnedFile,
        index_file: &PinnedFile,
        rows: &[PinnedVideoFrame<PinnedFile>],
    ) -> bool {
        socket.still_matches()
            && [&video.path, &instance.path, &recording.path]
                .into_iter()
                .all(|path| path_still_pinned(path, true))
            && path_still_pinned(&published_marker.path, false)
            && path_still_pinned(&index_file.path, false)
            && rows.iter().all(|row| path_still_pinned(&row.path, false))
    }

    struct VideoFramesRetention {
        socket: AbsoluteDirectory,
        video: PinnedDirectory,
        instance: PinnedDirectory,
        recording: PinnedDirectory,
        published_marker: PinnedFile,
        index_file: PinnedFile,
        rows: Vec<PinnedVideoFrame<PinnedFile>>,
        _lease: control_auth::ArtifactPathLease,
    }

    impl crate::control::WireRetention for VideoFramesRetention {
        fn prepare_write(&mut self) -> Result<(), String> {
            selection_still_matches(
                &self.socket,
                &self.video,
                &self.instance,
                &self.recording,
                &self.published_marker,
                &self.index_file,
                &self.rows,
            )
            .then_some(())
            .ok_or_else(|| "video frame identity changed before wire reply".to_string())
        }
    }

    pub(super) fn read(
        sock_dir: &std::path::Path,
        instance: &str,
        count: usize,
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        if control_auth::video_instance_root_for(sock_dir, instance).is_none() {
            return Err("recording namespace is not confined");
        }
        let socket_metadata = std::fs::symlink_metadata(sock_dir)
            .map_err(|_| "recording namespace is not confined")?;
        if is_reparse(&socket_metadata) {
            return Err("recording namespace is not confined");
        }
        let socket_path =
            std::fs::canonicalize(sock_dir).map_err(|_| "recording namespace is not confined")?;
        let socket =
            open_absolute_directory(&socket_path).ok_or("recording namespace is not confined")?;
        let video = open_directory_child(socket.leaf(), control_auth::VIDEO_DIR)
            .ok_or("recording namespace is not confined")?;
        let instance =
            open_directory_child(&video, instance).ok_or("recording namespace is not confined")?;
        let names = recording_names(&instance)?;

        let mut selected = None;
        for name in names.into_iter().rev() {
            let Some(recording) = open_directory_child(&instance, &name) else {
                continue;
            };
            let Some(published_marker) =
                open_file_child(&recording, control_auth::VIDEO_PUBLISHED_FILE)
            else {
                continue;
            };
            let Some((index_file, index)) = read_index(&recording) else {
                continue;
            };
            selected = Some((recording, published_marker, index_file, index));
            break;
        }
        let Some((recording, published_marker, index_file, index)) = selected else {
            return Ok(None);
        };
        let Some(lease) = control_auth::retain_video_artifact_path(&recording.path) else {
            drop(index_file);
            drop(published_marker);
            drop(recording);
            drop(instance);
            drop(video);
            drop(socket);
            return Err("recording retention sweep is in progress");
        };
        let sweep_root = match crate::pinned_dir::PinnedDir::open_resolved(&instance.path) {
            Ok(root) => root,
            Err(_) => {
                drop(index_file);
                drop(published_marker);
                drop(recording);
                drop(instance);
                drop(video);
                drop(socket);
                drop(lease);
                return Err("recording namespace changed during read");
            }
        };

        let mut rows = Vec::new();
        for candidate in video_frame_candidates(&index) {
            let Some(frame) = open_file_child(&recording, &candidate.file) else {
                continue;
            };
            let path = frame.path.clone();
            rows.push(PinnedVideoFrame {
                candidate,
                path,
                _pin: frame,
            });
            if rows.len() == count {
                break;
            }
        }

        // Every handle above omits FILE_SHARE_DELETE. Revalidating its canonical
        // path + type while all ancestor handles remain live therefore proves the
        // path still denotes the opened direct child at this commit point.
        if !selection_still_matches(
            &socket,
            &video,
            &instance,
            &recording,
            &published_marker,
            &index_file,
            &rows,
        ) {
            drop(rows);
            drop(index_file);
            drop(published_marker);
            drop(recording);
            drop(instance);
            drop(video);
            drop(socket);
            drop(sweep_root);
            drop(lease);
            return Err("recording namespace changed during read");
        }
        let recording_name = recording
            .path
            .file_name()
            .expect("validated recording component")
            .to_os_string();
        if sweep_root.validate_path_identity().is_err()
            || !sweep_root.same_directory_file(&instance._file)
            || lease
                .arm_video_reader_sweep(sweep_root, recording_name)
                .is_err()
        {
            drop(rows);
            drop(index_file);
            drop(published_marker);
            drop(recording);
            drop(instance);
            drop(video);
            drop(socket);
            drop(lease);
            return Err("recording namespace changed while arming retention");
        }
        let output = format_video_frame_rows(&rows);
        let retention = crate::control::ReplyRetention::new(VideoFramesRetention {
            socket,
            video,
            instance,
            recording,
            published_marker,
            index_file,
            rows,
            _lease: lease,
        });
        Ok(Some(crate::control::Retained::guarded(output, retention)))
    }
}

#[cfg(not(any(unix, windows)))]
mod confined_video_reader {
    pub(super) fn read(
        _sock_dir: &std::path::Path,
        _instance: &str,
        _count: usize,
    ) -> Result<Option<crate::control::Retained<String>>, &'static str> {
        Err("safe recording reads are unsupported on this platform")
    }
}

/// `video frames [count=N]` — read the newest finished recording's `index.json`
/// and emit its N highest-`delta` frames as `OK <n>` + n `frame …` rows. The
/// `delta` (fingerprint movement from the previous captured frame) is already in
/// the index, so this is a cheap read that hands an AI just the eventful frames
/// (the visual key moments) instead of the whole PNG sequence.
fn video_frames<'a>(
    sock_dir: &std::path::Path,
    args: impl Iterator<Item = &'a str>,
) -> crate::control::ControlReply {
    video_frames_for_instance(sock_dir, control_auth::process_instance_id(), args)
}

fn video_frames_for_instance<'a>(
    sock_dir: &std::path::Path,
    instance: &str,
    args: impl Iterator<Item = &'a str>,
) -> crate::control::ControlReply {
    let mut count = VIDEO_FRAMES_DEFAULT;
    for t in args {
        if let Some(v) = t.strip_prefix("count=") {
            match v.parse::<usize>() {
                Ok(n) if n >= 1 => count = n.min(VIDEO_FRAMES_MAX),
                _ => {
                    return format!("ERR video frames: bad count '{t}' (1..={VIDEO_FRAMES_MAX})\n")
                        .into();
                }
            }
        } else {
            return format!(
                "ERR video frames: unknown arg '{t}' (usage: video frames [count=N])\n"
            )
            .into();
        }
    }
    match confined_video_reader::read(sock_dir, instance, count) {
        Ok(Some(output)) => {
            let (body, retention) = output.into_parts();
            crate::control::ControlReply::guarded(body, retention)
        }
        Ok(None) => {
            "ERR video frames: no finished recording found (run `video <seconds>` first)\n".into()
        }
        Err(error) => format!("ERR video frames: {error}\n").into(),
    }
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
                        Ok(s) if s.is_finite() => args.secs = s,
                        Ok(_) => {
                            return Err(format!(
                                "ERR video: non-finite duration '{t}' ({VIDEO_USAGE})\n"
                            ));
                        }
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

/// Cancels the request on every control-handler exit except a completed reply.
/// This covers timeout, disconnect/unwind, and event-loop send failure without
/// relying on each return site to remember the cross-thread signal.
struct CancelVideoRequestOnDrop(Option<crate::VideoCancellation>);

impl CancelVideoRequestOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }

    fn cancel_won(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(crate::VideoCancellation::cancel)
    }
}

impl Drop for CancelVideoRequestOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            let _ = cancel.cancel();
        }
    }
}

fn recv_video_reply(
    rx: &std::sync::mpsc::Receiver<crate::control::Retained<String>>,
    cancel_on_drop: &mut CancelVideoRequestOnDrop,
    timeout: std::time::Duration,
) -> Result<crate::control::Retained<String>, String> {
    match rx.recv_timeout(timeout) {
        Ok(reply) => {
            cancel_on_drop.disarm();
            Ok(reply)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) if cancel_on_drop.cancel_won() => {
            cancel_on_drop.disarm();
            Err("timed out; recording publication cancelled".to_string())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Publication authorization is irrevocable. A second deadline would
            // recreate a false timeout ERR after the worker already won the CAS.
            match rx.recv() {
                Ok(reply) => {
                    cancel_on_drop.disarm();
                    Ok(reply)
                }
                Err(_) => {
                    cancel_on_drop.disarm();
                    Err("recording worker disconnected after commit authorization".to_string())
                }
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("recording worker disconnected".to_string())
        }
    }
}

/// `video <seconds> [full] [keys] [pace] [fps=<n>] [budget=<MiB>]` — record the
/// front window's exact swapchain bytes handed to the WSI present path,
/// including the swapchain-only glow/chrome layers every single-frame tool
/// misses. Compositor visibility and scanout are not observable here. The verb
/// dumps a PNG sequence + index.json and blocks until the durable `.published`
/// visibility marker is on disk (a multi-second recording can take a while, as
/// its help documents).
/// `keys` (the same-clock keystroke log) is OWNER-only: recording someone's
/// keystrokes is not a screen-read.
pub(crate) fn cmd_video(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    sock_dir: &std::path::Path,
    owner: bool,
) -> crate::control::ControlReply {
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
            }
            .into();
        }
        "stop" => {
            if !owner {
                return "ERR video: stop is owner-only (it truncates a recording)\n".into();
            }
            return match call_main(proxy, |tx| Wake::VideoStop { reply: tx }) {
                Ok(line) => line,
                Err(e) => format!("ERR video: {e}\n"),
            }
            .into();
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
        Err(e) => return e.into(),
    };
    if args.keys && !owner {
        return "ERR video: keys requires owner scope (a keystroke log is not a screen-read)\n"
            .into();
    }
    let Some(dir) = control_auth::confine_video_dir(sock_dir) else {
        return "ERR video: could not create the recording dir\n".into();
    };
    let cancel = crate::VideoCancellation::new();
    let mut cancel_on_drop = CancelVideoRequestOnDrop(Some(cancel.clone()));
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
            cancel,
            reply: tx,
        })
        .is_err()
    {
        // `send_event` returns the unsent `Wake`; dropping it drops the
        // unforgeable directory guard and removes the unpublished directory.
        return "ERR event loop gone\n".into();
    }
    // The reply arrives only after index.json is on disk (recording duration +
    // the PNG encode burst) — wait generously (scaled with the recording so a
    // long take cannot falsely time out mid-encode), but never forever.
    match recv_video_reply(
        &rx,
        &mut cancel_on_drop,
        std::time::Duration::from_secs_f64(args.secs + 120.0),
    ) {
        Ok(reply) => {
            let (body, retention) = reply.into_parts();
            crate::control::ControlReply::guarded(body, retention)
        }
        Err(error) => format!("ERR video: {error}\n").into(),
    }
}

/// `controls <target>` dumps a compatibility GUI target's semantic controls as text, the
/// analogue of `chrome`, so a driver can read native tabs and transient surfaces without
/// a screenshot.
///
/// Unlike the pixel `window` capture, this works HEADLESS and needs no Screen Recording
/// grant: the main thread compiles the native Settings model or calls a transient
/// surface's concrete serializer (`App::read_aux_controls`), never walking AppKit views.
/// A closed Settings tab truthfully reports zero visible controls. Framed `OK <n>\n` +
/// `<n>` rows, the SAME multi-line shape as `chrome`/`text`.
pub(crate) fn cmd_controls(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    use crate::app_introspect::AuxTarget;
    let trimmed = rest.trim();
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "perf" | "performance"
    ) {
        return "ERR target perf was removed with the bottom HUD\n".to_string();
    }
    // Native Settings aliases AND the front window's transient overlay have a controls
    // surface.
    // `front` (and a bare/empty arg, which `parse` maps to Front) reports the front
    // window's open overlay slot (open/closed + kind + fp + scroll extent) — headless-safe.
    let target = match AuxTarget::parse(trimmed) {
        Some(
            t @ (AuxTarget::Front
            | AuxTarget::Prefs
            | AuxTarget::About
            | AuxTarget::Menu
            | AuxTarget::TabMenu
            | AuxTarget::ConnCard
            | AuxTarget::SessionPicker
            | AuxTarget::Connections
            | AuxTarget::Update),
        ) => t,
        _ => {
            return format!(
                "ERR unsupported target {trimmed:?} (use: front | prefs | about | menu | tab-menu | conn-card | session-picker | connections | update)\n"
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
/// transient palette.
/// The versioned, explicit forms are `open app settings [/route]` and
/// `inspect app/v1 ...`. Reuses the SAME open paths as human menu items.
///
/// MAIN-THREAD HOP: touching `App` state may only
/// happen on the main thread, but this runs on a background control thread — so we post
/// [`Wake::OpenAuxWindow`] + a one-shot reply and BLOCK; the main thread opens the
/// surface and replies `Ok(())` (now open) or `Err(msg)`. Single-line
/// `OK opened <target>` / `ERR`.
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
            return "ERR usage: open <prefs|about|menu|connections|update> [close]\n".to_string();
        }
        None => (trimmed, false),
    };
    // Only the aux windows can be opened; `front` is always open (and a bare/empty arg
    // maps to Front) — reject with the verb's advertised contract.
    if matches!(
        target_tok.to_ascii_lowercase().as_str(),
        "perf" | "performance"
    ) {
        return "ERR target perf was removed with the bottom HUD\n".to_string();
    }
    let target = match AuxTarget::parse(target_tok) {
        Some(
            t @ (AuxTarget::Prefs
            | AuxTarget::About
            | AuxTarget::Menu
            | AuxTarget::TabMenu
            | AuxTarget::Connections
            | AuxTarget::Update),
        ) => t,
        _ => {
            return format!(
                "ERR unsupported target {target_tok:?} (use: prefs | about | menu | tab-menu | connections | update)\n"
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
fn normalize_settings_section_name(input: &str) -> String {
    input
        .trim()
        .trim_start_matches('/')
        .to_ascii_lowercase()
        .replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve the native label/path a person sees in Settings, plus the useful names from
/// the retired category model. Deliberately no `performance` alias: that visual page and
/// the bottom HUD it controlled were deleted, while specialist config remains in Manual.
fn parse_settings_section_route(input: &str) -> Option<crate::native_settings::SettingsRoute> {
    use crate::native_settings::SettingsRoute;

    let normalized = normalize_settings_section_name(input);
    if let Some(route) = SettingsRoute::ALL.into_iter().find(|route| {
        normalize_settings_section_name(route.label()) == normalized
            || normalize_settings_section_name(route.path()) == normalized
    }) {
        return Some(route);
    }
    match normalized.as_str() {
        "prefs" | "preferences" | "settings" | "top" | "general" => Some(SettingsRoute::Home),
        "modified settings" => Some(SettingsRoute::Modified),
        "manual config" | "config" => Some(SettingsRoute::Manual),
        "text" | "typography" | "text and fonts" => Some(SettingsRoute::TextFonts),
        "cursor" | "cursor and motion" => Some(SettingsRoute::CursorMotion),
        // The cat's page. "kitty" alone routes here and NOT to the Kitty Log
        // (a read-only book with no route): a bare "kitty" from `settings
        // section …` means the thing walking your cursor.
        "kitty" | "cat" | "cursor cat" | "cursor pet" | "kitty pet" => {
            Some(SettingsRoute::CursorKitty)
        }
        "windows" | "window and tabs" | "window & tabs" => Some(SettingsRoute::WindowTabs),
        "input" | "keyboard" | "keyboard and input" => Some(SettingsRoute::KeyboardInput),
        "update" | "software updates" => Some(SettingsRoute::SoftwareUpdate),
        _ => None,
    }
}

fn settings_section_usage() -> String {
    crate::native_settings::SettingsRoute::ALL
        .into_iter()
        .map(|route| route.label().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" | ")
}

pub(crate) fn cmd_settings_overlay(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    // `settings set <key> <value…>` / `settings unset <key>`: enqueue ONE field in
    // the same serialized/versioned config lane as native Settings (works with the
    // Settings tab closed and headless). Keys come from the shared Settings/Manual
    // schema; `controls prefs` intentionally reports only controls visible on the
    // current native route. A value may contain spaces. The main thread posts the job
    // and stays free; this control thread blocks on the carried reply until durable
    // completion/reconcile.
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
            Ok(completion) => settings_field_wire_reply(completion),
            Err(e) => format!("ERR {e}\n"),
        };
    }
    // `settings section <name>`: land the OPEN surface on the native page whose label or
    // stable path the caller supplied. Compatibility aliases remain accepted, but a
    // deleted legacy page can never creep back into the advertised choices.
    if word == "section" {
        let Some(route) = parse_settings_section_route(tail) else {
            return format!(
                "ERR unknown section {:?} (use: {})\n",
                tail.trim(),
                settings_section_usage()
            );
        };
        return match call_main(proxy, |tx| Wake::SettingsShowSection { route, reply: tx }) {
            Ok(Ok(())) => format!(
                "OK settings section {}\n",
                route.path().trim_start_matches('/')
            ),
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

fn settings_field_wire_reply(completion: Result<String, String>) -> String {
    match completion {
        Ok(status) => format!("OK {status}\n"),
        Err(error) => format!("ERR {error}\n"),
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
/// `OK config_enabled=<bool> session_override=<none|on|off> effective=<bool>
/// engine=<none|live> active=<bool> scope=<window|focused-pane>
/// focused=<bool> animating=<bool>` — the engine tail reports the actual
/// render state (split-pane audit): `scope` says whether emission covers the
/// whole window (single-pane / zoomed) or the focused pane of a split,
/// `active` is the wake-arming `is_active()`, and `focused`/`animating`
/// surface the W11 motion facts (an unfocused or Reduced window emits
/// nothing regardless of the enable bits).
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

/// `tone [status]` -> one status line for the FRONT window's tone-of-typing
/// state ([`crate::App::tone_status`]). Read-only: there is no `on`/`off`
/// form because the knob is durable config (`settings set tone_melody …`) —
/// this verb only ever observes. Main-thread hop like `rain`, since the
/// tracker is per-window App state.
pub(crate) fn cmd_tone(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    match rest.trim() {
        "" | "status" => {}
        other => return format!("ERR usage: tone [status] (got {other:?})\n"),
    }
    match call_main(proxy, |tx| Wake::ToneStatus { reply: tx }) {
        Ok(Ok(msg)) => format!("OK {msg}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// Which reading of the cursor-trail engine `trail …` was asked for.
///
/// Split out of [`cmd_trail`] as a pure parse so the vocabulary is provable
/// without an event loop: `status` must not be swallowed as a malformed count,
/// and a count must not be swallowed as an unknown keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrailForm {
    /// `trail [<n>]` — the per-verdict admission ring.
    Admissions(Option<usize>),
    /// `trail status` — the standing engine-state line.
    Status,
}

/// Parse the `trail` verb's tail. `Err` carries the whole usage line, so the
/// caller never rebuilds it (and the two forms cannot document themselves
/// differently).
pub(crate) fn parse_trail_form(rest: &str) -> Result<TrailForm, String> {
    match rest.trim() {
        "" => Ok(TrailForm::Admissions(None)),
        "status" => Ok(TrailForm::Status),
        n => n
            .parse::<usize>()
            .map(|n| TrailForm::Admissions(Some(n)))
            .map_err(|_| format!("usage: trail [status|<n>] (got {n:?})")),
    }
}

/// `trail [status|<n>]` -> the FOCUSED window's cursor-trail diagnostics, in
/// the two shapes a "the trail went dark" report needs.
///
/// * `trail [<n>]` — the last `n` SPAWN-SEAM VERDICTS (default: the whole
///   diagnostic ring, cap 32), one
///   `admission seq= phase= reason= age_ms= origin= target= alt=` row each,
///   newest last ([`crate::App::trail_admissions`]). `phase` is `licensed` or
///   `declined`; `reason` on a decline is `no-fresh-hint` (no key hint was
///   fresh, so the move was program output nobody's fingers asked for),
///   `no-credits` (a multi-cell coalesce outran the press CREDIT budget) or
///   `off-shape` (licensed and classified, but the style's shape gates laid
///   nothing). What the last few keystrokes DECIDED.
/// * `trail status` — ONE `trail style= … ribbon_active= …` line of standing
///   engine state ([`crate::App::trail_status`]): the resolved style, every
///   gate from the `cursor_trail` knob to the glass, the cumulative
///   `licensed=`/`declined=`/`last_decline_reason=` scoreboard, and the light
///   alive right now. What is TRUE.
///
/// READ THEM IN THAT ORDER. A zero `licensed` beside a nonzero `declined`
/// means every move the engine saw was unlicensed — read
/// `last_decline_reason`. A nonzero `licensed` over a dark screen means the
/// licence is fine and the failure is downstream (morphology, tuning, the
/// compositor), which the rest of the status row walks in frame order.
///
/// The one-command face of the sensor: the rainbow-trail blackout was
/// diagnosed with `ATERM_TRACE_SPAWN` stderr logs, and the standing owner
/// report *"I don't see the rainbow cursor trails"* was investigated by
/// recording video and scanning frames for hue diversity.
/// Both now read the engine's own truth, so a user report is ONE COMMAND.
/// Read-only, main-thread hop like `tone` (the state lives in per-window App
/// state); works headless. The ring form answers `OK <n>` + n rows and the
/// status form the single-line `OK <line>` `rain status` established.
pub(crate) fn cmd_trail(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let count = match parse_trail_form(rest) {
        Ok(TrailForm::Admissions(count)) => count,
        Ok(TrailForm::Status) => {
            // ONE FRAMING PER VERB: `trail` is declared `Lines` in the verb
            // catalog, so the status form answers `OK 1` + its single row
            // rather than the bare `OK <line>` of a natively Status-framed
            // verb like `rain`. A sub-form that flipped framing would need a
            // matching `framing_of` special case or the shipping client would
            // read the row as a count — the `temporal status` footgun.
            return match call_main(proxy, |tx| Wake::TrailStatus { reply: tx }) {
                Ok(Ok(msg)) => format!("OK 1\n{msg}\n"),
                Ok(Err(e)) => format!("ERR {e}\n"),
                Err(e) => format!("ERR {e}\n"),
            };
        }
        Err(usage) => return format!("ERR {usage}\n"),
    };
    let lines = match call_main(proxy, |tx| Wake::TrailAdmissions { count, reply: tx }) {
        Ok(Ok(lines)) => lines,
        Ok(Err(e)) => return format!("ERR {e}\n"),
        Err(e) => return format!("ERR {e}\n"),
    };
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// `status` -> the target session's SUBJECT + classified STATUS record, one
/// versioned key=value line (RFC: Tab Subject & Status §8).
///
/// Read-only, with no sub-form: everything that could change a status is either
/// the user's own metadata (`meta set`) or durable config (`settings set
/// tab_status …`), so this verb only ever observes. A main-thread hop like
/// `tone`/`panes` because the classifier lives in `App` state, which the control
/// thread cannot reach; that also makes it work headless, which is what lets an
/// end-to-end test assert on a classification at all.
pub(crate) fn cmd_session_status(proxy: &EventLoopProxy<Wake>, session: u64, rest: &str) -> String {
    if !rest.trim().is_empty() {
        return format!("ERR usage: status (got {:?})\n", rest.trim());
    }
    match call_main(proxy, |tx| Wake::ReadSessionStatus { session, reply: tx }) {
        Ok(Ok(record)) => format!("OK {record}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// Split `rest` into `key=value` tokens, honouring a DOUBLE-QUOTED value.
///
/// The control protocol is newline-delimited and otherwise whitespace-split,
/// which was fine while the only value anyone passed was a POSIX path. It is not
/// fine on Windows: `cwd=C:\Program Files\Git` arrives as two tokens and is
/// rejected as a usage error, and that is the majority of the paths a Windows
/// operator actually has. So a value may be wrapped in `"…"`, and only the
/// wrapping quotes are consumed — there is deliberately NO escape vocabulary
/// (`\"`, doubling, percent-encoding): a Windows path cannot contain `"` at all,
/// and inventing an escape grammar for a protocol whose framing is "one line"
/// buys a corner case at the cost of a second parser everyone has to agree on.
/// The front door refuses to forward such a path instead
/// (`aterm_cli::WindowRequest::control_request`).
///
/// Backward compatible in both directions: an UNQUOTED value parses exactly as
/// it always did, so every existing `aterm-ctl spawn cwd=/tmp/x` caller is
/// untouched, and an older server simply never sees a quoted one (the front door
/// only quotes when it must).
///
/// WHERE A `"` OPENS A QUOTE is therefore narrow on purpose: only at the start of
/// a token, or immediately after the `=` of a `key=` prefix — the one place the
/// wire format ever puts one. Anywhere else it is an ORDINARY CHARACTER. This
/// module is cross-platform while its justification ("a Windows path cannot
/// contain `\"`") is not: `/tmp/a"b` is a perfectly legal POSIX directory, it
/// used to arrive as one whitespace-delimited token and work, and a state
/// machine that toggled on every `"` would leave the line unterminated and
/// answer `ERR usage`. Treating a mid-value quote as data costs nothing — the
/// front door refuses to FORWARD such a path anyway
/// (`aterm_cli::WindowRequest::control_request`), so the only way one arrives is
/// a direct `aterm-ctl spawn` that used to be served.
///
/// Returns `None` for an unterminated quote — a malformed request, answered with
/// the verb's usage line rather than a half-path.
/// The spawn/act argument tokenizer. `pub(crate)` because the socket-policy fence
/// in `control.rs` MUST classify with the very tokenizer that later parses the
/// command: a fence that splits differently than the parser is a fence with a
/// spelling that walks past it (`spawn "connected=controller" of=…` parsed as a
/// connected spawn while `split_whitespace` saw a token starting with `"`).
pub(crate) fn split_quoted_tokens(rest: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut in_quotes = false;
    for ch in rest.chars() {
        if ch == '"' && in_quotes {
            in_quotes = false;
        } else if ch == '"' && (current.is_empty() || current.ends_with('=')) {
            in_quotes = true;
            started = true;
        } else if ch.is_whitespace() && !in_quotes {
            if started {
                out.push(std::mem::take(&mut current));
                started = false;
            }
        } else {
            current.push(ch);
            started = true;
        }
    }
    if in_quotes {
        return None;
    }
    if started {
        out.push(current);
    }
    Some(out)
}

/// The `spawn` verb's usage line — one string for every malformed form, naming
/// the whole grammar (plain + connected) so a caller who got one token wrong
/// sees the complete contract.
const SPAWN_USAGE: &str = "ERR usage: spawn [connected=controlled|controller place=window|tab of=<sid>] [cwd=<path>] [split=<v|h>]\n";

/// One parsed `spawn` request: the plain tab/split spawn, or the CONNECTED form
/// (design §6) with every argument present and validated.
#[derive(Debug, PartialEq, Eq)]
enum SpawnForm {
    Plain {
        cwd: Option<String>,
        split: Option<crate::pane::SplitDir>,
    },
    Connected {
        kind: crate::connections::ConnectedSpawnKind,
        place: crate::connections::ConnectedSpawnPlace,
        origin: aterm_session::SessionId,
        cwd: Option<String>,
    },
}

/// PURE parse of the `spawn` argument line (unit-tested off the event loop),
/// over [`split_quoted_tokens`] so a quoted `cwd=` keeps its spaces in both
/// forms. `of=` is MANDATORY when `connected=` is present — the App dispatch
/// discards selectors, and an authority-minting argument gets no guessed
/// default (design §6); `place=` is equally explicit (the grammar brackets
/// neither). A `place=`/`of=` without `connected=` is a usage error, as is any
/// unknown token or value.
///
/// `split=` and `connected=` are mutually exclusive: `place=` already says
/// where a connected newborn lands, and a pane split is not one of its two
/// answers — so the pair is refused rather than silently resolved.
fn parse_spawn_args(rest: &str) -> Result<SpawnForm, ()> {
    use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};
    let (mut cwd, mut split, mut kind, mut place, mut origin) = (None, None, None, None, None);
    let Some(tokens) = split_quoted_tokens(rest) else {
        return Err(());
    };
    for tok in tokens {
        if let Some(v) = tok.strip_prefix("cwd=") {
            cwd = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("split=") {
            split = Some(match v {
                "v" | "vertical" => crate::pane::SplitDir::Vertical,
                "h" | "horizontal" => crate::pane::SplitDir::Horizontal,
                _ => return Err(()),
            });
        } else if let Some(v) = tok.strip_prefix("connected=") {
            kind = Some(match v {
                "controlled" => ConnectedSpawnKind::Controlled,
                "controller" => ConnectedSpawnKind::Controller,
                _ => return Err(()),
            });
        } else if let Some(v) = tok.strip_prefix("place=") {
            place = Some(match v {
                "window" => ConnectedSpawnPlace::Window,
                "tab" => ConnectedSpawnPlace::Tab,
                _ => return Err(()),
            });
        } else if let Some(v) = tok.strip_prefix("of=") {
            if v.is_empty() {
                return Err(());
            }
            origin = Some(aterm_session::SessionId::new(v));
        } else {
            return Err(());
        }
    }
    match (kind, place, origin) {
        (None, None, None) => Ok(SpawnForm::Plain { cwd, split }),
        (Some(kind), Some(place), Some(origin)) if split.is_none() => Ok(SpawnForm::Connected {
            kind,
            place,
            origin,
            cwd,
        }),
        // A partial connected form (connected= without of=/place=, or a stray
        // place=/of=), or a connected form carrying split=, never guesses.
        _ => Err(()),
    }
}

/// `spawn [cwd=<path>] [split=<v|h>]` -> mint ONE new session in the frontmost
/// window and reply `OK <sid>` — birth as a socket primitive. The sid is live in
/// the registry before the reply is sent, so `@<sid> …` works immediately: an
/// orchestrator stands up a fleet with a loop of spawn calls and drives each
/// newborn with `turn`/`send`/`subscribe`, no process management.
///
/// `cwd=<path>` sets the newborn's working directory (default: inherit the
/// focused pane's cwd, like Cmd-T); a path containing spaces is quoted
/// (`cwd="C:\Program Files\Git"`, see [`split_quoted_tokens`]).
///
/// `split=v` / `split=h` divides the FOCUSED PANE instead of opening a tab —
/// side-by-side and stacked respectively, the same two directions Cmd-D and
/// Cmd-Shift-D take. This is the wire half of the front door's `aterm
/// split-pane [-d <dir>]` (S12): a split is the one "new terminal" shape that
/// could not be requested from outside the process, so the verb that opens a
/// terminal grew an option rather than the protocol growing a second verb (a new
/// verb would need its own op-class entry, roster line, and completion, for a
/// variation on the same act).
///
/// The newborn runs the default shell; give it a command with
/// `@<sid> turn '<cmd>'`. Main-thread hop like `open`/`controls`.
///
/// `spawn connected=controlled|controller place=window|tab of=<sid> [cwd=…]`
/// (design §6) additionally mints the `both` SESSION CONNECTION binding the
/// newborn to `of=` — Owner-only (the `escalated_op` fence runs before this
/// dispatch). The handler resolves `of=` on the main thread and refuses
/// `place=window` under headless with the NEW `ERR headless` reply (§1.4#7);
/// `place=tab` works headless. cwd default for the connected form is the
/// ORIGIN session's cwd.
pub(crate) fn cmd_spawn(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let sent = match parse_spawn_args(rest) {
        Ok(SpawnForm::Plain { cwd, split }) => call_main(proxy, |tx| Wake::SpawnSession {
            cwd,
            split,
            reply: tx,
        }),
        Ok(SpawnForm::Connected {
            kind,
            place,
            origin,
            cwd,
        }) => call_main(proxy, |tx| Wake::SpawnConnectedSession {
            kind,
            place,
            origin,
            cwd,
            reply: tx,
        }),
        Err(()) => return SPAWN_USAGE.to_string(),
    };
    match sent {
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

/// `panes` -> the ACTIVE-tab split-pane layout (split-pane audit
/// introspection): a `layout tab=<i> panes=<n> zoomed=<bool>` header + one
/// `pane session=<sid> rect=<row_off>,<col_off>,<rows>x<cols> focused=<bool>`
/// row per visible pane, in CELL coords. `session` = a cross-session target:
/// the layout of the window whose ACTIVE tab displays that session (the
/// `image` routing rule); `None` = the front window. A pure main-thread read
/// (`Wake::ReadPanes`, the `chrome` round-trip shape); works headless.
pub(crate) fn cmd_panes(proxy: &EventLoopProxy<Wake>, session: Option<u64>) -> String {
    let lines = match call_main(proxy, |tx| Wake::ReadPanes { session, reply: tx }) {
        Ok(lines) => lines,
        Err(e) => return format!("ERR {e}\n"),
    };
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod trail_parse_tests {
    use super::{TrailForm, parse_trail_form};

    /// The two forms of one verb must not swallow each other: `status` is a
    /// keyword, not a malformed count, and a count is a count, not an unknown
    /// keyword. Before the status form existed, `trail status` answered
    /// `ERR usage: trail [<n>] (got "status")` — which is exactly what a user
    /// reaching for the obvious spelling would have typed first.
    #[test]
    fn the_status_keyword_and_a_ring_count_are_separate_forms() {
        assert_eq!(parse_trail_form("status"), Ok(TrailForm::Status));
        assert_eq!(parse_trail_form("  status  "), Ok(TrailForm::Status));
        assert_eq!(parse_trail_form(""), Ok(TrailForm::Admissions(None)));
        assert_eq!(parse_trail_form("   "), Ok(TrailForm::Admissions(None)));
        assert_eq!(parse_trail_form("8"), Ok(TrailForm::Admissions(Some(8))));
        assert_eq!(parse_trail_form("0"), Ok(TrailForm::Admissions(Some(0))));
    }

    /// An unknown tail is refused with ONE usage line naming BOTH forms — so
    /// the error teaches the vocabulary it just rejected.
    #[test]
    fn an_unknown_tail_names_both_forms_in_one_usage_line() {
        for bad in ["stats", "Status", "on", "-1", "3 4"] {
            let err = parse_trail_form(bad).expect_err(&format!("{bad:?} must be refused"));
            assert!(
                err.starts_with("usage: trail [status|<n>] (got "),
                "{bad:?} -> {err}"
            );
            assert!(
                err.contains(bad.trim()),
                "the usage line echoes the input: {err}"
            );
        }
    }
}

#[cfg(test)]
mod spawn_parse_tests {
    use super::{SpawnForm, parse_spawn_args, split_quoted_tokens};
    use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};

    /// The BACKWARD-COMPATIBILITY leg: every `spawn` request that worked before
    /// quoting existed must tokenize byte-identically, or a quiet regression
    /// lands in every existing `aterm-ctl spawn` caller and every orchestrator
    /// script.
    #[test]
    fn unquoted_requests_tokenize_exactly_as_whitespace_splitting_did() {
        for rest in [
            "",
            "   ",
            "cwd=/tmp/x",
            "  cwd=/tmp/x  ",
            "cwd=/tmp/x split=v",
            // A `"` INSIDE a value: legal on macOS/Linux, and the whole point of
            // the "byte-identical for every unquoted shape" claim. Before the
            // quote rule was narrowed these toggled the state machine, left the
            // line unterminated, and turned a working `aterm-ctl spawn` into
            // `ERR usage`.
            r#"cwd=/tmp/a"b"#,
            r#"cwd=/tmp/a"b"c"#,
            r#"cwd=/tmp/say"hi split=h"#,
        ] {
            let expected: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            assert_eq!(split_quoted_tokens(rest), Some(expected), "{rest:?}");
        }
    }

    /// The reason quoting exists at all: an ordinary Windows path. Before this,
    /// `cwd=C:\Program Files\Git` split into three tokens and the verb answered
    /// with a usage error.
    #[test]
    fn a_quoted_value_keeps_its_spaces_as_one_token() {
        assert_eq!(
            split_quoted_tokens(r#"cwd="C:\Program Files\Git""#),
            Some(vec![r"cwd=C:\Program Files\Git".to_string()])
        );
        assert_eq!(
            split_quoted_tokens(r#"split=h cwd="C:\Users\a b\c""#),
            Some(vec![
                "split=h".to_string(),
                r"cwd=C:\Users\a b\c".to_string()
            ])
        );
        // An EMPTY quoted value is a token, not nothing: `cwd=""` must not
        // silently become "no cwd given".
        assert_eq!(
            split_quoted_tokens(r#"cwd="""#),
            Some(vec!["cwd=".to_string()])
        );
    }

    /// An unterminated quote is malformed, not "everything to end of line": the
    /// caller gets the usage line instead of a truncated directory it never named.
    #[test]
    fn an_unterminated_quote_is_rejected() {
        assert_eq!(split_quoted_tokens(r#"cwd="C:\Program Files"#), None);
    }

    #[test]
    fn plain_spawn_parses_with_and_without_cwd() {
        assert_eq!(
            parse_spawn_args(""),
            Ok(SpawnForm::Plain {
                cwd: None,
                split: None
            })
        );
        assert_eq!(
            parse_spawn_args("cwd=/tmp/x"),
            Ok(SpawnForm::Plain {
                cwd: Some("/tmp/x".to_string()),
                split: None
            })
        );
        assert_eq!(
            parse_spawn_args("cwd=/tmp/x split=v"),
            Ok(SpawnForm::Plain {
                cwd: Some("/tmp/x".to_string()),
                split: Some(crate::pane::SplitDir::Vertical)
            })
        );
        // The quoted `cwd=` reaches the plain form through the same tokenizer.
        assert_eq!(
            parse_spawn_args(r#"cwd="C:\Program Files\Git""#),
            Ok(SpawnForm::Plain {
                cwd: Some(r"C:\Program Files\Git".to_string()),
                split: None
            })
        );
        assert_eq!(parse_spawn_args("bogus"), Err(()));
        assert_eq!(parse_spawn_args("split=diagonal"), Err(()));
        // An unterminated quote is malformed for the verb, not "to end of line".
        assert_eq!(parse_spawn_args(r#"cwd="C:\Program Files"#), Err(()));
    }

    #[test]
    fn connected_spawn_requires_the_full_explicit_form() {
        // The complete form parses, cwd optional.
        let full = parse_spawn_args("connected=controlled place=tab of=s-abc cwd=/w");
        assert_eq!(
            full,
            Ok(SpawnForm::Connected {
                kind: ConnectedSpawnKind::Controlled,
                place: ConnectedSpawnPlace::Tab,
                origin: aterm_session::SessionId::new("s-abc"),
                cwd: Some("/w".to_string()),
            })
        );
        assert!(matches!(
            parse_spawn_args("connected=controller place=window of=s-abc"),
            Ok(SpawnForm::Connected {
                kind: ConnectedSpawnKind::Controller,
                place: ConnectedSpawnPlace::Window,
                ..
            })
        ));
        // `of=` is MANDATORY when connected= is present (an authority-minting
        // argument gets no guessed default — design §6) — and so is place=.
        assert_eq!(parse_spawn_args("connected=controlled place=tab"), Err(()));
        assert_eq!(parse_spawn_args("connected=controlled of=s-abc"), Err(()));
        assert_eq!(parse_spawn_args("connected=controlled"), Err(()));
        // A stray place=/of= without connected= never means a plain spawn.
        assert_eq!(parse_spawn_args("place=tab"), Err(()));
        assert_eq!(parse_spawn_args("of=s-abc"), Err(()));
        // Unknown values fail closed.
        assert_eq!(
            parse_spawn_args("connected=owner place=tab of=s-abc"),
            Err(())
        );
        assert_eq!(
            parse_spawn_args("connected=controlled place=pane of=s-abc"),
            Err(())
        );
        assert_eq!(
            parse_spawn_args("connected=controlled place=tab of="),
            Err(())
        );
    }

    /// The usage string the wire replies for every malformed form names the
    /// WHOLE grammar (a caller sees the complete contract).
    #[test]
    fn spawn_usage_names_the_connected_grammar() {
        assert!(super::SPAWN_USAGE.contains("connected=controlled|controller"));
        assert!(super::SPAWN_USAGE.contains("place=window|tab"));
        assert!(super::SPAWN_USAGE.contains("of=<sid>"));
        assert!(super::SPAWN_USAGE.contains("split=<v|h>"));
    }

    /// `place=` already says where a connected newborn lands; a pane split is
    /// not one of its two answers, so the pair is refused rather than one of
    /// the two silently winning.
    #[test]
    fn connected_and_split_are_mutually_exclusive() {
        assert_eq!(
            parse_spawn_args("connected=controlled place=tab of=s-abc split=v"),
            Err(())
        );
    }
}

#[cfg(test)]
mod video_parse_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn settings_section_parser_accepts_every_visible_label_and_stable_path() {
        for route in crate::native_settings::SettingsRoute::ALL {
            assert_eq!(
                parse_settings_section_route(route.label()),
                Some(route),
                "visible label {:?}",
                route.label()
            );
            assert_eq!(
                parse_settings_section_route(route.path()),
                Some(route),
                "stable path {:?}",
                route.path()
            );
        }
    }

    #[test]
    fn settings_section_parser_keeps_useful_aliases_but_not_deleted_pages() {
        use crate::native_settings::SettingsRoute;

        for (alias, route) in [
            ("cursor", SettingsRoute::CursorMotion),
            ("cursor and motion", SettingsRoute::CursorMotion),
            ("kitty", SettingsRoute::CursorKitty),
            ("cursor pet", SettingsRoute::CursorKitty),
            ("cursor kitty", SettingsRoute::CursorKitty),
            ("typography", SettingsRoute::TextFonts),
            ("input", SettingsRoute::KeyboardInput),
            ("update", SettingsRoute::SoftwareUpdate),
            ("prefs", SettingsRoute::Home),
        ] {
            assert_eq!(parse_settings_section_route(alias), Some(route), "{alias}");
        }
        for removed in [
            "performance",
            "hud",
            "bottom hud",
            "kitty log",
            "diagnostics",
        ] {
            assert_eq!(parse_settings_section_route(removed), None, "{removed}");
        }
        let usage = settings_section_usage();
        assert!(usage.contains("cursor & motion"));
        assert!(usage.contains("text & fonts"));
        assert!(!usage.contains("performance"));
        assert!(!usage.contains("hud"));
    }

    #[test]
    fn settings_field_wire_reply_preserves_success_and_failure_framing() {
        assert_eq!(
            settings_field_wire_reply(Ok("saved: copy_on_select = true".to_string())),
            "OK saved: copy_on_select = true\n"
        );
        assert_eq!(
            settings_field_wire_reply(Err(
                "publication unverified for copy_on_select; reload aterm.toml".to_string()
            )),
            "ERR publication unverified for copy_on_select; reload aterm.toml\n",
            "a non-success completion must never acquire an OK prefix"
        );
    }

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

    #[test]
    fn capture_timeout_reports_err_only_when_cancellation_wins() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = crate::control::CaptureCancellation::new();
        let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
        let cancelled =
            recv_capture_reply::<(u32, u32)>(&rx, &mut cancel_on_drop, Duration::ZERO, "image")
                .expect_err("live request is cancelled at timeout");
        assert!(cancelled.contains("publication cancelled"));
        drop(tx);

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = crate::control::CaptureCancellation::new();
        assert!(cancel.authorize_commit(), "worker wins before timeout");
        let cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let mut cancel_on_drop = cancel_on_drop;
            done_tx
                .send(recv_capture_reply(
                    &rx,
                    &mut cancel_on_drop,
                    Duration::ZERO,
                    "image",
                ))
                .unwrap();
        });
        assert!(
            matches!(
                done_rx.recv_timeout(Duration::from_millis(50)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "commit winner has no second deadline while its result is pending"
        );
        tx.send(Ok(crate::control::Retained::plain((7_u32, 9_u32))))
            .unwrap();
        let reply = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("committed result completes the pending waiter")
            .expect("commit winner is awaited instead of receiving a false timeout ERR");
        assert_eq!(reply.value, (7, 9));
        waiter.join().unwrap();
    }

    #[test]
    fn video_timeout_awaits_commit_winner_instead_of_returning_err() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = crate::VideoCancellation::new();
        let mut cancel_on_drop = CancelVideoRequestOnDrop(Some(cancel.clone()));
        let cancelled = recv_video_reply(&rx, &mut cancel_on_drop, Duration::ZERO)
            .expect_err("live recording is cancelled at timeout");
        assert!(cancelled.contains("publication cancelled"));
        drop(tx);

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = crate::VideoCancellation::new();
        assert!(cancel.authorize_commit(), "encoder wins before timeout");
        let cancel_on_drop = CancelVideoRequestOnDrop(Some(cancel));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let mut cancel_on_drop = cancel_on_drop;
            done_tx
                .send(recv_video_reply(&rx, &mut cancel_on_drop, Duration::ZERO))
                .unwrap();
        });
        assert!(
            matches!(
                done_rx.recv_timeout(Duration::from_millis(50)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "video commit winner has no second deadline while its result is pending"
        );
        tx.send(crate::control::Retained::plain("OK video\n".to_string()))
            .unwrap();
        let reply = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("committed video result completes the pending waiter")
            .expect("commit winner is awaited instead of receiving a false timeout ERR");
        assert_eq!(reply.value, "OK video\n");
        waiter.join().unwrap();
    }

    fn video_test_dir(tag: &str) -> std::path::PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "aterm-vf-{tag}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn instance_root(sock_dir: &std::path::Path, instance: &str) -> std::path::PathBuf {
        sock_dir.join(super::control_auth::VIDEO_DIR).join(instance)
    }

    /// A minimal fake recording under
    /// `<sock_dir>/video/<instance>/<rec>/index.json` with the exact frame-object
    /// shape `app_introspect` writes, for the `video frames` read.
    fn write_fake_recording(
        sock_dir: &std::path::Path,
        instance: &str,
        rec: &str,
        frames: &[(u64, u64)],
    ) {
        let dir = instance_root(sock_dir, instance).join(rec);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        for (i, (delta, t_us)) in frames.iter().enumerate() {
            if i > 0 {
                lines.push_str(",\n");
            }
            let n = i + 1;
            std::fs::write(dir.join(format!("frame_{n:04}.png")), b"png").unwrap();
            lines.push_str(&format!(
                "    {{\"n\":{n},\"seq\":{n},\"t_us\":{t_us},\"fp\":0,\"delta\":{delta},\"file\":\"frame_{n:04}.png\"}}"
            ));
        }
        std::fs::write(
            dir.join("index.json"),
            format!("{{\"frames\":[\n{lines}\n]}}"),
        )
        .unwrap();
        std::fs::write(
            dir.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"aterm-video-published-v1\n",
        )
        .unwrap();
    }

    /// `video frames` returns the highest-`delta` frames of the NEWEST finished
    /// recording, ordered most-changed first, framed `OK <n>` + n rows — and honors
    /// `count=`. A recording DIR with no index.json (still encoding) and every
    /// live sibling's instance namespace are skipped.
    #[test]
    fn video_frames_ranks_by_delta_picks_newest_and_isolates_instance() {
        const INSTANCE: &str = "p101-current";
        const SIBLING: &str = "p202-sibling";
        let tmp = video_test_dir("rank");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Older recording (should be ignored once a newer complete one exists).
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(5, 10), (1, 20)]);
        // A still-encoding dir: newest name, but NO index.json -> must be skipped.
        std::fs::create_dir_all(instance_root(&tmp, INSTANCE).join("rec-1000000002-000")).unwrap();
        // Newest COMPLETE recording: deltas 3, 40, 7, 2 across four frames.
        write_fake_recording(
            &tmp,
            INSTANCE,
            "rec-1000000001-000",
            &[(3, 0), (40, 16), (7, 33), (2, 50)],
        );
        // A live sibling can own a lexicographically newer recording, but this
        // instance's read must never discover or return it.
        write_fake_recording(&tmp, SIBLING, "rec-9999999999-000", &[(999, 99)]);

        // count=2 -> the two highest deltas (40 then 7), most-changed first.
        let out = video_frames_for_instance(&tmp, INSTANCE, "count=2".split_whitespace());
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
            !out.contains("delta=5") && !out.contains("delta=999"),
            "ignored the older recording and live sibling: {out:?}"
        );
        assert!(!out.contains(SIBLING), "no sibling path disclosed: {out:?}");

        // No count -> default cap, all four frames, still delta-ordered.
        let all = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(all.starts_with("OK 4\n"), "all frames: {all:?}");

        // Bad count / unknown arg are rejected.
        assert!(
            video_frames_for_instance(&tmp, INSTANCE, "count=0".split_whitespace())
                .starts_with("ERR ")
        );
        assert!(
            video_frames_for_instance(&tmp, INSTANCE, "bogus".split_whitespace())
                .starts_with("ERR ")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn video_frames_revalidates_selected_handles_at_wire_edge() {
        const INSTANCE: &str = "p303-wire";
        let tmp = video_test_dir("wire-revalidate");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(9, 10)]);

        let mut reply = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(reply.starts_with("OK 1\n"));
        let recording = instance_root(&tmp, INSTANCE).join("rec-1000000000-000");
        let frame = recording.join("frame_0001.png");
        std::fs::rename(&frame, recording.join("frame_moved.png")).unwrap();
        std::fs::write(&frame, b"replacement").unwrap();

        let error = reply
            .prepare_retention_for_test()
            .expect_err("a replacement after formatting cannot receive the queued OK");
        assert!(error.contains("video frame identity changed"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_wire_guard_survives_later_recording_retention() {
        let tmp = video_test_dir("wire-retention");
        let _ = std::fs::remove_dir_all(&tmp);
        super::control_auth::ensure_private_dir(&tmp).unwrap();

        let mut oldest = super::control_auth::confine_video_dir(&tmp).unwrap();
        let oldest_path = oldest.path().to_path_buf();
        let frame = oldest
            .write_new_private(std::ffi::OsStr::new("frame_0001.png"), b"png")
            .unwrap();
        let index = oldest
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                br#"{"frames":[{"n":1,"seq":1,"t_us":10,"fp":0,"delta":9,"file":"frame_0001.png"}]}"#,
            )
            .unwrap();
        oldest
            .publish(std::slice::from_ref(&frame), &index)
            .unwrap();
        drop(oldest.publish_marker().unwrap());
        drop(index);
        drop(frame);
        drop(oldest);

        let mut reply = video_frames_for_instance(
            &tmp,
            super::control_auth::process_instance_id(),
            std::iter::empty(),
        );
        assert!(reply.starts_with("OK 1\n"), "{reply:?}");
        assert!(reply.contains(oldest_path.to_string_lossy().as_ref()));

        for _ in 0..11 {
            let mut later = super::control_auth::confine_video_dir(&tmp).unwrap();
            let later_index = later
                .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
                .unwrap();
            later.publish(&[], &later_index).unwrap();
            drop(later.publish_marker().unwrap());
            later.prune_after_publish();
            drop(later_index);
            drop(later);
        }

        assert!(
            oldest_path.join("index.json").is_file(),
            "sibling retention cannot quarantine a recording named by a queued frames reply"
        );
        reply
            .prepare_retention_for_test()
            .expect("the queued frame paths still validate at the wire edge");
        drop(reply);

        let mut next = super::control_auth::confine_video_dir(&tmp).unwrap();
        let next_index = next
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        next.publish(&[], &next_index).unwrap();
        drop(next.publish_marker().unwrap());
        next.prune_after_publish();
        drop(next_index);
        drop(next);
        assert!(
            !oldest_path.exists(),
            "after the wire guard drops, the oldest recording becomes eligible"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn video_frames_release_sweep_stays_on_pinned_root_after_replacement() {
        const INSTANCE: &str = "p304-pinned-release";
        const RECORDINGS: usize = 12;
        let tmp = video_test_dir("pinned-release");
        let _ = std::fs::remove_dir_all(&tmp);
        for sequence in 0..RECORDINGS {
            write_fake_recording(
                &tmp,
                INSTANCE,
                &format!("rec-{sequence:020}-000"),
                &[(sequence as u64 + 1, 10)],
            );
        }

        let mut reply = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(reply.starts_with("OK 1\n"), "{reply:?}");
        reply
            .prepare_retention_for_test()
            .expect("reader identities validate before namespace replacement");

        let original = instance_root(&tmp, INSTANCE);
        let moved = original.with_file_name(format!("{INSTANCE}-original"));
        std::fs::rename(&original, &moved).unwrap();
        for sequence in 100..(100 + RECORDINGS) {
            write_fake_recording(
                &tmp,
                INSTANCE,
                &format!("rec-{sequence:020}-000"),
                &[(999, 99)],
            );
        }
        let replacement = instance_root(&tmp, INSTANCE);
        std::fs::write(replacement.join("sentinel"), b"replacement").unwrap();

        drop(reply);

        let recording_count = |root: &std::path::Path| {
            std::fs::read_dir(root)
                .unwrap()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("rec-"))
                .count()
        };
        assert!(
            recording_count(&moved) < RECORDINGS,
            "last-reader retention makes progress in the exact namespace that was read"
        );
        assert_eq!(
            recording_count(&replacement),
            RECORDINGS,
            "the replacement installed at the old lexical path is never swept"
        );
        assert_eq!(
            std::fs::read(replacement.join("sentinel")).unwrap(),
            b"replacement"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn failed_video_frames_read_before_arm_does_not_run_retention() {
        use super::confined_video_reader::{ReadStage, read_for_test};

        const INSTANCE: &str = "p305-failed-prearm";
        const RECORDINGS: usize = 12;
        let tmp = video_test_dir("failed-prearm");
        let _ = std::fs::remove_dir_all(&tmp);
        for sequence in 0..RECORDINGS {
            write_fake_recording(
                &tmp,
                INSTANCE,
                &format!("rec-{sequence:020}-000"),
                &[(sequence as u64 + 1, 10)],
            );
        }

        let newest = instance_root(&tmp, INSTANCE).join(format!("rec-{:020}-000", RECORDINGS - 1));
        let frame = newest.join("frame_0001.png");
        let mut replaced = false;
        let result = read_for_test(&tmp, INSTANCE, VIDEO_FRAMES_DEFAULT, |stage| {
            if replaced || stage != ReadStage::FramePinned {
                return;
            }
            replaced = true;
            std::fs::rename(&frame, newest.join("frame.original")).unwrap();
            std::fs::write(&frame, b"replacement").unwrap();
        });

        assert!(replaced, "the post-lease, pre-arm frame hook was reached");
        match result {
            Err(error) => assert_eq!(error, "recording namespace changed during read"),
            Ok(_) => panic!("the replaced frame identity must fail the reader"),
        }
        let remaining = std::fs::read_dir(instance_root(&tmp, INSTANCE))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("rec-"))
            .count();
        assert_eq!(
            remaining, RECORDINGS,
            "dropping an unarmed failed-read lease performs no retention sweep"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// With no recordings at all, `video frames` is a clean `ERR`, not a panic.
    #[test]
    fn video_frames_errors_when_no_recording() {
        const INSTANCE: &str = "p303-empty";
        let tmp = video_test_dir("empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(instance_root(&tmp, INSTANCE)).unwrap();
        let out = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(out.starts_with("ERR video frames:"), "{out:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_caps_index_and_falls_back_to_valid_prior_recording() {
        const INSTANCE: &str = "p404-capped";
        let tmp = video_test_dir("cap");
        let _ = std::fs::remove_dir_all(&tmp);
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(17, 10)]);
        let oversized = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
        std::fs::create_dir_all(&oversized).unwrap();
        std::fs::write(
            oversized.join("index.json"),
            vec![b' '; VIDEO_INDEX_MAX_BYTES + 1],
        )
        .unwrap();
        std::fs::write(
            oversized.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();

        let out = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(
            out.starts_with("OK 1\n") && out.contains("delta=17"),
            "oversized newest index is skipped for the valid prior recording: {out:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_skips_newest_index_until_publication_marker_exists() {
        const INSTANCE: &str = "p405-marker";
        let tmp = video_test_dir("marker");
        let _ = std::fs::remove_dir_all(&tmp);
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(17, 10)]);

        let queued = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
        std::fs::create_dir_all(&queued).unwrap();
        std::fs::write(queued.join("frame_0001.png"), b"queued").unwrap();
        std::fs::write(
            queued.join("index.json"),
            br#"{"frames":[{"n":1,"seq":1,"t_us":20,"fp":0,"delta":999,"file":"frame_0001.png"}]}"#,
        )
        .unwrap();
        assert!(
            !queued
                .join(super::control_auth::VIDEO_PUBLISHED_FILE)
                .exists(),
            "fixture is a fully encoded reply still queued before wire publication"
        );

        let out = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(
            out.starts_with("OK 1\n") && out.contains("delta=17"),
            "the reader falls back to the newest marker-visible recording: {out:?}"
        );
        assert!(
            !out.contains("delta=999") && !out.contains(queued.to_string_lossy().as_ref()),
            "an index alone never makes a queued recording discoverable: {out:?}"
        );

        drop(out);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_bounds_recording_namespace_scan() {
        const INSTANCE: &str = "p505-bounded";
        let tmp = video_test_dir("scan");
        let root = instance_root(&tmp, INSTANCE);
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..=VIDEO_RECORDING_SCAN_MAX {
            // Invalid names count too: otherwise an attacker could hide unbounded
            // work behind entries the server later ignores.
            std::fs::create_dir(root.join(format!("junk-{index:04}"))).unwrap();
        }

        let out = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(
            out.contains("recording namespace has too many entries"),
            "bounded scan fails closed: {out:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn video_frames_rejects_traversal_and_non_frame_manifest_names() {
        const INSTANCE: &str = "p606-names";
        let tmp = video_test_dir("names");
        let rec = instance_root(&tmp, INSTANCE).join("rec-1000000000-000");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::write(rec.join("frame_0001.png"), b"png").unwrap();
        std::fs::write(
            rec.join("index.json"),
            br#"{"frames":[
                {"n":1,"seq":1,"t_us":1,"delta":10,"file":"frame_0001.png"},
                {"n":2,"seq":2,"t_us":2,"delta":999,"file":"../../escape.png"},
                {"n":3,"seq":3,"t_us":3,"delta":998,"file":"/absolute.png"},
                {"n":4,"seq":4,"t_us":4,"delta":997,"file":"notes.txt"}
            ]}"#,
        )
        .unwrap();
        std::fs::write(
            rec.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();

        let out = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(out.starts_with("OK 1\n"), "only one safe row: {out:?}");
        assert!(
            out.contains("frame_0001.png"),
            "safe frame retained: {out:?}"
        );
        for forbidden in ["escape.png", "absolute.png", "notes.txt"] {
            assert!(!out.contains(forbidden), "{forbidden} rejected: {out:?}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn video_frames_rejects_linked_roots_recordings_indexes_and_frames() {
        use std::os::unix::fs::symlink;

        const INSTANCE: &str = "p707-links";

        // A link at the `video/` root must not redirect the reader outside the
        // canonical socket directory.
        let linked_root = video_test_dir("linked-root");
        let outside_root = video_test_dir("outside-root");
        let _ = std::fs::remove_dir_all(&linked_root);
        let _ = std::fs::remove_dir_all(&outside_root);
        std::fs::create_dir_all(outside_root.join(INSTANCE)).unwrap();
        std::fs::create_dir_all(&linked_root).unwrap();
        symlink(
            &outside_root,
            linked_root.join(super::control_auth::VIDEO_DIR),
        )
        .unwrap();
        let out = video_frames_for_instance(&linked_root, INSTANCE, std::iter::empty());
        assert!(
            out.contains("recording namespace is not confined"),
            "linked root refused: {out:?}"
        );

        // Inside a real namespace, a linked recording and linked index are
        // skipped. A linked frame in an otherwise valid newest index is omitted.
        let tmp = video_test_dir("linked-entries");
        let outside = video_test_dir("linked-outside");
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
        write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(7, 10)]);
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("index.json"),
            br#"{"frames":[{"n":1,"seq":1,"t_us":1,"delta":999,"file":"frame_0001.png"}]}"#,
        )
        .unwrap();
        std::fs::write(
            outside.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();
        symlink(
            &outside,
            instance_root(&tmp, INSTANCE).join("rec-1000000002-000"),
        )
        .unwrap();
        let linked_index = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
        std::fs::create_dir(&linked_index).unwrap();
        symlink(outside.join("index.json"), linked_index.join("index.json")).unwrap();
        std::fs::write(
            linked_index.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();
        let out = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert!(
            out.starts_with("OK 1\n") && out.contains("delta=7"),
            "linked recording and index fall back safely: {out:?}"
        );

        let linked_frame = instance_root(&tmp, INSTANCE).join("rec-1000000003-000");
        std::fs::create_dir(&linked_frame).unwrap();
        std::fs::write(outside.join("frame.png"), b"png").unwrap();
        symlink(
            outside.join("frame.png"),
            linked_frame.join("frame_0001.png"),
        )
        .unwrap();
        std::fs::write(
            linked_frame.join("index.json"),
            br#"{"frames":[{"n":1,"seq":1,"t_us":1,"delta":500,"file":"frame_0001.png"}]}"#,
        )
        .unwrap();
        std::fs::write(
            linked_frame.join(super::control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();
        let out = video_frames_for_instance(&tmp, INSTANCE, std::iter::empty());
        assert_eq!(
            out, "OK 0\n",
            "a linked frame is never returned even from a valid completion index"
        );

        for dir in [linked_root, outside_root, tmp, outside] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Retained directory/file descriptors close every in-request check→open
    /// window. If a same-user process renames and replaces any lexical namespace
    /// component after its handle is acquired, the reader continues on the exact
    /// original objects and then refuses to emit paths whose names no longer
    /// match those objects.
    #[cfg(unix)]
    #[test]
    fn video_frames_handle_anchors_fail_closed_on_concurrent_swaps() {
        use std::os::unix::fs::symlink;

        use super::confined_video_reader::{ReadStage, read_for_test};

        const INSTANCE: &str = "p808-race";
        const RECORDING: &str = "rec-1000000000-000";

        for stage in [
            ReadStage::NamespacePinned,
            ReadStage::RecordingPinned,
            ReadStage::IndexPinned,
            ReadStage::FramePinned,
        ] {
            let tag = match stage {
                ReadStage::SocketPathResolved => unreachable!("covered by the ancestor test"),
                ReadStage::NamespacePinned => "namespace",
                ReadStage::RecordingPinned => "recording",
                ReadStage::IndexPinned => "index",
                ReadStage::FramePinned => "frame",
            };
            let tmp = video_test_dir(&format!("race-{tag}"));
            let moved = video_test_dir(&format!("race-{tag}-moved"));
            let outside = video_test_dir(&format!("race-{tag}-outside"));
            for dir in [&tmp, &moved, &outside] {
                let _ = std::fs::remove_dir_all(dir);
                let _ = std::fs::remove_file(dir);
            }
            write_fake_recording(&tmp, INSTANCE, RECORDING, &[(17, 10)]);
            write_fake_recording(&outside, INSTANCE, RECORDING, &[(999, 99)]);

            let recording = instance_root(&tmp, INSTANCE).join(RECORDING);
            let outside_recording = instance_root(&outside, INSTANCE).join(RECORDING);
            let mut swapped = false;
            let result = read_for_test(&tmp, INSTANCE, VIDEO_FRAMES_DEFAULT, |seen| {
                if swapped || seen != stage {
                    return;
                }
                swapped = true;
                match stage {
                    ReadStage::SocketPathResolved => unreachable!("covered by the ancestor test"),
                    ReadStage::NamespacePinned => {
                        std::fs::rename(&tmp, &moved).unwrap();
                        symlink(&outside, &tmp).unwrap();
                    }
                    ReadStage::RecordingPinned => {
                        std::fs::rename(&recording, recording.with_extension("original")).unwrap();
                        symlink(&outside_recording, &recording).unwrap();
                    }
                    ReadStage::IndexPinned => {
                        let index = recording.join("index.json");
                        std::fs::rename(&index, recording.join("index.original")).unwrap();
                        symlink(outside_recording.join("index.json"), index).unwrap();
                    }
                    ReadStage::FramePinned => {
                        let frame = recording.join("frame_0001.png");
                        std::fs::rename(&frame, recording.join("frame.original")).unwrap();
                        symlink(outside_recording.join("frame_0001.png"), frame).unwrap();
                    }
                }
            });

            assert!(swapped, "{stage:?} hook was reached");
            assert_eq!(
                result,
                Err("recording namespace changed during read"),
                "{stage:?} replacement is never emitted"
            );
            let debug = format!("{result:?}");
            assert!(
                !debug.contains("999") && !debug.contains(&outside.display().to_string()),
                "{stage:?} never discloses attacker data/path: {debug}"
            );

            // The namespace-stage case leaves `tmp` as a symlink and the real
            // original tree at `moved`; all other cases leave links inside tmp.
            let _ = std::fs::remove_file(&tmp);
            for dir in [tmp, moved, outside] {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// Opening an absolute path with O_NOFOLLOW once protects only its final
    /// component. Resolve-then-swap an intermediate ancestor to a symlink and
    /// prove the component-by-component root walk refuses it before any attacker
    /// recording can be read.
    #[cfg(unix)]
    #[test]
    fn video_frames_rejects_intermediate_ancestor_swap_after_resolution() {
        use std::os::unix::fs::symlink;

        use super::confined_video_reader::{ReadStage, read_for_test};

        const INSTANCE: &str = "p909-ancestor";
        const RECORDING: &str = "rec-1000000000-000";

        let outer = video_test_dir("ancestor-race");
        let outside = video_test_dir("ancestor-race-outside");
        let ancestor = outer.join("controlled");
        let moved = outer.join("controlled-original");
        let socket = ancestor.join("socket");
        let outside_socket = outside.join("socket");
        for dir in [&outer, &outside] {
            let _ = std::fs::remove_dir_all(dir);
        }
        write_fake_recording(&socket, INSTANCE, RECORDING, &[(17, 10)]);
        write_fake_recording(&outside_socket, INSTANCE, RECORDING, &[(999, 99)]);

        let mut swapped = false;
        let result = read_for_test(&socket, INSTANCE, VIDEO_FRAMES_DEFAULT, |seen| {
            if swapped || seen != ReadStage::SocketPathResolved {
                return;
            }
            swapped = true;
            std::fs::rename(&ancestor, &moved).unwrap();
            symlink(&outside, &ancestor).unwrap();
        });

        assert!(swapped, "post-canonicalization hook was reached");
        assert_eq!(
            result,
            Err("recording namespace is not confined"),
            "a swapped intermediate ancestor is rejected"
        );
        assert!(
            !format!("{result:?}").contains("999"),
            "attacker index was not read"
        );

        let _ = std::fs::remove_file(&ancestor);
        for dir in [outer, outside] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
