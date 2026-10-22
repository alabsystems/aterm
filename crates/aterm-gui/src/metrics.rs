// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-global render/latency counters, read over the control socket by the `metrics`
//! verb so a driver can measure responsiveness and detect lag directly.
//!
//! aterm runs exactly one [`crate::App`] / event loop per process, so one `static`
//! root, [`METRICS`], is sufficient and avoids threading an `Arc` through the
//! control listener: the App (writer, on the present path) and the control thread
//! (reader, in `cmd_metrics`) live in the same process. The root's [`Window`] is
//! built of typed instruments ([`instrument`]), each declared once in a
//! `window_ledger!` whose generated reset covers every field; what must survive a
//! reset lives in [`Live`], which has no reset at all. All ops are `Relaxed` —
//! these are monotone diagnostics, never used for synchronization — except the
//! frame count's Release/Acquire pair, which publishes the startup sample.
//!
//! ## Reading the counters
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
//!   observe compositor selection, display timing, scanout, or photons. IT IS AN
//!   OPEN INTERVAL: the stamp is a first-edge-wins `compare_exchange(0, …)` in
//!   `spawn::stamp_output_arrival`, cleared only by a content present, so output
//!   that moves no pixels opens the interval and leaves it open — and every
//!   millisecond in which nothing presented is inside the reading, bounded only
//!   by a 5 s discard. A multi-second value means "nothing presented for that
//!   long", not "a frame took that long".
//! - `present_glass` (`metrics percentiles`: `n_present_glass`,
//!   `present_glass_p*_ms`, `last_/max_present_glass_ms`, `present_glass_skipped`)
//!   — the leg AFTER application present-return: `presentDrawable:` registration
//!   → the drawable's `presentedTime` (`aterm_gpu::present_glass`), process-wide
//!   over every window. A slow `present_latency` with a fast `present_glass` was
//!   the main thread; a slow `present_glass` was the compositor. A drawable never
//!   shown counts in `present_glass_skipped`, not in the distribution.
//! - `gpu_time` (`n_gpu_time` and `gpu_time_p50/p95/p99_ms`) is the device's
//!   busy time per present, virtual presents included: the sum of its Metal
//!   command buffers' execution spans (they run in commit order), or the union
//!   of its wgpu render passes' timestamp spans, read once the device reports
//!   the frame's work done. `gpu_time_source=device` names observed device samples;
//!   `unobserved` means a supported host has no sample, and `unsupported`
//!   means this build has no device clock wired. A wgpu device must grant
//!   timestamp queries. GL contributes no device samples; an empty
//!   distribution is not measured zero time.
//!   CPU encode is a separate distribution. `passes_last`, built instances
//!   and bytes describe the last encoded offscreen after a successful GPU
//!   present, which may reuse it; these are not transfer-completion counts.
//! - `last_/max_frame_render_ns` — causal CPU wall time: compose plus CPU
//!   raster/copy, or time spent encoding GPU commands and calling `queue.submit`,
//!   most recent and worst-since-reset. This is not completed GPU execution;
//!   surface acquisition and final-present pacing are deliberately excluded.
//! - `last_/max_gpu_park_ms` — the OTHER main-thread GPU block on the armed
//!   Metal arm: the Submit-A pipelining wait plus the pending-ring drain, both
//!   `waitUntilCompleted` with no timeout. It is completed GPU execution, so it
//!   is excluded from `frame_render` (which used to absorb the frame-slot half
//!   and price a contention stall as slow compose) and published here instead.
//!   A large `gpu_park` with a small `frame_render` is the GPU or WindowServer,
//!   not the compose path.
//! - `slow_frames` — frames whose render time blew the [`SLOW_FRAME_THRESHOLD_NS`]
//!   (~30 fps) budget. A rising count is the lag signature — most often the CPU
//!   rasterizer redrawing heavy colour output (the "GPU was off" trap), so
//!   `backend=cpu` + climbing `slow_frames`/`max_frame_render_ms` is what to watch.
//! - `max_wake_late_ms` / `max_deadline_late_ms`, each with the OWNER that
//!   produced it and an `_at_ms` instant — the worst event-loop wake and
//!   deadline lateness since reset. `wake_late_ms`/`deadline_late_ms` beside
//!   them are LAST-WRITER readings that the next wake (or the next arm) erases
//!   within a frame, so only these can report a late timer after the fact.
//! - `max_present_latency_at_ms` / `max_input_present_at_ms` and
//!   `max_present_latency_gap_ms` — WHEN the worst sample happened, on the same
//!   process clock as `first_present_ms` and anchored by `metrics_now_ms`, plus
//!   the gap since the presenting window's previous present for the frame that
//!   set the present max: a gap the size of the reading means that window
//!   presented nothing for that long (an open interval), a far smaller one
//!   means its frames kept coming while that output waited.
//! - `max_turn_ms` / `max_turn_owner` / `max_turn_at_ms`, with `last_turn_ms`,
//!   `turns` and `long_turns` — the MAIN-LOOP TURN census
//!   (`crate::watchdog`): how long the main thread spent in one winit root, and
//!   which root. Everything else here prices the redraw or a wake's lateness, so
//!   a 100–600 ms park in a NON-redraw handler — the `Wake::Output` arm's status
//!   observation, title drift and search refresh all run before the redraw
//!   fan-out — used to leave no attributable trace at all between a slow frame
//!   and the 5 s release stall watchdog, while still being charged to
//!   `present_latency` and `input_present`. Read `max_turn_ms` AGAINST
//!   `max_redraw_total_ms`: both large is a slow frame, a large `max_turn_ms`
//!   with a small `max_redraw_total_ms` is a park outside the redraw, named by
//!   its owner.
//! - `backend_gpu` — `true` when the live renderer is the GPU (Metal) path.
//! - `scrollback_truncated_lines` (summary only; read from the active terminal
//!   under `try_lock`, `busy`/`null` like `rows`/`cols`) — history lines the
//!   engine has LOST since the session began that the configured line limit
//!   would have kept: output that arrived faster than the scrollback
//!   compression worker could retain it once the staged backlog filled the
//!   scrollback's memory headroom (`Grid::flood_overflow`), plus store
//!   evictions under memory pressure. Lines past the line limit are ordinary
//!   eviction and never count; each cut adds the most lines it was missing
//!   inside the limit at once. Not a latency figure and not zeroed by
//!   `metrics reset` — it is the engine's own monotone counter, published here
//!   because the 2026-09-22 audit found it reached no surface at all: 120k
//!   lines printed into a 100k scrollback kept 28.5k, and nothing said the
//!   other 91k were gone. The same drops leave one dim `— aterm dropped N
//!   lines here …` row in the history at each cut, naming the lines missing
//!   there now; `lines` reports `truncated=<n>` when nonzero.
//! - `journal_resident_bytes` (summary only) — the RAM the crash-restore journal
//!   costs this window: every page the regions of its sessions have written,
//!   summed with no lock as each region's store counts them
//!   (`crash_restore::resident_bytes`). Not a latency figure and not zeroed by
//!   `metrics reset`; it falls when memory pressure sheds a history ring (the
//!   ring's region is replaced and unmapped) and when a session closes. Each
//!   session's own share is `status journal_bytes=`; `pressure` says what the
//!   shed did.
//!
//! To detect lag: `metrics reset`, drive the workload, then `metrics`. `slow_frames > 0`,
//! a large `max_frame_render_ms`, or `backend=cpu` under heavy output means lag.
//!
//! ## Two rules
//!
//! **An instrument that perturbs the system under test proves nothing.** A
//! counter here must describe ATERM, not the act of measuring it. Where the two
//! cannot be separated, the contaminated samples get their OWN ledger and both
//! are published — never a silent filter, never a poisoned average. See
//! [`record_offscreen_raster`] (a screenshot must not move the on-glass frame
//! ledger) and `PresentLedger::tainted` (an occluded window, or a
//! `video` capture pacing presents, must not move the output→glass histogram).
//!
//! **A number that cannot name its producer cannot end an investigation.** A
//! last-writer label over a min-fold of ~35 candidates points at whichever one
//! happened to win; that is how the 2026-08 event-loop spin was reported against
//! `title_summary` for its first hour. `SchedLedger::arms_by_owner` books arms —
//! and past arms — per owner instead, and `fires_by_owner` the timers that
//! actually woke the loop (`deadline_fires_by_owner`: an arm is a fold win, not a
//! wake). `wakes_by_variant` counts the delivered proxy events by `Wake` variant,
//! `metrics percentiles` carries each main-loop root's turn distribution
//! (`turns_by_root`), and `gpu_scissor_taken` / `gpu_full_repaints` /
//! `gpu_scroll_rescues` publish the renderer's repaint scope (2026-09-25,
//! responsiveness audit items 2, 3 and 5).

pub(crate) mod instrument;
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
// The boot's entry points into the startup ledger, where its call sites have
// always named them.
#[cfg(unix)]
pub(crate) use startup::claim_timeline_log_text;
pub(crate) use startup::{
    ClaimStamp, StartupAttachMilestones, StartupPresentTiming, StartupWorkerLegs, leg_elapsed_ns,
    mark_backend_worker_done, mark_backend_worker_spawn, mark_first_window_visible,
    mark_first_winit_resumed, mark_gui_ready_for_winit, mark_initial_surface_ready,
    mark_process_start, mark_rust_main_start, record_backend_worker_legs,
    record_initial_attach_milestones, stamp_claim,
};
#[cfg(target_os = "macos")]
pub(crate) use startup::{mark_handoff_claimed, process_main_start};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The process's metrics, written by the App and read by the control thread's
/// `metrics` verb. Tests drive a [`Metrics::new`] of their own, so what one
/// writes reaches no other.
pub(crate) static METRICS: Metrics = Metrics::new();

/// A metrics root: the measurement window, the live state beside it, and the
/// window's epoch.
pub(crate) struct Metrics {
    /// Every statistic `metrics reset` zeroes.
    pub(crate) window: Window,
    /// The state it never touches.
    pub(crate) live: Live,
    /// `now_ns` at the last [`reset`]: the epoch of all present-side state the
    /// UI thread owns. A window's [`WindowPresentState`] (its pending keys and
    /// its previous present) and the App's [`PresentObserver`] (the slow-present
    /// episode and the previous present of any window) are not in the root, so
    /// `reset` cannot write them; the next present reads this epoch and treats
    /// what was stamped before it as the previous measurement window's. A key
    /// ARMED before it books nothing into the input ledger, a present before it
    /// opens no gap, and an episode whose
    /// last slow present came before it is closed with its line.
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

    /// Open a new measurement window: every window ledger is zeroed by its
    /// generated reset, and the epoch moves to now. [`Live`] has no reset, and
    /// the UI thread's present-side state reads the epoch instead of being
    /// written. aterm-core's fx totals are sampled first
    /// ([`GpuLedger::sample_fx_totals`]), so the window's base for them is
    /// their value at this reset.
    pub(crate) fn reset(&self) {
        self.window.gpu.sample_fx_totals();
        self.window.reset();
        self.epoch.store(now_ns(), Ordering::Relaxed);
    }
}

window_ledger! {
    /// The measurement window, by ledger: every statistic that describes the
    /// time since the last `metrics reset`, which zeroes all of it. That
    /// includes each `last_*` that is an observation of the window, so a `0.00`
    /// means "no sample in this window", never the previous run's number.
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
    /// What the application presents measure, written on the UI thread
    /// ([`Metrics::record_present_at`], the resize stamps, the offscreen
    /// rasters, the capture episodes).
    pub(crate) struct PresentLedger {
        /// Successful application presents (`frames`). The D-1 early-out
        /// returns before present, so a steady app-render frame does not
        /// advance it. In the process's root it is counted with Release after
        /// [`record_present`] publishes the startup sample
        /// ([`startup::Startup::publish_first_present`]): a reader that
        /// acquires frame 1 also reads that sample.
        pub(crate) frames: Counter,
        /// Output→application-present-return on glass (the module header's
        /// `present_latency`, an OPEN interval).
        pub(crate) latency: Dist,
        /// When the worst `latency` was booked (`now_ns`), and the gap since
        /// its window's previous present: see [`SchedLedger::wake_late`].
        pub(crate) latency_at: Last,
        pub(crate) latency_gap: Last,
        // ---- PRESENT-LATENCY HONESTY: the two contaminating EPISODES ----------
        //
        // THE OBSERVED DEFECT (responsiveness audit, item 10). A live read showed
        // `present_p50=1.31ms` next to `present_p95=671ms / p99=1006ms`, with
        // `n_present=5663` against `frames=17706`. A p95 five hundred times the
        // median is not a tail of the same distribution — it is a SECOND
        // distribution mixed into the first, and it made the published
        // output→glass figure unusable in exactly the direction that hides
        // regressions: nobody trusts a number whose tail is already absurd.
        //
        // WHERE THE SECOND DISTRIBUTION COMES FROM. `present_latency_ns` measures
        // PTY-output leading edge → the present that showed it. That is honest
        // only while the window is actually presenting. Two episodes break that:
        //
        //   1. OCCLUSION / PARK. A `GpuOccluded` drop, or any drop the retry
        //      scheduler parked awaiting an external surface stimulus, means
        //      frames stopped reaching glass for an interval nobody was watching.
        //      Output keeps arriving and keeps stamping. The first present after
        //      the episode books the whole unwatched interval as render latency —
        //      under the 5 s cap, so the existing honesty bound passes it straight
        //      through.
        //   2. CAPTURE. A `video` recording paces presents on the RECORDER's
        //      schedule, not the output's, and (the 2026-08 blackout's root cause)
        //      pins gates the unrecorded path does not have. A capture-based
        //      instrument that moves the number it is reading proves nothing —
        //      see docs/RELEASE-PROOF-DISCIPLINE.md.
        //      A still capture (`image`, `window`, the SIGUSR1 snapshot) forces its
        //      own presents, on the CALLER's schedule, inside one synchronous
        //      barrier. Those presents say so themselves (`record_present`'s
        //      `capture_forced`), so the taint ends with the barrier and leaves no
        //      tail on the presents after it. Each such sample is also counted in
        //      `capture_presents`, so the tainted ledger says how much of it a
        //      still capture forced.
        //
        // THE FIX FOLLOWS `record_offscreen_raster` EXACTLY: do not discard the
        // sample, and do not let it touch the on-glass ledger. A sample taken
        // inside an episode (`Live::taint_until`, `Live::capture_depth`) goes to
        // its OWN distribution and its own last/max, so introspection and
        // occluded runs stay observable while `present_p95` means what it says.
        // Both ledgers are published, so the split is auditable rather than a
        // silent filter.
        pub(crate) tainted: Dist,
        /// Output→present samples diverted to `tainted`. A non-zero value is
        /// why `frames` exceeds `n_present`.
        pub(crate) tainted_samples: Counter,
        /// `video` capture episodes opened in the window. Any non-zero value
        /// means a capture-based instrument ran inside this measurement window:
        /// read every number in it knowing the recorder was in the loop.
        pub(crate) capture_episodes: Counter,
        /// Output→present samples a still capture's barrier forced (`image`,
        /// `window`, the SIGUSR1 snapshot). Each is also one of
        /// `tainted_samples`; the rest of those are occlusion/park or a `video`
        /// recording.
        pub(crate) capture_presents: Counter,
        /// Causal frame CPU work: compose plus CPU raster/copy, or encoding GPU
        /// commands and calling `queue.submit`.
        pub(crate) render: Dist,
        /// Frames whose `render` exceeded [`SLOW_FRAME_THRESHOLD_NS`].
        pub(crate) slow: Counter,
        // Inter-frame GAP: wall time between two CONSECUTIVE presents
        // (present→present), the direct hitch/stutter signal ARENA-SCROLL wants —
        // `max_frame_render_ms` times ONE frame's own work, but a scrub that skips
        // a frame (tier decode overran the vsync deadline, or the term lock was
        // held by a compression spike) shows up as a GAP, not a slow single frame.
        // Process-wide: the previous present of ANY window, which the UI thread
        // keeps in its [`PresentObserver`] (a present before the last reset opens
        // no gap, so the first present after a reset records none against stale
        // history). MEANINGFUL ONLY inside a `reset` → drive-continuously → read
        // window: presents stop at idle by design (the D-1 early-out), so a pause
        // between driven steps is itself a real gap — pace the redraws (as the
        // `video [pace]` verb does) for a clean cadence read.
        pub(crate) frame_gap: Max,
        // ---- RESIZE → PRESENT: the stale-frame (compositor stretch) window -----
        //
        // THE THING THIS MEASURES, precisely: from a window-bounds change arriving
        // (`WindowEvent::Resized`) to aterm having SUBMITTED a frame drawn at the
        // new size. For that whole interval the layer is already the new size
        // while the most recent drawable is the old one, so CoreAnimation shows
        // the previous frame rescaled onto the new bounds — the smeared
        // "shredded" text of a live drag. Shrinking this interval IS the fix;
        // this is how you check it in milliseconds instead of by eye.
        //
        // WHY A METRIC AND NOT THE `video` LEDGER. The swapchain tap allocates its
        // ring for one fixed geometry and sets `resized_early_stop` the instant
        // the frame texture changes size (`aterm-gpu/src/video_tap.rs`), so a
        // recording DIES on the first resize — the one event it would need to
        // observe. A counter pair on the present path has no geometry to outgrow,
        // costs two relaxed atomics, and is always on like every other slice here.
        //
        // KEEP-OLDEST, like the session-routed input stamp. A drag delivers a
        // burst of bounds changes; the honest number is how long the window spent
        // showing a frame that did not match it, so a later change must not reset
        // the clock and shrink the slice. The stamp is armed BEFORE the surface
        // reconfigure, so it cannot miss the gap it is measuring.
        //
        // HONESTY BOUND: the close is "the next present after a bounds change",
        // and the resize handler reconfigures the swapchain to the new size before
        // that present happens — so the frame is at the new size by construction.
        // It does NOT prove the grid REFLOWED to the new size (a width drag
        // reflows on the throttle's own schedule); it proves a correctly-SIZED
        // frame was submitted, which is exactly what ends the compositor's
        // rescale. A slice past the cap is a window that stopped presenting
        // entirely (occluded, parked surface) and is discarded rather than booked
        // as a resize stall.
        pub(crate) resize_stamp: Stamp,
        pub(crate) resize_present: Dist,
        // ---- RESIZE → REFLOW: the stale-GRID window ----------------------------
        //
        // THE SECOND HALF, and the one `resize_present` structurally cannot see.
        //
        // `resize_present` closes on a frame at the new SURFACE size, and the
        // surface is reconfigured in the `Resized` handler BEFORE the reflow
        // throttle runs — so it goes green the moment the swapchain matches the
        // window, whether or not the GRID has caught up. What a dragging user
        // actually watches is the text: the columns and rows the engine
        // committed. Between a bounds change and that commit the terminal body is
        // the OLD grid letterboxed into the NEW window, which is the content
        // trailing the window edge before it snaps.
        //
        // So this is armed with the same event and closed at the engine commit
        // (`apply_term_resize` returning that it changed the geometry). The two
        // together bracket a resize: `resize_present` = "did a correctly-sized
        // frame go out", `resize_reflow` = "did the text catch up". A width drag
        // deliberately keeps the second one long — that is the throttle bounding
        // scrollback rewrap — while a row-only drag should have it near zero,
        // because nothing needs rewrapping.
        //
        // KEEP-OLDEST and capped for the same reasons as its twin. A drag that
        // coalesces several bounds changes into one commit reports the whole span
        // it was stale, not just the tail; a resize whose commit never comes (the
        // geometry did not change) is discarded rather than booked against
        // whatever commits next.
        pub(crate) reflow_stamp: Stamp,
        pub(crate) resize_reflow: Dist,
        /// OFFSCREEN rasterizations (introspection `image`/`window`/`snapshot`):
        /// how many, and the last and worst cost, counted apart from real
        /// presents so a screenshot can never move the on-glass ledger (see
        /// [`record_offscreen_raster`]). Deliberately NOT folded back into
        /// `frames` / `max_frame_render_ms` / `slow_frames` / `max_frame_gap_ms`
        /// — that contamination is the bug these were split off to fix.
        pub(crate) offscreen_rasters: Counter,
        pub(crate) offscreen: Peak,
    }
}

