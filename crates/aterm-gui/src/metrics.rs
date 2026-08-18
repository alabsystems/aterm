// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-global render/latency counters, surfaced over the control socket as the
//! `metrics` verb so an AI driving aterm can MEASURE the terminal's responsiveness —
//! and DETECT lag — directly, instead of scraping the `$ATERM_TRACE_LATENCY` stderr
//! log or eyeballing it.
//!
//! aterm runs exactly one [`crate::App`] / event loop per process, so plain `static`
//! atomics are sufficient and avoid threading an `Arc` through the control listener:
//! the App (writer, on the present path) and the control thread (reader, in
//! `cmd_metrics`) live in the same process. All ops are `Relaxed` — these are monotone
//! diagnostics, never used for synchronization.
//!
//! ## Detecting lag (not just measuring a moment)
//!
//! A single "last frame" number can't reveal sustained jank, so this also keeps the
//! WORST-CASE and a SLOW-FRAME COUNT since the last [`reset`]:
//! - `frames_presented` — successful application present calls since reset (the
//!   D-1 early-out returns BEFORE present, so a steady app-render frame does not
//!   inflate this).
//! - `last_/max_present_latency_ns` — the
//!   `output→application-present-return` delay (PTY-output leading edge → the
//!   first attributed successful present return; the number
//!   `$ATERM_TRACE_LATENCY` logs), most recent and worst-since-reset. It does not
//!   observe compositor selection, display timing, scanout, or photons.
//! - `last_/max_frame_render_ns` — causal CPU wall time: compose plus CPU
//!   raster/copy, or time spent encoding GPU commands and calling `queue.submit`,
//!   most recent and worst-since-reset. This is not completed GPU execution;
//!   surface acquisition and final-present pacing are deliberately excluded.
//! - `slow_frames` — frames whose render time blew the [`SLOW_FRAME_THRESHOLD_NS`]
//!   (~30 fps) budget. A rising count is the lag signature — most often the CPU
//!   rasterizer redrawing heavy colour output (the "GPU was off" trap), so
//!   `backend=cpu` + climbing `slow_frames`/`max_frame_render_ms` is what to watch.
//! - `backend_gpu` — `true` when the live renderer is the GPU (Metal) path.
//!
//! A driver detects lag without OS profilers: `metrics reset`, drive the workload,
//! then `metrics` — if `slow_frames > 0`, or `max_frame_render_ms` is large, or
//! `backend=cpu` under heavy output, the terminal is lagging.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

static FRAMES_PRESENTED: AtomicU64 = AtomicU64::new(0);
static LAST_PRESENT_LATENCY_NS: AtomicU64 = AtomicU64::new(0);
static LAST_FRAME_RENDER_NS: AtomicU64 = AtomicU64::new(0);
// OFFSCREEN rasterizations (introspection `image`/`window`/`snapshot`): counted
// separately from real presents so a screenshot can never move the on-glass
// ledger. See `record_offscreen_raster`.
// Swapchain-acquire wait: the drawable-park slice. See `note_acquire_wait`.
static LAST_ACQUIRE_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_ACQUIRE_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static OFFSCREEN_RASTERS: AtomicU64 = AtomicU64::new(0);
static LAST_OFFSCREEN_RASTER_NS: AtomicU64 = AtomicU64::new(0);
static MAX_OFFSCREEN_RASTER_NS: AtomicU64 = AtomicU64::new(0);
static MAX_PRESENT_LATENCY_NS: AtomicU64 = AtomicU64::new(0);
static MAX_FRAME_RENDER_NS: AtomicU64 = AtomicU64::new(0);
static SLOW_FRAMES: AtomicU64 = AtomicU64::new(0);
static BACKEND_GPU: AtomicBool = AtomicBool::new(false);

// SYNC-1 (DEC-2026 frame-hold) observability. A pathological arm/timeout-release
// loop pins presents to ~1/timeout (the invisible ~5 fps failure class of
// 2026-07-05): `SYNC_RELEASES_TIMEOUT` climbing during ordinary typing IS that
// bug's fingerprint — a healthy interactive shell releases every episode via
// `?2026l` (`SYNC_RELEASES_END`) and times out ~never.
static SYNC_HOLDS_ARMED: AtomicU64 = AtomicU64::new(0);
static SYNC_RELEASES_END: AtomicU64 = AtomicU64::new(0);
static SYNC_RELEASES_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static SYNC_HOLDING: AtomicBool = AtomicBool::new(false);

// Load-adaptive shedding state (the `perf_reduced` latch + `MotionPolicy` fold).
// `PERF_REDUCED` mirrors the latch; `SHED_TRANSITIONS` counts its edges since
// reset. Shedding engaged during light typing, or flapping at idle, are both
// misbehaviour a driver can now assert against.
static PERF_REDUCED: AtomicBool = AtomicBool::new(false);
static SHED_TRANSITIONS: AtomicU64 = AtomicU64::new(0);

// Input→present latency: the slice a HUMAN feels when typing (key arrival →
// the next CONTENT present). `INPUT_STAMP_NS` holds the arrival of the oldest
// unpresented input (0 = none pending); the first content present consumes it.
// Monotonic ns via `now_ns` (an `Instant` can't live in an atomic).
//
// HONESTY BOUNDS: (1) a keystroke that produces NO output (`stty -echo`
// password prompts, an ignoring app) must not poison the metric when unrelated
// output arrives minutes later — a slice older than `INPUT_SLICE_CAP_NS` is
// DISCARDED at consume time, and `reset` drops any pending stamp. (2) The slice
// closes on the next content present, which under CONCURRENT streaming output
// (`tail -f` while typing) may be a log-line frame rather than the keystroke's
// echo — the metric then reads LOW, never high. It is a starvation detector
// (the smoke drives keys with no background stream), not an echo-attribution
// profiler.
static INPUT_STAMP_NS: AtomicU64 = AtomicU64::new(0);
static LAST_INPUT_PRESENT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_INPUT_PRESENT_NS: AtomicU64 = AtomicU64::new(0);

// THRU-2 SCHEDULING signal, split from the metric stamp above (touch-to-glass
// audit round 2). `INPUT_STAMP_NS` does double duty badly: it is consumed by the
// next CONTENT present, which under concurrent streaming output is a log-line
// frame, not the keystroke's echo — so the reader's fine 8 KiB slicing disarmed
// within one frame of each key and spent ~85% of a typing burst back on
// whole-64-KiB term-lock holds, exactly the starvation the slicing exists to
// prevent. This deadline is armed at HARDWARE key arrival (before any press-path
// term_lock) and decays `TYPING_HOT_TAIL_NS` after the LAST key, so it spans a
// whole typing burst and is immune to what the present path does.
static TYPING_HOT_UNTIL_NS: AtomicU64 = AtomicU64::new(0);
/// How long after a keystroke the reader keeps its term-lock holds fine. Longer
/// than a fast typist's inter-key gap (~125 ms at 100 wpm) so a burst never
/// disarms mid-word, short enough that a pure output flood pays the 8x lock
/// round-trip cost only while a human is actually at the keyboard.
const TYPING_HOT_TAIL_NS: u64 = 250_000_000;

// Lost-wake heals (the self-expiring `gated_output_wake` latch re-armed after a
// `Wake::Output` was consumed without a handler pass). ANY non-zero value means
// a wake was lost and healed — worth investigating even though the user only
// saw a ≤100 ms hiccup.
static WAKE_HEALS: AtomicU64 = AtomicU64::new(0);

// Whole-redraw wall time (redraw entry → present done). `frame_render` excludes
// surface acquisition and the final present, so a main-thread
// `nextDrawable`/compositor stall (~200 ms under GPU contention) hides from it —
// this pair makes that stall measurable.
static LAST_REDRAW_TOTAL_NS: AtomicU64 = AtomicU64::new(0);
static MAX_REDRAW_TOTAL_NS: AtomicU64 = AtomicU64::new(0);

// REDRAW-AUDIT: attempts are counted at `redraw_window` entry, before any
// target/hold/unchanged return.  A pass that reaches the present seam records
// its compose cost even when drawable acquisition subsequently fails, closing
// the old blind spot where `frames=0` could coexist with a 100%-CPU full-grid
// extraction loop.  The counters are intentionally separate rather than
// inferred from `frames_presented`: aborts can happen between these milestones.
static REDRAW_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static REDRAW_EARLY_OUTS: AtomicU64 = AtomicU64::new(0);
static REDRAW_SYNC_HOLDS: AtomicU64 = AtomicU64::new(0);
static REDRAW_RETRY_GATED: AtomicU64 = AtomicU64::new(0);
static PRE_PRESENT_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static LAST_PRE_PRESENT_NS: AtomicU64 = AtomicU64::new(0);
static PRE_PRESENT_TOTAL_NS: AtomicU64 = AtomicU64::new(0);
static MAX_PRE_PRESENT_NS: AtomicU64 = AtomicU64::new(0);
static PRESENT_DROPS: AtomicU64 = AtomicU64::new(0);
static LAST_PRESENT_DROP_REASON: AtomicU64 = AtomicU64::new(0);
static LAST_PRESENT_DROP_PARKED: AtomicBool = AtomicBool::new(false);

// EVENT-LOOP scheduler audit. `about_to_wait` publishes the owner of the one
// earliest armed deadline and its process-clock due stamp; `new_events`
// publishes the winit wake kind and attributes timer lateness back to that
// owner. A deadline selected already in the past is counted explicitly — the
// signature of a self-rearming `WaitUntil(past)` spin.
static EVENT_WAKES: AtomicU64 = AtomicU64::new(0);
static TIMER_WAKES: AtomicU64 = AtomicU64::new(0);
static WAIT_CANCELLED_WAKES: AtomicU64 = AtomicU64::new(0);
static POLL_WAKES: AtomicU64 = AtomicU64::new(0);
static LAST_WAKE_KIND: AtomicU64 = AtomicU64::new(0);
static LAST_WAKE_OWNER: AtomicU64 = AtomicU64::new(0);
static LAST_WAKE_LATE_NS: AtomicU64 = AtomicU64::new(0);
static LAST_DEADLINE_OWNER: AtomicU64 = AtomicU64::new(0);
static LAST_DEADLINE_DUE_NS: AtomicU64 = AtomicU64::new(0);
static LAST_DEADLINE_LATE_NS: AtomicU64 = AtomicU64::new(0);
static PAST_DEADLINE_ARMS: AtomicU64 = AtomicU64::new(0);

// Inter-frame GAP: wall time between two CONSECUTIVE presents (present→present),
// the direct hitch/stutter signal ARENA-SCROLL wants — `max_frame_render_ms`
// times ONE frame's own work, but a scrub that skips a frame (tier decode
// overran the vsync deadline, or the term lock was held by a compression spike)
// shows up as a GAP, not a slow single frame. `LAST_PRESENT_STAMP_NS` holds the
// previous present's `now_ns` (0 = none since reset, so the first present after
// a reset records no gap against stale history). MEANINGFUL ONLY inside a
// `reset` → drive-continuously → read window: presents stop at idle by design
// (the D-1 early-out), so a pause between driven steps is itself a real gap —
// pace the redraws (as the `video [pace]` verb does) for a clean cadence read.
static LAST_PRESENT_STAMP_NS: AtomicU64 = AtomicU64::new(0);
static MAX_FRAME_GAP_NS: AtomicU64 = AtomicU64::new(0);

/// A pending input slice older than this is a no-echo session (password
/// prompt, ignoring app), not a pacing measurement: discard, don't record.
const INPUT_SLICE_CAP_NS: u64 = 5_000_000_000;

