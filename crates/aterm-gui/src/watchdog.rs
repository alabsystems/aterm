// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev/CI main-thread STALL watchdog (L0 hazard guard).
//!
//! Standing guard against the "main thread does unbounded work under a contended
//! lock → whole-Mac freeze" hazard class. The archetype was a width change that
//! rewrapped the ENTIRE scrollback synchronously on the UI thread under the
//! per-session `term` `Arc<Mutex<Terminal>>` — a 42-second freeze. That specific
//! site is fixed by `resize_offloading_scrollback`, but sixteen sibling triggers
//! all funnel into `Grid::resize`'s width-reflow sink, so the CLASS needs a
//! standing tripwire that pins any FUTURE regression to a named main-loop root
//! *without symbols* — exactly what the stripped-release spindump lacked.
//!
//! ## How it works
//!
//! Each winit `ApplicationHandler` root calls [`beat`] on entry with a
//! [`Breadcrumb`] naming where the main thread is. `beat` bumps a monotonic
//! [`HEARTBEAT`] counter and stamps the [`BREADCRUMB`]. A background sampler
//! thread (started by [`start`]) wakes every [`sample_interval`] (half this
//! build's stall bar); if the
//! heartbeat has not advanced for longer than [`STALL_THRESHOLD`] *and* the last
//! breadcrumb is a WORK root (not the idle park point), the main thread is wedged
//! inside bounded event handling — it logs (and optionally aborts) with the last
//! breadcrumb NAME.
//!
//! ## The second word: phases
//!
//! A root is a coarse address — `UserEvent` is every control verb, every
//! config reload, every update step. Work that is known to be long, or that a
//! stall was once traced to, announces itself with [`phase`] for the length of
//! an RAII guard, and the stall line carries that [`Phase`] after the root
//! (`while inside \`UserEvent\`, in the headless pixel-backend redemption …`).
//! [`beat`] clears it, so a phase never outlives the root entry it was
//! announced in. The 2026-09-06 first-launch stall is the case that named
//! the first two phases; see [`Phase::PixelBackendRedeem`] for where that
//! line's window fell and what it could not be traced to without the word.
//!
//! ## Why the park-point exemption matters
//!
//! Between events the winit loop parks in the OS event wait (after
//! `about_to_wait`), so the heartbeat legitimately freezes while idle. Firing on
//! that would be pure noise. The fix: [`Breadcrumb::AboutToWait`] (and the
//! pre-loop [`Breadcrumb::Startup`]) are marked [`Breadcrumb::is_park_point`], and
//! the sampler NEVER reports a stall while the last breadcrumb is a park point. A
//! real freeze happens *inside* `window_event` / `user_event` / the resize-settle
//! flush — a WORK breadcrumb that never advances to `AboutToWait` — so it trips.
//!
//! ## ON IN RELEASE, at a coarser threshold
//!
//! It used to be off in release: [`enabled`] was `cfg!(debug_assertions) ||
//! $ATERM_WATCHDOG`, so a shipped binary spawned no sampler and reported
//! nothing. On 2026-08-30 that cost five hours. A self-recursive `OnceLock`
//! (`app_update_screen::debug_seamless_reexec_armed`, shipped in v0.65.0 and
//! v0.66.0) parked the main thread inside `user_event` on the first automatic
//! update apply. The window stayed up, the process stayed alive, and
//! `aterm.log` recorded nothing at all from the main thread from that second
//! on — the only evidence was a macOS hang report, which had to be
//! hand-symbolicated against the stripped release binary to name the frame.
//! That is the exact scenario this module's own header says it exists for
//! ("*without symbols* — exactly what the stripped-release spindump lacked"),
//! and it was compiled out of the build that needed it.
//!
//! So the sampler now runs in EVERY build. What changes with the build is the
//! threshold, not the existence of the guard:
//!
//! * debug builds, or any build with `$ATERM_WATCHDOG` set —
//!   [`STALL_THRESHOLD`] (500 ms). Tight, for catching a regression while
//!   developing it.
//! * a shipped release binary — [`RELEASE_STALL_THRESHOLD`] (5 s). A main
//!   thread frozen at a WORK root for five seconds is never normal, so the
//!   coarser bar keeps a slow-but-progressing frame, a huge paste or a cold
//!   font scan from ever writing an alarming line, while still turning a
//!   PERMANENT wedge into a named log line within seconds instead of never.
//!
//! `ATERM_WATCHDOG=off` disables the sampler entirely; `ATERM_WATCHDOG=abort`
//! still `process::abort()`s on a detected stall (CI / repro). Everything else
//! logs at error level and keeps going. [`beat`] is one monotonic clock read and
//! a handful of relaxed atomic writes in every build either way (negligible on
//! the hot event path, and the clock read is what buys the turn census below),
//! and the sampler is one thread asleep 99.99% of the time.
//!
//! A stall that PERSISTS is re-reported every [`STALL_REPEAT_INTERVAL`] with
//! the accumulated frozen duration, so the log distinguishes "wedged for a
//! moment" from "wedged for an hour and never recovered" — the distinction the
//! 0.65.0 log could not make, because it said nothing at all.
//!
//! ## The TURN CENSUS: the band between a slow frame and a wedge
//!
//! The sampler above reports a park that is STILL going after 5 s in a shipped
//! build, and `metrics` publishes `max_redraw_total_ms` for a park INSIDE a
//! redraw. Between those two bars lay a band no instrument in the process could
//! name: a main thread parked 100–600 ms inside a NON-redraw handler — the
//! `Wake::Output` arm runs status observation, bulk-scrollback routing, title
//! drift and search refresh before the redraw fan-out — recovers long before
//! the watchdog's threshold and never enters the redraw timer, so it left no
//! attributable trace at all. The user still waited for it, and so did
//! `present_latency` and `input_present`: that is how a live line came to read
//! `max_present_latency_ms=560.54` beside `max_redraw_total_ms=13.84` with
//! `present_drops=0`, a reading whose producer nothing published could name and
//! which was duly read as a GPU problem.
//!
//! [`beat`] already knows WHERE the main thread is and WHEN it got there, so it
//! is the one place that can close that band for free. Each beat stamps the
//! clock; the NEXT beat prices the span that just ended and books it to the root
//! that owned it ([`TurnLedger`]), published on the same `metrics` line as
//! `max_turn_ms` / `max_turn_owner` / `max_turn_at_ms`, `last_turn_ms`,
//! `turns`, and `long_turns` over [`LONG_TURN_THRESHOLD_NS`]. A
//! `max_turn_ms=312 max_turn_owner=user_event` beside `max_redraw_total_ms=13.84`
//! says the park was in the Output arm's bookkeeping rather than the GPU, and
//! says it on the line the reader was already looking at.
//!
//! The census and the sampler are complements, and neither replaces the other:
//! the census prices parks that END (it is closed by the next beat), the sampler
//! names parks that do NOT (nothing closes those, which is the point). A park
//! point's span is never booked — an idle wait, a modal dialog and the
//! update-handoff park are designed freezes, and pricing them would be the same
//! noise the sampler's park-point exemption exists to avoid.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How often the sampler thread wakes to inspect the heartbeat: HALF the bar
/// this build judges a stall against ([`threshold`]), so two consecutive
/// samples always straddle it.
///
/// DERIVED, NOT CONSTANT (2026-09-22 efficiency audit). It was a flat 250 ms
/// while the bar it serves is 500 ms in a debug build and
/// [`RELEASE_STALL_THRESHOLD`] (5 s) in a shipped one — so a release binary woke
/// 4 times a second, twenty samples per threshold window, to answer a question
/// that needs two. Measured on the owner's idle machine: the watchdog was the
/// single largest source of aterm's kernel wakeups at rest (4.0 of ~6.3 raw
/// wakes/s, before macOS coalescing), on a fanless laptop where deep-idle
/// residency is the whole battery story. Halving the threshold keeps the
/// detection property exactly — a wedge is still caught within 1.5 thresholds —
/// and the DEBUG lane is byte-identical at 250 ms, so nothing a developer
/// watches changes.
fn sample_interval() -> Duration {
    threshold() / 2
}

/// How long the heartbeat may stay frozen at a WORK breadcrumb before it counts
/// as a stall, in a DEBUG build or under an explicit `$ATERM_WATCHDOG`. The
/// sampler wakes at half this ([`sample_interval`]), so a genuine wedge is
/// caught within ~750 ms while a single slow-but-progressing frame never trips.
const STALL_THRESHOLD: Duration = Duration::from_millis(500);

/// The same bar for a SHIPPED release binary, where the reader is a user's
/// `aterm.log` rather than a developer's terminal.
///
/// The trade is deliberate and one-directional: 500 ms of
/// main-thread work is unusual but not impossible in the field (a cold font
/// catalog, a very large paste, a first-frame pipeline build), and an error line
/// for one of those is noise that teaches a reader to ignore the guard. Five
/// seconds is not survivable UI latency under any reading — nothing in this
/// program is allowed to hold the main thread that long — so a line at 5 s is
/// always a real finding. It still converts a PERMANENT park from silence into
/// a named log line within seconds, which is the whole point.
const RELEASE_STALL_THRESHOLD: Duration = Duration::from_secs(5);

/// How often a still-frozen main thread is re-reported after its first line.
/// One line proves a wedge happened; the repeats prove it never ended, and
/// carry the growing duration.
const STALL_REPEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Monotonic main-thread liveness counter. [`beat`] increments it on every winit
/// root entry; the sampler watches it for a frozen span.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);

/// The last main-loop root [`beat`] was called from, as a [`Breadcrumb`] `u8`.
/// Initialised to [`Breadcrumb::Startup`] so the pre-event-loop launch window is
/// treated as a park point (no false stall during heavy synchronous startup).
static BREADCRUMB: AtomicU8 = AtomicU8::new(Breadcrumb::Startup as u8);

