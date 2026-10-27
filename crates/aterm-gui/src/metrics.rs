// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-global render/latency counters, read over the control socket by the `metrics` verb so a driver can measure responsiveness and detect lag directly.

pub(crate) mod instrument;
#[cfg(all(unix, any(test, debug_assertions)))]
mod restore_experiment;
pub(crate) mod startup;
pub(crate) mod wakes;
pub(crate) mod wire;

use crate::deadline_heal::Heal;
use crate::label_enum::label_enum;
pub(crate) use instrument::Histogram;
use instrument::{
    Baselined, Counter, Dist, HistCopy, Last, LastFlag, LastLabel, Max, MaxBy, Peak, Stamp, Tally,
    Windowed, window_ledger,
};
#[cfg(all(unix, any(test, debug_assertions)))]
pub(crate) use restore_experiment::{RestoreExperiment, record_restore_experiment};
#[cfg(unix)]
pub(crate) use startup::claim_timeline_log_text;
#[cfg(all(unix, any(test, debug_assertions)))]
pub(crate) use startup::record_keeper_seam;
pub(crate) use startup::{
    ClaimStamp, StartupAttachMilestones, StartupPresentTiming, StartupWorkerLegs, leg_elapsed_ns,
    mark_backend_worker_done, mark_backend_worker_spawn, mark_first_window_visible,
    mark_first_winit_resumed, mark_gui_ready_for_winit, mark_initial_surface_ready,
    mark_process_start, mark_rust_main_start, record_backend_worker_legs,
    record_initial_attach_milestones, stamp_claim,
};
#[cfg(unix)]
pub(crate) use startup::{
    keeper_seam, mark_keeper_hello, mark_keeper_imported, mark_keeper_placed,
    record_keeper_admitted,
};
#[cfg(target_os = "macos")]
pub(crate) use startup::{mark_handoff_claimed, process_main_start};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The process's metrics, written by the App and read by the control thread's `metrics` verb.
pub(crate) static METRICS: Metrics = Metrics::new();

/// A metrics root: the measurement window, the live state beside it, and the window's epoch.
pub(crate) struct Metrics {
    pub(crate) window: Window,
    pub(crate) live: Live,
    epoch: AtomicU64,
}

impl Metrics {
    pub(crate) const fn new() -> Self {
        Self {
            window: Window::new(),
            live: Live::new(),
            epoch: AtomicU64::new(0),
        }
    }

    /// Open a new measurement window.
    pub(crate) fn reset(&self) {
        self.window.gpu.sample_fx_totals();
        self.window.reset();
        self.epoch.store(now_ns(), Ordering::Relaxed);
    }
}

window_ledger! {
    /// The measurement window, by ledger.
    pub(crate) struct Window {
        pub(crate) present: PresentLedger,
        pub(crate) input: InputLedger,
        pub(crate) redraw: RedrawLedger,
        pub(crate) glass: GlassLedger,
        pub(crate) sched: SchedLedger,
        pub(crate) sync: SyncLedger,
        pub(crate) gpu_scope: GpuScopeLedger,
        pub(crate) gpu: GpuLedger,
        pub(crate) command: CommandLedger,
    }
}

window_ledger! {
    /// What the application presents measure, written on the UI thread ([`Metrics::record_present_at`], the resize stamps, the offscreen rasters, the capture episodes).
    pub(crate) struct PresentLedger {
        /// Successful application presents (`frames`).
        pub(crate) frames: Counter,
        /// Output→application-present-return on glass (`present_latency`).
        pub(crate) latency: Dist,
        /// When the worst `latency` was booked (`now_ns`), and the gap since its window's previous present.
        pub(crate) latency_at: Last,
        pub(crate) latency_gap: Last,
        pub(crate) tainted: Dist,
        /// Output→present samples diverted to `tainted`.
        pub(crate) tainted_samples: Counter,
        /// `video` capture episodes opened in the window.
        pub(crate) capture_episodes: Counter,
        /// Output→present samples a still capture's barrier forced (`image`, `window`, the SIGUSR1 snapshot).
        pub(crate) capture_presents: Counter,
        /// Causal frame CPU work: compose plus CPU raster/copy, or encoding GPU commands and calling `queue.submit`.
        pub(crate) render: Dist,
        /// Frames whose `render` exceeded [`SLOW_FRAME_THRESHOLD_NS`].
        pub(crate) slow: Counter,
        pub(crate) frame_gap: Max,
        pub(crate) resize_stamp: Stamp,
        pub(crate) resize_present: Dist,
        pub(crate) reflow_stamp: Stamp,
        pub(crate) resize_reflow: Dist,
        /// OFFSCREEN rasterizations (introspection `image`/`window`/`snapshot`).
        pub(crate) offscreen_rasters: Counter,
        pub(crate) offscreen: Peak,
    }
}

window_ledger! {
    /// What input measures: the keystroke slices, the input legs the typing audit named as
    /// unmeasured, and the refusals of the input contract; and the costs printed beside them
    /// (menu-bar glance, need publication, transcript holds).
    pub(crate) struct InputLedger {
        pub(crate) input: Dist,
        /// When the worst `input` slice was booked (`now_ns`).
        pub(crate) input_at: Last,
        pub(crate) session_stamp: Stamp,
        /// Keystrokes a full edge ring could not hold (see [`INPUT_EDGE_RING`]).
        pub(crate) edges_dropped: Counter,
        /// Of the slices in `input`, those a PREDICTION closed.
        pub(crate) predicted: Counter,
        /// Hardware-key arrival → completed PTY write (`key_write`).
        pub(crate) key_write: Dist,
        /// The NSEvent-QUEUE RESIDENCE that `key_write` is BACKDATED by: how long each hardware key sat in the OS event queue before the loop dispatched it.
        pub(crate) key_queue: Dist,
        pub(crate) out_req: Dist,
        pub(crate) ime: Histogram,
        pub(crate) ime_cancelled: Counter,
        /// A Rewind step taken on the main thread → the first present showing its fold.
        pub(crate) rewind_step: Histogram,
        /// A Rewind step taken on the main thread → its fold shown and its redraw asked for (a headless instance presents nothing).
        pub(crate) rewind_shown: Histogram,
        /// The most output a step's fold applied past the fork or keyframe it started from (`max_rewind_fold_bytes`).
        pub(crate) rewind_folded: Max,
        /// Input REFUSED because its session's input queue was full: the program has stopped reading and the key (or paste) was refused.
        pub(crate) refused: Counter,
        /// Terminal query REPLIES dropped at submission (`spawn::ReplyLane::submit`).
        pub(crate) reply_dropped: Counter,
        pub(crate) status_glance: Histogram,
        pub(crate) status_glance_max: Max,
        pub(crate) need_publish: Histogram,
        pub(crate) need_publish_max: Max,
        pub(crate) tx_chunk: Histogram,
        pub(crate) tx_last: Histogram,
        pub(crate) tx_hold: Max,
    }
}

window_ledger! {
    /// What a redraw pass measures before and around its present.
    pub(crate) struct RedrawLedger {
        /// Whole-redraw wall time (redraw entry → present done or failed).
        pub(crate) total: Peak,
        pub(crate) attempts: Counter,
        pub(crate) early_outs: Counter,
        pub(crate) sync_holds: Counter,
        pub(crate) retry_gated: Counter,
        pub(crate) refills_scoped: Counter,
        pub(crate) refills_full: Counter,
        pub(crate) refills_skipped: Counter,
        pub(crate) refill_cause: Tally<{ aterm_core::render::FullRefillCause::COUNT }>,
        pub(crate) pet_world_observations: Counter,
        pub(crate) pet_world_cells: Counter,
        /// Cursor-effect ticks whose `glow_under` stream was non-empty: that stream is the rainbow ribbon body's own (with its jump zooms, and water's wake), so it reads 0 under the `off` style, whose momentum glow still lights `glow_add`.
        pub(crate) fx_under_frames: Counter,
        /// Every effect stream's length in the latest present's final composed input, whoever produced it, on either renderer ([`FX_STREAMS`] order).
        pub(crate) fx_streams: [Last; FX_STREAMS.len()] =
            [const { Last::new() }; FX_STREAMS.len()],
        /// Redraw entry → the surface-acquire seam, including terminal grid extraction.
        pub(crate) pre_present: Dist,
        pub(crate) pre_present_attempts: Counter,
        pub(crate) pre_present_total: Counter,
        /// Swapchain-acquire (`nextDrawable`) WAIT.
        pub(crate) acquire_wait: Dist,
        /// Drawable-acquire QUEUE: a drawable request admitted → the `aterm-drawable-acquire` worker SCHEDULED to start it, a different cause from `acquire_wait`.
        pub(crate) acquire_queue: Dist,
        /// Armed-Metal GPU park: the UI thread blocked in `waitUntilCompleted` (see [`note_gpu_park`]).
        pub(crate) gpu_park: Peak,
        /// Surface transactions that failed after a redraw reached the present seam, and the last one's disposition (a `PresentDropReason` index and whether the retry scheduler parked it).
        pub(crate) drops: Counter,
        pub(crate) drop_reason: LastLabel,
        pub(crate) drop_parked: LastFlag,
        /// UI-thread TERMINAL-MUTEX wait, per acquiring site ([`TermWaitSite`]).
        pub(crate) term_wait: [Histogram; TermWaitSite::COUNT] =
            [const { Histogram::new() }; TermWaitSite::COUNT],
        pub(crate) term_wait_max: [Max; TermWaitSite::COUNT] =
            [const { Max::new() }; TermWaitSite::COUNT],
    }
}

window_ledger! {
    /// PRESENT → GLASS, fed on a framework thread by the sink installed in [`install_present_glass_sink`] (lock-free: bucket increments and `fetch_max`).
    pub(crate) struct GlassLedger {
        /// `presentDrawable:` registration → the drawable's `presentedTime`.
        pub(crate) latency: Dist,
        /// Drawables never shown.
        pub(crate) skipped: Counter,
        /// Per-tag ledgers (`aterm_gpu::present_glass::PresentTag`).
        pub(crate) shown_by: Tally<{ aterm_gpu::present_glass::PresentTag::COUNT }>,
        pub(crate) skipped_by: Tally<{ aterm_gpu::present_glass::PresentTag::COUNT }>,
    }
}

window_ledger! {
    /// The EVENT-LOOP scheduler audit.
    pub(crate) struct SchedLedger {
        pub(crate) event_wakes: Counter,
        pub(crate) timer_wakes: Counter,
        pub(crate) wait_cancelled_wakes: Counter,
        pub(crate) poll_wakes: Counter,
        pub(crate) past_arms: Counter,
        pub(crate) arms_by_owner: Tally<{ DeadlineOwner::COUNT }>,
        pub(crate) past_arms_by_owner: Tally<{ DeadlineOwner::COUNT }>,
        pub(crate) fires_by_owner: Tally<{ DeadlineOwner::COUNT }>,
        pub(crate) stale_arm_heals: Counter,
        pub(crate) streak_heals_by_owner: Tally<{ DeadlineOwner::COUNT }>,
        pub(crate) wake_late: MaxBy,
        pub(crate) deadline_late: MaxBy,
    }
}

window_ledger! {
    /// Synchronized-output holds, the load-shed latch's edges and the output wake's losses.
    pub(crate) struct SyncLedger {
        pub(crate) holds_armed: Counter,
        pub(crate) releases_end: Counter,
        pub(crate) releases_timeout: Counter,
        /// Edges of the `perf_reduced` load-shed latch.
        pub(crate) shed_transitions: Counter,
        pub(crate) wake_heals: Counter,
        pub(crate) output_wake_late: Counter,
        pub(crate) output_wake_reposts: Counter,
    }
}

window_ledger! {
    /// THE GPU REPAINT SCOPE (responsiveness audit item 5).
    pub(crate) struct GpuScopeLedger {
        /// Presents that took the scissored dirty-row path.
        pub(crate) scissor_taken: Baselined,
        pub(crate) full_repaints: Baselined,
        /// Rigid scrolls the band blit rescued (a subset of the scissored ones).
        pub(crate) scroll_rescues: Baselined,
    }
}

window_ledger! {
    /// THE DEVICE'S OWN TIME, AND THE FRAME IT RAN.
    pub(crate) struct GpuLedger {
        /// Device busy time per present.
        pub(crate) time: Dist,
        /// How many parts the latest `time` sample summed.
        pub(crate) time_parts: Last,
        /// CPU encode per GPU present, and its latest sample.
        pub(crate) encode: Histogram,
        pub(crate) encode_last: Last,
        /// Render passes the latest offscreen plan opened.
        pub(crate) passes: Last,
        /// Instances the latest frame built, per stream (`aterm_gpu::FrameShape::STREAMS` order).
        pub(crate) instances: [Last; aterm_gpu::FrameShape::STREAMS.len()] =
            [const { Last::new() }; aterm_gpu::FrameShape::STREAMS.len()],
        /// Instance bytes the latest frame built for staging.
        pub(crate) instance_bytes: Last,
        /// CPU time building the latest frame's instances, and staging them.
        pub(crate) build: Last,
        pub(crate) stage: Last,
        /// The pass-economy ablation the latest frame ran under (a dev seam), as its [`ablation_code`].
        pub(crate) unorm_ablation: LastLabel,
        /// Where the latest frame drew its Unorm-law light, as its [`route_code`].
        pub(crate) unorm_route: LastLabel,
        /// The device's framebuffer-fetch verdict as of the latest frame, as its [`probe_code`].
        pub(crate) fetch_probe: LastLabel,
        /// The fx instances the arm staged in the latest frame, and the fx bytes it uploaded (instances, palette, atlas texels).
        pub(crate) fx_instances: Last,
        pub(crate) fx_upload_bytes: Last,
        /// The fx sprite items the latest frame refused, drawing nothing.
        pub(crate) fx_refused: Last,
        /// aterm-core's fx layer seals and payload deep copies (`aterm_core::render::fx_layer_builds`, `fx_payload_copies`) since the reset.
        pub(crate) fx_layer_builds: Baselined,
        pub(crate) fx_payload_copies: Baselined,
    }
}

window_ledger! {
    /// THE CARD ENGINE'S PER-COMMAND COST (card engine §3.3, §8's fork loop).
    pub(crate) struct CommandLedger {
        /// Process events the gathers drained (a fork, an exec, an exit each), and the gathers' time inside those drains.
        pub(crate) proc_events: Counter,
        pub(crate) proc_drain_ns: Counter,
        /// The process events that became foreground edges.
        pub(crate) exec_edges: Counter,
        /// Readers on macOS whose watch could not be made (no kqueue: `EMFILE`).
        pub(crate) procwatch_fallbacks: Counter,
        /// Children past a watch's cap, seen only by sampling.
        pub(crate) procwatch_overflow: Counter,
        /// The engine fence's cuts (`Terminal::note_holder_cut`).
        pub(crate) holder_cuts: Counter,
        /// Foreground handbacks run.
        pub(crate) fg_handbacks: Counter,
        /// The parse stages' slice holds, those that found the term lock taken, and how long those waited.
        pub(crate) parse_holds: Counter,
        pub(crate) parse_lock_waits: Counter,
        pub(crate) parse_lock_wait_ns: Counter,
        /// The UI thread's status sweeps that visit sessions (the deadline gate's O(1) no-op is none), and their time.
        pub(crate) status_sweeps: Counter,
        pub(crate) status_sweep_ns: Counter,
    }
}

/// State that describes the process NOW rather than the measurement window.
pub(crate) struct Live {
    pub(crate) backend_gpu: AtomicBool,
    pub(crate) perf_reduced: AtomicBool,
    pub(crate) sync_holding_windows: AtomicU64,
    pub(crate) capture_depth: AtomicU64,
    pub(crate) taint_until: AtomicU64,
    pub(crate) fx_glow_quads_last: AtomicU64,
    pub(crate) fx_under_quads_last: AtomicU64,
    pub(crate) fx_trail_cells_last: AtomicU64,
    pub(crate) last_wake_kind: AtomicU64,
    pub(crate) last_wake_owner: AtomicU64,
    pub(crate) last_wake_late: AtomicU64,
    pub(crate) last_deadline_owner: AtomicU64,
    pub(crate) last_deadline_due: AtomicU64,
    pub(crate) last_deadline_late: AtomicU64,
    pub(crate) lat_key: AtomicU64,
    pub(crate) handoff_input_dropped: AtomicU64,
    pub(crate) handoff_input_incoherent: AtomicU64,
    pub(crate) pty_sessions: AtomicUsize,
    pub(crate) spare_ptys: AtomicUsize,
}

impl Live {
    const fn new() -> Self {
        Self {
            backend_gpu: AtomicBool::new(false),
            perf_reduced: AtomicBool::new(false),
            sync_holding_windows: AtomicU64::new(0),
            capture_depth: AtomicU64::new(0),
            taint_until: AtomicU64::new(0),
            fx_glow_quads_last: AtomicU64::new(0),
            fx_under_quads_last: AtomicU64::new(0),
            fx_trail_cells_last: AtomicU64::new(0),
            last_wake_kind: AtomicU64::new(0),
            last_wake_owner: AtomicU64::new(0),
            last_wake_late: AtomicU64::new(0),
            last_deadline_owner: AtomicU64::new(0),
            last_deadline_due: AtomicU64::new(0),
            last_deadline_late: AtomicU64::new(0),
            lat_key: AtomicU64::new(0),
            handoff_input_dropped: AtomicU64::new(0),
            handoff_input_incoherent: AtomicU64::new(0),
            pty_sessions: AtomicUsize::new(0),
            spare_ptys: AtomicUsize::new(0),
        }
    }
}

const PRESENT_TAINT_TAIL_NS: u64 = 1_000_000_000;

const INPUT_SLICE_CAP_NS: u64 = 5_000_000_000;

const RESIZE_SLICE_CAP_NS: u64 = 2_000_000_000;