window_ledger! {
    /// What input measures: the keystroke slices, the input legs the typing
    /// audit named as unmeasured, and the refusals of the input contract; and
    /// the main-thread costs `metrics percentiles` prints beside them (the
    /// menu-bar glance, a need publication, the transcript apply's holds).
    pub(crate) struct InputLedger {
        // ---- Input→present latency: the slice a HUMAN feels when typing ----------
        // (key arrival → the next CONTENT present, or the present that first
        // shows the key's proven prediction: PREDICTIONS below).
        //
        // HONESTY BOUNDS: (1) a keystroke that produces NO output (`stty -echo`
        // password prompts, an ignoring app) must not poison the metric when
        // unrelated output arrives minutes later — a slice older than
        // `INPUT_SLICE_CAP_NS` is DISCARDED at consume time, and `reset` drops any
        // pending stamp. (2) The slice closes on the next content present OF THE
        // WINDOW THE KEY WAS TYPED INTO.
        //
        // PER-WINDOW ATTRIBUTION. A keystroke routed to a window (every key that
        // reaches `App::input_to_session`, hardware or controller) arms that
        // window's [`PendingInputStamp`], and only that window's content present
        // closes it. This used to be one process-wide stamp closed by ANY window's
        // content present: with the owner typing into window 0 while a streaming
        // session presented in window 1, window 1's log-line frame closed the
        // key's slice at a few ms and window 0's real echo, presented ~90 ms
        // later, found no stamp and recorded nothing — `input_p99` under-reported
        // precisely during the lag episode. The distribution and scalars stay
        // process-global: they are a max-fold over every window's attributed
        // slices.
        //
        // COORDINATED OMISSION (LAT-3; docs/WINDOWS_PARAGON_DESIGN.md item 2.5,
        // landed 2026-09-25): each window's `PendingInputStamp` is a bounded EDGE
        // RING (`INPUT_EDGE_RING` keystrokes), and a content present drains EVERY
        // edge into the distribution, so the keys typed during a stall are each a
        // sample rather than hidden behind the burst's oldest. A burst past the
        // ring's capacity keeps its OLDEST edges and counts the rest in
        // `edges_dropped` (published beside the percentiles), so the loss is
        // visible.
        //
        // NATIVE PAGES. A native page answers a key with pixels, never PTY
        // output, so a content present never closes a key it took: such a key
        // arms the window's native ring instead, and the present that puts the
        // page's answer on glass closes it into this same distribution
        // ([`record_native_content_present`]). Typing latency is one figure
        // whichever surface the keys land in.
        //
        // PREDICTIONS. A key whose effect predictive echo shows is on glass
        // before its echo, as a native page's answer is: a present with no
        // output that painted the predictor's overlay whole, under its proof
        // (`predictive_echo = "adaptive"`: every op it shows passed the display
        // gate), closes the slices of the keys whose prediction it shows
        // ([`note_prediction_shown`]), and `predicted` counts them. A key that
        // armed no op, whose op is still dark, or that `always` shows with no
        // proof waits for its content present, as with prediction off; so do
        // the keys after it. Over a slow link `input_*` is then the press to
        // the key's prediction on glass for the keys shown early, and the
        // press to the echo for the rest; `echo_*` still times the round trip.
        pub(crate) input: Dist,
        /// When the worst `input` slice was booked (`now_ns`).
        pub(crate) input_at: Last,
        // The session-routed stamp survives only for input that names a SESSION,
        // not a window (a control `send`/`feed` to a background session, a
        // controller host write): which window presents that session is not known
        // at the stamp, so any window's content present closes it and, under
        // concurrent output elsewhere, it can read low. Within one window,
        // concurrent streaming output in the SAME window can still close the slice
        // on a log-line frame rather than the echo (reads low); the cross-window
        // case is the one the per-window split removes. Keep-oldest, and the one
        // keep-oldest input lane left.
        pub(crate) session_stamp: Stamp,
        /// Keystrokes a full edge ring could not hold (see [`INPUT_EDGE_RING`]).
        /// Their slices are missing from `input`; this count says how many.
        pub(crate) edges_dropped: Counter,
        /// Of the slices in `input`, those a PREDICTION closed: the key's
        /// slice ended at the present that first painted its proven
        /// prediction, before its echo (the PREDICTIONS note on `input`).
        /// The rest of `n_input` closed at a content present, as with
        /// prediction off, so an A/B across prediction modes reads this too.
        pub(crate) predicted: Counter,
        /// Hardware-key arrival → completed PTY write (`key_write`): the
        /// UI-thread key→write component inside `input`, from the BACKDATED
        /// arrival (see the latency trace below `note_grid_committed`).
        pub(crate) key_write: Dist,
        /// The NSEvent-QUEUE RESIDENCE that `key_write` is BACKDATED by: how
        /// long each hardware key sat in the OS event queue before the loop
        /// dispatched it.
        ///
        /// WHY IT IS PUBLISHED APART. [`note_key_arrival_queued`] stamps the
        /// arrival at `now - queue_ns` deliberately, so `key_write` and
        /// `input_present` both price a parked event loop — the slice the
        /// touch-to-glass audit proved every instrument was blind to. Keeping
        /// ONLY that backdated stamp, though, left the two causes
        /// indistinguishable afterwards: a `max_key_write_ms=18.13` beside a
        /// `key_write_p99_ms=6.29` is a main thread parked somewhere ELSE (a
        /// present, a `term_lock` hold, a compose) if the key waited 17 ms to be
        /// dequeued, and aterm's own press path if it waited none. Opposite
        /// fixes, one number. With this leg published, `key_write - key_queue`
        /// at matching percentiles is the pure UI-DISPATCH slice, and a large
        /// `key_queue` against a small `acquire` says the loop was parked
        /// somewhere the slow-present breadcrumb can name.
        ///
        /// READ IT WITH ITS COUNT. Every PRESSED key reaching the winit arm
        /// books a sample here, while only a key that WRITES books one in
        /// `key_write`, so `n_key_queue >= n_key_write` by construction. A sample
        /// is the backdate that was actually APPLIED, which is why an
        /// unmeasurable age (no `NSApp` `currentEvent`, a synthesized event, a
        /// Wayland or Windows build, an X11 stamp from a clock that is not ours —
        /// see `platform::current_event_queue_age_ns`) books `0` rather than
        /// nothing: zero is the honest statement that nothing was added to that
        /// key's `key_write`.
        ///
        /// ON X11 THE LEG IS WIDER THAN A QUEUE. The age there runs from the X
        /// server's stamp, so it also carries the input-method round trip every
        /// key makes when `XMODIFIERS` names one (ibus: ~0.5-5 ms median,
        /// measured) — time the user waits and aterm does not spend, so it
        /// belongs here, beside `key_write`, rather than inside aterm's own
        /// dispatch.
        pub(crate) key_queue: Dist,
        // THE INPUT LEGS the typing audit named as unmeasured
        // (docs/AUDIT-typing-to-pixels-2026-08-26.md, P1 "measure the matching
        // key and matching frame"), plus the refusal count of the 2026-09-25
        // input contract.
        //
        // OUTPUT → REDRAW REQUEST. `present_*` runs from a PTY burst's leading
        // edge to the frame that presents it, and until this leg existed nothing
        // said how much of that was the wake reaching the event loop (the
        // reader's post, the loop's queue, whatever the UI thread was busy with)
        // and how much was pacing, compose and present after the loop had asked
        // for the frame. This is the first half: the burst's leading edge (the
        // same `last_output_ns` stamp) → the `Wake::Output` turn that admits the
        // visible session's redraw — the instant a frame is owed. Booked once per
        // burst (a second wake before the present finds the same stamp already
        // booked) and only for a visible session: a hidden one requests nothing.
        // The 5 s honesty cap of `present_latency` applies: a stamp older than
        // that aged unwatched.
        pub(crate) out_req: Dist,
        // IME PREEDIT → COMMIT. How long a composition stays open, from the
        // preedit that starts it to the commit that ends it — the clock a CJK or
        // dead-key typist feels as "how long until my character is typed", which
        // no key slice can see (the key events inside a composition write
        // nothing). A composition that ends without a commit (cleared, focus
        // lost, disabled) books no duration and is counted as cancelled.
        pub(crate) ime: Histogram,
        pub(crate) ime_cancelled: Counter,
        /// Input REFUSED because its session's input queue was full: the
        /// program has stopped reading and the key (or paste) was refused —
        /// said once per session, `App::refuse_input` — instead of parking the
        /// event loop or queueing without bound (decided 2026-09-25).
        pub(crate) refused: Counter,
        /// Terminal query REPLIES dropped at submission
        /// (`spawn::ReplyLane::submit`): the program floods queries it never
        /// reads the answers to, and the reply lane's byte budget or count cap
        /// is full.
        pub(crate) reply_dropped: Counter,
        // THE MENU-BAR ITEM'S GLANCE: what one build of it costs on the main
        // thread (`App::status_glance`, every session's published verdict and
        // task record), the cost the dirty flag pays once per loop turn rather
        // than once per change (`docs/DESIGN-owner-surfaces-2026-10-16.md` §9 S3),
        // and the worst build.
        pub(crate) status_glance: Histogram,
        pub(crate) status_glance_max: Max,
        // ONE SESSION'S NEED PUBLICATION: what one `App::publish_need` costs on
        // the main thread (the host's reach, the session's leaf locks, the
        // classifier and the timeline write), paid at every presence refresh of
        // the session, and the worst one.
        pub(crate) need_publish: Histogram,
        pub(crate) need_publish_max: Max,
        // THE TRANSCRIPT APPLY'S HOLD (session log, ruling T31): how long one
        // chunk of a transcript batch holds a session's timeline lock on the
        // footer resolver's thread (`session_log::transcript::apply_batch`), the
        // lock the composer and the sweep take. `tx_chunk` books a full chunk
        // (`CHUNK` events, more to follow), `tx_last` a batch's last hold, which
        // also judges the wall, flushes and takes the wake — a read that found
        // no new rows books that one hold too — and `tx_hold` the worst of
        // either.
        pub(crate) tx_chunk: Histogram,
        pub(crate) tx_last: Histogram,
        pub(crate) tx_hold: Max,
    }
}

window_ledger! {
    /// What a redraw pass measures before and around its present: the audit
    /// counters, frame extraction, the pre-present compose, the drawable and
    /// GPU parks, the drops and the terminal-mutex waits.
    pub(crate) struct RedrawLedger {
        /// Whole-redraw wall time (redraw entry → present done or failed).
        /// `frame_render` excludes surface acquisition and the final present, so
        /// a main-thread `nextDrawable`/compositor stall (~200 ms under GPU
        /// contention) hides from it — this makes that stall measurable.
        pub(crate) total: Peak,
        // REDRAW-AUDIT: attempts are counted at `redraw_window` entry, before any
        // target/hold/unchanged return. A pass that reaches the present seam
        // records its compose cost even when drawable acquisition subsequently
        // fails, closing the old blind spot where `frames=0` could coexist with a
        // 100%-CPU full-grid extraction loop. The counters are intentionally
        // separate rather than inferred from `frames`: aborts can happen between
        // these milestones.
        pub(crate) attempts: Counter,
        pub(crate) early_outs: Counter,
        pub(crate) sync_holds: Counter,
        pub(crate) retry_gated: Counter,
        // The damage-scoped frame-extraction reach (DMG-1): how many
        // presented-path refills rode the scoped arm vs fell back to the full
        // O(rows×cols) walk. A full-arm-dominated steady state means some
        // per-frame scratch mutator broke the continuity chain — the number that
        // says whether the 2.35×/2.65× win is actually reaching this machine's
        // frames.
        pub(crate) refills_scoped: Counter,
        pub(crate) refills_full: Counter,
        // …and the presented frames that took NO extraction at all because the
        // engine had not moved since the snapshot was filled (the effect-only
        // reuse gate). This counter is what keeps the gate honest: `scoped + full
        // + skipped` still accounts for every presented non-rescan frame, so a
        // fall in `scoped` can be read as work AVOIDED rather than work gone
        // missing.
        pub(crate) refills_skipped: Counter,
        // …and WHICH continuity clause refused, per clause. `frame_refills_full`
        // says the chain broke; this says what broke it, which is the difference
        // between an actionable number and a worrying one — a host mutator that
        // forgot its `snapshot_seq` bump, a scratch shared across panes, a window
        // whose row count disagrees with its engine, and a workload that is simply
        // scrolling all read as the same climbing counter and have four different
        // answers (the last one being "nothing, that is correct"). Indexed by
        // `FullRefillCause::index()`; one relaxed `fetch_add` on the arm that was
        // already paying for a full re-extract.
        pub(crate) refill_cause: Tally<{ aterm_core::render::FullRefillCause::COUNT }>,
        // THE RESIDENT PET'S WORLD WALK, the one unconditional per-frame O(window)
        // classification in the effects lane: min(rows,64) x min(cols,256) cells
        // reclassified on every presented frame the rainbow-kitty pet style owns,
        // whether or not anything on that screen moved. Two counters rather than
        // one because the cost is a PRODUCT: `pet_world_observations` is how often
        // the walk ran and `pet_world_cells` is how wide it was, so a change can
        // be read as "fewer walks" or "smaller walks" instead of one number that
        // hides which. Until these existed the walk was measurable only by a
        // criterion bench and by an A/B of two live instances — the two
        // instruments that disagreed by 4x in the 2026-09-24 audit, with no way
        // to ask a RUNNING aterm which was right.
        pub(crate) pet_world_observations: Counter,
        pub(crate) pet_world_cells: Counter,
        /// Cursor-effect ticks whose `glow_under` stream was non-empty: that
        /// stream is the rainbow ribbon body's own (with its jump zooms, and
        /// water's wake), so it reads 0 under the `off` style, whose momentum
        /// glow still lights `glow_add` — the witness a count of ANY quad could
        /// not be. The momentary `fx_*_last` readings are [`Live`].
        pub(crate) fx_under_frames: Counter,
        /// Every effect stream's length in the latest present's final composed
        /// input, whoever produced it, on either renderer ([`FX_STREAMS`]
        /// order): card light shows here, not only the cursor tick's quads.
        pub(crate) fx_streams: [Last; FX_STREAMS.len()] =
            [const { Last::new() }; FX_STREAMS.len()],
        /// Redraw entry → the surface-acquire seam, including terminal grid
        /// extraction; observable even when every actual present is dropped.
        pub(crate) pre_present: Dist,
        pub(crate) pre_present_attempts: Counter,
        pub(crate) pre_present_total: Counter,
        /// Swapchain-acquire (`nextDrawable`) WAIT — pure blocking on the
        /// compositor, not work, per attempt (refused ones included).
        ///
        /// This slice previously had no instrument at all: it fell between
        /// `note_pre_present` (which ends before the acquire) and the renderer's
        /// `last_present_work_ns` (which starts after it), so it was inferable
        /// only as `redraw_total - compose - raster_submit`, contaminated by the
        /// whole post-present tail. That is the single largest known
        /// typing-stall mechanism on macOS — a blocked acquire parks the winit
        /// main thread and queues keyDowns in the OS event queue (measured at up
        /// to ~84 ms) — so the system was blind to its own worst stall.
        ///
        /// A METRIC THAT EXISTS BUT IS NEVER PUBLISHED HIDES A WHOLE CLASS OF
        /// STALL (2026-08 draw-path audit, tier-1 item 3): the last and max were
        /// written on every present and cleared by `reset` from the day the
        /// slice was instrumented, and nothing published them, so the only
        /// acquire figures any reader could get were the percentiles — the shape
        /// this stall does not show up in. One 200 ms park among thousands of
        /// ~0.02 ms samples cannot move a p99; a max cannot miss it.
        pub(crate) acquire_wait: Dist,
        /// Drawable-acquire QUEUE: a drawable request admitted → the
        /// `aterm-drawable-acquire` worker SCHEDULED to start it, a different
        /// cause from `acquire_wait`, which starts its clock inside the worker
        /// around `nextDrawable` alone and is structurally blind to this leg.
        /// That gap is not idle time — a frame that found no prefetched drawable
        /// has already returned `AcquirePending` and its main thread is parked
        /// until the worker posts `GpuSurfaceReady` — so it is keystroke→glass
        /// latency that, until this slice existed, was stamped nowhere.
        ///
        /// It is also the direct readout on the worker's QoS: the thread is
        /// declared `USER_INTERACTIVE` precisely so this distribution stays flat
        /// while the machine is saturated, and a regression that drops it back
        /// to the compilers' band shows up here first. Booked from the same
        /// completion as `acquire_wait`, so `n_acquire_queue` tracks `n_acquire`.
        pub(crate) acquire_queue: Dist,
        /// Armed-Metal GPU park: the UI thread blocked in `waitUntilCompleted`
        /// (see [`note_gpu_park`]).
        pub(crate) gpu_park: Peak,
        /// Surface transactions that failed after a redraw reached the present
        /// seam, and the last one's disposition (a `PresentDropReason` index
        /// and whether the retry scheduler parked it). Evidence of the window:
        /// they clear with their counter.
        pub(crate) drops: Counter,
        pub(crate) drop_reason: LastLabel,
        pub(crate) drop_parked: LastFlag,
        /// UI-thread TERMINAL-MUTEX wait, per acquiring site ([`TermWaitSite`]):
        /// the distribution and the worst.
        pub(crate) term_wait: [Histogram; TermWaitSite::COUNT] =
            [const { Histogram::new() }; TermWaitSite::COUNT],
        pub(crate) term_wait_max: [Max; TermWaitSite::COUNT] =
            [const { Max::new() }; TermWaitSite::COUNT],
    }
}

window_ledger! {
    /// PRESENT → GLASS, fed on a framework thread by the sink installed in
    /// [`install_present_glass_sink`] (lock-free: bucket increments and
    /// `fetch_max`).
    pub(crate) struct GlassLedger {
        /// `presentDrawable:` registration → the drawable's `presentedTime`.
        pub(crate) latency: Dist,
        /// Drawables never shown.
        pub(crate) skipped: Counter,
        /// Per-tag ledgers (`aterm_gpu::present_glass::PresentTag`): a shown
        /// frame is booked under its OWN tag, a skipped one under its
        /// ATTRIBUTION (`GlassReport::skip_attribution` — the lane that replaced
        /// it, or its own occlusion, or `spaced` when nothing replaced it and the
        /// compositor simply dropped it). Together they answer the question the
        /// plain skip counter could not: WHICH pacing lane is paying for pixels
        /// no one sees.
        pub(crate) shown_by: Tally<{ aterm_gpu::present_glass::PresentTag::COUNT }>,
        pub(crate) skipped_by: Tally<{ aterm_gpu::present_glass::PresentTag::COUNT }>,
    }
}

window_ledger! {
    /// The EVENT-LOOP scheduler audit. `about_to_wait` publishes the owner of
    /// the one earliest armed deadline and its process-clock due stamp
    /// ([`Live`]); `new_events` publishes the winit wake kind and attributes
    /// timer lateness back to that owner. A deadline selected already in the
    /// past is counted explicitly — the signature of a self-rearming
    /// `WaitUntil(past)` spin.
    pub(crate) struct SchedLedger {
        pub(crate) event_wakes: Counter,
        pub(crate) timer_wakes: Counter,
        pub(crate) wait_cancelled_wakes: Counter,
        pub(crate) poll_wakes: Counter,
        pub(crate) past_arms: Counter,
        // PER-OWNER ARM ATTRIBUTION (responsiveness audit, item 6). WHY: the two
        // facts above are structurally unable to name a spin's producer.
        // `past_deadline_arms` is ONE global counter, and `deadline_owner` is a
        // LAST-WRITER snapshot of whichever candidate won the final min-fold — so
        // during the 2026-08 200 kHz event-loop spin the pair read "31,913 past
        // arms, owner=title_summary" while the actual producer was the
        // session-status observer folded under that same label (see
        // `DeadlineOwner::SessionStatus`), and the investigation spent its first
        // hour in the wrong module. A counter pair per owner makes the producer
        // NAMED rather than inferred: `arms` counts every deadline that owner won
        // the fold with, `past_arms` the subset already in the past when it was
        // armed. The cost is one extra relaxed `fetch_add` (two on a past arm)
        // per event-loop turn, on a line only this thread writes.
        pub(crate) arms_by_owner: Tally<{ DeadlineOwner::COUNT }>,
        pub(crate) past_arms_by_owner: Tally<{ DeadlineOwner::COUNT }>,
        // …AND THE FIRES, COUNTED APART (responsiveness audit item 2). An arm is a
        // fold WIN per park; a fire is a timer that actually woke the loop for
        // that owner (`EventWakeKind::Timer`). "cursor_effect 59/s" was read as 59
        // wakes a second by three agents during that audit; it was 59 arms, most
        // interrupted by output.
        pub(crate) fires_by_owner: Tally<{ DeadlineOwner::COUNT }>,
        // The deadline healer's two ledgers (`crate::deadline_heal`, which owns
        // the policy and its state): arms the stale-arm floor clamped, and per
        // owner the arms its windowed past-arm streak detector clamped.
        // Observations only: `reset` zeroes them, and nothing here feeds back
        // into what is armed.
        pub(crate) stale_arm_heals: Counter,
        pub(crate) streak_heals_by_owner: Tally<{ DeadlineOwner::COUNT }>,
        // LATENESS THAT SURVIVES THE NEXT WAKE, AND A MAX THAT SAYS WHEN
        // (2026-09-15 attribution audit). The two lateness facts in [`Live`] are
        // LAST-WRITER ONLY: a `Timer` wake stores `now - due` into
        // `last_wake_late` and the very next reader wake — a `WaitCancelled`,
        // which arrives at PTY-output rate — stores 0 over it; `last_deadline_late`
        // is a plain store. So a 400 ms late timer, the exact shape a main thread
        // starved by a compile produces, is erased milliseconds after it happens
        // and is long gone by the time anyone runs `aterm ctl metrics`.
        //
        // THE READING THAT COULD NOT BE ATTRIBUTED. A live line carried
        // `max_present_latency_ms=560.54` beside `wake_late_ms=0.00
        // deadline_late_ms=0.00`, with every slice INSIDE the redraw bounded
        // (`max_redraw_total_ms=13.84`, `max_pre_present_ms=7.32`,
        // `max_acquire_wait_ms=15.32`). Nothing published could say whether those
        // 560 ms were a late wake, a late deadline, or simply an OPEN INTERVAL in
        // which nothing needed to present — the reading `present_latency` actually
        // is (see the module header) — and `max_present_latency_ms` carried no
        // time of occurrence, so it could not even be separated from a spike in a
        // different hour of the same process lifetime.
        //
        // So each lateness now keeps a MAX, the OWNER that produced it, and WHEN
        // it happened (the `now_ns` process clock, published beside
        // `metrics_now_ms` so a reader can say "that was 47 minutes ago"), and the
        // worst present latency keeps its instant AND the GAP since its window's
        // previous present (`PresentLedger::latency_at`/`latency_gap`). That gap
        // is the discriminator the open interval otherwise hides: a gap the size
        // of the latency means the window presented nothing for that long, a gap
        // far smaller means its frames kept coming while this one's output waited.
        // It is the presenting window's own gap (`WindowPresentState`), because
        // another window presenting every 16 ms says nothing about whether this
        // one stalled.
        //
        // NO HISTOGRAM. These are per-wake facts on the hottest loop in the
        // process, and the question they exist to answer — how bad, whose, and
        // when — is one a max answers and a percentile structurally cannot: one
        // 400 ms timer among a million on-time wakes cannot move a p99.
        pub(crate) wake_late: MaxBy,
        pub(crate) deadline_late: MaxBy,
    }
}

window_ledger! {
    /// Synchronized-output holds, the load-shed latch's edges and the output
    /// wake's losses.
    pub(crate) struct SyncLedger {
        // SYNC-1 (DEC-2026 frame-hold) observability. A pathological
        // arm/timeout-release loop pins presents to ~1/timeout (the invisible
        // ~5 fps failure class of 2026-07-05): `releases_timeout` climbing during
        // ordinary typing IS that bug's fingerprint — a healthy interactive shell
        // releases every episode via `?2026l` (`releases_end`) and times out
        // ~never.
        pub(crate) holds_armed: Counter,
        pub(crate) releases_end: Counter,
        pub(crate) releases_timeout: Counter,
        /// Edges of the `perf_reduced` load-shed latch. Shedding engaged during
        /// light typing, or flapping at idle, are both misbehaviour a driver can
        /// assert against.
        pub(crate) shed_transitions: Counter,
        // Lost-wake heals: the watchdog found a `Wake::Output` latch still armed
        // after a user-event drain that began after its post
        // (`spawn::judge_output_wake`), i.e. the event was consumed without a
        // handler pass. ANY non-zero value means a wake was lost and healed —
        // worth investigating even though the user saw only a hiccup (one expiry
        // after the loop turn that saw the arm).
        pub(crate) wake_heals: Counter,
        // Late wakes: a posted `Wake::Output` older than
        // `spawn::WAKE_LATCH_EXPIRY_NS` still waiting for its handler because the
        // main thread's turn (a long frame) ran past it. A measurement of
        // main-thread lateness, never a loss and never acted on — before
        // 2026-09-29 each of these was booked (and healed) as a loss.
        pub(crate) output_wake_late: Counter,
        // Stranded wakes re-posted by a PTY reader: a `Wake::Output` posted after
        // the event loop went idle, with no loop turn since, older than the latch
        // expiry — lost together with its loop wake-up, which no watchdog pass can
        // see (`spawn::wake_stranded`). Not a heal (nothing was proven lost,
        // nothing is repainted). ANY non-zero value means a posted event failed to
        // wake an idle loop for 100 ms — worth investigating.
        pub(crate) output_wake_reposts: Counter,
    }
}

