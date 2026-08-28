// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The application-render client-frame introspection path.
//! `snapshot` (SIGUSR1 PNG+txt) and `render_image` (the control `image` verb)
//! capture the CURRENT app-owned route (terminal, native, or heterogeneous,
//! including host chrome/effects). A windowed SIGUSR1 snapshot copies the exact
//! successful application-present client destination, including swapchain-only
//! transforms and remainder bands. Headless capture has no presentation target
//! and is explicitly a semantic-renderer artifact. `read_native_chrome` and
//! `capture_window` form the distinct OS-chrome boundary; full-window capture
//! binds that chrome to a successful application-present client destination.
//! None of these client-frame paths claims compositor visibility or scanout.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_render::Frame;

use crate::WindowId;
#[cfg(test)]
use crate::app_render::sync_cursor_effect_scroll;
use crate::app_render::{
    OverlayGlow, apply_bell_invert, apply_drop_overlay, apply_host_chrome_at, apply_overlay_at,
    composite_tray_quad_at, prepare_resident_pet_tick, sync_cursor_effect_coordinate_space,
    tray_quad_below_y,
};
use crate::control::{DimsSnapshot, ImageReq};
use crate::platform::AppRt;
#[cfg(test)]
use crate::term_lock;
use crate::{App, accessibility, control_auth, snapshot_path};

/// Headless capture may arrive long after the output wake that created a word.
/// Ten seconds is the effects engine's episode-retention horizon: older stamps
/// are equivalent for every bounded one-shot, and clamping avoids carrying an
/// arbitrary process-lifetime duration into the reconstruction tick.
const CAPTURE_BIRTH_MAX_AGE: Duration = Duration::from_secs(10);

/// When capture is the first presentation, sample a feline episode at the end
/// of its 450 ms rise: the authored cat is fully visible, the sighting is real
/// (pixels landed), and no timer or synthetic frame loop is introduced between
/// capture requests. A still-pending output stamp proves the application
/// presentation accounting has not consumed it yet, even for a windowed
/// surface whose redraw was occluded.
const CAPTURE_PREVIEW_AGE: Duration = Duration::from_millis(450);

/// One terminal capture authorized by an application present submission that
/// completed in the same synchronous main-thread call. The owned input is cloned only for
/// an explicit capture (never on the present hot path), before any terminal,
/// layout, cursor-effect, resize, or DPI state can be staged again.
struct PresentedTerminalCapture {
    input: aterm_render::RenderInput,
    grid: crate::app_render::TerminalCaptureGrid,
    invert: bool,
    overlay: Option<OverlayGlow>,
    serial: u64,
    cell_size: (usize, usize),
    theme_fingerprint: u64,
    overlay_fingerprint: u64,
}

#[derive(Clone, Copy)]
struct PresentedTerminalAuthority {
    invert: bool,
    overlay: Option<OverlayGlow>,
    serial: u64,
    cell_size: (usize, usize),
    theme_fingerprint: u64,
    overlay_fingerprint: u64,
}

/// Route-neutral owned projection of one application present submission. Native and
/// heterogeneous captures have no all-terminal leaf inventory, but they share
/// the same serial, pixel input, host effects, and renderer geometry authority.
#[derive(Clone)]
struct PresentedFrameCapture {
    input: aterm_render::RenderInput,
    invert: bool,
    overlay: Option<OverlayGlow>,
    serial: u64,
}

/// One recording-owned successful-present tuple resolved against the GPU's
/// still-resident input epoch. Every validation happens before the input clone;
/// callers then own an immutable model while this main-thread request renders.
struct RecordingPresentedFrame {
    frame: PresentedFrameCapture,
    terminal_grid: Option<crate::app_render::TerminalCaptureGrid>,
    cell_size: (usize, usize),
    theme_fingerprint: u64,
    overlay_fingerprint: u64,
    native_metadata: Option<crate::control::ImageFrameMetadata>,
}

/// Exact pixels of the current persistent headless destination, bound to the
/// recording's last successful application-present tuple. This is deliberately
/// independent of the semantic resident model: later renderer/card state may
/// change while these already-presented bytes remain authoritative.
struct RecordingPresentedDestination {
    frame: Frame,
    route: crate::VisibleContentRoute,
    serial: u64,
}

/// Full-window capture authority produced by one successful surface
/// transaction: the serial proves ordering, while `client` is the exact raw
/// destination copied from that same CPU softbuffer or GPU swapchain present.
struct PresentedWindowCapture {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    serial: u64,
    client: crate::PresentedClientFrame,
    frame: PresentedFrameCapture,
}

/// One `image` request already resolved to the window it renders, on its way to
/// the compiled native/heterogeneous route.
///
/// Every field but `front` and `presented` is carried verbatim from [`ImageReq`]
/// and documented there. They travel as one value because they ARE one request:
/// `render_image` only chooses the route and forwards them, so nothing here may
/// be re-derived or reordered on the way through.
struct NativeImageRequest<'a> {
    /// The window `render_image` resolved — the frontmost one, or the window
    /// whose active tab displays a cross-session `@<sid> image`.
    front: WindowId,
    clean: bool,
    /// The successful application present this capture is bound to. `None` means
    /// no present authority exists, so the route stages its own input scratch.
    presented: Option<PresentedFrameCapture>,
    /// Success-time template carried only by a headless recording artifact.
    /// Windowed synchronous captures build metadata immediately from the same
    /// successful-present turn and therefore leave this `None`.
    presented_metadata: Option<crate::control::ImageFrameMetadata>,
    /// Exact current VirtualTarget pixels for a non-clean headless recording
    /// image. Semantic `presented` data may still supply metadata, but these
    /// bytes bypass every reraster and host-chrome recomposition step.
    exact_frame: Option<Frame>,
    target: crate::control_auth::ConfinedImage,
    want_bytes: bool,
    want_metadata: bool,
    cancel: crate::control::CaptureCancellation,
    frame_metadata: &'a std::sync::Arc<std::sync::OnceLock<crate::control::ImageFrameMetadata>>,
    reply: std::sync::mpsc::Sender<crate::control::ImageReply>,
}

/// Convert an exact straight-RGBA client destination into the renderer Frame
/// container used by the snapshot encoder without changing dimensions or bytes.
fn snapshot_frame_from_presented_client(
    client: &crate::PresentedClientFrame,
) -> Result<Frame, String> {
    let width = usize::try_from(client.width)
        .map_err(|_| "snapshot destination width does not fit memory".to_string())?;
    let height = usize::try_from(client.height)
        .map_err(|_| "snapshot destination height does not fit memory".to_string())?;
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "snapshot destination dimensions overflow".to_string())?;
    if client.rgba.len() != expected {
        return Err(format!(
            "snapshot destination has {} bytes, expected {expected}",
            client.rgba.len()
        ));
    }
    let pixels = client
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|rgba| {
            (u32::from(255 - rgba[3]) << 24)
                | (u32::from(rgba[0]) << 16)
                | (u32::from(rgba[1]) << 8)
                | u32::from(rgba[2])
        })
        .collect();
    Ok(Frame {
        width,
        height,
        pixels,
    })
}

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

/// Serialize the terminal band of an already-composed capture. Top tab-strip
/// rows are chrome, but split dividers and every pane's visible cells are part
/// of the terminal viewport and remain in the text.
fn terminal_capture_text(
    input: &aterm_render::RenderInput,
    strip_rows: usize,
    cols: usize,
) -> String {
    let start = strip_rows.min(input.cells.len());
    let mut text = String::with_capacity(input.cells.len().saturating_sub(start) * (cols + 1));
    for cells in &input.cells[start..] {
        accessibility::push_visible_row(&mut text, cells, cols);
    }
    text
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
/// worker owns it. Only after the write completes does the worker transfer a
/// guarded result to the control thread; that guard is revalidated and retained
/// through the complete response and explicit client ACK. The client can
/// therefore open the exact file named by a successful reply.
pub(crate) enum EncodeJob {
    /// The `image` verb's fully-composited application framebuffer (every
    /// app-owned splice/overlay is baked in before the `Frame` is moved here). A failed
    /// encode/write replies `Err` so the client is never told `OK` for a file
    /// that does not exist ("OK means the file is on disk" is the protocol
    /// contract). `Ok((0, 0))` stays the render's no-window sentinel.
    Image {
        frame: Frame,
        target: control_auth::ConfinedImage,
        /// `image --bytes`: return the PNG in the reply instead of writing `target`.
        want_bytes: bool,
        cancel: crate::control::CaptureCancellation,
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
        transaction: SnapshotTransaction,
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
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
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
        /// tap copied the swapchain bytes submitted through the WSI present path
        /// (`swapchain-tap`; compositor visibility/scanout remain unobserved) or
        /// the headless virtual target's bytes (`offscreen-present-real`; no
        /// OS presentation target existed). Disclosed verbatim in index.json's
        /// `meta.mode` with mode-matched `stamp_semantics`.
        mode: crate::VideoMode,
        /// Whether the request carried the `keys` flag. Drives the HONESTY
        /// tokens on the reply line: a recording that opted into the ledger
        /// always states `inputs=` and `unlogged_inputs=`, so a driver reading
        /// only the reply can tell "measured zero" from "could not measure".
        keys_enabled: bool,
        inputs: Vec<(u64, crate::VideoInputSample)>,
        /// TOTAL input attempts during this take that could not be put on this
        /// recording's frame clock, and therefore CANNOT be in `inputs`: the
        /// control-thread egresses (see [`crate::unseamed_control_inputs`])
        /// plus `unlogged_other_window`.
        unlogged_inputs: u64,
        /// The subset of `unlogged_inputs` that reached the App input seam but
        /// belonged to a window this recording was not capturing. Kept apart so
        /// `analysis.note` can name the RIGHT cause: "drive the front tab" and
        /// "drive the window being recorded" are different corrections.
        unlogged_other_window: u64,
        started_us: u64,
        dir: crate::control_auth::ConfinedVideoDir,
        reply: std::sync::mpsc::Sender<crate::control::Retained<String>>,
        cancel: crate::VideoCancellation,
        /// Busy/idle acknowledgement for serialization and graceful shutdown.
        permit: crate::VideoExportPermit,
    },
}

/// Reconstruct a snapshot sidecar as a whole pathname. Only tests want this
/// shape: production never names a sidecar this way, because the writer reaches
/// every payload through the pinned directory handle and a single component
/// (`sidecar_name` + [`crate::pinned_dir::PinnedDir`]), which is what keeps the
/// TOCTOU confinement contract. Tests, which just stat what a snapshot
/// published, need the ordinary path.
#[cfg(test)]
fn snapshot_sidecar_path(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    sidecar.into()
}

/// Invalidate the fixed SIGUSR1 completion marker before a new attempt. Absence is
/// success; any other removal failure means an older completed snapshot cannot be
/// distinguished from this attempt, so the new attempt must fail closed.
#[derive(Clone, Debug)]
struct SnapshotTarget {
    path: std::path::PathBuf,
    dir: crate::pinned_dir::PinnedDir,
    png: std::ffi::OsString,
    text: std::ffi::OsString,
    done: std::ffi::OsString,
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotTransaction {
    generation: u64,
    target: SnapshotTarget,
}

/// The generation is private on purpose: every production reader lives in this
/// module and takes the field directly, so nothing outside can invent or hold a
/// generation the fence did not mint. The refinement harness in
/// [`crate::artifact_transaction_conformance`] is the one outside observer, and
/// it only ever reads.
#[cfg(test)]
impl SnapshotTransaction {
    #[must_use]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

fn sidecar_name(file_name: &std::ffi::OsStr, suffix: &str) -> std::ffi::OsString {
    let mut name = file_name.to_os_string();
    name.push(suffix);
    name
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = aterm_digest::Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    hex
}

fn clear_snapshot_completion(target: &SnapshotTarget) -> std::io::Result<()> {
    clear_snapshot_completion_with_sync(target, |dir| dir.sync())
}

fn clear_snapshot_completion_with_sync(
    target: &SnapshotTarget,
    sync: impl FnOnce(&crate::pinned_dir::PinnedDir) -> std::io::Result<()>,
) -> std::io::Result<()> {
    target.dir.remove_file_if_exists(&target.done)?;
    sync(&target.dir)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotCommitPlan {
    Publish,
    DiscardStale,
}

/// Pure shipping decision at the fixed-marker commit point. Only the newest
/// request generation may publish `.done`.
#[must_use]
fn snapshot_commit_plan(job_generation: u64, latest_generation: u64) -> SnapshotCommitPlan {
    if job_generation == latest_generation {
        SnapshotCommitPlan::Publish
    } else {
        SnapshotCommitPlan::DiscardStale
    }
}

type SnapshotGenerations = std::collections::BTreeMap<std::path::PathBuf, u64>;

fn snapshot_generation_fence() -> &'static std::sync::Mutex<SnapshotGenerations> {
    static FENCE: std::sync::OnceLock<std::sync::Mutex<SnapshotGenerations>> =
        std::sync::OnceLock::new();
    FENCE.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// Begin a new fixed-path snapshot generation. Incrementing the generation and
/// clearing `.done` share the same mutex held by worker commit, so once this
/// returns no older worker can publish a marker afterward.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "SnapshotGenerationCommit",
        action = "BeginNew",
        project = "aterm_gui::artifact_transaction_conformance::project_snapshot"
    )
)]
pub(crate) fn begin_snapshot_generation(
    path: &std::path::Path,
) -> Result<SnapshotTransaction, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| "snapshot path has no filename".to_string())?;
    if std::path::Path::new(file_name).components().count() != 1 {
        return Err("snapshot filename is not one path component".to_string());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let dir = crate::pinned_dir::PinnedDir::open_resolved(parent)
        .map_err(|error| format!("could not pin snapshot directory: {error}"))?;
    let canonical_path = dir.path().join(file_name);
    let mut generations = snapshot_generation_fence()
        .lock()
        .map_err(|_| "snapshot generation fence is poisoned".to_string())?;
    let next = generations
        .get(&canonical_path)
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "snapshot generation counter exhausted".to_string())?;
    generations.insert(canonical_path.clone(), next);
    let target = SnapshotTarget {
        path: canonical_path,
        dir,
        png: file_name.to_os_string(),
        text: sidecar_name(file_name, ".txt"),
        done: sidecar_name(file_name, ".done"),
    };
    clear_snapshot_completion(&target)
        .map_err(|error| format!("could not clear stale completion marker: {error}"))?;
    target
        .dir
        .validate_path_identity()
        .map_err(|error| format!("snapshot directory changed during begin: {error}"))?;
    Ok(SnapshotTransaction {
        generation: next,
        target,
    })
}

/// Publish one SIGUSR1 snapshot transaction. `.done` is the sole commit marker
/// and lands only after both payloads completed. Any failed attempt removes its
/// payloads best-effort, leaving no completed artifact for requesters to trust.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "SnapshotGenerationCommit",
        action = "CommitOld",
        project = "aterm_gui::artifact_transaction_conformance::project_snapshot"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "SnapshotGenerationCommit",
        action = "CommitCurrent",
        project = "aterm_gui::artifact_transaction_conformance::project_snapshot"
    )
)]
pub(crate) fn write_snapshot_artifacts(
    frame: &Frame,
    text: &str,
    transaction: &SnapshotTransaction,
) -> Result<(), String> {
    write_snapshot_artifacts_with_hook(frame, text, transaction, || {})
}

/// Transactional implementation with a seam used by the deterministic
/// stale-worker regression. PNG encoding happens outside the generation lock;
/// every shared-path write happens after the generation check and while the
/// lock remains held through `.done`.
fn write_snapshot_artifacts_with_hook(
    frame: &Frame,
    text: &str,
    transaction: &SnapshotTransaction,
    before_done: impl FnOnce(),
) -> Result<(), String> {
    let generation = transaction.generation;
    let target = &transaction.target;
    let png = frame.to_png();

    let cleanup_names = || {
        let _ = target.dir.remove_file_if_exists(&target.png);
        let _ = target.dir.remove_file_if_exists(&target.text);
        let _ = target.dir.remove_file_if_exists(&target.done);
        let _ = target.dir.sync();
    };

    let generations = snapshot_generation_fence()
        .lock()
        .map_err(|_| "snapshot generation fence is poisoned".to_string())?;
    let latest = generations.get(&target.path).copied().unwrap_or(0);
    if snapshot_commit_plan(generation, latest) == SnapshotCommitPlan::DiscardStale {
        // This job has not touched the shared payload paths. In particular, do
        // not clean them: they may already belong to the current generation
        // whose transaction completed while this worker waited for the lock.
        return Err(format!(
            "snapshot generation {generation} was superseded by {latest}"
        ));
    }
    let png_file = target
        .dir
        .write_private(&target.png, &png)
        .map_err(|error| {
            cleanup_names();
            format!("PNG write failed: {error}")
        })?;
    let text_file = match target.dir.write_private(&target.text, text.as_bytes()) {
        Ok(file) => file,
        Err(error) => {
            let _ = png_file.remove_exact();
            let _ = target.dir.remove_file_if_exists(&target.text);
            let _ = target.dir.remove_file_if_exists(&target.done);
            let _ = target.dir.sync();
            return Err(format!("text write failed: {error}"));
        }
    };
    before_done();
    if let Err(error) = png_file
        .validate_path_identity()
        .and_then(|()| text_file.validate_path_identity())
    {
        let _ = png_file.remove_exact();
        let _ = text_file.remove_exact();
        let _ = target.dir.remove_file_if_exists(&target.done);
        let _ = target.dir.sync();
        return Err(format!("payload identity changed before commit: {error}"));
    }
    // The first line preserves the fixed-path compatibility contract. The
    // remaining fields let a poller that needs adversarial same-uid integrity
    // bind the marker to this exact generation and payload bytes; mere
    // existence remains only a writer-completion signal, not protection
    // against mutation by another process after commit.
    let done = format!(
        "{}x{}\ngeneration={generation}\npng_sha256={}\ntext_sha256={}\n",
        frame.width,
        frame.height,
        sha256_hex(&png),
        sha256_hex(text.as_bytes())
    );
    let done_file = match target.dir.write_new_private(&target.done, done.as_bytes()) {
        Ok(file) => file,
        Err(error) => {
            let _ = png_file.remove_exact();
            let _ = text_file.remove_exact();
            let _ = target.dir.remove_file_if_exists(&target.done);
            let _ = target.dir.sync();
            return Err(format!("completion marker write failed: {error}"));
        }
    };
    // `write_new_private` has already fsync'd the marker and containing
    // directory and validated both exact identities. Do not add a fallible
    // post-marker phase: once `.done` becomes externally observable this
    // transaction is committed. The guards live through this return.
    let _commit_guards = (png_file, text_file, done_file);
    Ok(())
}

/// The HONESTY tokens a `video ... keys` recording adds to its OK reply line.
///
/// A measuring instrument that reports zero must not be indistinguishable from
/// one that could not measure. When the take opted into the ledger, the reply
/// always states BOTH counts — `inputs=` (attempts this recording timestamped)
/// and `unlogged_inputs=` (attempts that took the control-thread path this
/// ledger structurally cannot see) — so the driver learns at recording time,
/// from the reply itself, whether an empty `inputs[]` means "nothing happened"
/// or "it happened somewhere I cannot watch". A take WITHOUT `keys` recorded no
/// ledger at all and says nothing rather than implying a measured zero.
///
/// PURE, and the leading space keeps the client invariant that the recording
/// path is always the LAST whitespace token of the reply.
pub(crate) fn video_keys_reply_tokens(keys_enabled: bool, logged: usize, unlogged: u64) -> String {
    if keys_enabled {
        format!(" inputs={logged} unlogged_inputs={unlogged}")
    } else {
        String::new()
    }
}

/// The same disclosure for the MID-FLIGHT `video status` line.
///
/// The finalized reply arrives only when the take is over, which is too late to
/// change how the take is driven. A driver that polls `video status` after its
/// first few keys must be able to see, right then, that the ledger is empty
/// BECAUSE its verbs are taking the unobservable path — so `keys=true inputs=0
/// unlogged=3` is the whole warning, delivered while the recording can still be
/// saved. A take without `keys` says `keys=false` rather than printing counts
/// that would read as a measured zero.
///
/// PURE. `unlogged` is deliberately spelled without the `_inputs` suffix the
/// finalized reply uses: this is a running subtotal of a live take, not the
/// take's published figure, and one token name for two different things would
/// invite a parser to treat them as interchangeable.
pub(crate) fn video_keys_status_tokens(keys_enabled: bool, logged: usize, unlogged: u64) -> String {
    if keys_enabled {
        format!(" keys=true inputs={logged} unlogged={unlogged}")
    } else {
        " keys=false".to_string()
    }
}

/// The `analysis.note` for a recording whose `inputs[]` came out EMPTY.
///
/// PURE so the three genuinely different situations can be pinned by a unit
/// test. They must never share a sentence: the old note guessed at three causes
/// at once (and named `send`/`feed` as an unloggable path, which the active-tab
/// arms no longer are), so a driver could not tell an idle screen from a
/// misrouted one.
pub(crate) fn video_empty_ledger_note(
    keys_enabled: bool,
    unlogged: u64,
    unlogged_other_window: u64,
) -> String {
    if !keys_enabled {
        return "no keystroke ledger was recorded: this take did not pass the `keys` flag. \
                Re-record as `video <secs> keys` (owner scope) to correlate input with frames."
            .to_string();
    }
    if unlogged > 0 {
        // The two causes need DIFFERENT corrections, so the note names whichever
        // actually happened (and both when both did) rather than blaming the
        // control-thread path for a key that simply went to another window.
        let control_thread = unlogged.saturating_sub(unlogged_other_window);
        let mut causes = String::new();
        if control_thread > 0 {
            causes.push_str(&format!(
                " {control_thread} of them reached a PTY on the CONTROL-THREAD path — a verb \
                 aimed at a session that is NOT the tab on screen (`@<sid>` naming a background \
                 tab, which is what `@self` expands to when the driving session is not front). A \
                 BACKGROUND target has no active tab to touch, so it never enters the App input \
                 seam and this ledger cannot timestamp it. Drive the tab that is ON \
                 SCREEN — flagless `key`/`ctrl`/`send`/`feed`/`paste`, or an explicit `@<sid>` \
                 naming the front tab, both of which ARE logged."
            ));
        }
        if unlogged_other_window > 0 {
            causes.push_str(&format!(
                " {unlogged_other_window} of them DID pass the App input seam but arrived on a \
                 WINDOW this recording was not capturing (the front window changed during the \
                 take — an `aterm ctl spawn` alone does it). No frame in frames[] can answer \
                 those keys, so logging them would have published a key->frame latency for a key \
                 that never touched the recorded surface. Re-record with the target window front \
                 for the whole take."
            ));
        }
        return format!(
            "MEASUREMENT GAP, not silence: {unlogged} input attempt(s) happened during this take \
             that this ledger could not stamp on its own frame clock, so inputs[] is empty DESPITE \
             them.{causes}"
        );
    }
    "no input reached this instance during the take: the ledger measured zero, it did not fail \
     to measure (unlogged_inputs is 0, so nothing took the unobservable control-thread path and \
     nothing landed on another window either). Active-tab `key`/`ctrl`/`send`/`feed`/`paste` and \
     hardware keys on the recorded window are all logged."
        .to_string()
}

#[cfg(test)]
mod video_keys_honesty_tests {
    use super::{video_empty_ledger_note, video_keys_reply_tokens, video_keys_status_tokens};

    /// The finalized reply comes too late to change how a take is DRIVEN, so the
    /// mid-flight `video status` line must carry the same disclosure. A driver
    /// that polls two seconds into a thirty-second take and reads `inputs=0
    /// unlogged=5` learns, while the take is still salvageable, that its verbs
    /// are taking the path this ledger cannot watch.
    #[test]
    fn status_line_discloses_the_running_ledger_mid_flight() {
        assert_eq!(
            video_keys_status_tokens(true, 0, 5),
            " keys=true inputs=0 unlogged=5",
            "a live take driven down the unobservable path must say so DURING the take"
        );
        assert_eq!(
            video_keys_status_tokens(true, 9, 0),
            " keys=true inputs=9 unlogged=0"
        );
        // No ledger was requested: say that, rather than printing an
        // `inputs=0` that reads as a measured zero.
        assert_eq!(video_keys_status_tokens(false, 0, 0), " keys=false");
        assert!(
            !video_keys_status_tokens(false, 0, 0).contains("inputs="),
            "a take without `keys` must not publish a count it never took"
        );

        // The status line stays one whitespace-token line with no path to
        // protect, so the tokens simply append cleanly.
        let line = format!(
            "OK recording=true mode=swapchain-tap elapsed_ms=1200 frames=71 resized=false{}",
            video_keys_status_tokens(true, 3, 0)
        );
        assert!(line.ends_with(" keys=true inputs=3 unlogged=0"), "{line}");
    }

    /// A measuring instrument that reports zero must never be indistinguishable
    /// from one that could not measure. The three situations behind an empty
    /// `inputs[]` are genuinely different and must never share a sentence.
    #[test]
    fn empty_ledger_note_separates_measured_zero_from_unmeasurable() {
        // (1) The take never asked for a ledger. Saying "no input attempts
        // logged" here implied a measurement that was never attempted.
        let no_flag = video_empty_ledger_note(false, 0, 0);
        assert!(
            no_flag.contains("`keys` flag"),
            "a take without `keys` must say so: {no_flag}"
        );
        assert!(
            !no_flag.contains("measured zero"),
            "an unrequested ledger never measured anything: {no_flag}"
        );

        // (2) Input DID happen, on the path this ledger structurally cannot
        // observe. The count is stated, so the reader is never left guessing.
        let gap = video_empty_ledger_note(true, 4, 0);
        assert!(
            gap.contains("MEASUREMENT GAP") && gap.contains('4'),
            "a bypassed drive must be named AND counted: {gap}"
        );
        assert!(
            gap.contains("@<sid>"),
            "the note must name the path that bypassed it: {gap}"
        );
        assert!(
            !gap.contains("WINDOW"),
            "no attempt landed on another window; the note must not invent that cause: {gap}"
        );

        // (2b) The OTHER unobservable cause: the attempts passed the App input
        // seam but belonged to a window this take was not capturing. It needs a
        // DIFFERENT correction from the background-target one ("re-record with
        // the target window front", not "drive the front tab"), so the note must
        // name it — and must not blame the control-thread path it never took.
        let other_window = video_empty_ledger_note(true, 3, 3);
        assert!(
            other_window.contains("MEASUREMENT GAP") && other_window.contains('3'),
            "a foreign-window drive must be named AND counted: {other_window}"
        );
        assert!(
            other_window.contains("WINDOW this recording was not capturing"),
            "the note must name the window cause: {other_window}"
        );
        assert!(
            !other_window.contains("CONTROL-THREAD"),
            "nothing took the control-thread path; blaming it misdirects the fix: {other_window}"
        );
        // Both causes at once: 5 total, 2 of them foreign-window, so 3 took the
        // control thread. Both are named with their own count.
        let both = video_empty_ledger_note(true, 5, 2);
        assert!(
            both.contains("CONTROL-THREAD") && both.contains("3 of them"),
            "the control-thread share must be derived and stated: {both}"
        );
        assert!(
            both.contains("2 of them") && both.contains("WINDOW"),
            "the foreign-window share must be stated too: {both}"
        );

        // (3) A genuine zero — and it says so positively, citing the evidence
        // (`unlogged_inputs` is 0) rather than guessing at causes.
        let quiet = video_empty_ledger_note(true, 0, 0);
        assert!(
            quiet.contains("measured zero"),
            "a real zero must be claimed as a measurement: {quiet}"
        );
        assert!(
            !quiet.contains("MEASUREMENT GAP"),
            "a real zero is not a gap: {quiet}"
        );

        // The stale advice is gone from every branch: flagless `send`/`feed` now
        // reach the App input seam and ARE logged, so no note may still tell a
        // driver they "write to the PTY directly and are not input attempts".
        for note in [&no_flag, &gap, &other_window, &both, &quiet] {
            assert!(
                !note.contains("are not input attempts"),
                "the send/feed exclusion is obsolete: {note}"
            );
            // The note is interpolated RAW into index.json's `"note": "…"`, so a
            // quote or backslash in any branch would emit a broken artifact —
            // the instrument destroying the very evidence it exists to publish.
            assert!(
                !note.contains('"') && !note.contains('\\'),
                "a note must stay JSON-safe unescaped: {note}"
            );
        }
    }

    /// The reply line is what a driver reads AT RECORDING TIME. A `keys` take
    /// always states both counts there; a take without `keys` adds nothing (it
    /// would otherwise imply a measured zero). The recording path stays the last
    /// whitespace token — the one client invariant of this reply shape.
    #[test]
    fn reply_tokens_state_both_counts_and_keep_the_path_last() {
        assert_eq!(
            video_keys_reply_tokens(true, 0, 3),
            " inputs=0 unlogged_inputs=3",
            "zero logged with three bypassed must be visible on the reply itself"
        );
        assert_eq!(
            video_keys_reply_tokens(true, 7, 0),
            " inputs=7 unlogged_inputs=0"
        );
        assert_eq!(
            video_keys_reply_tokens(false, 0, 0),
            "",
            "a take without `keys` must not imply a measured zero"
        );
        let line = format!(
            "OK frames=10 dropped=0 head_truncated=false{} /rec/index.json",
            video_keys_reply_tokens(true, 2, 0)
        );
        assert_eq!(
            line.split_whitespace().last(),
            Some("/rec/index.json"),
            "the path must remain the last whitespace token"
        );
    }
}

/// Abort a video export without leaving an `index.json` completion artifact.
/// The recording directory is server-created and unique to this job, so removing
/// it also prevents failed, never-prunable partial PNG sequences from accumulating.
fn fail_video_dump(
    dir: crate::control_auth::ConfinedVideoDir,
    reply: &std::sync::mpsc::Sender<crate::control::Retained<String>>,
    error: impl std::fmt::Display,
) {
    let cleanup = match dir.abort() {
        Ok(()) => String::new(),
        Err(cleanup_error) => format!("; partial export cleanup failed: {cleanup_error}"),
    };
    let _ = reply.send(crate::control::Retained::plain(format!(
        "ERR video: export failed: {error}{cleanup}\n"
    )));
}

/// Own the complete video publication transaction in an unwind-safe drop
/// order. Rust drops fields in declaration order: exact file guards release
/// first, then an unpublished directory capability can recursively clean the
/// tree (required by Windows deny-delete handles). A marker-visible directory
/// is irrevocable and survives later response/ACK failure.
struct VideoPublication {
    frame_files: Vec<crate::pinned_dir::PinnedFile>,
    index_file: crate::pinned_dir::PinnedFile,
    published_marker: Option<crate::pinned_dir::PinnedFile>,
    dir: crate::control_auth::ConfinedVideoDir,
}

impl VideoPublication {
    fn prepare(&mut self) -> std::io::Result<std::path::PathBuf> {
        self.dir.publish(&self.frame_files, &self.index_file)
    }

    fn validate_for_reply(&self) -> std::io::Result<()> {
        self.dir
            .validate_for_reply(&self.frame_files, &self.index_file)?;
        if let Some(marker) = &self.published_marker {
            marker.validate_path_identity()?;
        }
        Ok(())
    }

    fn publish_marker(&mut self) -> std::io::Result<()> {
        let marker = self.dir.publish_marker()?;
        marker.validate_path_identity()?;
        self.published_marker = Some(marker);
        Ok(())
    }

    fn prune_after_publish(&self) {
        self.dir.prune_after_publish();
    }

    fn abort(self) -> std::io::Result<()> {
        let Self {
            frame_files,
            index_file,
            published_marker,
            dir,
        } = self;
        drop(frame_files);
        drop(index_file);
        drop(published_marker);
        dir.abort()
    }
}

fn fail_video_publication(
    publication: VideoPublication,
    reply: &std::sync::mpsc::Sender<crate::control::Retained<String>>,
    error: impl std::fmt::Display,
) {
    let cleanup = match publication.abort() {
        Ok(()) => String::new(),
        Err(cleanup_error) => format!("; partial export cleanup failed: {cleanup_error}"),
    };
    let _ = reply.send(crate::control::Retained::plain(format!(
        "ERR video: export failed: {error}{cleanup}\n"
    )));
}

struct VideoReplyRetention {
    publication: VideoPublication,
    published: bool,
}

impl crate::control::WireRetention for VideoReplyRetention {
    fn prepare_write(&mut self) -> Result<(), String> {
        self.publication
            .validate_for_reply()
            .map_err(|error| format!("video identity changed before wire reply: {error}"))?;
        self.publication
            .publish_marker()
            .map_err(|error| format!("video publish marker failed before wire reply: {error}"))?;
        // Marker publication made the directory non-abortable before visibility.
        // Record that fact before the final identity pass so Drop may run safe
        // lease-aware retention even when the pass detects interference.
        self.published = true;
        self.publication
            .validate_for_reply()
            .map_err(|error| format!("video identity changed at wire reply: {error}"))?;
        Ok(())
    }
}

impl Drop for VideoReplyRetention {
    fn drop(&mut self) {
        if self.published {
            self.publication.prune_after_publish();
        }
    }
}

fn send_video_reply_with_retention(
    publication: VideoPublication,
    reply: &std::sync::mpsc::Sender<crate::control::Retained<String>>,
    value: String,
) {
    // Move every exact file/directory guard with the reply. Wire preparation
    // revalidates the bundle and atomically publishes its marker before any OK
    // byte; retention remains live through the client's explicit ACK. A channel
    // drop before preparation still aborts the invisible tree. Once the marker
    // could have been visible, a later write/ACK failure deliberately preserves
    // the recording and only runs bounded retention.
    let retention = crate::control::ReplyRetention::new(VideoReplyRetention {
        publication,
        published: false,
    });
    let _ = reply.send(crate::control::Retained::guarded(value, retention));
}

struct CaptureReplyRetention {
    target: crate::control_auth::ConfinedImage,
    file: Option<crate::pinned_dir::PinnedFile>,
    _lease: Option<crate::control_auth::ArtifactPathLease>,
    committed: bool,
}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReplyPublication",
        action = "AbortAuthorized",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn abort_authorized_artifact_anchor() {}

impl crate::control::WireRetention for CaptureReplyRetention {
    fn prepare_write(&mut self) -> Result<(), String> {
        self.target
            .validate_for_reply(self.file.as_ref().expect("live capture reply guard"))
            .map_err(|error| format!("capture identity changed before wire reply: {error}"))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for CaptureReplyRetention {
    fn drop(&mut self) {
        if self.committed {
            crate::control_auth::prune_automatic_image_dir(&self.target);
        } else if let Some(file) = self.file.take() {
            let _ = file.remove_exact();
        }
    }
}

fn send_capture_reply_after_validation<T: Send>(
    target: crate::control_auth::ConfinedImage,
    file: crate::pinned_dir::PinnedFile,
    lease: Option<crate::control_auth::ArtifactPathLease>,
    reply: &std::sync::mpsc::Sender<Result<crate::control::Retained<T>, String>>,
    value: T,
    context: &str,
    before_send: impl FnOnce(),
) -> bool {
    if let Err(error) = target.validate_for_reply(&file) {
        abort_authorized_artifact_anchor();
        let _ = file.remove_exact();
        let _ = reply.send(Err(format!(
            "{context} path identity changed before reply: {error}"
        )));
        return false;
    }
    before_send();
    if let Err(error) = target.validate_for_reply(&file) {
        abort_authorized_artifact_anchor();
        let _ = file.remove_exact();
        let _ = reply.send(Err(format!(
            "{context} path identity changed at reply barrier: {error}"
        )));
        return false;
    }
    // Move the file and every directory handle with the reply. The control
    // server retains this bundle through explicit response acknowledgement, so
    // another capture cannot replace/prune the advertised identity before the
    // client consumes OK. The shared namespace advisory lock also excludes
    // same-uid explicit-name replacement across processes.
    let retention = crate::control::ReplyRetention::new(CaptureReplyRetention {
        target,
        file: Some(file),
        _lease: lease,
        committed: false,
    });
    let _ = reply.send(Ok(crate::control::Retained::guarded(value, retention)));
    true
}

/// Encode + confined-write one job, then transfer its guarded result. Runs on
/// the encode worker (or inline when the worker cannot be spawned/reached).
/// The write keeps the TOCTOU confinement contract verbatim: each target owns
/// the directory handles retained when the control thread confined it; the
/// worker never re-opens a multi-segment pathname.
fn run_encode_job(job: EncodeJob) {
    match job {
        EncodeJob::Image {
            frame,
            target,
            want_bytes,
            cancel,
            reply,
        } => {
            let (w, h) = (frame.width as u32, frame.height as u32);
            let png = frame.to_png();
            if want_bytes {
                // `--bytes`: hand the PNG back over the wire; write no file (a remote
                // driver cannot read the server's filesystem).
                if cancel.is_cancelled() {
                    let _ =
                        reply.send(Err("image request cancelled before byte reply".to_string()));
                } else {
                    let _ = reply.send(Ok(crate::control::Retained::plain((w, h, Some(png)))));
                }
                return;
            }
            let lease = match crate::control_auth::acquire_capture_name_lease(&target, || {
                cancel.is_cancelled()
            }) {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let _ = reply.send(Err(
                        "image request cancelled while waiting for its output name".to_string(),
                    ));
                    return;
                }
                Err(error) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let _ = reply.send(Err(format!("image output-name lease failed: {error}")));
                    return;
                }
            };
            match target.write_private_authorized(&png, || cancel.authorize_commit()) {
                Ok(file) => {
                    send_capture_reply_after_validation(
                        target,
                        file,
                        Some(lease),
                        &reply,
                        (w, h, None),
                        "image",
                        || {},
                    );
                }
                Err(error) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let message = if error.kind() == std::io::ErrorKind::Interrupted
                        && cancel.is_cancelled()
                    {
                        "image request cancelled before publication".to_string()
                    } else {
                        format!("image write failed: {error}")
                    };
                    let _ = reply.send(Err(message));
                }
            }
        }
        EncodeJob::Snapshot {
            frame,
            text,
            transaction,
        } => {
            let path = transaction.target.path.display();
            match write_snapshot_artifacts(&frame, &text, &transaction) {
                Ok(()) => eprintln!("aterm-gui: snapshot written to {path} (+ .txt, .done)"),
                Err(error) => eprintln!("aterm-gui: snapshot failed for {path}: {error}"),
            }
        }
        #[cfg(any(target_os = "macos", windows))]
        EncodeJob::WindowRgba {
            rgba,
            width,
            height,
            target,
            cancel,
            reply,
        } => {
            let png = match encode_rgba8_png(&rgba, width, height) {
                Ok(png) => png,
                Err(error) => {
                    let _ = reply.send(Err(format!(
                        "window capture failed (PNG encode error: {error})"
                    )));
                    return;
                }
            };
            let lease = match crate::control_auth::acquire_capture_name_lease(&target, || {
                cancel.is_cancelled()
            }) {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let _ = reply.send(Err(
                        "window capture request cancelled while waiting for its output name"
                            .to_string(),
                    ));
                    return;
                }
                Err(error) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let _ = reply.send(Err(format!(
                        "window capture output-name lease failed: {error}"
                    )));
                    return;
                }
            };
            match target.write_private_authorized(&png, || cancel.authorize_commit()) {
                Ok(file) => {
                    send_capture_reply_after_validation(
                        target,
                        file,
                        Some(lease),
                        &reply,
                        (width, height),
                        "window capture",
                        || {},
                    );
                }
                Err(error) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let message = if error.kind() == std::io::ErrorKind::Interrupted
                        && cancel.is_cancelled()
                    {
                        "window capture request cancelled before publication".to_string()
                    } else {
                        format!("window capture failed (write error: {error})")
                    };
                    let _ = reply.send(Err(message));
                }
            }
        }
        EncodeJob::VideoDump {
            take,
            mode,
            keys_enabled,
            inputs,
            unlogged_inputs,
            unlogged_other_window,
            started_us,
            dir,
            reply,
            cancel,
            permit: _permit,
        } => {
            if cancel.is_cancelled() {
                fail_video_dump(dir, &reply, "request cancelled before export");
                return;
            }
            // A server-named directory should be fresh, but fail closed if a
            // collision/retry left a completion marker: this job may publish only
            // the index it builds after every requested frame is durable.
            for completion in ["index.json", "index.json.tmp"] {
                if let Err(error) = dir.remove_file_if_exists(std::ffi::OsStr::new(completion)) {
                    fail_video_dump(
                        dir,
                        &reply,
                        format!("could not clear stale {completion}: {error}"),
                    );
                    return;
                }
            }
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
            for (t, sample) in &inputs {
                let before = fps.iter().rev().find(|(ft, _)| ft < t);
                let Some((_, ref_fp)) = before else { continue };
                let hit = fps
                    .iter()
                    .filter(|(ft, _)| ft >= t)
                    .find(|(_, fp)| fp.abs_diff(*ref_fp) > 200);
                let field = sample.json_field();
                if !glyph_lines.is_empty() {
                    glyph_lines.push_str(",\n");
                }
                match hit {
                    Some((ft, _)) => {
                        let ms = (ft.saturating_sub(*t)) as f64 / 1000.0;
                        lats_ms.push(ms);
                        glyph_lines.push_str(&format!(
                            "      {{{field},\"t_us\":{t},\"response_ms\":{ms:.1}}}"
                        ));
                    }
                    None => glyph_lines.push_str(&format!(
                        "      {{{field},\"t_us\":{t},\"response_ms\":null}}"
                    )),
                }
            }
            lats_ms.sort_by(|a, b| a.total_cmp(b));
            let analysis = if inputs.is_empty() {
                format!(
                    "  \"analysis\": {{\"note\": \"{}\"}},\n",
                    video_empty_ledger_note(keys_enabled, unlogged_inputs, unlogged_other_window)
                )
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
            // Frames first, index.json last inside the still-private recording.
            // Guarded wire preparation later publishes `.published`, the sole
            // visibility marker readers accept, after revalidating these files.
            let mut frame_lines = String::new();
            let mut written = 0usize;
            let mut frame_files = Vec::with_capacity(take.frames.len());
            // `delta` = how much this frame's sampled fingerprint moved from the
            // previous captured frame. A multimodal AI reads index.json and pulls
            // only the high-`delta` frames (the visually eventful moments) instead
            // of downloading every PNG — the fingerprint is already computed above
            // for the keystroke analysis, so this is free. `prev_fp` tracks the
            // previous frame in capture order. Any encode/write error aborts the
            // whole export, so every adjacent fingerprint also has an adjacent PNG.
            let mut prev_fp: Option<u64> = None;
            for (i, f) in take.frames.iter().enumerate() {
                if cancel.is_cancelled() {
                    drop(frame_files);
                    fail_video_dump(dir, &reply, "request cancelled during export");
                    return;
                }
                let fp = fps.get(i).map_or(0, |&(_, fp)| fp);
                let delta = prev_fp.map_or(0, |p| fp.abs_diff(p));
                prev_fp = Some(fp);
                let name = format!("frame_{:04}.png", i + 1);
                let png = match encode_rgba8_png(&f.rgba, f.w, f.h) {
                    Ok(png) => png,
                    Err(error) => {
                        drop(frame_files);
                        fail_video_dump(
                            dir,
                            &reply,
                            format!("frame {} PNG encode failed: {error}", i + 1),
                        );
                        return;
                    }
                };
                let frame_file = match dir.write_new_private(std::ffi::OsStr::new(&name), &png) {
                    Ok(file) => file,
                    Err(error) => {
                        drop(frame_files);
                        fail_video_dump(
                            dir,
                            &reply,
                            format!("frame {} write failed ({name}): {error}", i + 1),
                        );
                        return;
                    }
                };
                frame_files.push(frame_file);
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
            if cancel.is_cancelled() {
                drop(frame_files);
                fail_video_dump(dir, &reply, "request cancelled before index publish");
                return;
            }
            let mut input_lines = String::new();
            for (t, sample) in &inputs {
                if !input_lines.is_empty() {
                    input_lines.push_str(",\n");
                }
                // `"ch"` for a character-shaped attempt, `"key"` for a named key
                // that has none (esc / arrows / F-keys). Both are JSON-escaped by
                // `json_field`.
                input_lines.push_str(&format!("    {{\"t_us\":{t},{}}}", sample.json_field()));
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
            // (`stamp_semantics`). Swapchain bytes were submitted to present;
            // compositor visibility/scanout is not observable here. An
            // offscreen-present-real recording never had an OS presentation target.
            let mode_str = mode.as_str();
            let stamp_semantics = mode.stamp_semantics();
            let index = format!(
                "{{\n  \"meta\": {{\n    \"w\": {}, \"h\": {}, \"device_px\": [{}, {}],\n    \
                 \"half_res\": {}, \"format\": \"{}\", \"mode\": \"{mode_str}\",\n    \
                 \"clock\": \"metrics now_us; same epoch as inputs[] — attempt->later-frame delta = frame.t_us - input.t_us\",\n    \
                 \"input_semantics\": \"pre-routing attempts (`ch` = character, `key` = named key with no character); not PTY-delivery or visible-glyph receipts\",\n    \
                 \"keys_requested\": {keys_enabled}, \"inputs_logged\": {}, \"unlogged_inputs\": {unlogged_inputs},\n    \
                 \"unlogged_other_window\": {unlogged_other_window},\n    \
                 \"unlogged_input_semantics\": \"input attempts during this take that this recording could not stamp on its own frame clock, from two causes. (1) The CONTROL-THREAD path: a verb aimed at a session that is NOT the tab on screen (`@<sid>` naming a background tab; `@self` when the driving session is not front). A background target has no active tab to touch, so it never enters the App input seam this ledger hooks. (2) `unlogged_other_window`: attempts that DID pass the seam but arrived on a window this take was not capturing — no frame here can answer them, so stamping them would fabricate a key->frame latency. unlogged_inputs is the TOTAL and includes unlogged_other_window. Same unit as inputs_logged (one per would-be inputs[] row); cause (1) is process-wide, delta over this take. Non-zero means attempts happened that inputs[] does NOT contain.\",\n    \
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
                inputs.len(),
                take.dropped,
                take.evicted,
                take.decimated,
                take.resized_early_stop,
                frame_lines,
                input_lines,
            );
            // Build and fsync `index.json` under a private create-new temporary
            // name, then atomically publish it with no-replace rename. A planted
            // final component fails the commit, and readers observe either no
            // index or the complete bounded JSON document—never partial bytes.
            let index_file = match dir.write_new_private_authorized(
                std::ffi::OsStr::new("index.json"),
                index.as_bytes(),
                || cancel.authorize_commit(),
            ) {
                Ok(file) => file,
                Err(error) => {
                    drop(frame_files);
                    let message = if error.kind() == std::io::ErrorKind::Interrupted
                        && cancel.is_cancelled()
                    {
                        "request cancelled before index publish".to_string()
                    } else {
                        format!("index.json write failed: {error}")
                    };
                    fail_video_dump(dir, &reply, message);
                    return;
                }
            };
            let mut publication = VideoPublication {
                frame_files,
                index_file,
                published_marker: None,
                dir,
            };
            let published_dir = match publication.prepare() {
                Ok(path) => path,
                Err(error) => {
                    fail_video_publication(
                        publication,
                        &reply,
                        format!("recording publish failed: {error}"),
                    );
                    return;
                }
            };
            if let Err(error) = publication.validate_for_reply() {
                fail_video_publication(
                    publication,
                    &reply,
                    format!("recording identity changed before reply: {error}"),
                );
                return;
            }
            // Publication ownership crosses before the observable reply. A
            // disconnected client or panic during send leaves the already
            // durable recording retained; it can never receive OK for a tree
            // that Drop subsequently removes.
            // Reply shape: new tokens go strictly BEFORE the path — the path
            // is ALWAYS the last whitespace token (the one client invariant).
            send_video_reply_with_retention(
                publication,
                &reply,
                format!(
                    "OK frames={written} dropped={dropped_total} head_truncated={head_truncated}{} {}\n",
                    video_keys_reply_tokens(keys_enabled, inputs.len(), unlogged_inputs),
                    published_dir.join("index.json").display()
                ),
            );
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
    /// The native Settings tab. `controls prefs` keeps its historical line prefixes,
    /// but its route, preference rows, and additive `ui` records come from the exact
    /// compiled native semantic tree. `inspect app/v1` remains the versioned form.
    Prefs,
    /// The native Settings `/about` route. `controls about` projects the exact compiled
    /// native semantic frame when that route is open.
    About,
    /// The in-window command PALETTE overlay (`WindowState::palette`) — an own-rendered card
    /// on the FRONT window, captured by the front `image`/`window` path. `controls menu`
    /// serialises its (filtered) command rows. The single source of truth for the menu.
    Menu,
    /// The native Settings `/updates` route. `controls update` projects the exact compiled
    /// native semantic frame when that route is open.
    Update,
    /// The IN-GRID session context menu overlay (`WindowState::tab_menu`, design
    /// §2.1 [v5]) — the second renderer of the composed tab-menu model. `controls
    /// tab-menu` serialises the OPEN card's rows + cursor (item text verbatim the
    /// `chrome` verb's per-tab mirror); closed reports `tab-menu open=false`.
    TabMenu,
    /// The connection confirm/configure card (`WindowState::conn_card`, design
    /// §3.3 + §2.5). `controls conn-card` reads the OPEN card's pair +
    /// direction/kind selection; closed reports `conn-card open=false`. Like
    /// the tab menu it cannot be `open`ed from the wire — it needs a pair
    /// argument only the UI gestures (menu/picker/drag) carry.
    ConnCard,
    /// The session picker (`WindowState::session_picker`, design §2.3/§2.5).
    /// `controls session-picker` reads the OPEN picker's intent + filtered
    /// rows; closed reports `session-picker open=false`.
    SessionPicker,
    /// The connection map (`WindowState::connection_map`, design §5). `open
    /// connections` raises it on the frontmost window (host+raise, §5.1);
    /// `controls connections` reads the OPEN map's groups/arrows/annotations;
    /// closed reports `connections open=false`. Every wire read of it is
    /// Owner-gated (§5.3 — the `flows` twin).
    Connections,
}

impl AuxTarget {
    /// Parse a verb's target keyword (case-insensitive). Empty / `front` / `window` /
    /// `terminal` → [`AuxTarget::Front`]; `prefs` / `preferences` / `settings` →
    /// [`AuxTarget::Prefs`]. An unrecognized keyword yields `None` so the verb can
    /// reject it with a clear error.
    pub(crate) fn parse(s: &str) -> Option<AuxTarget> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "front" | "window" | "terminal" => Some(AuxTarget::Front),
            "prefs" | "preferences" | "settings" => Some(AuxTarget::Prefs),
            "about" => Some(AuxTarget::About),
            "menu" | "palette" => Some(AuxTarget::Menu),
            "tab-menu" => Some(AuxTarget::TabMenu),
            "conn-card" => Some(AuxTarget::ConnCard),
            "session-picker" => Some(AuxTarget::SessionPicker),
            "connections" => Some(AuxTarget::Connections),
            "update" | "software-update" => Some(AuxTarget::Update),
            _ => None,
        }
    }

    /// The short keyword for this target (for error messages + default capture filenames).
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            AuxTarget::Front => "front",
            AuxTarget::Prefs => "prefs",
            AuxTarget::About => "about",
            AuxTarget::Menu => "menu",
            AuxTarget::TabMenu => "tab-menu",
            AuxTarget::ConnCard => "conn-card",
            AuxTarget::SessionPicker => "session-picker",
            AuxTarget::Connections => "connections",
            AuxTarget::Update => "update",
        }
    }
}

/// PHASE-1 honesty gate ("Headless Present-Real"): may an explicit capture ADVANCE
/// the live cursor-effect engines? With no OS presentation target, capture is that
/// logical window's only application-render tick, so ticking keeps effects live.
/// A WINDOWED target instead reuses its last successful application-present quads;
/// capture cannot advance effect state beyond the app-present artifact. This is
/// app-present/capture phase parity, not a compositor or scanout claim.
///
/// PHASE-3 (one clock owner): while a `video` recording targets this window, its
/// offscreen present loop drives `tick_cursor_fx` — the recording loop OWNS the
/// engine clock, and a concurrent `image` must reuse the loop's last-present
/// quads exactly like a windowed capture does (ticking here would compose state
/// newer than the frame the recording just minted).
fn capture_ticks_cursor_fx(has_os_window: bool, recording_this_window: bool) -> bool {
    !has_os_window && !recording_this_window
}

/// Advance the visual half of the sing-along only when the explicit capture
/// owns this window's cursor-effect clock. Application-present performs this
/// same detector → cat sync on both live render paths; a glass-less still has
/// to do it itself, while a windowed or recording-owned capture must retain
/// the already-presented state. Deliberately no audio handle enters this seam.
struct CaptureCompanionSongSync<'a> {
    style: crate::cursor_glow::GlowStyle,
    cursor_trail_enabled: bool,
    cursor_companions_allowed: bool,
    pet_mode: bool,
    now: Instant,
    detector: &'a mut aterm_effects::kitty_sing::KittySing,
    cat: &'a mut crate::kitty_cursor::CursorCat,
    glow: &'a mut crate::cursor_glow::CursorGlow,
    riff_bar: &'a mut Option<u64>,
}

fn sync_capture_companion_song(sync: CaptureCompanionSongSync<'_>) -> f32 {
    let CaptureCompanionSongSync {
        style,
        cursor_trail_enabled,
        cursor_companions_allowed,
        pet_mode,
        now,
        detector,
        cat,
        glow,
        riff_bar,
    } = sync;
    if !cursor_companions_allowed {
        // Input bookkeeping is intentionally source-agnostic and may have
        // re-armed the detector after Serious Mode's transition drain. A
        // capture can run before the event loop's next scheduler drain, so it
        // must hard-retire that state here instead of sampling a graceful tail.
        *detector = aterm_effects::kitty_sing::KittySing::default();
        *riff_bar = None;
    }
    let drive = if cursor_companions_allowed
        && matches!(style, crate::cursor_glow::GlowStyle::RainbowKitty)
    {
        detector.drive(now)
    } else {
        0.0
    };
    if drive > 0.0 {
        glow.celebrate(now, drive);
        // Keep the visual bar latch current without emitting a sound event. If
        // this logical window later acquires glass, application-present resumes
        // on the current bar instead of replaying one the still already showed.
        if let Some(bar) = detector.bar(now) {
            *riff_bar = Some(bar);
        }
    } else {
        detector.settle(now);
        *riff_bar = None;
    }
    cat.set_singing(
        now,
        crate::kitty_cursor::SingSync {
            drive,
            beat: detector.beat(now).unwrap_or(0.0),
        },
    );
    crate::app_render::retire_kitty_cursor_without_owner(
        cursor_trail_enabled && cursor_companions_allowed,
        pet_mode,
        style,
        drive,
        cat,
    );
    drive
}

/// The flying companion's exact capture-side presentation predicate. This is
/// the application-present law from BOTH live paths: surface presentability,
/// the sparkle sprite owner, and either ordinary/hello admission OR the
/// reduced-motion static singing frame.
fn capture_flying_companion_enabled(
    cursor_companion_presentable: bool,
    animate_cat: bool,
    cursor_trail_enabled: bool,
    style: crate::cursor_glow::GlowStyle,
    collection_hello: bool,
    sing: f32,
) -> bool {
    cursor_companion_presentable
        && (crate::app_render::cursor_cat_presentation_enabled(
            animate_cat,
            cursor_trail_enabled,
            style,
            collection_hello,
        ) || sing > 0.0)
}

/// Resolve the ONE cursor companion for an introspection frame through the
/// same custody law as application-present. The middle verdict is the pet
/// brain's caret feed (0.33 full-motion swap; always fed but hidden under the
/// reduced-motion singer); the last is the resident pet's actual draw
/// admission (held until the singing kitty releases pixel custody).
fn capture_companion_custody(
    pet_mode: bool,
    kitty_enabled: bool,
    cat_alpha: u8,
    sing: f32,
    caret_sing: f32,
    pet_visible: bool,
    reduced_motion: bool,
) -> (u8, bool, bool) {
    let kitty_alpha = if kitty_enabled && crate::app_render::flying_kitty_admitted(pet_mode, sing) {
        cat_alpha
    } else {
        0
    };
    let pet_caret_live =
        crate::app_render::pet_caret_admitted(pet_visible, caret_sing, reduced_motion);
    let pet_on_glass = crate::app_render::pet_companion_admitted(pet_visible, sing);
    (kitty_alpha, pet_caret_live, pet_on_glass)
}

/// Present-time placement of one frame axis inside one raw surface axis.
/// Positive remainder becomes leading/trailing background bands; negative
/// remainder becomes leading/trailing crop. The HORIZONTAL axis uses exactly
/// the shared CPU/GPU centred [`aterm_render::band_offset`] rule; the vertical
/// one takes [`dims_axis_y`] (the platform [`aterm_render::band_offset_y`],
/// top-pinned on Linux) so `dims` reports the placement the presenters used.
fn dims_axis(surface: u32, frame: u32) -> (i64, u32, u32, u32, u32) {
    dims_axis_at(
        aterm_render::band_offset(surface as usize, frame as usize),
        surface,
        frame,
    )
}

/// [`dims_axis`]'s vertical twin: same band/crop algebra over the platform
/// vertical placement offset.
fn dims_axis_y(surface: u32, frame: u32) -> (i64, u32, u32, u32, u32) {
    dims_axis_at(
        aterm_render::band_offset_y(surface as usize, frame as usize),
        surface,
        frame,
    )
}

fn dims_axis_at(offset: i64, surface: u32, frame: u32) -> (i64, u32, u32, u32, u32) {
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

fn native_settings_closed_controls_lines() -> Vec<String> {
    vec![
        // Keep the historical first line byte-for-byte for callers that only
        // test open/closed. The following records define the truthful native
        // scope: a closed tab has no visible route or controls.
        "state open=false".to_string(),
        "prefs fields=0".to_string(),
        "native surface=native-tab route=- controls=0 semantics=0".to_string(),
    ]
}

fn native_settings_state_line(
    state: &crate::native_settings::SettingsViewState,
    field_count: usize,
    status: &str,
) -> String {
    let searching = !state.search.trim().is_empty();
    let editing = state.editing_field.as_deref().unwrap_or("");
    format!(
        "state open=true landing={} pane=native-tab category={} selected={} scroll={} \
         editing={editing:?} status={status:?} searching={searching} query={:?} \
         shown={field_count} total={field_count} surface=native-tab route={} page={:?}",
        state.route == crate::native_settings::SettingsRoute::Home && !searching,
        state.route.label(),
        state.page_scroll,
        state.page_scroll,
        state.search,
        state.route.path(),
        state.route.label(),
    )
}

fn native_settings_field_kind(
    field: &crate::prefs::EditField,
    trail_pack_ids: &[String],
    themes: &crate::app_config::ThemeCatalog,
) -> String {
    match field.kind {
        crate::prefs::EditKind::Float => "float".to_string(),
        crate::prefs::EditKind::Integer => "integer".to_string(),
        crate::prefs::EditKind::Bool => "bool".to_string(),
        crate::prefs::EditKind::Text => "text".to_string(),
        crate::prefs::EditKind::Enum { .. }
            if field.key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE =>
        {
            format!(
                "enum options=[{}]",
                crate::prefs::cursor_trail_style_options(trail_pack_ids.iter().map(String::as_str))
                    .join(",")
            )
        }
        crate::prefs::EditKind::Enum { options } => {
            format!("enum options=[{}]", options.join(","))
        }
        crate::prefs::EditKind::Theme => format!(
            "theme options=[{}]",
            crate::native_settings::settings_theme_options(themes).join(",")
        ),
        crate::prefs::EditKind::Color => "color".to_string(),
    }
}

fn native_settings_effective_value(value: &crate::native_ui::SemanticValue) -> String {
    match value {
        crate::native_ui::SemanticValue::None => String::new(),
        crate::native_ui::SemanticValue::Text(value) => value.clone(),
        crate::native_ui::SemanticValue::Bool(value) => value.to_string(),
        crate::native_ui::SemanticValue::Number { value, .. } => value.to_string(),
    }
}

fn native_settings_controls_lines(
    state: &crate::native_settings::SettingsViewState,
    compiled: &crate::native_ui::CompiledUi,
    trail_pack_ids: &[String],
) -> Vec<String> {
    // A legacy `field` record exists only when the exact compiled native tree
    // contains that setting control. Navigation, actions, status cards, and
    // previews remain available in the additive canonical `ui` records below.
    let fields = compiled
        .semantics
        .iter()
        .filter_map(|node| {
            let key = node.key.as_str().strip_prefix("settings/control/")?;
            let field = state.legacy.fields.iter().find(|field| field.key == key)?;
            Some((node, field))
        })
        .collect::<Vec<_>>();
    let status = state.feedback.as_deref().unwrap_or("");
    let mut out = Vec::with_capacity(3 + fields.len() + compiled.semantics.len());
    out.push(native_settings_state_line(state, fields.len(), status));
    out.push(format!("prefs fields={}", fields.len()));
    for (node, field) in fields {
        // Preserve the compatibility meanings: `value` is the registry seed
        // (configured text, and resolved state for historical Bool rows), while
        // `effective` is the value the native control actually presents after
        // defaults and live policy projection.
        let value = field.seed.as_deref().unwrap_or("");
        let effective = native_settings_effective_value(&node.value);
        let kind = native_settings_field_kind(field, trail_pack_ids, &state.config_assets().themes);
        let action = node
            .action
            .as_ref()
            .map_or("-", crate::native_ui::ActionId::as_str);
        out.push(format!(
            "field key={} label={:?} value={:?} effective={effective:?} kind={kind} \
             visible=true native-key={:?} action={action}",
            field.key,
            field.label,
            value,
            node.key.as_str(),
        ));
    }
    let controls = compiled
        .semantics
        .iter()
        .filter(|node| node.action.is_some())
        .count();
    out.push(format!(
        "native surface=native-tab route={} controls={controls} semantics={}",
        state.route.path(),
        compiled.semantics.len(),
    ));
    out.extend(compiled.controls_lines());
    out
}

fn native_settings_unavailable_controls_lines(
    state: &crate::native_settings::SettingsViewState,
    error: &str,
) -> Vec<String> {
    vec![
        native_settings_state_line(state, 0, error),
        "prefs fields=0".to_string(),
        format!(
            "native surface=native-tab route={} controls=0 semantics=0 error={error:?}",
            state.route.path()
        ),
    ]
}

fn native_settings_route_not_open_controls_lines(
    alias: &str,
    expected: crate::native_settings::SettingsRoute,
    current: Option<&crate::native_settings::SettingsViewState>,
) -> Vec<String> {
    let current_route = current.map_or("-", |state| state.route.path());
    let error = if let Some(state) = current {
        format!(
            "controls {alias} targets the native Settings {} route, but the front Settings presentation is {}; use `open {alias}` first",
            expected.path(),
            state.route.path(),
        )
    } else {
        format!(
            "controls {alias} targets the native Settings {} route, which is not visible in the front window; use `open {alias}` first",
            expected.path(),
        )
    };
    let state = current.map_or_else(
        || "state open=false".to_string(),
        |state| native_settings_state_line(state, 0, &error),
    );
    vec![
        state,
        "prefs fields=0".to_string(),
        format!(
            "native surface=native-tab alias={alias} route={current_route} expected-route={} controls=0 semantics=0 error={error:?}",
            expected.path(),
        ),
    ]
}

impl App {
    /// The `trail` control-socket verb ([`crate::Wake::TrailAdmissions`]): the
    /// FOCUSED window's cursor-trail ADMISSION DIAGNOSIS ring — the last
    /// spawn-seam verdicts (`licensed` / `declined`, with the declining reason
    /// token and the move's endpoints), newest last
    /// (`aterm_effects::cursor_glow::AdmissionRecord::line` rows).
    ///
    /// It exists because the rainbow-trail blackout (201449c2) was diagnosed
    /// by rebuilding with `ATERM_TRACE_SPAWN` and reading stderr archaeology;
    /// a user report should be ONE COMMAND. Read-only over state the engine
    /// already records beside decisions it has already made; typed TEXT is
    /// never reported — records carry positions and reason tokens only.
    ///
    /// `Err` when there is no focused window — an honest refusal, never a
    /// fabricated empty ring. An idle ring answers `OK 0`, which is itself a
    /// finding: the engine has judged no cursor move at all.
    pub(crate) fn trail_admissions(&self, count: Option<usize>) -> Result<Vec<String>, String> {
        let Some(ws) = self.frontmost_window.and_then(|wid| self.windows.get(&wid)) else {
            return Err("no focused window".to_string());
        };
        let now = Instant::now();
        let all: Vec<String> = ws
            .cursor_glow
            .admission_log()
            .map(|record| record.line(now))
            .collect();
        let keep = count.unwrap_or(all.len()).min(all.len());
        Ok(all[all.len() - keep..].to_vec())
    }

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

    /// The DPI scale a DETACHED session's `dims` record is derived from — the one
    /// case where no window holds the session, so [`Self::dims_snapshot`] has no
    /// per-window record to read and reports the LIVE SHARED backend instead.
    ///
    /// That backend is not scale-less: `apply_window_scale` tunes it to exactly one
    /// window's [`crate::MetricsView`] at a time, so the scale the detached cells /
    /// pad / font were actually derived from is that window's — recoverable rather
    /// than unknowable. In order:
    ///
    /// 1. A force pin (`--scale` / `$ATERM_FORCE_SCALE`) overrides every window's
    ///    real factor, and is the only scale a headless boot ever has.
    /// 2. The window whose metric record the backend currently carries (the same
    ///    equality `apply_window_scale` guards its re-tune with), so the reported
    ///    scale and the pad/font reported beside it describe ONE window.
    /// 3. Failing an exact match (a live zoom moves `font_px` without a per-window
    ///    re-record), the same deterministic window `dims_snapshot` itself prefers:
    ///    the front window, else the lowest stable id. A real DPI beats a literal.
    /// 4. `1.0` only when there is no window at all — which is exactly the seed the
    ///    pre-attach cell/pad values in that branch are built from, so the record
    ///    stays internally consistent.
    fn detached_scale(&self) -> f64 {
        if let Some(pinned) = crate::app_config::resolve_force_scale() {
            return pinned;
        }
        // `windows` is a BTreeMap, so both scans below are id-ordered: a detached
        // `dims` is as repeatable as an attached one.
        let tuned = if self.backend.is_pending() {
            None
        } else {
            self.windows.values().find(|ws| {
                (ws.metrics.font_px - self.font_px).abs() < 0.5
                    && ws.metrics.pad == self.backend.pad()
                    && ws.metrics.pad_top == self.backend.pad_top()
                    && ws.metrics.head == self.backend.head()
            })
        };
        tuned
            .or_else(|| {
                self.frontmost_window
                    .and_then(|wid| self.windows.get(&wid))
                    .or_else(|| self.windows.values().next())
            })
            .map_or(1.0, |ws| ws.scale)
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
        // The DPI scale the rest of this snapshot is derived from. Read from the
        // per-window record (the W12 source of truth `attach_os_window` seeds and
        // `on_scale_factor_changed` keeps current), NOT from the live shared
        // backend — the backend is re-tuned to whichever window last drew, so on a
        // mixed-DPI desktop it would report the wrong window's DPI. With no window
        // holding the session the DETACHED branch below reports that shared backend,
        // so the scale is read the same way instead of being asserted as 1.0.
        let scale = selected.map_or_else(|| self.detached_scale(), |wid| self.windows[&wid].scale);

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
            let tab_rows = u32::from(self.chrome_rows());
            let window_rows = u32::from(ws.rows);
            let window_cols = u32::from(ws.cols);
            let composed_rows = window_rows.saturating_add(tab_rows);
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
            dims_axis_y(surface_h, frame_h);
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
            scale,
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
            viewers: u32::try_from(windows.len()).unwrap_or(u32::MAX),
            visible_viewers: u32::try_from(visible_viewers).unwrap_or(u32::MAX),
            geometry,
            present_retry_state,
            present_retry_count,
            present_retry_remaining,
            present_retry_in_ms,
            layer_presentation: selected
                .and_then(|wid| self.windows[&wid].os_window.as_ref())
                .and_then(|w| self.apprt.window_surface_presentation(w)),
        }
    }

    /// The COMPOSED (split) twin of [`Self::splice_word_decorations`]: run the
    /// glass's own per-pane sparkle pass and install its window-coordinate
    /// output over the composite this capture just built.
    ///
    /// Reusing `compose_word_decorations` rather than a second pipeline is what
    /// makes the capture WYSIWYG by construction — same engine, same per-pane
    /// binding, same damage gate, same translation into window coords.
    fn splice_composed_capture_decorations(
        &mut self,
        wid: WindowId,
        now: Instant,
        exact_focus: crate::app_render::TerminalCaptureFocus,
    ) {
        let Some(rects) = self
            .active_tree(wid)
            .zip(self.windows.get(&wid))
            .map(|(tree, ws)| tree.compute_layout(ws.rows, ws.cols))
        else {
            return;
        };
        let Some(focus) = self.active_tree(wid).map(crate::pane::PaneTree::focus) else {
            return;
        };
        let panes: Vec<(
            crate::pane::PaneRect,
            std::sync::Arc<std::sync::Mutex<Terminal>>,
        )> = rects
            .iter()
            .filter_map(|r| self.pool.get(r.session).map(|s| (*r, s.term.clone())))
            .collect();
        if panes.is_empty() {
            return;
        }
        let raw_focused = self.windows.get(&wid).is_some_and(|ws| ws.focused);
        // `cursor_fx_focus`, not bare `motion_focus`: the present seams fold
        // the TYPED WAKE into the focus input, and a capture must resolve the
        // same policy the glass painted (gauntlet F3 parity).
        let capture_focused = self.cursor_fx_focus(wid, raw_focused, now);
        let motion = self.motion_policy(capture_focused);
        let animate_sparkles = motion.animate(crate::motion::MotionEffect::WordSparkles);
        let (cell_w, cell_h) = self.backend.cell_size();
        let glow_cfg = self.glow_config();
        let cursor_companions_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorCat);
        let load_shed = self.load_shed_active();
        let pet_mode = crate::cursor_glow::GlowStyle::style_names_any_pet(
            self.config.cursor_trail_style_raw(),
        );
        let pet_species = self.trail_pet_species();
        // The ONE companion verdict (favourite > program with tenure > launch
        // kitty) — the same seam the composed PRESENT resolves through, so a
        // capture shows the breed the glass wears (gauntlet F3: a capture
        // must never resolve the companion through a different rule than the
        // present). Same window, same focused pane, same frame instant.
        let companion_look = self.companion_verdict(wid, focus, now);
        let windowless = self
            .windows
            .get(&wid)
            .is_none_or(|window| window.os_window.is_none());
        // The focused pane's caret, in PANE-LOCAL cells: the companion's anchor.
        if exact_focus.session != focus {
            return;
        }
        let focus_live_viewport = exact_focus.live_viewport;
        let focus_cursor = exact_focus.cursor;
        let focus_coordinate_space = Some((exact_focus.terminal_id, exact_focus.alternate_screen));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some((terminal_id, alternate_screen)) = focus_coordinate_space {
            sync_cursor_effect_coordinate_space(ws, terminal_id, alternate_screen);
        }
        let (win_rows, win_cols) = (ws.rows, ws.cols);
        ws.cursor_cat.set_look(companion_look);
        let cursor_companion_presentable =
            cursor_companions_allowed && capture_focused && !load_shed && focus_live_viewport;
        // A capture is itself a presentation boundary (the single-pane arm's rule).
        ws.cursor_cat
            .set_collection_presentable(now, cursor_companion_presentable);
        if !cursor_companions_allowed {
            ws.music_notes.clear();
        }
        let sing_drive = {
            let (detector, cat, glow, riff_bar) = (
                &mut ws.kitty_sing,
                &mut ws.cursor_cat,
                &mut ws.cursor_glow,
                &mut ws.sing_riff_bar,
            );
            sync_capture_companion_song(CaptureCompanionSongSync {
                style: glow_cfg.style,
                cursor_trail_enabled: glow_cfg.enabled,
                cursor_companions_allowed,
                pet_mode,
                now,
                detector,
                cat,
                glow,
                riff_bar,
            })
        };
        // A capture is a renderer, not a reduced-motion verdict. Sample one
        // live pose whenever the SAME policy as glass permits animation;
        // headless captures must not erase an already-earned companion merely
        // because they have no OS window.
        let animate_cat = motion.animate(crate::motion::MotionEffect::CursorGlow);
        let cat_frame = if animate_cat {
            ws.cursor_cat.frame(now)
        } else {
            ws.cursor_cat.static_frame(now)
        };
        let kitty_enabled = capture_flying_companion_enabled(
            cursor_companion_presentable,
            animate_cat,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
            cat_frame.collection_hello,
            cat_frame.sing,
        );
        // The PET companion. A capture is a presentation boundary, so it
        // resolves the pet the same way the composed present does — otherwise
        // `aterm-ctl image` on a split window would be blind to the one
        // companion the user actually selected.
        let pet_visible = crate::app_render::resident_pet_presentation_enabled(
            pet_mode,
            cursor_companion_presentable,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
        );
        let (kitty_alpha, pet_caret_live, pet_on_glass) = capture_companion_custody(
            pet_mode,
            kitty_enabled,
            cat_frame.alpha,
            cat_frame.sing,
            sing_drive,
            pet_visible,
            !animate_cat,
        );
        let (pane_rows, pane_cols, pane_origin) = panes
            .iter()
            .find(|(r, _)| r.session == focus)
            .map_or((0, 0, (0, 0)), |(r, _)| {
                (
                    r.rows,
                    r.cols,
                    (
                        i32::from(r.col_off) * cell_w as i32,
                        i32::from(r.row_off) * cell_h as i32,
                    ),
                )
            });
        if !capture_focused {
            ws.retire_cursor_pet_coordinate_space();
        }
        prepare_resident_pet_tick(
            &mut ws.word_decos,
            &mut ws.cursor_pet,
            pet_species,
            Some((focus, pane_origin)),
        );
        let pet_sense = aterm_effects::kitty_pet::PetSense {
            now,
            caret: if pet_caret_live { focus_cursor } else { None },
            rows: pane_rows,
            cols: pane_cols,
            cell_w: cell_w.min(usize::from(u16::MAX)) as u16,
            cell_h: cell_h.min(usize::from(u16::MAX)) as u16,
            reduced_motion: !animate_cat,
            // A capture is one isolated frame: the burst probe is a
            // frame-over-frame diff the windowed presents own, and a capture
            // must never charge the watch (or steal the diff's baseline).
            // No live pointer either — a capture has no mouse. The wrap fact
            // is the same kind of present-owned frame-over-frame diff
            // (`wrap_fact_edge`), so a capture never reads — or spends — it.
            output_burst: false,
            pointer: None,
            wrapped: false,
        };
        // The switch, on the capture path too: an unowned resident retires
        // outright, so a capture can never immortalize a sprite the live window
        // has already stopped owning (`retire_pet_without_owner`).
        crate::app_render::retire_pet_without_owner(
            pet_mode,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
            &mut ws.cursor_pet,
        );
        let pet = if windowless {
            ws.cursor_pet.tick_static_capture(pet_sense)
        } else {
            ws.cursor_pet.tick(pet_sense)
        };
        let ctx = crate::app_render::ComposeDecoCtx {
            panes: &panes,
            focus,
            win_rows,
            win_cols,
            cell_w: cell_w as u32,
            cell_h: cell_h as u32,
            focus_cursor,
            win_focused: raw_focused,
            animate_sparkles,
            animate_cat,
            kitty_alpha,
            cat_frame,
            pet,
            pet_visible: pet_on_glass,
            // A CAPTURE, not a drawn present: the pet sync site must not
            // spend a hello for a frame nobody's window presented (the
            // `kitty_summon` precedent — see `sync_pet_companion_look`).
            present: false,
            accent: glow_cfg.accent,
            cursor_color: ws.input_scratch.cursor_color,
            now,
        };
        self.compose_word_decorations(wid, &ctx);
        // The advancing capture already consumed every pane's represented
        // damage under its exact extraction lock. Nothing below may relock a
        // terminal and swallow a newer synchronized-output episode.
        if let Some(ws) = self.windows.get_mut(&wid) {
            crate::app_render::splice_word_deco_channels(ws);
        }
    }

    /// Build this window's sparkle-word decorations into `input_scratch`, mirroring
    /// the application-present redraw (`redraw_window`), so `image`/`snapshot` use
    /// the same app-owned transient-effect state. Must run AFTER `cell_frame_into`
    /// and BEFORE the tab-strip splice (which shifts decorations with the grid).
    /// Word-owned channels are omitted when the feature is off, or on the
    /// alt-screen with `suppress_in_alt_screen` set. Cursor companions remain
    /// independent and still follow their own presentation policy.
    #[cfg(test)]
    pub(crate) fn splice_word_decorations(&mut self, wid: crate::WindowId, now: Instant) {
        let Some(focus) = self
            .prepare_terminal_capture_grid_with_cursor_fx(
                wid,
                crate::app_render::ComposedCursorFxClock::Advance(now),
            )
            .and_then(|grid| grid.focus)
        else {
            return;
        };
        self.splice_word_decorations_sampled(wid, now, focus);
    }

    fn splice_word_decorations_sampled(
        &mut self,
        wid: crate::WindowId,
        now: Instant,
        exact_focus: crate::app_render::TerminalCaptureFocus,
    ) {
        // ONE CLOCK OWNER, including the companion engines. A windowed capture
        // reuses the application-present artifact; a concurrent still during a
        // headless recording reuses the recording loop's artifact. Rebuilding
        // either would advance KittySing, CursorCat, and PetBrain beyond the
        // pixels that owner most recently presented. The retained accumulators
        // are reinstalled because a composed grid refill clears host overlays.
        let recording_this_window = self.video_rec.as_ref().is_some_and(|r| r.window == wid);
        let has_os_window = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.os_window.is_some());
        let capture_owns_cursor_fx = capture_ticks_cursor_fx(has_os_window, recording_this_window);
        if !capture_owns_cursor_fx {
            if let Some(ws) = self.windows.get_mut(&wid) {
                // Rain remains intentionally excluded from introspection.
                ws.input_scratch.rain_quads.clear();
                ws.input_scratch.rain_atlas = None;
                ws.input_scratch.rain_add.clear();
                crate::app_render::splice_word_deco_channels(ws);
            }
            return;
        }
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
            // `cell_frame_into` intentionally preserves every host-owned
            // overlay. Clear the complete word-decoration family BEFORE the
            // feature-off early return below, or an enabled capture followed by
            // a config disable reuses the prior frame's cat/ink/nova forever.
            // A live feature repopulates these channels at the commit tail.
            ws.input_scratch.word_decorations.clear();
            ws.input_scratch.ink.clear();
            ws.input_scratch.cat_quads.clear();
            ws.input_scratch.cat_atlas = None;
            ws.input_scratch.free_sprites.clear();
            ws.input_scratch.free_atlas = None;
            ws.input_scratch.nova_add.clear();
        }
        // Capture and application-present install the same admitted outer catalog Arc.
        // Keep this before all feature early-outs so a capture can never retain
        // an earlier rainbow kitty generation merely because word sparkles are disabled.
        self.install_window_config_assets(wid);
        // COMPOSED (split) capture: introspection is sacred — the capture must
        // show what the GLASS shows, and the glass now decorates every visible
        // pane. Run the SAME per-pane pass the compose present runs, over the
        // composite `prepare_terminal_capture_grid` just built, and install it.
        //
        // What genuinely must be skipped on a composed frame is the coherence
        // REFILL further down: it re-extracts the FRONT terminal at window dims
        // and would overwrite the composite with the front-pane-stretched-over-
        // the-window lie the composed capture exists to fix. The capture verbs
        // gate on `TerminalCaptureGrid::composed` first; this stays as the
        // direct-caller backstop.
        if self
            .active_tree(wid)
            .is_some_and(|t| t.len() > 1 && !t.is_zoomed())
        {
            self.splice_composed_capture_decorations(wid, now, exact_focus);
            return;
        }
        let sparkle = self
            .sparkle
            .as_ref()
            .map(|r| (r.cfg.clone(), r.lexicon.clone()));
        // Resolve the same MOTION POLICY as application-present (W11), so a
        // reduced/unfocused window captures the same static app-owned decorations.
        // Folded into the effects engine's own `reduced_motion` seam below (it
        // cannot depend on `crate::motion`). Includes the same `motion_focus`
        // recording pin used by application-present, preserving phase parity —
        // and the same TYPED-WAKE fold (`cursor_fx_focus`), so a capture of a
        // typed-into unfocused window shows the decorations the glass painted.
        let capture_focused = self.cursor_fx_focus(
            wid,
            self.windows.get(&wid).is_some_and(|ws| ws.focused),
            now,
        );
        let motion = self.motion_policy(capture_focused);
        let sparkle = sparkle.map(|(mut cfg, lexicon)| {
            if !motion.animate(crate::motion::MotionEffect::WordSparkles) {
                cfg.reduced_motion = true;
            }
            (cfg, lexicon)
        });
        // Cell metrics for the cat emitter (§5.2 geometry / §5.7 floors); read
        // before the `ws` borrow — like the Kitty Log recorder gate (§F4.7).
        let (cell_w, cell_h) = self.backend.cell_size();
        let kitty_log_on = self.kitty_log_enabled();
        // The ONE companion verdict — the single-pane present's seam
        // ([`crate::App::companion_verdict`]): favourite > program with
        // tenure > launch kitty, resolved for this window's front session at
        // this frame's instant. A capture resolves through exactly the
        // present's rule so `aterm-ctl image` can never show a different cat
        // than the glass (gauntlet F3).
        let capture_front_session = self.focused_session_id(wid).unwrap_or(0);
        let companion_look = self.companion_verdict(wid, capture_front_session, now);
        let glow_cfg = self.glow_config();
        let cursor_companions_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorCat);
        let pet_mode = crate::cursor_glow::GlowStyle::style_names_any_pet(
            self.config.cursor_trail_style_raw(),
        );
        let pet_species = self.trail_pet_species();
        // The same alt-screen policy the live application-present resolves: only a
        // configured `suppress_in_alt_screen` blanks the capture's decorations.
        let suppress_alt = sparkle.is_some() && self.config.sparkle_suppress_alt_screen();
        // The same effective load-shed gate the live present folds into
        // `deco_suspend` (app_render). The raw diagnostic latch does not
        // suppress a user-forced Full policy or an explicit adaptive opt-out.
        let load_shed = self.load_shed_active();
        let windowless = self
            .windows
            .get(&wid)
            .is_none_or(|window| window.os_window.is_none());
        if exact_focus.session != capture_front_session || !exact_focus.damage_consumed {
            // A one-pane headless decoration pass is admitted only by the
            // exact extraction that consumed its own damage session. Missing
            // or mismatched custody fails closed instead of re-locking a
            // possibly newer synchronized-output episode.
            return;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let (rows, cols) = (ws.rows as usize, ws.cols as usize);
        let live_viewport = exact_focus.live_viewport;
        let grid_cursor = (
            u16::try_from(ws.input_scratch.cursor_row).unwrap_or(u16::MAX),
            u16::try_from(ws.input_scratch.cursor_col).unwrap_or(u16::MAX),
        );
        sync_cursor_effect_coordinate_space(
            ws,
            exact_focus.terminal_id,
            exact_focus.alternate_screen,
        );
        // Capture consumes the same admitted catalog Arc as application-present. Asset
        // installation above is Arc/scalar-only and cannot perform filesystem
        // or decode work.
        ws.cursor_cat.set_look(companion_look);
        let cursor_companion_presentable =
            cursor_companions_allowed && capture_focused && !load_shed && live_viewport;
        // The capture itself is a presentation boundary. This resumes a hello
        // that was discovered after the preceding capture had already resolved
        // its frame and was therefore paused unseen below.
        ws.cursor_cat
            .set_collection_presentable(now, cursor_companion_presentable);
        if !cursor_companions_allowed {
            ws.music_notes.clear();
        }
        // A CAPTURE IS A RENDERER, so it declares its session exactly like the
        // unsplit glass path does (`app_render::redraw_window`). This arm binds
        // no pane — `input_scratch` holds the FRONT terminal's whole grid — and
        // an unbound engine used to treat that as "every session matches",
        // which is a claim about geometry masquerading as one about identity.
        // Without this, capturing tab B ticked the engine with tab A's edit
        // keystroke still a live re-arm witness. Declared BEFORE the
        // suspension fork below for the same reason the glass path declares
        // above its own: identity does not depend on whether this frame draws.
        ws.word_decos.set_scan_session(Some(exact_focus.session));
        let sing_drive = {
            let (detector, cat, glow, riff_bar) = (
                &mut ws.kitty_sing,
                &mut ws.cursor_cat,
                &mut ws.cursor_glow,
                &mut ws.sing_riff_bar,
            );
            sync_capture_companion_song(CaptureCompanionSongSync {
                style: glow_cfg.style,
                cursor_trail_enabled: glow_cfg.enabled,
                cursor_companions_allowed,
                pet_mode,
                now,
                detector,
                cat,
                glow,
                riff_bar,
            })
        };
        // Explicit captures have no animation timer, but they are renderers:
        // sample the live lifecycle once under the same motion policy as glass.
        // Reduced motion still uses the static collection frame and never
        // synthesizes an unearned ordinary flight.
        let animate_cat = motion.animate(crate::motion::MotionEffect::CursorGlow);
        let cat_frame = if animate_cat {
            ws.cursor_cat.frame(now)
        } else {
            ws.cursor_cat.static_frame(now)
        };
        let kitty_enabled = capture_flying_companion_enabled(
            cursor_companion_presentable,
            animate_cat,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
            cat_frame.collection_hello,
            cat_frame.sing,
        );
        // The PET companion, resolved the same way the composed capture arm
        // does: a capture is a presentation boundary. The shared custody law
        // admits the flying singing face during pet mode's authored handoff,
        // then returns the caret and pixels to the resident pet.
        let pet_visible = crate::app_render::resident_pet_presentation_enabled(
            pet_mode,
            cursor_companion_presentable,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
        );
        let (kitty_alpha, pet_caret_live, pet_on_glass) = capture_companion_custody(
            pet_mode,
            kitty_enabled,
            cat_frame.alpha,
            cat_frame.sing,
            sing_drive,
            pet_visible,
            !animate_cat,
        );
        let effect_geom = crate::word_decorations::EffectGeom {
            cell_w: cell_w as u16,
            cell_h: cell_h as u16,
            rows: rows as u16,
            cols: cols as u16,
        };
        // The capture owns this tick only on the headless/no-recording arm.
        // Feed the same species and one-frame-stale scanner map as live glass;
        // `set_scan_session` already ran above, before any rescan decision.
        if !capture_focused {
            ws.retire_cursor_pet_coordinate_space();
        }
        prepare_resident_pet_tick(&mut ws.word_decos, &mut ws.cursor_pet, pet_species, None);
        // The ownership switch precedes every presentation fork, including
        // load shedding's cleared early return. Load shedding is a temporary
        // visibility freeze; it cannot immortalize a brain/mote cadence whose
        // trail or style owner was removed on this capture.
        crate::app_render::retire_pet_without_owner(
            pet_mode,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
            &mut ws.cursor_pet,
        );
        let words_suspended = exact_focus.alternate_screen && suppress_alt;
        if load_shed {
            // Load shedding owns the complete decorative surface. Undo the
            // companion presentation opportunity sampled above at the same
            // instant, so a one-shot collection hello cannot age out behind
            // this cleared early return.
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
        if words_suspended || sparkle.is_none() {
            // Sparkle Words and the cursor companion share bakers/atlas, not
            // ownership. An explicit feature disable resets word episodes;
            // alt-screen suppression freezes them exactly like live glass.
            // Either way, keep building the independent pet or authenticated
            // flying kitty into the same free-sprite channel.
            if words_suspended {
                ws.word_decos.freeze(now);
            } else {
                ws.word_decos.hard_reset_words();
                ws.pending_deco_birth = None;
            }
            ws.deco_scratch.clear();
            ws.ink_scratch.clear();
            ws.free_scratch.clear();
            ws.nova_scratch.clear();
            ws.word_decos.begin_host_frame();

            // The authorized extraction already consumed this capture's exact
            // damage session, even when no word scanner is enabled.
            let cur = exact_focus.cursor;
            let pet_sense = aterm_effects::kitty_pet::PetSense {
                now,
                caret: if pet_caret_live { cur } else { None },
                rows: effect_geom.rows,
                cols: effect_geom.cols,
                cell_w: effect_geom.cell_w,
                cell_h: effect_geom.cell_h,
                reduced_motion: !animate_cat,
                output_burst: false,
                pointer: None,
                // Present-owned frame-over-frame diffs (burst, wrap fact)
                // stay inert in a capture — see the composed capture's law.
                wrapped: false,
            };
            let pet = if windowless {
                ws.cursor_pet.tick_static_capture(pet_sense)
            } else {
                ws.cursor_pet.tick(pet_sense)
            };
            let default_bg = ws.input_scratch.default_bg;
            let cursor_color = ws.input_scratch.cursor_color;
            let _ = crate::app_render::emit_single_cursor_companion(
                ws,
                effect_geom,
                cur,
                pet_on_glass && pet.alpha > 0,
                // A CAPTURE: never a spent hello (`sync_pet_companion_look`).
                false,
                pet,
                cat_frame,
                kitty_alpha,
                now,
                !animate_cat,
                default_bg,
                cursor_color,
                glow_cfg.accent,
            );
            crate::app_render::splice_word_deco_channels(ws);
            return;
        }
        let (cfg, lexicon) = sparkle
            .as_ref()
            .expect("enabled, unsuspended Sparkle Words was checked above");
        let mut prime_at = None;
        // Resume from a freeze (perf_reduced cleared / alt-screen exit with
        // suppression) before the rescan/tick read the clock — a no-op when
        // not frozen. Mirrors app_render's thaw-before-rescan ordering.
        ws.word_decos.thaw(now);
        let epoch = ws.input_scratch.snapshot_seq;
        let word_cursor = live_viewport.then_some(grid_cursor);
        if ws.word_decos.needs_rescan(epoch) {
            // A normal application-present consumes `pending_deco_birth`. If the
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
                lexicon,
                cfg,
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
        // The capture preparation consumed this exact damage session while it
        // still held the sync-authorized extraction lock. Relocking here would
        // risk swallowing a newer partial DEC-2026 episode.
        // The capture tick shares the window's live effect state, so it must
        // preserve app-present phase parity: the same cursor cell (§5.8 gaze — read
        // under this same lock) and the window's real focus (so a capture
        // never clobbers a focused window's armed blink one-shot, and a
        // headless/unfocused capture arms nothing).
        let cur = exact_focus.cursor;
        // THE PET BRAIN TICKS on the capture too — the capture shares the
        // window's live effect state (the composed capture arm has always
        // ticked it), and the suppression below needs the animal's LIVE body,
        // not a guess. A pet that cannot be drawn is fed `caret: None`, so it
        // fades out and releases honestly, exactly as the windowed paths do.
        let pet_sense = aterm_effects::kitty_pet::PetSense {
            now,
            caret: if pet_caret_live { cur } else { None },
            rows: effect_geom.rows,
            cols: effect_geom.cols,
            cell_w: effect_geom.cell_w,
            cell_h: effect_geom.cell_h,
            reduced_motion: !animate_cat,
            // A capture is one isolated frame — see the windowed capture arm.
            output_burst: false,
            pointer: None,
            wrapped: false,
        };
        let pet = if windowless {
            ws.cursor_pet.tick_static_capture(pet_sense)
        } else {
            ws.cursor_pet.tick(pet_sense)
        };
        let pet_on_glass = pet_on_glass && pet.alpha > 0;
        // ONE CAT PER CARET: exactly the predicate the companion emission below
        // draws under. Told which cell the companion occupies, the engine drops
        // the ambient peek for the word beneath it — a capture must show the
        // same single cat the glass does, not the pre-fix pair. The pet counts
        // as the companion too (the windowed present's rule), and hands in its
        // live drawn body as the pixel-yield box.
        let companion_duty =
            crate::app_render::cursor_companion_duty(pet_on_glass, kitty_alpha, cur);
        let companion_at = crate::app_render::cursor_companion_on_glass(
            companion_duty,
            cur,
            // The flying head's live rect — a capture must yield to the same
            // pixels the glass does, and the head is nowhere near the caret.
            match companion_duty {
                crate::app_render::CompanionDuty::FlyingHead { cell } => {
                    crate::app_render::flying_head_footprint_px(
                        &ws.word_decos,
                        effect_geom,
                        cell,
                        cat_frame.render_look(),
                        cat_frame.bob,
                    )
                }
                _ => None,
            },
            pet_on_glass
                .then(|| {
                    pet.body_px(
                        effect_geom.cell_w,
                        effect_geom.cell_h,
                        effect_geom.cols,
                        effect_geom.rows,
                    )
                })
                .flatten(),
        );
        // The same selection view the animated tick sees (§6.4 nova ignition
        // deferral / per-quad attenuation) — a capture must not ignite a nova
        // the window itself would defer.
        let sel_view = crate::word_decorations::SelView {
            sel: &ws.input_scratch.selection,
            display_offset: ws.input_scratch.display_offset,
        };
        let mut primed_wince_hits = 0u8;
        if let Some(birth) = prime_at {
            // The synthetic birth frame is its own frame: bracket it so it gets
            // its own two-bake budget, exactly as when `tick` owned the reset.
            ws.word_decos.begin_host_frame();
            // Discard the birth frame's zero-reveal output. Its state mutations
            // (phase latch / limiter decisions) are exactly what an ordinary
            // damage-driven present would have performed at output time.
            ws.word_decos.tick(
                birth,
                cfg,
                effect_geom,
                companion_at,
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
                self.config.trail_sound_voice(),
                effect_geom.cols,
                None,
                false,
                |_| {},
            )
            .wince_hits;
        }
        // Open the capture's real frame: the two-bake budget and the baker's
        // LRU clock are per FRAME, and a bracketing host owns that reset.
        ws.word_decos.begin_host_frame();
        ws.word_decos.tick(
            now,
            cfg,
            effect_geom,
            companion_at,
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
        // AMBIENT PROVENANCE (owner ruling, 2026-08-07): a capture scans the
        // same OUTPUT text the windowed drains do — count and collect only,
        // never the companion, so no `on_collect` hello can start from a
        // capture either.
        self.kitty_log.observe_ambient(
            exact_focus.session,
            ws.word_decos.drain_kitty_sightings(),
            lexicon,
            now,
            kitty_log_on,
        );
        // Curse cues share the application-present drain's exact site dedupe. A capture is a
        // recording-shaped surface and must never make the Mac speak, so gain
        // stays `None` and no sound can escape; the VISUAL reaction still
        // belongs to the cursor companion. `cat_frame` was sampled above, so a
        // headless caller observes the wince on its next explicit capture.
        let curse_drain = crate::app_render::drain_curse_bonk_cues(
            &mut ws.word_decos,
            glow_cfg.style,
            self.config.trail_sound_voice(),
            effect_geom.cols,
            None,
            false,
            |_| {},
        );
        ws.cursor_cat.on_curse(
            now,
            primed_wince_hits.saturating_add(curse_drain.wince_hits),
        );
        // The shared config-free emitter owns BOTH animals. Sparkle Words may
        // contribute ambient sprites to `free_scratch`, but it neither chooses
        // the companion nor supplies its palette/lifecycle.
        let default_bg = ws.input_scratch.default_bg;
        let cursor_color = ws.input_scratch.cursor_color;
        let _ = crate::app_render::emit_single_cursor_companion(
            ws,
            effect_geom,
            cur,
            pet_on_glass,
            // A CAPTURE: never a spent hello (`sync_pet_companion_look`).
            false,
            pet,
            cat_frame,
            kitty_alpha,
            now,
            !animate_cat,
            default_bg,
            cursor_color,
            glow_cfg.accent,
        );
        ws.input_scratch
            .word_decorations
            .clone_from(&ws.deco_scratch);
        // Ink is part of the styled app-render capture too; the `plain` capture
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

    /// Select the sole legal animation-clock owner for a composed capture.
    fn composed_capture_cursor_fx_clock(
        &self,
        wid: crate::WindowId,
        now: Instant,
    ) -> crate::app_render::ComposedCursorFxClock {
        let recording_here = self.video_rec.as_ref().is_some_and(|r| r.window == wid);
        let has_os_window = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.os_window.is_some());
        if capture_ticks_cursor_fx(has_os_window, recording_here) {
            crate::app_render::ComposedCursorFxClock::Advance(now)
        } else {
            crate::app_render::ComposedCursorFxClock::Retain { observed_at: now }
        }
    }

    /// Reconstruct the leaf inventory of a just-presented pure-terminal frame
    /// without touching a terminal lock. `capture_leaf_snapshot_seqs` was filled
    /// in the exact pane extraction loop that built `input_scratch`; the
    /// synchronous present barrier guarantees no later staged frame can replace
    /// either half before this function runs.
    pub(crate) fn presented_terminal_capture_grid(
        &self,
        wid: crate::WindowId,
    ) -> Option<crate::app_render::TerminalCaptureGrid> {
        let mut grid = crate::app_render::TerminalCaptureGrid {
            composed: false,
            focus: None,
            leaves: Vec::new(),
        };
        let crate::VisibleContentRoute::Terminal { composed } =
            self.active_visible_content_route(wid)?
        else {
            return None;
        };
        let plan = self.active_visible_leaf_plan(wid)?;
        self.refill_presented_terminal_capture_grid(wid, composed, &plan, &mut grid)
            .then_some(grid)
    }

    /// Allocation-reusing twin used by the 60fps recording success hook.
    fn refill_presented_terminal_capture_grid(
        &self,
        wid: crate::WindowId,
        composed: bool,
        plan: &crate::tab_model::VisibleLeafPlan,
        grid: &mut crate::app_render::TerminalCaptureGrid,
    ) -> bool {
        let Some(window) = self.windows.get(&wid) else {
            return false;
        };
        if plan.leaves.is_empty() || plan.leaves.len() != window.capture_leaf_snapshot_seqs.len() {
            return false;
        }
        grid.composed = composed;
        grid.focus = None;
        grid.leaves.clear();
        for (leaf, snapshot_seq) in plan.leaves.iter().zip(&window.capture_leaf_snapshot_seqs) {
            let Some(crate::tab_model::View::Terminal(terminal)) =
                self.view_store.get(leaf.view).copied()
            else {
                grid.leaves.clear();
                return false;
            };
            grid.leaves.push(crate::app_render::TerminalCaptureLeaf {
                view: leaf.view,
                session: terminal.session,
                focused: leaf.focused,
                rows: (leaf.rect.size.height.round() as usize).max(1),
                cols: (leaf.rect.size.width.round() as usize).max(1),
                snapshot_seq: *snapshot_seq,
            });
        }
        true
    }

    fn recording_card_identity(
        card: Option<&crate::SettingsCard>,
    ) -> Option<crate::VideoPresentedCardIdentity> {
        card.map(|card| crate::VideoPresentedCardIdentity {
            width: card.pw,
            height: card.ph,
            x: card.dx,
            y: card.dy,
            model: card.fp,
            geometry: card.geom,
        })
    }

    /// Compact identity of every leaf in a native/mixed frame. Later stills
    /// compare this exact list before consulting retained native caches; a
    /// staged tab/layout/document change therefore defers instead of pairing
    /// the recording's resident input with newer semantic metadata.
    fn refill_recording_native_leaf_identities(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        identities: &mut Vec<crate::VideoPresentedLeafIdentity>,
    ) -> Option<bool> {
        let window = self.windows.get(&wid)?;
        let (cell_w, cell_h) = self.win_cell_size(wid);
        let mut changed = identities.len() != plan.leaves.len();
        for (index, leaf) in plan.leaves.iter().enumerate() {
            let retained = window.leaf_render_cache.get(&leaf.view)?;
            let identity = match self.view_store.get(leaf.view).copied()? {
                crate::tab_model::View::Terminal(terminal) => crate::VideoPresentedLeafIdentity {
                    kind: "terminal",
                    view: leaf.view.get(),
                    session: Some(terminal.session),
                    focused: leaf.focused,
                    width: (leaf.rect.size.width * cell_w as f32).round().max(1.0) as u32,
                    height: (leaf.rect.size.height * cell_h as f32).round().max(1.0) as u32,
                    snapshot_seq: Some(retained.input.snapshot_seq),
                    native_stamp: None,
                },
                crate::tab_model::View::Native(_) => {
                    let raster = retained.native.as_ref()?;
                    crate::VideoPresentedLeafIdentity {
                        kind: "native",
                        view: leaf.view.get(),
                        session: None,
                        focused: leaf.focused,
                        width: raster.width,
                        height: raster.height,
                        snapshot_seq: None,
                        native_stamp: Some(raster.stamp),
                    }
                }
            };
            if let Some(slot) = identities.get_mut(index) {
                changed |= *slot != identity;
                *slot = identity;
            } else {
                identities.push(identity);
                changed = true;
            }
        }
        identities.truncate(plan.leaves.len());
        Some(changed)
    }

    /// Commit the lightweight semantic half of the just-successful video
    /// present. The GPU's resident input is identified, not copied: explicit
    /// still/snapshot consumers pay the one owned clone later.
    pub(crate) fn commit_recording_presented_meta(
        &mut self,
        wid: crate::WindowId,
        route: crate::VisibleContentRoute,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        if !self.video_rec.as_ref().is_some_and(|rec| rec.window == wid) {
            return;
        }
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        let input_epoch = match &window.present {
            Some(
                crate::PresentTarget::Gpu { window_gpu, .. }
                | crate::PresentTarget::Virtual { window_gpu },
            ) => window_gpu.resident_input_epoch(),
            _ => 0,
        };
        if input_epoch == 0 || input_epoch == u64::MAX {
            return;
        }
        let serial = window.capture_present_serial;
        let invert = window.capture_present_invert;
        let overlay = window.capture_present_overlay;
        let metrics = window.metrics;
        let cell_size = self.win_cell_size(wid);
        let destination_height = window.win_px.map(|size| size.height.max(1) as usize);
        let overlay_fingerprint = window
            .last_present
            .map_or_else(|| window.overlay_fp(), |key| key.settings_fp);
        let card = Self::recording_card_identity(window.present_card());
        let theme = crate::VideoPresentedThemeIdentity {
            fg: self.theme.fg,
            bg: self.theme.bg,
            cursor: self.theme.cursor,
            selection: self.theme.selection,
        };
        let blink_phase = window.blink_phase;
        let cursor_override = (!window.focused && window.os_window.is_some())
            .then_some(aterm_core::terminal::CursorStyle::HollowBlock);
        let selection_inactive = self.render_knobs.selection_inactive && !window.focused;
        let previous = self
            .video_rec
            .as_mut()
            .filter(|recording| recording.window == wid)
            .and_then(|recording| recording.presented.take());
        let (mut terminal_grid, mut native_leaves) = previous.map_or_else(
            || {
                (
                    crate::app_render::TerminalCaptureGrid {
                        composed: false,
                        focus: None,
                        leaves: Vec::new(),
                    },
                    Vec::new(),
                )
            },
            |mut previous| {
                (
                    previous.terminal_grid.take().unwrap_or(
                        crate::app_render::TerminalCaptureGrid {
                            composed: false,
                            focus: None,
                            leaves: Vec::new(),
                        },
                    ),
                    previous.native_leaves.take().unwrap_or_default(),
                )
            },
        );

        let terminal_grid = if let crate::VisibleContentRoute::Terminal { composed } = route {
            if !self.refill_presented_terminal_capture_grid(wid, composed, plan, &mut terminal_grid)
            {
                return;
            }
            Some(terminal_grid)
        } else {
            None
        };
        let native_leaves = if route.has_visible_native() {
            let Some(_) =
                self.refill_recording_native_leaf_identities(wid, plan, &mut native_leaves)
            else {
                return;
            };
            Some(native_leaves)
        } else {
            None
        };

        let presented = crate::VideoPresentedMeta {
            input_epoch,
            route,
            serial,
            invert,
            overlay,
            terminal_grid,
            metrics,
            cell_size,
            destination_height,
            theme,
            overlay_fingerprint,
            blink_phase,
            cursor_override,
            selection_inactive,
            card,
            native_leaves,
        };
        if let Some(recording) = self
            .video_rec
            .as_mut()
            .filter(|recording| recording.window == wid)
        {
            recording.presented = Some(presented);
        }
    }

    /// Clone the staged model only when a successful-present serial proves it
    /// was the model consumed by a completed submission after `before`.
    fn presented_terminal_capture_after(
        &self,
        wid: crate::WindowId,
        before: u64,
    ) -> Option<PresentedTerminalCapture> {
        let presented = self.presented_frame_capture_after(wid, before)?;
        let grid = self.presented_terminal_capture_grid(wid)?;
        Some(PresentedTerminalCapture {
            input: presented.input,
            grid,
            invert: presented.invert,
            overlay: presented.overlay,
            serial: presented.serial,
            cell_size: self.win_cell_size(wid),
            theme_fingerprint: self.image_theme_fingerprint(),
            overlay_fingerprint: self.windows.get(&wid)?.overlay_fp(),
        })
    }

    /// Clone the staged renderer model only after a successful-present serial
    /// advances. Unlike the terminal-specific projection above, this accepts
    /// every zoom-aware visible route; the route's redraw already populated
    /// `input_scratch` and committed the exact host visual state beside the
    /// serial.
    fn presented_frame_capture_after(
        &self,
        wid: crate::WindowId,
        before: u64,
    ) -> Option<PresentedFrameCapture> {
        let window = self.windows.get(&wid)?;
        let serial = window.capture_present_serial;
        if serial == before {
            return None;
        }
        Some(PresentedFrameCapture {
            input: window.input_scratch.clone(),
            invert: window.capture_present_invert,
            overlay: window.capture_present_overlay,
            serial,
        })
    }

    /// Resolve the recording loop's latest successful application present.
    ///
    /// The lightweight metadata is committed only after video submission
    /// succeeds; the large input stays resident in `WindowGpu`. Every encode
    /// attempt advances that resident epoch, so a later failed or in-flight
    /// attempt makes the old metadata non-consumable instead of letting a still
    /// pair old semantics with newer pixels. Explicit captures alone pay the
    /// owned `RenderInput` clone.
    fn recording_presented_frame_capture(
        &self,
        wid: crate::WindowId,
        include_terminal_grid: bool,
        include_native_metadata: bool,
    ) -> Result<RecordingPresentedFrame, String> {
        let recording = self
            .video_rec
            .as_ref()
            .filter(|recording| recording.window == wid)
            .ok_or_else(|| "image has no active recording for this window".to_string())?;
        let presented = recording
            .presented
            .as_ref()
            .ok_or_else(|| "image deferred: recording has not presented a frame yet".to_string())?;
        let window = self
            .windows
            .get(&wid)
            .ok_or_else(|| "image deferred: recorded window closed before capture".to_string())?;

        let current_route = self.active_visible_content_route(wid);
        let current_plan = self
            .active_visible_leaf_plan(wid)
            .ok_or_else(|| "image deferred: recording layout changed before capture".to_string())?;
        let current_destination_height = window.win_px.map(|size| size.height.max(1) as usize);
        let current_card = Self::recording_card_identity(window.present_card());
        let current_theme = crate::VideoPresentedThemeIdentity {
            fg: self.theme.fg,
            bg: self.theme.bg,
            cursor: self.theme.cursor,
            selection: self.theme.selection,
        };
        let current_cursor_override = (!window.focused && window.os_window.is_some())
            .then_some(aterm_core::terminal::CursorStyle::HollowBlock);
        let current_selection_inactive = self.render_knobs.selection_inactive && !window.focused;
        if current_route != Some(presented.route)
            || window.capture_present_serial != presented.serial
            || window.metrics != presented.metrics
            || self.win_cell_size(wid) != presented.cell_size
            || current_destination_height != presented.destination_height
            || current_theme != presented.theme
            || window.overlay_fp() != presented.overlay_fingerprint
            || window.blink_phase != presented.blink_phase
            || current_cursor_override != presented.cursor_override
            || current_selection_inactive != presented.selection_inactive
            || current_card != presented.card
        {
            return Err(
                "image deferred: recording frame geometry or chrome changed before capture"
                    .to_string(),
            );
        }

        if let Some(grid) = presented.terminal_grid.as_ref() {
            if current_plan.leaves.len() != grid.leaves.len()
                || !current_plan
                    .leaves
                    .iter()
                    .zip(&grid.leaves)
                    .all(|(leaf, captured)| {
                        let session = match self.view_store.get(leaf.view).copied() {
                            Some(crate::tab_model::View::Terminal(terminal)) => terminal.session,
                            _ => return false,
                        };
                        leaf.view == captured.view
                            && session == captured.session
                            && leaf.focused == captured.focused
                            && (leaf.rect.size.height.round() as usize).max(1) == captured.rows
                            && (leaf.rect.size.width.round() as usize).max(1) == captured.cols
                    })
            {
                return Err(
                    "image deferred: recording terminal layout changed before capture".to_string(),
                );
            }
        } else if matches!(presented.route, crate::VisibleContentRoute::Terminal { .. }) {
            return Err("image deferred: recorded terminal inventory is unavailable".to_string());
        }

        if presented.route.has_visible_native() {
            let expected = presented.native_leaves.as_ref().ok_or_else(|| {
                "image deferred: recorded native inventory is unavailable".to_string()
            })?;
            let mut current = Vec::with_capacity(expected.len());
            self.refill_recording_native_leaf_identities(wid, &current_plan, &mut current)
                .ok_or_else(|| {
                    "image deferred: recording native layout changed before capture".to_string()
                })?;
            if &current != expected {
                return Err(
                    "image deferred: recording native content changed before capture".to_string(),
                );
            }
        }

        let native_metadata = if include_native_metadata && presented.route.has_visible_native() {
            Some(self.native_image_metadata(wid, "presented", Some(presented.serial), 0, 0, 0)?)
        } else {
            None
        };

        let window_gpu = match window.present.as_ref() {
            Some(
                crate::PresentTarget::Gpu { window_gpu, .. }
                | crate::PresentTarget::Virtual { window_gpu },
            ) => window_gpu,
            _ => {
                return Err("image deferred: recording resident frame is unavailable".to_string());
            }
        };
        let (mut input, effects_transport_shift_y) = window_gpu
            .clone_resident_input_with_transport_at(presented.input_epoch)
            .ok_or_else(|| {
                "image deferred: recording advanced before capture could clone its frame"
                    .to_string()
            })?;
        if effects_transport_shift_y != 0 {
            let canonical_shift = effects_transport_shift_y.checked_neg().ok_or_else(|| {
                "image deferred: recording effect transport shift is invalid".to_string()
            })?;
            if !crate::try_shift_window_absolute_effects_y(&mut input, canonical_shift) {
                return Err(
                    "image deferred: recording effect transport could not be canonicalized"
                        .to_string(),
                );
            }
        }
        Ok(RecordingPresentedFrame {
            frame: PresentedFrameCapture {
                input,
                invert: presented.invert,
                overlay: presented.overlay,
                serial: presented.serial,
            },
            terminal_grid: include_terminal_grid
                .then(|| presented.terminal_grid.clone())
                .flatten(),
            cell_size: presented.cell_size,
            theme_fingerprint: self.image_theme_fingerprint(),
            overlay_fingerprint: presented.overlay_fingerprint,
            native_metadata,
        })
    }

    /// Read the CURRENT persistent VirtualTarget without forcing another
    /// present. The application serial binds the frontend success seam and the
    /// GPU's submitted-input epoch binds the target generation. Live knob,
    /// chrome and layout state are intentionally irrelevant to these extant
    /// pixels; semantic callers validate those separately when they need
    /// metadata, text, or a clean reraster.
    fn recording_presented_destination_capture(
        &mut self,
        wid: crate::WindowId,
    ) -> Result<RecordingPresentedDestination, String> {
        let (input_epoch, serial, route) = {
            let recording = self
                .video_rec
                .as_ref()
                .filter(|recording| recording.window == wid)
                .ok_or_else(|| "image has no active recording for this window".to_string())?;
            let presented = recording.presented.as_ref().ok_or_else(|| {
                "image deferred: recording has not presented a frame yet".to_string()
            })?;
            (presented.input_epoch, presented.serial, presented.route)
        };
        let window = self
            .windows
            .get(&wid)
            .ok_or_else(|| "image deferred: recorded window closed before capture".to_string())?;
        if window.capture_present_serial != serial {
            return Err(
                "image deferred: recording destination advanced before capture".to_string(),
            );
        }
        if window.os_window.is_some() {
            return Err(
                "image deferred: recording destination is not a headless virtual target"
                    .to_string(),
            );
        }

        let App {
            backend, windows, ..
        } = self;
        let gpu = backend.gpu_mut().ok_or_else(|| {
            "image deferred: recording virtual GPU backend is unavailable".to_string()
        })?;
        let window_gpu = match windows.get(&wid).and_then(|window| window.present.as_ref()) {
            Some(crate::PresentTarget::Virtual { window_gpu }) => window_gpu,
            _ => {
                return Err(
                    "image deferred: recording virtual destination is unavailable".to_string(),
                );
            }
        };
        let captured = gpu.virtual_presented_snapshot_current(
            window_gpu,
            input_epoch,
            crate::metrics::now_us(),
        )?;
        let client = crate::PresentedClientFrame {
            width: captured.w,
            height: captured.h,
            rgba: captured.rgba,
        };
        Ok(RecordingPresentedDestination {
            frame: snapshot_frame_from_presented_client(&client)?,
            route,
            serial,
        })
    }

    /// Route-neutral successful-present barrier. Windowed terminal, native and
    /// heterogeneous captures all authorize pixels through the same serial.
    /// Headless callers receive `None` and stage one explicit present-real frame.
    fn present_before_frame_capture(
        &mut self,
        wid: crate::WindowId,
    ) -> Result<Option<PresentedFrameCapture>, String> {
        let has_os_window = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.os_window.is_some());
        if !has_os_window {
            return Ok(None);
        }

        let mut captured = None;
        let presented =
            crate::run_capture_present_barrier(crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, || {
                let before = self
                    .windows
                    .get(&wid)
                    .map_or(0, |window| window.capture_present_serial);
                let Some(window) = self.windows.get_mut(&wid) else {
                    return false;
                };
                let _ = window.present_retry.on_external_stimulus();
                window.last_present = None;
                self.redraw_window(wid);
                captured = self.presented_frame_capture_after(wid, before);
                captured.is_some()
            });
        if presented {
            Ok(captured)
        } else {
            Err(format!(
                "frame capture could not synchronize a coherent presented frame after {} present attempts",
                crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT,
            ))
        }
    }

    /// Synchronize a windowed pure-terminal capture with one application-present
    /// submission. Each attempt is an explicit external presentation stimulus:
    /// it reopens a parked/backoff retry episode and clears the optimistic
    /// repaint stamp so the redraw cannot early-out. Only a serial advance may
    /// authorize the immediate owned snapshot. Three dropped/held attempts fail
    /// closed; pre-first-present follows this same path.
    fn present_before_terminal_capture(
        &mut self,
        wid: crate::WindowId,
    ) -> Result<Option<PresentedTerminalCapture>, String> {
        let has_os_window = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.os_window.is_some());
        if !has_os_window {
            return Ok(None);
        }

        let mut captured = None;
        let presented =
            crate::run_capture_present_barrier(crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, || {
                let before = self
                    .windows
                    .get(&wid)
                    .map_or(0, |window| window.capture_present_serial);
                let Some(window) = self.windows.get_mut(&wid) else {
                    return false;
                };
                let _ = window.present_retry.on_external_stimulus();
                window.last_present = None;
                self.redraw_window(wid);

                captured = self.presented_terminal_capture_after(wid, before);
                captured.is_some()
            });
        if presented {
            Ok(captured)
        } else {
            Err(format!(
                "terminal capture could not synchronize a coherent presented frame after {} present attempts",
                crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT,
            ))
        }
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
    #[cfg(test)]
    fn splice_cursor_fx(&mut self, wid: crate::WindowId, now: Instant) {
        // LEGACY TEST HELPER: the headless capture tick below drives the
        // SINGLE-PANE effect machinery (front-terminal cursor, window-content
        // coords) — on a composed multi-pane frame its quads would land at
        // un-offset positions over the composite. A windowed capture keeps
        // its last-present quads (already composed-correct) via the gate
        // inside, while shipping composed captures use the composed preparation
        // path instead of this helper. Keep this helper split-gated so tests can
        // never stamp single-pane coordinates onto a composed frame.
        if self
            .active_tree(wid)
            .is_some_and(|t| t.len() > 1 && !t.is_zoomed())
        {
            return;
        }
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
            let content_scroll_state = term.content_scroll_state();
            let is_alt = term.is_alternate_screen();
            // Invalidation resets engine context; apply it before this exact
            // capture re-feeds alt/blink state below.
            sync_cursor_effect_coordinate_space(ws, term.render_identity(), is_alt);
            let scroll_change = sync_cursor_effect_scroll(ws, content_scroll_state);
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
            // `splice_cursor_fx` only ticks a single/zoomed pane, in the
            // capture grid's zero-based coordinate space. Restamp that shape
            // explicitly so a prior composed-frame offset cannot survive a
            // split/zoom transition and misclassify the physical margin.
            ws.cursor_glow.note_pane_columns(0, cols);
            ws.cursor_trail.note_pane_columns(0, cols);
            let blink_recent = ws.last_blink_at.is_some_and(|t| {
                now.saturating_duration_since(t) <= crate::app_render::BLINK_RECENT_MAX
            });
            // Plain-alt frames ship a ContentOnly probe instead of none —
            // the `no-row-probe` repair (less /-search, vi insert, the
            // ESC 7/ESC 8 streamer TUI); see `app_render::row_probe_trust`.
            let probe_trust = crate::app_render::row_probe_trust(is_alt, blink_recent);
            let row_probe = if display_offset == 0 && !scroll_change.changed() {
                let _fill = term.row_cols_into(cpos.row as usize, &mut ws.poof_row_buf);
                // STAR-LANDING NEIGHBORS — the windowed LOCK A capture's
                // twin, so a headless capture licenses (or forbids) the
                // displaced rainbow kitty stars exactly as a windowed present would.
                if cpos.row > 0 {
                    term.row_cols_into(cpos.row as usize - 1, &mut ws.poof_row_above_buf);
                }
                if (cpos.row as usize) + 1 < rows {
                    term.row_cols_into(cpos.row as usize + 1, &mut ws.poof_row_below_buf);
                }
                Some((cpos.row, cpos.col, probe_trust))
            } else {
                None
            };
            crate::app_render::CursorFxInputs {
                now,
                rows,
                cols,
                // Scrolled into history ⇒ no cursor for the effect engines — the
                // windowed path's twin (active-grid coords over scrollback rows
                // would spawn light on unrelated history lines in the capture).
                cur: (cursor_visible && display_offset == 0).then_some((cpos.row, cpos.col)),
                live_viewport: display_offset == 0,
                cursor_visible,
                cursor_style: term.cursor_style(),
                blink_phase: ws.blink_phase,
                live_cursor_rgb: crate::app_render::terminal_cursor_rgb(&term),
                default_bg: aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]),
                // THE PAIR FROM ONE BLANK CELL. The hand-rolled `dbg` branch
                // above swaps in the BACKGROUND direction only; asking the
                // blank cell for both is what keeps a DECSCNM-swapped capture
                // coherent, and it is the same expression the windowed twin
                // uses.
                default_fg: aterm_render::rgb_to_u32(
                    crate::app_render::terminal_blank_cell(&term).fg,
                ),
                row_probe,
                row_probe_neighbors: None,
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
        // Shape and the canonical resolved fill, mirroring `redraw_window`.
        // `tick_cursor_fx` already applied precedence and presentation opacity;
        // rebuilding the seven raw candidates here would bypass the shed fade.
        ws.input_scratch.cursor_effect_style_override = if fx.bolt_cursor {
            Some(aterm_core::terminal::CursorStyle::Bolt)
        } else if fx.twinkle_cursor {
            Some(aterm_core::terminal::CursorStyle::SteadyBlock)
        } else {
            None
        };
        ws.input_scratch.cursor_fill_override = fx.block_fill.map(|owned| owned.fill);
        ws.input_scratch.cursor_trail.clone_from(&ws.trail_scratch);
        ws.input_scratch.cursor_trail_color = fx.trail_color;
    }

    /// Introspect the current visible route and write PNG + semantic text.
    ///
    /// With a window, the PNG is the exact client destination copied beside one
    /// successful present submission. That includes raw destination bands and
    /// present-only GPU colour/glow passes, but does not claim OS compositor
    /// visibility or platform titlebar/backdrop pixels. Headless has no surface
    /// transaction, so its PNG is explicitly the semantic renderer output and may
    /// omit present-only passes. The parallel `.txt` is projected from the same
    /// successful-present serial when windowed. Triggered by SIGUSR1. The files
    /// are written 0600 into the per-user 0700 control dir;
    /// $ATERM_SNAPSHOT_PATH overrides only into a safe dir (see `snapshot_path`).
    fn snapshot_visible_text(
        &self,
        wid: WindowId,
        route: crate::VisibleContentRoute,
        input: &aterm_render::RenderInput,
        strip_rows: usize,
        cols: usize,
    ) -> String {
        let terminal = terminal_capture_text(input, strip_rows, cols);
        let native = if route.has_visible_native() {
            self.active_visible_leaf_plan(wid)
                .zip(self.windows.get(&wid))
                .map(|(plan, window)| {
                    let mut native = String::new();
                    for leaf in &plan.leaves {
                        if !matches!(
                            self.view_store.get(leaf.view),
                            Some(crate::tab_model::View::Native(_))
                        ) {
                            continue;
                        }
                        native.push_str(&format!(
                            "[native view={} focused={}]\n",
                            leaf.view.get(),
                            leaf.focused
                        ));
                        let Some(raster) = window
                            .leaf_render_cache
                            .get(&leaf.view)
                            .and_then(|cache| cache.native.as_ref())
                        else {
                            native.push_str("semantics unavailable\n");
                            continue;
                        };
                        for line in crate::app_control::semantic_text_lines(&raster.compiled) {
                            native.push_str(&line);
                            native.push('\n');
                        }
                    }
                    native
                })
        } else {
            None
        };
        let mut visible = match native {
            None => terminal,
            Some(native) => match route {
                crate::VisibleContentRoute::Native { .. } => native,
                crate::VisibleContentRoute::Heterogeneous if native.is_empty() => terminal,
                crate::VisibleContentRoute::Heterogeneous if terminal.is_empty() => native,
                crate::VisibleContentRoute::Heterogeneous => format!("{terminal}{native}"),
                crate::VisibleContentRoute::Terminal { .. } => terminal,
            },
        };
        if let Some(palette) = self.windows.get(&wid).and_then(|window| window.palette()) {
            if !visible.is_empty() && !visible.ends_with('\n') {
                visible.push('\n');
            }
            for line in palette.controls_lines() {
                visible.push_str(&line);
                visible.push('\n');
            }
        }
        visible
    }

    pub(crate) fn snapshot(&mut self) {
        let Some(path) = snapshot_path::resolve() else {
            return; // refusal already logged by resolve()
        };
        let transaction = match begin_snapshot_generation(std::path::Path::new(&path)) {
            Ok(transaction) => transaction,
            Err(error) => {
                eprintln!("aterm-gui: snapshot refused for {path}: {error}");
                return;
            }
        };
        let Some(front) = self.frontmost_window else {
            return;
        };
        let Some(route) = self.active_visible_content_route(front) else {
            return;
        };
        // A snapshot is a PIXEL demand: redeem a headless launch's deferred GPU
        // intent before anything reads the renderer, so the artifact this writes
        // is the one a boot-built device would have written. No-op everywhere
        // else (windowed, and headless once redeemed or declined).
        self.ensure_pixel_backend();
        let strip_rows = usize::from(self.chrome_rows());
        let has_os_window = self
            .windows
            .get(&front)
            .is_some_and(|window| window.os_window.is_some());
        let recording_presented = if !has_os_window
            && self
                .video_rec
                .as_ref()
                .is_some_and(|recording| recording.window == front)
        {
            match self.recording_presented_frame_capture(front, false, false) {
                Ok(presented) => Some(presented),
                Err(error) => {
                    eprintln!("aterm-gui: snapshot deferred: {error}");
                    return;
                }
            }
        } else {
            None
        };
        let recording_destination = if recording_presented.is_some() {
            match self.recording_presented_destination_capture(front) {
                Ok(destination) => Some(destination),
                Err(error) => {
                    eprintln!("aterm-gui: snapshot deferred: {error}");
                    return;
                }
            }
        } else {
            None
        };
        let route = recording_destination
            .as_ref()
            .map_or(route, |presented| presented.route);
        let exact_presented = if has_os_window {
            let has_gpu_surface = self.windows.get(&front).is_some_and(|window| {
                matches!(window.present, Some(crate::PresentTarget::Gpu { .. }))
            });
            if let Err(error) = crate::window_capture_translucency_guard(
                self.render_knobs.background_opacity,
                cfg!(target_os = "macos"),
                self.backend.is_gpu(),
                has_gpu_surface,
            ) {
                eprintln!("aterm-gui: snapshot refused: {error}");
                return;
            }
            if let Err(error) = crate::window_capture_material_guard(
                self.render_knobs.background_material,
                !cfg!(windows),
            ) {
                eprintln!("aterm-gui: snapshot refused: {error}");
                return;
            }
            match self.present_before_window_capture(front) {
                Ok(presented) => Some(presented),
                Err(error) => {
                    eprintln!("aterm-gui: snapshot refused: {error}");
                    return;
                }
            }
        } else {
            None
        };
        let presented = exact_presented
            .as_ref()
            .map(|presented| presented.frame.clone())
            .or_else(|| recording_presented.map(|presented| presented.frame));
        if presented.is_none() {
            // Headless has no surface destination to copy. Stage one
            // zoom-aware semantic-renderer artifact; this path intentionally
            // makes no byte-identity claim about present-only transforms.
            match route {
                crate::VisibleContentRoute::Terminal { .. } => {
                    // SPLIT TABS: the SIGUSR1 snapshot shares the `image` verb's
                    // composed-capture recipe — a terminal-only multi-pane tab writes
                    // the divider grid + per-pane composite (each pane keeping its OWN
                    // live default background), never the front pane stretched over the
                    // window. Single-pane keeps the historical front-terminal refill,
                    // byte-for-byte. The pass takes the presentation path's grid without
                    // its semantics: it reconciles no predictions and stamps no
                    // glass present. The advancing headless arm consumes exactly
                    // the damage paired with this explicit present-real capture.
                    let capture_now = Instant::now();
                    let clock = self.composed_capture_cursor_fx_clock(front, capture_now);
                    let Some(capture_grid) =
                        self.prepare_terminal_capture_grid_with_cursor_fx(front, clock)
                    else {
                        return;
                    };
                    let Some(capture_focus) = capture_grid.focus else {
                        return;
                    };
                    // Both one-pane and composed captures need the companion
                    // splice. The helper dispatches the split path itself and
                    // retains (rather than advances) under a recording owner.
                    self.splice_word_decorations_sampled(front, capture_now, capture_focus);
                    self.splice_tab_strip(front);
                    self.splice_find_bar(front);
                    self.splice_settings_panel(front);
                    self.splice_build_badge(front);
                    self.splice_notice(front);
                    self.splice_level_up(front);
                }
                crate::VisibleContentRoute::Native { .. } => {
                    if !self.prepare_native_input_scratch(front) {
                        return;
                    }
                    self.splice_find_bar(front);
                    self.splice_build_badge(front);
                    self.splice_notice(front);
                    self.splice_level_up(front);
                    if !self.compose_native_route_card(front) {
                        return;
                    }
                }
                crate::VisibleContentRoute::Heterogeneous => {
                    let clock = self.composed_capture_cursor_fx_clock(front, Instant::now());
                    if self
                        .prepare_heterogeneous_input_scratch_with_cursor_fx(front, Some(clock))
                        .is_none()
                    {
                        return;
                    }
                    self.splice_find_bar(front);
                    self.splice_build_badge(front);
                    self.splice_notice(front);
                    self.splice_level_up(front);
                    if !self.compose_native_route_card(front) {
                        return;
                    }
                }
            }
            self.splice_config_notice(front);
            self.splice_paste_banner(front);
            // A capture that omitted the link caption would tell a driving AI a
            // hyperlink is undisclosed while a human is reading its destination.
            // Last of the row bands, as on the presentation routes: it takes
            // only a row no other band declared.
            self.splice_link_target(front);
            // C5: the open tab context menu is the topmost chrome on the glass,
            // so it must be the topmost chrome in the capture too — an
            // introspection frame that omits it would tell a driving AI the
            // menu is closed while a human is looking at it. Same last-of-all
            // position as the presentation routes. A no-op with none open.
            self.splice_tab_menu(front);
        }
        let cols = match self.windows.get(&front) {
            Some(ws) => ws.cols as usize,
            None => return,
        };
        let presented_visuals = presented
            .as_ref()
            .map(|presented| (presented.invert, presented.overlay));
        let mut capture_input = match presented {
            Some(presented) => presented.input,
            None => self.windows[&front].input_scratch.clone(),
        };
        let text = self.snapshot_visible_text(front, route, &capture_input, strip_rows, cols);
        if let Some(recording_destination) = recording_destination {
            self.submit_encode_job(EncodeJob::Snapshot {
                frame: recording_destination.frame,
                text,
                transaction,
            });
            return;
        }
        if let Some(exact) = exact_presented.as_ref() {
            let frame = match snapshot_frame_from_presented_client(&exact.client) {
                Ok(frame) => frame,
                Err(error) => {
                    eprintln!("aterm-gui: snapshot refused: {error}");
                    return;
                }
            };
            self.submit_encode_job(EncodeJob::Snapshot {
                frame,
                text,
                transaction,
            });
            return;
        }
        // Accent for the drop-target highlight / level-up glow, read before the disjoint
        // borrow. The level-up glow's breathing alphas are sampled here too, matching
        // application-present transient state.
        let accent = self.theme.cursor;
        let level_up_glow = self
            .level_up
            .as_ref()
            .map(|l| (l.wash_alpha(Instant::now()), l.border_alpha(Instant::now())));
        let tray_floor_y = self.config_notice_tray_floor_y(front);
        self.bind_window_renderer_state(front);
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
        // Headless semantic-renderer pixels. `backend.render_input_for_destination`
        // returns an owned Frame on both backends; no surface exists, so this
        // branch does not claim swapchain byte identity.
        // P3 settings card → raw bytes + device-px rect for the GPU tray quad (same
        // builder application-present uses). The GPU arm bakes it into the offscreen so
        // app-present and capture share composition; the CPU arm ignores it here
        // (composited below, gated on !is_gpu).
        // Modal card FIRST, else the transient update notice, else the build/version badge.
        let tray_arg = ws
            .route_card
            .as_ref()
            .or(ws.settings_card.as_ref())
            .or(ws.conn_wire_card.as_ref())
            .or(ws.level_up_card.as_ref())
            .or(ws.notice_card.as_ref())
            .or(ws.badge_card.as_ref())
            .and_then(|card| tray_quad_below_y(card, tray_floor_y));
        let destination_height = ws.win_px.map(|size| size.height.max(1) as usize);
        let mut frame = match backend.render_input_for_destination(
            introspect_gpu,
            &mut capture_input,
            tray_arg,
            destination_height,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                eprintln!("aterm-gui: snapshot refused: {error}");
                return;
            }
        };
        if !backend.is_gpu()
            && let Some(quad) = ws
                .route_card
                .as_ref()
                .or(ws.settings_card.as_ref())
                .or(ws.conn_wire_card.as_ref())
                .or(ws.level_up_card.as_ref())
                .or(ws.notice_card.as_ref())
                .or(ws.badge_card.as_ref())
                .and_then(|card| tray_quad_below_y(card, tray_floor_y))
        {
            composite_tray_quad_at(&mut frame.pixels, frame.width, frame.height, 0, 0, quad);
        }
        // Match application-present visual-bell composition (CPU
        // `src ^ 0x00ff_ffff`; GPU blit shader) so capture preserves the same
        // app-owned transient state. Suppress it under any modal overlay, as the
        // application-present path does. The compositor and scanout remain
        // outside this comparison.
        let invert = presented_visuals.map_or_else(
            || ws.bell_flash.is_active(Instant::now()) && !ws.overlay_open(),
            |(invert, _)| invert,
        );
        apply_bell_invert(&mut frame, invert);
        // Match application-present drop-target/LEVEL-UP overlay composition so
        // capture preserves the same app-owned transient state. Suppressed under
        // a modal, matching the live `!overlay_open` gate.
        let retained_overlay = presented_visuals.and_then(|(_, overlay)| overlay);
        if let Some(overlay) = retained_overlay {
            apply_overlay_at(
                &mut frame.pixels,
                frame.width,
                frame.height,
                0,
                0,
                frame.width,
                frame.height,
                overlay,
            );
        } else if presented_visuals.is_none() {
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
        }
        // `text` was projected before the disjoint renderer borrow. Terminal
        // rows share the accessibility serializer; native rows come from the
        // exact retained `CompiledUi` that supplied this route's raster.
        // Deflate + writes belong on the encode worker (the same 50–150 ms Retina
        // stall the `image` verb routes around); the `.done` marker is written by
        // the worker LAST, so the requester's stat() contract is unchanged.
        self.submit_encode_job(EncodeJob::Snapshot {
            frame,
            text,
            transaction,
        });
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
    /// application-present render. Native views have no terminal to snapshot; their semantic
    /// tray is nevertheless a first-class framebuffer and must remain visible
    /// through the canonical `image` verb.
    ///
    /// The request travels as one value ([`NativeImageRequest`]) rather than as
    /// nine positional arguments: the capture, authority, cancellation, metadata
    /// and reply state are independent, so naming them at every call site is
    /// what keeps them from being silently reordered.
    fn render_native_image(&mut self, request: NativeImageRequest<'_>) {
        let NativeImageRequest {
            front,
            clean,
            presented,
            presented_metadata,
            exact_frame,
            target,
            want_bytes,
            want_metadata,
            cancel,
            frame_metadata,
            reply,
        } = request;
        if cancel.is_cancelled() {
            let _ = reply.send(Err("image request cancelled before render".to_string()));
            return;
        }
        // PIXEL demand — see `render_image`. Redeemed here too rather than only at
        // the caller: this is the entry the direct (test) callers use, and a second
        // call from `render_image` is a no-op.
        self.ensure_pixel_backend();
        let exact_frame = (!clean).then_some(exact_frame).flatten();
        if presented.is_none() && exact_frame.is_none() {
            let prepared = match self.active_visible_content_route(front) {
                Some(crate::VisibleContentRoute::Heterogeneous) => {
                    let clock = self.composed_capture_cursor_fx_clock(front, Instant::now());
                    match self.prepare_heterogeneous_input_scratch_with_cursor_fx_outcome(
                        front,
                        Some(clock),
                    ) {
                        crate::app_render::CapturePreparation::Ready(_) => true,
                        crate::app_render::CapturePreparation::Held { retry_at } => {
                            let retry_ms = retry_at
                                .saturating_duration_since(Instant::now())
                                .as_millis()
                                .max(1);
                            let _ = reply.send(Err(format!(
                                "image deferred: synchronized terminal update in progress; retry after {retry_ms} ms"
                            )));
                            return;
                        }
                        crate::app_render::CapturePreparation::Unavailable => false,
                    }
                }
                Some(crate::VisibleContentRoute::Native { .. }) => {
                    self.prepare_native_input_scratch(front)
                }
                Some(crate::VisibleContentRoute::Terminal { .. }) | None => false,
            };
            if !prepared {
                let _ = reply.send(Ok(crate::control::Retained::plain((0, 0, None))));
                return;
            }
            self.splice_find_bar(front);
            self.splice_build_badge(front);
            self.splice_notice(front);
            self.splice_level_up(front);
            if !self.compose_native_route_card(front) {
                let _ = reply.send(Ok(crate::control::Retained::plain((0, 0, None))));
                return;
            }
            // Native preparation leaves the semantic surface in the tray. Paint
            // diagnostic cells afterward, exactly like the application-present path.
            self.splice_config_notice(front);
            self.splice_paste_banner(front);
            self.splice_link_target(front);
            // C5 — topmost chrome; see the `chrome`-capture route above.
            self.splice_tab_menu(front);
        }
        let (presented_visuals, capture_serial, mut capture_input) = match presented {
            Some(presented) => (
                Some(crate::app_render::HostVisualState {
                    invert: presented.invert,
                    overlay: presented.overlay,
                }),
                Some(presented.serial),
                presented.input,
            ),
            None => (None, None, self.windows[&front].input_scratch.clone()),
        };
        let visuals =
            presented_visuals.unwrap_or_else(|| self.host_visual_state(front, Instant::now()));
        let phase = if presented_visuals.is_some() {
            "presented"
        } else {
            "staged"
        };
        if clean {
            capture_input.clear_overlays();
        }
        let frame = if let Some(frame) = exact_frame {
            // Already post-blit/post-crown/post-chrome. Re-applying any live
            // renderer or host state here would corrupt exact-present semantics.
            frame
        } else {
            let tray_floor_y = self.config_notice_tray_floor_y(front);
            self.bind_window_renderer_state(front);
            let render_t0 = Instant::now();
            let App {
                backend,
                introspect_gpu,
                windows,
                ..
            } = self;
            let Some(ws) = windows.get_mut(&front) else {
                let _ = reply.send(Ok(crate::control::Retained::plain((0, 0, None))));
                return;
            };
            let crate::WindowState {
                route_card,
                settings_card,
                conn_wire_card,
                level_up_card,
                notice_card,
                badge_card,
                win_px,
                ..
            } = ws;
            let tray_arg = route_card
                .as_ref()
                .or(settings_card.as_ref())
                .or(conn_wire_card.as_ref())
                .or(level_up_card.as_ref())
                .or(notice_card.as_ref())
                .or(badge_card.as_ref())
                .and_then(|card| tray_quad_below_y(card, tray_floor_y));
            let destination_height = win_px.map(|size| size.height.max(1) as usize);
            let mut frame = match backend.render_input_for_destination(
                introspect_gpu,
                &mut capture_input,
                tray_arg,
                destination_height,
            ) {
                Ok(frame) => frame,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            let cpu_tray = (!backend.is_gpu()).then_some(tray_arg).flatten();
            let (frame_width, frame_height) = (frame.width, frame.height);
            apply_host_chrome_at(
                &mut frame.pixels,
                frame_width,
                frame_height,
                0,
                0,
                frame_width,
                frame_height,
                cpu_tray,
                visuals.invert,
                visuals.overlay,
            );
            let render_ns = render_t0.elapsed().as_nanos() as u64;
            crate::metrics::record_offscreen_raster(render_ns);
            frame
        };
        if want_metadata {
            let Ok(width) = u32::try_from(frame.width) else {
                let _ = reply.send(Err("native image metadata width overflow".to_string()));
                return;
            };
            let Ok(height) = u32::try_from(frame.height) else {
                let _ = reply.send(Err("native image metadata height overflow".to_string()));
                return;
            };
            let pixel_fingerprint =
                Self::image_pixel_fingerprint(frame.width, frame.height, &frame.pixels);
            let metadata = if let Some(mut metadata) = presented_metadata {
                metadata.width = width;
                metadata.height = height;
                metadata.pixel_fingerprint = pixel_fingerprint;
                Ok(metadata)
            } else {
                self.native_image_metadata(
                    front,
                    phase,
                    capture_serial,
                    width,
                    height,
                    pixel_fingerprint,
                )
            };
            match metadata {
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
            cancel,
            reply,
        });
    }

    fn native_image_metadata(
        &self,
        front: WindowId,
        phase: &'static str,
        capture_serial: Option<u64>,
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
            .route_card
            .as_ref()
            .or(window.settings_card.as_ref())
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
            capture_serial: capture_serial.unwrap_or(window.capture_present_serial),
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

    /// Hash the exact terminal-owned model fields a renderer consumes.
    ///
    /// `damage_epoch`/`snapshot_seq` is deliberately not sufficient: damage is
    /// latched until a real present consumes it, while explicit captures are
    /// observational and may extract several different grids inside one
    /// outstanding damage session. Hashing cells and every sparse companion
    /// keeps idle captures stable and still distinguishes output that lands
    /// between two capture requests.
    fn hash_terminal_render_model<H: std::hash::Hasher>(
        input: &aterm_render::RenderInput,
        hash: &mut H,
    ) {
        use std::hash::Hash;

        "terminal-render-model-v5".hash(hash);
        input.rows.hash(hash);
        input.cols.hash(hash);
        for row in &input.cells {
            row.len().hash(hash);
            for cell in row {
                cell.ch.hash(hash);
                cell.fg.hash(hash);
                cell.bg.hash(hash);
                cell.wide.hash(hash);
                cell.emoji_presentation.hash(hash);
                cell.text_presentation.hash(hash);
                cell.bold.hash(hash);
                cell.italic.hash(hash);
                std::mem::discriminant(&cell.underline).hash(hash);
                cell.strikethrough.hash(hash);
                cell.overline.hash(hash);
                cell.underline_color.hash(hash);
            }
        }
        input.clusters.hash(hash);
        input.combining.hash(hash);

        // Image payloads can be large and one Arc appears in every covered
        // cell. Hash each unique payload once, while every placement/tile still
        // contributes its row/column coordinates.
        let mut seen_images = std::collections::HashSet::new();
        for row in &input.images {
            row.len().hash(hash);
            for (col, image_ref) in row {
                col.hash(hash);
                image_ref.cell_row.hash(hash);
                image_ref.cell_col.hash(hash);
                let identity = std::sync::Arc::as_ptr(&image_ref.image) as usize;
                let first = seen_images.insert(identity);
                first.hash(hash);
                if first {
                    let image = image_ref.image.as_ref();
                    std::mem::discriminant(&image.format).hash(hash);
                    if let aterm_core::grid::extra::ImageFormat::RawRgba8 { width, height } =
                        image.format
                    {
                        width.hash(hash);
                        height.hash(hash);
                    }
                    image.cols.hash(hash);
                    image.rows.hash(hash);
                    image.z_index.hash(hash);
                    image.bytes.hash(hash);
                }
            }
        }
        for line_size in &input.line_sizes {
            std::mem::discriminant(line_size).hash(hash);
        }
        for row in &input.line_size_spans {
            row.len().hash(hash);
            for span in row {
                span.start_col.hash(hash);
                span.end_col.hash(hash);
                std::mem::discriminant(&span.line_size).hash(hash);
            }
        }
        for row in &input.default_bg_spans {
            row.len().hash(hash);
            for span in row {
                span.start_col.hash(hash);
                span.end_col.hash(hash);
                span.default_bg.hash(hash);
            }
        }

        input.display_offset.hash(hash);
        input.base_y.hash(hash);
        input.absolute_row_revision.hash(hash);
        input.cursor_row.hash(hash);
        input.cursor_col.hash(hash);
        input.cursor_visible.hash(hash);
        std::mem::discriminant(&input.cursor_style).hash(hash);
        input.default_bg.hash(hash);
        input.cursor_color.hash(hash);
        input.selection_clip.hash(hash);
        input.selection_bg.hash(hash);
        input.selection_fg.hash(hash);
        input.selection.has_selection().hash(hash);
        std::mem::discriminant(&input.selection.state()).hash(hash);
        std::mem::discriminant(&input.selection.selection_type()).hash(hash);
        let last_col = u16::try_from(input.cols.saturating_sub(1)).unwrap_or(u16::MAX);
        if let Some(selection) = input.selection.project_range(last_col) {
            true.hash(hash);
            selection.start_row.hash(hash);
            selection.start_col.hash(hash);
            selection.end_row.hash(hash);
            selection.end_col.hash(hash);
            selection.is_block.hash(hash);
        } else {
            false.hash(hash);
        }
        // v5: the PER-PANE selection list. On a composed split frame this is the
        // renderer's actual selection authority — the scalar fields above are
        // only the focused pane's — so a capture that hashed just the scalars
        // would report two visibly different frames as the same model whenever
        // an unfocused pane's highlight moved. Empty (and therefore
        // hash-identical to v4 modulo the tag) on every single-terminal frame.
        input.selections.len().hash(hash);
        for pane in &input.selections {
            pane.clip.hash(hash);
            pane.bg.hash(hash);
            pane.fg.hash(hash);
            pane.inactive.hash(hash);
            pane.selection.has_selection().hash(hash);
            std::mem::discriminant(&pane.selection.state()).hash(hash);
            std::mem::discriminant(&pane.selection.selection_type()).hash(hash);
            if let Some(projected) = pane.selection.project_range(last_col) {
                true.hash(hash);
                projected.start_row.hash(hash);
                projected.start_col.hash(hash);
                projected.end_row.hash(hash);
                projected.end_col.hash(hash);
                projected.is_block.hash(hash);
            } else {
                false.hash(hash);
            }
        }
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
        Self::hash_terminal_render_model(input, &mut snapshot);
        snapshot.finish() | 1
    }

    /// Identity of either the historical one-terminal snapshot or every exact
    /// leaf extraction that fed a pure-terminal composite.  The one-leaf arm is
    /// intentionally the old function verbatim so existing metadata identities
    /// remain stable.
    fn terminal_capture_fingerprint(
        capture: &crate::app_render::TerminalCaptureGrid,
        input: &aterm_render::RenderInput,
    ) -> u64 {
        if !capture.composed
            && let [leaf] = capture.leaves.as_slice()
        {
            return Self::terminal_snapshot_fingerprint(leaf.view.get(), leaf.session, input);
        }

        use std::hash::{Hash, Hasher};

        let mut snapshot = std::collections::hash_map::DefaultHasher::new();
        "terminal-composite-v1".hash(&mut snapshot);
        Self::hash_terminal_render_model(input, &mut snapshot);
        for leaf in &capture.leaves {
            leaf.view.get().hash(&mut snapshot);
            leaf.session.hash(&mut snapshot);
            leaf.focused.hash(&mut snapshot);
            leaf.rows.hash(&mut snapshot);
            leaf.cols.hash(&mut snapshot);
            leaf.snapshot_seq.hash(&mut snapshot);
        }
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

    /// The `panes` control verb's payload: a window's ACTIVE-tab pane layout
    /// as parse-stable `k=v` rows (split-pane audit introspection — a
    /// headless driver asserts split state, reads zoom, and aims per-pane
    /// mouse/focus actions off these cell rects). One `layout …` header, then
    /// one `pane …` row per VISIBLE pane (a zoomed tab reports the single
    /// zoomed rect — what the glass shows). Cell coords; the 1-cell divider
    /// gaps are exactly the cells no rect covers. `session` = cross-session
    /// target: the window whose ACTIVE tab displays it (the `image` routing
    /// rule, so `@<sid> panes` and `@<sid> image` describe the SAME window).
    /// Empty when no window resolves (the verb frames `OK 0` — for a cross
    /// target that means "no window displays this session", never a silent
    /// fallback to the front window).
    pub(crate) fn read_pane_layout(&self, session: Option<u64>) -> Vec<String> {
        let win = match session {
            Some(id) => self.windows_displaying(id).next(),
            None => self.frontmost_window,
        };
        let Some(wid) = win else {
            return Vec::new();
        };
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let active = ws.tab_set.active_index().unwrap_or(0);
        let Some(tree) = self.active_tree(wid) else {
            // Native / mixed tree: no terminal pane geometry to report — an
            // honest empty layout, never a guess.
            return vec![format!(
                "layout tab={active} panes=0 zoomed=false terminal=false"
            )];
        };
        let focus = tree.focus();
        let rects = tree.compute_layout(ws.rows, ws.cols);
        let mut lines = Vec::with_capacity(rects.len() + 1);
        lines.push(format!(
            "layout tab={} panes={} zoomed={} terminal=true",
            active,
            rects.len(),
            tree.is_zoomed(),
        ));
        for r in &rects {
            lines.push(format!(
                "pane session={} rect={},{},{}x{} focused={}",
                r.session,
                r.row_off,
                r.col_off,
                r.rows,
                r.cols,
                r.session == focus,
            ));
        }
        lines
    }

    /// Render the CURRENT terminal for the control socket's `image` verb (the
    /// same renderer the window uses, GPU path if active). Runs on the main
    /// thread per [`crate::Wake::Control`] — but ONLY the render + app-composition
    /// composites: the PNG encode + confined write are handed to the encode
    /// worker, which transfers guarded `(width, height)` after the write. The
    /// control writer revalidates and retains it through the complete response
    /// challenge and ACK (or the failed-handoff quarantine).
    pub(crate) fn render_image(&mut self, req: ImageReq) {
        let ImageReq {
            target,
            clean,
            session,
            want_bytes,
            want_metadata,
            frame_metadata,
            cancel,
            reply,
        } = req;
        if cancel.is_cancelled() {
            let _ = reply.send(Err("image request cancelled before render".to_string()));
            return;
        }
        // THE headless pixel demand. Redeem the deferred GPU intent before any
        // capture geometry or renderer state is read, so this capture is served by
        // the same backend a boot-built one would have been — the parity the
        // deferral is only allowed to exist under. No-op windowed, and after the
        // first capture of a headless run.
        self.ensure_pixel_backend();
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
            let _ = reply.send(Ok(crate::control::Retained::plain((0, 0, None))));
            return;
        };
        let Some(route) = self.active_visible_content_route(front) else {
            let _ = reply.send(Ok(crate::control::Retained::plain((0, 0, None))));
            return;
        };
        let recording_here = self
            .video_rec
            .as_ref()
            .is_some_and(|recording| recording.window == front);
        let mut recording_destination = if recording_here && !clean {
            match self.recording_presented_destination_capture(front) {
                Ok(destination) => Some(destination),
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            }
        } else {
            None
        };
        let route = recording_destination
            .as_ref()
            .map_or(route, |destination| destination.route);
        if !want_metadata && let Some(destination) = recording_destination.take() {
            // The common live-repro path: no semantic clone, hash or reraster.
            // The explicit request pays one destination copy/readback only.
            self.submit_encode_job(EncodeJob::Image {
                frame: destination.frame,
                target,
                want_bytes,
                cancel,
                reply,
            });
            return;
        }
        if matches!(
            route,
            crate::VisibleContentRoute::Native { .. } | crate::VisibleContentRoute::Heterogeneous
        ) {
            // `image` is deliberately the route-neutral renderer framebuffer,
            // never a platform-window photograph. Native and heterogeneous
            // routes compile into that same semantic surface; only the distinct
            // `window` verb includes title bars, traffic lights, and OS chrome.
            // Synchronize windowed callers through the ordinary present serial
            // without acquiring Screen Recording permission or changing image
            // dimensions when focus crosses a terminal/native split.
            let presented = match self.present_before_frame_capture(front) {
                Ok(presented) => presented,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            let mut presented_metadata = None;
            let presented = if presented.is_some() || !recording_here {
                presented
            } else {
                let resolved =
                    match self.recording_presented_frame_capture(front, false, want_metadata) {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            return;
                        }
                    };
                presented_metadata = resolved.native_metadata;
                Some(resolved.frame)
            };
            self.render_native_image(NativeImageRequest {
                front,
                clean,
                presented,
                presented_metadata,
                exact_frame: recording_destination.map(|destination| destination.frame),
                target,
                want_bytes,
                want_metadata,
                cancel,
                frame_metadata: &frame_metadata,
                reply,
            });
            return;
        }
        // A WINDOWED capture routes through the real present and then reuses THAT
        // frame's composed grid, so the picture can never diverge from the glass.
        // A glass-less window has no such authority, so the fallback below builds
        // the terminal band without entering the present decision path: a
        // pure-terminal split is one composite capture, not a full-window refill of
        // only its focused terminal, and every pane keeps its OWN live default
        // background instead of inheriting the front pane's. That path stays a READ
        // — no damage consumed, no present stamped.
        let presented = match self.present_before_terminal_capture(front) {
            Ok(presented) => presented,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let presented = if presented.is_some() || !recording_here {
            presented
        } else {
            let resolved = match self.recording_presented_frame_capture(front, true, false) {
                Ok(resolved) => resolved,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            let Some(grid) = resolved.terminal_grid else {
                let _ = reply.send(Err(
                    "image deferred: recorded terminal inventory is unavailable".to_string(),
                ));
                return;
            };
            Some(PresentedTerminalCapture {
                input: resolved.frame.input,
                grid,
                invert: resolved.frame.invert,
                overlay: resolved.frame.overlay,
                serial: resolved.frame.serial,
                cell_size: resolved.cell_size,
                theme_fingerprint: resolved.theme_fingerprint,
                overlay_fingerprint: resolved.overlay_fingerprint,
            })
        };
        let mut presented_authority = None;
        let mut presented_input = None;
        let capture_grid = if let Some(presented) = presented {
            presented_authority = Some(PresentedTerminalAuthority {
                invert: presented.invert,
                overlay: presented.overlay,
                serial: presented.serial,
                cell_size: presented.cell_size,
                theme_fingerprint: presented.theme_fingerprint,
                overlay_fingerprint: presented.overlay_fingerprint,
            });
            presented_input = Some(presented.input);
            presented.grid
        } else {
            // A window without an OS presentation target has no prior
            // application-present artifact. Its explicit image is the one
            // present-real tick, built from current terminals.
            let capture_now = Instant::now();
            let clock = self.composed_capture_cursor_fx_clock(front, capture_now);
            let capture_grid = match self
                .prepare_terminal_capture_grid_with_cursor_fx_outcome(front, clock)
            {
                crate::app_render::CapturePreparation::Ready(grid) => grid,
                crate::app_render::CapturePreparation::Held { retry_at } => {
                    let retry_ms = retry_at
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                        .max(1);
                    let _ = reply.send(Err(format!(
                        "image deferred: synchronized terminal update in progress; retry after {retry_ms} ms"
                    )));
                    return;
                }
                crate::app_render::CapturePreparation::Unavailable => {
                    let _ = reply.send(Ok(crate::control::Retained::plain((0, 0, None))));
                    return;
                }
            };
            let Some(capture_focus) = capture_grid.focus else {
                let _ = reply.send(Ok(crate::control::Retained::plain((0, 0, None))));
                return;
            };
            // Unlike the cursor light extractor, the decoration splice already
            // dispatches on one-pane versus composed geometry. It also owns the
            // capture/recording clock boundary, so this is safe in both modes.
            self.splice_word_decorations_sampled(front, capture_now, capture_focus);
            self.splice_tab_strip(front);
            self.splice_find_bar(front);
            self.splice_settings_panel(front);
            self.splice_build_badge(front);
            self.splice_notice(front);
            self.splice_level_up(front);
            self.splice_config_notice(front);
            self.splice_paste_banner(front);
            self.splice_link_target(front);
            // C5 — topmost chrome; see the `chrome`-capture route above.
            self.splice_tab_menu(front);
            capture_grid
        };
        // Always rasterize an owned explicit-capture snapshot. In particular,
        // `image plain` clears overlays on this clone, never on the retained
        // window scratch that a later present/capture reuses.
        let mut capture_input =
            presented_input.unwrap_or_else(|| self.windows[&front].input_scratch.clone());
        // Accent for the drop-target highlight / level-up glow, read before the disjoint
        // borrow. The level-up glow's breathing alphas are sampled here too,
        // matching application-present transient state.
        let accent = self.theme.cursor;
        let level_up_glow = self
            .level_up
            .as_ref()
            .map(|l| (l.wash_alpha(Instant::now()), l.border_alpha(Instant::now())));
        let tray_floor_y = self.config_notice_tray_floor_y(front);
        let theme_fingerprint = presented_authority.map_or_else(
            || self.image_theme_fingerprint(),
            |presented| presented.theme_fingerprint,
        );
        let capture_cell_size = presented_authority.map_or_else(
            || self.win_cell_size(front),
            |presented| presented.cell_size,
        );
        if let (Some(destination), Some(authority)) =
            (recording_destination.as_ref(), presented_authority)
        {
            debug_assert_eq!(destination.serial, authority.serial);
        }
        let exact_frame = recording_destination.map(|destination| destination.frame);
        let exact_destination = exact_frame.is_some();
        if !exact_destination {
            self.bind_window_renderer_state(front);
        }
        // Disjoint borrows: `self.backend` (renderer), the introspection GPU
        // scratch, and the front window's input_scratch are separate fields.
        let App {
            backend,
            introspect_gpu,
            windows,
            ..
        } = self;
        let Some(ws) = windows.get_mut(&front) else {
            let _ = reply.send(Ok(crate::control::Retained::plain((0, 0, None))));
            return;
        };
        // CLEAN capture (`image plain`): drop every host-owned bling LAYER so the AI reads
        // the bare terminal — cursor trail + LUMEN glow + sparkle-word decorations + the
        // animated Scene. They live in separate `RenderInput` fields, so this is just those
        // layers emptied; the cell grid (text) is untouched.
        if clean {
            capture_input.clear_overlays();
        }
        // Time the rasterization so the `metrics` verb reports a real
        // `last_frame_render_ms` in HEADLESS mode too. Windowed application frames are timed in
        // `redraw_window`; without this, headless (no OS surface → no
        // RedrawRequested → no `record_present`) leaves every counter frozen at 0,
        // so a perf audit driven over the control socket could measure nothing.
        // Present latency is recorded as 0 — honest: the `image` verb rasterizes to
        // a buffer; it does not submit to an OS presentation target.
        let render_t0 = (!exact_destination).then(Instant::now);
        // P3 settings card → raw bytes + device-px rect for the GPU tray quad (same
        // builder application-present uses). The GPU arm bakes it into the
        // offscreen so capture and application-present share composition; the
        // CPU arm ignores it here (composited below, gated on !is_gpu).
        // Modal card FIRST, else the transient update notice, else the build/version badge.
        let tray_arg = ws
            .present_card()
            .and_then(|card| tray_quad_below_y(card, tray_floor_y));
        let destination_height = ws.win_px.map(|size| size.height.max(1) as usize);
        let mut frame = if let Some(frame) = exact_frame {
            // Exact current VirtualTarget destination. It already contains the
            // success-time crop, tray, bell/overlay and GPU-only crown passes.
            frame
        } else {
            match backend.render_input_for_destination(
                introspect_gpu,
                &mut capture_input,
                tray_arg,
                destination_height,
            ) {
                Ok(frame) => frame,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            }
        };
        if !exact_destination
            && !backend.is_gpu()
            && let Some(quad) = ws
                .present_card()
                .and_then(|card| tray_quad_below_y(card, tray_floor_y))
        {
            composite_tray_quad_at(&mut frame.pixels, frame.width, frame.height, 0, 0, quad);
        }
        if let Some(render_t0) = render_t0 {
            let render_ns = render_t0.elapsed().as_nanos() as u64;
            crate::metrics::record_offscreen_raster(render_ns);
        }
        // I-2: match the on-screen visual-bell invert (see `snapshot`) so the
        // `image` verb is WYSIWYG even during a bell flash. Suppressed while ANY modal
        // overlay is open — the SAME `overlay_open()` gate the glass present and the
        // snapshot path consult, so all three can never disagree (SACRED WYSIWYG).
        // The boundary is the APPLICATION surface: the compositor and scanout stay
        // outside this comparison.
        if !exact_destination {
            let invert = presented_authority.map_or_else(
                || ws.bell_flash.is_active(Instant::now()) && !ws.overlay_open(),
                |presented| presented.invert,
            );
            apply_bell_invert(&mut frame, invert);
            // Match application-present drop-target/LEVEL-UP overlay composition;
            // both are suppressed under the same modal-overlay predicate.
            let retained_overlay = presented_authority.and_then(|presented| presented.overlay);
            if let Some(overlay) = retained_overlay {
                apply_overlay_at(
                    &mut frame.pixels,
                    frame.width,
                    frame.height,
                    0,
                    0,
                    frame.width,
                    frame.height,
                    overlay,
                );
            } else if presented_authority.is_none() {
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
            }
        }
        if want_metadata {
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
                Self::terminal_capture_fingerprint(&capture_grid, &capture_input);
            let geometry =
                Self::terminal_geometry_fingerprint(frame.width, frame.height, &capture_input);
            let singular = (capture_grid.leaves.len() == 1).then(|| capture_grid.leaves[0]);
            let (cell_w, cell_h) = capture_cell_size;
            let leaves = capture_grid
                .leaves
                .iter()
                .map(|leaf| {
                    let leaf_width = singular.map_or_else(
                        || u32::try_from(leaf.cols.saturating_mul(cell_w)).unwrap_or(u32::MAX),
                        |_| width,
                    );
                    let leaf_height = singular.map_or_else(
                        || u32::try_from(leaf.rows.saturating_mul(cell_h)).unwrap_or(u32::MAX),
                        |_| height,
                    );
                    crate::control::ImageLeafFrameMetadata {
                        kind: "terminal",
                        view: leaf.view.get(),
                        session: Some(leaf.session),
                        focused: leaf.focused,
                        width: leaf_width,
                        height: leaf_height,
                        snapshot_seq: Some(leaf.snapshot_seq),
                        instance: None,
                        generation: None,
                        geometry: singular.map(|_| geometry),
                        config_revision: None,
                        update_revision: None,
                        document_seq: None,
                        presentation_revision: None,
                        paint_revision: None,
                        compiled_fingerprint: None,
                        raster_fingerprint: singular.map(|_| pixel_fingerprint),
                    }
                })
                .collect();
            let metadata = crate::control::ImageFrameMetadata {
                frame_kind: if singular.is_some() {
                    "terminal"
                } else {
                    "composite"
                },
                phase: "rendered",
                window: front.0,
                view: singular.map(|leaf| leaf.view.get()),
                generation: None,
                config_revision: None,
                update_revision: None,
                document_seq: None,
                presentation_revision: None,
                paint_revision: None,
                capture_serial: presented_authority
                    .map_or(ws.capture_present_serial, |presented| presented.serial),
                width,
                height,
                pixel_fingerprint,
                compiled_fingerprint: None,
                raster_fingerprint: pixel_fingerprint,
                raster_model_fingerprint: snapshot_fingerprint,
                raster_geometry: geometry,
                overlay_fingerprint: presented_authority.map_or_else(
                    || ws.overlay_fp(),
                    |presented| presented.overlay_fingerprint,
                ),
                theme_fingerprint,
                leaves,
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
        // encode worker, NOT this event-loop thread. Every app-owned splice/overlay
        // above is already baked into the moved `frame`, and the worker replies
        // only after the write — the client reads the file the moment it sees OK.
        self.submit_encode_job(EncodeJob::Image {
            frame,
            target,
            want_bytes,
            cancel,
            reply,
        });
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
        // Only the Windows arm below pushes; the mut is platform-conditional.
        #[cfg_attr(not(windows), allow(unused_mut))]
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

    #[cfg(target_os = "macos")]
    pub(crate) fn capture_window(
        &mut self,
        target: control_auth::ConfinedImage,
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        self.capture_window_of(self.frontmost_window, target, cancel, reply);
    }

    /// The body of [`Self::capture_window`], parameterized on which logical window to
    /// photograph. A capture failure replies
    /// `Err` immediately; captured pixels are handed to the encode worker.
    #[cfg(target_os = "macos")]
    fn capture_window_of(
        &mut self,
        wid: Option<WindowId>,
        target: control_auth::ConfinedImage,
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        if cancel.is_cancelled() {
            let _ = reply.send(Err(
                "window capture request cancelled before photograph".to_string()
            ));
            return;
        }
        if let Some(wid) = wid {
            let has_gpu_surface = self.windows.get(&wid).is_some_and(|window| {
                matches!(window.present, Some(crate::PresentTarget::Gpu { .. }))
            });
            if let Err(error) = crate::window_capture_translucency_guard(
                self.render_knobs.background_opacity,
                true,
                self.backend.is_gpu(),
                has_gpu_surface,
            ) {
                let _ = reply.send(Err(error.to_string()));
                return;
            }
        }
        let presented = match wid {
            Some(wid) => match self.present_before_window_capture(wid) {
                Ok(presented) => Some(presented),
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            },
            None => None,
        };
        let captured = match wid {
            Some(wid) => self.current_window_rgba_of(
                wid,
                presented
                    .as_ref()
                    .expect("a concrete window has a presented client"),
            ),
            None => self.window_rgba_of(None),
        };
        match captured {
            Ok((rgba, width, height)) => self.submit_encode_job(EncodeJob::WindowRgba {
                rgba,
                width,
                height,
                target,
                cancel,
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
    fn present_before_window_capture(
        &mut self,
        wid: WindowId,
    ) -> Result<PresentedWindowCapture, String> {
        let has_os_window = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.os_window.is_some());
        if !has_os_window {
            return Err("no window to capture (headless)".to_string());
        }

        let mut captured = None;
        let mut last_capture_error = None;
        let presented =
            crate::run_capture_present_barrier(crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, || {
                self.discard_presented_client_capture(wid);
                let before = self
                    .windows
                    .get(&wid)
                    .map_or(0, |window| window.capture_present_serial);
                if let Err(error) = self.arm_presented_client_capture(wid) {
                    last_capture_error = Some(error);
                    return false;
                }
                let Some(window) = self.windows.get_mut(&wid) else {
                    last_capture_error = Some("window capture lost its window".to_string());
                    return false;
                };
                // Each capture attempt is an explicit external stimulus. Reopen
                // a parked retry episode and invalidate the optimistic repaint
                // stamp so attempts two and three cannot be swallowed by the
                // failed-present gate or a steady-frame early-out.
                let _ = window.present_retry.on_external_stimulus();
                window.last_present = None;
                self.redraw_window(wid);
                let serial = self
                    .windows
                    .get(&wid)
                    .map_or(before, |window| window.capture_present_serial);
                if serial == before {
                    last_capture_error =
                        Some("window capture did not complete the present path".to_string());
                    return false;
                }
                match self.take_presented_client_capture(wid) {
                    Ok(client) => {
                        let Some(frame) = self.presented_frame_capture_after(wid, before) else {
                            last_capture_error = Some(
                                "window capture present had no matching semantic frame".to_string(),
                            );
                            return false;
                        };
                        if frame.serial != serial {
                            last_capture_error =
                                Some("window capture client/semantic serial mismatch".to_string());
                            return false;
                        }
                        captured = Some(PresentedWindowCapture {
                            serial,
                            client,
                            frame,
                        });
                        true
                    }
                    Err(error) => {
                        last_capture_error = Some(error);
                        false
                    }
                }
            });
        if presented {
            captured.ok_or_else(|| {
                "window capture barrier succeeded without a presented client frame".to_string()
            })
        } else {
            self.discard_presented_client_capture(wid);
            let detail = last_capture_error
                .map(|error| format!(": {error}"))
                .unwrap_or_default();
            Err(format!(
                "window capture could not synchronize the requested frame after {} present attempts{detail}",
                crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT,
            ))
        }
    }

    /// Arm one exact destination copy for the next successful surface
    /// transaction. CPU retains its final softbuffer only for this requested
    /// frame; GPU attaches an independent one-shot tap beside any active video
    /// recorder and copies the post-crown swapchain texture in the same encoder.
    fn arm_presented_client_capture(&mut self, wid: WindowId) -> Result<(), String> {
        let App {
            backend, windows, ..
        } = self;
        let window = windows
            .get_mut(&wid)
            .ok_or_else(|| "window capture lost its window".to_string())?;
        window.capture_present_client = None;
        match window.present.as_mut() {
            Some(crate::PresentTarget::Cpu { .. }) => {
                window.capture_client_requested = true;
                Ok(())
            }
            Some(crate::PresentTarget::Gpu {
                gpu_surface,
                window_gpu,
            }) => backend
                .gpu_mut()
                .ok_or_else(|| "window capture GPU target/backend mismatch".to_string())?
                .presented_snapshot_begin(window_gpu, gpu_surface),
            Some(crate::PresentTarget::Virtual { .. }) | None => {
                Err("window capture has no window application-present target".to_string())
            }
        }
    }

    /// Drain the exact destination armed above. This runs only after the
    /// successful-present serial advances, so blocking for the explicit GPU
    /// readback is off the ordinary present hot path.
    fn take_presented_client_capture(
        &mut self,
        wid: WindowId,
    ) -> Result<crate::PresentedClientFrame, String> {
        let App {
            backend, windows, ..
        } = self;
        let window = windows
            .get_mut(&wid)
            .ok_or_else(|| "window capture lost its window".to_string())?;
        match window.present.as_mut() {
            Some(crate::PresentTarget::Cpu { .. }) => {
                window.capture_client_requested = false;
                window.capture_present_client.take().ok_or_else(|| {
                    "window capture CPU present produced no exact client frame".to_string()
                })
            }
            Some(crate::PresentTarget::Gpu { window_gpu, .. }) => {
                let gpu = backend
                    .gpu_mut()
                    .ok_or_else(|| "window capture GPU target/backend mismatch".to_string())?;
                gpu.presented_snapshot_after_present(window_gpu, crate::metrics::now_us())?;
                gpu.presented_snapshot_finish(window_gpu)?;
                let frame = gpu.presented_snapshot_take(window_gpu)?;
                Ok(crate::PresentedClientFrame {
                    width: frame.w,
                    height: frame.h,
                    rgba: frame.rgba,
                })
            }
            Some(crate::PresentTarget::Virtual { .. }) | None => {
                Err("window capture lost its window application-present target".to_string())
            }
        }
    }

    /// Clear any failed/stale one-shot before a bounded retry. Taking an armed
    /// GPU tap consumes it even though it returns "not captured"; CPU simply
    /// drops the requested/staged clone.
    fn discard_presented_client_capture(&mut self, wid: WindowId) {
        let App {
            backend, windows, ..
        } = self;
        let Some(window) = windows.get_mut(&wid) else {
            return;
        };
        window.capture_client_requested = false;
        window.capture_present_client = None;
        if let Some(crate::PresentTarget::Gpu { window_gpu, .. }) = window.present.as_mut()
            && let Some(gpu) = backend.gpu_mut()
        {
            let _ = gpu.presented_snapshot_take(window_gpu);
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

    /// Resolve the renderer client layer's true origin inside the OS window
    /// photograph from winit's physical inner/outer geometry. Dimensions alone
    /// cannot identify this rectangle: Windows has asymmetric non-client resize
    /// borders, while macOS full-size content starts beneath overlapping toolbar
    /// chrome. Fail closed if geometry moved between present and capture.
    #[cfg(any(target_os = "macos", windows))]
    fn window_client_rect_of(
        &self,
        wid: WindowId,
        client: &crate::PresentedClientFrame,
    ) -> Result<WindowClientRect, String> {
        let state = self
            .windows
            .get(&wid)
            .ok_or_else(|| "window capture lost its logical window".to_string())?;
        let window = state
            .os_window
            .as_ref()
            .ok_or_else(|| "no window to capture (headless)".to_string())?;
        let inner_size = window.inner_size();
        if (inner_size.width, inner_size.height) != (client.width, client.height) {
            return Err(format!(
                "window client geometry changed during capture: presented {}x{}px, current {}x{}px",
                client.width, client.height, inner_size.width, inner_size.height
            ));
        }
        let inner = window
            .inner_position()
            .map_err(|error| format!("window capture could not read client origin: {error}"))?;
        let outer = window
            .outer_position()
            .map_err(|error| format!("window capture could not read frame origin: {error}"))?;
        let x = i64::from(inner.x) - i64::from(outer.x);
        let y = i64::from(inner.y) - i64::from(outer.y);
        let x = u32::try_from(x)
            .map_err(|_| "window capture reported a negative client x origin".to_string())?;
        let y = u32::try_from(y)
            .map_err(|_| "window capture reported a negative client y origin".to_string())?;
        Ok(WindowClientRect {
            x,
            y,
            width: client.width,
            height: client.height,
        })
    }

    /// Snapshot the actual AppKit titlebar subtree into a transparent RGBA layer.
    ///
    /// `FullSizeContentView` puts the renderer beneath the titlebar, so the exact
    /// presented client owns every row. The traffic lights, unified toolbar, tab
    /// chips, and titlebar decoration remain a distinct AppKit view subtree,
    /// though. AppKit's public `cacheDisplayInRect:toBitmapImageRep:` contract
    /// produces alpha zero wherever that subtree draws nothing, which gives us
    /// the per-pixel chrome mask a rectangular titlebar crop cannot provide.
    ///
    /// The root is discovered without private class names: start at the standard
    /// close button and walk upward only while the candidate ancestor does NOT
    /// contain Winit's content view. This reaches the titlebar container (and its
    /// decoration sibling) but can never absorb the stale CAMetalLayer/softbuffer
    /// subtree. If a future AppKit hierarchy cannot satisfy that separation, fail
    /// closed instead of falling back to a whole-row approximation.
    #[cfg(target_os = "macos")]
    fn native_chrome_overlay_of(
        &self,
        wid: WindowId,
        window_width: u32,
        window_height: u32,
    ) -> Result<Option<RgbaOverlay>, String> {
        use objc2::rc::Retained;
        use objc2_app_kit::{
            NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSButton,
            NSColorRenderingIntent, NSColorSpace, NSView, NSWindowButton,
        };
        use objc2_foundation::NSDictionary;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let state = self
            .windows
            .get(&wid)
            .ok_or_else(|| "window capture lost its logical window".to_string())?;
        // No overlapping titlebar exists in fullscreen or when the
        // ATERM_NO_FULLSIZE_CONTENT escape hatch is active. The platform capture
        // already owns the ordinary non-client rows outside the inset client rect.
        if state.metrics.head == 0 {
            return Ok(None);
        }
        let os_window = state
            .os_window
            .as_ref()
            .ok_or_else(|| "no window to capture (headless)".to_string())?;
        let handle = os_window
            .window_handle()
            .map_err(|error| format!("window capture could not read AppKit handle: {error}"))?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err("window capture has no AppKit window".to_string());
        };
        // SAFETY: Winit owns this live NSView for the OS window's lifetime. This
        // method runs only on the application's main event-loop thread.
        let content: &NSView =
            unsafe { &*(handle.ns_view.as_ptr() as *const objc2_app_kit::NSView) };
        let ns_window = content
            .window()
            .ok_or_else(|| "window capture lost its AppKit window".to_string())?;
        let close: Retained<NSButton> = ns_window
            .standardWindowButton(NSWindowButton::NSWindowCloseButton)
            .ok_or_else(|| {
                "window capture could not locate AppKit titlebar controls".to_string()
            })?;
        // SAFETY: NSButton inherits NSControl -> NSView; this is an upcast of the
        // same retained Objective-C object, never a dynamic downcast.
        let mut chrome_root: Retained<NSView> = unsafe { Retained::cast(close) };
        for _ in 0..32 {
            // SAFETY: side-effect-free parent read on a live main-thread NSView.
            let Some(parent) = (unsafe { chrome_root.superview() }) else {
                break;
            };
            if macos_view_is_ancestor_of(&parent, content)? {
                break;
            }
            chrome_root = parent;
        }
        if macos_view_is_ancestor_of(&chrome_root, content)? {
            return Err(
                "window capture refused an AppKit chrome root containing client pixels".to_string(),
            );
        }

        // The retained custom strip must be inside the selected titlebar root.
        // Otherwise the snapshot would silently omit native tabs or the '+' button.
        if let Some(toolbar) = self._toolbars.get(&wid) {
            let strip = crate::toolbar::native_strip_container(toolbar);
            if !macos_view_is_ancestor_of(&chrome_root, &strip)? {
                return Err(
                    "window capture could not isolate the complete AppKit toolbar subtree"
                        .to_string(),
                );
            }
        }

        let bounds = chrome_root.bounds();
        if !macos_rect_is_finite(bounds) || bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            return Err("window capture found invalid AppKit chrome bounds".to_string());
        }
        // nil means window-base coordinates. Convert those exact points through
        // NSWindow's backing transform rather than multiplying by a guessed scale.
        let window_rect = chrome_root.convertRect_toView(bounds, None);
        // SAFETY: side-effect-free geometry conversion on the live NSWindow.
        let backing_rect = unsafe { ns_window.convertRectToBacking(window_rect) };
        if !macos_rect_is_finite(backing_rect) {
            return Err("window capture found invalid AppKit backing geometry".to_string());
        }

        // SAFETY: both calls are the documented view-caching pair, on the main
        // thread. The first allocates a compatible bitmap; the second draws only
        // this chrome subtree and leaves every undrawn pixel transparent.
        let bitmap: Retained<NSBitmapImageRep> = unsafe {
            chrome_root
                .bitmapImageRepForCachingDisplayInRect(bounds)
                .ok_or_else(|| {
                    "window capture could not allocate an AppKit chrome bitmap".to_string()
                })?
        };
        unsafe {
            chrome_root.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);
        }
        // The caching rep inherits the live window/display backing profile.
        // Convert its pixels (do not merely retag them) before mixing them with
        // the renderer's canonical sRGB client and declaring sRGB in the PNG.
        let srgb = unsafe { NSColorSpace::sRGBColorSpace() };
        let bitmap = unsafe {
            bitmap
                .bitmapImageRepByConvertingToColorSpace_renderingIntent(
                    &srgb,
                    NSColorRenderingIntent::Perceptual,
                )
                .ok_or_else(|| {
                    "window capture could not convert AppKit chrome to sRGB".to_string()
                })?
        };
        let properties =
            NSDictionary::<NSBitmapImageRepPropertyKey, objc2::runtime::AnyObject>::new();
        // PNG standardizes the NSBitmapImageRep's implementation-defined channel
        // order and premultiplication into straight RGBA before Rust reads it.
        let png = unsafe {
            bitmap
                .representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
                .ok_or_else(|| {
                    "window capture could not encode the AppKit chrome bitmap".to_string()
                })?
        };
        let png_len = png.length();
        let mut png_bytes = vec![0_u8; png_len];
        if png_len != 0 {
            // SAFETY: the Vec owns `png_len` initialized bytes and the non-null
            // destination remains valid for the duration of NSData's bounded copy.
            let destination =
                std::ptr::NonNull::new(png_bytes.as_mut_ptr().cast()).ok_or_else(|| {
                    "window capture could not allocate AppKit chrome bytes".to_string()
                })?;
            unsafe {
                png.getBytes_length(destination, png_len);
            }
        }
        let (rgba, width, height) = decode_native_chrome_png(&png_bytes)?;

        let expected_width = backing_rect.size.width.round();
        let expected_height = backing_rect.size.height.round();
        if expected_width <= 0.0
            || expected_height <= 0.0
            || f64::from(width) != expected_width
            || f64::from(height) != expected_height
        {
            return Err(format!(
                "window capture AppKit chrome bitmap {}x{}px disagrees with backing geometry {:.0}x{:.0}px",
                width, height, expected_width, expected_height
            ));
        }
        let x = backing_rect.origin.x.round();
        let bottom = backing_rect.origin.y.round();
        if x < 0.0 || bottom < 0.0 || x > f64::from(u32::MAX) || bottom > f64::from(u32::MAX) {
            return Err("window capture AppKit chrome origin is outside the window".to_string());
        }
        let x = x as u32;
        let bottom = bottom as u32;
        let y = window_height
            .checked_sub(
                bottom
                    .checked_add(height)
                    .ok_or_else(|| "window capture AppKit chrome origin overflowed".to_string())?,
            )
            .ok_or_else(|| "window capture AppKit chrome lies outside the window".to_string())?;
        if x.checked_add(width)
            .is_none_or(|right| right > window_width)
        {
            return Err("window capture AppKit chrome lies outside the window".to_string());
        }
        Ok(Some(RgbaOverlay {
            x,
            y,
            width,
            height,
            rgba,
        }))
    }

    /// Capture the platform-owned window frame, then replace its visible client
    /// rows with the exact successful-present destination.
    ///
    /// A successful Metal `present()` means the drawable was accepted, not that the
    /// out-of-process WindowServer has already promoted it. CoreGraphics can therefore
    /// photograph the previous client frame for one compositor interval immediately
    /// after an `open`/`act`, even though the titlebar and semantic inspection are
    /// current. The one-shot surface capture is already the final raw client layer
    /// (letterbox bands/crops, tray, bell/overlay, SDR crown and HDR conversion
    /// included), so it is authoritative below the overlapping titlebar. CoreGraphics
    /// remains authoritative only for platform-owned title/toolbar chrome. This
    /// removes timing guesses and offscreen lookalikes from full-window introspection.
    #[cfg(target_os = "macos")]
    fn current_window_rgba_of(
        &self,
        wid: WindowId,
        presented: &PresentedWindowCapture,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        let (mut rgba, width, height) = self.window_rgba_of(Some(wid))?;
        // The platform photograph is tightly-packed RGBA8 (`width * height * 4`
        // bytes), so the chunk remainder is empty and the mask keeps exactly one
        // alpha byte per platform pixel — the length
        // `multiply_platform_outer_shape_alpha` fails closed on below.
        let platform_shape_alpha = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect::<Vec<_>>();
        let state = self
            .windows
            .get(&wid)
            .ok_or_else(|| "window capture lost its logical window".to_string())?;
        if state.capture_present_serial != presented.serial {
            return Err("window capture client serial is no longer current".to_string());
        }
        let rect = self.window_client_rect_of(wid, &presented.client)?;
        stitch_presented_client_into_window_rgba(
            &mut rgba,
            width,
            height,
            rect,
            &presented.client,
        )?;
        if let Some(chrome) = self.native_chrome_overlay_of(wid, width, height)? {
            composite_straight_rgba_overlay(&mut rgba, width, height, &chrome, rect)?;
        }
        multiply_platform_outer_shape_alpha(&mut rgba, width, height, &platform_shape_alpha)?;
        Ok((rgba, width, height))
    }

    #[cfg(windows)]
    pub(crate) fn capture_window(
        &mut self,
        target: control_auth::ConfinedImage,
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        if cancel.is_cancelled() {
            let _ = reply.send(Err(
                "window capture request cancelled before photograph".to_string()
            ));
            return;
        }
        if let Err(error) =
            crate::window_capture_material_guard(self.render_knobs.background_material, false)
        {
            let _ = reply.send(Err(error.to_string()));
            return;
        }
        let Some(wid) = self.frontmost_window else {
            let _ = reply.send(Err("no window to capture (headless)".to_string()));
            return;
        };
        let presented = match self.present_before_window_capture(wid) {
            Ok(presented) => presented,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
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
                let state = self
                    .windows
                    .get(&wid)
                    .ok_or_else(|| "window capture lost its logical window".to_string())?;
                if state.capture_present_serial != presented.serial {
                    return Err("window capture client serial is no longer current".to_string());
                }
                let rect = self.window_client_rect_of(wid, &presented.client)?;
                stitch_presented_client_into_window_rgba(
                    &mut rgba,
                    width,
                    height,
                    rect,
                    &presented.client,
                )?;
                Ok((rgba, width, height))
            },
        );
        match captured {
            Ok((rgba, width, height)) => self.submit_encode_job(EncodeJob::WindowRgba {
                rgba,
                width,
                height,
                target,
                cancel,
                reply,
            }),
            Err(e) => {
                let _ = reply.send(Err(format!("window capture failed: {e}")));
            }
        }
    }

    /// Off macOS/Windows there is no window server / `PrintWindow` to photograph, so
    /// the `window` verb reports that plainly. Kept as a method on every target so the
    /// [`Wake::CaptureWindow`] handler is platform-independent.
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    pub(crate) fn capture_window(
        &mut self,
        _target: control_auth::ConfinedImage,
        _cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        let _ = reply.send(Err(
            "window capture is only available on macOS and Windows".to_string()
        ));
    }
}

impl App {
    /// Capture an auxiliary own-rendered GUI surface to the confined PNG, replying its
    /// `(width, height)`. Serves Settings-route aliases such as `window prefs`
    /// (main thread, per
    /// [`crate::Wake::CaptureAuxWindow`]).
    ///
    /// Every supported target lives in the front aterm frame, so this delegates to the
    /// ordinary capture path and preserves its confinement and encoding behavior.
    ///
    /// Windows takes the same arm: Settings renders INSIDE the frame there too,
    /// and `capture_window` is fully implemented (`PrintWindow` + the presented
    /// client stitch). It used to answer "only available on macOS" — refusing a
    /// photograph it was already able to take, which left `window prefs` (the
    /// route an AI uses to SEE the Settings surface) unreachable on Windows.
    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn capture_aux_window(
        &mut self,
        target: AuxTarget,
        confined: control_auth::ConfinedImage,
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        let _ = target;
        self.capture_window(confined, cancel, reply);
    }

    /// Off macOS/Windows there is no window server / `PrintWindow` to photograph, so
    /// the aux-window capture reports that plainly (kept on every target so the
    /// [`crate::Wake::CaptureAuxWindow`] handler is platform-independent) — the
    /// same platform split, and the same wording, as `capture_window` itself.
    #[cfg(not(any(target_os = "macos", windows)))]
    pub(crate) fn capture_aux_window(
        &mut self,
        _target: AuxTarget,
        _confined: control_auth::ConfinedImage,
        _cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        let _ = reply.send(Err(
            "auxiliary window capture is only available on macOS and Windows".to_string(),
        ));
    }

    fn read_native_settings_controls(
        &self,
        route_alias: Option<(&'static str, crate::native_settings::SettingsRoute)>,
    ) -> Vec<String> {
        let target = route_alias.map_or_else(
            || self.native_settings_view_target(),
            |(_, route)| self.native_settings_route_view_target(route),
        );
        let Some((wid, instance, view, state)) = target else {
            return match route_alias {
                Some((alias, route)) => native_settings_route_not_open_controls_lines(
                    alias,
                    route,
                    self.native_settings_front_view_target()
                        .map(|(_, _, _, state)| state),
                ),
                None => native_settings_closed_controls_lines(),
            };
        };

        // Prefer the retained frame lowered into the Settings card whenever it is
        // current. This binds compatibility inspection to the same artifact as pixels,
        // hit testing, and accessibility. An inactive native view has no retained
        // application-present cache;
        // compile that exact stable view/viewport rather than borrowing the focused one.
        if let Some(frame) = self
            .cached_native_ui(wid)
            .filter(|frame| frame.stamp.instance == instance && frame.stamp.view == view)
        {
            return native_settings_controls_lines(
                state,
                &frame.compiled,
                &state.config_assets().trail_packs.ids,
            );
        }
        let compiled = self
            .native_ui_viewport_for(wid, view)
            .and_then(|viewport| self.compiled_native_ui_for(wid, instance, view, viewport));
        match compiled {
            Ok(compiled) => native_settings_controls_lines(
                state,
                &compiled,
                &state.config_assets().trail_packs.ids,
            ),
            Err(error) => native_settings_unavailable_controls_lines(state, &error),
        }
    }

    /// Read an auxiliary own-rendered surface's controls as human-readable text lines.
    /// Serves [`crate::Wake::ReadAuxControls`] on the main thread.
    ///
    /// Built from the same pure models the windows render from rather than walking OS
    /// subviews, so it is deterministic and AppKit-free (and works headlessly). For
    /// Settings, the compatibility `state` / `prefs fields` / `field` records are
    /// derived from the exact compiled native tree and followed by its canonical `ui`
    /// records; they never serialize the retired overlay model.
    pub(crate) fn read_aux_controls(&self, target: AuxTarget) -> Vec<String> {
        match target {
            AuxTarget::Prefs => self.read_native_settings_controls(None),
            AuxTarget::About => self.read_native_settings_controls(Some((
                "about",
                crate::native_settings::SettingsRoute::About,
            ))),
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
            AuxTarget::Update => self.read_native_settings_controls(Some((
                "update",
                crate::native_settings::SettingsRoute::SoftwareUpdate,
            ))),
            // The OPEN in-grid tab menu's live rows (cursor included). No
            // closed-state snapshot fallback here: the composed per-tab model is
            // already served closed by the `chrome` verb's `tab-menu` lines —
            // this target reads the INTERACTIVE surface, which either exists or
            // honestly does not.
            AuxTarget::TabMenu => match self.front().and_then(|ws| ws.tab_menu.as_ref()) {
                Some(menu) => menu.controls_lines(),
                None => vec!["tab-menu open=false".to_string()],
            },
            // The two connection surfaces read their INTERACTIVE state only
            // (the tab-menu rule: the surface exists or honestly does not).
            AuxTarget::ConnCard => match self.front().and_then(|ws| ws.conn_card()) {
                Some(card) => card.controls_lines(),
                None => vec!["conn-card open=false".to_string()],
            },
            AuxTarget::SessionPicker => match self.front().and_then(|ws| ws.session_picker()) {
                Some(picker) => picker.controls_lines(),
                None => vec!["session-picker open=false".to_string()],
            },
            // The aggregated fabric (§5.3 Owner-gated at the dispatch): the
            // OPEN map's live rows only — the closed aggregation is already
            // served by the `flows` verb, so no snapshot fallback here.
            AuxTarget::Connections => match self.front().and_then(|ws| ws.connection_map()) {
                Some(map) => map.controls_lines(),
                None => vec!["connections open=false".to_string()],
            },
            AuxTarget::Front => match self.front().and_then(|ws| ws.overlay()) {
                Some(o) => vec![o.status_line()],
                None => vec!["overlay open=false".to_string()],
            },
        }
    }
}

#[cfg(test)]
mod native_settings_compatibility_controls_tests {
    use super::{App, AuxTarget};
    use crate::WindowId;
    use crate::native_settings::SettingsRoute;
    use crate::native_ui::UiKey;

    #[test]
    fn about_and_update_aliases_emit_the_exact_retained_native_frame() {
        for (target, route, retired_prefix) in [
            (AuxTarget::About, SettingsRoute::About, "about "),
            (AuxTarget::Update, SettingsRoute::SoftwareUpdate, "update "),
        ] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            assert!(app.open_settings_tab(route));
            assert!(app.prepare_native_input_scratch(wid));

            let (expected_ui, actionable, semantics) = {
                let frame = app.windows[&wid]
                    .native_ui_compiled
                    .as_ref()
                    .expect("native Settings frame retained beside its raster");
                assert_eq!(frame.stamp.view, app.active_native_view(wid).unwrap().1);
                assert!(
                    frame
                        .compiled
                        .semantic(&UiKey::new(format!("settings/page{}", route.path())))
                        .is_some(),
                    "the retained frame owns the requested {route:?} page root"
                );
                (
                    frame.compiled.controls_lines(),
                    frame
                        .compiled
                        .semantics
                        .iter()
                        .filter(|node| node.action.is_some())
                        .count(),
                    frame.compiled.semantics.len(),
                )
            };

            let actual = app.read_aux_controls(target);
            assert!(
                actual[0].contains("state open=true")
                    && actual[0].contains("pane=native-tab")
                    && actual[0].contains(&format!("route={}", route.path()))
                    && actual[0].contains(&format!("page={:?}", route.label())),
                "alias identity must name the live native route: {:?}",
                actual[0]
            );
            assert!(actual.iter().any(|line| {
                line == &format!(
                    "native surface=native-tab route={} controls={actionable} semantics={semantics}",
                    route.path()
                )
            }));
            let actual_ui = actual
                .iter()
                .filter(|line| line.starts_with("ui "))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                actual_ui, expected_ui,
                "compatibility alias must serialize the exact semantic artifact retained for paint"
            );
            assert!(
                actual.iter().all(|line| !line.starts_with(retired_prefix)),
                "retired standalone projection leaked through {target:?}: {actual:?}"
            );
        }
    }

    #[test]
    fn route_specific_aliases_never_fabricate_a_closed_or_different_page() {
        let mut app = App::headless_for_test();
        for (target, route, alias) in [
            (AuxTarget::About, SettingsRoute::About, "about"),
            (AuxTarget::Update, SettingsRoute::SoftwareUpdate, "update"),
        ] {
            let closed = app.read_aux_controls(target);
            assert_eq!(closed[0], "state open=false");
            assert!(closed[2].contains(&format!("alias={alias}")));
            assert!(closed[2].contains(&format!("route=- expected-route={}", route.path())));
            assert!(closed[2].contains(&format!("use `open {alias}` first")));
            assert!(closed.iter().all(|line| !line.starts_with("ui ")));
        }

        assert!(app.open_settings_tab(SettingsRoute::Home));
        for (target, route, alias) in [
            (AuxTarget::About, SettingsRoute::About, "about"),
            (AuxTarget::Update, SettingsRoute::SoftwareUpdate, "update"),
        ] {
            let mismatch = app.read_aux_controls(target);
            assert!(
                mismatch[0].contains("state open=true")
                    && mismatch[0].contains("route=/home")
                    && mismatch[0].contains(&format!("use `open {alias}` first")),
                "route mismatch is explicit: {:?}",
                mismatch[0]
            );
            assert!(mismatch[2].contains(&format!("alias={alias}")));
            assert!(mismatch[2].contains(&format!("route=/home expected-route={}", route.path())));
            assert!(mismatch.iter().all(|line| !line.starts_with("ui ")));
            assert!(
                mismatch
                    .iter()
                    .all(|line| { !line.starts_with("about ") && !line.starts_with("update ") })
            );
        }
    }

    #[test]
    fn compatibility_field_metadata_uses_the_selected_view_asset_generation() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.cols = 160;
            window.rows = 50;
        }
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let view_assets = std::sync::Arc::clone(
            app.native_settings_view_target()
                .expect("native Settings view")
                .3
                .config_assets(),
        );

        // Simulate the host advancing to a different catalog while this view remains on
        // its admitted generation. The old projection mixed this host-only id into the
        // view's compatibility enum metadata even though its canonical `ui` rows were
        // compiled from `view_assets`.
        const HOST_ONLY: &str = "host-generation-only-pack";
        let mut host_assets = (*app.config_assets).clone();
        host_assets.trail_packs = std::sync::Arc::new(crate::app_config::TrailPackCatalog {
            ids: vec![HOST_ONLY.to_string()],
            ..crate::app_config::TrailPackCatalog::default()
        });
        app.config_assets = std::sync::Arc::new(host_assets);
        assert!(!std::sync::Arc::ptr_eq(&app.config_assets, &view_assets));

        let lines = app.read_aux_controls(AuxTarget::Prefs);
        let trail = lines
            .iter()
            .find(|line| {
                line.starts_with(&format!(
                    "field key={} ",
                    crate::prefs::EDIT_CURSOR_TRAIL_STYLE
                ))
            })
            .expect("Top Settings exposes the cursor-trail picker");
        let expected = crate::prefs::cursor_trail_style_options(
            view_assets.trail_packs.ids.iter().map(String::as_str),
        )
        .join(",");
        assert!(
            trail.contains(&format!("kind=enum options=[{expected}]")),
            "field metadata must share the selected view generation: {trail}"
        );
        assert!(
            !trail.contains(HOST_ONLY),
            "host generation leaked into a stale view: {trail}"
        );
    }

    #[test]
    fn route_aliases_never_describe_a_matching_background_window() {
        for (target, background_route, alias) in [
            (AuxTarget::About, SettingsRoute::About, "about"),
            (AuxTarget::Update, SettingsRoute::SoftwareUpdate, "update"),
        ] {
            let mut app = App::headless_for_test();
            let background = WindowId(0);
            assert!(app.open_settings_tab(background_route));
            let (_, background_view) = app
                .active_native_view(background)
                .expect("background Settings route");

            let session = app.next_session_id;
            let front = app.insert_logical_window(crate::stub_session(session), 24, 80);
            assert_eq!(app.frontmost_window, Some(front));
            assert!(app.open_settings_tab(SettingsRoute::Home));
            assert!(matches!(
                app.native_runtime.view_state(background_view),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state.route == background_route
            ));
            assert!(matches!(
                app.active_native_view(front)
                    .and_then(|(_, view)| app.native_runtime.view_state(view)),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state.route == SettingsRoute::Home
            ));

            let lines = app.read_aux_controls(target);
            assert!(
                lines[0].contains("route=/home")
                    && lines[0].contains(&format!("use `open {alias}` first")),
                "front-route mismatch must be explicit: {:?}",
                lines[0]
            );
            assert!(lines[2].contains(&format!(
                "alias={alias} route=/home expected-route={}",
                background_route.path()
            )));
            assert!(
                lines.iter().all(|line| !line.starts_with("ui ")),
                "background semantics must not describe the front capture: {lines:?}"
            );
        }
    }
}

/// Whether `ancestor` contains `descendant` in the live AppKit view hierarchy.
///
/// Bounded traversal makes a corrupt/cyclic foreign hierarchy a capture error
/// instead of an event-loop hang.
#[cfg(target_os = "macos")]
fn macos_view_is_ancestor_of(
    ancestor: &objc2_app_kit::NSView,
    descendant: &objc2_app_kit::NSView,
) -> Result<bool, String> {
    if std::ptr::eq(ancestor, descendant) {
        return Ok(true);
    }
    // SAFETY: side-effect-free superview reads on live NSViews, main thread only.
    let mut current = unsafe { descendant.superview() };
    for _ in 0..64 {
        let Some(view) = current else {
            return Ok(false);
        };
        if std::ptr::eq(ancestor, &*view) {
            return Ok(true);
        }
        current = unsafe { view.superview() };
    }
    Err("window capture found an invalid cyclic AppKit view hierarchy".to_string())
}

#[cfg(target_os = "macos")]
fn macos_rect_is_finite(rect: objc2_foundation::NSRect) -> bool {
    rect.origin.x.is_finite()
        && rect.origin.y.is_finite()
        && rect.size.width.is_finite()
        && rect.size.height.is_finite()
}

/// Decode AppKit's in-memory PNG normalization into tightly packed straight RGBA8.
#[cfg(target_os = "macos")]
fn decode_native_chrome_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    const MAX_CHROME_BYTES: usize = 256 * 1024 * 1024;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: MAX_CHROME_BYTES,
    });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("window capture could not decode AppKit chrome: {error}"))?;
    let (width, height) = (reader.info().width, reader.info().height);
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|&bytes| bytes <= MAX_CHROME_BYTES)
        .ok_or_else(|| "window capture AppKit chrome dimensions are invalid".to_string())?;
    if width == 0 || height == 0 {
        return Err("window capture AppKit chrome bitmap is empty".to_string());
    }
    let mut decoded = vec![0_u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut decoded)
        .map_err(|error| format!("window capture could not decode AppKit chrome: {error}"))?;
    if info.width != width
        || info.height != height
        || info.bit_depth != png::BitDepth::Eight
        || info.color_type != png::ColorType::Rgba
        || info.buffer_size() != expected
    {
        return Err(format!(
            "window capture AppKit chrome is not straight RGBA8 ({}x{}, {:?} {:?})",
            info.width, info.height, info.color_type, info.bit_depth
        ));
    }
    decoded.truncate(expected);
    Ok((decoded, width, height))
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

    // Normalize the platform image into the SAME explicit sRGB space as the
    // renderer-owned client and AppKit chrome overlay. DeviceRGB is
    // display-dependent and cannot truthfully be tagged sRGB in the output PNG.
    let srgb_name = objc2_foundation::NSString::from_str("kCGColorSpaceSRGB");
    // SAFETY: NSString is toll-free bridged to CFStringRef; CreateWithName returns
    // a new colour-space object that the guard below releases exactly once.
    let color_space: CGColorSpaceRef = unsafe {
        CGColorSpaceCreateWithName(
            (&*srgb_name as *const objc2_foundation::NSString).cast::<std::ffi::c_void>(),
        )
    };
    if color_space.is_null() {
        return Err("window capture failed (could not create sRGB color space)".to_string());
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
    let mut rgba = unsafe { std::slice::from_raw_parts(data_ptr, total) }.to_vec();
    // CoreGraphics bitmap contexts expose premultiplied RGBA, while PNG and
    // every renderer-owned capture buffer in aterm use straight alpha. Convert
    // exactly once at the platform boundary so later stitching never mixes two
    // alpha encodings (which darkens translucent titlebar/rounded-edge pixels).
    unpremultiply_rgba8(&mut rgba);

    Ok((rgba, width as u32, height as u32))
}

/// Convert tightly-packed premultiplied RGBA8 to straight RGBA8 in place.
///
/// Endpoint behavior is deliberate: fully transparent pixels carry no
/// meaningful colour and are normalized to black; opaque pixels are already
/// straight and remain byte-identical. Intermediate channels use round-half and
/// clamp after division, matching the integer definition of unassociation.
#[cfg(any(target_os = "macos", test))]
fn unpremultiply_rgba8(rgba: &mut [u8]) {
    // Tightly-packed RGBA8: callers pass a whole number of pixels, so the chunk
    // remainder is empty. A trailing partial pixel would carry no alpha byte to
    // unassociate by, so leaving it byte-identical is the only defined answer.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        match alpha {
            0 => pixel[..3].fill(0),
            255 => {}
            _ => {
                for channel in &mut pixel[..3] {
                    *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
    }
}

/// Exact placement of the platform client layer inside a full-window capture.
///
/// On macOS a `FullSizeContentView` consumes the whole window, including the
/// transparent titlebar band. There is therefore deliberately no "protected"
/// row range here: every client pixel comes from the exact successful present.
/// Native chrome is a separate transparent [`RgbaOverlay`] composited afterward.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowClientRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// One straight-alpha native view snapshot placed in full-window pixel space.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct RgbaOverlay {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Splice the exact successful-present destination into the platform frame.
///
/// Unlike the retired logical-frame stitch, this performs no centering,
/// background reconstruction, or alpha borrowing. The supplied client already
/// has raw destination dimensions and includes live remainder bands, crops,
/// host effects, and GPU present-only passes. Copying all four straight-RGBA
/// channels prevents a current generation from inheriting stale platform alpha.
/// A full-size macOS client replaces the titlebar-underlay rows too; actual
/// titlebar controls are restored later from a transparent AppKit view snapshot.
/// The platform's boundary-connected outer shape alpha is deliberately multiplied
/// back after all current client/chrome composition; see
/// [`multiply_platform_outer_shape_alpha`].
#[cfg(any(target_os = "macos", windows, test))]
fn stitch_presented_client_into_window_rgba(
    window_rgba: &mut [u8],
    window_width: u32,
    window_height: u32,
    rect: WindowClientRect,
    client: &crate::PresentedClientFrame,
) -> Result<(), String> {
    let width = usize::try_from(window_width)
        .map_err(|_| "window capture width does not fit memory".to_string())?;
    let height = usize::try_from(window_height)
        .map_err(|_| "window capture height does not fit memory".to_string())?;
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "window capture dimensions overflow".to_string())?;
    if window_rgba.len() != expected {
        return Err(format!(
            "window capture buffer has {} bytes, expected {expected}",
            window_rgba.len()
        ));
    }
    if (client.width, client.height) != (rect.width, rect.height) {
        return Err(format!(
            "presented client {}x{}px does not match platform client {}x{}px",
            client.width, client.height, rect.width, rect.height
        ));
    }
    let client_width = usize::try_from(client.width)
        .map_err(|_| "presented client width does not fit memory".to_string())?;
    let client_height = usize::try_from(client.height)
        .map_err(|_| "presented client height does not fit memory".to_string())?;
    let client_expected = client_width
        .checked_mul(client_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "presented client dimensions overflow".to_string())?;
    if client.rgba.len() != client_expected {
        return Err(format!(
            "presented client has {} bytes, expected {client_expected}",
            client.rgba.len()
        ));
    }
    let x =
        usize::try_from(rect.x).map_err(|_| "platform client x does not fit memory".to_string())?;
    let y =
        usize::try_from(rect.y).map_err(|_| "platform client y does not fit memory".to_string())?;
    if x.checked_add(client_width)
        .is_none_or(|right| right > width)
        || y.checked_add(client_height)
            .is_none_or(|bottom| bottom > height)
    {
        return Err(format!(
            "platform client at ({},{}) size {}x{}px exceeds window {}x{}px",
            rect.x, rect.y, rect.width, rect.height, window_width, window_height
        ));
    }
    for source_y in 0..client_height {
        let source = source_y * client_width * 4;
        let destination = ((y + source_y) * width + x) * 4;
        window_rgba[destination..destination + client_width * 4]
            .copy_from_slice(&client.rgba[source..source + client_width * 4]);
    }
    Ok(())
}

/// Multiply the current full-window result by the platform photograph's outer
/// shape mask without borrowing stale platform colour or ordinary client alpha.
///
/// AppKit's full-size content view is rectangular, while WindowServer applies an
/// antialiased rounded outer clip. The exact presented client must replace every
/// ordinary client pixel, but doing that naively turns the four transparent outer
/// corners opaque. Boundary-connected non-opaque platform alpha is precisely the
/// compositor shape mask: flood it inward through non-opaque neighbors, multiply
/// only those current alpha values, and leave current RGB untouched. Any isolated
/// non-opaque platform pixel in an ordinary client row is stale capture content,
/// not outer shape, and is ignored.
#[cfg(any(target_os = "macos", test))]
fn multiply_platform_outer_shape_alpha(
    current_rgba: &mut [u8],
    width: u32,
    height: u32,
    platform_alpha: &[u8],
) -> Result<(), String> {
    let width = usize::try_from(width)
        .map_err(|_| "window capture width does not fit memory".to_string())?;
    let height = usize::try_from(height)
        .map_err(|_| "window capture height does not fit memory".to_string())?;
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| "window capture dimensions overflow".to_string())?;
    let bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| "window capture dimensions overflow".to_string())?;
    if current_rgba.len() != bytes {
        return Err(format!(
            "window capture buffer has {} bytes, expected {bytes}",
            current_rgba.len()
        ));
    }
    if platform_alpha.len() != pixels {
        return Err(format!(
            "window capture shape mask has {} pixels, expected {pixels}",
            platform_alpha.len()
        ));
    }
    if width == 0 || height == 0 {
        return Ok(());
    }

    let mut queued = vec![false; pixels];
    let mut boundary = std::collections::VecDeque::new();
    let mut enqueue = |index: usize| {
        if platform_alpha[index] != 255 && !queued[index] {
            queued[index] = true;
            boundary.push_back(index);
        }
    };
    for x in 0..width {
        enqueue(x);
        enqueue((height - 1) * width + x);
    }
    for y in 0..height {
        enqueue(y * width);
        enqueue(y * width + width - 1);
    }

    while let Some(index) = boundary.pop_front() {
        let alpha = u16::from(platform_alpha[index]);
        let current = &mut current_rgba[index * 4..index * 4 + 4];
        current[3] = ((u16::from(current[3]) * alpha + 127) / 255) as u8;

        let x = index % width;
        let y = index / width;
        for dy in -1_isize..=1 {
            for dx in -1_isize..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let Some(nx) = x.checked_add_signed(dx) else {
                    continue;
                };
                let Some(ny) = y.checked_add_signed(dy) else {
                    continue;
                };
                if nx >= width || ny >= height {
                    continue;
                }
                let neighbor = ny * width + nx;
                if platform_alpha[neighbor] != 255 && !queued[neighbor] {
                    queued[neighbor] = true;
                    boundary.push_back(neighbor);
                }
            }
        }
    }
    Ok(())
}

/// Composite a straight-RGBA overlay over `destination`, clipped to `clip`.
///
/// Both RGB triplets are unassociated (straight) alpha. The integer source-over
/// calculation therefore weights destination RGB by its own alpha before
/// unassociating the result; treating destination RGB as opaque would produce
/// dark fringes around antialiased traffic lights and toolbar glyphs.
#[cfg(any(target_os = "macos", test))]
fn composite_straight_rgba_overlay(
    destination: &mut [u8],
    destination_width: u32,
    destination_height: u32,
    overlay: &RgbaOverlay,
    clip: WindowClientRect,
) -> Result<(), String> {
    let dst_width = usize::try_from(destination_width)
        .map_err(|_| "window capture width does not fit memory".to_string())?;
    let dst_height = usize::try_from(destination_height)
        .map_err(|_| "window capture height does not fit memory".to_string())?;
    let dst_expected = dst_width
        .checked_mul(dst_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "window capture dimensions overflow".to_string())?;
    if destination.len() != dst_expected {
        return Err(format!(
            "window capture buffer has {} bytes, expected {dst_expected}",
            destination.len()
        ));
    }

    let overlay_width = usize::try_from(overlay.width)
        .map_err(|_| "native chrome width does not fit memory".to_string())?;
    let overlay_height = usize::try_from(overlay.height)
        .map_err(|_| "native chrome height does not fit memory".to_string())?;
    let overlay_expected = overlay_width
        .checked_mul(overlay_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "native chrome dimensions overflow".to_string())?;
    if overlay.rgba.len() != overlay_expected {
        return Err(format!(
            "native chrome has {} bytes, expected {overlay_expected}",
            overlay.rgba.len()
        ));
    }

    let overlay_x = usize::try_from(overlay.x)
        .map_err(|_| "native chrome x does not fit memory".to_string())?;
    let overlay_y = usize::try_from(overlay.y)
        .map_err(|_| "native chrome y does not fit memory".to_string())?;
    if overlay_x
        .checked_add(overlay_width)
        .is_none_or(|right| right > dst_width)
        || overlay_y
            .checked_add(overlay_height)
            .is_none_or(|bottom| bottom > dst_height)
    {
        return Err(format!(
            "native chrome at ({},{}) size {}x{}px exceeds window {}x{}px",
            overlay.x,
            overlay.y,
            overlay.width,
            overlay.height,
            destination_width,
            destination_height
        ));
    }

    let clip_x0 =
        usize::try_from(clip.x).map_err(|_| "platform client x does not fit memory".to_string())?;
    let clip_y0 =
        usize::try_from(clip.y).map_err(|_| "platform client y does not fit memory".to_string())?;
    let clip_x1 = clip_x0
        .checked_add(
            usize::try_from(clip.width)
                .map_err(|_| "platform client width does not fit memory".to_string())?,
        )
        .ok_or_else(|| "platform client dimensions overflow".to_string())?;
    let clip_y1 = clip_y0
        .checked_add(
            usize::try_from(clip.height)
                .map_err(|_| "platform client height does not fit memory".to_string())?,
        )
        .ok_or_else(|| "platform client dimensions overflow".to_string())?;
    if clip_x1 > dst_width || clip_y1 > dst_height {
        return Err("platform client clip exceeds the window capture".to_string());
    }

    let x0 = overlay_x.max(clip_x0);
    let y0 = overlay_y.max(clip_y0);
    let x1 = overlay_x.saturating_add(overlay_width).min(clip_x1);
    let y1 = overlay_y.saturating_add(overlay_height).min(clip_y1);
    if x0 >= x1 || y0 >= y1 {
        return Ok(());
    }

    for dst_y in y0..y1 {
        let src_y = dst_y - overlay_y;
        for dst_x in x0..x1 {
            let src_x = dst_x - overlay_x;
            let source = (src_y * overlay_width + src_x) * 4;
            let target = (dst_y * dst_width + dst_x) * 4;
            let src = &overlay.rgba[source..source + 4];
            let dst = &mut destination[target..target + 4];
            let source_alpha = u32::from(src[3]);
            if source_alpha == 0 {
                continue;
            }
            if source_alpha == 255 {
                dst.copy_from_slice(src);
                continue;
            }
            let destination_alpha = u32::from(dst[3]);
            let inverse_source = 255 - source_alpha;
            let alpha_numerator = source_alpha * 255 + destination_alpha * inverse_source;
            if alpha_numerator == 0 {
                dst.fill(0);
                continue;
            }
            for channel in 0..3 {
                let numerator = u32::from(src[channel]) * source_alpha * 255
                    + u32::from(dst[channel]) * destination_alpha * inverse_source;
                dst[channel] = ((numerator + alpha_numerator / 2) / alpha_numerator).min(255) as u8;
            }
            dst[3] = ((alpha_numerator + 127) / 255).min(255) as u8;
        }
    }
    Ok(())
}

/// Encode a tightly-packed RGBA8 buffer (`width * height * 4` bytes, no row
/// padding) to PNG bytes, reusing the same `png` crate the `image` verb's
/// framebuffer path uses. Platform-neutral because both `window` capture and
/// Linux/macOS/Windows video export consume it; only the OS pixel acquisition is
/// platform-gated.
pub(crate) fn encode_rgba8_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        // Both CoreGraphics normalization and AppKit's transparent chrome PNG
        // arrive in sRGB. Declare that transfer function explicitly so viewers
        // never reinterpret the stitched bytes as untagged device RGB.
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod dims_snapshot_tests {
    use std::time::Instant;

    use super::{App, dims_axis, dims_axis_y};
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
        // The vertical axis follows the platform placement: top-pinned on Linux
        // (all slack/crop trailing), the centred rule elsewhere.
        if cfg!(target_os = "linux") {
            assert_eq!(dims_axis_y(111, 100), (0, 0, 11, 0, 0));
            assert_eq!(dims_axis_y(100, 111), (0, 0, 0, 0, 11));
        } else {
            assert_eq!(dims_axis_y(111, 100), dims_axis(111, 100));
            assert_eq!(dims_axis_y(100, 111), dims_axis(100, 111));
        }
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

    /// A DETACHED session (no window holds it) still has a knowable DPI: the
    /// shared backend its cells/pad/font are read out of is tuned to one window's
    /// record. `dims` used to answer a flat `scale=1.0` there, which on a 2x
    /// display contradicted every other field in the same record.
    #[test]
    fn detached_dims_reports_the_scale_its_own_geometry_came_from() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let metrics = crate::MetricsView::for_scale(2.0);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.scale = 2.0;
            ws.metrics = metrics;
        }
        // Tune the shared backend to that window, exactly as `apply_window_scale`
        // does before the window composes.
        app.font_px = metrics.font_px;
        app.backend.activate_px(metrics.font_px);
        app.backend.set_pad(metrics.pad);
        app.backend.set_pad_top(metrics.pad_top);
        app.backend.set_head(metrics.head);

        let attached = app.dims_snapshot(0, 24, 80);
        let detached = app.dims_snapshot(u64::MAX, 24, 80);
        assert_eq!(
            (detached.geometry, detached.window),
            ("detached", None),
            "no window contains this session"
        );
        assert!(
            (detached.scale - 2.0).abs() < f64::EPSILON,
            "the detached record reports the 2x it was derived from, not 1.0 \
             (scale={})",
            detached.scale
        );
        assert_eq!(
            (detached.scale, detached.font_px, detached.pad),
            (attached.scale, attached.font_px, attached.pad),
            "one backend, one scale: detaching a session cannot change the DPI \
             the same pixels were rasterized at"
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
        // Vertical placement is the platform policy: top-pinned on Linux (the
        // whole 28 px remainder lands in the bottom band, keeping the chrome
        // band glued to the titlebar), centred elsewhere.
        if cfg!(target_os = "linux") {
            assert_eq!((banded.band_top, banded.band_bottom), (0, 28));
        } else {
            assert_eq!((banded.band_top, banded.band_bottom), (14, 14));
        }
        assert_eq!(banded.offset_y, i64::from(banded.band_top));
        assert_eq!(
            banded.band_top + banded.band_bottom,
            banded.surface_h - banded.frame_h,
        );
        assert_eq!(
            banded.pad_bottom + banded.band_bottom,
            if cfg!(target_os = "linux") { 40 } else { 26 },
            "visible trailing edge is base bottom plus the raw-surface remainder \
             this platform's vertical placement leaves below the frame \
             (all 28 px on top-pinned Linux, the centred half elsewhere)",
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
mod split_capture_tests {
    //! INTROSPECTION IS SACRED: a split capture must show what the glass
    //! shows. The glass now decorates every visible pane, so the capture does.

    use crate::{App, WindowId, term_lock};
    use std::time::{Duration, Instant};

    #[test]
    fn first_headless_split_capture_keeps_the_pet_without_sparkle_words() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // The trail is asked for, not inherited — its absent-key default is
        // platform-split and this fixture measures the SPLIT-capture pet, on every host.
        app.config.cursor_trail = Some(true);
        app.split_active_stub_tab(wid);
        app.recompute_sparkle();
        app.sparkle = None;
        app.windows.get_mut(&wid).expect("window").focused = true;

        app.splice_word_decorations(wid, Instant::now());
        let ws = app.windows.get(&wid).expect("window");
        assert!(
            ws.cursor_pet.is_active(),
            "the split pet is presentation-live"
        );
        assert!(
            !ws.input_scratch.free_sprites.is_empty() && ws.input_scratch.free_atlas.is_some(),
            "the composed capture carries the focused pane's resident pet and atlas"
        );
    }

    #[test]
    fn first_headless_split_capture_applies_dog_species() {
        let capture = |style: &str| {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.split_active_stub_tab(wid);
            app.config.cursor_trail = Some(true);
            app.config.cursor_trail_style = Some(style.into());
            app.config.motion = Some("full".into());
            app.recompute_sparkle();
            app.sparkle = None;
            app.windows.get_mut(&wid).expect("window").focused = true;

            app.splice_word_decorations(wid, Instant::now());
            let ws = app.windows.get(&wid).expect("window");
            assert!(
                !ws.input_scratch.free_sprites.is_empty() && ws.input_scratch.free_atlas.is_some(),
                "the first composed still emits the configured pet and its atlas"
            );
            (
                ws.cursor_pet.species(),
                std::sync::Arc::clone(
                    ws.input_scratch
                        .free_atlas
                        .as_ref()
                        .expect("split capture publishes the pet atlas"),
                ),
            )
        };

        let cat = capture("rainbow kitty pet");
        let dog = capture("rainbow dog pet");
        assert_eq!(cat.0, aterm_effects::kitty_pet::PetSpecies::Cat);
        assert_eq!(dog.0, aterm_effects::kitty_pet::PetSpecies::Dog);
        assert!(
            dog.1.rgba != cat.1.rgba,
            "the first composed atlas must contain the configured dog skin"
        );
    }

    /// T7 — a composed (split) capture splices the sparkle channels. The gate
    /// this replaces returned early on ANY multi-pane tab, so a split capture
    /// was decoration-free no matter what the window was showing.
    #[test]
    fn split_capture_splices_decorations() {
        let t0 = Instant::now();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid = app.split_active_stub_tab(wid);
        app.recompute_sparkle();
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.focused = true;
        }
        // Establish both panes first (a never-scanned pane spends whatever was
        // already on it), then type the words that must summon cats.
        app.splice_word_decorations(wid, t0);
        for s in [0u64, sid] {
            let term = app.pool.get(s).expect("pane session").term.clone();
            term_lock(&term).process(b"\r\nhello kitty friend\r\n");
        }
        app.windows
            .get_mut(&wid)
            .expect("window")
            .pending_deco_birth = Some(t0);
        app.splice_word_decorations(wid, t0);
        app.splice_word_decorations(wid, t0 + Duration::from_millis(600));
        let ws = app.windows.get(&wid).expect("window");
        assert!(
            !ws.input_scratch.free_sprites.is_empty(),
            "a split capture carries the cats the glass draws (deco={} ink={})",
            ws.input_scratch.word_decorations.len(),
            ws.input_scratch.ink.len()
        );
        assert!(
            ws.input_scratch.free_atlas.is_some(),
            "and the atlas those sprites address"
        );
    }

    /// The composed capture shares the split glass pass, including its
    /// visibility-correct companion custody. A DECTCEM-off program relocation
    /// may not lend the fading resident pet a new word claim at a cursor the
    /// capture does not draw.
    #[test]
    fn split_capture_hidden_pet_fade_does_not_claim_program_relocated_word() {
        let t0 = Instant::now();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let focus = app.split_active_stub_tab(wid);
        // Seeded for the same reason as its present-path twin
        // (`app_render::split_sparkle_tests::split_present_hidden_pet_fade_does_not_
        // claim_program_relocated_word`): the resident is the subject, and
        // `cursor_trail`'s default is platform-split since `bda06044`, so an unseeded
        // fixture measured the platform and went red on Windows.
        app.config.cursor_trail = Some(true);
        app.recompute_sparkle();
        app.windows.get_mut(&wid).expect("window").focused = true;

        app.splice_word_decorations(wid, t0);
        let term = app.pool.get(focus).expect("focused pane").term.clone();
        term_lock(&term).process(b"\x1b[11;3Hkitty\x1b[4;31H");
        app.splice_word_decorations(wid, t0 + Duration::from_millis(16));
        app.splice_word_decorations(wid, t0 + Duration::from_millis(616));
        let ambient_before = app.windows[&wid]
            .input_scratch
            .free_sprites
            .iter()
            .filter(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::UnderText))
            .count();
        assert_eq!(
            ambient_before, 1,
            "negative control: the isolated far word has one settled ambient kitty"
        );

        term_lock(&term).process(b"\x1b[?25l\x1b[11;3H");
        app.splice_word_decorations(wid, t0 + Duration::from_millis(632));
        {
            let term = term_lock(&term);
            assert!(!term.cursor_visible());
            assert_eq!((term.cursor().row, term.cursor().col), (10, 2));
        }
        let ws = &app.windows[&wid];
        let ambient_after = ws
            .input_scratch
            .free_sprites
            .iter()
            .filter(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::UnderText))
            .count();
        let resident_after = ws
            .input_scratch
            .free_sprites
            .iter()
            .filter(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::OverText))
            .count();
        assert!(
            resident_after >= 1 && ws.cursor_pet.is_active(),
            "the capture genuinely contains the hidden-caret resident fade"
        );
        assert_eq!(
            ambient_after, ambient_before,
            "capture must not suppress a word-cat at the invisible relocated cursor"
        );
    }

    #[test]
    fn split_capture_holds_a_hidden_midflight_pet_and_quick_return_walks() {
        let t0 = Instant::now();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let focus = app.split_active_stub_tab(wid);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty pet".into());
        app.config.motion = Some("full".into());
        app.recompute_sparkle();
        app.sparkle = None;
        app.windows.get_mut(&wid).expect("window").focused = true;
        app.splice_word_decorations(wid, t0);

        let (pane_rows, pane_cols) = {
            let ws = &app.windows[&wid];
            let tree = &ws.layouts[ws.tabs.active];
            let rect = tree
                .compute_layout(ws.rows, ws.cols)
                .into_iter()
                .find(|rect| rect.session == focus)
                .expect("focused pane layout");
            (rect.rows, rect.cols)
        };
        let (cw, ch) = app.win_cell_size(wid);
        let target = (2, 24);
        let (flight_at, flying) = {
            let ws = app.windows.get_mut(&wid).expect("window");
            crate::app_render::seed_resident_pet_mid_flight_for_test(
                &mut ws.cursor_pet,
                t0 + Duration::from_millis(16),
                pane_rows,
                pane_cols,
                cw as u16,
                ch as u16,
                target,
            )
        };
        let width =
            aterm_effects::kitty_pet::ART_ROWS * ch as f32 * aterm_effects::kitty_pet::ART_ASPECT
                / cw as f32;
        let target_col = aterm_effects::kitty_pet::PetBrain::station(target.1, pane_cols, width);
        assert!(
            (flying.col - target_col).abs() > 10.0,
            "negative control: the capture fixture is visibly mid-flight"
        );

        let term = app.pool.get(focus).expect("focused pane").term.clone();
        let captured_body = |app: &App| {
            let sprite = app.windows[&wid]
                .input_scratch
                .free_sprites
                .iter()
                .filter(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::OverText))
                .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                .expect("capture contains the resident body");
            (sprite.x, sprite.y, sprite.w, sprite.h)
        };
        term_lock(&term).process(b"\x1b[3;25H\x1b[?25h");
        let visible_at = flight_at + Duration::from_millis(16);
        app.splice_word_decorations(wid, visible_at);
        let before = captured_body(&app);

        term_lock(&term).process(b"\x1b[?25l");
        let hidden_at = visible_at + Duration::from_millis(16);
        app.splice_word_decorations(wid, hidden_at);
        let hidden = captured_body(&app);
        assert!(
            (hidden.0 - before.0).abs() <= cw as i32,
            "hidden capture teleported the fading body: {before:?} -> {hidden:?}"
        );
        assert!(
            ((hidden.1 + i32::from(hidden.3)) - (before.1 + i32::from(before.3))).abs() <= 1,
            "hidden capture dropped the fading body's feet: {before:?} -> {hidden:?}"
        );
        assert!(
            app.windows[&wid]
                .input_scratch
                .free_sprites
                .iter()
                .any(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::OverText)),
            "the inspected hidden capture really contains the resident body"
        );

        term_lock(&term).process(b"\x1b[?25h");
        let returned_at = hidden_at + Duration::from_millis(16);
        app.splice_word_decorations(wid, returned_at);
        let returned = captured_body(&app);
        assert!(
            (returned.0 - hidden.0).abs() <= (2 * cw) as i32,
            "quick-return capture applied the deferred far landing: \
             {hidden:?} -> {returned:?}"
        );
        assert!(
            ((returned.1 + i32::from(returned.3)) - (hidden.1 + i32::from(hidden.3))).abs()
                <= ch as i32,
            "quick-return capture jumped the fading body's feet: \
             {hidden:?} -> {returned:?}"
        );
    }
}

#[cfg(test)]
mod chrome_output_tests {
    use super::{
        RgbaOverlay, WindowClientRect, composite_straight_rgba_overlay,
        multiply_platform_outer_shape_alpha, non_macos_chrome_output,
        snapshot_frame_from_presented_client, stitch_presented_client_into_window_rgba,
        unpremultiply_rgba8,
    };

    #[test]
    fn non_macos_app_chrome_includes_live_titles_and_icon_policy() {
        let terminal = crate::tab_model::TabPresentation::terminal("build-server");
        let settings = crate::tab_model::TabPresentation {
            title: "Settings".to_string(),
            icon: Some(crate::tab_model::TabIconKind::Settings),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
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
    fn window_stitch_replaces_fullsize_titlebar_underlay_and_rgba_atomically() {
        // FullSizeContentView means all three rows are renderer-owned, including
        // the titlebar underlay. Native controls are a later transparent overlay;
        // no stale platform row survives this exact client replacement.
        let mut rgba = vec![
            1, 2, 3, 255, 4, 5, 6, 255, // stale titlebar underlay
            7, 8, 9, 255, 10, 11, 12, 31, // stale client + stale alpha
            13, 14, 15, 255, 16, 17, 18, 255, // stale client
        ];
        let client = crate::PresentedClientFrame {
            width: 2,
            height: 3,
            rgba: vec![
                99, 98, 97, 96, 95, 94, 93, 92, // hidden beneath titlebar
                0x11, 0x22, 0x33, 255, 0x44, 0x55, 0x66, 127, // current client
                0x77, 0x88, 0x99, 64, 0xAA, 0xBB, 0xCC, 0, // current client
            ],
        };
        stitch_presented_client_into_window_rgba(
            &mut rgba,
            2,
            3,
            WindowClientRect {
                x: 0,
                y: 0,
                width: 2,
                height: 3,
            },
            &client,
        )
        .unwrap();

        assert_eq!(&rgba[..8], &[99, 98, 97, 96, 95, 94, 93, 92]);
        assert_eq!(&rgba[8..12], &[0x11, 0x22, 0x33, 255]);
        assert_eq!(
            &rgba[12..16],
            &[0x44, 0x55, 0x66, 127],
            "current destination RGB and alpha move as one generation"
        );
        assert_eq!(&rgba[16..], &[0x77, 0x88, 0x99, 64, 0xAA, 0xBB, 0xCC, 0]);
    }

    #[test]
    fn mac_window_stitch_keeps_fresh_client_and_outer_shape_alpha_only() {
        let (width, height) = (5_u32, 5_u32);
        let mut platform_alpha = vec![255_u8; (width * height) as usize];
        for (x, y) in [(0, 0), (4, 0), (0, 4), (4, 4)] {
            platform_alpha[(y * width + x) as usize] = 0;
        }
        for (x, y) in [
            (1, 0),
            (0, 1),
            (3, 0),
            (4, 1),
            (0, 3),
            (1, 4),
            (4, 3),
            (3, 4),
        ] {
            platform_alpha[(y * width + x) as usize] = 128;
        }
        // An isolated stale alpha sample in an ordinary client row is not an
        // outer shape pixel and must not survive.
        platform_alpha[(2 * width + 2) as usize] = 31;

        let mut window = vec![0_u8; (width * height * 4) as usize];
        let mut fresh = Vec::with_capacity(window.len());
        for i in 0..(width * height) {
            fresh.extend_from_slice(&[(100 + i) as u8, 70, 40, 200]);
        }
        let client = crate::PresentedClientFrame {
            width,
            height,
            rgba: fresh.clone(),
        };
        stitch_presented_client_into_window_rgba(
            &mut window,
            width,
            height,
            WindowClientRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            &client,
        )
        .unwrap();
        multiply_platform_outer_shape_alpha(&mut window, width, height, &platform_alpha).unwrap();

        // Byte offset of the (x, y) pixel's first channel in the RGBA window,
        // so every assertion below keeps its row/column shape on the page.
        let at = |x: u32, y: u32| ((y * width + x) * 4) as usize;
        for (x, y) in [(0, 0), (4, 0), (0, 4), (4, 4)] {
            let index = at(x, y);
            assert_eq!(
                &window[index..index + 3],
                &fresh[index..index + 3],
                "shape retention never restores stale platform RGB"
            );
            assert_eq!(window[index + 3], 0);
        }
        // The half-shape pixel at (1, 0): 200 fresh alpha scaled by the 128
        // platform sample.
        assert_eq!(window[at(1, 0) + 3], 100);
        let center = at(2, 2);
        assert_eq!(&window[center..center + 3], &fresh[center..center + 3]);
        assert_eq!(
            window[center + 3],
            200,
            "isolated platform alpha in an ordinary row is stale and ignored"
        );
    }

    #[test]
    fn sigusr_windowed_snapshot_roundtrips_exact_non_cell_destination_and_present_passes() {
        // 3×2 deliberately cannot be expressed as this fixture's hypothetical
        // 2×2 cell grid. The last column is a destination remainder band; the
        // other sentinels stand for already-applied SDR crown / HDR glow output.
        let client = crate::PresentedClientFrame {
            width: 3,
            height: 2,
            rgba: vec![
                11, 12, 13, 255, // semantic base
                90, 80, 70, 255, // present-only crown transformed pixel
                3, 4, 5, 255, // right remainder band
                21, 31, 41, 255, // semantic base
                210, 160, 120, 192, // present-only glow + alpha
                6, 7, 8, 255, // right remainder band
            ],
        };
        let frame = snapshot_frame_from_presented_client(&client).unwrap();
        assert_eq!((frame.width, frame.height), (3, 2));
        assert_eq!(
            frame.rgba_bytes(),
            client.rgba,
            "snapshot container conversion cannot rerender, crop, or recolor the exact destination"
        );

        let source = include_str!("app_introspect.rs");
        let snapshot = source
            .split("pub(crate) fn snapshot(&mut self)")
            .nth(1)
            .and_then(|tail| tail.split("pub(crate) fn submit_encode_job").next())
            .expect("SIGUSR snapshot source");
        assert!(snapshot.contains("present_before_window_capture(front)"));
        let exact_branch = snapshot
            .split("if let Some(exact) = exact_presented")
            .nth(1)
            .and_then(|tail| tail.split("return;").next())
            .expect("windowed exact-destination branch");
        assert!(exact_branch.contains("snapshot_frame_from_presented_client(&exact.client)"));
        assert!(
            !exact_branch.contains("render_input_for_destination"),
            "windowed SIGUSR must never replace the captured destination with a semantic rerender"
        );
        assert!(
            source.contains("presented_snapshot_begin(window_gpu, gpu_surface)"),
            "the windowed source path remains wired to the GPU one-shot destination tap"
        );
    }

    #[test]
    fn window_stitch_uses_explicit_asymmetric_client_origin() {
        // A 2×2 client at (2,1) inside asymmetric chrome/borders. The pixels
        // already contain arbitrary live remainder/crop output; the stitch
        // neither recenters nor synthesizes any band.
        let mut window = vec![9_u8; 5 * 4 * 4];
        let client = crate::PresentedClientFrame {
            width: 2,
            height: 2,
            rgba: (0_u8..16).collect(),
        };
        stitch_presented_client_into_window_rgba(
            &mut window,
            5,
            4,
            WindowClientRect {
                x: 2,
                y: 1,
                width: 2,
                height: 2,
            },
            &client,
        )
        .unwrap();
        // Byte offset of the (row, col) pixel in the 5-wide RGBA window, so the
        // client's two rows stay legible as rows rather than as byte counts.
        let at = |row: usize, col: usize| (row * 5 + col) * 4;
        assert_eq!(&window[at(1, 2)..at(1, 4)], &client.rgba[..8]);
        assert_eq!(&window[at(2, 2)..at(2, 4)], &client.rgba[8..]);
        assert!(window[..at(1, 2)].iter().all(|&byte| byte == 9));
        assert!(window[at(3, 0)..].iter().all(|&byte| byte == 9));
    }

    #[test]
    fn window_stitch_and_unpremultiply_fail_closed_and_cover_alpha_endpoints() {
        let honest = crate::PresentedClientFrame {
            width: 2,
            height: 1,
            rgba: vec![0; 8],
        };
        let rect = WindowClientRect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        assert!(
            stitch_presented_client_into_window_rgba(&mut [0; 7], 2, 1, rect, &honest).is_err()
        );
        let wrong_shape = crate::PresentedClientFrame {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        assert!(
            stitch_presented_client_into_window_rgba(&mut [0; 8], 2, 1, rect, &wrong_shape)
                .is_err()
        );
        let outside = WindowClientRect { x: 1, ..rect };
        assert!(
            stitch_presented_client_into_window_rgba(&mut [0; 8], 2, 1, outside, &honest).is_err()
        );

        let mut premultiplied = vec![
            9, 8, 7, 0, // transparent normalizes colour
            64, 32, 16, 128, // half-alpha unassociates to approximately 128/64/32
            4, 5, 6, 255, // opaque is byte-identical
        ];
        unpremultiply_rgba8(&mut premultiplied);
        assert_eq!(premultiplied, [0, 0, 0, 0, 128, 64, 32, 128, 4, 5, 6, 255]);
    }

    #[test]
    fn appkit_chrome_uses_straight_alpha_source_over_and_exact_placement() {
        let mut destination = vec![
            9, 8, 7, 255, // untouched left
            0, 0, 255, 128, // translucent blue beneath chrome
            4, 5, 6, 255, // transparent source leaves this exact
            1, 2, 3, 255, // second row untouched
            1, 2, 3, 255, //
            1, 2, 3, 255, //
        ];
        let chrome = RgbaOverlay {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
            rgba: vec![
                255, 0, 0, 128, // half-red over half-blue
                200, 100, 50, 0, // transparent payload must not alter destination
            ],
        };
        let clip = WindowClientRect {
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        };
        composite_straight_rgba_overlay(&mut destination, 3, 2, &chrome, clip).unwrap();
        assert_eq!(&destination[..4], &[9, 8, 7, 255]);
        assert_eq!(
            &destination[4..8],
            &[170, 0, 85, 192],
            "straight-alpha source-over must weight both source and destination alpha"
        );
        assert_eq!(&destination[8..12], &[4, 5, 6, 255]);
        // Bytes 12.. are the whole second row — three whole pixels — so the
        // `as_chunks` remainder is always empty; the discarded tail is the one
        // `chunks_exact` also dropped.
        let (second_row, _) = destination[12..].as_chunks::<4>();
        assert!(second_row.iter().all(|px| *px == [1, 2, 3, 255]));
    }

    #[test]
    fn appkit_chrome_is_clipped_to_replaced_client_and_fails_closed_on_bad_shape() {
        let chrome = RgbaOverlay {
            x: 0,
            y: 0,
            width: 3,
            height: 1,
            rgba: vec![11, 12, 13, 255, 21, 22, 23, 255, 31, 32, 33, 255],
        };
        let clip = WindowClientRect {
            x: 1,
            y: 0,
            width: 1,
            height: 1,
        };
        let mut destination = vec![0_u8; 3 * 2 * 4];
        composite_straight_rgba_overlay(&mut destination, 3, 2, &chrome, clip).unwrap();
        assert_eq!(&destination[..4], &[0, 0, 0, 0]);
        assert_eq!(&destination[4..8], &[21, 22, 23, 255]);
        assert_eq!(&destination[8..], &[0; 16]);

        let wrong_buffer = RgbaOverlay {
            rgba: vec![0; 11],
            ..chrome.clone()
        };
        assert!(
            composite_straight_rgba_overlay(&mut destination, 3, 2, &wrong_buffer, clip).is_err()
        );
        let outside = RgbaOverlay {
            x: 2,
            width: 2,
            rgba: vec![0; 8],
            ..chrome
        };
        assert!(composite_straight_rgba_overlay(&mut destination, 3, 2, &outside, clip).is_err());
        let invalid_clip = WindowClientRect {
            x: 3,
            width: 1,
            ..clip
        };
        let inside = RgbaOverlay {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        assert!(
            composite_straight_rgba_overlay(&mut destination, 3, 2, &inside, invalid_clip).is_err()
        );
    }

    #[test]
    fn mac_window_capture_source_closes_draw_count_and_srgb_boundaries() {
        let source = include_str!("app_introspect.rs");
        let capture = source
            .split("pub(crate) fn capture_window_pixels")
            .nth(1)
            .and_then(|tail| tail.split("fn unpremultiply_rgba8").next())
            .expect("capture function source");
        assert_eq!(
            capture
                .matches("CGContextDrawImage(context, full, image)")
                .count(),
            1,
            "a translucent platform image must be normalized exactly once"
        );
        assert!(capture.contains("CGColorSpaceCreateWithName"));
        assert!(
            !capture.contains("CGColorSpaceCreateDeviceRGB"),
            "canonical-sRGB output must never normalize through DeviceRGB"
        );

        let chrome = source
            .split("fn native_chrome_overlay_of")
            .nth(1)
            .and_then(|tail| tail.split("fn current_window_rgba_of").next())
            .expect("native chrome capture source");
        assert!(
            chrome.contains("bitmapImageRepByConvertingToColorSpace_renderingIntent"),
            "AppKit backing pixels need a real profile conversion before composition"
        );
        assert!(chrome.contains("NSColorSpace::sRGBColorSpace"));

        let ffi = include_str!("lib.rs");
        assert!(ffi.contains("pub fn CGColorSpaceCreateWithName"));
        assert!(!ffi.contains("pub fn CGColorSpaceCreateDeviceRGB"));
    }
}

#[cfg(test)]
mod terminal_split_capture_tests {
    use super::{App, terminal_capture_text};
    use crate::{WindowId, control_auth, term_lock};
    use std::time::{Duration, Instant};

    fn split_fixture() -> (App, WindowId, u64) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let right = app.split_active_stub_tab(wid);
        let left_term = app.pool.get(0).expect("left pane").term.clone();
        let right_term = app.pool.get(right).expect("right pane").term.clone();
        term_lock(&left_term).process(b"\x1b]11;#102030\x07LEFT");
        term_lock(&right_term).process(b"\x1b]11;#405060\x07RIGHT");
        (app, wid, right)
    }

    fn confined(dir: &std::path::Path, name: &str) -> control_auth::ConfinedImage {
        control_auth::ConfinedImage::for_test(dir, name)
    }

    #[test]
    fn presented_terminal_authority_rejects_unpresented_staging_and_binds_geometry() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let terminal = app.front_terminal(wid).unwrap().term.clone();
        term_lock(&terminal).process(b"A");
        app.prepare_terminal_capture_grid(wid)
            .expect("initial terminal model");
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.capture_leaf_snapshot_seqs = vec![window.input_scratch.snapshot_seq];
        }

        assert!(
            app.presented_terminal_capture_after(wid, 0).is_none(),
            "pre-first-present staged pixels have no capture authority"
        );

        let overlay = crate::app_render::OverlayGlow {
            accent: 0x0012_3456,
            wash_a: 17,
            border_a: 203,
        };
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.capture_present_serial = 1;
            window.capture_present_invert = true;
            window.capture_present_overlay = Some(overlay);
            window.input_scratch.cursor_row = 3;
            window.input_scratch.cursor_col = 7;
        }
        let frame_a = app
            .presented_terminal_capture_after(wid, 0)
            .expect("successful A is authorized");
        assert_eq!(frame_a.serial, 1);
        assert!(frame_a.invert);
        assert_eq!(frame_a.overlay, Some(overlay));
        assert_eq!((frame_a.input.cursor_row, frame_a.input.cursor_col), (3, 7));

        // Stage a materially different B (cursor + content + font/DPI metric),
        // but model a dropped surface transaction by NOT advancing the serial.
        // Neither the old A capture nor a new capture-after-A may observe B.
        let old_cell_size = frame_a.cell_size;
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.input_scratch.cursor_row = 9;
            window.input_scratch.cursor_col = 11;
            window.input_scratch.cells[0][0].ch = 'B';
            window.metrics = crate::MetricsView::applied(27.0, 9, 4, 3);
        }
        assert!(
            app.presented_terminal_capture_after(wid, 1).is_none(),
            "a dropped B cannot authorize mutable staged buffers"
        );
        assert_eq!(frame_a.input.cells[0][0].ch, 'A');
        assert_eq!(
            (frame_a.input.cursor_row, frame_a.input.cursor_col),
            (3, 7),
            "the owned A snapshot cannot tear with staged B"
        );

        // Once B succeeds, all of its generation moves together: pixels,
        // cursor and DPI-derived cell geometry are sampled in the same
        // main-thread success turn.
        app.windows.get_mut(&wid).unwrap().capture_present_serial = 2;
        let frame_b = app
            .presented_terminal_capture_after(wid, 1)
            .expect("successful B is authorized");
        assert_eq!(frame_b.input.cells[0][0].ch, 'B');
        assert_eq!(
            (frame_b.input.cursor_row, frame_b.input.cursor_col),
            (9, 11)
        );
        assert_ne!(frame_b.cell_size, old_cell_size);
    }

    #[test]
    fn presented_frame_authority_accepts_native_and_rejects_unpresented_staging() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(app.prepare_native_input_scratch(wid));
        assert!(
            app.presented_frame_capture_after(wid, 0).is_none(),
            "staged native pixels have no successful-present authority"
        );

        let overlay = crate::app_render::OverlayGlow {
            accent: 0x0065_43AA,
            wash_a: 21,
            border_a: 177,
        };
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.capture_present_serial = 1;
            window.capture_present_invert = true;
            window.capture_present_overlay = Some(overlay);
            window.input_scratch.cursor_row = 4;
        }
        let frame_a = app
            .presented_frame_capture_after(wid, 0)
            .expect("native present A is authorized");
        assert_eq!(frame_a.serial, 1);
        assert!(frame_a.invert);
        assert_eq!(frame_a.overlay, Some(overlay));
        assert_eq!(frame_a.input.cursor_row, 4);

        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.input_scratch.cursor_row = 9;
            window.capture_present_invert = false;
            window.capture_present_overlay = None;
        }
        assert!(
            app.presented_frame_capture_after(wid, 1).is_none(),
            "a staged native B cannot tear into the authorized A capture"
        );
        assert_eq!(frame_a.input.cursor_row, 4);
        assert!(frame_a.invert);

        app.windows.get_mut(&wid).unwrap().capture_present_serial = 2;
        let frame_b = app
            .presented_frame_capture_after(wid, 1)
            .expect("native present B is authorized");
        assert_eq!(frame_b.input.cursor_row, 9);
        assert!(!frame_b.invert);
        assert_eq!(frame_b.overlay, None);
    }

    #[test]
    fn presented_split_authority_tracks_the_successful_divider_generation() {
        let (mut app, wid, _) = split_fixture();
        let (rows, cols) = {
            let window = &app.windows[&wid];
            (usize::from(window.rows), usize::from(window.cols))
        };
        assert!(
            app.redraw_compose(wid, rows, cols, false, false, None, 0, Instant::now(),)
                .is_some()
        );
        app.windows.get_mut(&wid).unwrap().capture_present_serial = 1;
        let frame_a = app
            .presented_terminal_capture_after(wid, 0)
            .expect("initial split frame");
        assert!(frame_a.grid.composed);
        assert_eq!(frame_a.grid.leaves.len(), 2);
        let widths_a: Vec<_> = frame_a.grid.leaves.iter().map(|leaf| leaf.cols).collect();

        let hit = app
            .active_tree(wid)
            .unwrap()
            .divider_at(5, 40, rows as u16, cols as u16)
            .expect("initial divider");
        let ratio = app
            .active_tree(wid)
            .unwrap()
            .ratio_for_pointer(&hit, 5, 20)
            .expect("new divider ratio");
        assert!(
            app.active_tree_mut(wid)
                .unwrap()
                .set_divider_ratio(&hit, ratio)
        );
        assert!(
            app.sync_tab_model_from_layout(wid, 0),
            "the shipping terminal-divider path updates canonical geometry too"
        );
        app.windows.get_mut(&wid).unwrap().last_present = None;
        assert!(
            app.redraw_compose(
                wid,
                rows,
                cols,
                false,
                false,
                None,
                0,
                Instant::now() + Duration::from_millis(1),
            )
            .is_some()
        );
        assert!(
            app.presented_terminal_capture_after(wid, 1).is_none(),
            "the resized divider remains staged until a real present succeeds"
        );
        assert_eq!(
            frame_a
                .grid
                .leaves
                .iter()
                .map(|leaf| leaf.cols)
                .collect::<Vec<_>>(),
            widths_a,
            "retained A geometry is immutable"
        );

        app.windows.get_mut(&wid).unwrap().capture_present_serial = 2;
        let frame_b = app
            .presented_terminal_capture_after(wid, 1)
            .expect("resized split B");
        let widths_b: Vec<_> = frame_b.grid.leaves.iter().map(|leaf| leaf.cols).collect();
        assert_ne!(widths_b, widths_a);
        assert_eq!(widths_b.iter().sum::<usize>() + 1, cols);
    }

    #[test]
    fn recording_image_keeps_the_exact_successful_present_cursor_layers() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let now = Instant::now();
        let font_px = app.windows[&wid].metrics.font_px;
        let single = app
            .prepare_terminal_capture_grid(wid)
            .expect("single-pane model");
        assert!(
            !single.composed,
            "regression covers the normal one-pane route"
        );
        let capture_leaf_snapshot_seqs: Vec<_> =
            single.leaves.iter().map(|leaf| leaf.snapshot_seq).collect();
        assert_eq!(capture_leaf_snapshot_seqs.len(), 1);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .capture_leaf_snapshot_seqs = capture_leaf_snapshot_seqs;

        let mut gpu = match aterm_gpu::GpuRenderer::new(font_px, app.theme) {
            Ok(gpu) => gpu,
            Err(error) => {
                eprintln!("SKIP: no headless GPU/font available: {error}");
                return;
            }
        };
        gpu.set_pad(12);
        gpu.set_pad_top(2);
        assert_eq!((gpu.pad(), gpu.pad_top()), (12, 2));
        app.backend = crate::BackendSlot::Ready(crate::Backend::Gpu(crate::GpuBackend::new(gpu)));
        app.bind_window_renderer_state(wid);
        let (input_rows, input_cols) = {
            let input = &app.windows[&wid].input_scratch;
            (input.rows, input.cols)
        };
        let (width, height) = app.backend.frame_size(input_rows, input_cols);
        let (_, cell_h) = app.backend.cell_size();
        let mut window_gpu = aterm_gpu::WindowGpu::new();
        app.backend
            .gpu_ref()
            .expect("installed GPU fixture")
            .virtual_begin(
                &mut window_gpu,
                u32::try_from(width).expect("fixture width"),
                u32::try_from(height).expect("fixture height"),
                aterm_gpu::video_tap::CaptureOpts {
                    half_res: false,
                    budget_bytes: aterm_gpu::video_tap::DEFAULT_BUDGET,
                    fps_cap: None,
                    requested_ms: 0,
                },
            )
            .expect("real virtual recording target");
        app.windows.get_mut(&wid).unwrap().present =
            Some(crate::PresentTarget::Virtual { window_gpu });

        let root = std::env::temp_dir().join(format!(
            "aterm-recording-image-retain-{}-{}",
            std::process::id(),
            crate::metrics::now_us()
        ));
        let _ = std::fs::remove_dir_all(&root);
        control_auth::ensure_private_dir(&root).expect("private capture root");
        let video_dir = control_auth::confine_video_dir(&root).expect("recording directory");
        let (video_reply, _video_rx) = std::sync::mpsc::channel();
        app.video_rec = Some(crate::VideoRec {
            window: wid,
            deadline: now + Duration::from_secs(3),
            started_us: 0,
            keys: false,
            key_log: Vec::new(),
            unseamed_at_begin: 0,
            unlogged_other_window: 0,
            mode: crate::VideoMode::OffscreenPresentReal,
            next_frame: None,
            presented: None,
            dir: video_dir,
            cancel: crate::VideoCancellation::new(),
            reply: video_reply,
        });

        let (early_reply, early_rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            target: confined(&root, "too-early.png"),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply: early_reply,
        });
        let early = early_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("pre-baseline image reply")
            .expect_err("a recording with no successful frame must defer");
        assert!(
            early.contains("recording has not presented a frame yet"),
            "unexpected pre-baseline verdict: {early}"
        );

        let trail_color = 0x00FA_01B5;
        let kitty_color = [0x22, 0xDD, 0x66, 0xFF];
        {
            let window = app.windows.get_mut(&wid).expect("recorded window");
            window.input_scratch.cursor_trail = vec![aterm_render::TrailCell {
                row: 4,
                col: 12,
                alpha: 255,
            }];
            window.input_scratch.cursor_trail_color = trail_color;
            window.input_scratch.cursor_glow_add = vec![aterm_render::GlowQuad {
                row: 4,
                x: 1,
                y: 1,
                w: 1,
                h: 1,
                color: 0x0001_0101,
            }];
            window.input_scratch.glow_halo = vec![aterm_render::RainHalo {
                row: 4,
                x: 8,
                y: 3,
                w: 3,
                h: 3,
                color: 0x0008_1020,
                cx: 9,
                cy: 4,
                rx: 2,
                ry: 2,
                mode: aterm_core::render::HaloMode::Add,
            }];
            window.input_scratch.word_decorations = vec![aterm_render::WordDecoration {
                row: 7,
                col: 20,
                dx: 0,
                dy: 0,
                glyph: aterm_render::DecoGlyph::Dot,
                blend: aterm_render::DecoBlend::Over,
                color: 0x00CC_44FF,
                alpha: 255,
            }];
            window.input_scratch.free_atlas = Some(std::sync::Arc::new(aterm_render::SceneAtlas {
                width: 2,
                height: 2,
                rgba: kitty_color.repeat(4),
                version: 0xC47,
            }));
            window.input_scratch.free_sprites = vec![aterm_core::render::FreeSprite {
                x: 30,
                y: i32::try_from(6 * cell_h).expect("fixture kitty y"),
                w: 2,
                h: 2,
                ax: 0,
                ay: 0,
                aw: 2,
                ah: 2,
                tint: 0x00FF_FFFF,
                alpha: 255,
                flip_x: false,
                z: aterm_core::render::FreeZ::OverText,
                sampler: aterm_core::render::FreeSampler::Nearest,
            }];
        }
        app.bind_window_renderer_state(wid);
        app.present_input_scratch_for_test(wid, false, None)
            .expect("the real virtual submission succeeds");
        let (video_status, device_lost) = {
            let gpu = app.backend.gpu_ref().expect("installed GPU fixture");
            let window_gpu = match app.windows[&wid].present.as_ref() {
                Some(crate::PresentTarget::Virtual { window_gpu }) => window_gpu,
                _ => panic!("installed virtual recording target"),
            };
            (gpu.video_status(window_gpu), gpu.device_lost())
        };
        assert_eq!(
            video_status,
            Some((0, false)),
            "fixture must keep its video tap live through the successful virtual submission"
        );
        assert!(
            !device_lost,
            "fixture GPU must remain live after submission"
        );
        assert!(
            app.video_rec.as_ref().is_some_and(|recording| {
                !recording.cancel.is_cancelled() && Instant::now() < recording.deadline
            }),
            "fixture recording must be live and before its deadline"
        );
        let route = app.active_visible_content_route(wid).unwrap();
        app.finalize_successful_terminal_present_for_test(
            wid,
            route,
            crate::app_render::HostVisualState::default(),
        );
        let successful_serial = app.windows[&wid].capture_present_serial;
        // Font fallback convergence may clear this immediately after a
        // successful submit. It is deliberately not capture authority.
        app.windows.get_mut(&wid).unwrap().last_present = None;

        // Interleave the old failure mode: a fresh snapshot/layout extraction
        // rewrites the shared scratch and its leaf sequence vector after the
        // video frame succeeds. The recording artifact must remain immutable.
        app.prepare_terminal_capture_grid(wid)
            .expect("interleaved terminal snapshot");
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.capture_leaf_snapshot_seqs.clear();
            let scratch = &mut window.input_scratch;
            scratch.cursor_trail.clear();
            scratch.cursor_glow_add.clear();
            scratch.glow_halo.clear();
            scratch.word_decorations.clear();
            scratch.free_sprites.clear();
            scratch.free_atlas = None;
        }
        let retained = app
            .recording_presented_frame_capture(wid, true, false)
            .expect("resident successful frame survives scratch interleave");
        assert_eq!(retained.frame.input.cursor_trail.len(), 1);
        assert_eq!(retained.frame.input.cursor_trail_color, trail_color);
        assert_eq!(retained.frame.input.cursor_glow_add.len(), 1);
        assert_eq!(
            retained.frame.input.cursor_glow_add[0].y, 1,
            "resident semantic capture undoes the nonzero pad transport shift"
        );
        assert_eq!(retained.frame.input.glow_halo[0].y, 3);
        assert_eq!(retained.frame.input.word_decorations.len(), 1);
        assert_eq!(retained.frame.input.free_sprites.len(), 1);
        drop(retained);

        app.windows.get_mut(&wid).unwrap().blink_phase = false;
        assert!(
            app.recording_presented_frame_capture(wid, true, false)
                .is_err(),
            "renderer-state drift must defer rather than reraster old input with a new blink phase"
        );
        assert!(
            app.recording_presented_destination_capture(wid).is_ok(),
            "live renderer drift cannot invalidate already-presented destination pixels"
        );
        app.windows.get_mut(&wid).unwrap().blink_phase = true;

        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            target: confined(&root, "retained.png"),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(&metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        let (_, _, png) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("image worker reply")
            .expect("recording still succeeds")
            .value;
        let png = png.expect("the retained model is encoded");
        let (rgba, _, _) = aterm_render::decode_png_rgba8(&png).expect("encoded RGBA8");
        let expected = [0xFA, 0x01, 0xB5];
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[..3] == expected),
            "the exact encoded pixels retain the opaque rainbow-trail sentinel"
        );
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel.as_slice() == kitty_color.as_slice()),
            "the exact encoded pixels retain the free-sprite/kitty sentinel"
        );
        assert_eq!(
            metadata.get().expect("image metadata").capture_serial,
            successful_serial,
            "the still identifies the same successful present as the recording"
        );
        let staged = &app.windows[&wid].input_scratch;
        assert!(staged.cursor_trail.is_empty());
        assert!(staged.cursor_glow_add.is_empty());

        if let Some(crate::PresentTarget::Virtual { window_gpu }) =
            app.windows.get_mut(&wid).unwrap().present.as_mut()
        {
            window_gpu.invalidate_present();
        }
        assert!(
            app.recording_presented_destination_capture(wid).is_ok(),
            "semantic invalidation leaves the last successful virtual destination readable"
        );
        app.present_input_scratch_for_test(wid, false, None)
            .expect("stage a newer virtual destination without success authority");
        assert!(
            app.recording_presented_destination_capture(wid).is_err(),
            "a newer destination generation cannot pair with the old successful serial"
        );

        let _ = app.video_rec.take();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn single_capture_dynamic_cursor_tracks_osc10_in_state_and_pixels() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut term = term_lock(&terminal);
            term.process(b"\x1b]21;cursor=\x07\x1b]10;#21C365\x07\x1b[2 q");
            assert_eq!(term.cursor_color(), None);
        }

        let capture = app
            .prepare_terminal_capture_grid(wid)
            .expect("single terminal capture");
        assert!(!capture.composed);
        let input = app.windows[&wid].input_scratch.clone();
        assert_eq!(input.cursor_color, 0x0021_C365);

        let Some(mut renderer) =
            aterm_render::Renderer::from_system(16.0, aterm_render::Theme::default())
        else {
            eprintln!("SKIP: no system monospace font");
            return;
        };
        let (cw, ch) = renderer.cell_size();
        let frame = renderer.render_input(&input);
        assert_eq!(
            frame
                .pixels
                .iter()
                .filter(|&&pixel| pixel == 0x0021_C365)
                .count(),
            cw * ch,
            "the capture's steady blank cursor pixels follow OSC 10 after OSC 21 cursor="
        );
    }

    /// The capture-only compositor must include both pane snapshots without
    /// borrowing any present semantics: no damage consume and no repaint stamp.
    #[test]
    fn split_capture_grid_composes_sparse_panes_without_present_side_effects() {
        let (mut app, wid, right) = split_fixture();
        let rects = app.active_tree(wid).unwrap().compute_layout(24, 80);
        let left_rect = rects.iter().find(|rect| rect.session == 0).unwrap();
        let right_rect = rects.iter().find(|rect| rect.session == right).unwrap();
        let left_term = app.pool.get(0).unwrap().term.clone();
        let left_epoch = term_lock(&left_term).damage_epoch();
        let predictor_now = Instant::now();
        let predictor_deadline = {
            let window = app.windows.get_mut(&wid).unwrap();
            window
                .predictor
                .set_mode(crate::predict::PredictMode::Always);
            assert!(
                window
                    .predictor
                    .predict_char('?', (0, 0), window.cols, predictor_now)
            );
            window.predictor.next_deadline()
        };

        assert!(app.windows[&wid].last_present.is_none());
        let capture = app
            .prepare_terminal_capture_grid(wid)
            .expect("terminal split capture");
        assert!(capture.composed);
        assert_eq!(capture.leaves.len(), 2);
        assert_eq!(
            capture
                .leaves
                .iter()
                .filter(|leaf| leaf.focused)
                .map(|leaf| leaf.session)
                .collect::<Vec<_>>(),
            vec![right],
            "capture inventory identifies exactly the focused pane"
        );
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.glow_scratch = vec![aterm_render::GlowQuad {
                row: right_rect.row_off,
                x: 0,
                y: 0,
                w: u16::MAX,
                h: u16::MAX,
                color: 0x0012_3456,
            }];
            window.trail_scratch = vec![
                aterm_render::TrailCell {
                    row: usize::from(right_rect.row_off),
                    col: usize::from(right_rect.col_off),
                    alpha: 220,
                },
                aterm_render::TrailCell {
                    row: usize::from(left_rect.row_off),
                    col: usize::from(left_rect.col_off),
                    alpha: 1,
                },
            ];
            window.composed_cursor_effect_valid = true;
            window.composed_cursor_effect_session = Some(right);
            window.composed_cursor_fill = Some(0x00AB_CDEF);
            window.composed_cursor_trail_color = 0x00DE_ADBE;
        }
        let retained_glow = app.windows[&wid].glow_scratch.clone();
        let retained_trail = app.windows[&wid].trail_scratch.clone();
        assert!(
            app.splice_focused_composed_cursor_effects(
                wid,
                crate::app_render::ComposedCursorFxClock::Retain {
                    observed_at: predictor_now,
                },
            ),
            "styled capture projects the retained composed effect tick"
        );

        let ws = &app.windows[&wid];
        let row = &ws.input_scratch.cells[0];
        assert_eq!(row[usize::from(left_rect.col_off)].ch, 'L');
        assert_eq!(row[usize::from(right_rect.col_off)].ch, 'R');
        let divider = usize::from(left_rect.col_off + left_rect.cols);
        assert_eq!(
            row[divider],
            crate::app_render::active_pane_edge_cell(
                app.theme,
                crate::app_render::ActivePaneEdge::Left
            ),
            "the one-cell layout gap is the only cell no pane owns, and this one \
             borders the FOCUSED (right) pane, so it carries that pane's left-edge \
             mark — the seam colour as its ground with the ink on the side facing \
             the pane"
        );
        assert_eq!(
            row[usize::from(left_rect.col_off + left_rect.cols - 1)].bg,
            [0x10, 0x20, 0x30],
            "left sparse tail uses the left terminal's implicit OSC-11 blank"
        );
        assert_eq!(
            row[usize::from(right_rect.col_off + right_rect.cols - 1)].bg,
            [0x40, 0x50, 0x60],
            "right sparse tail uses the right terminal's implicit OSC-11 blank"
        );
        assert_eq!(ws.input_scratch.cursor_row, 0);
        assert!(
            (usize::from(right_rect.col_off)..usize::from(right_rect.col_off + right_rect.cols))
                .contains(&ws.input_scratch.cursor_col),
            "the only visible cursor is offset into the focused pane"
        );
        // Line geometry is a run-based ESCAPE HATCH here: a pane records a run
        // only for a non-single DEC size (see `blit_leaves_ordinary_splits_uniform`),
        // so this all-ordinary split must stay uniform and every pane column must
        // still resolve to a single-width run — the same clip both renderers read.
        assert!(
            ws.input_scratch.line_size_spans[0].is_empty(),
            "an all-single-width split records no DEC runs"
        );
        assert!(
            aterm_render::row_is_uniform(&ws.input_scratch, 0),
            "the capture composite must present row 0 to the renderers as uniform"
        );
        let uniform_run = (
            aterm_core::grid::LineSize::SingleWidth,
            0,
            ws.input_scratch.cols,
        );
        for col in [
            usize::from(left_rect.col_off),
            usize::from(left_rect.col_off + left_rect.cols - 1),
            usize::from(right_rect.col_off),
            usize::from(right_rect.col_off + right_rect.cols - 1),
        ] {
            assert_eq!(
                ws.input_scratch.line_size_run_at(0, col),
                uniform_run,
                "ordinary pane column {col} resolves to the uniform single-width run"
            );
        }
        assert_eq!(
            ws.input_scratch.default_bg_spans[0].len(),
            2,
            "both panes retain independent live default-background provenance"
        );
        assert_eq!(
            ws.input_scratch
                .default_bg_at(0, usize::from(left_rect.col_off + left_rect.cols - 1),),
            0x0010_2030,
        );
        assert_eq!(
            ws.input_scratch
                .default_bg_at(0, usize::from(right_rect.col_off + right_rect.cols - 1),),
            0x0040_5060,
        );
        let (cell_w, cell_h) = app.win_cell_size(wid);
        let (origin_x, origin_y, _, _, effect_head) = app.effects_origin_win(
            wid,
            usize::from(app.windows[&wid].rows),
            usize::from(app.windows[&wid].cols),
            cell_h,
        );
        let expected_x0 = u32::from(origin_x) + u32::from(right_rect.col_off) * cell_w as u32;
        let mut expected_y0 = u32::from(origin_y) + u32::from(right_rect.row_off) * cell_h as u32;
        if right_rect.row_off == 0 {
            expected_y0 = expected_y0.saturating_sub(u32::from(effect_head));
        }
        let expected_x1 =
            u32::from(origin_x) + u32::from(right_rect.col_off + right_rect.cols) * cell_w as u32;
        let expected_y1 =
            u32::from(origin_y) + u32::from(right_rect.row_off + right_rect.rows) * cell_h as u32;
        assert_eq!(
            ws.input_scratch.cursor_glow_add.len(),
            1,
            "styled split capture retains a non-vacuous focused-pane light"
        );
        let glow = ws.input_scratch.cursor_glow_add[0];
        assert!(
            u32::from(glow.x) >= expected_x0
                && u32::from(glow.y) >= expected_y0
                && u32::from(glow.x) + u32::from(glow.w) <= expected_x1
                && u32::from(glow.y) + u32::from(glow.h) <= expected_y1,
            "window-absolute light is clipped exactly to the focused pane"
        );
        assert_eq!(
            ws.input_scratch.cursor_trail,
            vec![aterm_render::TrailCell {
                row: usize::from(right_rect.row_off),
                col: usize::from(right_rect.col_off),
                alpha: 220,
            }],
            "cell effects reject the sibling pane"
        );
        assert_eq!(ws.input_scratch.cursor_trail_color, 0x00DE_ADBE);
        assert_eq!(ws.input_scratch.cursor_fill_override, Some(0x00AB_CDEF));
        assert!(
            ws.last_present.is_none(),
            "capture is not an application-present decision"
        );
        let styled_cells = ws.input_scratch.cells.clone();
        let window = app.windows.get_mut(&wid).unwrap();
        window.input_scratch.clear_overlays();
        assert!(
            window.input_scratch.cursor_glow_add.is_empty()
                && window.input_scratch.cursor_trail.is_empty()
                && window.input_scratch.cursor_fill_override.is_none(),
            "clean split capture is a non-vacuous negative control"
        );
        assert_eq!(
            window.input_scratch.cells, styled_cells,
            "clean strips effects without changing the composed terminal grid"
        );
        assert_eq!(window.glow_scratch, retained_glow);
        assert_eq!(window.trail_scratch, retained_trail);
        assert_eq!(
            window.predictor.next_deadline(),
            predictor_deadline,
            "retained/windowed capture advances neither effect output nor predictor state"
        );
        assert!(!window.predictor.idle());

        // A still-outstanding damage session remains latched: another write does
        // not advance the epoch unless capture incorrectly called take_damage().
        term_lock(&left_term).process(b"!");
        assert_eq!(
            term_lock(&left_term).damage_epoch(),
            left_epoch,
            "capture does not consume terminal damage"
        );
    }

    #[test]
    fn focused_split_selection_is_identical_in_live_and_capture_composites() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let (mut app, wid, right) = split_fixture();
        let (rows, cols) = {
            let window = &app.windows[&wid];
            (usize::from(window.rows), usize::from(window.cols))
        };
        let rects = app
            .active_tree(wid)
            .expect("split tree")
            .compute_layout(rows as u16, cols as u16);
        let left_rect = rects.iter().find(|rect| rect.session == 0).unwrap();
        let right_rect = rects.iter().find(|rect| rect.session == right).unwrap();
        let right_term = app.pool.get(right).expect("right terminal").term.clone();
        {
            let mut terminal = term_lock(&right_term);
            terminal.process(b"\x1b]17;rgb:12/34/56\x07\x1b]19;rgb:fe/cd/32\x07");
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(2, 2, SelectionSide::Right);
            selection.complete_selection();
        }

        let assert_projection = |input: &aterm_render::RenderInput, lane: &str| {
            let row0 = usize::from(right_rect.row_off);
            let col0 = usize::from(right_rect.col_off);
            let row_end = row0 + usize::from(right_rect.rows);
            let col_end = col0 + usize::from(right_rect.cols);
            assert_eq!(
                input.selection_clip,
                Some(aterm_render::SelectionClip::new(
                    row0, row_end, col0, col_end,
                )),
                "{lane}: the renderer clip is the focused pane rectangle"
            );
            assert_eq!(input.selection_bg, 0x0012_3456, "{lane}: OSC 17");
            assert_eq!(input.selection_fg, 0x00fe_cd32, "{lane}: OSC 19");
            assert!(input.selection_contains_cell(row0, col0 + 1, false, false));
            assert!(
                input.selection_contains_cell(row0 + 1, col_end - 1, false, false),
                "{lane}: the linear selection's middle row reaches the pane edge"
            );
            assert!(input.selection_contains_cell(row0 + 2, col0 + 2, false, false));
            assert!(
                !input.selection_contains_cell(row0 + 2, col0 + 3, false, false),
                "{lane}: the final endpoint remains exact"
            );
            let divider = usize::from(left_rect.col_off + left_rect.cols);
            assert!(
                !input.selection_contains_cell(row0 + 1, divider, false, false),
                "{lane}: the divider never receives selection tint"
            );
            assert!(
                !input.selection_contains_cell(
                    row0 + 1,
                    usize::from(left_rect.col_off),
                    false,
                    false,
                ),
                "{lane}: the sibling pane never receives selection tint"
            );
        };

        assert!(
            app.redraw_compose(
                wid,
                rows,
                cols,
                false,
                false,
                None,
                0,
                std::time::Instant::now(),
            )
            .is_some(),
            "the focused selection change presents"
        );
        assert_projection(&app.windows[&wid].input_scratch, "live");

        let capture = app
            .prepare_terminal_capture_grid(wid)
            .expect("split capture");
        assert!(capture.composed);
        assert_projection(&app.windows[&wid].input_scratch, "capture");
    }

    /// SELECTION CUSTODY — the deferred Phase-2 item, both halves at once.
    ///
    /// An UNFOCUSED split pane's selection (1) paints in the composed frame and
    /// (2) is what ⌘-C resolves to when the focused pane holds none. The halves
    /// ship together on purpose: paint without the copy resolution leaves a
    /// highlight ⌘-C ignores, and the copy resolution without the paint copies
    /// text the user cannot see highlighted — the hazard §3 row 26 exists to
    /// close. Focus keeps absolute priority, so a focused pane with a selection
    /// behaves exactly as it always did.
    #[test]
    fn an_unfocused_pane_selection_paints_and_is_what_a_copy_resolves() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let (mut app, wid, right) = split_fixture();
        let (rows, cols) = {
            let window = &app.windows[&wid];
            (usize::from(window.rows), usize::from(window.cols))
        };
        let rects = app
            .active_tree(wid)
            .expect("split tree")
            .compute_layout(rows as u16, cols as u16);
        let left_rect = *rects.iter().find(|rect| rect.session == 0).unwrap();
        assert_eq!(
            app.active_tree(wid).unwrap().focus(),
            right,
            "the split fixture focuses the NEW pane, so session 0 is unfocused"
        );

        // Select "LEFT" in the UNFOCUSED pane.
        let left_term = app.pool.get(0).expect("left pane").term.clone();
        {
            let mut terminal = term_lock(&left_term);
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 3, SelectionSide::Right);
            selection.complete_selection();
        }

        assert!(
            app.redraw_compose(
                wid,
                rows,
                cols,
                false,
                false,
                None,
                0,
                std::time::Instant::now(),
            )
            .is_some(),
            "an unfocused pane's new selection must reach the present early-out"
        );
        let input = &app.windows[&wid].input_scratch;
        assert!(
            !input.selection.has_selection(),
            "the FOCUSED pane holds no selection, so the scalar anchor is empty"
        );
        assert_eq!(
            input.selections.len(),
            1,
            "…and the unfocused pane's highlight rides the per-pane list"
        );
        assert!(input.selections[0].inactive);
        let row0 = usize::from(left_rect.row_off);
        let col0 = usize::from(left_rect.col_off);
        assert!(
            input.selection_contains_cell(row0, col0, false, false),
            "the unfocused pane's selection PAINTS (this is the whole item)"
        );
        let divider = usize::from(left_rect.col_off + left_rect.cols);
        assert!(
            !input.selection_contains_cell(row0, divider, false, false),
            "and is still confined to its own pane box"
        );

        // The copy half. Asserted through the resolver rather than
        // `copy_selection_in`, because `pbcopy` writes the developer's real
        // pasteboard (see `a_copy_resolves_the_routed_window_not_the_frontmost_one`).
        assert_eq!(
            app.window_selection_text(wid).as_deref(),
            Some("LEFT"),
            "⌘-C resolves the pane whose highlight is on screen"
        );

        // Focus keeps absolute priority once the focused pane has its own.
        let right_term = app.pool.get(right).expect("right pane").term.clone();
        {
            let mut terminal = term_lock(&right_term);
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 4, SelectionSide::Right);
            selection.complete_selection();
        }
        assert_eq!(
            app.window_selection_text(wid).as_deref(),
            Some("RIGHT"),
            "the focused pane wins whenever it has a selection at all"
        );

        // …INCLUDING when its selection resolves to NOTHING. Ownership is decided by
        // `has_selection()`, never by whether the text is non-empty:
        // `selection_to_string_capped` returns `None` for an all-whitespace span (every
        // row is trailing-trimmed), so gating on the TEXT would fall through to the
        // sibling and hand back a pane the user never touched. That is a silent
        // wrong-copy, and with `copy_on_select` defaulting on it would fire on every
        // drag across a blank region — sweeping past a short prompt line is enough.
        {
            let mut terminal = term_lock(&right_term);
            let selection = terminal.text_selection_mut();
            // A row the seed never wrote: resolves to "" and therefore to `None`.
            selection.start_selection(3, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(3, 6, SelectionSide::Right);
            selection.complete_selection();
            assert!(
                terminal.selection_to_string().is_none(),
                "precondition: this selection is real but resolves to no text"
            );
            assert!(
                terminal.text_selection().has_selection(),
                "precondition: …and the pane genuinely holds it"
            );
        }
        assert_eq!(
            app.window_selection_text(wid),
            None,
            "a focused pane holding a whitespace-only selection copies NOTHING — it \
             must never fall through to a sibling's text"
        );
    }

    /// Observing an unchanged composite is idempotent at the audit-model layer,
    /// while changed cells inside the same latched damage epoch still produce a
    /// new identity. The same idle-stability law applies to zoom, whose one
    /// visible leaf is still a composed split view.
    #[test]
    fn unchanged_split_and_zoom_captures_keep_stable_model_identity() {
        let (mut app, wid, _) = split_fixture();
        let first = app.prepare_terminal_capture_grid(wid).unwrap();
        let first_fp = App::terminal_capture_fingerprint(&first, &app.windows[&wid].input_scratch);
        let second = app.prepare_terminal_capture_grid(wid).unwrap();
        assert_eq!(
            App::terminal_capture_fingerprint(&second, &app.windows[&wid].input_scratch),
            first_fp
        );
        // Capture intentionally did not consume damage. A later write can
        // therefore retain the same engine epoch; exact model hashing must
        // still see the newly extracted cell.
        let left = app.pool.get(0).unwrap().term.clone();
        crate::term_lock(&left).process(b"!");
        let changed = app.prepare_terminal_capture_grid(wid).unwrap();
        assert_ne!(
            App::terminal_capture_fingerprint(&changed, &app.windows[&wid].input_scratch),
            first_fp
        );

        assert!(
            app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .active_mut()
                .unwrap()
                .toggle_zoom()
        );
        let zoom_first = app.prepare_terminal_capture_grid(wid).unwrap();
        assert!(zoom_first.composed);
        assert_eq!(zoom_first.leaves.len(), 1, "zoom exposes one split leaf");
        let zoom_fp =
            App::terminal_capture_fingerprint(&zoom_first, &app.windows[&wid].input_scratch);
        let zoom_second = app.prepare_terminal_capture_grid(wid).unwrap();
        assert_eq!(
            App::terminal_capture_fingerprint(&zoom_second, &app.windows[&wid].input_scratch),
            zoom_fp
        );
    }

    /// Drive the real control `image` path with chrome enabled. The encoded frame
    /// is a composite, metadata enumerates both leaves, and the retained scratch
    /// has exactly one strip splice (never cumulative/doubled).
    #[test]
    fn image_capture_reports_split_composite_and_splices_chrome_once() {
        let (mut app, wid, right) = split_fixture();
        // The single-splice law and the one-companion law below are both about
        // what the composed route DRAWS, so the trail master is asked for rather
        // than inherited: its absent-key default is platform-split
        // (`app_config::DEFAULT_DECORATIVE_EFFECTS`) and this coverage must hold
        // on Windows too.
        app.config.cursor_trail = Some(true);
        app.recompute_sparkle();
        // Cursor companions share the sprite atlas with Sparkle Words, but do
        // not belong to that feature. Exercise the actual composed-image route
        // with the word scanner structurally absent.
        app.sparkle = None;
        app.windows.get_mut(&wid).expect("window").focused = true;
        app.tab_strip_rows = 1;
        let base_rows = usize::from(app.windows[&wid].rows);
        let cols = usize::from(app.windows[&wid].cols);

        let dir =
            std::env::temp_dir().join(format!("aterm-terminal-split-image-{}", std::process::id()));
        control_auth::ensure_private_dir(&dir).unwrap();
        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            target: confined(&dir, "split.png"),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(&metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        let (_, _, png) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("image worker reply")
            .expect("split image succeeds")
            .value;
        assert!(png.is_some(), "real image path encoded the composed frame");

        let metadata = metadata.get().expect("image metadata");
        assert_eq!(metadata.frame_kind, "composite");
        assert_eq!(
            metadata.view, None,
            "a composite makes no singular-view claim"
        );
        assert_eq!(metadata.leaves.len(), 2);
        assert_eq!(
            metadata
                .leaves
                .iter()
                .map(|leaf| (leaf.session.unwrap(), leaf.focused))
                .collect::<Vec<_>>(),
            vec![(0, false), (right, true)]
        );
        assert!(
            metadata
                .leaves
                .iter()
                .all(|leaf| leaf.snapshot_seq.is_some()),
            "each visible terminal binds its own engine snapshot"
        );

        let ws = &app.windows[&wid];
        assert_eq!(
            ws.input_scratch.rows,
            base_rows + 1,
            "one tab-strip row, exactly once"
        );
        assert_eq!(ws.input_scratch.cells.len(), base_rows + 1);
        let text = terminal_capture_text(&ws.input_scratch, 1, cols);
        assert!(text.lines().next().unwrap().contains("LEFT"));
        assert!(text.lines().next().unwrap().contains("RIGHT"));
        assert!(
            ws.cursor_pet.is_active(),
            "the production composed-image route advances the default resident pet"
        );
        assert_eq!(
            ws.input_scratch.free_sprites.len(),
            1,
            "the split capture carries exactly one resident companion, never zero or a pet+kitty pair"
        );
        assert!(
            ws.input_scratch.free_atlas.is_some(),
            "the retained split companion carries its atlas"
        );
        assert_eq!(
            text.lines().count(),
            base_rows,
            "SIGUSR1 text projection excludes chrome but retains the full composite grid"
        );
        assert!(
            ws.last_present.is_none(),
            "image capture does not stamp application-present"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod capture_overlay_reset_tests {
    use super::App;
    use crate::WindowId;
    use std::sync::Arc;
    use std::time::Instant;

    /// `cell_frame_into` preserves host overlays. Disabling sparkle words between
    /// captures must therefore clear the prior capture before the feature-off
    /// early return, including every Arc-backed sprite channel.
    #[test]
    fn disabled_word_decorations_cannot_leak_prior_capture_layers() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Isolate word-owned residue from the independent cursor companion.
        // Other capture tests prove the shipped pet remains present when only
        // Sparkle Words is disabled; this stale-layer test deliberately turns
        // the cursor-trail owner off so any surviving free sprite is a leak.
        app.config.cursor_trail = Some(false);
        app.prepare_terminal_capture_grid(wid).unwrap();
        app.sparkle_dirty = false;
        app.sparkle = None;
        let atlas = Arc::new(aterm_render::SceneAtlas {
            width: 1,
            height: 1,
            rgba: vec![255, 255, 255, 255],
            version: 1,
        });
        {
            let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
            input.word_decorations.push(aterm_render::WordDecoration {
                row: 0,
                col: 0,
                dx: 0,
                dy: 0,
                glyph: aterm_render::DecoGlyph::Star4,
                blend: aterm_render::DecoBlend::Add,
                color: 0x00FF_FFFF,
                alpha: 255,
            });
            input.ink.push(aterm_render::InkCell {
                row: 0,
                col: 0,
                color: [1, 2, 3],
            });
            input.cat_quads.push(aterm_render::SpriteQuad::default());
            input.cat_atlas = Some(Arc::clone(&atlas));
            input
                .free_sprites
                .push(aterm_core::render::FreeSprite::default());
            input.free_atlas = Some(atlas);
            input.nova_add.push(aterm_render::GlowQuad::default());
        }

        app.splice_word_decorations(wid, Instant::now());

        let input = &app.windows[&wid].input_scratch;
        assert!(input.word_decorations.is_empty());
        assert!(input.ink.is_empty());
        assert!(input.cat_quads.is_empty() && input.cat_atlas.is_none());
        assert!(input.free_sprites.is_empty() && input.free_atlas.is_none());
        assert!(input.nova_add.is_empty());
    }

    /// The decoration helper deliberately takes a second coherent terminal
    /// snapshot to close the initial-capture/PTY race. Live OSC 11/12 state must
    /// come from that same lock too; otherwise cells and sequence describe
    /// snapshot B while padding and cursor colour remain torn from snapshot A.
    #[test]
    fn decoration_refill_restamps_live_default_and_cursor_colors() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.prepare_terminal_capture_grid(wid).unwrap();
        {
            let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
            input.default_bg = 0x00AA_BBCC;
            input.cursor_color = 0x00CC_BBAA;
        }
        let terminal = app.front_terminal(wid).unwrap().term.clone();
        crate::term_lock(&terminal).process(b"\x1b]11;#123456\x07\x1b]12;#ABCDEF\x07");

        app.splice_word_decorations(wid, Instant::now());

        let input = &app.windows[&wid].input_scratch;
        assert_eq!(input.default_bg, 0x0012_3456);
        assert_eq!(input.cursor_color, 0x00AB_CDEF);

        crate::term_lock(&terminal).process(b"\x1b]21;cursor=\x07\x1b]10;#FEDCBA\x07");
        app.splice_word_decorations(wid, Instant::now());
        let input = &app.windows[&wid].input_scratch;
        assert_eq!(
            input.cursor_color, 0x00FE_DCBA,
            "the introspection refill resolves a dynamic cursor from live OSC 10"
        );
    }
}

#[cfg(test)]
mod encode_worker_tests {
    use super::*;
    use crate::control_auth::ensure_private_dir;
    use std::time::Duration;

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aterm-{label}-{}-{nonce}", std::process::id()))
    }

    fn confined(dir: &std::path::Path, name: &str) -> control_auth::ConfinedImage {
        control_auth::ConfinedImage::for_test(dir, name)
    }

    /// The encode worker's contract: a guarded result is transferred only AFTER
    /// the confined write, in FIFO submission order, through EXACTLY ONE worker.
    /// The control writer later revalidates it at the socket edge, so `OK <w> <h>`
    /// means the PNG is complete and bursts retain queue order.
    #[test]
    fn encode_worker_replies_fifo_and_only_after_write() {
        let root = unique_dir("encode-worker");
        ensure_private_dir(&root).unwrap();
        let mut app = App::headless_for_test();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut paths = Vec::new();
        // Server-automatic targets have independent per-path leases, so this
        // isolates the encode worker's FIFO contract from the intentionally
        // one-at-a-time caller-explicit namespace contract tested separately.
        // One shared reply channel makes recv order exactly worker reply order.
        for (i, side) in [1usize, 2, 3].into_iter().enumerate() {
            let target = control_auth::confine_automatic_image_path(&root, &format!("shot{i}"))
                .expect("confine automatic image target");
            paths.push(target.display_path());
            app.submit_encode_job(EncodeJob::Image {
                frame: Frame {
                    width: side,
                    height: side,
                    pixels: vec![0u32; side * side],
                },
                target,
                want_bytes: false,
                cancel: crate::control::CaptureCancellation::new(),
                reply: tx.clone(),
            });
        }
        for (i, side) in [1u32, 2, 3].into_iter().enumerate() {
            let mut retained = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("worker reply")
                .expect("encode succeeds");
            assert_eq!(
                retained.value,
                (side, side, None),
                "reply {i} out of FIFO order"
            );
            retained
                .retention
                .as_mut()
                .expect("file reply carries exact handles")
                .prepare_write()
                .expect("wire-edge identity remains exact");
            // Reply-after-write: at recv time this job's PNG must already be a
            // complete file (the client reads it the moment it sees OK).
            let bytes = std::fs::read(&paths[i]).expect("file written before reply");
            assert!(
                bytes.starts_with(&[0x89, b'P', b'N', b'G']),
                "PNG signature missing"
            );
        }
        // The lazily-spawned worker is retained for reuse across captures.
        assert!(app.encode_tx.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn capture_regression_snapshot_done_is_a_commit_marker() {
        let dir = unique_dir("snapshot-commit");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snapshot.png");
        let text_path = snapshot_sidecar_path(&path, ".txt");
        let done_path = snapshot_sidecar_path(&path, ".done");
        let frame = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x0011_2233],
        };

        // Negative control: a stale marker exists and the text destination is a
        // directory, forcing the second payload write to fail after PNG creation.
        std::fs::write(&done_path, b"stale\n").unwrap();
        std::fs::create_dir(&text_path).unwrap();
        let transaction = begin_snapshot_generation(&path).unwrap();
        let error = write_snapshot_artifacts(&frame, "visible text", &transaction)
            .expect_err("a failed payload write must abort the snapshot");
        assert!(
            error.contains("text write failed"),
            "precise error: {error}"
        );
        assert!(
            !done_path.exists(),
            "the stale/partial attempt must own no completion marker"
        );
        assert!(
            !path.exists(),
            "a payload written before the failure is cleaned up"
        );

        std::fs::remove_dir(&text_path).unwrap();
        let transaction = begin_snapshot_generation(&path).unwrap();
        write_snapshot_artifacts(&frame, "visible text", &transaction)
            .expect("a complete snapshot commits");
        assert!(path.is_file() && text_path.is_file() && done_path.is_file());
        assert_eq!(std::fs::read_to_string(&text_path).unwrap(), "visible text");
        let done = std::fs::read_to_string(&done_path).unwrap();
        assert!(done.starts_with("1x1\ngeneration="));
        assert!(done.contains("png_sha256=") && done.contains("text_sha256="));
        assert!(done.contains(&format!(
            "png_sha256={}",
            sha256_hex(&std::fs::read(&path).unwrap())
        )));
        assert!(done.contains(&format!(
            "text_sha256={}",
            sha256_hex(&std::fs::read(&text_path).unwrap())
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_begin_unlinks_stale_marker_before_directory_sync() {
        let dir = unique_dir("snapshot-clear-sync-order");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snapshot.png");
        let done = sidecar_name(path.file_name().unwrap(), ".done");
        std::fs::write(dir.join(&done), b"stale").unwrap();
        let target = SnapshotTarget {
            path,
            dir: crate::pinned_dir::PinnedDir::open_resolved(&dir).unwrap(),
            png: std::ffi::OsString::from("snapshot.png"),
            text: std::ffi::OsString::from("snapshot.png.txt"),
            done,
        };
        let sync_observed = std::cell::Cell::new(false);

        clear_snapshot_completion_with_sync(&target, |pinned| {
            assert!(
                !target.dir.path().join(&target.done).exists(),
                "the stale marker must be absent before the durability barrier"
            );
            sync_observed.set(true);
            pinned.sync()
        })
        .unwrap();

        assert!(sync_observed.get(), "the containing directory was synced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn image_reply_barrier_keeps_exact_guard_and_fails_closed_on_ancestor_swap() {
        use std::os::unix::fs::symlink;

        let root = unique_dir("image-reply-barrier");
        let images = root.join("images");
        let outside = unique_dir("image-reply-outside");
        ensure_private_dir(&images).unwrap();
        ensure_private_dir(&outside).unwrap();
        let target = crate::control_auth::ConfinedImage::for_test(&images, "shot.png");
        let file = target.write_private(b"png").unwrap();
        let moved = root.join("images-moved");
        let images_for_hook = images.clone();
        let moved_for_hook = moved.clone();
        let outside_for_hook = outside.clone();
        let (reply, result) = std::sync::mpsc::channel();

        send_capture_reply_after_validation(
            target,
            file,
            None,
            &reply,
            (1_u32, 1_u32, None::<Vec<u8>>),
            "image",
            move || {
                std::fs::rename(&images_for_hook, &moved_for_hook).unwrap();
                symlink(&outside_for_hook, &images_for_hook).unwrap();
            },
        );
        assert!(result.recv().unwrap().is_err());
        assert!(
            std::fs::read_dir(&moved).unwrap().next().is_none(),
            "the exact failed artifact is removed through its retained handle"
        );
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());

        let _ = std::fs::remove_file(images);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn image_reply_guard_defers_prune_and_preserves_its_exact_path() {
        let sock = unique_dir("image-reply-before-prune");
        ensure_private_dir(&sock).unwrap();
        let target = control_auth::confine_automatic_image_path(&sock, "image").unwrap();
        let path = target.display_path().to_path_buf();
        let parent = path.parent().unwrap().to_path_buf();
        let lease = control_auth::acquire_capture_name_lease(&target, || false)
            .expect("automatic lease acquisition")
            .expect("lease queued automatic capture");
        let file = target.write_private(b"fresh-png").unwrap();
        for _ in 0..(control_auth::AUTO_IMAGE_KEEP + 5) {
            let newer = control_auth::automatic_capture_name("image");
            std::fs::write(parent.join(newer), b"newer").unwrap();
        }
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        send_capture_reply_after_validation(
            target,
            file,
            Some(lease),
            &reply_tx,
            (1_u32, 1_u32, None::<Vec<u8>>),
            "image",
            || {},
        );
        let mut retained = reply_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker handed the exact guard to the control reply")
            .expect("capture reply remains valid");
        assert_eq!(
            retained.value,
            (1, 1, None),
            "the guard carries the original wire payload"
        );
        assert!(
            std::fs::read_dir(&parent).unwrap().count() > control_auth::AUTO_IMAGE_KEEP,
            "retention has not run while the reply guard is queued"
        );
        retained
            .retention
            .as_mut()
            .expect("file reply carries a guard")
            .prepare_write()
            .expect("wire-edge identity validates");
        drop(retained);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"fresh-png",
            "the post-wire sweep reserves one keep slot for this exact path"
        );

        let _ = std::fs::remove_dir_all(sock);
    }

    #[test]
    fn explicit_capture_namespace_stays_busy_across_distinct_names_until_wire_release() {
        let root = unique_dir("image-namespace-wire-lease");
        ensure_private_dir(&root).unwrap();
        let first_target = crate::control_auth::ConfinedImage::for_test(&root, "shot.png");
        let first_path = first_target.display_path();
        let first_cancel = crate::control::CaptureCancellation::new();
        let first_lease = crate::control_auth::acquire_capture_name_lease(&first_target, || {
            first_cancel.is_cancelled()
        })
        .expect("first lease acquisition")
        .expect("first name lease");
        let first_file = first_target
            .write_private_authorized(b"first", || first_cancel.authorize_commit())
            .unwrap();
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        send_capture_reply_after_validation(
            first_target,
            first_file,
            Some(first_lease),
            &first_tx,
            (),
            "image",
            || {},
        );
        let mut first = first_rx.recv().unwrap().unwrap();

        let second_target = crate::control_auth::ConfinedImage::for_test(&root, "other.png");
        let second_path = second_target.display_path();
        let second_cancel = crate::control::CaptureCancellation::new();
        let busy = crate::control_auth::acquire_capture_name_lease(&second_target, || {
            second_cancel.is_cancelled()
        })
        .expect_err("a differently named explicit capture still contends on the namespace");
        assert_eq!(busy.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(
            std::fs::read(&first_path).unwrap(),
            b"first",
            "a busy successor cannot mutate the first guarded artifact"
        );
        assert!(
            !second_path.exists(),
            "the distinct successor cannot publish while the namespace is leased"
        );
        first
            .retention
            .as_mut()
            .unwrap()
            .prepare_write()
            .expect("first identity validates at its wire edge");
        assert_eq!(std::fs::read(&first_path).unwrap(), b"first");
        drop(first);

        let second_lease =
            crate::control_auth::acquire_capture_name_lease(&second_target, || false)
                .expect("retry after release")
                .expect("second name lease");
        let second_file = second_target
            .write_private_authorized(b"second", || true)
            .unwrap();
        let (second_tx, second_rx) = std::sync::mpsc::channel();
        send_capture_reply_after_validation(
            second_target,
            second_file,
            Some(second_lease),
            &second_tx,
            (),
            "image",
            || {},
        );
        let mut second = second_rx.recv().unwrap().unwrap();
        second
            .retention
            .as_mut()
            .unwrap()
            .prepare_write()
            .expect("second identity validates at its wire edge");
        assert_eq!(std::fs::read(&second_path).unwrap(), b"second");
        assert_eq!(
            std::fs::read(&first_path).unwrap(),
            b"first",
            "a distinct successor never overwrites the first result"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn capture_timeout_cancellation_wins_before_final_name_publish() {
        let root = unique_dir("image-cancel-before-publish");
        ensure_private_dir(&root).unwrap();
        let target = crate::control_auth::ConfinedImage::for_test(&root, "shot.png");
        let path = target.display_path();
        let cancel = crate::control::CaptureCancellation::new();
        assert!(cancel.cancel());

        let error = target
            .write_private_authorized(b"never-visible", || cancel.authorize_commit())
            .expect_err("cancelled publication cannot acquire the final name");

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(!path.exists());
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "the fully written private temporary is cleaned when cancellation wins"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn video_publication_bundle_drops_guards_before_unpublished_directory_on_panic() {
        let sock = unique_dir("video-windows-unwind");
        ensure_private_dir(&sock).unwrap();
        let recording = control_auth::confine_video_dir(&sock).unwrap();
        let path = recording.path().to_path_buf();
        let frame = recording
            .write_new_private(std::ffi::OsStr::new("frame-000000.png"), b"png")
            .unwrap();
        let index = recording
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
            )
            .unwrap();

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _publication = VideoPublication {
                frame_files: vec![frame],
                index_file: index,
                published_marker: None,
                dir: recording,
            };
            panic!("injected before publication");
        }));
        assert!(unwind.is_err());
        assert!(
            !path.exists(),
            "file deny-delete handles must release before directory cleanup"
        );

        let retained = control_auth::confine_video_dir(&sock).unwrap();
        let retained_path = retained.path().to_path_buf();
        let frame = retained
            .write_new_private(std::ffi::OsStr::new("frame-000000.png"), b"png")
            .unwrap();
        let index = retained
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
            )
            .unwrap();
        let mut publication = VideoPublication {
            frame_files: vec![frame],
            index_file: index,
            published_marker: None,
            dir: retained,
        };
        publication.prepare().unwrap();
        publication.publish_marker().unwrap();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _publication = publication;
            panic!("injected after publication");
        }));
        assert!(unwind.is_err());
        assert!(
            retained_path.join("index.json").is_file(),
            "publication is irrevocably retained before any observable reply"
        );

        let _ = std::fs::remove_dir_all(sock);
    }

    #[test]
    fn video_reply_guard_owns_publication_until_wire_prepare() {
        let sock = unique_dir("video-reply-before-prune");
        ensure_private_dir(&sock).unwrap();
        let recording = control_auth::confine_video_dir(&sock).unwrap();
        let path = recording.path().to_path_buf();
        let frame = recording
            .write_new_private(std::ffi::OsStr::new("frame-000000.png"), b"png")
            .unwrap();
        let index = recording
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
            )
            .unwrap();
        let mut publication = VideoPublication {
            frame_files: vec![frame],
            index_file: index,
            published_marker: None,
            dir: recording,
        };
        publication.prepare().unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        send_video_reply_with_retention(publication, &reply_tx, "OK video\n".to_string());
        let mut retained = reply_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker handed publication to the control writer");
        assert_eq!(retained.value, "OK video\n");
        assert!(path.join("index.json").is_file());
        for _ in 0..11 {
            let mut later = control_auth::confine_video_dir(&sock).unwrap();
            let later_index = later
                .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
                .unwrap();
            later.publish(&[], &later_index).unwrap();
            let later_marker = later.publish_marker().unwrap();
            later.prune_after_publish();
            drop(later_marker);
            drop(later_index);
            drop(later);
        }
        assert!(
            path.join("index.json").is_file(),
            "later recording sweeps cannot quarantine a queued video OK"
        );
        retained
            .retention
            .as_mut()
            .expect("video OK carries every exact file handle")
            .prepare_write()
            .expect("wire-edge video validation succeeds");
        drop(retained);
        assert!(
            path.join("index.json").is_file(),
            "wire preparation makes publication irrevocable before OK bytes"
        );

        let _ = std::fs::remove_dir_all(sock);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_transaction_rejects_ancestor_replacement_without_leaking_payload() {
        use std::os::unix::fs::symlink;

        let root = unique_dir("snapshot-ancestor-swap");
        let outside = unique_dir("snapshot-ancestor-outside");
        let snapshots = root.join("snapshots");
        ensure_private_dir(&snapshots).unwrap();
        ensure_private_dir(&outside).unwrap();
        let path = snapshots.join("snapshot.png");
        let transaction = begin_snapshot_generation(&path).unwrap();
        let frame = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x0011_2233],
        };
        let moved = root.join("snapshots-moved");
        let snapshots_for_hook = snapshots.clone();
        let outside_for_hook = outside.clone();
        let moved_for_hook = moved.clone();

        let error =
            write_snapshot_artifacts_with_hook(&frame, "private text", &transaction, move || {
                std::fs::rename(&snapshots_for_hook, &moved_for_hook).unwrap();
                symlink(&outside_for_hook, &snapshots_for_hook).unwrap();
            })
            .expect_err("a replaced ancestor cannot commit a completion marker");
        assert!(error.contains("identity changed"), "precise error: {error}");
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "the replacement directory receives no payload or marker"
        );
        assert!(
            std::fs::read_dir(&moved).unwrap().next().is_none(),
            "the retained original directory is cleaned handle-relatively"
        );

        let _ = std::fs::remove_file(&snapshots);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn snapshot_generation_model_proves_and_catches_stale_publish() {
        let model = aterm_spec::derive::snapshot_generation_commit_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn snapshot_generation_fence_rejects_old_worker_after_new_begin() {
        let model = aterm_spec::derive::snapshot_generation_commit_model();
        let mut state = model.init_state();
        assert_eq!(snapshot_commit_plan(1, 1), SnapshotCommitPlan::Publish);
        assert!(model.fire("CommitCurrent", &mut state));
        assert!(model.fire("BeginNew", &mut state));
        assert_eq!(snapshot_commit_plan(1, 2), SnapshotCommitPlan::DiscardStale);
        assert!(model.fire("CommitOld", &mut state));
        assert!(model.check_invariant("CommittedPayloadIsCurrent", &state));

        // Deterministic real interleaving: A owns generation 1, B begins and
        // clears the fixed marker, then A reaches commit. A may have encoded its
        // payload outside the fence, but it can never republish `.done`.
        let dir = unique_dir("snapshot-generation");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snapshot.png");
        let done_path = snapshot_sidecar_path(&path, ".done");
        let frame = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x0011_2233],
        };
        let transaction_a = begin_snapshot_generation(&path).unwrap();
        let transaction_b = begin_snapshot_generation(&path).unwrap();
        assert!(
            !done_path.exists(),
            "B begin invalidates every prior marker"
        );
        let stale = write_snapshot_artifacts(&frame, "A", &transaction_a)
            .expect_err("A cannot commit after B begins");
        assert!(stale.contains("superseded"));
        assert!(
            !done_path.exists(),
            "once B begin returns, A can never republish the fixed marker"
        );
        write_snapshot_artifacts(&frame, "B", &transaction_b)
            .expect("the current generation commits");
        assert!(done_path.is_file());

        assert!(model.fire("SelectCurrent", &mut state));
        assert_eq!(snapshot_commit_plan(2, 2), SnapshotCommitPlan::Publish);
        assert!(model.fire("CommitCurrent", &mut state));
        let mut bad = state.clone();
        bad.insert("payload", 1);
        assert!(
            !model.check_invariant("CommittedPayloadIsCurrent", &bad),
            "negative control: a current marker certifying stale payload is rejected"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_generation_transaction_cannot_certify_stale_payload() {
        let dir = unique_dir("snapshot-payload-identity");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snapshot.png");
        let text_path = snapshot_sidecar_path(&path, ".txt");
        let done_path = snapshot_sidecar_path(&path, ".done");
        let frame_a = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x00AA_1020],
        };
        let frame_b = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x0010_BB30],
        };
        let transaction_a = begin_snapshot_generation(&path).unwrap();
        let transaction_b = begin_snapshot_generation(&path).unwrap();

        // B owns the fence and has written both B payloads. At the hook, start
        // stale A: it can encode, but must block before touching a shared path.
        // B then publishes `.done` and releases; only afterward can A observe
        // that it is stale and return without deleting/overwriting B.
        let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        write_snapshot_artifacts_with_hook(&frame_b, "B payload", &transaction_b, move || {
            std::thread::spawn(move || {
                attempted_tx.send(()).unwrap();
                let result = write_snapshot_artifacts(&frame_a, "A payload", &transaction_a);
                result_tx.send(result).unwrap();
            });
            attempted_rx.recv().unwrap();
        })
        .expect("B transaction commits");
        let stale = result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stale A finishes after B unlocks")
            .expect_err("A remains stale");
        assert!(stale.contains("superseded"));

        let png = std::fs::read(&path).unwrap();
        let (rgba, width, height) = aterm_render::decode_png_rgba8(&png).unwrap();
        assert_eq!((width, height), (1, 1));
        assert_eq!(rgba, frame_b.rgba_bytes(), "B marker certifies B pixels");
        assert_eq!(std::fs::read_to_string(text_path).unwrap(), "B payload");
        let done = std::fs::read_to_string(done_path).unwrap();
        assert!(done.starts_with("1x1\ngeneration="));
        assert!(done.contains("png_sha256=") && done.contains("text_sha256="));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_regression_platform_neutral_rgba_encoder_is_available() {
        let png = encode_rgba8_png(&[0x11, 0x22, 0x33, 0x80], 1, 1)
            .expect("the video/window encoder is available on every host");
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .expect("PNG header");
        assert_eq!(
            reader.info().srgb,
            Some(png::SrgbRenderingIntent::Perceptual)
        );
        let mut rgba = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut rgba).expect("PNG pixels");
        assert_eq!(&rgba[..info.buffer_size()], &[0x11, 0x22, 0x33, 0x80]);
    }

    #[test]
    fn capture_regression_video_write_failure_has_no_completion_artifact() {
        let root = unique_dir("video-export-failure");
        ensure_private_dir(&root).unwrap();
        let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
        let dir_path = dir.path().to_path_buf();
        // A directory at the first frame's filename makes the confined file open
        // fail on every platform without relying on permissions or symlink support.
        std::fs::create_dir(dir_path.join("frame_0001.png")).unwrap();
        let take = aterm_gpu::video_tap::VideoTake {
            frames: [aterm_gpu::video_tap::CapturedFrame {
                seq: 1,
                t_us: 1,
                w: 1,
                h: 1,
                rgba: vec![0, 0, 0, 255],
            }]
            .into(),
            dropped: 0,
            evicted: 0,
            decimated: 0,
            fps_cap: None,
            budget_bytes: 16 << 20,
            requested_ms: 100,
            w: 1,
            h: 1,
            device_px: (1, 1),
            half_res: false,
            format: "rgba8",
            resized_early_stop: false,
        };
        let (reply, rx) = std::sync::mpsc::channel();
        let cancel = crate::VideoCancellation::new();
        let export = std::sync::Arc::new(crate::VideoExportState::default());
        let permit = export
            .try_begin(cancel.clone())
            .expect("fresh export permit");
        run_encode_job(EncodeJob::VideoDump {
            take,
            mode: crate::VideoMode::SwapchainTap,
            keys_enabled: false,
            inputs: Vec::new(),
            unlogged_inputs: 0,
            unlogged_other_window: 0,
            started_us: 0,
            dir,
            reply,
            cancel,
            permit,
        });
        let error = rx.recv().expect("failure reply");
        assert!(
            error.starts_with("ERR video: export failed: frame 1 write failed"),
            "precise protocol error: {error}"
        );
        assert!(
            !dir_path.join("index.json").exists(),
            "a failed export never publishes its completion artifact"
        );
        assert!(
            !dir_path.exists(),
            "the server-owned partial export is removed rather than leaked forever"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_video_export_aborts_dir_and_releases_busy_permit() {
        let root = unique_dir("video-export-cancel");
        ensure_private_dir(&root).unwrap();
        let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
        let dir_path = dir.path().to_path_buf();
        let take = aterm_gpu::video_tap::VideoTake {
            frames: [aterm_gpu::video_tap::CapturedFrame {
                seq: 1,
                t_us: 1,
                w: 1,
                h: 1,
                rgba: vec![0, 0, 0, 255],
            }]
            .into(),
            dropped: 0,
            evicted: 0,
            decimated: 0,
            fps_cap: None,
            budget_bytes: 16 << 20,
            requested_ms: 100,
            w: 1,
            h: 1,
            device_px: (1, 1),
            half_res: false,
            format: "rgba8",
            resized_early_stop: false,
        };
        let cancel = crate::VideoCancellation::new();
        let export = std::sync::Arc::new(crate::VideoExportState::default());
        let permit = export
            .try_begin(cancel.clone())
            .expect("fresh export permit");
        let _ = cancel.cancel();
        let (reply, rx) = std::sync::mpsc::channel();
        run_encode_job(EncodeJob::VideoDump {
            take,
            mode: crate::VideoMode::SwapchainTap,
            keys_enabled: false,
            inputs: Vec::new(),
            unlogged_inputs: 0,
            unlogged_other_window: 0,
            started_us: 0,
            dir,
            reply,
            cancel,
            permit,
        });
        assert!(
            rx.recv()
                .expect("cancel reply")
                .contains("request cancelled before export")
        );
        assert!(
            !dir_path.exists(),
            "cancellation removes the unpublished partial directory"
        );
        assert!(
            !export.is_busy(),
            "worker acknowledgement drops the sole export permit"
        );
        let _ = std::fs::remove_dir_all(root);
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
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        let (width, height, png) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("terminal image reply")
            .expect("terminal image succeeds")
            .value;
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
        app.config.show_build_badge = Some(true);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_native_image(NativeImageRequest {
            front: wid,
            clean: false,
            presented: None,
            presented_metadata: None,
            exact_frame: None,
            target: confined(&dir, "binding.png"),
            want_bytes: true,
            want_metadata: true,
            cancel: crate::control::CaptureCancellation::new(),
            frame_metadata: &metadata,
            reply: tx,
        });
        let (width, height, png) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("native image reply")
            .expect("native image succeeds")
            .value;
        assert!(width > 0 && height > 0);
        assert!(png.is_some());

        let metadata = metadata.get().expect("native frame metadata is bound");
        let route_card = app.windows[&wid]
            .route_card
            .as_ref()
            .expect("enabled build badge creates a composed route card");
        let native_card = app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("native card remains retained");
        assert_ne!(
            route_card.fp, native_card.fp,
            "negative control: route and base native cards have distinct identities"
        );
        assert_eq!(
            metadata.raster_model_fingerprint, route_card.fp,
            "metadata names the route card actually passed to the renderer"
        );
        assert_eq!(
            metadata.raster_geometry, route_card.geom,
            "metadata geometry comes from the rendered route card"
        );
        assert_eq!(
            metadata.phase, "staged",
            "headless capture has no OS application-present target"
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
            app.render_native_image(NativeImageRequest {
                front: wid,
                clean: false,
                presented: None,
                presented_metadata: None,
                exact_frame: None,
                target: confined(&dir, name),
                want_bytes: true,
                want_metadata: true,
                cancel: crate::control::CaptureCancellation::new(),
                frame_metadata: &metadata,
                reply: tx,
            });
            let image = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("paint identity image reply")
                .expect("paint identity image succeeds")
                .value;
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
    fn snapshot_text_tracks_terminal_native_and_mixed_visible_routes() {
        let wid = WindowId(0);

        let mut terminal_app = App::headless_for_test();
        terminal_app.tab_strip_rows = 0;
        let terminal = terminal_app.front_terminal(wid).unwrap().term.clone();
        crate::term_lock(&terminal).process(b"TERMINAL_SNAPSHOT_MARKER");
        terminal_app
            .prepare_terminal_capture_grid(wid)
            .expect("terminal route stages");
        let terminal_route = terminal_app.active_visible_content_route(wid).unwrap();
        let terminal_input = terminal_app.windows[&wid].input_scratch.clone();
        let terminal_text = terminal_app.snapshot_visible_text(
            wid,
            terminal_route,
            &terminal_input,
            0,
            usize::from(terminal_app.windows[&wid].cols),
        );
        assert_eq!(
            terminal_text,
            terminal_capture_text(
                &terminal_input,
                0,
                usize::from(terminal_app.windows[&wid].cols)
            ),
            "terminal-only SIGUSR1 text remains byte-for-byte compatible"
        );
        assert!(terminal_text.contains("TERMINAL_SNAPSHOT_MARKER"));

        let mut native_app = App::headless_for_test();
        assert!(native_app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(native_app.prepare_native_input_scratch(wid));
        let (_, native_view) = native_app.active_native_view(wid).unwrap();
        let native_route = native_app.active_visible_content_route(wid).unwrap();
        let native_input = native_app.windows[&wid].input_scratch.clone();
        let native_text = native_app.snapshot_visible_text(
            wid,
            native_route,
            &native_input,
            usize::from(native_app.tab_strip_rows),
            usize::from(native_app.windows[&wid].cols),
        );
        assert!(native_text.contains(&format!("[native view={} focused=true]", native_view.get())));
        assert!(
            native_text.contains("text key="),
            "native SIGUSR1 text projects the retained semantic tree: {native_text}"
        );

        let (session, terminal_view) = native_app
            .split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let mixed_terminal = native_app.pool.get(session).unwrap().term.clone();
        crate::term_lock(&mixed_terminal).process(b"MIXED_SNAPSHOT_MARKER");
        assert!(
            native_app
                .prepare_heterogeneous_input_scratch(wid)
                .is_some()
        );
        let mixed_route = native_app.active_visible_content_route(wid).unwrap();
        let mixed_input = native_app.windows[&wid].input_scratch.clone();
        let mixed_text = native_app.snapshot_visible_text(
            wid,
            mixed_route,
            &mixed_input,
            usize::from(native_app.tab_strip_rows),
            usize::from(native_app.windows[&wid].cols),
        );
        assert!(mixed_text.contains("MIXED_SNAPSHOT_MARKER"));
        assert!(mixed_text.contains(&format!("[native view={}", native_view.get())));
        assert!(mixed_text.contains("text key="));
        assert_ne!(
            native_app.windows[&wid].tab_set.active().unwrap().focus,
            native_view,
            "negative control: native semantics are retained even while terminal view {} owns focus",
            terminal_view.get()
        );

        let base_card_fp = native_app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("mixed native layer is rasterized")
            .fp;
        native_app.palette_enter();
        let palette_lines = native_app.windows[&wid]
            .palette()
            .expect("palette is visibly open")
            .controls_lines();
        assert!(
            native_app
                .prepare_heterogeneous_input_scratch(wid)
                .is_some()
        );
        let palette_card_fp = native_app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("visible palette is lowered into the mixed PNG route card")
            .fp;
        assert_ne!(
            base_card_fp, palette_card_fp,
            "negative control: opening the palette changes the raster captured by the PNG route"
        );
        let palette_route = native_app.active_visible_content_route(wid).unwrap();
        let palette_input = native_app.windows[&wid].input_scratch.clone();
        let palette_text = native_app.snapshot_visible_text(
            wid,
            palette_route,
            &palette_input,
            usize::from(native_app.tab_strip_rows),
            usize::from(native_app.windows[&wid].cols),
        );
        assert!(palette_text.contains("MIXED_SNAPSHOT_MARKER"));
        assert!(palette_text.contains(&format!("[native view={}", native_view.get())));
        let expected_palette_suffix = format!("{}\n", palette_lines.join("\n"));
        assert!(
            palette_text.ends_with(&expected_palette_suffix),
            "SIGUSR1 text must append the exact serializer for the palette painted in the PNG: {palette_text}"
        );
    }

    /// SELECTION CUSTODY: a mixed composite paints EVERY terminal leaf's
    /// selection, and the focused terminal leaf additionally stamps the scalar
    /// anchor. Focusing the NATIVE pane leaves the terminal's highlight on
    /// screen — it used to vanish, while `⌘-C` still copied it — with the scalar
    /// fields neutral, because no terminal leaf is focused.
    #[test]
    fn mixed_composite_paints_every_terminal_selection_and_anchors_the_focused_one() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let retained_at = Instant::now();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, native_view) = app.active_native_view(wid).expect("Settings view");
        let (terminal_session, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let terminal = app
            .pool
            .get(terminal_session)
            .expect("terminal session")
            .term
            .clone();
        {
            let mut terminal = crate::term_lock(&terminal);
            terminal.process(b"\x1b]17;rgb:21/43/65\x07\x1b]19;rgb:fe/dc/ba\x07");
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(2, 2, SelectionSide::Right);
            selection.complete_selection();
        }
        let plan = app.active_visible_leaf_plan(wid).expect("mixed plan");
        let terminal_leaf = plan
            .leaves
            .iter()
            .find(|leaf| leaf.view == terminal_view)
            .expect("terminal leaf");
        let native_leaf = plan
            .leaves
            .iter()
            .find(|leaf| leaf.view == native_view)
            .expect("native leaf");
        let terminal_rect = (
            terminal_leaf.rect.origin.y.round().max(0.0) as usize,
            terminal_leaf.rect.origin.x.round().max(0.0) as usize,
            terminal_leaf.rect.size.height.round().max(1.0) as usize,
            terminal_leaf.rect.size.width.round().max(1.0) as usize,
        );
        let native_point = (
            native_leaf.rect.origin.y.round().max(0.0) as usize,
            native_leaf.rect.origin.x.round().max(0.0) as usize,
        );
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.glow_scratch = vec![aterm_render::GlowQuad {
                row: u16::try_from(terminal_rect.0).unwrap(),
                x: 0,
                y: 0,
                w: u16::MAX,
                h: u16::MAX,
                color: 0x0042_84C6,
            }];
            window.trail_scratch = vec![
                aterm_render::TrailCell {
                    row: terminal_rect.0,
                    col: terminal_rect.1,
                    alpha: 210,
                },
                aterm_render::TrailCell {
                    row: native_point.0,
                    col: native_point.1,
                    alpha: 1,
                },
            ];
            window.composed_cursor_effect_valid = true;
            window.composed_cursor_effect_session = Some(terminal_session);
            window.composed_cursor_fill = Some(0x0012_3456);
            window.composed_cursor_trail_color = 0x0065_4321;
        }

        assert!(
            app.prepare_heterogeneous_input_scratch_with_cursor_fx(
                wid,
                Some(crate::app_render::ComposedCursorFxClock::Retain {
                    observed_at: retained_at,
                }),
            )
            .is_some()
        );
        let input = &app.windows[&wid].input_scratch;
        let (row, col, pane_rows, pane_cols) = terminal_rect;
        assert_eq!(
            input.selection_clip,
            Some(aterm_render::SelectionClip::new(
                row,
                row + pane_rows,
                col,
                col + pane_cols,
            ))
        );
        assert_eq!(input.selection_bg, 0x0021_4365);
        assert_eq!(input.selection_fg, 0x00fe_dcba);
        assert_eq!(
            input.selections.len(),
            1,
            "the one terminal leaf contributes one entry"
        );
        assert!(
            !input.selections[0].inactive,
            "it is the focused leaf, so its band is the active colour"
        );
        assert!(input.selection_contains_cell(row + 1, col + pane_cols - 1, false, false));
        assert!(
            !input.selection_contains_cell(native_point.0, native_point.1, false, false),
            "the native sibling never receives the terminal selection"
        );
        assert!(
            col > 0 && !input.selection_contains_cell(row + 1, col - 1, false, false),
            "the one-cell divider before the focused terminal remains unselected"
        );
        assert_eq!(input.cursor_glow_add.len(), 1);
        assert_eq!(
            input.cursor_trail,
            vec![aterm_render::TrailCell {
                row: terminal_rect.0,
                col: terminal_rect.1,
                alpha: 210,
            }],
            "a mixed frame projects effects only into its focused terminal leaf"
        );
        assert_eq!(input.cursor_fill_override, Some(0x0012_3456));

        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(native_view);
        app.sync_window(wid);
        assert!(
            app.prepare_heterogeneous_input_scratch_with_cursor_fx(
                wid,
                Some(crate::app_render::ComposedCursorFxClock::Retain {
                    observed_at: retained_at,
                }),
            )
            .is_some()
        );
        let input = &app.windows[&wid].input_scratch;
        assert!(!input.selection.has_selection());
        assert_eq!(input.selection_clip, None);
        assert_eq!(input.selection_bg, aterm_core::render::COLOR_UNSET);
        assert_eq!(input.selection_fg, aterm_core::render::COLOR_UNSET);
        // …but the terminal leaf's highlight is STILL PAINTED. The scalar fields
        // are the FOCUSED leaf's authority and the focused leaf is native, so
        // they stay neutral; the per-pane list carries the live selection.
        assert_eq!(input.selections.len(), 1);
        assert!(
            input.selections[0].inactive,
            "an unfocused pane's band takes the inactive colour"
        );
        assert_eq!(input.selections[0].bg, 0x0021_4365);
        assert_eq!(input.selections[0].fg, 0x00fe_dcba);
        assert!(
            input.selection_contains_cell(row + 1, col + pane_cols - 1, false, false),
            "the unfocused terminal pane keeps painting its selection"
        );
        assert!(
            !input.selection_contains_cell(native_point.0, native_point.1, false, false),
            "and still never reaches the native sibling"
        );
        assert!(
            input.cursor_glow_add.is_empty()
                && input.cursor_trail.is_empty()
                && input.cursor_fill_override.is_none(),
            "native focus clears the terminal-only cursor-effect authority"
        );
        assert!(
            !app.windows[&wid].composed_cursor_effect_valid,
            "a later terminal focus cannot resurrect native-focused stale effects"
        );
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
            app.render_native_image(NativeImageRequest {
                front: wid,
                clean: false,
                presented: None,
                presented_metadata: None,
                exact_frame: None,
                target: confined(&dir, name),
                want_bytes: true,
                want_metadata: true,
                cancel: crate::control::CaptureCancellation::new(),
                frame_metadata: &frame_metadata,
                reply: tx,
            });
            let image = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("mixed capture reply")
                .expect("mixed capture succeeds")
                .value;
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

    #[test]
    fn mixed_plain_capture_strips_effects_from_an_owned_copy_only() {
        let dir =
            std::env::temp_dir().join(format!("aterm-mixed-plain-capture-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let terminal_leaf = app
            .active_visible_leaf_plan(wid)
            .unwrap()
            .leaf(terminal_view)
            .expect("terminal leaf")
            .clone();
        let trail = aterm_render::TrailCell {
            row: usize::from(app.tab_strip_rows)
                + terminal_leaf.rect.origin.y.round().max(0.0) as usize
                + (terminal_leaf.rect.size.height * 0.5).floor().max(0.0) as usize,
            col: terminal_leaf.rect.origin.x.round().max(0.0) as usize
                + (terminal_leaf.rect.size.width * 0.5).floor().max(0.0) as usize,
            alpha: 255,
        };
        let decoration = aterm_render::WordDecoration {
            row: u16::try_from(trail.row).expect("fixture row fits"),
            col: u16::try_from(trail.col).expect("fixture column fits"),
            dx: 0,
            dy: 0,
            glyph: aterm_render::DecoGlyph::Paw,
            blend: aterm_render::DecoBlend::Over,
            color: 0x00FF_00FF,
            alpha: 255,
        };
        {
            let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
            assert!(trail.row < input.rows && trail.col < input.cols);
            input.cursor_trail = vec![trail];
            input.cursor_trail_color = 0x00FF_FFFF;
            input.word_decorations = vec![decoration];
        }
        let presented = PresentedFrameCapture {
            input: app.windows[&wid].input_scratch.clone(),
            invert: false,
            overlay: None,
            serial: 1,
        };
        let capture = |app: &mut App, clean: bool, name: &str, frame: PresentedFrameCapture| {
            let (tx, rx) = std::sync::mpsc::channel();
            let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
            app.render_native_image(NativeImageRequest {
                front: wid,
                clean,
                presented: Some(frame),
                presented_metadata: None,
                exact_frame: None,
                target: confined(&dir, name),
                want_bytes: true,
                want_metadata: false,
                cancel: crate::control::CaptureCancellation::new(),
                frame_metadata: &metadata,
                reply: tx,
            });
            rx.recv_timeout(Duration::from_secs(10))
                .expect("mixed plain reply")
                .expect("mixed plain capture")
                .value
                .2
                .expect("inline PNG bytes")
        };

        let styled = capture(&mut app, false, "styled.png", presented.clone());
        let plain = capture(&mut app, true, "plain.png", presented);
        let png_fingerprint = |bytes: &[u8]| {
            use std::hash::{Hash, Hasher};

            let mut hash = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut hash);
            hash.finish()
        };
        assert_ne!(
            png_fingerprint(&styled),
            png_fingerprint(&plain),
            "the styled mixed frame contains terminal effects that plain removes"
        );
        assert_eq!(
            app.windows[&wid].input_scratch.cursor_trail,
            vec![trail],
            "plain capture never mutates the retained application-present frame"
        );
        assert_eq!(
            app.windows[&wid].input_scratch.word_decorations,
            vec![decoration],
            "plain capture never mutates retained sparkle decorations"
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
        let root = unique_dir("video-dump");
        ensure_private_dir(&root).unwrap();
        let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
        let dir_path = dir.path().to_path_buf();
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
        let cancel = crate::VideoCancellation::new();
        let export = std::sync::Arc::new(crate::VideoExportState::default());
        let permit = export
            .try_begin(cancel.clone())
            .expect("fresh export permit");
        run_encode_job(EncodeJob::VideoDump {
            take,
            mode: crate::VideoMode::SwapchainTap,
            keys_enabled: false,
            inputs: Vec::new(),
            unlogged_inputs: 0,
            unlogged_other_window: 0,
            started_us: 500,
            dir,
            reply: tx,
            cancel,
            permit,
        });
        let reply = rx.recv().expect("dump reply");
        assert!(reply.starts_with("OK "), "reply: {reply}");
        let toks: Vec<&str> = reply.split_whitespace().collect();
        assert_eq!(
            toks.last().copied(),
            Some(dir_path.join("index.json").display().to_string().as_str()),
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
        let index =
            std::fs::read_to_string(dir_path.join("index.json")).expect("index.json written");
        for key in [
            "\"head_truncated\": true",
            "\"evicted_frames\": 3",
            "\"ring_skipped\": 1",
            "\"decimated_frames\": 2",
            "\"requested_ms\": 8000",
            "\"covered_us\": [1000, 2000]",
            "\"fps_cap\": 10",
            "\"budget_mib\": 64",
            // The honesty label: a swapchain recording says exactly what the
            // CPU timestamp observes and what remains outside observation.
            "\"mode\": \"swapchain-tap\"",
            "\"stamp_semantics\": \"CPU time frame.present() returned; submitted to WSI present; compositor visibility and scanout not observed\"",
        ] {
            assert!(index.contains(key), "index.json missing {key}:\n{index}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A real headless `image` capture must advance the same feline overlay
    /// path as application-present. This pins the complete app-owned seam: terminal row
    /// scan, focused phase start, bounded cat bake, free-atlas publication,
    /// and collection observation — under the AMBIENT provenance law (owner
    /// ruling, 2026-08-07): the scanned word still peeks and still collects,
    /// but it never activates or re-dresses the cursor companion, and no
    /// discovery hello starts from a capture.
    #[test]
    fn headless_capture_emits_and_collects_an_eligible_cat() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        // This test isolates AMBIENT word-cat provenance. The resident cursor
        // companion is independently covered above and legitimately uses the
        // over-text free-sprite tier, so disable its owner here rather than
        // confusing it with a forbidden discovery hello.
        app.config.cursor_trail = Some(false);
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
        assert_eq!(app.kitty_log.log().sightings, 1);
        assert!(
            app.kitty_log
                .log()
                .collectibles
                .iter()
                .any(|item| item.count > 0),
            "the same visible sprite must be observed as a collectible"
        );
        // The AMBIENT half of the law: the collection grew, the companion did
        // not — a scanned word is output text, not a keystroke (and since the
        // launch-kitty ruling no discovery of any kind pins the cat).
        assert_eq!(
            app.kitty_log.favourite_look(),
            None,
            "an ambient discovery never becomes the companion"
        );
        assert!(
            !app.windows[&wid].cursor_cat.is_active(),
            "and no discovery hello starts from a capture"
        );

        // A second REQUESTED windowless capture, long after any hold: still no
        // cursor companion over the text — the hello is the TYPED path's
        // presentation alone.
        app.splice_word_decorations(wid, first_capture + Duration::from_secs(30));
        assert!(
            app.windows[&wid]
                .input_scratch
                .free_sprites
                .iter()
                .all(|sprite| sprite.z != aterm_core::render::FreeZ::OverText),
            "an ambient discovery presents no over-text cursor hello"
        );
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "a no-damage capture continues the episode without recollecting it"
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

    /// A CAPTURE IS A RENDERER, AND IT MUST HONOUR THE SAME TAB SCOPE.
    ///
    /// `WordDecorations` is per WINDOW and a tab switch retires nothing, so a
    /// capture that does not DECLARE the session it is drawing inherits the
    /// previous tab's scan. The two tabs are two different terminals whose
    /// damage epochs are independent counters that can read equal — and when
    /// they do, a capture that skipped its rescan re-emits the PREVIOUS tab's
    /// word list over the new tab's text. That is why the witness here is a cat
    /// AT A NAMED COLUMN and not "some cat is present": the latter cannot tell
    /// a leaked cat from the tab's own.
    ///
    /// NON-VACUITY: each tab carries a feline word of its own, at a DIFFERENT
    /// column, so both directions of the check have something to find.
    #[test]
    fn a_capture_of_another_tab_carries_only_that_tabs_words() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let t0 = Instant::now();
        app.recompute_sparkle();
        let session_a = app.front_terminal(wid).expect("front terminal").session;
        let write = |app: &App, session: u64, bytes: &[u8]| {
            let term = app.pool.get(session).expect("pane session").term.clone();
            crate::term_lock(&term).process(bytes);
        };
        write(&app, session_a, b"\r\n\r\n\r\nhello kitty friend");
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.pending_deco_birth = Some(t0);
        }
        app.splice_word_decorations(wid, t0);
        let (cell_w, cell_h) = {
            let (w, h) = app.win_cell_size(wid);
            (w as i32, h as i32)
        };
        assert!(
            cell_h > 0 && cell_w > 0,
            "PRECONDITION: the fixture has real cell metrics"
        );
        // A peeking cat's dest rect starts near its word's first cell, so one
        // cell of tolerance is generous.
        let cat_at_col = |app: &App, col: u16| {
            app.windows[&wid]
                .input_scratch
                .free_sprites
                .iter()
                .any(|s| (s.x - i32::from(col) * cell_w).abs() <= cell_w)
        };
        // Where tab A's ambient cat stands: `kitty` begins at column 6 of
        // "hello kitty friend". Pinned as a PRECONDITION so the negative it
        // anchors further down — "tab A's cat is NOT in tab B's image" — cannot
        // pass merely because the column test never matches anything.
        let first_word_col: u16 = 6;
        assert!(
            cat_at_col(&app, first_word_col),
            "PRECONDITION: tab A's own cat really stands at its own word"
        );

        // A SECOND TAB, made front exactly as ⌘T does, with a feline word of
        // its own so its capture cannot be silent for an unrelated reason —
        // planted at a column tab A's word does not occupy.
        let session_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(session_b));
        assert_eq!(
            app.front_terminal(wid).expect("front terminal").session,
            session_b,
            "PRECONDITION: the new tab is the one on glass now"
        );
        let tab_b_col: u16 = 30;
        write(
            &app,
            session_b,
            format!("\x1b[6;{}Hkitty waits", tab_b_col + 1).as_bytes(),
        );
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.pending_deco_birth = Some(t0);
        }
        let at = t0 + Duration::from_millis(16);
        app.splice_word_decorations(wid, at);
        assert!(
            cat_at_col(&app, tab_b_col),
            "tab B's OWN cat must be the cat in this image"
        );
        assert!(
            !cat_at_col(&app, first_word_col),
            "and tab A's word list must not have survived the switch — this \
             capture is of tab B's grid, where that column is blank"
        );

        // Back to tab A: a SCOPE, not a deletion.
        app.switch_tab_in(wid, 0);
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.pending_deco_birth = Some(at);
        }
        assert_eq!(
            app.front_terminal(wid).expect("front terminal").session,
            session_a,
            "PRECONDITION: tab A is front again"
        );
        app.splice_word_decorations(wid, at + Duration::from_millis(16));
        assert!(
            cat_at_col(&app, first_word_col),
            "and tab A's own capture has its own cat back"
        );
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
            // The curse is TYPED, and the engine's typed witness is now the
            // caret's completion position AND a committed key (a passive output
            // line parks the caret one past a curse just as readily). Without
            // this the fixture stages OUTPUT, which correctly never winces.
            ws.word_decos.note_typed_edit(t0, None);
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
            aterm_effects::kitty_cursor::CatReaction::Wince,
            "the silent capture drain must retain the visual wince"
        );
        assert_eq!(
            frame.pose,
            aterm_effects::kitty_cursor::CatPose::STILL,
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
                .any(|sprite| sprite.z == aterm_core::render::FreeZ::UnderText),
            "the first drawable capture still presents the paused collection hello \
             (UnderText since c92dcf56: a text-scale cat tucks behind the line's \
             ink rather than standing on it)"
        );
    }

    #[test]
    fn load_shed_capture_retires_an_unowned_pet_before_its_early_return() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let t0 = Instant::now();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty pet".into());
        app.recompute_sparkle();
        app.sparkle = None;
        app.splice_word_decorations(wid, t0);
        assert!(
            app.windows[&wid].cursor_pet.is_active(),
            "negative control: an owned capture materializes the resident pet"
        );

        app.perf_reduced = true;
        assert!(
            app.load_shed_active(),
            "fixture reaches the early-return arm"
        );
        app.splice_word_decorations(wid, t0 + Duration::from_millis(16));
        assert!(
            app.windows[&wid].cursor_pet.is_active(),
            "load shedding alone freezes presentation without revoking ownership"
        );
        assert!(app.windows[&wid].input_scratch.free_sprites.is_empty());

        app.config.cursor_trail = Some(false);
        assert!(
            app.load_shed_active(),
            "owner removal does not end load shedding"
        );
        app.splice_word_decorations(wid, t0 + Duration::from_millis(32));
        let ws = &app.windows[&wid];
        assert!(
            !ws.cursor_pet.is_active() && !ws.cursor_pet.needs_frames(),
            "owner removal must retire the brain before load shedding returns"
        );
        assert!(
            ws.free_scratch.is_empty()
                && ws.input_scratch.free_sprites.is_empty()
                && ws.input_scratch.free_atlas.is_none(),
            "the retired capture publishes no companion projection"
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
            cancel: crate::control::CaptureCancellation::new(),
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
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        let mut retained = rx
            .recv()
            .expect("worker alive after dead client")
            .expect("second encode succeeds");
        assert_eq!(retained.value, (2, 2, None));
        retained
            .retention
            .as_mut()
            .expect("file reply carries exact handles")
            .prepare_write()
            .expect("live reply validates at the wire edge");
        // A dropped receiver never observes OK, so its unpublished exact file is
        // removed when the guard cannot cross into the control writer.
        assert!(!dir.join("dead.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod window_render_context_capture_tests {
    use super::App;
    use crate::{Backend, MetricsView, WindowId, control_auth};
    use aterm_core::terminal::CursorStyle;
    use std::time::Duration;

    #[derive(Debug, PartialEq)]
    struct BoundContext {
        font_px: f32,
        cell_size: (usize, usize),
        pad: usize,
        pad_top: usize,
        head: usize,
        blink_phase: bool,
        cursor_style: Option<CursorStyle>,
        selection_inactive: bool,
    }

    fn bound_context(app: &App) -> BoundContext {
        let Backend::Cpu(renderer) = app.backend.ready() else {
            panic!("headless test backend is CPU")
        };
        BoundContext {
            font_px: app.font_px,
            cell_size: renderer.cell_size(),
            pad: renderer.pad(),
            pad_top: renderer.pad_top(),
            head: renderer.head(),
            blink_phase: renderer.cursor_blink_phase(),
            cursor_style: renderer.cursor_style_override(),
            selection_inactive: renderer.selection_inactive(),
        }
    }

    fn expected_frame_size(app: &App, wid: WindowId) -> (u32, u32) {
        let window = app.windows.get(&wid).expect("capture window");
        let metrics = window.metrics;
        let (cell_w, cell_h) = app.win_cell_size(wid);
        (
            u32::try_from(usize::from(window.cols) * cell_w + 2 * metrics.pad).unwrap(),
            u32::try_from(
                usize::from(window.rows) * cell_h + metrics.pad + metrics.pad_top + metrics.head,
            )
            .unwrap(),
        )
    }

    fn capture(app: &mut App, wid: WindowId, dir: &std::path::Path, name: &str) -> (u32, u32) {
        app.frontmost_window = Some(wid);
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            target: control_auth::ConfinedImage::for_test(dir, name),
            clean: false,
            session: None,
            want_bytes: false,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        let mut retained = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("image worker reply")
            .expect("image capture succeeds");
        retained
            .retention
            .as_mut()
            .expect("file capture retains its exact path")
            .prepare_write()
            .expect("capture identity survives to the simulated wire edge");
        let (width, height, _) = retained.value;
        (width, height)
    }

    #[test]
    fn two_window_capture_rebinds_unequal_complete_render_contexts() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-window-render-context-{}",
            std::process::id()
        ));
        control_auth::ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        app.render_knobs.selection_inactive = true;
        app.font_px_explicit = false;

        let first = WindowId(0);
        let session = app.next_session_id;
        let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
        let mut first_metrics = MetricsView::for_scale(1.0);
        first_metrics.head = 3;
        let mut second_metrics = MetricsView::for_scale(2.0);
        second_metrics.head = 11;
        assert_ne!(
            first_metrics, second_metrics,
            "the regression must exercise genuinely unequal render contexts"
        );
        {
            let window = app.windows.get_mut(&first).unwrap();
            window.scale = 1.0;
            window.metrics = first_metrics;
            window.blink_phase = false;
            window.focused = false;
        }
        {
            let window = app.windows.get_mut(&second).unwrap();
            window.scale = 2.0;
            window.metrics = second_metrics;
            window.blink_phase = true;
            window.focused = true;
        }

        // Poison all shared channels with B before capturing A.
        app.font_px = second_metrics.font_px;
        app.backend.activate_px(second_metrics.font_px);
        app.backend.set_pad(second_metrics.pad);
        app.backend.set_pad_top(second_metrics.pad_top);
        app.backend.set_head(second_metrics.head);
        app.backend.set_cursor_blink_phase(true);
        app.backend
            .set_cursor_style_override(Some(CursorStyle::HollowBlock));
        app.backend.set_selection_inactive(false);

        assert_eq!(
            capture(&mut app, first, &dir, "first.png"),
            expected_frame_size(&app, first),
            "A's capture dimensions come from A's 1x metrics"
        );
        assert_eq!(
            bound_context(&app),
            BoundContext {
                font_px: first_metrics.font_px,
                cell_size: app.win_cell_size(first),
                pad: first_metrics.pad,
                pad_top: first_metrics.pad_top,
                head: first_metrics.head,
                blink_phase: false,
                cursor_style: None,
                selection_inactive: true,
            },
            "A owns every renderer-global channel at its raster seam"
        );

        assert_eq!(
            capture(&mut app, second, &dir, "second.png"),
            expected_frame_size(&app, second),
            "B's capture dimensions come from B's 2x metrics"
        );
        assert_eq!(
            bound_context(&app),
            BoundContext {
                font_px: second_metrics.font_px,
                cell_size: app.win_cell_size(second),
                pad: second_metrics.pad,
                pad_top: second_metrics.pad_top,
                head: second_metrics.head,
                blink_phase: true,
                cursor_style: None,
                selection_inactive: false,
            },
            "B cannot inherit A's typography or cursor/selection globals"
        );

        assert_eq!(
            capture(&mut app, first, &dir, "first-again.png"),
            expected_frame_size(&app, first),
            "switching back restores A's exact raster dimensions"
        );
        assert_eq!(
            bound_context(&app),
            BoundContext {
                font_px: first_metrics.font_px,
                cell_size: app.win_cell_size(first),
                pad: first_metrics.pad,
                pad_top: first_metrics.pad_top,
                head: first_metrics.head,
                blink_phase: false,
                cursor_style: None,
                selection_inactive: true,
            },
            "the complete context is rebound, never retained from the last capture"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod headless_cursor_fx_tests {
    use super::{
        CaptureCompanionSongSync, capture_companion_custody, capture_flying_companion_enabled,
        capture_ticks_cursor_fx, sync_capture_companion_song,
    };
    use crate::{App, WindowId, control_auth, term_lock};
    use std::time::{Duration, Instant};

    #[test]
    fn capture_companion_custody_matches_live_pet_song_handoff() {
        assert_eq!(
            capture_companion_custody(true, true, 211, 0.0, 0.0, true, false),
            (0, true, true),
            "the resting resident pet exclusively owns pet mode"
        );
        assert_eq!(
            capture_companion_custody(true, true, 211, 1.0, 1.0, true, true),
            (211, true, false),
            "the reduced-motion singer exclusively owns pixels while the hidden pet keeps its caret"
        );
        assert_eq!(
            capture_companion_custody(false, true, 211, 0.0, 0.0, false, false),
            (211, false, false),
            "classic mode admits its earned flying kitty"
        );
        assert_eq!(
            capture_companion_custody(true, true, 211, 0.49, 0.49, true, true),
            (211, true, false),
            "a late 1.0→0.49 reduced sample retains the opaque singer while preloading the pet"
        );
        assert_eq!(
            capture_companion_custody(true, false, 211, 0.0, 0.329, true, true),
            (0, true, true),
            "below the static face swap, capture returns both caret and pixel custody to the resident pet"
        );
    }

    #[test]
    fn capture_flying_gate_includes_the_live_static_song_exception() {
        use crate::cursor_glow::GlowStyle;

        assert!(capture_flying_companion_enabled(
            true,
            false,
            true,
            GlowStyle::RainbowKitty,
            false,
            1.0,
        ));
        assert!(!capture_flying_companion_enabled(
            true,
            false,
            true,
            GlowStyle::RainbowKitty,
            false,
            0.0,
        ));
        assert!(!capture_flying_companion_enabled(
            false,
            false,
            true,
            GlowStyle::RainbowKitty,
            false,
            1.0,
        ));
        assert!(capture_flying_companion_enabled(
            true,
            true,
            true,
            GlowStyle::RainbowKitty,
            false,
            0.0,
        ));
    }

    #[test]
    fn capture_owned_clock_syncs_and_retires_the_visual_singer_without_audio() {
        use aterm_effects::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
        use aterm_effects::kitty_sing::SING_ARM_REPEATS;

        let now = Instant::now();
        let start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
        let mut detector = aterm_effects::kitty_sing::KittySing::default();
        for i in 0..SING_ARM_REPEATS {
            detector.note_char(start + Duration::from_millis(u64::from(i) * 10), 0, 'a');
        }
        let mut cat = crate::kitty_cursor::CursorCat::default();
        let tenure_start = now - Duration::from_secs(7);
        for i in 0..160u64 {
            cat.on_pet_mode_motion_pulse(CursorCatMotionPulse {
                at: tenure_start + Duration::from_millis(i * 40),
                kind: CursorCatMotionKind::Advance,
            });
        }
        assert!(!cat.is_active(), "pet tenure stays hidden before the song");
        let mut glow = crate::cursor_glow::CursorGlow::default();
        let mut riff_bar = None;
        assert_eq!(
            sync_capture_companion_song(CaptureCompanionSongSync {
                style: crate::cursor_glow::GlowStyle::RainbowKitty,
                cursor_trail_enabled: true,
                cursor_companions_allowed: true,
                pet_mode: true,
                now,
                detector: &mut detector,
                cat: &mut cat,
                glow: &mut glow,
                riff_bar: &mut riff_bar,
            }),
            1.0
        );
        assert!(
            cat.is_active(),
            "the capture-owned song spends hidden tenure"
        );
        assert_eq!(cat.static_frame(now).sing, 1.0);
        assert!(riff_bar.is_some(), "the silent visual bar still advances");

        let drained = now + Duration::from_secs(2);
        assert_eq!(
            sync_capture_companion_song(CaptureCompanionSongSync {
                style: crate::cursor_glow::GlowStyle::RainbowKitty,
                cursor_trail_enabled: true,
                cursor_companions_allowed: true,
                pet_mode: true,
                now: drained,
                detector: &mut detector,
                cat: &mut cat,
                glow: &mut glow,
                riff_bar: &mut riff_bar,
            }),
            0.0
        );
        assert!(
            !cat.is_active(),
            "capture-owned pet mode grounds a fully drained singer"
        );
        assert_eq!(riff_bar, None);
    }

    #[test]
    fn production_headless_image_shows_one_reduced_motion_singing_companion() {
        use aterm_effects::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
        use aterm_effects::kitty_sing::SING_ARM_REPEATS;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // The companion this measures is the trail's; ask for its master explicitly so
        // the reduced-motion singing law is proved on Windows too, where the absent-key
        // default is OFF.
        app.config.cursor_trail = Some(true);
        app.config.motion = Some("reduced".into());
        app.recompute_sparkle();
        app.sparkle = None;
        let now = Instant::now();
        {
            let ws = app.windows.get_mut(&wid).expect("headless window 0");
            ws.focused = true;
            let tenure_start = now - Duration::from_secs(7);
            for i in 0..160u64 {
                ws.cursor_cat
                    .on_pet_mode_motion_pulse(CursorCatMotionPulse {
                        at: tenure_start + Duration::from_millis(i * 40),
                        kind: CursorCatMotionKind::Advance,
                    });
            }
            let sing_start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
            for i in 0..SING_ARM_REPEATS {
                ws.kitty_sing.note_char(
                    sing_start + Duration::from_millis(u64::from(i) * 10),
                    0,
                    'a',
                );
            }
        }

        let dir = std::env::temp_dir().join(format!(
            "aterm-headless-singing-companion-{}",
            std::process::id()
        ));
        control_auth::ensure_private_dir(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            target: control_auth::ConfinedImage::for_test(&dir, "singer.png"),
            clean: false,
            session: None,
            want_bytes: false,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("image worker reply")
            .expect("headless singer image succeeds");

        let ws = app.windows.get_mut(&wid).expect("headless window 0");
        assert!(
            ws.cursor_cat.is_active(),
            "the capture-owned detector sync spends the authenticated hidden tenure"
        );
        assert!(
            ws.cursor_pet.is_active(),
            "reduced motion keeps the resident pet caret-fed behind the opaque singer"
        );
        assert_eq!(
            ws.input_scratch.free_sprites.len(),
            1,
            "the production still contains one singing kitty, never a missing or doubled companion"
        );
        assert!(ws.input_scratch.free_atlas.is_some());
        assert_eq!(ws.cursor_cat.static_frame(Instant::now()).sing, 1.0);
        assert!(
            ws.input_scratch.word_decorations.is_empty()
                && ws.input_scratch.ink.is_empty()
                && ws.input_scratch.nova_add.is_empty(),
            "the disabled word feed stays byte-empty while its independent singing companion draws"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn production_headless_image_keeps_classic_kitty_without_sparkle_words() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        // THE FLYING HEAD'S OWN SPELLING. `rainbow kitty` selected it until
        // 2026-08-26 and now names the WALKING pet (the owner asked twice for
        // the kitty their config names), so this fixture — which is about the
        // earned classic flypast, and asserts `!cursor_pet.is_active()` below —
        // must say which animal it means.
        app.config.cursor_trail_style = Some("rainbow kitty flying".into());
        app.config.motion = Some("full".into());
        app.recompute_sparkle();
        app.sparkle = None;
        let now = Instant::now();
        {
            let ws = app.windows.get_mut(&wid).expect("headless window 0");
            ws.focused = true;
            let start = now - Duration::from_millis(95 * 40);
            for i in 0..96u64 {
                ws.cursor_cat
                    .on_key(start + Duration::from_millis(i * 40), true);
            }
            assert!(
                ws.cursor_cat.is_active(),
                "the fixture earns the real classic cursor-cat lifecycle"
            );
        }

        let dir = std::env::temp_dir().join(format!(
            "aterm-headless-classic-companion-{}",
            std::process::id()
        ));
        control_auth::ensure_private_dir(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            target: control_auth::ConfinedImage::for_test(&dir, "classic.png"),
            clean: false,
            session: None,
            want_bytes: false,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("image worker reply")
            .expect("classic kitty image succeeds");

        let ws = app.windows.get(&wid).expect("headless window 0");
        assert!(ws.cursor_cat.is_active());
        assert!(!ws.cursor_pet.is_active());
        assert_eq!(
            ws.input_scratch.free_sprites.len(),
            1,
            "the config-free capture carries exactly the one earned classic kitty"
        );
        assert!(ws.input_scratch.free_atlas.is_some());
        assert!(
            ws.input_scratch.word_decorations.is_empty()
                && ws.input_scratch.ink.is_empty()
                && ws.input_scratch.nova_add.is_empty(),
            "no word-owned channel is synthesized to host the independent kitty"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_headless_capture_keeps_the_default_pet_without_sparkle_words() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // ASK for the trail rather than inherit it: the master's absent-key default is
        // platform-split (`DEFAULT_DECORATIVE_EFFECTS`), and this fixture is about what
        // the pet RENDERS once it is on, which must hold identically on every host.
        // The STYLE is still the shipped default, asserted immediately below.
        app.config.cursor_trail = Some(true);
        assert_eq!(
            app.config.cursor_trail_style_raw(),
            crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
            "the fixture exercises the shipped default, not a forced style"
        );
        app.recompute_sparkle();
        app.sparkle = None;
        app.splice_word_decorations(wid, Instant::now());

        let ws = app.windows.get(&wid).expect("headless window 0");
        assert!(
            ws.cursor_pet.is_active(),
            "the first requested still and trail status agree that the pet is visible"
        );
        assert_eq!(
            ws.input_scratch.free_sprites.len(),
            1,
            "the first requested still carries exactly one default pet body"
        );
        assert!(
            ws.input_scratch.free_atlas.is_some(),
            "the pet body carries the atlas it addresses"
        );
        assert!(
            ws.input_scratch.word_decorations.is_empty()
                && ws.input_scratch.ink.is_empty()
                && ws.input_scratch.nova_add.is_empty(),
            "disabling Sparkle Words keeps every word-owned channel empty"
        );
    }

    #[test]
    fn first_headless_capture_applies_the_configured_pet_species() {
        use aterm_effects::kitty_pet::PetSpecies;

        let capture = |style: &str| {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.config.cursor_trail = Some(true);
            app.config.cursor_trail_style = Some(style.into());
            app.config.motion = Some("full".into());
            app.recompute_sparkle();
            app.sparkle = None;
            app.splice_word_decorations(wid, Instant::now());
            let ws = app.windows.get(&wid).expect("headless window 0");
            let sprite = ws
                .input_scratch
                .free_sprites
                .first()
                .copied()
                .expect("first capture emits one pet body");
            (
                ws.cursor_pet.species(),
                sprite,
                std::sync::Arc::clone(
                    ws.input_scratch
                        .free_atlas
                        .as_ref()
                        .expect("first capture publishes the pet atlas"),
                ),
            )
        };

        let cat = capture("rainbow kitty pet");
        let dog = capture("rainbow dog pet");
        assert_eq!(cat.0, PetSpecies::Cat, "negative control: cat stays cat");
        assert_eq!(dog.0, PetSpecies::Dog);
        assert_eq!(
            (dog.1.ax, dog.1.ay, dog.1.aw, dog.1.ah),
            (cat.1.ax, cat.1.ay, cat.1.aw, cat.1.ah),
            "equivalent poses deliberately occupy the same atlas slot"
        );
        assert!(
            dog.2.rgba != cat.2.rgba,
            "the first captured atlas must contain the configured dog skin"
        );
    }

    #[test]
    fn serious_mode_capture_hard_retires_a_song_rearmed_after_transition() {
        use aterm_effects::kitty_sing::SING_ARM_REPEATS;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".into());
        app.recompute_sparkle();
        app.sparkle = None;
        assert!(app.set_serious_mode(true));

        // Model the adversarial event ordering: cosmetic input bookkeeping is
        // source-agnostic and can run after the transition drain but before an
        // explicit capture. The old capture path consumed this live drive and
        // resurrected the singing face through its presentation exception.
        let now = Instant::now();
        let start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
        {
            let ws = app.windows.get_mut(&wid).expect("headless window 0");
            for i in 0..SING_ARM_REPEATS {
                ws.kitty_sing
                    .note_char(start + Duration::from_millis(u64::from(i) * 10), 0, 'a');
            }
            assert_eq!(
                ws.kitty_sing.drive(now),
                1.0,
                "negative control: the post-transition input really re-armed the singer"
            );
        }

        app.splice_word_decorations(wid, now);
        let ws = app.windows.get(&wid).expect("headless window 0");
        assert_eq!(ws.kitty_sing.drive(now), 0.0);
        assert!(!ws.cursor_cat.is_active());
        assert!(!ws.cursor_pet.is_active());
        assert!(!ws.music_notes.is_active());
        assert!(
            ws.input_scratch.free_sprites.is_empty() && ws.input_scratch.free_atlas.is_none(),
            "Serious Mode capture must publish no companion pixels"
        );
    }

    #[test]
    fn alt_screen_word_suppression_keeps_the_independent_capture_pet() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // The pet must be ON for "the word switch cannot blank it" to mean anything, so
        // the fixture asks for it instead of leaning on a platform-split default.
        app.config.cursor_trail = Some(true);
        app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
            enabled: Some(true),
            suppress_in_alt_screen: Some(true),
            ..Default::default()
        });
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.sparkle_dirty = true;
        app.recompute_sparkle();
        assert!(
            app.sparkle.is_some(),
            "negative control: word sparkles resolved on"
        );
        let terminal = app.front_terminal(wid).expect("front terminal");
        term_lock(&terminal.term).process(b"\x1b[?1049h");

        app.splice_word_decorations(wid, Instant::now());
        let ws = app.windows.get(&wid).expect("headless window 0");
        assert_eq!(
            ws.input_scratch.free_sprites.len(),
            1,
            "the word-only alt-screen switch cannot blank the resident cursor pet"
        );
        assert!(ws.input_scratch.free_atlas.is_some());
        assert!(
            ws.input_scratch.word_decorations.is_empty()
                && ws.input_scratch.ink.is_empty()
                && ws.input_scratch.nova_add.is_empty(),
            "the suppressed word-owned channels remain empty"
        );
    }

    #[test]
    fn composed_capture_decorations_use_the_extracted_screen_sample() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".into());
        let wid = WindowId(0);
        let focused_session = app.split_active_stub_tab(wid);
        let term = app
            .pool
            .get(focused_session)
            .expect("focused terminal")
            .term
            .clone();
        term_lock(&term).process(b"\x1b[?1049h\x1b[4;7H");
        let terminal_id = term_lock(&term).render_identity();
        let switch_term = term.clone();
        let now = Instant::now();
        let capture = app
            .prepare_terminal_capture_grid_with_cursor_fx_interleaved(
                wid,
                crate::app_render::ComposedCursorFxClock::Advance(now),
                move || term_lock(&switch_term).process(b"\x1b[?1049l\x1b[2;2H"),
            )
            .expect("composed capture");
        assert!(capture.focus.is_some_and(|sample| {
            sample.terminal_id == terminal_id && sample.alternate_screen
        }));
        assert!(
            !term_lock(&term).is_alternate_screen(),
            "negative control: the live terminal moved to a newer screen"
        );

        app.windows
            .get_mut(&wid)
            .expect("test window")
            .cursor_effect_coordinate_space = None;
        app.splice_word_decorations_sampled(
            wid,
            now,
            capture.focus.expect("focused capture sample"),
        );
        assert_eq!(
            app.windows[&wid].cursor_effect_coordinate_space,
            Some((terminal_id, true)),
            "decoration custody must stay on the screen paired with captured cells"
        );
    }

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
        // This headless fixture deliberately scripts the next cursor move. A
        // timestamp alone is not authored-movement evidence; use the engines'
        // explicit preview/test licence before changing the terminal cursor.
        {
            let ws = app.windows.get_mut(&wid).expect("headless window 0");
            ws.cursor_glow.note_synthetic_move(t0);
            ws.cursor_trail.note_synthetic_move(t0);
        }
        // Advance the scripted cursor without minting a parser generation: a
        // synthetic licence is for preview/test geometry, while parser output
        // is admitted only through the separate exact-content proof path.
        {
            let terminal = app.front_terminal(wid).expect("front terminal");
            term_lock(&terminal.term).grid_mut().cursor_forward(1);
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

    /// The PHASE-1 honesty gate BOTH ways: a capture without an OS window may
    /// tick the engines because it is that logical window's only app-render tick.
    /// A WINDOWED capture must retain LAST-PRESENT state; ticking would advance
    /// effects beyond the successful application-present artifact. PHASE-3
    /// adds the ONE-CLOCK-OWNER gate: while a recording targets the window its
    /// offscreen present loop owns the engine tick, so a concurrent `image`
    /// keeps the loop's last-present quads exactly like a windowed capture.
    #[test]
    fn windowed_capture_never_ticks_cursor_fx() {
        assert!(
            capture_ticks_cursor_fx(false, false),
            "no OS window and no recording: capture owns the app-render tick"
        );
        assert!(
            !capture_ticks_cursor_fx(true, false),
            "OS window: capture must keep the last application-present quads"
        );
        assert!(
            !capture_ticks_cursor_fx(false, true),
            "recording in flight: the offscreen present loop owns the engine clock"
        );
        assert!(
            !capture_ticks_cursor_fx(true, true),
            "OS window plus recording: still never tick at capture time"
        );
    }
}