#[cfg(all(unix, any(test, feature = "bench-support"), debug_assertions))]
thread_local! {
    /// The instant this thread's host clock reads are pinned to ([`pin_host_clock`]).
    static HOST_CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Pin (or, with `None`, release) this thread's host clock ([`host_now`]): `lab trail-diff`'s
/// dump (`crate::trail_rig`) drives every frame and event at its own injected instant.
#[cfg(all(unix, any(test, feature = "bench-support"), debug_assertions))]
pub(crate) fn pin_host_clock(at: Option<Instant>) {
    HOST_CLOCK.with(|clock| clock.set(at));
}

/// Whether this thread's host clock is pinned ([`pin_host_clock`]): its frames and events run at
/// injected instants, so a cost measured on the wall clock belongs to none of them. Always `false`
/// outside a debug test or `bench-support` build.
#[inline]
pub(crate) fn host_clock_pinned() -> bool {
    #[cfg(all(unix, any(test, feature = "bench-support"), debug_assertions))]
    if HOST_CLOCK.with(std::cell::Cell::get).is_some() {
        return true;
    }
    false
}

/// Now: the host's clock read where no frame or event instant reaches the code (this clock's
/// [`now_ns`], the input path's, a window's focus, the resize throttle, the trail curtain a
/// replaced front or a grid resize draws, the cursor effects' owner fence and scroll seam, the
/// stats surfaces' pointer rest, the instant a present reached glass), or the instant
/// `pin_host_clock` pinned on this thread in a debug test or `bench-support` build.
#[inline]
pub(crate) fn host_now() -> Instant {
    #[cfg(all(unix, any(test, feature = "bench-support"), debug_assertions))]
    if let Some(at) = HOST_CLOCK.with(std::cell::Cell::get) {
        return at;
    }
    Instant::now()
}

/// Monotonic nanoseconds since the process clock's anchor.
pub(crate) fn now_ns() -> u64 {
    let start = startup::STARTUP.clock_anchor();
    u64::try_from(host_now().saturating_duration_since(start).as_nanos()).unwrap_or(u64::MAX)
}

/// How many frames this process has presented (the `metrics` verb's `frames`, zeroed by `metrics reset`).
pub(crate) fn frames_presented() -> u64 {
    METRICS.window.present.frames.get_acquire()
}

/// The `Instant` a [`now_us`] stamp was read at — this clock's exact inverse, floored to the microsecond the stamp kept.
pub(crate) fn instant_at_us(stamp_us: u64) -> Option<Instant> {
    startup::STARTUP
        .clock_anchor()
        .checked_add(Duration::from_micros(stamp_us))
}

/// The [`now_us`] stamp of `at`, a reading taken earlier.
pub(crate) fn us_at(at: Instant) -> u64 {
    let start = startup::STARTUP.clock_anchor();
    u64::try_from(at.saturating_duration_since(start).as_micros()).unwrap_or(u64::MAX)
}

label_enum! {
    /// The event loop's single earliest `WaitUntil` owner.
    pub(crate) enum DeadlineOwner {
        None => "none",
        BootHealth => "boot_health",
        PendingReveal => "pending_reveal",
        Blink => "blink",
        BellFlash => "bell_flash",
        SyncHold => "sync_hold",
        Autoscroll => "autoscroll",
        ResizeSettle => "resize_settle",
        PresentRetry => "present_retry",
        FrameCap => "frame_cap",
        CursorEffect => "cursor_effect",
        Rain => "rain",
        NativePreview => "native_preview",
        Predictor => "predictor",
        ScrollGlide => "scroll_glide",
        Overscroll => "overscroll",
        ScrollPill => "scroll_pill",
        WordDecorations => "word_decorations",
        LevelUp => "level_up",
        AutoApply => "auto_apply",
        ReaderResume => "reader_resume",
        UpgradeRealized => "upgrade_realized",
        WakeHeal => "wake_heal",
        Video => "video",
        SessionChrome => "session_chrome",
        TitleDrift => "title_drift",
        KittyTenure => "kitty_tenure",
        MessageBand => "message_band",
        SessionStatus => "session_status",
        ConsentCard => "consent_card",
        Presence => "presence",
        HandoffPark => "handoff_park",
        MessageBandMotion => "message_band_motion",
        SessionWaits => "session_waits",
        InputWatch => "input_watch",
        CrashJournal => "crash_journal",
        UpdateChecker => "update_checker",
        SplitRegridHold => "split_regrid_hold",
        KittyAnimation => "kitty_animation",
        RenderEpoch => "render_epoch",
        DebuggerChange => "debugger_change",
        DevToolsHandOver => "devtools_hand_over",
        ClaudeCard => "claude_card",
        ThemePreview => "theme_preview",
        OverviewFabric => "overview_fabric",
        SessionLog => "session_log",
        HarnessActs => "harness_acts",
        AutopilotTimer => "autopilot_timer",
        ConsentObserver => "consent_observer",
        Spare => "spare",
        OutputCadence => "output_cadence",
        SystemVitals => "system_vitals",
        StatsLens => "stats_lens",
        AgentsPanel => "agents_panel",
        StatsLive => "stats_live",
        StatsNeedle => "stats_needle",
    }
}

impl DeadlineOwner {
    fn from_raw(raw: u64) -> Self {
        usize::try_from(raw)
            .ok()
            .and_then(Self::from_index)
            .unwrap_or(Self::None)
    }
}

label_enum! {
    /// Winit's reason for starting the current event-loop iteration.
    pub(crate) enum EventWakeKind {
        None => "none",
        Init => "init",
        Poll => "poll",
        Timer => "timer",
        WaitCancelled => "wait_cancelled",
    }
}

impl EventWakeKind {
    fn from_raw(raw: u64) -> Self {
        usize::try_from(raw)
            .ok()
            .and_then(Self::from_index)
            .unwrap_or(Self::None)
    }
}

label_enum! {
    /// Typed failure labels from the application-present seam.
    pub(crate) enum PresentDropReason {
        None => "none",
        GpuReconfigured => "gpu_reconfigured",
        GpuTimeout => "gpu_timeout",
        GpuUnavailable => "gpu_unavailable",
        GpuOccluded => "gpu_occluded",
        GpuValidation => "gpu_validation",
        CpuResize => "cpu_resize",
        CpuAcquire => "cpu_acquire",
        CpuCommit => "cpu_commit",
        TargetMismatch => "target_mismatch",
        Virtual => "virtual",
        CpuRaster => "cpu_raster",
    }
}

impl PresentDropReason {
    fn from_raw(raw: u64) -> Self {
        usize::try_from(raw)
            .ok()
            .and_then(Self::from_index)
            .unwrap_or(Self::None)
    }
}

impl Metrics {
    fn arm_present_taint(&self) {
        self.live.taint_until.fetch_max(
            now_ns().saturating_add(PRESENT_TAINT_TAIL_NS),
            Ordering::Relaxed,
        );
    }

    fn note_capture_episode(&self, active: bool) {
        if active {
            self.live.capture_depth.fetch_add(1, Ordering::Relaxed);
            self.window.present.capture_episodes.inc();
        } else {
            let _ = self
                .live
                .capture_depth
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |d| {
                    Some(d.saturating_sub(1))
                });
        }
        self.arm_present_taint();
    }
}

const fn tainted_at(
    capture_forced: bool,
    now_ns: u64,
    taint_until_ns: u64,
    capture_depth: u64,
) -> bool {
    capture_forced || capture_depth != 0 || now_ns < taint_until_ns
}

const fn drop_taints_present_latency(reason: PresentDropReason, parked: bool) -> bool {
    parked || matches!(reason, PresentDropReason::GpuOccluded)
}

/// A capture episode (`video` recording, paced or offscreen) opened or closed.
pub(crate) fn note_capture_episode(active: bool) {
    METRICS.note_capture_episode(active);
}

label_enum! {
    /// The UI-thread sites that BLOCK on a session's terminal mutex on the hot paths.
    pub(crate) enum TermWaitSite {
        Redraw => "redraw",
        Press => "press",
    }
}

/// Record one UI-thread terminal-mutex wait at `site`.
pub(crate) fn note_term_wait(site: TermWaitSite, ns: u64) {
    METRICS.window.redraw.note_term_wait(site, ns);
}

impl RedrawLedger {
    fn note_term_wait(&self, site: TermWaitSite, ns: u64) {
        self.term_wait[site.index()].record(ns);
        let _ = self.term_wait_max[site.index()].record(ns);
    }
}

/// Record one swapchain-acquire attempt's wait, including refused acquisitions.
pub(crate) fn note_acquire_wait(ns: u64) {
    let _ = METRICS.window.redraw.acquire_wait.record(ns);
}

/// Record one completed acquisition's queue span, from the same completion as [`note_acquire_wait`].
pub(crate) fn note_acquire_queue(ns: u64) {
    let _ = METRICS.window.redraw.acquire_queue.record(ns);
}

/// Record one armed present's GPU park.
pub(crate) fn note_gpu_park(ns: u64) {
    let _ = METRICS.window.redraw.gpu_park.record(ns);
}

/// A frame whose causal CPU work exceeds the ~30 fps (33.3 ms) budget below which interaction visibly stutters.
pub(crate) const SLOW_FRAME_THRESHOLD_NS: u64 = 33_333_333;

/// One window's present-side state, kept in its `WindowState` (UI thread only, so plain integers).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowPresentState {
    pub(crate) pending_input: PendingInputStamp,
    pub(crate) pending_native_input: PendingNativeInputStamp,
    last_present_ns: u64,
    shown_through: Option<u64>,
}

/// The present-side state that belongs to the whole process, owned by the `App` (UI thread).
#[derive(Debug, Default)]
pub(crate) struct PresentObserver {
    episode: SlowEpisode,
    last_present_ns: u64,
}

#[derive(Clone, Copy, Debug)]
struct PresentClock {
    now: u64,
    epoch: u64,
    capture_forced: bool,
    tainted: bool,
}

impl PresentClock {
    fn read(m: &Metrics, capture_forced: bool) -> Self {
        Self::at(
            capture_forced,
            now_ns(),
            m.epoch.load(Ordering::Relaxed),
            m.live.taint_until.load(Ordering::Relaxed),
            m.live.capture_depth.load(Ordering::Relaxed),
        )
    }

    const fn at(
        capture_forced: bool,
        now: u64,
        epoch: u64,
        taint_until_ns: u64,
        capture_depth: u64,
    ) -> Self {
        Self {
            now,
            epoch,
            capture_forced,
            tainted: tainted_at(capture_forced, now, taint_until_ns, capture_depth),
        }
    }

    const fn gap_since(self, previous: u64) -> Option<u64> {
        if previous == 0 || previous < self.epoch {
            None
        } else {
            Some(self.now.saturating_sub(previous))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PresentBooking {
    output_tainted: Option<bool>,
    window_gap_ns: Option<u64>,
    frame_gap_ns: Option<u64>,
    input_booked: u32,
    input_predicted: u32,
}

/// Record one successful application present.
pub(crate) fn record_present(
    latency_ns: u64,
    render_ns: u64,
    startup_timing: StartupPresentTiming,
    window: Option<&mut WindowPresentState>,
    observer: &mut PresentObserver,
    capture_forced: bool,
) {
    let clock = PresentClock::read(&METRICS, capture_forced);
    startup::STARTUP.publish_first_present(startup_timing);
    let booking = METRICS.record_present_at(clock, latency_ns, render_ns, window, observer);
    #[cfg(test)]
    if booking.output_tainted.is_some() {
        crate::work_counts::present_sample_booked(capture_forced);
    }
    #[cfg(test)]
    crate::work_counts::input_slices_booked(booking.input_booked, booking.input_predicted);
    #[cfg(not(test))]
    let _ = booking;
}

impl Metrics {
    fn record_present_at(
        &self,
        clock: PresentClock,
        latency_ns: u64,
        render_ns: u64,
        mut window: Option<&mut WindowPresentState>,
        observer: &mut PresentObserver,
    ) -> PresentBooking {
        let now = clock.now;
        let present = &self.window.present;
        present.frames.inc_release();
        observer.honour(clock.epoch);
        if let Some(resize_stamp) = present.resize_stamp.take() {
            let d = now.saturating_sub(resize_stamp);
            if d <= RESIZE_SLICE_CAP_NS {
                let _ = present.resize_present.record(d);
            }
        }
        let mut booking = PresentBooking {
            output_tainted: (latency_ns != 0).then_some(clock.tainted),
            window_gap_ns: window
                .as_deref()
                .and_then(|w| clock.gap_since(w.last_present_ns)),
            frame_gap_ns: clock.gap_since(observer.last_present_ns),
            input_booked: 0,
            input_predicted: 0,
        };
        if let Some(tainted) = booking.output_tainted {
            // See `PresentLedger::tainted`.
            if clock.capture_forced {
                present.capture_presents.inc();
            }
            if tainted {
                present.tainted_samples.inc();
                let _ = present.tainted.record(latency_ns);
            } else {
                let since_previous_present = booking.window_gap_ns.unwrap_or(0);
                if present.latency.record(latency_ns) {
                    present.latency_at.set(now);
                    present.latency_gap.set(since_previous_present);
                }
                observer.note_slow_present(
                    self,
                    latency_ns,
                    since_previous_present,
                    now,
                    DeadlineOwner::from_raw(self.live.last_deadline_owner.load(Ordering::Relaxed)),
                );
            }
            if let Some(w) = window.as_deref_mut() {
                w.pending_input.drain_into(now, clock.epoch, |edge| {
                    if edge.before_epoch {
                        return;
                    }
                    self.window.input.book_present(edge.slice_ns, now);
                    booking.input_booked += 1;
                });
            }
            if let Some(stamp) = self.window.input.session_stamp.take() {
                let d = now.saturating_sub(stamp);
                if d <= INPUT_SLICE_CAP_NS {
                    self.window.input.book_present(d, now);
                }
            }
        }
        if let Some(w) = window.as_deref_mut() {
            let shown = w.shown_through.take();
            if booking.output_tainted.is_none()
                && let Some(through) = shown
            {
                w.pending_input
                    .drain_shown(now, clock.epoch, through, |edge| {
                        if edge.before_epoch {
                            return;
                        }
                        self.window.input.book_present(edge.slice_ns, now);
                        booking.input_booked += 1;
                        self.window.input.predicted.inc();
                        booking.input_predicted += 1;
                    });
            }
        }
        let _ = present.render.record(render_ns);
        if render_ns > SLOW_FRAME_THRESHOLD_NS {
            present.slow.inc();
        }
        if let Some(gap) = booking.frame_gap_ns {
            let _ = present.frame_gap.record(gap);
        }
        if let Some(w) = window {
            w.last_present_ns = now;
        }
        observer.last_present_ns = now;
        observer.note_quiet(now);
        booking
    }
}

const SLOW_PRESENT_LOG_THRESHOLD_NS: u64 = SLOW_FRAME_THRESHOLD_NS * 3;

const SLOW_PRESENT_EPISODE_QUIET_NS: u64 = 60_000_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SlowEpisode {
    start_ns: u64,
    last_ns: u64,
    samples: u64,
    worst_ns: u64,
    worst_at_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlowEpisodeStep {
    Ignored,
    Opened { closed: Option<SlowEpisode> },
    Joined,
}

fn slow_episode_step(
    episode: &mut SlowEpisode,
    latency_ns: u64,
    deadline_owner: DeadlineOwner,
    at_ns: u64,
) -> SlowEpisodeStep {
    if latency_ns <= SLOW_PRESENT_LOG_THRESHOLD_NS || deadline_owner == DeadlineOwner::SessionStatus
    {
        return SlowEpisodeStep::Ignored;
    }
    let mut closed = None;
    if episode.samples != 0 {
        if at_ns.saturating_sub(episode.last_ns) < SLOW_PRESENT_EPISODE_QUIET_NS {
            episode.samples += 1;
            episode.last_ns = at_ns;
            if latency_ns > episode.worst_ns {
                episode.worst_ns = latency_ns;
                episode.worst_at_ns = at_ns;
            }
            return SlowEpisodeStep::Joined;
        }
        closed = Some(*episode);
    }
    *episode = SlowEpisode {
        start_ns: at_ns,
        last_ns: at_ns,
        samples: 1,
        worst_ns: latency_ns,
        worst_at_ns: at_ns,
    };
    SlowEpisodeStep::Opened { closed }
}

fn slow_episode_close(episode: &mut SlowEpisode, now_ns: u64, force: bool) -> Option<SlowEpisode> {
    if episode.samples == 0
        || !(force || now_ns.saturating_sub(episode.last_ns) >= SLOW_PRESENT_EPISODE_QUIET_NS)
    {
        return None;
    }
    Some(std::mem::take(episode))
}

fn log_span(ns: u64) -> String {
    let secs = ns as f64 / 1e9;
    if secs < 120.0 {
        format!("{secs:.1} s")
    } else if secs < 7200.0 {
        format!("{:.0} min", secs / 60.0)
    } else {
        format!("{:.1} h", secs / 3600.0)
    }
}

fn log_slow_episode_end(episode: SlowEpisode) {
    if episode.samples < 2 {
        return;
    }
    aterm_log::info!(
        "slow presents ended: {} over {}, worst {:.1} ms at t+{:.1} s",
        episode.samples,
        log_span(episode.last_ns.saturating_sub(episode.start_ns)),
        episode.worst_ns as f64 / 1e6,
        episode.worst_at_ns as f64 / 1e9,
    );
}

impl PresentObserver {
    fn honour(&mut self, epoch: u64) {
        if self.episode.last_ns < epoch
            && let Some(closed) = slow_episode_close(&mut self.episode, 0, true)
        {
            log_slow_episode_end(closed);
        }
    }

    fn note_slow_present(
        &mut self,
        m: &Metrics,
        latency_ns: u64,
        since_previous_present_ns: u64,
        at_ns: u64,
        owner: DeadlineOwner,
    ) {
        let SlowEpisodeStep::Opened { closed } =
            slow_episode_step(&mut self.episode, latency_ns, owner, at_ns)
        else {
            return;
        };
        if let Some(closed) = closed {
            log_slow_episode_end(closed);
        }
        let ms = |ns: u64| ns as f64 / 1e6;
        aterm_log::info!(
            "slow present: {:.1} ms at t+{:.1} s (prev present {:.1} ms ago); wake={} late {:.1} ms; \
             deadline={} late {:.1} ms; acquire {:.1} ms",
            ms(latency_ns),
            at_ns as f64 / 1e9,
            ms(since_previous_present_ns),
            EventWakeKind::from_raw(m.live.last_wake_kind.load(Ordering::Relaxed)).as_str(),
            ms(m.live.last_wake_late.load(Ordering::Relaxed)),
            owner.as_str(),
            ms(m.live.last_deadline_late.load(Ordering::Relaxed)),
            ms(m.window.redraw.acquire_wait.last()),
        );
    }

    fn note_quiet(&mut self, now_ns: u64) {
        if let Some(closed) = slow_episode_close(&mut self.episode, now_ns, false) {
            log_slow_episode_end(closed);
        }
    }
}

/// An offscreen rasterization (`image` / `window` / `snapshot` introspection).
pub(crate) fn record_offscreen_raster(render_ns: u64) {
    METRICS.window.present.note_offscreen_raster(render_ns);
}

impl PresentLedger {
    fn note_offscreen_raster(&self, render_ns: u64) {
        self.offscreen_rasters.inc();
        let _ = self.offscreen.record(render_ns);
    }
}

/// Install the compositor and device-execution ledgers once at GUI entry.
pub(crate) fn install_present_glass_sink() {
    let _ = aterm_gpu::present_glass::install_sink(note_present_glass);
    let _ = aterm_gpu::gpu_time::install_sink(note_gpu_time);
}

fn note_gpu_time(frame: aterm_gpu::gpu_time::GpuFrameTime) {
    METRICS.window.gpu.note_time(frame);
}

/// Book a successful GPU present's encode and the shape of the offscreen it presented (virtual presents included), into [`GpuLedger`].
pub(crate) fn note_gpu_frame(shape: aterm_gpu::FrameShape, encode_ns: u64) {
    METRICS.window.gpu.note_frame(shape, encode_ns);
}

const fn ablation_code(mode: aterm_gpu::UnormAblation) -> u8 {
    use aterm_gpu::UnormAblation::{Drop, Off, Thin};
    match mode {
        Off => 0,
        Drop => 1,
        Thin => 2,
    }
}

fn ablation_of(code: u8) -> Option<aterm_gpu::UnormAblation> {
    use aterm_gpu::UnormAblation::{Drop, Off, Thin};
    [Off, Drop, Thin]
        .into_iter()
        .find(|&mode| ablation_code(mode) == code)
}

const fn route_code(route: aterm_gpu::UnormRoute) -> u8 {
    match route {
        aterm_gpu::UnormRoute::Split => 0,
        aterm_gpu::UnormRoute::Fetch => 1,
    }
}

/// A framebuffer-fetch verdict's [`GpuLedger::fetch_probe`] code: 0 while none settled.
const fn probe_code(probe: Option<aterm_gpu::FetchProbe>) -> u8 {
    match probe {
        None => 0,
        Some(aterm_gpu::FetchProbe::Unsupported) => 1,
        Some(aterm_gpu::FetchProbe::Passed) => 2,
        Some(aterm_gpu::FetchProbe::Failed { .. }) => 3,
        Some(aterm_gpu::FetchProbe::Refused) => 4,
    }
}

/// The `gpu_fetch_probe=` word of a [`probe_code`]: the verdict's name, `pending` before one settled.
const fn probe_name(code: u64) -> &'static str {
    match code {
        0 => "pending",
        1 => "unsupported",
        2 => "passed",
        3 => "failed",
        4 => "refused",
        _ => "unknown",
    }
}

fn route_of(code: u8) -> Option<aterm_gpu::UnormRoute> {
    use aterm_gpu::UnormRoute::{Fetch, Split};
    [Split, Fetch]
        .into_iter()
        .find(|&route| route_code(route) == code)
}

impl GpuLedger {
    fn note_time(&self, frame: aterm_gpu::gpu_time::GpuFrameTime) {
        let _ = self.time.record(frame.gpu_ns);
        self.time_parts.set(u64::from(frame.parts));
    }

    fn note_frame(&self, shape: aterm_gpu::FrameShape, encode_ns: u64) {
        self.encode.record(encode_ns);
        self.encode_last.set(encode_ns);
        self.passes.set(u64::from(shape.passes));
        self.instance_bytes.set(shape.instance_bytes);
        self.build.set(shape.build_ns);
        self.stage.set(shape.stage_ns);
        self.unorm_ablation
            .set(usize::from(ablation_code(shape.ablation)));
        self.unorm_route.set(usize::from(route_code(shape.route)));
        self.fetch_probe
            .set(usize::from(probe_code(shape.fetch_probe)));
        for (slot, count) in self.instances.iter().zip(shape.instances) {
            slot.set(u64::from(count));
        }
        self.fx_instances.set(u64::from(shape.fx_instances));
        self.fx_upload_bytes.set(shape.fx_upload_bytes);
        self.fx_refused.set(shape.fx_refused);
    }

    fn unorm_ablation_name(&self) -> &'static str {
        u8::try_from(self.unorm_ablation.get())
            .ok()
            .and_then(ablation_of)
            .map_or("unknown", aterm_gpu::UnormAblation::name)
    }

    fn fetch_probe_name(&self) -> &'static str {
        probe_name(self.fetch_probe.get())
    }

    fn unorm_route_name(&self) -> &'static str {
        u8::try_from(self.unorm_route.get())
            .ok()
            .and_then(route_of)
            .map_or("unknown", aterm_gpu::UnormRoute::name)
    }