window_ledger! {
    /// THE GPU REPAINT SCOPE (responsiveness audit item 5): the renderer's own
    /// cumulative counters, stored after every GPU present by the main thread,
    /// published since the last `metrics reset` so they are window stats like
    /// the rest of the line.
    pub(crate) struct GpuScopeLedger {
        /// Presents that took the scissored dirty-row path.
        pub(crate) scissor_taken: Baselined,
        pub(crate) full_repaints: Baselined,
        /// Rigid scrolls the band blit rescued (a subset of the scissored ones).
        pub(crate) scroll_rescues: Baselined,
    }
}

window_ledger! {
    /// THE DEVICE'S OWN TIME, AND THE FRAME IT RAN. Every other slice here is
    /// CPU wall time; this ledger holds the GPU's execution of each present
    /// (`aterm_gpu::gpu_time`, booked by [`note_gpu_time`] on a Metal
    /// completion thread or on the UI thread when the wgpu arm harvests its
    /// timestamp readback), the CPU encode beside it, and the shape of the
    /// latest encoded offscreen ([`note_gpu_frame`], after each successful GPU
    /// present, virtual presents included). A supported backend supplies the
    /// device samples; no synthetic zero stands in for a missing clock, so an
    /// empty `time` is unmeasured, never zero time. The shape describes the
    /// latest encoded offscreen, which a present can reuse, and its bytes are
    /// built bytes, not proof of a completed transfer.
    pub(crate) struct GpuLedger {
        /// Device busy time per present: the sum of one frame's completed Metal
        /// command buffers' execution spans, or the union of its wgpu render
        /// passes' timestamp spans.
        pub(crate) time: Dist,
        /// How many parts the latest `time` sample summed.
        pub(crate) time_parts: Last,
        /// CPU encode per GPU present, and its latest sample.
        pub(crate) encode: Histogram,
        pub(crate) encode_last: Last,
        /// Render passes the latest offscreen plan opened.
        pub(crate) passes: Last,
        /// Instances the latest frame built, per stream
        /// (`aterm_gpu::FrameShape::STREAMS` order).
        pub(crate) instances: [Last; aterm_gpu::FrameShape::STREAMS.len()] =
            [const { Last::new() }; aterm_gpu::FrameShape::STREAMS.len()],
        /// Instance bytes the latest frame built for staging.
        pub(crate) instance_bytes: Last,
        /// CPU time building the latest frame's instances, and staging them.
        pub(crate) build: Last,
        pub(crate) stage: Last,
        /// The pass-economy ablation the latest frame ran under (a dev seam),
        /// as its [`ablation_code`]: `off` in every shipped binary, so a
        /// reading that says otherwise names a build measuring itself.
        pub(crate) unorm_ablation: LastLabel,
        /// The fx instances the arm staged in the latest frame, and the fx
        /// bytes it uploaded (instances, palette, atlas texels): both 0 for a
        /// frame whose fx the arm did not stage.
        pub(crate) fx_instances: Last,
        pub(crate) fx_upload_bytes: Last,
        /// The fx sprite items the latest frame refused, drawing nothing: those
        /// the frame art budget refuses, as every backend does, and on a device
        /// whose texture limit holds the fx atlas below 4096² those whose art
        /// the atlas could not hold. 0 on every frame within the budget.
        pub(crate) fx_refused: Last,
        /// aterm-core's fx layer seals and payload deep copies
        /// (`aterm_core::render::fx_layer_builds`, `fx_payload_copies`) since
        /// the reset: process totals that never restart, raised (never
        /// lowered) by [`GpuLedger::sample_fx_totals`] before the reset takes
        /// their base, so a seal before the reset never counts after it.
        pub(crate) fx_layer_builds: Baselined,
        pub(crate) fx_payload_copies: Baselined,
    }
}

window_ledger! {
    /// THE CARD ENGINE'S PER-COMMAND COST (card engine §3.3, §8's fork loop):
    /// what the kernel exec edges cost the PTY readers and the UI thread, read
    /// off a live instance as counts per 1000 commands. Every `*_ns` field is
    /// a running total of nanoseconds, published as milliseconds.
    pub(crate) struct CommandLedger {
        /// Process events the gathers drained (a fork, an exec, an exit each),
        /// and the gathers' time inside those drains.
        pub(crate) proc_events: Counter,
        pub(crate) proc_drain_ns: Counter,
        /// The process events that became foreground edges.
        pub(crate) exec_edges: Counter,
        /// Readers on macOS whose watch could not be made (no kqueue: `EMFILE`),
        /// which run the byte-driven arm.
        pub(crate) procwatch_fallbacks: Counter,
        /// Children past a watch's cap, seen only by sampling.
        pub(crate) procwatch_overflow: Counter,
        /// The engine fence's cuts (`Terminal::note_holder_cut`).
        pub(crate) holder_cuts: Counter,
        /// Foreground handbacks run.
        pub(crate) fg_handbacks: Counter,
        /// The parse stages' slice holds, those that found the term lock taken,
        /// and how long those waited.
        pub(crate) parse_holds: Counter,
        pub(crate) parse_lock_waits: Counter,
        pub(crate) parse_lock_wait_ns: Counter,
        /// The UI thread's status sweeps that visit sessions (the deadline
        /// gate's O(1) no-op is none), and their time.
        pub(crate) status_sweeps: Counter,
        pub(crate) status_sweep_ns: Counter,
    }
}

/// State that describes the process NOW rather than the measurement window: a
/// gauge, a latch, an episode still in progress, a last-writer reading. It has
/// no reset, so `metrics reset` cannot pretend a live capture ended (the rest
/// of it would land in the clean histogram) or that the backend changed.
pub(crate) struct Live {
    /// The live renderer is the GPU (Metal) path.
    pub(crate) backend_gpu: AtomicBool,
    /// The `perf_reduced` load-shed latch (the `MotionPolicy` fold).
    pub(crate) perf_reduced: AtomicBool,
    /// Windows holding a synchronized-output frame ([`SyncHoldingGauge`]).
    pub(crate) sync_holding_windows: AtomicU64,
    /// Captures live RIGHT NOW (`capture_active` is `!= 0`), nesting-safe.
    pub(crate) capture_depth: AtomicU64,
    /// Until when (`now_ns`) an ended occlusion/park/capture episode still
    /// taints output→present samples ([`PRESENT_TAINT_TAIL_NS`]).
    pub(crate) taint_until: AtomicU64,
    // THE COMPOSE-SIDE PRIM COUNTERS (docs/RELEASE-PROOF-DISCIPLINE.md, "still
    // owed" item 6): what the cursor-effect tick handed the frame, read off a
    // live instance without a pixel capture. Every tick — the live single-pane
    // and composed presents and the explicit capture — goes through
    // `App::cursor_frame`, so these see all three. The three readings are
    // momentary, like every `last_*`: the additive `glow_add` quads (every
    // style's halo, the momentum glow on ANY style, the rod, the bodies), the
    // `glow_under` quads, and the comet's trail cells.
    pub(crate) fx_glow_quads_last: AtomicU64,
    pub(crate) fx_under_quads_last: AtomicU64,
    pub(crate) fx_trail_cells_last: AtomicU64,
    /// Winit's reason for the current loop iteration, the owner a timer wake is
    /// attributed to, and its lateness: last-writer readings, erased by the
    /// next wake ([`SchedLedger::wake_late`] keeps the worst).
    pub(crate) last_wake_kind: AtomicU64,
    pub(crate) last_wake_owner: AtomicU64,
    pub(crate) last_wake_late: AtomicU64,
    /// The selected deadline's owner, its process-clock due stamp (0 = pure
    /// `Wait`) and its lateness when armed past.
    pub(crate) last_deadline_owner: AtomicU64,
    pub(crate) last_deadline_due: AtomicU64,
    pub(crate) last_deadline_late: AtomicU64,
    /// The pending keystroke's TRUE arrival (`now_ns`, backdated by its OS
    /// queue wait; 0 = none): see [`note_key_arrival_queued`].
    pub(crate) lat_key: AtomicU64,
    /// THE UPDATE'S PRE-COMMIT INPUT, AS NUMBERS (gap #33). Input typed into a
    /// successor's revealed window waits in a bounded queue until Commit; every
    /// event that queue REFUSED is a lost keystroke, counted here as it happens.
    /// Cumulative for the process: they describe the one handoff it adopted, at
    /// its birth, and a `metrics reset` taken afterwards must not erase a lost
    /// keystroke.
    pub(crate) handoff_input_dropped: AtomicU64,
    /// …and every replay that had to DISARM the ambient modifiers because the
    /// queue could not be replayed as a coherent key stream: a drop broke a
    /// press/release pair, or a window lost focus with input already queued for
    /// it. Before gap #33 this fired on every handoff (the owner's log: 17 of
    /// 17), because winit's own startup `Focused(false)` for each new window
    /// marked it; a non-zero reading now means a replay was really repaired.
    pub(crate) handoff_input_incoherent: AtomicU64,
    /// How many live sessions hold a PTY of their own: one per [`PtyCensus`]
    /// alive, i.e. per `Session` a real spawn or adoption built and nothing has
    /// dropped yet (a test stub, whose sink borrows or has no master, holds
    /// none).
    pub(crate) pty_sessions: AtomicUsize,
    /// How many warm spares hold a PTY ([`PtyCensus::spare`]): their masters
    /// are left out of `pty_masters`, so masters equal sessions at rest.
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

/// How long after an episode ends a present-latency sample is still suspect.
///
/// Stamps are per session and several can be armed while the window was off glass; the
/// drop→resume seam is not instantaneous and a park can re-arm between them. One second
/// bounds that drain, stays far below any stall, and (unlike a latch consumed by the next
/// present) cannot be defeated by a blink repaint arriving first. Excluded samples count
/// in `present_tainted`.
const PRESENT_TAINT_TAIL_NS: u64 = 1_000_000_000;

/// A pending input slice older than this is a no-echo session (password
/// prompt, ignoring app), not a pacing measurement: discard, don't record.
const INPUT_SLICE_CAP_NS: u64 = 5_000_000_000;

/// A pending resize slice older than this is a window that stopped presenting
/// (occluded / parked surface), not a resize stall: discard, don't record.
const RESIZE_SLICE_CAP_NS: u64 = 2_000_000_000;

/// Monotonic nanoseconds since the process clock's anchor: the GUI entry
/// ([`mark_process_start`]), or the first read when it came first
/// ([`startup::Startup::clock_anchor`]). The process-lifetime clock of the
/// input→present stamps; saturates far beyond any session length.
///
/// Crate-visible because the main-loop turn census
/// (`crate::watchdog::turn_census`) prices its spans and stamps its
/// `max_turn_at_ms` on THIS clock: an `_at_ms` on a different clock could not be
/// read against `metrics_now_ms`, which is the one subtraction that turns a
/// stamp into "how long ago".
pub(crate) fn now_ns() -> u64 {
    let start = startup::STARTUP.clock_anchor();
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// How many frames this process has presented (the `metrics` verb's
/// `frames`, zeroed by `metrics reset`): the present ordinal the card
/// engine's dev timeline rows carry (`crate::holder_image::dev_tail`).
pub(crate) fn frames_presented() -> u64 {
    METRICS.window.present.frames.get_acquire()
}

/// The `Instant` a [`now_us`] stamp was read at — this clock's exact inverse,
/// floored to the microsecond the stamp kept. The clock is the anchor's
/// `elapsed()`, so every stamp is an offset from ONE fixed anchor and needs no
/// second clock read to place: the answer is a pure function of the stamp,
/// ordered exactly as the stamps are and identical on every call. `None` only
/// past `Instant`'s range.
pub(crate) fn instant_at_us(stamp_us: u64) -> Option<Instant> {
    startup::STARTUP
        .clock_anchor()
        .checked_add(Duration::from_micros(stamp_us))
}

/// The [`now_us`] stamp of `at`, a reading taken earlier: [`instant_at_us`]'s
/// forward, floored to the microsecond as [`now_us`] floors. An instant before
/// the clock's anchor is stamp 0.
pub(crate) fn us_at(at: Instant) -> u64 {
    let start = startup::STARTUP.clock_anchor();
    u64::try_from(at.saturating_duration_since(start).as_micros()).unwrap_or(u64::MAX)
}

label_enum! {
    /// The event loop's single earliest `WaitUntil` owner. An atomic holds an
    /// owner as its index, and the wire carries only its label, so an owner is
    /// added or retired anywhere in the list. The per-owner ledgers print in
    /// this order.
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
        /// The program cat's tenure gate (`aterm_effects::companion_look::KittyTenure`): one wake at
        /// the instant a pending claim earns or releases the cursor.
        KittyTenure => "kitty_tenure",
        /// The message band (`crate::messages_host::App::messages_deadline`): ONE
        /// wake at the nearest fold, staleness cap, overflow patience or D1 shrink
        /// instant the center has armed. A live row folds nothing (its paints ride
        /// its reporter's wakes), and an idle band arms nothing — an idle window
        /// never wakes for it (FL-1).
        MessageBand => "message_band",
        /// The per-session STATUS observer (`SessionStatus::next_wake`): a candidate
        /// serving its dwell, or a Running session aging into Quiet. Its own label
        /// (item 8): it once shared the retired `title_summary` owner's, because the
        /// two folded at the same seam in `about_to_wait`, so the 2026-08 event-loop
        /// spin, whose producer was THIS observer, was reported as `title_summary`
        /// and cost the investigation its first hour on the wrong module.
        SessionStatus => "session_status",
        /// The macOS access card's watch (`consent_card::CardState::next_wake`):
        /// the probe cadence while the owner may be in System Settings, the
        /// decide timeout while a verdict is awaited.
        ConsentCard => "consent_card",
        /// PRESENCE (`crate::presence`, `drive::deadline`): a visible background
        /// pill's age figure (once a second while it prints seconds, once a
        /// minute after), a cooperative lease's or a supervisor claim's lapse,
        /// and an owed notification — armed only while one stands. A quiet
        /// desktop folds nothing here (`presence_fp == 0`).
        Presence => "presence",
        /// The late park's gate re-run (`App::try_park_for_prelaunched_successor`):
        /// a prelaunched update attempt waiting for a quiet moment to park.
        HandoffPark => "handoff_park",
        /// The MESSAGE BAND's MOTION (design §10.8): the frames of a live row's
        /// indicator — a sweeping comet, a gliding fill, a travelling glint, an
        /// echo — at most one per 33 ms frame (the 30 fps cap), plus the text ticks
        /// of an elapsed time or an ETA. Armed ONLY while a live row or an echo is
        /// on an on-screen window's glass; a resting comet or glint arms its next
        /// start, not a frame. The band's STATE wakes (a fold, a reveal, an echo's
        /// end) stay on `MessageBand`. Since the merge with main's full-width
        /// meter (ruling 140) this is the band's ONLY motion clock: main's 125 ms
        /// busy frame, which folded under `MessageBand`, retired into it — the
        /// busy comet steps on this grid — and nothing is armed while a handoff
        /// freezes the band.
        MessageBandMotion => "message_band_motion",
        /// A SESSION'S WAITS (design rulings 231–234): the next sample of a large
        /// paste on its way to a program or of a scrollback rewrap the person
        /// waits on — every 250 ms while its row is up, every second while none
        /// is — and nothing while nothing is watched (`crate::session_waits`).
        SessionWaits => "session_waits",
        /// The INPUT WATCH (`crate::input_stall::InputWatches`, 2026-09-24): one
        /// wake at the instant a session's oldest unread input byte turns a stall
        /// (`STALL_AFTER` old), then every `RECHECK` while a stall, a stopped job
        /// or canonical type-ahead stands. Armed only while some session HAS
        /// unread input — the sink's input hook starts a watch, a clear reading
        /// ends it — so an idle machine folds nothing here.
        InputWatch => "input_watch",
        /// THE CRASH JOURNAL (`crate::crash_journal::Lane::next_wake`, PTY keeper
        /// P1): the trailing edge of a capture a change inside the write interval
        /// owes, and the end of a reopened layout's probation, when the image still
        /// carries its mark. An idle window owes neither.
        CrashJournal => "crash_journal",
        /// The update checker's watchdog (`crate::update_checker_watch`, plan P2-1):
        /// ONE wake at the instant the background check loop's heartbeat would go
        /// stale — twice its phase's budget past its last stamp. A healthy loop
        /// re-stamps long before it, so this wakes an idle window about once an hour
        /// to find it fresh; a window with no checker (headless, automatic checks
        /// off, an uninstalled copy) never arms it.
        UpdateChecker => "update_checker",
        /// THE SPLIT'S TAKE-BACK HOLD (`crate::split_regrid_hold`): one wake at
        /// the end of the quiet that withheld a collapse's grow of a pane back to
        /// its pre-split grid. Its own owner, not `ResizeSettle`, so the settle's
        /// same-owner heal and its attribution never mix the two.
        SplitRegridHold => "split_regrid_hold",
        /// KITTY ANIMATION PLAYBACK (`crate::kitty_animation`): one wake at the
        /// soonest frame a shown session's Kitty graphics animation has due. A
        /// still image, a stopped animation or a session no window shows arms
        /// nothing.
        KittyAnimation => "kitty_animation",
        /// A RENDER EPOCH's look (`crate::resize_epoch`, 2026-09-29): ONE wake at
        /// the instant a session's open epoch (rows the engine owes its program)
        /// has waited out the risk window since its last resize, or its short retry
        /// when the terminal was locked. The ledger books every resize at one funnel
        /// and registers the look there; an idle machine, and a session whose
        /// screen is not displaced, arm nothing.
        RenderEpoch => "render_epoch",
        /// The owner's debugger-access change (`debugger_access`, design
        /// `docs/DESIGN-macos-debugger-access-2026-09-29.md` §3.4 step 4): the
        /// park's `RESULT_RECHECK` re-read while the lane `wants_poll`: the
        /// worker's shared result slot while a change is under way, and the
        /// shared records while a read is out or due again (an unsettled read, a
        /// drift re-read). An idle lane arms nothing.
        DebuggerChange => "debugger_change",
        /// The Developer Tools hand-over (`crate::devtools_helper`, owner ask
        /// 2026-10-15): a waiting hand-over's next look for its quiet moment, every
        /// `RECHECK_EVERY` and only while an aterm window is focused; the helper
        /// panel's turn every `PANEL_TICK` while it is up (placement, System
        /// Settings still running, one list read); and a worker's slot re-read
        /// while one is out. The folder warm-up's launch look and the once-per-Mac
        /// debugger-access launch ask's next look (owner ruling O29) fold under it
        /// too, each only while an aterm window is focused. Nothing waiting and no
        /// panel arm nothing.
        DevToolsHandOver => "devtools_hand_over",
        /// THE HARNESS CARD (`crate::claude_card`, design
        /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §3): a frame ahead while
        /// a card on show plays its episode, then one wake for the settled frame
        /// it owes. A settled card, and a window off screen, arm nothing.
        ClaudeCard => "claude_card",
        /// A transient theme preview's TTL (`theme preview … ttl=`,
        /// `crate::control_theme`): one wake at the instant the preview reverts to
        /// the configured theme by itself. No preview arms nothing.
        ThemePreview => "theme_preview",
        /// THE SESSION LOG's settle tick (`crate::session_log::Wakes`): one wake at
        /// the instant a session's turn tracker has waited out the settle on a
        /// resting word (`aterm_slog::SETTLE_MS` after it first held), so the turn
        /// closes with no screen change to say so. Armed only while some tracker
        /// has a settle pending; an idle machine folds nothing here.
        SessionLog => "session_log",
        /// THE HARNESS'S GLASS (`crate::harness_glass`, design
        /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.2.3): one wake at the
        /// end of an act's label on an Autopilot chip, or at a re-read of WHAT
        /// THE HARNESS DID that Settings ▸ Harness owes. No label up and no read
        /// owed arms nothing.
        HarnessActs => "harness_acts",
        /// AUTOPILOT'S TIMER (`crate::autopilot_timers`, design §9.3.6): one wake
        /// at the earliest instant a timed Cruise lands, or at the minute an open
        /// Autopilot menu's or Settings ▸ Harness page's `land_at` offer turns
        /// its words (§9.3.3). No timer armed and no offer open arms nothing.
        AutopilotTimer => "autopilot_timer",
        /// THE tccd OBSERVER's threshold crossing (`App::poll_consent_observer`,
        /// design §3.6): one wake at the instant an open request a running Full
        /// stream observed becomes a pending prompt, so its banner does not wait
        /// for an unrelated wake. No Full stream, or nothing on its way to the
        /// threshold, arms nothing.
        ConsentObserver => "consent_observer",
        /// THE WARM SPARE's tick (`crate::spare`): a landing's settle, a booking's
        /// boot grace or replacement spacing, and the slot's own deadlines (the
        /// parked mark's second, the warm run's `T_READY`, the ages). A parked or
        /// ready spare on an idle machine arms only its age.
        Spare => "spare",
        /// AN OUTPUT HOLD's release (`App::release_output_holds`,
        /// `spawn::OUTPUT_CADENCE_NS`): one wake at the instant a streaming
        /// session's hold falls due, to run the pass for what its reader fed under
        /// it. Armed only while output streams; a quiet session holds nothing.
        OutputCadence => "output_cadence",
        /// THE STATS DECK's next reading (`crate::vitals_host`): one a second while
        /// a focused window shows the Deck, one every five seconds in the
        /// background; with no window showing it, the jobs engine's own (2 s or
        /// 30 s) while a suspect agent call stands, else none (the disk watch's
        /// minute look asks for one by a wake, not a deadline).
        SystemVitals => "system_vitals",
        /// THE STATS LENS (`crate::stats_lens`): one wake when a pointer at rest
        /// on a Deck or sill instrument has rested 250 ms (re-armed at each
        /// motion, so passing over arms and re-arms but never opens), and one
        /// when an unpinned hover lens's pointer has been gone 300 ms. No rest
        /// and no hover lens arm nothing.
        StatsLens => "stats_lens",
    }
}

