// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The SACRED introspection render path: the AI-reads-the-real-screen feature.
//! `snapshot` (SIGUSR1 PNG+txt) and `render_image` (the control `image` verb)
//! render the CURRENT terminal through the SAME renderer the window uses — what
//! the AI sees is byte-identical to what is presented (WYSIWYG, incl. the bell
//! invert + tab-strip splice). `read_native_chrome`/`capture_window` add the OS
//! chrome + the on-glass window capture. Verbatim relocation — never alter this
//! logic; this is a hard project invariant.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_render::Frame;

// Window capture names it bare on the two hosts with a compositor capture path.
#[cfg(any(target_os = "macos", windows))]
use crate::WindowId;
use crate::app_render::{
    OverlayGlow, apply_bell_invert, apply_drop_overlay, apply_overlay_at, composite_tray,
};
use crate::control::{DimsSnapshot, ImageReq};
use crate::platform::AppRt;
use crate::{App, accessibility, control_auth, snapshot_path, term_lock};

/// Headless capture may arrive long after the output wake that created a word.
/// Ten seconds is the effects engine's episode-retention horizon: older stamps
/// are equivalent for every bounded one-shot, and clamping avoids carrying an
/// arbitrary process-lifetime duration into the reconstruction tick.
const CAPTURE_BIRTH_MAX_AGE: Duration = Duration::from_secs(10);

/// When capture is the first presentation, sample a feline episode at the end
/// of its 450 ms rise: the authored cat is fully visible, the sighting is real
/// (pixels landed), and no timer or synthetic frame loop is introduced between
/// capture requests. A still-pending output stamp proves the glass has not
/// rescanned it yet, even for a windowed surface whose redraw was occluded.
const CAPTURE_PREVIEW_AGE: Duration = Duration::from_millis(450);

fn bounded_capture_birth(now: Instant, pending: Option<Instant>) -> Option<Instant> {
    pending.map(|stamp| {
        let age = now
            .saturating_duration_since(stamp)
            .min(CAPTURE_BIRTH_MAX_AGE);
        now.checked_sub(age).unwrap_or(now)
    })
}

fn capture_rescan_birth(now: Instant, pending: Option<Instant>, windowless: bool) -> Instant {
    let birth = bounded_capture_birth(now, pending).unwrap_or(now);
    if windowless || now.saturating_duration_since(birth) > CAPTURE_PREVIEW_AGE {
        now.checked_sub(CAPTURE_PREVIEW_AGE).unwrap_or(now)
    } else {
        birth
    }
}

/// Assemble the non-macOS `chrome` response after the platform adapter has read
/// its live tab model. Kept pure so the app-level output contract is testable on
/// the macOS development host even though this production arm is cfg-selected
/// only on Linux/Windows.
#[cfg(any(test, not(target_os = "macos")))]
fn non_macos_chrome_output(
    mut platform_lines: Vec<String>,
    toolbar_tabs: Option<String>,
    tab_menu_lines: Vec<String>,
    menu_lines: Vec<String>,
) -> Vec<String> {
    platform_lines.push("toolbar (none)".to_string());
    if let Some(toolbar_tabs) = toolbar_tabs {
        platform_lines.push(toolbar_tabs);
    }
    // Per-tab context-menu mirror lines (`tab-menu tab=<i> items=[...]`) ride
    // between the toolbar-tabs line and the app menu, matching the macOS order.
    platform_lines.extend(tab_menu_lines);
    platform_lines.extend(menu_lines);
    platform_lines
}

/// A finished capture handed OFF the winit event-loop thread for PNG encode +
/// confined write. A Retina-sized deflate is a 50–150 ms stall, so the encode
/// worker owns it; the client's reply is sent by the worker ONLY AFTER the
/// write completes — the control client reads the file the moment it sees OK,
/// so replying post-encode/pre-write would break the protocol contract.
pub(crate) enum EncodeJob {
    /// The `image` verb's fully-composited framebuffer (every WYSIWYG splice /
    /// overlay is already baked in before the `Frame` is moved here). A failed
    /// encode/write replies `Err` so the client is never told `OK` for a file
    /// that does not exist ("OK means the file is on disk" is the protocol
    /// contract). `Ok((0, 0))` stays the render's no-window sentinel.
    Image {
        frame: Frame,
        target: control_auth::ConfinedImage,
        /// `image --bytes`: return the PNG in the reply instead of writing `target`.
        want_bytes: bool,
        reply: std::sync::mpsc::Sender<crate::control::ImageReply>,
    },
    /// A platform window photograph serving `image` for a native tab. Native
    /// tabs use platform-owned title/tab chrome on glass, so this preserves the
    /// image verb's WYSIWYG contract while retaining the modern `--bytes`
    /// response shape.
    #[cfg(any(target_os = "macos", windows))]
    ImageRgba {
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        target: control_auth::ConfinedImage,
        want_bytes: bool,
        reply: std::sync::mpsc::Sender<crate::control::ImageReply>,
    },
    /// SIGUSR1 `snapshot` output (PNG + parallel .txt + .done marker). The frame
    /// is fully composited on the main thread; only the deflate + writes run
    /// here — the same 50–150 ms Retina stall the `image` verb already routes
    /// around. No reply channel: the `.done` marker file IS the completion
    /// signal (requesters stat() for it; stderr is unreliable for GUIs).
    Snapshot {
        frame: Frame,
        text: String,
        path: String,
    },
    /// A `window`/`window <aux>` capture: tightly-packed RGBA8 pixels photographed
    /// on the main thread (macOS: CGWindowList; Windows: `PrintWindow` — both need
    /// main-thread window state); only the encode + write run here.
    #[cfg(any(target_os = "macos", windows))]
    WindowRgba {
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        target: control_auth::ConfinedImage,
        reply: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    },
    /// VIDEO introspection dump: one finalized recording (already byte-budget
    /// bounded at capture — at most one recording's RAM transits this channel,
    /// MOVED not copied). Encodes every frame to PNG, writes `index.json` LAST
    /// (the completion marker), then replies with its path. The pre-routing
    /// input-attempt log (`inputs`) is dropped with this job after the dump —
    /// recording-lifetime only, never global.
    VideoDump {
        take: aterm_gpu::video_tap::VideoTake,
        /// The recording's honesty label ([`crate::VideoMode`]): whether the
        /// tap copied the actually-presented swapchain bytes
        /// (`swapchain-tap`) or the headless virtual target's bytes
        /// (`offscreen-present-real` — what glass WOULD have shown; nothing
        /// reached photons). Disclosed verbatim in index.json's `meta.mode`
        /// with mode-matched `stamp_semantics`.
        mode: crate::VideoMode,
        inputs: Vec<(u64, char)>,
        started_us: u64,
        dir: std::path::PathBuf,
        reply: std::sync::mpsc::Sender<String>,
    },
}

/// Encode + confined-write one job, then reply. Runs on the encode worker (or
/// inline, on the fallback path, when the worker cannot be spawned/reached).
/// The write keeps the TOCTOU confinement contract verbatim: `target.dir` +
/// `target.file_name` via `write_private_at`, never a re-joined path string.
fn run_encode_job(job: EncodeJob) {
    match job {
        EncodeJob::Image {
            frame,
            target,
            want_bytes,
            reply,
        } => {
            let (w, h) = (frame.width as u32, frame.height as u32);
            let png = frame.to_png();
            let result = if want_bytes {
                // `--bytes`: hand the PNG back over the wire; write no file (a remote
                // driver cannot read the server's filesystem).
                Ok((w, h, Some(png)))
            } else {
                snapshot_path::write_private_at(&target.dir, &target.file_name, &png)
                    .map(|()| (w, h, None))
                    .map_err(|e| format!("image write failed: {e}"))
            };
            let _ = reply.send(result);
        }
        #[cfg(any(target_os = "macos", windows))]
        EncodeJob::ImageRgba {
            rgba,
            width,
            height,
            target,
            want_bytes,
            reply,
        } => {
            let result = encode_rgba8_png(&rgba, width, height)
                .map_err(|e| format!("native image capture failed (PNG encode error: {e})"))
                .and_then(|png| {
                    if want_bytes {
                        Ok((width, height, Some(png)))
                    } else {
                        snapshot_path::write_private_at(&target.dir, &target.file_name, &png)
                            .map(|()| (width, height, None))
                            .map_err(|e| format!("native image capture failed (write error: {e})"))
                    }
                });
            let _ = reply.send(result);
        }
        EncodeJob::Snapshot { frame, text, path } => {
            let _ = snapshot_path::write_private(std::path::Path::new(&path), &frame.to_png());
            let _ = snapshot_path::write_private(
                std::path::Path::new(&format!("{path}.txt")),
                text.as_bytes(),
            );
            // a marker the requester can stat() for; stderr is unreliable for GUIs
            let _ = snapshot_path::write_private(
                std::path::Path::new(&format!("{path}.done")),
                format!("{}x{}\n", frame.width, frame.height).as_bytes(),
            );
            eprintln!("aterm-gui: snapshot written to {path} (+ .txt, .done)");
        }
        #[cfg(any(target_os = "macos", windows))]
        EncodeJob::WindowRgba {
            rgba,
            width,
            height,
            target,
            reply,
        } => {
            let result = encode_rgba8_png(&rgba, width, height)
                .map_err(|e| format!("window capture failed (PNG encode error: {e})"))
                .and_then(|png| {
                    snapshot_path::write_private_at(&target.dir, &target.file_name, &png)
                        .map_err(|e| format!("window capture failed (write error: {e})"))
                })
                .map(|()| (width, height));
            let _ = reply.send(result);
        }
        EncodeJob::VideoDump {
            take,
            mode,
            inputs,
            started_us,
            dir,
            reply,
        } => {
            // SELF-EVALUATION: correlate each PRE-ROUTING input attempt with the
            // first later whole-frame change. This is useful response-latency
            // evidence, not proof that the event reached a PTY or caused that
            // change. For each attempt, the reference is the last earlier frame;
            // the first later frame that pixel-differs is the observed response.
            // WHOLE frame, not a fixed band: the typed line lives wherever the
            // screen has scrolled to (the top-band v1 went blind after one scroll).
            let frame_fp = |f: &aterm_gpu::video_tap::CapturedFrame| -> u64 {
                // Sampled sum — cheap, stable, sensitive to any visible change.
                f.rgba.iter().step_by(64).map(|&b| b as u64).sum()
            };
            let fps: Vec<(u64, u64)> = take.frames.iter().map(|f| (f.t_us, frame_fp(f))).collect();
            let mut glyph_lines = String::new();
            let mut lats_ms: Vec<f64> = Vec::new();
            for (t, ch) in &inputs {
                let before = fps.iter().rev().find(|(ft, _)| ft < t);
                let Some((_, ref_fp)) = before else { continue };
                let hit = fps
                    .iter()
                    .filter(|(ft, _)| ft >= t)
                    .find(|(_, fp)| fp.abs_diff(*ref_fp) > 200);
                let esc = match ch {
                    '"' => "\\\"".to_string(),
                    '\\' => "\\\\".to_string(),
                    c if c.is_control() => format!("\\u{:04x}", *c as u32),
                    c => c.to_string(),
                };
                if !glyph_lines.is_empty() {
                    glyph_lines.push_str(",\n");
                }
                match hit {
                    Some((ft, _)) => {
                        let ms = (ft.saturating_sub(*t)) as f64 / 1000.0;
                        lats_ms.push(ms);
                        glyph_lines.push_str(&format!(
                            "      {{\"ch\":\"{esc}\",\"t_us\":{t},\"response_ms\":{ms:.1}}}"
                        ));
                    }
                    None => glyph_lines.push_str(&format!(
                        "      {{\"ch\":\"{esc}\",\"t_us\":{t},\"response_ms\":null}}"
                    )),
                }
            }
            lats_ms.sort_by(|a, b| a.total_cmp(b));
            let analysis = if inputs.is_empty() {
                "  \"analysis\": {\"note\": \"no input attempts logged (pass `keys` and type during the recording)\"},\n".to_string()
            } else if lats_ms.is_empty() {
                format!(
                    "  \"analysis\": {{\n    \"key_response\": [\n{glyph_lines}\n    ],\n    \
                     \"note\": \"input attempts logged but no later visible change detected; inputs are not delivery receipts\"\n  }},\n"
                )
            } else {
                let p50 = lats_ms[lats_ms.len() / 2];
                let p90 = lats_ms[(lats_ms.len() * 9 / 10).min(lats_ms.len() - 1)];
                let max = lats_ms[lats_ms.len() - 1];
                // Verdict thresholds: one 60Hz frame (16.7ms) p50 = instant;
                // two frames p50 = good; beyond = investigate.
                let verdict = if p50 <= 16.7 && max <= 50.0 {
                    "INSTANT: every logged input attempt was followed by a frame change within ~1-2 frames"
                } else if p50 <= 33.4 {
                    "GOOD: typical logged input attempt was followed by a frame change within 2 frames; check max outliers"
                } else {
                    "SLOW: later frame changes exceed 2 frames — investigate routing, shell echo, and load"
                };
                format!(
                    "  \"analysis\": {{\n    \"key_response\": [\n{glyph_lines}\n    ],\n    \
                     \"p50_ms\": {p50:.1}, \"p90_ms\": {p90:.1}, \"max_ms\": {max:.1}, \"n\": {},\n    \
                     \"verdict\": \"{verdict}\"\n  }},\n",
                    lats_ms.len()
                )
            };
            // Frames first, index.json LAST (the completion marker — its presence
            // means every frame it names is readable; the OK reply follows it).
            let mut frame_lines = String::new();
            let mut written = 0usize;
            // `delta` = how much this frame's sampled fingerprint moved from the
            // previous captured frame. A multimodal AI reads index.json and pulls
            // only the high-`delta` frames (the visually eventful moments) instead
            // of downloading every PNG — the fingerprint is already computed above
            // for the keystroke analysis, so this is free. `prev_fp` tracks the
            // previous frame in capture order (skipped-on-encode-error frames still
            // update it, so the delta always spans real adjacent captures).
            let mut prev_fp: Option<u64> = None;
            for (i, f) in take.frames.iter().enumerate() {
                let fp = fps.get(i).map_or(0, |&(_, fp)| fp);
                let delta = prev_fp.map_or(0, |p| fp.abs_diff(p));
                prev_fp = Some(fp);
                let name = format!("frame_{:04}.png", i + 1);
                let Ok(png) = encode_rgba8_png(&f.rgba, f.w, f.h) else {
                    continue;
                };
                if snapshot_path::write_private_at(
                    &dir,
                    &std::ffi::OsString::from(name.clone()),
                    &png,
                )
                .is_err()
                {
                    continue;
                }
                written += 1;
                if !frame_lines.is_empty() {
                    frame_lines.push_str(",\n");
                }
                frame_lines.push_str(&format!(
                    "    {{\"n\":{},\"seq\":{},\"t_us\":{},\"fp\":{fp},\"delta\":{delta},\"file\":\"{name}\"}}",
                    i + 1,
                    f.seq,
                    f.t_us
                ));
            }
            let mut input_lines = String::new();
            for (t, ch) in &inputs {
                if !input_lines.is_empty() {
                    input_lines.push_str(",\n");
                }
                // Escape the char for JSON (printables only reach here, but be safe).
                let esc = match ch {
                    '"' => "\\\"".to_string(),
                    '\\' => "\\\\".to_string(),
                    c if c.is_control() => format!("\\u{:04x}", *c as u32),
                    c => c.to_string(),
                };
                input_lines.push_str(&format!("    {{\"t_us\":{t},\"ch\":\"{esc}\"}}"));
            }
            let stop_us = crate::metrics::now_us();
            // HONEST COVERAGE: `dropped` on the wire stays the TOTAL loss
            // (mid-stream ring skips + head evictions — every presented frame
            // not in the artifact), while the meta splits it: `ring_skipped`
            // (GPU behind) vs `evicted_frames` (the HEAD of the recording is
            // gone — `head_truncated`). `decimated_frames` (the fps= gate) is
            // deliberate, so it is reported but never folded into `dropped`.
            // `covered_us` is the actual captured window on the frame-stamp
            // clock; judge it against `requested_ms`.
            let dropped_total = take.dropped + take.evicted;
            let head_truncated = take.evicted > 0;
            let requested_ms = take.requested_ms;
            let covered_us = match (take.frames.front(), take.frames.back()) {
                (Some(first), Some(last)) => format!("[{}, {}]", first.t_us, last.t_us),
                _ => "null".to_string(),
            };
            let fps_cap = take
                .fps_cap
                .map_or_else(|| "null".to_string(), |n| n.to_string());
            let budget_mib = take.budget_bytes >> 20;
            // The recording's HONESTY disclosure: which texture the tap copied
            // (`mode`) and what the frame stamps therefore mean
            // (`stamp_semantics`) — a swapchain recording's bytes reached
            // photons; an offscreen-present-real recording's bytes are what
            // glass WOULD have shown, and the label says so.
            let mode_str = mode.as_str();
            let stamp_semantics = mode.stamp_semantics();
            let index = format!(
                "{{\n  \"meta\": {{\n    \"w\": {}, \"h\": {}, \"device_px\": [{}, {}],\n    \
                 \"half_res\": {}, \"format\": \"{}\", \"mode\": \"{mode_str}\",\n    \
                 \"clock\": \"metrics now_us; same epoch as inputs[] — attempt->later-frame delta = frame.t_us - input.t_us\",\n    \
                 \"input_semantics\": \"pre-routing character attempts; not PTY-delivery or visible-glyph receipts\",\n    \
                 \"stamp_semantics\": \"{stamp_semantics}\",\n    \
                 \"wall_start_us\": {started_us}, \"wall_stop_us\": {stop_us},\n    \
                 \"requested_ms\": {requested_ms}, \"covered_us\": {covered_us},\n    \
                 \"frames_written\": {written}, \"dropped_frames\": {dropped_total},\n    \
                 \"ring_skipped\": {}, \"evicted_frames\": {}, \"decimated_frames\": {},\n    \
                 \"head_truncated\": {head_truncated},\n    \
                 \"fps_cap\": {fps_cap}, \"budget_mib\": {budget_mib},\n    \
                 \"resized_early_stop\": {}\n  }},\n{analysis}  \"frames\": [\n{}\n  ],\n  \"inputs\": [\n{}\n  ]\n}}\n",
                take.w,
                take.h,
                take.device_px.0,
                take.device_px.1,
                take.half_res,
                take.format,
                take.dropped,
                take.evicted,
                take.decimated,
                take.resized_early_stop,
                frame_lines,
                input_lines,
            );
            // Write index.json ATOMICALLY: a short/interrupted write (ENOSPC, crash)
            // must not leave a TORN index.json, whose mere presence is the completion
            // marker meaning "every frame it names is readable" — a reader
            // (`video frames`, `newest_recording_with_index`) would then select a
            // half-written index and shadow a good prior recording. Write a temp
            // sibling, then `rename()` it into place (atomic within the dir), so a
            // reader ever sees either NO index or the WHOLE one, never a partial.
            let ok = snapshot_path::write_private_at(
                &dir,
                &std::ffi::OsString::from("index.json.tmp"),
                index.as_bytes(),
            )
            .is_ok()
                && std::fs::rename(dir.join("index.json.tmp"), dir.join("index.json")).is_ok();
            // Reply shape: new tokens go strictly BEFORE the path — the path
            // is ALWAYS the last whitespace token (the one client invariant).
            let _ = reply.send(if ok {
                format!(
                    "OK frames={written} dropped={dropped_total} head_truncated={head_truncated} {}\n",
                    dir.join("index.json").display()
                )
            } else {
                "ERR video: index.json write failed\n".to_string()
            });
        }
    }
}

/// Which window an introspection verb targets. The existing `image`/`window`/`chrome`
/// verbs only ever see the FRONTMOST terminal window (`App::front()`); the auxiliary
/// targets are compatibility names for other GUI surfaces and Settings routes that a
/// bare `window`/`chrome` doesn't name. Carried by
/// [`crate::Wake::CaptureAuxWindow`] / [`crate::Wake::ReadAuxControls`] so the
/// generalized `window <name>` / `controls <name>` verbs can reach any GUI screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AuxTarget {
    /// The frontmost terminal window (the existing `window`/`chrome` behavior).
    Front,
    /// The native Settings home route. `controls prefs` retains its legacy preference-row
    /// projection; versioned semantic inspection uses `inspect app/v1`.
    Prefs,
    /// The native Performance control panel (`App::_perf_panel`).
    Perf,
    /// The native Settings `/about` route. `controls about` retains its compatibility
    /// provenance projection.
    About,
    /// The in-window command PALETTE overlay (`WindowState::palette`) — an own-rendered card
    /// on the FRONT window, captured by the front `image`/`window` path. `controls menu`
    /// serialises its (filtered) command rows. The single source of truth for the menu.
    Menu,
    /// The native Settings `/updates` route. `controls update` retains its compatibility
    /// updater-state projection.
    Update,
}

impl AuxTarget {
    /// Parse a verb's target keyword (case-insensitive). Empty / `front` / `window` /
    /// `terminal` → [`AuxTarget::Front`]; `prefs` / `preferences` / `settings` →
    /// [`AuxTarget::Prefs`]; `perf` / `performance` → [`AuxTarget::Perf`]. An unrecognized
    /// keyword yields `None` so the verb can reject it with a clear error.
    pub(crate) fn parse(s: &str) -> Option<AuxTarget> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "front" | "window" | "terminal" => Some(AuxTarget::Front),
            "prefs" | "preferences" | "settings" => Some(AuxTarget::Prefs),
            "perf" | "performance" => Some(AuxTarget::Perf),
            "about" => Some(AuxTarget::About),
            "menu" | "palette" => Some(AuxTarget::Menu),
            "update" | "software-update" => Some(AuxTarget::Update),
            _ => None,
        }
    }

    /// The short keyword for this target (for error messages + default capture filenames).
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            AuxTarget::Front => "front",
            AuxTarget::Prefs => "prefs",
            AuxTarget::Perf => "perf",
            AuxTarget::About => "about",
            AuxTarget::Menu => "menu",
            AuxTarget::Update => "update",
        }
    }
}