    fn sample_fx_totals(&self) {
        self.fx_layer_builds
            .raise_total(aterm_core::render::fx_layer_builds());
        self.fx_payload_copies
            .raise_total(aterm_core::render::fx_payload_copies());
    }
}

const fn gpu_time_source(samples: u64) -> &'static str {
    if !aterm_gpu::gpu_time::SUPPORTED {
        "unsupported"
    } else if samples == 0 {
        "unobserved"
    } else {
        "device"
    }
}

fn emit_gpu(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    use wire::ms;
    let gpu = &ctx.metrics.window.gpu;
    let time = gpu.time.copy_out();
    s.label("gpu_time_source", gpu_time_source(time.n()));
    emit_quantiles(
        s,
        [
            "n_gpu_time",
            "gpu_time_p50_ms",
            "gpu_time_p95_ms",
            "gpu_time_p99_ms",
        ],
        &time,
    );
    s.decimal("last_gpu_time_ms", ms(gpu.time.last()));
    s.decimal("max_gpu_time_ms", ms(gpu.time.max()));
    s.count("gpu_time_parts_last", gpu.time_parts.get());
    let (dropped, skipped) = if ctx.reads_process() {
        (
            aterm_gpu::gpu_time::dropped(),
            aterm_gpu::gpu_time::skipped(),
        )
    } else {
        (0, 0)
    };
    s.count("gpu_time_dropped_total", dropped);
    s.count("gpu_time_skipped_total", skipped);
    emit_quantiles(
        s,
        [
            "n_gpu_encode",
            "gpu_encode_p50_ms",
            "gpu_encode_p95_ms",
            "gpu_encode_p99_ms",
        ],
        &gpu.encode.copy_out(),
    );
    s.decimal("gpu_encode_last_ms", ms(gpu.encode_last.get()));
    s.count("passes_last", gpu.passes.get());
    let instances: Vec<(&str, u64)> = aterm_gpu::FrameShape::STREAMS
        .iter()
        .zip(&gpu.instances)
        .filter_map(|(&name, count)| {
            let count = count.get();
            (count != 0).then_some((name, count))
        })
        .collect();
    s.count_pairs("instances_by_stream", &instances, "none");
    s.count("instance_bytes_last", gpu.instance_bytes.get());
    s.decimal("gpu_build_last_ms", ms(gpu.build.get()));
    s.decimal("gpu_stage_last_ms", ms(gpu.stage.get()));
    s.label("gpu_unorm_ablation", gpu.unorm_ablation_name());
    s.label("gpu_unorm_route", gpu.unorm_route_name());
    s.label("gpu_fetch_probe", gpu.fetch_probe_name());
    s.count("fx_instances_last", gpu.fx_instances.get());
    s.count("fx_upload_bytes_last", gpu.fx_upload_bytes.get());
    s.count("fx_refused_last", gpu.fx_refused.get());
    if ctx.reads_process() {
        gpu.sample_fx_totals();
    }
    s.count("fx_layer_builds", gpu.fx_layer_builds.since_reset());
    s.count("fx_payload_copies", gpu.fx_payload_copies.since_reset());
}

fn note_present_glass(report: aterm_gpu::present_glass::GlassReport) {
    METRICS.window.glass.note(report);
}

impl GlassLedger {
    fn note(&self, report: aterm_gpu::present_glass::GlassReport) {
        match report.sample {
            aterm_gpu::present_glass::GlassSample::OnGlass { ns } => {
                let _ = self.latency.record(ns);
                self.shown_by.inc(report.tag.index());
            }
            aterm_gpu::present_glass::GlassSample::Skipped => {
                self.skipped.inc();
                self.skipped_by.inc(report.skip_attribution().index());
            }
        }
    }
}

/// The `present_latency` honesty cap, shared: see `present_latency_ns_with_plan`.
pub(crate) const OUTPUT_REDRAW_REQUEST_CAP_NS: u64 = 5_000_000_000;

/// Book one output → redraw-request span (see `InputLedger::out_req`).
pub(crate) fn note_output_redraw_request(ns: u64) {
    let _ = METRICS.window.input.out_req.record(ns);
}

/// Book one committed IME composition: preedit start → commit, in ns.
pub(crate) fn note_ime_compose(ns: u64) {
    METRICS.window.input.ime.record(ns);
}