/// The work the main thread last ANNOUNCED inside the current root, as a
/// [`Phase`] `u8` — the second word of a stall line, where the root alone is
/// not enough. [`beat`] clears it: a phase is scoped to the work inside ONE
/// root entry and never carried into the next, and a guard restores its outer
/// phase only over its OWN announcement (a compare-exchange on drop), so a
/// stale announcement can never name the wrong work — not even a nested
/// guard's outer phase, dropped after a beat cleared the cell for a root
/// that never announced it. Written through [`phase`], read by the sampler.
static PHASE: AtomicU8 = AtomicU8::new(Phase::None as u8);

/// The main-loop roots the watchdog can pin a stall to. `#[repr(u8)]` so it round
/// trips through the [`BREADCRUMB`] atomic with no allocation and no symbols — the
/// NAME survives into a stripped-release log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Breadcrumb {
    /// Before the event loop runs (synchronous launch). Park point: startup does
    /// heavy main-thread work by design and must not trip the guard.
    Startup = 0,
    /// `about_to_wait`: the loop finished this iteration and is about to park in
    /// the OS event wait. Park point — the heartbeat legitimately freezes here.
    AboutToWait = 1,
    /// `window_event`: OS-delivered input/resize/close for a window. WORK root.
    WindowEvent = 2,
    /// `user_event`: a proxy `Wake` (control socket, config reload, …). WORK root.
    UserEvent = 3,
    /// `new_events`: a `WaitUntil` deadline fired (blink / bell / resize settle).
    /// WORK root.
    NewEvents = 4,
    /// The resize-settle flush — the coalesced final width lands here, the exact
    /// path that funnels into `Grid::resize`'s width-reflow sink. WORK root.
    ResizeSettle = 5,
    /// The overlap-handoff park + readiness wait: the main thread deliberately
    /// blocks (bounded) while the update child boots under the frozen frame,
    /// beating every poll tick. Park point — a frozen heartbeat here is the
    /// designed wait, not a stall (the wait itself is deadline-bounded).
    UpdateHandoff = 6,
    /// A NESTED MODAL RUN LOOP spun from inside a work root — `-[NSAlert
    /// runModal]` (`menu::confirm`, `menu::notify`) and `-[NSOpenPanel
    /// runModal]` (`menu::choose_local_file`). AppKit is running the loop and
    /// the user is looking at a dialog; winit's observers fire but bail because
    /// its handler cell is borrowed for the outer event, so no App root runs and
    /// nothing beats. Park point — the freeze lasts exactly as long as the user
    /// takes to answer. Found by the 2026-09-02 abort audit: without it every
    /// confirm/notify/open dialog left open past the threshold logged a spurious
    /// `MAIN-THREAD STALL`, and aborted the process under `ATERM_WATCHDOG=abort`.
    /// Entered and left through [`park_modal`], never by a bare [`beat`].
    Modal = 7,
}

impl Breadcrumb {
    /// Reconstruct a breadcrumb from its stored `u8` (unknown values fold to
    /// [`Breadcrumb::Startup`], the benign park point).
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Breadcrumb::AboutToWait,
            2 => Breadcrumb::WindowEvent,
            3 => Breadcrumb::UserEvent,
            4 => Breadcrumb::NewEvents,
            5 => Breadcrumb::ResizeSettle,
            6 => Breadcrumb::UpdateHandoff,
            7 => Breadcrumb::Modal,
            _ => Breadcrumb::Startup,
        }
    }

    /// The same root as a published METRIC label: snake_case, matching the owner
    /// vocabulary the rest of the `metrics` line already speaks
    /// (`deadline_owner=frame_cap`, `wake_owner=session_status`), so a reader
    /// never has to know that one field spells its owners differently from its
    /// neighbours. Stable like [`Breadcrumb::name`] — it is a wire label.
    pub fn metric_name(self) -> &'static str {
        match self {
            Breadcrumb::Startup => "startup",
            Breadcrumb::AboutToWait => "about_to_wait",
            Breadcrumb::WindowEvent => "window_event",
            Breadcrumb::UserEvent => "user_event",
            Breadcrumb::NewEvents => "new_events",
            Breadcrumb::ResizeSettle => "resize_settle",
            Breadcrumb::UpdateHandoff => "update_handoff",
            Breadcrumb::Modal => "modal",
        }
    }

    /// Stable, symbol-free name for the log line.
    pub fn name(self) -> &'static str {
        match self {
            Breadcrumb::Startup => "Startup",
            Breadcrumb::AboutToWait => "AboutToWait",
            Breadcrumb::WindowEvent => "WindowEvent",
            Breadcrumb::UserEvent => "UserEvent",
            Breadcrumb::NewEvents => "NewEvents",
            Breadcrumb::ResizeSettle => "ResizeSettle",
            Breadcrumb::UpdateHandoff => "UpdateHandoff",
            Breadcrumb::Modal => "Modal",
        }
    }

    /// A root where a frozen heartbeat is EXPECTED (idle park / pre-loop startup),
    /// so the sampler must not report a stall while parked here.
    fn is_park_point(self) -> bool {
        matches!(
            self,
            Breadcrumb::Startup
                | Breadcrumb::AboutToWait
                | Breadcrumb::UpdateHandoff
                | Breadcrumb::Modal
        )
    }
}

/// The pure stall decision, factored out of the sampler loop so it is
/// deterministically testable: a frozen heartbeat is a STALL iff the main thread
/// is sitting in a WORK root (not a park point) and has been frozen at least
/// `threshold` — [`STALL_THRESHOLD`] for the dev lane,
/// [`RELEASE_STALL_THRESHOLD`] for a shipped binary. This is the exact
/// predicate that FLAGS the L0 hazard — a wedge in the `ResizeSettle` reflow
/// that never reaches `AboutToWait`, or a main thread parked forever inside
/// `user_event` on a lazy-init cycle.
fn is_stall_at(bc: Breadcrumb, frozen: Duration, threshold: Duration) -> bool {
    !bc.is_park_point() && frozen >= threshold
}

/// The sampler's detection state machine, split out of the background thread so
/// its behaviour (fire once per contiguous stall, re-arm on progress, never fire
/// at a park point) is testable against a SYNTHETIC clock — no real sleeps, no
/// global logger. The live thread just feeds it real samples.
struct Sampler {
    /// The heartbeat value last observed to advance.
    last_beat: u64,
    /// When the heartbeat last advanced (start of the current frozen span).
    last_advance: Instant,
    /// Whether the current contiguous stall was already reported (fire once).
    reported: bool,
    /// When the current contiguous stall was last reported, for the repeat
    /// cadence. `None` until the first report.
    last_report: Option<Instant>,
    /// How many times THIS contiguous stall has been reported. Lives here, with
    /// the rest of the per-stall state, so it re-arms on recovery: kept in the
    /// sampler loop instead, a second stall hours after the first was announced
    /// as "STALL CONTINUES" and two distinct incidents read as one wedge.
    reports: u32,
    /// The bar this sampler judges against — [`STALL_THRESHOLD`] for the dev
    /// lane, [`RELEASE_STALL_THRESHOLD`] for a shipped binary.
    threshold: Duration,
}

impl Sampler {
    fn with_threshold(now: Instant, beat: u64, threshold: Duration) -> Self {
        Self {
            last_beat: beat,
            last_advance: now,
            reported: false,
            last_report: None,
            reports: 0,
            threshold,
        }
    }

    /// Fold one sample. Returns `Some(bc)` when this sample should be REPORTED:
    /// once when a contiguous stall crosses the threshold, and then once per
    /// [`STALL_REPEAT_INTERVAL`] for as long as it lasts. A main thread that
    /// never comes back is the case this guard exists for, and one line an hour
    /// ago is not the same evidence as a line saying it is still frozen now.
    fn poll(&mut self, now: Instant, cur_beat: u64, bc: Breadcrumb) -> Option<Breadcrumb> {
        if cur_beat != self.last_beat {
            // Progress: the main thread is alive. Reset the stall clock + re-arm.
            self.last_beat = cur_beat;
            self.last_advance = now;
            self.reported = false;
            self.last_report = None;
            self.reports = 0;
            return None;
        }
        if bc.is_park_point() {
            // Idle / startup park: a frozen heartbeat is expected here. Keep the
            // clock reset so leaving idle starts a fresh span.
            self.last_advance = now;
            self.reported = false;
            self.last_report = None;
            self.reports = 0;
            return None;
        }
        let frozen = now.saturating_duration_since(self.last_advance);
        if !is_stall_at(bc, frozen, self.threshold) {
            return None;
        }
        let due = match self.last_report {
            None => !self.reported,
            Some(at) => now.saturating_duration_since(at) >= STALL_REPEAT_INTERVAL,
        };
        if !due {
            return None;
        }
        self.reported = true;
        self.last_report = Some(now);
        self.reports = self.reports.saturating_add(1);
        Some(bc)
    }
}

/// A main-loop turn at or over this is COUNTED as long. One 30 fps frame budget
/// — the same bar [`crate::metrics::SLOW_FRAME_THRESHOLD_NS`] applies to a
/// frame's render, so "long" means one thing across the whole `metrics` line:
/// this turn cannot have been part of a frame delivered on time.
const LONG_TURN_THRESHOLD_NS: u64 = crate::metrics::SLOW_FRAME_THRESHOLD_NS;