/// PHASE-1 honesty gate ("Headless Present-Real"): may an explicit capture ADVANCE
/// the live cursor-effect engines? ONLY for a glass-less window — with no OS window
/// the capture IS that window's only present, so ticking is what keeps the effects
/// live. A WINDOWED target's capture must instead reuse its LAST-PRESENT quads:
/// ticking the engines for the capture would compose effect state NEWER than what
/// is actually on the glass — the sacred introspection contract is "exactly what
/// the screen shows", never newer.
///
/// PHASE-3 (one clock owner): while a `video` recording targets this window, its
/// offscreen present loop drives `tick_cursor_fx` — the recording loop OWNS the
/// engine clock, and a concurrent `image` must reuse the loop's last-present
/// quads exactly like a windowed capture does (ticking here would compose state
/// newer than the frame the recording just minted).
fn capture_ticks_cursor_fx(has_glass: bool, recording_this_window: bool) -> bool {
    !has_glass && !recording_this_window
}

/// Present-time placement of one frame axis inside one raw surface axis.
/// Positive remainder becomes leading/trailing background bands; negative
/// remainder becomes leading/trailing crop. The offset is exactly the shared
/// CPU/GPU [`aterm_render::band_offset`] rule used on glass.
fn dims_axis(surface: u32, frame: u32) -> (i64, u32, u32, u32, u32) {
    let offset = aterm_render::band_offset(surface as usize, frame as usize);
    let surface = i64::from(surface);
    let frame = i64::from(frame);
    let end = offset + frame;
    let band_before = offset.clamp(0, surface) as u32;
    let band_after = (surface - end).clamp(0, surface) as u32;
    let crop_before = (-offset).clamp(0, frame) as u32;
    let crop_after = (end - surface).clamp(0, frame) as u32;
    (offset, band_before, band_after, crop_before, crop_after)
}

fn dims_px(cells: u32, cell_px: u32, extra_px: u32) -> u32 {
    cells.saturating_mul(cell_px).saturating_add(extra_px)
}

