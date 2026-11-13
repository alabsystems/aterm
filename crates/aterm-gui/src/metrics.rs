// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-global render/latency counters, read over the control socket by the `metrics` verb so a driver can measure responsiveness and detect lag directly.

pub(crate) mod instrument;
#[cfg(test)]
mod key_queue_tests;
#[cfg(all(unix, any(test, debug_assertions)))]
mod restore_experiment;
pub(crate) mod startup;
pub(crate) mod wakes;
pub(crate) mod wire;

use crate::deadline_heal::Heal;
use crate::label_enum::label_enum;
pub(crate) use instrument::Histogram;
use instrument::{
    Baselined, Counter, Dist, HistCopy, Last, LastFlag, LastLabel, LatencyDist, Max, MaxBy, Peak,
    Stamp, Tally, Windowed, window_ledger,
};
#[cfg(all(unix, any(test, debug_assertions)))]
pub(crate) use restore_experiment::{RestoreExperiment, record_restore_experiment};
#[cfg(unix)]
pub(crate) use startup::claim_timeline_log_text;
#[cfg(all(unix, any(test, debug_assertions)))]
pub(crate) use startup::record_keeper_seam;
pub(crate) use startup::{
    ClaimStamp, StartupAttachMilestones, StartupPresentTiming, StartupWorkerLegs,
    begin_handoff_diagnostics, leg_elapsed_ns, mark_backend_worker_done, mark_backend_worker_spawn,
    mark_first_window_visible, mark_first_winit_resumed, mark_gui_ready_for_winit,
    mark_initial_surface_ready, mark_keeper_served, mark_process_start, mark_rust_main_start,
    record_backend_worker_legs, record_initial_attach_milestones, stamp_claim,
};
#[cfg(unix)]
pub(crate) use startup::{
    first_present_done, keeper_seam, launch_finished, mark_keeper_hello, mark_keeper_imported,
    mark_keeper_placed, record_keeper_admitted, record_keeper_cohort,
};
#[cfg(target_os = "macos")]
pub(crate) use startup::{mark_handoff_claimed, process_age, process_main_start};
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The process's metrics, written by the App and read by the control thread's `metrics` verb.
pub(crate) static METRICS: Metrics = Metrics::new();

#[derive(Clone, Copy)]
struct KeyArrival {
    // Identity only: never dereferenced. The scope borrows this metrics root.
    metrics: *const Metrics,
    at_ns: u64,
    queue_measured: bool,
}

/// One claimed write's dispatch clock and whether its native queue age was observed.
/// A missing stamp is control/release input, not an unavailable hardware measurement.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct KeyWriteStamp {
    at_ns: u64,
    queue_measured: bool,
}

thread_local! {
    static KEY_ARRIVAL: Cell<Option<KeyArrival>> = const { Cell::new(None) };
}

/// Restores the enclosing hardware dispatch; only closure-scoped entry exposes it.
struct KeyArrivalScope<'a> {
    previous: Option<KeyArrival>,
    _metrics: &'a Metrics,
    _thread: PhantomData<Rc<()>>,
}

impl Drop for KeyArrivalScope<'_> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DispatchClockOwnership",
            action = "ExitInner",
            project = "aterm_gui::metrics::dispatch_clock_ownership_tests::ownership_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DispatchClockOwnership",
            action = "ExitOuter",
            project = "aterm_gui::metrics::dispatch_clock_ownership_tests::ownership_transition"
        )
    )]
    fn drop(&mut self) {
        KEY_ARRIVAL.with(|arrival| arrival.set(self.previous));
    }
}

/// A metrics root: the measurement window, the live state beside it, and the window's epoch.
pub(crate) struct Metrics {
    pub(crate) window: Window,
    pub(crate) live: Live,
    epoch: AtomicU64,
    /// Chrome receipts never repeat across windows or measurement resets.
    chrome_generation: AtomicU64,
}