impl DeadlineOwner {
    /// The owner an atomic holds as its [`index`](Self::index); a value no
    /// owner has reads as `None`.
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
    /// The kind an atomic holds as its [`index`](Self::index); a value no kind
    /// has reads as `None`.
    fn from_raw(raw: u64) -> Self {
        usize::try_from(raw)
            .ok()
            .and_then(Self::from_index)
            .unwrap_or(Self::None)
    }
}

label_enum! {
    /// Typed failure labels from the application-present seam. Besides making the
    /// metric actionable, the retry scheduler (`crate::autonomous_retry`) parks
    /// conditions that should wait for an external surface/window stimulus.
    pub(crate) enum PresentDropReason {
        None => "none",
        GpuReconfigured => "gpu_reconfigured",
        GpuTimeout => "gpu_timeout",
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
    /// The reason an atomic holds as its [`index`](Self::index); a value no
    /// reason has reads as `None`.
    fn from_raw(raw: u64) -> Self {
        usize::try_from(raw)
            .ok()
            .and_then(Self::from_index)
            .unwrap_or(Self::None)
    }
}

impl Metrics {
    /// Arm the suspect window: presents for the next [`PRESENT_TAINT_TAIL_NS`]
    /// are booked to the tainted ledger. `fetch_max` so a later episode can only
    /// extend it, never cut a still-open one short.
    fn arm_present_taint(&self) {
        self.live.taint_until.fetch_max(
            now_ns().saturating_add(PRESENT_TAINT_TAIL_NS),
            Ordering::Relaxed,
        );
    }

    /// [`note_capture_episode`] on this root.
    fn note_capture_episode(&self, active: bool) {
        if active {
            self.live.capture_depth.fetch_add(1, Ordering::Relaxed);
            self.window.present.capture_episodes.inc();
        } else {
            // Saturating: an unpaired close (a recording finalized twice) must
            // not wrap the depth to u64::MAX and taint the ledger for the rest
            // of the process.
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

/// The taint decision, PURE, so the policy has a race-free test that touches no
/// process-global state (the `wake_owner` precedent). A present a capture
/// forced, or one inside a live capture, taints unconditionally; otherwise the
/// sample is suspect until the tail expires.
const fn tainted_at(
    capture_forced: bool,
    now_ns: u64,
    taint_until_ns: u64,
    capture_depth: u64,
) -> bool {
    capture_forced || capture_depth != 0 || now_ns < taint_until_ns
}

/// WHICH present drops make the following output→present samples meaningless.
///
/// PURE and deliberately NARROW. Occluded glass, or a drop the retry scheduler
/// parked awaiting an external surface stimulus, means frames stopped reaching
/// the screen for an interval nobody was watching — output kept stamping and
/// the resuming present books the whole unwatched gap. A transient
/// `CpuAcquire`/`GpuTimeout`/`GpuReconfigured` that retries a millisecond later
/// is a stall the user genuinely FELT: tainting those would delete exactly the
/// samples this histogram exists to catch.
const fn drop_taints_present_latency(reason: PresentDropReason, parked: bool) -> bool {
    parked || matches!(reason, PresentDropReason::GpuOccluded)
}

/// A capture episode (`video` recording, paced or offscreen) opened or closed.
///
/// THE OBSERVER RULE IN CODE: a recording paces presents on its own schedule
/// and pins gates the unrecorded path does not have, so every output→present
/// sample taken while it runs describes the INSTRUMENT, not the terminal. Marking
/// the episode is what lets `present_p95` stay a statement about aterm while the
/// recorded numbers remain readable in their own ledger. Nesting-safe (depth,
/// not a bool) so overlapping captures cannot leave the flag stuck either way,
/// and the close arms the tail because presents already queued by the pacer land
/// after it.
pub(crate) fn note_capture_episode(active: bool) {
    METRICS.note_capture_episode(active);
}

label_enum! {
    /// The UI-thread sites that BLOCK on a session's terminal mutex on the hot
    /// paths — the single-pane redraw's one hold per frame and the key press's
    /// consolidated scope. The seam's own acquisition is gone (it reads the
    /// lock-free `ModeMirror`), so these two are the whole set.
    ///
    /// Each acquisition books its wait here so the reader's slice-boundary handoff
    /// (`yield_to_ui_waiter`) is MEASURABLE: the `term_wait_*` percentiles under
    /// `metrics percentiles` must sit at or under one reader slice while output
    /// floods, where the unfair-mutex behaviour they were added to expose put them
    /// at hundreds of microseconds.
    ///
    /// The label is the `metrics` field stem, and `ALL` is the report order.
    pub(crate) enum TermWaitSite {
        /// The single-pane `redraw_window`'s ONE terminal hold per frame.
        Redraw => "redraw",
        /// The key press's one consolidated scope in `input_to_session`.
        Press => "press",
    }
}

/// Record one UI-thread terminal-mutex wait at `site` (cheap: one bucket
/// increment + a `fetch_max`). Uncontended acquisitions book `0` so `n_*`
/// counts every acquisition and the percentiles are over the true population.
pub(crate) fn note_term_wait(site: TermWaitSite, ns: u64) {
    METRICS.window.redraw.note_term_wait(site, ns);
}

impl RedrawLedger {
    /// [`note_term_wait`] into this ledger.
    fn note_term_wait(&self, site: TermWaitSite, ns: u64) {
        self.term_wait[site.index()].record(ns);
        let _ = self.term_wait_max[site.index()].record(ns);
    }
}

/// Record one swapchain-acquire attempt's wait, including refused acquisitions.
/// Cheap (one histogram bucket increment); the shared GPU present seam consumes
/// each sample once, before either success or failure routing.
pub(crate) fn note_acquire_wait(ns: u64) {
    let _ = METRICS.window.redraw.acquire_wait.record(ns);
}

/// Record one completed acquisition's queue span. Booked from the same
/// completion as [`note_acquire_wait`], so the two legs of one acquisition are
/// always readable together and `n_acquire_queue` tracks `n_acquire`.
pub(crate) fn note_acquire_queue(ns: u64) {
    let _ = METRICS.window.redraw.acquire_queue.record(ns);
}

/// Record one armed present's GPU PARK: the wall time its UI thread spent
/// blocked in `waitUntilCompleted` waiting for the GPU to finish EARLIER work —
/// the Submit-A pipelining wait before the frame's shared buffers may be
/// rewritten, plus the pending-ring drain past depth 3. Neither wait has a
/// timeout.
///
/// This exists because the frame-slot half used to be charged to
/// `frame_render`: it runs inside the renderer's present work timer, so a GPU
/// or WindowServer contention stall arrived at `record_present` as CPU render
/// cost, counted against `slow_frames`, and fed the load-shed latch — while the
/// ring drain, which runs before that timer starts, was invisible except as
/// `redraw_total` slack. Same rule as the acquire park: a wait on the GPU is
/// not compose work, so it is excluded from the causal cost and gets its own
/// published last/max. The max is the point — one long park among thousands of
/// zero ones is the stall, and only a max can see it.
pub(crate) fn note_gpu_park(ns: u64) {
    let _ = METRICS.window.redraw.gpu_park.record(ns);
}

/// A frame whose causal compose+raster/encode-submit CPU work exceeds a ~30 fps
/// (33.3 ms)
/// budget — the floor below which interaction visibly stutters. `slow_frames` counts
/// these so a driver can DETECT sustained lag rather than read a momentary value.
pub(crate) const SLOW_FRAME_THRESHOLD_NS: u64 = 33_333_333; // 1/30 s

/// One window's present-side state, kept in its `WindowState` (UI thread only,
/// so plain integers): the keys typed into it that still wait on its content
/// present, the keys a native page in it took that still wait on the present
/// of the page's answer, and when it last presented. [`reset`] never writes
/// it; the next present reads the root's epoch instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowPresentState {
    pub(crate) pending_input: PendingInputStamp,
    /// The native ring: closed only by a present of the native revision its
    /// keys produced ([`record_native_content_present`]), never by terminal
    /// output, so a native repaint never closes a terminal key early.
    pub(crate) pending_native_input: PendingNativeInputStamp,
    /// This window's previous successful present (`now_ns`; 0 = none yet): the
    /// gap the worst present latency is witnessed against, and the slow-present
    /// line's `prev present`.
    last_present_ns: u64,
    /// The frame being drawn painted the predictor's overlay whole, under its
    /// proof, through this op ([`note_prediction_shown`]); taken by its present.
    shown_through: Option<u64>,
}

/// The present-side state that belongs to the whole process, owned by the
/// `App` (UI thread): the open slow-present episode, and the previous present
/// of any window. [`reset`] runs on a control thread and never writes it, so a
/// reset cannot stitch two episodes into one line; the next present reads
/// the root's epoch and retires what was stamped before it.
#[derive(Debug, Default)]
pub(crate) struct PresentObserver {
    /// The open slow-present episode, so a run of slow presents costs two
    /// `aterm.log` lines instead of one per frame.
    episode: SlowEpisode,
    /// The previous successful present of any window (`now_ns`; 0 = none yet):
    /// what `max_frame_gap_ms` is measured against.
    last_present_ns: u64,
}

/// One present's clock, read once: the instant, the reset epoch, and the one
/// taint decision made at that instant. The output ledger, the input slices
/// and both gaps all read these values, so they cannot
/// disagree about a frame.
#[derive(Clone, Copy, Debug)]
struct PresentClock {
    now: u64,
    epoch: u64,
    /// A capture's barrier forced this present (`App::present_before_capture`):
    /// its output→present sample is a `capture_presents` sample.
    capture_forced: bool,
    /// Output→present latency cannot be attributed to the terminal: a capture
    /// forced this present, a capture is live, or an occlusion/park episode
    /// ended within the tail.
    tainted: bool,
}

impl PresentClock {
    /// The clock of a present booked into `m`: now, its epoch, and its taint.
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

    /// The time since the present stamped `previous`, or `None` when there was
    /// none in this measurement window (no present yet, or the last one came
    /// before the reset epoch).
    const fn gap_since(self, previous: u64) -> Option<u64> {
        if previous == 0 || previous < self.epoch {
            None
        } else {
            Some(self.now.saturating_sub(previous))
        }
    }
}

/// What one present measured, as [`record_present_at`] booked it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PresentBooking {
    /// Where the output→present slice went: `Some(true)` to the tainted
    /// ledger, `Some(false)` on glass, `None` when the present closed none (a
    /// latency of 0).
    output_tainted: Option<bool>,
    /// Since the presenting window's previous present: the worst-latency
    /// witness and the slow-present line's `prev present`.
    window_gap_ns: Option<u64>,
    /// Since the previous present of any window: `max_frame_gap_ms`.
    frame_gap_ns: Option<u64>,
    /// The presenting window's pending keys booked into the input ledger: the
    /// in-cap edges armed at or after the reset epoch. The session-routed
    /// stamp is not counted.
    input_booked: u32,
    /// Of `input_booked`, the keys a proven prediction this present painted
    /// showed before their echo ([`note_prediction_shown`]).
    input_predicted: u32,
}

/// Record one successful application present. `latency_ns` is the
/// `output→application-present-return` delay for this frame, or `0` when no
/// output burst was pending (a blink/selection/resize repaint) — a `0` leaves
/// the last real measurement in place and is NOT a slow-frame input.
/// `render_ns` is this frame's causal CPU wall time: compose plus CPU raster/copy,
/// or time spent encoding GPU commands and calling `queue.submit`. It is not
/// completed GPU execution and excludes surface acquisition and final-present
/// pacing.
///
/// `window` is the PRESENTING window's state (`None` when it is gone). Only its
/// pending keys — plus the session-routed global stamp — can be closed by this
/// present; another window's pending key is untouched. `observer` is the App's
/// process-wide present state.
///
/// `capture_forced`: a capture's barrier forced this present
/// (`App::present_before_capture`), so its output→present sample says when the
/// caller asked, not how the terminal paces, and books to the tainted ledger.
///
/// The process's first present also publishes the startup sample
/// ([`startup::Startup::publish_first_present`]) before the frame count
/// becomes observable.
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
    // The test twin counts this thread's samples of each kind, which a
    // parallel test's presents cannot move.
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
    /// [`record_present`] into this root at a given clock, so a test passes
    /// the instant, the epoch and the taint decision instead of racing the
    /// process's.
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
        // A reset since the open episode's last slow present ended its window:
        // close it, with its line, before this frame can join it.
        observer.honour(clock.epoch);
        // Close the RESIZE→PRESENT slice on ANY successful present, not only a
        // content one: what ends the compositor's rescale is a frame at the new
        // size reaching the WSI, whatever drew it. (The input slice below is
        // deliberately content-gated instead — a blink repaint is not an echo
        // — but for the keys a frame shows a proven prediction of, below.)
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
            // HONESTY SPLIT (item 10): an output→present slice measured while the
            // window was occluded/parked, or while a capture was pacing or
            // forcing presents, describes the episode and not the terminal. Book
            // it — visibly — to the tainted ledger instead of the on-glass one.
            // See `PresentLedger::tainted`.
            if clock.capture_forced {
                present.capture_presents.inc();
            }
            if tainted {
                present.tainted_samples.inc();
                let _ = present.tainted.record(latency_ns);
            } else {
                // WHEN, AND AGAINST WHAT. The max alone cannot be told apart from a
                // spike in another hour of the same process, and the reading is an
                // OPEN interval, so the gap since this window's previous present
                // is captured with it: gap ≈ latency means the window presented
                // nothing for that long; gap ≪ latency means its frames kept
                // coming and this frame's output waited. Single-writer (the UI
                // thread), so "the `fetch_max` that won also writes its stamps"
                // needs no CAS.
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
            // A CONTENT present: close THIS window's pending
            // input→application-present-return slices, if any. A latency of 0
            // (blink/selection repaint) leaves them pending — no attributed
            // content present has completed yet.
            //
            // DELIBERATELY NOT taint-gated. The edges must be CONSUMED either way
            // (leaving them armed only ages them into the `INPUT_SLICE_CAP_NS`
            // discard), and input→present already carries its own honesty bound: a
            // keystroke typed into an occluded window is a real keystroke that
            // really waited. Only the OUTPUT slice above measures an interval
            // nobody asked for.
            //
            // EVERY booked edge is a sample (coordinated omission corrected).
            if let Some(w) = window.as_deref_mut() {
                w.pending_input.drain_into(now, clock.epoch, |edge| {
                    if edge.before_epoch {
                        return;
                    }
                    self.window.input.book_present(edge.slice_ns, now);
                    booking.input_booked += 1;
                });
            }
            // The session-routed stamp (no window known at arm time): any content
            // present closes it, as before.
            if let Some(stamp) = self.window.input.session_stamp.take() {
                let d = now.saturating_sub(stamp);
                // A slice past the cap means the keystroke never echoed (see the
                // HONESTY BOUNDS on `InputLedger::input`) — recording it would peg
                // the max with a latency that never happened.
                if d <= INPUT_SLICE_CAP_NS {
                    self.window.input.book_present(d, now);
                }
            }
        }
        // A present with no output that painted the predictor's overlay whole,
        // under its proof ([`note_prediction_shown`]): the keys whose prediction
        // it shows are on glass, so their slices close here, as a native page's
        // answer closes its keys', and each is counted in `predicted`. A content
        // present above drained every key already; either way the mark is this
        // frame's alone.
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
        // Present→present gap of any window, booked only against a previous present
        // in this measurement window, so the first post-reset present never books a
        // spurious gap against pre-reset (or cold-start) history.
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

/// An `output→application-present-return` reading big enough to be what a user
/// calls lag: THREE slow-frame budgets (100 ms). Well above the ~33 ms budget a
/// single heavy frame can legitimately spend, so an ordinary slow frame never
/// writes a line.
const SLOW_PRESENT_LOG_THRESHOLD_NS: u64 = SLOW_FRAME_THRESHOLD_NS * 3;

/// Quiet time that separates two slow-present EPISODES. A stall is a RUN of
/// slow presents: a line per frame would bury `aterm.log` and spend main-thread
/// time inside the very stall it describes, and a line every few seconds still
/// wrote 9,908 of the file's 11,587 lines during one 27-hour run (2026-09-18).
/// So a slow present after at least this long without one opens an episode
/// (one line), and one that has had none for this long is closed (one line).
const SLOW_PRESENT_EPISODE_QUIET_NS: u64 = 60_000_000_000;

/// One run of slow presents, as `aterm.log` reports it. `samples == 0` means
/// none is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SlowEpisode {
    start_ns: u64,
    last_ns: u64,
    samples: u64,
    worst_ns: u64,
    worst_at_ns: u64,
}

/// What one present-latency sample did to the episode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlowEpisodeStep {
    /// Not a slow present, or one whose delay belongs to the session-status
    /// observer: no line.
    Ignored,
    /// It opened an episode — first closing `closed`, when one was still open
    /// but had gone quiet: its lines are written.
    Opened { closed: Option<SlowEpisode> },
    /// It joined the open episode: no line.
    Joined,
}

/// Fold one sample into `episode`. Pure, so the threshold, the quiet gap and
/// the session-status rule are testable without the process globals or a real
/// stall — the same rule [`wake_owner`] is kept pure for.
///
/// A sample whose last deadline owner is the session-status observer is
/// skipped: it owned 9,842 of the 9,908 breadcrumbs of the 2026-09-18 run, and
/// those readings timed that observer's wait on output nobody was watching, not
/// lag in front of a user. Output that was not on screen never gets here at
/// all: a sample taken while the window was occluded or parked goes to the
/// tainted ledger in [`record_present`], and `present_latency_ns_with_plan`
/// only times panes on this window's glass.
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

/// Close `episode` when it has had no slow present for
/// [`SLOW_PRESENT_EPISODE_QUIET_NS`] by `now_ns` (or unconditionally when
/// `force`), answering the episode to report.
fn slow_episode_close(episode: &mut SlowEpisode, now_ns: u64, force: bool) -> Option<SlowEpisode> {
    if episode.samples == 0
        || !(force || now_ns.saturating_sub(episode.last_ns) >= SLOW_PRESENT_EPISODE_QUIET_NS)
    {
        return None;
    }
    Some(std::mem::take(episode))
}

/// A duration for a log line: seconds under two minutes, then minutes, then
/// hours.
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

/// The closing line of an episode. A one-sample episode has none: its opening
/// line already said everything.
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
    /// Close an episode whose last slow present came before the reset `epoch`
    /// (a `metrics reset` ended its window), writing its closing line, so a
    /// slow present after the reset opens a fresh episode instead of joining
    /// it and the log never shows a run that simply stops.
    fn honour(&mut self, epoch: u64) {
        if self.episode.last_ns < epoch
            && let Some(closed) = slow_episode_close(&mut self.episode, 0, true)
        {
            log_slow_episode_end(closed);
        }
    }

    /// ONE UNGATED LINE WHEN A SLOW-PRESENT EPISODE OPENS, AND ONE WHEN IT
    /// ENDS, with the scheduler facts that name it, read from `m`. `owner` is
    /// the last deadline owner, which [`slow_episode_step`] reads.
    ///
    /// WHY IT EXISTS. The only per-spike line aterm had ran behind
    /// `$ATERM_TRACE_LATENCY`, so a shipped binary wrote nothing; the main-thread
    /// stall watchdog is the other fallback and its release threshold is 5 SECONDS,
    /// far above a 560 ms spike. Six hours of `aterm.log` around the reading that
    /// opened this block therefore contained not one frame, present or latency
    /// line — the counters knew the worst number and the log could not say when it
    /// happened or what the loop was doing. The maxima above make the summary
    /// answer "how bad and when"; these lines are what let `aterm.log` answer "what
    /// happened at 08:31" after the fact, without a driver having been attached.
    ///
    /// READING THE OPENING LINE. `slow present: 676.7 ms at t+52355.3 s (prev
    /// present 728.1 ms ago); wake=timer late 1.0 ms; deadline=cursor_effect late
    /// 0.0 ms; acquire 0.4 ms`. The latency is an OPEN INTERVAL (output leading
    /// edge to the present that showed it): a previous present of the same window
    /// about that long ago means it presented nothing for that long, a much
    /// shorter one means its frames kept coming while this output waited. Every
    /// other field is a `last_` reading at the moment the slow frame closed, so
    /// this is a breadcrumb, not an attribution proof; `max_wake_late_ms` and
    /// `max_present_latency_at_ms` on `aterm ctl metrics` carry the worst cases.
    /// The closing line gives the count, the span and the worst reading of the
    /// whole episode.
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

    /// Close the open episode once it has gone quiet, writing its closing line.
    /// Called on every present; a field test when nothing is open.
    fn note_quiet(&mut self, now_ns: u64) {
        if let Some(closed) = slow_episode_close(&mut self.episode, now_ns, false) {
            log_slow_episode_end(closed);
        }
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
pub(crate) fn record_offscreen_raster(render_ns: u64) {
    METRICS.window.present.note_offscreen_raster(render_ns);
}

impl PresentLedger {
    /// [`record_offscreen_raster`] into this ledger.
    fn note_offscreen_raster(&self, render_ns: u64) {
        self.offscreen_rasters.inc();
        let _ = self.offscreen.record(render_ns);
    }
}

/// Install the compositor and device-execution ledgers once at GUI entry.
/// Until then neither Metal timing handler is registered.
pub(crate) fn install_present_glass_sink() {
    let _ = aterm_gpu::present_glass::install_sink(note_present_glass);
    let _ = aterm_gpu::gpu_time::install_sink(note_gpu_time);
}

/// Book one frame's device execution ([`GpuLedger::time`]): called on a Metal
/// completion thread, or on the UI thread when the wgpu arm harvests its
/// timestamp readback, with no UI lock, allocation or application callback.
fn note_gpu_time(frame: aterm_gpu::gpu_time::GpuFrameTime) {
    METRICS.window.gpu.note_time(frame);
}

/// Book a successful GPU present's encode and the shape of the offscreen it
/// presented (virtual presents included), into [`GpuLedger`].
pub(crate) fn note_gpu_frame(shape: aterm_gpu::FrameShape, encode_ns: u64) {
    METRICS.window.gpu.note_frame(shape, encode_ns);
}

/// The ablation's index in [`GpuLedger::unorm_ablation`]. Exhaustive: a new
/// mode is a compile error here, never a reading that says `off`; `off` is 0,
/// the reset state.
fn ablation_code(mode: aterm_gpu::UnormAblation) -> u8 {
    use aterm_gpu::UnormAblation::{Drop, Off, Thin};
    match mode {
        Off => 0,
        Drop => 1,
        Thin => 2,
    }
}

/// The mode `ablation_code` stored as `code`; `None` for a byte it never
/// writes.
fn ablation_of(code: u8) -> Option<aterm_gpu::UnormAblation> {
    use aterm_gpu::UnormAblation::{Drop, Off, Thin};
    [Off, Drop, Thin]
        .into_iter()
        .find(|&mode| ablation_code(mode) == code)
}

impl GpuLedger {
    /// [`note_gpu_time`] into this ledger.
    fn note_time(&self, frame: aterm_gpu::gpu_time::GpuFrameTime) {
        let _ = self.time.record(frame.gpu_ns);
        self.time_parts.set(u64::from(frame.parts));
    }

    /// [`note_gpu_frame`] into this ledger.
    fn note_frame(&self, shape: aterm_gpu::FrameShape, encode_ns: u64) {
        self.encode.record(encode_ns);
        self.encode_last.set(encode_ns);
        self.passes.set(u64::from(shape.passes));
        self.instance_bytes.set(shape.instance_bytes);
        self.build.set(shape.build_ns);
        self.stage.set(shape.stage_ns);
        self.unorm_ablation
            .set(usize::from(ablation_code(shape.ablation)));
        for (slot, count) in self.instances.iter().zip(shape.instances) {
            slot.set(u64::from(count));
        }
        self.fx_instances.set(u64::from(shape.fx_instances));
        self.fx_upload_bytes.set(shape.fx_upload_bytes);
        self.fx_refused.set(shape.fx_refused);
    }

    /// The latest frame's ablation word (`off`, `drop`, `thin`), or `unknown`
    /// for an index [`ablation_code`] never writes.
    fn unorm_ablation_name(&self) -> &'static str {
        u8::try_from(self.unorm_ablation.get())
            .ok()
            .and_then(ablation_of)
            .map_or("unknown", aterm_gpu::UnormAblation::name)
    }

    /// Raise [`GpuLedger::fx_layer_builds`] and [`GpuLedger::fx_payload_copies`]
    /// to aterm-core's fx engine totals: [`Metrics::reset`] calls it before the
    /// base is taken, and a reply that reads the process before it reads them.
    /// The totals never restart, so a sample a concurrent reply loaded before a
    /// reset's and stores after it cannot lower the total that reset rebases on.
    fn sample_fx_totals(&self) {
        self.fx_layer_builds
            .raise_total(aterm_core::render::fx_layer_builds());
        self.fx_payload_copies
            .raise_total(aterm_core::render::fx_payload_copies());
    }
}