/// Book one Rewind step's latency, from the main thread taking it to the present that shows it (`InputLedger::rewind_step`).
pub(crate) fn note_rewind_step(elapsed: std::time::Duration) {
    METRICS
        .window
        .input
        .rewind_step
        .record(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
}

/// Book one Rewind step's latency, from the main thread taking it to its fold shown and its redraw asked for (`InputLedger::rewind_shown`).
pub(crate) fn note_rewind_shown(elapsed: std::time::Duration) {
    METRICS
        .window
        .input
        .rewind_shown
        .record(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
}

/// Book the output one Rewind step's fold applied past its start (`InputLedger::rewind_folded`).
pub(crate) fn note_rewind_folded(bytes: usize) {
    let _ = METRICS
        .window
        .input
        .rewind_folded
        .record(u64::try_from(bytes).unwrap_or(u64::MAX));
}

/// Book one build of the menu-bar item's glance, in ns (`InputLedger::status_glance`).
pub(crate) fn note_status_glance(ns: u64) {
    let input = &METRICS.window.input;
    input.status_glance.record(ns);
    let _ = input.status_glance_max.record(ns);
}

/// Book one session's need publication, in ns (`InputLedger::need_publish`).
pub(crate) fn note_need_publish(ns: u64) {
    let input = &METRICS.window.input;
    input.need_publish.record(ns);
    let _ = input.need_publish_max.record(ns);
}

/// Book one transcript apply hold of `kind`, in ns (`InputLedger::tx_chunk`).
pub(crate) fn note_tx_hold(kind: crate::session_log::transcript::Hold, ns: u64) {
    let input = &METRICS.window.input;
    match kind {
        crate::session_log::transcript::Hold::Full => input.tx_chunk.record(ns),
        crate::session_log::transcript::Hold::Last => input.tx_last.record(ns),
    }
    let _ = input.tx_hold.record(ns);
}

/// A composition ended without a commit.
pub(crate) fn note_ime_cancelled() {
    METRICS.window.input.ime_cancelled.inc();
}

/// Input was refused because its session's input queue is full.
pub(crate) fn note_input_refused() {
    METRICS.window.input.refused.inc();
}

/// A terminal query reply was dropped because its session's reply lane is full.
pub(crate) fn note_reply_dropped() {
    METRICS.window.input.reply_dropped.inc();
}

fn emit_input_legs(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    use wire::ms;
    let input = &ctx.metrics.window.input;
    emit_quantiles(
        s,
        [
            "n_out_req",
            "out_req_p50_ms",
            "out_req_p95_ms",
            "out_req_p99_ms",
        ],
        &input.out_req.copy_out(),
    );
    s.decimal("last_out_req_ms", ms(input.out_req.last()));
    s.decimal("max_out_req_ms", ms(input.out_req.max()));
    emit_quantiles(
        s,
        ["n_ime", "ime_p50_ms", "ime_p95_ms", "ime_p99_ms"],
        &input.ime.copy_out(),
    );
    s.count("ime_cancelled", input.ime_cancelled.get());
    emit_quantiles(
        s,
        [
            "n_rewind_step",
            "rewind_step_p50_ms",
            "rewind_step_p95_ms",
            "rewind_step_p99_ms",
        ],
        &input.rewind_step.copy_out(),
    );
    emit_quantiles(
        s,
        [
            "n_rewind_shown",
            "rewind_shown_p50_ms",
            "rewind_shown_p95_ms",
            "rewind_shown_p99_ms",
        ],
        &input.rewind_shown.copy_out(),
    );
    s.count("max_rewind_fold_bytes", input.rewind_folded.get());
    s.count("input_refused", input.refused.get());
    s.count("reply_dropped", input.reply_dropped.get());
    emit_quantiles_ms3(
        s,
        [
            "n_status_glance",
            "status_glance_p50_ms",
            "status_glance_p95_ms",
            "status_glance_p99_ms",
        ],
        &input.status_glance.copy_out(),
    );
    s.decimal(
        "max_status_glance_ms",
        wire::ms3(input.status_glance_max.get()),
    );
    emit_quantiles_ms3(
        s,
        [
            "n_need_publish",
            "need_publish_p50_ms",
            "need_publish_p95_ms",
            "need_publish_p99_ms",
        ],
        &input.need_publish.copy_out(),
    );
    s.decimal(
        "max_need_publish_ms",
        wire::ms3(input.need_publish_max.get()),
    );
    emit_quantiles_ms3(
        s,
        [
            "n_tx_chunk",
            "tx_chunk_p50_ms",
            "tx_chunk_p95_ms",
            "tx_chunk_p99_ms",
        ],
        &input.tx_chunk.copy_out(),
    );
    emit_quantiles_ms3(
        s,
        [
            "n_tx_last",
            "tx_last_p50_ms",
            "tx_last_p95_ms",
            "tx_last_p99_ms",
        ],
        &input.tx_last.copy_out(),
    );
    s.decimal("max_tx_hold_ms", wire::ms3(input.tx_hold.get()));
}

fn owner_labels() -> impl Iterator<Item = (usize, &'static str)> {
    DeadlineOwner::ALL
        .into_iter()
        .map(|owner| (owner.index(), owner.as_str()))
}

fn tag_labels() -> impl Iterator<Item = (usize, &'static str)> {
    aterm_gpu::present_glass::PresentTag::ALL
        .iter()
        .map(|tag| (tag.index(), tag.as_str()))
}

fn emit_present_glass(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    use wire::ms;
    let glass = &ctx.metrics.window.glass;
    emit_quantiles(
        s,
        [
            "n_present_glass",
            "present_glass_p50_ms",
            "present_glass_p95_ms",
            "present_glass_p99_ms",
        ],
        &glass.latency.copy_out(),
    );
    s.decimal("last_present_glass_ms", ms(glass.latency.last()));
    s.decimal("max_present_glass_ms", ms(glass.latency.max()));
    s.count("present_glass_skipped", glass.skipped.get());
    s.count_pairs(
        "present_glass_shown_by",
        &glass.shown_by.sparse(tag_labels()),
        "none",
    );
    s.count_pairs(
        "present_glass_skipped_by",
        &glass.skipped_by.sparse(tag_labels()),
        "none",
    );
}

fn emit_lateness(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    use wire::ms;
    let w = &ctx.metrics.window;
    s.decimal("max_wake_late_ms", ms(w.sched.wake_late.max()));
    s.label(
        "max_wake_late_owner",
        DeadlineOwner::from_raw(w.sched.wake_late.by()).as_str(),
    );
    s.decimal("max_wake_late_at_ms", ms(w.sched.wake_late.at()));
    s.decimal("max_deadline_late_ms", ms(w.sched.deadline_late.max()));
    s.label(
        "max_deadline_late_owner",
        DeadlineOwner::from_raw(w.sched.deadline_late.by()).as_str(),
    );
    s.decimal("max_deadline_late_at_ms", ms(w.sched.deadline_late.at()));
    s.decimal("max_present_latency_at_ms", ms(w.present.latency_at.get()));
    s.decimal(
        "max_present_latency_gap_ms",
        ms(w.present.latency_gap.get()),
    );
    s.decimal("max_input_present_at_ms", ms(w.input.input_at.get()));
    s.decimal("metrics_now_ms", ms(ctx.now_ns));
}

impl InputLedger {
    fn book_present(&self, d: u64, now: u64) {
        if self.input.record(d) {
            self.input_at.set(now);
        }
    }
}

/// How many unpresented keystrokes one window's [`PendingInputStamp`] holds.
pub(crate) const INPUT_EDGE_RING: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InputEdge {
    arrival_ns: u64,
    armed_ns: u64,
    op: Option<u64>,
}

/// One window's pending input→present EDGES.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PendingInputStamp {
    edges: [InputEdge; INPUT_EDGE_RING],
    len: usize,
    fresh: bool,
}

impl PendingInputStamp {
    fn arm(&mut self, arrival_ns: u64, armed_ns: u64) -> bool {
        if self.len == INPUT_EDGE_RING {
            self.evict_aged(armed_ns);
        }
        let Some(slot) = self.edges.get_mut(self.len) else {
            self.fresh = false;
            return false;
        };
        *slot = InputEdge {
            arrival_ns: arrival_ns.max(1),
            armed_ns,
            op: None,
        };
        self.len += 1;
        self.fresh = true;
        true
    }

    /// Tag the key just armed with the predictor's op it armed, or settle it as one that armed none (`None`).
    pub(crate) fn tag_newest(&mut self, op: Option<u64>) {
        if self.fresh
            && let Some(edge) = self
                .len
                .checked_sub(1)
                .and_then(|newest| self.edges.get_mut(newest))
        {
            edge.op = op;
        }
        self.fresh = false;
    }

    fn drain_shown(
        &mut self,
        now_ns: u64,
        epoch: u64,
        through: u64,
        mut f: impl FnMut(DrainedEdge),
    ) {
        let shown = self.edges[..self.len]
            .iter()
            .take_while(|edge| edge.op.is_some_and(|op| op <= through))
            .count();
        if shown == 0 {
            return;
        }
        for edge in &self.edges[..shown] {
            let slice_ns = now_ns.saturating_sub(edge.arrival_ns);
            if slice_ns <= INPUT_SLICE_CAP_NS {
                f(DrainedEdge {
                    slice_ns,
                    before_epoch: edge.armed_ns < epoch,
                });
            }
        }
        self.edges.copy_within(shown..self.len, 0);
        for edge in &mut self.edges[self.len - shown..self.len] {
            *edge = InputEdge::default();
        }
        self.len -= shown;
        self.fresh &= self.len > 0;
    }

    fn evict_aged(&mut self, now_ns: u64) {
        let mut kept = 0;
        for index in 0..self.len {
            let edge = self.edges[index];
            if now_ns.saturating_sub(edge.arrival_ns) <= INPUT_SLICE_CAP_NS {
                self.edges[kept] = edge;
                kept += 1;
            }
        }
        for edge in &mut self.edges[kept..self.len] {
            *edge = InputEdge::default();
        }
        self.len = kept;
    }

    /// Whether a keystroke in this window is still waiting on a content present.
    #[cfg(test)]
    pub(crate) const fn is_pending(self) -> bool {
        self.len != 0
    }

    fn drain_into(&mut self, now_ns: u64, epoch: u64, mut f: impl FnMut(DrainedEdge)) {
        for edge in &mut self.edges[..self.len] {
            let slice_ns = now_ns.saturating_sub(edge.arrival_ns);
            if slice_ns <= INPUT_SLICE_CAP_NS {
                f(DrainedEdge {
                    slice_ns,
                    before_epoch: edge.armed_ns < epoch,
                });
            }
            *edge = InputEdge::default();
        }
        self.len = 0;
        self.fresh = false;
    }

    #[cfg(test)]
    fn drained(&mut self, now_ns: u64, epoch: u64) -> Vec<DrainedEdge> {
        let mut edges = Vec::new();
        self.drain_into(now_ns, epoch, |edge| edges.push(edge));
        edges
    }

    #[cfg(test)]
    fn drain_slices(&mut self, now_ns: u64) -> Vec<u64> {
        let mut slices = Vec::new();
        self.drain_into(now_ns, 0, |edge| slices.push(edge.slice_ns));
        slices
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DrainedEdge {
    slice_ns: u64,
    before_epoch: bool,
}

impl Metrics {
    fn input_arrival_ns(&self, now: u64) -> u64 {
        let key = self.live.lat_key.load(Ordering::Relaxed);
        if key != 0 && now.saturating_sub(key) < 500_000_000 {
            key
        } else {
            now
        }
    }

    fn note_window_input(&self, pending: &mut PendingInputStamp, now: u64) {
        if !pending.arm(self.input_arrival_ns(now), now) {
            self.window.input.edges_dropped.inc();
        }
    }

    fn note_native_window_input(&self, pending: &mut PendingInputStamp, arrival_ns: u64, now: u64) {
        if !pending.arm(arrival_ns, now) {
            self.window.input.edges_dropped.inc();
        }
    }

    fn record_native_content_present(&self, pending: &mut PendingInputStamp, now: u64) {
        let epoch = self.epoch.load(Ordering::Relaxed);
        pending.drain_into(now, epoch, |edge| {
            if !edge.before_epoch {
                self.window.input.book_present(edge.slice_ns, now);
            }
        });
    }
}

/// Stamp the arrival of a keystroke routed to ONE window (the `App::input_to_session` seam, hardware keys and controller `key` alike) into that window's [`PendingInputStamp`].
pub(crate) fn note_window_input(pending: &mut PendingInputStamp) {
    METRICS.note_window_input(pending, now_ns());
}

/// The frame being drawn in this window shows the predictor's overlay whole and under its proof, through op `through` (`Predictor::shown_through`), or shows none (`None`; the render paths' `closes_input_through`).
pub(crate) fn note_prediction_shown(
    window: &mut WindowPresentState,
    through: Option<u64>,
    mut op_is_pending: impl FnMut(u64) -> bool,
) {
    if through.is_some() {
        for edge in &mut window.pending_input.edges[..window.pending_input.len] {
            if edge.op.is_some_and(|op| !op_is_pending(op)) {
                edge.op = None;
            }
        }
    }
    window.shown_through = through;
}

/// Native edges awaiting the first present of their page's pixels drawn after them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PendingNativeInputStamp {
    /// The view the open edges were typed into and the paint serial their answer is drawn at
    /// or after (`App::note_native_paint`).
    awaited: Option<(crate::tab_model::ViewId, u64)>,
    edges: PendingInputStamp,
}

impl PendingNativeInputStamp {
    fn advance(&mut self, view: crate::tab_model::ViewId, serial: u64) {
        if self.awaited.is_some_and(|(was, _)| was != view) {
            self.edges = PendingInputStamp::default();
        }
        self.awaited = Some((view, serial));
    }

    /// Whether a key a native page took is still waiting on its present.
    #[cfg(test)]
    pub(crate) const fn is_pending(self) -> bool {
        self.edges.len != 0
    }
}

/// The arrival a keystroke a NATIVE page takes (Settings search, the editor) is timed from: read before the page's reducer runs.
pub(crate) fn native_input_arrival() -> u64 {
    METRICS.input_arrival_ns(now_ns())
}

/// Arm a keystroke a native page took (`arrival_ns` from [`native_input_arrival`]) in its window's
/// native ring ([`WindowPresentState::pending_native_input`]): its dispatch changed what `view`
/// draws, which the page's pixels show from paint serial `serial` on.
pub(crate) fn note_native_window_input(
    pending: &mut PendingNativeInputStamp,
    view: crate::tab_model::ViewId,
    serial: u64,
    arrival_ns: u64,
) {
    pending.advance(view, serial);
    METRICS.note_native_window_input(&mut pending.edges, arrival_ns, now_ns());
}

/// A present that put `view`'s pixels of paint serial `serial` on glass for the first time: the
/// answer of every key it was awaited for at or before that serial.
pub(crate) fn record_native_content_present(
    pending: &mut PendingNativeInputStamp,
    view: crate::tab_model::ViewId,
    serial: u64,
) {
    if !pending
        .awaited
        .is_some_and(|(awaited, from)| awaited == view && serial >= from)
    {
        return;
    }
    let mut taken = std::mem::take(pending);
    METRICS.record_native_content_present(&mut taken.edges, now_ns());
}

/// Stamp the arrival of PTY-bound input that names a session but no window (a control `send`/`feed` to a background session, a controller host write), keeping the oldest unpresented arrival.
pub(crate) fn note_input() {
    let m = &METRICS;
    m.window
        .input
        .session_stamp
        .arm(m.input_arrival_ns(now_ns()));
}

/// Stamp the arrival of a window-bounds change (`WindowEvent::Resized`), opening the stale-frame window that the next present closes.
pub(crate) fn note_resize_arrival() {
    let present = &METRICS.window.present;
    let now = now_ns();
    present.resize_stamp.arm(now);
    present.reflow_stamp.arm(now);
}

/// The engine just committed a new grid geometry (`apply_term_resize` reported a real change), closing the stale-grid slice a bounds change opened.
pub(crate) fn note_grid_committed() {
    let present = &METRICS.window.present;
    let Some(stamp) = present.reflow_stamp.take() else {
        return;
    };
    let d = now_ns().saturating_sub(stamp);
    if d <= RESIZE_SLICE_CAP_NS {
        let _ = present.resize_reflow.record(d);
    }
}

static LAT_TRACE_ON: OnceLock<bool> = OnceLock::new();

/// Whether the latency stderr traces are armed.
pub(crate) fn latency_trace_on() -> bool {
    *LAT_TRACE_ON.get_or_init(|| aterm_types::dev_seam!("ATERM_TRACE_LATENCY").is_some())
}

/// Microsecond view of the metrics clock, the epoch of [`note_key_arrival_queued`].
#[must_use]
pub(crate) fn now_us() -> u64 {
    now_ns() / 1000
}

/// Stamp the true arrival of a printable hardware keystroke (winit `KeyboardInput`, before encode/write), backdated by `queue_ns`, the NSEvent's time in the OS event queue (macOS and X11: `platform::current_event_queue_age_ns`; 0 elsewhere).
pub(crate) fn note_key_arrival_queued(queue_ns: u64) {
    METRICS.note_key_arrival_queued(queue_ns);
}

/// Disarm the key-arrival stamp after a key's dispatch ends without a PTY write.
pub(crate) fn clear_key_arrival() {
    METRICS.live.lat_key.store(0, Ordering::Relaxed);
}

/// The current key's true hardware arrival in ms (the backdated stamp [`note_key_arrival_queued`] wrote), as a peek.
#[must_use]
pub(crate) fn key_arrival_ms() -> Option<u32> {
    match METRICS.live.lat_key.load(Ordering::Relaxed) {
        0 => None,
        ns => Some(((ns / 1_000_000) as u32).max(1)),
    }
}

/// The PTY write just returned (after the press-path `term_lock`s and, on Windows, the blocking `WriteFile`).
pub(crate) fn note_pty_write() {
    note_pty_write_at(take_key_arrival());
}

/// Claim the pending key-arrival stamp without recording anything, for whoever performs the write.
#[must_use]
pub(crate) fn take_key_arrival() -> u64 {
    METRICS.live.lat_key.swap(0, Ordering::Relaxed)
}

/// Record a key→write slice against a stamp claimed by [`take_key_arrival`], for writes that complete off the UI thread.
pub(crate) fn note_pty_write_at(key: u64) {
    if key == 0 {
        return;
    }
    let input = &METRICS.window.input;
    let d = now_ns().saturating_sub(key);
    let _ = input.key_write.record(d);
    if latency_trace_on() {
        crate::logging::stderr_line!(
            "KEYWRITE key->write={:.2}ms (last OS queue {:.2}ms + UI encode + term_locks \
             + PTY write)",
            d as f64 / 1_000_000.0,
            input.key_queue.last() as f64 / 1_000_000.0
        );
    }
}

impl Metrics {
    fn note_key_arrival_queued(&self, queue_ns: u64) {
        self.live
            .lat_key
            .store(now_ns().saturating_sub(queue_ns).max(1), Ordering::Relaxed);
        let _ = self.window.input.key_queue.record(queue_ns);
    }
}

/// A DEC 2026 hold episode armed (the rising edge saw its first held frame).
pub(crate) fn note_sync_armed() {
    METRICS.window.sync.holds_armed.inc();
}

/// An armed episode released because the app ended sync (`?2026l`): the healthy cause.
pub(crate) fn note_sync_release_end() {
    METRICS.window.sync.releases_end.inc();
}

/// An armed episode released by the safety-valve deadline.
pub(crate) fn note_sync_release_timeout() {
    METRICS.window.sync.releases_timeout.inc();
}

/// Per-window lease behind the process-wide synchronized-output hold gauge.
pub(crate) struct SyncHoldingGauge {
    held: bool,
}

fn transition_sync_holding(held: &mut bool, new: bool, count: &AtomicU64) {
    if *held == new {
        return;
    }
    *held = new;
    if new {
        count.fetch_add(1, Ordering::Relaxed);
    } else {
        let previous = count.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0, "sync holding gauge underflow");
    }
}

impl SyncHoldingGauge {
    pub(crate) const fn new() -> Self {
        Self { held: false }
    }

    pub(crate) fn set(&mut self, held: bool) {
        transition_sync_holding(&mut self.held, held, &METRICS.live.sync_holding_windows);
    }

    /// Whether the window holds a synchronized update (the tests, and `crate::trail_rig`'s
    /// present).
    #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
    pub(crate) fn is_held_for_test(&self) -> bool {
        self.held
    }
}

impl Drop for SyncHoldingGauge {
    fn drop(&mut self) {
        transition_sync_holding(&mut self.held, false, &METRICS.live.sync_holding_windows);
    }
}

#[cfg(test)]
mod sync_holding_gauge_tests {
    use super::transition_sync_holding;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn independent_window_leases_are_idempotent_and_release_on_drop_transition() {
        let count = AtomicU64::new(0);
        let mut a = false;
        let mut b = false;

        transition_sync_holding(&mut a, true, &count);
        transition_sync_holding(&mut a, true, &count);
        assert_eq!(count.load(Ordering::Relaxed), 1);

        transition_sync_holding(&mut b, true, &count);
        assert_eq!(count.load(Ordering::Relaxed), 2);

        transition_sync_holding(&mut a, false, &count);
        transition_sync_holding(&mut a, false, &count);
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "one window releasing cannot clear its held sibling"
        );

        transition_sync_holding(&mut b, false, &count);
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
}

/// The `perf_reduced` load-shed latch flipped (called on the edge only).
pub(crate) fn note_shed_transition(reduced: bool) {
    METRICS.live.perf_reduced.store(reduced, Ordering::Relaxed);
    METRICS.window.sync.shed_transitions.inc();
}

/// A lost `Wake::Output` was proven lost and healed by the watchdog.
pub(crate) fn note_wake_heal() {
    METRICS.window.sync.wake_heals.inc();
}

/// A posted `Wake::Output` waited past the latch expiry behind the main thread's own turn (booked once per wake; no action follows).
pub(crate) fn note_output_wake_late() {
    METRICS.window.sync.output_wake_late.inc();
}

/// A PTY reader re-posted a `Wake::Output` stranded with its loop wake-up (see `SyncLedger::output_wake_reposts`).
pub(crate) fn note_output_wake_repost() {
    METRICS.window.sync.output_wake_reposts.inc();
}

/// Record one redraw pass's total wall time (entry → present done or failed surface transaction), including the acquire/present wait `frame_render` excludes.
pub(crate) fn record_redraw_total(total_ns: u64) {
    let _ = METRICS.window.redraw.total.record(total_ns);
}

/// A `RedrawRequested` entered the real redraw pipeline.
pub(crate) fn note_redraw_attempt() {
    METRICS.window.redraw.attempts.inc();
}

/// The stable RepaintKey/predictor/effect gate proved that no pixels changed.
pub(crate) fn note_redraw_early_out() {
    METRICS.window.redraw.early_outs.inc();
}

/// The presented-path frame extraction completed.
pub(crate) fn note_frame_refill(refill: aterm_core::render::FrameRefill) {
    METRICS.window.redraw.note_refill(refill);
}

/// A presented frame reused the existing snapshot.
pub(crate) fn note_frame_refill_skipped() {
    METRICS.window.redraw.refills_skipped.inc();
}

/// One resident-pet world observation that stood, and the cells it classified.
pub(crate) fn note_pet_world_observation(cells: usize) {
    METRICS.window.redraw.note_pet_world(cells);
}

/// A gather drained `n` process events ([`CommandLedger::proc_events`]).
#[cfg(unix)]
pub(crate) fn note_proc_events(n: usize) {
    if n > 0 {
        METRICS.window.command.proc_events.add(n as u64);
    }
}

/// A gather spent `ns` draining its process watch and judging what it heard.
#[cfg(unix)]
pub(crate) fn note_proc_drain(ns: u64) {
    METRICS.window.command.proc_drain_ns.add(ns);
}

/// A process event became a foreground edge.
#[cfg(unix)]
pub(crate) fn note_exec_edge() {
    METRICS.window.command.exec_edges.inc();
}

/// A reader that wanted a process watch got none and runs the byte-driven arm.
#[cfg(unix)]
pub(crate) fn note_procwatch_fallback() {
    METRICS.window.command.procwatch_fallbacks.inc();
}

/// `n` more children past a watch's cap.
#[cfg(unix)]
pub(crate) fn note_procwatch_overflow(n: u64) {
    METRICS.window.command.procwatch_overflow.add(n);
}

/// The engine fence was cut in a parse hold.
pub(crate) fn note_holder_cut() {
    METRICS.window.command.holder_cuts.inc();
}

/// A foreground handback ran.
pub(crate) fn note_fg_handback() {
    METRICS.window.command.fg_handbacks.inc();
}

/// A parse stage took a slice hold, after waiting `waited_ns` for the term lock (`0`: it was free).
pub(crate) fn note_parse_hold(waited_ns: u64) {
    METRICS.window.command.note_parse_hold(waited_ns);
}

/// The UI thread's status sweep took `ns`.
pub(crate) fn note_status_sweep(ns: u64) {
    METRICS.window.command.note_status_sweep(ns);
}

impl CommandLedger {
    fn note_parse_hold(&self, waited_ns: u64) {
        self.parse_holds.inc();
        if waited_ns > 0 {
            self.parse_lock_waits.inc();
            self.parse_lock_wait_ns.add(waited_ns);
        }
    }

    fn note_status_sweep(&self, ns: u64) {
        self.status_sweeps.inc();
        self.status_sweep_ns.add(ns);
    }
}

fn emit_command_cost(c: &CommandLedger, s: &mut dyn wire::FieldSink) {
    use wire::ms3;
    s.count("proc_events", c.proc_events.get());
    s.decimal("proc_drain_ms", ms3(c.proc_drain_ns.get()));
    s.count("exec_edges", c.exec_edges.get());
    s.count("procwatch_fallbacks", c.procwatch_fallbacks.get());
    s.count("procwatch_overflow", c.procwatch_overflow.get());
    s.count("holder_cuts", c.holder_cuts.get());
    s.count("fg_handbacks", c.fg_handbacks.get());
    s.count("parse_holds", c.parse_holds.get());
    s.count("parse_lock_waits", c.parse_lock_waits.get());
    s.decimal("parse_lock_wait_ms", ms3(c.parse_lock_wait_ns.get()));
    s.count("status_sweeps", c.status_sweeps.get());
    s.decimal("status_sweep_ms", ms3(c.status_sweep_ns.get()));
}

#[cfg(test)]
mod command_cost_tests {
    use super::{Metrics, emit_summary, text_field, wire};

    #[test]
    fn the_card_engine_cost_rides_the_summary_and_resets() {
        const KEYS: [&str; 12] = [
            "proc_events",
            "proc_drain_ms",
            "exec_edges",
            "procwatch_fallbacks",
            "procwatch_overflow",
            "holder_cuts",
            "fg_handbacks",
            "parse_holds",
            "parse_lock_waits",
            "parse_lock_wait_ms",
            "status_sweeps",
            "status_sweep_ms",
        ];
        let m = Metrics::new();
        let ctx = wire::ReadCtx::inert(&m, 0);
        let keys = wire::law_tests::body(|s| emit_summary(&ctx, None, s)).keys;
        let at = keys
            .iter()
            .position(|key| key == "pet_world_cells")
            .expect("pet_world_cells is a summary key");
        assert_eq!(keys[at + 1..at + 1 + KEYS.len()], KEYS);
        assert_eq!(keys[at + 1 + KEYS.len()], "fx_glow_quads_last");

        let c = &m.window.command;
        c.note_parse_hold(0);
        c.note_parse_hold(1_500_000);
        c.note_status_sweep(250_000);
        c.proc_events.add(3);
        c.proc_drain_ns.add(42_000);
        let text = wire::render_text(|s| emit_summary(&ctx, None, s));
        for (key, value) in [
            ("proc_events", "3"),
            ("proc_drain_ms", "0.042"),
            ("parse_holds", "2"),
            ("parse_lock_waits", "1"),
            ("parse_lock_wait_ms", "1.500"),
            ("status_sweeps", "1"),
            ("status_sweep_ms", "0.250"),
        ] {
            assert_eq!(text_field(&text, key), Some(value), "{key}: {text}");
        }

        m.reset();
        let text = wire::render_text(|s| emit_summary(&ctx, None, s));
        for key in KEYS {
            let value = text_field(&text, key).expect(key);
            assert!(
                value.bytes().all(|b| b == b'0' || b == b'.'),
                "{key}={value}"
            );
        }
    }
}

/// Times one status sweep that visits sessions (not the deadline gate's no-op), booked when it drops (every return path after it).
pub(crate) struct SweepTimer(Instant);

impl SweepTimer {
    pub(crate) fn start() -> Self {
        Self(Instant::now())
    }
}

impl Drop for SweepTimer {
    fn drop(&mut self) {
        note_status_sweep(leg_elapsed_ns(self.0));
    }
}

/// One cursor-effect tick's output.
pub(crate) fn note_fx_composed(glow_quads: usize, under_quads: usize, trail_cells: usize) {
    METRICS.note_fx_composed(glow_quads, under_quads, trail_cells);
}

type FxStream = (&'static str, fn(&aterm_render::RenderInput) -> usize);

static FX_STREAMS: [FxStream; 14] = [
    ("trail", |i| i.cursor_trail.len()),
    ("glow_add", |i| {
        i.cursor_glow_add_ramps.laid(i.cursor_glow_add.len())
    }),
    ("glow_halo", |i| i.glow_halo.len()),
    ("glow_under", |i| {
        i.glow_under_ramps.laid(i.glow_under.len())
    }),
    ("fire_patch", |i| i.fire_patch.len()),
    ("fire_halo", |i| i.fire_halo.len()),
    ("nova_add", |i| i.nova_add.len()),
    ("rain_rows", |i| {
        let rows = i.fx_layers.iter();
        rows.filter(|l| aterm_effects::matrix_rain::is_rain_row(l))
            .count()
    }),
    ("rain_add", |i| i.rain_add.len()),
    ("sprites", |i| {
        let layers = i.fx_layers.iter();
        layers
            .filter(|l| aterm_render::sprites::is_sprite_layer(l))
            .count()
    }),
    ("wdeco", |i| i.word_decorations.len()),
    ("ink", |i| i.ink.len()),
    ("char_fg", |i| i.char_fg.len()),
    ("fx_layers", |i| i.fx_layers.len()),
];

/// Book every effect stream's length off the final composed `input`, at the present seam ([`RedrawLedger::fx_streams`]).
pub(crate) fn note_fx_streams(input: &aterm_render::RenderInput) {
    METRICS.window.redraw.note_fx_streams(input);
}

impl RedrawLedger {
    fn note_fx_streams(&self, input: &aterm_render::RenderInput) {
        for (slot, (_, len)) in self.fx_streams.iter().zip(FX_STREAMS.iter()) {
            slot.set(len(input) as u64);
        }
    }
}

fn emit_fx_streams(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    let streams: Vec<(&str, u64)> = FX_STREAMS
        .iter()
        .zip(&ctx.metrics.window.redraw.fx_streams)
        .filter_map(|(&(label, _), slot)| {
            let n = slot.get();
            (n != 0).then_some((label, n))
        })
        .collect();
    s.count_pairs("fx_streams_last", &streams, "none");
}

#[cfg(test)]
mod effect_stream_tests {
    use super::{Metrics, emit_fx_streams, wire};

    #[test]
    fn effect_stream_fields_follow_the_final_composed_input() {
        use aterm_core::render::{CharFg, FxBlend, FxContent, FxLayer, FxSlot, FxSpace, InkCell};
        let m = Metrics::new();
        let ctx = wire::ReadCtx::inert(&m, 0);
        let layer = FxLayer::new(
            7,
            std::sync::Arc::new(FxContent::default()),
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Exact,
        );
        let input = aterm_render::RenderInput {
            glow_under: vec![aterm_render::GlowQuad::default(); 3],
            nova_add: vec![aterm_render::GlowQuad::default(); 2],
            ink: vec![
                InkCell {
                    row: 0,
                    col: 0,
                    color: [1, 2, 3],
                };
                4
            ],
            char_fg: vec![
                CharFg {
                    row: 0,
                    col: 0,
                    fg: 0
                };
                5
            ],
            fx_layers: vec![layer],
            ..aterm_render::RenderInput::default()
        };
        m.window.redraw.note_fx_streams(&input);
        assert_eq!(
            wire::render_text(|s| emit_fx_streams(&ctx, s)),
            " fx_streams_last=glow_under:3,nova_add:2,ink:4,char_fg:5,fx_layers:1"
        );
        assert_eq!(
            wire::render_json(|s| emit_fx_streams(&ctx, s)),
            "{\"fx_streams_last\":{\"glow_under\":3,\"nova_add\":2,\"ink\":4,\"char_fg\":5,\
             \"fx_layers\":1}}"
        );
        m.window
            .redraw
            .note_fx_streams(&aterm_render::RenderInput::default());
        assert_eq!(
            wire::render_text(|s| emit_fx_streams(&ctx, s)),
            " fx_streams_last=none"
        );
        assert_eq!(
            wire::render_json(|s| emit_fx_streams(&ctx, s)),
            "{\"fx_streams_last\":{}}"
        );
    }
}

pub(crate) fn note_redraw_sync_hold() {
    METRICS.window.redraw.sync_holds.inc();
}

pub(crate) fn note_redraw_retry_gated() {
    METRICS.window.redraw.retry_gated.inc();
}

pub(crate) fn note_pre_present(compose_ns: u64) {
    METRICS.window.redraw.note_pre_present(compose_ns);
}

pub(crate) fn note_present_drop(reason: PresentDropReason, parked: bool) {
    METRICS.note_present_drop(reason, parked);
}

pub(crate) fn update_present_drop_disposition(reason: PresentDropReason, parked: bool) {
    METRICS.window.redraw.set_drop_disposition(reason, parked);
}

impl RedrawLedger {
    fn note_refill(&self, refill: aterm_core::render::FrameRefill) {
        match refill {
            aterm_core::render::FrameRefill::Scoped { .. } => self.refills_scoped.inc(),
            aterm_core::render::FrameRefill::Full { cause } => {
                self.refills_full.inc();
                self.refill_cause.inc(cause.index());
            }
        }
    }

    fn note_pet_world(&self, cells: usize) {
        self.pet_world_observations.inc();
        self.pet_world_cells.add(cells as u64);
    }

    fn note_pre_present(&self, compose_ns: u64) {
        self.pre_present_attempts.inc();
        self.pre_present_total.add(compose_ns);
        let _ = self.pre_present.record(compose_ns);
    }

    fn set_drop_disposition(&self, reason: PresentDropReason, parked: bool) {
        self.drop_reason.set(reason.index());
        self.drop_parked.set(parked);
    }
}

impl Metrics {
    fn note_fx_composed(&self, glow_quads: usize, under_quads: usize, trail_cells: usize) {
        let live = &self.live;
        live.fx_glow_quads_last
            .store(glow_quads as u64, Ordering::Relaxed);
        live.fx_under_quads_last
            .store(under_quads as u64, Ordering::Relaxed);
        live.fx_trail_cells_last
            .store(trail_cells as u64, Ordering::Relaxed);
        if under_quads > 0 {
            self.window.redraw.fx_under_frames.inc();
        }
    }

    fn note_present_drop(&self, reason: PresentDropReason, parked: bool) {
        self.window.redraw.drops.inc();
        self.window.redraw.set_drop_disposition(reason, parked);
        if drop_taints_present_latency(reason, parked) {
            self.arm_present_taint();
        }
    }
}

pub(crate) fn note_deadline(
    owner: DeadlineOwner,
    requested: Option<Instant>,
    now: Instant,
    heal: Heal,
) {
    METRICS.note_deadline(owner, requested, now, heal);
}

pub(crate) fn note_gpu_repaint_totals(scissor_taken: u64, full_repaints: u64, scroll_rescues: u64) {
    let scope = &METRICS.window.gpu_scope;
    scope.scissor_taken.set_total(scissor_taken);
    scope.full_repaints.set_total(full_repaints);
    scope.scroll_rescues.set_total(scroll_rescues);
}

pub(crate) fn note_event_wake(kind: EventWakeKind) {
    METRICS.note_event_wake(kind);
}

impl Metrics {
    fn note_deadline(
        &self,
        owner: DeadlineOwner,
        requested: Option<Instant>,
        now: Instant,
        heal: Heal,
    ) {
        let sched = &self.window.sched;
        let clock_now = now_ns();
        if heal.floor {
            sched.stale_arm_heals.inc();
        }
        if heal.streak {
            sched.streak_heals_by_owner.inc(owner.index());
        }
        let (owner, due, late) = match requested {
            None => (DeadlineOwner::None, 0, 0),
            Some(deadline) if deadline >= now => {
                let ahead =
                    u64::try_from(deadline.duration_since(now).as_nanos()).unwrap_or(u64::MAX);
                sched.arms_by_owner.inc(owner.index());
                (owner, clock_now.saturating_add(ahead), 0)
            }
            Some(deadline) => {
                let late =
                    u64::try_from(now.duration_since(deadline).as_nanos()).unwrap_or(u64::MAX);
                sched.past_arms.inc();
                sched.arms_by_owner.inc(owner.index());
                sched.past_arms_by_owner.inc(owner.index());
                (owner, clock_now.saturating_sub(late), late)
            }
        };
        let live = &self.live;
        live.last_deadline_owner
            .store(owner.index() as u64, Ordering::Relaxed);
        live.last_deadline_due.store(due, Ordering::Relaxed);
        live.last_deadline_late.store(late, Ordering::Relaxed);
        sched.deadline_late.record(late, owner.index(), clock_now);
    }

    fn note_event_wake(&self, kind: EventWakeKind) {
        let (sched, live) = (&self.window.sched, &self.live);
        sched.event_wakes.inc();
        live.last_wake_kind
            .store(kind.index() as u64, Ordering::Relaxed);
        let owner = wake_owner(
            kind,
            DeadlineOwner::from_raw(live.last_deadline_owner.load(Ordering::Relaxed)),
        );
        live.last_wake_owner
            .store(owner.index() as u64, Ordering::Relaxed);
        let late = match kind {
            EventWakeKind::Timer => {
                sched.timer_wakes.inc();
                sched.fires_by_owner.inc(owner.index());
                let due = live.last_deadline_due.load(Ordering::Relaxed);
                let at = now_ns();
                let late = if due == 0 { 0 } else { at.saturating_sub(due) };
                sched.wake_late.record(late, owner.index(), at);
                late
            }
            EventWakeKind::WaitCancelled => {
                sched.wait_cancelled_wakes.inc();
                0
            }
            EventWakeKind::Poll => {
                sched.poll_wakes.inc();
                0
            }
            EventWakeKind::Init | EventWakeKind::None => 0,
        };
        live.last_wake_late.store(late, Ordering::Relaxed);
    }
}

const fn wake_owner(kind: EventWakeKind, deadline_owner: DeadlineOwner) -> DeadlineOwner {
    if matches!(kind, EventWakeKind::Timer) {
        deadline_owner
    } else {
        DeadlineOwner::None
    }
}

pub(crate) fn set_backend_gpu(on: bool) {
    METRICS.live.backend_gpu.store(on, Ordering::Relaxed);
}

#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) static HANDOFF_INPUT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn note_handoff_input_dropped() {
    METRICS
        .live
        .handoff_input_dropped
        .fetch_add(1, Ordering::Relaxed);
}

#[cfg(any(unix, test))]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn note_handoff_input_incoherent() {
    METRICS
        .live
        .handoff_input_incoherent
        .fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
#[must_use]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn handoff_input_counts() -> (u64, u64) {
    let live = &METRICS.live;
    (
        live.handoff_input_dropped.load(Ordering::Relaxed),
        live.handoff_input_incoherent.load(Ordering::Relaxed),
    )
}