/// The main-loop TURN census: how long the main thread spent in each work root,
/// attributed to that root. See the module header for the band it closes.
///
/// A "turn" is the span from one [`beat`] to the next, charged to the root that
/// was current when it opened. That is the span the USER waited, which is why it
/// is the span booked: the handler body PLUS whatever the winit loop did after
/// the handler returned and before the next root opened. It is deliberately not
/// a handler-body timer — a park between two handlers is still a main thread
/// that is not painting, and a census with a hole in it invites exactly the
/// "this outlier has no producer" reading it exists to end.
///
/// A `WindowEvent` turn therefore CONTAINS the redraw when the event was
/// `RedrawRequested`, so `max_turn_ms` is read AGAINST `max_redraw_total_ms`:
/// both large is a slow frame (already attributable, already published), a large
/// `max_turn_ms` with a small `max_redraw_total_ms` is a park no other
/// instrument in the process reaches — the finding this census answers.
///
/// Every field is written by the main thread alone (the only caller of [`beat`]),
/// so a max and its owner cannot tear against each other. A concurrent
/// [`reset_turn_census`] from the control socket can at worst clear a max between
/// its two writes, leaving a fresh window with a stale owner label on a zero —
/// the same benign race every `max_`/owner pair on that line already accepts.
struct TurnLedger {
    /// When the CURRENT root was entered (`crate::metrics::now_ns` clock).
    /// 0 = disarmed: no turn is open, so the next beat books nothing. That is
    /// the state at process start and immediately after a reset, both of which
    /// would otherwise book a span that began outside the window.
    open_ns: AtomicU64,
    /// The most recently booked turn.
    last_ns: AtomicU64,
    /// The worst booked turn since reset, the root that owned it, and when it
    /// ended. A max with no owner and no instant cannot end an investigation —
    /// the lesson `max_present_latency_ms=560.54` already taught this line.
    max_ns: AtomicU64,
    max_owner: AtomicU8,
    max_at_ns: AtomicU64,
    /// How many turns were booked, and how many were at or over
    /// [`LONG_TURN_THRESHOLD_NS`]. READ THE MAX WITH THESE: one 600 ms turn in a
    /// window of 40,000 is a hitch; the same max with `long_turns=3000` is a main
    /// thread that is late all the time.
    turns: AtomicU64,
    long_turns: AtomicU64,
}

impl TurnLedger {
    const fn new() -> Self {
        Self {
            open_ns: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            max_owner: AtomicU8::new(Breadcrumb::Startup as u8),
            max_at_ns: AtomicU64::new(0),
            turns: AtomicU64::new(0),
            long_turns: AtomicU64::new(0),
        }
    }

    /// The span a turn that just ended is ATTRIBUTABLE for, or `None` when it is
    /// not this census's to price: a park point (idle, startup, a modal dialog,
    /// the update-handoff wait — all designed freezes), or a disarmed stamp (no
    /// turn was open, so the span began outside this window).
    ///
    /// Pure, so the attribution rule is testable without the process-global
    /// ledger that every other test in this binary also writes.
    fn attributable_span(previous: Breadcrumb, open_ns: u64, now_ns: u64) -> Option<u64> {
        if open_ns == 0 || previous.is_park_point() {
            return None;
        }
        Some(now_ns.saturating_sub(open_ns))
    }

    /// Close the turn `previous` owned at `now_ns` and open one for the root
    /// being entered. Returns the span booked, or `None` when there was nothing
    /// attributable to book.
    fn close(&self, previous: Breadcrumb, now_ns: u64) -> Option<u64> {
        let open = self.open_ns.swap(now_ns, Ordering::Relaxed);
        let span = Self::attributable_span(previous, open, now_ns)?;
        self.last_ns.store(span, Ordering::Relaxed);
        self.turns.fetch_add(1, Ordering::Relaxed);
        if span >= LONG_TURN_THRESHOLD_NS {
            self.long_turns.fetch_add(1, Ordering::Relaxed);
        }
        if self.max_ns.fetch_max(span, Ordering::Relaxed) < span {
            self.max_owner.store(previous as u8, Ordering::Relaxed);
            self.max_at_ns.store(now_ns, Ordering::Relaxed);
        }
        Some(span)
    }

    fn snapshot(&self) -> TurnCensus {
        let turns = self.turns.load(Ordering::Relaxed);
        TurnCensus {
            last_ns: self.last_ns.load(Ordering::Relaxed),
            max_ns: self.max_ns.load(Ordering::Relaxed),
            // No booked turn means no owner: `startup` (the atomic's initial
            // value) would be a claim about a root that never ran.
            max_owner: (turns != 0)
                .then(|| Breadcrumb::from_u8(self.max_owner.load(Ordering::Relaxed))),
            max_at_ns: self.max_at_ns.load(Ordering::Relaxed),
            turns,
            long_turns: self.long_turns.load(Ordering::Relaxed),
        }
    }

    /// Clear the window, INCLUDING the open stamp: the turn straddling a reset
    /// began before the window it would be booked to, and a driver that resets,
    /// drives a workload and reads must see that workload's worst turn.
    fn reset(&self) {
        self.open_ns.store(0, Ordering::Relaxed);
        self.last_ns.store(0, Ordering::Relaxed);
        self.max_ns.store(0, Ordering::Relaxed);
        self.max_owner
            .store(Breadcrumb::Startup as u8, Ordering::Relaxed);
        self.max_at_ns.store(0, Ordering::Relaxed);
        self.turns.store(0, Ordering::Relaxed);
        self.long_turns.store(0, Ordering::Relaxed);
    }
}

/// The process-global turn census. See [`TurnLedger`].
static TURNS: TurnLedger = TurnLedger::new();

/// A read of [`TURNS`] for the `metrics` verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnCensus {
    /// The most recently booked turn.
    pub last_ns: u64,
    /// The worst booked turn since the last reset.
    pub max_ns: u64,
    /// The root that owned the worst turn, or `None` when none was booked.
    pub max_owner: Option<Breadcrumb>,
    /// When the worst turn ended, on the `crate::metrics::now_ns` process clock.
    pub max_at_ns: u64,
    /// Turns booked since the last reset, and the subset at or over
    /// [`LONG_TURN_THRESHOLD_NS`].
    pub turns: u64,
    pub long_turns: u64,
}

/// Record that the main thread just entered `bc`, and price the turn that ended.
/// A monotonic clock read and a handful of relaxed atomic writes — cheap enough
/// to sit on the hot event path in every build, and the only place in the process
/// that can price a park OUTSIDE the redraw (see the module header). The
/// breadcrumb is stamped BEFORE the heartbeat bumps so the sampler never reads a
/// fresh count against a stale location, and any announced [`Phase`] is cleared
/// with it: the work a phase names belongs to the root that announced it.
#[inline]
pub fn beat(bc: Breadcrumb) {
    beat_at(bc, crate::metrics::now_ns());
}

/// [`beat`] with the clock passed IN, returning the span it booked — a real clock
/// would make any assertion about a span a race.
#[inline]
fn beat_at(bc: Breadcrumb, now_ns: u64) -> Option<u64> {
    beat_into(&TURNS, &PHASE, bc, now_ns)
}

/// The beat path with its LEDGER and its PHASE cell passed in too, on the
/// [`Sampler`] precedent: factored out so the stamp-close-clear-bump sequence is
/// testable against cells no other test in this binary can write, and no
/// `metrics reset` can clear mid-assertion. The phase cell needs that as much as
/// the ledger does: every lib.rs test that arms a deferral before
/// `ensure_pixel_backend` holds [`Phase::PixelBackendRedeem`] on [`PHASE`] for
/// the length of a font seal, and none of them takes this module's beat lock.
#[inline]
fn beat_into(turns: &TurnLedger, phase: &AtomicU8, bc: Breadcrumb, now_ns: u64) -> Option<u64> {
    let previous = Breadcrumb::from_u8(BREADCRUMB.swap(bc as u8, Ordering::Relaxed));
    let booked = turns.close(previous, now_ns);
    // A root entry starts with NO announced phase (see `PHASE`): one more
    // relaxed store on the hot path, the only way a phase ever ends besides its
    // guard dropping, and stamped BEFORE the heartbeat bumps for the same
    // reason the breadcrumb is — the sampler must never read a fresh count
    // against a stale word.
    phase.store(Phase::None as u8, Ordering::Relaxed);
    HEARTBEAT.fetch_add(1, Ordering::Relaxed);
    booked
}

/// The turn census, for the `metrics` verb.
#[must_use]
pub fn turn_census() -> TurnCensus {
    TURNS.snapshot()
}

/// Clear the turn census. Called by [`crate::metrics::reset`], so the census is a
/// window stat like every other `max_` on that line.
pub fn reset_turn_census() {
    TURNS.reset();
}

/// The turn-census fields of the `metrics` summary, text form.
///
/// Spliced as ONE fragment (the `echo_rtt::percentile_fields_text` discipline) so
/// the text and JSON summaries cannot drift apart and neither giant `format!`
/// grows seven more positional holes.
///
/// `max_turn_at_ms` is on the `crate::metrics::now_ns` PROCESS clock — the one
/// `metrics_now_ms` reads at the same instant — so "how long ago" is one
/// subtraction, the rule every other `_at_ms` on the line already follows.
#[must_use]
pub fn turn_census_fields_text() -> String {
    let c = turn_census();
    let ms = |ns: u64| ns as f64 / 1e6;
    format!(
        " max_turn_ms={:.2} max_turn_owner={} max_turn_at_ms={:.2} last_turn_ms={:.2} \
         turns={} long_turns={} long_turn_threshold_ms={:.1}",
        ms(c.max_ns),
        c.max_owner.map_or("none", Breadcrumb::metric_name),
        ms(c.max_at_ns),
        ms(c.last_ns),
        c.turns,
        c.long_turns,
        ms(LONG_TURN_THRESHOLD_NS),
    )
}