/// Whether the device distribution holds samples (`device`), or why it holds
/// none: this build has no device clock (`unsupported`), or no frame was timed
/// (`unobserved`: no present yet, the CPU fallback, GL, or a wgpu adapter
/// without timestamp queries).
fn gpu_time_source(samples: u64) -> &'static str {
    if !aterm_gpu::gpu_time::SUPPORTED {
        "unsupported"
    } else if samples == 0 {
        "unobserved"
    } else {
        "device"
    }
}

/// The GPU fields, which close both the summary and `metrics percentiles`:
/// device execution and CPU encode as distributions, then the latest encoded
/// shape (`instances_by_stream` is `stream:n` of the non-empty streams), then
/// the fx engine's cost (design §7): the latest frame's fx instances and the fx
/// bytes it uploaded (instances, palette, atlas texels), both 0 for a frame
/// whose fx the arm did not stage, the fx sprite items it refused
/// (`fx_refused_last`, 0 within the frame art budget), and aterm-core's layer
/// seals and payload deep copies since the reset, which a settled card leaves
/// at 0 and 0. The device's dropped and skipped frame totals are
/// `aterm_gpu::gpu_time`'s, for the process's lifetime; the rest is
/// [`GpuLedger`], zeroed by `metrics reset`. A
/// reply that reads the process samples aterm-core's fx totals first
/// ([`GpuLedger::sample_fx_totals`]); an inert one reads what the ledger holds.
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
    s.count("fx_instances_last", gpu.fx_instances.get());
    s.count("fx_upload_bytes_last", gpu.fx_upload_bytes.get());
    s.count("fx_refused_last", gpu.fx_refused.get());
    if ctx.reads_process() {
        gpu.sample_fx_totals();
    }
    s.count("fx_layer_builds", gpu.fx_layer_builds.since_reset());
    s.count("fx_payload_copies", gpu.fx_payload_copies.since_reset());
}

/// Book one presented-handler sample (runs on a Metal/CoreAnimation thread).
fn note_present_glass(report: aterm_gpu::present_glass::GlassReport) {
    METRICS.window.glass.note(report);
}

impl GlassLedger {
    /// [`note_present_glass`] into this ledger.
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
/// Spans past the honesty cap are discarded, never booked: the one caller,
/// `App::note_output_redraw_request`, tests `OUTPUT_REDRAW_REQUEST_CAP_NS`
/// before it books.
pub(crate) fn note_output_redraw_request(ns: u64) {
    let _ = METRICS.window.input.out_req.record(ns);
}

/// Book one committed IME composition: preedit start → commit, in ns.
pub(crate) fn note_ime_compose(ns: u64) {
    METRICS.window.input.ime.record(ns);
}

/// Book one build of the menu-bar item's glance, in ns
/// (`InputLedger::status_glance`).
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

/// Input was refused because its session's input queue is full: a key or
/// paste (said once per session, `App::refuse_input`), or a mouse, wheel,
/// focus or colour-scheme report (silently).
pub(crate) fn note_input_refused() {
    METRICS.window.input.refused.inc();
}

/// A terminal query reply was dropped because its session's reply lane is
/// full.
pub(crate) fn note_reply_dropped() {
    METRICS.window.input.reply_dropped.inc();
}

/// The input-leg fields of `metrics percentiles`: the output→redraw-request
/// leg with its last/max, the IME composition clock with its cancel count, and
/// the refused-input and dropped-reply counts — then the menu-bar item's glance
/// build (`n_status_glance status_glance_p50/p95/p99_ms max_status_glance_ms`),
/// one session's need publication (`n_need_publish need_publish_p50/p95/p99_ms
/// max_need_publish_ms`) and the transcript apply's holds (`n_tx_chunk
/// tx_chunk_p50/p95/p99_ms n_tx_last tx_last_p50/p95/p99_ms max_tx_hold_ms`),
/// each to three places.
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

/// `(index, label)` of every deadline owner, in `DeadlineOwner::ALL` order:
/// the order each per-owner ledger publishes in.
fn owner_labels() -> impl Iterator<Item = (usize, &'static str)> {
    DeadlineOwner::ALL
        .into_iter()
        .map(|owner| (owner.index(), owner.as_str()))
}

/// `(index, label)` of every present tag, in `PresentTag::ALL` order.
fn tag_labels() -> impl Iterator<Item = (usize, &'static str)> {
    aterm_gpu::present_glass::PresentTag::ALL
        .iter()
        .map(|tag| (tag.index(), tag.as_str()))
}

/// The present→glass fields of `metrics percentiles`: the distribution with
/// its last/max, the drawables never shown, and the per-tag shown and skipped
/// ledgers (`tag:n` of the non-zero tags, the `frame_refill_full_causes`
/// shape, so a reader that parses one parses the other).
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

/// The WHEN-AND-WHOSE fields of the `metrics` summary.
///
/// Every `*_at_ms` is on the `now_ns` PROCESS clock — monotonic nanoseconds
/// since GUI entry, the clock `first_present_ms` already uses — and
/// `metrics_now_ms` is that clock read for this reply, so a reader converts a
/// stamp to "N ms ago" with one subtraction and never has to guess which hour
/// of a long-lived process a max belongs to.
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
    /// One attributed input→present slice, closed by the present at `now`,
    /// into the process-global max-fold. Same rule as the present max: a worst
    /// case with no time of occurrence cannot be correlated with anything else
    /// that happened in the process.
    fn book_present(&self, d: u64, now: u64) {
        if self.input.record(d) {
            self.input_at.set(now);
        }
    }
}

/// How many unpresented keystrokes one window's [`PendingInputStamp`] holds.
/// A fast typist's burst through a multi-second stall fits; past it the OLDEST
/// edges are kept and the rest counted in `input_edges_dropped`.
pub(crate) const INPUT_EDGE_RING: usize = 32;

/// One unpresented keystroke routed to a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InputEdge {
    /// The key's arrival (`now_ns` clock, possibly backdated by NSEvent queue
    /// age); never 0 for a held edge.
    arrival_ns: u64,
    /// When the edge was armed — compared against the last [`reset`], because a
    /// backdated arrival can precede a reset the key itself followed.
    armed_ns: u64,
    /// The predictor's op the key armed (`Predictor::newest_op`), when it armed
    /// one: a present that shows the prediction through it closes the slice
    /// ([`note_prediction_shown`]).
    op: Option<u64>,
}

/// One window's pending input→present EDGES: every unpresented keystroke routed
/// to that window, oldest first, up to [`INPUT_EDGE_RING`]. Owned by the
/// window's state (UI thread only, so plain integers), armed by
/// [`note_window_input`] and drained by that window's own content present in
/// [`record_present`] — never by another window's. See the PER-WINDOW
/// ATTRIBUTION and COORDINATED OMISSION notes on `InputLedger::input` for why
/// every edge is a sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PendingInputStamp {
    edges: [InputEdge; INPUT_EDGE_RING],
    len: usize,
    /// The newest edge is the key being handled and is not yet tagged: set by
    /// an [`arm`](Self::arm) that held the key, cleared by a refused one, by
    /// a drain and by [`tag_newest`](Self::tag_newest).
    fresh: bool,
}

impl PendingInputStamp {
    /// Arm one edge; `false` when the ring was full.
    /// Appends. A full ring first lets go of the edges already past
    /// `INPUT_SLICE_CAP_NS` (keys that never echoed: the drain would discard
    /// them anyway, so they must not hold the slots a fresh key needs); a ring
    /// still full keeps its oldest edges and answers `false`, and the caller
    /// counts the key in `input_edges_dropped`.
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

    /// Tag the key just armed with the predictor's op it armed, or settle it
    /// as one that armed none (`None`): the press path, right after the
    /// predictor took the key. A key the full ring refused tags nothing: the
    /// ring's newest edge is then another key's.
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

    /// Drain, at a present that showed the prediction through op `through`,
    /// the leading run of edges whose key armed an op up to it, oldest first,
    /// as [`drain_into`](Self::drain_into) drains them; the first key it does
    /// not show (one that armed no op, or a newer op) and every key after it
    /// stay pending, in order.
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

    /// Drop every edge whose slice would already exceed `INPUT_SLICE_CAP_NS` at
    /// `now_ns`, keeping the rest in order.
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

    /// Drain every edge at a content present of THIS window, oldest first, in
    /// place: each edge still inside `INPUT_SLICE_CAP_NS` at `now_ns` goes to
    /// `f` with its slice and whether it was armed before the reset
    /// `epoch`; an aged-out edge (a keystroke that never echoed) yields
    /// nothing. Only the held slots are touched, and the ring is empty
    /// afterwards either way.
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

    /// Every drained edge, in order (tests).
    #[cfg(test)]
    fn drained(&mut self, now_ns: u64, epoch: u64) -> Vec<DrainedEdge> {
        let mut edges = Vec::new();
        self.drain_into(now_ns, epoch, |edge| edges.push(edge));
        edges
    }

    /// The slices a drain at `now_ns` yields with no reset behind it (epoch 0
    /// classes no edge as armed before it). Which drained edges a present
    /// books is `record_present_at`'s rule, tested there (tests).
    #[cfg(test)]
    fn drain_slices(&mut self, now_ns: u64) -> Vec<u64> {
        let mut slices = Vec::new();
        self.drain_into(now_ns, 0, |edge| slices.push(edge.slice_ns));
        slices
    }
}

/// One edge as a content present drains it ([`PendingInputStamp::drain_into`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DrainedEdge {
    /// Arrival → this present.
    slice_ns: u64,
    /// Armed before the last [`reset`]: its wait belongs to the previous
    /// measurement window, so the input ledger does not book it.
    before_epoch: bool,
}

impl Metrics {
    /// The arrival a PTY-bound input stamps: the pending TRUE key arrival when
    /// one is fresh (see [`note_input`]), else `now`.
    fn input_arrival_ns(&self, now: u64) -> u64 {
        let key = self.live.lat_key.load(Ordering::Relaxed);
        if key != 0 && now.saturating_sub(key) < 500_000_000 {
            key
        } else {
            now
        }
    }

    /// [`note_window_input`] at `now`.
    fn note_window_input(&self, pending: &mut PendingInputStamp, now: u64) {
        if !pending.arm(self.input_arrival_ns(now), now) {
            self.window.input.edges_dropped.inc();
        }
    }

    /// [`note_native_window_input`] at `now`.
    fn note_native_window_input(&self, pending: &mut PendingInputStamp, arrival_ns: u64, now: u64) {
        if !pending.arm(arrival_ns, now) {
            self.window.input.edges_dropped.inc();
        }
    }

    /// [`record_native_content_present`] at `now`: the rule a content present
    /// applies to a terminal's keys (`record_present_at`), so an edge armed
    /// before the reset epoch, or aged past `INPUT_SLICE_CAP_NS`, books
    /// nothing.
    fn record_native_content_present(&self, pending: &mut PendingInputStamp, now: u64) {
        let epoch = self.epoch.load(Ordering::Relaxed);
        pending.drain_into(now, epoch, |edge| {
            if !edge.before_epoch {
                self.window.input.book_present(edge.slice_ns, now);
            }
        });
    }
}

/// Stamp the arrival of a keystroke routed to ONE window (the
/// `App::input_to_session` seam, hardware keys and controller `key` alike) into
/// that window's [`PendingInputStamp`], so only that window's content present
/// closes the slice. Same arrival and keep-oldest rules as [`note_input`]; a
/// key a full ring refuses is counted in `input_edges_dropped`.
pub(crate) fn note_window_input(pending: &mut PendingInputStamp) {
    METRICS.note_window_input(pending, now_ns());
}

/// The frame being drawn in this window shows the predictor's overlay whole
/// and under its proof, through op `through` (`Predictor::shown_through`), or
/// shows none (`None`; the render paths' `closes_input_through`): its present
/// closes the slice of every key whose prediction it shows, before that key's
/// echo, as a native page's present closes the keys its answer shows (the
/// PREDICTIONS note on `InputLedger::input`), and counts it in
/// `input_predicted`. The mark is that one frame's: the render paths set it
/// at every frame they compose. `op_is_pending` reads that same predictor:
/// cancelled or retired ops below `through` are not shown by its overlay.
pub(crate) fn note_prediction_shown(
    window: &mut WindowPresentState,
    through: Option<u64>,
    mut op_is_pending: impl FnMut(u64) -> bool,
) {
    if through.is_some() {
        // The frame's numeric prefix can skip a cancelled op. Its key stays
        // pending for a content present, as does an unmodelled key, and it
        // stops prediction booking of every key after it.
        for edge in &mut window.pending_input.edges[..window.pending_input.len] {
            if edge.op.is_some_and(|op| !op_is_pending(op)) {
                edge.op = None;
            }
        }
    }
    window.shown_through = through;
}

/// The exact non-geometric inputs a native input changed. Lifecycle identity
/// and reducer/document/service revisions come from the existing compile stamp;
/// pixel geometry and paint hashes are not an input authorization version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NativeInputRevision {
    instance: crate::tab_model::AppInstanceId,
    view: crate::tab_model::ViewId,
    generation: u64,
    config: u64,
    update: u64,
    document: Option<u64>,
    presentation: u64,
}

impl From<crate::app_native::NativeUiCompileStamp> for NativeInputRevision {
    fn from(stamp: crate::app_native::NativeUiCompileStamp) -> Self {
        Self {
            instance: stamp.instance,
            view: stamp.view,
            generation: stamp.generation,
            config: stamp.config_revision,
            update: stamp.update_revision,
            document: stamp.document_seq,
            presentation: stamp.presentation_revision,
        }
    }
}

/// Native edges awaiting the revision they changed. A different page or a
/// later unrelated revision cannot close them. Consecutive changes coalesce
/// only while the next input starts at the last input's revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PendingNativeInputStamp {
    revision: Option<NativeInputRevision>,
    edges: PendingInputStamp,
}

impl PendingNativeInputStamp {
    /// Move the ring to `after` for a dispatch that started at `before`, and
    /// answer whether its key is armed: `false` when the dispatch left the
    /// revision unchanged or crossed to another instance or view. Edges armed
    /// toward a revision other than `before` are let go, so only consecutive
    /// changes coalesce.
    fn advance(&mut self, before: NativeInputRevision, after: NativeInputRevision) -> bool {
        if before.instance != after.instance || before.view != after.view || before == after {
            return false;
        }
        if self.revision != Some(before) {
            self.edges = PendingInputStamp::default();
        }
        self.revision = Some(after);
        true
    }

    /// Whether a key a native page took is still waiting on its present.
    #[cfg(test)]
    pub(crate) const fn is_pending(self) -> bool {
        self.edges.len != 0
    }
}

/// The arrival a keystroke a NATIVE page takes (Settings search, the editor) is
/// timed from: read before the page's reducer runs, so the reducer and the
/// compile it schedules are inside the slice. The same backdated arrival as
/// [`note_window_input`]'s.
pub(crate) fn native_input_arrival() -> u64 {
    METRICS.input_arrival_ns(now_ns())
}

/// Arm a keystroke a native page took (`arrival_ns` from
/// [`native_input_arrival`]) in its window's native ring
/// ([`WindowPresentState::pending_native_input`]) when its dispatch moved the
/// page from revision `before` to `after`; only a present that puts that
/// revision's pixels on glass closes it ([`record_native_content_present`]).
/// A native page answers a key with pixels, never with PTY output, so the
/// terminal ring's close (a present with output pending) never sees one. A
/// dispatch that left the revision unchanged, or crossed to another page,
/// arms nothing. No PTY is waiting on it, so it leaves the reader's
/// typing-hot slicing alone. A key a full ring refuses is counted in
/// `input_edges_dropped`.
pub(crate) fn note_native_window_input(
    pending: &mut PendingNativeInputStamp,
    before: crate::app_native::NativeUiCompileStamp,
    after: crate::app_native::NativeUiCompileStamp,
    arrival_ns: u64,
) {
    if pending.advance(before.into(), after.into()) {
        METRICS.note_native_window_input(&mut pending.edges, arrival_ns, now_ns());
    }
}

/// A present that put a native page's new pixels on glass: close THIS
/// window's pending native keystrokes, every edge a sample, into the same
/// input→present ledger (`input_*`) a terminal's echo books, when `presented`
/// is the revision those keys produced. Typing latency is one figure
/// whichever surface the keys land in.
pub(crate) fn record_native_content_present(
    pending: &mut PendingNativeInputStamp,
    presented: crate::app_native::NativeUiCompileStamp,
) {
    if pending.revision != Some(NativeInputRevision::from(presented)) {
        return;
    }
    let mut taken = std::mem::take(pending);
    METRICS.record_native_content_present(&mut taken.edges, now_ns());
}

/// Stamp the arrival of PTY-bound input that names a session but no window (a control
/// `send`/`feed` to a background session, a controller host write), keeping the oldest
/// unpresented arrival; any window's content present closes it. Window-routed keys use
/// [`note_window_input`].
///
/// Inherits the pending true key arrival when armed (backdated by the NSEvent queue age;
/// see [`note_key_arrival_queued`]), so `input->present` includes time parked in the OS
/// event queue. Freshness-gated to 500 ms: a key that never reached the PTY (a UI
/// shortcut, where `note_pty_write` never runs) must not lend its arrival to a later
/// control `send`.
pub(crate) fn note_input() {
    let m = &METRICS;
    m.window
        .input
        .session_stamp
        .arm(m.input_arrival_ns(now_ns()));
}

/// Stamp the arrival of a window-bounds change (`WindowEvent::Resized`), opening
/// the stale-frame window that the next present closes.
///
/// Call this BEFORE the swapchain is reconfigured, so the slice contains the
/// whole interval during which the layer is the new size and the newest drawable
/// is still the old one — the interval CoreAnimation fills by rescaling the
/// previous frame. Keeps the OLDEST unpresented change (see
/// `PresentLedger::resize_stamp`): a drag burst reports how long the window was
/// actually mismatched, not just the tail after its final event.
pub(crate) fn note_resize_arrival() {
    let present = &METRICS.window.present;
    let now = now_ns();
    present.resize_stamp.arm(now);
    present.reflow_stamp.arm(now);
}

/// The engine just committed a new grid geometry (`apply_term_resize` reported a real
/// change), closing the stale-grid slice a bounds change opened: from here the rows/cols
/// on screen match the window. A commit with no bounds change behind it (the control
/// `resize` verb, a font zoom, a config re-grid) finds no stamp.
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

// ---- The latency trace: isolate the UI-thread key->write component -----------
// `note_input` now runs BEFORE encode/write, so `input_present` already spans the
// full key-arrival → content-present path (including encode and the PTY write).
// This trace keeps an independently useful key→write slice: it identifies UI
// dispatch/encoding/lock/write stalls inside a high end-to-end sample — but it
// starts at the BACKDATED hardware arrival, so the OS event queue's share is
// inside it too, and a high reading ALONE cannot say which of the two it was.
// That share is published apart as `key_queue_*` (see `InputLedger::key_queue`),
// and `key_write - key_queue` is the UI-dispatch slice this trace is named for.
// Do NOT add it to `input_present`; it is a contained component of that total.
// Logging is a development seam ([`latency_trace_on`]); the cheap metrics atomics
// remain always on.
static LAT_TRACE_ON: OnceLock<bool> = OnceLock::new();

/// Whether the latency stderr traces are armed: the per-key `KEYWRITE` line here and the
/// per-present `output->present` line (`present_latency_ns_with_plan`). A development seam
/// (`$ATERM_TRACE_LATENCY`, [`aterm_types::dev_seam!`]): a shipped binary never logs them,
/// and `metrics` publishes the same numbers in every build.
pub(crate) fn latency_trace_on() -> bool {
    *LAT_TRACE_ON.get_or_init(|| aterm_types::dev_seam!("ATERM_TRACE_LATENCY").is_some())
}

