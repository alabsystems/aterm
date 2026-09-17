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
//! thread (started by [`start`]) wakes every [`SAMPLE_INTERVAL`]; if the
//! heartbeat has not advanced for longer than [`STALL_THRESHOLD`] *and* the last
//! breadcrumb is a WORK root (not the idle park point), the main thread is wedged
//! inside bounded event handling — it logs (and optionally aborts) with the last
//! breadcrumb NAME.
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

/// How often the sampler thread wakes to inspect the heartbeat.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// How long the heartbeat may stay frozen at a WORK breadcrumb before it counts
/// as a stall, in a DEBUG build or under an explicit `$ATERM_WATCHDOG`. Two
/// sample intervals, so a genuine wedge is caught within ~750 ms while a single
/// slow-but-progressing frame never trips.
const STALL_THRESHOLD: Duration = Duration::from_millis(500);

/// The same bar for a SHIPPED release binary, where the reader is a user's
/// `aterm.log` rather than a developer's terminal.
///
/// Ten sample intervals. The trade is deliberate and one-directional: 500 ms of
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
/// fresh count against a stale location.
#[inline]
pub fn beat(bc: Breadcrumb) {
    beat_at(bc, crate::metrics::now_ns());
}

/// [`beat`] with the clock passed IN, returning the span it booked — a real clock
/// would make any assertion about a span a race.
#[inline]
fn beat_at(bc: Breadcrumb, now_ns: u64) -> Option<u64> {
    beat_into(&TURNS, bc, now_ns)
}

/// The beat path with its LEDGER passed in too, on the [`Sampler`] precedent:
/// factored out so the stamp-close-bump sequence is testable against a ledger no
/// other test in this binary can write, and no `metrics reset` can clear
/// mid-assertion.
#[inline]
fn beat_into(turns: &TurnLedger, bc: Breadcrumb, now_ns: u64) -> Option<u64> {
    let previous = Breadcrumb::from_u8(BREADCRUMB.swap(bc as u8, Ordering::Relaxed));
    let booked = turns.close(previous, now_ns);
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
/// `run_app`. A no-op (spawns nothing) when [`enabled`] is false, so release
/// binaries pay nothing. No self-terminate handshake is needed — the process is
/// exiting when this thread would otherwise notice, and it is a daemon by nature.
pub fn start() {
    if !enabled() {
        return;
    }
    let abort = abort_on_stall();
    let threshold = threshold();
    let builder = std::thread::Builder::new().name("aterm-watchdog".into());
    // A spawn failure is non-fatal: the app runs fine without the tripwire.
    let _ = builder.spawn(move || {
        aterm_log::info!(
            "main-thread stall watchdog armed (sample {SAMPLE_INTERVAL:?}, threshold \
             {threshold:?}, repeat {STALL_REPEAT_INTERVAL:?}, abort={abort})"
        );
        let mut sampler =
            Sampler::with_threshold(Instant::now(), HEARTBEAT.load(Ordering::Relaxed), threshold);
        loop {
            std::thread::sleep(SAMPLE_INTERVAL);
            let now = Instant::now();
            let cur = HEARTBEAT.load(Ordering::Relaxed);
            let bc = Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Relaxed));
            if let Some(hit) = sampler.poll(now, cur, bc) {
                let frozen = now.saturating_duration_since(sampler.last_advance);
                if sampler.reports == 1 {
                    aterm_log::error!(
                        "MAIN-THREAD STALL: no heartbeat for {frozen:?} while inside \
                         `{}` — the UI is not responding. Either unbounded work under a \
                         contended lock (the L0 freeze hazard) or a park that will never \
                         end (a lock or lazy-init cycle). This line names the main-loop \
                         root without symbols; a hang report is not required to find it.",
                        hit.name()
                    );
                } else {
                    aterm_log::error!(
                        "MAIN-THREAD STALL CONTINUES: still no heartbeat after {frozen:?} \
                         inside `{}` — this is a wedge, not a slow frame.",
                        hit.name()
                    );
                }
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
    /// The ledger is local and the beat path is serialized, so nothing here
    /// depends on test order. What is left uncovered is the one-line binding of
    /// `beat` to the global ledger and the process clock.
    #[test]
    fn a_beat_books_the_turn_it_closes_to_the_root_that_owned_it() {
        let _serial = beat_serial();
        const T0: u64 = 7_000_000_000;
        const PARKED: u64 = 250_000_000;
        let turns = TurnLedger::new();

        // Enter the Output arm's root; nothing is closed yet.
        assert_eq!(beat_into(&turns, Breadcrumb::UserEvent, T0), None);
        assert_eq!(current(), Breadcrumb::UserEvent);

        // 250 ms of bookkeeping later the loop reaches its park point, and the
        // beat that gets there prices what just happened.
        assert_eq!(
            beat_into(&turns, Breadcrumb::AboutToWait, T0 + PARKED),
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
            beat_into(&turns, Breadcrumb::NewEvents, T0 + PARKED + 600_000_000_000),
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