/// Field-for-field JSON twin of [`turn_census_fields_text`] — a leading comma, so
/// it splices straight in before the closing brace.
#[must_use]
pub fn turn_census_fields_json() -> String {
    let c = turn_census();
    let ms = |ns: u64| ns as f64 / 1e6;
    format!(
        ",\"max_turn_ms\":{:.2},\"max_turn_owner\":\"{}\",\"max_turn_at_ms\":{:.2},\
         \"last_turn_ms\":{:.2},\"turns\":{},\"long_turns\":{},\
         \"long_turn_threshold_ms\":{:.1}",
        ms(c.max_ns),
        c.max_owner.map_or("none", Breadcrumb::metric_name),
        ms(c.max_at_ns),
        ms(c.last_ns),
        c.turns,
        c.long_turns,
        ms(LONG_TURN_THRESHOLD_NS),
    )
}

/// The breadcrumb the main thread last stamped.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn current() -> Breadcrumb {
    Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Relaxed))
}

/// Park the watchdog for the duration of a nested modal run loop.
///
/// Stamps [`Breadcrumb::Modal`] on entry and, on drop, restores the breadcrumb
/// that was current when the modal opened WITH a fresh beat — the outer work
/// root resumes from a heartbeat that says "now", not from one frozen since
/// before the dialog. Must be held on the main thread across the `runModal`
/// send and nothing else; a guard that outlives its dialog is a park that
/// never ends, which is exactly the wedge the sampler exists to name.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[must_use = "the park lasts only as long as the guard lives"]
pub fn park_modal() -> ModalPark {
    let previous = current();
    beat(Breadcrumb::Modal);
    ModalPark { previous }
}

/// The RAII half of [`park_modal`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct ModalPark {
    previous: Breadcrumb,
}

impl Drop for ModalPark {
    fn drop(&mut self) {
        beat(self.previous);
    }
}

/// The work a stall line can name INSIDE a root — the answer to "which of the
/// many things `user_event` does was it doing?", which the root alone cannot
/// give. `#[repr(u8)]` for the same reason [`Breadcrumb`] is: the NAME must
/// survive into a stripped-release log line with no allocation and no symbols.
///
/// Added after the 2026-09-06 first-launch stall on an Intel MacBook Pro:
/// `MAIN-THREAD STALL: no heartbeat for 5.045556453s while inside
/// \`UserEvent\`` was recorded 14 s into the first headless launch on the
/// machine, and `UserEvent` covers every control verb. Which verb had to be
/// inferred from the instance's stderr and its socket-open time, and what
/// that verb was doing on the main thread had to be re-measured with a
/// probe afterwards — the one extra word this carries. A phase is announced
/// with [`phase`] and lasts as long as its guard; the sampler prints the
/// phase that is current when it reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// Nothing announced: the root is all the line says.
    None = 0,
    /// `App::ensure_pixel_backend` — a headless run's FIRST pixel demand
    /// redeeming, on the main thread, what the launch deferred: the font seal
    /// (three font files read and parsed; Apple Color Emoji is 190 MB of it
    /// on macOS), the CPU face fork, and `GpuRenderer::new_with_family`. That
    /// last leg is NOT just the device: `new_with_family` spawns a font
    /// thread (`Renderer::from_system_with_family` — font resolution, file
    /// read, parse — then `prewarm_ascii`), builds the GPU context on the
    /// calling thread, JOINS the font thread, and only then calls
    /// `from_parts`, so the leg costs max(context, font thread) plus
    /// `from_parts`. On the wgpu arms the context is the instance, the
    /// adapter, the device and the context's tail (`GpuContextTail`), and
    /// `from_parts`, after the join, compiles the shader and builds the
    /// pipelines. On the macOS Metal arm `GpuContext::new` only NAMES the
    /// preferred device and keeps nothing of it but the name — on the
    /// system-default pick (`ATERM_GPU_POWER=high`, or no low-power GPU
    /// listed) that naming IS `MTLCreateSystemDefaultDevice` — while
    /// `MetalArmLive` mints the device (calling `Device::preferred` again:
    /// on that pick, a second `MTLCreateSystemDefaultDevice`), its queue and
    /// `cell.metal` at the first armed frame — inside [`Phase::ImageCapture`]
    /// when the first pixel demand is an `image` capture (a `snapshot`'s or
    /// `video`'s first frame mints under no phase) — and `from_parts`' shader
    /// and pipeline legs are `cfg(wgpu_arm)`; so there the leg is max(device
    /// name, font discovery + parse + prewarm) plus a struct-assembly tail.
    /// The redemption's own log line splits the max (font thread, join wait).
    ///
    /// This is where the 2026-09-06 stall's reported window fell. The line
    /// (`no heartbeat for 5.045556453s while inside \`UserEvent\``) was
    /// written at 14:29:36.4, on a headless instance whose stderr did not
    /// name its device ("GPU rendering on AMD Radeon Pro 560") until
    /// 14:29:40.7 (the file's last write; that line is written AFTER
    /// `new_with_family` returns) and whose capture PNG was created at
    /// 14:29:41.7. So the last heartbeat (14:29:31.4) and the whole reported
    /// window lie BEFORE that device line: in the seal, the fork and the
    /// `new_with_family` legs, ~9 s in all, of a ~10 s capture. Which of the
    /// three, and inside the third whether the device name or the font
    /// thread, is what the log line's legs exist to say next time. That
    /// instance predates `Device::preferred`: its `GpuContext::new` and its
    /// `MetalArmLive::new` both took `MTLCreateSystemDefaultDevice`, the
    /// call Apple documents as switching a dual-GPU Mac to the discrete GPU.
    /// So aterm's first call that could set off a switch to the Radeon was
    /// here, in the device leg; the first-frame mint made a second one, in
    /// the 945 ms tail (see [`Phase::ImageCapture`]), after `GpuContext::new`
    /// had released its reference to the device it named; nothing observed
    /// the mux or timed either call. The aterm.log the line sits in
    /// holds no other `UserEvent` stall line (checked 2026-09-10; every
    /// other one is inside `NewEvents`). The surviving re-measurements
    /// (that evening, debug build, on the Intel HD Graphics
    /// 630, with a test build running alongside) put the redemption at
    /// 509-533 ms: font seal 488-510 ms, fork 0, `new_with_family` 21-22 ms,
    /// install 0.
    PixelBackendRedeem = 1,
    /// `App::render_image` — the `image` verb's capture on the main thread,
    /// the control verb the 2026-09-06 stall was inside. Not the only verb
    /// that does renderer work there — the offscreen present-real `video`
    /// loop and the SIGUSR1 `snapshot` path render on the main thread too,
    /// and announce nothing but the redemption they nest — but the one that
    /// finding named. Headless, when a capture is the run's first pixel
    /// demand, the redemption above is nested in it, and on macOS that
    /// capture also mints the Metal device, its command queue and
    /// `cell.metal`, then demand-builds the three pipelines the first frame
    /// binds — the shader-cache-sensitive work. The device is
    /// `Device::preferred`'s pick: the low-power GPU of a dual-GPU Mac,
    /// unless `ATERM_GPU_POWER=high` asks for the system default, which on
    /// such a Mac is the discrete GPU. On that pick the naming in
    /// [`Phase::PixelBackendRedeem`]'s device leg is aterm's FIRST call that
    /// can set off the switch — `MTLCreateSystemDefaultDevice`, the call
    /// Apple documents as switching a dual-GPU Mac to the discrete GPU — and,
    /// `GpuContext::new` keeping only the name, this mint makes a SECOND.
    /// Whether that second call switches again, and in which of the two the
    /// power-up's latency is paid, was not measured (nothing observed the
    /// mux), so neither phase is ruled out for a mux stall.
    ///
    /// In the 2026-09-06 stall that mint was the TAIL, not the window: the
    /// instance's stderr named its device (the Radeon Pro 560; the pick
    /// predates `Device::preferred`) at 14:29:40.7, after the stall line had
    /// been written, and its PNG was created at 14:29:41.7 — so the
    /// first-frame mint on the discrete GPU (that build's second
    /// `MTLCreateSystemDefaultDevice`) sits in the 945 ms between those
    /// two file-system timestamps, with the install, the frame, the encode
    /// and the write, after the redemption's ~9 s of a ~10 s capture. Nothing
    /// timed the mint alone, and the stall line's wording cites no figure.
    /// The surviving in-situ line from this phase is the debug watchdog's
    /// 504 ms `in an \`image\` capture` on the Intel GPU that evening — a
    /// 509 ms redemption (font seal 488 ms) with the capture around it,
    /// under a concurrent test build, named as the capture because the
    /// phase is read at the report and the redemption had logged its own
    /// line 166 ms earlier. Nothing was building at 14:29 (the release
    /// build's log last wrote 22 s before the instance's first log line; the
    /// next test build's log was created 7 s after the stall line).
    ImageCapture = 2,
}

impl Phase {
    /// Reconstruct a phase from its stored `u8` (unknown values fold to
    /// [`Phase::None`], which adds nothing to the line).
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Phase::PixelBackendRedeem,
            2 => Phase::ImageCapture,
            _ => Phase::None,
        }
    }

    /// Stable, symbol-free wording for the log line.
    pub fn describe(self) -> &'static str {
        match self {
            Phase::None => "",
            Phase::PixelBackendRedeem => {
                "the headless pixel-backend redemption (the deferred font seal, the face \
                 fork, and the GPU context with its font thread)"
            }
            Phase::ImageCapture => {
                "an `image` capture (headless, the first one also mints the GPU device and \
                 compiles its shaders)"
            }
        }
    }

    /// The clause the stall line carries after the root name: empty for
    /// [`Phase::None`], `, in <description>` otherwise.
    fn clause(self) -> String {
        match self {
            Phase::None => String::new(),
            named => format!(", in {}", named.describe()),
        }
    }
}