/// Microsecond view of the metrics clock, the epoch of [`note_key_arrival_queued`], so
/// `video` frame stamps and input stamps subtract validly.
#[must_use]
pub(crate) fn now_us() -> u64 {
    now_ns() / 1000
}

/// Stamp the true arrival of a printable hardware keystroke (winit `KeyboardInput`, before
/// encode/write), backdated by `queue_ns`, the NSEvent's time in the OS event queue
/// (macOS and X11: `platform::current_event_queue_age_ns`; 0 elsewhere), so `key->write`
/// and the inherited `input->present` include queueing a parked loop caused. Keep-latest:
/// each key is measured against its own write.
pub(crate) fn note_key_arrival_queued(queue_ns: u64) {
    METRICS.note_key_arrival_queued(queue_ns);
}

/// Disarm the key-arrival stamp after a key's dispatch ends without a PTY write, so a
/// later control `send` never inherits it through [`note_input`]. A no-op after a writing
/// key.
pub(crate) fn clear_key_arrival() {
    METRICS.live.lat_key.store(0, Ordering::Relaxed);
}

/// The current key's true hardware arrival in ms (the backdated stamp
/// [`note_key_arrival_queued`] wrote), as a peek; [`note_pty_write`] owns the consuming
/// swap. `None` when unarmed (a bare modifier, an IME run, a control `send`, or a key
/// whose write already ran).
///
/// The rainbow-kitty melody's clock: two keys are heard the interval apart the fingers
/// struck them, not the interval their writes returned in. The write's `swap(0)` runs
/// before the audio push, so the cue's mint peeks while the stamp is armed. Same epoch
/// and unit as `app_render::input_clock_ms` (metrics clock in ms, never 0).
#[must_use]
pub(crate) fn key_arrival_ms() -> Option<u32> {
    match METRICS.live.lat_key.load(Ordering::Relaxed) {
        0 => None,
        ns => Some(((ns / 1_000_000) as u32).max(1)),
    }
}

/// The PTY write just returned (after the press-path `term_lock`s and, on Windows, the
/// blocking `WriteFile`). Records the key→write component of `input_present`; always
/// recorded, and logged under [`latency_trace_on`].
pub(crate) fn note_pty_write() {
    note_pty_write_at(take_key_arrival());
}

/// Claim the pending key-arrival stamp without recording anything, for whoever performs
/// the write. The paste-ordering FIFO defers a keystroke's egress to a writer thread;
/// consuming the stamp at enqueue would time the channel push, not the real write queued
/// behind the paste. Pair with [`note_pty_write_at`].
#[must_use]
pub(crate) fn take_key_arrival() -> u64 {
    METRICS.live.lat_key.swap(0, Ordering::Relaxed)
}

/// Record a key→write slice against a stamp claimed by [`take_key_arrival`], for writes
/// that complete off the UI thread. A 0 stamp (no key behind the write) records nothing.
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
             + blocking WriteFile)",
            d as f64 / 1_000_000.0,
            input.key_queue.last() as f64 / 1_000_000.0
        );
    }
}

impl Metrics {
    /// [`note_key_arrival_queued`] into this root.
    fn note_key_arrival_queued(&self, queue_ns: u64) {
        self.live
            .lat_key
            .store(now_ns().saturating_sub(queue_ns).max(1), Ordering::Relaxed);
        // AND PUBLISH THE BACKDATE ITSELF. The stamp above folds the queue wait
        // into every downstream total; this is the only record of how much it
        // folded in, and without it `key_write` cannot be split back into the
        // parked loop and aterm's own dispatch. See `InputLedger::key_queue`.
        let _ = self.window.input.key_queue.record(queue_ns);
    }
}

/// A DEC-2026 hold episode armed (the FALSE→TRUE rising edge saw its first held
/// frame).
pub(crate) fn note_sync_armed() {
    METRICS.window.sync.holds_armed.inc();
}

/// An armed episode released because the app ended sync (`?2026l`): the healthy cause.
pub(crate) fn note_sync_release_end() {
    METRICS.window.sync.releases_end.inc();
}

/// An armed episode released by the safety-valve deadline: the app went silent (or the
/// hold mis-paced) after `?2026h`. Climbing during ordinary typing is the ~5 fps failure.
pub(crate) fn note_sync_release_timeout() {
    METRICS.window.sync.releases_timeout.inc();
}

/// Per-window lease behind the process-wide synchronized-output hold gauge. A last-writer
/// boolean would let one window's release clear a sibling's active hold, so each window
/// contributes at most one count, idempotently, released on drop.
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

    #[cfg(test)]
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

/// A posted `Wake::Output` waited past the latch expiry behind the main
/// thread's own turn (booked once per wake; no action follows).
pub(crate) fn note_output_wake_late() {
    METRICS.window.sync.output_wake_late.inc();
}

/// A PTY reader re-posted a `Wake::Output` stranded with its loop wake-up
/// (see `SyncLedger::output_wake_reposts`).
pub(crate) fn note_output_wake_repost() {
    METRICS.window.sync.output_wake_reposts.inc();
}

/// Record one redraw pass's total wall time (entry → present done or failed surface
/// transaction), including the acquire/present wait `frame_render` excludes. Failed
/// attempts count: omitting them would hide the very redraw responsible for a stall.
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

/// The presented-path frame extraction completed: which arm refilled the snapshot
/// (`Scoped` keeps undamaged rows; `Full` is the fallback walk) and, on the full arm,
/// which continuity clause refused.
pub(crate) fn note_frame_refill(refill: aterm_core::render::FrameRefill) {
    METRICS.window.redraw.note_refill(refill);
}

/// A presented frame reused the existing snapshot: the engine marked no damage and
/// processed no bytes since it was filled. Counted so `frame_refills_scoped +
/// frame_refills_full + frame_refills_skipped` stays one per presented non-rescan frame.
pub(crate) fn note_frame_refill_skipped() {
    METRICS.window.redraw.refills_skipped.inc();
}

/// One resident-pet world observation that stood, and the cells it classified. Called on
/// every path that runs the walk (live presented, composed-focus, explicit capture), so
/// `pet_world_observations` is not bounded by `frames`. A refused observation counts with
/// zero cells: a walk thrown away is still cost.
pub(crate) fn note_pet_world_observation(cells: usize) {
    METRICS.window.redraw.note_pet_world(cells);
}

/// A gather drained `n` process events ([`CommandLedger::proc_events`]). These five are
/// noted by the unix PTY gather alone (`exec_edges`, the reader's watch).
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

/// A parse stage took a slice hold, after waiting `waited_ns` for the term
/// lock (`0`: it was free).
pub(crate) fn note_parse_hold(waited_ns: u64) {
    METRICS.window.command.note_parse_hold(waited_ns);
}

/// The UI thread's status sweep took `ns`.
pub(crate) fn note_status_sweep(ns: u64) {
    METRICS.window.command.note_status_sweep(ns);
}

impl CommandLedger {
    /// [`note_parse_hold`] into this ledger: every hold counts, and only a
    /// wait (`waited_ns > 0`) books a lock wait and its time.
    fn note_parse_hold(&self, waited_ns: u64) {
        self.parse_holds.inc();
        if waited_ns > 0 {
            self.parse_lock_waits.inc();
            self.parse_lock_wait_ns.add(waited_ns);
        }
    }

    /// [`note_status_sweep`] into this ledger.
    fn note_status_sweep(&self, ns: u64) {
        self.status_sweeps.inc();
        self.status_sweep_ns.add(ns);
    }
}

/// The summary's card-engine cost ([`CommandLedger`]), its times in
/// milliseconds to three places.
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

    /// The card engine's cost rides the summary between the pet world's
    /// counters and the cursor tick's, its times in milliseconds to three
    /// places. A free parse hold is a hold that books no wait; a contended one
    /// books the hold, the wait and its time. The control: after a reset every
    /// field reads 0 again.
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

/// Times one status sweep that visits sessions (not the deadline gate's
/// no-op), booked when it drops (every return path after it).
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

/// One cursor-effect tick's output: the `glow_add` quads, the `glow_under`
/// quads and the trail cells it composed for the frame (see `Live::fx_*` and
/// `RedrawLedger::fx_under_frames`). Only a non-empty `glow_under` stream
/// books an under frame.
pub(crate) fn note_fx_composed(glow_quads: usize, under_quads: usize, trail_cells: usize) {
    METRICS.note_fx_composed(glow_quads, under_quads, trail_cells);
}