// ---- RESIZE → PRESENT: the stale-frame (compositor stretch) window ----------
//
// THE THING THIS MEASURES, precisely: from a window-bounds change arriving
// (`WindowEvent::Resized`) to aterm having SUBMITTED a frame drawn at the new
// size. For that whole interval the layer is already the new size while the most
// recent drawable is the old one, so CoreAnimation shows the previous frame
// rescaled onto the new bounds — the smeared "shredded" text of a live drag.
// Shrinking this interval IS the fix; this is how you check it in milliseconds
// instead of by eye.
//
// WHY A METRIC AND NOT THE `video` LEDGER. The swapchain tap allocates its ring
// for one fixed geometry and sets `resized_early_stop` the instant the frame
// texture changes size (`aterm-gpu/src/video_tap.rs`), so a recording DIES on
// the first resize — the one event it would need to observe. A counter pair on
// the present path has no geometry to outgrow, costs two relaxed atomics, and is
// always on like every other slice here.
//
// KEEP-OLDEST, like `INPUT_STAMP_NS`. A drag delivers a burst of bounds changes;
// the honest number is how long the window spent showing a frame that did not
// match it, so a later change must not reset the clock and shrink the slice.
// The stamp is armed BEFORE the surface reconfigure, so it cannot miss the gap
// it is measuring.
//
// HONESTY BOUND: the close is "the next present after a bounds change", and the
// resize handler reconfigures the swapchain to the new size before that present
// happens — so the frame is at the new size by construction. It does NOT prove
// the grid REFLOWED to the new size (a width drag reflows on the throttle's own
// schedule); it proves a correctly-SIZED frame was submitted, which is exactly
// what ends the compositor's rescale. A slice past the cap is a window that
// stopped presenting entirely (occluded, parked surface) and is discarded rather
// than booked as a resize stall.
static RESIZE_STAMP_NS: AtomicU64 = AtomicU64::new(0);
static LAST_RESIZE_PRESENT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_RESIZE_PRESENT_NS: AtomicU64 = AtomicU64::new(0);

// ---- RESIZE → REFLOW: the stale-GRID window -------------------------------
//
// THE SECOND HALF, and the one `resize_present` structurally cannot see.
//
// `resize_present` closes on a frame at the new SURFACE size, and the surface is
// reconfigured in the `Resized` handler BEFORE the reflow throttle runs — so it
// goes green the moment the swapchain matches the window, whether or not the GRID
// has caught up. What a dragging user actually watches is the text: the columns
// and rows the engine committed. Between a bounds change and that commit the
// terminal body is the OLD grid letterboxed into the NEW window, which is the
// content trailing the window edge before it snaps.
//
// So this is armed with the same event and closed at the engine commit
// (`apply_term_resize` returning that it changed the geometry). The two together
// bracket a resize: `resize_present` = "did a correctly-sized frame go out",
// `resize_reflow` = "did the text catch up". A width drag deliberately keeps the
// second one long — that is the throttle bounding scrollback rewrap — while a
// row-only drag should have it near zero, because nothing needs rewrapping.
//
// KEEP-OLDEST and capped for the same reasons as its twin. A drag that coalesces
// several bounds changes into one commit reports the whole span it was stale, not
// just the tail; a resize whose commit never comes (the geometry did not change)
// is discarded rather than booked against whatever commits next.
static RESIZE_REFLOW_STAMP_NS: AtomicU64 = AtomicU64::new(0);
static LAST_RESIZE_REFLOW_NS: AtomicU64 = AtomicU64::new(0);
static MAX_RESIZE_REFLOW_NS: AtomicU64 = AtomicU64::new(0);

/// A pending resize slice older than this is a window that stopped presenting
/// (occluded / parked surface), not a resize stall: discard, don't record.
const RESIZE_SLICE_CAP_NS: u64 = 2_000_000_000;

/// Monotonic nanoseconds since the first call (process-lifetime clock for the
/// input→present stamps). Saturates far beyond any session length.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

fn now_ns() -> u64 {
    let start = *PROCESS_START.get_or_init(Instant::now);
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Stable labels for the event loop's single earliest `WaitUntil` owner.
/// Values are stored in atomics, so keep discriminants append-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum DeadlineOwner {
    None = 0,
    BootHealth = 1,
    PendingReveal = 2,
    Blink = 3,
    BellFlash = 4,
    CloseWarning = 5,
    SyncHold = 6,
    Autoscroll = 7,
    ResizeSettle = 8,
    PresentRetry = 9,
    FrameCap = 10,
    /// Retired bottom-HUD deadline slot. The numeric tombstone is retained because
    /// deadline-owner discriminants are an append-only diagnostics wire contract.
    Retired11 = 11,
    CursorEffect = 12,
    Rain = 13,
    SettingsDemo = 14,
    NativePreview = 15,
    /// Retired native Settings Diagnostics refresh slot. Preserve both the
    /// numeric value and legacy wire label for recorded-metrics compatibility.
    RetiredNativeDiagnostics16 = 16,
    Predictor = 17,
    ScrollGlide = 18,
    Overscroll = 19,
    ScrollPill = 20,
    WordDecorations = 21,
    ConfigNotice = 22,
    UpdateNotice = 23,
    LevelUp = 24,
    AutoApply = 25,
    ReaderResume = 26,
    UpgradeRealized = 27,
    WakeHeal = 28,
    Video = 29,
    TitleSummary = 30,
    SessionChrome = 31,
    TitleDrift = 32,
    /// The program cat's tenure gate (`app_kitty::KittyTenure`): one wake at
    /// the instant a pending claim earns or releases the cursor.
    KittyTenure = 33,
}

impl DeadlineOwner {
    fn from_raw(raw: u64) -> Self {
        match raw {
            1 => Self::BootHealth,
            2 => Self::PendingReveal,
            3 => Self::Blink,
            4 => Self::BellFlash,
            5 => Self::CloseWarning,
            6 => Self::SyncHold,
            7 => Self::Autoscroll,
            8 => Self::ResizeSettle,
            9 => Self::PresentRetry,
            10 => Self::FrameCap,
            11 => Self::Retired11,
            12 => Self::CursorEffect,
            13 => Self::Rain,
            14 => Self::SettingsDemo,
            15 => Self::NativePreview,
            16 => Self::RetiredNativeDiagnostics16,
            17 => Self::Predictor,
            18 => Self::ScrollGlide,
            19 => Self::Overscroll,
            20 => Self::ScrollPill,
            21 => Self::WordDecorations,
            22 => Self::ConfigNotice,
            23 => Self::UpdateNotice,
            24 => Self::LevelUp,
            25 => Self::AutoApply,
            26 => Self::ReaderResume,
            27 => Self::UpgradeRealized,
            28 => Self::WakeHeal,
            29 => Self::Video,
            30 => Self::TitleSummary,
            31 => Self::SessionChrome,
            32 => Self::TitleDrift,
            33 => Self::KittyTenure,
            _ => Self::None,
        }
    }

    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::BootHealth => "boot_health",
            Self::PendingReveal => "pending_reveal",
            Self::Blink => "blink",
            Self::BellFlash => "bell_flash",
            Self::CloseWarning => "close_warning",
            Self::SyncHold => "sync_hold",
            Self::Autoscroll => "autoscroll",
            Self::ResizeSettle => "resize_settle",
            Self::PresentRetry => "present_retry",
            Self::FrameCap => "frame_cap",
            Self::Retired11 => "retired-11",
            Self::CursorEffect => "cursor_effect",
            Self::Rain => "rain",
            Self::SettingsDemo => "settings_demo",
            Self::NativePreview => "native_preview",
            Self::RetiredNativeDiagnostics16 => "native_diagnostics",
            Self::Predictor => "predictor",
            Self::ScrollGlide => "scroll_glide",
            Self::Overscroll => "overscroll",
            Self::ScrollPill => "scroll_pill",
            Self::WordDecorations => "word_decorations",
            Self::ConfigNotice => "config_notice",
            Self::UpdateNotice => "update_notice",
            Self::LevelUp => "level_up",
            Self::AutoApply => "auto_apply",
            Self::ReaderResume => "reader_resume",
            Self::UpgradeRealized => "upgrade_realized",
            Self::WakeHeal => "wake_heal",
            Self::Video => "video",
            Self::TitleSummary => "title_summary",
            Self::SessionChrome => "session_chrome",
            Self::TitleDrift => "title_drift",
            Self::KittyTenure => "kitty_tenure",
        }
    }
}

/// Winit's reason for starting the current event-loop iteration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum EventWakeKind {
    None = 0,
    Init = 1,
    Poll = 2,
    Timer = 3,
    WaitCancelled = 4,
}

impl EventWakeKind {
    fn from_raw(raw: u64) -> Self {
        match raw {
            1 => Self::Init,
            2 => Self::Poll,
            3 => Self::Timer,
            4 => Self::WaitCancelled,
            _ => Self::None,
        }
    }

    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Init => "init",
            Self::Poll => "poll",
            Self::Timer => "timer",
            Self::WaitCancelled => "wait_cancelled",
        }
    }
}

/// Typed failure labels from the application-present seam. Besides making the
/// metric actionable, the retry scheduler uses `autonomous_retry` to park
/// conditions that should wait for an external surface/window stimulus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum PresentDropReason {
    None = 0,
    GpuReconfigured = 1,
    GpuTimeout = 2,
    GpuOccluded = 3,
    GpuValidation = 4,
    CpuResize = 5,
    CpuAcquire = 6,
    CpuCommit = 7,
    TargetMismatch = 8,
    Virtual = 9,
}

impl PresentDropReason {
    fn from_raw(raw: u64) -> Self {
        match raw {
            1 => Self::GpuReconfigured,
            2 => Self::GpuTimeout,
            3 => Self::GpuOccluded,
            4 => Self::GpuValidation,
            5 => Self::CpuResize,
            6 => Self::CpuAcquire,
            7 => Self::CpuCommit,
            8 => Self::TargetMismatch,
            9 => Self::Virtual,
            _ => Self::None,
        }
    }

    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::GpuReconfigured => "gpu_reconfigured",
            Self::GpuTimeout => "gpu_timeout",
            Self::GpuOccluded => "gpu_occluded",
            Self::GpuValidation => "gpu_validation",
            Self::CpuResize => "cpu_resize",
            Self::CpuAcquire => "cpu_acquire",
            Self::CpuCommit => "cpu_commit",
            Self::TargetMismatch => "target_mismatch",
            Self::Virtual => "virtual",
        }
    }

    #[must_use]
    pub(crate) const fn autonomous_retry(self) -> bool {
        matches!(
            self,
            Self::GpuReconfigured
                | Self::GpuTimeout
                | Self::CpuResize
                | Self::CpuAcquire
                | Self::CpuCommit
        )
    }
}

/// Anchor the GUI process-lifetime clock at the top of [`crate::main_entry`] so
/// `first_present_ns` retains its compatibility-stable GUI-entry → first
/// startup-metrics publication point inside the successful-present finalizer.
/// Publication runs after submit succeeds and includes initial reveal,
/// application acknowledgement, and any synchronous post-submit recovery
/// bookkeeping. Without this call the clock self-anchors at whichever metric
/// fires first and the startup figure is meaningless — call it before any
/// thread spawns. Platform compositor and scanout timing are outside this
/// boundary.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(Instant::now());
}

/// Anchor the shipped one-binary Rust entry before argv0/router work. Kept
/// separate from [`mark_process_start`] so the long-standing
/// `first_present_ns` boundary stays comparable across versions and thin GUI
/// binaries. Dyld/process-loader work precedes this stamp and remains outside
/// both metrics.
pub fn mark_rust_main_start() {
    let _ = RUST_MAIN_START.set(Instant::now());
}

static RUST_MAIN_START: OnceLock<Instant> = OnceLock::new();
static GUI_READY_FOR_WINIT: OnceLock<Instant> = OnceLock::new();
static FIRST_WINIT_RESUMED: OnceLock<Instant> = OnceLock::new();
static INITIAL_SURFACE_READY: OnceLock<Instant> = OnceLock::new();
static INITIAL_ATTACH_MILESTONES: OnceLock<StartupAttachMilestones> = OnceLock::new();

/// Mark the end of synchronous GUI preparation immediately before entering
/// winit. First-write wins because a process has one startup timeline.
pub(crate) fn mark_gui_ready_for_winit() {
    let _ = GUI_READY_FOR_WINIT.set(Instant::now());
}

/// Mark the first instruction of winit's first `resumed` callback.
pub(crate) fn mark_first_winit_resumed() {
    let _ = FIRST_WINIT_RESUMED.set(Instant::now());
}

/// Mark the first successfully attached OS window surface. A failed attachment
/// never advances the startup timeline.
pub(crate) fn mark_initial_surface_ready() {
    let _ = INITIAL_SURFACE_READY.set(Instant::now());
}

/// Wire schema for the exclusive Rust-main → first-present phase partition.
pub(crate) const STARTUP_PHASE_SCHEMA: u64 = 1;

/// Wire schema for the exclusive first-resume → initial-surface-ready attach
/// partition nested inside [`STARTUP_PHASE_SCHEMA`]'s surface-attach phase.
pub(crate) const STARTUP_ATTACH_SCHEMA: u64 = 1;