impl App {
    /// Sample the selected terminal grid and all main-thread-owned presentation
    /// geometry as one event-turn record. A busy terminal returns immediately;
    /// callers can retry without ever parking the renderer behind this lock.
    pub(crate) fn try_dims_snapshot(
        &self,
        session: u64,
        term: &Arc<Mutex<Terminal>>,
    ) -> Result<DimsSnapshot, &'static str> {
        let (rows, cols) = match term.try_lock() {
            Ok(terminal) => (u32::from(terminal.rows()), u32::from(terminal.cols())),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                let terminal = poisoned.into_inner();
                (u32::from(terminal.rows()), u32::from(terminal.cols()))
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err("terminal busy; retry dims");
            }
        };
        // The guard is dropped at the end of the match before viewer sorting or
        // backend geometry lookup. Resize/zoom ownership is main-thread-only, so
        // same-turn coherence remains while the PTY reader is never held behind
        // unrelated introspection work.
        Ok(self.dims_snapshot(session, rows, cols))
    }

    /// Project one selected session through the LIVE geometry of a deterministic
    /// window that contains it. Prefer the front window when it contains the
    /// session; otherwise choose the lowest stable logical window id. This makes
    /// `@session dims` repeatable even when a shared session has multiple viewers.
    ///
    /// `rows`/`cols` are read from the target
    /// [`Terminal`](aterm_core::terminal::Terminal) by the `Wake::ReadDims`
    /// handler immediately before this projection, in the same main-loop turn.
    /// The handler uses `try_lock`, so a busy grid produces an explicit retryable
    /// error instead of either a split-generation record or an event-loop stall.
    pub(crate) fn dims_snapshot(&self, session: u64, rows: u32, cols: u32) -> DimsSnapshot {
        let mut windows: Vec<crate::WindowId> = self
            .windows
            .keys()
            .copied()
            .filter(|wid| self.window_contains_session(*wid, session))
            .collect();
        windows.sort_unstable_by_key(|wid| wid.0);
        let visible_viewers = self.windows_displaying(session).count();
        let selected = self
            .frontmost_window
            .filter(|wid| windows.contains(wid))
            .or_else(|| windows.first().copied());

        let (
            cell_w,
            cell_h,
            font_px,
            window,
            window_rows,
            window_cols,
            composed_rows,
            pad,
            pad_top,
            head,
            tab_rows,
            hud_rows,
            frame_w,
            frame_h,
            surface_w,
            surface_h,
            geometry,
        ) = if let Some(wid) = selected {
            let ws = &self.windows[&wid];
            // The listener is published just before the first window attaches,
            // while the async backend may still be `Pending`. Use the same
            // bounded seed only for that short pre-attach interval; every ready
            // window resolves the exact live face metrics at its own font px.
            let (cell_w, cell_h) = if self.backend.is_pending() {
                crate::seed_cell_px(ws.metrics.font_px)
            } else {
                let (cell_w, cell_h, _) = self.backend.cell_geometry(ws.metrics.font_px);
                (cell_w, cell_h)
            };
            let cell_w = u32::try_from(cell_w).unwrap_or(u32::MAX);
            let cell_h = u32::try_from(cell_h).unwrap_or(u32::MAX);
            let pad = u32::try_from(ws.metrics.pad).unwrap_or(u32::MAX);
            let pad_top = u32::try_from(ws.metrics.pad_top).unwrap_or(u32::MAX);
            let head = u32::try_from(ws.metrics.head).unwrap_or(u32::MAX);
            let tab_rows = u32::from(self.tab_strip_rows);
            let hud_rows = u32::from(self.hud_rows.min(ws.hud_cap));
            let window_rows = u32::from(ws.rows);
            let window_cols = u32::from(ws.cols);
            let composed_rows = window_rows
                .saturating_add(tab_rows)
                .saturating_add(hud_rows);
            let frame_w = dims_px(window_cols, cell_w, pad.saturating_mul(2));
            let frame_h = dims_px(
                composed_rows,
                cell_h,
                pad_top.saturating_add(pad).saturating_add(head),
            );
            let (surface_w, surface_h, geometry) = match ws.win_px {
                Some(size) => (size.width.max(1), size.height.max(1), "window"),
                None if self.headless => (frame_w, frame_h, "headless"),
                None => (frame_w, frame_h, "pre_attach"),
            };
            (
                cell_w,
                cell_h,
                ws.metrics.font_px,
                Some(wid.0),
                window_rows,
                window_cols,
                composed_rows,
                pad,
                pad_top,
                head,
                tab_rows,
                hud_rows,
                frame_w,
                frame_h,
                surface_w,
                surface_h,
                geometry,
            )
        } else {
            // A session can be briefly detached while a tab/window transition is
            // being installed. Report the live shared renderer truth and make the
            // absence explicit instead of falling back to a startup estimate.
            let (cell_w, cell_h) = if self.backend.is_pending() {
                crate::seed_cell_px(self.font_px)
            } else {
                self.cell_size()
            };
            let cell_w = u32::try_from(cell_w).unwrap_or(u32::MAX);
            let cell_h = u32::try_from(cell_h).unwrap_or(u32::MAX);
            let (pad, pad_top, head) = if self.backend.is_pending() {
                (crate::pad_for_scale(1.0), crate::pad_top_for_scale(1.0), 0)
            } else {
                (
                    self.backend.pad(),
                    self.backend.pad_top(),
                    self.backend.head(),
                )
            };
            let pad = u32::try_from(pad).unwrap_or(u32::MAX);
            let pad_top = u32::try_from(pad_top).unwrap_or(u32::MAX);
            let head = u32::try_from(head).unwrap_or(u32::MAX);
            let frame_w = dims_px(cols, cell_w, pad.saturating_mul(2));
            let frame_h = dims_px(
                rows,
                cell_h,
                pad_top.saturating_add(pad).saturating_add(head),
            );
            (
                cell_w,
                cell_h,
                self.font_px,
                None,
                rows,
                cols,
                rows,
                pad,
                pad_top,
                head,
                0,
                0,
                frame_w,
                frame_h,
                frame_w,
                frame_h,
                "detached",
            )
        };

        let (offset_x, band_left, band_right, crop_left, crop_right) =
            dims_axis(surface_w, frame_w);
        let (offset_y, band_top, band_bottom, crop_top, crop_bottom) =
            dims_axis(surface_h, frame_h);
        let (
            present_retry_state,
            present_retry_count,
            present_retry_remaining,
            present_retry_in_ms,
        ) = selected.map_or(("detached", 0, 0, None), |wid| {
            let retry = &self.windows[&wid].present_retry;
            let state = if retry.parked {
                "parked"
            } else if retry.deadline.is_some() {
                "backoff"
            } else if retry.recovery_redraw_outstanding {
                "redraw"
            } else {
                "ready"
            };
            let now = Instant::now();
            let in_ms = retry.deadline.map(|deadline| {
                let duration = deadline.saturating_duration_since(now);
                u64::try_from(duration.as_millis())
                    .unwrap_or(u64::MAX)
                    .max(u64::from(!duration.is_zero()))
            });
            (
                state,
                u32::from(retry.autonomous_retries),
                u32::from(crate::PRESENT_RETRY_CAP.saturating_sub(retry.autonomous_retries)),
                in_ms,
            )
        });

        DimsSnapshot {
            session,
            rows,
            cols,
            pixel_w: dims_px(cols, cell_w, 0),
            pixel_h: dims_px(rows, cell_h, 0),
            cell_w,
            cell_h,
            font_px,
            window,
            window_rows,
            window_cols,
            composed_rows,
            frame_w,
            frame_h,
            surface_w,
            surface_h,
            offset_x,
            offset_y,
            band_left,
            band_right,
            band_top,
            band_bottom,
            crop_left,
            crop_right,
            crop_top,
            crop_bottom,
            pad,
            pad_top,
            pad_bottom: pad,
            head,
            tab_rows,
            hud_rows,
            viewers: u32::try_from(windows.len()).unwrap_or(u32::MAX),
            visible_viewers: u32::try_from(visible_viewers).unwrap_or(u32::MAX),
            geometry,
            present_retry_state,
            present_retry_count,
            present_retry_remaining,
            present_retry_in_ms,
        }
    }

    /// Build this window's sparkle-word decorations into `input_scratch`, mirroring the
    /// live redraw (`redraw_window`), so the headless `image`/`snapshot` capture renders
    /// the SAME sparkle/cat-paw/orca-splash decorations as the glass — WYSIWYG. Must run
    /// AFTER `cell_frame_into` and BEFORE the tab-strip/HUD splices (which shift the
    /// decorations down with the grid). No-op when the feature is off, or on the
    /// alt-screen with `suppress_in_alt_screen` set (default off — the capture
    /// decorates full-screen TUIs exactly like the glass).
    pub(crate) fn splice_word_decorations(&mut self, wid: crate::WindowId, now: Instant) {
        // Same redraw-safe atomic/try-recv poll as the on-glass path. Image
        // capture never opens or decodes the configured sprite on the UI thread.
        let _ = self.poll_nyan_sprite_and_fanout();
        // Resolve the sparkle config if it hasn't been (headless never runs
        // `redraw_window`, which is the only other site that recomputes it), so the
        // capture reflects the configured decorations.
        if self.sparkle_dirty {
            self.recompute_sparkle();
        }
        // PHOSPHOR rain: keep the cached resolve fresh on the headless path
        // too (a reload/toggle before any windowed present must still land),
        // mirroring the sparkle gate above.
        if self.rain_dirty {
            self.recompute_matrix_rain();
        }
        // Rain is EXCLUDED from introspection/control-socket captures by never
        // splicing it (design §6: captures render the three rain channels
        // EMPTY — rain would pollute VLM/OCR reads and make byte-identical
        // grids diff in the rain layer; the `include_rain` opt-in is deferred
        // to v1.1). The capture reuses the window's live `input_scratch` and
        // `cell_frame_into` does not touch overlay channels, so a capture
        // right after a rainy present would inherit stale quads — clear the
        // three channels explicitly, BEFORE the sparkle early-returns below.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.input_scratch.rain_quads.clear();
            ws.input_scratch.rain_atlas = None;
            ws.input_scratch.rain_add.clear();
        }
        // Capture and glass install the same exact admitted outer catalog Arc.
        // Keep this before all feature early-outs so a capture can never retain
        // an earlier Nyan generation merely because word sparkles are disabled.
        self.install_window_config_assets(wid);
        let Some((cfg, lexicon)) = self
            .sparkle
            .as_ref()
            .map(|r| (r.cfg.clone(), r.lexicon.clone()))
        else {
            return;
        };
        // The same MOTION POLICY the glass present resolves (W11), so the capture
        // is WYSIWYG: a reduced/unfocused window captures its static decorations.
        // Folded into the effects engine's own `reduced_motion` seam below (it
        // cannot depend on `crate::motion`). Includes the same `motion_focus`
        // recording pin the glass resolves, so capture and glass never diverge.
        let motion = self.motion_policy(
            self.motion_focus(wid, self.windows.get(&wid).is_some_and(|ws| ws.focused)),
        );
        let cfg = if !motion.animate(crate::motion::MotionEffect::WordSparkles) {
            let mut c = cfg;
            c.reduced_motion = true;
            c
        } else {
            cfg
        };
        // Cell metrics for the cat emitter (§5.2 geometry / §5.7 floors); read
        // before the `ws` borrow — like the Kitty Log recorder gate (§F4.7).
        let (cell_w, cell_h) = self.backend.cell_size();
        let kitty_log_on = self.kitty_log_enabled();
        let companion_look = self.kitty_log.companion_look();
        let glow_cfg = self.glow_config();
        // The same alt-screen policy the live present resolves (WYSIWYG): only a
        // configured `suppress_in_alt_screen` blanks the capture's decorations.
        let suppress_alt = self.config.sparkle_suppress_alt_screen();
        // The same effective load-shed gate the live present folds into
        // `deco_suspend` (app_render). The raw diagnostic latch does not
        // suppress a user-forced Full policy or an explicit adaptive opt-out.
        let load_shed = self.load_shed_active();
        let windowless = self.headless;
        let Some(front_terminal) = self.front_terminal_mirror(wid) else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // Capture consumes the same exact admitted catalog Arc as glass. Asset
        // installation above is Arc/scalar-only and cannot perform filesystem
        // or decode work.
        if let Some(look) = companion_look {
            ws.cursor_cat.set_look(look);
        }
        // The capture itself is a presentation boundary. This resumes a hello
        // that was discovered after the preceding capture had already resolved
        // its frame and was therefore paused unseen below.
        ws.cursor_cat.set_collection_presentable(now, true);
        // Explicit captures have no animation timer. A focused/full-motion
        // window samples its live lifecycle; reduced or windowless captures
        // show a collection as one full-opacity still, never a synthetic idle
        // animation. Ordinary earned Nyan remains focus + full-motion gated.
        let animate_cat = !windowless
            && ws.os_window.is_some()
            && ws.focused
            && motion.animate(crate::motion::MotionEffect::CursorGlow);
        let cat_frame = if animate_cat {
            ws.cursor_cat.frame(now)
        } else {
            ws.cursor_cat.static_frame(now)
        };
        let nyan_enabled = (animate_cat
            && matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Nyan))
            || cat_frame.collection_hello;
        let (rows, cols) = (ws.rows as usize, ws.cols as usize);
        let effect_geom = crate::word_decorations::EffectGeom {
            cell_w: cell_w as u16,
            cell_h: cell_h as u16,
            rows: rows as u16,
            cols: cols as u16,
        };
        let mut term = term_lock(&front_terminal.term);
        // Refill under the SAME lock as the damage-epoch read + effect rescan.
        // `render_image` takes an initial snapshot before entering this helper,
        // but PTY output can land in that narrow gap.  A damage epoch is latched
        // for the whole outstanding damage session, so comparing only the two
        // epoch values cannot detect every such write.  Explicit captures are a
        // cold path; one allocation-reusing grid refill here makes the cells,
        // line sizes, cursor and epoch one coherent observation and prevents a
        // fresh word from being consumed against a stale snapshot.
        term.cell_frame_into(&mut ws.input_scratch, rows, cols);
        if (term.is_alternate_screen() && suppress_alt) || load_shed {
            // The capture cannot draw decorations after all. Undo the
            // presentation opportunity sampled above at the same instant, so
            // a one-shot collection hello cannot age out behind this cleared
            // early return.
            ws.cursor_cat.set_collection_presentable(now, false);
            // v3 §1.1 reset table: BOTH suspension causes are freeze/thaw, not
            // resets — mirrors app_render's `deco_suspend` arm exactly, so a
            // capture mid-suspension preserves every episode (the engine's own
            // frozen guards additionally no-op any rescan/tick) and recovery
            // resumes each animation where it paused instead of mass-replaying.
            ws.word_decos.freeze(now);
            ws.free_scratch.clear();
            ws.nova_scratch.clear();
            ws.input_scratch.word_decorations.clear();
            ws.input_scratch.ink.clear();
            ws.input_scratch.cat_quads.clear();
            ws.input_scratch.cat_atlas = None;
            ws.input_scratch.free_sprites.clear();
            ws.input_scratch.free_atlas = None;
            ws.input_scratch.nova_add.clear();
            return;
        }
        // Resume from a freeze (perf_reduced cleared / alt-screen exit with
        // suppression) before the rescan/tick read the clock — a no-op when
        // not frozen. Mirrors app_render's thaw-before-rescan ordering.
        ws.word_decos.thaw(now);
        let epoch = term.damage_epoch();
        let word_cursor = (term.grid().display_offset() == 0).then(|| {
            let cursor = term.cursor();
            (cursor.row, cursor.col)
        });
        let mut prime_at = None;
        if ws.word_decos.needs_rescan(epoch) {
            // A normal glass present consumes `pending_deco_birth`. If the
            // stamp is still here, capture is this episode's first observable
            // presentation (headless or an occluded window), so an old stamp
            // clamps to one fully-risen preview instead of aging an unseen peek
            // through Done. This remains bounded work only on a damaged
            // capture, never a synthetic animation loop.
            let birth = capture_rescan_birth(now, ws.pending_deco_birth.take(), windowless);
            ws.word_decos.rescan_from_cells_with_geom_at_cursor(
                &ws.input_scratch.cells,
                &ws.input_scratch.line_sizes,
                rows,
                cols,
                &lexicon,
                &cfg,
                epoch,
                birth,
                effect_geom,
                ws.input_scratch.default_bg,
                word_cursor,
            );
            if birth < now {
                prime_at = Some(birth);
            }
        }
        // Consume the damage session (the capture IS this surface's present).
        // The epoch latch (`Terminal::damage_epoch` counts at most once per
        // session, re-armed only by `take_damage`) is otherwise never re-armed
        // in HEADLESS mode — no OS window means `redraw_window` never runs —
        // so after the first capture the epoch froze forever: `clear` + new
        // output kept the OLD occurrences (stale cats painted over new text)
        // and newly typed words never decorated. A windowed present is
        // unaffected: its early-out compares epoch VALUES, and the main
        // thread serializes captures against `redraw_window`.
        term.take_damage();
        // The capture tick shares the window's live effect state, so it must
        // be FAITHFUL to the screen: the same cursor cell (§5.8 gaze — read
        // under this same lock) and the window's real focus (so a capture
        // never clobbers a focused window's armed blink one-shot, and a
        // headless/unfocused capture arms nothing).
        let cpos = term.cursor();
        let cur = term.cursor_visible().then_some((cpos.row, cpos.col));
        // The same selection view the animated tick sees (§6.4 nova ignition
        // deferral / per-quad attenuation) — a capture must not ignite a nova
        // the window itself would defer.
        let sel_view = crate::word_decorations::SelView {
            sel: term.text_selection(),
            display_offset: term.grid().display_offset() as i32,
        };
        let mut primed_wince_hits = 0u8;
        if let Some(birth) = prime_at {
            // Discard the birth frame's zero-reveal output. Its state mutations
            // (phase latch / limiter decisions) are exactly what an ordinary
            // damage-driven present would have performed at output time.
            ws.word_decos.tick(
                birth,
                &cfg,
                effect_geom,
                cur,
                Some(sel_view),
                ws.focused,
                &mut ws.deco_scratch,
                &mut ws.ink_scratch,
                &mut ws.free_scratch,
                &mut ws.nova_scratch,
            );
            // The final `now` tick below clears per-tick cue storage. Preserve
            // only the visual site count from this synthetic birth sample;
            // capture audio remains unconditionally muted.
            primed_wince_hits = crate::app_render::drain_curse_bonk_cues(
                &mut ws.word_decos,
                glow_cfg.style,
                effect_geom.cols,
                None,
                false,
                |_| {},
            )
            .wince_hits;
        }
        ws.word_decos.tick(
            now,
            &cfg,
            effect_geom,
            cur,
            Some(sel_view),
            ws.focused,
            &mut ws.deco_scratch,
            &mut ws.ink_scratch,
            &mut ws.free_scratch,
            &mut ws.nova_scratch,
        );
        // Kitty Log drain (§F4.3): the capture tick is a REAL tick — the
        // sightings vec clears at the next tick's start, so a headless-only
        // session must drain here too or its sightings are lost. Same
        // (session, ident) dedupe as the windowed drain, so a capture racing
        // a present never double-counts.
        let discovered = self.kitty_log.observe(
            front_terminal.session,
            ws.word_decos.drain_kitty_sightings(),
            &lexicon,
            now,
            kitty_log_on,
        );
        // Curse cues share the glass drain's exact site dedupe. A capture is a
        // recording-shaped surface and must never make the Mac speak, so gain
        // stays `None` and no sound can escape; the VISUAL reaction still
        // belongs to the cursor companion. `cat_frame` was sampled above, so a
        // headless caller observes the wince on its next explicit capture.
        let curse_drain = crate::app_render::drain_curse_bonk_cues(
            &mut ws.word_decos,
            glow_cfg.style,
            effect_geom.cols,
            None,
            false,
            |_| {},
        );
        ws.cursor_cat.on_curse(
            now,
            primed_wince_hits.saturating_add(curse_drain.wince_hits),
        );
        if let Some(look) = discovered {
            // Match the on-glass path: the newly collected identity receives a
            // guaranteed bounded hello beginning now. The next requested
            // windowless capture presents its static discovery pose.
            ws.cursor_cat.on_collect(now, look);
            // `cat_frame` was resolved before observation, so this capture did
            // not contain the new hello. Freeze its full hold until the next
            // capture or on-glass frame actually has a chance to present it.
            ws.cursor_cat.set_collection_presentable(now, false);
        }
        if nyan_enabled
            && cat_frame.alpha > 0
            && let Some(cell) = cur
        {
            let layout = aterm_effects::word_decorations::NyanCursorLayout {
                geom: effect_geom,
                cursor: cell,
                look: cat_frame.render_look(),
                bob: cat_frame.bob,
            };
            if let Some(footprint) = ws.word_decos.nyan_cursor_footprint(layout) {
                let colors = ws.cursor_cat.episode_colors().unwrap_or_else(|| {
                    let sampled = crate::app_render::cursor_cat_color_key(
                        &ws.input_scratch.cells,
                        effect_geom,
                        footprint,
                        ws.input_scratch.default_bg,
                        ws.input_scratch.cursor_color,
                        glow_cfg.accent,
                    );
                    ws.cursor_cat.colors_for_episode(sampled)
                });
                let _ = ws.word_decos.nyan_cursor(
                    aterm_effects::word_decorations::NyanCursorFrame {
                        geom: effect_geom,
                        cursor: cell,
                        look: layout.look,
                        colors,
                        bob: cat_frame.bob,
                        alpha: cat_frame.alpha,
                        // Windowless capture is the reduced-motion still path, so
                        // this is always the neutral pose; thread it through so
                        // introspection and on-glass share one emission contract.
                        pose: cat_frame.pose,
                        // The still capture carries the singing FACE (`sing`
                        // reaches `render_look` upstream via `cat_frame`) but
                        // no note stream: notes are an animation channel, and
                        // the windowless path draws none.
                        sing: cat_frame.sing,
                        notes: [None; aterm_effects::nyan_sing::MAX_NOTES],
                    },
                    &mut ws.free_scratch,
                );
            }
        }
        drop(term);
        ws.input_scratch
            .word_decorations
            .clone_from(&ws.deco_scratch);
        // Ink is part of the styled capture too (WYSIWYG); the `plain` capture
        // strips it via `clear_overlays` like every other overlay channel.
        ws.input_scratch.ink.clone_from(&ws.ink_scratch);
        // Overlay Phase 4: the engine no longer produces legacy per-row cat
        // quads — keep the channel empty (nothing else feeds it here).
        ws.input_scratch.cat_quads.clear();
        ws.input_scratch.cat_atlas = None;
        // And the free-overlay sprites (overlay Phase 4: the peeking cat +
        // its gaze dots) — without this copy a headless styled capture
        // silently loses the cat and the driven A-gates pass vacuously
        // (v3 §5). Stripped by `plain` via `clear_overlays` like every other
        // overlay channel.
        ws.input_scratch.free_sprites.clone_from(&ws.free_scratch);
        ws.input_scratch.free_atlas = if ws.free_scratch.is_empty() {
            None
        } else {
            ws.word_decos.free_atlas()
        };
        // And the supernova light (additive overlay, stripped by `plain` too).
        ws.input_scratch.nova_add.clone_from(&ws.nova_scratch);
    }

    /// Test-only cross-module seam for driving the real word-decoration host
    /// path in native scheduler regressions.
    #[cfg(test)]
    pub(crate) fn splice_word_decorations_for_test(&mut self, wid: crate::WindowId, now: Instant) {
        self.splice_word_decorations(wid, now);
    }

    /// PHASE 1 of "Headless Present-Real": advance the cursor-effect engines
    /// (LUMEN glow / FORGE fire / rainbow / cadence-comet trail) at capture time
    /// via the SAME extracted pass the windowed present runs
    /// ([`App::tick_cursor_fx`]) and splice the produced scratches into
    /// `input_scratch` exactly as `redraw_window`'s commit does — so a headless
    /// `image` carries the LIVE effect state (the fire at its true thermal/decay
    /// state) instead of the stale-or-empty overlays a never-presenting window
    /// would keep. STRICTLY headless-gated ([`capture_ticks_cursor_fx`]): a
    /// WINDOWED target keeps its last-present quads. `image plain` still strips
    /// the spliced layers via `clear_overlays` downstream, and this runs BEFORE
    /// `splice_tab_strip` (the strip shifts the quads down with the grid).
    fn splice_cursor_fx(&mut self, wid: crate::WindowId, now: Instant) {
        // ONE engine clock: while a recording targets this window its offscreen
        // present loop owns the tick (see `capture_ticks_cursor_fx`).
        let recording_here = self.video_rec.as_ref().is_some_and(|r| r.window == wid);
        let Some(front_terminal) = self.front_terminal_mirror(wid) else {
            return;
        };
        let inputs = {
            let Some(ws) = self.windows.get_mut(&wid) else {
                return;
            };
            if !capture_ticks_cursor_fx(ws.os_window.is_some(), recording_here) {
                return;
            }
            let (rows, cols) = (ws.rows as usize, ws.cols as usize);
            // The same per-frame terminal state `redraw_window`'s LOCK A reads
            // for this pass, snapshotted under ONE lock (cursor + colours stay
            // one coherent observation). The ERASE-POOF probe below is the one
            // grid-SCANNING effect input (the rest are cursor-positioned) — it
            // reads the live grid under this same lock into its own resident
            // buffer, so the caller's earlier grid refill stays valid.
            let term = term_lock(&front_terminal.term);
            let cpos = term.cursor();
            let cursor_visible = term.cursor_visible();
            let dbg = if term.modes().reverse_video() {
                term.default_foreground()
            } else {
                term.default_background()
            };
            // ERASE-POOF probe, headless parity: the exact LOCK A capture +
            // guards from `redraw_window`, so a control-socket-driven capture
            // session exercises the same kill-poof seam a windowed present
            // would (captures are sparse; the engine's probe-staleness cap
            // fences any long gap between them).
            let display_offset = term.grid().display_offset();
            let scrollback_lines = term.grid().scrollback_lines();
            let is_alt = term.is_alternate_screen();
            // REPAINT-BLINK edge + context feed — the windowed LOCK A
            // detector's twin, so a headless capture classifies the same.
            let blink_epoch = term.repaint_blink_epoch();
            if ws.blink_reseed {
                // Tab/pane switch: adopt the NEW terminal's epoch silently —
                // a cross-terminal mismatch is not a repaint (see sync_window).
                ws.blink_reseed = false;
                ws.blink_epoch_seen = blink_epoch;
            } else if blink_epoch != ws.blink_epoch_seen {
                ws.blink_epoch_seen = blink_epoch;
                ws.last_blink_at = Some(now);
                ws.cursor_glow.note_repaint_blink(now);
                ws.cursor_trail.note_repaint_blink(now);
            }
            ws.cursor_glow.note_context(is_alt);
            ws.cursor_trail.note_context(is_alt);
            let blink_recent = ws.last_blink_at.is_some_and(|t| {
                now.saturating_duration_since(t) <= crate::app_render::BLINK_RECENT_MAX
            });
            let probe_ok = !is_alt || blink_recent;
            let row_probe = if probe_ok
                && display_offset == 0
                && ws.poof_scrollback == Some(scrollback_lines)
            {
                let _fill = term.row_cols_into(cpos.row as usize, &mut ws.poof_row_buf);
                // STAR-LANDING NEIGHBORS — the windowed LOCK A capture's
                // twin, so a headless capture licenses (or forbids) the
                // displaced nyan stars exactly as a windowed present would.
                if cpos.row > 0 {
                    term.row_cols_into(cpos.row as usize - 1, &mut ws.poof_row_above_buf);
                }
                if (cpos.row as usize) + 1 < rows {
                    term.row_cols_into(cpos.row as usize + 1, &mut ws.poof_row_below_buf);
                }
                Some((cpos.row, cpos.col))
            } else {
                None
            };
            // Scroll translation + fenced-frame probe drop — the windowed
            // path's twins (captures are sparse, so the delta may span many
            // frames; the anchor translation still bounds at the grid height).
            let scrolled = ws
                .poof_scrollback
                .map_or(0, |p| scrollback_lines.saturating_sub(p));
            if scrolled > 0 {
                let d = scrolled.min(rows).min(u16::MAX as usize) as u16;
                ws.cursor_glow.note_scroll(d);
                ws.cursor_trail.note_scroll(d);
                ws.cursor_glow.drop_row_probe();
            }
            ws.poof_scrollback = Some(scrollback_lines);
            crate::app_render::CursorFxInputs {
                now,
                rows,
                cols,
                // Scrolled into history ⇒ no cursor for the effect engines — the
                // windowed path's twin (active-grid coords over scrollback rows
                // would spawn light on unrelated history lines in the capture).
                cur: (cursor_visible && display_offset == 0).then_some((cpos.row, cpos.col)),
                cursor_visible,
                cursor_style: term.cursor_style(),
                blink_phase: ws.blink_phase,
                live_cursor_rgb: term.cursor_color().map(|c| [c.r, c.g, c.b]),
                default_bg: aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]),
                row_probe,
            }
        };
        let Some(fx) = self.tick_cursor_fx(wid, inputs) else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // The exact `redraw_window` commit splice for these channels: the aurora
        // quads (grid-interior px), the RADIAL halos (fire embers / crown /
        // impact flash — same coords, same splice rules), the block-fill
        // override (rainbow wins over FORGE), and the comet cells + the heated
        // colour they render at.
        ws.input_scratch
            .cursor_glow_add
            .clone_from(&ws.glow_scratch);
        ws.input_scratch.glow_halo.clear();
        ws.input_scratch
            .glow_halo
            .extend_from_slice(ws.cursor_glow.halos());
        ws.input_scratch.fire_patch.clear();
        ws.input_scratch
            .fire_patch
            .extend_from_slice(ws.cursor_glow.patches());
        ws.input_scratch.glow_under.clear();
        ws.input_scratch
            .glow_under
            .extend_from_slice(ws.cursor_glow.under_quads());
        ws.input_scratch.char_fg.clear();
        ws.input_scratch
            .char_fg
            .extend_from_slice(ws.cursor_glow.charred());
        ws.input_scratch.fire_halo.clear();
        ws.input_scratch
            .fire_halo
            .extend_from_slice(ws.cursor_glow.halo_cells());
        // The full fill-override chain, mirroring `redraw_window`'s splice (the
        // styles are mutually exclusive, so at most one is ever `Some`).
        ws.input_scratch.cursor_fill_override = fx
            .rainbow_fill
            .or(fx.forge_fill)
            .or(fx.bolt_fill)
            .or(fx.phaser_fill)
            .or(fx.comet_fill)
            .or(fx.droplet_fill)
            .or(fx.beamrod_fill);
        ws.input_scratch.cursor_trail.clone_from(&ws.trail_scratch);
        ws.input_scratch.cursor_trail_color = fx.trail_color;
    }

    /// Introspect the live screen: render the CURRENT terminal to a PNG (the
    /// exact pixels on screen, via the same renderer the window uses) and write a
    /// parallel .txt of the visible text. Triggered by SIGUSR1. The files are
    /// written 0600 into the per-user 0700 control dir by default;
    /// $ATERM_SNAPSHOT_PATH overrides only into a safe dir (see `snapshot_path`).
    pub(crate) fn snapshot(&mut self) {
        let Some(path) = snapshot_path::resolve() else {
            return; // refusal already logged by resolve()
        };
        let Some(front) = self.frontmost_window else {
            return;
        };
        let Some(front_terminal) = self.front_terminal_mirror(front) else {
            return;
        };
        let strip_rows = self.tab_strip_rows as usize;
        // Trailing HUD rows are chrome too — captured here so the .txt below can drop
        // them (the front borrow can't read `self.*`). Use the window's EFFECTIVE HUD
        // count (`min(hud_rows, hud_cap)`) so the trim matches what `splice_hud_bar`
        // actually appended on a window too short for the full stack.
        let hud_rows = self
            .hud_rows
            .min(self.windows.get(&front).map_or(u16::MAX, |ws| ws.hud_cap))
            as usize;
        let (rows, cols) = match self.windows.get(&front) {
            Some(ws) => (ws.rows as usize, ws.cols as usize),
            None => return,
        };
        // Lock only to snapshot the grid; render + serialize without the lock.
        let theme_cursor = self.theme.cursor & 0x00FF_FFFF;
        {
            let Some(ws) = self.windows.get_mut(&front) else {
                return;
            };
            let mut term = term_lock(&front_terminal.term);
            // REFILL the reused snapshot in place (no per-frame container-Vec alloc).
            // A-3: the ENGINE builds the snapshot (`Terminal::cell_frame_into`).
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
            // WYSIWYG: the `image`/`snapshot` introspection must match the GLASS, so
            // populate the live OSC 11/111/DECSCNM default-bg and OSC 12/112 cursor
            // colour exactly as the windowed `redraw_window` does — otherwise a program
            // that recoloured the background/cursor would render here in the static
            // theme while the real window shows the live colour.
            let dbg = if term.modes().reverse_video() {
                term.default_foreground()
            } else {
                term.default_background()
            };
            ws.input_scratch.default_bg = aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]);
            ws.input_scratch.cursor_color = term
                .cursor_color()
                .map_or(theme_cursor, |c| aterm_render::rgb_to_u32([c.r, c.g, c.b]));
        }
        // PHASE 1 of "Headless Present-Real": a GLASS-LESS window never presents, so
        // its cursor-effect engines only ever advance HERE — tick them at capture
        // time and splice the live quads exactly as a present would, so the snapshot
        // shows the fire at its true heat/decay. Headless-gated inside: a WINDOWED
        // capture keeps its last-present quads (never newer than the glass).
        self.splice_cursor_fx(front, Instant::now());
        // Sparkle-word decorations (orca splash / profanity sparkle / cat-paw), so the
        // capture shows the same decorations as the glass. Before the tab-strip shift.
        self.splice_word_decorations(front, Instant::now());
        // WYSIWYG: the on-screen present splices the tab strip above the terminal
        // grid, so splice it here too — the snapshot pixels then match the glass. A
        // no-op when the strip is disabled. Done BEFORE the disjoint-field borrow.
        self.splice_tab_strip(front);
        // Sample the interval-driven panels here too: headless has no window tick,
        // so this is what makes CPU/net/app-fed live in `image`/`snapshot` output.
        // SKIPPED when the windowed HUD tick is driving (same gate as `render_image`):
        // the sample is a process-tree + sysctl scan on the event-loop thread, and the
        // capture then shows the tick's figures — the same numbers the on-glass HUD
        // shows (WYSIWYG). Headless, and a windowed front with the tick DISARMED
        // (HUD off / unfocused), still sample here so the capture stays live.
        let hud_tick_driving = self
            .windows
            .get(&front)
            .is_some_and(|ws| ws.os_window.is_some() && ws.next_hud_tick.is_some());
        if !hud_tick_driving {
            let hud_now = Instant::now();
            let tab_pid = self.frontmost_tab_pid();
            self.metrics_service.sample(tab_pid, None, hud_now);
            for p in &mut self.panels {
                p.poll(hud_now);
            }
        }
        // WYSIWYG: the live present paints the Cmd-F find bar over the bottom terminal row
        // while searching, so splice it here too — a headless `image`/`snapshot` then shows
        // the find bar exactly as the glass does. A no-op when not searching.
        self.splice_find_bar(front);
        self.splice_hud_bar(front);
        // OVERLAY the modal Settings panel (a no-op when closed), exactly as the live
        // present does — so the SACRED `image`/`snapshot` introspection captures the open
        // settings surface WYSIWYG (what an AI reads == what is on glass). Completes
        // introspection of the settings overlay for headless verification.
        self.splice_settings_panel(front);
        // Subtle top-right build/version badge — same paint-only slot the live present
        // uses, so the headless capture shows it WYSIWYG (composite prefers the modal).
        self.splice_build_badge(front);
        self.splice_notice(front);
        // The LEVEL-UP arrow burst — same paint-only slot the live present uses (priority
        // over the pill), so a headless capture during the celebration is WYSIWYG.
        self.splice_level_up(front);
        // OVERLAY the transient config-notice banner LAST (topmost), exactly as the live
        // present does (app_render splices it after the settings panel) — so the SACRED
        // `image`/`snapshot` capture shows a restart-only / dropped-rule notice WYSIWYG
        // (what an AI reads == what is on glass). A no-op when no banner is up.
        self.splice_config_notice(front);
        // Accent for the drop-target highlight / level-up glow, read before the disjoint
        // borrow. The level-up glow's breathing alphas are sampled here too (matching the
        // on-glass present) so a capture during the celebration shows the pulsing frame.
        let accent = self.theme.cursor;
        let level_up_glow = self
            .level_up
            .as_ref()
            .map(|l| (l.wash_alpha(Instant::now()), l.border_alpha(Instant::now())));
        // Disjoint borrows: `self.backend` (renderer), the introspection GPU
        // scratch, and the front window's input_scratch are separate fields.
        let App {
            backend,
            introspect_gpu,
            windows,
            ..
        } = self;
        let Some(ws) = windows.get_mut(&front) else {
            return;
        };
        // pixels: the same offscreen frame the window blits on screen (GPU path
        // if active) — byte-identical, so the AI sees exactly what is presented.
        // `backend.render_input` returns an owned Frame on both backends (the
        // snapshot/image path keeps the pixels past the next render, unlike the
        // borrowing window hot path).
        // P3 settings card → raw bytes + device-px rect for the GPU tray quad (same
        // builder the live present uses). The GPU arm BAKES it into the offscreen so the
        // readback is WYSIWYG; the CPU arm ignores it (composited below, gated on !is_gpu).
        // Modal card FIRST, else the transient update notice, else the build/version badge.
        let tray_arg = ws
            .settings_card
            .as_ref()
            .or(ws.level_up_card.as_ref())
            .or(ws.notice_card.as_ref())
            .or(ws.badge_card.as_ref())
            .map(|c| aterm_gpu::TrayQuad {
                rgba: c.rgba.as_slice(),
                pw: c.pw,
                ph: c.ph,
                dx: c.dx,
                dy: c.dy,
            });
        let mut frame = backend.render_input(introspect_gpu, &mut ws.input_scratch, tray_arg);
        // I-2: WYSIWYG — the on-screen present inverts the whole frame during a
        // visual-bell flash (CPU `src ^ 0x00ff_ffff`; GPU blit shader). Apply the
        // SAME invert here so a snapshot taken DURING a flash matches the glass
        // instead of showing the un-inverted frame.
        // Suppressed while ANY modal overlay (Settings, About, or the command Palette) is open (mirrors the live
        // present), so the card and the terminal behind it never invert/wash — snapshot == glass.
        apply_bell_invert(
            &mut frame,
            ws.bell_flash.is_active(Instant::now()) && !ws.overlay_open(),
        );
        // WYSIWYG inset-border overlay: a snapshot taken while a file is dragged over the
        // window shows the drop-target highlight; one taken during the LEVEL-UP celebration
        // shows the breathing accent glow — the same inset border + wash as the glass.
        // Suppressed under a modal (mirrors the live present's `!overlay_open`).
        if ws.drag_hover && !ws.overlay_open() {
            apply_drop_overlay(&mut frame.pixels, frame.width, frame.height, accent);
        } else if !ws.overlay_open()
            && let Some((wash_a, border_a)) = level_up_glow
        {
            apply_overlay_at(
                &mut frame.pixels,
                frame.width,
                frame.height,
                0,
                0,
                frame.width,
                frame.height,
                OverlayGlow {
                    accent,
                    wash_a,
                    border_a,
                },
            );
        }
        // P3: composite the frosted Settings card so the headless capture matches glass.
        // The GPU backend already BAKED it into the offscreen (`render_input` tray param),
        // so only the CPU backend needs the post-readback composite — gate to avoid
        // double-compositing on GPU.
        if !backend.is_gpu()
            && let Some(card) = ws
                .settings_card
                .as_ref()
                .or(ws.level_up_card.as_ref())
                .or(ws.notice_card.as_ref())
                .or(ws.badge_card.as_ref())
        {
            composite_tray(&mut frame.pixels, frame.width, frame.height, card);
        }
        // text: the visible grid, row by row, from the same snapshot. Shares the
        // exact row serialization with the accessibility snapshot (push_visible_row)
        // so "what an AI sees" and "what a screen reader reads" never diverge. The
        // tab-strip chrome rows (top `tab_strip_rows`) are skipped — the .txt is the
        // terminal text only (a no-op skip when the strip is disabled).
        let mut text = String::with_capacity(rows * (cols + 1));
        // Skip the tab-strip CHROME rows so the .txt is terminal text only (a no-op
        // skip when the strip is disabled — byte-identical to the pre-strip snapshot).
        let txt_end = ws.input_scratch.cells.len().saturating_sub(hud_rows);
        for cells in ws.input_scratch.cells[strip_rows..txt_end].iter() {
            accessibility::push_visible_row(&mut text, cells, cols);
        }
        // Deflate + writes belong on the encode worker (the same 50–150 ms Retina
        // stall the `image` verb routes around); the `.done` marker is written by
        // the worker LAST, so the requester's stat() contract is unchanged.
        self.submit_encode_job(EncodeJob::Snapshot { frame, text, path });
    }

    /// Hand a capture to the single PNG encode/write worker, spawning it lazily
    /// on first use (a run that never screenshots pays nothing). EXACTLY ONE
    /// worker by design: queued `image` requests drained in one [`crate::Wake::Control`]
    /// turn must reply in FIFO queue order, and a single mpsc consumer preserves
    /// submission order. Falls back to encoding inline (the old behavior) when
    /// the thread cannot be spawned or has died, so a reply is never dropped.
    pub(crate) fn submit_encode_job(&mut self, mut job: EncodeJob) {
        // A VIDEO dump can hold the shared worker for seconds-to-minutes (hundreds of
        // full-frame PNG encodes); run it on its own ONE-SHOT lane so a subsequent
        // `image`/`window`/`snapshot` reply is never queued behind it (head-of-line).
        // At most one recording exists at a time, so this spawns rarely. Still/text
        // jobs stay on the single shared worker — their FIFO reply order is preserved.
        // A failed spawn falls through to the shared worker (the old path).
        if matches!(&job, EncodeJob::VideoDump { .. }) {
            let (vtx, vrx) = std::sync::mpsc::channel::<EncodeJob>();
            let spawned = std::thread::Builder::new()
                .name("aterm-video-encode".to_string())
                .spawn(move || {
                    if let Ok(j) = vrx.recv() {
                        run_encode_job(j);
                    }
                })
                .is_ok();
            if spawned {
                // The receiver is owned by the just-spawned thread: this send
                // cannot fail, and the thread exits after the one job.
                let _ = vtx.send(job);
                return;
            }
        }
        if let Some(tx) = &self.encode_tx {
            match tx.send(job) {
                Ok(()) => return,
                // Worker gone (its loop only ends when every sender drops, so
                // this is defensive); respawn below with the returned job.
                Err(std::sync::mpsc::SendError(j)) => {
                    self.encode_tx = None;
                    job = j;
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel::<EncodeJob>();
        let spawned = std::thread::Builder::new()
            .name("aterm-png-encode".to_string())
            .spawn(move || {
                // Drain until every sender is gone (process teardown); a dead
                // client's dropped reply receiver only makes send() fail, which
                // `run_encode_job` ignores — never a worker panic.
                while let Ok(j) = rx.recv() {
                    run_encode_job(j);
                }
            })
            .is_ok();
        if spawned {
            // The receiver is owned by the just-spawned thread, so this send
            // cannot fail; a (theoretical) failure returns the job to the drop
            // path, where the client sees the same ERR a dead render does.
            let _ = tx.send(job);
            self.encode_tx = Some(tx);
        } else {
            // No thread available — encode inline rather than drop the reply.
            run_encode_job(job);
        }
    }

    /// Capture a native tab app from the exact retained UI tree used for its
    /// glass present. Native views have no terminal to snapshot; their semantic
    /// tray is nevertheless a first-class framebuffer and must remain visible
    /// through the canonical `image` verb.
    fn render_native_image(
        &mut self,
        front: WindowId,
        target: crate::control_auth::ConfinedImage,
        want_bytes: bool,
        want_metadata: bool,
        frame_metadata: &std::sync::Arc<std::sync::OnceLock<crate::control::ImageFrameMetadata>>,
        reply: std::sync::mpsc::Sender<crate::control::ImageReply>,
    ) {
        let heterogeneous = self
            .active_visible_leaf_plan(front)
            .is_some_and(|plan| plan.leaves.len() > 1);
        let prepared = if heterogeneous {
            self.prepare_heterogeneous_input_scratch(front).is_some()
        } else {
            self.prepare_native_input_scratch(front)
        };
        if !prepared {
            let _ = reply.send(Ok((0, 0, None)));
            return;
        }
        let render_t0 = Instant::now();
        let App {
            backend,
            introspect_gpu,
            windows,
            ..
        } = self;
        let Some(ws) = windows.get_mut(&front) else {
            let _ = reply.send(Ok((0, 0, None)));
            return;
        };
        let tray_arg = ws.settings_card.as_ref().map(|card| aterm_gpu::TrayQuad {
            rgba: card.rgba.as_slice(),
            pw: card.pw,
            ph: card.ph,
            dx: card.dx,
            dy: card.dy,
        });
        let mut frame = backend.render_input(introspect_gpu, &mut ws.input_scratch, tray_arg);
        let render_ns = render_t0.elapsed().as_nanos() as u64;
        crate::metrics::record_present(0, render_ns);
        if !backend.is_gpu()
            && let Some(card) = ws.settings_card.as_ref()
        {
            composite_tray(&mut frame.pixels, frame.width, frame.height, card);
        }
        if want_metadata {
            let Ok(width) = u32::try_from(frame.width) else {
                let _ = reply.send(Err("native image metadata width overflow".to_string()));
                return;
            };
            let Ok(height) = u32::try_from(frame.height) else {
                let _ = reply.send(Err("native image metadata height overflow".to_string()));
                return;
            };
            match self.native_image_metadata(
                front,
                "staged",
                width,
                height,
                Self::image_pixel_fingerprint(frame.width, frame.height, &frame.pixels),
            ) {
                Ok(metadata) => {
                    let _ = frame_metadata.set(metadata);
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            }
        }
        self.submit_encode_job(EncodeJob::Image {
            frame,
            target,
            want_bytes,
            reply,
        });
        let now = Instant::now();
        for panel in &mut self.panels {
            panel.on_present(render_ns, 0, now);
        }
    }

    fn native_image_metadata(
        &self,
        front: WindowId,
        phase: &'static str,
        width: u32,
        height: u32,
        pixel_fingerprint: u64,
    ) -> Result<crate::control::ImageFrameMetadata, String> {
        use std::hash::{Hash, Hasher};

        let fingerprint = |width: u32, height: u32, rgba: &[u8]| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            width.hash(&mut hash);
            height.hash(&mut hash);
            rgba.hash(&mut hash);
            hash.finish() | 1
        };
        let plan = self
            .active_visible_leaf_plan(front)
            .ok_or_else(|| "native image metadata has no visible leaf plan".to_string())?;
        let window = self
            .windows
            .get(&front)
            .ok_or_else(|| "native image metadata lost its window".to_string())?;
        let card = window
            .settings_card
            .as_ref()
            .ok_or_else(|| "native image metadata has no retained composite raster".to_string())?;
        let (cw, ch) = self.win_cell_size(front);
        let mut leaves = Vec::with_capacity(plan.leaves.len());
        for leaf in &plan.leaves {
            let retained = window.leaf_render_cache.get(&leaf.view).ok_or_else(|| {
                format!(
                    "native image metadata is missing retained view {}",
                    leaf.view
                )
            })?;
            match self.view_store.get(leaf.view).copied() {
                Some(crate::tab_model::View::Terminal(terminal)) => {
                    leaves.push(crate::control::ImageLeafFrameMetadata {
                        kind: "terminal",
                        view: leaf.view.get(),
                        session: Some(terminal.session),
                        focused: leaf.focused,
                        width: (leaf.rect.size.width * cw as f32).round().max(1.0) as u32,
                        height: (leaf.rect.size.height * ch as f32).round().max(1.0) as u32,
                        snapshot_seq: Some(retained.input.snapshot_seq),
                        instance: None,
                        generation: None,
                        geometry: None,
                        config_revision: None,
                        update_revision: None,
                        document_seq: None,
                        presentation_revision: None,
                        paint_revision: None,
                        compiled_fingerprint: None,
                        raster_fingerprint: None,
                    });
                }
                Some(crate::tab_model::View::Native(_)) => {
                    let raster = retained.native.as_ref().ok_or_else(|| {
                        format!(
                            "native image metadata is missing retained native raster {}",
                            leaf.view
                        )
                    })?;
                    let stamp = raster.stamp;
                    leaves.push(crate::control::ImageLeafFrameMetadata {
                        kind: "native",
                        view: leaf.view.get(),
                        session: None,
                        focused: leaf.focused,
                        width: raster.width,
                        height: raster.height,
                        snapshot_seq: None,
                        instance: Some(stamp.instance.get()),
                        generation: Some(stamp.generation),
                        geometry: Some(stamp.geometry),
                        config_revision: Some(stamp.config_revision),
                        update_revision: Some(stamp.update_revision),
                        document_seq: stamp.document_seq,
                        presentation_revision: Some(stamp.presentation_revision),
                        paint_revision: Some(stamp.paint_revision),
                        compiled_fingerprint: Some(raster.compiled.fingerprint()),
                        raster_fingerprint: Some(fingerprint(
                            raster.width,
                            raster.height,
                            &raster.rgba,
                        )),
                    });
                }
                None => {
                    return Err(format!(
                        "native image metadata found stale view {}",
                        leaf.view
                    ));
                }
            }
        }
        if !leaves.iter().any(|leaf| leaf.kind == "native") {
            return Err("native image metadata composite contains no native leaf".to_string());
        }

        let frame_kind = if leaves.len() == 1 && leaves[0].kind == "native" {
            "native"
        } else {
            "composite"
        };
        let primary = (frame_kind == "native").then(|| &leaves[0]);
        Ok(crate::control::ImageFrameMetadata {
            frame_kind,
            phase,
            window: front.0,
            view: primary.map(|leaf| leaf.view),
            generation: primary.and_then(|leaf| leaf.generation),
            config_revision: primary.and_then(|leaf| leaf.config_revision),
            update_revision: primary.and_then(|leaf| leaf.update_revision),
            document_seq: primary.and_then(|leaf| leaf.document_seq),
            presentation_revision: primary.and_then(|leaf| leaf.presentation_revision),
            paint_revision: primary.and_then(|leaf| leaf.paint_revision),
            capture_serial: window.capture_present_serial,
            width,
            height,
            pixel_fingerprint,
            compiled_fingerprint: primary.and_then(|leaf| leaf.compiled_fingerprint),
            raster_fingerprint: fingerprint(card.pw, card.ph, &card.rgba),
            raster_model_fingerprint: card.fp,
            raster_geometry: card.geom,
            overlay_fingerprint: window.overlay_fp(),
            theme_fingerprint: self.image_theme_fingerprint(),
            leaves,
        })
    }

    fn image_theme_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut theme = std::collections::hash_map::DefaultHasher::new();
        self.theme.fg.hash(&mut theme);
        self.theme.bg.hash(&mut theme);
        self.theme.cursor.hash(&mut theme);
        self.theme.selection.hash(&mut theme);
        theme.finish() | 1
    }

    /// Identity of the engine-owned terminal snapshot that fed one capture.
    /// `snapshot_seq` is the terminal's monotone damage epoch, captured under
    /// the same lock that extracted every cell; session + view make that epoch
    /// unambiguous across panes, while viewport coordinates bind the retained
    /// model to the exact visible slice that was rasterized.
    fn terminal_snapshot_fingerprint(
        view: u64,
        session: u64,
        input: &aterm_render::RenderInput,
    ) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut snapshot = std::collections::hash_map::DefaultHasher::new();
        view.hash(&mut snapshot);
        session.hash(&mut snapshot);
        input.snapshot_seq.hash(&mut snapshot);
        input.rows.hash(&mut snapshot);
        input.cols.hash(&mut snapshot);
        input.display_offset.hash(&mut snapshot);
        input.base_y.hash(&mut snapshot);
        input.cursor_row.hash(&mut snapshot);
        input.cursor_col.hash(&mut snapshot);
        input.cursor_visible.hash(&mut snapshot);
        input.default_bg.hash(&mut snapshot);
        input.cursor_color.hash(&mut snapshot);
        snapshot.finish() | 1
    }

    fn terminal_geometry_fingerprint(
        width: usize,
        height: usize,
        input: &aterm_render::RenderInput,
    ) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut geometry = std::collections::hash_map::DefaultHasher::new();
        width.hash(&mut geometry);
        height.hash(&mut geometry);
        input.rows.hash(&mut geometry);
        input.cols.hash(&mut geometry);
        input.grid_top_row.hash(&mut geometry);
        input.grid_bot_row.hash(&mut geometry);
        geometry.finish() | 1
    }

    fn image_pixel_fingerprint<T: std::hash::Hash>(
        width: usize,
        height: usize,
        pixels: &[T],
    ) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hash = std::collections::hash_map::DefaultHasher::new();
        width.hash(&mut hash);
        height.hash(&mut hash);
        pixels.hash(&mut hash);
        hash.finish() | 1
    }

    /// Render the CURRENT terminal for the control socket's `image` verb (the
    /// same renderer the window uses, GPU path if active). Runs on the main
    /// thread per [`crate::Wake::Control`] — but ONLY the render + WYSIWYG
    /// composites: the PNG encode + confined write are handed to the encode
    /// worker, which sends the client's `(width, height)` reply after the write
    /// (so an OK on the wire still means the file is complete).
    pub(crate) fn render_image(&mut self, req: ImageReq) {
        let ImageReq {
            target,
            clean,
            session,
            want_bytes,
            want_metadata,
            frame_metadata,
            reply,
        } = req;
        // Cross-session (`@<sid> image`): render the window whose ACTIVE tab
        // displays the target session — the frame that session's viewer actually
        // sees (splits, decorations, tab strip included). Self keeps the frontmost
        // window byte-for-byte. No displaying window (background tab / no windows)
        // replies (0,0), which the control verb reports as an honest ERR.
        let win = match session {
            Some(id) => self.windows_displaying(id).next(),
            None => self.frontmost_window,
        };
        let Some(front) = win else {
            let _ = reply.send(Ok((0, 0, None)));
            return;
        };
        if self.active_tab_has_native(front) {
            // A windowed native tab uses platform-owned tab/title chrome. The
            // internal app framebuffer cannot contain those pixels, so route
            // the canonical image capture through the real window compositor
            // where available. A preceding `open`/`act` reply guarantees the
            // MODEL mutation, but winit may not have delivered its requested
            // redraw yet; present synchronously before asking the window server
            // for pixels so command order is also FRAME order. Headless native
            // apps keep the deterministic retained-frame path below (and have
            // no OS chrome to omit).
            #[cfg(any(target_os = "macos", windows))]
            if self
                .windows
                .get(&front)
                .is_some_and(|window| window.os_window.is_some())
            {
                if let Err(error) = self.present_before_window_capture(front) {
                    let _ = reply.send(Err(error));
                    return;
                }
                self.capture_native_window_image(
                    front,
                    target,
                    want_bytes,
                    want_metadata,
                    &frame_metadata,
                    reply,
                );
                return;
            }
            self.render_native_image(
                front,
                target,
                want_bytes,
                want_metadata,
                &frame_metadata,
                reply,
            );
            return;
        }
        let (rows, cols) = match self.windows.get(&front) {
            Some(ws) => (ws.rows as usize, ws.cols as usize),
            None => {
                let _ = reply.send(Ok((0, 0, None)));
                return;
            }
        };
        let Some(front_terminal) = self.front_terminal_mirror(front) else {
            let _ = reply.send(Ok((0, 0, None)));
            return;
        };
        let terminal_identity =
            self.windows
                .get(&front)
                .and_then(|window| match window.front_content {
                    Some(crate::front_content::FrontContent::Terminal { view, session })
                        if session == front_terminal.session =>
                    {
                        Some((view, session))
                    }
                    _ => None,
                });
        // Theme cursor fallback, read before the &mut self.windows borrow below.
        let theme_cursor = self.theme.cursor & 0x00FF_FFFF;
        // Lock only to snapshot the grid; render without the lock.
        {
            let Some(ws) = self.windows.get_mut(&front) else {
                let _ = reply.send(Ok((0, 0, None)));
                return;
            };
            let mut term = term_lock(&front_terminal.term);
            // REFILL the reused snapshot in place (no per-frame container-Vec alloc).
            // A-3: the ENGINE builds the snapshot (`Terminal::cell_frame_into`).
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
            // WYSIWYG: the `image`/`snapshot` introspection must match the GLASS, so
            // populate the live OSC 11/111/DECSCNM default-bg and OSC 12/112 cursor
            // colour exactly as the windowed `redraw_window` does — otherwise a program
            // that recoloured the background/cursor would render here in the static
            // theme while the real window shows the live colour.
            let dbg = if term.modes().reverse_video() {
                term.default_foreground()
            } else {
                term.default_background()
            };
            ws.input_scratch.default_bg = aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]);
            ws.input_scratch.cursor_color = term
                .cursor_color()
                .map_or(theme_cursor, |c| aterm_render::rgb_to_u32([c.r, c.g, c.b]));
        }
        // PHASE 1 of "Headless Present-Real": a GLASS-LESS window never presents, so
        // its cursor-effect engines only ever advance HERE — tick them at capture
        // time and splice the live quads exactly as a present would, so the image
        // shows the fire at its true heat/decay. Headless-gated inside: a WINDOWED
        // capture keeps its last-present quads (never newer than the glass).
        self.splice_cursor_fx(front, Instant::now());
        // Sparkle-word decorations (orca splash etc.), so `image` matches the glass.
        self.splice_word_decorations(front, Instant::now());
        // WYSIWYG: splice the tab strip above the terminal grid so the `image` verb
        // matches the glass (a no-op when the strip is disabled). Before the borrow.
        self.splice_tab_strip(front);
        // Sample the interval-driven panels here too: headless has no window tick,
        // so this is what makes CPU/net/app-fed live in `image`/`snapshot` output.
        // The single metrics sample, so headless `image`/`snapshot` (and the `widgets`
        // verb read right after) reflect current figures even with no window tick.
        // SKIPPED when the windowed HUD tick is driving (an on-glass front window
        // with its HUD tick ARMED — main.rs samples once per HUD_INTERVAL): the
        // sample is a process-tree + sysctl scan on the event-loop thread, and a
        // visually-polling agent (1-5 Hz `image`) would charge it to every capture
        // while the human types. The capture then shows the tick's figures — the
        // same numbers the on-glass HUD shows (WYSIWYG). Headless, and a windowed
        // front with the tick DISARMED (HUD off / unfocused), still sample here so
        // `image`/`widgets` stay live.
        let hud_tick_driving = self
            .windows
            .get(&front)
            .is_some_and(|ws| ws.os_window.is_some() && ws.next_hud_tick.is_some());
        if !hud_tick_driving {
            let hud_now = Instant::now();
            let tab_pid = self.frontmost_tab_pid();
            self.metrics_service.sample(tab_pid, None, hud_now);
            for p in &mut self.panels {
                p.poll(hud_now);
            }
        }
        // WYSIWYG: paint the Cmd-F find bar over the bottom terminal row (a no-op when not
        // searching), exactly as the live present does, so a headless capture matches glass.
        self.splice_find_bar(front);
        self.splice_hud_bar(front);
        // OVERLAY the modal Settings panel (a no-op when closed), exactly as the live
        // present does — so the SACRED `image`/`snapshot` introspection captures the open
        // settings surface WYSIWYG (what an AI reads == what is on glass). Completes
        // introspection of the settings overlay for headless verification.
        self.splice_settings_panel(front);
        // Subtle top-right build/version badge — same paint-only slot the live present
        // uses, so the headless capture shows it WYSIWYG (composite prefers the modal).
        self.splice_build_badge(front);
        self.splice_notice(front);
        // The LEVEL-UP arrow burst — same paint-only slot the live present uses (priority
        // over the pill), so a headless capture during the celebration is WYSIWYG.
        self.splice_level_up(front);
        // OVERLAY the transient config-notice banner LAST (topmost), exactly as the live
        // present does (app_render splices it after the settings panel) — so the SACRED
        // `image`/`snapshot` capture shows a restart-only / dropped-rule notice WYSIWYG
        // (what an AI reads == what is on glass). A no-op when no banner is up.
        self.splice_config_notice(front);
        // Accent for the drop-target highlight / level-up glow, read before the disjoint
        // borrow. The level-up glow's breathing alphas are sampled here too (matching the
        // on-glass present) so a capture during the celebration shows the pulsing frame.
        let accent = self.theme.cursor;
        let level_up_glow = self
            .level_up
            .as_ref()
            .map(|l| (l.wash_alpha(Instant::now()), l.border_alpha(Instant::now())));
        let theme_fingerprint = self.image_theme_fingerprint();
        // Disjoint borrows: `self.backend` (renderer), the introspection GPU
        // scratch, and the front window's input_scratch are separate fields.
        let App {
            backend,
            introspect_gpu,
            windows,
            ..
        } = self;
        let Some(ws) = windows.get_mut(&front) else {
            let _ = reply.send(Ok((0, 0, None)));
            return;
        };
        // CLEAN capture (`image plain`): drop every host-owned bling LAYER so the AI reads
        // the bare terminal — cursor trail + LUMEN glow + sparkle-word decorations + the
        // animated Scene. They live in separate `RenderInput` fields, so this is just those
        // layers emptied; the cell grid (text) is untouched.
        if clean {
            ws.input_scratch.clear_overlays();
        }
        // Time the rasterization so the `metrics` verb reports a real
        // `last_frame_render_ms` in HEADLESS mode too. On-screen frames are timed in
        // `redraw_window`; without this, headless (no OS surface → no
        // RedrawRequested → no `record_present`) leaves every counter frozen at 0,
        // so a perf audit driven over the control socket could measure nothing.
        // Present latency is recorded as 0 — honest: the `image` verb rasterizes to
        // a buffer, it does not present on glass.
        let render_t0 = Instant::now();
        // P3 settings card → raw bytes + device-px rect for the GPU tray quad (same
        // builder the live present uses). The GPU arm BAKES it into the offscreen so the
        // readback is WYSIWYG; the CPU arm ignores it (composited below, gated on !is_gpu).
        // Modal card FIRST, else the transient update notice, else the build/version badge.
        let tray_arg = ws
            .settings_card
            .as_ref()
            .or(ws.level_up_card.as_ref())
            .or(ws.notice_card.as_ref())
            .or(ws.badge_card.as_ref())
            .map(|c| aterm_gpu::TrayQuad {
                rgba: c.rgba.as_slice(),
                pw: c.pw,
                ph: c.ph,
                dx: c.dx,
                dy: c.dy,
            });
        let mut frame = backend.render_input(introspect_gpu, &mut ws.input_scratch, tray_arg);
        let render_ns = render_t0.elapsed().as_nanos() as u64;
        crate::metrics::record_present(0, render_ns);
        // I-2: match the on-screen visual-bell invert (see `snapshot`) so the
        // `image` verb is WYSIWYG even during a bell flash. Suppressed while ANY modal
        // overlay is open — the SAME `overlay_open()` gate the glass present and the
        // snapshot path consult, so all three can never disagree (SACRED WYSIWYG).
        apply_bell_invert(
            &mut frame,
            ws.bell_flash.is_active(Instant::now()) && !ws.overlay_open(),
        );
        // WYSIWYG inset-border overlay (matches the on-glass present + snapshot): the
        // drop-target highlight while dragging, else the LEVEL-UP celebration's glow —
        // both suppressed under a modal overlay, mirroring the glass `overlay_open` gate.
        if ws.drag_hover && !ws.overlay_open() {
            apply_drop_overlay(&mut frame.pixels, frame.width, frame.height, accent);
        } else if !ws.overlay_open()
            && let Some((wash_a, border_a)) = level_up_glow
        {
            apply_overlay_at(
                &mut frame.pixels,
                frame.width,
                frame.height,
                0,
                0,
                frame.width,
                frame.height,
                OverlayGlow {
                    accent,
                    wash_a,
                    border_a,
                },
            );
        }
        // P3: composite the frosted Settings card so the headless capture matches glass.
        // The GPU backend already BAKED it into the offscreen (`render_input` tray param),
        // so only the CPU backend needs the post-readback composite — gate to avoid
        // double-compositing on GPU.
        if !backend.is_gpu()
            && let Some(card) = ws
                .settings_card
                .as_ref()
                .or(ws.level_up_card.as_ref())
                .or(ws.notice_card.as_ref())
                .or(ws.badge_card.as_ref())
        {
            composite_tray(&mut frame.pixels, frame.width, frame.height, card);
        }
        if want_metadata {
            let Some((view, session)) = terminal_identity else {
                let _ = reply.send(Err(
                    "terminal image metadata lost focused terminal provenance".to_string(),
                ));
                return;
            };
            let Ok(width) = u32::try_from(frame.width) else {
                let _ = reply.send(Err(
                    "terminal image metadata width exceeds the wire format".to_string()
                ));
                return;
            };
            let Ok(height) = u32::try_from(frame.height) else {
                let _ = reply.send(Err(
                    "terminal image metadata height exceeds the wire format".to_string(),
                ));
                return;
            };
            let pixel_fingerprint =
                Self::image_pixel_fingerprint(frame.width, frame.height, &frame.pixels);
            let snapshot_fingerprint =
                Self::terminal_snapshot_fingerprint(view.get(), session, &ws.input_scratch);
            let geometry =
                Self::terminal_geometry_fingerprint(frame.width, frame.height, &ws.input_scratch);
            let metadata = crate::control::ImageFrameMetadata {
                frame_kind: "terminal",
                phase: "rendered",
                window: front.0,
                view: Some(view.get()),
                generation: None,
                config_revision: None,
                update_revision: None,
                document_seq: None,
                presentation_revision: None,
                paint_revision: None,
                capture_serial: ws.capture_present_serial,
                width,
                height,
                pixel_fingerprint,
                compiled_fingerprint: None,
                raster_fingerprint: pixel_fingerprint,
                raster_model_fingerprint: snapshot_fingerprint,
                raster_geometry: geometry,
                overlay_fingerprint: ws.overlay_fp(),
                theme_fingerprint,
                leaves: vec![crate::control::ImageLeafFrameMetadata {
                    kind: "terminal",
                    view: view.get(),
                    session: Some(session),
                    focused: true,
                    width,
                    height,
                    snapshot_seq: Some(ws.input_scratch.snapshot_seq),
                    instance: None,
                    generation: None,
                    geometry: Some(geometry),
                    config_revision: None,
                    update_revision: None,
                    document_seq: None,
                    presentation_revision: None,
                    paint_revision: None,
                    compiled_fingerprint: None,
                    raster_fingerprint: Some(pixel_fingerprint),
                }],
            };
            if frame_metadata.set(metadata).is_err() {
                let _ = reply.send(Err(
                    "terminal image metadata was already initialized".to_string()
                ));
                return;
            }
        }
        // `confine_image_path` (control thread) produced `target` as a canonical
        // `images/` dir + a SINGLE filename, forbidding nested target dirs. The
        // worker writes by opening THAT directory `O_DIRECTORY|O_NOFOLLOW` and
        // `openat`-ing the final component `O_NOFOLLOW|O_CREAT|O_TRUNC` — so the
        // only guarantee we rely on is: the write lands in the canonical images
        // dir and never follows a symlink at the directory OR the final name.
        // (We do NOT claim atomicity vs. a same-uid client deleting+recreating
        // the directory between threads; we DO close the intermediate-dir
        // symlink-swap window by never re-resolving a multi-segment path string.)
        //
        // The PNG deflate (50–150 ms on a Retina-sized frame) + write run on the
        // encode worker, NOT this event-loop thread. Every WYSIWYG splice/overlay
        // above is already baked into the moved `frame`, and the worker replies
        // only after the write — the client reads the file the moment it sees OK.
        self.submit_encode_job(EncodeJob::Image {
            frame,
            target,
            want_bytes,
            reply,
        });
        // Feed the frame-coupled panels (Perf) AFTER the disjoint-field borrows above
        // end (the destructure held `windows`/`backend`; `self.panels` is separate).
        let hud_now = Instant::now();
        for p in &mut self.panels {
            p.on_present(render_ns, 0, hud_now);
        }
    }

    /// Read the frontmost window's NATIVE macOS chrome — the window's `NSToolbar`
    /// items and the application menu bar — into human-readable text lines for the
    /// `chrome` introspection verb. Runs on the MAIN thread (the SOLE place AppKit
    /// objects may be touched), driven by [`Wake::ReadChrome`]; the control thread
    /// posts that and blocks on the reply.
    ///
    /// This is the ONLY introspection path that sees OS chrome: `image`/`text`
    /// render just the terminal content view, never the toolbar or menu bar, so a
    /// driving AI uses `chrome` to confirm e.g. the "+" New Tab toolbar button and
    /// the menu structure. Pure read: it only CALLS getters (`toolbar()`/`items()`/
    /// `itemIdentifier()`/`label()`, `mainMenu()`/`itemArray()`/`title()`/
    /// `submenu()`), never mutating AppKit state.
    ///
    /// Off macOS there is no native chrome, so it returns a single explanatory line.
    #[cfg(target_os = "macos")]
    pub(crate) fn read_native_chrome(&self) -> Vec<String> {
        use objc2_app_kit::{NSApplication, NSToolbarDisplayMode, NSView, NSWindowToolbarStyle};
        use objc2_foundation::MainThreadMarker;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let mut out: Vec<String> = Vec::new();

        // We are on the winit main-loop thread (this runs via `user_event`), so the
        // marker is always present; bail gracefully if somehow not.
        let Some(mtm) = MainThreadMarker::new() else {
            out.push("ERR not on main thread".to_string());
            return out;
        };

        // --- The frontmost window's NSToolbar ---------------------------------
        // Reach the NSWindow the SAME way `match_window_colorspace_to_content` /
        // `toolbar::install_window_toolbar` do: winit Window -> AppKit
        // RawWindowHandle -> NSView -> NSWindow.
        let ns_window = self
            .front()
            .and_then(|ws| ws.os_window.as_ref())
            .and_then(|w| w.window_handle().ok())
            .and_then(|handle| match handle.as_raw() {
                // SAFETY: `ns_view` points at the front window's live NSView (owned
                // by winit for the window's lifetime); we only borrow it on the main
                // thread, as AppKit requires, to read its `window`.
                RawWindowHandle::AppKit(h) => {
                    let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
                    view.window()
                }
                _ => None,
            });

        // SAFETY: all the AppKit getters below (`toolbar`/`toolbarStyle`/
        // `displayMode`/`items`/`itemIdentifier`/`label`) are plain side-effect-free
        // accessors with no preconditions beyond a live receiver, called here on the
        // MAIN thread (this method runs only via `Wake::ReadChrome` in `user_event`).
        unsafe {
            match ns_window.as_deref().and_then(|w| w.toolbar()) {
                Some(toolbar) => {
                    let style = match ns_window.as_deref().map(|w| w.toolbarStyle()) {
                        Some(NSWindowToolbarStyle::Automatic) => "automatic",
                        Some(NSWindowToolbarStyle::Expanded) => "expanded",
                        Some(NSWindowToolbarStyle::Preference) => "preference",
                        Some(NSWindowToolbarStyle::Unified) => "unified",
                        Some(NSWindowToolbarStyle::UnifiedCompact) => "unified-compact",
                        _ => "?",
                    };
                    let display_mode = match toolbar.displayMode() {
                        NSToolbarDisplayMode::IconOnly => "icon-only",
                        NSToolbarDisplayMode::LabelOnly => "label-only",
                        NSToolbarDisplayMode::IconAndLabel => "icon-and-label",
                        _ => "default",
                    };
                    let items = toolbar.items();
                    out.push(format!(
                        "toolbar style={style} displayMode={display_mode} items={}",
                        items.len()
                    ));
                    for item in &items {
                        let id = item.itemIdentifier();
                        let label = item.label();
                        out.push(format!("toolbar-item id={id} label={label:?}"));
                    }
                }
                None => out.push("toolbar (none)".to_string()),
            }
        }

        // Native title chrome: including a one-tab window, emit canonical titles,
        // selection, independent states, and tooltips. This is app/session identity,
        // not merely a multi-tab switcher. Read off the retained handle (we own the
        // control), not
        // via a toolbar-item view downcast (objc2 0.5 has no `Retained::downcast`).
        if let Some(handle) = self.frontmost_window.and_then(|w| self._toolbars.get(&w)) {
            if let Some(line) = self.apprt.read_toolbar_chrome(handle) {
                out.push(line);
            }
            // The per-tab CONTEXT MENUS (session-metadata stage 2): one
            // `tab-menu tab=<i> items=[...]` line per tab chip, read off the
            // SAME stored models a right-click pops — so a driving AI sees
            // exactly the items (and greyed states) a human would. Empty at ≤1
            // tab, like the switcher line above.
            out.extend(self.apprt.read_toolbar_tab_menus(handle));
        }

        // --- The application menu bar (NSApplication.mainMenu) ----------------
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: `mainMenu`/`itemArray`/`title`/`submenu` are side-effect-free
        // getters with no preconditions beyond a live receiver, on the main thread.
        unsafe {
            match app.mainMenu() {
                Some(main) => {
                    for top in &main.itemArray() {
                        let title = top.title();
                        match top.submenu() {
                            Some(sub) => {
                                let names: Vec<String> = sub
                                    .itemArray()
                                    .iter()
                                    // Skip separators (empty title) so the listing
                                    // reads as the command set, not the dividers.
                                    .filter(|i| !i.title().is_empty())
                                    .map(|i| i.title().to_string())
                                    .collect();
                                out.push(format!("menu {title:?}: {}", names.join(", ")));
                            }
                            // A top-level item with no submenu (uncommon for a bar).
                            None => out.push(format!("menu {title:?}: (no submenu)")),
                        }
                    }
                }
                None => out.push("menu (none)".to_string()),
            }
        }

        out
    }

    /// Off macOS there is no native window TOOLBAR (no `NSToolbar`), but the application
    /// MENU is still introspectable: it is serialised from the platform-neutral
    /// [`crate::menu::MENU_MODEL`] so the `chrome` verb reports the SAME logical menu
    /// (`menu "File": …` lines) the macOS bar shows — what an AI/automation reads matches
    /// across platforms. Kept as a method on every target so the [`crate::Wake::ReadChrome`]
    /// handler is platform-independent.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn read_native_chrome(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        // Windows: report the REAL native chrome the `AppRtWindows` backend applied,
        // read back from the live HWND via DwmGetWindowAttribute (dark-mode / corner
        // preference / system backdrop) — the W0 acceptance surface, ground truth not
        // intent. Off Windows there is no native window chrome to read.
        #[cfg(windows)]
        {
            let window = self.front().and_then(|ws| ws.os_window.as_deref());
            out.extend(crate::platform_win::read_chrome_lines(
                window,
                self.window_theme,
                self.render_knobs.background_material,
            ));
        }
        // Off macOS the host keeps the title/tab model in memory even where no native
        // header-bar widget exists yet. Read it through the same AppRt seam as macOS so
        // Settings identity, route tooltip, selection, state, and the terminal-no-icon
        // policy remain visible to introspection on every host.
        let toolbar_tabs = self
            .frontmost_window
            .and_then(|wid| self._toolbars.get(&wid))
            .and_then(|handle| self.apprt.read_toolbar_chrome(handle));
        // No native window TOOLBAR off macOS (the tab strip is in-grid); say so, then
        // serialise the platform-neutral menu model so `chrome` reports the same menu.
        // The per-tab CONTEXT-MENU mirror (session-metadata stage 2) IS surfaced
        // off macOS: the in-memory toolbar model records the same composed
        // per-tab menus the macOS strip pops, and the shared serialiser emits
        // the same `tab-menu tab=<i> items=[...]` line shape — so what an
        // AI/automation reads matches across platforms (empty at ≤1 tab).
        let tab_menus = self
            .frontmost_window
            .and_then(|w| self._toolbars.get(&w))
            .map_or_else(Vec::new, |handle| self.apprt.read_toolbar_tab_menus(handle));
        non_macos_chrome_output(
            out,
            toolbar_tabs,
            tab_menus,
            crate::menu::menu_chrome_lines(),
        )
    }

    /// Capture the frontmost window's ENTIRE on-screen pixels — the native OS
    /// chrome (titlebar, traffic lights, the unified toolbar, the full-width tab
    /// strip) AND the native tab content — to a PNG at the CONFINED `target`,
    /// replying the captured `(width, height)`. This is the windowed-native arm
    /// of the control socket's `image` verb; `window front` reaches the same
    /// photograph through [`Self::capture_window_of`]. Both run on the main
    /// thread because AppKit and the compositor window number are main-thread
    /// state.
    ///
    /// Terminal-tab `image` requests can rasterize their deterministic renderer
    /// framebuffer directly. A native tab cannot: its title/tab chrome is owned
    /// by the platform. This arm therefore resolves the front `NSWindow`'s
    /// `windowNumber()` (a CGWindowID) and asks CoreGraphics for the actual
    /// composited on-screen pixels—the whole frame the user sees.
    ///
    /// Replies `Err(msg)` (never panics) when there is no front OS window (headless),
    /// or the CoreGraphics capture fails — most commonly because macOS Screen
    /// Recording permission has not been granted (the verb surfaces that as a clear,
    /// actionable error so the user can grant it and retry). Only the AppKit +
    /// CoreGraphics photograph runs here; the PNG encode + confined write run on
    /// the encode worker, which sends `reply` after the write (an OK on the wire
    /// still means the file is complete).
    #[cfg(target_os = "macos")]
    fn capture_native_window_image(
        &mut self,
        wid: WindowId,
        target: control_auth::ConfinedImage,
        want_bytes: bool,
        want_metadata: bool,
        frame_metadata: &std::sync::Arc<std::sync::OnceLock<crate::control::ImageFrameMetadata>>,
        reply: std::sync::mpsc::Sender<crate::control::ImageReply>,
    ) {
        match self.current_window_rgba_of(wid) {
            Ok((rgba, width, height)) => {
                if want_metadata {
                    match self.native_image_metadata(
                        wid,
                        "presented",
                        width,
                        height,
                        Self::image_pixel_fingerprint(width as usize, height as usize, &rgba),
                    ) {
                        Ok(metadata) => {
                            let _ = frame_metadata.set(metadata);
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            return;
                        }
                    }
                }
                self.submit_encode_job(EncodeJob::ImageRgba {
                    rgba,
                    width,
                    height,
                    target,
                    want_bytes,
                    reply,
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn capture_window(
        &mut self,
        target: control_auth::ConfinedImage,
        reply: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    ) {
        self.capture_window_of(self.frontmost_window, target, reply);
    }

    /// The body of [`Self::capture_window`], parameterized on which logical window to
    /// photograph. A capture failure replies
    /// `Err` immediately; captured pixels are handed to the encode worker.
    #[cfg(target_os = "macos")]
    fn capture_window_of(
        &mut self,
        wid: Option<WindowId>,
        target: control_auth::ConfinedImage,
        reply: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    ) {
        if let Some(wid) = wid
            && let Err(error) = self.present_before_window_capture(wid)
        {
            let _ = reply.send(Err(error));
            return;
        }
        let captured = match wid {
            Some(wid) if self.active_tab_has_native(wid) => self.current_window_rgba_of(wid),
            _ => self.window_rgba_of(wid),
        };
        match captured {
            Ok((rgba, width, height)) => self.submit_encode_job(EncodeJob::WindowRgba {
                rgba,
                width,
                height,
                target,
                reply,
            }),
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        }
    }

    /// Make screenshot ordering match control ordering.
    ///
    /// GUI mutations (`open`, `act`, tab selection, Settings edits) run on this
    /// same main loop and reply after changing the model, while their
    /// `request_redraw` is asynchronous. Without this barrier, an immediately
    /// following `image`/`window` can photograph the prior composited frame even
    /// though `inspect` already reports the new semantic tree. A present can also
    /// be dropped while acquiring the GPU surface, so one blind redraw is not a
    /// sufficient barrier: authorize capture only when the window's real-present
    /// serial advances after redrawing the CURRENT composite. The serial belongs to
    /// the whole window rather than one focused native leaf, so this remains correct
    /// for heterogeneous terminal/native splits and native leaves with sub-viewports.
    /// Retry a bounded three times, then fail closed instead of returning pixels from
    /// the wrong route or setting generation.
    #[cfg(any(target_os = "macos", windows))]
    fn present_before_window_capture(&mut self, wid: WindowId) -> Result<(), String> {
        let has_os_window = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.os_window.is_some());
        if !has_os_window {
            // Preserve the established, more specific headless/no-window error from
            // the platform capture below.
            return Ok(());
        }

        if !self.active_tab_has_native(wid) {
            self.redraw_window(wid);
            return Ok(());
        }

        let presented =
            crate::run_capture_present_barrier(crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, || {
                let before = self
                    .windows
                    .get(&wid)
                    .map_or(0, |window| window.capture_present_serial);
                self.redraw_window(wid);
                self.windows
                    .get(&wid)
                    .is_some_and(|window| window.capture_present_serial != before)
            });
        if presented {
            Ok(())
        } else {
            Err(format!(
                "window capture could not synchronize the requested native frame after {} present attempts",
                crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT,
            ))
        }
    }

    /// The MAIN-THREAD half of a window capture: resolve the logical window's
    /// `NSWindow` → `windowNumber()` (AppKit objects are main-thread-only) and
    /// photograph its composited on-screen pixels, returning tightly-packed
    /// RGBA8 + dims. The encode/write half lives on the encode worker.
    #[cfg(target_os = "macos")]
    fn window_rgba_of(&self, wid: Option<WindowId>) -> Result<(Vec<u8>, u32, u32), String> {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        // Reach the window's NSView the SAME way `read_native_chrome` /
        // `match_window_colorspace_to_content` / `toolbar::install_window_toolbar`
        // do: winit Window -> AppKit RawWindowHandle -> NSView. `None` here means
        // there is no attached OS surface — i.e. headless — so the capture has no
        // window to photograph.
        let Some(os_window) = wid
            .and_then(|id| self.windows.get(&id))
            .and_then(|ws| ws.os_window.as_ref())
        else {
            return Err("no window to capture (headless)".to_string());
        };
        let Ok(handle) = os_window.window_handle() else {
            return Err("no window to capture (headless)".to_string());
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return Err("no window to capture (headless)".to_string());
        };
        // SAFETY: `ns_view` points at the front window's live NSView (owned by winit
        // for the window's lifetime); we only borrow it on the main thread, as AppKit
        // requires, to read its `window` and the window's `windowNumber`.
        let view: &objc2_app_kit::NSView =
            unsafe { &*(h.ns_view.as_ptr() as *const objc2_app_kit::NSView) };
        let Some(ns_window) = view.window() else {
            return Err("no window to capture (headless)".to_string());
        };
        // `windowNumber()` is the CGWindowID the window server knows this NSWindow
        // by — the handle `CGWindowListCreateImage` keys off. A negative / zero number
        // means the window is off-screen / not yet committed; treat as uncapturable.
        // SAFETY: a side-effect-free accessor on the live front-window `NSWindow`,
        // called on the main thread (this runs only via `Wake::CaptureWindow`).
        let window_number = unsafe { ns_window.windowNumber() };
        if window_number <= 0 {
            return Err(
                "window capture failed (front window has no on-screen window number)".to_string(),
            );
        }

        // Off to CoreGraphics: photograph the composited on-screen pixels. Any
        // failure (most commonly a missing Screen Recording grant) returns a clear
        // `Err`. The PNG encode + confined write (identical to `render_image`'s:
        // `openat` the final component under the canonical `images/` dir fd,
        // `O_NOFOLLOW`) happen on the encode worker.
        capture_window_pixels(window_number as u32)
    }

    /// Capture the platform-owned window frame, then replace its native-app client
    /// pixels with the exact current semantic-renderer framebuffer.
    ///
    /// A successful Metal `present()` means the drawable was accepted, not that the
    /// out-of-process WindowServer has already promoted it. CoreGraphics can therefore
    /// photograph the previous client frame for one compositor interval immediately
    /// after an `open`/`act`, even though the titlebar and semantic inspection are
    /// current. The native content is already aterm-owned retained renderer output, so
    /// use that authoritative buffer for the client region and retain CoreGraphics only
    /// for the platform-owned titlebar, traffic lights, shadows, and rounded-edge alpha.
    /// This removes timing guesses from introspection and makes the returned PNG one
    /// atomic projection of current aterm state plus current OS chrome.
    #[cfg(target_os = "macos")]
    fn current_window_rgba_of(&mut self, wid: WindowId) -> Result<(Vec<u8>, u32, u32), String> {
        let (mut rgba, width, height) = self.window_rgba_of(Some(wid))?;
        let platform_overlay_height = u32::try_from(self.native_content_origin_y(wid))
            .map_err(|_| "native image capture chrome height overflow".to_string())?;
        let max_width_remainder = u32::try_from(self.win_cell_size(wid).0)
            .map_err(|_| "native image capture cell width overflow".to_string())?;
        let frame = self.current_native_capture_frame(wid)?;
        stitch_native_frame_into_window_rgba(
            &mut rgba,
            width,
            height,
            platform_overlay_height,
            max_width_remainder,
            &frame,
        )?;
        Ok((rgba, width, height))
    }

    /// Windows: photograph the frontmost window (caption chrome + GPU grid) via
    /// `PrintWindow` and hand the RGBA8 to the shared encode/confined-write worker —
    /// the same `EncodeJob::WindowRgba` path macOS uses. Runs on the main thread (per
    /// [`Wake::CaptureWindow`]). This is how the native on-glass look is verified.
    #[cfg(windows)]
    fn capture_native_window_image(
        &mut self,
        wid: WindowId,
        target: control_auth::ConfinedImage,
        want_bytes: bool,
        want_metadata: bool,
        frame_metadata: &std::sync::Arc<std::sync::OnceLock<crate::control::ImageFrameMetadata>>,
        reply: std::sync::mpsc::Sender<crate::control::ImageReply>,
    ) {
        let Some(window) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_deref())
        else {
            let _ = reply.send(Err("no window to capture (headless)".to_string()));
            return;
        };
        let captured = crate::platform_win::capture_window_rgba(window).and_then(
            |(mut rgba, width, height)| {
                let frame = self.current_native_capture_frame(wid)?;
                let max_width_remainder = u32::try_from(self.win_cell_size(wid).0)
                    .map_err(|_| "native image capture cell width overflow".to_string())?;
                stitch_native_frame_into_window_rgba(
                    &mut rgba,
                    width,
                    height,
                    0,
                    max_width_remainder,
                    &frame,
                )?;
                Ok((rgba, width, height))
            },
        );
        match captured {
            Ok((rgba, width, height)) => {
                if want_metadata {
                    match self.native_image_metadata(
                        wid,
                        "presented",
                        width,
                        height,
                        Self::image_pixel_fingerprint(width as usize, height as usize, &rgba),
                    ) {
                        Ok(metadata) => {
                            let _ = frame_metadata.set(metadata);
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            return;
                        }
                    }
                }
                self.submit_encode_job(EncodeJob::ImageRgba {
                    rgba,
                    width,
                    height,
                    target,
                    want_bytes,
                    reply,
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    #[cfg(windows)]
    pub(crate) fn capture_window(
        &mut self,
        target: control_auth::ConfinedImage,
        reply: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    ) {
        let Some(wid) = self.frontmost_window else {
            let _ = reply.send(Err("no window to capture (headless)".to_string()));
            return;
        };
        if let Err(error) = self.present_before_window_capture(wid) {
            let _ = reply.send(Err(error));
            return;
        }
        let Some(window) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_deref())
        else {
            let _ = reply.send(Err("no window to capture (headless)".to_string()));
            return;
        };
        let captured = crate::platform_win::capture_window_rgba(window).and_then(
            |(mut rgba, width, height)| {
                if self.active_tab_has_native(wid) {
                    let frame = self.current_native_capture_frame(wid)?;
                    let max_width_remainder = u32::try_from(self.win_cell_size(wid).0)
                        .map_err(|_| "native image capture cell width overflow".to_string())?;
                    stitch_native_frame_into_window_rgba(
                        &mut rgba,
                        width,
                        height,
                        0,
                        max_width_remainder,
                        &frame,
                    )?;
                }
                Ok((rgba, width, height))
            },
        );
        match captured {
            Ok((rgba, width, height)) => self.submit_encode_job(EncodeJob::WindowRgba {
                rgba,
                width,
                height,
                target,
                reply,
            }),
            Err(e) => {
                let _ = reply.send(Err(format!("window capture failed: {e}")));
            }
        }
    }

    /// Read back the exact renderer-owned native composite last prepared by the
    /// synchronous present barrier. This is intentionally separate from
    /// `render_native_image`: callers need the pixels in-process so they can splice
    /// them under the real OS chrome before the encode worker takes ownership.
    #[cfg(any(target_os = "macos", windows))]
    fn current_native_capture_frame(
        &mut self,
        wid: WindowId,
    ) -> Result<aterm_render::Frame, String> {
        let render_t0 = Instant::now();
        let App {
            backend,
            introspect_gpu,
            windows,
            ..
        } = self;
        let Some(window) = windows.get_mut(&wid) else {
            return Err("native image capture lost its window".to_string());
        };
        let Some(compiled) = window.native_ui_compiled.as_ref() else {
            return Err("native image capture has no prepared semantic frame".to_string());
        };
        if crate::native_capture_source_decision(
            compiled.phase == crate::app_native::NativeCompiledPhase::Presented,
            true,
        ) != crate::NativeCaptureSourceDecision::StitchRenderer
        {
            return Err("native image capture semantic frame was not presented".to_string());
        }
        let tray = window
            .settings_card
            .as_ref()
            .map(|card| aterm_gpu::TrayQuad {
                rgba: card.rgba.as_slice(),
                pw: card.pw,
                ph: card.ph,
                dx: card.dx,
                dy: card.dy,
            });
        let mut frame = backend.render_input(introspect_gpu, &mut window.input_scratch, tray);
        if !backend.is_gpu()
            && let Some(card) = window.settings_card.as_ref()
        {
            composite_tray(&mut frame.pixels, frame.width, frame.height, card);
        }
        crate::metrics::record_present(0, render_t0.elapsed().as_nanos() as u64);
        Ok(frame)
    }

    /// Off macOS/Windows there is no window server / `PrintWindow` to photograph, so
    /// the `window` verb reports that plainly. Kept as a method on every target so the
    /// [`Wake::CaptureWindow`] handler is platform-independent.
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    pub(crate) fn capture_window(
        &mut self,
        _target: control_auth::ConfinedImage,
        reply: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    ) {
        let _ = reply.send(Err(
            "window capture is only available on macOS and Windows".to_string()
        ));
    }
}

impl App {
    /// Capture an AUXILIARY GUI window — the Preferences window or the Performance
    /// control panel — to the confined PNG, replying its `(width, height)`.
    /// Serves the `window prefs` / `window perf` introspection verbs (main thread, per
    /// [`crate::Wake::CaptureAuxWindow`]).
    ///
    /// [`AuxTarget::Front`] delegates to the unchanged [`Self::capture_window`] (the
    /// frontmost terminal window). For the aux windows we read the directly-owned
    /// `NSWindow`'s `windowNumber()` straight off its retained handle (no winit
    /// `NSView -> window()` hop) and reuse the EXACT same `capture_window_pixels` +
    /// `encode_rgba8_png` + confined `write_private_at` path `capture_window` uses — so
    /// the sacred capture path, the path confinement, and the Screen-Recording-permission
    /// error are all identical. Replies `Err` (never panics) when the target window is not
    /// open or capture fails; captured pixels are encoded + written on the encode
    /// worker, which sends `reply` after the write.
    #[cfg(target_os = "macos")]
    pub(crate) fn capture_aux_window(
        &mut self,
        target: AuxTarget,
        confined: control_auth::ConfinedImage,
        reply: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    ) {
        // Front, native Settings, and every overlay are rendered into the front
        // window's WYSIWYG frame. Preferences has no auxiliary OS window.
        if matches!(target, AuxTarget::Prefs) {
            return self.capture_window_of(self.frontmost_window, confined, reply);
        }
        if matches!(
            target,
            AuxTarget::Front | AuxTarget::About | AuxTarget::Menu | AuxTarget::Update
        ) {
            return self.capture_window(confined, reply);
        }
        // Resolve the aux window's CGWindowID off its retained handle (None = not open).
        let window_number = match target {
            AuxTarget::Perf => self._perf_panel.as_ref().and_then(|h| h.window_number()),
            AuxTarget::Front
            | AuxTarget::Prefs
            | AuxTarget::About
            | AuxTarget::Menu
            | AuxTarget::Update => {
                unreachable!("handled above")
            }
        };
        let Some(n) = window_number else {
            // `window_number()` is None both when the window was never built AND when it
            // was closed/minimized (an ordered-out NSWindow reports windowNumber() <= 0),
            // so the handle may be present-but-hidden — point the caller at `open` either
            // way (it shows/re-shows the window).
            let kw = target.keyword();
            let _ = reply.send(Err(format!(
                "{kw} window is not on screen (closed or never opened); \
                 send `open {kw}` to show it, then retry"
            )));
            return;
        };
        // Same confined-write path as `capture_window`: photograph by CGWindowID here
        // (main thread), then encode RGBA8 → PNG + write via the `images/` dir fd
        // (O_NOFOLLOW) on the encode worker.
        match capture_window_pixels(n as u32) {
            Ok((rgba, width, height)) => self.submit_encode_job(EncodeJob::WindowRgba {
                rgba,
                width,
                height,
                target: confined,
                reply,
            }),
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        }
    }

    /// Off macOS there is no CoreGraphics window server to photograph, so the aux-window
    /// capture reports that plainly (kept on every target so the
    /// [`crate::Wake::CaptureAuxWindow`] handler is platform-independent).
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn capture_aux_window(
        &mut self,
        _target: AuxTarget,
        _confined: control_auth::ConfinedImage,
        reply: std::sync::mpsc::Sender<Result<(u32, u32), String>>,
    ) {
        let _ = reply.send(Err(
            "auxiliary window capture is only available on macOS".to_string()
        ));
    }

    /// Read an AUXILIARY window's CONTROLS as human-readable text lines — the `controls
    /// prefs` / `controls perf` introspection verbs (the analogue of `chrome` for aux
    /// windows). Serves [`crate::Wake::ReadAuxControls`] on the main thread.
    ///
    /// Built from the SAME PURE models the windows render from — `prefs::editable_fields`
    /// over the live `self.config`, `perf_panel::perf_panel_lines` over the live panel
    /// state — rather than walking `NSView` subviews. So it is deterministic, AppKit-free
    /// (works HEADLESS, where the window may never be built), and byte-identical to what
    /// the window shows. `Front` has no settings list — point the caller at `chrome`.
    pub(crate) fn read_aux_controls(&self, target: AuxTarget) -> Vec<String> {
        match target {
            AuxTarget::Prefs => {
                // Preserve the historical `controls prefs` schema by projecting the
                // SettingsState embedded in the live native Settings view. New clients
                // use `inspect app/v1`; both read the same controller, never a hidden
                // overlay or auxiliary window.
                if let Some(s) = self.native_settings_legacy_state() {
                    return crate::overlay::OverlayModel::controls_lines(s);
                }
                let snapshot = crate::prefs::editable_fields(&self.config);
                // The loaded Trail Pack ids for the `cursor_trail_style` domain
                // (empty + no IO when none are configured).
                let trail_pack_ids = self.config_assets.trail_packs.ids.clone();
                let mut out = Vec::with_capacity(snapshot.len() + 2);
                out.push("state open=false".to_string());
                out.push(format!("prefs fields={}", snapshot.len()));
                for f in &snapshot {
                    // `value` is the user's CONFIGURED raw value (blank = unset);
                    // `effective` is what is actually in use (the placeholder hint).
                    let value = f.seed.as_deref().unwrap_or("");
                    // `kind` makes the control TYPE machine-readable; an Enum also lists
                    // its allowed `options` so a reader (an AI driving settings) knows the
                    // exact value domain — e.g. cursor_trail_style's phaser/fire + packs.
                    let kind = match f.kind {
                        crate::prefs::EditKind::Float => "float".to_string(),
                        crate::prefs::EditKind::Integer => "integer".to_string(),
                        crate::prefs::EditKind::Bool => "bool".to_string(),
                        crate::prefs::EditKind::Text => "text".to_string(),
                        crate::prefs::EditKind::Enum { .. }
                            if f.key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE =>
                        {
                            format!(
                                "enum options=[{}]",
                                crate::prefs::cursor_trail_style_options(
                                    trail_pack_ids.iter().map(String::as_str)
                                )
                                .join(",")
                            )
                        }
                        crate::prefs::EditKind::Enum { options } => {
                            format!("enum options=[{}]", options.join(","))
                        }
                        // Theme options are the live built-in registry (resolved here so
                        // the introspected domain tracks `scheme::builtin_names`).
                        crate::prefs::EditKind::Theme => {
                            format!(
                                "theme options=[{}]",
                                aterm_types::scheme::builtin_names().join(",")
                            )
                        }
                        crate::prefs::EditKind::Color => "color".to_string(),
                    };
                    out.push(format!(
                        "field key={} label={:?} value={:?} effective={:?} kind={}",
                        f.key, f.label, value, f.placeholder, kind
                    ));
                }
                // §F4.6: the Kitty Log collection book is part of the settings
                // surface — append its rows from the App's in-memory store so
                // the page verifies headlessly with the overlay CLOSED, via the
                // same `kittylog …` serialization the open overlay emits.
                out.extend(crate::kitty_log::book_lines(self.kitty_log.log()));
                out
            }
            AuxTarget::Perf => {
                let toggles: Vec<(crate::hud_bar::PanelId, bool)> = crate::hud_bar::PanelId::ALL
                    .iter()
                    .map(|&id| (id, self.panel_enabled(id)))
                    .collect();
                crate::perf_panel::perf_panel_lines(&toggles)
            }
            AuxTarget::About => {
                // Prefer the LIVE open overlay (focused row + copy status) so the text
                // matches the painted card; else a fresh snapshot so `controls about` works
                // headless / when the overlay is closed.
                match self.front().and_then(|ws| ws.about()) {
                    Some(a) => a.controls_lines(),
                    None => crate::about::AboutState::new().controls_lines(),
                }
            }
            AuxTarget::Menu => {
                // Prefer the LIVE open palette (query + cursor + resolved enabled/checked)
                // so the text matches the painted card; else a fresh LIVE-RESOLVED
                // snapshot so `controls menu` works headless / when the palette is
                // closed — resolved (not the raw model) so the Version section's
                // dynamic update row (staged / realized / absent) reads truthfully.
                match self.front().and_then(|ws| ws.palette()) {
                    Some(p) => p.controls_lines(),
                    None => self
                        .frontmost_window
                        .map_or_else(crate::palette::PaletteState::new, |wid| {
                            self.palette_snapshot(wid)
                        })
                        .controls_lines(),
                }
            }
            AuxTarget::Update => {
                // Prefer the LIVE open overlay (its captured snapshot + checking state) so
                // the text matches the painted card; else a fresh snapshot so `controls
                // update` works headless / when the overlay is closed.
                match self.front().and_then(|ws| ws.update_screen()) {
                    Some(u) => u.controls_lines(),
                    None => self.update_snapshot(false).controls_lines(),
                }
            }
            AuxTarget::Front => match self.front().and_then(|ws| ws.overlay()) {
                // The front window's modal overlay is what a front `image` capture shows,
                // so `controls front` reports THAT slot (open/closed + kind + fp + scroll
                // extent). A windowed-⌘ Settings NSWindow is reached via `controls settings`
                // / `window prefs`, not here. Live state, no geometry — headless-safe.
                Some(o) => vec![o.status_line()],
                None => vec!["overlay open=false".to_string()],
            },
        }
    }
}

/// Photograph the on-screen window with CoreGraphics window id `window_id` and
/// return its `(tightly-packed RGBA8 bytes, width, height)`. Runs on the MAIN
/// thread (called from the `capture_window`/`capture_aux_window` verbs' AppKit
/// half; the PNG encode + write run on the encode worker).
///
/// Robust-format strategy (per the implementation note): rather than read the
/// source `CGImage`'s native, possibly-padded pixel layout, we draw it into a
/// freshly-created RGBA8 `CGBitmapContext` we own, then read THAT context's
/// tightly-packed buffer (`width * 4` stride, premultiplied-alpha-last). So the
/// bytes are always plain RGBA8 no matter what the window server hands us.
///
/// Returns `Err` (never panics / leaks) when CoreGraphics cannot capture — almost
/// always a missing Screen Recording grant, which the caller turns into the clear,
/// actionable permission error.
#[cfg(target_os = "macos")]
pub(crate) fn capture_window_pixels(window_id: u32) -> Result<(Vec<u8>, u32, u32), String> {
    use crate::cg_capture::*;

    // SAFETY: `CGWindowListCreateImage` is the documented capture entry point; we
    // pass `CGRectNull` (use the window's own bounds), the single-window option keyed
    // by `window_id`, and the ignore-framing | best-resolution image options. It
    // returns either a NEW CGImage we own (and release below) or NULL on failure.
    let image: CGImageRef = unsafe {
        CGWindowListCreateImage(
            CG_RECT_NULL,
            K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
            window_id,
            K_CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING | K_CG_WINDOW_IMAGE_BEST_RESOLUTION,
        )
    };
    if image.is_null() {
        // The single most common cause is a missing Screen Recording grant; give the
        // exact, actionable remediation rather than a bare failure.
        return Err(
            "window capture failed (grant Screen Recording permission to aterm-gui in \
             System Settings > Privacy & Security > Screen Recording, then retry)"
                .to_string(),
        );
    }

    // From here on, `image` MUST be released on every path — use a tiny guard so an
    // early `?`/return cannot leak it. SAFETY: `image` is the live CGImage we just
    // created; `CGImageGetWidth/Height` are side-effect-free accessors on it.
    struct ImageGuard(CGImageRef);
    impl Drop for ImageGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the CGImage created above, released exactly once.
            unsafe { CGImageRelease(self.0) };
        }
    }
    let _image_guard = ImageGuard(image);

    let width = unsafe { CGImageGetWidth(image) };
    let height = unsafe { CGImageGetHeight(image) };
    if width == 0 || height == 0 {
        return Err("window capture failed (captured image has zero size)".to_string());
    }

    let bytes_per_row = width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or_else(|| "window capture failed (image too large)".to_string())?;

    // SAFETY: standard CG calls. `CGColorSpaceCreateDeviceRGB` returns a new colour
    // space we release below. `CGBitmapContextCreate` with NULL data + RGBA8 /
    // premultiplied-last creates a context whose backing buffer CG allocates and
    // owns until we release the context; we read it (via `CGBitmapContextGetData`)
    // strictly before that release.
    let color_space: CGColorSpaceRef = unsafe { CGColorSpaceCreateDeviceRGB() };
    if color_space.is_null() {
        return Err("window capture failed (could not create RGB color space)".to_string());
    }
    struct CsGuard(CGColorSpaceRef);
    impl Drop for CsGuard {
        fn drop(&mut self) {
            // SAFETY: the colour space created above, released exactly once.
            unsafe { CGColorSpaceRelease(self.0) };
        }
    }
    let _cs_guard = CsGuard(color_space);

    let context: CGContextRef = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width,
            height,
            BITS_PER_COMPONENT,
            bytes_per_row,
            color_space,
            K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST,
        )
    };
    if context.is_null() {
        return Err("window capture failed (could not create bitmap context)".to_string());
    }
    struct CtxGuard(CGContextRef);
    impl Drop for CtxGuard {
        fn drop(&mut self) {
            // SAFETY: the context created above, released exactly once. Its backing
            // buffer is freed here — AFTER we have already copied the bytes out.
            unsafe { CGContextRelease(self.0) };
        }
    }
    let _ctx_guard = CtxGuard(context);

    // Draw the captured image to fill the whole context, normalizing it to our
    // known RGBA8 layout. SAFETY: `context` and `image` are both live objects we
    // created; the rect spans the full context.
    let full = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: width as f64,
            height: height as f64,
        },
    };
    unsafe { CGContextDrawImage(context, full, image) };

    // Read the tightly-packed RGBA8 bytes back out. SAFETY: `CGBitmapContextGetData`
    // returns a pointer to the context's backing buffer (valid until the context is
    // released, which the guard does only AFTER this copy). We copy exactly
    // `bytes_per_row * height` bytes — the buffer's full size for our chosen stride.
    let data_ptr = unsafe { CGBitmapContextGetData(context) } as *const u8;
    if data_ptr.is_null() {
        return Err("window capture failed (bitmap context has no data)".to_string());
    }
    let total = bytes_per_row
        .checked_mul(height)
        .ok_or_else(|| "window capture failed (image too large)".to_string())?;
    // SAFETY: `data_ptr` is the context's backing buffer of exactly `total` bytes
    // (width*4 stride, no extra padding — CG honours the stride we requested).
    let rgba = unsafe { std::slice::from_raw_parts(data_ptr, total) }.to_vec();

    Ok((rgba, width as u32, height as u32))
}