/// Announce the work the main thread is about to do inside the current root.
///
/// Stamps `p` now and, on drop, restores the phase that was current when the
/// guard was made — but only over `p` itself, so a phase nested inside
/// another (the redemption inside a capture) reads correctly at every
/// instant, the outer phase comes back when the inner one ends, and a guard
/// a [`beat`] outlived (the beat cleared the cell for the next root) restores
/// nothing: the inner guard of a nested pair must not re-announce the OUTER
/// phase into a root that never announced it. Never a beat: the heartbeat is
/// the root's, and a phase that beat would hide the very stall it exists to
/// name.
#[must_use = "the announcement lasts only as long as the guard lives"]
pub fn phase(p: Phase) -> PhaseGuard {
    announce(&PHASE, p)
}

/// The announcement itself, over an EXPLICIT cell — [`phase`] is this on the
/// process-global [`PHASE`], and the phase test runs it on a cell of its own
/// (see [`beat_into`] for why). The guard carries the cell it wrote so its
/// drop restores the right one.
fn announce(cell: &'static AtomicU8, p: Phase) -> PhaseGuard {
    let previous = phase_in(cell);
    cell.store(p as u8, Ordering::Relaxed);
    PhaseGuard {
        cell,
        written: p,
        previous,
    }
}

/// The phase the main thread last announced (none between roots).
pub fn current_phase() -> Phase {
    phase_in(&PHASE)
}

/// The phase a cell holds.
fn phase_in(cell: &AtomicU8) -> Phase {
    Phase::from_u8(cell.load(Ordering::Relaxed))
}

/// The RAII half of [`phase`]: the cell it wrote, what it wrote there, and
/// what stood before.
pub struct PhaseGuard {
    cell: &'static AtomicU8,
    written: Phase,
    previous: Phase,
}