fn emit_handoff_input(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    let live = &ctx.metrics.live;
    s.count(
        "input_dropped",
        live.handoff_input_dropped.load(Ordering::Relaxed),
    );
    s.count(
        "input_incoherent",
        live.handoff_input_incoherent.load(Ordering::Relaxed),
    );
}

#[derive(Debug)]
pub(crate) struct PtyCensus(CensusPlace);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CensusPlace {
    None,
    Session,
    Spare,
}

impl PtyCensus {
    pub(crate) fn counted() -> Self {
        METRICS.live.pty_sessions.fetch_add(1, Ordering::AcqRel);
        Self(CensusPlace::Session)
    }

    pub(crate) fn spare() -> Self {
        METRICS.live.spare_ptys.fetch_add(1, Ordering::AcqRel);
        Self(CensusPlace::Spare)
    }

    pub(crate) const fn none() -> Self {
        Self(CensusPlace::None)
    }
}

impl Drop for PtyCensus {
    fn drop(&mut self) {
        match std::mem::replace(&mut self.0, CensusPlace::None) {
            CensusPlace::Session => {
                METRICS.live.pty_sessions.fetch_sub(1, Ordering::AcqRel);
            }
            CensusPlace::Spare => {
                METRICS.live.spare_ptys.fetch_sub(1, Ordering::AcqRel);
            }
            CensusPlace::None => {}
        }
    }
}

pub(crate) fn spare_ptys() -> usize {
    METRICS.live.spare_ptys.load(Ordering::Acquire)
}