/// Replace the client-area pixels in a full platform-window photograph with the
/// exact native frame produced by aterm's semantic renderer.
///
/// Platform captures and renderer frames share physical-pixel width. The native
/// frame is bottom-aligned because the only extra vertical extent in the platform
/// image is titlebar chrome above the winit content view. Fully transparent and
/// antialiased platform-edge pixels are retained so rounded corners and shadows stay
/// genuinely native; every opaque client pixel comes from the current renderer frame.
#[cfg(any(target_os = "macos", windows, test))]
fn stitch_native_frame_into_window_rgba(
    window_rgba: &mut [u8],
    window_width: u32,
    window_height: u32,
    platform_overlay_height: u32,
    max_width_remainder: u32,
    frame: &aterm_render::Frame,
) -> Result<(), String> {
    let width = usize::try_from(window_width)
        .map_err(|_| "native image capture width does not fit memory".to_string())?;
    let height = usize::try_from(window_height)
        .map_err(|_| "native image capture height does not fit memory".to_string())?;
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "native image capture dimensions overflow".to_string())?;
    let frame_pixels = frame.width.checked_mul(frame.height);
    let overlay_height = usize::try_from(platform_overlay_height)
        .map_err(|_| "native image capture chrome height does not fit memory".to_string())?;
    let max_width_remainder = usize::try_from(max_width_remainder)
        .map_err(|_| "native image capture cell width does not fit memory".to_string())?;
    let width_remainder = width.abs_diff(frame.width);
    let geometry_valid = window_rgba.len() == expected
        && width_remainder <= max_width_remainder
        && frame.height <= height
        && overlay_height <= height
        && frame_pixels == Some(frame.pixels.len());
    if crate::native_capture_source_decision(true, geometry_valid)
        != crate::NativeCaptureSourceDecision::StitchRenderer
    {
        if window_rgba.len() != expected {
            return Err(format!(
                "native image capture buffer has {} bytes, expected {expected}",
                window_rgba.len()
            ));
        }
        if width_remainder > max_width_remainder {
            return Err(format!(
                "native image capture width mismatch (window {width}px, renderer {}px, maximum centered remainder {max_width_remainder}px)",
                frame.width,
            ));
        }
        if frame.height > height {
            return Err(format!(
                "native image capture height mismatch (window {height}px, renderer {}px)",
                frame.height
            ));
        }
        if overlay_height > height {
            return Err(format!(
                "native image capture chrome mismatch (window {height}px, chrome {overlay_height}px)"
            ));
        }
        if frame_pixels.is_none() {
            return Err("native renderer frame dimensions overflow".to_string());
        }
        return Err(format!(
            "native renderer frame has {} pixels, expected {}",
            frame.pixels.len(),
            frame_pixels.unwrap_or_default()
        ));
    }
    let y_offset = height - frame.height;
    let x_offset = aterm_render::band_offset(width, frame.width);
    for (index, &pixel) in frame.pixels.iter().enumerate() {
        let src_y = index / frame.width;
        let src_x = index % frame.width;
        let dst_x = x_offset + src_x as i64;
        if !(0..width as i64).contains(&dst_x) {
            continue;
        }
        let dst_y = y_offset + src_y;
        if dst_y < overlay_height.max(y_offset) {
            continue;
        }
        let dst = (dst_y * width + dst_x as usize) * 4;
        // Preserve transparent/partially transparent pixels from the platform
        // capture: they carry the OS-owned rounded-edge antialias and shadow.
        if window_rgba[dst + 3] != u8::MAX {
            continue;
        }
        window_rgba[dst] = (pixel >> 16) as u8;
        window_rgba[dst + 1] = (pixel >> 8) as u8;
        window_rgba[dst + 2] = pixel as u8;
    }
    Ok(())
}

