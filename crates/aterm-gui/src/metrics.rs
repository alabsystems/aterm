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
//!   the present→present gap of the frame that set the present max: a gap the
//!   size of the reading means nothing presented for that long (an open
//!   interval), a far smaller one means frames kept coming while that output
//!   waited.
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
//!
//! A driver detects lag without OS profilers: `metrics reset`, drive the workload,
//! then `metrics` — if `slow_frames > 0`, or `max_frame_render_ms` is large, or
//! `backend=cpu` under heavy output, the terminal is lagging.
//!
//! ## Two rules this file keeps
//!
//! **An instrument that perturbs the system under test proves nothing.** A
//! counter here must describe ATERM, not the act of measuring it. Where the two
//! cannot be separated, the contaminated samples get their OWN ledger and both
//! are published — never a silent filter, never a poisoned average. See
//! [`record_offscreen_raster`] (a screenshot must not move the on-glass frame
//! ledger) and the `PRESENT_TAINT_UNTIL_NS` block (an occluded window, or a
//! `video` capture pacing presents, must not move the output→glass histogram).
//!
//! **A number that cannot name its producer cannot end an investigation.** A
//! last-writer label over a min-fold of ~35 candidates points at whichever one
//! happened to win; that is how the 2026-08 event-loop spin was reported against
//! `title_summary` for its first hour. `DEADLINE_ARMS_BY_OWNER` books arms — and
//! past arms — per owner instead, and `DEADLINE_FIRES_BY_OWNER` the timers that
//! actually woke the loop (`deadline_fires_by_owner`: an arm is a fold win, not a
//! wake). `wakes_by_variant` counts the delivered proxy events by `Wake` variant,
//! `metrics percentiles` carries each main-loop root's turn distribution
//! (`turns_by_root`), and `gpu_scissor_taken` / `gpu_full_repaints` /
//! `gpu_scroll_rescues` publish the renderer's repaint scope (2026-09-25,
//! responsiveness audit items 2, 3 and 5).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static FRAMES_PRESENTED: AtomicU64 = AtomicU64::new(0);
static LAST_PRESENT_LATENCY_NS: AtomicU64 = AtomicU64::new(0);
static LAST_FRAME_RENDER_NS: AtomicU64 = AtomicU64::new(0);
// OFFSCREEN rasterizations (introspection `image`/`window`/`snapshot`): counted
// separately from real presents so a screenshot can never move the on-glass
// ledger. See `record_offscreen_raster`.
// Swapchain-acquire wait: the drawable-park slice. See `note_acquire_wait`.
static LAST_ACQUIRE_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_ACQUIRE_WAIT_NS: AtomicU64 = AtomicU64::new(0);
// Drawable-worker QUEUE: request admitted -> worker scheduled to start it.
// See `note_acquire_queue`.
static LAST_ACQUIRE_QUEUE_NS: AtomicU64 = AtomicU64::new(0);
static MAX_ACQUIRE_QUEUE_NS: AtomicU64 = AtomicU64::new(0);
// Armed-Metal GPU park: the UI thread blocked in `waitUntilCompleted` — the
// Submit-A pipelining wait plus the pending-ring drain. See `note_gpu_park`.
static LAST_GPU_PARK_NS: AtomicU64 = AtomicU64::new(0);
static MAX_GPU_PARK_NS: AtomicU64 = AtomicU64::new(0);
static OFFSCREEN_RASTERS: AtomicU64 = AtomicU64::new(0);
static LAST_OFFSCREEN_RASTER_NS: AtomicU64 = AtomicU64::new(0);
static MAX_OFFSCREEN_RASTER_NS: AtomicU64 = AtomicU64::new(0);
static MAX_PRESENT_LATENCY_NS: AtomicU64 = AtomicU64::new(0);
static MAX_FRAME_RENDER_NS: AtomicU64 = AtomicU64::new(0);
static SLOW_FRAMES: AtomicU64 = AtomicU64::new(0);
static BACKEND_GPU: AtomicBool = AtomicBool::new(false);

// ---- PRESENT-LATENCY HONESTY: the two contaminating EPISODES --------------
//
// THE OBSERVED DEFECT (responsiveness audit, item 10). A live read showed
// `present_p50=1.31ms` next to `present_p95=671ms / p99=1006ms`, with
// `n_present=5663` against `frames=17706`. A p95 five hundred times the median
// is not a tail of the same distribution — it is a SECOND distribution mixed
// into the first, and it made the published output→glass figure unusable in
// exactly the direction that hides regressions: nobody trusts a number whose
// tail is already absurd.
//
// WHERE THE SECOND DISTRIBUTION COMES FROM. `present_latency_ns` measures
// PTY-output leading edge → the present that showed it. That is honest only
// while the window is actually presenting. Two episodes break that:
//
//   1. OCCLUSION / PARK. A `GpuOccluded` drop, or any drop the retry scheduler
//      parked awaiting an external surface stimulus, means frames stopped
//      reaching glass for an interval nobody was watching. Output keeps arriving
//      and keeps stamping. The first present after the episode books the whole
//      unwatched interval as render latency — under the 5 s cap, so the existing
//      honesty bound passes it straight through.
//   2. CAPTURE. A `video` recording paces presents on the RECORDER's schedule,
//      not the output's, and (the 2026-08 blackout's root cause) pins gates the
//      unrecorded path does not have. A capture-based instrument that moves the
//      number it is reading proves nothing — see docs/RELEASE-PROOF-DISCIPLINE.md.
//
// THE FIX FOLLOWS `record_offscreen_raster` EXACTLY: do not discard the sample,
// and do not let it touch the on-glass ledger. A sample taken inside an episode
// goes to its OWN histogram and its own last/max, so introspection and occluded
// runs stay observable while `present_p95` means what it says. Both ledgers are
// published, so the split is auditable rather than a silent filter.
static PRESENT_TAINT_UNTIL_NS: AtomicU64 = AtomicU64::new(0);
static CAPTURE_DEPTH: AtomicU64 = AtomicU64::new(0);
static CAPTURE_EPISODES: AtomicU64 = AtomicU64::new(0);
static TAINTED_PRESENT_SAMPLES: AtomicU64 = AtomicU64::new(0);
static LAST_TAINTED_PRESENT_LATENCY_NS: AtomicU64 = AtomicU64::new(0);
static MAX_TAINTED_PRESENT_LATENCY_NS: AtomicU64 = AtomicU64::new(0);

/// How long after an episode ENDS a present-latency sample is still suspect.
///
/// The stamp being consumed is per SESSION, and several sessions can hold
/// stamps armed while the window was off glass; the first present drains the
/// visible set, but the drop→resume seam is not instantaneous and a park can
/// re-arm between two of them. One second bounds that drain generously while
/// staying far below the shortest interval anyone calls a stall, and — unlike a
/// latch consumed by the next present — it cannot be defeated by a blink
/// repaint arriving first. Every excluded sample is counted, so an over-broad
/// tail is visible as `present_tainted` rather than as a missing measurement.
const PRESENT_TAINT_TAIL_NS: u64 = 1_000_000_000;

// SYNC-1 (DEC-2026 frame-hold) observability. A pathological arm/timeout-release
// loop pins presents to ~1/timeout (the invisible ~5 fps failure class of
// 2026-07-05): `SYNC_RELEASES_TIMEOUT` climbing during ordinary typing IS that
// bug's fingerprint — a healthy interactive shell releases every episode via
// `?2026l` (`SYNC_RELEASES_END`) and times out ~never.
static SYNC_HOLDS_ARMED: AtomicU64 = AtomicU64::new(0);
static SYNC_RELEASES_END: AtomicU64 = AtomicU64::new(0);
static SYNC_RELEASES_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static SYNC_HOLDING_WINDOWS: AtomicU64 = AtomicU64::new(0);

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
// closes on the next content present OF THE WINDOW THE KEY WAS TYPED INTO.
//
// PER-WINDOW ATTRIBUTION. A keystroke routed to a window (every key that reaches
// `App::input_to_session`, hardware or controller) arms that window's
// [`PendingInputStamp`], and only that window's content present closes it. This
// used to be one process-wide stamp closed by ANY window's content present: with
// the owner typing into window 0 while a streaming session presented in window 1,
// window 1's log-line frame closed the key's slice at a few ms and window 0's real
// echo, presented ~90 ms later, found no stamp and recorded nothing — `input_p99`
// under-reported precisely during the lag episode. The distributions and scalars
// below stay process-global: they are a max-fold over every window's attributed
// slices.
//
// `INPUT_STAMP_NS` survives only for input that names a SESSION, not a window (a
// control `send`/`feed` to a background session, a controller host write): which
// window presents that session is not known at the stamp, so any window's content
// present closes it and, under concurrent output elsewhere, it can still read LOW,
// never high. Within one window, concurrent streaming output in the SAME window
// can still close the slice on a log-line frame rather than the echo (the same
// low-never-high bound); the cross-window case is the one this split removes.
static INPUT_STAMP_NS: AtomicU64 = AtomicU64::new(0);
/// `now_ns` at the last [`reset`]. A window's [`PendingInputStamp`] lives in its
/// `WindowState`, not here, so `reset` cannot clear it; a stamp ARMED before this
/// instant is discarded at consume time instead, so a pre-reset key never books
/// its wait into the fresh measurement window.
static INPUT_RESET_AT_NS: AtomicU64 = AtomicU64::new(0);
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
pub(crate) const TYPING_HOT_TAIL_NS: u64 = 250_000_000;

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
// The damage-scoped frame-extraction reach (DMG-1): how many presented-path
// refills rode the scoped arm vs fell back to the full O(rows×cols) walk. A
// full-arm-dominated steady state means some per-frame scratch mutator broke
// the continuity chain — the number that says whether the 2.35×/2.65× win is
// actually reaching this machine's frames.
static FRAME_REFILLS_SCOPED: AtomicU64 = AtomicU64::new(0);
static FRAME_REFILLS_FULL: AtomicU64 = AtomicU64::new(0);
// …and the presented frames that took NO extraction at all because the engine
// had not moved since the snapshot was filled (the effect-only reuse gate). This
// counter is what keeps the gate honest: `scoped + full + skipped` still
// accounts for every presented non-rescan frame, so a fall in `scoped` can be
// read as work AVOIDED rather than work gone missing.
static FRAME_REFILLS_SKIPPED: AtomicU64 = AtomicU64::new(0);
// THE RESIDENT PET'S WORLD WALK, the one unconditional per-frame O(window)
// classification in the effects lane: min(rows,64) x min(cols,256) cells
// reclassified on every presented frame the rainbow-kitty pet style owns,
// whether or not anything on that screen moved. Two counters rather than one
// because the cost is a PRODUCT: `pet_world_observations` is how often the
// walk ran and `pet_world_cells` is how wide it was, so a change can be read
// as "fewer walks" or "smaller walks" instead of one number that hides which.
// Until these existed the walk was measurable only by a criterion bench and by
// an A/B of two live instances — the two instruments that disagreed by 4x in
// the 2026-09-24 audit, with no way to ask a RUNNING aterm which was right.
static PET_WORLD_OBSERVATIONS: AtomicU64 = AtomicU64::new(0);
static PET_WORLD_CELLS: AtomicU64 = AtomicU64::new(0);
// THE COMPOSE-SIDE PRIM COUNTERS (docs/RELEASE-PROOF-DISCIPLINE.md, "still
// owed" item 6): what the cursor-effect tick handed the frame, read off a live
// instance without a pixel capture. Every tick — the live single-pane and
// composed presents and the explicit capture — goes through
// `App::tick_cursor_fx`, so these see all three. The three `_last` readings
// are momentary, like every `last_*`: the additive `glow_add` quads (every
// style's halo, the momentum glow on ANY style, the rod, the bodies), the
// `glow_under` quads, and the comet's trail cells. `fx_under_frames` (reset
// with the window stats) counts ticks whose `glow_under` stream was non-empty:
// that stream is the rainbow ribbon body's own (with its jump zooms, and
// water's wake), so it reads 0 under the `off` style, whose momentum glow
// still lights `glow_add` — the witness a count of ANY quad could not be.
static FX_GLOW_QUADS_LAST: AtomicU64 = AtomicU64::new(0);
static FX_UNDER_QUADS_LAST: AtomicU64 = AtomicU64::new(0);
static FX_TRAIL_CELLS_LAST: AtomicU64 = AtomicU64::new(0);
static FX_UNDER_FRAMES: AtomicU64 = AtomicU64::new(0);
// …and WHICH continuity clause refused, per clause. `frame_refills_full` says
// the chain broke; this says what broke it, which is the difference between an
// actionable number and a worrying one — a host mutator that forgot its
// `snapshot_seq` bump, a scratch shared across panes, a window whose row count
// disagrees with its engine, and a workload that is simply scrolling all read
// as the same climbing counter and have four different answers (the last one
// being "nothing, that is correct"). Indexed by `FullRefillCause::index()`; one
// relaxed `fetch_add` on the arm that was already paying for a full re-extract.
static FRAME_REFILL_FULL_BY_CAUSE: [AtomicU64; aterm_core::render::FullRefillCause::COUNT] =
    [const { AtomicU64::new(0) }; aterm_core::render::FullRefillCause::COUNT];
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

// LATENESS THAT SURVIVES THE NEXT WAKE, AND A MAX THAT SAYS WHEN (2026-09-15
// attribution audit). The two lateness facts above are LAST-WRITER ONLY: a
// `Timer` wake stores `now - due` into `LAST_WAKE_LATE_NS` and the very next
// reader wake — a `WaitCancelled`, which arrives at PTY-output rate — stores 0
// over it; `LAST_DEADLINE_LATE_NS` is a plain store. So a 400 ms late timer,
// the exact shape a main thread starved by a compile produces, is erased
// milliseconds after it happens and is long gone by the time anyone runs
// `aterm ctl metrics`.
//
// THE READING THAT COULD NOT BE ATTRIBUTED. A live line carried
// `max_present_latency_ms=560.54` beside `wake_late_ms=0.00
// deadline_late_ms=0.00`, with every slice INSIDE the redraw bounded
// (`max_redraw_total_ms=13.84`, `max_pre_present_ms=7.32`,
// `max_acquire_wait_ms=15.32`). Nothing published could say whether those
// 560 ms were a late wake, a late deadline, or simply an OPEN INTERVAL in
// which nothing needed to present — the reading `present_latency` actually is
// (see the module header) — and `max_present_latency_ms` carried no time of
// occurrence, so it could not even be separated from a spike in a different
// hour of the same process lifetime.
//
// So each lateness now keeps a MAX, the OWNER that produced it, and WHEN it
// happened (the `now_ns` process clock, published beside `metrics_now_ms` so a
// reader can say "that was 47 minutes ago"), and the worst present latency
// keeps its instant AND the present→present GAP of the frame that set it. That
// gap is the discriminator the open interval otherwise hides: a gap the size of
// the latency means nothing presented for that long, a gap far smaller means
// frames kept coming while this one's output waited.
//
// NO HISTOGRAM. These are per-wake facts on the hottest loop in the process,
// and the question they exist to answer — how bad, whose, and when — is one a
// max answers and a percentile structurally cannot: one 400 ms timer among a
// million on-time wakes cannot move a p99.
static MAX_WAKE_LATE_NS: AtomicU64 = AtomicU64::new(0);
static MAX_WAKE_LATE_OWNER: AtomicU64 = AtomicU64::new(0);
static MAX_WAKE_LATE_AT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_DEADLINE_LATE_NS: AtomicU64 = AtomicU64::new(0);
static MAX_DEADLINE_LATE_OWNER: AtomicU64 = AtomicU64::new(0);
static MAX_DEADLINE_LATE_AT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_PRESENT_LATENCY_AT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_PRESENT_LATENCY_GAP_NS: AtomicU64 = AtomicU64::new(0);
static MAX_INPUT_PRESENT_AT_NS: AtomicU64 = AtomicU64::new(0);
/// The open slow-present episode ([`SlowEpisode`], one field per atomic), so a
/// run of slow presents costs two `aterm.log` lines instead of one per frame.
/// `SLOW_EPISODE_SAMPLES == 0` = none open since process start or the last
/// [`reset`]. Single writer: the UI thread, which is why [`reset`] (a control
/// thread) only raises [`SLOW_EPISODE_RESET`] — five separate stores racing the
/// UI thread's five could stitch two episodes into one line.
static SLOW_EPISODE_START_NS: AtomicU64 = AtomicU64::new(0);
static SLOW_EPISODE_LAST_NS: AtomicU64 = AtomicU64::new(0);
static SLOW_EPISODE_SAMPLES: AtomicU64 = AtomicU64::new(0);
static SLOW_EPISODE_WORST_NS: AtomicU64 = AtomicU64::new(0);
static SLOW_EPISODE_WORST_AT_NS: AtomicU64 = AtomicU64::new(0);
/// A `metrics reset` the UI thread has not yet acted on: its next present closes
/// the open episode, with its line, before it looks at anything else.
static SLOW_EPISODE_RESET: AtomicBool = AtomicBool::new(false);

// PER-OWNER ARM ATTRIBUTION (responsiveness audit, item 6). WHY: the two facts
// above are structurally unable to name a spin's producer. `past_deadline_arms`
// is ONE global counter, and `deadline_owner` is a LAST-WRITER snapshot of
// whichever candidate won the final min-fold — so during the 2026-08 200 kHz
// event-loop spin the pair read "31,913 past arms, owner=title_summary" while
// the actual producer was the session-status observer folded under that same
// label (see `DeadlineOwner::SessionStatus`), and the investigation spent its
// first hour in the wrong module. A counter pair per owner makes the producer
// NAMED rather than inferred: `arms` counts every deadline that owner won the
// fold with, `past_arms` the subset already in the past when it was armed. The
// cost is one extra relaxed `fetch_add` (two on a past arm) per event-loop
// turn, on a line only this thread writes.
const DEADLINE_OWNER_SLOTS: usize = 45;
static DEADLINE_ARMS_BY_OWNER: [AtomicU64; DEADLINE_OWNER_SLOTS] =
    [const { AtomicU64::new(0) }; DEADLINE_OWNER_SLOTS];
static PAST_DEADLINE_ARMS_BY_OWNER: [AtomicU64; DEADLINE_OWNER_SLOTS] =
    [const { AtomicU64::new(0) }; DEADLINE_OWNER_SLOTS];
// …AND THE FIRES, COUNTED APART (responsiveness audit item 2). An arm is a fold
// WIN per park; a fire is a timer that actually woke the loop for that owner
// (`EventWakeKind::Timer`). "cursor_effect 59/s" was read as 59 wakes a second
// by three agents during that audit; it was 59 arms, most interrupted by output.
static DEADLINE_FIRES_BY_OWNER: [AtomicU64; DEADLINE_OWNER_SLOTS] =
    [const { AtomicU64::new(0) }; DEADLINE_OWNER_SLOTS];