impl Drop for PhaseGuard {
    /// Restore `previous` only over this guard's own announcement. Anything
    /// else in the cell means a [`beat`] cleared it for a new root (guards
    /// are stack-scoped on one thread, so a later announcement is always
    /// dropped before this one), and a stale guard leaves that clean slate
    /// alone rather than naming work the new root is not doing. `Relaxed`
    /// like every other access to the cell: one writer thread, and a sampler
    /// that only reads.
    fn drop(&mut self) {
        let _ = self.cell.compare_exchange(
            self.written as u8,
            self.previous as u8,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

/// The stall line, assembled from the sampler's facts alone so its wording is
/// testable without a thread or a logger: the FIRST report of a contiguous
/// stall names the root and the announced phase and says what the class of
/// hazard is; every later one says the same stall is still there.
fn stall_message(reports: u32, frozen: Duration, bc: Breadcrumb, phase: Phase) -> String {
    let phase = phase.clause();
    if reports == 1 {
        format!(
            "MAIN-THREAD STALL: no heartbeat for {frozen:?} while inside `{}`{phase} — \
             the UI is not responding. Either unbounded work under a contended lock \
             (the L0 freeze hazard) or a park that will never end (a lock or lazy-init \
             cycle). This line names the main-loop root without symbols; a hang report \
             is not required to find it.",
            bc.name()
        )
    } else {
        format!(
            "MAIN-THREAD STALL CONTINUES: still no heartbeat after {frozen:?} inside \
             `{}`{phase} — this is a wedge, not a slow frame.",
            bc.name()
        )
    }
}

/// Whether the watchdog sampler should run. EVERY build, unless explicitly
/// switched off with `ATERM_WATCHDOG=off` — see the module header for why a
/// release binary is the build that needs this most.
fn enabled() -> bool {
    !std::env::var("ATERM_WATCHDOG").is_ok_and(|v| {
        let v = v.trim();
        v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("0") || v.is_empty()
    })
}

/// The bar this build judges a stall against: tight when a developer is
/// watching (debug, or an explicit `$ATERM_WATCHDOG`), coarse in a shipped
/// binary where the reader is a user's `aterm.log`.
fn threshold() -> Duration {
    if cfg!(debug_assertions) || std::env::var_os("ATERM_WATCHDOG").is_some() {
        STALL_THRESHOLD
    } else {
        RELEASE_STALL_THRESHOLD
    }
}

/// Whether a detected stall should `process::abort()` (repro / CI) rather than
/// just log. Opt-in via `ATERM_WATCHDOG=abort`.
fn abort_on_stall() -> bool {
    std::env::var("ATERM_WATCHDOG").is_ok_and(|v| v.eq_ignore_ascii_case("abort"))
}

/// Spawn the background stall sampler. Call once from `main` just before
/// `run_app`. A no-op (spawns nothing) only when [`enabled`] is false — and
/// [`enabled`] defaults TRUE, so a shipped binary DOES run this thread; what it
/// pays is one wake per [`sample_interval`], which in a release build is half of
/// [`RELEASE_STALL_THRESHOLD`] = 2.5 s. (This doc said "release binaries pay
/// nothing" while the sampler woke four times a second in every shipped build;
/// the 2026-09-22 efficiency audit measured it as the largest single source of
/// aterm's kernel wakeups at rest.) No self-terminate handshake is needed — the
/// process is exiting when this thread would otherwise notice, and it is a
/// daemon by nature.
pub fn start() {
    if !enabled() {
        return;
    }
    let abort = abort_on_stall();
    let threshold = threshold();
    let builder = std::thread::Builder::new().name("aterm-watchdog".into());
    // A spawn failure is non-fatal: the app runs fine without the tripwire.
    let _ = builder.spawn(move || {
        let sample = sample_interval();
        aterm_log::info!(
            "main-thread stall watchdog armed (sample {sample:?}, threshold \
             {threshold:?}, repeat {STALL_REPEAT_INTERVAL:?}, abort={abort})"
        );
        let mut sampler =
            Sampler::with_threshold(Instant::now(), HEARTBEAT.load(Ordering::Relaxed), threshold);
        loop {
            std::thread::sleep(sample);
            let now = Instant::now();
            let cur = HEARTBEAT.load(Ordering::Relaxed);
            let bc = Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Relaxed));
            if let Some(hit) = sampler.poll(now, cur, bc) {
                let frozen = now.saturating_duration_since(sampler.last_advance);
                // The phase is read AT the report, just after the sample
                // that decided it. If the main thread beat in that instant it
                // reads none or the next root's; either way it was announced
                // in the root the main thread is in now (a beat clears it).
                // A nested phase that ended before the report reads as the
                // phase around it.
                aterm_log::error!(
                    "{}",
                    stall_message(sampler.reports, frozen, hit, current_phase())
                );
                if abort {
                    std::process::abort();
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE SAMPLER'S CADENCE IS DERIVED FROM THE BAR IT SERVES (2026-09-22).
    ///
    /// Two consecutive samples must always straddle the threshold, or a wedge
    /// could sit undetected for longer than the bar promises; and the sampler
    /// must not wake more often than that, because on a fanless laptop at rest
    /// every one of those wakes is charged to deep-idle residency and nothing
    /// else. Half is the one ratio that satisfies both, and it keeps the debug
    /// lane byte-identical at the 250 ms it has always used.
    #[test]
    fn the_sample_interval_is_half_the_stall_threshold() {
        assert_eq!(
            sample_interval(),
            threshold() / 2,
            "the sampler's cadence is derived from the bar, never set beside it"
        );
        assert!(
            sample_interval() * 2 <= threshold(),
            "two samples must straddle the threshold"
        );
        // The two bars this build can have, checked by construction so the
        // arithmetic is pinned even in the build that does not take that arm.
        assert_eq!(STALL_THRESHOLD / 2, Duration::from_millis(250));
        assert_eq!(RELEASE_STALL_THRESHOLD / 2, Duration::from_millis(2_500));
    }

    /// [`BREADCRUMB`] and [`HEARTBEAT`] are process-global and the tests in this
    /// binary run in parallel, so every test that BEATS takes this first. Without
    /// it a beat from a sibling test lands between two of this one's and swaps the
    /// breadcrumb out from under the assertion — a green run under one filter and
    /// a red one under another.
    static BEAT_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn beat_serial() -> std::sync::MutexGuard<'static, ()> {
        BEAT_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The modal park point never reports, however long the dialog stays up:
    /// a user reading a confirm sheet for a minute is not a stall.
    #[test]
    fn a_modal_park_never_reports_however_long_it_lasts() {
        assert!(!is_stall_at(
            Breadcrumb::Modal,
            Duration::from_secs(600),
            STALL_THRESHOLD
        ));
        assert!(!is_stall_at(
            Breadcrumb::Modal,
            Duration::from_secs(600),
            RELEASE_STALL_THRESHOLD
        ));
        // …while the work root it interrupted still does.
        assert!(is_stall_at(
            Breadcrumb::WindowEvent,
            Duration::from_secs(600),
            STALL_THRESHOLD
        ));
    }

    /// `park_modal` round-trips: the outer root's breadcrumb comes back when
    /// the guard drops, and it comes back with a fresh beat.
    #[test]
    fn park_modal_restores_the_outer_root_with_a_fresh_beat() {
        let _serial = beat_serial();
        beat(Breadcrumb::WindowEvent);
        let before = HEARTBEAT.load(Ordering::Relaxed);
        {
            let _park = park_modal();
            assert_eq!(current(), Breadcrumb::Modal);
        }
        assert_eq!(current(), Breadcrumb::WindowEvent);
        assert!(HEARTBEAT.load(Ordering::Relaxed) >= before + 2);
        assert_eq!(Breadcrumb::from_u8(7), Breadcrumb::Modal);
        assert_eq!(Breadcrumb::Modal.name(), "Modal");
    }

    /// A phase is the SECOND word of a stall line: announced by a guard,
    /// nested correctly, restored when the guard drops, and cleared by the
    /// next root's beat — never carried into work it does not describe, not
    /// even by a nested guard the beat outlived.
    ///
    /// The phase cell and the ledger are this test's OWN, on the same grounds
    /// [`beat_into`] already states for the ledger: the default harness is
    /// threaded, and the process-global [`PHASE`] has other writers — every
    /// lib.rs test that arms `deferred_font_seal` or `deferred_gpu` before
    /// `ensure_pixel_backend` HOLDS [`Phase::PixelBackendRedeem`] on it for the
    /// whole of a font seal, and `spawn::park_reader`'s handoff spin CLEARS it
    /// on every turn of a loop that takes no lock at all — so an assertion on
    /// the shared cell would race them both ways. Every claim here is therefore
    /// about this test's own cell; the lock is still taken because [`beat_into`]
    /// writes the real [`BREADCRUMB`] and [`HEARTBEAT`], which locked siblings
    /// do assert on, and the heartbeat is only ever read for ADVANCE (that same
    /// unlocked spin makes any exact count a coin flip).
    #[test]
    fn a_phase_is_scoped_to_its_guard_and_cleared_by_the_next_root() {
        static PH: AtomicU8 = AtomicU8::new(Phase::None as u8);
        const T0: u64 = 11_000_000_000;
        const MS: u64 = 1_000_000;

        let _serial = beat_serial();
        let turns = TurnLedger::new();
        let before = HEARTBEAT.load(Ordering::Relaxed);

        beat_into(&turns, &PH, Breadcrumb::UserEvent, T0);
        assert!(
            HEARTBEAT.load(Ordering::Relaxed) > before,
            "a root entry beats"
        );
        assert_eq!(phase_in(&PH), Phase::None, "a root entry announces nothing");
        {
            let _capture = announce(&PH, Phase::ImageCapture);
            assert_eq!(phase_in(&PH), Phase::ImageCapture);
            {
                let _redeem = announce(&PH, Phase::PixelBackendRedeem);
                assert_eq!(
                    phase_in(&PH),
                    Phase::PixelBackendRedeem,
                    "the inner phase reads while it lasts"
                );
            }
            assert_eq!(
                phase_in(&PH),
                Phase::ImageCapture,
                "the outer phase comes back when the inner one ends"
            );
        }
        assert_eq!(phase_in(&PH), Phase::None);
        let held = announce(&PH, Phase::PixelBackendRedeem);
        beat_into(&turns, &PH, Breadcrumb::AboutToWait, T0 + MS);
        assert_eq!(
            phase_in(&PH),
            Phase::None,
            "the next root's beat clears a phase whose guard is still alive"
        );
        // The stale guard finds its announcement gone and restores nothing.
        // (That it does not BEAT either is structural rather than asserted:
        // `announce` and `PhaseGuard::drop` are handed the phase cell and
        // nothing else, so neither can reach the heartbeat — the property a
        // phase needs, since a phase that beat would hide the very stall it
        // exists to name.)
        drop(held);
        assert_eq!(phase_in(&PH), Phase::None);
        // The wrong-work case the `PHASE` doc rules out: a beat between a
        // NESTED guard's creation and its drop. The inner guard sampled the
        // outer phase, and a plain store on drop would re-announce that outer
        // phase into the new root — work the new root is not doing. The
        // compare-exchange leaves the beat's clean slate alone, and so does
        // the outer guard's drop after it. Unreachable today (neither
        // `render_image` nor `ensure_pixel_backend` beats or parks); pinned
        // so it stays a non-event if either ever does.
        beat_into(&turns, &PH, Breadcrumb::UserEvent, T0 + 2 * MS);
        let outer = announce(&PH, Phase::ImageCapture);
        let inner = announce(&PH, Phase::PixelBackendRedeem);
        beat_into(&turns, &PH, Breadcrumb::AboutToWait, T0 + 3 * MS);
        assert_eq!(phase_in(&PH), Phase::None);
        drop(inner);
        assert_eq!(
            phase_in(&PH),
            Phase::None,
            "a stale inner guard must not re-announce the outer phase"
        );
        drop(outer);
        assert_eq!(phase_in(&PH), Phase::None);
        // …and a fresh announcement in the new root, made while the stale
        // pair is still alive, keeps its own restore chain intact.
        let outer = announce(&PH, Phase::ImageCapture);
        let inner = announce(&PH, Phase::PixelBackendRedeem);
        beat_into(&turns, &PH, Breadcrumb::UserEvent, T0 + 4 * MS);
        let fresh = announce(&PH, Phase::ImageCapture);
        assert_eq!(phase_in(&PH), Phase::ImageCapture);
        drop(fresh);
        assert_eq!(
            phase_in(&PH),
            Phase::None,
            "the fresh guard sampled the beat's None and puts it back"
        );
        drop(inner);
        drop(outer);
        assert_eq!(phase_in(&PH), Phase::None);

        // Leave the global breadcrumb where the rest of the suite expects it.
        beat(Breadcrumb::AboutToWait);
    }

    /// The line a stripped-release log gets, root AND phase — the 2026-09-06
    /// line as it would have read with the phase in it, the bare form when
    /// nothing was announced, and the repeat.
    #[test]
    fn the_stall_line_names_the_root_and_the_announced_phase() {
        for p in [Phase::None, Phase::PixelBackendRedeem, Phase::ImageCapture] {
            assert_eq!(Phase::from_u8(p as u8), p);
        }
        assert_eq!(Phase::from_u8(200), Phase::None, "unknown folds to nothing");
        let line = stall_message(
            1,
            Duration::from_millis(5045),
            Breadcrumb::UserEvent,
            Phase::PixelBackendRedeem,
        );
        assert!(
            line.starts_with(
                "MAIN-THREAD STALL: no heartbeat for 5.045s while inside `UserEvent`, in \
                 the headless pixel-backend redemption"
            ),
            "{line}"
        );
        assert!(
            line.contains("the GPU context with its font thread)"),
            "{line}"
        );
        let bare = stall_message(
            1,
            Duration::from_secs(1),
            Breadcrumb::ResizeSettle,
            Phase::None,
        );
        assert!(
            bare.contains("inside `ResizeSettle` — the UI is not responding"),
            "no phase, no clause: {bare}"
        );
        let again = stall_message(
            2,
            Duration::from_secs(103),
            Breadcrumb::NewEvents,
            Phase::ImageCapture,
        );
        assert!(
            again.starts_with(
                "MAIN-THREAD STALL CONTINUES: still no heartbeat after 103s inside \
                 `NewEvents`, in an `image` capture (headless, the first one also mints \
                 the GPU device"
            ),
            "{again}"
        );
        assert!(
            again.ends_with("compiles its shaders) — this is a wedge, not a slow frame."),
            "the mechanism, and no figure: {again}"
        );
        assert!(!again.contains("up to a second"), "{again}");
    }

    /// LIVE end-to-end proof that the REAL background sampler thread wakes, sees a
    /// frozen heartbeat sitting on the `ResizeSettle` breadcrumb, and emits the
    /// named stall log line — the exact behaviour a stripped-release freeze needs.
    ///
    /// `#[ignore]` because it (a) sleeps ~1 s and (b) installs the process-global
    /// logger (a `OnceLock`), which would collide with the rest of the suite. Run
    /// it in isolation:
    ///
    /// ```text
    /// ATERM_WATCHDOG=1 cargo test -p aterm-gui -- --ignored --exact \
    ///     watchdog::tests::live_sampler_thread_logs_a_named_resize_stall
    /// ```
    #[test]
    #[ignore = "live: sleeps ~1s and installs the global logger; run with --ignored"]
    fn live_sampler_thread_logs_a_named_resize_stall() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        struct Capture {
            fired: Arc<AtomicBool>,
            saw_name: Arc<std::sync::Mutex<String>>,
        }
        impl aterm_log::Log for Capture {
            fn enabled(&self, _m: &aterm_log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, record: &aterm_log::Record<'_>) {
                let line = format!("{}", record.args());
                if line.contains("MAIN-THREAD STALL") {
                    self.fired.store(true, Ordering::SeqCst);
                    *self.saw_name.lock().unwrap() = line;
                }
            }
            fn flush(&self) {}
        }

        let fired = Arc::new(AtomicBool::new(false));
        let saw = Arc::new(std::sync::Mutex::new(String::new()));
        // Leak the logger to obtain the `&'static` `set_logger` requires.
        let cap: &'static Capture = Box::leak(Box::new(Capture {
            fired: fired.clone(),
            saw_name: saw.clone(),
        }));
        let _ = aterm_log::set_logger(cap);
        aterm_log::set_max_level(aterm_log::LevelFilter::Trace);

        // Ensure the sampler is enabled regardless of debug/release. Routed
        // through the workspace's one lock-scoped env helper.
        aterm_log::env::set("ATERM_WATCHDOG", "1");

        // Enter the resize-settle arm, then STOP beating (simulate the wedge).
        beat(Breadcrumb::ResizeSettle);
        start();

        // Give the sampler (250 ms tick, 500 ms threshold) time to trip.
        std::thread::sleep(Duration::from_millis(1100));

        assert!(
            fired.load(Ordering::SeqCst),
            "watchdog should have logged a stall for the frozen ResizeSettle beat"
        );
        assert!(
            saw.lock().unwrap().contains("ResizeSettle"),
            "the stall line must NAME the breadcrumb; got: {}",
            saw.lock().unwrap()
        );
    }

    /// THE ATTRIBUTION RULE (2026-09-15 responsiveness audit). A turn that ended
    /// inside a WORK root is the span this census exists to name — the 100–600 ms
    /// band that is too short for the release sampler's 5 s bar and never enters
    /// the redraw timer, so before this it left no attributable trace while still
    /// being charged to `present_latency` and `input_present`.
    ///
    /// A PARK POINT's span is never booked, however long it lasts: idling in the
    /// OS event wait, a modal dialog the user is reading, and the update-handoff
    /// wait are designed freezes, and pricing them would be exactly the noise the
    /// sampler's park-point exemption exists to avoid.
    #[test]
    fn a_work_root_turn_is_attributable_and_a_designed_park_is_never_booked() {
        const OPEN: u64 = 1_000_000;
        const PARK_NS: u64 = 600 * 1_000_000_000; // ten minutes
        for work in [
            Breadcrumb::WindowEvent,
            Breadcrumb::UserEvent,
            Breadcrumb::NewEvents,
            Breadcrumb::ResizeSettle,
        ] {
            assert_eq!(
                TurnLedger::attributable_span(work, OPEN, OPEN + 300_000_000),
                Some(300_000_000),
                "a 300 ms park in `{}` is the exact reading this census exists to \
                 name: the release sampler ignores it and the redraw timer never \
                 sees it",
                work.metric_name()
            );
        }
        for park in [
            Breadcrumb::Startup,
            Breadcrumb::AboutToWait,
            Breadcrumb::UpdateHandoff,
            Breadcrumb::Modal,
        ] {
            assert_eq!(
                TurnLedger::attributable_span(park, OPEN, OPEN + PARK_NS),
                None,
                "`{}` is a DESIGNED freeze — booking it would make every reading \
                 on this line meaningless",
                park.metric_name()
            );
        }
        // A disarmed stamp — process start, or the turn straddling a reset —
        // books nothing: that span began outside the window it would be
        // charged to.
        assert_eq!(
            TurnLedger::attributable_span(Breadcrumb::UserEvent, 0, OPEN + PARK_NS),
            None,
            "a turn that began before this window is not this window's to price"
        );
    }

    /// THE READING THAT HAD NO PRODUCER. A `Wake::Output` turn parks 312 ms in
    /// the bookkeeping that runs ahead of the redraw fan-out; `present_latency`
    /// and `input_present` both book it, `redraw_total` cannot see it, and the
    /// 5 s release sampler never fires. This is the census that names it, with
    /// the counts that say whether it was one hitch or a habit.
    ///
    /// Driven against a LOCAL ledger on a synthetic clock: the process-global one
    /// is written by every other test in this binary and cleared by
    /// `metrics::reset`, so asserting against it would be schedule-dependent —
    /// exactly the flake the key-queue split was rewritten to avoid.
    #[test]
    fn the_turn_census_names_the_worst_root_and_counts_the_long_turns() {
        const T0: u64 = 5_000_000_000;
        const LONG: u64 = 312_000_000;
        const SHORT: u64 = 2_000_000;
        let ledger = TurnLedger::new();

        // The first beat only OPENS a turn; there is nothing to close yet.
        assert_eq!(ledger.close(Breadcrumb::AboutToWait, T0), None);
        let c = ledger.snapshot();
        assert_eq!(c.turns, 0);
        assert_eq!(
            c.max_owner, None,
            "an empty census must name no owner at all: `startup` would be a \
             claim about a root that never ran"
        );

        // …then the Output arm parks for 312 ms and the next root closes it.
        assert_eq!(ledger.close(Breadcrumb::UserEvent, T0 + LONG), Some(LONG));
        let c = ledger.snapshot();
        assert_eq!(c.max_ns, LONG);
        assert_eq!(
            c.max_owner,
            Some(Breadcrumb::UserEvent),
            "a max that cannot name its root sends the next reader to the GPU"
        );
        assert_eq!(c.max_at_ns, T0 + LONG, "the worst turn says WHEN it ended");
        assert_eq!(c.last_ns, LONG);
        assert_eq!((c.turns, c.long_turns), (1, 1));

        // A short turn moves `last` and the count, never the max or its owner.
        assert_eq!(
            ledger.close(Breadcrumb::WindowEvent, T0 + LONG + SHORT),
            Some(SHORT)
        );
        let c = ledger.snapshot();
        assert_eq!((c.max_ns, c.last_ns), (LONG, SHORT));
        assert_eq!(
            c.max_owner,
            Some(Breadcrumb::UserEvent),
            "a shorter turn must never steal the max's owner label"
        );
        assert_eq!(
            (c.turns, c.long_turns),
            (2, 1),
            "the count is what separates one hitch from a main thread that is \
             late all the time"
        );

        // Ten minutes idle at the park point books nothing at all.
        assert_eq!(
            ledger.close(Breadcrumb::AboutToWait, T0 + LONG + SHORT + 600_000_000_000),
            None
        );
        assert_eq!(ledger.snapshot().turns, 2);

        // A reset clears the window AND disarms the open stamp, so the turn
        // straddling it is not charged to the fresh window.
        ledger.reset();
        let c = ledger.snapshot();
        assert_eq!((c.max_ns, c.last_ns, c.turns, c.long_turns), (0, 0, 0, 0));
        assert_eq!(c.max_owner, None);
        assert_eq!(
            ledger.close(Breadcrumb::UserEvent, T0 + 900_000_000_000),
            None,
            "the turn open across a `metrics reset` began outside the new window"
        );
    }

    /// THE CENSUS IS ON THE HOT PATH, not beside it. The rule above is only
    /// worth anything if the winit roots actually feed it: `beat` is what every
    /// root calls, so this drives the real stamp-close-bump sequence and pins
    /// that the turn it closes is booked to the root that OWNED it — the
    /// `user_event` park, not the `about_to_wait` that ended it.
    ///
    /// The ledger and the phase cell are local and the beat path is serialized,
    /// so nothing here depends on test order. What is left uncovered is the
    /// one-line binding of `beat` to the global ledger, the global phase cell
    /// and the process clock.
    #[test]
    fn a_beat_books_the_turn_it_closes_to_the_root_that_owned_it() {
        let _serial = beat_serial();
        const T0: u64 = 7_000_000_000;
        const PARKED: u64 = 250_000_000;
        let turns = TurnLedger::new();
        let phase = AtomicU8::new(Phase::None as u8);

        // Enter the Output arm's root; nothing is closed yet.
        assert_eq!(beat_into(&turns, &phase, Breadcrumb::UserEvent, T0), None);
        assert_eq!(current(), Breadcrumb::UserEvent);

        // 250 ms of bookkeeping later the loop reaches its park point, and the
        // beat that gets there prices what just happened.
        assert_eq!(
            beat_into(&turns, &phase, Breadcrumb::AboutToWait, T0 + PARKED),
            Some(PARKED),
            "the beat that ends a 250 ms `user_event` turn must book it"
        );
        let c = turns.snapshot();
        assert_eq!(c.max_ns, PARKED);
        assert_eq!(
            c.max_owner,
            Some(Breadcrumb::UserEvent),
            "the turn belongs to the root that HELD the thread, not to the one \
             that ended it"
        );
        assert_eq!((c.turns, c.long_turns), (1, 1));

        // The idle park that follows books nothing, however long the user is away.
        assert_eq!(
            beat_into(
                &turns,
                &phase,
                Breadcrumb::NewEvents,
                T0 + PARKED + 600_000_000_000
            ),
            None
        );
        assert_eq!(turns.snapshot().turns, 1);

        // Leave the global breadcrumb where the rest of the suite expects it.
        beat(Breadcrumb::AboutToWait);
    }

    /// The long-turn bar is ONE FRAME, and it is the same frame budget the rest
    /// of the `metrics` line already uses — so `long_turns` and `slow_frames`
    /// cannot mean two different things by "late".
    #[test]
    fn the_long_turn_bar_is_one_frame_budget() {
        assert_eq!(
            LONG_TURN_THRESHOLD_NS,
            crate::metrics::SLOW_FRAME_THRESHOLD_NS
        );
        let ledger = TurnLedger::new();
        assert_eq!(ledger.close(Breadcrumb::AboutToWait, 1), None);
        assert_eq!(
            ledger.close(Breadcrumb::UserEvent, 1 + LONG_TURN_THRESHOLD_NS - 1),
            Some(LONG_TURN_THRESHOLD_NS - 1)
        );
        assert_eq!(
            ledger.snapshot().long_turns,
            0,
            "a turn that still fits in a frame is not a long turn"
        );
        assert_eq!(
            ledger.close(
                Breadcrumb::UserEvent,
                1 + LONG_TURN_THRESHOLD_NS - 1 + LONG_TURN_THRESHOLD_NS
            ),
            Some(LONG_TURN_THRESHOLD_NS)
        );
        assert_eq!(ledger.snapshot().long_turns, 1, "a whole frame late counts");
    }

    #[test]
    fn breadcrumb_round_trips_through_u8() {
        for bc in [
            Breadcrumb::Startup,
            Breadcrumb::AboutToWait,
            Breadcrumb::WindowEvent,
            Breadcrumb::UserEvent,
            Breadcrumb::NewEvents,
            Breadcrumb::ResizeSettle,
        ] {
            assert_eq!(Breadcrumb::from_u8(bc as u8), bc);
            assert!(!bc.name().is_empty());
        }
    }

    #[test]
    fn unknown_u8_folds_to_the_benign_park_point() {
        assert_eq!(Breadcrumb::from_u8(200), Breadcrumb::Startup);
        assert!(Breadcrumb::from_u8(200).is_park_point());
    }

    #[test]
    fn only_idle_and_startup_are_park_points() {
        assert!(Breadcrumb::Startup.is_park_point());
        assert!(Breadcrumb::AboutToWait.is_park_point());
        assert!(!Breadcrumb::WindowEvent.is_park_point());
        assert!(!Breadcrumb::UserEvent.is_park_point());
        assert!(!Breadcrumb::NewEvents.is_park_point());
        assert!(!Breadcrumb::ResizeSettle.is_park_point());
    }

    #[test]
    fn flags_a_wedged_resize_settle_but_not_a_slow_progressing_frame() {
        // The hazard: stuck in the ResizeSettle reflow past the threshold → FLAG.
        assert!(is_stall_at(
            Breadcrumb::ResizeSettle,
            STALL_THRESHOLD,
            STALL_THRESHOLD
        ));
        assert!(is_stall_at(
            Breadcrumb::ResizeSettle,
            STALL_THRESHOLD + Duration::from_secs(42),
            STALL_THRESHOLD
        ));
        assert!(is_stall_at(
            Breadcrumb::WindowEvent,
            STALL_THRESHOLD,
            STALL_THRESHOLD
        ));
        // A sub-threshold freeze (one slow frame) is NOT a stall.
        assert!(!is_stall_at(
            Breadcrumb::ResizeSettle,
            STALL_THRESHOLD - Duration::from_millis(1),
            STALL_THRESHOLD
        ));
        // Idle at a park point, even for a long time, is NEVER a stall.
        assert!(!is_stall_at(
            Breadcrumb::AboutToWait,
            STALL_THRESHOLD + Duration::from_secs(600),
            STALL_THRESHOLD
        ));
        assert!(!is_stall_at(
            Breadcrumb::Startup,
            STALL_THRESHOLD + Duration::from_secs(600),
            STALL_THRESHOLD
        ));
    }

    #[test]
    fn sampler_fires_once_on_a_wedged_resize_and_rearms_after_recovery() {
        // Synthetic clock: the exact archetype — the main thread beats into the
        // ResizeSettle reflow arm, then the heartbeat FREEZES (a 42 s wedge under
        // the term lock). The breadcrumb never advances to AboutToWait.
        let t0 = Instant::now();
        let beat_val = 100; // whatever `beat` left in HEARTBEAT before the wedge
        let mut s = Sampler::with_threshold(t0, beat_val, STALL_THRESHOLD);

        // 250 ms in, still frozen but under threshold → no fire yet.
        assert_eq!(
            s.poll(
                t0 + Duration::from_millis(250),
                beat_val,
                Breadcrumb::ResizeSettle
            ),
            None
        );
        // 600 ms in, past the 500 ms threshold → FLAG, naming ResizeSettle.
        assert_eq!(
            s.poll(
                t0 + Duration::from_millis(600),
                beat_val,
                Breadcrumb::ResizeSettle
            ),
            Some(Breadcrumb::ResizeSettle)
        );
        // Still wedged at 42 s → does NOT spam; one report per contiguous stall.
        assert_eq!(
            s.poll(
                t0 + Duration::from_secs(42),
                beat_val,
                Breadcrumb::ResizeSettle
            ),
            None
        );
        // Main thread recovers (heartbeat advances) then wedges AGAIN → re-arms
        // and fires a second time. Proves the guard keeps standing.
        assert_eq!(
            s.poll(
                t0 + Duration::from_secs(43),
                beat_val + 1,
                Breadcrumb::AboutToWait
            ),
            None
        );
        assert_eq!(
            s.poll(
                t0 + Duration::from_secs(44),
                beat_val + 1,
                Breadcrumb::ResizeSettle
            ),
            Some(Breadcrumb::ResizeSettle)
        );
    }

    #[test]
    fn sampler_never_fires_while_idle_at_a_park_point() {
        // The heartbeat is frozen for ten minutes because the app is IDLE (parked
        // in the OS event wait after about_to_wait). This must never be a stall.
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 7, STALL_THRESHOLD);
        for secs in [1u64, 5, 60, 600] {
            assert_eq!(
                s.poll(t0 + Duration::from_secs(secs), 7, Breadcrumb::AboutToWait),
                None,
                "idle park at {secs}s must not fire"
            );
        }
    }

    #[test]
    fn beat_advances_the_heartbeat_and_stamps_the_breadcrumb() {
        let _serial = beat_serial();
        let before = HEARTBEAT.load(Ordering::Relaxed);
        beat(Breadcrumb::ResizeSettle);
        assert!(HEARTBEAT.load(Ordering::Relaxed) > before);
        assert_eq!(
            Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Relaxed)),
            Breadcrumb::ResizeSettle
        );
    }

    /// REGRESSION (2026-08-30, v0.65.0): the shipped binary must ARM this. The
    /// watchdog was `cfg!(debug_assertions) || $ATERM_WATCHDOG`, so the release
    /// that parked its main thread inside `user_event` for five hours spawned no
    /// sampler and logged nothing — and the only evidence left was a macOS hang
    /// report that had to be hand-symbolicated against a stripped binary.
    #[test]
    fn the_watchdog_is_armed_unless_explicitly_switched_off() {
        // `enabled()` reads the environment, which is process-global and shared
        // with every other test in this binary — so assert the DECISION, not by
        // mutating the env. With nothing set, it must be on.
        if std::env::var_os("ATERM_WATCHDOG").is_none() {
            assert!(
                enabled(),
                "a shipped build must arm the stall watchdog: silence is what \
                 cost five hours on 2026-08-30"
            );
        }
        // And the shipped bar is coarse enough that a slow frame is never a line,
        // while a permanent park still is.
        assert!(
            RELEASE_STALL_THRESHOLD > STALL_THRESHOLD,
            "the release bar must be the coarser of the two"
        );
        assert!(
            RELEASE_STALL_THRESHOLD < Duration::from_secs(30),
            "a bar this coarse stops being a freeze guard"
        );
    }

    /// The shipped lane judges against [`RELEASE_STALL_THRESHOLD`]: a 1 s frozen
    /// frame is NOT a line (that is a slow frame), a 6 s one is (that is a wedge).
    #[test]
    fn the_release_lane_ignores_a_slow_frame_and_reports_a_wedge() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 1, RELEASE_STALL_THRESHOLD);
        assert!(
            s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::WindowEvent)
                .is_none(),
            "one second of main-thread work must not write an error line"
        );
        assert_eq!(
            s.poll(t0 + Duration::from_secs(6), 1, Breadcrumb::WindowEvent),
            Some(Breadcrumb::WindowEvent),
            "six seconds frozen at a WORK root is a wedge and must be named"
        );
    }

    /// A stall that never ends is re-reported on a cadence. One line at the
    /// start proves a wedge happened; the repeats prove it never ended — the
    /// distinction the 0.65.0 log could not make, because it said nothing.
    #[test]
    fn a_persisting_stall_is_reported_again_on_the_repeat_cadence() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 1, STALL_THRESHOLD);
        assert_eq!(
            s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::UserEvent),
            Some(Breadcrumb::UserEvent),
            "the first crossing must report"
        );
        assert!(
            s.poll(t0 + Duration::from_secs(30), 1, Breadcrumb::UserEvent)
                .is_none(),
            "inside the repeat interval it stays quiet — no flood"
        );
        assert_eq!(
            s.poll(
                t0 + Duration::from_secs(1) + STALL_REPEAT_INTERVAL,
                1,
                Breadcrumb::UserEvent
            ),
            Some(Breadcrumb::UserEvent),
            "a stall still live one interval later must say so again"
        );
        // …and progress re-arms it completely.
        assert!(
            s.poll(t0 + Duration::from_secs(200), 2, Breadcrumb::UserEvent)
                .is_none(),
            "a heartbeat means the main thread is back"
        );
    }

    /// The 0.65.0 breadcrumb, exactly: the automatic apply arrives as
    /// `Wake::ApplyStagedUpdate` inside `user_event`, which is a WORK root, and
    /// the park happens before `UpdateHandoff` is ever stamped. So the guard
    /// covers the state the process was actually in — this is the assertion that
    /// makes "it would have fired" a fact rather than a claim.
    #[test]
    fn the_v065_park_state_is_one_this_watchdog_reports() {
        assert!(
            !Breadcrumb::UserEvent.is_park_point(),
            "user_event is WORK: a frozen heartbeat there is a stall"
        );
        assert!(
            is_stall_at(
                Breadcrumb::UserEvent,
                Duration::from_secs(3588),
                RELEASE_STALL_THRESHOLD
            ),
            "the field hang (3588 s unresponsive, parked in user_event) must be \
             a reported stall in a SHIPPED build"
        );
        // The handoff's own park point stays exempt — a quiesced reader wait is
        // not a wedge, and reporting it would be the noise that gets a guard
        // ignored.
        assert!(Breadcrumb::UpdateHandoff.is_park_point());
    }

    /// TWO SEPARATE WEDGES READ AS TWO. The report counter used to live in the
    /// sampler LOOP rather than in the sampler, so it never re-armed: a stall
    /// hours after the first announced itself as "STALL CONTINUES", and two
    /// distinct incidents read as one. Found by `codex review`.
    #[test]
    fn a_second_stall_after_recovery_reports_as_a_first_again() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 1, STALL_THRESHOLD);
        assert_eq!(
            s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::UserEvent),
            Some(Breadcrumb::UserEvent)
        );
        assert_eq!(s.reports, 1, "the first line of the first stall");
        // The main thread comes back…
        assert!(
            s.poll(t0 + Duration::from_secs(2), 2, Breadcrumb::UserEvent)
                .is_none()
        );
        assert_eq!(s.reports, 0, "recovery re-arms the report count");
        // …and wedges again, hours later. That is a NEW incident.
        assert_eq!(
            s.poll(t0 + Duration::from_secs(9000), 2, Breadcrumb::WindowEvent),
            Some(Breadcrumb::WindowEvent)
        );
        assert_eq!(
            s.reports, 1,
            "a separate wedge must announce itself in full, not as a continuation"
        );
    }
}