/// Encode a tightly-packed RGBA8 buffer (`width * height * 4` bytes, no row
/// padding) to PNG bytes, reusing the same `png` crate the `image` verb's
/// framebuffer path uses. Used by the `window` capture verb.
#[cfg(any(target_os = "macos", windows))]
pub(crate) fn encode_rgba8_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod dims_snapshot_tests {
    use std::time::Instant;

    use super::{App, dims_axis};
    use crate::WindowId;
    use winit::dpi::PhysicalSize;

    #[test]
    fn dims_axis_exposes_odd_trailing_bands_and_centered_crop() {
        assert_eq!(
            dims_axis(111, 100),
            (5, 5, 6, 0, 0),
            "odd spare pixel stays in the trailing band"
        );
        assert_eq!(
            dims_axis(100, 111),
            (-6, 0, 0, 6, 5),
            "the same centered rule exposes transient crop on both sides"
        );
    }

    #[test]
    fn coherent_dims_snapshot_is_nonblocking_when_the_grid_is_busy() {
        let app = App::headless_for_test();
        let term = app.front_terminal(WindowId(0)).unwrap().term.clone();
        let snapshot = app.try_dims_snapshot(0, &term).unwrap();
        assert_eq!((snapshot.rows, snapshot.cols), (24, 80));

        let _held = term.lock().unwrap();
        assert_eq!(
            app.try_dims_snapshot(0, &term).unwrap_err(),
            "terminal busy; retry dims"
        );
    }

    #[test]
    fn dims_snapshot_tracks_live_zoom_and_raw_surface_remainder() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.backend.set_pad(12);
        app.backend.set_pad_top(2);
        app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
        app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
        let initial = app.dims_snapshot(0, 24, 80);
        assert_eq!(initial.window, Some(0));
        assert_eq!((initial.viewers, initial.visible_viewers), (1, 1));
        assert_eq!(initial.geometry, "headless");
        assert_eq!(
            (initial.surface_w, initial.surface_h),
            (initial.frame_w, initial.frame_h)
        );
        assert_eq!(initial.pixel_w, 80 * initial.cell_w);
        assert_eq!(initial.pixel_h, 24 * initial.cell_h);
        assert_eq!((initial.pad_top, initial.pad_bottom), (2, 12));
        assert_eq!(initial.pad, initial.pad_bottom);
        assert_eq!(
            initial.frame_h,
            initial
                .composed_rows
                .saturating_mul(initial.cell_h)
                .saturating_add(initial.head)
                .saturating_add(initial.pad_top)
                .saturating_add(initial.pad_bottom),
            "dims exposes the same explicit top/bottom frame law as presentation",
        );
        assert_ne!(
            initial.frame_h,
            initial
                .composed_rows
                .saturating_mul(initial.cell_h)
                .saturating_add(initial.head)
                .saturating_add(initial.pad.saturating_mul(2)),
            "negative control: dims must not retain the old conserved-height law",
        );
        assert_eq!(initial.present_retry_state, "ready");
        assert_eq!(initial.present_retry_count, 0);
        assert_eq!(
            initial.present_retry_remaining,
            u32::from(crate::PRESENT_RETRY_CAP)
        );
        assert_eq!(initial.present_retry_in_ms, None);

        app.windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_recovery_redraw_requested();
        let redraw = app.dims_snapshot(0, 24, 80);
        assert_eq!(redraw.present_retry_state, "redraw");
        assert_eq!(redraw.present_retry_count, 0);
        assert_eq!(redraw.present_retry_in_ms, None);
        let _ = app
            .windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_external_stimulus();

        // The same live window snapshot exposes CURRENT recovery state rather
        // than forcing an operator to infer it from a stale process-global last
        // drop. Exercise both transient backoff and persistent parking.
        app.windows.get_mut(&wid).unwrap().present_retry.on_drop(
            crate::metrics::PresentDropReason::GpuTimeout,
            Instant::now(),
        );
        let backing_off = app.dims_snapshot(0, 24, 80);
        assert_eq!(backing_off.present_retry_state, "backoff");
        assert_eq!(backing_off.present_retry_count, 1);
        assert_eq!(backing_off.present_retry_remaining, 4);
        assert!(backing_off.present_retry_in_ms.is_some());
        let _ = app
            .windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_external_stimulus();
        app.windows.get_mut(&wid).unwrap().present_retry.on_drop(
            crate::metrics::PresentDropReason::GpuOccluded,
            Instant::now(),
        );
        let parked = app.dims_snapshot(0, 24, 80);
        assert_eq!(parked.present_retry_state, "parked");
        assert_eq!(parked.present_retry_in_ms, None);
        let _ = app
            .windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_external_stimulus();

        // Model the exact class seen during an interactive resize: the raw
        // surface has a non-cell remainder around the centered renderer frame.
        let surface = PhysicalSize::new(initial.frame_w + 11, initial.frame_h + 28);
        app.windows.get_mut(&wid).unwrap().win_px = Some(surface);
        let banded = app.dims_snapshot(0, 24, 80);
        assert_eq!(banded.geometry, "window");
        assert_eq!(
            (banded.surface_w, banded.surface_h),
            (surface.width, surface.height)
        );
        assert_eq!((banded.band_left, banded.band_right), (5, 6));
        assert_eq!((banded.band_top, banded.band_bottom), (14, 14));
        assert_eq!(banded.offset_y, i64::from(banded.band_top));
        assert_eq!(
            banded.band_top + banded.band_bottom,
            banded.surface_h - banded.frame_h,
        );
        assert_eq!(
            banded.pad_bottom + banded.band_bottom,
            26,
            "visible trailing edge is base bottom plus only the raw-surface remainder",
        );
        assert_eq!(
            (
                banded.crop_left,
                banded.crop_right,
                banded.crop_top,
                banded.crop_bottom,
            ),
            (0, 0, 0, 0)
        );

        // Live font zoom rebuilds the backend and refreshes every window's
        // MetricsView. `dims` must immediately reflect the new cells/frame while
        // retaining the independently observed raw surface size.
        let old_font_px = app.font_px;
        app.windows.get_mut(&wid).unwrap().present_retry.on_drop(
            crate::metrics::PresentDropReason::GpuOccluded,
            Instant::now(),
        );
        app.set_font_px(old_font_px + 8.0);
        let zoomed = app.dims_snapshot(0, 24, 80);
        assert_eq!(
            zoomed.present_retry_state, "ready",
            "font rebuild is an explicit recovery stimulus"
        );
        assert!(zoomed.cell_w > banded.cell_w && zoomed.cell_h > banded.cell_h);
        assert!(zoomed.pixel_w > banded.pixel_w && zoomed.pixel_h > banded.pixel_h);
        assert!(zoomed.frame_w > banded.frame_w && zoomed.frame_h > banded.frame_h);
        assert_eq!(
            (zoomed.surface_w, zoomed.surface_h),
            (surface.width, surface.height),
            "the raw surface is an independent live fact"
        );
        assert_eq!(
            zoomed.crop_left + zoomed.crop_right,
            zoomed.frame_w.saturating_sub(zoomed.surface_w)
        );
        assert_eq!(
            zoomed.crop_top + zoomed.crop_bottom,
            zoomed.frame_h.saturating_sub(zoomed.surface_h)
        );
    }
}