// THE USER-EVENT WAKES BY `Wake` VARIANT (responsiveness audit item 2).
// `event_wakes` counts every loop iteration; nothing said which proxy event
// drove them. One entry per variant seen (the enum is ~150 variants; an idle
// instance delivers a handful), its name read once from its `Debug` spelling.
static WAKES_BY_VARIANT: std::sync::Mutex<
    Vec<(std::mem::Discriminant<crate::Wake>, &'static str, u64)>,
> = std::sync::Mutex::new(Vec::new());

// THE GPU REPAINT SCOPE (responsiveness audit item 5): the renderer's own
// cumulative counters, stored after every GPU present by the main thread, and
// the values they had at the last `metrics reset`, so the published figures are
// window stats like the rest of the line.
static GPU_SCISSOR_TAKEN: AtomicU64 = AtomicU64::new(0);
static GPU_FULL_REPAINTS: AtomicU64 = AtomicU64::new(0);
static GPU_SCROLL_RESCUES: AtomicU64 = AtomicU64::new(0);
static GPU_SCISSOR_TAKEN_BASE: AtomicU64 = AtomicU64::new(0);
static GPU_FULL_REPAINTS_BASE: AtomicU64 = AtomicU64::new(0);
static GPU_SCROLL_RESCUES_BASE: AtomicU64 = AtomicU64::new(0);

// THE MAIN-LOOP TURN DISTRIBUTION PER ROOT (responsiveness audit item 3): the
// watchdog's census keeps last/max/count only, so a p95 key-queue tail could not
// be attributed to a main-thread root. Indexed by `watchdog::Breadcrumb as u8`.
pub(crate) const TURN_ROOT_SLOTS: usize = 8;
static H_TURN_BY_ROOT: [Histogram; TURN_ROOT_SLOTS] = [const { Histogram::new() }; TURN_ROOT_SLOTS];

// STALE-ARM HEAL (busy-rearm audit, item 3): the SAME owner arming a deadline
// more than [`STALE_ARM_HEAL_FLOOR`] in the past on CONSECUTIVE turns is a
// scheduler bug — an honest deadline can be a little late (the turn that
// computed it did real work first), but a deadline that stays far in the past
// across turns is a self-rearming `WaitUntil(past)` spin. [`record_deadline`]
// clamps the second and later arms of such a streak to `now + floor` and counts
// them here, so the loop survives the whole class VISIBLY (the wake_heals
// precedent: heal, count, log once per episode) instead of burning a core at
// the event loop's floor. `STALE_ARM_STREAK_OWNER` holds the previous turn's
// over-floor-late owner (`DeadlineOwner::None` = no streak); the episode flag
// makes the log once-per-episode, re-arming when a healthy arm ends the streak.
static STALE_ARM_HEALS: AtomicU64 = AtomicU64::new(0);
static STALE_ARM_STREAK_OWNER: AtomicU64 = AtomicU64::new(0);
static STALE_ARM_EPISODE: AtomicBool = AtomicBool::new(false);

/// Detection threshold AND clamp distance for the stale-arm heal. At the
/// observation-gate scale (the status classifier's 250 ms `min_interval`) on
/// purpose: a healed spin degrades to <= 4 Hz, and no legitimate late arm —
/// a busy turn runs tens of milliseconds, not hundreds — ever trips it.
const STALE_ARM_HEAL_FLOOR_NS: u64 = 250_000_000;
const STALE_ARM_HEAL_FLOOR: Duration = Duration::from_nanos(STALE_ARM_HEAL_FLOOR_NS);

// PER-OWNER PAST-ARM STREAK HEAL (follow-ups items 18/19): the detector the
// 250 ms floor above is structurally blind to. That heal only *considers* arms
// with `late > 250 ms`, so a producer arming `Instant::now()` every turn — the
// exact pre-fix `next_title_summary_retry` shape — spins at full event-loop
// rate with `late ≈ 0` and takes the branch that RESETS its streak; and its
// single global streak slot is defeated outright by two owners alternating
// stale arms. This detector is DECOUPLED from the floor and PER OWNER: each
// owner keeps a 32-arm past/future window ([`PAST_ARM_HISTORY_BY_OWNER`],
// packed bits + fill count, single-writer event-loop state), and when more
// than 90% of one owner's last 32 arms were already past when armed
// (>= [`PAST_ARM_WINDOW_TRIGGER`] of [`PAST_ARM_WINDOW`]) that owner's
// re-arms are clamped to `now + frame` — a spin degrades to display cadence
// instead of the event loop's floor — and counted in a NAMED per-owner heal
// ledger ([`PAST_ARM_STREAK_HEALS_BY_OWNER`], surfaced as
// `past_arm_streak_heals` beside `deadline_arms_by_owner`). Occasional past
// arms never trigger: a busy turn's late arm is legitimate, and the window
// tolerates 3 healthy arms in 32. When the coarser 250 ms heal already moved
// the same arm further out, that stronger clamp is kept — this one only ever
// raises a deadline that would otherwise re-arm the past.
const PAST_ARM_WINDOW: u32 = 32;
/// `> 90%` of the 32-arm window: `ceil(0.9 * 32) + ...` — the smallest count
/// strictly greater than 28.8 past arms.
const PAST_ARM_WINDOW_TRIGGER: u32 = 29;
/// One 60 Hz display frame: the clamp cadence for a windowed-streak spin. The
/// producer's own work still runs (its deadline stays armed and near), but at
/// frame rate rather than event-loop rate.
const PAST_ARM_STREAK_CLAMP_NS: u64 = 16_666_667;
const PAST_ARM_STREAK_CLAMP: Duration = Duration::from_nanos(PAST_ARM_STREAK_CLAMP_NS);
/// Low 32 bits: the last-32-arms past bits (bit 0 = newest; 1 = armed already
/// past). Bits 32..: how many arms the window has seen, saturating at 32.
static PAST_ARM_HISTORY_BY_OWNER: [AtomicU64; DEADLINE_OWNER_SLOTS] =
    [const { AtomicU64::new(0) }; DEADLINE_OWNER_SLOTS];
static PAST_ARM_STREAK_HEALS_BY_OWNER: [AtomicU64; DEADLINE_OWNER_SLOTS] =
    [const { AtomicU64::new(0) }; DEADLINE_OWNER_SLOTS];
const PAST_ARM_BITS_MASK: u64 = 0xFFFF_FFFF;

/// Advance one owner's packed past-arm window by one arm and decide whether
/// the streak detector fires. PURE on the packed word so the law is
/// unit-testable with synthetic histories: the trigger requires a FULL window
/// (32 arms seen) with >= [`PAST_ARM_WINDOW_TRIGGER`] of them past — never a
/// short, mostly-past warmup.
const fn past_arm_window_step(packed: u64, past: bool) -> (u64, bool) {
    let bits = (((packed & PAST_ARM_BITS_MASK) << 1) | past as u64) & PAST_ARM_BITS_MASK;
    let seen = {
        let next = (packed >> 32) + 1;
        if next > PAST_ARM_WINDOW as u64 {
            PAST_ARM_WINDOW as u64
        } else {
            next
        }
    };
    let trigger = seen == PAST_ARM_WINDOW as u64 && bits.count_ones() >= PAST_ARM_WINDOW_TRIGGER;
    ((seen << 32) | bits, trigger)
}

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

/// Crate-visible because the main-loop turn census
/// (`crate::watchdog::turn_census`) prices its spans and stamps its
/// `max_turn_at_ms` on THIS clock: an `_at_ms` on a different clock could not be
/// read against `metrics_now_ms`, which is the one subtraction that turns a
/// stamp into "how long ago".
pub(crate) fn now_ns() -> u64 {
    let start = *PROCESS_START.get_or_init(Instant::now);
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// The `Instant` a [`now_us`] stamp was read at — this clock's exact inverse,
/// floored to the microsecond the stamp kept. The clock is
/// `PROCESS_START.elapsed()`, so every stamp is an offset from ONE fixed anchor
/// and needs no second clock read to place: the answer is a pure function of
/// the stamp, ordered exactly as the stamps are and identical on every call.
/// `None` only past `Instant`'s range.
pub(crate) fn instant_at_us(stamp_us: u64) -> Option<Instant> {
    PROCESS_START
        .get_or_init(Instant::now)
        .checked_add(Duration::from_micros(stamp_us))
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
    /// Retired config-warning banner slot (its warnings are message-band rows
    /// since 2026-09-22, folded under `MessageBand`). The numeric tombstone is
    /// retained because deadline-owner discriminants are an append-only
    /// diagnostics wire contract.
    Retired22 = 22,
    /// Robi's tip bubble (`crate::robi_bubble::RobiBubble::deadline`): a
    /// frame cadence through its entrance and exit ramps, one wake at the exit
    /// boundary through the hold. Slot 23 was the retired transient notice's
    /// (`update_notice`, the floating card whose producers are message-band
    /// rows since 2026-09-23); the bubble is what survived of it, and the name
    /// moved with the surface — the precedent `MessageBand` records.
    RobiBubble = 23,
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
    /// The macOS access card's watch (`consent_card::CardState::next_wake`):
    /// the probe cadence while the owner may be in System Settings, the
    /// decide timeout while a verdict is awaited.
    ConsentCard = 36,
    /// The message band (`crate::messages_host::App::messages_deadline`): ONE
    /// wake at the nearest fold, staleness cap, overflow patience or D1 shrink
    /// instant the center has armed. A live row folds nothing (its paints ride
    /// its reporter's wakes), and an idle band arms nothing — an idle window
    /// never wakes for it (FL-1). Slot 34 was the retired floating progress
    /// card's, then the status bars' (`status_bars`, 2026-09-07 → 2026-09-22);
    /// the name moved with the surface each time, and this is the third.
    MessageBand = 34,
    /// The per-session STATUS observer (`SessionStatus::next_wake`): a candidate
    /// serving its dwell, or a Running session aging into Quiet. Split out of
    /// `TitleSummary` (item 8) because the two fold at the same seam in
    /// `about_to_wait` and used to share one label — so the 2026-08 event-loop
    /// spin, whose producer was THIS observer, was reported as `title_summary`
    /// and cost the investigation its first hour on the wrong module.
    SessionStatus = 35,
    /// PRESENCE (`crate::presence`): the band's `since` text tick (once a
    /// second while it prints seconds, once a minute after) and the nine steps
    /// of the turn-submit ripple — armed only while a band row or a ripple is
    /// up. A quiet window folds nothing here (`presence_fp == 0`).
    Presence = 37,
    /// The late park's gate re-run (`App::try_park_for_prelaunched_successor`):
    /// a prelaunched update attempt waiting for a quiet moment to park.
    ///
    /// 38 and not 37: this and `Presence` were written against the same tree
    /// and both took the next free slot. The table is an APPEND-ONLY WIRE
    /// CONTRACT (`every_deadline_owner_has_its_own_label_and_its_own_slot`), so
    /// the one that reached main first keeps its number and this one moves.
    HandoffPark = 38,
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
    MessageBandMotion = 39,
    /// STRAIN (design §10.14, ruling 211): the next reading of the machine
    /// while the strain engine is Suspect or Open — every 2 s, a sweep on every
    /// other — and only while the `explain_heavy_load` switch is on, a focused
    /// window's band is on screen and no reading is in flight. A calm machine,
    /// an unfocused, occluded, minimized or headless window and a folded row
    /// arm nothing (`crate::strain_host`).
    SystemStrain = 40,
    /// A SESSION'S WAITS (design rulings 231–234): the next sample of a large
    /// paste on its way to a program or of a scrollback rewrap the person
    /// waits on — every 250 ms while its row is up, every second while none
    /// is — and nothing while nothing is watched (`crate::session_waits`).
    SessionWaits = 41,
    /// The INPUT WATCH (`crate::input_stall::InputWatches`, 2026-09-24): one
    /// wake at the instant a session's oldest unread input byte turns a stall
    /// (`STALL_AFTER` old), then every `RECHECK` while a stall, a stopped job
    /// or canonical type-ahead stands. Armed only while some session HAS
    /// unread input — the sink's input hook starts a watch, a clear reading
    /// ends it — so an idle machine folds nothing here.
    ///
    /// 42 and not 40: this was written against a tree where 40 was free, and
    /// main's `SystemStrain` (40) and `SessionWaits` (41) reached main first.
    /// The table is an APPEND-ONLY WIRE CONTRACT, so this one moves.
    InputWatch = 42,
    /// THE CRASH JOURNAL (`crate::crash_journal::Lane::next_wake`, PTY keeper
    /// P1): the trailing edge of a capture a change inside the write interval
    /// owes, and the end of a reopened layout's probation, when the image still
    /// carries its mark. An idle window owes neither.
    CrashJournal = 43,
    /// The update checker's watchdog (`crate::update_checker_watch`, plan P2-1):
    /// ONE wake at the instant the background check loop's heartbeat would go
    /// stale — twice its phase's budget past its last stamp. A healthy loop
    /// re-stamps long before it, so this wakes an idle window about once an hour
    /// to find it fresh; a window with no checker (headless, automatic checks
    /// off, an uninstalled copy) never arms it.
    ///
    /// 44 and not 40: this was written against a tree where 40 was free, and
    /// main's `SystemStrain` (40) through `CrashJournal` (43) reached main
    /// first. The table is an APPEND-ONLY WIRE CONTRACT, so this one moves.
    UpdateChecker = 44,
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
            22 => Self::Retired22,
            23 => Self::RobiBubble,
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
            36 => Self::ConsentCard,
            34 => Self::MessageBand,
            35 => Self::SessionStatus,
            37 => Self::Presence,
            38 => Self::HandoffPark,
            39 => Self::MessageBandMotion,
            40 => Self::SystemStrain,
            41 => Self::SessionWaits,
            42 => Self::InputWatch,
            43 => Self::CrashJournal,
            44 => Self::UpdateChecker,
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
            Self::Retired22 => "retired-22",
            Self::RobiBubble => "robi_bubble",
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
            Self::ConsentCard => "consent_card",
            Self::MessageBand => "message_band",
            Self::SessionStatus => "session_status",
            Self::Presence => "presence",
            Self::HandoffPark => "handoff_park",
            Self::MessageBandMotion => "message_band_motion",
            Self::SystemStrain => "system_strain",
            Self::SessionWaits => "session_waits",
            Self::InputWatch => "input_watch",
            Self::CrashJournal => "crash_journal",
            Self::UpdateChecker => "update_checker",
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
pub(crate) fn mark_process_start() {
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

/// L1 (early reveal): the instant the FIRST OS window actually became visible —
/// a `set_visible(true)` on the first window's reveal path, or (for a window
/// created visible) the moment right after its creation. This is the number the
/// user's eye measures at launch: "when did a window exist AT ALL", as opposed
/// to `first_present` ("when did real content land"). The warm-launch early
/// reveal moves it BEFORE the backend join, so it must be its own stamp — it is
/// not derivable from any phase of the exclusive present partition, and unlike
/// that partition it can also legally land AFTER the first present (an overlap
/// handoff reveals only once the carried pixels are on). First-write-wins:
/// every window's reveal path offers the stamp; only the first records, so
/// later Cmd-N windows can never replace it.
static FIRST_WINDOW_VISIBLE: OnceLock<Instant> = OnceLock::new();

/// Offer the first-window-reveal stamp (see [`FIRST_WINDOW_VISIBLE`]).
pub(crate) fn mark_first_window_visible() {
    let _ = FIRST_WINDOW_VISIBLE.set(Instant::now());
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

    /// The two stamps that bracket `backend_finalize_ns` — the join entry and
    /// the join exit. Named rather than indexed at the call site so the worker
    /// drill-down and `derive_startup_attach` can never disagree about which
    /// pair of points the phase is.
    fn backend_finalize_bounds(self) -> (Instant, Instant) {
        let [
            _,
            _,
            _,
            before_backend_finalize,
            after_backend_finalize,
            _,
            _,
        ] = self.points;
        (before_backend_finalize, after_backend_finalize)
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

/// `anchor` → the first-window reveal, or 0 while either stamp is missing.
/// Unlike the present-anchored startup sample this is read LIVE from the
/// stamps at snapshot time: the reveal (early-reveal path) legitimately exists
/// long before any present publishes `STARTUP_PRESENT`, and time-to-visible
/// must be reportable in exactly that window.
fn first_visible_since(anchor: Option<Instant>) -> u64 {
    anchor
        .zip(FIRST_WINDOW_VISIBLE.get().copied())
        .and_then(|(start, end)| duration_ns(start, end))
        .unwrap_or(0)
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

// ---------------------------------------------------------------------------
// BACKEND-BUILD WORKER — the inside of `backend_finalize`.
//
// `startup_attach_backend_finalize_ns` measures ONE `handle.join()`: the event
// loop blocking on the backend-build worker spawned in `crate::main_entry`.
// That single number was READ AS the largest phase in the whole ledger
// (300.83 ms median on macOS, 66.1% of rust_main → first_present, recorded
// 2026-07-30) and, until these stamps, the only phase with no drill-down — so
// every optimization proposed inside it had to be argued from a guess, and
// three in a row were refused for being unsizeable. These stamps settled it on
// 2026-08-23: `backend_finalize` is 0.01 ms median (0.02 ms max, 40 fresh
// processes) with `after_join_ns` exactly 0.00 in every sample — the SMALLEST
// phase in the ledger, not the largest
// (`docs/measured/arena/2026-08-23-start-backend-finalize-drilldown-dev-smoke.md`,
// M5 Max DEV-SMOKE / NON-PUBLISHABLE). The 300.83 ms figure is superseded and
// must not be used to size work.
//
// TWO SEPARATE QUESTIONS live in that number, and conflating them is what made
// the guesses wrong:
//
//   1. WHAT does the worker do? The exclusive legs below ([`StartupWorkerLegs`])
//      plus the renderer-side split in `aterm_gpu::startup_probe`, which times
//      the wgpu instance/adapter/device legs, the parallel font thread and its
//      join, and every render pipeline.
//   2. HOW MUCH of that was still OUTSTANDING when the join was reached? A
//      worker already 90% done at the join has nothing left to win from more
//      overlap; a worker 10% done has everything. `backend_finalize_ns` cannot
//      tell those apart — it reads the same either way. `overlap_ns` vs
//      `after_join_ns` IS that split, and it is what any "start it earlier /
//      overlap it harder" proposal has to be sized against.
//
// Both stamps ride the one process `Instant` clock: `spawn` is taken on the
// MAIN thread immediately before `thread::spawn` (thread-creation cost lands
// inside the worker's own timeline, where it belongs), `done` on the WORKER
// thread immediately before it publishes the finished backend. First-write-wins
// like every other startup stamp — a process has one cold build.
static BACKEND_WORKER_SPAWN: OnceLock<Instant> = OnceLock::new();
static BACKEND_WORKER_DONE: OnceLock<Instant> = OnceLock::new();
static BACKEND_WORKER_LEGS: OnceLock<StartupWorkerLegs> = OnceLock::new();

/// Wire schema for the backend-build worker partition — the exclusive
/// drill-down of `startup_attach_backend_finalize_ns`.
pub(crate) const STARTUP_WORKER_SCHEMA: u64 = 1;

/// Wire schema for the renderer-side sub-split of the worker's GPU build,
/// sourced from `aterm_gpu::startup_probe`.
pub(crate) const STARTUP_GPU_SCHEMA: u64 = 1;

/// The worker's own exclusive legs, timed on the worker thread and published in
/// ONE transaction so a snapshot can never pair a filled leg with a missing one.
/// The remaining slice (`epilogue_ns`) is DERIVED against the worker's total
/// rather than stamped, so an unmeasured line inside the worker surfaces as
/// epilogue instead of silently vanishing from the partition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StartupWorkerLegs {
    /// Worker entry → the renderer constructor call.
    pub(crate) prelude_ns: u64,
    /// `GpuRenderer::new_with_family` (or the CPU `from_system_with_family`
    /// fallback) — the leg `aterm_gpu::startup_probe` splits further.
    pub(crate) gpu_build_ns: u64,
    /// The worker's wait for the main thread to hand over the resolved font
    /// generation (`startup_font_rx.recv()`). Non-zero means the worker
    /// out-ran launch config resolution, not that fonts are slow.
    pub(crate) font_admit_ns: u64,
    /// `apply_font_config_to_backend`.
    pub(crate) font_apply_ns: u64,
    /// `seal_admitted_font_sources` — the worker-only broad fallback/symbol/
    /// emoji admission. Effectively ZERO on a headless launch, which defers that
    /// admission to its first pixel demand (`defer_font_seal`) and so does not
    /// perform it on this worker at all; the leg names the work this worker did,
    /// never the work the generation will eventually cost.
    pub(crate) font_seal_ns: u64,
}

/// Mark the instant the backend-build worker is spawned (main thread, strictly
/// before `thread::spawn`).
pub(crate) fn mark_backend_worker_spawn() {
    let _ = BACKEND_WORKER_SPAWN.set(Instant::now());
}

/// Nanoseconds elapsed since `started`, saturating.
///
/// The backend-build worker runs on a background thread with no ledger state of
/// its own: it times its legs against plain `Instant`s and hands the finished
/// durations over in ONE transaction ([`record_backend_worker_legs`]), so this
/// is the only ns conversion it needs.
pub(crate) fn leg_elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Publish the worker's complete leg timing (worker thread, before `done`).
pub(crate) fn record_backend_worker_legs(legs: StartupWorkerLegs) {
    let _ = BACKEND_WORKER_LEGS.set(legs);
}

/// Mark the instant the worker finished building the backend (worker thread,
/// immediately before it publishes).
pub(crate) fn mark_backend_worker_done() {
    let _ = BACKEND_WORKER_DONE.set(Instant::now());
}

// ---------------------------------------------------------------------------
// THE SUCCESSOR'S CLAIM TIMELINE (warm successor P0,
// docs/DESIGN-warm-successor-2026-09-29.md "Where the milliseconds go").
//
// Under the late park the outgoing terminal is frozen from just before this
// successor's rendezvous claim until Commit, and `claim->proof` is the part of
// that freeze this process pays. That one number used to be all the log said;
// every segment inside it was inferred from unrelated lines. These stamps make
// each boundary a measurement, relative to the claim instant.
//
// OBSERVATION ONLY. Every stamp is a first-write-wins `OnceLock` set that
// nothing reads back to decide anything, and every one is a no-op until a claim
// is recorded — so an ordinary launch, and every launch off macOS, records
// nothing here. The boundaries the startup ledger already stamps (`run_app`,
// the first `resumed`, the seven attach points, the backend worker's spawn and
// done, the first reveal) are READ from that ledger, never stamped twice.

/// The boundaries only a handoff boot stamps for itself: the ones the startup
/// ledger above has no stamp for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))] // `Proof` is stamped on the unix lane only
pub(crate) enum ClaimStamp {
    /// `seamless::take_incoming` returned: the intake's authentication is done.
    Intake,
    /// Session 0 (the first adopted shell) was bound by `spawn_session`.
    Adopt0,
    /// The winit `EventLoop` was built.
    EventLoopBuilt,
    /// The first successful present of any window (`first_present_done`).
    FirstPresent,
    /// The deferred restore (windows 2..N, the other adopted shells) began.
    RestoreStart,
    /// The deferred restore returned: every carried shell is placed.
    RestoreDrained,
    /// The adoption proof is about to be written to the parent.
    Proof,
}

impl ClaimStamp {
    const COUNT: usize = 7;
}

/// The earliest in-process instant this process stamped: the shipped binary's
/// Rust `main` ([`mark_rust_main_start`]), else the GUI entry (the thin dev
/// binary). The loader's work before it is outside every stamp here.
#[must_use]
pub(crate) fn process_main_start() -> Option<Instant> {
    RUST_MAIN_START
        .get()
        .or_else(|| PROCESS_START.get())
        .copied()
}

/// When the claim was granted. `None` on every launch that did not claim.
static CLAIM_AT: OnceLock<Instant> = OnceLock::new();
/// When this successor dialled the rendezvous (before the grant hold).
static CLAIM_DIALLED_AT: OnceLock<Instant> = OnceLock::new();
/// How long this successor spent warming BEFORE its dial. Recorded beside the
/// claim; zero until a warm stage exists (the design's P2).
static CLAIM_WARM_PRE_DIAL: OnceLock<std::time::Duration> = OnceLock::new();
static CLAIM_STAMPS: [OnceLock<Instant>; ClaimStamp::COUNT] =
    [const { OnceLock::new() }; ClaimStamp::COUNT];

/// Record the rendezvous claim: the zero point of every [`ClaimStamp`]. First
/// write wins, as for every startup stamp.
#[cfg(target_os = "macos")]
pub(crate) fn mark_handoff_claimed(
    dialled_at: Instant,
    claimed_at: Instant,
    warm_pre_dial: std::time::Duration,
) {
    let _ = CLAIM_DIALLED_AT.set(dialled_at);
    let _ = CLAIM_WARM_PRE_DIAL.set(warm_pre_dial);
    let _ = CLAIM_AT.set(claimed_at);
}

/// Offer one claim-relative boundary. A no-op on a launch that claimed nothing,
/// and after the first write.
pub(crate) fn stamp_claim(stamp: ClaimStamp) {
    if CLAIM_AT.get().is_some() {
        let _ = CLAIM_STAMPS[stamp as usize].set(Instant::now());
    }
}

/// Everything the claim timeline is derived from, gathered so the derivation is
/// a pure function a test can drive with invented instants.
#[derive(Clone, Copy, Debug)]
struct ClaimTimelineInputs {
    claim: Instant,
    /// [`process_main_start`].
    exec: Option<Instant>,
    dial: Option<Instant>,
    own: [Option<Instant>; ClaimStamp::COUNT],
    worker_spawn: Option<Instant>,
    worker_done: Option<Instant>,
    run_app: Option<Instant>,
    resumed: Option<Instant>,
    attach: Option<StartupAttachMilestones>,
    surface_ready: Option<Instant>,
    visible: Option<Instant>,
}

/// One labelled boundary, in signed microseconds after the claim (the Rust
/// main and the dial precede it, so they read negative).
type ClaimPoint = (&'static str, i64);

fn signed_us(zero: Instant, at: Instant) -> i64 {
    match at.checked_duration_since(zero) {
        Some(after) => i64::try_from(after.as_micros()).unwrap_or(i64::MAX),
        None => {
            i64::try_from(zero.duration_since(at).as_micros()).map_or(i64::MIN, std::ops::Neg::neg)
        }
    }
}

/// The claim timeline in its NOMINAL boot order (the order the code runs them;
/// the values say what really happened). A boundary not reached is left out, so
/// every point printed is a measurement.
fn claim_timeline_points(inputs: &ClaimTimelineInputs) -> Vec<ClaimPoint> {
    let own = |stamp: ClaimStamp| inputs.own[stamp as usize];
    let attach = inputs.attach.map(|milestones| milestones.points);
    let attach_point = |index: usize| attach.map(|points| points[index]);
    let ordered: [(&'static str, Option<Instant>); 22] = [
        ("main", inputs.exec),
        ("dial", inputs.dial),
        ("intake", own(ClaimStamp::Intake)),
        ("worker_spawn", inputs.worker_spawn),
        ("event_loop", own(ClaimStamp::EventLoopBuilt)),
        ("adopt0", own(ClaimStamp::Adopt0)),
        ("run_app", inputs.run_app),
        ("resumed", inputs.resumed),
        ("attach", attach_point(0)),
        ("window_create", attach_point(1)),
        ("window_created", attach_point(2)),
        ("join", attach_point(3)),
        ("worker_done", inputs.worker_done),
        ("joined", attach_point(4)),
        ("surface_create", attach_point(5)),
        // The seventh attach point: without it the surface's own creation and
        // the finish after it read as one slice.
        ("surface_created", attach_point(6)),
        ("surface_ready", inputs.surface_ready),
        ("first_present", own(ClaimStamp::FirstPresent)),
        ("visible", inputs.visible),
        ("restore", own(ClaimStamp::RestoreStart)),
        ("drained", own(ClaimStamp::RestoreDrained)),
        ("proof", own(ClaimStamp::Proof)),
    ];
    ordered
        .into_iter()
        .filter_map(|(label, at)| Some((label, signed_us(inputs.claim, at?))))
        .collect()
}

/// The backend worker against the join that waits for it: how long the join
/// blocked, and how far the worker's `done` sat before (positive) or after
/// (negative) the join's entry. `None` until both halves exist.
#[cfg_attr(not(unix), allow(dead_code))]
fn claim_worker_vs_join(inputs: &ClaimTimelineInputs) -> Option<(i64, i64)> {
    let (join_entry, join_exit) = inputs.attach?.backend_finalize_bounds();
    let done = inputs.worker_done?;
    Some((
        signed_us(join_entry, join_exit),
        signed_us(done, join_entry),
    ))
}

fn claim_timeline_inputs() -> Option<ClaimTimelineInputs> {
    let claim = CLAIM_AT.get().copied()?;
    let mut own = [None; ClaimStamp::COUNT];
    for (slot, stamp) in own.iter_mut().zip(CLAIM_STAMPS.iter()) {
        *slot = stamp.get().copied();
    }
    Some(ClaimTimelineInputs {
        claim,
        exec: process_main_start(),
        dial: CLAIM_DIALLED_AT.get().copied(),
        own,
        worker_spawn: BACKEND_WORKER_SPAWN.get().copied(),
        worker_done: BACKEND_WORKER_DONE.get().copied(),
        run_app: GUI_READY_FOR_WINIT.get().copied(),
        resumed: FIRST_WINIT_RESUMED.get().copied(),
        attach: INITIAL_ATTACH_MILESTONES.get().copied(),
        surface_ready: INITIAL_SURFACE_READY.get().copied(),
        visible: FIRST_WINDOW_VISIBLE.get().copied(),
    })
}

/// Signed microseconds as milliseconds with `decimals` places (`-3.1`, `12.40`).
fn signed_ms(us: i64, decimals: usize) -> String {
    let sign = if us < 0 { "-" } else { "" };
    let ms = us.unsigned_abs() as f64 / 1000.0;
    format!("{sign}{ms:.decimals$}")
}

/// The log's rendering of the whole timeline, or `None` on a launch that did not
/// claim: `intake +12.3, adopt0 +80.1, … ; the backend join waited 0.4 ms, the
/// worker done 3.2 ms before it`.
#[cfg_attr(not(unix), allow(dead_code))]
#[must_use]
pub(crate) fn claim_timeline_log_text() -> Option<String> {
    let inputs = claim_timeline_inputs()?;
    Some(render_claim_timeline_log(&inputs))
}

#[cfg_attr(not(unix), allow(dead_code))]
fn render_claim_timeline_log(inputs: &ClaimTimelineInputs) -> String {
    let points = claim_timeline_points(inputs)
        .into_iter()
        .map(|(label, us)| {
            let sign = if us < 0 { "" } else { "+" };
            format!("{label} {sign}{}", signed_ms(us, 1))
        })
        .collect::<Vec<_>>()
        .join(", ");
    match claim_worker_vs_join(inputs) {
        Some((waited, slack)) => format!(
            "{points}; the backend join waited {} ms, the worker done {} ms {} it",
            signed_ms(waited, 1),
            signed_ms(slack.saturating_abs(), 1),
            if slack < 0 { "after" } else { "before" },
        ),
        None => points,
    }
}

/// The warm stage's cost before the dial, as the log and `metrics` say it.
#[must_use]
pub(crate) fn claim_warm_pre_dial() -> Option<std::time::Duration> {
    CLAIM_AT.get()?;
    CLAIM_WARM_PRE_DIAL.get().copied()
}

/// The `metrics` summary's claim fragment: ` handoff_claim_stamps=<label:ms,…|none>
/// handoff_warm_pre_dial_ms=<ms|none>`. Self-labelling pairs, like
/// `startup_gpu_cell_pipeline_ms`, so a new boundary never shifts a column.
#[must_use]
pub(crate) fn handoff_claim_fields_text() -> String {
    let inputs = claim_timeline_inputs();
    let stamps = inputs.as_ref().map_or_else(
        || "none".to_string(),
        |inputs| {
            claim_timeline_points(inputs)
                .into_iter()
                .map(|(label, us)| format!("{label}:{}", signed_ms(us, 2)))
                .collect::<Vec<_>>()
                .join(",")
        },
    );
    let warm = claim_warm_pre_dial().map_or_else(
        || "none".to_string(),
        |warm| format!("{:.2}", warm.as_secs_f64() * 1000.0),
    );
    format!(" handoff_claim_stamps={stamps} handoff_warm_pre_dial_ms={warm}")
}

/// Field-for-field JSON twin of [`handoff_claim_fields_text`], with a leading
/// comma: an object of `label: ms` (or `null`), and a number (or `null`).
#[must_use]
pub(crate) fn handoff_claim_fields_json() -> String {
    let inputs = claim_timeline_inputs();
    let stamps = inputs.as_ref().map_or_else(
        || "null".to_string(),
        |inputs| {
            let pairs = claim_timeline_points(inputs)
                .into_iter()
                .map(|(label, us)| format!("\"{label}\":{}", signed_ms(us, 2)))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{pairs}}}")
        },
    );
    let warm = claim_warm_pre_dial().map_or_else(
        || "null".to_string(),
        |warm| format!("{:.2}", warm.as_secs_f64() * 1000.0),
    );
    format!(",\"handoff_claim_stamps\":{stamps},\"handoff_warm_pre_dial_ms\":{warm}")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupWorkerSample {
    valid: bool,
    total_ns: u64,
    overlap_ns: u64,
    after_join_ns: u64,
    post_join_ns: u64,
    prelude_ns: u64,
    gpu_build_ns: u64,
    font_admit_ns: u64,
    font_apply_ns: u64,
    font_seal_ns: u64,
    epilogue_ns: u64,
}

impl StartupWorkerSample {
    /// The measured legs, in worker order. `epilogue_ns` closes them against
    /// `total_ns`, so this sum plus the epilogue IS the worker's wall time.
    fn measured_leg_total_ns(self) -> Option<u64> {
        [
            self.prelude_ns,
            self.gpu_build_ns,
            self.font_admit_ns,
            self.font_apply_ns,
            self.font_seal_ns,
        ]
        .into_iter()
        .try_fold(0u64, |total, leg| total.checked_add(leg))
    }
}

/// Derive the worker partition from the four `Instant`s that bound it plus the
/// worker's own legs.
///
/// The worker can legally finish BEFORE the join is reached (a warm launch, a
/// CPU backend, a machine where window creation outlasts the build) or AFTER it
/// (every measured macOS cold launch so far). Both orders produce a well-formed
/// partition — which one happened is the finding, not an error — so the split
/// point is `min(done, join_entry)` and the resume point `max(done, join_entry)`,
/// giving two exact identities:
///
/// * `overlap_ns + after_join_ns == total_ns` — the worker's own wall time,
///   split at the join.
/// * `after_join_ns + post_join_ns == backend_finalize_ns` — the join's wall
///   time, split at the worker's finish.
///
/// `post_join_ns` is therefore the part of `backend_finalize` that is NOT the
/// worker at all: the thread handoff plus the post-join setup `finalize_backend`
/// runs (pad/head seed, GPU effect pins, font-feature warnings).
fn derive_startup_worker(
    spawn: Option<Instant>,
    done: Option<Instant>,
    legs: Option<StartupWorkerLegs>,
    join_entry: Option<Instant>,
    join_exit: Option<Instant>,
) -> StartupWorkerSample {
    let Some(spawn) = spawn else {
        return StartupWorkerSample::default();
    };
    let Some(done) = done else {
        return StartupWorkerSample::default();
    };
    let Some(legs) = legs else {
        return StartupWorkerSample::default();
    };
    let Some(join_entry) = join_entry else {
        return StartupWorkerSample::default();
    };
    let Some(join_exit) = join_exit else {
        return StartupWorkerSample::default();
    };
    let Some(total_ns) = duration_ns(spawn, done) else {
        return StartupWorkerSample::default();
    };
    let split = done.min(join_entry);
    let resume = done.max(join_entry);
    let Some(overlap_ns) = duration_ns(spawn, split) else {
        return StartupWorkerSample::default();
    };
    let after_join_ns = duration_ns(join_entry, done).unwrap_or(0);
    let Some(post_join_ns) = duration_ns(resume, join_exit) else {
        return StartupWorkerSample::default();
    };
    let sample = StartupWorkerSample {
        valid: true,
        total_ns,
        overlap_ns,
        after_join_ns,
        post_join_ns,
        prelude_ns: legs.prelude_ns,
        gpu_build_ns: legs.gpu_build_ns,
        font_admit_ns: legs.font_admit_ns,
        font_apply_ns: legs.font_apply_ns,
        font_seal_ns: legs.font_seal_ns,
        epilogue_ns: 0,
    };
    let Some(measured_ns) = sample.measured_leg_total_ns() else {
        return StartupWorkerSample::default();
    };
    let Some(epilogue_ns) = total_ns.checked_sub(measured_ns) else {
        return StartupWorkerSample::default();
    };
    if overlap_ns.checked_add(after_join_ns) != Some(total_ns)
        || after_join_ns.checked_add(post_join_ns) != duration_ns(join_entry, join_exit)
    {
        return StartupWorkerSample::default();
    }
    StartupWorkerSample {
        epilogue_ns,
        ..sample
    }
}

/// The renderer-side split of the worker's GPU build, read back from
/// `aterm_gpu::startup_probe` — the crate where the work happens and which
/// cannot see this ledger.
///
/// `font_thread_ns` is the ONE parallel leg (it overlaps the GPU legs by
/// design), so it is reported ALONGSIDE the partition, never inside it;
/// `font_join_ns` is its exclusive residue — the wait GPU init actually paid
/// for it, and the honest ceiling on what removing the font work could win.
/// `tail_ns` is derived, closing the measured legs against `gpu_build_ns`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupGpuSample {
    valid: bool,
    instance_ns: u64,
    adapter_ns: u64,
    device_ns: u64,
    context_tail_ns: u64,
    font_thread_ns: u64,
    font_join_ns: u64,
    pipelines_ns: u64,
    pipe_shader_ns: u64,
    pipe_uniform_atlas_ns: u64,
    pipe_cell_ns: u64,
    pipe_blit_ns: u64,
    pipe_tray_ns: u64,
    pipe_bloom_ns: u64,
    pipe_vbuf_ns: u64,
    pipe_tail_ns: u64,
    tail_ns: u64,
    cell_pipeline_ns: [u64; aterm_gpu::startup_probe::CELL_PIPELINE_COUNT],
}

/// Fold the GPU probe's slots into a sample, closed against the worker's own
/// measurement of the same call (`gpu_build_ns`).
///
/// `valid` is false — and every field stays zero — whenever the process took the
/// CPU backend (no GPU leg ever ran), the probe is incomplete, or the legs do
/// not fit inside `gpu_build_ns`. An honest "no data" beats a partition that
/// does not reconcile.
fn derive_startup_gpu(gpu_build_ns: u64) -> StartupGpuSample {
    close_startup_gpu(read_gpu_probe(), gpu_build_ns)
}

/// Read the process-global probe slots into an unclosed sample (`tail_ns` still
/// 0, `valid` still provisional). Kept separate from [`close_startup_gpu`] so
/// the reconciliation rules have a local, GPU-free test — the same split
/// [`record_initial_attach_milestones_once`] uses for first-writer-wins.
fn read_gpu_probe() -> StartupGpuSample {
    use aterm_gpu::startup_probe::{Leg, cell_pipeline_ns, leg_ns};
    StartupGpuSample {
        valid: true,
        instance_ns: leg_ns(Leg::GpuInstance),
        adapter_ns: leg_ns(Leg::GpuAdapter),
        device_ns: leg_ns(Leg::GpuDevice),
        context_tail_ns: leg_ns(Leg::GpuContextTail),
        font_thread_ns: leg_ns(Leg::FontThread),
        font_join_ns: leg_ns(Leg::FontJoin),
        pipelines_ns: leg_ns(Leg::PipeTotal),
        pipe_shader_ns: leg_ns(Leg::PipeShader),
        pipe_uniform_atlas_ns: leg_ns(Leg::PipeUniformAtlas),
        pipe_cell_ns: leg_ns(Leg::PipeCell),
        pipe_blit_ns: leg_ns(Leg::PipeBlit),
        pipe_tray_ns: leg_ns(Leg::PipeTray),
        pipe_bloom_ns: leg_ns(Leg::PipeBloom),
        pipe_vbuf_ns: leg_ns(Leg::PipeVertexBuffers),
        pipe_tail_ns: leg_ns(Leg::PipeTail),
        tail_ns: 0,
        cell_pipeline_ns: cell_pipeline_ns(),
    }
}

/// Close a probe read against the worker's own measurement of the same call.
fn close_startup_gpu(sample: StartupGpuSample, gpu_build_ns: u64) -> StartupGpuSample {
    // A zero slot is the probe's UNSET sentinel (a recorded leg floors at 1 ns),
    // so any zero here means this process never took the GPU path.
    if [
        sample.instance_ns,
        sample.adapter_ns,
        sample.device_ns,
        sample.context_tail_ns,
        sample.font_thread_ns,
        sample.font_join_ns,
        sample.pipelines_ns,
    ]
    .into_iter()
    .any(|leg| leg == 0)
    {
        return StartupGpuSample::default();
    }
    let exclusive_ns = [
        sample.instance_ns,
        sample.adapter_ns,
        sample.device_ns,
        sample.context_tail_ns,
        sample.font_join_ns,
        sample.pipelines_ns,
    ]
    .into_iter()
    .try_fold(0u64, |total, leg| total.checked_add(leg));
    let Some(tail_ns) = exclusive_ns.and_then(|measured| gpu_build_ns.checked_sub(measured)) else {
        return StartupGpuSample::default();
    };
    StartupGpuSample { tail_ns, ..sample }
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
// Latency distributions (LAT-3).
//
// `last_*`/`max_*` scalars can't answer "what does typing feel like at p99",
// and a driver publishing latency claims needs percentiles, not anecdotes
// (FASTER_THAN_GHOSTTY_PLAN.md §4/LAT-3). Three log-linear histograms record
// every sample the scalars already see — same funnel, no new stamps, so the
// honesty bounds documented on the scalars apply to the distributions too.
// COORDINATED OMISSION: each window's `PendingInputStamp` is a bounded EDGE RING
// (`INPUT_EDGE_RING` keystrokes), and a content present drains EVERY edge into
// the input histogram, so the keys typed during a stall are each a sample
// rather than hidden behind the burst's oldest (docs/WINDOWS_PARAGON_DESIGN.md
// item 2.5, landed 2026-09-25). A burst past the ring's capacity keeps its
// OLDEST edges and counts the rest in `input_edges_dropped` (published beside
// the percentiles), so the loss is visible. The session-routed `INPUT_STAMP_NS`
// CAS — input that names a session but no window — remains the one keep-oldest
// lane, since which window presents it is unknown at the stamp.

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
pub(crate) struct Histogram {
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
    pub(crate) fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// The value at quantile `q` (0 < q ≤ 1), as the containing bucket's upper
    /// edge in ns — conservative by construction. `None` until a sample lands.
    /// Concurrent writers can skew a racing read by a sample or two; that is
    /// the same take-what-you-see contract as every scalar here.
    pub(crate) fn percentile(&self, q: f64) -> Option<u64> {
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
/// The occluded/parked/capture twin of [`H_PRESENT_LATENCY`] — see the
/// `PRESENT_TAINT_UNTIL_NS` block.
static H_PRESENT_LATENCY_TAINTED: Histogram = Histogram::new();
static H_FRAME_RENDER: Histogram = Histogram::new();
static H_KEY_WRITE: Histogram = Histogram::new();
/// The OS-EVENT-QUEUE leg [`H_KEY_WRITE`] is BACKDATED by, kept apart so the
/// total can be split back into its two causes. See [`key_queue_distribution`].
static H_KEY_QUEUE: Histogram = Histogram::new();
static H_PRE_PRESENT: Histogram = Histogram::new();
static H_ACQUIRE_WAIT: Histogram = Histogram::new();
static H_ACQUIRE_QUEUE: Histogram = Histogram::new();
/// UI-thread TERMINAL-MUTEX wait, per acquiring site — see [`TermWaitSite`].
static H_TERM_WAIT: [Histogram; TermWaitSite::COUNT] =
    [const { Histogram::new() }; TermWaitSite::COUNT];
static MAX_TERM_WAIT_NS: [AtomicU64; TermWaitSite::COUNT] =
    [const { AtomicU64::new(0) }; TermWaitSite::COUNT];
static H_RESIZE_PRESENT: Histogram = Histogram::new();
static H_RESIZE_REFLOW: Histogram = Histogram::new();

/// The three live distributions, for the `metrics percentiles` verb:
/// input→application-present-return (a software-side typing proxy),
/// output→application-present-return (the `$ATERM_TRACE_LATENCY` slice), and
/// causal frame CPU work (compose plus CPU raster/copy or GPU command
/// encode/queue-submit).
#[must_use]
pub(crate) fn distributions() -> (&'static Histogram, &'static Histogram, &'static Histogram) {
    (&H_INPUT_PRESENT, &H_PRESENT_LATENCY, &H_FRAME_RENDER)
}

/// Hardware-key arrival → completed PTY write distribution. Kept separate from
/// [`distributions`] so existing callers of that three-histogram API remain
/// source-compatible.
#[must_use]
pub(crate) fn key_write_distribution() -> &'static Histogram {
    &H_KEY_WRITE
}

/// The NSEvent-QUEUE RESIDENCE that [`key_write_distribution`] is BACKDATED by:
/// how long each hardware key sat in the OS event queue before the loop
/// dispatched it.
///
/// WHY IT IS PUBLISHED APART. [`note_key_arrival_queued`] stamps the arrival at
/// `now - queue_ns` deliberately, so `key_write` and `input_present` both price
/// a parked event loop — the slice the touch-to-glass audit proved every
/// instrument was blind to. Keeping ONLY that backdated stamp, though, left the
/// two causes indistinguishable afterwards: a `max_key_write_ms=18.13` beside a
/// `key_write_p99_ms=6.29` is a main thread parked somewhere ELSE (a present, a
/// `term_lock` hold, a compose) if the key waited 17 ms to be dequeued, and
/// aterm's own press path if it waited none. Opposite fixes, one number. With
/// this leg published, `key_write - key_queue` at matching percentiles is the
/// pure UI-DISPATCH slice, and a large `key_queue` against a small `acquire`
/// says the loop was parked somewhere the slow-present breadcrumb can name.
///
/// READ IT WITH ITS COUNT. Every PRESSED key reaching the winit arm books a
/// sample here, while only a key that WRITES books one in `key_write`, so
/// `n_key_queue >= n_key_write` by construction. A sample is the backdate that
/// was actually APPLIED, which is why an unmeasurable age (no `NSApp`
/// `currentEvent`, a synthesized event, a Wayland or Windows build, an X11
/// stamp from a clock that is not ours — see
/// `platform::current_event_queue_age_ns`) books `0` rather than nothing: zero
/// is the honest statement that nothing was added to that key's `key_write`.
///
/// ON X11 THE LEG IS WIDER THAN A QUEUE. The age there runs from the X server's
/// stamp, so it also carries the input-method round trip every key makes when
/// `XMODIFIERS` names one (ibus: ~0.5-5 ms median, measured) — time the user
/// waits and aterm does not spend, so it belongs here, beside `key_write`,
/// rather than inside aterm's own dispatch.
#[must_use]
pub(crate) fn key_queue_distribution() -> &'static Histogram {
    &H_KEY_QUEUE
}

/// The key-QUEUE `(last, max)` pair in nanoseconds, on the same rule as
/// [`acquire_queue_last_max_ns`]: one key that waited 17 ms behind a parked loop
/// cannot move a p99 taken over thousands of promptly dispatched keys, and the
/// max is the only field that reports it — which is exactly the reading this
/// split exists to explain. Read straight off the statics by the `percentiles`
/// verb; deliberately NOT carried on [`Snapshot`], as the summary verb does not
/// publish them.
#[must_use]
pub(crate) fn key_queue_last_max_ns() -> (u64, u64) {
    (
        LAST_KEY_QUEUE_NS.load(Ordering::Relaxed),
        MAX_KEY_QUEUE_NS.load(Ordering::Relaxed),
    )
}

/// Window-bounds-change → first submitted frame at the new size. The live-drag
/// stale-frame (compositor rescale) window; see [`note_resize_arrival`].
#[must_use]
pub(crate) fn resize_present_distribution() -> &'static Histogram {
    &H_RESIZE_PRESENT
}

/// Window-bounds change → the engine committing the new GRID. The interval the
/// terminal body spends trailing the window edge; see [`note_grid_committed`].
#[must_use]
pub(crate) fn resize_reflow_distribution() -> &'static Histogram {
    &H_RESIZE_REFLOW
}

/// Redraw-entry → surface-acquire-seam distribution, including terminal grid
/// extraction. This remains observable when every actual present is dropped.
#[must_use]
pub(crate) fn pre_present_distribution() -> &'static Histogram {
    &H_PRE_PRESENT
}

/// Output→present samples taken inside an OCCLUSION/PARK or CAPTURE episode:
/// the second distribution that used to be mixed into `present_*` and put a
/// 671 ms p95 next to a 1.31 ms p50. Kept rather than dropped so an occluded or
/// recorded run stays observable; see the `PRESENT_TAINT_UNTIL_NS` block.
#[must_use]
pub(crate) fn tainted_present_distribution() -> &'static Histogram {
    &H_PRESENT_LATENCY_TAINTED
}

/// Arm the suspect window: presents for the next [`PRESENT_TAINT_TAIL_NS`] are
/// booked to the tainted ledger. `fetch_max` so a later episode can only extend
/// it, never cut a still-open one short.
fn arm_present_taint() {
    PRESENT_TAINT_UNTIL_NS.fetch_max(
        now_ns().saturating_add(PRESENT_TAINT_TAIL_NS),
        Ordering::Relaxed,
    );
}

/// True while output→present latency cannot be attributed to the terminal:
/// a capture is live, or an occlusion/park episode ended within the tail.
fn present_latency_tainted() -> bool {
    tainted_at(
        now_ns(),
        PRESENT_TAINT_UNTIL_NS.load(Ordering::Relaxed),
        CAPTURE_DEPTH.load(Ordering::Relaxed),
    )
}

/// The taint decision, PURE, so the policy has a race-free test that touches no
/// process-global state (the `wake_owner` precedent). A live capture taints
/// unconditionally; otherwise the sample is suspect until the tail expires.
const fn tainted_at(now_ns: u64, taint_until_ns: u64, capture_depth: u64) -> bool {
    capture_depth != 0 || now_ns < taint_until_ns
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
    if active {
        CAPTURE_DEPTH.fetch_add(1, Ordering::Relaxed);
        CAPTURE_EPISODES.fetch_add(1, Ordering::Relaxed);
    } else {
        // Saturating: an unpaired close (a recording finalized twice, or one
        // that began before `reset` zeroed the depth) must not wrap the depth to
        // u64::MAX and taint the ledger for the rest of the process.
        let _ = CAPTURE_DEPTH.try_update(Ordering::Relaxed, Ordering::Relaxed, |d| {
            Some(d.saturating_sub(1))
        });
    }
    arm_present_taint();
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
pub(crate) fn acquire_wait_distribution() -> &'static Histogram {
    &H_ACQUIRE_WAIT
}

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TermWaitSite {
    /// The single-pane `redraw_window`'s ONE terminal hold per frame.
    Redraw = 0,
    /// The key press's one consolidated scope in `input_to_session`.
    Press = 1,
}

impl TermWaitSite {
    /// Number of sites — the histogram array length.
    pub(crate) const COUNT: usize = 2;
    /// Every site, in report order.
    pub const ALL: [Self; Self::COUNT] = [Self::Redraw, Self::Press];

    /// The `metrics` field stem for this site.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Redraw => "redraw",
            Self::Press => "press",
        }
    }
}

/// Record one UI-thread terminal-mutex wait at `site` (cheap: one bucket
/// increment + a `fetch_max`). Uncontended acquisitions book `0` so `n_*`
/// counts every acquisition and the percentiles are over the true population.
pub(crate) fn note_term_wait(site: TermWaitSite, ns: u64) {
    H_TERM_WAIT[site as usize].record(ns);
    MAX_TERM_WAIT_NS[site as usize].fetch_max(ns, Ordering::Relaxed);
}

/// The terminal-mutex wait distribution for one UI-thread site.
#[must_use]
pub(crate) fn term_wait_distribution(site: TermWaitSite) -> &'static Histogram {
    &H_TERM_WAIT[site as usize]
}