impl Metrics {
    pub(crate) const fn new() -> Self {
        Self {
            window: Window::new(),
            live: Live::new(),
            epoch: AtomicU64::new(0),
            chrome_generation: AtomicU64::new(0),
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
        pub(crate) latency: LatencyDist,
        /// When the worst `latency` was booked (`now_ns`), and the gap since its window's previous present.
        pub(crate) latency_at: Last,
        pub(crate) latency_gap: Last,
        /// The worst `latency`'s own frame: its `render`, its redraw's `pre_present` and
        /// `acquire_wait` (the latest of each when it presented, so a second window's redraw in
        /// between would be read as its own), and how long its window had shown nothing before it,
        /// whether or not that stretch began before the measurement window.
        pub(crate) latency_render: Last,
        pub(crate) latency_pre_present: Last,
        pub(crate) latency_acquire: Last,
        pub(crate) latency_idle: Last,
        /// The worst `latency`'s frame from its compose's end to its present's booking: the wall
        /// time, the UI thread's CPU time in it (off a core for the rest: blocked or preempted),
        /// the process's page faults in it (those three read only by a debug or `dev-seams`
        /// build, 0 in a shipped one), and its GPU encode ([`GpuLedger::encode_last`]).
        pub(crate) latency_submit_wall: Last,
        pub(crate) latency_submit_cpu: Last,
        pub(crate) latency_submit_faults: Last,
        pub(crate) latency_encode: Last,
        /// The latest frame's (`last_submit_ms`, `last_submit_cpu_ms`, `last_submit_faults`), as
        /// [`record_present`] closed them for the snapshot above (debug and `dev-seams` builds).
        pub(crate) submit_wall: Last,
        pub(crate) submit_cpu: Last,
        pub(crate) submit_faults: Last,
        pub(crate) tainted: Dist,
        /// Output→present samples diverted to `tainted`.
        pub(crate) tainted_samples: Counter,
        /// `video` capture episodes opened in the window.
        pub(crate) capture_episodes: Counter,
        /// Output→present samples a still capture's barrier forced (`image`, `window`, the SIGUSR1 snapshot).
        pub(crate) capture_presents: Counter,
        /// Causal frame CPU work: compose plus CPU raster/copy, or encoding GPU commands and calling `queue.submit`.
        pub(crate) render: Dist,
        /// A present that showed a changed stats deck card: main-thread time from what asked for it (the deck clock's tick, else the redraw's entry) to the present's return. `present_*` books none of the deck's frames, which are not PTY output.
        pub(crate) deck_frame: Dist,
        /// The deck clock (`stats_clock.rs`): the vsyncs it delivered, those whose sample of the
        /// deck asked for a frame, and how long, ns, some window's clock ran.
        pub(crate) deck_clock_ticks: Counter,
        pub(crate) deck_clock_frames: Counter,
        pub(crate) deck_clock_run: Counter,
        /// The deck clock's display links (`stats_clock.rs`, `LINK_SILENCE`): the ticks the links
        /// themselves sent, the times a link asked to run sent none for `LINK_SILENCE` and its
        /// window's clock fell back to the loop's timer, and the times such a link ticked again
        /// and its clock went back on it.
        pub(crate) deck_clock_link_ticks: Counter,
        pub(crate) deck_clock_link_fallbacks: Counter,
        pub(crate) deck_clock_link_resumes: Counter,
        /// A deck frame's lateness on glass: its drawable's `presentedTime` after the vsync its
        /// clock tick targeted, 0 when on time (Metal on screen only).
        pub(crate) deck_glass_late: Dist,
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
        pub(crate) input: LatencyDist,
        /// When the worst `input` slice was booked (`now_ns`).
        pub(crate) input_at: Last,
        pub(crate) session_stamp: Stamp,
        /// Keystrokes a full edge ring could not hold (see [`INPUT_EDGE_RING`]).
        pub(crate) edges_dropped: Counter,
        /// Of the slices in `input`, those a PREDICTION closed.
        pub(crate) predicted: Counter,
        /// Of the slices in `input`, those an application present booked when it came
        /// [`COLD_GAP_NS`] or more after the process's previous present (`n_input_cold`,
        /// `input_cold_p50/p95/p99_ms`), at `input`'s resolution so their quantiles compare.
        pub(crate) input_cold: LatencyDist,
        /// Of the window slices in `input`, those booked in the first present of a window that a
        /// person's event woke from rest while it was focused
        /// ([`WindowPresentState::note_rest_wake`], `n_rest_wake_key`).
        pub(crate) rest_wake_key: Counter,
        /// Hardware-key arrival → completed PTY write (`key_write`).
        pub(crate) key_write: Dist,
        /// The NSEvent-QUEUE RESIDENCE that `key_write` is BACKDATED by: how long each hardware key sat in the OS event queue before the loop dispatched it.
        pub(crate) key_queue: Dist,
        /// Presses without a usable native queue clock; excluded from `key_queue`.
        pub(crate) key_queue_unavailable: Counter,
        /// Completed writes with only a dispatch-time fallback; excluded from `key_write`.
        pub(crate) key_write_unavailable: Counter,
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
        /// Overlay-only presents (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §3.3): frames
        /// whose only change was the deck's, which the renderer showed as the last frame with the
        /// deck's new tray.
        pub(crate) overlay_presents: Counter,
        /// Deck-only frames the renderer would not show overlay-only, presented in full instead.
        pub(crate) overlay_refusals: Counter,
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
        /// Window events other than a redraw (focus, occlusion, the pointer, keys, a resize), each counted under its kind as the loop dispatches it, however many redraws it brings.
        pub(crate) os_window_events: Tally<{ OsWindowEvent::COUNT }>,
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
        /// Readings that set the dial's needle sweeping, and the frames their episodes owe the windows that animate a dial ([`crate::stats_needle::Needle::frames_owed`]): each a frame of a window's deck clock, so its `deck_clock` deadlines' fires in a window are at most these and what the episode in flight as it opened had left to fire ([`needle_episode_left`]), while no window moves to a faster display mid-episode.
        pub(crate) needle_sweeps: Counter,
        pub(crate) needle_frames_owed: Counter,
        /// The vitals reader's latest reading on [`now_ns`]'s clock (0: none since the reset): when
        /// the loop asked it, when the reader began it, and when the reader handed it over. A
        /// window's first key after rest reads these beside its own booking
        /// (`max_input_present_at_ms`) to tell whether a reading ran beside its echo and frame.
        pub(crate) reading_asked: Last,
        pub(crate) reading_began: Last,
        pub(crate) reading_handed: Last,
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
        /// The renderer's tray card uploads, the bytes they sent, and the card bytes it compared against the last upload to find what changed, since the reset (`GpuRenderer::tray_uploads`, `tray_upload_bytes`, `tray_compare_bytes`).
        pub(crate) tray_uploads: Baselined,
        pub(crate) tray_upload_bytes: Baselined,
        pub(crate) tray_compare_bytes: Baselined,
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
    /// Windows whose deck clock's display link fell silent ([`DeckLinkSilence`]).
    pub(crate) deck_links_silent: AtomicU64,
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
    pub(crate) handoff_input_dropped: AtomicU64,
    pub(crate) handoff_input_incoherent: AtomicU64,
    /// The frames the needle's latest episode owed the windows when its reading landed (0 where it rested), and the `deck_clock` deadlines fired since.
    pub(crate) needle_episode_owed: AtomicU64,
    pub(crate) needle_episode_fired: AtomicU64,
    pub(crate) pty_sessions: AtomicUsize,
    pub(crate) spare_ptys: AtomicUsize,
}

impl Live {
    const fn new() -> Self {
        Self {
            backend_gpu: AtomicBool::new(false),
            perf_reduced: AtomicBool::new(false),
            sync_holding_windows: AtomicU64::new(0),
            deck_links_silent: AtomicU64::new(0),
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
            handoff_input_dropped: AtomicU64::new(0),
            handoff_input_incoherent: AtomicU64::new(0),
            needle_episode_owed: AtomicU64::new(0),
            needle_episode_fired: AtomicU64::new(0),
            pty_sessions: AtomicUsize::new(0),
            spare_ptys: AtomicUsize::new(0),
        }
    }
}

const PRESENT_TAINT_TAIL_NS: u64 = 1_000_000_000;

const INPUT_SLICE_CAP_NS: u64 = 5_000_000_000;

const RESIZE_SLICE_CAP_NS: u64 = 2_000_000_000;

#[cfg(all(any(test, all(unix, feature = "bench-support")), debug_assertions))]
thread_local! {
    /// The instant this thread's host clock reads are pinned to ([`pin_host_clock`]).
    static HOST_CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Pin (or, with `None`, release) this thread's host clock ([`host_now`]): `lab trail-diff`'s
/// dump (`crate::trail_rig`) drives every frame and event at its own injected instant, and the
/// curtain tests stamp a seam's curtain at their own instant on every target they build for.
#[cfg(all(any(test, all(unix, feature = "bench-support")), debug_assertions))]
pub(crate) fn pin_host_clock(at: Option<Instant>) {
    HOST_CLOCK.with(|clock| clock.set(at));
}

/// Whether this thread's host clock is pinned ([`pin_host_clock`]): its frames and events run at
/// injected instants, so a cost measured on the wall clock belongs to none of them. Always `false`
/// outside a debug test build or a debug unix `bench-support` one.
#[inline]
pub(crate) fn host_clock_pinned() -> bool {
    #[cfg(all(any(test, all(unix, feature = "bench-support")), debug_assertions))]
    {
        HOST_CLOCK.with(std::cell::Cell::get).is_some()
    }
    #[cfg(not(all(any(test, all(unix, feature = "bench-support")), debug_assertions)))]
    {
        false
    }
}

/// Now: the host's clock read where no frame or event instant reaches the code (this clock's
/// [`now_ns`], the input path's, a window's focus, the resize throttle, the trail curtain a
/// replaced front or a grid resize draws, the cursor effects' owner fence and scroll seam, the
/// stats surfaces' pointer rest, the instant a present reached glass), or the instant
/// `pin_host_clock` pinned on this thread in a debug test build or a debug unix `bench-support`
/// one.
#[inline]
pub(crate) fn host_now() -> Instant {
    #[cfg(all(any(test, all(unix, feature = "bench-support")), debug_assertions))]
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
        Messages => "messages",
        SessionStatus => "session_status",
        AccountProgress => "account_progress",
        ConsentCard => "consent_card",
        Presence => "presence",
        HandoffPark => "handoff_park",
        AccountDrain => "account_drain",
        SessionWaits => "session_waits",
        AosEchoHold => "aos_echo_hold",
        InputWatch => "input_watch",
        CrashJournal => "crash_journal",
        UpdateChecker => "update_checker",
        SplitRegridHold => "split_regrid_hold",
        ParkedTell => "parked_tell",
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
        DeckClock => "deck_clock",
        NativeMotion => "native_motion",
        ChipWord => "chip_word",
        CardWash => "card_wash",
        FleetAccess => "fleet_access",
        DiskFacts => "disk_facts",
        RestoreGlass => "restore_glass",
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
    /// The kind of a window event other than `RedrawRequested`, as `os_window_events_by_kind` names it.
    pub(crate) enum OsWindowEvent {
        Focus => "focus",
        Occlusion => "occlusion",
        Pointer => "pointer",
        Key => "key",
        Ime => "ime",
        Resize => "resize",
        Move => "move",
        Theme => "theme",
        File => "file",
        Close => "close",
        Touch => "touch",
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
        Mouse => "mouse",
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

/// A step of the vitals reader's latest reading, now ([`SchedLedger::reading_asked`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadingStep {
    /// The loop asked it.
    Asked,
    /// The reader began it.
    Began,
    /// The reader handed it over.
    Handed,
}

/// Stamps `step` of the vitals reader's latest reading at [`now_ns`].
pub(crate) fn note_reading(step: ReadingStep) {
    let sched = &METRICS.window.sched;
    let stamp = match step {
        ReadingStep::Asked => &sched.reading_asked,
        ReadingStep::Began => &sched.reading_began,
        ReadingStep::Handed => &sched.reading_handed,
    };
    stamp.set(now_ns());
}

/// A frame whose causal CPU work exceeds the ~30 fps (33.3 ms) budget below which interaction visibly stutters.
pub(crate) const SLOW_FRAME_THRESHOLD_NS: u64 = 33_333_333;

/// A present this long or more after the process's previous present is COLD: it is the first
/// frame any window drew in that long. On macOS the Apple GPU driver frees the block it keeps for
/// a class of GPU work 1 to 2 s after that work's command buffer completes
/// (`docs/measured/idle-memory-window-2026-10-27.md`), so a cold frame takes that block back.
pub(crate) const COLD_GAP_NS: u64 = 2_000_000_000;

/// One window's present-side state, kept in its `WindowState` (UI thread only, so plain integers).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowPresentState {
    pub(crate) pending_input: PendingInputStamp,
    pub(crate) pending_native_input: PendingNativeInputStamp,
    last_present_ns: u64,
    shown_through: Option<u64>,
    rest_woke: bool,
}

impl WindowPresentState {
    /// A person's event woke this window from rest while it was focused (`App::stir_rest`): the
    /// window's next application present, whatever it books, clears the mark, and each window
    /// slice that present books counts as a rest-wake key ([`InputLedger::rest_wake_key`]).
    pub(crate) const fn note_rest_wake(&mut self) {
        self.rest_woke = true;
    }

    /// Whether a rest wake is marked and no present has cleared it yet.
    #[cfg(test)]
    pub(crate) const fn rest_woke(&self) -> bool {
        self.rest_woke
    }
}

/// The present-side state that belongs to the whole process, owned by the `App` (UI thread).
#[derive(Debug, Default)]
pub(crate) struct PresentObserver {
    episode: SlowEpisode,
    last_present_ns: u64,
}

#[derive(Clone, Copy, Debug, Default)]
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

/// The application present an input slice is booked in: when, whether it is cold
/// ([`COLD_GAP_NS`]), and whether it is the first present of a window a rest wake marked.
#[derive(Clone, Copy, Debug)]
struct SliceAt {
    now: u64,
    cold: bool,
    rest_woke: bool,
}

/// What one window's coming application present is to the input slices it books, read before
/// [`record_present`] moves the process's last present and clears the window's rest wake: the
/// shared clock, process's previous present (for [`COLD_GAP_NS`]) and the window's mark
/// ([`WindowPresentState::note_rest_wake`]). A native page's slices
/// ([`record_native_content_present`]) and the stats lens's ([`PendingChromeInputStamp::record`])
/// book from it, so they count as the terminal's slices of the same present do.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PresentInput {
    clock: PresentClock,
    previous_present_ns: u64,
    rest_woke: bool,
}

impl PresentInput {
    /// Read one clock for every input surface of this successful application present.
    pub(crate) fn read(
        observer: &PresentObserver,
        window: Option<&WindowPresentState>,
        capture_forced: bool,
    ) -> Self {
        Self::of(
            observer,
            window,
            PresentClock::read(&METRICS, capture_forced),
        )
    }

    const fn of(
        observer: &PresentObserver,
        window: Option<&WindowPresentState>,
        clock: PresentClock,
    ) -> Self {
        Self {
            clock,
            previous_present_ns: observer.last_present_ns,
            rest_woke: match window {
                Some(window) => window.rest_woke,
                None => false,
            },
        }
    }

    pub(crate) const fn capture_forced(self) -> bool {
        self.clock.capture_forced
    }

    /// Book at `now`, with coldness fixed by the present's shared clock rather than the
    /// individual surface's booking time. A callback cannot make one present cross the threshold.
    const fn at(self, now: u64) -> SliceAt {
        SliceAt {
            now,
            cold: self.previous_present_ns != 0
                && self.clock.now.saturating_sub(self.previous_present_ns) >= COLD_GAP_NS,
            rest_woke: self.rest_woke,
        }
    }
}

/// Record one successful application present.
pub(crate) fn record_present(
    latency_ns: u64,
    render_ns: u64,
    startup_timing: StartupPresentTiming,
    window: Option<&mut WindowPresentState>,
    observer: &mut PresentObserver,
    input: PresentInput,
) {
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    submit_split::close();
    let clock = input.clock;
    startup::STARTUP.publish_first_present(startup_timing);
    let booking = METRICS.record_present_at(clock, latency_ns, render_ns, window, observer);
    #[cfg(test)]
    if booking.output_tainted.is_some() {
        crate::work_counts::present_sample_booked(clock.capture_forced);
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
        // Cold reads the process's previous present whatever the epoch, as `latency_idle` reads
        // the window's: the first present after a reset still knows how long the GPU was quiet.
        let at = PresentInput {
            clock,
            previous_present_ns: observer.last_present_ns,
            rest_woke: window
                .as_deref_mut()
                .is_some_and(|w| std::mem::take(&mut w.rest_woke)),
        }
        .at(now);
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
                    present.latency_render.set(render_ns);
                    present
                        .latency_pre_present
                        .set(self.window.redraw.pre_present.last());
                    present
                        .latency_acquire
                        .set(self.window.redraw.acquire_wait.last());
                    present.latency_idle.set(
                        window
                            .as_deref()
                            .map(|w| w.last_present_ns)
                            .filter(|&at| at != 0)
                            .map_or(0, |at| now.saturating_sub(at)),
                    );
                    present.latency_submit_wall.set(present.submit_wall.get());
                    present.latency_submit_cpu.set(present.submit_cpu.get());
                    present
                        .latency_submit_faults
                        .set(present.submit_faults.get());
                    present
                        .latency_encode
                        .set(self.window.gpu.encode_last.get());
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
                    self.window.input.book_slice_at(edge.slice_ns, at);
                    booking.input_booked += 1;
                });
            }
            if let Some(stamp) = self.window.input.session_stamp.take_through(now) {
                let d = now.saturating_sub(stamp);
                if d <= INPUT_SLICE_CAP_NS {
                    // A session's input names no window, so it is no window's rest-wake key.
                    self.window.input.book_slice_at(
                        d,
                        SliceAt {
                            rest_woke: false,
                            ..at
                        },
                    );
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
                        self.window.input.book_slice_at(edge.slice_ns, at);
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
    if latency_ns <= SLOW_PRESENT_LOG_THRESHOLD_NS
        || matches!(
            deadline_owner,
            DeadlineOwner::SessionStatus | DeadlineOwner::AccountProgress
        )
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

/// The renderer's running tray card totals: uploads, bytes uploaded and card bytes compared
/// (`GpuRenderer::tray_uploads`, `tray_upload_bytes`, `tray_compare_bytes`). One renderer serves
/// every window, so each total only rises.
pub(crate) fn note_tray_totals(uploads: u64, upload_bytes: u64, compare_bytes: u64) {
    let gpu = &METRICS.window.gpu;
    gpu.tray_uploads.raise_total(uploads);
    gpu.tray_upload_bytes.raise_total(upload_bytes);
    gpu.tray_compare_bytes.raise_total(compare_bytes);
}

/// A present that showed a changed stats deck card, `ns` after what asked for it
/// ([`PresentLedger::deck_frame`]).
pub(crate) fn note_deck_frame(ns: u64) {
    let _ = METRICS.window.present.deck_frame.record(ns);
}

/// A vsync the deck clock delivered, and whether its sample asked for a frame
/// ([`PresentLedger::deck_clock_ticks`]).
pub(crate) fn note_deck_clock_tick(asked: bool) {
    let present = &METRICS.window.present;
    present.deck_clock_ticks.add(1);
    if asked {
        present.deck_clock_frames.add(1);
    }
}

/// `ns` more of some window's deck clock running ([`PresentLedger::deck_clock_run`]).
pub(crate) fn note_deck_clock_run(ns: u64) {
    METRICS.window.present.deck_clock_run.add(ns);
}

/// A tick some window's display link sent ([`PresentLedger::deck_clock_link_ticks`]).
pub(crate) fn note_deck_link_tick() {
    METRICS.window.present.deck_clock_link_ticks.inc();
}

/// Some window's display link fell silent and its deck clock fell back to the loop's timer
/// ([`PresentLedger::deck_clock_link_fallbacks`]).
pub(crate) fn note_deck_link_silent() {
    METRICS.window.present.deck_clock_link_fallbacks.inc();
}

/// A silent display link ticked and its window's deck clock went back on it
/// ([`PresentLedger::deck_clock_link_resumes`]).
pub(crate) fn note_deck_link_resumed() {
    METRICS.window.present.deck_clock_link_resumes.inc();
}

/// How many windows' deck clocks have a silent display link now ([`Live::deck_links_silent`]).
pub(crate) fn deck_links_silent() -> u64 {
    METRICS.live.deck_links_silent.load(Ordering::Relaxed)
}

/// A window's lease on [`Live::deck_links_silent`]: whether its deck clock's display link fell
/// silent (`stats_clock::LINK_SILENCE`). A dropped window lets go.
#[derive(Debug, Default)]
pub(crate) struct DeckLinkSilence {
    held: bool,
}

impl DeckLinkSilence {
    pub(crate) fn set(&mut self, silent: bool) {
        transition_window_gauge(&mut self.held, silent, &METRICS.live.deck_links_silent);
    }

    pub(crate) const fn get(&self) -> bool {
        self.held
    }
}

impl Drop for DeckLinkSilence {
    fn drop(&mut self) {
        transition_window_gauge(&mut self.held, false, &METRICS.live.deck_links_silent);
    }
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
    s.count("tray_uploads", gpu.tray_uploads.since_reset());
    s.count("tray_upload_bytes", gpu.tray_upload_bytes.since_reset());
    s.count("tray_compare_bytes", gpu.tray_compare_bytes.since_reset());
}

fn note_present_glass(report: aterm_gpu::present_glass::GlassReport) {
    METRICS.window.glass.note(report);
    if let Some(late) = report.deck_late_ns {
        let _ = METRICS.window.present.deck_glass_late.record(late);
    }
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

fn os_event_labels() -> impl Iterator<Item = (usize, &'static str)> {
    OsWindowEvent::ALL
        .into_iter()
        .map(|kind| (kind.index(), kind.as_str()))
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
    s.decimal(
        "max_present_latency_render_ms",
        ms(w.present.latency_render.get()),
    );
    s.decimal(
        "max_present_latency_pre_present_ms",
        ms(w.present.latency_pre_present.get()),
    );
    s.decimal(
        "max_present_latency_acquire_ms",
        ms(w.present.latency_acquire.get()),
    );
    s.decimal(
        "max_present_latency_idle_ms",
        ms(w.present.latency_idle.get()),
    );
    s.decimal(
        "max_present_latency_submit_wall_ms",
        ms(w.present.latency_submit_wall.get()),
    );
    s.decimal(
        "max_present_latency_submit_cpu_ms",
        ms(w.present.latency_submit_cpu.get()),
    );
    s.decimal(
        "max_present_latency_encode_ms",
        ms(w.present.latency_encode.get()),
    );
    s.decimal("last_submit_ms", ms(w.present.submit_wall.get()));
    s.decimal("last_submit_cpu_ms", ms(w.present.submit_cpu.get()));
    s.count("last_submit_faults", w.present.submit_faults.get());
    s.count(
        "max_present_latency_submit_faults",
        w.present.latency_submit_faults.get(),
    );
    s.decimal("max_input_present_at_ms", ms(w.input.input_at.get()));
    s.decimal("last_reading_asked_at_ms", ms(w.sched.reading_asked.get()));
    s.decimal("last_reading_began_at_ms", ms(w.sched.reading_began.get()));
    s.decimal(
        "last_reading_handed_at_ms",
        ms(w.sched.reading_handed.get()),
    );
    s.decimal("metrics_now_ms", ms(ctx.now_ns));
}

impl InputLedger {
    fn book_present(&self, d: u64, now: u64) {
        if self.input.record(d) {
            self.input_at.set(now);
        }
    }

    /// Books slice `d` in the application present `at`: in `input`, in `input_cold` too when
    /// that present is cold, and as a rest-wake key when it is a woken window's first. Each is
    /// its own relaxed instrument, so only a ledger no present books into while it is read holds
    /// `n_input_cold <= n_input` and `n_rest_wake_key <= n_input`.
    fn book_slice_at(&self, d: u64, at: SliceAt) {
        if at.cold {
            self.input_cold.record(d);
        }
        if at.rest_woke {
            self.rest_wake_key.inc();
        }
        self.book_present(d, at.now);
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
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DispatchClockOwnership",
            action = "EnterOuter",
            project = "aterm_gui::metrics::dispatch_clock_ownership_tests::ownership_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DispatchClockOwnership",
            action = "EnterInner",
            project = "aterm_gui::metrics::dispatch_clock_ownership_tests::ownership_transition"
        )
    )]
    fn with_key_arrival_at<R>(
        &self,
        at_ns: u64,
        queue_measured: bool,
        dispatch: impl FnOnce() -> R,
    ) -> R {
        let previous = KEY_ARRIVAL.with(|arrival| {
            arrival.replace(Some(KeyArrival {
                metrics: std::ptr::from_ref(self),
                at_ns,
                queue_measured,
            }))
        });
        let _scope = KeyArrivalScope {
            previous,
            _metrics: self,
            _thread: PhantomData,
        };
        dispatch()
    }

    fn key_arrival_ns(&self) -> u64 {
        KEY_ARRIVAL.with(|arrival| {
            arrival
                .get()
                .filter(|stamp| std::ptr::eq(stamp.metrics, self))
                .map_or(0, |stamp| stamp.at_ns)
        })
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DispatchClockOwnership",
            action = "ClaimUi",
            project = "aterm_gui::metrics::dispatch_clock_ownership_tests::ownership_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DispatchClockOwnership",
            action = "ClaimWorker",
            project = "aterm_gui::metrics::dispatch_clock_ownership_tests::ownership_transition"
        )
    )]
    fn take_key_arrival(&self) -> u64 {
        KEY_ARRIVAL.with(|arrival| match arrival.get() {
            Some(stamp) if std::ptr::eq(stamp.metrics, self) => {
                arrival.set(Some(KeyArrival { at_ns: 0, ..stamp }));
                stamp.at_ns
            }
            _ => 0,
        })
    }