/// One effect stream of the final composed input: its `fx_streams_last` label
/// and the reader of its length.
type FxStream = (&'static str, fn(&aterm_render::RenderInput) -> usize);

/// Every effect stream `fx_streams_last` publishes, in its order.
static FX_STREAMS: [FxStream; 15] = [
    ("trail", |i| i.cursor_trail.len()),
    ("glow_add", |i| i.cursor_glow_add.len()),
    ("glow_halo", |i| i.glow_halo.len()),
    ("glow_under", |i| i.glow_under.len()),
    ("fire_patch", |i| i.fire_patch.len()),
    ("fire_halo", |i| i.fire_halo.len()),
    ("nova_add", |i| i.nova_add.len()),
    ("rain", |i| i.rain_quads.len()),
    ("rain_add", |i| i.rain_add.len()),
    ("cat", |i| i.cat_quads.len()),
    ("free", |i| i.free_sprites.len()),
    ("wdeco", |i| i.word_decorations.len()),
    // The per-cell glyph recolours: the card line's and the cursor glow's ink,
    // the fire's dark cores.
    ("ink", |i| i.ink.len()),
    ("char_fg", |i| i.char_fg.len()),
    // The retained fx engine's layers (design §2), each a sealed payload.
    ("fx_layers", |i| i.fx_layers.len()),
];

/// Book every effect stream's length off the final composed `input`, at the
/// present seam ([`RedrawLedger::fx_streams`]): one relaxed store per stream per
/// present.
pub(crate) fn note_fx_streams(input: &aterm_render::RenderInput) {
    METRICS.window.redraw.note_fx_streams(input);
}

impl RedrawLedger {
    /// [`note_fx_streams`] into this ledger.
    fn note_fx_streams(&self, input: &aterm_render::RenderInput) {
        for (slot, (_, len)) in self.fx_streams.iter().zip(FX_STREAMS.iter()) {
            slot.set(len(input) as u64);
        }
    }
}

/// The summary's `fx_streams_last`: `stream:n` of the non-empty effect streams
/// in the latest present's final composed input, in [`FX_STREAMS`] order. The
/// GPU frame's shape and timing are [`emit_gpu`]'s.
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

    /// `fx_streams_last` reads every stream off the final composed input,
    /// whoever produced it: a stream the cursor tick never writes (`nova_add`)
    /// is counted beside the tick's own, and an empty stream is left out. The
    /// control: the next frame with no effect clears both, and the field reads
    /// `none` (`{}` in JSON), never a bare `key=`.
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

/// A synchronized-output hold intentionally retained the previous frame.
pub(crate) fn note_redraw_sync_hold() {
    METRICS.window.redraw.sync_holds.inc();
}

/// A redraw source fired while surface recovery was backed off or parked. Expected for an
/// occasional animation tick; sustained growth points at a caller still requesting
/// redraws.
pub(crate) fn note_redraw_retry_gated() {
    METRICS.window.redraw.retry_gated.inc();
}

/// A redraw reached the surface-acquire seam. `compose_ns` includes every
/// pre-acquire operation, notably full `Terminal::cell_frame_into` extraction.
pub(crate) fn note_pre_present(compose_ns: u64) {
    METRICS.window.redraw.note_pre_present(compose_ns);
}

/// The surface transaction failed after a redraw reached the present seam.
pub(crate) fn note_present_drop(reason: PresentDropReason, parked: bool) {
    METRICS.note_present_drop(reason, parked);
}

/// Update the live disposition of an already-counted dropped frame. Recovery
/// can discover a more specific downstream cause (for example CPU fallback
/// construction after a lost GPU) without pretending that the same frame was
/// dropped twice.
pub(crate) fn update_present_drop_disposition(reason: PresentDropReason, parked: bool) {
    METRICS.window.redraw.set_drop_disposition(reason, parked);
}

impl RedrawLedger {
    /// [`note_frame_refill`] into this ledger.
    fn note_refill(&self, refill: aterm_core::render::FrameRefill) {
        match refill {
            aterm_core::render::FrameRefill::Scoped { .. } => self.refills_scoped.inc(),
            aterm_core::render::FrameRefill::Full { cause } => {
                self.refills_full.inc();
                self.refill_cause.inc(cause.index());
            }
        }
    }

    /// [`note_pet_world_observation`] into this ledger.
    fn note_pet_world(&self, cells: usize) {
        self.pet_world_observations.inc();
        self.pet_world_cells.add(cells as u64);
    }

    /// [`note_pre_present`] into this ledger.
    fn note_pre_present(&self, compose_ns: u64) {
        self.pre_present_attempts.inc();
        self.pre_present_total.add(compose_ns);
        let _ = self.pre_present.record(compose_ns);
    }

    /// [`update_present_drop_disposition`] into this ledger.
    fn set_drop_disposition(&self, reason: PresentDropReason, parked: bool) {
        self.drop_reason.set(reason.index());
        self.drop_parked.set(parked);
    }
}

impl Metrics {
    /// [`note_fx_composed`] into this root.
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

    /// [`note_present_drop`] into this root.
    fn note_present_drop(&self, reason: PresentDropReason, parked: bool) {
        self.window.redraw.drops.inc();
        self.window.redraw.set_drop_disposition(reason, parked);
        // OCCLUSION/PARK taint (item 10). Only drops that stop the present stream
        // for an UNBOUNDED interval count: occluded glass, or a drop the retry
        // scheduler parked awaiting an external stimulus. A transient
        // `CpuAcquire`/`GpuTimeout` that retries a millisecond later is a stall the
        // user genuinely felt and MUST stay in the on-glass distribution — taint it
        // and the honesty pass would delete the very samples it exists to protect.
        if drop_taints_present_latency(reason, parked) {
            self.arm_present_taint();
        }
    }
}

/// Publish the event loop's selected deadline as the producer REQUESTED it
/// (`None` means pure `Wait`), and book how the healer answered it.
///
/// The healer ([`crate::deadline_heal::DeadlineHealer::heal`]) decides what
/// is armed; this only observes. The recorded `deadline_late_ms` and
/// `past_deadline_arms` describe the requested deadline, so the diagnostics
/// stay honest about what the producer asked for even on healed turns, and
/// `heal`'s two flags land in `stale_arm_heals` and the owner's
/// `past_arm_streak_heals` entry.
pub(crate) fn note_deadline(
    owner: DeadlineOwner,
    requested: Option<Instant>,
    now: Instant,
    heal: Heal,
) {
    METRICS.note_deadline(owner, requested, now, heal);
}

/// Store the GPU renderer's cumulative repaint-scope counters (main thread,
/// after each GPU present). A renderer rebuilt since the reset counts from its
/// own start.
pub(crate) fn note_gpu_repaint_totals(scissor_taken: u64, full_repaints: u64, scroll_rescues: u64) {
    let scope = &METRICS.window.gpu_scope;
    scope.scissor_taken.set_total(scissor_taken);
    scope.full_repaints.set_total(full_repaints);
    scope.scroll_rescues.set_total(scroll_rescues);
}

/// Record why winit began this event-loop iteration and attribute a timer wake
/// to the owner published by the preceding `about_to_wait` pass.
pub(crate) fn note_event_wake(kind: EventWakeKind) {
    METRICS.note_event_wake(kind);
}

impl Metrics {
    /// [`note_deadline`] into this root.
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
            // Per owner, not one global counter, for the same reason as
            // `deadline_arms_by_owner`: a heal that cannot name its producer
            // sends the investigation to the wrong module.
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
                // Booked against the owner that WON the fold, beside the global
                // count it disambiguates, so the two can never drift.
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
        // The same last-writer erasure as the wake lateness: every event-loop
        // turn arms a deadline, so this store is overwritten within a frame.
        // Keep the worst, the owner that asked for it, and when it was asked.
        sched.deadline_late.record(late, owner.index(), clock_now);
    }

    /// [`note_event_wake`] into this root.
    fn note_event_wake(&self, kind: EventWakeKind) {
        let (sched, live) = (&self.window.sched, &self.live);
        sched.event_wakes.inc();
        live.last_wake_kind
            .store(kind.index() as u64, Ordering::Relaxed);
        // Only `ResumeTimeReached` is caused by the selected deadline. An external
        // event (`WaitCancelled`), explicit polling, or initialization may merely
        // INTERRUPT a pending blink/predictor/retry deadline; attributing that wake
        // to the interrupted owner makes the diagnostic falsely causal.
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
                // …AND THE WORST ONE, WHICH THE NEXT WAKE MAY NOT ERASE. The
                // last-writer store below is exactly how a 400 ms late timer used
                // to vanish: the `WaitCancelled` behind it — one per PTY output
                // burst — stores 0 over it before anyone can read it. Single
                // writer (the event loop), so whichever sample wins the fold
                // stamps its own owner and instant.
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
pub(crate) fn set_backend_gpu(on: bool) {
    METRICS.live.backend_gpu.store(on, Ordering::Relaxed);
}

/// Serializes the tests that move or read the two handoff input counters
/// (`Live::handoff_input_*`): they are process-wide, and an exact reading must
/// not race a sibling's drop. EVERY test that overflows the pre-Commit queue or
/// settles an incoherent replay holds it — the Tier-1 replays in
/// `precommit_input_conformance` included, which move both.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))] // its tests are unix-only
pub(crate) static HANDOFF_INPUT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Count one pre-Commit input event the successor's queue refused.
pub(crate) fn note_handoff_input_dropped() {
    METRICS
        .live
        .handoff_input_dropped
        .fetch_add(1, Ordering::Relaxed);
}

/// Count one pre-Commit replay whose modifiers had to be disarmed.
#[cfg(any(unix, test))]
#[cfg_attr(not(unix), allow(dead_code))] // its tests are unix-only
pub(crate) fn note_handoff_input_incoherent() {
    METRICS
        .live
        .handoff_input_incoherent
        .fetch_add(1, Ordering::Relaxed);
}

/// `(input_dropped, input_incoherent)`, cumulative for this process (tests,
/// under [`HANDOFF_INPUT_TEST_LOCK`]).
#[cfg(test)]
#[must_use]
#[cfg_attr(not(unix), allow(dead_code))] // its tests are unix-only
pub(crate) fn handoff_input_counts() -> (u64, u64) {
    let live = &METRICS.live;
    (
        live.handoff_input_dropped.load(Ordering::Relaxed),
        live.handoff_input_incoherent.load(Ordering::Relaxed),
    )
}

/// The summary's handoff-input fields: `input_dropped` and `input_incoherent`.
/// CUMULATIVE and deliberately NOT cleared by [`reset`] ([`Live`]): they
/// describe the one handoff this process adopted, at its birth, and a `metrics
/// reset` taken afterwards must not erase a lost keystroke.
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

/// One live session's place in `Live::pty_sessions`: taken by `spawn_session`
/// for the `Session` it builds, given back when that `Session` drops. The
/// session closes its master only through its sink's last clone, so a count of
/// masters the sinks still own (`aterm_session::sink::owned_masters_open`)
/// above this one, once the closing sessions' readers have wound down, is a
/// master a closed tab left open ([`emit_pty_census`]).
#[derive(Debug)]
pub(crate) struct PtyCensus(CensusPlace);

/// Where a session is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CensusPlace {
    None,
    Session,
    Spare,
}

impl PtyCensus {
    /// A session that owns a PTY master: counted until it drops.
    pub(crate) fn counted() -> Self {
        METRICS.live.pty_sessions.fetch_add(1, Ordering::AcqRel);
        Self(CensusPlace::Session)
    }

    /// A warm spare's master (`crate::spare`): counted apart, in `spare_ptys=`,
    /// until its adoption swaps in [`Self::counted`].
    pub(crate) fn spare() -> Self {
        METRICS.live.spare_ptys.fetch_add(1, Ordering::AcqRel);
        Self(CensusPlace::Spare)
    }

    /// A session with no PTY of its own (a stub, a held pane): not counted.
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

/// The warm spares' PTYs (`spare_ptys=`, `Live::spare_ptys`).
pub(crate) fn spare_ptys() -> usize {
    METRICS.live.spare_ptys.load(Ordering::Acquire)
}

/// The PTY masters waiting in the spawn-kit pool (`pty_kits=`): opened ahead of
/// a spawn by `spawn_prep`'s fill (`aterm_pty::fill_spawn_kits`), owned by no
/// session until a spawn takes one. Raw descriptors in `aterm_pty`, never a
/// sink's, so `pty_masters` leaves them out; every `/dev/ptmx` the process holds
/// is a session's, a spare's (`spare_ptys=`) or one of these.
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

/// The summary's PTY census: `pty_masters` (the PTY masters this process's
/// session sinks hold open, less the warm spares'), `pty_sessions` (the live
/// sessions that own one, [`PtyCensus`]) and `pty_kits` ([`pty_kits`], the
/// masters opened ahead for the next spawns). Masters equal sessions at rest. A
/// closing session's master stays open until its reader has wound down (a
/// hang-up and a wake, normally well under a second), so `pty_masters` may lead
/// briefly after a close; a lead that STAYS is a master nothing will close —
/// the leak of 2026-09-29, where a closed tab 0's master outlived it for the
/// window's life. The process's open `/dev/ptmx` descriptors number
/// `pty_masters + pty_kits + spare_ptys`.
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

/// Open a new measurement window so a driver can time a SPECIFIC operation:
/// `metrics reset`, run the workload, then `metrics`. [`Metrics::reset`] zeroes
/// every window ledger, so the fresh window's maxima drop the cold-start spike
/// and every `last_*` that is an observation of the window reads `0.00` until
/// a sample lands: a `0.00` means "no sample in this window", never the
/// previous run's number, so a zero-sample run cannot pass as a regression
/// test (the touch-to-glass audit). [`Live`] state and the startup facts
/// survive. The wake table and the turn census are window stats kept beside
/// the root and reset with it; the verb resets the echo ring
/// (`crate::echo_rtt::reset`).
pub(crate) fn reset() {
    METRICS.reset();
    wakes::WAKES.reset();
    // The main-loop turn census is a window stat like every other `max_` on this
    // line: a driver that resets, drives a workload and reads must see THAT
    // workload's worst main-thread turn, not the launch storm's.
    crate::watchdog::reset_turn_census();
}

/// The `metrics` summary, in wire order: one sink call per field, so the text
/// line and the `--json` object ([`wire`]) carry the same keys, in the same
/// order, with the same values. `dims` is the grid `(rows, cols,
/// scrollback_truncated_lines)`, or `None` while the terminal mutex is busy.
///
/// `metrics [reset]` -> one `OK k=v ...\n` line of live render/latency counters so a
/// driving AI can MEASURE responsiveness AND DETECT lag in the same loop it drives
/// with — `send`/`key`, then `metrics`. `metrics reset` first zeroes the
/// measurement-window stats (frames / maxima / slow count) so a SPECIFIC workload can
/// be timed: `metrics reset`, drive it, then `metrics`.
///
/// Fields: `backend=<cpu|gpu>`, grid `rows`/`cols` (`busy` in text / `null` in
/// JSON when the terminal mutex is occupied, without withholding the remaining
/// lock-free diagnostics), `frames` (successful application presents since reset
/// — a steady app-render frame does NOT advance it),
/// `last_/max_present_latency_ms` (the `output→application-present-return`
/// slice `$ATERM_TRACE_LATENCY` logs, most-recent + worst), and the
/// LAG SIGNATURE: `last_/max_frame_render_ms` + `slow_frames` (frames over the ~30 fps
/// budget, `slow_threshold_ms`). A non-zero `slow_frames`, a large
/// `max_frame_render_ms`, or `backend=cpu` under heavy output all mean the terminal is
/// lagging. Values are this module's process-global counters + the grid size.
///
/// PACING/SHED SIGNATURE (the 2026-07-05 incident class):
/// `last_/max_input_present_ms` (key arrival → the first attributed successful
/// content-present return — a software-side typing-latency proxy),
/// `sync_armed`/`sync_rel_end`/`sync_rel_timeout` (DEC-2026 hold episodes by release
/// cause — timeouts climbing during ordinary typing = the SYNC-1 bug), `sync_holding`,
/// and `perf_reduced` + `shed_transitions` (the load-shed latch; engaged during light
/// typing, or flapping at idle, are both wrong).
///
/// WHEN, AND WHOSE. A worst case with no time of occurrence cannot be
/// correlated with anything else that happened in the process, and a LAST-WRITER
/// lateness reading is erased before anyone can read it: a live line carried
/// `max_present_latency_ms=560.54` beside `wake_late_ms=0.00
/// deadline_late_ms=0.00`, because a late `Timer` wake's lateness is overwritten
/// with `0` by the next `WaitCancelled` — one per output burst. So the summary
/// also carries `max_wake_late_ms`/`max_deadline_late_ms` with the OWNER and the
/// `_at_ms` instant of each, `max_present_latency_at_ms` /
/// `max_input_present_at_ms`, and `max_present_latency_gap_ms` — the gap since
/// the presenting window's previous present for the frame that set the present
/// max, which separates "that window presented nothing for that long" from "its
/// frames kept coming while this output waited". Every `_at_ms` is on the process clock `metrics_now_ms` reads at the
/// same instant, so "how long ago" is one subtraction. A run of spikes over
/// 100 ms also writes an `aterm.log` line when it starts and, when it had more
/// than one, a summary when it ends, in EVERY build — the only other per-spike
/// record is `$ATERM_TRACE_LATENCY`'s stderr line, and the release stall
/// watchdog's threshold is 5 s.
///
/// THE PARK OUTSIDE THE REDRAW. Everything named so far prices a redraw, a
/// wake's lateness, or an end-to-end interval that contains both. None of them
/// could name a main thread parked inside a NON-redraw handler for 100–600 ms —
/// too short for the 5 s release stall watchdog, never inside the redraw timer,
/// and charged in full to `present_latency` and `input_present`. So the summary
/// carries the MAIN-LOOP TURN census (`crate::watchdog`): `max_turn_ms` with the
/// winit root that owned it (`max_turn_owner=user_event`) and the `_at_ms`
/// instant it ended, plus `last_turn_ms`, the `turns` booked and the
/// `long_turns` among them at or over `long_turn_threshold_ms` (one 30 fps frame
/// budget). Read the max AGAINST `max_redraw_total_ms`: both large is a slow
/// frame, a large `max_turn_ms` with a small `max_redraw_total_ms` is a park no
/// other instrument reaches — the bookkeeping the `Wake::Output` arm runs ahead
/// of its redraw fan-out being the standing candidate. A turn that never ENDS
/// books nothing here by construction; that one is the stall watchdog's, and it
/// logs a named root.
///
/// THE TAIL, ON THE SUMMARY LINE. `last_` is a momentary reading and `max_` is
/// an unbounded worst case; neither brackets a distribution. During the 2026-08
/// spin `last_input_present_ms=8.1` read healthy while the live p99 was 335 ms.
/// So the summary now also carries `n_input` + `input_p50/p95/p99_ms` and
/// `n_present` + `present_p50/p95/p99_ms` — the same histograms `percentiles`
/// publishes, so a healthy median cannot mask a sick tail from a reader who
/// asked only one question.
///
/// US OR THEM, ON THE SAME LINE. Every slice named so far measures ATERM, so a
/// sick tail among them cannot say whose lag it is. The summary therefore also
/// carries [`crate::echo_rtt`]'s fragment — `n_echo` + `echo_p50/p95/p99_ms`,
/// `echo_last_/echo_max_ms`, and the `echo_arms`/`echo_coalesced`/
/// `echo_expired`/`echo_dropped_locked` ledger that qualifies them — measuring
/// bytes out to the PTY → the first bytes back, which belongs to the CHILD.
/// Read against `input_*`: echo high with input low means the program is
/// behind; the reverse means we are. It is spliced whole, never as a lone
/// percentile, and aterm cannot fix a sick one by fiat — it can only decline to
/// compete with the child (its gate, cutter and package lanes run at utility).
///
/// PRESENT-LATENCY HONESTY. `present_*` describes aterm ONLY while the window is
/// actually presenting, so an output→present sample taken while the window was
/// occluded/parked, while a `video` capture was pacing presents, or at a present
/// a still capture's barrier forced (`image`, `window`, the SIGUSR1 snapshot), is
/// booked to a separate ledger: `present_tainted` / `last_/max_present_tainted_ms`,
/// with its own `present_tainted_p50/p95/p99_ms` under `percentiles`.
/// `capture_episodes` counts recordings opened in this window and
/// `capture_active` is the live gauge; `capture_presents` counts the samples at
/// presents a still capture's barrier forced, each also in `present_tainted`, so
/// the rest of `present_tainted` is occlusion/park or a recording. Any non-zero
/// `capture_episodes` or `capture_presents` means a capture-based instrument
/// (`video`, or `image`/`window`/the snapshot) ran inside your measurement
/// window. Nothing is discarded: `n_present + n_present_tainted` still accounts
/// for every content present.
///
/// WHAT THESE SLICES INCLUDE — READ BEFORE QUOTING ONE AS "LATENCY". Both
/// key→present slices are OPEN INTERVALS closed by the next qualifying present,
/// so any time in which nothing presented is INSIDE the number. They are not
/// floored by a sampling rate (that is the `video … keys` failure mode; see
/// `crate::video_key_analysis`), but they have their own two:
///
///  * `present_*` (`output→application-present-return`) opens on the LEADING
///    EDGE of a PTY output burst — a `compare_exchange(0, …)`, first edge wins —
///    and closes on the next content present that books it. Output that moves
///    no pixels opens the interval and does not close it, so the wait until
///    something else presents is published as if it were render time. The only
///    bound is a 5 s discard (`PRESENT_LATENCY_CAP_NS`), so a multi-SECOND
///    reading means "nothing presented for that long", NOT "a frame took that
///    long". Read it with `n_present` and the percentiles beside it, never as a
///    lone `last_`/`max_`.
///  * `input_*` (`key-arrival→content-present-return`): a key typed into a
///    window is closed only by that window's next content present, or, when
///    a present of that window first shows the key's prediction under its
///    proof (`predictive_echo = "adaptive"`), by that present, before the
///    echo (`input_predicted` counts those; the PREDICTIONS note on
///    `InputLedger::input`); input sent
///    to a session (`send`/`feed`) closes on any window's present. Under
///    streaming output in the SAME window the closing
///    present may be a log-line frame rather than the key's echo (reads low);
///    a key that never echoes — a password prompt — books its whole wait until
///    the next content present, up to the 5 s discard (reads high).
///
///  * `resize_present` has the same shape with a 2 s bound
///    (`RESIZE_SLICE_CAP_NS`) and closes on ANY successful present, not only a
///    content one — deliberately, since what ends a compositor rescale is a
///    frame at the new size, whatever drew it. Same caveat: idle inside the
///    interval is inside the number.
///
/// None of them is key→photon: all stop at application-present return, upstream
/// of compositor selection, scanout and display. The leg after that return is
/// published separately under `percentiles` as `present_glass`
/// (`presentDrawable:` registration → the drawable's `presentedTime`); it still
/// stops at the display's reported present time, not at photons.
///
/// FRAME-EXTRACTION ATTRIBUTION. `frame_refills_scoped` / `frame_refills_full`
/// split the presented-path refills between the damage-scoped arm and the full
/// O(rows x cols) walk, and `frame_refill_full_causes=<clause>:<frames>,...` (a
/// JSON object in the JSON form) names WHICH continuity clause refused on
/// each of the full ones. The split alone cannot: `host_mutation` (some
/// per-frame scratch mutator broke the chain — fixable),
/// `terminal_mismatch` (panes sharing one scratch — fixable), `base_y` (the
/// content is scrolling — correct, nothing to fix) and `scratch_unstamped` (the
/// window's first frame — unavoidable) all read as the same climbing
/// `frame_refills_full`. Non-zero clauses only, so a healthy instance prints
/// one or two pairs. `frame_refills_skipped` is the third arm: presented frames
/// that took NO extraction because the effect-only reuse gate proved the engine
/// had not moved since the snapshot was filled. It is what keeps a FALL in
/// `frame_refills_scoped` readable as work avoided rather than work gone
/// missing — `scoped + full + skipped` is still exactly one per presented
/// non-rescan frame.
///
/// SCHEDULER ATTRIBUTION. `past_deadline_arms` is the global spin witness;
/// `deadline_arms_by_owner=<owner>:<arms>/<past>,...` (a JSON object in the
/// JSON form) names WHICH producer armed them. `deadline_owner` alone is a
/// last-writer snapshot and cannot do that — it reported the 2026-08 spin as
/// `title_summary` when the producer was the session-status observer, now its
/// own `session_status` owner. Beside it, `past_arm_streak_heals=<owner>:<n>,...`
/// names every producer whose windowed past-arm streak (> 90% of its last 32
/// arms already past, at ANY lateness) the fold is actively clamping to frame
/// cadence — a live, named scheduler bug, not a suspicion.
///
/// CARD ENGINE COST. `proc_events` through `status_sweep_ms` are
/// [`CommandLedger`]'s: what the kernel exec edges cost the PTY readers and
/// the UI thread, read as counts per 1000 commands.
///
/// STARTUP: the startup span and the fields of everything before winit runs
/// are the startup ledger's ([`startup::StartupFacts::emit_span`],
/// [`startup::Startup::emit_boot`]), read once per reply and kept by
/// `metrics reset`.
pub(crate) fn emit_summary(
    ctx: &wire::ReadCtx,
    dims: Option<(u32, u32, u64)>,
    s: &mut dyn wire::FieldSink,
) {
    use wire::{ms, ms1};
    let w = &ctx.metrics.window;
    let live = &ctx.metrics.live;
    let frames = w.present.frames.get_acquire();
    // Read only after acquiring the publication counter (`Startup::facts`).
    let startup = if ctx.reads_process() {
        startup::STARTUP.facts()
    } else {
        startup::StartupFacts::default()
    };
    // TAIL SURFACING (audit item 10). The summary used to publish only `last_`
    // and `max_`, and during the 2026-08 spin `last_input_present_ms=8.1` read
    // perfectly healthy while the live p99 was 335 ms — a momentary reading and
    // an unbounded worst case cannot bracket a distribution between them. The
    // p95/p99 the `percentiles` verb already computed now ride the SUMMARY, so
    // a healthy median can never again mask a sick tail from a reader who did
    // not think to ask a second question.
    //
    // AND THE CHILD'S SLICE WITH THEM (2026-09-15). That rule was first applied
    // to `input` and `present` only — both of them ATERM's own — so the summary
    // could report a sick tail and still not say WHOSE. During a typing-lag
    // episode the owner read `max_present_latency_ms=560.54` and
    // `input_p99_ms=83.89` off this line and filed an aterm present regression;
    // `metrics percentiles` in the same minute held the answer, `n_echo=499
    // echo_p95_ms=75.69 echo_p99_ms=150.04 echo_max_ms=367.85` against aterm's
    // own `key_write_p99_ms=6.29` and `render_p99_ms=2.36` — the child waiting
    // for the scheduler, not aterm failing to paint. A second question nobody
    // asks mid-incident is not a published fact, so the echo fragment rides the
    // summary too.
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
    // THE CRASH-RESTORE JOURNAL'S RAM (2026-10-17): every page the regions of
    // this window's sessions have written, summed with no session's lock (each
    // region's `aterm_restore::Touched` keeps the running total), the cost the
    // memory shed weighs.
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
    // EVERY EFFECT STREAM, not only the cursor tick's three: read off the
    // final composed input at the present seam, so card light shows too.
    emit_fx_streams(ctx, s);
    // The damage-scoped refill's FULL-arm attribution, `clause:frames`, the
    // follow-up a78dd8a1 deferred when it wired DMG-1 into the shipping
    // frontend with only a scoped/full split: non-zero clauses only, so a
    // healthy instance prints its first-frame `scratch_unstamped:1` and
    // whatever its content is actually doing.
    s.count_pairs(
        "frame_refill_full_causes",
        &w.redraw.refill_cause.sparse(
            aterm_core::render::FullRefillCause::ALL
                .iter()
                .map(|cause| (cause.index(), cause.as_str())),
        ),
        "none",
    );
    // THE PIXELS THAT NEVER REACH GLASS. `record_offscreen_raster` exists
    // so an `image` / `window` / `snapshot` rasterization stops moving
    // `frames`, `max_frame_render_ms`, `slow_frames` and `max_frame_gap_ms`
    // — correct, and argued at length where it is defined. But the three
    // counters it moved that work INTO were read by nothing at all, so the
    // cure took the contamination and the observability together: a
    // full-frame raster of a 2208x1788 window changed no published number.
    // Published here, on the ledger that same comment tells a driver to
    // read.
    s.count("offscreen_rasters", w.present.offscreen_rasters.get());
    s.decimal("last_offscreen_raster_ms", ms(w.present.offscreen.last()));
    s.decimal("max_offscreen_raster_ms", ms(w.present.offscreen.max()));
    s.count("pre_present_attempts", w.redraw.pre_present_attempts.get());
    s.decimal("last_pre_present_ms", ms(w.redraw.pre_present.last()));
    s.decimal("pre_present_total_ms", ms(w.redraw.pre_present_total.get()));
    s.decimal("max_pre_present_ms", ms(w.redraw.pre_present.max()));
    // THE DRAWABLE PARK, AS A SCALAR. `acquire_p99_ms` (the `percentiles`
    // verb) is a statement about the bulk of thousands of ~0.02 ms samples;
    // a single 200 ms `nextDrawable` park cannot move it. These two were
    // recorded on every present and reset with the window since the slice
    // was instrumented, and nothing had ever read them — so the worst
    // acquire of a run was, until now, unpublishable.
    s.decimal("last_acquire_wait_ms", ms(w.redraw.acquire_wait.last()));
    s.decimal("max_acquire_wait_ms", ms(w.redraw.acquire_wait.max()));
    // THE OTHER MAIN-THREAD GPU BLOCK. The armed arm parks in
    // `waitUntilCompleted` twice per present (the Submit-A pipelining wait,
    // the pending-ring drain). The first used to be charged to
    // `frame_render` and the second to nothing at all; both are published
    // here so a contention stall is attributable at a glance.
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
    // The per-owner arm ledger, `owner:arms/past` (audit item 6).
    // `past_deadline_arms` is a single global number and `deadline_owner` a
    // last-writer snapshot, so the two together cannot name a spin's producer
    // — during the 2026-08 200 kHz spin they read "31,913 past arms,
    // owner=title_summary" while the producer was the session-status observer
    // folded under that label. Non-zero owners only, so a healthy instance
    // prints two or three pairs and a sick one puts its culprit in plain sight.
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
    // The timers that actually WOKE the loop, per owner: an arm is a fold
    // win, a fire is a wake.
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
    // The per-owner WINDOWED streak-heal ledger, `owner:heals`: the arms the
    // deadline healer's 32-arm past/future window detector clamped to frame
    // cadence (wake follow-ups items 18/19). Beside `deadline_arms_by_owner`
    // on purpose: that field says who kept arming the past, this one says
    // whose spin the fold is actively healing.
    s.count_pairs(
        "past_arm_streak_heals",
        &w.sched.streak_heals_by_owner.sparse(owner_labels()),
        "none",
    );
    s.count("stale_arm_heals", w.sched.stale_arm_heals.get());
    // WHEN, AND WHOSE (2026-09-15 attribution audit). `wake_late_ms` and
    // `deadline_late_ms` above are LAST-WRITER readings — the next wake
    // stores 0 over a late timer — and every `max_` on this line used to
    // carry no time of occurrence. This fragment publishes the worst wake
    // and deadline lateness with the owner and instant of each, the
    // instants of the present/input maxima, the frame gap that says whether
    // the present max was a stall or an idle stretch, and the process-clock
    // `metrics_now_ms` that anchors all of them.
    emit_lateness(ctx, s);
    s.decimal("max_frame_gap_ms", ms(w.present.frame_gap.get()));
    startup.emit_span(s);
    if ctx.reads_process() {
        // THE CHILD'S SLICE, ON THE SUMMARY LINE (2026-09-15 misattribution).
        // Every other field above measures ATERM. This one measures the program
        // on the far side of the PTY, and without it the summary cannot answer
        // the only question a lag report actually asks. Spliced as the WHOLE
        // fragment `echo_rtt` publishes, never a cherry-picked p99: its
        // percentiles may not be read apart from the sample/coalesce/expiry
        // ledger that qualifies them.
        crate::echo_rtt::emit_metrics(s);
        // THE PARK OUTSIDE THE REDRAW. Every slice above prices a redraw, a
        // wake's lateness or an end-to-end interval; none of them can name a
        // main thread parked in a NON-redraw handler, which is how
        // `max_present_latency_ms=560.54` came to sit beside a bounded
        // `max_redraw_total_ms=13.84` with no publishable producer. Spliced
        // whole, never as a lone max: the count and the long-turn tally are what
        // separate one hitch from a main thread that is late all the time.
        crate::watchdog::emit_turn_census(ctx, s);
    }
    // THE UPDATE'S PRE-COMMIT INPUT (gap #33): keystrokes the successor's
    // queue lost, and replays it had to repair — cumulative, so a real
    // drop is a number, not one more line beside a message on every update.
    emit_handoff_input(ctx, s);
    if ctx.reads_process() {
        // THE SUCCESSOR'S CLAIM TIMELINE (warm successor P0): on a boot that
        // claimed a handoff, each boundary of its boot in ms after the claim,
        // and the warm it did before its dial. `none` on every other launch.
        // Then BEFORE WINIT RUNS (CS-2): the kernel's birth → Rust main (the
        // loader's share, a Gatekeeper hold included), and GUI preparation in
        // ten exclusive phases that sum to `startup_gui_prepare_ms` — read
        // live, so a headless launch answers too.
        startup::STARTUP.emit_boot(&startup, s);
        // THE CONTROL LISTENER (2026-09-25 incident): accepts, the last
        // accept's age, the kernel queue it last read, rebinds of a wedged
        // listening socket, and the lane-admission refusals.
        crate::control_listener::emit_metrics(ctx, s);
    }
    // THE PTY CENSUS (2026-09-29): the masters the sessions' sinks hold
    // open beside the live sessions that own one — a lead that stays is a
    // closed tab's master nothing will close.
    emit_pty_census(ctx, s);
    if ctx.reads_process() {
        // THE WARM SPARE (`crate::spare`): its state and age, its PTY, the
        // learned gate, adoptions, misses, discards and budgets.
        crate::spare::emit_metrics(s);
    }
    // THE DEVICE'S OWN TIME and the latest frame's shape, which close
    // `metrics percentiles` too.
    emit_gpu(ctx, s);
}

/// `metrics percentiles` -> the latency DISTRIBUTIONS:
/// input→application-present-return, output→application-present-return,
/// frame-render, key→write, and pre-present compose histograms
/// as `n_*` + p50/p95/p99 ms fields, conservative by construction (bucket upper
/// edge), plus the occluded/parked/capture twin `present_tainted_*`. Same funnel
/// and honesty bounds as the scalars; zeroed by `metrics reset` like the maxima.
/// The input legs follow ([`emit_input_legs`]):
/// `out_req_*` is output → redraw request, the first half of `present_*` (the
/// burst's leading edge to the turn that admits its redraw, once per burst);
/// `ime_*` is an IME composition's preedit → commit, with `ime_cancelled` for
/// compositions that ended without one; `input_refused` counts input a full
/// input queue refused rather than park or queue without bound — keys and
/// pastes (said once per session in Messages), and mouse, wheel, focus and
/// colour-scheme reports (silently); `reply_dropped` counts terminal query replies a full reply lane
/// dropped (a program flooding queries whose answers it never reads). The GPU
/// fields close the line ([`emit_gpu`]), as they close the summary.
pub(crate) fn emit_percentiles(ctx: &wire::ReadCtx, s: &mut dyn wire::FieldSink) {
    use wire::ms;
    let w = &ctx.metrics.window;
    emit_quantiles(
        s,
        ["n_input", "input_p50_ms", "input_p95_ms", "input_p99_ms"],
        &w.input.input.copy_out(),
    );
    // Keystrokes a full per-window edge ring could not hold: their slices are
    // missing from `input_*`, and this says how many.
    s.count("input_edges_dropped", w.input.edges_dropped.get());
    // Of `n_input`, the keys a proven prediction showed before their echo:
    // their slices closed at its present (the PREDICTIONS note on
    // `InputLedger::input`).
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
    // THE OS-QUEUE SHARE OF `key_write`. The key-arrival stamp is backdated by
    // the NSEvent queue age, so `key_write_*` (and `input_*`) price a parked
    // event loop — and used to price it INSEPARABLY: an 18.13 ms max could be
    // 17 ms of queue wait or 17 ms of on-thread dispatch, which are opposite
    // fixes. Published beside the total it splits, never instead of it, with
    // the last and the max: one key that waited 17 ms behind a parked loop
    // cannot move a p99 taken over thousands of promptly dispatched keys.
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
    // The drawable-park slice. Until this existed the largest known macOS
    // typing stall could only be estimated as `redraw_total - compose -
    // render`, mixed with the post-present tail — so a regression in it moved
    // no published number.
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
    // …and the two scalars the distribution cannot express. A p99 over
    // thousands of ~0.02 ms acquires absorbs a single 200 ms park; the max is
    // the only field that reports it. Published HERE beside the percentiles it
    // qualifies (and on the summary line, which is the read most drivers
    // actually take).
    s.decimal("last_acquire_wait_ms", ms(w.redraw.acquire_wait.last()));
    s.decimal("max_acquire_wait_ms", ms(w.redraw.acquire_wait.max()));
    // THE LEG IN FRONT OF THE PARK. `acquire_*` starts its clock inside the
    // drawable worker, so it reports what `nextDrawable` cost and nothing of
    // the wait to be scheduled to call it — a wait the frame spends parked on
    // `AcquirePending`. Its own slice, so a worker starved behind other
    // runnable threads is attributable instead of landing in `input_present`
    // with no cause.
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
    // The live-drag STALE-FRAME window: bounds change → first frame submitted
    // at the new size. For that interval the compositor has new bounds and an
    // old drawable, so it shows the previous frame rescaled — the smear a drag
    // reads as "shredded" text. The `video` tap cannot measure it (its ring is
    // one geometry and it early-stops on the first resize), so this is the
    // only millisecond-resolution read of it.
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
    // Its twin: bounds change -> the engine committing the new GRID. The text
    // trails the window edge for exactly this long, and a width drag keeps it
    // deliberately long (the throttle bounding scrollback rewrap).
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
    // The occluded/parked/capture twin of `present`. It is published rather
    // than dropped so an occluded or RECORDED run stays observable — and so
    // `n_present + n_present_tainted` still accounts for every content
    // present, which is what makes the split auditable instead of a silent
    // filter. See `PresentLedger::tainted`.
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
        // THE MAIN-LOOP TURN DISTRIBUTION PER ROOT (responsiveness audit item 3):
        // the census on the summary line keeps last/max/count only.
        crate::watchdog::emit_turns_by_root(s);
        // …and their sum per second of the window: the main thread's occupancy,
        // which no percentile of one turn can show.
        s.decimal(
            "main_busy_ms_per_s",
            wire::ms_f64(crate::watchdog::main_busy_ms_per_s(ctx.now_ns)),
        );
    }
    // UI-THREAD TERMINAL-MUTEX WAIT, per acquiring site (the redraw's one
    // frame hold, the key press): the span the typing audit listed as
    // unmeasured.
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
        // ECHO ROUND TRIP (audit item 5): the one slice on this line that is NOT
        // aterm's own cost — bytes out to the PTY, first bytes back from the
        // child. Emitted by `echo_rtt` itself so its percentiles can never be
        // published apart from the counters that qualify them.
        crate::echo_rtt::emit_metrics(s);
    }
    // PRESENT → GLASS: the compositor leg after present-return that every
    // slice above stops short of.
    emit_present_glass(ctx, s);
    // THE INPUT LEGS: output → redraw request (the first half of
    // `present_*`), the IME preedit → commit clock, and the inputs a full
    // input queue refused.
    emit_input_legs(ctx, s);
    // THE DEVICE'S OWN TIME: every slice above is CPU wall time; this is the
    // GPU's execution of each present, with the CPU encode beside it.
    emit_gpu(ctx, s);
}