/// The worst terminal-mutex wait booked at `site` since the last reset, ns.
#[must_use]
pub(crate) fn term_wait_max_ns(site: TermWaitSite) -> u64 {
    MAX_TERM_WAIT_NS[site as usize].load(Ordering::Relaxed)
}

/// The acquire-wait `(last, max)` pair in nanoseconds — the scalars the
/// histogram above cannot express.
///
/// Published beside the acquire percentiles by the `percentiles` verb, which
/// does not build a whole [`Snapshot`] (the summary verb reads the same two
/// values off its snapshot). A percentile is a statement about the BULK; the max
/// is the only statement about the one frame that stalled.
#[must_use]
pub(crate) fn acquire_wait_last_max_ns() -> (u64, u64) {
    (
        LAST_ACQUIRE_WAIT_NS.load(Ordering::Relaxed),
        MAX_ACQUIRE_WAIT_NS.load(Ordering::Relaxed),
    )
}

/// Record one swapchain-acquire attempt's wait, including refused acquisitions.
/// Cheap (one histogram bucket increment); the shared GPU present seam consumes
/// each sample once, before either success or failure routing.
pub(crate) fn note_acquire_wait(ns: u64) {
    H_ACQUIRE_WAIT.record(ns);
    LAST_ACQUIRE_WAIT_NS.store(ns, Ordering::Relaxed);
    MAX_ACQUIRE_WAIT_NS.fetch_max(ns, Ordering::Relaxed);
}

/// Drawable-acquire QUEUE distribution — the span from a drawable request
/// being admitted to the `aterm-drawable-acquire` worker being SCHEDULED to
/// start it, which is a different cause from the wait above.
///
/// [`acquire_wait_distribution`] starts its clock inside the worker, around
/// `nextDrawable` alone, so it is structurally blind to this leg: it reports
/// the ~0.02 ms the acquisition cost once the worker was running, however long
/// the worker took to get there. That gap is not idle time — a frame that
/// found no prefetched drawable has already returned `AcquirePending` and its
/// main thread is parked until the worker posts `GpuSurfaceReady` — so it is
/// keystroke→glass latency that, until this slice existed, was stamped nowhere
/// and could only surface as unexplained `input_present`.
///
/// It is also the direct readout on the worker's QoS: the thread is declared
/// `USER_INTERACTIVE` precisely so this distribution stays flat while the
/// machine is saturated, and a regression that drops it back to the compilers'
/// band shows up here first.
#[must_use]
pub(crate) fn acquire_queue_distribution() -> &'static Histogram {
    &H_ACQUIRE_QUEUE
}

/// The acquire-QUEUE `(last, max)` pair in nanoseconds — the scalars the
/// distribution cannot express, on the same rule as
/// [`acquire_wait_last_max_ns`]: one descheduled worker among thousands of
/// promptly served requests cannot move a p99, and the max is the only field
/// that reports the frame that actually stalled.
///
/// Read straight off the statics by the `percentiles` verb, which builds no
/// [`Snapshot`]. Deliberately NOT carried on `Snapshot` as the acquire-wait
/// pair is: the summary verb does not publish these, so a snapshot field would
/// be written every sample and read by nobody.
#[must_use]
pub(crate) fn acquire_queue_last_max_ns() -> (u64, u64) {
    (
        LAST_ACQUIRE_QUEUE_NS.load(Ordering::Relaxed),
        MAX_ACQUIRE_QUEUE_NS.load(Ordering::Relaxed),
    )
}

/// Record one completed acquisition's queue span. Booked from the same
/// completion as [`note_acquire_wait`], so the two legs of one acquisition are
/// always readable together and `n_acquire_queue` tracks `n_acquire`.
pub(crate) fn note_acquire_queue(ns: u64) {
    H_ACQUIRE_QUEUE.record(ns);
    LAST_ACQUIRE_QUEUE_NS.store(ns, Ordering::Relaxed);
    MAX_ACQUIRE_QUEUE_NS.fetch_max(ns, Ordering::Relaxed);
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
    LAST_GPU_PARK_NS.store(ns, Ordering::Relaxed);
    MAX_GPU_PARK_NS.fetch_max(ns, Ordering::Relaxed);
}

/// A frame whose causal compose+raster/encode-submit CPU work exceeds a ~30 fps
/// (33.3 ms)
/// budget — the floor below which interaction visibly stutters. `slow_frames` counts
/// these so a driver can DETECT sustained lag rather than read a momentary value.
pub(crate) const SLOW_FRAME_THRESHOLD_NS: u64 = 33_333_333; // 1/30 s