#[cfg(test)]
mod chrome_output_tests {
    use super::{non_macos_chrome_output, stitch_native_frame_into_window_rgba};

    #[test]
    fn non_macos_app_chrome_includes_live_titles_and_icon_policy() {
        let terminal = crate::tab_model::TabPresentation::terminal("build-server");
        let settings = crate::tab_model::TabPresentation {
            title: "Settings".to_string(),
            icon: Some(crate::tab_model::TabIconKind::Settings),
            indicators: crate::tab_model::TabIndicators::default(),
            closable: true,
            tooltip: Some("Settings · Cursor & Motion".to_string()),
        };
        let metadata = [
            crate::tab_bar::TabStripMetadata::from_presentation(&terminal),
            crate::tab_bar::TabStripMetadata::from_presentation(&settings),
        ];
        let toolbar_tabs = crate::toolbar::format_tab_chrome(
            &[terminal.title, settings.title],
            &metadata,
            &[None, settings.tooltip],
            1,
        );
        let output = non_macos_chrome_output(
            vec!["window-platform chrome".to_string()],
            toolbar_tabs,
            vec!["tab-menu tab=0 items=[]".to_string()],
            vec!["menu \"File\": New Tab".to_string()],
        );

        assert_eq!(output[0], "window-platform chrome");
        assert_eq!(output[1], "toolbar (none)");
        assert!(output[2].contains("selected=1"));
        assert!(output[2].contains(r#"labels=["build-server", "Settings"]"#));
        assert!(output[2].contains(r#"icons=[None, Some("settings")]"#));
        assert!(output[2].contains("Settings · Cursor & Motion"));
        assert_eq!(output[3], "tab-menu tab=0 items=[]");
        assert_eq!(output[4], "menu \"File\": New Tab");
    }

    #[test]
    fn native_capture_stitch_keeps_chrome_and_rounded_edges_but_replaces_client() {
        // 2×3 platform image: top row is titlebar chrome, lower two rows are the
        // client area. One client pixel is a partially-transparent rounded edge.
        let mut rgba = vec![
            1, 2, 3, 255, 4, 5, 6, 255, // platform titlebar
            7, 8, 9, 255, 10, 11, 12, 127, // stale client + rounded edge
            13, 14, 15, 255, 16, 17, 18, 255, // stale client
        ];
        let frame = aterm_render::Frame {
            width: 2,
            height: 2,
            pixels: vec![0x0011_2233, 0x0044_5566, 0x0077_8899, 0x00AA_BBCC],
        };

        stitch_native_frame_into_window_rgba(&mut rgba, 2, 3, 1, 0, &frame).unwrap();

        assert_eq!(&rgba[..8], &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(&rgba[8..12], &[0x11, 0x22, 0x33, 255]);
        assert_eq!(
            &rgba[12..16],
            &[10, 11, 12, 127],
            "platform antialias is authoritative at rounded edges"
        );
        assert_eq!(&rgba[16..], &[0x77, 0x88, 0x99, 255, 0xAA, 0xBB, 0xCC, 255]);
    }

    #[test]
    fn native_capture_stitch_fails_closed_on_geometry_or_buffer_drift() {
        let honest = aterm_render::Frame {
            width: 2,
            height: 1,
            pixels: vec![0, 0],
        };
        assert!(stitch_native_frame_into_window_rgba(&mut [0; 7], 2, 1, 0, 0, &honest).is_err());

        let wrong_width = aterm_render::Frame {
            width: 1,
            height: 1,
            pixels: vec![0],
        };
        assert!(
            stitch_native_frame_into_window_rgba(&mut [0; 8], 2, 1, 0, 0, &wrong_width).is_err()
        );

        let truncated = aterm_render::Frame {
            width: 2,
            height: 1,
            pixels: vec![0],
        };
        assert!(stitch_native_frame_into_window_rgba(&mut [0; 8], 2, 1, 0, 0, &truncated).is_err());
    }

    #[test]
    fn native_capture_stitch_uses_the_renderers_centered_remainder_rule() {
        let mut rgba = vec![0_u8; 3 * 4];
        for alpha in rgba.iter_mut().skip(3).step_by(4) {
            *alpha = u8::MAX;
        }
        let frame = aterm_render::Frame {
            width: 2,
            height: 1,
            pixels: vec![0x0011_2233, 0x0044_5566],
        };

        stitch_native_frame_into_window_rgba(&mut rgba, 3, 1, 0, 1, &frame).unwrap();

        assert_eq!(&rgba[..4], &[0x11, 0x22, 0x33, 255]);
        assert_eq!(&rgba[4..8], &[0x44, 0x55, 0x66, 255]);
        assert_eq!(
            &rgba[8..],
            &[0, 0, 0, 255],
            "odd remainder stays in the trailing band, matching band_offset"
        );
    }
}

#[cfg(test)]
mod encode_worker_tests {
    use super::*;
    use crate::control_auth::ensure_private_dir;
    use std::time::Duration;

    fn confined(dir: &std::path::Path, name: &str) -> control_auth::ConfinedImage {
        control_auth::ConfinedImage {
            dir: dir.to_path_buf(),
            file_name: std::ffi::OsString::from(name),
        }
    }

    /// The encode worker's contract: the client's reply is sent only AFTER the
    /// confined write, in FIFO submission order, through EXACTLY ONE worker — so
    /// `OK <w> <h>` on the control wire always means the PNG is complete, and a
    /// burst of queued `image` requests replies in queue order.
    #[test]
    fn encode_worker_replies_fifo_and_only_after_write() {
        let dir = std::env::temp_dir().join(format!("aterm-encode-worker-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let (tx, rx) = std::sync::mpsc::channel();
        // Three jobs with distinct dims + targets on ONE shared reply channel,
        // so the recv order below is exactly the worker's reply order.
        for (i, side) in [1usize, 2, 3].into_iter().enumerate() {
            app.submit_encode_job(EncodeJob::Image {
                frame: Frame {
                    width: side,
                    height: side,
                    pixels: vec![0u32; side * side],
                },
                target: confined(&dir, &format!("shot{i}.png")),
                want_bytes: false,
                reply: tx.clone(),
            });
        }
        for (i, side) in [1u32, 2, 3].into_iter().enumerate() {
            let dims = rx.recv().expect("worker reply");
            assert_eq!(dims, Ok((side, side, None)), "reply {i} out of FIFO order");
            // Reply-after-write: at recv time this job's PNG must already be a
            // complete file (the client reads it the moment it sees OK).
            let bytes =
                std::fs::read(dir.join(format!("shot{i}.png"))).expect("file written before reply");
            assert!(
                bytes.starts_with(&[0x89, b'P', b'N', b'G']),
                "PNG signature missing"
            );
        }
        // The lazily-spawned worker is retained for reuse across captures.
        assert!(app.encode_tx.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terminal_image_metadata_binds_exact_snapshot_and_encoded_pixels() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-terminal-image-meta-binding-{}",
            std::process::id()
        ));
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let terminal = app.front_terminal_mirror(wid).expect("terminal front");
        crate::term_lock(&terminal.term).process(b"exact terminal metadata");
        let (view, session) = match app.windows[&wid].front_content.unwrap() {
            crate::front_content::FrontContent::Terminal { view, session } => (view, session),
            crate::front_content::FrontContent::Native { .. } => panic!("terminal front expected"),
        };

        let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            target: confined(&dir, "terminal.png"),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(&frame_metadata),
            reply: tx,
        });
        let (width, height, png) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("terminal image reply")
            .expect("terminal image succeeds");
        let png = png.expect("wire capture includes PNG bytes");
        let metadata = frame_metadata
            .get()
            .expect("terminal frame metadata is bound");
        assert_eq!(metadata.frame_kind, "terminal");
        assert_eq!(metadata.phase, "rendered");
        assert_eq!(metadata.view, Some(view.get()));
        assert_eq!((metadata.width, metadata.height), (width, height));
        assert_eq!(metadata.compiled_fingerprint, None);
        assert_eq!(metadata.leaves.len(), 1);
        let leaf = &metadata.leaves[0];
        assert_eq!(leaf.kind, "terminal");
        assert_eq!(leaf.view, view.get());
        assert_eq!(leaf.session, Some(session));
        assert!(leaf.focused);

        let retained = &app.windows[&wid].input_scratch;
        assert_eq!(leaf.snapshot_seq, Some(retained.snapshot_seq));
        assert_eq!(
            metadata.raster_model_fingerprint,
            App::terminal_snapshot_fingerprint(view.get(), session, retained),
        );
        assert_eq!(
            metadata.raster_geometry,
            App::terminal_geometry_fingerprint(width as usize, height as usize, retained),
        );

        let (rgba, decoded_width, decoded_height) =
            aterm_render::decode_png_rgba8(&png).expect("encoded terminal PNG decodes");
        assert_eq!(
            (decoded_width as u32, decoded_height as u32),
            (width, height)
        );
        let (rgba_pixels, remainder) = rgba.as_chunks::<4>();
        assert!(remainder.is_empty(), "decoded RGBA8 has complete pixels");
        let pixels = rgba_pixels
            .iter()
            .map(|pixel| {
                (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2])
            })
            .collect::<Vec<_>>();
        let encoded_pixel_fingerprint =
            App::image_pixel_fingerprint(decoded_width, decoded_height, &pixels);
        assert_eq!(metadata.pixel_fingerprint, encoded_pixel_fingerprint);
        assert_eq!(metadata.raster_fingerprint, encoded_pixel_fingerprint);
        assert_eq!(leaf.raster_fingerprint, Some(encoded_pixel_fingerprint));

        let wire = metadata.wire_fields();
        assert!(wire.contains("frame-kind=terminal"));
        assert!(wire.contains(&format!("view={}", view.get())));
        assert!(wire.contains(&format!("v{}:terminal:s{}:f1:", view.get(), session)));
        assert!(wire.contains(&format!("seq{}", retained.snapshot_seq)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn native_image_metadata_binds_dimensions_to_the_same_audit_frame() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-image-audit-binding-{}",
            std::process::id()
        ));
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_native_image(
            wid,
            confined(&dir, "binding.png"),
            true,
            true,
            &metadata,
            tx,
        );
        let (width, height, png) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("native image reply")
            .expect("native image succeeds");
        assert!(width > 0 && height > 0);
        assert!(png.is_some());

        let metadata = metadata.get().expect("native frame metadata is bound");
        assert_eq!(
            metadata.phase, "staged",
            "headless capture has no glass present"
        );
        assert_eq!(metadata.frame_kind, "native");
        assert_eq!(metadata.view, Some(view.get()));
        assert_eq!((metadata.width, metadata.height), (width, height));
        assert_ne!(metadata.pixel_fingerprint, 0);
        let generation = metadata.generation.unwrap();
        let config_revision = metadata.config_revision.unwrap();
        let update_revision = metadata.update_revision.unwrap();
        let presentation_revision = metadata.presentation_revision.unwrap();
        let paint_revision = metadata.paint_revision.unwrap();
        let geometry = metadata.leaves[0].geometry.unwrap();
        let compiled_fingerprint = metadata.compiled_fingerprint.unwrap();
        let audit = app
            .inspect_app(crate::app_control::InspectRequest::View {
                view,
                projection: aterm_types::app_inspection::InspectionProjection::Audit,
            })
            .unwrap();
        let source = &audit[1];
        for field in [
            format!("source={}", metadata.phase),
            format!("view={}", metadata.view.unwrap()),
            format!("generation={generation}"),
            format!("geometry={geometry:016x}"),
            format!("config-revision={config_revision}"),
            format!("update-revision={update_revision}"),
            format!("presentation-revision={presentation_revision}"),
            format!("paint-revision={paint_revision:016x}"),
            "model-current=true".to_string(),
            format!("capture-serial={}", metadata.capture_serial),
            format!("compiled-fingerprint={compiled_fingerprint:016x}"),
        ] {
            assert!(
                source.contains(&field),
                "Audit/source missing {field}: {source}"
            );
        }
        assert!(audit.iter().any(|line| {
            line.starts_with("paint-audit ")
                && line.contains(&format!("compiled-fingerprint={compiled_fingerprint:016x}"))
        }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn native_image_metadata_changes_for_paint_only_pixel_identity() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-image-paint-identity-{}",
            std::process::id()
        ));
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));

        let capture = |app: &mut App, name: &str| {
            let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
            let (tx, rx) = std::sync::mpsc::channel();
            app.render_native_image(wid, confined(&dir, name), true, true, &metadata, tx);
            let image = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("paint identity image reply")
                .expect("paint identity image succeeds");
            assert!(image.2.is_some());
            metadata.get().cloned().unwrap()
        };

        let before = capture(&mut app, "paint-before.png");
        app.theme.bg ^= 0x0008_1018;
        app.theme.fg ^= 0x0010_0804;
        let after = capture(&mut app, "paint-after.png");
        assert_eq!(before.frame_kind, "native");
        assert_eq!(before.view, after.view);
        assert_eq!(before.generation, after.generation);
        assert_eq!(before.config_revision, after.config_revision);
        assert_eq!(
            before.compiled_fingerprint, after.compiled_fingerprint,
            "theme-only paint does not invent a different semantic tree"
        );
        assert_ne!(before.paint_revision, after.paint_revision);
        assert_ne!(before.theme_fingerprint, after.theme_fingerprint);
        assert_ne!(
            before.raster_fingerprint, after.raster_fingerprint,
            "retained native raster bytes identify paint-only changes"
        );
        assert_ne!(
            before.pixel_fingerprint, after.pixel_fingerprint,
            "the metadata binds the exact final RGBA frame"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn headless_native_capture_composes_every_mixed_leaf_for_either_focus() {
        let dir =
            std::env::temp_dir().join(format!("aterm-mixed-native-capture-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, native_view) = app.active_native_view(wid).expect("Settings view");
        let (_, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert_eq!(
            app.windows[&wid].tab_set.active().unwrap().focus,
            terminal_view
        );

        let capture = |app: &mut App, name: &str| {
            let (tx, rx) = std::sync::mpsc::channel();
            let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
            app.render_native_image(wid, confined(&dir, name), true, true, &frame_metadata, tx);
            let image = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("mixed capture reply")
                .expect("mixed capture succeeds");
            (image, frame_metadata.get().cloned().unwrap())
        };
        let (terminal_focused, terminal_metadata) = capture(&mut app, "terminal-focused.png");
        assert!(terminal_focused.0 > 0 && terminal_focused.1 > 0);
        assert!(terminal_focused.2.is_some());
        assert_eq!(terminal_metadata.frame_kind, "composite");
        assert_eq!(terminal_metadata.view, None);
        assert_eq!(terminal_metadata.leaves.len(), 2);
        let terminal_wire = terminal_metadata.wire_fields();
        assert!(terminal_wire.contains("frame-kind=composite"));
        assert!(terminal_wire.contains("view=-"));
        assert!(terminal_wire.contains("compiled-fingerprint=-"));
        assert!(terminal_wire.contains("leaf-count=2"));
        assert!(terminal_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "terminal" && leaf.focused && leaf.view == terminal_view.get()
        }));
        assert!(terminal_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "native" && !leaf.focused && leaf.view == native_view.get()
        }));
        assert!(app.windows[&wid].settings_card.is_some());
        assert!(
            app.windows[&wid].leaf_render_cache[&native_view]
                .native
                .is_some(),
            "terminal focus still captures the native sibling"
        );

        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(native_view);
        app.sync_window(wid);
        let (native_focused, native_metadata) = capture(&mut app, "native-focused.png");
        assert_eq!(
            (native_focused.0, native_focused.1),
            (terminal_focused.0, terminal_focused.1)
        );
        assert!(native_focused.2.is_some());
        assert_eq!(native_metadata.frame_kind, "composite");
        assert_eq!(native_metadata.view, None);
        assert!(native_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "native" && leaf.focused && leaf.view == native_view.get()
        }));
        assert!(native_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "terminal" && !leaf.focused && leaf.view == terminal_view.get()
        }));
        assert!(
            app.windows[&wid]
                .leaf_render_cache
                .contains_key(&terminal_view),
            "native focus still captures the terminal sibling"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// PHASE-1 LAW (reply shape): the `video` OK line may grow tokens, but ONLY
    /// strictly before the path — the path is ALWAYS the last whitespace token
    /// (the one shape invariant clients rely on). `dropped=` is the TOTAL loss
    /// (ring skips + evictions), `head_truncated=` iff any frame was evicted,
    /// and index.json carries the honest split + the requested/covered window.
    #[test]
    fn video_dump_reply_keeps_path_last_and_reports_honest_coverage() {
        let dir = std::env::temp_dir().join(format!("aterm-video-dump-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let frame = |seq: u64, t_us: u64| aterm_gpu::video_tap::CapturedFrame {
            seq,
            t_us,
            w: 2,
            h: 2,
            rgba: vec![0u8; 16],
        };
        let take = aterm_gpu::video_tap::VideoTake {
            frames: [frame(4, 1_000), frame(5, 2_000)].into(),
            dropped: 1,   // ring skips (mid-stream)
            evicted: 3,   // budget evictions (head truncation)
            decimated: 2, // fps= gate (deliberate, not a loss)
            fps_cap: Some(10),
            budget_bytes: 64 << 20,
            requested_ms: 8_000,
            w: 2,
            h: 2,
            device_px: (4, 4),
            half_res: true,
            format: "rgba8",
            resized_early_stop: false,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        run_encode_job(EncodeJob::VideoDump {
            take,
            mode: crate::VideoMode::SwapchainTap,
            inputs: Vec::new(),
            started_us: 500,
            dir: dir.clone(),
            reply: tx,
        });
        let reply = rx.recv().expect("dump reply");
        assert!(reply.starts_with("OK "), "reply: {reply}");
        let toks: Vec<&str> = reply.split_whitespace().collect();
        assert_eq!(
            toks.last().copied(),
            Some(dir.join("index.json").display().to_string().as_str()),
            "the path must remain the LAST whitespace token"
        );
        assert!(toks.contains(&"frames=2"), "reply: {reply}");
        assert!(
            toks.contains(&"dropped=4"),
            "dropped stays the TOTAL loss (1 ring + 3 evicted): {reply}"
        );
        let ht = toks.iter().position(|t| *t == "head_truncated=true");
        assert!(
            ht.is_some_and(|i| i < toks.len() - 1),
            "head_truncated goes strictly BEFORE the path: {reply}"
        );
        let index = std::fs::read_to_string(dir.join("index.json")).expect("index.json written");
        for key in [
            "\"head_truncated\": true",
            "\"evicted_frames\": 3",
            "\"ring_skipped\": 1",
            "\"decimated_frames\": 2",
            "\"requested_ms\": 8000",
            "\"covered_us\": [1000, 2000]",
            "\"fps_cap\": 10",
            "\"budget_mib\": 64",
            // The honesty label: a swapchain recording says so, with the
            // photon-real stamp semantics.
            "\"mode\": \"swapchain-tap\"",
            "\"stamp_semantics\": \"CPU time frame.present() returned; photons follow by <= 1 vsync\"",
        ] {
            assert!(index.contains(key), "index.json missing {key}:\n{index}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real headless `image` capture must advance the same feline overlay
    /// path as an on-glass present. This pins the complete seam: terminal row
    /// scan, focused phase start, bounded cat bake, free-atlas publication,
    /// and collection observation.
    #[test]
    fn headless_capture_emits_and_collects_an_eligible_cat() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let t0 = Instant::now();
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            // The live `Wake::Output` arm records this before a headless image
            // can run; direct Terminal injection in this test models that edge.
            ws.pending_deco_birth = Some(t0);
            let mut term = crate::term_lock(&terminal);
            term.process(b"\r\n\r\n\r\nhello kitty friend");
            // `render_image` performs this extraction immediately before the
            // splice; keep the regression's framebuffer path equally literal.
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }

        // A control client is allowed to inspect long after the output wake.
        // No output-driven frame existed in between, so the first requested
        // image must still present a fully-risen cat instead of aging the
        // never-seen one-shot straight through Done.
        let first_capture = t0 + Duration::from_secs(15);
        app.splice_word_decorations(wid, first_capture);

        {
            let ws = &app.windows[&wid];
            assert!(
                !ws.input_scratch.free_sprites.is_empty(),
                "first headless capture after output includes the peeking cat"
            );
            assert!(
                ws.input_scratch.free_atlas.is_some(),
                "a visible cat must publish its atlas"
            );
        }
        assert!(
            app.kitty_log.companion_look().is_some(),
            "the same visible sprite must be observed as a collectible"
        );
        assert_eq!(app.kitty_log.log().sightings, 1);

        // Discovery starts the guaranteed cursor hello. A second REQUESTED
        // windowless capture shows the bounded static pose even after far more
        // than the ordinary hold; no timer or autonomous frame loop existed to
        // present it in between.
        app.splice_word_decorations(wid, first_capture + Duration::from_secs(30));
        let mut input = app.windows[&wid].input_scratch.clone();
        let cursor_sprite = input
            .free_sprites
            .iter()
            .find(|sprite| sprite.z == aterm_core::render::FreeZ::OverText)
            .expect("collection hello emits the cursor companion over text");
        assert_eq!(
            cursor_sprite.alpha, 255,
            "windowless collection hello is one full-opacity static pose"
        );
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "a no-damage capture continues the episode without recollecting it"
        );

        // Pixel-level pin: removing only the over-text companion changes the
        // same CPU frame `image` renders, while the peeking word cat remains.
        let mut without_cursor = input.clone();
        without_cursor
            .free_sprites
            .retain(|sprite| sprite.z != aterm_core::render::FreeZ::OverText);
        let with_cursor = app
            .backend
            .render_input(&mut app.introspect_gpu, &mut input, None);
        let without_cursor =
            app.backend
                .render_input(&mut app.introspect_gpu, &mut without_cursor, None);
        assert_ne!(
            with_cursor.pixels, without_cursor.pixels,
            "cursor companion must land in the captured framebuffer"
        );

        // A later no-damage capture advances the original one-shot; it does
        // not synthesize another preview, replay the word cat, or recollect it.
        app.splice_word_decorations(wid, first_capture + Duration::from_secs(40));
        assert!(
            app.windows[&wid]
                .input_scratch
                .free_sprites
                .iter()
                .all(|sprite| sprite.z != aterm_core::render::FreeZ::UnderText),
            "the preview is created only by a damaged rescan"
        );
        assert_eq!(app.kitty_log.log().sightings, 1);
    }

    /// Capture is silent, but it must not discard the cursor companion's
    /// visual curse reaction while draining the shared cue queue.
    #[test]
    fn headless_capture_preserves_complete_curse_wince_for_next_frame() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let t0 = Instant::now();
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.cursor_cat
                .on_collect(t0, aterm_effects::kitty_registry::KittyLook::default());
            ws.cursor_cat.set_collection_presentable(t0, false);
            ws.pending_deco_birth = Some(t0);
            let mut term = crate::term_lock(&terminal);
            term.process(b"fuck");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }

        let capture_at = t0 + Duration::from_millis(16);
        app.splice_word_decorations(wid, capture_at);
        let frame = app
            .windows
            .get_mut(&wid)
            .expect("window 0")
            .cursor_cat
            .static_frame(capture_at + Duration::from_millis(1));
        assert_eq!(
            frame.reaction,
            aterm_effects::nyan_cursor::CatReaction::Wince,
            "the silent capture drain must retain the visual wince"
        );
        assert_eq!(
            frame.pose,
            aterm_effects::nyan_cursor::CatPose::STILL,
            "headless reduced-motion capture keeps the wince expression static"
        );
    }

    /// A capture snapshots the terminal before it enters the decoration
    /// splice. PTY output can arrive between those lock scopes while the
    /// existing damage epoch is already latched, so an epoch comparison alone
    /// cannot reveal the stale cells. The splice must re-extract and rescan one
    /// coherent snapshot or the new word and its collectible are lost.
    #[test]
    fn capture_rescan_refreshes_cells_after_snapshot_race() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let now = Instant::now();
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            let mut term = crate::term_lock(&terminal);
            term.process(b"ordinary text");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);

            // Model output arriving after `render_image` dropped this lock.
            // Keep the prior damage session outstanding deliberately: the
            // damage epoch may remain equal even though the content changed.
            ws.pending_deco_birth = Some(now);
            term.process(b"\r\n\r\n\r\nhello kitty friend");
        }

        app.splice_word_decorations(wid, now);

        let ws = &app.windows[&wid];
        assert!(
            !ws.input_scratch.free_sprites.is_empty(),
            "the splice rescans the post-snapshot word from fresh cells"
        );
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "the fresh word's collectible is not consumed against stale cells"
        );
    }

    #[test]
    fn capture_birth_stamp_is_bounded_and_headless_preview_is_stable() {
        let now = Instant::now();
        assert_eq!(bounded_capture_birth(now, None), None);
        assert_eq!(
            bounded_capture_birth(now, Some(now + Duration::from_secs(1))),
            Some(now),
            "a future stamp sanitizes to capture time"
        );
        assert_eq!(
            now.duration_since(
                bounded_capture_birth(now, Some(now - Duration::from_secs(60)))
                    .expect("bounded stamp"),
            ),
            CAPTURE_BIRTH_MAX_AGE,
            "stale stamps clamp to the episode-retention horizon"
        );
        assert_eq!(
            now.duration_since(capture_rescan_birth(
                now,
                Some(now - Duration::from_secs(60)),
                true,
            )),
            CAPTURE_PREVIEW_AGE,
            "windowless first presentation samples a visible pose, not wall time"
        );
        assert_eq!(
            now.duration_since(capture_rescan_birth(
                now,
                Some(now - Duration::from_secs(6)),
                false,
            )),
            CAPTURE_PREVIEW_AGE,
            "an occluded window's capture-first presentation cannot age unseen past Done"
        );
        assert_eq!(
            now.duration_since(capture_rescan_birth(
                now,
                Some(now - Duration::from_millis(100)),
                false,
            )),
            Duration::from_millis(100),
            "a genuinely fresh windowed capture preserves its output timing"
        );
    }

    /// Sparkle-words v3 §1.1 fix #2 (adversarial-review regression): an
    /// introspection capture during a `perf_reduced` freeze must FREEZE the
    /// engine (never `reset()` it), skip the rescan/tick, and let recovery
    /// resume every episode where it paused — a capture mid-suspension used
    /// to grace-expire and done-mark every episode (and the suppressed-alt
    /// branch reset the engine wholesale).
    #[test]
    fn capture_during_freeze_preserves_and_resumes_episodes() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let t0 = Instant::now();
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            let mut term = crate::term_lock(&terminal);
            term.process(b"a happy kitty naps");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }
        app.splice_word_decorations(wid, t0);
        assert!(
            app.windows[&wid].word_decos.is_active(t0),
            "the fixture's feline effect is animating (sparkle defaults on)"
        );
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.cursor_cat
                .on_collect(t0, aterm_effects::kitty_registry::KittyLook::default());
            ws.cursor_cat.set_collection_presentable(t0, false);
        }
        // Load-shed latch: captures during the suspension freeze the engine
        // and emit cleared channels (mirrors app_render's deco_suspend arm).
        app.perf_reduced = true;
        app.splice_word_decorations(wid, t0 + Duration::from_millis(100));
        {
            let ws = &app.windows[&wid];
            assert!(ws.input_scratch.word_decorations.is_empty());
            assert!(ws.input_scratch.ink.is_empty());
            assert!(ws.input_scratch.free_sprites.is_empty());
            assert!(ws.input_scratch.nova_add.is_empty());
            assert_eq!(
                ws.cursor_cat
                    .static_deadline(t0 + Duration::from_millis(100)),
                None,
                "a suppressed capture keeps the collection hello paused"
            );
        }
        // 20 s of suspension (> the 10 s grace TTL) with new output landing
        // mid-freeze: the frozen capture must neither rescan nor tick, so no
        // episode grace-expires against the suspended clock.
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            let mut term = crate::term_lock(&terminal);
            term.process(b" zzz");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }
        app.splice_word_decorations(wid, t0 + Duration::from_secs(20));
        assert!(
            app.windows[&wid].input_scratch.word_decorations.is_empty(),
            "still suspended: cleared channels"
        );
        assert_eq!(
            app.windows[&wid]
                .cursor_cat
                .static_deadline(t0 + Duration::from_secs(20)),
            None,
            "twenty seconds of suppressed captures consume no hello time"
        );
        // Recovery: the capture thaws (shifting every stored clock by the
        // freeze duration), so the episode resumes ~100 ms into its window —
        // instead of having silently completed (or come back born-done) 20 s
        // later, which is exactly what `is_active == false` would mean here.
        app.perf_reduced = false;
        let t1 = t0 + Duration::from_secs(20) + Duration::from_millis(50);
        app.splice_word_decorations(wid, t1);
        assert!(
            app.windows[&wid].word_decos.is_active(t1),
            "episodes resume where they paused (freeze/thaw, not reset)"
        );
        assert!(
            app.windows[&wid]
                .input_scratch
                .free_sprites
                .iter()
                .any(|sprite| sprite.z == aterm_core::render::FreeZ::OverText),
            "the first drawable capture still presents the paused collection hello"
        );
    }

    /// Sparkle-words v3 §2.1 (adversarial-review fix #4 chain): a sighting
    /// carrying `TRAIT_BOW` increments the ledger's bow counter through the
    /// REAL observe path (the engine side — a Bow-decoding magic word's
    /// sighting carries the bit — is pinned in
    /// `aterm-effects::word_decorations::bow_cat_sighting_carries_trait_bow`).
    #[test]
    fn bow_sighting_increments_the_ledger_counter() {
        use aterm_effects::cat_glyphs_gen::CatGlyphId;
        use aterm_effects::kitty_registry::{
            KittyLook, KittyMagic, KittyShownAs, KittySighting, KittyType, TRAIT_BOW,
        };
        let mut host = crate::kitty_log::KittyLogHost::in_memory();
        let lexicon = aterm_lexicon::Lexicon::with_languages(&["en"]);
        let s = KittySighting {
            kitty_type: KittyType::HeadPeek,
            magic: KittyMagic::None,
            shown_as: KittyShownAs::Cat,
            langs: aterm_lexicon::LangSet::EMPTY,
            traits: TRAIT_BOW,
            look: KittyLook {
                accessory: Some(CatGlyphId::AccBow),
                ..KittyLook::default()
            },
            ident: 0xB0B,
        };
        assert_eq!(host.log().accessory_bow, 0);
        host.observe(1, std::iter::once(s), &lexicon, Instant::now(), true);
        assert_eq!(
            host.log().accessory_bow,
            1,
            "a TRAIT_BOW sighting increments the bow ledger counter"
        );
    }

    /// A dead client (dropped reply receiver) must not panic the worker or wedge
    /// later jobs: the write still lands, the failed send is ignored, and a
    /// subsequent job on the SAME worker still replies.
    #[test]
    fn encode_worker_survives_dropped_reply_receiver() {
        let dir = std::env::temp_dir().join(format!("aterm-encode-dead-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let (dead_tx, dead_rx) = std::sync::mpsc::channel();
        drop(dead_rx);
        app.submit_encode_job(EncodeJob::Image {
            frame: Frame {
                width: 1,
                height: 1,
                pixels: vec![0u32; 1],
            },
            target: confined(&dir, "dead.png"),
            want_bytes: false,
            reply: dead_tx,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        app.submit_encode_job(EncodeJob::Image {
            frame: Frame {
                width: 2,
                height: 2,
                pixels: vec![0u32; 4],
            },
            target: confined(&dir, "live.png"),
            want_bytes: false,
            reply: tx,
        });
        assert_eq!(
            rx.recv().expect("worker alive after dead client"),
            Ok((2, 2, None))
        );
        // The dead client's file was still written (the write precedes the reply).
        assert!(dir.join("dead.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod headless_cursor_fx_tests {
    use super::capture_ticks_cursor_fx;
    use crate::{App, WindowId, term_lock};
    use std::time::{Duration, Instant};

    /// PHASE-1 law ("Headless Present-Real"): a HEADLESS `image` capture ticks
    /// the fire LIVE — a cursor move puts hot quads into the capture's
    /// `cursor_glow_add` — and a much-later capture with no further motion
    /// composes them back to EXACTLY empty (the engines' idle-zero decay law,
    /// observed through the same capture seam `render_image` drives).
    #[test]
    fn headless_capture_ticks_fire_live_then_decays_to_zero() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true); // master switch (default OFF)
        app.config.cursor_trail_style = Some("fire".into());
        let wid = WindowId(0);
        let t0 = Instant::now();
        // Capture 1: primes the engine's last-seen cursor cell (no motion yet).
        app.splice_cursor_fx(wid, t0);
        // A keystroke echoes: the cursor advances one cell.
        {
            let terminal = app.front_terminal(wid).expect("front terminal");
            term_lock(&terminal.term).process(b"x");
        }
        // Capture 2, one frame later: the observed move must spawn live fire.
        app.splice_cursor_fx(wid, t0 + Duration::from_millis(16));
        {
            let ws = app.windows.get(&wid).expect("headless window 0");
            assert!(
                !ws.input_scratch.cursor_glow_add.is_empty(),
                "a headless capture right after a cursor move carries live fire quads"
            );
        }
        // Capture 3, >30 s later with no further motion: every thermal
        // integrator (heat/flare/coal/quench + spark TTLs + the crown window)
        // has decayed and the capture composes ZERO quads again.
        app.splice_cursor_fx(wid, t0 + Duration::from_secs(31));
        let ws = app.windows.get(&wid).expect("headless window 0");
        assert!(
            ws.input_scratch.cursor_glow_add.is_empty(),
            "31 s idle: the fire must decay to exactly zero quads in the capture"
        );
        // v0.31 ("the cursor color starts green" → warm it): the fire block
        // cursor rests as a DULL WARM EMBER, never the theme's accent, so the
        // forge fill settles to a constant ember (Some) — NOT None — at idle. The
        // idle-zero law it must still honour is the ANIMATED fire (the quads
        // above) decaying to nothing; the rest ember is a static, u8-quantized
        // override that rides `cursor_fill_override` WITHOUT churning the fp
        // (the fold is temp-gated), so the present still dedups at rest.
        let ember = ws
            .input_scratch
            .cursor_fill_override
            .expect("31 s idle: the fire cursor rests as a warm ember, not the theme fill");
        let (r, b) = ((ember >> 16) & 0xFF, ember & 0xFF);
        assert!(
            r > b,
            "the rest cursor is warm metal (r > b), got {ember:#08x}"
        );
    }

    /// The PHASE-1 honesty gate BOTH ways: a glass-less capture may tick the
    /// engines (the capture is that window's only present); a WINDOWED capture
    /// never does — it must show LAST-PRESENT state, because ticking would
    /// compose effects NEWER than the glass (a WYSIWYG violation). PHASE-3
    /// adds the ONE-CLOCK-OWNER gate: while a recording targets the window its
    /// offscreen present loop owns the engine tick, so a concurrent `image`
    /// keeps the loop's last-present quads exactly like a windowed capture.
    #[test]
    fn windowed_capture_never_ticks_cursor_fx() {
        assert!(
            capture_ticks_cursor_fx(false, false),
            "no glass, no recording: the capture is the window's only present — tick live"
        );
        assert!(
            !capture_ticks_cursor_fx(true, false),
            "glass: the capture must keep the last-present quads"
        );
        assert!(
            !capture_ticks_cursor_fx(false, true),
            "recording in flight: the offscreen present loop owns the engine clock"
        );
        assert!(
            !capture_ticks_cursor_fx(true, true),
            "glass + recording: still never tick at capture time"
        );
    }
}