/// `n` and the p50/p95/p99 of one distribution, under the four keys given in
/// that order, the quantiles in ms at the bucket upper edge (0.00 until a
/// sample lands: check `n`).
fn emit_quantiles(s: &mut dyn wire::FieldSink, keys: [&str; 4], h: &HistCopy) {
    emit_quantiles_in(s, keys, h, wire::ms);
}

/// [`emit_quantiles`] to three places: the sub-millisecond main-thread costs.
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

/// What a reply reads from `m` alone at `now_ns` ([`wire::ReadCtx::inert`]),
/// as text: the summary with a grid, then the percentiles (tests).
#[cfg(test)]
fn inert_text(m: &Metrics, now_ns: u64) -> String {
    let ctx = wire::ReadCtx::inert(m, now_ns);
    let mut text = wire::render_text(|s| emit_summary(&ctx, Some((24, 80, 7)), s));
    text.push_str(&wire::render_text(|s| emit_percentiles(&ctx, s)));
    text
}

/// The value of the first `key=` token in a text body (tests).
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

    /// ITEM 6. The failure this prevents: `past_deadline_arms` was one global
    /// number, so a spin could only ever be reported against `deadline_owner`,
    /// a last-writer snapshot. Two owners arming in the same window must now
    /// come back separated, past arms included. The healer's verdicts are
    /// booked against the owner they name, and the lateness describes the
    /// REQUESTED deadline even when the healer armed a clamp.
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

        // One healthy arm from Blink, two PAST arms from SessionStatus. Under
        // the old single counter this window read "2 past arms, owner=blink".
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
        // The second past arm was clamped by both detectors' verdicts.
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

        // The published fields name the owners, in table order, and never
        // invent one: silent owners are omitted, not zero-padded.
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

    /// Only drops that stop the present stream taint output→present; a transient retry
    /// does not, or the histogram would lose the stalls it exists to catch.
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

    /// The taint window, pure so it cannot race the process globals.
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

    /// A capture episode must open and close exactly once however the recording
    /// ends, and an UNPAIRED close must not wrap the depth to `u64::MAX` and
    /// taint every present for the rest of the process.
    #[test]
    fn capture_episodes_nest_and_never_underflow() {
        let m = Metrics::new();
        let active = |m: &Metrics| m.live.capture_depth.load(Ordering::Relaxed) != 0;
        assert!(!active(&m), "no capture is running");
        m.note_capture_episode(true);
        assert!(active(&m));
        m.note_capture_episode(true); // overlapping capture
        m.note_capture_episode(false);
        assert!(
            active(&m),
            "one close does not end two overlapping captures"
        );
        m.note_capture_episode(false);
        assert!(!active(&m), "the last close ends the episode");
        m.note_capture_episode(false); // unpaired: a double finalize
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

    /// Every ablation mode reads back as itself, and a byte the encoder never
    /// writes reads as `unknown`, never as the shipped plan.
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

    /// The compositor leg is published beside the application-side slices: an
    /// on-glass sample lands in the distribution, the max and its own tag's
    /// shown ledger; a skipped drawable only in the skip counter and the ledger
    /// of the lane that REPLACED it. Empty ledgers print `none` / `{}`, never a
    /// bare `key=`.
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
        // A frame replaced by a hot-burst successor: charged to `hot_burst`.
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

    /// The device leg is published beside the CPU slices: a device sample
    /// lands in `gpu_time` with its part count, a GPU present's encode and its
    /// frame's shape in theirs, and `gpu_time_source` says whether the
    /// distribution holds device samples or why it holds none. Before any
    /// sample, and again after a reset, every field reads empty, the
    /// instances `none` (`{}` in JSON) and the ablation `off`.
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
        assert_eq!(object["gpu_time_source"].as_str(), Some(empty_source));
        assert_eq!(object["fx_instances_last"].as_u64(), Some(0));
        assert_eq!(object["fx_upload_bytes_last"].as_u64(), Some(0));
        assert_eq!(object["fx_refused_last"].as_u64(), Some(0));
    }

    /// The fx engine's seal and deep-copy counters read since the last reset,
    /// in both forms: a seal before the reset is not counted after it, and a
    /// seal and a copy after it are. aterm-core's totals are process-wide, so
    /// another test's seals may add to a read: every assertion on a process
    /// reply is a bound that holds whatever they do. An inert reply samples
    /// nothing and reads back what the ledger holds.
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

    /// A sample a concurrent reply loaded before a reset's own, and stores
    /// between that reset's sample and its rebase, never moves the base: the
    /// reset's sample is a total no process reaches, the stale reply stores the
    /// process's lower totals, the rebase follows, and one seal and one copy
    /// later each counter reads 1. NEGATIVE CONTROL: the same interleaving
    /// through a plain store lets the stale sample lower the total, and every
    /// seal between the two samples counts after the reset.
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

    /// A key typed into window A is not closed by window B's content present; A's echo
    /// books the whole wait.
    #[test]
    fn another_windows_present_cannot_close_this_windows_key() {
        let mut typed_into = PendingInputStamp::default();
        let mut streaming = PendingInputStamp::default();
        typed_into.arm(1_000, 1_000);

        // Window B presents its own streaming output 3 ms later.
        assert_eq!(streaming.drain_slices(4_000_000), Vec::<u64>::new());
        assert!(
            typed_into.is_pending(),
            "window B's frame closed window A's keystroke: the slice would read LOW"
        );

        // Window A's real echo, 90 ms after the key.
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

    /// Coordinated omission: N keystrokes before one slow present are N samples, each its
    /// own wait.
    #[test]
    fn a_burst_before_one_present_books_every_edge() {
        let mut pending = PendingInputStamp::default();
        for arrival in [10, 20, 30, 40] {
            pending.arm(arrival, arrival);
        }
        assert_eq!(pending.drain_slices(110), vec![100, 90, 80, 70]);
        assert!(!pending.is_pending());
    }

    /// A burst past the ring keeps its OLDEST edges (the longest waits) and
    /// refuses the rest, answering `false` for each: the answer
    /// `note_window_input` counts in `input_edges_dropped`, so the loss is
    /// published rather than silent.
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

    /// The ring counts nothing itself: `note_window_input` adds one to
    /// `input_edges_dropped` for each key the ring refuses. A burst three past
    /// a ring of live edges is three. The control: the same full ring, aged
    /// past the discard cap, takes the next key, and the count holds.
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

    /// A ring filled by keys that never echoed (a password prompt, an app
    /// ignoring input) does not refuse the next fresh key: the edges already
    /// past the discard cap — which the drain would throw away — give up their
    /// slots first, so the key that actually produces the present is the one
    /// counted, not the one in `input_edges_dropped`.
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
        // The control: a full ring of edges inside the cap keeps them all and refuses the
        // newcomer.
        for arrival in 1..=super::INPUT_EDGE_RING as u64 {
            assert!(pending.arm(arrival, arrival));
        }
        assert!(!pending.arm(1_000, 1_000), "live edges are never evicted");
    }

    /// The drain classes each edge by when it was ARMED against the reset
    /// epoch; that a present books only this window's edges is pinned through
    /// `record_present_at`.
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

        // Armed at the reset instant or later is this window's, even if the
        // backdated arrival precedes it (the key sat in the OS queue across
        // the reset).
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

    /// One present into `window`, booked into `m` at a given clock, untainted.
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

    /// THE WITNESS IS THE PRESENTING WINDOW'S OWN GAP. Window B streams a
    /// frame every 16 ms while window A, stalled, presents once after 500 ms.
    /// A's worst-latency witness must read A's 500 ms; one process-wide stamp
    /// read B's last frame and hid the stall. `max_frame_gap_ms` stays
    /// process-wide, since idle windows stop presenting by design and a
    /// per-window frame gap would book a background window's whole idle span
    /// into a foreground cadence read: while B presents, it stays at 16 ms.
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

        // The control: both windows through ONE stamp, as the global was.
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

    /// A present stamped before the reset epoch belongs to the previous
    /// measurement window, so the first present after a reset books no gap,
    /// per window or process-wide, and stamps afresh for the next one.
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

        // The control: the same presents with no reset between them.
        let mut window = WindowPresentState::default();
        let mut observer = PresentObserver::default();
        let _ = present(&m, t, 0, 0, &mut window, &mut observer);
        let unreset = present(&m, t + 10 * MS, 0, 0, &mut window, &mut observer);
        assert_eq!(
            (unreset.window_gap_ns, unreset.frame_gap_ns),
            (Some(10 * MS), Some(10 * MS))
        );

        // `reset` writes the epoch the next present reads, and nothing else
        // of the present-side state.
        let before = now_ns();
        m.reset();
        assert!(PresentClock::read(&m, false).epoch >= before);
    }

    /// ONE TAINT DECISION PER PRESENT. The output ledger, the input slices and
    /// both gaps follow the one `PresentClock`: at `taint_until - 1` the output
    /// slice goes to the tainted ledger; at `taint_until` it is on glass.
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

    /// A PRESENT A CAPTURE FORCED IS A `capture_presents` SAMPLE. Its output
    /// slice goes to the tainted ledger with no episode and no tail;
    /// `capture_presents` counts it and stays at most `present_tainted`.
    /// Control: the same present unforced books on glass and leaves
    /// `capture_presents` alone.
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

    /// A KEY TYPED BEFORE A RESET IS DRAINED, NOT BOOKED. A `metrics reset`
    /// retires the key's wait from the input ledger, whose measurement window
    /// it opens: the present drains every key and books only those armed
    /// after the epoch.
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
        // The control: with no reset behind them, both keys book.
        assert_eq!(present_keys(&[before, after], 0), 2);
        // The negative control: a key that never echoed within the cap books
        // nothing, before the reset or not.
        let aged = echo - INPUT_SLICE_CAP_NS - MS;
        assert_eq!(present_keys(&[aged], reset_at), 0);
        assert_eq!(present_keys(&[aged], 0), 0);
    }

    /// A KEY'S PREDICTION ON GLASS CLOSES ITS SLICE, as a native page's answer
    /// does: the person sees the key there, before its echo (measured live,
    /// `aterm-lab predict-live`, 2026-10-20: every key's slice waited for its
    /// echo's present, 109 ms over a 100 ms link, while its glyph was on glass
    /// about a millisecond after the press). A present with no output that
    /// painted the prediction through op 7 books, oldest first, the keys tagged
    /// with ops up to 7, and stops at the first key it does not show (one that
    /// armed no op, or a dark op after it): those wait for the echo's content
    /// present, as before. The controls: a present with no output that painted
    /// no prediction books nothing (a blink repaint), and the mark is that
    /// one frame's.
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

    /// A key the full ring dropped is tagged with nothing: its op names no
    /// other key's edge. Every key the ring holds but the newest is tagged
    /// with an op the frame shows; the newest armed none (`tag_newest(None)`,
    /// as an Enter does). The dropped key, pressed next, armed an op the frame
    /// also shows: written onto the newest edge, it would book that key too.
    /// So the frame books every key but the newest, and the newest waits for
    /// its echo (the review of P7, 2026-10-20: an earlier version of this test
    /// left every edge untagged, and passed with the guard removed).
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

    /// The healer's verdict for an arm it left alone.
    fn passed(deadline: Option<Instant>) -> Heal {
        Heal {
            armed: deadline,
            floor: false,
            streak: false,
        }
    }

    /// `note_deadline` stamps `due = now - late` on the PROCESS clock, and a
    /// saturated `due` of 0 is read as "no deadline armed". Wait that clock past
    /// the lateness this test fabricates rather than racing it — a no-op once a
    /// test binary is warm, and a sleep rather than a spin so it costs no CPU.
    fn wait_out_the_process_clock() {
        while now_ns() < LATE_NS + 50_000_000 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// RESPONSIVENESS AUDIT ITEMS 2 AND 5 (2026-09-21). A timer that WOKE the
    /// loop is counted for its owner apart from the fold wins
    /// (`deadline_fires_by_owner` beside `deadline_arms_by_owner`), and the
    /// GPU's repaint-scope counters are window stats that `reset` rebases.
    /// Before, neither was readable from a live instance. The wakes by `Wake`
    /// variant are `metrics::wakes`' own tests, and item 3's turns by root are
    /// the watchdog census's.
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
        // A WaitCancelled interrupting the same deadline is not a fire.
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

    /// A 400 ms late timer wake survives the next reader wake's store of 0 as a named,
    /// timed max, while `wake_late_ms` keeps its last-writer meaning.
    #[test]
    fn a_late_timer_survives_the_reader_wake_that_erases_the_last_reading() {
        wait_out_the_process_clock();
        let m = Metrics::new();
        let sched = &m.window.sched;
        let last_wake_late = |m: &Metrics| m.live.last_wake_late.load(Ordering::Relaxed);
        let now = Instant::now();
        let stale = now - Duration::from_nanos(LATE_NS);
        // One offence from one owner: no heal clamps it (that needs a streak),
        // so the arm passes through and books its lateness as-asked.
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

        // The erasing wake: one output burst is all it takes.
        m.note_event_wake(EventWakeKind::WaitCancelled);
        assert_eq!(
            last_wake_late(&m),
            0,
            "`wake_late_ms` is still the LAST reading — that contract is unchanged"
        );

        // …and the worst case outlives it, named and timed.
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

        // And it is PUBLISHED — in both forms, with the owner spelled.
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

    /// The worst output→present reading is an OPEN interval, so "560 ms" alone
    /// cannot say whether the main thread stalled or simply had nothing to
    /// draw. The frame that sets the max therefore also records WHEN it
    /// happened and how long it had been since its window's previous present:
    /// a gap the size of the reading is an idle stretch, a far smaller one is
    /// output that waited while frames kept coming.
    ///
    /// The present is driven at a given clock, so the instant and the gap are
    /// exact: no bracket around a live clock read, no poked global stamp.
    #[test]
    fn the_worst_present_latency_records_its_instant_and_its_frame_gap() {
        let m = Metrics::new();
        let present = &m.window.present;
        // A present 300 ms after the window's previous one, closing a 4 s
        // slice: a stall that is NOT explained by an idle stretch.
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

        // A smaller sample cannot overwrite the winner's stamps.
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

    /// The breadcrumb reaches `aterm.log` in a shipped binary: an ordinary slow frame
    /// writes nothing, a stall costs one line when it starts and one when it ends, and a
    /// loop waiting only on the session-status observer writes nothing.
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

        // A slow present every 10 s for 27 hours: the opening line above and the closing
        // line below are the only two.
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

        // A slow present after a quiet gap, with the old episode still open
        // (no present in between closed it), closes it and opens another.
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

    /// `metrics reset` runs on a control thread, and the open episode is the UI
    /// thread's (`PresentObserver`), so a reset only moves the epoch: the next
    /// present closes an episode whose last slow present came before it (with
    /// its line), and a slow present after the reset opens a fresh episode
    /// rather than joining the old.
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

        // Any present reads the epoch, a blink repaint (latency 0) included.
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

        // The control: with no reset between them, the same slow present joins.
        let mut unreset = open();
        unreset.note_slow_present(&m, slow, 0, at + 4, owner);
        assert_eq!((unreset.episode.samples, unreset.episode.start_ns), (3, at));
    }

    /// THE CONFLATION THIS FIXES. `note_key_arrival_queued` backdates the arrival
    /// stamp by the NSEvent queue age and used to keep ONLY the backdated stamp,
    /// so the queue residence went into `key_write` and `input_present` and was
    /// published nowhere apart. A `max_key_write_ms=18.13` therefore could not be
    /// told from a parked event loop (fix: present pacing) or 18 ms spent inside
    /// `on_key`/`input_to_session` (fix: the press path).
    ///
    /// Drives the real recorder and pins what the fix adds: the backdate lands in
    /// a reading of its own, a `0` is a SAMPLE rather than a silence, a key that
    /// never writes still books its wait (which is why `n_key_queue` may exceed
    /// `n_key_write`), and the whole leg clears with the measurement window.
    /// That `key_write` CONTAINS this leg is structural — both clocks start at
    /// the same backdated stamp.
    #[test]
    fn the_os_queue_share_of_key_write_is_published_apart_from_it() {
        let m = Metrics::new();
        let queue = &m.window.input.key_queue;

        // A key that waited 17 ms behind a parked event loop: the arrival is
        // backdated by the wait, and the wait is a reading of its own.
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

        // A key that waited NOTHING books a real `0`: the max keeps the worst key,
        // `last` reports this one, and the count moves — so "no queueing" can
        // never be read as "not measured", which is also what an unmeasurable age
        // (any non-macOS build) books.
        m.note_key_arrival_queued(0);
        assert_eq!(
            (queue.last(), queue.max()),
            (0, parked_ns),
            "a 0 is this key's reading; the worst key still owns the max"
        );
        assert_eq!(queue.copy_out().n(), 2);

        // A key that never writes (a UI shortcut) still books its queue wait, which
        // is why `n_key_queue >= n_key_write` rather than equal.
        m.note_key_arrival_queued(parked_ns);
        assert_eq!(queue.copy_out().n(), 3);
        assert_eq!((queue.last(), queue.max()), (parked_ns, parked_ns));
        assert_eq!(m.window.input.key_write.copy_out().n(), 0);

        // A window stat, cleared by `reset` like the total it qualifies.
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

    /// The observations under a root whose poke moves no byte of what `render`
    /// publishes from that root: each one is written, reset, and read by
    /// nothing. `fresh` builds an empty root and `visit` walks its cells (a
    /// composite instrument's parts apart, `path#part`); each observation is
    /// poked alone into a root of its own.
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

    /// EVERY OBSERVATION REACHES A SURFACE. The class "written on every
    /// sample, cleared by reset, published nowhere" shipped twice (the acquire
    /// wait's last and max, then the offscreen raster ledger), and nothing
    /// failed either time. Derived from the declarations, not from a list:
    /// every observation `Window::visit` yields, poked alone into an empty
    /// root, must move the summary or the percentiles read from that root
    /// alone. A composite is checked part by part, since one sample moves
    /// every part and would hide an unpublished one behind the rest: the
    /// acquire wait's percentiles alone would have passed for its last and
    /// max. The controls: a ledger with one instrument its renderer never
    /// prints fails the same check, and only that instrument does; a
    /// distribution published only as percentiles fails it for its last and
    /// max, and only those.
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

    /// RESET IS GENERATED, AND LIVE STATE HAS NONE. Every cell the window
    /// holds, stamps and each part of a composite included, reads non-empty
    /// once poked and empty again after `Metrics::reset`, and the epoch moves
    /// to the reset. The control is the live half: the backend, the shed
    /// latch, a live capture and its taint, the momentary effect readings, the
    /// last wake and deadline, a pending key, the handoff input counts and the
    /// PTY sessions, all set before the reset, survive it.
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