/// Record one successful application present. `latency_ns` is the
/// `output→application-present-return` delay for this frame, or `0` when no
/// output burst was pending (a blink/selection/resize repaint) — a `0` leaves
/// the last real measurement in place and is NOT a slow-frame input.
/// `render_ns` is this frame's causal CPU wall time: compose plus CPU raster/copy,
/// or time spent encoding GPU commands and calling `queue.submit`. It is not
/// completed GPU execution and excludes surface acquisition and final-present
/// pacing.
///
/// `window_input` is the PRESENTING window's pending input stamp (`None` when the
/// window state is gone). Only it — plus the session-routed global stamp — can be
/// closed by this present; another window's pending key is untouched.
///
/// Returns the slice it closed for that window when the key came through the
/// HARDWARE path and the present is untainted — one strain sample (design
/// §10.14, ruling 211): the host hands it to `StrainTracker::note_key` on a
/// frame that is drawn anyway. A controller's key, a tainted present and a
/// frame that closed nothing return `None`.
pub(crate) fn record_present(
    latency_ns: u64,
    render_ns: u64,
    startup_timing: StartupPresentTiming,
    window_input: Option<&mut PendingInputStamp>,
) -> Option<u64> {
    let mut hardware_slice = None;
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
        // HONESTY SPLIT (item 10): an output→present slice measured while the
        // window was occluded/parked, or while a capture was pacing presents,
        // describes the episode and not the terminal. Book it — visibly — to
        // the tainted ledger instead of the on-glass one. See the
        // `PRESENT_TAINT_UNTIL_NS` block.
        if present_latency_tainted() {
            TAINTED_PRESENT_SAMPLES.fetch_add(1, Ordering::Relaxed);
            LAST_TAINTED_PRESENT_LATENCY_NS.store(latency_ns, Ordering::Relaxed);
            MAX_TAINTED_PRESENT_LATENCY_NS.fetch_max(latency_ns, Ordering::Relaxed);
            H_PRESENT_LATENCY_TAINTED.record(latency_ns);
        } else {
            // WHEN, AND AGAINST WHAT. The max alone cannot be told apart from a
            // spike in another hour of the same process, and the reading is an
            // OPEN interval, so the present→present gap of the frame that set
            // it is captured with it: gap ≈ latency means nothing presented for
            // that long; gap ≪ latency means frames kept coming and this
            // frame's output waited. Single-writer (the UI thread), so
            // "the `fetch_max` that won also writes its stamps" needs no CAS.
            let at = now_ns();
            let previous_present = LAST_PRESENT_STAMP_NS.load(Ordering::Relaxed);
            let since_previous_present = if previous_present == 0 {
                0
            } else {
                at.saturating_sub(previous_present)
            };
            LAST_PRESENT_LATENCY_NS.store(latency_ns, Ordering::Relaxed);
            if MAX_PRESENT_LATENCY_NS.fetch_max(latency_ns, Ordering::Relaxed) < latency_ns {
                MAX_PRESENT_LATENCY_AT_NS.store(at, Ordering::Relaxed);
                MAX_PRESENT_LATENCY_GAP_NS.store(since_previous_present, Ordering::Relaxed);
            }
            H_PRESENT_LATENCY.record(latency_ns);
            note_slow_present(latency_ns, since_previous_present, at);
        }
        // A CONTENT present: close THIS window's pending
        // input→application-present-return slice, if any. A latency of 0
        // (blink/selection repaint) leaves the stamp pending — no attributed
        // content present has completed yet.
        //
        // DELIBERATELY NOT taint-gated. The stamp must be CONSUMED either way
        // (leaving it armed only ages it into the `INPUT_SLICE_CAP_NS` discard),
        // and input→present already carries its own honesty bound: a keystroke
        // typed into an occluded window is a real keystroke that really waited.
        // Only the OUTPUT slice above measures an interval nobody asked for.
        let now = now_ns();
        let reset_at = INPUT_RESET_AT_NS.load(Ordering::Relaxed);
        let slices =
            window_input.map_or_else(Vec::new, |pending| pending.drain_attributed(now, reset_at));
        // EVERY edge is a sample (coordinated omission corrected); the strain
        // engine keeps its one sample per present: the OLDEST edge's, which is
        // the longest wait. The strain sample IS taint-gated, unlike the
        // metric: an occluded or captured window's echo describes the episode,
        // not the machine.
        if let Some(&(d, hardware)) = slices.first()
            && hardware
            && !present_latency_tainted()
        {
            hardware_slice = Some(d);
        }
        for (d, _) in slices {
            book_input_present(d);
        }
        // The session-routed stamp (no window known at arm time): any content
        // present closes it, as before.
        let stamp = INPUT_STAMP_NS.swap(0, Ordering::Relaxed);
        if stamp != 0 {
            let d = now.saturating_sub(stamp);
            // A slice past the cap means the keystroke never echoed (see the
            // HONESTY BOUNDS above) — recording it would peg the max with a
            // latency that never happened.
            if d <= INPUT_SLICE_CAP_NS {
                book_input_present(d);
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
    note_slow_episode_quiet(now);
    hardware_slice
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

/// The open episode, read on the UI thread. A [`reset`] since the last read
/// closes it here first, writing its closing line.
fn load_slow_episode() -> SlowEpisode {
    let mut episode = SlowEpisode {
        start_ns: SLOW_EPISODE_START_NS.load(Ordering::Relaxed),
        last_ns: SLOW_EPISODE_LAST_NS.load(Ordering::Relaxed),
        samples: SLOW_EPISODE_SAMPLES.load(Ordering::Relaxed),
        worst_ns: SLOW_EPISODE_WORST_NS.load(Ordering::Relaxed),
        worst_at_ns: SLOW_EPISODE_WORST_AT_NS.load(Ordering::Relaxed),
    };
    if SLOW_EPISODE_RESET.swap(false, Ordering::Relaxed)
        && let Some(closed) = slow_episode_close(&mut episode, 0, true)
    {
        store_slow_episode(episode);
        log_slow_episode_end(closed);
    }
    episode
}

fn store_slow_episode(episode: SlowEpisode) {
    SLOW_EPISODE_START_NS.store(episode.start_ns, Ordering::Relaxed);
    SLOW_EPISODE_LAST_NS.store(episode.last_ns, Ordering::Relaxed);
    SLOW_EPISODE_SAMPLES.store(episode.samples, Ordering::Relaxed);
    SLOW_EPISODE_WORST_NS.store(episode.worst_ns, Ordering::Relaxed);
    SLOW_EPISODE_WORST_AT_NS.store(episode.worst_at_ns, Ordering::Relaxed);
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

/// ONE UNGATED LINE WHEN A SLOW-PRESENT EPISODE OPENS, AND ONE WHEN IT ENDS,
/// with the scheduler facts that name it.
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
/// edge to the present that showed it): a previous present about that long ago
/// means nothing presented for that long, a much shorter one means frames kept
/// coming while this output waited. Every other field is a `last_` reading at
/// the moment the slow frame closed, so this is a breadcrumb, not an
/// attribution proof; `max_wake_late_ms` and `max_present_latency_at_ms` on
/// `aterm ctl metrics` carry the worst cases. The closing line gives the count,
/// the span and the worst reading of the whole episode.
fn note_slow_present(latency_ns: u64, since_previous_present_ns: u64, at_ns: u64) {
    let owner = DeadlineOwner::from_raw(LAST_DEADLINE_OWNER.load(Ordering::Relaxed));
    let mut episode = load_slow_episode();
    let step = slow_episode_step(&mut episode, latency_ns, owner, at_ns);
    if step == SlowEpisodeStep::Ignored {
        return;
    }
    store_slow_episode(episode);
    let SlowEpisodeStep::Opened { closed } = step else {
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
        EventWakeKind::from_raw(LAST_WAKE_KIND.load(Ordering::Relaxed)).as_str(),
        ms(LAST_WAKE_LATE_NS.load(Ordering::Relaxed)),
        owner.as_str(),
        ms(LAST_DEADLINE_LATE_NS.load(Ordering::Relaxed)),
        ms(LAST_ACQUIRE_WAIT_NS.load(Ordering::Relaxed)),
    );
}

/// Close the open episode once it has gone quiet (or a [`reset`] asked),
/// writing its closing line. Called on every present, so it is two relaxed loads
/// when nothing is open.
fn note_slow_episode_quiet(now_ns: u64) {
    if SLOW_EPISODE_SAMPLES.load(Ordering::Relaxed) == 0 {
        SLOW_EPISODE_RESET.store(false, Ordering::Relaxed);
        return;
    }
    let mut episode = load_slow_episode();
    if let Some(closed) = slow_episode_close(&mut episode, now_ns, false) {
        store_slow_episode(episode);
        log_slow_episode_end(closed);
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
    OFFSCREEN_RASTERS.fetch_add(1, Ordering::Relaxed);
    LAST_OFFSCREEN_RASTER_NS.store(render_ns, Ordering::Relaxed);
    MAX_OFFSCREEN_RASTER_NS.fetch_max(render_ns, Ordering::Relaxed);
}

// PRESENT → GLASS ledger, fed on a framework thread by the sink installed in
// `install_present_glass_sink` (lock-free: bucket increments and `fetch_max`).
static H_PRESENT_GLASS: Histogram = Histogram::new();
static LAST_PRESENT_GLASS_NS: AtomicU64 = AtomicU64::new(0);
static MAX_PRESENT_GLASS_NS: AtomicU64 = AtomicU64::new(0);
static PRESENT_GLASS_SKIPPED: AtomicU64 = AtomicU64::new(0);
/// Per-tag ledgers (`aterm_gpu::present_glass::PresentTag`): a shown frame is
/// booked under its OWN tag, a skipped one under its ATTRIBUTION
/// (`GlassReport::skip_attribution` — the lane that replaced it, or its own
/// occlusion, or `spaced` when nothing replaced it and the compositor simply
/// dropped it). Together they answer the question the plain skip counter could
/// not: WHICH pacing lane is paying for pixels no one sees.
static PRESENT_GLASS_SHOWN_BY: [AtomicU64; aterm_gpu::present_glass::PresentTag::COUNT] =
    [const { AtomicU64::new(0) }; aterm_gpu::present_glass::PresentTag::COUNT];
static PRESENT_GLASS_SKIPPED_BY: [AtomicU64; aterm_gpu::present_glass::PresentTag::COUNT] =
    [const { AtomicU64::new(0) }; aterm_gpu::present_glass::PresentTag::COUNT];
/// `label:count` pairs of the NON-ZERO slots, comma-joined, `none` when every
/// slot is zero — the shape of `frame_refill_full_causes`, so a reader that
/// parses one parses the other.
fn sparse_pairs(pairs: &[(&str, u64)]) -> String {
    if pairs.is_empty() {
        return "none".to_string();
    }
    pairs
        .iter()
        .map(|(label, count)| format!("{label}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The same pairs as a JSON object (`{}` when empty), the structured twin.
fn sparse_object(pairs: &[(&str, u64)]) -> String {
    let body = pairs
        .iter()
        .map(|(label, count)| format!("\"{label}\":{count}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// The non-zero entries of a per-tag ledger, in `PresentTag::ALL` order.
fn tag_pairs(
    slots: &[AtomicU64; aterm_gpu::present_glass::PresentTag::COUNT],
) -> Vec<(&'static str, u64)> {
    aterm_gpu::present_glass::PresentTag::ALL
        .iter()
        .filter_map(|tag| {
            let count = slots[tag.index()].load(Ordering::Relaxed);
            (count != 0).then_some((tag.as_str(), count))
        })
        .collect()
}

/// Install this ledger as `aterm_gpu`'s present→glass sink, once per process at
/// GUI entry. Until it runs no presented handler is registered at all.
pub(crate) fn install_present_glass_sink() {
    let _ = aterm_gpu::present_glass::install_sink(note_present_glass);
}

/// Book one presented-handler sample (runs on a Metal/CoreAnimation thread).
fn note_present_glass(report: aterm_gpu::present_glass::GlassReport) {
    match report.sample {
        aterm_gpu::present_glass::GlassSample::OnGlass { ns } => {
            H_PRESENT_GLASS.record(ns);
            LAST_PRESENT_GLASS_NS.store(ns, Ordering::Relaxed);
            MAX_PRESENT_GLASS_NS.fetch_max(ns, Ordering::Relaxed);
            PRESENT_GLASS_SHOWN_BY[report.tag.index()].fetch_add(1, Ordering::Relaxed);
        }
        aterm_gpu::present_glass::GlassSample::Skipped => {
            PRESENT_GLASS_SKIPPED.fetch_add(1, Ordering::Relaxed);
            PRESENT_GLASS_SKIPPED_BY[report.skip_attribution().index()]
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

// THE INPUT LEGS the typing audit named as unmeasured
// (docs/AUDIT-typing-to-pixels-2026-08-26.md, P1 "measure the matching key and
// matching frame"), plus the refusal count of the 2026-09-25 input contract.
//
// OUTPUT → REDRAW REQUEST. `present_*` runs from a PTY burst's leading edge to
// the frame that presents it, and until this leg existed nothing said how much
// of that was the wake reaching the event loop (the reader's post, the loop's
// queue, whatever the UI thread was busy with) and how much was pacing,
// compose and present after the loop had asked for the frame. This is the
// first half: the burst's leading edge (the same `last_output_ns` stamp) →
// the `Wake::Output` turn that admits the visible session's redraw — the
// instant a frame is owed. Booked once per burst (a second wake before the
// present finds the same stamp already booked) and only for a visible
// session: a hidden one requests nothing. The 5 s honesty cap of
// `present_latency` applies: a stamp older than that aged unwatched.
static H_OUTPUT_REDRAW_REQUEST: Histogram = Histogram::new();
static LAST_OUTPUT_REDRAW_REQUEST_NS: AtomicU64 = AtomicU64::new(0);
static MAX_OUTPUT_REDRAW_REQUEST_NS: AtomicU64 = AtomicU64::new(0);
/// The `present_latency` honesty cap, shared: see `present_latency_ns_with_plan`.
pub(crate) const OUTPUT_REDRAW_REQUEST_CAP_NS: u64 = 5_000_000_000;
// IME PREEDIT → COMMIT. How long a composition stays open, from the preedit
// that starts it to the commit that ends it — the clock a CJK or dead-key
// typist feels as "how long until my character is typed", which no key slice
// can see (the key events inside a composition write nothing). A composition
// that ends without a commit (cleared, focus lost, disabled) books no duration
// and is counted as cancelled.
static H_IME_COMPOSE: Histogram = Histogram::new();
static IME_CANCELLED: AtomicU64 = AtomicU64::new(0);
// Input REFUSED because its session's input queue was full: the program has
// stopped reading and the key (or paste) was refused — said once per session,
// `App::refuse_input` — instead of parking the event loop or queueing without
// bound (decided 2026-09-25).
static INPUT_REFUSED: AtomicU64 = AtomicU64::new(0);
// Terminal query REPLIES dropped at submission (`spawn::ReplyLane::submit`):
// the program floods queries it never reads the answers to, and the reply
// lane's byte budget or count cap is full.
static REPLY_DROPPED: AtomicU64 = AtomicU64::new(0);

/// Book one output → redraw-request span (see `H_OUTPUT_REDRAW_REQUEST`).
/// Spans past the honesty cap are discarded, never booked.
pub(crate) fn note_output_redraw_request(ns: u64) {
    if ns > OUTPUT_REDRAW_REQUEST_CAP_NS {
        return;
    }
    H_OUTPUT_REDRAW_REQUEST.record(ns);
    LAST_OUTPUT_REDRAW_REQUEST_NS.store(ns, Ordering::Relaxed);
    MAX_OUTPUT_REDRAW_REQUEST_NS.fetch_max(ns, Ordering::Relaxed);
}

/// Book one committed IME composition: preedit start → commit, in ns.
pub(crate) fn note_ime_compose(ns: u64) {
    H_IME_COMPOSE.record(ns);
}

/// A composition ended without a commit.
pub(crate) fn note_ime_cancelled() {
    IME_CANCELLED.fetch_add(1, Ordering::Relaxed);
}

/// Input was refused because its session's input queue is full: a key or
/// paste (said once per session, `App::refuse_input`), or a mouse, wheel,
/// focus or colour-scheme report (silently).
pub(crate) fn note_input_refused() {
    INPUT_REFUSED.fetch_add(1, Ordering::Relaxed);
}

/// A terminal query reply was dropped because its session's reply lane is
/// full.
pub(crate) fn note_reply_dropped() {
    REPLY_DROPPED.fetch_add(1, Ordering::Relaxed);
}

/// Replies dropped by a full reply lane since the last reset.
#[cfg(test)]
#[must_use]
pub(crate) fn reply_dropped() -> u64 {
    REPLY_DROPPED.load(Ordering::Relaxed)
}

/// Inputs refused by a full input queue since the last reset.
#[cfg(test)]
#[must_use]
pub(crate) fn input_refused() -> u64 {
    INPUT_REFUSED.load(Ordering::Relaxed)
}

/// The output→redraw-request distribution (see `H_OUTPUT_REDRAW_REQUEST`).
#[cfg(test)]
#[must_use]
pub(crate) fn output_redraw_request_distribution() -> &'static Histogram {
    &H_OUTPUT_REDRAW_REQUEST
}

/// The IME preedit→commit distribution (see `H_IME_COMPOSE`).
#[cfg(test)]
#[must_use]
pub(crate) fn ime_compose_distribution() -> &'static Histogram {
    &H_IME_COMPOSE
}

/// The input-leg fields of `metrics percentiles`, text form: the
/// output→redraw-request leg with its last/max, the IME composition clock
/// with its cancel count, and the refused-input count. One fragment, so the
/// text and JSON forms cannot drift apart.
#[must_use]
pub(crate) fn input_legs_fields_text() -> String {
    let ms = |ns: u64| ns as f64 / 1e6;
    let out = &H_OUTPUT_REDRAW_REQUEST;
    let ime = &H_IME_COMPOSE;
    let p = |h: &Histogram, q: f64| ms(h.percentile(q).unwrap_or(0));
    format!(
        " n_out_req={} out_req_p50_ms={:.2} out_req_p95_ms={:.2} out_req_p99_ms={:.2} \
         last_out_req_ms={:.2} max_out_req_ms={:.2} \
         n_ime={} ime_p50_ms={:.2} ime_p95_ms={:.2} ime_p99_ms={:.2} ime_cancelled={} \
         input_refused={} reply_dropped={}",
        out.count(),
        p(out, 0.50),
        p(out, 0.95),
        p(out, 0.99),
        ms(LAST_OUTPUT_REDRAW_REQUEST_NS.load(Ordering::Relaxed)),
        ms(MAX_OUTPUT_REDRAW_REQUEST_NS.load(Ordering::Relaxed)),
        ime.count(),
        p(ime, 0.50),
        p(ime, 0.95),
        p(ime, 0.99),
        IME_CANCELLED.load(Ordering::Relaxed),
        INPUT_REFUSED.load(Ordering::Relaxed),
        REPLY_DROPPED.load(Ordering::Relaxed),
    )
}

/// Field-for-field JSON twin of [`input_legs_fields_text`].
#[must_use]
pub(crate) fn input_legs_fields_json() -> String {
    let ms = |ns: u64| ns as f64 / 1e6;
    let out = &H_OUTPUT_REDRAW_REQUEST;
    let ime = &H_IME_COMPOSE;
    let p = |h: &Histogram, q: f64| ms(h.percentile(q).unwrap_or(0));
    format!(
        ",\"n_out_req\":{},\"out_req_p50_ms\":{:.2},\"out_req_p95_ms\":{:.2},\
         \"out_req_p99_ms\":{:.2},\"last_out_req_ms\":{:.2},\"max_out_req_ms\":{:.2},\
         \"n_ime\":{},\"ime_p50_ms\":{:.2},\"ime_p95_ms\":{:.2},\"ime_p99_ms\":{:.2},\
         \"ime_cancelled\":{},\"input_refused\":{},\"reply_dropped\":{}",
        out.count(),
        p(out, 0.50),
        p(out, 0.95),
        p(out, 0.99),
        ms(LAST_OUTPUT_REDRAW_REQUEST_NS.load(Ordering::Relaxed)),
        ms(MAX_OUTPUT_REDRAW_REQUEST_NS.load(Ordering::Relaxed)),
        ime.count(),
        p(ime, 0.50),
        p(ime, 0.95),
        p(ime, 0.99),
        IME_CANCELLED.load(Ordering::Relaxed),
        INPUT_REFUSED.load(Ordering::Relaxed),
        REPLY_DROPPED.load(Ordering::Relaxed),
    )
}

/// The present→glass fields of `metrics percentiles`, text form.
#[must_use]
pub(crate) fn present_glass_fields_text() -> String {
    let h = &H_PRESENT_GLASS;
    let ms = |ns: u64| ns as f64 / 1e6;
    let p = |q: f64| ms(h.percentile(q).unwrap_or(0));
    format!(
        " n_present_glass={} present_glass_p50_ms={:.2} present_glass_p95_ms={:.2} \
         present_glass_p99_ms={:.2} last_present_glass_ms={:.2} \
         max_present_glass_ms={:.2} present_glass_skipped={} \
         present_glass_shown_by={} present_glass_skipped_by={}",
        h.count(),
        p(0.50),
        p(0.95),
        p(0.99),
        ms(LAST_PRESENT_GLASS_NS.load(Ordering::Relaxed)),
        ms(MAX_PRESENT_GLASS_NS.load(Ordering::Relaxed)),
        PRESENT_GLASS_SKIPPED.load(Ordering::Relaxed),
        sparse_pairs(&tag_pairs(&PRESENT_GLASS_SHOWN_BY)),
        sparse_pairs(&tag_pairs(&PRESENT_GLASS_SKIPPED_BY)),
    )
}

/// Field-for-field JSON twin of [`present_glass_fields_text`].
#[must_use]
pub(crate) fn present_glass_fields_json() -> String {
    let h = &H_PRESENT_GLASS;
    let ms = |ns: u64| ns as f64 / 1e6;
    let p = |q: f64| ms(h.percentile(q).unwrap_or(0));
    format!(
        ",\"n_present_glass\":{},\"present_glass_p50_ms\":{:.2},\
         \"present_glass_p95_ms\":{:.2},\"present_glass_p99_ms\":{:.2},\
         \"last_present_glass_ms\":{:.2},\"max_present_glass_ms\":{:.2},\
         \"present_glass_skipped\":{},\"present_glass_shown_by\":{},\
         \"present_glass_skipped_by\":{}",
        h.count(),
        p(0.50),
        p(0.95),
        p(0.99),
        ms(LAST_PRESENT_GLASS_NS.load(Ordering::Relaxed)),
        ms(MAX_PRESENT_GLASS_NS.load(Ordering::Relaxed)),
        PRESENT_GLASS_SKIPPED.load(Ordering::Relaxed),
        sparse_object(&tag_pairs(&PRESENT_GLASS_SHOWN_BY)),
        sparse_object(&tag_pairs(&PRESENT_GLASS_SKIPPED_BY)),
    )
}

/// The WHEN-AND-WHOSE fields of the `metrics` summary, text form.
///
/// Spliced as one fragment (the [`present_glass_fields_text`] discipline) so
/// the text and JSON summaries cannot drift apart and neither giant `format!`
/// grows ten more positional holes.
///
/// Every `*_at_ms` is on the `now_ns` PROCESS clock — monotonic nanoseconds
/// since GUI entry, the clock `first_present_ms` already uses — and
/// `metrics_now_ms` is that clock read at this instant, so a reader converts a
/// stamp to "N ms ago" with one subtraction and never has to guess which hour
/// of a long-lived process a max belongs to.
#[must_use]
pub(crate) fn lateness_fields_text() -> String {
    let ms = |ns: u64| ns as f64 / 1e6;
    format!(
        " max_wake_late_ms={:.2} max_wake_late_owner={} max_wake_late_at_ms={:.2} \
         max_deadline_late_ms={:.2} max_deadline_late_owner={} \
         max_deadline_late_at_ms={:.2} max_present_latency_at_ms={:.2} \
         max_present_latency_gap_ms={:.2} max_input_present_at_ms={:.2} \
         metrics_now_ms={:.2}",
        ms(MAX_WAKE_LATE_NS.load(Ordering::Relaxed)),
        DeadlineOwner::from_raw(MAX_WAKE_LATE_OWNER.load(Ordering::Relaxed)).as_str(),
        ms(MAX_WAKE_LATE_AT_NS.load(Ordering::Relaxed)),
        ms(MAX_DEADLINE_LATE_NS.load(Ordering::Relaxed)),
        DeadlineOwner::from_raw(MAX_DEADLINE_LATE_OWNER.load(Ordering::Relaxed)).as_str(),
        ms(MAX_DEADLINE_LATE_AT_NS.load(Ordering::Relaxed)),
        ms(MAX_PRESENT_LATENCY_AT_NS.load(Ordering::Relaxed)),
        ms(MAX_PRESENT_LATENCY_GAP_NS.load(Ordering::Relaxed)),
        ms(MAX_INPUT_PRESENT_AT_NS.load(Ordering::Relaxed)),
        ms(now_ns()),
    )
}

/// Field-for-field JSON twin of [`lateness_fields_text`].
#[must_use]
pub(crate) fn lateness_fields_json() -> String {
    let ms = |ns: u64| ns as f64 / 1e6;
    format!(
        ",\"max_wake_late_ms\":{:.2},\"max_wake_late_owner\":\"{}\",\
         \"max_wake_late_at_ms\":{:.2},\"max_deadline_late_ms\":{:.2},\
         \"max_deadline_late_owner\":\"{}\",\"max_deadline_late_at_ms\":{:.2},\
         \"max_present_latency_at_ms\":{:.2},\"max_present_latency_gap_ms\":{:.2},\
         \"max_input_present_at_ms\":{:.2},\"metrics_now_ms\":{:.2}",
        ms(MAX_WAKE_LATE_NS.load(Ordering::Relaxed)),
        DeadlineOwner::from_raw(MAX_WAKE_LATE_OWNER.load(Ordering::Relaxed)).as_str(),
        ms(MAX_WAKE_LATE_AT_NS.load(Ordering::Relaxed)),
        ms(MAX_DEADLINE_LATE_NS.load(Ordering::Relaxed)),
        DeadlineOwner::from_raw(MAX_DEADLINE_LATE_OWNER.load(Ordering::Relaxed)).as_str(),
        ms(MAX_DEADLINE_LATE_AT_NS.load(Ordering::Relaxed)),
        ms(MAX_PRESENT_LATENCY_AT_NS.load(Ordering::Relaxed)),
        ms(MAX_PRESENT_LATENCY_GAP_NS.load(Ordering::Relaxed)),
        ms(MAX_INPUT_PRESENT_AT_NS.load(Ordering::Relaxed)),
        ms(now_ns()),
    )
}

/// One attributed input→present slice into the process-global max-fold.
fn book_input_present(d: u64) {
    LAST_INPUT_PRESENT_NS.store(d, Ordering::Relaxed);
    // Same rule as the present max: a worst case with no time of occurrence
    // cannot be correlated with anything else that happened in the process.
    if MAX_INPUT_PRESENT_NS.fetch_max(d, Ordering::Relaxed) < d {
        MAX_INPUT_PRESENT_AT_NS.store(now_ns(), Ordering::Relaxed);
    }
    H_INPUT_PRESENT.record(d);
}

/// How many unpresented keystrokes one window's [`PendingInputStamp`] holds.
/// A fast typist's burst through a multi-second stall fits; past it the OLDEST
/// edges are kept and the rest counted in `input_edges_dropped`.
pub(crate) const INPUT_EDGE_RING: usize = 32;

/// Keystrokes a full edge ring could not hold (see [`INPUT_EDGE_RING`]). Their
/// slices are missing from the input histogram; this count says how many.
static INPUT_EDGES_DROPPED: AtomicU64 = AtomicU64::new(0);

/// One unpresented keystroke routed to a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InputEdge {
    /// The key's arrival (`now_ns` clock, possibly backdated by NSEvent queue
    /// age); never 0 for a held edge.
    arrival_ns: u64,
    /// When the edge was armed — compared against the last [`reset`], because a
    /// backdated arrival can precede a reset the key itself followed.
    armed_ns: u64,
    /// The key came through the HARDWARE path (the winit `KeyboardInput` arm — a
    /// physical key, or `ctl hwkey`'s real `NSEvent`), never a control
    /// `send`/`feed`/`key`. Only such a slice is a sample for the strain engine's
    /// FELT (design §10.14, ruling 206): a controller's key is not a person
    /// waiting.
    hardware: bool,
}

/// One window's pending input→present EDGES: every unpresented keystroke routed
/// to that window, oldest first, up to [`INPUT_EDGE_RING`]. Owned by the
/// window's state (UI thread only, so plain integers), armed by
/// [`note_window_input`] and drained by that window's own content present in
/// [`record_present`] — never by another window's. See the PER-WINDOW
/// ATTRIBUTION note on `INPUT_STAMP_NS`, and COORDINATED OMISSION above the
/// histograms for why every edge is a sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PendingInputStamp {
    edges: [InputEdge; INPUT_EDGE_RING],
    len: usize,
}

impl PendingInputStamp {
    /// Arm one edge (a controller key); `false` when the ring was full.
    #[cfg(test)]
    fn arm(&mut self, arrival_ns: u64, armed_ns: u64) -> bool {
        self.arm_from(arrival_ns, armed_ns, false)
    }

    /// [`Self::arm`], saying whether the key came through the hardware path.
    /// Appends. A full ring first lets go of the edges already past
    /// `INPUT_SLICE_CAP_NS` (keys that never echoed: the drain would discard
    /// them anyway, so they must not hold the slots a fresh key needs); a ring
    /// still full keeps its oldest edges, counts this one in
    /// `input_edges_dropped` and answers `false`.
    fn arm_from(&mut self, arrival_ns: u64, armed_ns: u64, hardware: bool) -> bool {
        if self.len == INPUT_EDGE_RING {
            self.evict_aged(armed_ns);
        }
        let Some(slot) = self.edges.get_mut(self.len) else {
            INPUT_EDGES_DROPPED.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        *slot = InputEdge {
            arrival_ns: arrival_ns.max(1),
            armed_ns,
            hardware,
        };
        self.len += 1;
        true
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

    /// Whether the OLDEST pending edge was armed by a HARDWARE key.
    #[cfg(test)]
    pub(crate) const fn armed_by_hardware(self) -> bool {
        self.len != 0 && self.edges[0].hardware
    }

    /// Drain every edge at a content present of THIS window, oldest first, as
    /// `(slice, hardware)`. An edge armed before the last reset (`reset_at_ns`)
    /// or aged past `INPUT_SLICE_CAP_NS` (a keystroke that never echoed) yields
    /// nothing; the ring is empty afterwards either way.
    fn drain_attributed(&mut self, now_ns: u64, reset_at_ns: u64) -> Vec<(u64, bool)> {
        let taken = std::mem::take(self);
        taken.edges[..taken.len]
            .iter()
            .filter(|edge| edge.armed_ns >= reset_at_ns)
            .map(|edge| (now_ns.saturating_sub(edge.arrival_ns), edge.hardware))
            .filter(|&(d, _)| d <= INPUT_SLICE_CAP_NS)
            .collect()
    }

    /// The drained slices alone (tests).
    #[cfg(test)]
    fn drain_slices(&mut self, now_ns: u64, reset_at_ns: u64) -> Vec<u64> {
        self.drain_attributed(now_ns, reset_at_ns)
            .into_iter()
            .map(|(d, _)| d)
            .collect()
    }
}

/// Keystrokes the edge rings could not hold (the `input_edges_dropped` field).
#[must_use]
pub(crate) fn input_edges_dropped() -> u64 {
    INPUT_EDGES_DROPPED.load(Ordering::Relaxed)
}

/// The arrival a PTY-bound input stamps: the pending TRUE key arrival when one is
/// fresh (see [`note_input`]), else `now`.
fn input_arrival_ns(now: u64) -> u64 {
    let key = LAT_KEY_NS.load(Ordering::Relaxed);
    if key != 0 && now.saturating_sub(key) < 500_000_000 {
        key
    } else {
        now
    }
}

/// Stamp the arrival of a keystroke routed to ONE window (the
/// `App::input_to_session` seam, hardware keys and controller `key` alike) into
/// that window's [`PendingInputStamp`], so only that window's content present
/// closes the slice. Same arrival and keep-oldest rules as [`note_input`].
///
/// `hardware` is whether the key came through the winit `KeyboardInput` arm
/// (see [`PendingInputStamp`]'s field): only such a slice comes back out of
/// [`record_present`] for the strain engine.
pub(crate) fn note_window_input(pending: &mut PendingInputStamp, hardware: bool) {
    let now = now_ns();
    // A full ring counts the edge itself (`input_edges_dropped`).
    let _ = pending.arm_from(input_arrival_ns(now), now, hardware);
    note_typing_hot();
}

/// Stamp the arrival of user input bound for the PTY that names a SESSION but no
/// window (a control `send`/`feed` to a background session, a controller host
/// write). Keeps the OLDEST unpresented arrival — the worst case is the honest
/// one — so a burst does not shrink the measured slice. Any window's content
/// present closes it; window-routed keys use [`note_window_input`] instead.
///
/// INHERITS the pending TRUE key arrival when one is armed (macOS backdates it
/// by the NSEvent queue age — see [`note_key_arrival_queued`]), so
/// `input->present` includes time the keystroke spent parked in the OS event
/// queue: the drawable-park slice a stamp taken here (post-dequeue, mid-handler)
/// structurally hides. Freshness-gated to 500 ms: a key that never reached the
/// PTY (a UI shortcut leaves its stamp unconsumed — `note_pty_write` never runs)
/// must not lend its stale arrival to a later control-verb `send`.
pub(crate) fn note_input() {
    let stamp = input_arrival_ns(now_ns());
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
pub(crate) fn note_resize_arrival() {
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
pub(crate) fn note_grid_committed() {
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

/// Arm the THRU-2 INTERACTIVE-INPUT-PENDING window: a human is waiting on the UI
/// thread — a key press, a wheel/trackpad gesture, a mouse button press — so
/// the PTY reader must keep its term-lock holds FINE (and the gather its short
/// batch budget) until `TYPING_HOT_TAIL_NS` after the LAST such input. Hover
/// motion is deliberately NOT an arming event: it is not a wait, and it would
/// keep the reader on twice the lock round-trips through any mouse movement.
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
pub(crate) fn note_typing_hot() {
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
pub(crate) fn input_pending() -> bool {
    let until = TYPING_HOT_UNTIL_NS.load(Ordering::Relaxed);
    until != 0 && now_ns() < until
}

/// TEST SEAM: the raw interactive-input-pending deadline (ns), so a handler test
/// can pin "this event ARMED the hint" as a monotone advance of the stamp —
/// robust to other tests arming it concurrently, unlike a bare `input_pending()`.
#[cfg(test)]
pub(crate) fn interactive_input_deadline_ns() -> u64 {
    TYPING_HOT_UNTIL_NS.load(Ordering::Relaxed)
}

// ---- The latency trace: isolate the UI-thread key->write component -----------
// `note_input` now runs BEFORE encode/write, so `input_present` already spans the
// full key-arrival → content-present path (including encode and the PTY write).
// This trace keeps an independently useful key→write slice: it identifies UI
// dispatch/encoding/lock/write stalls inside a high end-to-end sample — but it
// starts at the BACKDATED hardware arrival, so the OS event queue's share is
// inside it too, and a high reading ALONE cannot say which of the two it was.
// That share is published apart as `key_queue_*` (see `key_queue_distribution`),
// and `key_write - key_queue` is the UI-dispatch slice this trace is named for.
// Do NOT add it to `input_present`; it is a contained component of that total.
// Logging is a development seam ([`latency_trace_on`]); the cheap metrics atomics
// remain always on.
static LAT_TRACE_ON: OnceLock<bool> = OnceLock::new();
static LAT_KEY_NS: AtomicU64 = AtomicU64::new(0);
// Rolling worst-case key->write (µs) since the last `metrics` read, for the verb.
static MAX_KEY_WRITE_NS: AtomicU64 = AtomicU64::new(0);
static LAST_KEY_WRITE_NS: AtomicU64 = AtomicU64::new(0);
// The OS-EVENT-QUEUE share of that reading: the backdate `note_key_arrival_queued`
// applied, kept so `key->write` splits back into "waited to be dequeued" and
// "aterm's own dispatch". See `key_queue_distribution`.
static LAST_KEY_QUEUE_NS: AtomicU64 = AtomicU64::new(0);
static MAX_KEY_QUEUE_NS: AtomicU64 = AtomicU64::new(0);

/// Whether the latency stderr traces are armed: the per-key `KEYWRITE` line here
/// and the per-present `output->present` line (`App::trace_latency`). A
/// DEVELOPMENT seam (`$ATERM_TRACE_LATENCY`, [`aterm_types::dev_seam!`]): a shipped
/// binary never logs them, and the `metrics` verb publishes the same numbers in
/// every build.
pub(crate) fn latency_trace_on() -> bool {
    *LAT_TRACE_ON.get_or_init(|| aterm_types::dev_seam!("ATERM_TRACE_LATENCY").is_some())
}

/// Public µs view of the shared metrics clock — the SAME epoch as
/// [`note_key_arrival`], so the VIDEO introspection's frame stamps and its
/// input stamps subtract validly (key→frame latency by construction).
#[must_use]
pub(crate) fn now_us() -> u64 {
    now_ns() / 1000
}

/// Stamp the TRUE arrival of a printable hardware keystroke (winit
/// `KeyboardInput`, before encode/write), BACKDATED by `queue_ns` — the time the
/// key's NSEvent sat in the OS event queue before dispatch (macOS and X11:
/// `platform::current_event_queue_age_ns`; 0 elsewhere). The stamp lands at the
/// HARDWARE arrival, so `key->write` (and `note_input`'s inherited
/// `input->present` clock) finally includes queueing a parked event loop
/// inflicted — the slice the touch-to-glass audit proved every instrument was
/// blind to. Overwrites (keep-LATEST) so each key is measured against its OWN
/// write, never accumulating across keys.
pub(crate) fn note_key_arrival_queued(queue_ns: u64) {
    LAT_KEY_NS.store(now_ns().saturating_sub(queue_ns).max(1), Ordering::Relaxed);
    // AND PUBLISH THE BACKDATE ITSELF. The stamp above folds the queue wait into
    // every downstream total; this is the only record of how much it folded in,
    // and without it `key_write` cannot be split back into the parked loop and
    // aterm's own dispatch. See [`key_queue_distribution`].
    H_KEY_QUEUE.record(queue_ns);
    LAST_KEY_QUEUE_NS.store(queue_ns, Ordering::Relaxed);
    MAX_KEY_QUEUE_NS.fetch_max(queue_ns, Ordering::Relaxed);
}

/// Disarm the key-arrival stamp: called after a key's dispatch ends WITHOUT a
/// PTY write (`note_pty_write` never consumed it), so a later control-verb
/// `send` can never inherit an unrelated key's arrival through [`note_input`].
/// A no-op after a writing key (the stamp is already 0).
pub(crate) fn clear_key_arrival() {
    LAT_KEY_NS.store(0, Ordering::Relaxed);
}

/// The current key's TRUE hardware arrival in ms — the same backdated stamp
/// [`note_key_arrival_queued`] wrote — as a PEEK; [`note_pty_write`] owns the
/// consuming swap. `None` when unarmed (a bare modifier, an IME run, a control
/// verb's `send`, or a key whose write already ran).
///
/// This is the MELODY'S clock (the rainbow-kitty keyed seam stamps its cue
/// with it): two keys must be heard the interval apart that the fingers
/// struck them, not the interval their PTY writes happened to return in. It
/// exists because the write's `swap(0)` runs BEFORE the audio push on the key
/// path, so the push cannot read the arrival itself — the peek is taken at
/// the cue's mint, while the stamp is still armed. Same epoch and unit as
/// `app_render::input_clock_ms` (the metrics clock in ms, never `0`), so the
/// two seams keep measuring one clock. One relaxed load per key.
#[must_use]
pub(crate) fn key_arrival_ms() -> Option<u32> {
    match LAT_KEY_NS.load(Ordering::Relaxed) {
        0 => None,
        ns => Some(((ns / 1_000_000) as u32).max(1)),
    }
}

/// The PTY write just RETURNED (after the — on Windows blocking — `WriteFile` and
/// the press-path `term_lock`s). Record the key→write COMPONENT already contained
/// by the end-to-end `input_present` clock. Always recorded (cheap atomics) so the
/// `metrics` verb can surface it; also logged under [`latency_trace_on`].
pub(crate) fn note_pty_write() {
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
pub(crate) fn take_key_arrival() -> u64 {
    LAT_KEY_NS.swap(0, Ordering::Relaxed)
}

/// Record a key→write slice against an arrival stamp claimed earlier by
/// [`take_key_arrival`] — for writes that complete OFF the UI thread. A `0` stamp
/// (no key behind this write: a control verb, an already-consumed arrival) records
/// nothing, exactly as the inline path's early return does.
pub(crate) fn note_pty_write_at(key: u64) {
    if key == 0 {
        return;
    }
    let d = now_ns().saturating_sub(key);
    LAST_KEY_WRITE_NS.store(d, Ordering::Relaxed);
    MAX_KEY_WRITE_NS.fetch_max(d, Ordering::Relaxed);
    H_KEY_WRITE.record(d);
    if latency_trace_on() {
        crate::logging::stderr_line!(
            "KEYWRITE key->write={:.2}ms (last OS queue {:.2}ms + UI encode + term_locks \
             + blocking WriteFile)",
            d as f64 / 1_000_000.0,
            LAST_KEY_QUEUE_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0
        );
    }
}

/// A DEC-2026 hold episode armed (the FALSE→TRUE rising edge saw its first held
/// frame).
pub(crate) fn note_sync_armed() {
    SYNC_HOLDS_ARMED.fetch_add(1, Ordering::Relaxed);
}

/// An armed episode released because the app ended sync (`?2026l`) — the healthy
/// release cause.
pub(crate) fn note_sync_release_end() {
    SYNC_RELEASES_END.fetch_add(1, Ordering::Relaxed);
}

/// An armed episode released by the safety-valve deadline — the app went silent
/// (or the hold machinery mis-paced) after `?2026h`. Climbing during ordinary
/// typing = the SYNC-1 failure class.
pub(crate) fn note_sync_release_timeout() {
    SYNC_RELEASES_TIMEOUT.fetch_add(1, Ordering::Relaxed);
}

/// Per-window lease behind the process-wide synchronized-output hold gauge.
///
/// A last-writer boolean is incorrect with multiple windows (or multiple pane
/// releases): one release can overwrite a sibling's still-active hold. Each
/// window instead contributes at most one count, idempotently, and dropping a
/// window releases its contribution automatically.
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
        transition_sync_holding(&mut self.held, held, &SYNC_HOLDING_WINDOWS);
    }

    #[cfg(test)]
    pub(crate) fn is_held_for_test(&self) -> bool {
        self.held
    }
}

impl Default for SyncHoldingGauge {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SyncHoldingGauge {
    fn drop(&mut self) {
        transition_sync_holding(&mut self.held, false, &SYNC_HOLDING_WINDOWS);
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

/// The `perf_reduced` load-shed latch flipped (called on the EDGE only).
pub(crate) fn note_shed_transition(reduced: bool) {
    PERF_REDUCED.store(reduced, Ordering::Relaxed);
    SHED_TRANSITIONS.fetch_add(1, Ordering::Relaxed);
}

/// A lost `Wake::Output` was healed by the self-expiring latch.
pub(crate) fn note_wake_heal() {
    WAKE_HEALS.fetch_add(1, Ordering::Relaxed);
}

/// Record one redraw pass's TOTAL wall time (entry → present done OR failed
/// surface transaction), which includes the acquire/present wait `frame_render`
/// deliberately excludes. Failed attempts are load-bearing evidence: omitting
/// them makes the exact redraw responsible for a stall disappear from metrics.
pub(crate) fn record_redraw_total(total_ns: u64) {
    LAST_REDRAW_TOTAL_NS.store(total_ns, Ordering::Relaxed);
    MAX_REDRAW_TOTAL_NS.fetch_max(total_ns, Ordering::Relaxed);
}

/// A `RedrawRequested` entered the real redraw pipeline.
pub(crate) fn note_redraw_attempt() {
    REDRAW_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
}

/// The stable RepaintKey/predictor/effect gate proved that no pixels changed.
pub(crate) fn note_redraw_early_out() {
    REDRAW_EARLY_OUTS.fetch_add(1, Ordering::Relaxed);
}

/// The presented-path frame extraction completed: which arm refilled the
/// snapshot (`Scoped` retains undamaged rows; `Full` is the fallback walk), and
/// on the full arm, WHICH continuity clause refused.
///
/// The scoped arm's cost is unchanged (one `fetch_add`, as before); the full
/// arm pays a second `fetch_add` on a line indexed by the cause's discriminant,
/// which is noise beside the O(rows x cols) re-extract it just did.
pub(crate) fn note_frame_refill(refill: aterm_core::render::FrameRefill) {
    match refill {
        aterm_core::render::FrameRefill::Scoped { .. } => {
            FRAME_REFILLS_SCOPED.fetch_add(1, Ordering::Relaxed);
        }
        aterm_core::render::FrameRefill::Full { cause } => {
            FRAME_REFILLS_FULL.fetch_add(1, Ordering::Relaxed);
            // `index()` is the discriminant and `ALL` is pinned to be dense and
            // in index order by aterm-core's own test, so this cannot be out of
            // bounds; `get` keeps that a fact rather than a panic anyway.
            if let Some(slot) = FRAME_REFILL_FULL_BY_CAUSE.get(cause.index()) {
                slot.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// A presented frame REUSED the existing snapshot: the engine had marked no
/// damage and processed no bytes since the snapshot was filled, so re-extracting
/// the grid under the terminal mutex would have reproduced it exactly.
///
/// Counted so the reuse gate can never hide work instead of avoiding it —
/// `frame_refills_scoped + frame_refills_full + frame_refills_skipped` is still
/// one per presented non-rescan frame.
pub(crate) fn note_frame_refill_skipped() {
    FRAME_REFILLS_SKIPPED.fetch_add(1, Ordering::Relaxed);
}

/// One resident-pet world observation that STOOD, and the cells it classified.
///
/// Called wherever the walk really runs — the live presented path, the
/// composed-focus path, and the explicit-capture path — because all three pay
/// it and a counter that saw only one of them would read as a saving whenever
/// the work merely moved. So `pet_world_observations` is NOT bounded by
/// `frames`: a capture-heavy interval legitimately exceeds it.
///
/// A refused observation reports zero cells and is still counted: a walk that
/// ran and was thrown away is cost, not absence.
pub(crate) fn note_pet_world_observation(cells: usize) {
    PET_WORLD_OBSERVATIONS.fetch_add(1, Ordering::Relaxed);
    PET_WORLD_CELLS.fetch_add(cells as u64, Ordering::Relaxed);
}

/// One cursor-effect tick's output: the `glow_add` quads, the `glow_under`
/// quads and the trail cells it composed for the frame (see the `FX_*`
/// statics). Only a non-empty `glow_under` stream books an under frame.
pub(crate) fn note_fx_composed(glow_quads: usize, under_quads: usize, trail_cells: usize) {
    FX_GLOW_QUADS_LAST.store(glow_quads as u64, Ordering::Relaxed);
    FX_UNDER_QUADS_LAST.store(under_quads as u64, Ordering::Relaxed);
    FX_TRAIL_CELLS_LAST.store(trail_cells as u64, Ordering::Relaxed);
    if under_quads > 0 {
        FX_UNDER_FRAMES.fetch_add(1, Ordering::Relaxed);
    }
}

/// A synchronized-output hold intentionally retained the previous frame.
pub(crate) fn note_redraw_sync_hold() {
    REDRAW_SYNC_HOLDS.fetch_add(1, Ordering::Relaxed);
}

/// A redraw source fired while surface recovery was backed off or parked. This
/// is cheap and expected for an occasional animation tick; sustained growth
/// identifies the caller that is still requesting redraws, without repeating
/// grid extraction or surface acquisition.
pub(crate) fn note_redraw_retry_gated() {
    REDRAW_RETRY_GATED.fetch_add(1, Ordering::Relaxed);
}

/// A redraw reached the surface-acquire seam. `compose_ns` includes every
/// pre-acquire operation, notably full `Terminal::cell_frame_into` extraction.
pub(crate) fn note_pre_present(compose_ns: u64) {
    PRE_PRESENT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    LAST_PRE_PRESENT_NS.store(compose_ns, Ordering::Relaxed);
    PRE_PRESENT_TOTAL_NS.fetch_add(compose_ns, Ordering::Relaxed);
    MAX_PRE_PRESENT_NS.fetch_max(compose_ns, Ordering::Relaxed);
    H_PRE_PRESENT.record(compose_ns);
}

/// The surface transaction failed after a redraw reached the present seam.
pub(crate) fn note_present_drop(reason: PresentDropReason, parked: bool) {
    PRESENT_DROPS.fetch_add(1, Ordering::Relaxed);
    update_present_drop_disposition(reason, parked);
    // OCCLUSION/PARK taint (item 10). Only drops that stop the present stream
    // for an UNBOUNDED interval count: occluded glass, or a drop the retry
    // scheduler parked awaiting an external stimulus. A transient
    // `CpuAcquire`/`GpuTimeout` that retries a millisecond later is a stall the
    // user genuinely felt and MUST stay in the on-glass distribution — taint it
    // and the honesty pass would delete the very samples it exists to protect.
    if drop_taints_present_latency(reason, parked) {
        arm_present_taint();
    }
}

/// Update the live disposition of an already-counted dropped frame. Recovery
/// can discover a more specific downstream cause (for example CPU fallback
/// construction after a lost GPU) without pretending that the same frame was
/// dropped twice.
pub(crate) fn update_present_drop_disposition(reason: PresentDropReason, parked: bool) {
    LAST_PRESENT_DROP_REASON.store(reason as u64, Ordering::Relaxed);
    LAST_PRESENT_DROP_PARKED.store(parked, Ordering::Relaxed);
}

/// Publish the event loop's selected deadline. `None` means pure `Wait`.
///
/// Returns the deadline the caller should actually arm: normally the input,
/// unchanged — clamped to `now + STALE_ARM_HEAL_FLOOR` only when the SAME
/// owner arms a deadline more than the floor in the past on consecutive turns
/// (see the `STALE_ARM_HEALS` statics' note), and clamped to
/// `now + PAST_ARM_STREAK_CLAMP` (one display frame) when > 90% of that
/// owner's last [`PAST_ARM_WINDOW`] arms were already past regardless of HOW
/// late (the windowed detector's note above `PAST_ARM_HISTORY_BY_OWNER`; the
/// stronger of the two clamps wins). A single late arm passes through
/// untouched: a busy turn legitimately computes a deadline that is already
/// behind it, and healing that would delay real work. The recorded
/// `deadline_late_ms`/`past_deadline_arms` always describe the REQUESTED
/// deadline, so the diagnostics stay honest about what the producer asked for
/// even on healed turns.
#[must_use = "the RETURNED instant is the healed one — arming the argument re-arms the stale deadline this fn exists to clamp"]
pub(crate) fn record_deadline(
    owner: DeadlineOwner,
    deadline: Option<Instant>,
    now: Instant,
) -> Option<Instant> {
    let clock_now = now_ns();
    let mut armed = deadline;
    let (owner, due, late) = match deadline {
        None => {
            // A pure-Wait turn breaks any streak: consecutiveness is the bug's
            // signature, and it just ended.
            STALE_ARM_STREAK_OWNER.store(DeadlineOwner::None as u64, Ordering::Relaxed);
            STALE_ARM_EPISODE.store(false, Ordering::Relaxed);
            (DeadlineOwner::None, 0, 0)
        }
        Some(deadline) if deadline >= now => {
            let ahead = u64::try_from(deadline.duration_since(now).as_nanos()).unwrap_or(u64::MAX);
            STALE_ARM_STREAK_OWNER.store(DeadlineOwner::None as u64, Ordering::Relaxed);
            STALE_ARM_EPISODE.store(false, Ordering::Relaxed);
            note_owner_arm(owner, false);
            // A healthy arm feeds the owner's 32-arm window too: it dilutes a
            // past streak instead of resetting it, which is exactly what makes
            // this detector immune to the `late ≈ 0`/"a nanosecond in the
            // future" blip that clears the consecutive-turn latch above.
            let _ = note_past_arm_window(owner, false);
            (owner, clock_now.saturating_add(ahead), 0)
        }
        Some(deadline) => {
            let late = u64::try_from(now.duration_since(deadline).as_nanos()).unwrap_or(u64::MAX);
            PAST_DEADLINE_ARMS.fetch_add(1, Ordering::Relaxed);
            note_owner_arm(owner, true);
            if late > STALE_ARM_HEAL_FLOOR_NS && owner as u64 != DeadlineOwner::None as u64 {
                let streak = STALE_ARM_STREAK_OWNER.swap(owner as u64, Ordering::Relaxed);
                if streak == owner as u64 {
                    STALE_ARM_HEALS.fetch_add(1, Ordering::Relaxed);
                    armed = Some(now + STALE_ARM_HEAL_FLOOR);
                    if !STALE_ARM_EPISODE.swap(true, Ordering::Relaxed) {
                        aterm_log::warn!(
                            "healed a stale deadline arm: owner={} late {} ms on consecutive \
                             turns — clamped to now+{} ms (scheduler bug; see stale_arm_heals)",
                            owner.as_str(),
                            late / 1_000_000,
                            STALE_ARM_HEAL_FLOOR_NS / 1_000_000
                        );
                    }
                }
            } else if late <= STALE_ARM_HEAL_FLOOR_NS {
                STALE_ARM_STREAK_OWNER.store(DeadlineOwner::None as u64, Ordering::Relaxed);
                STALE_ARM_EPISODE.store(false, Ordering::Relaxed);
            }
            // The WINDOWED per-owner detector (items 18/19): fires when > 90%
            // of this owner's last 32 arms were already past, at ANY lateness
            // — the `late ≈ 0` spin the floor above cannot see, and the
            // alternating-owner spin its single streak slot cannot see. Clamp
            // this owner's re-arm to one display frame ahead unless a stronger
            // heal (the 250 ms floor above) already moved it further out; the
            // clamped arms are counted in the owner's NAMED streak-heal ledger.
            if note_past_arm_window(owner, true) {
                let clamp = now + PAST_ARM_STREAK_CLAMP;
                if armed.is_none_or(|current| current < clamp) {
                    armed = Some(clamp);
                    note_past_arm_streak_heal(owner);
                }
            }
            (owner, clock_now.saturating_sub(late), late)
        }
    };
    LAST_DEADLINE_OWNER.store(owner as u64, Ordering::Relaxed);
    LAST_DEADLINE_DUE_NS.store(due, Ordering::Relaxed);
    LAST_DEADLINE_LATE_NS.store(late, Ordering::Relaxed);
    // The same last-writer erasure as the wake lateness: every event-loop turn
    // arms a deadline, so this store is overwritten within a frame. Keep the
    // worst, the owner that asked for it, and when it was asked.
    if late != 0 && MAX_DEADLINE_LATE_NS.fetch_max(late, Ordering::Relaxed) < late {
        MAX_DEADLINE_LATE_OWNER.store(owner as u64, Ordering::Relaxed);
        MAX_DEADLINE_LATE_AT_NS.store(clock_now, Ordering::Relaxed);
    }
    armed
}

/// Book one armed deadline against the owner that WON the fold, and the past
/// subset separately. Kept next to the global counters it disambiguates so the
/// two can never drift: every path that bumps `PAST_DEADLINE_ARMS` bumps this.
fn note_owner_arm(owner: DeadlineOwner, past: bool) {
    // `owner as u64` is a discriminant of this enum, so the index is in range
    // for any variant the slot count covers; the guard keeps a future variant
    // added without bumping `DEADLINE_OWNER_SLOTS` from panicking a release
    // build's event loop (the unit test below fails loudly instead).
    let Some(slot) = usize::try_from(owner as u64)
        .ok()
        .filter(|i| *i < DEADLINE_OWNER_SLOTS)
    else {
        return;
    };
    DEADLINE_ARMS_BY_OWNER[slot].fetch_add(1, Ordering::Relaxed);
    if past {
        PAST_DEADLINE_ARMS_BY_OWNER[slot].fetch_add(1, Ordering::Relaxed);
    }
}

/// Push one arm into the owner's windowed past/future history and report
/// whether the streak detector fires (see the `PAST_ARM_HISTORY_BY_OWNER`
/// note). Single-writer state — only the event-loop thread records deadlines —
/// so the load/store pair is not a torn read-modify-write in practice, and
/// Relaxed matches every other counter on this path.
fn note_past_arm_window(owner: DeadlineOwner, past: bool) -> bool {
    if matches!(owner, DeadlineOwner::None) {
        return false;
    }
    let Some(slot) = usize::try_from(owner as u64)
        .ok()
        .filter(|i| *i < DEADLINE_OWNER_SLOTS)
    else {
        return false;
    };
    let packed = PAST_ARM_HISTORY_BY_OWNER[slot].load(Ordering::Relaxed);
    let (next, trigger) = past_arm_window_step(packed, past);
    PAST_ARM_HISTORY_BY_OWNER[slot].store(next, Ordering::Relaxed);
    trigger
}

/// Book one arm the WINDOWED streak detector actually clamped, against its
/// owner. Kept per-owner (not one global counter) for the same reason as
/// `deadline_arms_by_owner`: a heal that cannot name its producer sends the
/// investigation to the wrong module.
fn note_past_arm_streak_heal(owner: DeadlineOwner) {
    let Some(slot) = usize::try_from(owner as u64)
        .ok()
        .filter(|i| *i < DEADLINE_OWNER_SLOTS)
    else {
        return;
    };
    PAST_ARM_STREAK_HEALS_BY_OWNER[slot].fetch_add(1, Ordering::Relaxed);
}

/// One owner's arm ledger since the last [`reset`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnerArms {
    /// The stable wire label (`DeadlineOwner::as_str`).
    pub owner: &'static str,
    /// Deadlines this owner won the `about_to_wait` min-fold with.
    pub arms: u64,
    /// The subset of those already in the past when armed — the spin signature,
    /// now attributable to ONE producer instead of the whole event loop.
    pub past_arms: u64,
}

/// One continuity clause's refusal ledger since the last [`reset`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RefillCause {
    /// The stable wire label (`FullRefillCause::as_str`).
    pub cause: &'static str,
    /// Presented-path frames the full arm refused under this clause.
    pub frames: u64,
}

/// Per-clause attribution of the damage-scoped refill's FULL arm, NON-ZERO
/// CLAUSES ONLY — the follow-up a78dd8a1 deferred when it wired DMG-1 into the
/// shipping frontend with only a scoped/full split.
///
/// Same shape and same reasoning as [`deadline_arm_attribution`]: sparse, so a
/// healthy instance prints one or two pairs and a degraded one puts its culprit
/// in plain sight, and deliberately NOT a [`Snapshot`] field (a `Copy` snapshot
/// read per introspection call should not grow an array whose only consumer is
/// this attribution).
#[must_use]
pub(crate) fn frame_refill_full_causes() -> Vec<RefillCause> {
    aterm_core::render::FullRefillCause::ALL
        .iter()
        .filter_map(|cause| {
            let frames = FRAME_REFILL_FULL_BY_CAUSE
                .get(cause.index())
                .map_or(0, |slot| slot.load(Ordering::Relaxed));
            (frames != 0).then_some(RefillCause {
                cause: cause.as_str(),
                frames,
            })
        })
        .collect()
}

/// Per-owner arm attribution, NON-ZERO OWNERS ONLY (item 6).
///
/// Deliberately not a [`Snapshot`] field: `snapshot()` is `Copy` and read per
/// Settings ROW, so folding a 36-entry pair table into it would put ~576 bytes
/// and 72 atomic loads on a path that wants one bool. Callers that want the
/// attribution ask for it.
///
/// Sparse output keeps the wire field small on a healthy instance — an idle
/// terminal arms two or three owners — and makes a spin's producer the FIRST
/// thing a reader sees rather than a needle in 36 zeroes.
#[must_use]
pub(crate) fn deadline_arm_attribution() -> Vec<OwnerArms> {
    (0..DEADLINE_OWNER_SLOTS)
        .filter_map(|slot| {
            let arms = DEADLINE_ARMS_BY_OWNER[slot].load(Ordering::Relaxed);
            let past_arms = PAST_DEADLINE_ARMS_BY_OWNER[slot].load(Ordering::Relaxed);
            (arms != 0 || past_arms != 0).then(|| OwnerArms {
                owner: DeadlineOwner::from_raw(slot as u64).as_str(),
                arms,
                past_arms,
            })
        })
        .collect()
}

/// Per-owner ledger of arms the WINDOWED streak detector clamped
/// (`(owner label, heals)`; see the `PAST_ARM_HISTORY_BY_OWNER` note).
/// Sparse — non-zero owners only — and surfaced beside
/// `deadline_arms_by_owner` for the same reason that field exists: ANY
/// non-zero entry names a producer that kept re-arming the past across a full
/// 32-arm window, i.e. a live scheduler bug degraded to frame cadence.
#[must_use]
pub(crate) fn past_arm_streak_heal_attribution() -> Vec<(&'static str, u64)> {
    (0..DEADLINE_OWNER_SLOTS)
        .filter_map(|slot| {
            let heals = PAST_ARM_STREAK_HEALS_BY_OWNER[slot].load(Ordering::Relaxed);
            (heals != 0).then(|| (DeadlineOwner::from_raw(slot as u64).as_str(), heals))
        })
        .collect()
}

/// Per-owner timer FIRES — wakes a deadline actually caused — sparse, non-zero
/// owners only; the twin of [`deadline_arm_attribution`], which counts arms.
#[must_use]
pub(crate) fn deadline_fire_attribution() -> Vec<(&'static str, u64)> {
    (0..DEADLINE_OWNER_SLOTS)
        .filter_map(|slot| {
            let fires = DEADLINE_FIRES_BY_OWNER[slot].load(Ordering::Relaxed);
            (fires != 0).then(|| (DeadlineOwner::from_raw(slot as u64).as_str(), fires))
        })
        .collect()
}

/// Count one delivered proxy event by its `Wake` variant. The variant's name is
/// read ONCE from its `Debug` spelling (the leading identifier) and kept; every
/// later delivery of the variant is a lookup by discriminant.
pub(crate) fn note_user_wake(ev: &crate::Wake) {
    let d = std::mem::discriminant(ev);
    let mut seen = WAKES_BY_VARIANT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = seen.iter_mut().find(|e| e.0 == d) {
        entry.2 = entry.2.saturating_add(1);
        return;
    }
    let spelled = format!("{ev:?}");
    let name: String = spelled
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    // Bounded: one short string per variant, for the life of the process.
    let name: &'static str = Box::leak(name.into_boxed_str());
    seen.push((d, name, 1));
}

/// The delivered proxy events by `Wake` variant since the last reset, busiest
/// first, zero entries omitted.
#[must_use]
pub(crate) fn user_wake_attribution() -> Vec<(&'static str, u64)> {
    let mut out: Vec<(&'static str, u64)> = WAKES_BY_VARIANT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|e| e.2 != 0)
        .map(|e| (e.1, e.2))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out
}

/// Store the GPU renderer's cumulative repaint-scope counters (main thread,
/// after each GPU present).
pub(crate) fn note_gpu_repaint_totals(scissor_taken: u64, full_repaints: u64, scroll_rescues: u64) {
    GPU_SCISSOR_TAKEN.store(scissor_taken, Ordering::Relaxed);
    GPU_FULL_REPAINTS.store(full_repaints, Ordering::Relaxed);
    GPU_SCROLL_RESCUES.store(scroll_rescues, Ordering::Relaxed);
}

/// `(scissor_taken, full_repaints, scroll_rescues)` since the last reset: the
/// presents that took the scissored dirty-row path, the full repaints, and the
/// rigid scrolls the band blit rescued (a subset of the scissored ones). A
/// renderer rebuilt since the reset counts from its own start.
#[must_use]
pub(crate) fn gpu_repaint_counts() -> (u64, u64, u64) {
    let since = |total: &AtomicU64, base: &AtomicU64| {
        total
            .load(Ordering::Relaxed)
            .saturating_sub(base.load(Ordering::Relaxed))
    };
    (
        since(&GPU_SCISSOR_TAKEN, &GPU_SCISSOR_TAKEN_BASE),
        since(&GPU_FULL_REPAINTS, &GPU_FULL_REPAINTS_BASE),
        since(&GPU_SCROLL_RESCUES, &GPU_SCROLL_RESCUES_BASE),
    )
}

/// Book one main-loop turn to its root's distribution (`watchdog`'s census).
pub(crate) fn note_turn(root: u8, span_ns: u64) {
    if let Some(h) = H_TURN_BY_ROOT.get(usize::from(root)) {
        h.record(span_ns);
    }
}

/// The turn distribution of root `root` (`watchdog::Breadcrumb as u8`).
#[must_use]
pub(crate) fn turn_distribution(root: u8) -> Option<&'static Histogram> {
    H_TURN_BY_ROOT.get(usize::from(root))
}

/// Record why winit began this event-loop iteration and attribute a timer wake
/// to the owner published by the preceding `about_to_wait` pass.
pub(crate) fn note_event_wake(kind: EventWakeKind) {
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
            if let Some(fires) = DEADLINE_FIRES_BY_OWNER.get(owner as usize) {
                fires.fetch_add(1, Ordering::Relaxed);
            }
            let due = LAST_DEADLINE_DUE_NS.load(Ordering::Relaxed);
            let at = now_ns();
            let late = if due == 0 { 0 } else { at.saturating_sub(due) };
            LAST_WAKE_LATE_NS.store(late, Ordering::Relaxed);
            // …AND THE WORST ONE, WHICH THE NEXT WAKE MAY NOT ERASE. The store
            // above is exactly how a 400 ms late timer used to vanish: the
            // `WaitCancelled` behind it — one per PTY output burst — stores 0
            // over it before anyone can read it. Single writer (the event
            // loop), so whichever sample wins the fold stamps its own owner
            // and instant.
            if late != 0 && MAX_WAKE_LATE_NS.fetch_max(late, Ordering::Relaxed) < late {
                MAX_WAKE_LATE_OWNER.store(owner as u64, Ordering::Relaxed);
                MAX_WAKE_LATE_AT_NS.store(at, Ordering::Relaxed);
            }
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
pub(crate) fn set_backend_gpu(on: bool) {
    BACKEND_GPU.store(on, Ordering::Relaxed);
}

/// THE UPDATE'S PRE-COMMIT INPUT, AS NUMBERS (gap #33). Input typed into a
/// successor's revealed window waits in a bounded queue until Commit; every
/// event that queue REFUSED is a lost keystroke, counted here as it happens.
static HANDOFF_INPUT_DROPPED: AtomicU64 = AtomicU64::new(0);
/// …and every replay that had to DISARM the ambient modifiers because the
/// queue could not be replayed as a coherent key stream: a drop broke a
/// press/release pair, or a window lost focus with input already queued for
/// it. Before gap #33 this fired on every handoff (the owner's log: 17 of 17),
/// because winit's own startup `Focused(false)` for each new window marked it;
/// a non-zero reading now means a replay was really repaired.
static HANDOFF_INPUT_INCOHERENT: AtomicU64 = AtomicU64::new(0);

/// Serializes the tests that move or read the two handoff input counters: they
/// are process-wide, and an exact reading must not race a sibling's drop.
/// EVERY test that overflows the pre-Commit queue or settles an incoherent
/// replay holds it — the Tier-1 replays in `precommit_input_conformance`
/// included, which move both.
#[cfg(test)]
pub(crate) static HANDOFF_INPUT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Count one pre-Commit input event the successor's queue refused.
pub(crate) fn note_handoff_input_dropped() {
    HANDOFF_INPUT_DROPPED.fetch_add(1, Ordering::Relaxed);
}

/// Count one pre-Commit replay whose modifiers had to be disarmed.
pub(crate) fn note_handoff_input_incoherent() {
    HANDOFF_INPUT_INCOHERENT.fetch_add(1, Ordering::Relaxed);
}

/// `(input_dropped, input_incoherent)`, cumulative for this process.
#[must_use]
pub(crate) fn handoff_input_counts() -> (u64, u64) {
    (
        HANDOFF_INPUT_DROPPED.load(Ordering::Relaxed),
        HANDOFF_INPUT_INCOHERENT.load(Ordering::Relaxed),
    )
}

/// The summary line's handoff-input fragment, leading space included:
/// ` input_dropped=<n> input_incoherent=<n>`. CUMULATIVE and deliberately NOT
/// cleared by [`reset`]: they describe the one handoff this process adopted, at
/// its birth, and a `metrics reset` taken afterwards must not erase a lost
/// keystroke.
#[must_use]
pub(crate) fn handoff_input_fields_text() -> String {
    let (dropped, incoherent) = handoff_input_counts();
    format!(" input_dropped={dropped} input_incoherent={incoherent}")
}

/// Field-for-field JSON twin of [`handoff_input_fields_text`], with a leading
/// comma.
#[must_use]
pub(crate) fn handoff_input_fields_json() -> String {
    let (dropped, incoherent) = handoff_input_counts();
    format!(",\"input_dropped\":{dropped},\"input_incoherent\":{incoherent}")
}

/// Zero the measurement-window stats (frame count, maxima, slow count) so a driver
/// can time a SPECIFIC operation: `metrics reset`, run the workload, then `metrics`.
/// Keeps `backend` and legacy momentary `last_*` readings. The redraw-audit
/// `last_pre_present` and drop disposition are measurement-window evidence, so
/// those clear with their counters. Resetting also removes the cold-start spike
/// from maxima, making the worst case reflect steady state.
pub(crate) fn reset() {
    FRAMES_PRESENTED.store(0, Ordering::Relaxed);
    MAX_PRESENT_LATENCY_NS.store(0, Ordering::Relaxed);
    // A max's instant, the frame gap that qualifies it, and the open slow-present
    // episode are all observations OF THE WINDOW: they clear with it, so a fresh
    // window's first spike is logged and never carries a stale stamp. The
    // episode is the UI thread's to close, with its line, at its next present,
    // so the log never shows a run that simply stops.
    MAX_PRESENT_LATENCY_AT_NS.store(0, Ordering::Relaxed);
    MAX_PRESENT_LATENCY_GAP_NS.store(0, Ordering::Relaxed);
    SLOW_EPISODE_RESET.store(true, Ordering::Relaxed);
    MAX_FRAME_RENDER_NS.store(0, Ordering::Relaxed);
    SLOW_FRAMES.store(0, Ordering::Relaxed);
    SYNC_HOLDS_ARMED.store(0, Ordering::Relaxed);
    SYNC_RELEASES_END.store(0, Ordering::Relaxed);
    SYNC_RELEASES_TIMEOUT.store(0, Ordering::Relaxed);
    SHED_TRANSITIONS.store(0, Ordering::Relaxed);
    MAX_INPUT_PRESENT_NS.store(0, Ordering::Relaxed);
    MAX_INPUT_PRESENT_AT_NS.store(0, Ordering::Relaxed);
    INPUT_EDGES_DROPPED.store(0, Ordering::Relaxed);
    MAX_KEY_WRITE_NS.store(0, Ordering::Relaxed);
    WAKE_HEALS.store(0, Ordering::Relaxed);
    MAX_REDRAW_TOTAL_NS.store(0, Ordering::Relaxed);
    REDRAW_ATTEMPTS.store(0, Ordering::Relaxed);
    REDRAW_EARLY_OUTS.store(0, Ordering::Relaxed);
    REDRAW_SYNC_HOLDS.store(0, Ordering::Relaxed);
    REDRAW_RETRY_GATED.store(0, Ordering::Relaxed);
    FRAME_REFILLS_SCOPED.store(0, Ordering::Relaxed);
    FRAME_REFILLS_FULL.store(0, Ordering::Relaxed);
    FRAME_REFILLS_SKIPPED.store(0, Ordering::Relaxed);
    PET_WORLD_OBSERVATIONS.store(0, Ordering::Relaxed);
    PET_WORLD_CELLS.store(0, Ordering::Relaxed);
    FX_UNDER_FRAMES.store(0, Ordering::Relaxed);
    for slot in &FRAME_REFILL_FULL_BY_CAUSE {
        slot.store(0, Ordering::Relaxed);
    }
    PRE_PRESENT_ATTEMPTS.store(0, Ordering::Relaxed);
    LAST_PRE_PRESENT_NS.store(0, Ordering::Relaxed);
    PRE_PRESENT_TOTAL_NS.store(0, Ordering::Relaxed);
    MAX_PRE_PRESENT_NS.store(0, Ordering::Relaxed);
    PRESENT_DROPS.store(0, Ordering::Relaxed);
    // The tainted ledger is a window stat like its clean twin. `CAPTURE_DEPTH`
    // is LIVE STATE, not an observation: a reset taken mid-recording must not
    // pretend the recording ended, or the rest of it lands in the clean
    // histogram. `PRESENT_TAINT_UNTIL_NS` survives for the same reason — the
    // episode it describes is still in progress.
    CAPTURE_EPISODES.store(0, Ordering::Relaxed);
    TAINTED_PRESENT_SAMPLES.store(0, Ordering::Relaxed);
    LAST_TAINTED_PRESENT_LATENCY_NS.store(0, Ordering::Relaxed);
    MAX_TAINTED_PRESENT_LATENCY_NS.store(0, Ordering::Relaxed);
    H_PRESENT_LATENCY_TAINTED.reset();
    LAST_PRESENT_DROP_REASON.store(0, Ordering::Relaxed);
    LAST_PRESENT_DROP_PARKED.store(false, Ordering::Relaxed);
    EVENT_WAKES.store(0, Ordering::Relaxed);
    for entry in WAKES_BY_VARIANT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter_mut()
    {
        entry.2 = 0;
    }
    GPU_SCISSOR_TAKEN_BASE.store(GPU_SCISSOR_TAKEN.load(Ordering::Relaxed), Ordering::Relaxed);
    GPU_FULL_REPAINTS_BASE.store(GPU_FULL_REPAINTS.load(Ordering::Relaxed), Ordering::Relaxed);
    GPU_SCROLL_RESCUES_BASE.store(
        GPU_SCROLL_RESCUES.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    for h in &H_TURN_BY_ROOT {
        h.reset();
    }
    TIMER_WAKES.store(0, Ordering::Relaxed);
    WAIT_CANCELLED_WAKES.store(0, Ordering::Relaxed);
    POLL_WAKES.store(0, Ordering::Relaxed);
    PAST_DEADLINE_ARMS.store(0, Ordering::Relaxed);
    // The lateness maxima are window stats like every other `max_`: a driver
    // that resets, drives a workload and reads must see THIS workload's worst
    // wake, not the launch storm's.
    MAX_WAKE_LATE_NS.store(0, Ordering::Relaxed);
    MAX_WAKE_LATE_OWNER.store(0, Ordering::Relaxed);
    MAX_WAKE_LATE_AT_NS.store(0, Ordering::Relaxed);
    MAX_DEADLINE_LATE_NS.store(0, Ordering::Relaxed);
    MAX_DEADLINE_LATE_OWNER.store(0, Ordering::Relaxed);
    MAX_DEADLINE_LATE_AT_NS.store(0, Ordering::Relaxed);
    for slot in 0..DEADLINE_OWNER_SLOTS {
        DEADLINE_ARMS_BY_OWNER[slot].store(0, Ordering::Relaxed);
        PAST_DEADLINE_ARMS_BY_OWNER[slot].store(0, Ordering::Relaxed);
        DEADLINE_FIRES_BY_OWNER[slot].store(0, Ordering::Relaxed);
    }
    // The heal counter clears like `wake_heals`; the streak/episode latches are
    // live detection state, reset so a fresh window re-logs a still-live spin.
    STALE_ARM_HEALS.store(0, Ordering::Relaxed);
    STALE_ARM_STREAK_OWNER.store(0, Ordering::Relaxed);
    STALE_ARM_EPISODE.store(false, Ordering::Relaxed);
    // The windowed detector's ledger AND history clear together: the counters
    // are window stats, and the packed histories are detection state whose
    // stale 32-arm windows would otherwise convict the fresh measurement
    // window with the previous one's evidence.
    for slot in 0..DEADLINE_OWNER_SLOTS {
        PAST_ARM_HISTORY_BY_OWNER[slot].store(0, Ordering::Relaxed);
        PAST_ARM_STREAK_HEALS_BY_OWNER[slot].store(0, Ordering::Relaxed);
    }
    // Frame-gap window: clear the max AND the previous-present stamp so the idle
    // gap straddling the reset is never counted (the next present starts fresh).
    MAX_FRAME_GAP_NS.store(0, Ordering::Relaxed);
    LAST_PRESENT_STAMP_NS.store(0, Ordering::Relaxed);
    // A stale no-echo stamp must not leak a bogus slice into the fresh window.
    // Per-window stamps live in `WindowState`; the epoch discards them at consume.
    INPUT_STAMP_NS.store(0, Ordering::Relaxed);
    INPUT_RESET_AT_NS.store(now_ns(), Ordering::Relaxed);
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
    LAST_ACQUIRE_QUEUE_NS.store(0, Ordering::Relaxed);
    MAX_ACQUIRE_QUEUE_NS.store(0, Ordering::Relaxed);
    LAST_GPU_PARK_NS.store(0, Ordering::Relaxed);
    MAX_GPU_PARK_NS.store(0, Ordering::Relaxed);
    H_ACQUIRE_WAIT.reset();
    H_ACQUIRE_QUEUE.reset();
    for site in TermWaitSite::ALL {
        H_TERM_WAIT[site as usize].reset();
        MAX_TERM_WAIT_NS[site as usize].store(0, Ordering::Relaxed);
    }
    LAST_INPUT_PRESENT_NS.store(0, Ordering::Relaxed);
    LAST_RESIZE_PRESENT_NS.store(0, Ordering::Relaxed);
    LAST_RESIZE_REFLOW_NS.store(0, Ordering::Relaxed);
    LAST_KEY_WRITE_NS.store(0, Ordering::Relaxed);
    LAST_KEY_QUEUE_NS.store(0, Ordering::Relaxed);
    MAX_KEY_QUEUE_NS.store(0, Ordering::Relaxed);
    LAST_PRESENT_LATENCY_NS.store(0, Ordering::Relaxed);
    LAST_FRAME_RENDER_NS.store(0, Ordering::Relaxed);
    LAST_REDRAW_TOTAL_NS.store(0, Ordering::Relaxed);
    // The distributions are window stats like the maxima they generalize.
    H_INPUT_PRESENT.reset();
    H_PRESENT_LATENCY.reset();
    H_FRAME_RENDER.reset();
    H_KEY_WRITE.reset();
    H_KEY_QUEUE.reset();
    H_PRE_PRESENT.reset();
    H_RESIZE_PRESENT.reset();
    H_RESIZE_REFLOW.reset();
    H_PRESENT_GLASS.reset();
    LAST_PRESENT_GLASS_NS.store(0, Ordering::Relaxed);
    MAX_PRESENT_GLASS_NS.store(0, Ordering::Relaxed);
    PRESENT_GLASS_SKIPPED.store(0, Ordering::Relaxed);
    for slot in &PRESENT_GLASS_SHOWN_BY {
        slot.store(0, Ordering::Relaxed);
    }
    for slot in &PRESENT_GLASS_SKIPPED_BY {
        slot.store(0, Ordering::Relaxed);
    }
    H_OUTPUT_REDRAW_REQUEST.reset();
    LAST_OUTPUT_REDRAW_REQUEST_NS.store(0, Ordering::Relaxed);
    MAX_OUTPUT_REDRAW_REQUEST_NS.store(0, Ordering::Relaxed);
    H_IME_COMPOSE.reset();
    IME_CANCELLED.store(0, Ordering::Relaxed);
    INPUT_REFUSED.store(0, Ordering::Relaxed);
    REPLY_DROPPED.store(0, Ordering::Relaxed);
    // The main-loop turn census is a window stat like every other `max_` on this
    // line: a driver that resets, drives a workload and reads must see THAT
    // workload's worst main-thread turn, not the launch storm's.
    crate::watchdog::reset_turn_census();
    crate::strain_host::reset_metrics();
    // Gauges (`SYNC_HOLDING`, `PERF_REDUCED`), legacy momentary `last_*`
    // readings, and `first_present` (a startup FACT, not a window stat) survive
    // a reset, like `backend`. Redraw-audit last/drop fields intentionally reset
    // above with their diagnostic window.
}

/// A consistent-enough read of the counters for the `metrics` control verb.
#[derive(Clone, Copy)]
pub(crate) struct Snapshot {
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
    /// Presented-path refills that rode the damage-scoped arm (DMG-1) vs the
    /// full O(rows×cols) fallback — see the statics' note.
    pub frame_refills_scoped: u64,
    pub frame_refills_full: u64,
    /// Presented frames that reused the existing snapshot outright — no
    /// extraction, no terminal-mutex grid walk — because the engine had not
    /// moved since the fill. See [`note_frame_refill_skipped`].
    pub frame_refills_skipped: u64,
    /// Resident-pet world observations and the cells they classified — the
    /// effects lane's one unconditional O(window) per-frame walk, made
    /// readable from a live instance. `pet_world_cells / pet_world_observations`
    /// is the walk's width; `pet_world_observations / frames` is how many
    /// presented frames paid for it. See [`note_pet_world_observation`].
    pub pet_world_observations: u64,
    pub pet_world_cells: u64,
    /// The compose-side prim counters: the last cursor-effect tick's
    /// `glow_add` quads, `glow_under` quads and trail cells, and how many
    /// ticks composed a `glow_under` (ribbon) stream. See [`note_fx_composed`].
    pub fx_glow_quads_last: u64,
    pub fx_under_quads_last: u64,
    pub fx_trail_cells_last: u64,
    pub fx_under_frames: u64,
    /// OFFSCREEN rasterizations (the `image` / `window` / `snapshot`
    /// introspection path): how many, the last one's cost, and the worst.
    ///
    /// [`record_offscreen_raster`] has maintained all three since the on-glass
    /// ledger was cleaned of them — correctly, because an `image` between two
    /// presents shrinks a real hitch out of existence and one taken long after
    /// the last present books a phantom gap. But the counters it moved that
    /// work INTO reached no surface, so the observability its own doc promises
    /// ("keep headless/introspection runs observable") was never delivered:
    /// a full-frame raster moved no published number at all, and the cure had
    /// removed the contamination and the visibility together.
    ///
    /// Deliberately NOT folded back into `frames` / `max_frame_render_ms` /
    /// `slow_frames` / `max_frame_gap_ms` — that contamination is the bug these
    /// were split off to fix.
    pub offscreen_rasters: u64,
    pub last_offscreen_raster_ns: u64,
    pub max_offscreen_raster_ns: u64,
    pub pre_present_attempts: u64,
    pub last_pre_present_ns: u64,
    pub pre_present_total_ns: u64,
    pub max_pre_present_ns: u64,
    /// Swapchain-acquire (`nextDrawable`) park — the LAST sample and the WORST
    /// one in this measurement window. See [`note_acquire_wait`].
    ///
    /// A METRIC THAT EXISTS BUT IS NEVER PUBLISHED HIDES A WHOLE CLASS OF STALL
    /// (2026-08 draw-path audit, tier-1 item 3). Both statics were declared,
    /// written on every present and cleared by `reset` from the day the slice was
    /// instrumented — and NO snapshot read them, so the only acquire figures any
    /// reader could get were `H_ACQUIRE_WAIT`'s percentiles. That is precisely
    /// the shape this stall does not show up in: one 200 ms park among thousands
    /// of ~0.02 ms samples cannot move a p99, and a blocked acquire is the
    /// largest known macOS typing stall (it parks the winit main thread while
    /// keyDowns queue in the OS event queue, measured at up to ~84 ms). A max
    /// cannot miss it.
    pub last_acquire_wait_ns: u64,
    pub max_acquire_wait_ns: u64,
    /// Armed-Metal GPU park — the LAST sample and the WORST one in this
    /// measurement window. See [`note_gpu_park`]. Zero on every arm that never
    /// blocks the UI thread on GPU completion.
    pub last_gpu_park_ns: u64,
    pub max_gpu_park_ns: u64,
    pub present_drops: u64,
    /// Output→present samples diverted to the TAINTED ledger because the window
    /// was occluded/parked or a capture was pacing presents. A non-zero value
    /// is why `frames` exceeds `n_present`; see the `PRESENT_TAINT_UNTIL_NS`
    /// block. These never touch `last_/max_present_latency_ns`.
    pub tainted_present_samples: u64,
    pub last_tainted_present_latency_ns: u64,
    pub max_tainted_present_latency_ns: u64,
    /// `video` capture episodes opened since reset. Any non-zero value means a
    /// capture-based instrument ran inside this measurement window — read every
    /// number in it knowing the recorder was in the loop.
    pub capture_episodes: u64,
    /// True while a capture is live RIGHT NOW (a gauge, so it survives `reset`).
    pub capture_active: bool,
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
    /// Consecutive same-owner past-deadline arms that [`record_deadline`]
    /// clamped to `now + STALE_ARM_HEAL_FLOOR`. ANY non-zero value means a
    /// scheduler armed a self-rearming `WaitUntil(past)` spin and the fold
    /// healed it — worth investigating even though the loop survived.
    pub stale_arm_heals: u64,
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
    /// GUI entry → the FIRST window's actual reveal (0 until a window is on
    /// glass) — L1's time-to-VISIBLE, the number the eye measures at launch.
    /// On a warm Windows launch the reveal precedes the backend join, so this
    /// runs well under `first_present_ns`; on an overlap-handoff boot it can
    /// legally EXCEED it (carried pixels present first, reveal after). Read
    /// live from its own stamp, so it needs no present to publish. Survives
    /// `reset`.
    pub first_visible_ns: u64,
    /// Shipped one-binary Rust entry → the same reveal instant (0 until
    /// revealed, and unavailable in the thin GUI binary). THE L1 acceptance
    /// number: warm-launch target well under 150 ms against a
    /// `rust_main_to_first_present_ms` of ~440. Survives `reset`.
    pub rust_main_to_first_visible_ns: u64,
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
    /// Exclusive drill-down of `startup_attach_backend_finalize_ns` — the
    /// backend-build worker. `startup_worker_valid` is false until the worker's
    /// spawn/done stamps, its leg transaction, and the join bracket all exist
    /// and reconcile. See the `BACKEND_WORKER_SPAWN` block for what the two
    /// halves (`overlap` vs `after_join`) actually answer.
    pub startup_worker_schema: u64,
    pub startup_worker_valid: bool,
    /// Worker spawn → worker publish: the build's own wall time, which is NOT
    /// `backend_finalize_ns` (most of it runs concurrently with launch).
    pub startup_worker_total_ns: u64,
    /// The part of the worker that had already run when the join was reached.
    pub startup_worker_overlap_ns: u64,
    /// The part still outstanding at the join — the ONLY part more overlap
    /// could win back.
    pub startup_worker_after_join_ns: u64,
    /// The part of `backend_finalize_ns` that is not the worker: thread handoff
    /// plus `finalize_backend`'s own post-join setup.
    pub startup_worker_post_join_ns: u64,
    pub startup_worker_prelude_ns: u64,
    pub startup_worker_gpu_build_ns: u64,
    pub startup_worker_font_admit_ns: u64,
    pub startup_worker_font_apply_ns: u64,
    pub startup_worker_font_seal_ns: u64,
    /// Derived remainder closing the legs above against
    /// `startup_worker_total_ns`.
    pub startup_worker_epilogue_ns: u64,
    /// Renderer-side split of `startup_worker_gpu_build_ns`, from
    /// `aterm_gpu::startup_probe`. `startup_gpu_valid` is false on any CPU-backend
    /// launch (no GPU leg ran) and whenever the legs do not fit their parent.
    pub startup_gpu_schema: u64,
    pub startup_gpu_valid: bool,
    pub startup_gpu_instance_ns: u64,
    pub startup_gpu_adapter_ns: u64,
    pub startup_gpu_device_ns: u64,
    pub startup_gpu_context_tail_ns: u64,
    /// PARALLEL leg: the font thread's own wall time. Overlaps the GPU legs by
    /// design, so it is NOT part of the exclusive partition — compare it against
    /// `startup_gpu_font_join_ns`, which is what the GPU leg actually paid.
    pub startup_gpu_font_thread_ns: u64,
    pub startup_gpu_font_join_ns: u64,
    /// `GpuRenderer::from_parts` — the FOUR pipelines a launch builds (the three
    /// cell pipelines every frame binds, plus the bloom composite). Parent of
    /// every `startup_gpu_pipe_*` field.
    ///
    /// It was THIRTEEN until the nine effect-only cell pipelines went
    /// demand-driven (`aterm_gpu::EffectPipeline`): on a dx12 Windows launch
    /// those nine measured 136.13 ms of a 174.43 ms total, all of it on
    /// time-to-first-present, for effects the shipped defaults never draw.
    pub startup_gpu_pipelines_ns: u64,
    pub startup_gpu_pipe_shader_ns: u64,
    pub startup_gpu_pipe_uniform_atlas_ns: u64,
    pub startup_gpu_pipe_cell_ns: u64,
    pub startup_gpu_pipe_blit_ns: u64,
    pub startup_gpu_pipe_tray_ns: u64,
    pub startup_gpu_pipe_bloom_ns: u64,
    pub startup_gpu_pipe_vbuf_ns: u64,
    pub startup_gpu_pipe_tail_ns: u64,
    /// Derived remainder closing the GPU legs against
    /// `startup_worker_gpu_build_ns`.
    pub startup_gpu_tail_ns: u64,
    /// Per-cell-pipeline split of `startup_gpu_pipe_cell_ns`, in
    /// `aterm_gpu::startup_probe::CELL_PIPELINE_NAMES` order.
    pub startup_gpu_cell_pipeline_ns: [u64; aterm_gpu::startup_probe::CELL_PIPELINE_COUNT],
    /// How many EFFECT-only cell pipelines this process has compiled on demand,
    /// and the wall time it spent (`aterm_gpu::startup_probe::effect_build_ledger`).
    ///
    /// NOT part of the `startup_gpu_*` partition and deliberately outside its
    /// reconciliation: those legs close against `startup_worker_gpu_build_ns` and
    /// are first-write-wins cold-build facts, whereas this ledger ACCUMULATES for
    /// the life of the process — an effect switched on at minute ten books a
    /// build here long after the startup partition sealed.
    ///
    /// `0 / 0` is the healthy reading for a default launch and is the standing
    /// proof of the demand-driven design: the nine effect pipelines cost
    /// **136.13 ms of every launch** while they were built eagerly, for pixels a
    /// `cursor_trail = false` config never draws.
    pub effect_pipeline_builds: u64,
    pub effect_pipeline_build_ns: u64,
    /// WHICH slots were built, as a bitmask over `aterm_gpu::EffectPipeline as
    /// usize` (indexes `aterm_gpu::EFFECT_PIPELINE_NAMES`). The count says a
    /// launch paid for something; only this says what.
    pub effect_pipeline_built_mask: u64,
}

/// Read the current counters (lock-free).
#[must_use]
pub(crate) fn snapshot() -> Snapshot {
    let deadline_due = LAST_DEADLINE_DUE_NS.load(Ordering::Relaxed);
    let frames_presented = FRAMES_PRESENTED.load(Ordering::Acquire);
    // Read the OnceLock only after acquiring the publication counter. Once a
    // caller observes frame 1, it must also observe the startup sample that
    // record_present initialized before its Release increment.
    let startup = STARTUP_PRESENT.get().copied().unwrap_or_default();
    // The worker partition is derived LIVE rather than baked into
    // STARTUP_PRESENT: every stamp it needs is set long before the first
    // present (the join precedes the first frame by construction), and reading
    // them here keeps the immutable present-anchored fact one struct smaller.
    let (join_entry, join_exit) =
        INITIAL_ATTACH_MILESTONES
            .get()
            .copied()
            .map_or((None, None), |milestones| {
                let (entry, exit) = milestones.backend_finalize_bounds();
                (Some(entry), Some(exit))
            });
    let worker = derive_startup_worker(
        BACKEND_WORKER_SPAWN.get().copied(),
        BACKEND_WORKER_DONE.get().copied(),
        BACKEND_WORKER_LEGS.get().copied(),
        join_entry,
        join_exit,
    );
    let gpu = derive_startup_gpu(worker.gpu_build_ns);
    // Read STRAIGHT from the probe, outside `derive_startup_gpu`: the demand-build
    // ledger is a running total for the life of the process, not a cold-build leg,
    // so it must not be folded into (or invalidated by) the startup partition's
    // reconciliation against `gpu_build_ns`.
    let (effect_builds, effect_build_ns, effect_built_mask) =
        aterm_gpu::startup_probe::effect_build_ledger();
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
        sync_holding: SYNC_HOLDING_WINDOWS.load(Ordering::Relaxed) != 0,
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
        frame_refills_scoped: FRAME_REFILLS_SCOPED.load(Ordering::Relaxed),
        frame_refills_full: FRAME_REFILLS_FULL.load(Ordering::Relaxed),
        frame_refills_skipped: FRAME_REFILLS_SKIPPED.load(Ordering::Relaxed),
        pet_world_observations: PET_WORLD_OBSERVATIONS.load(Ordering::Relaxed),
        pet_world_cells: PET_WORLD_CELLS.load(Ordering::Relaxed),
        fx_glow_quads_last: FX_GLOW_QUADS_LAST.load(Ordering::Relaxed),
        fx_under_quads_last: FX_UNDER_QUADS_LAST.load(Ordering::Relaxed),
        fx_trail_cells_last: FX_TRAIL_CELLS_LAST.load(Ordering::Relaxed),
        fx_under_frames: FX_UNDER_FRAMES.load(Ordering::Relaxed),
        offscreen_rasters: OFFSCREEN_RASTERS.load(Ordering::Relaxed),
        last_offscreen_raster_ns: LAST_OFFSCREEN_RASTER_NS.load(Ordering::Relaxed),
        max_offscreen_raster_ns: MAX_OFFSCREEN_RASTER_NS.load(Ordering::Relaxed),
        pre_present_attempts: PRE_PRESENT_ATTEMPTS.load(Ordering::Relaxed),
        last_pre_present_ns: LAST_PRE_PRESENT_NS.load(Ordering::Relaxed),
        pre_present_total_ns: PRE_PRESENT_TOTAL_NS.load(Ordering::Relaxed),
        max_pre_present_ns: MAX_PRE_PRESENT_NS.load(Ordering::Relaxed),
        last_acquire_wait_ns: LAST_ACQUIRE_WAIT_NS.load(Ordering::Relaxed),
        max_acquire_wait_ns: MAX_ACQUIRE_WAIT_NS.load(Ordering::Relaxed),
        last_gpu_park_ns: LAST_GPU_PARK_NS.load(Ordering::Relaxed),
        max_gpu_park_ns: MAX_GPU_PARK_NS.load(Ordering::Relaxed),
        present_drops: PRESENT_DROPS.load(Ordering::Relaxed),
        tainted_present_samples: TAINTED_PRESENT_SAMPLES.load(Ordering::Relaxed),
        last_tainted_present_latency_ns: LAST_TAINTED_PRESENT_LATENCY_NS.load(Ordering::Relaxed),
        max_tainted_present_latency_ns: MAX_TAINTED_PRESENT_LATENCY_NS.load(Ordering::Relaxed),
        capture_episodes: CAPTURE_EPISODES.load(Ordering::Relaxed),
        capture_active: CAPTURE_DEPTH.load(Ordering::Relaxed) != 0,
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
        stale_arm_heals: STALE_ARM_HEALS.load(Ordering::Relaxed),
        max_frame_gap_ns: MAX_FRAME_GAP_NS.load(Ordering::Relaxed),
        first_present_ns: startup.gui_entry_ns,
        rust_main_to_first_present_ns: startup.rust_main_ns,
        first_visible_ns: first_visible_since(PROCESS_START.get().copied()),
        rust_main_to_first_visible_ns: first_visible_since(RUST_MAIN_START.get().copied()),
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
        startup_worker_schema: STARTUP_WORKER_SCHEMA,
        startup_worker_valid: worker.valid,
        startup_worker_total_ns: worker.total_ns,
        startup_worker_overlap_ns: worker.overlap_ns,
        startup_worker_after_join_ns: worker.after_join_ns,
        startup_worker_post_join_ns: worker.post_join_ns,
        startup_worker_prelude_ns: worker.prelude_ns,
        startup_worker_gpu_build_ns: worker.gpu_build_ns,
        startup_worker_font_admit_ns: worker.font_admit_ns,
        startup_worker_font_apply_ns: worker.font_apply_ns,
        startup_worker_font_seal_ns: worker.font_seal_ns,
        startup_worker_epilogue_ns: worker.epilogue_ns,
        startup_gpu_schema: STARTUP_GPU_SCHEMA,
        startup_gpu_valid: gpu.valid,
        startup_gpu_instance_ns: gpu.instance_ns,
        startup_gpu_adapter_ns: gpu.adapter_ns,
        startup_gpu_device_ns: gpu.device_ns,
        startup_gpu_context_tail_ns: gpu.context_tail_ns,
        startup_gpu_font_thread_ns: gpu.font_thread_ns,
        startup_gpu_font_join_ns: gpu.font_join_ns,
        startup_gpu_pipelines_ns: gpu.pipelines_ns,
        startup_gpu_pipe_shader_ns: gpu.pipe_shader_ns,
        startup_gpu_pipe_uniform_atlas_ns: gpu.pipe_uniform_atlas_ns,
        startup_gpu_pipe_cell_ns: gpu.pipe_cell_ns,
        startup_gpu_pipe_blit_ns: gpu.pipe_blit_ns,
        startup_gpu_pipe_tray_ns: gpu.pipe_tray_ns,
        startup_gpu_pipe_bloom_ns: gpu.pipe_bloom_ns,
        startup_gpu_pipe_vbuf_ns: gpu.pipe_vbuf_ns,
        startup_gpu_pipe_tail_ns: gpu.pipe_tail_ns,
        startup_gpu_tail_ns: gpu.tail_ns,
        startup_gpu_cell_pipeline_ns: gpu.cell_pipeline_ns,
        effect_pipeline_builds: effect_builds,
        effect_pipeline_build_ns: effect_build_ns,
        effect_pipeline_built_mask: effect_built_mask,
    }
}

/// `record_deadline` and `reset` mutate PROCESS-GLOBAL state — the stale-arm
/// streak latch, the per-owner arm counters, and the windowed past-arm
/// histories — so the tests that drive them cannot interleave: one test's
/// healthy arm clears another's streak, both book arms into the same tables,
/// and an unserialized `reset()` erases arms another test just booked (the
/// pre-2026-08-25 flake in `arms_are_attributed_to_the_owner_that_armed_them`).
/// Serialize the drivers. At module scope because the drivers span
/// `startup_phase_tests` (reset) and `histogram_tests` (record_deadline).
#[cfg(test)]
static SCHEDULER_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod claim_timeline_tests {
    use super::{
        ClaimStamp, ClaimTimelineInputs, StartupAttachMilestones, claim_timeline_points,
        claim_worker_vs_join, render_claim_timeline_log,
    };
    use std::time::{Duration, Instant};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The shape of a real handoff boot (the 24802 run's segments), with the
    /// worker finishing before the join.
    fn a_boot() -> ClaimTimelineInputs {
        let exec = Instant::now();
        let dial = exec + ms(31);
        let claim = dial + ms(5);
        let mut own = [None; ClaimStamp::COUNT];
        own[ClaimStamp::Intake as usize] = Some(claim + ms(12));
        own[ClaimStamp::Adopt0 as usize] = Some(claim + ms(99));
        own[ClaimStamp::EventLoopBuilt as usize] = Some(claim + ms(60));
        own[ClaimStamp::FirstPresent as usize] = Some(claim + ms(203));
        own[ClaimStamp::RestoreStart as usize] = Some(claim + ms(204));
        own[ClaimStamp::RestoreDrained as usize] = Some(claim + ms(219));
        own[ClaimStamp::Proof as usize] = Some(claim + ms(223));
        let attach = [110, 115, 135, 140, 141, 160, 170].map(|n| claim + ms(n));
        ClaimTimelineInputs {
            claim,
            exec: Some(exec),
            dial: Some(dial),
            own,
            worker_spawn: Some(claim + ms(32)),
            worker_done: Some(claim + ms(90)),
            run_app: Some(claim + ms(106)),
            resumed: Some(claim + ms(108)),
            attach: Some(StartupAttachMilestones::new(attach)),
            surface_ready: Some(claim + ms(172)),
            visible: Some(claim + ms(205)),
        }
    }

    #[test]
    fn every_boundary_is_signed_ms_after_the_claim_in_boot_order() {
        let points = claim_timeline_points(&a_boot());
        let labels: Vec<_> = points.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            [
                "main",
                "dial",
                "intake",
                "worker_spawn",
                "event_loop",
                "adopt0",
                "run_app",
                "resumed",
                "attach",
                "window_create",
                "window_created",
                "join",
                "worker_done",
                "joined",
                "surface_create",
                "surface_created",
                "surface_ready",
                "first_present",
                "visible",
                "restore",
                "drained",
                "proof",
            ]
        );
        let at = |label: &str| points.iter().find(|(l, _)| *l == label).unwrap().1;
        // The Rust main and the dial PRECEDE the claim, so they read negative.
        assert_eq!(at("main"), -36_000);
        assert_eq!(at("dial"), -5_000);
        assert_eq!(at("intake"), 12_000);
        // Every one of the seven attach points is printed, the last included.
        assert_eq!(at("surface_create"), 160_000);
        assert_eq!(at("surface_created"), 170_000);
        assert_eq!(at("proof"), 223_000);
    }

    #[test]
    fn a_boundary_never_reached_is_left_out_not_printed_as_zero() {
        let mut boot = a_boot();
        boot.own[ClaimStamp::RestoreStart as usize] = None;
        boot.own[ClaimStamp::RestoreDrained as usize] = None;
        boot.attach = None;
        let points = claim_timeline_points(&boot);
        for gone in [
            "restore",
            "drained",
            "attach",
            "join",
            "joined",
            "surface_created",
        ] {
            assert!(points.iter().all(|(l, _)| *l != gone), "{gone}");
        }
        assert!(claim_worker_vs_join(&boot).is_none());
    }

    #[test]
    fn the_worker_is_judged_against_the_join_on_both_sides() {
        let boot = a_boot();
        // Done at +90, join entered at +140 and left at +141.
        assert_eq!(claim_worker_vs_join(&boot), Some((1_000, 50_000)));
        let line = render_claim_timeline_log(&boot);
        assert!(
            line.starts_with("main -36.0, dial -5.0, intake +12.0,"),
            "{line}"
        );
        assert!(
            line.ends_with("the backend join waited 1.0 ms, the worker done 50.0 ms before it"),
            "{line}"
        );
        // A worker still running at the join is AFTER it (the negative control).
        let mut late = boot;
        late.worker_done = Some(boot.claim + ms(141));
        let line = render_claim_timeline_log(&late);
        assert!(line.ends_with("the worker done 1.0 ms after it"), "{line}");
    }
}

#[cfg(test)]
mod startup_worker_tests {
    use super::{StartupGpuSample, StartupWorkerLegs, close_startup_gpu, derive_startup_worker};
    use std::time::{Duration, Instant};

    /// Legs summing to 30 ms, the shape a real cold launch produces (the seal
    /// dominates, the GPU build is next, everything else is noise).
    fn legs() -> StartupWorkerLegs {
        StartupWorkerLegs {
            prelude_ns: 1_000_000,
            gpu_build_ns: 12_000_000,
            font_admit_ns: 2_000_000,
            font_apply_ns: 1_000_000,
            font_seal_ns: 14_000_000,
        }
    }

    #[test]
    fn a_worker_still_running_at_the_join_splits_both_intervals_exactly() {
        let spawn = Instant::now();
        let join_entry = spawn + Duration::from_millis(20);
        let done = join_entry + Duration::from_millis(10);
        let join_exit = done + Duration::from_millis(1);
        let sample = derive_startup_worker(
            Some(spawn),
            Some(done),
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        );
        assert!(sample.valid);
        assert_eq!(sample.total_ns, 30_000_000);
        assert_eq!(sample.overlap_ns, 20_000_000);
        assert_eq!(sample.after_join_ns, 10_000_000);
        assert_eq!(sample.post_join_ns, 1_000_000);
        // The two identities the whole drill-down rests on.
        assert_eq!(sample.overlap_ns + sample.after_join_ns, sample.total_ns);
        assert_eq!(sample.after_join_ns + sample.post_join_ns, 11_000_000);
        // Legs plus the derived epilogue close against the worker's wall time.
        assert_eq!(sample.epilogue_ns, 0);
    }

    #[test]
    fn a_worker_finished_before_the_join_costs_the_join_nothing() {
        // The measured macOS shape today: the whole build is hidden, so the
        // join waits on NOTHING and `after_join` must read exactly zero.
        let spawn = Instant::now();
        let done = spawn + Duration::from_millis(30);
        let join_entry = done + Duration::from_millis(5);
        let join_exit = join_entry + Duration::from_micros(10);
        let sample = derive_startup_worker(
            Some(spawn),
            Some(done),
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        );
        assert!(sample.valid);
        assert_eq!(sample.after_join_ns, 0);
        assert_eq!(sample.overlap_ns, sample.total_ns);
        assert_eq!(sample.post_join_ns, 10_000);
    }

    #[test]
    fn a_missing_stamp_or_leg_transaction_publishes_nothing() {
        let spawn = Instant::now();
        let join_entry = spawn + Duration::from_millis(20);
        let done = join_entry + Duration::from_millis(10);
        let join_exit = done + Duration::from_millis(1);
        // Each row drops exactly one input; every one must publish nothing.
        for (spawn, done, legs, entry, exit) in [
            (
                None,
                Some(done),
                Some(legs()),
                Some(join_entry),
                Some(join_exit),
            ),
            (
                Some(spawn),
                None,
                Some(legs()),
                Some(join_entry),
                Some(join_exit),
            ),
            (
                Some(spawn),
                Some(done),
                None,
                Some(join_entry),
                Some(join_exit),
            ),
            (Some(spawn), Some(done), Some(legs()), None, Some(join_exit)),
            (
                Some(spawn),
                Some(done),
                Some(legs()),
                Some(join_entry),
                None,
            ),
        ] {
            let sample = derive_startup_worker(spawn, done, legs, entry, exit);
            assert!(!sample.valid);
            assert_eq!(sample.total_ns, 0);
        }
    }

    #[test]
    fn legs_that_overrun_the_worker_refuse_to_publish_a_partition() {
        // A leg longer than the whole worker means the stamps disagree; an
        // honest "no data" beats an epilogue that would have to be negative.
        let spawn = Instant::now();
        let join_entry = spawn + Duration::from_millis(1);
        let done = join_entry + Duration::from_millis(1);
        let join_exit = done + Duration::from_millis(1);
        let sample = derive_startup_worker(
            Some(spawn),
            Some(done),
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        );
        assert!(!sample.valid);
    }

    /// A probe read whose legs sum to 12 ms — the parent `gpu_build_ns` a
    /// reconciling sample must be closed against.
    fn probe() -> StartupGpuSample {
        StartupGpuSample {
            valid: true,
            instance_ns: 10_000,
            adapter_ns: 7_000_000,
            device_ns: 2_400_000,
            context_tail_ns: 5_000,
            font_thread_ns: 5_400_000,
            font_join_ns: 10_000,
            pipelines_ns: 2_500_000,
            pipe_shader_ns: 850_000,
            pipe_uniform_atlas_ns: 50_000,
            pipe_cell_ns: 1_400_000,
            pipe_blit_ns: 100_000,
            pipe_tray_ns: 40_000,
            pipe_bloom_ns: 30_000,
            pipe_vbuf_ns: 20_000,
            pipe_tail_ns: 1_000,
            tail_ns: 0,
            cell_pipeline_ns: [100_000; aterm_gpu::startup_probe::CELL_PIPELINE_COUNT],
        }
    }

    #[test]
    fn the_gpu_tail_absorbs_whatever_the_probe_did_not_measure() {
        let exclusive_ns = 10_000 + 7_000_000 + 2_400_000 + 5_000 + 10_000 + 2_500_000;
        let sample = close_startup_gpu(probe(), exclusive_ns + 80_000);
        assert!(sample.valid);
        assert_eq!(sample.tail_ns, 80_000);
        // The parallel font leg is reported but never spent from the parent.
        assert_eq!(sample.font_thread_ns, 5_400_000);
    }

    #[test]
    fn an_unset_probe_is_not_a_zero_length_gpu_build() {
        // The CPU-backend launch: no GPU leg ever ran, so every slot is the
        // UNSET sentinel and the sample must refuse rather than report zeros.
        assert!(!close_startup_gpu(StartupGpuSample::default(), 12_000_000).valid);
        let mut partial = probe();
        partial.device_ns = 0;
        assert!(!close_startup_gpu(partial, 12_000_000).valid);
    }

    #[test]
    fn legs_that_do_not_fit_their_parent_refuse_to_publish() {
        assert!(!close_startup_gpu(probe(), 1_000_000).valid);
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

    /// `input_dropped=` / `input_incoherent=` are CUMULATIVE (gap #33): they
    /// describe the one handoff this process adopted, at its birth, so a
    /// `metrics reset` taken afterwards must not erase a lost keystroke.
    #[test]
    fn reset_keeps_the_handoff_input_counts() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _counters = HANDOFF_INPUT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        note_handoff_input_dropped();
        note_handoff_input_incoherent();
        let counts = handoff_input_counts();
        assert!(counts.0 >= 1 && counts.1 >= 1, "{counts:?}");
        reset();
        assert_eq!(
            handoff_input_counts(),
            counts,
            "a reset erased a lost keystroke"
        );
    }

    #[test]
    fn reset_preserves_the_immutable_startup_fact() {
        // `reset()` zeroes the per-owner arm tables the serialized
        // `record_deadline` drivers increment-and-read, so an unserialized
        // reset lands mid-test and (pre-existing flake) erased the arms
        // `arms_are_attributed_to_the_owner_that_armed_them` had just booked.
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    /// HEAL-AT-THE-FOLD (busy-rearm audit, item 3). One late arm is legal — a
    /// busy turn computes its deadline after real work — but the SAME owner
    /// arming more than the floor in the past on CONSECUTIVE turns is a
    /// self-rearming `WaitUntil(past)` spin, and the fold must clamp it to
    /// `now + floor` and count the heal so the loop survives the whole class
    /// visibly.
    #[test]
    fn consecutive_same_owner_stale_arms_heal_to_the_floor() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let future = now + Duration::from_millis(5);
        let stale = now - Duration::from_secs(2);
        let owner = DeadlineOwner::Blink;

        // A healthy arm first, so this test never inherits streak state.
        assert_eq!(record_deadline(owner, Some(future), now), Some(future));
        let heals_before = STALE_ARM_HEALS.load(Ordering::Relaxed);

        // First offence passes through unhealed: it may be honest lateness.
        assert_eq!(record_deadline(owner, Some(stale), now), Some(stale));
        assert_eq!(STALE_ARM_HEALS.load(Ordering::Relaxed), heals_before);

        // The second consecutive same-owner offence is the bug's signature:
        // clamped to the floor, and counted.
        assert_eq!(
            record_deadline(owner, Some(stale), now),
            Some(now + STALE_ARM_HEAL_FLOOR),
            "the armed deadline is healed to now + floor"
        );
        assert_eq!(STALE_ARM_HEALS.load(Ordering::Relaxed), heals_before + 1);

        // A DIFFERENT owner does not inherit the streak.
        assert_eq!(
            record_deadline(DeadlineOwner::TitleSummary, Some(stale), now),
            Some(stale)
        );

        // A healthy arm ends the episode; the next late arm is a fresh first
        // offence and passes through again.
        assert_eq!(record_deadline(owner, Some(future), now), Some(future));
        assert_eq!(record_deadline(owner, Some(stale), now), Some(stale));

        // Lateness at or under the floor never trips the healer: that is the
        // ordinary busy-turn case, on any number of consecutive turns.
        let barely = now - Duration::from_millis(100);
        assert_eq!(record_deadline(owner, Some(barely), now), Some(barely));
        assert_eq!(record_deadline(owner, Some(barely), now), Some(barely));
        assert_eq!(STALE_ARM_HEALS.load(Ordering::Relaxed), heals_before + 1);

        // A pure-Wait turn also breaks a streak.
        assert_eq!(record_deadline(owner, Some(stale), now), Some(stale));
        assert_eq!(record_deadline(owner, None, now), None);
        assert_eq!(record_deadline(owner, Some(stale), now), Some(stale));

        // Leave no streak behind for other tests.
        assert_eq!(record_deadline(owner, Some(future), now), Some(future));
    }

    /// The PURE window law behind the items-18/19 detector: a trigger needs a
    /// FULL 32-arm window with more than 90% of it past (>= 29 of 32) — never
    /// a short mostly-past warmup — and a healthy arm DILUTES the window
    /// instead of resetting it (the property the consecutive-turn latch above
    /// lacks, and exactly how a `late ≈ 0`/nanosecond-in-the-future blip used
    /// to hide a spin).
    #[test]
    fn past_arm_window_step_requires_a_full_window_over_ninety_percent_past() {
        let run = |arms: &[bool]| {
            let mut packed = 0u64;
            let mut last = false;
            for &past in arms {
                let (next, trigger) = past_arm_window_step(packed, past);
                packed = next;
                last = trigger;
            }
            (packed, last)
        };
        // 31 consecutive past arms: window not yet full, never triggers.
        let mut warmup = vec![true; 31];
        assert!(!run(&warmup).1, "a not-yet-full window must not convict");
        // The 32nd closes an all-past window; the fill count saturates at 32.
        warmup.push(true);
        let (packed, trigger) = run(&warmup);
        assert!(trigger, "32/32 past is the streak signature");
        assert_eq!(packed >> 32, u64::from(PAST_ARM_WINDOW), "fill saturates");
        assert!(
            past_arm_window_step(packed, true).1,
            "the streak keeps triggering while it persists"
        );
        // Boundary: 29/32 past (> 90%) triggers; 28/32 does not.
        let mut edge = vec![false; 3];
        edge.extend(std::iter::repeat_n(true, 29));
        assert!(run(&edge).1, "29 of 32 past is over the 90% line");
        let mut under = vec![false; 4];
        under.extend(std::iter::repeat_n(true, 28));
        assert!(!run(&under).1, "28 of 32 past is under the 90% line");
        // Dilution, not reset: one healthy arm inside an otherwise-past
        // window leaves 31/32 past — still a conviction.
        let mut interleaved = vec![true; 16];
        interleaved.push(false);
        interleaved.extend(std::iter::repeat_n(true, 16));
        assert!(
            run(&interleaved).1,
            "a single healthy blip must not acquit a windowed spin"
        );
    }

    /// ITEMS 18/19 (2026-08-24 wake follow-ups): the WINDOWED per-owner
    /// detector heals the two flavours the 250 ms floor above is structurally
    /// blind to — the `late ≈ 0` spin (every arm barely past, each one taking
    /// the branch that RESETS the consecutive-turn latch) and two owners
    /// alternating stale arms (defeating the single global streak slot). The
    /// clamp is one display frame, so a healed spin degrades to cadence, and
    /// every clamped arm lands in the owner's NAMED ledger beside
    /// `deadline_arms_by_owner`.
    #[test]
    fn windowed_past_arm_streaks_heal_at_any_lateness_and_per_owner() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let clear = |owner: DeadlineOwner| {
            PAST_ARM_HISTORY_BY_OWNER[owner as usize].store(0, Ordering::Relaxed);
            PAST_ARM_STREAK_HEALS_BY_OWNER[owner as usize].store(0, Ordering::Relaxed);
        };
        let heals_of = |owner: DeadlineOwner| {
            PAST_ARM_STREAK_HEALS_BY_OWNER[owner as usize].load(Ordering::Relaxed)
        };
        let now = Instant::now();
        let future = now + Duration::from_millis(5);
        // 1 ms late: UNDER the 250 ms floor, so the consecutive-turn heal
        // above never fires — this is the pre-fix `arms Instant::now() every
        // turn` shape, previously invisible.
        let barely = now - Duration::from_millis(1);

        // (1) The `late ≈ 0` spin, on the fix's own owner: 31 barely-late arms
        // pass through (an honest busy stretch must not be delayed), the arm
        // that completes an all-past window is clamped to one frame ahead and
        // counted, and the heal SUSTAINS while the spin does.
        let video = DeadlineOwner::Video;
        clear(video);
        for _ in 0..31 {
            assert_eq!(
                record_deadline(video, Some(barely), now),
                Some(barely),
                "a not-yet-full window never heals"
            );
        }
        assert_eq!(heals_of(video), 0);
        assert_eq!(
            record_deadline(video, Some(barely), now),
            Some(now + PAST_ARM_STREAK_CLAMP),
            "arm 32 of an all-past window is clamped to now + one frame"
        );
        assert_eq!(
            record_deadline(video, Some(barely), now),
            Some(now + PAST_ARM_STREAK_CLAMP),
            "the clamp sustains at frame cadence while the spin persists"
        );
        assert_eq!(heals_of(video), 2);
        assert!(
            past_arm_streak_heal_attribution()
                .iter()
                .any(|&(owner, heals)| owner == "video" && heals == 2),
            "the ledger NAMES the producer it clamped"
        );

        // (2) 28/32 past stays under the > 90% trigger: occasional lateness —
        // even a lot of it — is not the streak signature.
        let word = DeadlineOwner::WordDecorations;
        clear(word);
        for _ in 0..4 {
            assert_eq!(record_deadline(word, Some(future), now), Some(future));
        }
        for _ in 0..28 {
            assert_eq!(
                record_deadline(word, Some(barely), now),
                Some(barely),
                "28 of 32 past must pass through unhealed"
            );
        }
        assert_eq!(heals_of(word), 0);

        // (3) Two owners ALTERNATING deeply stale arms: the global
        // consecutive-turn latch never sees the same owner twice in a row and
        // stays silent, but each owner's own window convicts it.
        let (left, right) = (DeadlineOwner::CursorEffect, DeadlineOwner::Predictor);
        let stale = now - Duration::from_secs(2);
        clear(left);
        clear(right);
        let floor_heals_before = STALE_ARM_HEALS.load(Ordering::Relaxed);
        for _ in 0..31 {
            assert_eq!(record_deadline(left, Some(stale), now), Some(stale));
            assert_eq!(record_deadline(right, Some(stale), now), Some(stale));
        }
        assert_eq!(
            STALE_ARM_HEALS.load(Ordering::Relaxed),
            floor_heals_before,
            "the single-slot consecutive heal is defeated by alternation"
        );
        assert_eq!(
            record_deadline(left, Some(stale), now),
            Some(now + PAST_ARM_STREAK_CLAMP)
        );
        assert_eq!(
            record_deadline(right, Some(stale), now),
            Some(now + PAST_ARM_STREAK_CLAMP)
        );
        assert_eq!((heals_of(left), heals_of(right)), (1, 1));

        // (4) When the coarser 250 ms heal already moved a consecutive
        // same-owner arm further out, the stronger clamp is KEPT and the
        // windowed ledger does not double-count that arm.
        assert_eq!(
            record_deadline(left, Some(stale), now),
            Some(now + PAST_ARM_STREAK_CLAMP),
            "first consecutive offence: the floor heal is not armed yet"
        );
        assert_eq!(heals_of(left), 2);
        assert_eq!(
            record_deadline(left, Some(stale), now),
            Some(now + STALE_ARM_HEAL_FLOOR),
            "the stronger (floor) clamp wins over the frame clamp"
        );
        assert_eq!(
            heals_of(left),
            2,
            "an arm the floor heal owns is not re-counted"
        );

        // Leave neither latch nor window residue behind for other tests.
        assert_eq!(record_deadline(left, Some(future), now), Some(future));
        for owner in [video, word, left, right] {
            clear(owner);
        }
    }

    /// ITEM 8, and the append-only wire contract that protects it. `SessionStatus`
    /// exists so the status observer stops reporting as `title_summary`; the slot
    /// table behind the per-owner attribution must cover it and every future
    /// sibling, or `note_owner_arm` silently drops that owner's arms on the floor.
    #[test]
    fn every_deadline_owner_has_its_own_label_and_its_own_slot() {
        assert_eq!(
            DeadlineOwner::from_raw(DeadlineOwner::SessionStatus as u64),
            DeadlineOwner::SessionStatus
        );
        assert_eq!(DeadlineOwner::SessionStatus.as_str(), "session_status");
        // Slot 34 moved with its surface (2026-09-22): the message band
        // inherits the status bars' number, under the band's own label.
        assert_eq!(
            DeadlineOwner::from_raw(34),
            DeadlineOwner::MessageBand,
            "slot 34 is the message band's"
        );
        assert_eq!(DeadlineOwner::MessageBand.as_str(), "message_band");
        // Slot 39 is the band's MOTION (design §10.8): its own label, so a
        // spin in the frames is never attributed to the band's state wakes.
        assert_eq!(
            DeadlineOwner::from_raw(39),
            DeadlineOwner::MessageBandMotion
        );
        assert_eq!(
            DeadlineOwner::MessageBandMotion.as_str(),
            "message_band_motion"
        );
        // Slot 40 is the strain engine's reading cadence (design §10.14): its
        // own label, so an idle machine's `system_strain` arms read 0 and a
        // spin in the probe is never charged to the band.
        assert_eq!(DeadlineOwner::from_raw(40), DeadlineOwner::SystemStrain);
        assert_eq!(DeadlineOwner::SystemStrain.as_str(), "system_strain");
        assert_eq!(DeadlineOwner::from_raw(41), DeadlineOwner::SessionWaits);
        assert_eq!(DeadlineOwner::SessionWaits.as_str(), "session_waits");
        // Slot 42 is the input watch (2026-09-24), under its own label, so a
        // wake it owes is never read as the status observer's.
        assert_eq!(DeadlineOwner::from_raw(42), DeadlineOwner::InputWatch);
        assert_eq!(DeadlineOwner::InputWatch.as_str(), "input_watch");
        // Slot 43 is the crash journal's (PTY keeper P1): a capture's trailing
        // edge is never charged to another owner.
        assert_eq!(DeadlineOwner::from_raw(43), DeadlineOwner::CrashJournal);
        assert_eq!(DeadlineOwner::CrashJournal.as_str(), "crash_journal");
        // Slot 44 is the update checker's watchdog (plan P2-1): its one wake at
        // the heartbeat's stale instant is never charged to another owner.
        assert_eq!(DeadlineOwner::from_raw(44), DeadlineOwner::UpdateChecker);
        assert_eq!(DeadlineOwner::UpdateChecker.as_str(), "update_checker");
        // Slot 22 is a tombstone (2026-09-22): the config banner's owner
        // retired with `config_notice.rs`, and the number stays taken under a
        // label no live owner wears, so an older wire reader never attributes
        // a later owner's arms to the banner.
        assert_eq!(DeadlineOwner::from_raw(22), DeadlineOwner::Retired22);
        assert_eq!(DeadlineOwner::Retired22.as_str(), "retired-22");
        assert_ne!(
            DeadlineOwner::SessionStatus.as_str(),
            DeadlineOwner::TitleSummary.as_str(),
            "the two folds that shared a label must not share one again"
        );
        // Every slot below the count decodes to a DISTINCT owner whose own
        // discriminant is that slot — this is what makes `note_owner_arm`'s
        // index a bijection onto the enum.
        let mut seen = std::collections::BTreeSet::new();
        for slot in 1..DEADLINE_OWNER_SLOTS {
            let owner = DeadlineOwner::from_raw(slot as u64);
            assert_eq!(
                owner as u64, slot as u64,
                "slot {slot} does not round-trip: DEADLINE_OWNER_SLOTS is stale"
            );
            assert!(
                seen.insert(owner.as_str()),
                "duplicate wire label at slot {slot}: {}",
                owner.as_str()
            );
        }
        // …and the count is EXACTLY one past the last variant: appending a
        // variant without widening the table fails here instead of losing its
        // attribution at runtime.
        assert_eq!(
            DeadlineOwner::from_raw(DEADLINE_OWNER_SLOTS as u64),
            DeadlineOwner::None,
            "DEADLINE_OWNER_SLOTS must be one past the last discriminant"
        );
    }

    /// ITEM 6. The failure this prevents: `past_deadline_arms` was one global
    /// number, so a spin could only ever be reported against `deadline_owner`,
    /// a last-writer snapshot. Two owners arming in the same window must now
    /// come back separated, past arms included.
    #[test]
    fn arms_are_attributed_to_the_owner_that_armed_them() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let future = now + Duration::from_millis(5);
        let stale = now - Duration::from_millis(1);

        let before = |owner: DeadlineOwner| {
            let slot = owner as usize;
            (
                DEADLINE_ARMS_BY_OWNER[slot].load(Ordering::Relaxed),
                PAST_DEADLINE_ARMS_BY_OWNER[slot].load(Ordering::Relaxed),
            )
        };
        let (status_arms, status_past) = before(DeadlineOwner::SessionStatus);
        let (blink_arms, blink_past) = before(DeadlineOwner::Blink);

        // One healthy arm from Blink, two PAST arms from SessionStatus. Under
        // the old single counter this window read "2 past arms, owner=blink".
        let _ = record_deadline(DeadlineOwner::SessionStatus, Some(stale), now);
        let _ = record_deadline(DeadlineOwner::Blink, Some(future), now);
        let _ = record_deadline(DeadlineOwner::SessionStatus, Some(stale), now);

        assert_eq!(
            before(DeadlineOwner::SessionStatus),
            (status_arms + 2, status_past + 2),
            "both stale arms are booked to the observer that armed them"
        );
        assert_eq!(
            before(DeadlineOwner::Blink),
            (blink_arms + 1, blink_past),
            "a healthy arm counts as an arm and NOT as a past arm"
        );

        // The published view names the owner, and never invents one.
        let published = deadline_arm_attribution();
        let status = published
            .iter()
            .find(|o| o.owner == "session_status")
            .expect("the arming owner is published by name");
        assert!(status.past_arms >= 2);
        assert!(status.arms >= status.past_arms);
        assert!(
            published.iter().all(|o| o.arms != 0 || o.past_arms != 0),
            "the attribution is sparse: silent owners are omitted, not zero-padded"
        );

        // Leave no streak behind for the healer test.
        let _ = record_deadline(DeadlineOwner::SessionStatus, Some(future), now);
    }

    /// ITEM 10, the policy half. A drop only taints the output→present
    /// distribution when it means the present stream STOPPED for an interval
    /// nobody was watching. Tainting a transient retry would delete the stalls
    /// the histogram exists to catch — so this pins the narrow set explicitly.
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

    /// ITEM 10, the window half — pure, so it cannot race the process globals.
    #[test]
    fn the_taint_window_covers_live_captures_and_the_episode_tail() {
        assert!(
            !tainted_at(1_000, 0, 0),
            "no episode, no capture: the sample is aterm's own"
        );
        assert!(tainted_at(1_000, 2_000, 0), "inside the episode tail");
        assert!(
            !tainted_at(2_000, 2_000, 0),
            "the tail is exclusive: the boundary sample is clean again"
        );
        assert!(
            tainted_at(u64::MAX, 0, 1),
            "a LIVE capture taints regardless of the clock — the recorder is              pacing presents right now"
        );
    }

    /// A capture episode must open and close exactly once however the recording
    /// ends, and an UNPAIRED close must not wrap the depth to `u64::MAX` and
    /// taint every present for the rest of the process.
    ///
    /// This is the one test here that touches the globals; it leaves the depth
    /// at zero. It does arm the ~1 s taint tail, which is harmless: no other
    /// test asserts on present-latency VALUES (`record_present` has exactly one
    /// caller, the real present path).
    #[test]
    fn capture_episodes_nest_and_never_underflow() {
        assert!(!snapshot().capture_active, "no capture is running");
        note_capture_episode(true);
        assert!(snapshot().capture_active);
        note_capture_episode(true); // overlapping capture
        note_capture_episode(false);
        assert!(
            snapshot().capture_active,
            "one close does not end two overlapping captures"
        );
        note_capture_episode(false);
        assert!(
            !snapshot().capture_active,
            "the last close ends the episode"
        );
        note_capture_episode(false); // unpaired: a double finalize
        assert!(
            !snapshot().capture_active,
            "an unpaired close saturates at zero instead of wrapping"
        );
        assert!(present_latency_tainted(), "the closing tail is still armed");
    }
}

#[cfg(test)]
mod present_glass_tests {
    use super::{
        H_PRESENT_GLASS, MAX_PRESENT_GLASS_NS, PRESENT_GLASS_SKIPPED, SCHEDULER_STATE,
        note_present_glass, present_glass_fields_json, present_glass_fields_text, reset,
    };
    use aterm_gpu::present_glass::{GlassReport, GlassSample, PresentTag};
    use std::sync::atomic::Ordering;

    fn report(sample: GlassSample, tag: PresentTag, successor: PresentTag) -> GlassReport {
        GlassReport {
            sample,
            tag,
            successor,
        }
    }

    /// The compositor leg is published beside the application-side slices: an
    /// on-glass sample lands in the distribution, the max and its own tag's
    /// shown ledger; a skipped drawable only in the skip counter and the ledger
    /// of the lane that REPLACED it; and `reset` clears all of it. Empty ledgers
    /// print `none` / `{}`, never a bare `key=`.
    #[test]
    fn glass_samples_book_on_glass_and_skipped_apart_and_reset_clears_them() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        let text = present_glass_fields_text();
        assert!(
            text.ends_with(" present_glass_shown_by=none present_glass_skipped_by=none"),
            "{text}"
        );
        let json = present_glass_fields_json();
        assert!(json.ends_with(",\"present_glass_skipped_by\":{}"), "{json}");

        note_present_glass(report(
            GlassSample::OnGlass { ns: 7_000_000 },
            PresentTag::Spaced,
            PresentTag::Spaced,
        ));
        // A frame replaced by a hot-burst successor: charged to `hot_burst`.
        note_present_glass(report(
            GlassSample::Skipped,
            PresentTag::Spaced,
            PresentTag::HotBurst,
        ));
        assert_eq!(H_PRESENT_GLASS.count(), 1, "a skip is not a duration");
        assert_eq!(MAX_PRESENT_GLASS_NS.load(Ordering::Relaxed), 7_000_000);
        assert_eq!(PRESENT_GLASS_SKIPPED.load(Ordering::Relaxed), 1);
        let text = present_glass_fields_text();
        assert!(text.contains(" n_present_glass=1 "), "{text}");
        assert!(text.contains(" max_present_glass_ms=7.00 "), "{text}");
        assert!(text.contains(" present_glass_skipped=1 "), "{text}");
        assert!(text.contains(" present_glass_shown_by=spaced:1 "), "{text}");
        assert!(
            text.ends_with(" present_glass_skipped_by=hot_burst:1"),
            "{text}"
        );
        let json = present_glass_fields_json();
        assert!(json.contains("\"n_present_glass\":1,"), "{json}");
        assert!(json.contains("\"present_glass_skipped\":1,"), "{json}");
        assert!(
            json.contains("\"present_glass_shown_by\":{\"spaced\":1}"),
            "{json}"
        );
        assert!(
            json.ends_with("\"present_glass_skipped_by\":{\"hot_burst\":1}"),
            "{json}"
        );
        reset();
        assert_eq!(H_PRESENT_GLASS.count(), 0);
        assert_eq!(MAX_PRESENT_GLASS_NS.load(Ordering::Relaxed), 0);
        assert_eq!(PRESENT_GLASS_SKIPPED.load(Ordering::Relaxed), 0);
        assert!(present_glass_fields_text().ends_with(" present_glass_skipped_by=none"));
    }
}

#[cfg(test)]
mod window_input_attribution_tests {
    use super::{INPUT_SLICE_CAP_NS, PendingInputStamp};

    /// The two-window streaming scenario: a key typed into window A must not be
    /// closed by window B's content present. Each window's present can only take
    /// its OWN stamp, so B's log-line frame leaves A armed, and A's echo frame
    /// books the whole wait.
    #[test]
    fn another_windows_present_cannot_close_this_windows_key() {
        let mut typed_into = PendingInputStamp::default();
        let mut streaming = PendingInputStamp::default();
        typed_into.arm(1_000, 1_000);

        // Window B presents its own streaming output 3 ms later.
        assert_eq!(streaming.drain_slices(4_000_000, 0), Vec::<u64>::new());
        assert!(
            typed_into.is_pending(),
            "window B's frame closed window A's keystroke: the slice would read LOW"
        );

        // Window A's real echo, 90 ms after the key.
        let echo_at = 1_000 + 90_000_000;
        assert_eq!(typed_into.drain_slices(echo_at, 0), vec![90_000_000]);
        assert!(
            !typed_into.is_pending(),
            "a content present consumes the stamp"
        );
        assert!(
            typed_into.drain_slices(echo_at + 1, 0).is_empty(),
            "consumed once"
        );
    }

    /// COORDINATED OMISSION, corrected: N keystrokes typed before one (slow)
    /// present are N samples, each its own wait — not one sample for the
    /// oldest with the other N-1 hidden, which is what let a stall read LOW.
    #[test]
    fn a_burst_before_one_present_books_every_edge() {
        let mut pending = PendingInputStamp::default();
        for arrival in [10, 20, 30, 40] {
            pending.arm(arrival, arrival);
        }
        assert_eq!(pending.drain_slices(110, 0), vec![100, 90, 80, 70]);
        assert!(!pending.is_pending());
    }

    /// A burst past the ring keeps its OLDEST edges (the longest waits) and
    /// refuses the rest — each refusal counted in `input_edges_dropped`, so the
    /// loss is published rather than silent.
    #[test]
    fn a_full_ring_keeps_the_oldest_edges_and_refuses_the_rest() {
        let mut pending = PendingInputStamp::default();
        let held: Vec<bool> = (1..=(super::INPUT_EDGE_RING as u64 + 3))
            .map(|arrival| pending.arm(arrival, arrival))
            .collect();
        assert!(held[..super::INPUT_EDGE_RING].iter().all(|&h| h));
        assert_eq!(&held[super::INPUT_EDGE_RING..], &[false, false, false]);
        let slices = pending.drain_slices(1_000, 0);
        assert_eq!(slices.len(), super::INPUT_EDGE_RING);
        assert_eq!(slices.first(), Some(&999), "the oldest edge is kept");
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
            pending.drain_slices(fresh + 7, 0),
            vec![7],
            "only the fresh key books a slice"
        );
        // The control: a full ring of edges still inside the cap keeps them all
        // and refuses the newcomer, exactly as before.
        for arrival in 1..=super::INPUT_EDGE_RING as u64 {
            assert!(pending.arm(arrival, arrival));
        }
        assert!(!pending.arm(1_000, 1_000), "live edges are never evicted");
    }

    #[test]
    fn a_stamp_armed_before_reset_is_discarded_not_booked() {
        let mut pending = PendingInputStamp::default();
        pending.arm(50, 60);
        assert!(
            pending.drain_slices(10_000, 61).is_empty(),
            "armed before the reset"
        );
        assert!(!pending.is_pending(), "discarded, not left armed");

        // Armed at the reset instant or later counts, even if the backdated
        // arrival precedes it (the key sat in the OS queue across the reset).
        pending.arm(50, 61);
        assert_eq!(pending.drain_slices(10_050, 61), vec![10_000]);
    }

    /// Every edge carries its OWN key's path: a controller key never becomes a
    /// hardware sample by sharing a burst with a real key, and vice versa.
    #[test]
    fn each_edge_carries_its_own_keys_path() {
        let mut pending = PendingInputStamp::default();
        pending.arm_from(10, 10, true);
        pending.arm_from(20, 20, false);
        assert_eq!(
            pending.drain_attributed(110, 0),
            vec![(100, true), (90, false)]
        );
        pending.arm_from(10, 10, false);
        pending.arm_from(20, 20, true);
        assert_eq!(
            pending.drain_attributed(110, 0),
            vec![(100, false), (90, true)],
            "a ctl send/feed/key never counts as hardware, even ahead of a real key"
        );
    }

    #[test]
    fn a_keystroke_that_never_echoed_ages_out() {
        let mut pending = PendingInputStamp::default();
        pending.arm(1, 1);
        assert!(
            pending
                .drain_slices(1 + INPUT_SLICE_CAP_NS + 1, 0)
                .is_empty()
        );
        pending.arm(1, 1);
        assert_eq!(
            pending.drain_slices(1 + INPUT_SLICE_CAP_NS, 0),
            vec![INPUT_SLICE_CAP_NS]
        );
    }
}

#[cfg(test)]
mod lateness_attribution_tests {
    use super::{
        DeadlineOwner, EventWakeKind, LAST_DEADLINE_OWNER, LAST_PRESENT_STAMP_NS,
        LAST_WAKE_LATE_NS, MAX_DEADLINE_LATE_AT_NS, MAX_DEADLINE_LATE_NS, MAX_DEADLINE_LATE_OWNER,
        MAX_PRESENT_LATENCY_AT_NS, MAX_PRESENT_LATENCY_GAP_NS, MAX_PRESENT_LATENCY_NS,
        MAX_WAKE_LATE_AT_NS, MAX_WAKE_LATE_NS, MAX_WAKE_LATE_OWNER, PRESENT_TAINT_UNTIL_NS,
        SCHEDULER_STATE, SLOW_EPISODE_RESET, SLOW_EPISODE_SAMPLES, SLOW_EPISODE_START_NS,
        SLOW_PRESENT_EPISODE_QUIET_NS, SLOW_PRESENT_LOG_THRESHOLD_NS, SlowEpisode, SlowEpisodeStep,
        StartupPresentTiming, clear_key_arrival, deadline_fire_attribution, gpu_repaint_counts,
        key_queue_distribution, key_queue_last_max_ns, lateness_fields_json, lateness_fields_text,
        log_span, note_event_wake, note_gpu_repaint_totals, note_key_arrival_queued,
        note_slow_episode_quiet, note_slow_present, note_turn, note_user_wake, now_ns,
        record_deadline, record_present, reset, slow_episode_close, slow_episode_step,
        turn_distribution, user_wake_attribution,
    };
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    const LATE_NS: u64 = 400_000_000;

    /// `record_deadline` stamps `due = now - late` on the PROCESS clock, and a
    /// saturated `due` of 0 is read as "no deadline armed". Wait that clock past
    /// the lateness this test fabricates rather than racing it — a no-op once a
    /// test binary is warm, and a sleep rather than a spin so it costs no CPU.
    fn wait_out_the_process_clock() {
        while now_ns() < LATE_NS + 50_000_000 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// RESPONSIVENESS AUDIT ITEMS 2, 3 AND 5 (2026-09-21), published. A timer
    /// that WOKE the loop is counted for its owner apart from the fold wins
    /// (`deadline_fires_by_owner` beside `deadline_arms_by_owner`); every
    /// delivered proxy event is counted by its `Wake` variant; each main-loop
    /// turn lands in its root's distribution; and the GPU's repaint-scope
    /// counters are window stats that `reset` rebases. Before, none of these
    /// was readable from a live instance.
    #[test]
    fn fires_wake_variants_turn_roots_and_gpu_scope_are_published() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        let now = Instant::now();
        let soon = now + Duration::from_millis(5);
        let _ = record_deadline(DeadlineOwner::FrameCap, Some(soon), now);
        note_event_wake(EventWakeKind::Timer);
        let fires = deadline_fire_attribution();
        assert!(
            fires
                .iter()
                .any(|(owner, n)| *owner == "frame_cap" && *n >= 1),
            "a timer wake is a fire for its owner: {fires:?}"
        );
        // A WaitCancelled interrupting the same deadline is not a fire.
        let before: u64 = deadline_fire_attribution().iter().map(|(_, n)| n).sum();
        note_event_wake(EventWakeKind::WaitCancelled);
        let after: u64 = deadline_fire_attribution().iter().map(|(_, n)| n).sum();
        assert_eq!(before, after, "an interrupted deadline did not fire");

        note_user_wake(&crate::Wake::TitleSummaryReady);
        note_user_wake(&crate::Wake::TitleSummaryReady);
        let wakes = user_wake_attribution();
        assert!(
            wakes
                .iter()
                .any(|(name, n)| *name == "TitleSummaryReady" && *n >= 2),
            "{wakes:?}"
        );

        note_turn(crate::watchdog::Breadcrumb::UserEvent as u8, 3_000_000);
        let h = turn_distribution(crate::watchdog::Breadcrumb::UserEvent as u8).expect("a slot");
        assert!(h.count() >= 1);

        note_gpu_repaint_totals(10, 4, 2);
        reset();
        assert_eq!(gpu_repaint_counts(), (0, 0, 0), "reset rebases the window");
        note_gpu_repaint_totals(13, 5, 3);
        assert_eq!(gpu_repaint_counts(), (3, 1, 1));
        reset();
    }

    /// THE ERASURE THIS FIXES. A 400 ms late timer wake is recorded, and the
    /// very next reader wake — the `WaitCancelled` a PTY output burst produces,
    /// milliseconds later — stores 0 over it. That is not a bug in itself
    /// (`wake_late_ms` is documented as the LAST reading), but until these
    /// maxima existed it meant the terminal could report
    /// `max_present_latency_ms=560.54 wake_late_ms=0.00 deadline_late_ms=0.00`
    /// and no reader could tell a starved main thread from an idle stretch.
    ///
    /// The last-writer semantics are pinned here too: this must ADD a surviving
    /// worst case, not change what `wake_late_ms` means.
    #[test]
    fn a_late_timer_survives_the_reader_wake_that_erases_the_last_reading() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        wait_out_the_process_clock();
        reset();
        let now = Instant::now();
        let stale = now - Duration::from_nanos(LATE_NS);
        // One offence from one owner: no heal clamps it (that needs a streak),
        // so the arm passes through and books its lateness as-asked.
        assert_eq!(
            record_deadline(DeadlineOwner::FrameCap, Some(stale), now),
            Some(stale)
        );
        note_event_wake(EventWakeKind::Timer);
        assert!(
            LAST_WAKE_LATE_NS.load(Ordering::Relaxed) >= LATE_NS,
            "the timer wake must book its lateness at all"
        );

        // The erasing wake: one output burst is all it takes.
        note_event_wake(EventWakeKind::WaitCancelled);
        assert_eq!(
            LAST_WAKE_LATE_NS.load(Ordering::Relaxed),
            0,
            "`wake_late_ms` is still the LAST reading — that contract is unchanged"
        );

        // …and the worst case outlives it, named and timed.
        assert!(
            MAX_WAKE_LATE_NS.load(Ordering::Relaxed) >= LATE_NS,
            "a 400 ms late timer must not be erased by the next wake"
        );
        assert_eq!(
            DeadlineOwner::from_raw(MAX_WAKE_LATE_OWNER.load(Ordering::Relaxed)),
            DeadlineOwner::FrameCap,
            "a number that cannot name its producer cannot end an investigation"
        );
        assert!(
            MAX_WAKE_LATE_AT_NS.load(Ordering::Relaxed) != 0,
            "the worst wake must say WHEN it happened"
        );
        assert!(
            MAX_DEADLINE_LATE_NS.load(Ordering::Relaxed) >= LATE_NS
                && MAX_DEADLINE_LATE_AT_NS.load(Ordering::Relaxed) != 0,
            "the deadline lateness keeps its worst case on the same rule"
        );
        assert_eq!(
            DeadlineOwner::from_raw(MAX_DEADLINE_LATE_OWNER.load(Ordering::Relaxed)),
            DeadlineOwner::FrameCap
        );

        // And it is PUBLISHED — in both forms, with the owner spelled.
        let text = lateness_fields_text();
        assert!(
            text.contains(" max_wake_late_owner=frame_cap "),
            "the text fragment must name the owner: {text}"
        );
        assert!(
            text.contains(" metrics_now_ms="),
            "an instant with no anchor cannot be read as `how long ago`: {text}"
        );
        let json = lateness_fields_json();
        assert!(
            json.contains("\"max_deadline_late_owner\":\"frame_cap\""),
            "the JSON twin must carry the same owner: {json}"
        );

        // A reset clears the window's worst case with the rest of the window.
        reset();
        assert_eq!(MAX_WAKE_LATE_NS.load(Ordering::Relaxed), 0);
        assert_eq!(MAX_DEADLINE_LATE_NS.load(Ordering::Relaxed), 0);

        // Leave no streak behind for the other scheduler tests.
        let _ = record_deadline(
            DeadlineOwner::FrameCap,
            Some(now + Duration::from_millis(5)),
            now,
        );
    }

    /// The worst output→present reading is an OPEN interval, so "560 ms" alone
    /// cannot say whether the main thread stalled or simply had nothing to
    /// draw. The frame that sets the max therefore also records WHEN it
    /// happened and how long it had been since the previous present: a gap the
    /// size of the reading is an idle stretch, a far smaller one is output that
    /// waited while frames kept coming.
    #[test]
    fn the_worst_present_latency_records_its_instant_and_its_frame_gap() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        wait_out_the_process_clock();
        reset();
        // This sample must reach the on-glass ledger, not the tainted twin.
        PRESENT_TAINT_UNTIL_NS.store(0, Ordering::Relaxed);
        // A present 300 ms after the previous one, closing a 4 s slice: a
        // stall that is NOT explained by an idle stretch.
        //
        // The present reads the live process clock, so the gap it stores is
        // 300 ms plus however long this thread took to reach that read. The
        // bounds are therefore the clock readings on either side of the call,
        // not a fixed tolerance (the load-sensitive test audit of 2026-09-27):
        // the old `+ 1 s` window, and a 500 ms slice only 200 ms above the
        // gap, failed on a preempted thread with nothing wrong. The slice now
        // sits 3.7 s above the gap, so a present that stored the slice as its
        // gap still falls outside the bracket, and only a multi-second stall
        // between two adjacent statements could blur the two.
        let gap_ns = 300_000_000;
        let latency_ns = 4_000_000_000;
        let stamped_at = now_ns();
        let previous_present = stamped_at - gap_ns;
        LAST_PRESENT_STAMP_NS.store(previous_present, Ordering::Relaxed);
        record_present(
            latency_ns,
            1_000_000,
            StartupPresentTiming::collapsed(Instant::now()),
            None,
        );
        let returned_at = now_ns();

        assert_eq!(MAX_PRESENT_LATENCY_NS.load(Ordering::Relaxed), latency_ns);
        assert!(
            (stamped_at..=returned_at).contains(&MAX_PRESENT_LATENCY_AT_NS.load(Ordering::Relaxed)),
            "the worst present must say when it happened"
        );
        let gap = MAX_PRESENT_LATENCY_GAP_NS.load(Ordering::Relaxed);
        assert!(
            (gap_ns..=returned_at - previous_present).contains(&gap),
            "the frame gap of the winning sample is what separates a stall from an \
             idle stretch; got {gap} ns"
        );
        assert!(
            gap < latency_ns,
            "a gap well under the reading is the `output waited` signature"
        );

        // A smaller sample cannot overwrite the winner's stamps.
        let at = MAX_PRESENT_LATENCY_AT_NS.load(Ordering::Relaxed);
        record_present(
            1_000_000,
            1_000_000,
            StartupPresentTiming::collapsed(Instant::now()),
            None,
        );
        assert_eq!(MAX_PRESENT_LATENCY_AT_NS.load(Ordering::Relaxed), at);
        assert_eq!(MAX_PRESENT_LATENCY_GAP_NS.load(Ordering::Relaxed), gap);
        reset();
    }

    /// The breadcrumb is the half of this that reaches `aterm.log` in a SHIPPED
    /// binary, so its bounds matter: an ordinary slow frame must not write a
    /// line, a stall must cost one line when it starts and one when it ends —
    /// never one per frame or one every few seconds — and a loop waiting only
    /// on the session-status observer writes nothing.
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

        // The 2026-09-18 condition: a slow present every 10 s for 27 hours. The
        // opening line above and the closing line below are the only two.
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

    /// `metrics reset` runs on a control thread, and the open episode is five
    /// atomics the UI thread writes. So reset only ASKS: the UI thread's next
    /// present closes the episode (with its line), and a slow present after the
    /// reset opens a fresh one rather than joining the old.
    #[test]
    fn metrics_reset_leaves_the_episode_for_the_ui_thread_to_close() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let samples = || SLOW_EPISODE_SAMPLES.load(Ordering::Relaxed);
        reset();
        note_slow_episode_quiet(0);
        assert_eq!(samples(), 0);
        assert!(!SLOW_EPISODE_RESET.load(Ordering::Relaxed));
        let owner = LAST_DEADLINE_OWNER.swap(DeadlineOwner::Blink as u64, Ordering::Relaxed);
        let slow = SLOW_PRESENT_LOG_THRESHOLD_NS + 1;
        let at = 1_000_000_000;
        note_slow_present(slow, 0, at);
        note_slow_present(slow, 0, at + 1);
        assert_eq!(samples(), 2);

        reset();
        assert_eq!(
            samples(),
            2,
            "the control thread does not touch the episode"
        );
        note_slow_episode_quiet(at + 2);
        assert_eq!(samples(), 0, "the next present closes it");

        note_slow_present(slow, 0, at + 3);
        reset();
        note_slow_present(slow, 0, at + 4);
        assert_eq!(
            samples(),
            1,
            "a slow present after a reset opens a new episode"
        );
        assert_eq!(SLOW_EPISODE_START_NS.load(Ordering::Relaxed), at + 4);
        reset();
        note_slow_episode_quiet(0);
        LAST_DEADLINE_OWNER.store(owner, Ordering::Relaxed);
    }

    #[test]
    fn log_spans_read_as_seconds_minutes_or_hours() {
        assert_eq!(log_span(1_500_000_000), "1.5 s");
        assert_eq!(log_span(27 * 60 * 1_000_000_000), "27 min");
        assert_eq!(log_span(27 * 3600 * 1_000_000_000), "27.0 h");
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
    ///
    /// SCOPED TO STATE THIS TEST OWNS, DELIBERATELY. `LAT_KEY_NS`,
    /// `LAST_KEY_WRITE_NS` and `H_KEY_WRITE` are process-global and the press-path
    /// tests in `app_input` drive real `InputEvent::Key` presses through the seam,
    /// so they arm and consume the arrival stamp too — and they cannot take this
    /// module's `SCHEDULER_STATE` lock, which guards only the metrics scheduler
    /// state. An earlier cut of this test asserted `LAST_KEY_WRITE_NS` and was
    /// SCHEDULE-DEPENDENT because of it: green under a `metrics` filter, red under
    /// a `key_write` one, where `failed_inline_key_write_revokes_every_new_movement_licence`
    /// runs alongside and swapped the stamp out from under it (measured: 0.3 ms
    /// booked against a 17 ms wait). The queue statics below have exactly one
    /// writer, `note_key_arrival_queued`, so they are assertable; that `key_write`
    /// CONTAINS this leg is structural — both clocks start at the same backdated
    /// stamp — and is pinned where it is observable, on the published line.
    /// Each arm is disarmed before the next, so no stale arrival leaks into
    /// another test's `note_input`.
    #[test]
    fn the_os_queue_share_of_key_write_is_published_apart_from_it() {
        let _serial = SCHEDULER_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        assert_eq!(
            key_queue_last_max_ns(),
            (0, 0),
            "a fresh measurement window starts with no queue reading"
        );
        assert_eq!(key_queue_distribution().count(), 0);

        // A key that waited 17 ms behind a parked event loop.
        let parked_ns = 17_000_000;
        note_key_arrival_queued(parked_ns);
        clear_key_arrival();

        assert_eq!(
            key_queue_last_max_ns(),
            (parked_ns, parked_ns),
            "the backdate is now a reading of its own, not just a moved stamp"
        );
        assert_eq!(key_queue_distribution().count(), 1);
        let queue_p99 = key_queue_distribution().percentile(0.99).unwrap();
        assert!(
            (parked_ns..parked_ns + parked_ns / 4).contains(&queue_p99),
            "the queue distribution reports the wait it was handed, as a bucket \
             upper edge; got {queue_p99} ns"
        );

        // A key that waited NOTHING books a real `0`: the max keeps the worst key,
        // `last` reports this one, and the count moves — so "no queueing" can
        // never be read as "not measured", which is also what an unmeasurable age
        // (any non-macOS build) books.
        note_key_arrival_queued(0);
        clear_key_arrival();
        assert_eq!(
            key_queue_last_max_ns(),
            (0, parked_ns),
            "a 0 is this key's reading; the worst key still owns the max"
        );
        assert_eq!(key_queue_distribution().count(), 2);

        // A key that never writes (a UI shortcut) still books its queue wait, which
        // is why `n_key_queue >= n_key_write` rather than equal.
        note_key_arrival_queued(parked_ns);
        clear_key_arrival();
        assert_eq!(key_queue_distribution().count(), 3);
        assert_eq!(key_queue_last_max_ns(), (parked_ns, parked_ns));

        // A window stat, cleared by `reset` like the total it qualifies.
        reset();
        assert_eq!(key_queue_last_max_ns(), (0, 0));
        assert_eq!(key_queue_distribution().count(), 0);
    }
}