/// Ordered internal boundaries of the one successful initial window attach.
/// The caller records the complete array only after a present target has been
/// installed, so a failed OS-window or surface attempt cannot publish a partial
/// timeline. The outer `winit_resumed` and `surface_ready` stamps are supplied
/// by the existing startup ledger and make the eight subphases reconcile
/// exactly with `startup_initial_surface_attach_ns`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StartupAttachMilestones {
    points: [Instant; 7],
}

impl StartupAttachMilestones {
    pub(crate) const fn new(points: [Instant; 7]) -> Self {
        Self { points }
    }
}

/// Install a complete attach timeline into a first-write slot. Kept separate
/// from the process-global wrapper so the first-writer-wins rule has a local,
/// parallel negative-control test without contaminating process startup state.
fn record_initial_attach_milestones_once(
    slot: &OnceLock<StartupAttachMilestones>,
    milestones: StartupAttachMilestones,
) -> bool {
    slot.set(milestones).is_ok()
}

/// Publish the complete internal timeline for the first successfully installed
/// window surface. Every successful `attach_os_window` call may offer a complete
/// candidate; `OnceLock` admits exactly the first and later Cmd-N windows cannot
/// replace it. The winit application handler serializes production attaches on
/// the event-loop thread, while the atomic slot remains a backstop if that
/// lifecycle ever changes.
pub(crate) fn record_initial_attach_milestones(milestones: StartupAttachMilestones) {
    let _ = record_initial_attach_milestones_once(&INITIAL_ATTACH_MILESTONES, milestones);
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StartupPresentTiming {
    frame_started: Instant,
    pre_present: Instant,
    surface_return: Instant,
}

impl StartupPresentTiming {
    pub(crate) const fn new(
        frame_started: Instant,
        pre_present: Instant,
        surface_return: Instant,
    ) -> Self {
        Self {
            frame_started,
            pre_present,
            surface_return,
        }
    }

    pub(crate) const fn collapsed(at: Instant) -> Self {
        Self::new(at, at, at)
    }

    pub(crate) const fn frame_started(self) -> Instant {
        self.frame_started
    }

    pub(crate) fn finish(frame_started: Instant, pre_present: Option<Instant>) -> Self {
        pre_present.map_or_else(
            || Self::collapsed(frame_started),
            |pre_present| Self::new(frame_started, pre_present, Instant::now()),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupPhaseSample {
    valid: bool,
    router_ns: u64,
    gui_prepare_ns: u64,
    winit_dispatch_ns: u64,
    initial_surface_attach_ns: u64,
    surface_to_successful_redraw_ns: u64,
    successful_compose_ns: u64,
    successful_surface_transaction_ns: u64,
    successful_finalize_ns: u64,
}

impl StartupPhaseSample {
    fn total_ns(self) -> Option<u64> {
        [
            self.router_ns,
            self.gui_prepare_ns,
            self.winit_dispatch_ns,
            self.initial_surface_attach_ns,
            self.surface_to_successful_redraw_ns,
            self.successful_compose_ns,
            self.successful_surface_transaction_ns,
            self.successful_finalize_ns,
        ]
        .into_iter()
        .try_fold(0u64, |total, phase| total.checked_add(phase))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupAttachSample {
    valid: bool,
    dispatch_ns: u64,
    prepare_ns: u64,
    window_create_ns: u64,
    window_setup_ns: u64,
    backend_finalize_ns: u64,
    chrome_geometry_ns: u64,
    surface_create_ns: u64,
    finish_ns: u64,
}

impl StartupAttachSample {
    fn total_ns(self) -> Option<u64> {
        [
            self.dispatch_ns,
            self.prepare_ns,
            self.window_create_ns,
            self.window_setup_ns,
            self.backend_finalize_ns,
            self.chrome_geometry_ns,
            self.surface_create_ns,
            self.finish_ns,
        ]
        .into_iter()
        .try_fold(0u64, |total, phase| total.checked_add(phase))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct StartupMilestones {
    rust_main: Option<Instant>,
    gui_entry: Option<Instant>,
    gui_ready: Option<Instant>,
    winit_resumed: Option<Instant>,
    surface_ready: Option<Instant>,
}

fn duration_ns(start: Instant, end: Instant) -> Option<u64> {
    u64::try_from(end.checked_duration_since(start)?.as_nanos()).ok()
}

fn derive_startup_phases(
    milestones: StartupMilestones,
    timing: StartupPresentTiming,
    published_at: Instant,
) -> StartupPhaseSample {
    let Some(rust_main) = milestones.rust_main else {
        return StartupPhaseSample::default();
    };
    let Some(gui_entry) = milestones.gui_entry else {
        return StartupPhaseSample::default();
    };
    let Some(gui_ready) = milestones.gui_ready else {
        return StartupPhaseSample::default();
    };
    let Some(winit_resumed) = milestones.winit_resumed else {
        return StartupPhaseSample::default();
    };
    let Some(surface_ready) = milestones.surface_ready else {
        return StartupPhaseSample::default();
    };
    let Some(router_ns) = duration_ns(rust_main, gui_entry) else {
        return StartupPhaseSample::default();
    };
    let Some(gui_prepare_ns) = duration_ns(gui_entry, gui_ready) else {
        return StartupPhaseSample::default();
    };
    let Some(winit_dispatch_ns) = duration_ns(gui_ready, winit_resumed) else {
        return StartupPhaseSample::default();
    };
    let Some(initial_surface_attach_ns) = duration_ns(winit_resumed, surface_ready) else {
        return StartupPhaseSample::default();
    };
    let Some(surface_to_successful_redraw_ns) = duration_ns(surface_ready, timing.frame_started)
    else {
        return StartupPhaseSample::default();
    };
    let Some(successful_compose_ns) = duration_ns(timing.frame_started, timing.pre_present) else {
        return StartupPhaseSample::default();
    };
    let Some(successful_surface_transaction_ns) =
        duration_ns(timing.pre_present, timing.surface_return)
    else {
        return StartupPhaseSample::default();
    };
    let Some(successful_finalize_ns) = duration_ns(timing.surface_return, published_at) else {
        return StartupPhaseSample::default();
    };
    let sample = StartupPhaseSample {
        valid: true,
        router_ns,
        gui_prepare_ns,
        winit_dispatch_ns,
        initial_surface_attach_ns,
        surface_to_successful_redraw_ns,
        successful_compose_ns,
        successful_surface_transaction_ns,
        successful_finalize_ns,
    };
    let rust_main_total = duration_ns(rust_main, published_at);
    let gui_total = duration_ns(gui_entry, published_at);
    let partition_total = sample.total_ns();
    if partition_total != rust_main_total
        || partition_total.and_then(|total| total.checked_sub(router_ns)) != gui_total
    {
        return StartupPhaseSample::default();
    }
    sample
}

fn derive_startup_attach(
    winit_resumed: Option<Instant>,
    milestones: Option<StartupAttachMilestones>,
    surface_ready: Option<Instant>,
) -> StartupAttachSample {
    let Some(winit_resumed) = winit_resumed else {
        return StartupAttachSample::default();
    };
    let Some(milestones) = milestones else {
        return StartupAttachSample::default();
    };
    let Some(surface_ready) = surface_ready else {
        return StartupAttachSample::default();
    };
    let [
        attach_entry,
        before_window_create,
        after_window_create,
        before_backend_finalize,
        after_backend_finalize,
        before_surface_create,
        after_surface_create,
    ] = milestones.points;
    let Some(dispatch_ns) = duration_ns(winit_resumed, attach_entry) else {
        return StartupAttachSample::default();
    };
    let Some(prepare_ns) = duration_ns(attach_entry, before_window_create) else {
        return StartupAttachSample::default();
    };
    let Some(window_create_ns) = duration_ns(before_window_create, after_window_create) else {
        return StartupAttachSample::default();
    };
    let Some(window_setup_ns) = duration_ns(after_window_create, before_backend_finalize) else {
        return StartupAttachSample::default();
    };
    let Some(backend_finalize_ns) = duration_ns(before_backend_finalize, after_backend_finalize)
    else {
        return StartupAttachSample::default();
    };
    let Some(chrome_geometry_ns) = duration_ns(after_backend_finalize, before_surface_create)
    else {
        return StartupAttachSample::default();
    };
    let Some(surface_create_ns) = duration_ns(before_surface_create, after_surface_create) else {
        return StartupAttachSample::default();
    };
    let Some(finish_ns) = duration_ns(after_surface_create, surface_ready) else {
        return StartupAttachSample::default();
    };
    let sample = StartupAttachSample {
        valid: true,
        dispatch_ns,
        prepare_ns,
        window_create_ns,
        window_setup_ns,
        backend_finalize_ns,
        chrome_geometry_ns,
        surface_create_ns,
        finish_ns,
    };
    if sample.total_ns() != duration_ns(winit_resumed, surface_ready) {
        return StartupAttachSample::default();
    }
    sample
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupPresentSample {
    gui_entry_ns: u64,
    rust_main_ns: u64,
    phases: StartupPhaseSample,
    attach: StartupAttachSample,
}

// One coherent, immutable startup fact. `OnceLock` publishes both clock
// projections together, so a racing control snapshot cannot pair a populated
// compatibility field with a missing router field. `reset` deliberately keeps
// it. `rust_main_ns` is 0 only for the thin dev GUI binary.
static STARTUP_PRESENT: OnceLock<StartupPresentSample> = OnceLock::new();

// ---------------------------------------------------------------------------
// Latency distributions (the PERF_GYM §2 histogram slice — LAT-3).
//
// `last_*`/`max_*` scalars can't answer "what does typing feel like at p99",
// and a driver publishing latency claims needs percentiles, not anecdotes
// (FASTER_THAN_GHOSTTY_PLAN.md §4/LAT-3). Three log-linear histograms record
// every sample the scalars already see — same funnel, no new stamps, so the
// honesty bounds documented on the scalars apply to the distributions too.
// KNOWN LIMIT (PERF_GYM §2.4, unchanged by this slice): the single
// `INPUT_STAMP_NS` CAS keeps only the OLDEST edge of a coalesced input burst,
// so burst percentiles are conservative-high per group, and unmeasured
// keystrokes inside a group are simply absent (coordinated omission is NOT
// corrected here — the full edge-ring is a later slice).

/// 8 linear sub-buckets below 2^16 ns (8.2 µs grain), then 8 per octave up
/// through 2^36 ns (~69 s): 168 buckets ≈ ±6% relative error, 1.3 KiB each.
const H_SUB_BITS: u32 = 3;
const H_MIN_SHIFT: u32 = 16;
const H_OCTAVES: usize = 20;
const H_BUCKETS: usize = (1 << H_SUB_BITS) * (H_OCTAVES + 1);

/// Lock-free fixed-bucket log-linear histogram. Writers `fetch_add` one
/// bucket per sample (Relaxed — diagnostics, same contract as the scalars);
/// readers walk the buckets. Percentiles report the bucket UPPER edge, so a
/// published figure errs conservative (never better than reality).
pub struct Histogram {
    buckets: [AtomicU64; H_BUCKETS],
    count: AtomicU64,
}

impl Histogram {
    const fn new() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; H_BUCKETS],
            count: AtomicU64::new(0),
        }
    }

    fn index(v_ns: u64) -> usize {
        if v_ns < (1 << H_MIN_SHIFT) {
            return (v_ns >> (H_MIN_SHIFT - H_SUB_BITS)) as usize;
        }
        let msb = 63 - v_ns.leading_zeros(); // v_ns >= 2^16 here, so msb >= 16
        let octave = (msb - H_MIN_SHIFT + 1) as usize;
        let sub = ((v_ns >> (msb - H_SUB_BITS)) & ((1 << H_SUB_BITS) - 1)) as usize;
        ((octave << H_SUB_BITS) + sub).min(H_BUCKETS - 1)
    }

    /// The EXCLUSIVE upper edge of bucket `idx` in ns — every value in the
    /// bucket is strictly below it, so reporting it keeps percentiles
    /// conservative (never under the true value).
    fn upper_edge(idx: usize) -> u64 {
        let sub_count = 1u64 << H_SUB_BITS;
        let i = idx as u64;
        if idx < (1 << H_SUB_BITS) {
            return (i + 1) << (H_MIN_SHIFT - H_SUB_BITS);
        }
        let octave = i >> H_SUB_BITS; // >= 1
        let sub = i & (sub_count - 1);
        let msb = H_MIN_SHIFT + u32::try_from(octave).unwrap_or(u32::MAX) - 1;
        (1u64 << msb) + ((sub + 1) << (msb - H_SUB_BITS))
    }

    fn record(&self, v_ns: u64) {
        self.buckets[Self::index(v_ns)].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn reset(&self) {
        for b in &self.buckets {
            b.store(0, Ordering::Relaxed);
        }
        self.count.store(0, Ordering::Relaxed);
    }

    /// Samples recorded since the last reset.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// The value at quantile `q` (0 < q ≤ 1), as the containing bucket's upper
    /// edge in ns — conservative by construction. `None` until a sample lands.
    /// Concurrent writers can skew a racing read by a sample or two; that is
    /// the same take-what-you-see contract as every scalar here.
    pub fn percentile(&self, q: f64) -> Option<u64> {
        let total = self.count.load(Ordering::Relaxed);
        if total == 0 {
            return None;
        }
        // ceil(q * total), clamped to [1, total]: p50 of one sample is that sample.
        let target = ((q * total as f64).ceil() as u64).clamp(1, total);
        let mut seen = 0u64;
        for (i, b) in self.buckets.iter().enumerate() {
            seen += b.load(Ordering::Relaxed);
            if seen >= target {
                return Some(Self::upper_edge(i));
            }
        }
        // Writers raced `count` ahead of their bucket store: report the max edge.
        Some(Self::upper_edge(H_BUCKETS - 1))
    }
}

static H_INPUT_PRESENT: Histogram = Histogram::new();
static H_PRESENT_LATENCY: Histogram = Histogram::new();
static H_FRAME_RENDER: Histogram = Histogram::new();
static H_KEY_WRITE: Histogram = Histogram::new();
static H_PRE_PRESENT: Histogram = Histogram::new();
static H_ACQUIRE_WAIT: Histogram = Histogram::new();
static H_RESIZE_PRESENT: Histogram = Histogram::new();
static H_RESIZE_REFLOW: Histogram = Histogram::new();

/// The three live distributions, for the `metrics percentiles` verb:
/// input→application-present-return (a software-side typing proxy),
/// output→application-present-return (the `$ATERM_TRACE_LATENCY` slice), and
/// causal frame CPU work (compose plus CPU raster/copy or GPU command
/// encode/queue-submit).
#[must_use]
pub fn distributions() -> (&'static Histogram, &'static Histogram, &'static Histogram) {
    (&H_INPUT_PRESENT, &H_PRESENT_LATENCY, &H_FRAME_RENDER)
}

/// Hardware-key arrival → completed PTY write distribution. Kept separate from
/// [`distributions`] so existing callers of that three-histogram API remain
/// source-compatible.
#[must_use]
pub fn key_write_distribution() -> &'static Histogram {
    &H_KEY_WRITE
}

/// Window-bounds-change → first submitted frame at the new size. The live-drag
/// stale-frame (compositor rescale) window; see [`note_resize_arrival`].
#[must_use]
pub fn resize_present_distribution() -> &'static Histogram {
    &H_RESIZE_PRESENT
}

/// Window-bounds change → the engine committing the new GRID. The interval the
/// terminal body spends trailing the window edge; see [`note_grid_committed`].
#[must_use]
pub fn resize_reflow_distribution() -> &'static Histogram {
    &H_RESIZE_REFLOW
}

/// Redraw-entry → surface-acquire-seam distribution, including terminal grid
/// extraction. This remains observable when every actual present is dropped.
#[must_use]
pub fn pre_present_distribution() -> &'static Histogram {
    &H_PRE_PRESENT
}

/// Swapchain-acquire (`nextDrawable`) WAIT distribution — pure blocking on the
/// compositor, not work.
///
/// This slice previously had no instrument at all: it fell between
/// `note_pre_present` (which ends before the acquire) and the renderer's
/// `last_present_work_ns` (which starts after it), so it was inferable only as
/// `redraw_total - compose - raster_submit`, contaminated by the whole
/// post-present tail. That is the single largest known typing-stall mechanism on
/// macOS — a blocked acquire parks the winit main thread and queues keyDowns in
/// the OS event queue (measured at up to ~84 ms) — so the system was blind to its
/// own worst stall. Now it has a p50/p95/p99 of its own.
#[must_use]
pub fn acquire_wait_distribution() -> &'static Histogram {
    &H_ACQUIRE_WAIT
}

/// Record one present's swapchain-acquire wait. Cheap (one histogram bucket
/// increment); called on every successful present.
pub fn note_acquire_wait(ns: u64) {
    H_ACQUIRE_WAIT.record(ns);
    LAST_ACQUIRE_WAIT_NS.store(ns, Ordering::Relaxed);
    MAX_ACQUIRE_WAIT_NS.fetch_max(ns, Ordering::Relaxed);
}

/// A frame whose causal compose+raster/encode-submit CPU work exceeds a ~30 fps
/// (33.3 ms)
/// budget — the floor below which interaction visibly stutters. `slow_frames` counts
/// these so a driver can DETECT sustained lag rather than read a momentary value.
pub const SLOW_FRAME_THRESHOLD_NS: u64 = 33_333_333; // 1/30 s

/// Record one successful application present. `latency_ns` is the
/// `output→application-present-return` delay for this frame, or `0` when no
/// output burst was pending (a blink/selection/resize repaint) — a `0` leaves
/// the last real measurement in place and is NOT a slow-frame input.
/// `render_ns` is this frame's causal CPU wall time: compose plus CPU raster/copy,
/// or time spent encoding GPU commands and calling `queue.submit`. It is not
/// completed GPU execution and excludes surface acquisition and final-present
/// pacing.
pub(crate) fn record_present(
    latency_ns: u64,
    render_ns: u64,
    startup_timing: StartupPresentTiming,
) {
    // First startup-metrics publication point inside the successful-present
    // finalizer (see `mark_process_start`). Capture one end Instant and derive
    // both scopes from it, then publish the pair atomically through OnceLock
    // before the frame count becomes observable.
    let _ = STARTUP_PRESENT.get_or_init(|| {
        let published_at = Instant::now();
        let elapsed_ns = |start: Instant| {
            u64::try_from(published_at.saturating_duration_since(start).as_nanos())
                .unwrap_or(u64::MAX)
                .max(1)
        };
        StartupPresentSample {
            gui_entry_ns: elapsed_ns(*PROCESS_START.get_or_init(|| published_at)),
            rust_main_ns: RUST_MAIN_START.get().copied().map_or(0, elapsed_ns),
            phases: derive_startup_phases(
                StartupMilestones {
                    rust_main: RUST_MAIN_START.get().copied(),
                    gui_entry: PROCESS_START.get().copied(),
                    gui_ready: GUI_READY_FOR_WINIT.get().copied(),
                    winit_resumed: FIRST_WINIT_RESUMED.get().copied(),
                    surface_ready: INITIAL_SURFACE_READY.get().copied(),
                },
                startup_timing,
                published_at,
            ),
            attach: derive_startup_attach(
                FIRST_WINIT_RESUMED.get().copied(),
                INITIAL_ATTACH_MILESTONES.get().copied(),
                INITIAL_SURFACE_READY.get().copied(),
            ),
        }
    });
    FRAMES_PRESENTED.fetch_add(1, Ordering::Release);
    // Close the RESIZE→PRESENT slice on ANY successful present, not only a
    // content one: what ends the compositor's rescale is a frame at the new
    // size reaching the WSI, whatever drew it. (The input slice below is
    // deliberately content-gated instead — a blink repaint is not an echo.)
    let resize_stamp = RESIZE_STAMP_NS.swap(0, Ordering::Relaxed);
    if resize_stamp != 0 {
        let d = now_ns().saturating_sub(resize_stamp);
        if d <= RESIZE_SLICE_CAP_NS {
            LAST_RESIZE_PRESENT_NS.store(d, Ordering::Relaxed);
            MAX_RESIZE_PRESENT_NS.fetch_max(d, Ordering::Relaxed);
            H_RESIZE_PRESENT.record(d);
        }
    }
    if latency_ns != 0 {
        LAST_PRESENT_LATENCY_NS.store(latency_ns, Ordering::Relaxed);
        MAX_PRESENT_LATENCY_NS.fetch_max(latency_ns, Ordering::Relaxed);
        H_PRESENT_LATENCY.record(latency_ns);
        // A CONTENT present: close the pending
        // input→application-present-return slice, if any. A latency of 0
        // (blink/selection repaint) leaves the stamp pending — no attributed
        // content present has completed yet.
        let stamp = INPUT_STAMP_NS.swap(0, Ordering::Relaxed);
        if stamp != 0 {
            let d = now_ns().saturating_sub(stamp);
            // A slice past the cap means the keystroke never echoed (see the
            // HONESTY BOUNDS above) — recording it would peg the max with a
            // latency that never happened.
            if d <= INPUT_SLICE_CAP_NS {
                LAST_INPUT_PRESENT_NS.store(d, Ordering::Relaxed);
                MAX_INPUT_PRESENT_NS.fetch_max(d, Ordering::Relaxed);
                H_INPUT_PRESENT.record(d);
            }
        }
    }
    LAST_FRAME_RENDER_NS.store(render_ns, Ordering::Relaxed);
    MAX_FRAME_RENDER_NS.fetch_max(render_ns, Ordering::Relaxed);
    H_FRAME_RENDER.record(render_ns);
    if render_ns > SLOW_FRAME_THRESHOLD_NS {
        SLOW_FRAMES.fetch_add(1, Ordering::Relaxed);
    }
    // Present→present gap: record the delta only when a previous present exists
    // in this window (stamp != 0), so the first post-reset present never books a
    // spurious gap against pre-reset (or cold-start) history.
    let now = now_ns();
    let prev_stamp = LAST_PRESENT_STAMP_NS.swap(now, Ordering::Relaxed);
    if prev_stamp != 0 {
        MAX_FRAME_GAP_NS.fetch_max(now.saturating_sub(prev_stamp), Ordering::Relaxed);
    }
}

/// An OFFSCREEN rasterization (`image` / `window` / `snapshot` introspection) —
/// pixels built into a buffer that never reach glass.
///
/// These used to call [`record_present`] with a zero latency, which bumped
/// `frames`, `last_/max_frame_render_ns`, `slow_frames` and the present-gap clock
/// with work no real present ever did. A full-frame `image` rasterization is far
/// more expensive than a scissored dirty-row on-glass frame and happens at an
/// arbitrary time relative to real presents, so the natural measurement protocol
/// (`metrics reset` → drive → `image` → `metrics`) silently corrupted the exact
/// counters it was reading: `max_frame_render_ms` took a value nothing on the
/// present path produced, `slow_frames` could go 0 → 1 on a healthy run, and
/// `max_frame_gap_ms` was skewed in BOTH directions (an `image` between two
/// presents shrinks a real hitch out of existence; one taken long after the last
/// present books a multi-second phantom gap).
///
/// Its own counters keep headless/introspection runs observable — the reason the
/// original calls existed — without contaminating the on-glass ledger.
pub fn record_offscreen_raster(render_ns: u64) {
    OFFSCREEN_RASTERS.fetch_add(1, Ordering::Relaxed);
    LAST_OFFSCREEN_RASTER_NS.store(render_ns, Ordering::Relaxed);
    MAX_OFFSCREEN_RASTER_NS.fetch_max(render_ns, Ordering::Relaxed);
}

/// Stamp the arrival of user input bound for the PTY (a keystroke or a control
/// `send`/`key`). Keeps the OLDEST unpresented arrival — the worst case is the
/// honest one — so a burst does not shrink the measured slice.
///
/// INHERITS the pending TRUE key arrival when one is armed (macOS backdates it
/// by the NSEvent queue age — see [`note_key_arrival_queued`]), so
/// `input->present` includes time the keystroke spent parked in the OS event
/// queue: the drawable-park slice a stamp taken here (post-dequeue, mid-handler)
/// structurally hides. Freshness-gated to 500 ms: a key that never reached the
/// PTY (a UI shortcut leaves its stamp unconsumed — `note_pty_write` never runs)
/// must not lend its stale arrival to a later control-verb `send`.
pub fn note_input() {
    let now = now_ns();
    let key = LAT_KEY_NS.load(Ordering::Relaxed);
    let stamp = if key != 0 && now.saturating_sub(key) < 500_000_000 {
        key
    } else {
        now
    };
    let _ = INPUT_STAMP_NS.compare_exchange(0, stamp, Ordering::Relaxed, Ordering::Relaxed);
    note_typing_hot();
}

/// Stamp the arrival of a window-bounds change (`WindowEvent::Resized`), opening
/// the stale-frame window that the next present closes.
///
/// Call this BEFORE the swapchain is reconfigured, so the slice contains the
/// whole interval during which the layer is the new size and the newest drawable
/// is still the old one — the interval CoreAnimation fills by rescaling the
/// previous frame. Keeps the OLDEST unpresented change (see `RESIZE_STAMP_NS`):
/// a drag burst reports how long the window was actually mismatched, not just
/// the tail after its final event.
pub fn note_resize_arrival() {
    let now = now_ns();
    let _ = RESIZE_STAMP_NS.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
    let _ = RESIZE_REFLOW_STAMP_NS.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
}

/// The engine just COMMITTED a new grid geometry (`apply_term_resize` reported a
/// real change), closing the stale-grid slice a bounds change opened.
///
/// Closed here rather than at a present because this is the moment the text stops
/// trailing the window: the rows/cols the user is reading are now the ones the
/// window has. A commit with no bounds change behind it (the control `resize`
/// verb, a font zoom, a config re-grid) finds no stamp and records nothing.
pub fn note_grid_committed() {
    let stamp = RESIZE_REFLOW_STAMP_NS.swap(0, Ordering::Relaxed);
    if stamp == 0 {
        return;
    }
    let d = now_ns().saturating_sub(stamp);
    if d <= RESIZE_SLICE_CAP_NS {
        LAST_RESIZE_REFLOW_NS.store(d, Ordering::Relaxed);
        MAX_RESIZE_REFLOW_NS.fetch_max(d, Ordering::Relaxed);
        H_RESIZE_REFLOW.record(d);
    }
}

/// Arm the THRU-2 interactivity window: a human just pressed a key, so the PTY
/// reader must keep its term-lock holds FINE until `TYPING_HOT_TAIL_NS` after the
/// LAST key.
///
/// Called at the TRUE hardware arrival (the `WindowEvent::KeyboardInput` arm),
/// BEFORE any press-path `term_lock` — that ordering is the whole point. The
/// previous design armed the signal from `note_input` deep inside
/// `input_to_session`, i.e. AFTER three-to-four terminal-mutex acquisitions had
/// already queued behind whole-64-KiB reader holds; only the final `seam_egress`
/// acquisition benefited from the signal the keystroke itself had set.
///
/// Deliberately NOT tied to `INPUT_STAMP_NS`: that stamp is the latency METRIC and
/// is consumed by the next content present (a flood frame, not necessarily this
/// key's echo), which silently disarmed the scheduling hint mid-burst.
pub fn note_typing_hot() {
    TYPING_HOT_UNTIL_NS.store(
        now_ns().saturating_add(TYPING_HOT_TAIL_NS),
        Ordering::Relaxed,
    );
}

/// Whether a human is actively typing (armed by [`note_typing_hot`] at hardware
/// key arrival, decaying `TYPING_HOT_TAIL_NS` after the last key). THRU-2's ingest
/// signal: the PTY reader keeps its term-lock holds FINE (8 KiB slices) inside
/// this window and takes wider holds otherwise — so the interactivity bound the
/// chunking exists for is enforced exactly when a human is waiting on it, and a
/// pure output flood pays far fewer lock round-trips. Read-only, lock-free,
/// Relaxed (a stale read for one hold is harmless: the next hold re-reads, and a
/// mid-hold arrival waits at most one capped process — sub-frame).
#[must_use]
pub fn input_pending() -> bool {
    let until = TYPING_HOT_UNTIL_NS.load(Ordering::Relaxed);
    until != 0 && now_ns() < until
}

// ---- ATERM_LATENCY_TRACE: isolate the UI-thread key->write component ---------
// `note_input` now runs BEFORE encode/write, so `input_present` already spans the
// full key-arrival → content-present path (including encode and the PTY write).
// This trace keeps an independently useful key→write slice: it identifies UI
// dispatch/encoding/lock/write stalls inside a high end-to-end sample. Do NOT add
// it to `input_present`; it is a contained component of that total. Logging is
// env-gated; the cheap metrics atomics remain always on.
static LAT_TRACE_ON: OnceLock<bool> = OnceLock::new();
static LAT_KEY_NS: AtomicU64 = AtomicU64::new(0);
// Rolling worst-case key->write (µs) since the last `metrics` read, for the verb.
static MAX_KEY_WRITE_NS: AtomicU64 = AtomicU64::new(0);
static LAST_KEY_WRITE_NS: AtomicU64 = AtomicU64::new(0);

fn lat_trace_on() -> bool {
    *LAT_TRACE_ON.get_or_init(|| std::env::var_os("ATERM_LATENCY_TRACE").is_some())
}

/// Public µs view of the shared metrics clock — the SAME epoch as
/// [`note_key_arrival`], so the VIDEO introspection's frame stamps and its
/// input stamps subtract validly (key→frame latency by construction).
#[must_use]
pub fn now_us() -> u64 {
    now_ns() / 1000
}

/// Stamp the TRUE arrival of a printable hardware keystroke (winit
/// `KeyboardInput`, before encode/write), BACKDATED by `queue_ns` — the time the
/// key's NSEvent sat in the OS event queue before dispatch (macOS:
/// `platform::current_event_queue_age_ns`; 0 elsewhere). The stamp lands at the
/// HARDWARE arrival, so `key->write` (and `note_input`'s inherited
/// `input->present` clock) finally includes queueing a parked event loop
/// inflicted — the slice the touch-to-glass audit proved every instrument was
/// blind to. Overwrites (keep-LATEST) so each key is measured against its OWN
/// write, never accumulating across keys.
pub fn note_key_arrival_queued(queue_ns: u64) {
    LAT_KEY_NS.store(now_ns().saturating_sub(queue_ns).max(1), Ordering::Relaxed);
}

/// Disarm the key-arrival stamp: called after a key's dispatch ends WITHOUT a
/// PTY write (`note_pty_write` never consumed it), so a later control-verb
/// `send` can never inherit an unrelated key's arrival through [`note_input`].
/// A no-op after a writing key (the stamp is already 0).
pub fn clear_key_arrival() {
    LAT_KEY_NS.store(0, Ordering::Relaxed);
}

/// The PTY write just RETURNED (after the — on Windows blocking — `WriteFile` and
/// the press-path `term_lock`s). Record the key→write COMPONENT already contained
/// by the end-to-end `input_present` clock. Always recorded (cheap atomics) so the
/// `metrics` verb can surface it; also logged under `ATERM_LATENCY_TRACE`.
pub fn note_pty_write() {
    let key = LAT_KEY_NS.swap(0, Ordering::Relaxed);
    note_pty_write_at(key);
}

/// CLAIM the pending key-arrival stamp without recording anything, handing it to
/// whoever will actually perform the write.
///
/// The paste-ordering FIFO defers a keystroke's egress to a writer thread, so the
/// UI thread's `note_pty_write` was consuming the arrival stamp at ENQUEUE time —
/// recording the ~microsecond cost of pushing onto a channel and leaving the real
/// write, which is queued behind up to the whole paste, measured by nothing. The
/// instrument therefore reported its BEST numbers in precisely the window where a
/// human's key→write is at its worst. Pair with [`note_pty_write_at`].
#[must_use]
pub fn take_key_arrival() -> u64 {
    LAT_KEY_NS.swap(0, Ordering::Relaxed)
}

/// Hand a claimed arrival stamp BACK, for a deferral that did not happen after all
/// (the FIFO writer was gone, so the caller falls back to an inline write). Without
/// this the claim would silently swallow the sample on the fallback path — the same
/// measurement hole [`take_key_arrival`] exists to close, just one branch over.
/// A `0` restores nothing; a stamp already re-armed by a newer key wins.
pub fn restore_key_arrival(key: u64) {
    if key != 0 {
        let _ = LAT_KEY_NS.compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// Record a key→write slice against an arrival stamp claimed earlier by
/// [`take_key_arrival`] — for writes that complete OFF the UI thread. A `0` stamp
/// (no key behind this write: a control verb, an already-consumed arrival) records
/// nothing, exactly as the inline path's early return does.
pub fn note_pty_write_at(key: u64) {
    if key == 0 {
        return;
    }
    let d = now_ns().saturating_sub(key);
    LAST_KEY_WRITE_NS.store(d, Ordering::Relaxed);
    MAX_KEY_WRITE_NS.fetch_max(d, Ordering::Relaxed);
    H_KEY_WRITE.record(d);
    if lat_trace_on() {
        eprintln!(
            "KEYWRITE key->write={:.2}ms (UI encode + term_locks + blocking WriteFile)",
            d as f64 / 1_000_000.0
        );
    }
}

/// A DEC-2026 hold episode armed (the FALSE→TRUE rising edge saw its first held
/// frame).
pub fn note_sync_armed() {
    SYNC_HOLDS_ARMED.fetch_add(1, Ordering::Relaxed);
}

/// An armed episode released because the app ended sync (`?2026l`) — the healthy
/// release cause.
pub fn note_sync_release_end() {
    SYNC_RELEASES_END.fetch_add(1, Ordering::Relaxed);
    SYNC_HOLDING.store(false, Ordering::Relaxed);
}

/// An armed episode released by the safety-valve deadline — the app went silent
/// (or the hold machinery mis-paced) after `?2026h`. Climbing during ordinary
/// typing = the SYNC-1 failure class.
pub fn note_sync_release_timeout() {
    SYNC_RELEASES_TIMEOUT.fetch_add(1, Ordering::Relaxed);
    SYNC_HOLDING.store(false, Ordering::Relaxed);
}

/// Whether a present was just held (gauge, not a counter).
pub fn set_sync_holding(on: bool) {
    SYNC_HOLDING.store(on, Ordering::Relaxed);
}

/// The `perf_reduced` load-shed latch flipped (called on the EDGE only).
pub fn note_shed_transition(reduced: bool) {
    PERF_REDUCED.store(reduced, Ordering::Relaxed);
    SHED_TRANSITIONS.fetch_add(1, Ordering::Relaxed);
}

/// A lost `Wake::Output` was healed by the self-expiring latch.
pub fn note_wake_heal() {
    WAKE_HEALS.fetch_add(1, Ordering::Relaxed);
}

/// Record one redraw pass's TOTAL wall time (entry → present done OR failed
/// surface transaction), which includes the acquire/present wait `frame_render`
/// deliberately excludes. Failed attempts are load-bearing evidence: omitting
/// them makes the exact redraw responsible for a stall disappear from metrics.
pub fn record_redraw_total(total_ns: u64) {
    LAST_REDRAW_TOTAL_NS.store(total_ns, Ordering::Relaxed);
    MAX_REDRAW_TOTAL_NS.fetch_max(total_ns, Ordering::Relaxed);
}

/// A `RedrawRequested` entered the real redraw pipeline.
pub fn note_redraw_attempt() {
    REDRAW_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
}

/// The stable RepaintKey/predictor/effect gate proved that no pixels changed.
pub fn note_redraw_early_out() {
    REDRAW_EARLY_OUTS.fetch_add(1, Ordering::Relaxed);
}

/// A synchronized-output hold intentionally retained the previous frame.
pub fn note_redraw_sync_hold() {
    REDRAW_SYNC_HOLDS.fetch_add(1, Ordering::Relaxed);
}

/// A redraw source fired while surface recovery was backed off or parked. This
/// is cheap and expected for an occasional animation tick; sustained growth
/// identifies the caller that is still requesting redraws, without repeating
/// grid extraction or surface acquisition.
pub fn note_redraw_retry_gated() {
    REDRAW_RETRY_GATED.fetch_add(1, Ordering::Relaxed);
}

/// A redraw reached the surface-acquire seam. `compose_ns` includes every
/// pre-acquire operation, notably full `Terminal::cell_frame_into` extraction.
pub fn note_pre_present(compose_ns: u64) {
    PRE_PRESENT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    LAST_PRE_PRESENT_NS.store(compose_ns, Ordering::Relaxed);
    PRE_PRESENT_TOTAL_NS.fetch_add(compose_ns, Ordering::Relaxed);
    MAX_PRE_PRESENT_NS.fetch_max(compose_ns, Ordering::Relaxed);
    H_PRE_PRESENT.record(compose_ns);
}

/// The surface transaction failed after a redraw reached the present seam.
pub fn note_present_drop(reason: PresentDropReason, parked: bool) {
    PRESENT_DROPS.fetch_add(1, Ordering::Relaxed);
    update_present_drop_disposition(reason, parked);
}

/// Update the live disposition of an already-counted dropped frame. Recovery
/// can discover a more specific downstream cause (for example CPU fallback
/// construction after a lost GPU) without pretending that the same frame was
/// dropped twice.
pub fn update_present_drop_disposition(reason: PresentDropReason, parked: bool) {
    LAST_PRESENT_DROP_REASON.store(reason as u64, Ordering::Relaxed);
    LAST_PRESENT_DROP_PARKED.store(parked, Ordering::Relaxed);
}

/// Publish the event loop's selected deadline. `None` means pure `Wait`.
pub fn record_deadline(owner: DeadlineOwner, deadline: Option<Instant>, now: Instant) {
    let clock_now = now_ns();
    let (owner, due, late) = deadline.map_or((DeadlineOwner::None, 0, 0), |deadline| {
        if deadline >= now {
            let ahead = u64::try_from(deadline.duration_since(now).as_nanos()).unwrap_or(u64::MAX);
            (owner, clock_now.saturating_add(ahead), 0)
        } else {
            let late = u64::try_from(now.duration_since(deadline).as_nanos()).unwrap_or(u64::MAX);
            PAST_DEADLINE_ARMS.fetch_add(1, Ordering::Relaxed);
            (owner, clock_now.saturating_sub(late), late)
        }
    });
    LAST_DEADLINE_OWNER.store(owner as u64, Ordering::Relaxed);
    LAST_DEADLINE_DUE_NS.store(due, Ordering::Relaxed);
    LAST_DEADLINE_LATE_NS.store(late, Ordering::Relaxed);
}

/// Record why winit began this event-loop iteration and attribute a timer wake
/// to the owner published by the preceding `about_to_wait` pass.
pub fn note_event_wake(kind: EventWakeKind) {
    EVENT_WAKES.fetch_add(1, Ordering::Relaxed);
    LAST_WAKE_KIND.store(kind as u64, Ordering::Relaxed);
    // Only `ResumeTimeReached` is caused by the selected deadline. An external
    // event (`WaitCancelled`), explicit polling, or initialization may merely
    // INTERRUPT a pending blink/predictor/retry deadline; attributing that wake
    // to the interrupted owner makes the diagnostic falsely causal.
    let owner = wake_owner(
        kind,
        DeadlineOwner::from_raw(LAST_DEADLINE_OWNER.load(Ordering::Relaxed)),
    );
    LAST_WAKE_OWNER.store(owner as u64, Ordering::Relaxed);
    match kind {
        EventWakeKind::Timer => {
            TIMER_WAKES.fetch_add(1, Ordering::Relaxed);
            let due = LAST_DEADLINE_DUE_NS.load(Ordering::Relaxed);
            LAST_WAKE_LATE_NS.store(
                if due == 0 {
                    0
                } else {
                    now_ns().saturating_sub(due)
                },
                Ordering::Relaxed,
            );
        }
        EventWakeKind::WaitCancelled => {
            WAIT_CANCELLED_WAKES.fetch_add(1, Ordering::Relaxed);
            LAST_WAKE_LATE_NS.store(0, Ordering::Relaxed);
        }
        EventWakeKind::Poll => {
            POLL_WAKES.fetch_add(1, Ordering::Relaxed);
            LAST_WAKE_LATE_NS.store(0, Ordering::Relaxed);
        }
        EventWakeKind::Init | EventWakeKind::None => {
            LAST_WAKE_LATE_NS.store(0, Ordering::Relaxed);
        }
    }
}

/// Attribute an event-loop wake only when winit says a timer actually reached
/// its resume instant. Kept pure so the causal-label rule has a race-free test.
const fn wake_owner(kind: EventWakeKind, deadline_owner: DeadlineOwner) -> DeadlineOwner {
    if matches!(kind, EventWakeKind::Timer) {
        deadline_owner
    } else {
        DeadlineOwner::None
    }
}

/// Record which renderer is live (called once at startup and on any backend swap).
pub fn set_backend_gpu(on: bool) {
    BACKEND_GPU.store(on, Ordering::Relaxed);
}

/// Just the live-renderer flag — the single field of [`snapshot`] that the
/// Settings view needs, without building the ~400-byte `Snapshot` (≈50 atomic
/// loads, three enum decodes and a `now_ns()` monotonic-clock read) and throwing
/// all of it away. `setting_row` asks per ROW, so a Settings page rebuild used to
/// pay that a few dozen times per keystroke/hover for one bool.
#[must_use]
pub fn backend_gpu() -> bool {
    BACKEND_GPU.load(Ordering::Relaxed)
}

/// Zero the measurement-window stats (frame count, maxima, slow count) so a driver
/// can time a SPECIFIC operation: `metrics reset`, run the workload, then `metrics`.
/// Keeps `backend` and legacy momentary `last_*` readings. The redraw-audit
/// `last_pre_present` and drop disposition are measurement-window evidence, so
/// those clear with their counters. Resetting also removes the cold-start spike
/// from maxima, making the worst case reflect steady state.
pub fn reset() {
    FRAMES_PRESENTED.store(0, Ordering::Relaxed);
    MAX_PRESENT_LATENCY_NS.store(0, Ordering::Relaxed);
    MAX_FRAME_RENDER_NS.store(0, Ordering::Relaxed);
    SLOW_FRAMES.store(0, Ordering::Relaxed);
    SYNC_HOLDS_ARMED.store(0, Ordering::Relaxed);
    SYNC_RELEASES_END.store(0, Ordering::Relaxed);
    SYNC_RELEASES_TIMEOUT.store(0, Ordering::Relaxed);
    SHED_TRANSITIONS.store(0, Ordering::Relaxed);
    MAX_INPUT_PRESENT_NS.store(0, Ordering::Relaxed);
    MAX_KEY_WRITE_NS.store(0, Ordering::Relaxed);
    WAKE_HEALS.store(0, Ordering::Relaxed);
    MAX_REDRAW_TOTAL_NS.store(0, Ordering::Relaxed);
    REDRAW_ATTEMPTS.store(0, Ordering::Relaxed);
    REDRAW_EARLY_OUTS.store(0, Ordering::Relaxed);
    REDRAW_SYNC_HOLDS.store(0, Ordering::Relaxed);
    REDRAW_RETRY_GATED.store(0, Ordering::Relaxed);
    PRE_PRESENT_ATTEMPTS.store(0, Ordering::Relaxed);
    LAST_PRE_PRESENT_NS.store(0, Ordering::Relaxed);
    PRE_PRESENT_TOTAL_NS.store(0, Ordering::Relaxed);
    MAX_PRE_PRESENT_NS.store(0, Ordering::Relaxed);
    PRESENT_DROPS.store(0, Ordering::Relaxed);
    LAST_PRESENT_DROP_REASON.store(0, Ordering::Relaxed);
    LAST_PRESENT_DROP_PARKED.store(false, Ordering::Relaxed);
    EVENT_WAKES.store(0, Ordering::Relaxed);
    TIMER_WAKES.store(0, Ordering::Relaxed);
    WAIT_CANCELLED_WAKES.store(0, Ordering::Relaxed);
    POLL_WAKES.store(0, Ordering::Relaxed);
    PAST_DEADLINE_ARMS.store(0, Ordering::Relaxed);
    // Frame-gap window: clear the max AND the previous-present stamp so the idle
    // gap straddling the reset is never counted (the next present starts fresh).
    MAX_FRAME_GAP_NS.store(0, Ordering::Relaxed);
    LAST_PRESENT_STAMP_NS.store(0, Ordering::Relaxed);
    // A stale no-echo stamp must not leak a bogus slice into the fresh window.
    INPUT_STAMP_NS.store(0, Ordering::Relaxed);
    // Likewise a bounds change whose present never came: it must not book its
    // whole pre-reset wait against the first present of the new window.
    RESIZE_STAMP_NS.store(0, Ordering::Relaxed);
    MAX_RESIZE_PRESENT_NS.store(0, Ordering::Relaxed);
    RESIZE_REFLOW_STAMP_NS.store(0, Ordering::Relaxed);
    MAX_RESIZE_REFLOW_NS.store(0, Ordering::Relaxed);
    // MEASUREMENT-WINDOW HONESTY (touch-to-glass audit): every `last_*` that is an
    // OBSERVATION of the window clears with it, so a `0.00` unambiguously means "no
    // sample in this window" instead of silently reprinting the PREVIOUS run's
    // number. Without this, the documented `reset` → drive → read protocol turns a
    // zero-sample run into a passing regression test. True GAUGES (`SYNC_HOLDING`,
    // `PERF_REDUCED`, `BACKEND_GPU`) and the coherent `STARTUP_PRESENT` fact are
    // state, not observations, and deliberately survive.
    OFFSCREEN_RASTERS.store(0, Ordering::Relaxed);
    LAST_OFFSCREEN_RASTER_NS.store(0, Ordering::Relaxed);
    MAX_OFFSCREEN_RASTER_NS.store(0, Ordering::Relaxed);
    LAST_ACQUIRE_WAIT_NS.store(0, Ordering::Relaxed);
    MAX_ACQUIRE_WAIT_NS.store(0, Ordering::Relaxed);
    H_ACQUIRE_WAIT.reset();
    LAST_INPUT_PRESENT_NS.store(0, Ordering::Relaxed);
    LAST_RESIZE_PRESENT_NS.store(0, Ordering::Relaxed);
    LAST_RESIZE_REFLOW_NS.store(0, Ordering::Relaxed);
    LAST_KEY_WRITE_NS.store(0, Ordering::Relaxed);
    LAST_PRESENT_LATENCY_NS.store(0, Ordering::Relaxed);
    LAST_FRAME_RENDER_NS.store(0, Ordering::Relaxed);
    LAST_REDRAW_TOTAL_NS.store(0, Ordering::Relaxed);
    // The distributions are window stats like the maxima they generalize.
    H_INPUT_PRESENT.reset();
    H_PRESENT_LATENCY.reset();
    H_FRAME_RENDER.reset();
    H_KEY_WRITE.reset();
    H_PRE_PRESENT.reset();
    H_RESIZE_PRESENT.reset();
    H_RESIZE_REFLOW.reset();
    // Gauges (`SYNC_HOLDING`, `PERF_REDUCED`), legacy momentary `last_*`
    // readings, and `first_present` (a startup FACT, not a window stat) survive
    // a reset, like `backend`. Redraw-audit last/drop fields intentionally reset
    // above with their diagnostic window.
}

/// A consistent-enough read of the counters for the `metrics` control verb.
#[derive(Clone, Copy)]
pub struct Snapshot {
    pub frames_presented: u64,
    pub last_present_latency_ns: u64,
    pub last_frame_render_ns: u64,
    pub max_present_latency_ns: u64,
    pub max_frame_render_ns: u64,
    pub slow_frames: u64,
    pub backend_gpu: bool,
    pub sync_holds_armed: u64,
    pub sync_releases_end: u64,
    pub sync_releases_timeout: u64,
    pub sync_holding: bool,
    pub perf_reduced: bool,
    pub shed_transitions: u64,
    pub last_input_present_ns: u64,
    pub max_input_present_ns: u64,
    /// UI-thread key->write cost (winit key arrival -> PTY write returned): the
    /// blocking WriteFile + press-path term_locks component inside the end-to-end
    /// `input_present` slice. This isolates writer work; it must not be added to
    /// `input_present` as though the slices were disjoint.
    pub last_key_write_ns: u64,
    pub max_key_write_ns: u64,
    /// Window-bounds change → first frame SUBMITTED at the new size: the
    /// interval a live drag spends showing the previous frame rescaled onto the
    /// new bounds. See [`note_resize_arrival`].
    pub last_resize_present_ns: u64,
    pub max_resize_present_ns: u64,
    /// Window-bounds change → the engine committing the new GRID: the interval the
    /// terminal body spends trailing the window edge. See [`note_grid_committed`].
    pub last_resize_reflow_ns: u64,
    pub max_resize_reflow_ns: u64,
    pub wake_heals: u64,
    pub last_redraw_total_ns: u64,
    pub max_redraw_total_ns: u64,
    pub redraw_attempts: u64,
    pub redraw_early_outs: u64,
    pub redraw_sync_holds: u64,
    pub redraw_retry_gated: u64,
    pub pre_present_attempts: u64,
    pub last_pre_present_ns: u64,
    pub pre_present_total_ns: u64,
    pub max_pre_present_ns: u64,
    pub present_drops: u64,
    pub last_present_drop_reason: PresentDropReason,
    pub last_present_drop_parked: bool,
    pub event_wakes: u64,
    pub timer_wakes: u64,
    pub wait_cancelled_wakes: u64,
    pub poll_wakes: u64,
    pub last_wake_kind: EventWakeKind,
    pub last_wake_owner: DeadlineOwner,
    pub last_wake_late_ns: u64,
    pub last_deadline_owner: DeadlineOwner,
    pub deadline_in_ns: u64,
    pub last_deadline_late_ns: u64,
    pub past_deadline_arms: u64,
    /// Worst present→present gap since reset (0 = fewer than two presents in the
    /// window). The hitch/stutter signal for scrub sweeps — see the static's
    /// note on the reset→drive→read discipline. ARENA-SCROLL's frame-gap number.
    pub max_frame_gap_ns: u64,
    /// GUI entry → first startup-metrics publication point inside the
    /// successful-present finalizer (0 until the first present); see
    /// [`mark_process_start`]. Survives `reset`.
    pub first_present_ns: u64,
    /// Shipped one-binary Rust entry → the same publication point (0 until the
    /// first present, and unavailable in the thin GUI binary); see
    /// [`mark_rust_main_start`]. Survives `reset`.
    pub rust_main_to_first_present_ns: u64,
    /// Exclusive startup-phase schema. `startup_phase_valid` is false until a
    /// complete, ordered one-binary timeline reaches its first successful
    /// present. The immutable phase fact survives `reset`.
    pub startup_phase_schema: u64,
    pub startup_phase_valid: bool,
    pub startup_router_ns: u64,
    pub startup_gui_prepare_ns: u64,
    pub startup_winit_dispatch_ns: u64,
    pub startup_initial_surface_attach_ns: u64,
    pub startup_surface_to_successful_redraw_ns: u64,
    pub startup_successful_compose_ns: u64,
    pub startup_successful_surface_transaction_ns: u64,
    pub startup_successful_finalize_ns: u64,
    /// Exclusive drill-down of `startup_initial_surface_attach_ns`.
    /// `startup_attach_valid` is false until all successful initial-attach
    /// boundaries are ordered and reconcile exactly with their parent phase.
    pub startup_attach_schema: u64,
    pub startup_attach_valid: bool,
    pub startup_attach_dispatch_ns: u64,
    pub startup_attach_prepare_ns: u64,
    pub startup_attach_window_create_ns: u64,
    pub startup_attach_window_setup_ns: u64,
    pub startup_attach_backend_finalize_ns: u64,
    pub startup_attach_chrome_geometry_ns: u64,
    pub startup_attach_surface_create_ns: u64,
    pub startup_attach_finish_ns: u64,
}

/// Read the current counters (lock-free).
#[must_use]
pub fn snapshot() -> Snapshot {
    let deadline_due = LAST_DEADLINE_DUE_NS.load(Ordering::Relaxed);
    let frames_presented = FRAMES_PRESENTED.load(Ordering::Acquire);
    // Read the OnceLock only after acquiring the publication counter. Once a
    // caller observes frame 1, it must also observe the startup sample that
    // record_present initialized before its Release increment.
    let startup = STARTUP_PRESENT.get().copied().unwrap_or_default();
    Snapshot {
        frames_presented,
        last_present_latency_ns: LAST_PRESENT_LATENCY_NS.load(Ordering::Relaxed),
        last_frame_render_ns: LAST_FRAME_RENDER_NS.load(Ordering::Relaxed),
        max_present_latency_ns: MAX_PRESENT_LATENCY_NS.load(Ordering::Relaxed),
        max_frame_render_ns: MAX_FRAME_RENDER_NS.load(Ordering::Relaxed),
        slow_frames: SLOW_FRAMES.load(Ordering::Relaxed),
        backend_gpu: BACKEND_GPU.load(Ordering::Relaxed),
        sync_holds_armed: SYNC_HOLDS_ARMED.load(Ordering::Relaxed),
        sync_releases_end: SYNC_RELEASES_END.load(Ordering::Relaxed),
        sync_releases_timeout: SYNC_RELEASES_TIMEOUT.load(Ordering::Relaxed),
        sync_holding: SYNC_HOLDING.load(Ordering::Relaxed),
        perf_reduced: PERF_REDUCED.load(Ordering::Relaxed),
        shed_transitions: SHED_TRANSITIONS.load(Ordering::Relaxed),
        last_input_present_ns: LAST_INPUT_PRESENT_NS.load(Ordering::Relaxed),
        max_input_present_ns: MAX_INPUT_PRESENT_NS.load(Ordering::Relaxed),
        last_key_write_ns: LAST_KEY_WRITE_NS.load(Ordering::Relaxed),
        max_key_write_ns: MAX_KEY_WRITE_NS.load(Ordering::Relaxed),
        last_resize_present_ns: LAST_RESIZE_PRESENT_NS.load(Ordering::Relaxed),
        max_resize_present_ns: MAX_RESIZE_PRESENT_NS.load(Ordering::Relaxed),
        last_resize_reflow_ns: LAST_RESIZE_REFLOW_NS.load(Ordering::Relaxed),
        max_resize_reflow_ns: MAX_RESIZE_REFLOW_NS.load(Ordering::Relaxed),
        wake_heals: WAKE_HEALS.load(Ordering::Relaxed),
        last_redraw_total_ns: LAST_REDRAW_TOTAL_NS.load(Ordering::Relaxed),
        max_redraw_total_ns: MAX_REDRAW_TOTAL_NS.load(Ordering::Relaxed),
        redraw_attempts: REDRAW_ATTEMPTS.load(Ordering::Relaxed),
        redraw_early_outs: REDRAW_EARLY_OUTS.load(Ordering::Relaxed),
        redraw_sync_holds: REDRAW_SYNC_HOLDS.load(Ordering::Relaxed),
        redraw_retry_gated: REDRAW_RETRY_GATED.load(Ordering::Relaxed),
        pre_present_attempts: PRE_PRESENT_ATTEMPTS.load(Ordering::Relaxed),
        last_pre_present_ns: LAST_PRE_PRESENT_NS.load(Ordering::Relaxed),
        pre_present_total_ns: PRE_PRESENT_TOTAL_NS.load(Ordering::Relaxed),
        max_pre_present_ns: MAX_PRE_PRESENT_NS.load(Ordering::Relaxed),
        present_drops: PRESENT_DROPS.load(Ordering::Relaxed),
        last_present_drop_reason: PresentDropReason::from_raw(
            LAST_PRESENT_DROP_REASON.load(Ordering::Relaxed),
        ),
        last_present_drop_parked: LAST_PRESENT_DROP_PARKED.load(Ordering::Relaxed),
        event_wakes: EVENT_WAKES.load(Ordering::Relaxed),
        timer_wakes: TIMER_WAKES.load(Ordering::Relaxed),
        wait_cancelled_wakes: WAIT_CANCELLED_WAKES.load(Ordering::Relaxed),
        poll_wakes: POLL_WAKES.load(Ordering::Relaxed),
        last_wake_kind: EventWakeKind::from_raw(LAST_WAKE_KIND.load(Ordering::Relaxed)),
        last_wake_owner: DeadlineOwner::from_raw(LAST_WAKE_OWNER.load(Ordering::Relaxed)),
        last_wake_late_ns: LAST_WAKE_LATE_NS.load(Ordering::Relaxed),
        last_deadline_owner: DeadlineOwner::from_raw(LAST_DEADLINE_OWNER.load(Ordering::Relaxed)),
        deadline_in_ns: if deadline_due == 0 {
            0
        } else {
            deadline_due.saturating_sub(now_ns())
        },
        last_deadline_late_ns: LAST_DEADLINE_LATE_NS.load(Ordering::Relaxed),
        past_deadline_arms: PAST_DEADLINE_ARMS.load(Ordering::Relaxed),
        max_frame_gap_ns: MAX_FRAME_GAP_NS.load(Ordering::Relaxed),
        first_present_ns: startup.gui_entry_ns,
        rust_main_to_first_present_ns: startup.rust_main_ns,
        startup_phase_schema: STARTUP_PHASE_SCHEMA,
        startup_phase_valid: startup.phases.valid,
        startup_router_ns: startup.phases.router_ns,
        startup_gui_prepare_ns: startup.phases.gui_prepare_ns,
        startup_winit_dispatch_ns: startup.phases.winit_dispatch_ns,
        startup_initial_surface_attach_ns: startup.phases.initial_surface_attach_ns,
        startup_surface_to_successful_redraw_ns: startup.phases.surface_to_successful_redraw_ns,
        startup_successful_compose_ns: startup.phases.successful_compose_ns,
        startup_successful_surface_transaction_ns: startup.phases.successful_surface_transaction_ns,
        startup_successful_finalize_ns: startup.phases.successful_finalize_ns,
        startup_attach_schema: STARTUP_ATTACH_SCHEMA,
        startup_attach_valid: startup.attach.valid,
        startup_attach_dispatch_ns: startup.attach.dispatch_ns,
        startup_attach_prepare_ns: startup.attach.prepare_ns,
        startup_attach_window_create_ns: startup.attach.window_create_ns,
        startup_attach_window_setup_ns: startup.attach.window_setup_ns,
        startup_attach_backend_finalize_ns: startup.attach.backend_finalize_ns,
        startup_attach_chrome_geometry_ns: startup.attach.chrome_geometry_ns,
        startup_attach_surface_create_ns: startup.attach.surface_create_ns,
        startup_attach_finish_ns: startup.attach.finish_ns,
    }
}

#[cfg(test)]
mod startup_phase_tests {
    use super::*;

    fn timeline() -> (StartupMilestones, StartupPresentTiming, Instant) {
        let rust_main = Instant::now();
        let gui_entry = rust_main + std::time::Duration::from_millis(1);
        let gui_ready = gui_entry + std::time::Duration::from_millis(2);
        let winit_resumed = gui_ready + std::time::Duration::from_millis(3);
        let surface_ready = winit_resumed + std::time::Duration::from_millis(4);
        let frame_started = surface_ready + std::time::Duration::from_millis(5);
        let pre_present = frame_started + std::time::Duration::from_millis(6);
        let surface_return = pre_present + std::time::Duration::from_millis(7);
        let published_at = surface_return + std::time::Duration::from_millis(8);
        (
            StartupMilestones {
                rust_main: Some(rust_main),
                gui_entry: Some(gui_entry),
                gui_ready: Some(gui_ready),
                winit_resumed: Some(winit_resumed),
                surface_ready: Some(surface_ready),
            },
            StartupPresentTiming::new(frame_started, pre_present, surface_return),
            published_at,
        )
    }

    fn attach_timeline() -> (Instant, StartupAttachMilestones, Instant) {
        let winit_resumed = Instant::now();
        let points = std::array::from_fn(|index| {
            winit_resumed + std::time::Duration::from_millis((index + 1) as u64)
        });
        let surface_ready = winit_resumed + std::time::Duration::from_millis(8);
        (
            winit_resumed,
            StartupAttachMilestones::new(points),
            surface_ready,
        )
    }

    #[test]
    fn initial_attach_milestones_are_parallel_first_write_wins() {
        let slot = std::sync::Arc::new(OnceLock::new());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let base = Instant::now();
        let mut workers = Vec::new();
        for offset in [1_u64, 2] {
            let slot = std::sync::Arc::clone(&slot);
            let barrier = std::sync::Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let point = base + std::time::Duration::from_millis(offset);
                barrier.wait();
                record_initial_attach_milestones_once(
                    &slot,
                    StartupAttachMilestones::new([point; 7]),
                )
            }));
        }
        barrier.wait();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().expect("milestone writer"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1, "exactly one concurrent candidate may publish");

        let stored = slot.get().expect("one candidate published");
        let replacement = base + std::time::Duration::from_millis(3);
        assert!(
            !record_initial_attach_milestones_once(
                &slot,
                StartupAttachMilestones::new([replacement; 7]),
            ),
            "negative control: a later window cannot replace startup milestones"
        );
        assert_ne!(
            stored.points[0], replacement,
            "the rejected candidate must not mutate the admitted timeline"
        );
    }

    #[test]
    fn startup_phases_are_an_exact_exclusive_partition() {
        let (milestones, timing, published_at) = timeline();
        let phases = derive_startup_phases(milestones, timing, published_at);
        assert!(phases.valid);
        assert_eq!(
            [
                phases.router_ns,
                phases.gui_prepare_ns,
                phases.winit_dispatch_ns,
                phases.initial_surface_attach_ns,
                phases.surface_to_successful_redraw_ns,
                phases.successful_compose_ns,
                phases.successful_surface_transaction_ns,
                phases.successful_finalize_ns,
            ],
            [
                1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000, 6_000_000, 7_000_000,
                8_000_000,
            ]
        );
        assert_eq!(phases.total_ns(), Some(36_000_000));
        assert_eq!(
            phases
                .total_ns()
                .and_then(|total| total.checked_sub(phases.router_ns)),
            Some(35_000_000)
        );
    }

    #[test]
    fn startup_phases_fail_closed_on_missing_or_out_of_order_milestones() {
        let (_, timing, published_at) = timeline();
        assert_eq!(
            derive_startup_phases(StartupMilestones::default(), timing, published_at),
            StartupPhaseSample::default()
        );

        let (milestones, timing, published_at) = timeline();
        let reversed = StartupPresentTiming::new(
            timing.pre_present,
            timing.frame_started,
            timing.surface_return,
        );
        assert_eq!(
            derive_startup_phases(milestones, reversed, published_at),
            StartupPhaseSample::default()
        );
    }

    #[test]
    fn startup_phase_sum_overflow_fails_closed() {
        let phases = StartupPhaseSample {
            valid: true,
            router_ns: u64::MAX,
            successful_finalize_ns: 1,
            ..StartupPhaseSample::default()
        };
        assert_eq!(phases.total_ns(), None);
    }

    #[test]
    fn startup_attach_is_an_exact_exclusive_parent_partition() {
        let (winit_resumed, milestones, surface_ready) = attach_timeline();
        let attach =
            derive_startup_attach(Some(winit_resumed), Some(milestones), Some(surface_ready));
        assert!(attach.valid);
        assert_eq!(
            [
                attach.dispatch_ns,
                attach.prepare_ns,
                attach.window_create_ns,
                attach.window_setup_ns,
                attach.backend_finalize_ns,
                attach.chrome_geometry_ns,
                attach.surface_create_ns,
                attach.finish_ns,
            ],
            [1_000_000; 8]
        );
        assert_eq!(attach.total_ns(), Some(8_000_000));
        assert_eq!(
            attach.total_ns(),
            duration_ns(winit_resumed, surface_ready),
            "the drill-down must equal its initial-surface-attach parent"
        );
    }

    #[test]
    fn startup_attach_fails_closed_on_missing_reordered_or_overflowing_data() {
        let (winit_resumed, milestones, surface_ready) = attach_timeline();
        assert_eq!(
            derive_startup_attach(Some(winit_resumed), None, Some(surface_ready)),
            StartupAttachSample::default()
        );
        let mut reversed = milestones;
        reversed.points.swap(3, 4);
        assert_eq!(
            derive_startup_attach(Some(winit_resumed), Some(reversed), Some(surface_ready)),
            StartupAttachSample::default()
        );
        let overflow = StartupAttachSample {
            valid: true,
            dispatch_ns: u64::MAX,
            finish_ns: 1,
            ..StartupAttachSample::default()
        };
        assert_eq!(overflow.total_ns(), None);
    }

    #[test]
    fn failed_attempt_stamp_cannot_replace_the_successful_redraw_boundary() {
        let (milestones, timing, published_at) = timeline();
        let failed_attempt =
            milestones.surface_ready.unwrap() + std::time::Duration::from_millis(1);
        assert!(failed_attempt < timing.frame_started);
        let phases = derive_startup_phases(milestones, timing, published_at);
        assert!(phases.valid);
        assert_eq!(phases.surface_to_successful_redraw_ns, 5_000_000);

        let wrong_projection = derive_startup_phases(
            milestones,
            StartupPresentTiming::new(failed_attempt, timing.pre_present, timing.surface_return),
            published_at,
        );
        assert!(wrong_projection.valid);
        assert_eq!(wrong_projection.surface_to_successful_redraw_ns, 1_000_000);
        assert_ne!(
            wrong_projection, phases,
            "negative control: selecting the failed attempt must change the partition"
        );
    }

    #[test]
    fn release_published_frame_exposes_the_same_immutable_phase_sample() {
        let sample = std::sync::Arc::new(OnceLock::new());
        let frames = std::sync::Arc::new(AtomicU64::new(0));
        let expected = StartupPresentSample {
            gui_entry_ns: 35,
            rust_main_ns: 36,
            phases: StartupPhaseSample {
                valid: true,
                router_ns: 1,
                successful_finalize_ns: 35,
                ..StartupPhaseSample::default()
            },
            attach: StartupAttachSample::default(),
        };
        let writer_sample = std::sync::Arc::clone(&sample);
        let writer_frames = std::sync::Arc::clone(&frames);
        let writer = std::thread::spawn(move || {
            writer_sample.set(expected).unwrap();
            writer_frames.store(1, Ordering::Release);
        });
        while frames.load(Ordering::Acquire) == 0 {
            std::thread::yield_now();
        }
        assert_eq!(sample.get().copied(), Some(expected));
        writer.join().unwrap();
    }

    #[test]
    fn reset_preserves_the_immutable_startup_fact() {
        let before = *STARTUP_PRESENT.get_or_init(|| StartupPresentSample {
            gui_entry_ns: 35,
            rust_main_ns: 36,
            phases: StartupPhaseSample {
                valid: true,
                router_ns: 1,
                successful_finalize_ns: 35,
                ..StartupPhaseSample::default()
            },
            attach: StartupAttachSample::default(),
        });
        reset();
        assert_eq!(STARTUP_PRESENT.get().copied(), Some(before));
    }

    #[test]
    fn phase_derivation_conforms_to_the_derived_publication_gate() {
        // The derived model intentionally covers only interval completeness.
        // Ordering and exact arithmetic remain real host obligations exercised
        // by the reordered controls below and the exact-partition tests above.
        let model = aterm_spec::derive::startup_phase_publication_model();
        let (milestones, timing, published_at) = timeline();
        let mut incomplete_milestones = milestones;
        incomplete_milestones.surface_ready = None;

        let mut state = model.init_state();
        for _ in 0..7 {
            assert!(model.fire("Step", &mut state));
        }
        let host_valid = derive_startup_phases(incomplete_milestones, timing, published_at).valid;
        assert_eq!(
            !model.successors("Publish", &state).is_empty(),
            host_valid,
            "Tier-1: the real missing-milestone decision matches phase 7"
        );

        assert!(model.fire("Step", &mut state));
        let host_valid = derive_startup_phases(milestones, timing, published_at).valid;
        assert_eq!(
            !model.successors("Publish", &state).is_empty(),
            host_valid,
            "Tier-1: the complete real partition matches phase 8"
        );
        assert!(model.fire("Publish", &mut state));

        let reversed = StartupPresentTiming::new(
            timing.pre_present,
            timing.frame_started,
            timing.surface_return,
        );
        assert!(
            !derive_startup_phases(milestones, reversed, published_at).valid,
            "negative control: real out-of-order stamps must fail closed"
        );

        let attach_model = aterm_spec::derive::startup_phase_publication_model();
        let (winit_resumed, attach_milestones, surface_ready) = attach_timeline();
        let mut attach_state = attach_model.init_state();
        for _ in 0..7 {
            assert!(attach_model.fire("Step", &mut attach_state));
        }
        let incomplete_attach =
            derive_startup_attach(Some(winit_resumed), Some(attach_milestones), None).valid;
        assert_eq!(
            !attach_model.successors("Publish", &attach_state).is_empty(),
            incomplete_attach,
            "Tier-1: the missing surface-ready boundary matches attach phase 7"
        );
        assert!(attach_model.fire("Step", &mut attach_state));
        let complete_attach = derive_startup_attach(
            Some(winit_resumed),
            Some(attach_milestones),
            Some(surface_ready),
        )
        .valid;
        assert_eq!(
            !attach_model.successors("Publish", &attach_state).is_empty(),
            complete_attach,
            "Tier-1: the complete attach partition matches phase 8"
        );
        let mut reversed_attach = attach_milestones;
        reversed_attach.points.swap(1, 2);
        assert!(
            !derive_startup_attach(
                Some(winit_resumed),
                Some(reversed_attach),
                Some(surface_ready),
            )
            .valid,
            "negative control: an out-of-order attach stamp must fail closed"
        );
    }
}

#[cfg(test)]
mod histogram_tests {
    use super::*;

    /// Every representative value lands in a bucket whose edges contain it,
    /// and indices are monotone in the value — across the linear range, every
    /// octave boundary, and the saturation clamp.
    #[test]
    fn bucket_index_is_monotone_and_edges_contain_values() {
        let mut last_idx = 0usize;
        let mut v = 1u64;
        // Sweep the FAITHFUL range only (< 2^36); beyond it values clamp into
        // the top bucket and containment intentionally stops holding.
        while v < (1u64 << 36) {
            let idx = Histogram::index(v);
            assert!(
                idx >= last_idx,
                "index regressed at v={v}: {idx} < {last_idx}"
            );
            assert!(
                v < Histogram::upper_edge(idx),
                "v={v} not below its bucket's exclusive upper edge {}",
                Histogram::upper_edge(idx)
            );
            if idx > 0 {
                assert!(
                    v >= Histogram::upper_edge(idx - 1),
                    "v={v} below the previous bucket's exclusive upper edge"
                );
            }
            last_idx = idx;
            v = v.saturating_mul(2) - v / 3; // dense-ish sweep, hits odd offsets
        }
        assert_eq!(
            Histogram::index(u64::MAX),
            H_BUCKETS - 1,
            "saturates at the top"
        );
    }

    /// Percentiles on a known distribution: conservative (>= true value) and
    /// within one bucket's relative error of it.
    #[test]
    fn percentiles_are_conservative_and_tight() {
        let h = Histogram::new();
        assert_eq!(h.percentile(0.5), None, "empty histogram has no percentile");
        // 1..=1000 µs, one sample each: true p50=500µs, p99=990µs.
        for us in 1..=1000u64 {
            h.record(us * 1000);
        }
        assert_eq!(h.count(), 1000);
        for (q, true_ns) in [(0.5, 500_000u64), (0.95, 950_000), (0.99, 990_000)] {
            let got = h.percentile(q).unwrap();
            assert!(
                got >= true_ns,
                "p{q} {got} below true {true_ns} — not conservative"
            );
            assert!(
                got <= true_ns + true_ns / 4,
                "p{q} {got} more than 25% above true {true_ns} — bucket too coarse"
            );
        }
        // p at the extremes: clamped, never panics, single-sample sanity.
        let one = Histogram::new();
        one.record(7_777);
        assert_eq!(one.percentile(0.5), one.percentile(1.0));
        assert!(one.percentile(0.0001).unwrap() >= 7_777);
    }

    /// reset() empties the distribution; recording after reset works.
    #[test]
    fn histogram_reset_clears_and_reuses() {
        let h = Histogram::new();
        h.record(1_000_000);
        assert_eq!(h.count(), 1);
        h.reset();
        assert_eq!(h.count(), 0);
        assert_eq!(h.percentile(0.99), None);
        h.record(2_000_000);
        assert!(h.percentile(0.5).unwrap() >= 2_000_000);
    }

    #[test]
    fn only_timer_wakes_inherit_the_selected_deadline_owner() {
        for kind in [
            EventWakeKind::None,
            EventWakeKind::Init,
            EventWakeKind::Poll,
            EventWakeKind::WaitCancelled,
        ] {
            assert_eq!(
                wake_owner(kind, DeadlineOwner::Predictor),
                DeadlineOwner::None,
                "{kind:?} is not caused by the interrupted deadline"
            );
        }
        assert_eq!(
            wake_owner(EventWakeKind::Timer, DeadlineOwner::PresentRetry),
            DeadlineOwner::PresentRetry
        );
        assert_eq!(
            DeadlineOwner::from_raw(DeadlineOwner::TitleDrift as u64),
            DeadlineOwner::TitleDrift
        );
        assert_eq!(DeadlineOwner::TitleDrift.as_str(), "title_drift");
        assert_eq!(
            DeadlineOwner::from_raw(DeadlineOwner::KittyTenure as u64),
            DeadlineOwner::KittyTenure
        );
        assert_eq!(DeadlineOwner::KittyTenure.as_str(), "kitty_tenure");
    }
}