    fn take_key_write_stamp(&self) -> KeyWriteStamp {
        let queue_measured = KEY_ARRIVAL.with(|arrival| {
            arrival
                .get()
                .filter(|stamp| std::ptr::eq(stamp.metrics, self))
                .is_some_and(|stamp| stamp.queue_measured)
        });
        KeyWriteStamp {
            at_ns: self.take_key_arrival(),
            queue_measured,
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DispatchClockOwnership",
            action = "PeekWorker",
            project = "aterm_gui::metrics::dispatch_clock_ownership_tests::ownership_transition"
        )
    )]
    fn input_arrival_ns(&self, now: u64) -> u64 {
        let key = self.key_arrival_ns();
        if key == 0 { now } else { key }
    }

    fn note_window_input(&self, pending: &mut PendingInputStamp, now: u64) {
        if !pending.arm(self.input_arrival_ns(now), now) {
            self.window.input.edges_dropped.inc();
        }
    }

    fn note_session_input(&self, now: u64) {
        self.window
            .input
            .session_stamp
            .arm(self.input_arrival_ns(now));
    }

    fn note_native_window_input(&self, pending: &mut PendingInputStamp, arrival_ns: u64, now: u64) {
        if !pending.arm(arrival_ns, now) {
            self.window.input.edges_dropped.inc();
        }
    }

    fn record_native_content_present(&self, pending: &mut PendingInputStamp, at: SliceAt) {
        let epoch = self.epoch.load(Ordering::Relaxed);
        pending.drain_into(at.now, epoch, |edge| {
            if !edge.before_epoch {
                self.window.input.book_slice_at(edge.slice_ns, at);
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
/// answer of every key it was awaited for at or before that serial, booked as that window's
/// present `input` books its slices.
pub(crate) fn record_native_content_present(
    pending: &mut PendingNativeInputStamp,
    view: crate::tab_model::ViewId,
    serial: u64,
    input: PresentInput,
) {
    if !pending
        .awaited
        .is_some_and(|(awaited, from)| awaited == view && serial >= from)
    {
        return;
    }
    let mut taken = std::mem::take(pending);
    METRICS.record_native_content_present(&mut taken.edges, input.at(input.clock.now));
}

/// An opaque receipt for the latest contiguous chrome input in one window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChromeInputToken(u64);

/// Per-window input whose answer is painted chrome, independent of native leaves and PTYs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PendingChromeInputStamp {
    token: Option<ChromeInputToken>,
    edges: PendingInputStamp,
}

impl PendingChromeInputStamp {
    /// Retain contiguous pending keys and issue a fresh receipt for their latest answer.
    /// A full edge ring still advances the receipt: the older accepted keys await the new
    /// pixels too. Receipt exhaustion refuses further input rather than reusing a token.
    pub(crate) fn arm(&mut self, arrival_ns: u64) -> Option<ChromeInputToken> {
        METRICS.arm_chrome_input_at(self, arrival_ns, now_ns())
    }

    /// The receipt a composition must carry with the chrome it actually painted.
    pub(crate) const fn token(&self) -> Option<ChromeInputToken> {
        self.token
    }

    /// Abandon an answer whose selection or visibility changed outside its key sequence.
    pub(crate) fn retire(&mut self) {
        self.token = None;
        self.edges = PendingInputStamp::default();
    }

    /// Book only a successful live present carrying this ring's latest painted receipt, as
    /// that window's present `input` books its slices.
    /// Return the number of clean samples booked. Captures and their taint retire the ring,
    /// so an observer's frame cannot satisfy an edge or leave it for a later clean sample.
    pub(crate) fn record(&mut self, token: ChromeInputToken, input: PresentInput) -> usize {
        METRICS.record_chrome_input_at(self, token, input.clock, input)
    }
}

impl Metrics {
    fn arm_chrome_input_at(
        &self,
        pending: &mut PendingChromeInputStamp,
        arrival_ns: u64,
        now: u64,
    ) -> Option<ChromeInputToken> {
        let next = self
            .chrome_generation
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1));
        let Ok(previous) = next else {
            pending.retire();
            self.window.input.edges_dropped.inc();
            return None;
        };
        pending.token = previous.checked_add(1).map(ChromeInputToken);
        if !pending.edges.arm(arrival_ns, now) {
            self.window.input.edges_dropped.inc();
        }
        pending.token
    }

    fn record_chrome_input_at(
        &self,
        pending: &mut PendingChromeInputStamp,
        token: ChromeInputToken,
        clock: PresentClock,
        input: PresentInput,
    ) -> usize {
        if clock.capture_forced || clock.tainted {
            pending.retire();
            return 0;
        }
        if pending.token != Some(token) {
            return 0;
        }
        let mut booked = 0;
        pending.edges.drain_into(clock.now, clock.epoch, |edge| {
            if !edge.before_epoch {
                self.window
                    .input
                    .book_slice_at(edge.slice_ns, input.at(clock.now));
                booked += 1;
            }
        });
        pending.retire();
        booked
    }
}

/// Stamp the arrival of PTY-bound input that names a session but no window (a control `send`/`feed` to a background session, a controller host write), keeping the oldest unpresented arrival.
pub(crate) fn note_input() {
    METRICS.note_session_input(now_ns());
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

/// Microsecond view of the metrics clock, the epoch of [`with_key_arrival_queued`].
#[must_use]
pub(crate) fn now_us() -> u64 {
    now_ns() / 1000
}

/// A press keeps its dispatch-time fallback for input/FX even when its native
/// queue age is unavailable. Only observed ages enter hardware distributions.
/// A release shields its dispatch from any enclosing press's stamp.
pub(crate) fn with_key_arrival_queued<R>(
    pressed: bool,
    queue_ns: Option<u64>,
    dispatch: impl FnOnce() -> R,
) -> R {
    METRICS.with_key_dispatch(pressed, queue_ns, dispatch)
}

/// Disarm the key-arrival stamp after a key's dispatch ends without a PTY write.
pub(crate) fn clear_key_arrival() {
    let _ = METRICS.take_key_arrival();
}

/// The current dispatch's arrival in ms: observed native age when available,
/// otherwise dispatch time. This is input/FX timing, not proof of a native clock.
#[must_use]
pub(crate) fn key_arrival_ms() -> Option<u32> {
    match METRICS.key_arrival_ns() {
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
pub(crate) fn take_key_arrival() -> KeyWriteStamp {
    METRICS.take_key_write_stamp()
}

/// Record a key→write slice against a stamp claimed by [`take_key_arrival`], for writes that complete off the UI thread.
pub(crate) fn note_pty_write_at(key: KeyWriteStamp) {
    let measured = METRICS.record_key_write_at(key, now_ns());
    if let Some(d) = measured
        && latency_trace_on()
    {
        crate::logging::stderr_line!(
            "KEYWRITE key->write={:.2}ms (last OS queue {:.2}ms + UI encode + term_locks \
             + PTY write)",
            d as f64 / 1_000_000.0,
            METRICS.window.input.key_queue.last() as f64 / 1_000_000.0
        );
    }
}

impl Metrics {
    fn with_key_dispatch<R>(
        &self,
        pressed: bool,
        queue_ns: Option<u64>,
        dispatch: impl FnOnce() -> R,
    ) -> R {
        if pressed {
            self.with_key_arrival_queued(queue_ns, dispatch)
        } else {
            self.with_key_arrival_at(0, false, dispatch)
        }
    }

    fn record_key_write_at(&self, key: KeyWriteStamp, now: u64) -> Option<u64> {
        if key.at_ns == 0 {
            None
        } else if key.queue_measured {
            self.record_pty_write_at(key.at_ns, now)
        } else {
            self.window.input.key_write_unavailable.inc();
            None
        }
    }

    fn record_pty_write_at(&self, key: u64, now: u64) -> Option<u64> {
        if key == 0 {
            return None;
        }
        let elapsed = now.saturating_sub(key);
        let _ = self.window.input.key_write.record(elapsed);
        Some(elapsed)
    }

    fn with_key_arrival_queued<R>(&self, queue_ns: Option<u64>, dispatch: impl FnOnce() -> R) -> R {
        let arrival = now_ns().saturating_sub(queue_ns.unwrap_or(0)).max(1);
        if let Some(queue_ns) = queue_ns {
            let _ = self.window.input.key_queue.record(queue_ns);
        } else {
            self.window.input.key_queue_unavailable.inc();
        }
        self.with_key_arrival_at(arrival, queue_ns.is_some(), dispatch)
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

/// Moves one window's lease on a count of windows (`held`) to `new`.
fn transition_window_gauge(held: &mut bool, new: bool, count: &AtomicU64) {
    if *held == new {
        return;
    }
    *held = new;
    if new {
        count.fetch_add(1, Ordering::Relaxed);
    } else {
        let previous = count.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0, "window gauge underflow");
    }
}

impl SyncHoldingGauge {
    pub(crate) const fn new() -> Self {
        Self { held: false }
    }

    pub(crate) fn set(&mut self, held: bool) {
        transition_window_gauge(&mut self.held, held, &METRICS.live.sync_holding_windows);
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
        transition_window_gauge(&mut self.held, false, &METRICS.live.sync_holding_windows);
    }
}

#[cfg(test)]
#[path = "metrics_sync_holding_gauge_tests.rs"]
mod sync_holding_gauge_tests;

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

/// A window event other than `RedrawRequested`, of `kind`, reached the loop.
pub(crate) fn note_os_window_event(kind: OsWindowEvent) {
    METRICS.window.sched.os_window_events.inc(kind.index());
}

/// The stable RepaintKey/predictor/effect gate proved that no pixels changed.
pub(crate) fn note_redraw_early_out() {
    METRICS.window.redraw.early_outs.inc();
}

/// A redraw showed its deck-only change by an overlay-only present (`true`), or the renderer
/// refused one and the redraw presents in full (`false`).
pub(crate) fn note_overlay_present(shown: bool) {
    if shown {
        METRICS.window.redraw.overlay_presents.inc();
    } else {
        METRICS.window.redraw.overlay_refusals.inc();
    }
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
#[path = "metrics_command_cost_tests.rs"]
mod command_cost_tests;

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
#[path = "metrics_effect_stream_tests.rs"]
mod effect_stream_tests;

pub(crate) fn note_redraw_sync_hold() {
    METRICS.window.redraw.sync_holds.inc();
}

pub(crate) fn note_redraw_retry_gated() {
    METRICS.window.redraw.retry_gated.inc();
}

pub(crate) fn note_pre_present(compose_ns: u64) {
    METRICS.window.redraw.note_pre_present(compose_ns);
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    submit_split::open();
}

/// The latest frame's submit, from its compose's end ([`note_pre_present`]) to its present's
/// booking ([`record_present`]): wall time, the UI thread's CPU time and the process's page faults.
/// A debug or `dev-seams` build reads it; a shipped one pays none of its reads on a frame. Each
/// read is taken outside the spans the others report: faults, then CPU time, then the wall clock
/// at the open, and the reverse at the close.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
mod submit_split {
    use super::{METRICS, now_ns};

    thread_local! {
        /// Where the latest frame's submit began on this thread: the wall clock, the thread's CPU
        /// time and the process's page faults.
        static FROM: std::cell::Cell<(u64, u64, u32)> = const { std::cell::Cell::new((0, 0, 0)) };
    }

    pub(super) fn open() {
        let faults = process_faults();
        let cpu = thread_cpu_ns();
        FROM.set((now_ns(), cpu, faults));
    }

    pub(super) fn close() {
        let (wall_from, cpu_from, faults_from) = FROM.get();
        if wall_from == 0 {
            return;
        }
        let wall = now_ns();
        let cpu = thread_cpu_ns();
        let faults = process_faults();
        let present = &METRICS.window.present;
        present.submit_wall.set(wall.saturating_sub(wall_from));
        present.submit_cpu.set(cpu.saturating_sub(cpu_from));
        present
            .submit_faults
            .set(u64::from(faults.wrapping_sub(faults_from)));
    }

    /// The process's page faults so far (`pti_faults` of its task info, which wraps; the read walks
    /// the whole task); 0 off macOS.
    fn process_faults() -> u32 {
        #[cfg(target_os = "macos")]
        {
            crate::input_stall::pid_task_info(std::process::id().cast_signed())
                .map_or(0, |info| info.pti_faults.cast_unsigned())
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// The calling thread's CPU time in nanoseconds; 0 where it cannot be read.
    fn thread_cpu_ns() -> u64 {
        #[cfg(unix)]
        if let Some(stamp) = super::calling_thread_cpu_stamp() {
            return u64::try_from(stamp.tv_sec)
                .unwrap_or(0)
                .saturating_mul(1_000_000_000)
                .saturating_add(u64::try_from(stamp.tv_nsec).unwrap_or(0));
        }
        0
    }
}

#[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
fn calling_thread_cpu_stamp() -> Option<libc::timespec> {
    let mut stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `stamp` is a valid, writable `timespec` that outlives the call.
    (unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut stamp) } == 0)
        .then_some(stamp)
}

/// The calling thread's CPU time in ns, unknown on an unread or unrepresentable clock.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) fn calling_thread_cpu_ns() -> Option<u64> {
    #[cfg(unix)]
    {
        let stamp = calling_thread_cpu_stamp()?;
        let seconds = u64::try_from(stamp.tv_sec).ok()?;
        let nanos = u64::try_from(stamp.tv_nsec)
            .ok()
            .filter(|n| *n < 1_000_000_000)?;
        seconds.checked_mul(1_000_000_000)?.checked_add(nanos)
    }
    #[cfg(not(unix))]
    {
        None
    }
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

/// A reading retargeted the dial's needle, or a layout rested it, its new episode owing the windows `owed` frames.
pub(crate) fn note_needle_episode(owed: u64) {
    METRICS.note_needle_episode(owed);
}

/// What the needle's latest episode has left to fire: what it owed less the `deck_clock` deadlines fired since.
fn needle_episode_left(live: &Live) -> u64 {
    live.needle_episode_owed
        .load(Ordering::Relaxed)
        .saturating_sub(live.needle_episode_fired.load(Ordering::Relaxed))
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
    fn note_needle_episode(&self, owed: u64) {
        if owed > 0 {
            self.window.sched.needle_sweeps.inc();
            self.window.sched.needle_frames_owed.add(owed);
        }
        self.live.needle_episode_fired.store(0, Ordering::Relaxed);
        self.live.needle_episode_owed.store(owed, Ordering::Relaxed);
    }

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
                if owner == DeadlineOwner::DeckClock {
                    live.needle_episode_fired.fetch_add(1, Ordering::Relaxed);
                }
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
    s.decimal("last_deck_frame_ms", ms(w.present.deck_frame.last()));
    s.decimal("max_deck_frame_ms", ms(w.present.deck_frame.max()));
    s.count("deck_clock_ticks", w.present.deck_clock_ticks.get());
    s.count("deck_clock_frames", w.present.deck_clock_frames.get());
    s.decimal("deck_clock_run_ms", ms(w.present.deck_clock_run.get()));
    s.count(
        "deck_clock_link_ticks",
        w.present.deck_clock_link_ticks.get(),
    );
    s.count(
        "deck_clock_link_fallbacks",
        w.present.deck_clock_link_fallbacks.get(),
    );
    s.count(
        "deck_clock_link_resumes",
        w.present.deck_clock_link_resumes.get(),
    );
    s.count("deck_clock_links_silent", deck_links_silent());
    s.decimal(
        "last_deck_glass_late_ms",
        ms(w.present.deck_glass_late.last()),
    );
    s.decimal(
        "max_deck_glass_late_ms",
        ms(w.present.deck_glass_late.max()),
    );
    s.count("slow_frames", w.present.slow.get());
    s.decimal("slow_threshold_ms", ms1(SLOW_FRAME_THRESHOLD_NS));
    s.decimal("last_input_present_ms", ms(w.input.input.last()));
    s.decimal("max_input_present_ms", ms(w.input.input.max()));
    emit_quantiles(
        s,
        ["n_input", "input_p50_ms", "input_p95_ms", "input_p99_ms"],
        &h_input,
    );
    emit_cold_input(&w.input, s);
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
    emit_key_clock_availability(&w.input, s);
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
    s.count("redraw_overlay_presents", w.redraw.overlay_presents.get());
    s.count("redraw_overlay_refusals", w.redraw.overlay_refusals.get());
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
    let os_events = w.sched.os_window_events.sparse(os_event_labels());
    s.count(
        "os_window_events",
        os_events.iter().map(|(_, count)| count).sum(),
    );
    s.count_pairs("os_window_events_by_kind", &os_events, "none");
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
    s.count("needle_sweeps", w.sched.needle_sweeps.get());
    s.count("needle_frames_owed", w.sched.needle_frames_owed.get());
    s.count("needle_episode_left", needle_episode_left(live));
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
        #[cfg(unix)]
        emit_commons_cadence(s);
    }
    emit_gpu(ctx, s);
}

/// Commons repair's counts in this process since it started (`aterm_db::runtime::cadence`):
/// each only rises, so no `reset` clears one and a reader subtracts two readings. Commons
/// repair runs on unix alone, so a Windows build carries none of them.
#[cfg(unix)]
fn emit_commons_cadence(s: &mut dyn wire::FieldSink) {
    let counts = aterm_db::runtime::cadence();
    s.count("commons_rounds_out", counts.rounds_out);
    s.count("commons_rounds_in", counts.rounds_in);
    s.count("commons_rounds_quiet", counts.rounds_quiet);
    s.count("commons_owner_ticks", counts.owner_ticks);
    s.count("commons_beacons", counts.beacons);
    s.count("commons_announces", counts.announces);
    s.count("commons_signature_checks", counts.signature_checks);
    s.count("commons_deadline_spawns", counts.deadline_spawns);
}

pub(crate) fn emit_percentiles(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    use wire::ms;
    let w = &ctx.metrics.window;
    emit_quantiles(
        s,
        ["n_input", "input_p50_ms", "input_p95_ms", "input_p99_ms"],
        &w.input.input.copy_out(),
    );
    emit_cold_input(&w.input, s);
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
            "n_deck_frame",
            "deck_frame_p50_ms",
            "deck_frame_p95_ms",
            "deck_frame_p99_ms",
        ],
        &w.present.deck_frame.copy_out(),
    );
    emit_quantiles(
        s,
        [
            "n_deck_glass_late",
            "deck_glass_late_p50_ms",
            "deck_glass_late_p95_ms",
            "deck_glass_late_p99_ms",
        ],
        &w.present.deck_glass_late.copy_out(),
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
    emit_key_clock_availability(&w.input, s);
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

fn emit_key_clock_availability(input: &InputLedger, s: &mut dyn wire::FieldSink) {
    s.count("key_queue_unavailable", input.key_queue_unavailable.get());
    s.count("key_write_unavailable", input.key_write_unavailable.get());
}

/// Beside `n_input`, in `metrics` and `metrics percentiles` alike: the slices booked in a cold
/// present and the rest-wake keys ([`InputLedger::input_cold`], [`InputLedger::rest_wake_key`]).
fn emit_cold_input(input: &InputLedger, s: &mut dyn wire::FieldSink) {
    emit_quantiles(
        s,
        [
            "n_input_cold",
            "input_cold_p50_ms",
            "input_cold_p95_ms",
            "input_cold_p99_ms",
        ],
        &input.input_cold.copy_out(),
    );
    s.decimal("last_input_cold_ms", wire::ms6(input.input_cold.last()));
    s.decimal("max_input_cold_ms", wire::ms6(input.input_cold.max()));
    s.count("n_rest_wake_key", input.rest_wake_key.get());
}

fn emit_quantiles(s: &mut dyn wire::FieldSink, keys: [&str; 4], h: &HistCopy) {
    let unit = if matches!(keys[0], "n_input" | "n_input_cold" | "n_present") {
        wire::ms6
    } else {
        wire::ms
    };
    emit_quantiles_in(s, keys, h, unit);
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
#[path = "metrics_ledger_tests.rs"]
mod ledger_tests;

#[cfg(test)]
#[path = "metrics_present_glass_tests.rs"]
mod present_glass_tests;

#[cfg(test)]
#[path = "metrics_gpu_tests.rs"]
mod gpu_tests;

#[cfg(test)]
#[path = "metrics_dispatch_clock_ownership_tests.rs"]
mod dispatch_clock_ownership_tests;

#[cfg(test)]
#[path = "metrics_window_input_attribution_tests.rs"]
mod window_input_attribution_tests;

#[cfg(test)]
#[path = "metrics_chrome_input_receipt_tests.rs"]
mod chrome_input_receipt_tests;

#[cfg(test)]
#[path = "metrics_present_clock_tests.rs"]
mod present_clock_tests;

/// The cold-key and rest-wake counters (ATERM-KI-1027-4): which slices a present books as cold
/// ([`super::COLD_GAP_NS`], the process's gap, whatever the epoch) and which as rest-wake keys
/// (a stir that woke a focused window, cleared by that window's next present). Instant and ns
/// arithmetic only.
#[cfg(test)]
#[path = "metrics_cold_input_tests.rs"]
mod cold_input_tests;

#[cfg(test)]
#[path = "metrics_lateness_attribution_tests.rs"]
mod lateness_attribution_tests;

#[cfg(test)]
#[path = "metrics_window_tests.rs"]
mod window_tests;