#[must_use]
pub(crate) fn pty_kits() -> usize {
    #[cfg(unix)]
    {
        aterm_pty::ready_spawn_kits()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn emit_pty_census(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    let live = &ctx.metrics.live;
    let (masters, kits) = if ctx.reads_process() {
        (
            aterm_session::sink::owned_masters_open()
                .saturating_sub(live.spare_ptys.load(Ordering::Acquire)),
            pty_kits(),
        )
    } else {
        (0, 0)
    };
    s.count("pty_masters", masters as u64);
    s.count(
        "pty_sessions",
        live.pty_sessions.load(Ordering::Acquire) as u64,
    );
    s.count("pty_kits", kits as u64);
}

pub(crate) fn reset() {
    METRICS.reset();
    wakes::WAKES.reset();
    crate::watchdog::reset_turn_census();
}

pub(crate) fn emit_summary(
    ctx: &wire::ReadCtx,
    dims: Option<(u32, u32, u64)>,
    s: &mut dyn wire::FieldSink,
) {
    use wire::{ms, ms1};
    let w = &ctx.metrics.window;
    let live = &ctx.metrics.live;
    let frames = w.present.frames.get_acquire();
    let startup = if ctx.reads_process() {
        startup::STARTUP.facts()
    } else {
        startup::StartupFacts::default()
    };
    let (h_input, h_present) = (w.input.input.copy_out(), w.present.latency.copy_out());
    let backend_gpu = live.backend_gpu.load(Ordering::Relaxed);
    s.label("backend", if backend_gpu { "gpu" } else { "cpu" });
    s.opt_count("rows", dims.map(|(rows, _, _)| u64::from(rows)), "busy");
    s.opt_count("cols", dims.map(|(_, cols, _)| u64::from(cols)), "busy");
    s.opt_count(
        "scrollback_truncated_lines",
        dims.map(|(_, _, truncated)| truncated),
        "busy",
    );
    let journal_resident = if ctx.reads_process() {
        crate::crash_restore::resident_bytes()
    } else {
        0
    };
    s.count("journal_resident_bytes", journal_resident);
    s.count("frames", frames);
    s.decimal("last_present_latency_ms", ms(w.present.latency.last()));
    s.decimal("max_present_latency_ms", ms(w.present.latency.max()));
    s.decimal("last_frame_render_ms", ms(w.present.render.last()));
    s.decimal("max_frame_render_ms", ms(w.present.render.max()));
    s.count("slow_frames", w.present.slow.get());
    s.decimal("slow_threshold_ms", ms1(SLOW_FRAME_THRESHOLD_NS));
    s.decimal("last_input_present_ms", ms(w.input.input.last()));
    s.decimal("max_input_present_ms", ms(w.input.input.max()));
    emit_quantiles(
        s,
        ["n_input", "input_p50_ms", "input_p95_ms", "input_p99_ms"],
        &h_input,
    );
    emit_quantiles(
        s,
        [
            "n_present",
            "present_p50_ms",
            "present_p95_ms",
            "present_p99_ms",
        ],
        &h_present,
    );
    s.decimal("last_key_write_ms", ms(w.input.key_write.last()));
    s.decimal("max_key_write_ms", ms(w.input.key_write.max()));
    s.decimal(
        "last_resize_present_ms",
        ms(w.present.resize_present.last()),
    );
    s.decimal("max_resize_present_ms", ms(w.present.resize_present.max()));
    s.decimal("last_resize_reflow_ms", ms(w.present.resize_reflow.last()));
    s.decimal("max_resize_reflow_ms", ms(w.present.resize_reflow.max()));
    s.count("sync_armed", w.sync.holds_armed.get());
    s.count("sync_rel_end", w.sync.releases_end.get());
    s.count("sync_rel_timeout", w.sync.releases_timeout.get());
    s.flag(
        "sync_holding",
        live.sync_holding_windows.load(Ordering::Relaxed) != 0,
    );
    s.flag("perf_reduced", live.perf_reduced.load(Ordering::Relaxed));
    s.count("shed_transitions", w.sync.shed_transitions.get());
    s.count("wake_heals", w.sync.wake_heals.get());
    s.count("output_wake_late", w.sync.output_wake_late.get());
    s.count("output_wake_reposts", w.sync.output_wake_reposts.get());
    s.decimal("last_redraw_total_ms", ms(w.redraw.total.last()));
    s.decimal("max_redraw_total_ms", ms(w.redraw.total.max()));
    s.count("redraw_attempts", w.redraw.attempts.get());
    s.count("redraw_early_outs", w.redraw.early_outs.get());
    s.count("redraw_sync_holds", w.redraw.sync_holds.get());
    s.count("redraw_retry_gated", w.redraw.retry_gated.get());
    s.count("frame_refills_scoped", w.redraw.refills_scoped.get());
    s.count("frame_refills_full", w.redraw.refills_full.get());
    s.count("frame_refills_skipped", w.redraw.refills_skipped.get());
    s.count(
        "pet_world_observations",
        w.redraw.pet_world_observations.get(),
    );
    s.count("pet_world_cells", w.redraw.pet_world_cells.get());
    emit_command_cost(&w.command, s);
    s.count(
        "fx_glow_quads_last",
        live.fx_glow_quads_last.load(Ordering::Relaxed),
    );
    s.count(
        "fx_under_quads_last",
        live.fx_under_quads_last.load(Ordering::Relaxed),
    );
    s.count(
        "fx_trail_cells_last",
        live.fx_trail_cells_last.load(Ordering::Relaxed),
    );
    s.count("fx_under_frames", w.redraw.fx_under_frames.get());
    emit_fx_streams(ctx, s);
    s.count_pairs(
        "frame_refill_full_causes",
        &w.redraw.refill_cause.sparse(
            aterm_core::render::FullRefillCause::ALL
                .iter()
                .map(|cause| (cause.index(), cause.as_str())),
        ),
        "none",
    );
    s.count("offscreen_rasters", w.present.offscreen_rasters.get());
    s.decimal("last_offscreen_raster_ms", ms(w.present.offscreen.last()));
    s.decimal("max_offscreen_raster_ms", ms(w.present.offscreen.max()));
    s.count("pre_present_attempts", w.redraw.pre_present_attempts.get());
    s.decimal("last_pre_present_ms", ms(w.redraw.pre_present.last()));
    s.decimal("pre_present_total_ms", ms(w.redraw.pre_present_total.get()));
    s.decimal("max_pre_present_ms", ms(w.redraw.pre_present.max()));
    s.decimal("last_acquire_wait_ms", ms(w.redraw.acquire_wait.last()));
    s.decimal("max_acquire_wait_ms", ms(w.redraw.acquire_wait.max()));
    s.decimal("last_gpu_park_ms", ms(w.redraw.gpu_park.last()));
    s.decimal("max_gpu_park_ms", ms(w.redraw.gpu_park.max()));
    s.count("present_drops", w.redraw.drops.get());
    s.label(
        "last_present_drop_reason",
        PresentDropReason::from_raw(w.redraw.drop_reason.get()).as_str(),
    );
    s.flag("last_present_drop_parked", w.redraw.drop_parked.get());
    s.count("present_tainted", w.present.tainted_samples.get());
    s.decimal("last_present_tainted_ms", ms(w.present.tainted.last()));
    s.decimal("max_present_tainted_ms", ms(w.present.tainted.max()));
    s.count("capture_episodes", w.present.capture_episodes.get());
    s.count("capture_presents", w.present.capture_presents.get());
    s.flag(
        "capture_active",
        live.capture_depth.load(Ordering::Relaxed) != 0,
    );
    s.count("event_wakes", w.sched.event_wakes.get());
    s.count("timer_wakes", w.sched.timer_wakes.get());
    s.count("wait_cancelled_wakes", w.sched.wait_cancelled_wakes.get());
    s.count("poll_wakes", w.sched.poll_wakes.get());
    s.label(
        "wake_kind",
        EventWakeKind::from_raw(live.last_wake_kind.load(Ordering::Relaxed)).as_str(),
    );
    s.label(
        "wake_owner",
        DeadlineOwner::from_raw(live.last_wake_owner.load(Ordering::Relaxed)).as_str(),
    );
    s.decimal(
        "wake_late_ms",
        ms(live.last_wake_late.load(Ordering::Relaxed)),
    );
    s.label(
        "deadline_owner",
        DeadlineOwner::from_raw(live.last_deadline_owner.load(Ordering::Relaxed)).as_str(),
    );
    let deadline_due = live.last_deadline_due.load(Ordering::Relaxed);
    let deadline_in_ns = if deadline_due == 0 {
        0
    } else {
        deadline_due.saturating_sub(ctx.now_ns)
    };
    s.decimal("deadline_in_ms", ms(deadline_in_ns));
    s.decimal(
        "deadline_late_ms",
        ms(live.last_deadline_late.load(Ordering::Relaxed)),
    );
    s.count("past_deadline_arms", w.sched.past_arms.get());
    let arms: Vec<(&str, [(&str, wire::Cell<'_>); 2])> = DeadlineOwner::ALL
        .into_iter()
        .filter_map(|owner| {
            let arms = w.sched.arms_by_owner.get(owner.index());
            let past = w.sched.past_arms_by_owner.get(owner.index());
            (arms != 0 || past != 0).then_some((
                owner.as_str(),
                [
                    ("arms", wire::Cell::Count(arms)),
                    ("past", wire::Cell::Count(past)),
                ],
            ))
        })
        .collect();
    let arm_pairs: Vec<(&str, wire::Cell<'_>)> = arms
        .iter()
        .map(|(owner, record)| (*owner, wire::Cell::Rec(record)))
        .collect();
    s.pairs("deadline_arms_by_owner", &arm_pairs, "none");
    s.count_pairs(
        "deadline_fires_by_owner",
        &w.sched.fires_by_owner.sparse(owner_labels()),
        "none",
    );
    let wakes = if ctx.reads_process() {
        wakes::WAKES.attribution()
    } else {
        Vec::new()
    };
    s.count_pairs("wakes_by_variant", &wakes, "none");
    s.count("gpu_scissor_taken", w.gpu_scope.scissor_taken.since_reset());
    s.count("gpu_full_repaints", w.gpu_scope.full_repaints.since_reset());
    s.count(
        "gpu_scroll_rescues",
        w.gpu_scope.scroll_rescues.since_reset(),
    );
    s.count_pairs(
        "past_arm_streak_heals",
        &w.sched.streak_heals_by_owner.sparse(owner_labels()),
        "none",
    );
    s.count("stale_arm_heals", w.sched.stale_arm_heals.get());
    emit_lateness(ctx, s);
    s.decimal("max_frame_gap_ms", ms(w.present.frame_gap.get()));
    startup.emit_span(s);
    if ctx.reads_process() {
        crate::echo_rtt::emit_metrics(s);
        crate::watchdog::emit_turn_census(ctx, s);
    }
    emit_handoff_input(ctx, s);
    if ctx.reads_process() {
        startup::STARTUP.emit_boot(&startup, s);
        crate::control_listener::emit_metrics(ctx, s);
    }
    emit_pty_census(ctx, s);
    if ctx.reads_process() {
        crate::spare::emit_metrics(s);
    }
    emit_gpu(ctx, s);
}

pub(crate) fn emit_percentiles(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    use wire::ms;
    let w = &ctx.metrics.window;
    emit_quantiles(
        s,
        ["n_input", "input_p50_ms", "input_p95_ms", "input_p99_ms"],
        &w.input.input.copy_out(),
    );
    s.count("input_edges_dropped", w.input.edges_dropped.get());
    s.count("input_predicted", w.input.predicted.get());
    emit_quantiles(
        s,
        [
            "n_present",
            "present_p50_ms",
            "present_p95_ms",
            "present_p99_ms",
        ],
        &w.present.latency.copy_out(),
    );
    emit_quantiles(
        s,
        [
            "n_render",
            "render_p50_ms",
            "render_p95_ms",
            "render_p99_ms",
        ],
        &w.present.render.copy_out(),
    );
    emit_quantiles(
        s,
        [
            "n_key_write",
            "key_write_p50_ms",
            "key_write_p95_ms",
            "key_write_p99_ms",
        ],
        &w.input.key_write.copy_out(),
    );
    emit_quantiles(
        s,
        [
            "n_key_queue",
            "key_queue_p50_ms",
            "key_queue_p95_ms",
            "key_queue_p99_ms",
        ],
        &w.input.key_queue.copy_out(),
    );
    s.decimal("last_key_queue_ms", ms(w.input.key_queue.last()));
    s.decimal("max_key_queue_ms", ms(w.input.key_queue.max()));
    emit_quantiles(
        s,
        [
            "n_pre_present",
            "pre_present_p50_ms",
            "pre_present_p95_ms",
            "pre_present_p99_ms",
        ],
        &w.redraw.pre_present.copy_out(),
    );
    emit_quantiles(
        s,
        [
            "n_acquire",
            "acquire_p50_ms",
            "acquire_p95_ms",
            "acquire_p99_ms",
        ],
        &w.redraw.acquire_wait.copy_out(),
    );
    s.decimal("last_acquire_wait_ms", ms(w.redraw.acquire_wait.last()));
    s.decimal("max_acquire_wait_ms", ms(w.redraw.acquire_wait.max()));
    emit_quantiles(
        s,
        [
            "n_acquire_queue",
            "acquire_queue_p50_ms",
            "acquire_queue_p95_ms",
            "acquire_queue_p99_ms",
        ],
        &w.redraw.acquire_queue.copy_out(),
    );
    s.decimal("last_acquire_queue_ms", ms(w.redraw.acquire_queue.last()));
    s.decimal("max_acquire_queue_ms", ms(w.redraw.acquire_queue.max()));
    emit_quantiles(
        s,
        [
            "n_resize",
            "resize_p50_ms",
            "resize_p95_ms",
            "resize_p99_ms",
        ],
        &w.present.resize_present.copy_out(),
    );
    emit_quantiles(
        s,
        [
            "n_reflow",
            "reflow_p50_ms",
            "reflow_p95_ms",
            "reflow_p99_ms",
        ],
        &w.present.resize_reflow.copy_out(),
    );
    emit_quantiles(
        s,
        [
            "n_present_tainted",
            "present_tainted_p50_ms",
            "present_tainted_p95_ms",
            "present_tainted_p99_ms",
        ],
        &w.present.tainted.copy_out(),
    );
    if ctx.reads_process() {
        crate::watchdog::emit_turns_by_root(s);
        s.decimal(
            "main_busy_ms_per_s",
            wire::ms_f64(crate::watchdog::main_busy_ms_per_s(ctx.now_ns)),
        );
    }
    for site in TermWaitSite::ALL {
        let label = site.as_str();
        emit_quantiles(
            s,
            [
                &format!("n_term_wait_{label}"),
                &format!("term_wait_{label}_p50_ms"),
                &format!("term_wait_{label}_p95_ms"),
                &format!("term_wait_{label}_p99_ms"),
            ],
            &w.redraw.term_wait[site.index()].copy_out(),
        );
        s.decimal(
            &format!("max_term_wait_{label}_ms"),
            ms(w.redraw.term_wait_max[site.index()].get()),
        );
    }
    if ctx.reads_process() {
        crate::echo_rtt::emit_metrics(s);
    }
    emit_present_glass(ctx, s);
    emit_input_legs(ctx, s);
    emit_gpu(ctx, s);
}

fn emit_quantiles(s: &mut dyn wire::FieldSink, keys: [&str; 4], h: &HistCopy) {
    emit_quantiles_in(s, keys, h, wire::ms);
}

fn emit_quantiles_ms3(s: &mut dyn wire::FieldSink, keys: [&str; 4], h: &HistCopy) {
    emit_quantiles_in(s, keys, h, wire::ms3);
}

fn emit_quantiles_in(
    s: &mut dyn wire::FieldSink,
    keys: [&str; 4],
    h: &HistCopy,
    unit: fn(u64) -> wire::Decimal,
) {
    let [n, p50, p95, p99] = keys;
    let quantile = |q: f64| unit(h.percentile(q).unwrap_or(0));
    s.count(n, h.n());
    s.decimal(p50, quantile(0.50));
    s.decimal(p95, quantile(0.95));
    s.decimal(p99, quantile(0.99));
}

#[cfg(test)]
fn inert_text(m: &Metrics, now_ns: u64) -> String {
    let ctx = wire::ReadCtx::inert(m, now_ns);
    let mut text = wire::render_text(|s| emit_summary(&ctx, Some((24, 80, 7)), s));
    text.push_str(&wire::render_text(|s| emit_percentiles(&ctx, s)));
    text
}

#[cfg(test)]
fn text_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split_whitespace()
        .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
}

#[cfg(test)]
mod ledger_tests {
    use super::*;

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
    }

    #[test]
    fn arms_are_attributed_to_the_owner_that_armed_them() {
        let m = Metrics::new();
        let now = Instant::now();
        let future = now + Duration::from_millis(5);
        let stale = now - Duration::from_millis(1);
        let passed = |deadline: Option<Instant>| Heal {
            armed: deadline,
            floor: false,
            streak: false,
        };
        let sched = &m.window.sched;
        let ledger = |owner: DeadlineOwner| {
            (
                sched.arms_by_owner.get(owner.index()),
                sched.past_arms_by_owner.get(owner.index()),
                sched.streak_heals_by_owner.get(owner.index()),
            )
        };

        m.note_deadline(
            DeadlineOwner::SessionStatus,
            Some(stale),
            now,
            passed(Some(stale)),
        );
        m.note_deadline(
            DeadlineOwner::Blink,
            Some(future),
            now,
            passed(Some(future)),
        );
        m.note_deadline(
            DeadlineOwner::SessionStatus,
            Some(stale),
            now,
            Heal {
                armed: Some(now + Duration::from_millis(250)),
                floor: true,
                streak: true,
            },
        );

        assert_eq!(
            ledger(DeadlineOwner::SessionStatus),
            (2, 2, 1),
            "both stale arms and the streak heal are booked to the owner that armed them"
        );
        assert_eq!(
            ledger(DeadlineOwner::Blink),
            (1, 0, 0),
            "a healthy arm counts as an arm and NOT as a past arm"
        );
        assert_eq!(sched.past_arms.get(), 2);
        assert_eq!(sched.stale_arm_heals.get(), 1);
        assert_eq!(
            m.live.last_deadline_late.load(Ordering::Relaxed),
            1_000_000,
            "the lateness is the requested deadline's, not the clamp's"
        );

        let text = inert_text(&m, 0);
        assert_eq!(
            text_field(&text, "deadline_arms_by_owner"),
            Some("blink:1/0,session_status:2/2"),
            "{text}"
        );
        assert_eq!(
            text_field(&text, "past_arm_streak_heals"),
            Some("session_status:1"),
            "the streak ledger NAMES the producer it clamped: {text}"
        );
    }

    #[test]
    fn only_unbounded_present_stoppages_taint_the_on_glass_ledger() {
        for reason in [
            PresentDropReason::None,
            PresentDropReason::GpuReconfigured,
            PresentDropReason::GpuTimeout,
            PresentDropReason::GpuValidation,
            PresentDropReason::CpuResize,
            PresentDropReason::CpuAcquire,
            PresentDropReason::CpuCommit,
            PresentDropReason::TargetMismatch,
            PresentDropReason::Virtual,
            PresentDropReason::CpuRaster,
        ] {
            assert!(
                !drop_taints_present_latency(reason, false),
                "{} retries; the stall it caused is REAL and stays on glass",
                reason.as_str()
            );
            assert!(
                drop_taints_present_latency(reason, true),
                "{} parked: nothing reaches glass until an external stimulus",
                reason.as_str()
            );
        }
        assert!(
            drop_taints_present_latency(PresentDropReason::GpuOccluded, false),
            "occluded glass is the canonical unwatched interval"
        );
    }

    #[test]
    fn the_taint_window_covers_live_captures_and_the_episode_tail() {
        assert!(
            !tainted_at(false, 1_000, 0, 0),
            "no episode, no capture: the sample is aterm's own"
        );
        assert!(
            tainted_at(false, 1_000, 2_000, 0),
            "inside the episode tail"
        );
        assert!(
            !tainted_at(false, 2_000, 2_000, 0),
            "the tail is exclusive: the boundary sample is clean again"
        );
        assert!(
            tainted_at(true, 1_000, 0, 0),
            "a present a capture forced taints with no episode and no tail"
        );
        assert!(
            tainted_at(false, u64::MAX, 0, 1),
            "a LIVE capture taints regardless of the clock — the recorder is              pacing presents right now"
        );
    }

    #[test]
    fn capture_episodes_nest_and_never_underflow() {
        let m = Metrics::new();
        let active = |m: &Metrics| m.live.capture_depth.load(Ordering::Relaxed) != 0;
        assert!(!active(&m), "no capture is running");
        m.note_capture_episode(true);
        assert!(active(&m));
        m.note_capture_episode(true);
        m.note_capture_episode(false);
        assert!(
            active(&m),
            "one close does not end two overlapping captures"
        );
        m.note_capture_episode(false);
        assert!(!active(&m), "the last close ends the episode");
        m.note_capture_episode(false);
        assert!(
            !active(&m),
            "an unpaired close saturates at zero instead of wrapping"
        );
        assert_eq!(
            m.window.present.capture_episodes.get(),
            2,
            "each opening is one episode"
        );
        assert!(
            PresentClock::read(&m, false).tainted,
            "the closing tail is still armed"
        );
    }
}

#[cfg(test)]
mod present_glass_tests {
    use super::{Metrics, emit_present_glass, wire};
    use aterm_gpu::present_glass::{GlassReport, GlassSample, PresentTag};

    fn report(sample: GlassSample, tag: PresentTag, successor: PresentTag) -> GlassReport {
        GlassReport {
            sample,
            tag,
            successor,
        }
    }

    #[test]
    fn every_ablation_mode_reads_back_as_itself() {
        use aterm_gpu::UnormAblation::{Drop, Off, Thin};
        for mode in [Off, Drop, Thin] {
            assert_eq!(super::ablation_of(super::ablation_code(mode)), Some(mode));
        }
        assert_eq!(super::ablation_of(3), None);
        let m = super::Metrics::new();
        m.window.gpu.unorm_ablation.set(3);
        assert_eq!(m.window.gpu.unorm_ablation_name(), "unknown");
    }

    #[test]
    fn every_unorm_route_reads_back_as_itself() {
        use aterm_gpu::UnormRoute::{Fetch, Split};
        for route in [Split, Fetch] {
            assert_eq!(super::route_of(super::route_code(route)), Some(route));
        }
        assert_eq!(super::route_of(2), None);
        let m = super::Metrics::new();
        m.window.gpu.unorm_route.set(2);
        assert_eq!(m.window.gpu.unorm_route_name(), "unknown");
    }

    #[test]
    fn every_fetch_verdict_publishes_its_own_name() {
        use aterm_gpu::{FetchProbe, FetchStage};
        let failed = FetchProbe::Failed {
            stage: FetchStage::InPass,
            channel: 1,
            want: 2,
            got: 3,
        };
        for verdict in [
            FetchProbe::Unsupported,
            FetchProbe::Passed,
            failed,
            FetchProbe::Refused,
        ] {
            assert_eq!(
                super::probe_name(u64::from(super::probe_code(Some(verdict)))),
                verdict.name()
            );
        }
        assert_eq!(
            super::probe_name(u64::from(super::probe_code(None))),
            "pending"
        );
        assert_eq!(super::probe_name(5), "unknown");
    }

    #[test]
    fn glass_samples_book_on_glass_and_skipped_apart() {
        let m = Metrics::new();
        let ctx = wire::ReadCtx::inert(&m, 0);
        let text = wire::render_text(|s| emit_present_glass(&ctx, s));
        assert!(
            text.ends_with(" present_glass_shown_by=none present_glass_skipped_by=none"),
            "{text}"
        );
        let json = wire::render_json(|s| emit_present_glass(&ctx, s));
        assert!(
            json.ends_with(",\"present_glass_skipped_by\":{}}"),
            "{json}"
        );

        let glass = &m.window.glass;
        glass.note(report(
            GlassSample::OnGlass { ns: 7_000_000 },
            PresentTag::Spaced,
            PresentTag::Spaced,
        ));
        glass.note(report(
            GlassSample::Skipped,
            PresentTag::Spaced,
            PresentTag::HotBurst,
        ));
        assert_eq!(glass.latency.copy_out().n(), 1, "a skip is not a duration");
        assert_eq!(glass.latency.max(), 7_000_000);
        assert_eq!(glass.skipped.get(), 1);
        let text = wire::render_text(|s| emit_present_glass(&ctx, s));
        assert!(text.contains(" n_present_glass=1 "), "{text}");
        assert!(text.contains(" max_present_glass_ms=7.00 "), "{text}");
        assert!(text.contains(" present_glass_skipped=1 "), "{text}");
        assert!(text.contains(" present_glass_shown_by=spaced:1 "), "{text}");
        assert!(
            text.ends_with(" present_glass_skipped_by=hot_burst:1"),
            "{text}"
        );
        let json = wire::render_json(|s| emit_present_glass(&ctx, s));
        assert!(json.contains("\"n_present_glass\":1,"), "{json}");
        assert!(json.contains("\"present_glass_skipped\":1,"), "{json}");
        assert!(
            json.contains("\"present_glass_shown_by\":{\"spaced\":1}"),
            "{json}"
        );
        assert!(
            json.ends_with("\"present_glass_skipped_by\":{\"hot_burst\":1}}"),
            "{json}"
        );
    }
}

#[cfg(test)]
mod gpu_tests {
    use super::{Metrics, emit_gpu, text_field, wire};

    #[test]
    fn gpu_time_and_frame_shape_are_published_and_reset() {
        let m = Metrics::new();
        let ctx = wire::ReadCtx::inert(&m, 0);
        let text = || wire::render_text(|s| emit_gpu(&ctx, s));
        let json = || {
            aterm_json::from_str::<aterm_json::Value>(&wire::render_json(|s| emit_gpu(&ctx, s)))
                .expect("one JSON object")
        };
        let empty_source = if aterm_gpu::gpu_time::SUPPORTED {
            "unobserved"
        } else {
            "unsupported"
        };
        let empty = text();
        assert_eq!(text_field(&empty, "gpu_time_source"), Some(empty_source));
        assert_eq!(text_field(&empty, "instances_by_stream"), Some("none"));

        let mut shape = aterm_gpu::FrameShape {
            passes: 3,
            instance_bytes: 512,
            build_ns: 2_000_000,
            stage_ns: 3_000_000,
            ablation: aterm_gpu::UnormAblation::Thin,
            route: aterm_gpu::UnormRoute::Fetch,
            fetch_probe: Some(aterm_gpu::FetchProbe::Passed),
            fx_instances: 6,
            fx_upload_bytes: 1_216,
            fx_refused: 2,
            ..aterm_gpu::FrameShape::default()
        };
        let glyph = aterm_gpu::FrameShape::STREAMS
            .iter()
            .position(|&name| name == "glyph")
            .expect("a glyph stream");
        shape.instances[glyph] = 8;
        m.window.gpu.note_frame(shape, 5_000_000);
        m.window.gpu.note_time(aterm_gpu::gpu_time::GpuFrameTime {
            serial: 1,
            gpu_ns: 7_000_000,
            parts: 2,
        });
        let published = text();
        for field in [
            "n_gpu_time=1",
            "max_gpu_time_ms=7.00",
            "gpu_time_parts_last=2",
            "n_gpu_encode=1",
            "gpu_encode_last_ms=5.00",
            "passes_last=3",
            "instances_by_stream=glyph:8",
            "instance_bytes_last=512",
            "gpu_build_last_ms=2.00",
            "gpu_stage_last_ms=3.00",
            "gpu_unorm_ablation=thin",
            "gpu_unorm_route=fetch",
            "gpu_fetch_probe=passed",
            "fx_instances_last=6",
            "fx_upload_bytes_last=1216",
            "fx_refused_last=2",
        ] {
            assert!(
                published.split_whitespace().any(|token| token == field),
                "{field}: {published}"
            );
        }
        let object = json();
        assert_eq!(object["n_gpu_time"].as_u64(), Some(1));
        assert_eq!(object["instances_by_stream"]["glyph"].as_u64(), Some(8));
        assert_eq!(object["gpu_unorm_ablation"].as_str(), Some("thin"));
        assert_eq!(object["gpu_unorm_route"].as_str(), Some("fetch"));
        assert_eq!(object["gpu_fetch_probe"].as_str(), Some("passed"));
        assert_eq!(object["fx_instances_last"].as_u64(), Some(6));
        assert_eq!(object["fx_upload_bytes_last"].as_u64(), Some(1_216));
        assert_eq!(object["fx_refused_last"].as_u64(), Some(2));
        assert_eq!(
            object["gpu_time_source"].as_str(),
            Some(if aterm_gpu::gpu_time::SUPPORTED {
                "device"
            } else {
                "unsupported"
            })
        );

        m.reset();
        let object = json();
        assert_eq!(object["n_gpu_time"].as_u64(), Some(0));
        assert_eq!(object["n_gpu_encode"].as_u64(), Some(0));
        assert_eq!(object["passes_last"].as_u64(), Some(0));
        assert_eq!(
            object["instances_by_stream"]
                .as_object()
                .map(|streams| streams.len()),
            Some(0)
        );
        assert_eq!(object["gpu_unorm_ablation"].as_str(), Some("off"));
        assert_eq!(object["gpu_unorm_route"].as_str(), Some("split"));
        assert_eq!(object["gpu_fetch_probe"].as_str(), Some("pending"));
        assert_eq!(object["gpu_time_source"].as_str(), Some(empty_source));
        assert_eq!(object["fx_instances_last"].as_u64(), Some(0));
        assert_eq!(object["fx_upload_bytes_last"].as_u64(), Some(0));
        assert_eq!(object["fx_refused_last"].as_u64(), Some(0));
    }

    #[test]
    fn fx_counters_count_since_reset() {
        use aterm_core::render::{FxContent, fx_layer_builds, fx_payload_copies};
        let m = Metrics::new();
        let process = wire::ReadCtx {
            metrics: &m,
            now_ns: 0,
            fragments: wire::Fragments::Process,
        };
        let count = |text: &str, key: &str| -> u64 {
            text_field(text, key)
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| panic!("{key} missing from {text}"))
        };
        let _sealed = FxContent::new(Vec::new(), Vec::new());
        let before = (fx_layer_builds(), fx_payload_copies());
        assert!(before.0 >= 1, "a seal is booked on aterm-core's total");
        m.reset();
        let text = wire::render_text(|s| emit_gpu(&process, s));
        assert!(
            count(&text, "fx_layer_builds") <= fx_layer_builds() - before.0,
            "a reset re-bases the seals: {text}"
        );
        assert!(
            count(&text, "fx_payload_copies") <= fx_payload_copies() - before.1,
            "a reset re-bases the copies: {text}"
        );

        let content = FxContent::new(Vec::new(), Vec::new());
        let _copy = content.clone();
        let text = wire::render_text(|s| emit_gpu(&process, s));
        let (builds, copies) = (
            count(&text, "fx_layer_builds"),
            count(&text, "fx_payload_copies"),
        );
        assert!(
            builds >= 1 && copies >= 1,
            "builds {builds}, copies {copies}"
        );
        let inert = wire::ReadCtx::inert(&m, 0);
        let held = wire::render_text(|s| emit_gpu(&inert, s));
        assert_eq!(
            (
                count(&held, "fx_layer_builds"),
                count(&held, "fx_payload_copies")
            ),
            (builds, copies),
            "an inert reply reads the ledger alone"
        );
        let object = aterm_json::from_str::<aterm_json::Value>(&wire::render_json(|s| {
            emit_gpu(&process, s)
        }))
        .expect("one JSON object");
        for (key, floor) in [("fx_layer_builds", builds), ("fx_payload_copies", copies)] {
            let read = object[key].as_u64().expect(key);
            assert!(read >= floor, "{key}: {read} < {floor}");
        }
    }

    #[test]
    fn a_stale_fx_sample_never_lowers_the_reset_base() {
        use super::instrument::{Baselined, Windowed as _};
        let m = Metrics::new();
        let gpu = &m.window.gpu;
        let at_reset = u64::MAX >> 1;
        gpu.fx_layer_builds.raise_total(at_reset);
        gpu.fx_payload_copies.raise_total(at_reset);
        gpu.sample_fx_totals();
        m.window.reset();
        gpu.fx_layer_builds.raise_total(at_reset + 1);
        gpu.fx_payload_copies.raise_total(at_reset + 1);
        assert_eq!(
            (
                gpu.fx_layer_builds.since_reset(),
                gpu.fx_payload_copies.since_reset()
            ),
            (1, 1),
            "a stale sample moved the base"
        );

        let stale = aterm_core::render::fx_layer_builds();
        let plain = Baselined::new();
        plain.set_total(at_reset);
        plain.set_total(stale);
        plain.reset();
        plain.set_total(at_reset + 1);
        assert_eq!(plain.since_reset(), at_reset + 1 - stale);
    }
}

#[cfg(test)]
mod window_input_attribution_tests {
    use super::{DrainedEdge, INPUT_SLICE_CAP_NS, Metrics, PendingInputStamp};

    #[test]
    fn another_windows_present_cannot_close_this_windows_key() {
        let mut typed_into = PendingInputStamp::default();
        let mut streaming = PendingInputStamp::default();
        typed_into.arm(1_000, 1_000);

        assert_eq!(streaming.drain_slices(4_000_000), Vec::<u64>::new());
        assert!(
            typed_into.is_pending(),
            "window B's frame closed window A's keystroke: the slice would read LOW"
        );

        let echo_at = 1_000 + 90_000_000;
        assert_eq!(typed_into.drain_slices(echo_at), vec![90_000_000]);
        assert!(
            !typed_into.is_pending(),
            "a content present consumes the stamp"
        );
        assert!(
            typed_into.drain_slices(echo_at + 1).is_empty(),
            "consumed once"
        );
    }

    #[test]
    fn a_burst_before_one_present_books_every_edge() {
        let mut pending = PendingInputStamp::default();
        for arrival in [10, 20, 30, 40] {
            pending.arm(arrival, arrival);
        }
        assert_eq!(pending.drain_slices(110), vec![100, 90, 80, 70]);
        assert!(!pending.is_pending());
    }

    #[test]
    fn a_full_ring_keeps_the_oldest_edges_and_refuses_the_rest() {
        let mut pending = PendingInputStamp::default();
        let held: Vec<bool> = (1..=(super::INPUT_EDGE_RING as u64 + 3))
            .map(|arrival| pending.arm(arrival, arrival))
            .collect();
        assert!(held[..super::INPUT_EDGE_RING].iter().all(|&h| h));
        assert_eq!(&held[super::INPUT_EDGE_RING..], &[false, false, false]);
        let slices = pending.drain_slices(1_000);
        assert_eq!(slices.len(), super::INPUT_EDGE_RING);
        assert_eq!(slices.first(), Some(&999), "the oldest edge is kept");
    }

    #[test]
    fn each_key_a_full_ring_refuses_is_counted_and_no_other() {
        let m = Metrics::new();
        let dropped = || m.window.input.edges_dropped.get();
        let mut pending = PendingInputStamp::default();
        for _ in 0..super::INPUT_EDGE_RING + 3 {
            m.note_window_input(&mut pending, 1_000);
        }
        assert_eq!(dropped(), 3, "one count per refused key");

        let fresh = 1_000 + INPUT_SLICE_CAP_NS + 1;
        m.note_window_input(&mut pending, fresh);
        assert_eq!(dropped(), 3, "an accepted key was counted");
        assert_eq!(
            pending.drain_slices(fresh + 7),
            vec![7],
            "the fresh key was refused"
        );
    }

    #[test]
    fn a_full_ring_of_aged_out_edges_takes_a_fresh_key() {
        let mut pending = PendingInputStamp::default();
        for arrival in 1..=super::INPUT_EDGE_RING as u64 {
            assert!(pending.arm(arrival, arrival));
        }
        let fresh = super::INPUT_EDGE_RING as u64 + INPUT_SLICE_CAP_NS + 1;
        assert!(
            pending.arm(fresh, fresh),
            "a full ring of never-echoed keys refused the fresh one"
        );
        assert_eq!(
            pending.drain_slices(fresh + 7),
            vec![7],
            "only the fresh key books a slice"
        );
        for arrival in 1..=super::INPUT_EDGE_RING as u64 {
            assert!(pending.arm(arrival, arrival));
        }
        assert!(!pending.arm(1_000, 1_000), "live edges are never evicted");
    }

    #[test]
    fn a_stamp_armed_before_reset_is_classed_as_the_previous_windows() {
        let mut pending = PendingInputStamp::default();
        pending.arm(50, 60);
        assert_eq!(
            pending.drained(10_000, 61),
            vec![DrainedEdge {
                slice_ns: 9_950,
                before_epoch: true,
            }],
            "drained, and classed as the previous window's"
        );
        assert!(!pending.is_pending(), "discarded, not left armed");

        pending.arm(50, 61);
        assert_eq!(
            pending.drained(10_050, 61),
            vec![DrainedEdge {
                slice_ns: 10_000,
                before_epoch: false,
            }]
        );
    }

    #[test]
    fn a_keystroke_that_never_echoed_ages_out() {
        let mut pending = PendingInputStamp::default();
        pending.arm(1, 1);
        assert!(pending.drain_slices(1 + INPUT_SLICE_CAP_NS + 1).is_empty());
        pending.arm(1, 1);
        assert_eq!(
            pending.drain_slices(1 + INPUT_SLICE_CAP_NS),
            vec![INPUT_SLICE_CAP_NS]
        );
    }
}

#[cfg(test)]
mod present_clock_tests {
    use super::{
        INPUT_SLICE_CAP_NS, Metrics, PresentBooking, PresentClock, PresentObserver,
        WindowPresentState, now_ns,
    };

    const MS: u64 = 1_000_000;

    fn present(
        m: &Metrics,
        now: u64,
        epoch: u64,
        latency_ns: u64,
        window: &mut WindowPresentState,
        observer: &mut PresentObserver,
    ) -> PresentBooking {
        let clock = PresentClock {
            now,
            epoch,
            capture_forced: false,
            tainted: false,
        };
        m.record_present_at(clock, latency_ns, MS, Some(window), observer)
    }

    #[test]
    fn another_windows_present_cannot_hide_this_windows_gap() {
        let m = Metrics::new();
        let t0 = 10_000 * MS;
        let stall = t0 + 500 * MS;
        let mut a = WindowPresentState::default();
        let mut b = WindowPresentState::default();
        let mut observer = PresentObserver::default();
        let first = present(&m, t0, 0, 0, &mut a, &mut observer);
        assert_eq!(
            first.window_gap_ns, None,
            "a window's first present has no gap"
        );
        let mut t = t0;
        while t + 16 * MS < stall {
            t += 16 * MS;
            let streamed = present(&m, t, 0, 0, &mut b, &mut observer);
            assert_eq!(streamed.frame_gap_ns, Some(16 * MS), "at {t}");
        }
        let stalled = present(&m, stall, 0, 0, &mut a, &mut observer);
        assert_eq!(
            stalled.window_gap_ns,
            Some(500 * MS),
            "window A's 500 ms stall is its witness gap"
        );
        assert_eq!(
            stalled.frame_gap_ns,
            Some(stall - t),
            "the process-wide cadence still counts B's frames"
        );

        let mut shared = WindowPresentState::default();
        let mut observer = PresentObserver::default();
        let _ = present(&m, t0, 0, 0, &mut shared, &mut observer);
        let mut t = t0;
        while t + 16 * MS < stall {
            t += 16 * MS;
            let _ = present(&m, t, 0, 0, &mut shared, &mut observer);
        }
        let hidden = present(&m, stall, 0, 0, &mut shared, &mut observer);
        assert_eq!(
            hidden.window_gap_ns,
            Some(stall - t),
            "one shared stamp reads B's last frame, not A's stall"
        );
    }

    #[test]
    fn the_first_present_after_a_reset_books_no_gap() {
        let m = Metrics::new();
        let t = 10_000 * MS;
        let mut window = WindowPresentState::default();
        let mut observer = PresentObserver::default();
        let _ = present(&m, t, 0, 0, &mut window, &mut observer);
        let first = present(&m, t + 10 * MS, t + 1, 0, &mut window, &mut observer);
        assert_eq!((first.window_gap_ns, first.frame_gap_ns), (None, None));
        let second = present(&m, t + 20 * MS, t + 1, 0, &mut window, &mut observer);
        assert_eq!(
            (second.window_gap_ns, second.frame_gap_ns),
            (Some(10 * MS), Some(10 * MS)),
            "the post-reset present is the next one's previous present"
        );

        let mut window = WindowPresentState::default();
        let mut observer = PresentObserver::default();
        let _ = present(&m, t, 0, 0, &mut window, &mut observer);
        let unreset = present(&m, t + 10 * MS, 0, 0, &mut window, &mut observer);
        assert_eq!(
            (unreset.window_gap_ns, unreset.frame_gap_ns),
            (Some(10 * MS), Some(10 * MS))
        );

        let before = now_ns();
        m.reset();
        assert!(PresentClock::read(&m, false).epoch >= before);
    }

    #[test]
    fn the_taint_decision_is_made_once() {
        let m = Metrics::new();
        let until = 20_000 * MS;
        let typed = until - 50 * MS;
        let at = |now: u64| {
            let mut window = WindowPresentState::default();
            assert!(window.pending_input.arm(typed, typed));
            let clock = PresentClock::at(false, now, 0, until, 0);
            let mut observer = PresentObserver::default();
            let booked = m.record_present_at(clock, MS, MS, Some(&mut window), &mut observer);
            (booked.output_tainted, booked.input_booked)
        };
        assert_eq!(
            at(until - 1),
            (Some(true), 1),
            "inside the tail: the tainted ledger"
        );
        assert_eq!(
            at(until),
            (Some(false), 1),
            "the tail is exclusive: on glass"
        );
    }

    #[test]
    fn a_forced_present_books_a_tainted_capture_present() {
        let m = Metrics::new();
        let now = 20_000 * MS;
        let typed = now - 50 * MS;
        let book = |capture_forced: bool| {
            let mut window = WindowPresentState::default();
            assert!(window.pending_input.arm(typed, typed));
            let clock = PresentClock::at(capture_forced, now, 0, 0, 0);
            let mut observer = PresentObserver::default();
            let booked = m.record_present_at(clock, MS, MS, Some(&mut window), &mut observer);
            booked.output_tainted
        };
        assert_eq!(book(true), Some(true), "forced: tainted");
        let present = &m.window.present;
        assert_eq!(present.capture_presents.get(), 1);
        assert_eq!(present.tainted_samples.get(), 1);
        assert_eq!(book(false), Some(false), "unforced: on glass");
        assert_eq!(
            present.capture_presents.get(),
            1,
            "an unforced present is no capture_presents sample"
        );
        assert_eq!(present.tainted_samples.get(), 1);
    }

    #[test]
    fn a_key_typed_before_a_reset_is_drained_not_booked() {
        let m = Metrics::new();
        let reset_at = 10_000 * MS;
        let before = reset_at - 20 * MS;
        let after = reset_at + 30 * MS;
        let echo = reset_at + 100 * MS;
        let present_keys = |keys: &[u64], epoch: u64| {
            let mut window = WindowPresentState::default();
            for &key in keys {
                assert!(window.pending_input.arm(key, key));
            }
            let mut observer = PresentObserver::default();
            let booked = present(&m, echo, epoch, MS, &mut window, &mut observer);
            assert!(
                !window.pending_input.is_pending(),
                "the present drains every key"
            );
            booked.input_booked
        };
        assert_eq!(
            present_keys(&[before], reset_at),
            0,
            "the lone key typed before the reset books nothing"
        );
        assert_eq!(
            present_keys(&[before, after], reset_at),
            1,
            "only the key armed after the reset books"
        );
        assert_eq!(present_keys(&[before, after], 0), 2);
        let aged = echo - INPUT_SLICE_CAP_NS - MS;
        assert_eq!(present_keys(&[aged], reset_at), 0);
        assert_eq!(present_keys(&[aged], 0), 0);
    }

    #[test]
    fn a_present_that_shows_a_keys_prediction_closes_its_slice() {
        let m = Metrics::new();
        let t = 10_000 * MS;
        let mut window = WindowPresentState::default();
        for (i, op) in [Some(5), Some(6), None, Some(7)].into_iter().enumerate() {
            let key = t + i as u64 * 10 * MS;
            assert!(window.pending_input.arm(key, key));
            if let Some(op) = op {
                window.pending_input.tag_newest(Some(op));
            }
        }
        let mut observer = PresentObserver::default();
        let blink = present(&m, t + 50 * MS, 0, 0, &mut window, &mut observer);
        assert_eq!(blink.input_booked, 0, "no prediction shown: nothing booked");
        super::note_prediction_shown(&mut window, Some(7), |_| true);
        let shown = present(&m, t + 60 * MS, 0, 0, &mut window, &mut observer);
        assert_eq!(
            shown.input_booked, 2,
            "the keys of ops 5 and 6; the key that armed no op stops the run"
        );
        assert_eq!(
            (shown.input_predicted, m.window.input.predicted.get()),
            (2, 2),
            "both counted as closed by the prediction"
        );
        assert!(window.pending_input.is_pending());
        let after = present(&m, t + 70 * MS, 0, 0, &mut window, &mut observer);
        assert_eq!(after.input_booked, 0, "the mark was the one frame's");
        let echo = present(&m, t + 200 * MS, 0, MS, &mut window, &mut observer);
        assert_eq!(
            echo.input_booked, 2,
            "the echo's content present books the rest"
        );
        assert_eq!(
            (echo.input_predicted, m.window.input.predicted.get()),
            (0, 2),
            "a content present's keys are not counted as predicted"
        );
        assert!(!window.pending_input.is_pending());
    }

    #[test]
    fn a_dropped_key_tags_no_other_keys_edge() {
        let m = Metrics::new();
        let t = 10_000 * MS;
        let mut window = WindowPresentState::default();
        let ring = super::INPUT_EDGE_RING as u64;
        for i in 0..ring {
            assert!(window.pending_input.arm(t + i, t + i));
            window.pending_input.tag_newest((i + 1 < ring).then_some(i));
        }
        assert!(
            !window.pending_input.arm(t + 100, t + 100),
            "the ring is full"
        );
        window.pending_input.tag_newest(Some(ring));
        super::note_prediction_shown(&mut window, Some(ring), |_| true);
        let mut observer = PresentObserver::default();
        let shown = present(&m, t + 50 * MS, 0, 0, &mut window, &mut observer);
        assert_eq!(
            shown.input_booked,
            u32::try_from(super::INPUT_EDGE_RING - 1).expect("a small ring"),
            "every key the frame shows; the ring's newest edge is not the dropped key's"
        );
        assert!(
            window.pending_input.is_pending(),
            "the newest key waits for its echo"
        );
    }
}

#[cfg(test)]
mod lateness_attribution_tests {
    use super::{
        DeadlineOwner, EventWakeKind, Heal, Metrics, PresentClock, PresentObserver,
        SLOW_PRESENT_EPISODE_QUIET_NS, SLOW_PRESENT_LOG_THRESHOLD_NS, SlowEpisode, SlowEpisodeStep,
        WindowPresentState, emit_lateness, now_ns, slow_episode_close, slow_episode_step, wire,
    };
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    const LATE_NS: u64 = 400_000_000;

    fn passed(deadline: Option<Instant>) -> Heal {
        Heal {
            armed: deadline,
            floor: false,
            streak: false,
        }
    }

    fn wait_out_the_process_clock() {
        while now_ns() < LATE_NS + 50_000_000 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn fires_and_gpu_scope_are_published() {
        let m = Metrics::new();
        let fires = |m: &Metrics| {
            m.window
                .sched
                .fires_by_owner
                .get(DeadlineOwner::FrameCap.index())
        };
        let now = Instant::now();
        let soon = now + Duration::from_millis(5);
        m.note_deadline(DeadlineOwner::FrameCap, Some(soon), now, passed(Some(soon)));
        m.note_event_wake(EventWakeKind::Timer);
        assert_eq!(fires(&m), 1, "a timer wake is a fire for its owner");
        m.note_event_wake(EventWakeKind::WaitCancelled);
        assert_eq!(fires(&m), 1, "an interrupted deadline did not fire");

        let scope = &m.window.gpu_scope;
        let counts = || {
            (
                scope.scissor_taken.since_reset(),
                scope.full_repaints.since_reset(),
                scope.scroll_rescues.since_reset(),
            )
        };
        scope.scissor_taken.set_total(10);
        scope.full_repaints.set_total(4);
        scope.scroll_rescues.set_total(2);
        m.reset();
        assert_eq!(counts(), (0, 0, 0), "reset rebases the window");
        scope.scissor_taken.set_total(13);
        scope.full_repaints.set_total(5);
        scope.scroll_rescues.set_total(3);
        assert_eq!(counts(), (3, 1, 1));
    }

    #[test]
    fn a_late_timer_survives_the_reader_wake_that_erases_the_last_reading() {
        wait_out_the_process_clock();
        let m = Metrics::new();
        let sched = &m.window.sched;
        let last_wake_late = |m: &Metrics| m.live.last_wake_late.load(Ordering::Relaxed);
        let now = Instant::now();
        let stale = now - Duration::from_nanos(LATE_NS);
        m.note_deadline(
            DeadlineOwner::FrameCap,
            Some(stale),
            now,
            passed(Some(stale)),
        );
        m.note_event_wake(EventWakeKind::Timer);
        assert!(
            last_wake_late(&m) >= LATE_NS,
            "the timer wake must book its lateness at all"
        );

        m.note_event_wake(EventWakeKind::WaitCancelled);
        assert_eq!(
            last_wake_late(&m),
            0,
            "`wake_late_ms` is still the LAST reading — that contract is unchanged"
        );

        assert!(
            sched.wake_late.max() >= LATE_NS,
            "a 400 ms late timer must not be erased by the next wake"
        );
        assert_eq!(
            DeadlineOwner::from_raw(sched.wake_late.by()),
            DeadlineOwner::FrameCap,
            "a number that cannot name its producer cannot end an investigation"
        );
        assert!(
            sched.wake_late.at() != 0,
            "the worst wake must say WHEN it happened"
        );
        assert!(
            sched.deadline_late.max() >= LATE_NS && sched.deadline_late.at() != 0,
            "the deadline lateness keeps its worst case on the same rule"
        );
        assert_eq!(
            DeadlineOwner::from_raw(sched.deadline_late.by()),
            DeadlineOwner::FrameCap
        );

        let ctx = wire::ReadCtx::inert(&m, now_ns());
        let text = wire::render_text(|s| emit_lateness(&ctx, s));
        assert!(
            text.contains(" max_wake_late_owner=frame_cap "),
            "the text fragment must name the owner: {text}"
        );
        assert!(
            text.contains(" metrics_now_ms="),
            "an instant with no anchor cannot be read as `how long ago`: {text}"
        );
        let json = wire::render_json(|s| emit_lateness(&ctx, s));
        assert!(
            json.contains("\"max_deadline_late_owner\":\"frame_cap\""),
            "the JSON twin must carry the same owner: {json}"
        );
    }

    #[test]
    fn the_worst_present_latency_records_its_instant_and_its_frame_gap() {
        let m = Metrics::new();
        let present = &m.window.present;
        let gap_ns = 300_000_000;
        let latency_ns = 4_000_000_000;
        let now = 10_000_000_000;
        let clock = PresentClock {
            now,
            epoch: 0,
            capture_forced: false,
            tainted: false,
        };
        let mut window = WindowPresentState {
            last_present_ns: now - gap_ns,
            ..WindowPresentState::default()
        };
        let mut observer = PresentObserver::default();
        let booked = m.record_present_at(
            clock,
            latency_ns,
            1_000_000,
            Some(&mut window),
            &mut observer,
        );

        assert_eq!(booked.window_gap_ns, Some(gap_ns));
        assert_eq!(present.latency.max(), latency_ns);
        assert_eq!(
            present.latency_at.get(),
            now,
            "the worst present must say when it happened"
        );
        assert_eq!(
            present.latency_gap.get(),
            gap_ns,
            "the frame gap of the winning sample is what separates a stall from an \
             idle stretch"
        );

        let later = PresentClock {
            now: now + 1_000_000,
            ..clock
        };
        let _ = m.record_present_at(
            later,
            1_000_000,
            1_000_000,
            Some(&mut window),
            &mut observer,
        );
        assert_eq!(present.latency_at.get(), now);
        assert_eq!(present.latency_gap.get(), gap_ns);
    }

    #[test]
    fn the_slow_present_breadcrumb_is_one_line_per_episode() {
        let slow = SLOW_PRESENT_LOG_THRESHOLD_NS + 1;
        let quiet = SLOW_PRESENT_EPISODE_QUIET_NS;
        let mut ep = SlowEpisode::default();
        let owner = DeadlineOwner::Blink;
        assert_eq!(
            slow_episode_step(&mut ep, SLOW_PRESENT_LOG_THRESHOLD_NS, owner, 1),
            SlowEpisodeStep::Ignored,
            "a frame at the threshold is a slow frame, not an episode"
        );
        assert_eq!(
            slow_episode_step(&mut ep, slow, DeadlineOwner::SessionStatus, 1),
            SlowEpisodeStep::Ignored,
            "a delay owned by the session-status observer is not logged"
        );
        assert_eq!(
            slow_episode_step(&mut ep, slow, owner, 1),
            SlowEpisodeStep::Opened { closed: None },
            "the first spike of a process opens an episode, even in its first second"
        );

        let mut at = 1;
        while at < 27 * 3600 * 1_000_000_000 {
            at += 10_000_000_000;
            assert_eq!(
                slow_episode_step(&mut ep, slow + at % 7, owner, at),
                SlowEpisodeStep::Joined,
                "a continuous stall wrote another line at {at}"
            );
            assert_eq!(slow_episode_close(&mut ep, at + 1, false), None);
        }
        assert_eq!(ep.samples, 27 * 360 + 1);
        assert_eq!(ep.worst_ns, slow + 6);
        let closed = slow_episode_close(&mut ep, at + quiet, false)
            .expect("a minute without a slow present closes the episode");
        assert_eq!(closed.samples, 27 * 360 + 1);
        assert_eq!(ep, SlowEpisode::default());

        assert_eq!(
            slow_episode_step(&mut ep, slow, owner, at),
            SlowEpisodeStep::Opened { closed: None }
        );
        let again = at + quiet;
        match slow_episode_step(&mut ep, slow * 2, owner, again) {
            SlowEpisodeStep::Opened { closed: Some(old) } => {
                assert_eq!((old.start_ns, old.samples), (at, 1));
            }
            other => panic!("expected a new episode, got {other:?}"),
        }
        assert_eq!((ep.start_ns, ep.worst_ns), (again, slow * 2));
        assert_eq!(
            slow_episode_step(&mut ep, slow, owner, again + quiet - 1),
            SlowEpisodeStep::Joined,
            "under a minute apart is the same episode"
        );
        assert_eq!(
            slow_episode_close(&mut ep, again + 1, true).map(|e| e.samples),
            Some(2),
            "`metrics reset` closes an open episode"
        );
    }

    #[test]
    fn metrics_reset_leaves_the_episode_for_the_ui_thread_to_close() {
        let m = Metrics::new();
        let slow = SLOW_PRESENT_LOG_THRESHOLD_NS + 1;
        let owner = DeadlineOwner::Blink;
        let at = 1_000_000_000;
        let open = || {
            let mut observer = PresentObserver::default();
            observer.note_slow_present(&m, slow, 0, at, owner);
            observer.note_slow_present(&m, slow, 0, at + 1, owner);
            observer
        };
        let mut observer = open();
        assert_eq!(observer.episode.samples, 2);
        observer.honour(at + 1);
        assert_eq!(
            observer.episode.samples, 2,
            "a slow present at the epoch belongs to the new window"
        );

        let clock = PresentClock {
            now: at + 3,
            epoch: at + 2,
            capture_forced: false,
            tainted: false,
        };
        let _ = m.record_present_at(clock, 0, 1_000_000, None, &mut observer);
        assert_eq!(
            observer.episode.samples, 0,
            "the next present after a reset closes it"
        );

        observer.note_slow_present(&m, slow, 0, at + 4, owner);
        observer.honour(at + 2);
        assert_eq!(
            (observer.episode.samples, observer.episode.start_ns),
            (1, at + 4),
            "a slow present after the reset opens a new episode at its own instant"
        );

        let mut unreset = open();
        unreset.note_slow_present(&m, slow, 0, at + 4, owner);
        assert_eq!((unreset.episode.samples, unreset.episode.start_ns), (3, at));
    }

    #[test]
    fn the_os_queue_share_of_key_write_is_published_apart_from_it() {
        let m = Metrics::new();
        let queue = &m.window.input.key_queue;

        let parked_ns = 17_000_000;
        let before = now_ns();
        m.note_key_arrival_queued(parked_ns);
        let after = now_ns();
        let arrival = m.live.lat_key.load(Ordering::Relaxed);
        assert!(
            (before.saturating_sub(parked_ns).max(1)..=after.saturating_sub(parked_ns).max(1))
                .contains(&arrival),
            "the arrival stamp is backdated by the queue wait"
        );
        assert_eq!(
            (queue.last(), queue.max()),
            (parked_ns, parked_ns),
            "the backdate is now a reading of its own, not just a moved stamp"
        );
        assert_eq!(queue.copy_out().n(), 1);
        let queue_p99 = queue.copy_out().percentile(0.99).unwrap();
        assert!(
            (parked_ns..parked_ns + parked_ns / 4).contains(&queue_p99),
            "the queue distribution reports the wait it was handed, as a bucket \
             upper edge; got {queue_p99} ns"
        );

        m.note_key_arrival_queued(0);
        assert_eq!(
            (queue.last(), queue.max()),
            (0, parked_ns),
            "a 0 is this key's reading; the worst key still owns the max"
        );
        assert_eq!(queue.copy_out().n(), 2);

        m.note_key_arrival_queued(parked_ns);
        assert_eq!(queue.copy_out().n(), 3);
        assert_eq!((queue.last(), queue.max()), (parked_ns, parked_ns));
        assert_eq!(m.window.input.key_write.copy_out().n(), 0);

        m.reset();
        assert_eq!((queue.last(), queue.max()), (0, 0));
        assert_eq!(queue.copy_out().n(), 0);
    }
}

#[cfg(test)]
mod window_tests {
    use super::instrument::{Counter, Dist, Probe, Windowed, window_ledger};
    use super::{Metrics, emit_quantiles, inert_text, now_ns, wire};
    use std::sync::atomic::Ordering;

    fn silent<R>(
        fresh: impl Fn() -> R,
        visit: impl Fn(&R, &mut dyn FnMut(&str, &dyn Probe)),
        render: impl Fn(&R) -> String,
    ) -> Vec<String> {
        let mut names = Vec::new();
        visit(&fresh(), &mut |name, probe| {
            if probe.observes() {
                names.push(name.to_string());
            }
        });
        assert!(!names.is_empty(), "the root holds no observation");
        names
            .into_iter()
            .filter(|name| {
                let root = fresh();
                let before = render(&root);
                visit(&root, &mut |path, probe| {
                    if path == name.as_str() {
                        probe.poke();
                    }
                });
                render(&root) == before
            })
            .collect()
    }

    #[test]
    fn every_observation_instrument_reaches_a_surface() {
        assert_eq!(
            silent(
                Metrics::new,
                |m, f| m.window.visit("window", f),
                |m| inert_text(m, 0)
            ),
            Vec::<String>::new(),
            "these instruments are written and published nowhere"
        );

        window_ledger! {
            struct Muted {
                published: Counter,
                unpublished: Counter,
            }
        }
        assert_eq!(
            silent(
                Muted::new,
                |l, f| l.visit("muted", f),
                |l| wire::render_text(|s| s.count("published", l.published.get()))
            ),
            ["muted.unpublished"]
        );

        window_ledger! {
            struct Spread {
                wait: Dist,
            }
        }
        assert_eq!(
            silent(
                Spread::new,
                |l, f| l.visit("spread", f),
                |l| wire::render_text(|s| emit_quantiles(
                    s,
                    ["n_wait", "wait_p50_ms", "wait_p95_ms", "wait_p99_ms"],
                    &l.wait.copy_out()
                ))
            ),
            ["spread.wait#last", "spread.wait#max"]
        );
    }

    #[test]
    fn reset_clears_every_window_instrument_and_no_live_state() {
        let m = Metrics::new();
        let mut cells = 0;
        m.window.visit("window", &mut |name, probe| {
            probe.poke();
            assert!(!probe.is_empty(), "{name}: a poke left it empty");
            cells += 1;
        });
        assert_ne!(cells, 0);
        let live = &m.live;
        live.backend_gpu.store(true, Ordering::Relaxed);
        live.perf_reduced.store(true, Ordering::Relaxed);
        live.pty_sessions.store(3, Ordering::Relaxed);
        live.spare_ptys.store(2, Ordering::Relaxed);
        let counts = [
            &live.sync_holding_windows,
            &live.capture_depth,
            &live.taint_until,
            &live.fx_glow_quads_last,
            &live.fx_under_quads_last,
            &live.fx_trail_cells_last,
            &live.last_wake_kind,
            &live.last_wake_owner,
            &live.last_wake_late,
            &live.last_deadline_owner,
            &live.last_deadline_due,
            &live.last_deadline_late,
            &live.lat_key,
            &live.handoff_input_dropped,
            &live.handoff_input_incoherent,
        ];
        for (n, cell) in (1..).zip(counts) {
            cell.store(n, Ordering::Relaxed);
        }

        let before = now_ns();
        m.reset();
        m.window.visit("window", &mut |name, probe| {
            assert!(probe.is_empty(), "{name} survived the reset");
        });
        assert!(m.epoch.load(Ordering::Relaxed) >= before);

        assert!(live.backend_gpu.load(Ordering::Relaxed));
        assert!(live.perf_reduced.load(Ordering::Relaxed));
        assert_eq!(live.pty_sessions.load(Ordering::Relaxed), 3);
        assert_eq!(live.spare_ptys.load(Ordering::Relaxed), 2);
        for (n, cell) in (1..).zip(counts) {
            assert_eq!(cell.load(Ordering::Relaxed), n, "live state #{n} was reset");
        }
    }
}
