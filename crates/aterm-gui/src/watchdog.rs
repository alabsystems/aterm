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
//! * debug builds (and a `dev-seams` build with `$ATERM_WATCHDOG` set) —
//!   [`STALL_THRESHOLD`] (500 ms). Tight, for catching a regression while
//!   developing it.
//! * a shipped release binary — [`RELEASE_STALL_THRESHOLD`] (5 s). A main
//!   thread frozen at a WORK root for five seconds is never normal, so the
//!   coarser bar keeps a slow-but-progressing frame, a huge paste or a cold
//!   font scan from ever writing an alarming line, while still turning a
//!   PERMANENT wedge into a named log line within seconds instead of never.
//!
//! `$ATERM_WATCHDOG` is a DEVELOPMENT seam ([`aterm_types::dev_seam!`]): in a
//! build that compiles seams, `=off` disables the sampler and `=abort`
//! `process::abort()`s on a detected stall (CI / repro). A shipped binary reads
//! none of it — the guard is on, at the coarse bar, and it logs; nothing in the
//! environment can switch the user's freeze guard off. Everything else logs at
//! error level and keeps going. [`beat`] is one monotonic clock read and
//! a handful of atomic writes (all relaxed but one release) in every build
//! either way (negligible on the hot event path, and the clock read is what
//! buys the turn census below), and the sampler is one thread asleep 99.99% of
//! the time.
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
//!
//! ## Where the thread IS, not which root ran last (2026-09-26)
//!
//! 0.93.0 froze after the process had been stopped for 26.6 hours: on resume,
//! CoreFoundation walked winit's 0.1 µs waker timer forward one interval at a
//! time (`aterm_objc::wake_timer` has the mechanism and its fix). The sampler
//! caught it within five seconds — and wrote `while inside \`NewEvents\``,
//! because that was the last root entered. `new_events` had returned; no aterm
//! frame was on the stack. Three things close that gap:
//!
//! * **Locus.** Every root is entered through [`enter`], whose guard marks it
//!   RETURNED when the handler ends, so a stall after the handler reads
//!   "since \`NewEvents\` returned — outside every aterm handler".
//! * **CPU.** Each sample reads the main thread's CPU time
//!   ([`crate::main_thread_probe::cpu_time`]). A thread parked in the OS event
//!   wait uses none, so a frozen heartbeat on a thread that is ON-CPU for a
//!   whole threshold is a spin even at the idle park point `AboutToWait`,
//!   which the heartbeat rule alone must exempt ([`Breadcrumb::spin_is_a_stall`]).
//! * **Stack.** Each report is followed by the main thread's stack
//!   ([`crate::main_thread_probe::stack`]): `image + offset` frames with the
//!   executable's load address and UUID, the same facts a hang report gives,
//!   in `aterm.log` of a stripped release — in as many numbered lines as keep
//!   every frame ([`stack_lines`]).
//!
//! And the trigger itself gets a line: a sampler wake more than
//! [`PROCESS_GAP`] late on a clock that stops while the Mac sleeps means the
//! whole process was not running ([`process_gap`]); that is logged and the
//! sampler starts over, so the gap is never charged to the main thread.
//!
//! ## A gap inside the report pass (2026-09-28)
//!
//! On 2026-09-28 (0.98.0) the whole process went unscheduled for 3 h 22 m on a
//! Mac at load 45-151 with its swap full. App Nap is the likely cause (the thaw
//! came 5 ms after the window server took an App Nap `AppDrawing` assertion on
//! the process), CPU starvation the other one; neither is proven. The sampler
//! had just reported a stall — the main thread was in AppKit's
//! `-[NSSceneStatusItem _setupScene:]`, waiting on a synchronous FrontBoard
//! scene activation — and its log went wrong three ways:
//!
//! * the stack was ONE record, and the logger's 1 KiB cap on an ERROR body
//!   ([`aterm_log::MAX_ALERT_RECORD_BYTES`]) cut it at frame #11;
//! * only the sampler's SLEEP was timed, and no gap line was written, so the
//!   gap fell in the rest of the loop: the report pass that writes the stall
//!   line, captures the stack and writes it;
//! * so the `STALL ENDED` line at the thaw charged all 12138 s to the main
//!   thread.
//!
//! So the stack is logged in parts, each under the cap and each stamped with
//! when it was captured ([`stack_lines`]); every wake is timed from the wake
//! before it ([`Clocks`]), so a gap anywhere in the loop — the report pass,
//! the stack capture included — is seen, logged and resynced; and the gap is
//! read on both clocks ([`classify_gap`]), so its line says whether this
//! process was not scheduled or the Mac slept.
//!
//! ## A verb the main thread cannot take
//!
//! In the same incident the main thread was stuck in AppKit before the whole
//! process stopped running. A control verb that needs the main thread could
//! only post its hop and wait out the whole reply deadline (30 s, holding a
//! worker lane) for a thread that could not answer, though the heartbeat
//! already showed it stuck. [`main_stall`] reads the time [`beat_into`]
//! stamps, the root, and how long a hop has waited for the main thread to
//! take it, at the shipped bar in every build. While it finds the thread
//! stalled, such a verb is refused before anything is posted, with
//! `ERR main thread stalled <N>s since <root>; retry` (`control_media`).

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// as a stall, in a DEBUG build or under the `$ATERM_WATCHDOG` seam. The
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

/// Whether the root named by [`BREADCRUMB`] has RETURNED — its handler is off
/// the stack and the main thread is back in AppKit / CoreFoundation, between
/// aterm's handlers. [`beat`] clears it (a root was just entered); the guard
/// [`enter`] hands out sets it when the handler returns. The 2026-09-26 line
/// said "while inside `NewEvents`" for a thread whose `new_events` had long
/// returned and that was spinning in CoreFoundation's timer catch-up; this is
/// the bit that tells those apart.
static RETURNED: AtomicBool = AtomicBool::new(false);

/// The main-loop roots the watchdog can pin a stall to. `#[repr(u8)]` so it round
/// trips through the [`BREADCRUMB`] atomic with no allocation and no symbols — the
/// NAME survives into a stripped-release log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Breadcrumb {
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
    pub(crate) fn from_u8(v: u8) -> Self {
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
    pub(crate) fn metric_name(self) -> &'static str {
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
    pub(crate) fn name(self) -> &'static str {
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

    /// Whether a main thread that is ON-CPU with a frozen heartbeat at this
    /// root is a stall. True everywhere except the designed freezes that do
    /// real work — startup (GPU, shaders, fonts), a modal dialog's AppKit
    /// loop and the update handoff — and in particular true at the idle park,
    /// [`Breadcrumb::AboutToWait`]: a thread parked in the OS event wait uses
    /// no CPU, so one that burns it there is spinning somewhere aterm's
    /// heartbeat cannot see (the 2026-09-26 timer catch-up could have landed
    /// there as easily as after `NewEvents`).
    fn spin_is_a_stall(self) -> bool {
        !matches!(
            self,
            Breadcrumb::Startup | Breadcrumb::UpdateHandoff | Breadcrumb::Modal
        )
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

/// The bar a main-thread control verb is refused at ([`main_stall`]): the
/// shipped stall bar, in EVERY build. Not [`threshold`]: a debug build's
/// 500 ms is a developer's tripwire, and turning verbs away at it would fail a
/// slow-but-live debug turn (a cold first raster) that the reply deadline was
/// written to wait for. No setting and no environment switch changes it.
const MAIN_STALL_BAR: Duration = RELEASE_STALL_THRESHOLD;

/// A main thread that cannot take a verb now, as [`main_stall`] reads it. Its
/// `Display` is the reason a refused verb answers with:
/// ``main thread stalled <N>s since `<root>`; retry``, with ` returned` after
/// the root when its handler had returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MainStall {
    /// How long the main thread has gone without a beat while it had work:
    /// since its last beat at a work root, or, past its idle park, since the
    /// oldest waiting hop was posted when that is later than the beat.
    pub(crate) stalled: Duration,
    /// The root the heartbeat last named.
    pub(crate) root: Breadcrumb,
    /// That root's handler had returned: the thread is in AppKit or
    /// CoreFoundation, outside every aterm handler (the 2026-09-28 incident's
    /// `-[NSSceneStatusItem _setupScene:]`, after `NewEvents` returned).
    pub(crate) returned: bool,
}

impl std::fmt::Display for MainStall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let returned = if self.returned { " returned" } else { "" };
        write!(
            f,
            "main thread stalled {}s since `{}`{returned}; retry",
            self.stalled.as_secs(),
            self.root.name()
        )
    }
}

/// Whether a verb that needs the main thread should be refused NOW rather than
/// posted, and why: the READER half of the heartbeat contract whose writer is
/// [`beat_into`] (the derived `MainThreadStallRefusal` machine states both
/// halves). Pure, so the rule is testable with no thread and no clock.
///
/// `last_beat_ns` is the heartbeat's timestamp (0: no beat yet, so no
/// evidence and no refusal), `root`/`returned` are where the thread last was,
/// and `hop_since_ns` is when the oldest run of main-thread hops the main
/// thread has not yet taken began (`None` when none is). A hop it has taken
/// and whose reply it queued (`settings set`, answered by the config worker)
/// waits on that worker, not on this thread, and is not in the run
/// (`control_media::Hops`). Two ways to be stalled, at [`MAIN_STALL_BAR`]:
///
/// * the thread sits at a WORK root with no beat for the bar — the sampler's
///   own verdict ([`is_stall_at`]), whether its handler is still on the stack
///   or has returned into AppKit;
/// * or it is past a park point that is not a designed freeze (the idle park,
///   `AboutToWait`) and a hop it has not taken has waited the bar with no beat
///   since it was posted: the thread left its event wait for something that is
///   not aterm's and never came back. The heartbeat alone cannot say this,
///   because an idle thread's heartbeat is old too. Judged on the heartbeat
///   alone, the first verb after ten idle minutes would make every verb that
///   arrives while it is being answered look stalled, so the hop's own wait
///   is the other half of the test.
///
/// A dialog, the update handoff and startup are designed freezes and never
/// read as stalled here: the dialog has its own refusal, and the other two end
/// by themselves.
fn main_stall(
    now_ns: u64,
    last_beat_ns: u64,
    root: Breadcrumb,
    returned: bool,
    hop_since_ns: Option<u64>,
) -> Option<MainStall> {
    if last_beat_ns == 0 {
        return None;
    }
    let quiet = Duration::from_nanos(now_ns.saturating_sub(last_beat_ns));
    let stalled = if is_stall_at(root, quiet, MAIN_STALL_BAR) {
        quiet
    } else {
        // Every root but the designed freezes: the set a spin is judged at.
        let since = hop_since_ns.filter(|_| root.spin_is_a_stall())?;
        let waited = Duration::from_nanos(now_ns.saturating_sub(since.max(last_beat_ns)));
        if waited < MAIN_STALL_BAR {
            return None;
        }
        waited
    };
    Some(MainStall {
        stalled,
        root,
        returned,
    })
}

/// [`main_stall`] now, for a control worker about to post a main-thread hop:
/// the heartbeat as [`beat`] last stamped it and `hop_since_ns` from the
/// caller's own hop count (`control_media`).
///
/// The root is read FIRST and with `Acquire`: [`beat_into`] stamps the time
/// before it publishes the root (`Release`), so the time read after a root is
/// never older than that root's beat. The other order could pair the root a
/// beat just entered with the stamp of the long idle before it, and refuse a
/// verb on a thread that had just woken.
pub(crate) fn main_stall_now(hop_since_ns: Option<u64>) -> Option<MainStall> {
    let root = Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Acquire));
    let last_beat_ns = TURNS.last_beat_ns.load(Ordering::Relaxed);
    let returned = RETURNED.load(Ordering::Relaxed);
    main_stall(
        crate::metrics::now_ns(),
        last_beat_ns,
        root,
        returned,
        hop_since_ns,
    )
}

/// A stall the sampler decided to report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hit {
    /// The root the heartbeat last named.
    root: Breadcrumb,
    /// How long the stall has lasted: since the heartbeat last advanced, or,
    /// for a spin at a park point, since the main thread went on-CPU.
    frozen: Duration,
    /// The main thread was ON-CPU across the samples leading here — a spin,
    /// not a park on a lock.
    busy: bool,
}

/// The share of a sample interval the main thread must spend on-CPU for the
/// interval to count as busy. A parked thread uses ~0; a spinning one ~100.
const BUSY_PERCENT: u128 = 80;

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
    /// The previous sample's `(instant, main-thread CPU time)`.
    cpu_sample: Option<(Instant, Duration)>,
    /// Since when every sample found the main thread on-CPU for at least
    /// [`BUSY_PERCENT`] of the interval with no heartbeat in between.
    busy_since: Option<Instant>,
    /// The REPORTED stall that has not ended yet: the root it was first reported
    /// at and when it began, until the main thread moves again — a heartbeat, or
    /// a reported spin at a park point that a CPU reading finds quiet. It
    /// survives a [`Sampler::resync`]: a stall the process was stopped inside is
    /// still open when the process runs again.
    open: Option<OpenStall>,
    /// A reported stall that has since ENDED, waiting for [`Sampler::take_ended`].
    /// Without its line, a stall that recovered and one that lasted until the
    /// process died read the same in `aterm.log`, and the recovery census
    /// (`recovery_census`) could not tell a wedged death from a run that merely
    /// had a slow moment.
    ended: Option<Ended>,
}

/// A reported stall that has not ended yet ([`Sampler::open`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenStall {
    /// The root it was first reported at.
    root: Breadcrumb,
    /// When it began: the last heartbeat, or, for a spin at a park point, when
    /// the main thread went on-CPU. A process gap moves it later by the gap, so
    /// the time it lasted is the main thread's own, never the gap's.
    since: Instant,
}

/// A reported stall that ended, as [`Sampler::take_ended`] hands it out once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ended {
    /// How long it lasted, a process gap not counted.
    frozen: Duration,
    /// The root it was first reported at.
    root: Breadcrumb,
    /// What showed it over.
    by: EndedBy,
}

/// The evidence that a reported stall is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndedBy {
    /// The heartbeat moved: the main thread entered a root again.
    Beat,
    /// A spin reported at a park point went quiet: a CPU reading found the main
    /// thread off-CPU, parked in the OS event wait again, heartbeat unmoved.
    Quiet,
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
            cpu_sample: None,
            busy_since: None,
            open: None,
            ended: None,
        }
    }

    /// Start over at `now`: the process was not running for `gap` (see
    /// [`process_gap`]), and nothing measured across that gap is about the
    /// main thread — except a stall REPORTED before it, which is carried
    /// across. Dropped here, its end would never be logged, and the recovery
    /// census would read a stall the run survived as the run's last word
    /// ("stalled at its end"). If the heartbeat moved across the gap the stall
    /// ended inside it and is handed to [`Sampler::take_ended`] now; if not,
    /// it is still open and ends — with its line — when the main thread moves.
    fn resync(&mut self, now: Instant, beat: u64, gap: Duration) {
        let moved = beat != self.last_beat;
        let open = self.open.take().map(|stall| OpenStall {
            since: stall
                .since
                .checked_add(gap)
                .map_or(now, |since| since.min(now)),
            ..stall
        });
        let ended = self.ended.take();
        *self = Self::with_threshold(now, beat, self.threshold);
        self.open = open;
        self.ended = ended;
        if moved {
            self.end_open(now, EndedBy::Beat);
        }
    }

    /// The reported stall that ended since the last call, once.
    fn take_ended(&mut self) -> Option<Ended> {
        self.ended.take()
    }

    /// One wake of the sampler thread, as the thread takes it: it asked to
    /// sleep `requested`, was gone `elapsed` since the wake before it (the
    /// whole report pass counts, not only the sleep), and read `beat`, `bc`
    /// and `cpu` on waking. A gap of awake time ([`Gap::unscheduled`]) resyncs
    /// and judges nothing else; a sleep alone ([`Wall::Slept`]) stopped the
    /// monotonic clock this accounting runs on, so it changes nothing here and
    /// is only reported.
    fn wake(
        &mut self,
        now: Instant,
        requested: Duration,
        elapsed: Elapsed,
        beat: u64,
        bc: Breadcrumb,
        cpu: Option<Duration>,
    ) -> SamplerWake {
        let gap = classify_gap(requested, elapsed);
        if let Some(late) = gap.and_then(|gap| gap.unscheduled) {
            self.resync(now, beat, late);
            return SamplerWake {
                gap,
                ended: self.take_ended(),
                hit: None,
            };
        }
        let hit = self.poll_with(now, beat, bc, cpu);
        SamplerWake {
            gap,
            ended: self.take_ended(),
            hit,
        }
    }

    /// The heartbeat rule alone, with no CPU reading — what every sample was
    /// before 2026-09-26, and what a platform without the probe still gets.
    #[cfg(test)]
    fn poll(&mut self, now: Instant, cur_beat: u64, bc: Breadcrumb) -> Option<Breadcrumb> {
        self.poll_with(now, cur_beat, bc, None).map(|hit| hit.root)
    }

    /// Fold one sample. Returns a [`Hit`] when this sample should be REPORTED:
    /// once when a contiguous stall crosses the threshold, and then once per
    /// [`STALL_REPEAT_INTERVAL`] for as long as it lasts. A main thread that
    /// never comes back is the case this guard exists for, and one line an hour
    /// ago is not the same evidence as a line saying it is still frozen now.
    ///
    /// Two rules. A frozen heartbeat at a WORK root past the threshold is a
    /// stall whatever the thread is doing (parked on a lock, or spinning). A
    /// frozen heartbeat at a park point is expected — unless `cpu` shows the
    /// main thread ON-CPU for a whole threshold there, which a parked thread
    /// never is ([`Breadcrumb::spin_is_a_stall`]).
    ///
    /// A reported stall stays [`Sampler::open`] until the main thread moves;
    /// its end is handed to [`Sampler::take_ended`].
    fn poll_with(
        &mut self,
        now: Instant,
        cur_beat: u64,
        bc: Breadcrumb,
        cpu: Option<Duration>,
    ) -> Option<Hit> {
        let judged = self.fold_cpu(now, cpu);
        if cur_beat != self.last_beat {
            // Progress: the main thread is alive. Reset the stall clock + re-arm —
            // and when a stall was REPORTED, say that it ended.
            self.end_open(now, EndedBy::Beat);
            self.last_beat = cur_beat;
            self.last_advance = now;
            self.clear_reports();
            self.busy_since = None;
            return None;
        }
        if bc.is_park_point() {
            match self.busy_since.filter(|_| bc.spin_is_a_stall()) {
                Some(since) => {
                    let spun = now.saturating_duration_since(since);
                    if spun < self.threshold || !self.report_due(now) {
                        return None;
                    }
                    return Some(self.open_stall(
                        Hit {
                            root: bc,
                            frozen: spun,
                            busy: true,
                        },
                        since,
                    ));
                }
                None => {
                    // Idle / startup park: a frozen heartbeat is expected here.
                    // Keep the clock reset so leaving idle starts a fresh span.
                    // A spin reported here has ended once a reading finds the
                    // thread quiet; with no reading to go by it stays open
                    // until the heartbeat moves. A stall reported at a WORK
                    // root is not ended here: a park point under its unmoved
                    // heartbeat is the next root's breadcrumb read before its
                    // beat (the breadcrumb is stamped first), and that beat
                    // ends it — it was never a spin.
                    if judged == Some(false)
                        && self.open.is_some_and(|stall| stall.root.is_park_point())
                    {
                        self.end_open(now, EndedBy::Quiet);
                    }
                    self.last_advance = now;
                    self.clear_reports();
                    return None;
                }
            }
        }
        let frozen = now.saturating_duration_since(self.last_advance);
        if !is_stall_at(bc, frozen, self.threshold) || !self.report_due(now) {
            return None;
        }
        let since = self.last_advance;
        Some(self.open_stall(
            Hit {
                root: bc,
                frozen,
                busy: self.busy_since.is_some(),
            },
            since,
        ))
    }

    /// A report is going out: the stall it reports is open until the main
    /// thread moves. A repeat — or a report of a stall a process gap
    /// interrupted — keeps the stall's first root and start.
    fn open_stall(&mut self, hit: Hit, since: Instant) -> Hit {
        self.open.get_or_insert(OpenStall {
            root: hit.root,
            since,
        });
        hit
    }

    /// The open stall, if any, ended at `now`.
    fn end_open(&mut self, now: Instant, by: EndedBy) {
        if let Some(stall) = self.open.take() {
            self.ended = Some(Ended {
                frozen: now.saturating_duration_since(stall.since),
                root: stall.root,
                by,
            });
        }
    }

    /// Track whether the main thread stayed on-CPU between samples. Returns
    /// whether the interval since the previous sample was busy, or `None` when
    /// there is no pair of readings to judge it by.
    fn fold_cpu(&mut self, now: Instant, cpu: Option<Duration>) -> Option<bool> {
        let Some(cpu) = cpu else {
            self.cpu_sample = None;
            self.busy_since = None;
            return None;
        };
        let mut judged = None;
        if let Some((then, was)) = self.cpu_sample {
            let wall = now.saturating_duration_since(then).as_nanos();
            let used = cpu.saturating_sub(was).as_nanos();
            let busy = wall > 0 && used * 100 >= wall * BUSY_PERCENT;
            if busy {
                self.busy_since.get_or_insert(then);
            } else {
                self.busy_since = None;
            }
            judged = Some(busy);
        }
        self.cpu_sample = Some((now, cpu));
        judged
    }

    /// Whether a stall that is past the threshold is due a line now, and
    /// count it if so.
    fn report_due(&mut self, now: Instant) -> bool {
        let due = match self.last_report {
            None => !self.reported,
            Some(at) => now.saturating_duration_since(at) >= STALL_REPEAT_INTERVAL,
        };
        if due {
            self.reported = true;
            self.last_report = Some(now);
            self.reports = self.reports.saturating_add(1);
        }
        due
    }

    fn clear_reports(&mut self) {
        self.reported = false;
        self.last_report = None;
        self.reports = 0;
    }
}

/// What one [`Sampler::wake`] decided, before anything is logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SamplerWake {
    /// The wake was late on either clock ([`classify_gap`]).
    gap: Option<Gap>,
    /// A reported stall that ended by this wake.
    ended: Option<Ended>,
    /// A stall to report now.
    hit: Option<Hit>,
}

impl SamplerWake {
    /// The warn-level lines this wake logs, in order, ahead of any stall
    /// report: a gap of awake time, then a reported stall that ended.
    fn notices(&self) -> Vec<String> {
        self.gap
            .and_then(|gap| {
                gap.unscheduled
                    .map(|late| gap_message(late, gap.wall, gap.pass))
            })
            .into_iter()
            .chain(self.ended.map(stall_ended_message))
            .collect()
    }

    /// The info-level line for a wake that found the Mac had slept and this
    /// process had lost no awake time: routine, so not a warning, and logged
    /// ahead of [`SamplerWake::notices`].
    fn sleep_note(&self) -> Option<String> {
        match self.gap {
            Some(Gap {
                unscheduled: None,
                wall: Wall::Slept(slept),
                ..
            }) => Some(slept_message(slept)),
            _ => None,
        }
    }
}

/// How late a sampler wake must be before it means the whole PROCESS was not
/// running. `Instant` does not advance while the Mac sleeps, so lateness on it
/// is time the system was awake and this process was not scheduled: napped
/// (App Nap), starved of CPU, stopped (SIGSTOP), paused under a debugger, or
/// suspended. The wall clock running this much further than `Instant` is the
/// Mac asleep instead ([`Wall::Slept`]).
const PROCESS_GAP: Duration = Duration::from_secs(30);

/// The lateness of a sampler wake that asked to sleep `requested` and was gone
/// `observed` on the monotonic clock, when it is a [`PROCESS_GAP`].
fn process_gap(requested: Duration, observed: Duration) -> Option<Duration> {
    let late = observed.saturating_sub(requested);
    (late >= PROCESS_GAP).then_some(late)
}

/// How long one sampler wake was gone, on both clocks, since the wake before
/// it — so the report pass between two sleeps is inside the span, the stack
/// capture included ([`Clocks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Elapsed {
    /// On the monotonic clock ([`Instant`]), which stops while the Mac sleeps
    /// (on Apple platforms std reads `CLOCK_UPTIME_RAW`).
    mono: Duration,
    /// On the wall clock ([`SystemTime`]), which keeps running while the Mac
    /// sleeps; `None` when it went backwards (the clock was set back).
    wall: Option<Duration>,
    /// Of `mono`, the report pass before the sleep: the stall lines, the
    /// stack capture and the stack lines when the wake before this one
    /// reported, next to nothing otherwise.
    pass: Duration,
}

/// Both clocks, read together at one sampler wake. The sampler thread keeps
/// the previous wake's reading and measures the next wake from it, never from
/// the start of that wake's own sleep: timed from the sleep alone, the
/// 2026-09-28 gap — after a stall line, in the pass that captures and writes
/// the stack — left every wake on time and was charged to the main thread.
#[derive(Clone, Copy)]
struct Clocks {
    mono: Instant,
    wall: SystemTime,
}

impl Clocks {
    fn now() -> Self {
        Self {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    /// The span from `earlier` to this reading, for a sampler thread that
    /// went to sleep at `asleep`: what came before that was its report pass.
    fn since(self, earlier: Self, asleep: Instant) -> Elapsed {
        Elapsed {
            mono: self.mono.saturating_duration_since(earlier.mono),
            wall: self.wall.duration_since(earlier.wall).ok(),
            pass: asleep.saturating_duration_since(earlier.mono),
        }
    }
}

/// What the wall clock says about a span the monotonic clock measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wall {
    /// It ran at least [`PROCESS_GAP`] further than the monotonic clock,
    /// which stops while the Mac sleeps: the Mac slept about this long (or its
    /// clock was set forward this far).
    Slept(Duration),
    /// It ran no more than that further: the Mac was awake throughout.
    Awake,
    /// It went backwards (the clock was set back), so it says nothing.
    Unknown,
}

/// A sampler wake that was late on either clock ([`classify_gap`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gap {
    /// Time the system was awake and this process did not run, when it is a
    /// [`PROCESS_GAP`]: the sampler resyncs over it.
    unscheduled: Option<Duration>,
    /// What the wall clock says about the same span.
    wall: Wall,
    /// How much of the span was the report pass before the sleep
    /// ([`Elapsed::pass`]).
    pass: Duration,
}

/// Read a wake's `elapsed` span on both clocks: lateness on the monotonic
/// clock is awake time this process did not run ([`process_gap`]), and the
/// wall clock's lead over the monotonic one is time the Mac slept. `None`
/// when neither reaches [`PROCESS_GAP`].
fn classify_gap(requested: Duration, elapsed: Elapsed) -> Option<Gap> {
    let unscheduled = process_gap(requested, elapsed.mono);
    let wall = match elapsed.wall {
        None => Wall::Unknown,
        Some(wall) => match wall.checked_sub(elapsed.mono) {
            Some(ahead) if ahead >= PROCESS_GAP => Wall::Slept(ahead),
            _ => Wall::Awake,
        },
    };
    (unscheduled.is_some() || matches!(wall, Wall::Slept(_))).then_some(Gap {
        unscheduled,
        wall,
        pass: elapsed.pass,
    })
}

/// A report pass at least this long is named in the gap line: it normally
/// takes milliseconds, so a second of it is part of the gap.
const PASS_WORTH_NAMING: Duration = Duration::from_secs(1);

/// The warn line for a gap of `late` awake time ([`Gap::unscheduled`]), with
/// how much of it fell in the watchdog's own report pass and what the wall
/// clock says about the same span. Time in the pass is named because it is
/// the weaker evidence: a sleeping thread that wakes late was not scheduled,
/// but a pass can also have waited on a log write stuck on the disk. It must
/// never contain `MAIN-THREAD STALL`: the recovery census reads any line that
/// does as a stall (`recovery_census::STALL_LINE`).
fn gap_message(late: Duration, wall: Wall, pass: Duration) -> String {
    let secs = late.as_secs();
    let pass = if pass >= PASS_WORTH_NAMING {
        format!(
            " {}s of it fell in the watchdog's own report pass (the stall lines and the stack \
             capture), where a log write stuck on the disk would read the same.",
            pass.as_secs()
        )
    } else {
        String::new()
    };
    let wall: String = match wall {
        Wall::Slept(slept) => format!(
            "The wall clock ran another {}s past that, so the Mac also slept (or its clock \
             was set forward).",
            slept.as_secs()
        ),
        Wall::Awake => "The wall clock ran no further, so the Mac did not sleep through it.".into(),
        Wall::Unknown => {
            "The wall clock was set back meanwhile, so it cannot say whether the Mac also \
             slept."
                .into()
        }
    };
    format!(
        "this process was not running for {secs}s while the system was awake: it was not \
         scheduled (App Nap or CPU starvation; on 2026-09-28 a window lost 3 h 22 m this way \
         on a Mac at load 45-151 with its swap full), or it was stopped (SIGSTOP), paused \
         under a debugger, or suspended.{pass} {wall} The stall accounting starts over here, so \
         none of this is charged to the main thread. Every timer the process had armed came \
         due at once; the 2026-09-26 freeze was CoreFoundation walking one such timer \
         forward after 26.6 hours of this."
    )
}

/// The info line for a wake that found only the Mac asleep ([`Wall::Slept`]
/// with no [`Gap::unscheduled`]).
fn slept_message(slept: Duration) -> String {
    format!(
        "the Mac slept for about {}s: the wall clock ran that much further than the \
         monotonic clock, which stops in sleep (or the clock was set forward). This process \
         lost no awake time, so the stall accounting is unchanged.",
        slept.as_secs()
    )
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
    /// When the main thread last entered a root, on the same clock: the
    /// heartbeat's timestamp, which [`main_stall`] reads. Not `open_ns`, which
    /// [`TurnLedger::reset`] zeroes: a `metrics reset` sent while the main
    /// thread is stuck would make a reader of that stamp see a thread that
    /// never beat, and let every verb through to wait out its deadline (the
    /// defect the `MainThreadStallRefusal` machine replays). `reset` leaves
    /// this one alone. 0 = no beat yet.
    last_beat_ns: AtomicU64,
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
    /// The strain engine's FREEZE mailbox (design §10.14, ruling 211): the worst
    /// turn of at least [`FREEZE_NS`] since the host last drained it
    /// ([`take_freeze`]), when it ended and its owner. `freeze_span_ns == 0`
    /// is empty. Not a window stat: `reset` leaves it for its reader.
    freeze_span_ns: AtomicU64,
    freeze_end_ns: AtomicU64,
    freeze_owner: AtomicU8,
}

/// A main-loop turn at least this long is a FREEZE for the strain engine
/// (`aterm_messages::FREEZE_MS`); the host decides whether a hardware key was
/// near enough to make it a hitch.
const FREEZE_NS: u64 = aterm_messages::FREEZE_MS as u64 * 1_000_000;

impl TurnLedger {
    const fn new() -> Self {
        Self {
            open_ns: AtomicU64::new(0),
            last_beat_ns: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            max_owner: AtomicU8::new(Breadcrumb::Startup as u8),
            max_at_ns: AtomicU64::new(0),
            turns: AtomicU64::new(0),
            long_turns: AtomicU64::new(0),
            freeze_span_ns: AtomicU64::new(0),
            freeze_end_ns: AtomicU64::new(0),
            freeze_owner: AtomicU8::new(Breadcrumb::Startup as u8),
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
        crate::metrics::note_turn(previous as u8, span);
        self.last_ns.store(span, Ordering::Relaxed);
        self.turns.fetch_add(1, Ordering::Relaxed);
        if span >= LONG_TURN_THRESHOLD_NS {
            self.long_turns.fetch_add(1, Ordering::Relaxed);
        }
        if self.max_ns.fetch_max(span, Ordering::Relaxed) < span {
            self.max_owner.store(previous as u8, Ordering::Relaxed);
            self.max_at_ns.store(now_ns, Ordering::Relaxed);
        }
        if span >= FREEZE_NS {
            // Keep-worst until the host drains it: two freezes between drains
            // are rarer than the loop's own turns, and the worse one is the one
            // a person felt.
            if self.freeze_span_ns.load(Ordering::Relaxed) < span {
                self.freeze_owner.store(previous as u8, Ordering::Relaxed);
                self.freeze_end_ns.store(now_ns, Ordering::Relaxed);
                self.freeze_span_ns.store(span, Ordering::Relaxed);
            }
        }
        Some(span)
    }

    /// The worst turn of at least [`FREEZE_NS`] booked since the last take:
    /// `(end_ns, span_ns, owner)`, and the mailbox is emptied.
    fn take_freeze(&self) -> Option<(u64, u64, Breadcrumb)> {
        let span = self.freeze_span_ns.swap(0, Ordering::Relaxed);
        (span != 0).then(|| {
            (
                self.freeze_end_ns.load(Ordering::Relaxed),
                span,
                Breadcrumb::from_u8(self.freeze_owner.load(Ordering::Relaxed)),
            )
        })
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
    /// drives a workload and reads must see that workload's worst turn. The
    /// heartbeat's timestamp (`last_beat_ns`) is not a window stat and stays.
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
pub(crate) struct TurnCensus {
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
/// A monotonic clock read and a handful of atomic writes, all relaxed but the
/// breadcrumb's release (see [`main_stall_now`]) — cheap enough to sit on the
/// hot event path in every build, and the only place in the process that can
/// price a park OUTSIDE the redraw (see the module header). The
/// breadcrumb is stamped BEFORE the heartbeat bumps so the sampler never reads a
/// fresh count against a stale location, and any announced [`Phase`] is cleared
/// with it: the work a phase names belongs to the root that announced it.
#[inline]
pub(crate) fn beat(bc: Breadcrumb) {
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
    // The heartbeat's timestamp, for `main_stall`: the clock value this beat
    // already read, stamped BEFORE the root is published (the swap below is
    // `Release`, and `main_stall_now` reads the root with `Acquire` before the
    // stamp), so a reader never pairs this root with the previous beat's time.
    // `max(1)` keeps 0 meaning "no beat yet".
    turns.last_beat_ns.store(now_ns.max(1), Ordering::Relaxed);
    let previous = Breadcrumb::from_u8(BREADCRUMB.swap(bc as u8, Ordering::Release));
    let booked = turns.close(previous, now_ns);
    // A root entry starts with NO announced phase (see `PHASE`): one more
    // relaxed store on the hot path, the only way a phase ever ends besides its
    // guard dropping, and stamped BEFORE the heartbeat bumps for the same
    // reason the breadcrumb is — the sampler must never read a fresh count
    // against a stale word.
    phase.store(Phase::None as u8, Ordering::Relaxed);
    RETURNED.store(false, Ordering::Relaxed);
    HEARTBEAT.fetch_add(1, Ordering::Relaxed);
    booked
}

/// Enter a winit root: [`beat`] now, and mark the root RETURNED when the
/// guard drops at the end of the handler, so a stall that happens after the
/// handler is gone — in AppKit or CoreFoundation, between aterm's handlers —
/// is not reported as being inside it. Every `ApplicationHandler` root holds
/// one for its whole body.
#[must_use = "the root lasts only as long as the guard lives"]
pub(crate) fn enter(bc: Breadcrumb) -> RootGuard {
    beat(bc);
    RootGuard { _private: () }
}

/// The RAII half of [`enter`].
pub(crate) struct RootGuard {
    _private: (),
}

impl Drop for RootGuard {
    fn drop(&mut self) {
        RETURNED.store(true, Ordering::Relaxed);
    }
}

/// The turn census, for the `metrics` verb.
#[must_use]
pub(crate) fn turn_census() -> TurnCensus {
    TURNS.snapshot()
}

/// The worst main-loop turn of at least `aterm_messages::FREEZE_MS` since the
/// last call — `(end_ns, span_ns, owner)` on the `crate::metrics::now_ns` clock —
/// emptying the mailbox. The strain host drains it on the main thread (the only
/// writer is the same thread's [`beat`]), and alone decides whether a hardware
/// key was near enough to make it a hitch.
pub(crate) fn take_freeze() -> Option<(u64, u64, Breadcrumb)> {
    TURNS.take_freeze()
}

/// Clear the turn census. Called by [`crate::metrics::reset`], so the census is a
/// window stat like every other `max_` on that line.
pub(crate) fn reset_turn_census() {
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
pub(crate) fn turn_census_fields_text() -> String {
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
pub(crate) fn turn_census_fields_json() -> String {
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
#[cfg(any(target_os = "macos", test))]
pub(crate) fn current() -> Breadcrumb {
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
#[must_use = "the park lasts only as long as the guard lives"]
#[cfg(any(target_os = "macos", test))]
pub(crate) fn park_modal() -> ModalPark {
    let previous = current();
    beat(Breadcrumb::Modal);
    ModalPark { previous }
}

/// The RAII half of [`park_modal`].
#[cfg(any(target_os = "macos", test))]
pub(crate) struct ModalPark {
    previous: Breadcrumb,
}

#[cfg(any(target_os = "macos", test))]
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
pub(crate) enum Phase {
    /// Nothing announced: the root is all the line says.
    None = 0,
    /// `App::ensure_pixel_backend` — a headless run's FIRST pixel demand
    /// redeeming, on the main thread, what the launch deferred: the font seal
    /// (three font files read and parsed; Apple Color Emoji is 190 MB of it
    /// on macOS), the chrome re-sync that follows it (`sync_chrome_fonts`:
    /// the chrome rasterizer's faces handed over again and its semantic
    /// surface re-forked from the sealed generation — not those three files,
    /// so the redemption's line times it as a leg of its own), the CPU face
    /// fork, and `GpuRenderer::new_with_family`. That last leg is NOT just
    /// the device: `new_with_family` spawns a font thread
    /// (`Renderer::from_system_with_family` — font resolution, file read,
    /// parse — then `prewarm_ascii`), builds the GPU context on the
    /// calling thread, JOINS the font thread, and only then calls
    /// `from_parts`, so the leg costs max(context, font thread) plus
    /// `from_parts`. On the wgpu arms the context is the instance, the
    /// adapter, the device and the context's tail (`GpuContextTail`), and
    /// `from_parts`, after the join, compiles the shader and builds the
    /// pipelines. On the macOS Metal arm `GpuContext::new` only NAMES the
    /// preferred device and keeps nothing of it but the name — on the
    /// system-default pick that naming IS `MTLCreateSystemDefaultDevice`.
    /// That pick is taken only where `MTLCopyAllDevices`' listing cannot
    /// decide (`aterm_gpu`'s `choose_listed`, since 2026-09-26): two or more
    /// display GPUs listed and `ATERM_GPU_POWER=high` (a dual-GPU Mac under
    /// that seam), two or more and none low-power, or none; everywhere else
    /// — an Apple-silicon Mac's one GPU, a dual-GPU Mac's low-power one by
    /// default — the device is read out of the listing, which (measured on
    /// Apple silicon) asks the display server nothing and (per the SDK
    /// header `Device::preferred` quotes) switches no mux. Meanwhile
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
    /// window lie BEFORE that device line: in the seal, the chrome re-sync,
    /// the fork and the `new_with_family` legs, ~9 s in all, of a ~10 s
    /// capture. Which of them, and inside the last whether the device name
    /// or the font thread, is what the log line's legs exist to say next
    /// time. That instance predates `Device::preferred`: its
    /// `GpuContext::new` and its `MetalArmLive::new` both took
    /// `MTLCreateSystemDefaultDevice`, the call Apple documents as switching
    /// a dual-GPU Mac to the discrete GPU.
    /// So aterm's first call that could set off a switch to the Radeon was
    /// here, in the device leg; the first-frame mint made a second one, in
    /// the 945 ms tail (see [`Phase::ImageCapture`]), after `GpuContext::new`
    /// had released its reference to the device it named; nothing observed
    /// the mux or timed either call. The aterm.log the line sits in
    /// holds no other `UserEvent` stall line (checked 2026-09-10; every
    /// other one is inside `NewEvents`). The surviving re-measurements
    /// (that evening, debug build, on the Intel HD Graphics
    /// 630, with a test build running alongside) put the redemption at
    /// 509-533 ms: 488-510 ms in the seal and the chrome re-sync together
    /// (that build's line timed the two as one `font seal` leg, so how the
    /// figure divides between them is not known), fork 0, `new_with_family`
    /// 21-22 ms, install 0.
    PixelBackendRedeem = 1,
    /// `App::render_image` — the `image` verb's capture on the main thread,
    /// the control verb the 2026-09-06 stall was inside. Not the only verb
    /// that does renderer work there — the offscreen present-real `video`
    /// loop and the SIGUSR1 `snapshot` path render on the main thread too,
    /// and announce nothing but the redemption they nest — but the one that
    /// finding named. Headless, when a capture is the run's first pixel
    /// demand, the redemption above is nested in it, and on macOS, when the
    /// run has a GPU intent (not `--cpu`), that capture also mints the Metal
    /// device, its command queue and `cell.metal`, then demand-builds the
    /// three pipelines the first frame binds — the shader-cache-sensitive
    /// work. The device is
    /// `Device::preferred`'s pick: the low-power GPU of a dual-GPU Mac,
    /// unless `ATERM_GPU_POWER=high` asks for the system default, which on
    /// such a Mac is the discrete GPU (its listing shows two display GPUs,
    /// so under `high` `Device::preferred` does not answer from the listing
    /// — it does that only for a single display GPU — and makes the call
    /// below). On that pick the naming in
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
    /// 509 ms redemption (488 ms of it the seal and the chrome re-sync, timed
    /// as one) with the capture around it,
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
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Phase::None => "",
            Phase::PixelBackendRedeem => {
                "the headless pixel-backend redemption (the deferred font seal and its chrome \
                 re-sync, the face fork, and the GPU context with its font thread)"
            }
            Phase::ImageCapture => {
                "an `image` capture (headless with a GPU intent, the capture that is the run's \
                 first pixel demand also mints the GPU device and compiles its shaders)"
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
pub(crate) fn phase(p: Phase) -> PhaseGuard {
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
pub(crate) fn current_phase() -> Phase {
    phase_in(&PHASE)
}

/// The phase a cell holds.
fn phase_in(cell: &AtomicU8) -> Phase {
    Phase::from_u8(cell.load(Ordering::Relaxed))
}

/// The RAII half of [`phase`]: the cell it wrote, what it wrote there, and
/// what stood before.
pub(crate) struct PhaseGuard {
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

/// Where the main thread was when a stall was reported, as far as the
/// heartbeat can tell: inside the root it last entered, or back in AppKit /
/// CoreFoundation after that root RETURNED ([`RETURNED`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Locus {
    /// The root's handler is still on the stack.
    Inside,
    /// The root's handler returned; no aterm handler is running.
    Returned,
}

/// The stall line, assembled from the sampler's facts alone so its wording is
/// testable without a thread or a logger: the FIRST report of a contiguous
/// stall names the root and the announced phase and says what the class of
/// hazard is; every later one says the same stall is still there. A stall
/// after the root RETURNED says so instead of naming the root as the place
/// (the 2026-09-26 line named `NewEvents` for a spin in CoreFoundation), and
/// `busy` says the main thread was on-CPU — a spin, not a park.
fn stall_message(
    reports: u32,
    frozen: Duration,
    bc: Breadcrumb,
    phase: Phase,
    locus: Locus,
    busy: bool,
) -> String {
    let cpu = if busy {
        ", ON-CPU the whole time (a spin, not a park)"
    } else {
        ""
    };
    match (locus, reports) {
        (Locus::Inside, 1) => format!(
            "MAIN-THREAD STALL: no heartbeat for {frozen:?} while inside `{}`{}{cpu} — \
             the UI is not responding. Either unbounded work under a contended lock \
             (the L0 freeze hazard) or a park that will never end (a lock or lazy-init \
             cycle). This line names the main-loop root without symbols; a hang report \
             is not required to find it.",
            bc.name(),
            phase.clause()
        ),
        (Locus::Inside, _) => format!(
            "MAIN-THREAD STALL CONTINUES: still no heartbeat after {frozen:?} inside \
             `{}`{}{cpu} — this is a wedge, not a slow frame.",
            bc.name(),
            phase.clause()
        ),
        (Locus::Returned, 1) => format!(
            "MAIN-THREAD STALL: no heartbeat for {frozen:?} since `{}` returned{cpu} — \
             the main thread is outside every aterm handler, in AppKit or CoreFoundation's \
             own run-loop work (a timer, source or observer; the 2026-09-26 freeze was \
             CoreFoundation's catch-up for a late timer), and the UI is not responding. \
             The stack lines that follow name the frame.",
            bc.name()
        ),
        (Locus::Returned, _) => format!(
            "MAIN-THREAD STALL CONTINUES: still no heartbeat after {frozen:?} since `{}` \
             returned{cpu} — the main thread is still outside aterm's handlers.",
            bc.name()
        ),
    }
}

/// The line when a REPORTED stall ends: how long it lasted, the root it was
/// reported at, and what showed it over. It is what lets the next launch's
/// recovery census tell a stall the run survived from one it died in
/// (`recovery_census::STALL_ENDED_LINE` is its prefix). The root is where the
/// stall was reported, not a claim that its handler was still on the stack: the
/// report line already said which (`Locus`).
fn stall_ended_message(ended: Ended) -> String {
    let Ended { frozen, root, by } = ended;
    match by {
        EndedBy::Beat => format!(
            "MAIN-THREAD STALL ENDED: the main thread beat again after about {frozen:?} \
             stalled at `{}`.",
            root.name()
        ),
        EndedBy::Quiet => format!(
            "MAIN-THREAD STALL ENDED: the main thread stopped spinning at `{}` after about \
             {frozen:?} on-CPU and is parked in the event wait again.",
            root.name()
        ),
    }
}

/// The prefix of every stack line. It contains `MAIN-THREAD STALL`, which the
/// recovery census reads as a stall (`recovery_census::STALL_LINE`), so a
/// stack is only ever logged after the stall line it belongs to.
const STACK_LINE: &str = "MAIN-THREAD STALL stack:";

/// Between two frames of a stack line.
const FRAME_SEPARATOR: &str = " | ";

/// The main thread's stack as log lines, as many as it takes to keep each one
/// within `cap`, the logger's body cap for the level they are logged at
/// ([`aterm_log::record_cap`]): the logger cuts a longer body short, and on
/// 2026-09-28 that cut the stack's one line at frame #11.
///
/// Each line is `MAIN-THREAD STALL stack: (part i/n, captured at <stamp>)`
/// and then its frames in order, ` | ` between them. The stamp is `captured`
/// in the log's own `<epoch seconds>.<millis>` form, so it reads against each
/// line's leading timestamp: a part written long after its capture — the
/// process stopped running in between — says so. A frame too long for a line
/// of its own is clipped with `…`. No frames, no lines.
fn stack_lines(frames: &[String], captured: SystemTime, cap: usize) -> Vec<String> {
    let captured = epoch_stamp(captured);
    let header = |part: usize, parts: usize| {
        format!("{STACK_LINE} (part {part}/{parts}, captured at {captured}) ")
    };
    // No stack has more parts than frames, so a header numbered with the frame
    // count is at least as long as any header a part will carry.
    let widest = frames.len().max(1);
    let budget = cap.saturating_sub(logged_len(&header(widest, widest)));
    let mut bodies = Vec::new();
    let mut body = String::new();
    let mut used = 0usize;
    let mut in_body = 0usize;
    for frame in frames {
        let frame = clip(frame, budget);
        let len = logged_len(&frame);
        if in_body > 0 && used + FRAME_SEPARATOR.len() + len > budget {
            bodies.push(std::mem::take(&mut body));
            used = 0;
            in_body = 0;
        }
        if in_body > 0 {
            body.push_str(FRAME_SEPARATOR);
            used += FRAME_SEPARATOR.len();
        }
        body.push_str(&frame);
        used += len;
        in_body += 1;
    }
    if in_body > 0 {
        bodies.push(body);
    }
    let parts = bodies.len();
    bodies
        .iter()
        .enumerate()
        .map(|(i, body)| format!("{}{body}", header(i + 1, parts)))
        .collect()
}

/// `at` as the log writes its own timestamps: Unix seconds, a dot, millis.
fn epoch_stamp(at: SystemTime) -> String {
    match at.duration_since(UNIX_EPOCH) {
        Ok(since) => format!("{}.{:03}", since.as_secs(), since.subsec_millis()),
        Err(_) => "an unknown time (the clock reads before 1970)".into(),
    }
}

/// The bytes `c` takes in a log line: the logger writes every control
/// character as U+FFFD ([`aterm_log::sanitize_record_for`]).
fn logged_char_len(c: char) -> usize {
    if c.is_control() {
        char::REPLACEMENT_CHARACTER.len_utf8()
    } else {
        c.len_utf8()
    }
}

/// The bytes `s` takes in a log line ([`logged_char_len`]).
fn logged_len(s: &str) -> usize {
    s.chars().map(logged_char_len).sum()
}

/// `frame` as it fits in `budget` logged bytes: whole when it fits, else cut
/// on a character boundary and ended with `…` inside the budget.
fn clip(frame: &str, budget: usize) -> String {
    if logged_len(frame) <= budget {
        return frame.to_string();
    }
    let room = budget.saturating_sub('…'.len_utf8());
    let mut out = String::new();
    let mut used = 0usize;
    for c in frame.chars() {
        let len = logged_char_len(c);
        if used + len > room {
            break;
        }
        out.push(c);
        used += len;
    }
    out.push('…');
    out
}

/// The `$ATERM_WATCHDOG` development seam's value; `None` in every shipped binary.
fn seam() -> Option<String> {
    aterm_types::dev_seam!("ATERM_WATCHDOG").map(|v| v.to_string_lossy().trim().to_string())
}

/// Whether the watchdog sampler should run. EVERY build, unless a development
/// build switches it off with `ATERM_WATCHDOG=off` — see the module header for
/// why a release binary is the build that needs this most.
fn enabled() -> bool {
    !seam().is_some_and(|v| {
        v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("0") || v.is_empty()
    })
}

/// The bar this build judges a stall against: tight when a developer is
/// watching (debug, or the `$ATERM_WATCHDOG` seam), coarse in a shipped binary
/// where the reader is a user's `aterm.log`.
fn threshold() -> Duration {
    if cfg!(debug_assertions) || seam().is_some() {
        STALL_THRESHOLD
    } else {
        RELEASE_STALL_THRESHOLD
    }
}

/// Whether a detected stall should `process::abort()` (repro / CI) rather than
/// just log. A development build's `ATERM_WATCHDOG=abort`.
fn abort_on_stall() -> bool {
    seam().is_some_and(|v| v.eq_ignore_ascii_case("abort"))
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
pub(crate) fn start() {
    if !enabled() {
        return;
    }
    let abort = abort_on_stall();
    let threshold = threshold();
    // The main thread is the caller; the probe reads its CPU time every
    // sample and its stack at a report.
    crate::main_thread_probe::register();
    let builder = std::thread::Builder::new().name("aterm-watchdog".into());
    // A spawn failure is non-fatal: the app runs fine without the tripwire.
    let _ = builder.spawn(move || {
        let sample = sample_interval();
        aterm_log::info!(
            "main-thread stall watchdog armed (sample {sample:?}, threshold \
             {threshold:?}, repeat {STALL_REPEAT_INTERVAL:?}, abort={abort})"
        );
        // The previous wake on both clocks: each wake is measured from it, not
        // from the start of its own sleep, so the report pass in between —
        // the stall line, the stack capture that suspends the main thread,
        // the stack lines — is inside the span a gap is judged on ([`Clocks`]).
        // The start of the sleep still marks where the pass ended, so the gap
        // line can say how much of a gap fell in it.
        let mut last = Clocks::now();
        let mut sampler =
            Sampler::with_threshold(last.mono, HEARTBEAT.load(Ordering::Relaxed), threshold);
        loop {
            let asleep = Instant::now();
            std::thread::sleep(sample);
            let here = Clocks::now();
            // A wake far later than asked, on a clock that stops while the
            // Mac sleeps, is the whole process not running: `wake` says so —
            // it is the trigger no other line records — and starts over, since
            // none of the gap was the main thread's doing. A stall reported
            // before the gap stays open across it, and its end is still said.
            // The wall clock tells that apart from the Mac asleep.
            let wake = sampler.wake(
                here.mono,
                sample,
                here.since(last, asleep),
                HEARTBEAT.load(Ordering::Relaxed),
                Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Relaxed)),
                crate::main_thread_probe::cpu_time(),
            );
            last = here;
            if let Some(line) = wake.sleep_note() {
                aterm_log::info!("{line}");
            }
            for line in wake.notices() {
                aterm_log::warn!("{line}");
            }
            if let Some(hit) = wake.hit {
                // The phase and the locus are read AT the report, just after
                // the sample that decided it. If the main thread beat in that
                // instant they read the next root's; either way they describe
                // the root the main thread is in now (a beat clears both). A
                // nested phase that ended before the report reads as the phase
                // around it.
                let locus = if RETURNED.load(Ordering::Relaxed) {
                    Locus::Returned
                } else {
                    Locus::Inside
                };
                aterm_log::error!(
                    "{}",
                    stall_message(
                        sampler.reports,
                        hit.frozen,
                        hit.root,
                        current_phase(),
                        locus,
                        hit.busy
                    )
                );
                // Where the thread IS, not just which root ran last: the frame
                // a hang report would have named, in the log, in every build —
                // every frame of it, in parts under the ERROR body cap. Stamped
                // just BEFORE the capture: `stack` suspends the thread first
                // and symbolizes after, and symbolizing (dladdr, Mach-O reads
                // that can page in under swap pressure) is the slower half.
                let captured = SystemTime::now();
                if let Some(frames) = crate::main_thread_probe::stack() {
                    let cap = aterm_log::record_cap(aterm_log::Level::Error);
                    for line in stack_lines(&frames, captured, cap) {
                        aterm_log::error!("{line}");
                    }
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
            Locus::Inside,
            false,
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
        assert!(
            line.contains("(the deferred font seal and its chrome re-sync, the face fork,"),
            "the re-sync is named, not folded into the seal: {line}"
        );
        let bare = stall_message(
            1,
            Duration::from_secs(1),
            Breadcrumb::ResizeSettle,
            Phase::None,
            Locus::Inside,
            false,
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
            Locus::Inside,
            false,
        );
        assert!(
            again.starts_with(
                "MAIN-THREAD STALL CONTINUES: still no heartbeat after 103s inside \
                 `NewEvents`, in an `image` capture (headless with a GPU intent, the capture \
                 that is the run's first pixel demand"
            ),
            "only the run's first pixel demand, and only with a device to mint: {again}"
        );
        assert!(
            again.ends_with(
                "first pixel demand also mints the GPU device and compiles its shaders) — this \
                 is a wedge, not a slow frame."
            ),
            "the mechanism, and no figure: {again}"
        );
        assert!(!again.contains("the first one"), "{again}");
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
            saw_stack: Arc<std::sync::Mutex<String>>,
        }
        impl aterm_log::Log for Capture {
            fn enabled(&self, _m: &aterm_log::Metadata) -> bool {
                true
            }
            fn log(&self, record: &aterm_log::Record<'_>) {
                let line = format!("{}", record.args());
                if line.starts_with(STACK_LINE) {
                    // Every part, in order: the stack is several lines now.
                    let mut stack = self.saw_stack.lock().unwrap();
                    stack.push_str(&line);
                    stack.push('\n');
                } else if line.contains("MAIN-THREAD STALL") {
                    self.fired.store(true, Ordering::SeqCst);
                    *self.saw_name.lock().unwrap() = line;
                }
            }
            fn flush(&self) {}
        }

        let fired = Arc::new(AtomicBool::new(false));
        let saw = Arc::new(std::sync::Mutex::new(String::new()));
        let stack = Arc::new(std::sync::Mutex::new(String::new()));
        // Leak the logger to obtain the `&'static` `set_logger` requires.
        let cap: &'static Capture = Box::leak(Box::new(Capture {
            fired: fired.clone(),
            saw_name: saw.clone(),
            saw_stack: stack.clone(),
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
        // `start` registered THIS thread as the one to probe, so the stack line
        // that follows the stall line is this test's own, asleep.
        #[cfg(target_os = "macos")]
        {
            let stack = stack.lock().unwrap().clone();
            eprintln!("{stack}");
            assert!(
                stack.contains("load address 0x") && stack.contains(" | #1 "),
                "a stall line is followed by the main thread's stack; got: {stack}"
            );
            assert!(
                stack.starts_with("MAIN-THREAD STALL stack: (part 1/"),
                "numbered from its first part; got: {stack}"
            );
            let cap = aterm_log::record_cap(aterm_log::Level::Error);
            assert!(
                stack.lines().all(|part| part.len() <= cap),
                "every part fits the ERROR body cap; got: {stack}"
            );
        }
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

    /// THE STRAIN ENGINE'S FREEZE MAILBOX (design §10.14, ruling 211): a
    /// turn of at least `FREEZE_MS` waits, worst first, for the host to take
    /// it once; an ordinary long turn (a slow frame) and a designed park never
    /// reach it.
    #[test]
    fn a_freeze_waits_worst_first_for_the_strain_host_and_is_taken_once() {
        const MS: u64 = 1_000_000;
        let turns = TurnLedger::new();
        assert_eq!(turns.close(Breadcrumb::WindowEvent, MS), None, "arms only");
        let t1 = MS + 450 * MS;
        assert_eq!(turns.close(Breadcrumb::WindowEvent, t1), Some(450 * MS));
        let t2 = t1 + 600 * MS;
        assert_eq!(turns.close(Breadcrumb::UserEvent, t2), Some(600 * MS));
        // A slow frame is not a freeze; a ten-minute idle is a designed park.
        let t3 = t2 + 120 * MS;
        assert_eq!(turns.close(Breadcrumb::WindowEvent, t3), Some(120 * MS));
        assert_eq!(
            turns.close(Breadcrumb::AboutToWait, t3 + 600_000 * MS),
            None
        );
        assert_eq!(
            turns.take_freeze(),
            Some((t2, 600 * MS, Breadcrumb::UserEvent)),
            "the worse of the two, with its end and its owner"
        );
        assert_eq!(turns.take_freeze(), None, "taken once");
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

    /// A REPORTED stall that ends says so, once, with how long it lasted and where —
    /// the line that lets the next launch's recovery census tell a stall the run
    /// survived from one it died in. A freeze that never crossed the bar was never
    /// reported, so its end is not news either.
    #[test]
    fn a_reported_stall_that_ends_says_so_once_and_an_unreported_one_never_does() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 7, STALL_THRESHOLD);
        // A 300 ms freeze under the bar, then progress: nothing reported, nothing ended.
        assert_eq!(
            s.poll(t0 + Duration::from_millis(300), 7, Breadcrumb::UserEvent),
            None
        );
        assert_eq!(
            s.poll(t0 + Duration::from_millis(400), 8, Breadcrumb::UserEvent),
            None
        );
        assert_eq!(s.take_ended(), None);
        // A reported stall inside `UserEvent`, still frozen at the next sample.
        let from = t0 + Duration::from_millis(400);
        assert_eq!(
            s.poll(from + Duration::from_millis(600), 8, Breadcrumb::UserEvent),
            Some(Breadcrumb::UserEvent)
        );
        assert_eq!(s.take_ended(), None, "still frozen: nothing has ended");
        // The heartbeat moves 9 s after it froze: the stall ended, inside the root it
        // was reported in, and it is handed out exactly once.
        assert_eq!(
            s.poll(from + Duration::from_secs(9), 9, Breadcrumb::AboutToWait),
            None
        );
        let ended = Ended {
            frozen: Duration::from_secs(9),
            root: Breadcrumb::UserEvent,
            by: EndedBy::Beat,
        };
        assert_eq!(s.take_ended(), Some(ended));
        assert_eq!(s.take_ended(), None);
        #[cfg(unix)]
        for by in [EndedBy::Beat, EndedBy::Quiet] {
            assert!(
                stall_ended_message(Ended { by, ..ended })
                    .starts_with(crate::recovery_census::STALL_ENDED_LINE)
            );
        }
        // The ended line must never read as a stall report to the census, which
        // checks for the ENDED prefix first; the report itself carries no ENDED.
        for locus in [Locus::Inside, Locus::Returned] {
            for reports in [1, 2] {
                assert!(
                    !stall_message(
                        reports,
                        Duration::from_secs(5),
                        Breadcrumb::UserEvent,
                        Phase::None,
                        locus,
                        true
                    )
                    .contains("ENDED")
                );
            }
        }
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
        if seam().is_none() {
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

    /// THE 2026-09-26 FREEZE, AS THE SAMPLER SEES IT NOW. The last root entered
    /// was `NewEvents`; it returned; the main thread then spun in
    /// CoreFoundation's timer catch-up, on-CPU, for minutes. The heartbeat rule
    /// already fired at the threshold; the line must now say the thread is
    /// OUTSIDE aterm and spinning, not "inside `NewEvents`".
    #[test]
    fn the_2026_09_26_spin_is_named_as_outside_aterm_and_on_cpu() {
        let _serial = beat_serial();
        {
            let _root = enter(Breadcrumb::NewEvents);
            assert!(!RETURNED.load(Ordering::Relaxed), "inside the handler");
        }
        assert!(RETURNED.load(Ordering::Relaxed), "the handler returned");
        assert_eq!(current(), Breadcrumb::NewEvents);

        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 9, RELEASE_STALL_THRESHOLD);
        let mut hit = None;
        for tick in 1..=4u32 {
            // Spinning: all of every 2.5 s interval on-CPU.
            let at = t0 + Duration::from_millis(2_500) * tick;
            let cpu = Duration::from_millis(2_500) * tick;
            hit = hit.or(s.poll_with(at, 9, Breadcrumb::NewEvents, Some(cpu)));
        }
        let hit = hit.expect("a frozen work root past the bar is a stall");
        assert!(hit.busy, "on-CPU across the samples: a spin, not a park");
        let line = stall_message(
            1,
            hit.frozen,
            hit.root,
            Phase::None,
            Locus::Returned,
            hit.busy,
        );
        assert!(
            line.starts_with(
                "MAIN-THREAD STALL: no heartbeat for 5s since `NewEvents` returned, ON-CPU"
            ),
            "{line}"
        );
        assert!(line.contains("outside every aterm handler"), "{line}");
        assert!(!line.contains("while inside"), "{line}");
        let again = stall_message(
            2,
            Duration::from_secs(65),
            hit.root,
            Phase::None,
            Locus::Returned,
            true,
        );
        assert!(
            again.starts_with("MAIN-THREAD STALL CONTINUES: still no heartbeat after 65s since `NewEvents` returned"),
            "{again}"
        );
        // A modal restores the OUTER root as still running.
        {
            let _root = enter(Breadcrumb::UserEvent);
            drop(park_modal());
            assert!(
                !RETURNED.load(Ordering::Relaxed),
                "back inside the outer root"
            );
        }
    }

    /// A spin at the IDLE park point is a stall; a quiet park is not, however
    /// long; and the designed freezes that do real work (startup, a modal, the
    /// handoff) stay exempt even on-CPU.
    #[test]
    fn a_spin_at_the_idle_park_is_a_stall_and_a_quiet_park_is_not() {
        let t0 = Instant::now();
        let step = Duration::from_millis(250);
        let run = |bc: Breadcrumb, busy: bool| {
            let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
            let mut hits = Vec::new();
            for tick in 1..=12u32 {
                let cpu = if busy {
                    step * tick
                } else {
                    Duration::from_millis(1) * tick
                };
                if let Some(hit) = s.poll_with(t0 + step * tick, 3, bc, Some(cpu)) {
                    hits.push((tick, hit));
                }
            }
            hits
        };
        let spun = run(Breadcrumb::AboutToWait, true);
        assert_eq!(spun.len(), 1, "one report per contiguous spin: {spun:?}");
        let (tick, hit) = spun[0];
        assert!(hit.busy && hit.root == Breadcrumb::AboutToWait);
        assert!(hit.frozen >= STALL_THRESHOLD, "{hit:?}");
        assert_eq!(
            tick, 3,
            "busy from the first interval, the bar crossed at the third sample"
        );
        assert!(
            run(Breadcrumb::AboutToWait, false).is_empty(),
            "idle is not a stall"
        );
        for designed in [
            Breadcrumb::Startup,
            Breadcrumb::Modal,
            Breadcrumb::UpdateHandoff,
        ] {
            assert!(
                run(designed, true).is_empty(),
                "{designed:?} does real work by design"
            );
        }
        // With no CPU reading at all, the park point keeps its old exemption.
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        for tick in 1..=12u32 {
            assert!(
                s.poll_with(t0 + step * tick, 3, Breadcrumb::AboutToWait, None)
                    .is_none()
            );
        }
    }

    /// The trigger gets a line, and is not charged to the main thread: a
    /// 26.6 h gap in the sampler's own sleep reads as the process not running,
    /// ordinary scheduling jitter does not, and after a resync a work root
    /// needs a full threshold of its own before it is a stall.
    #[test]
    fn a_process_gap_is_named_and_restarts_the_stall_clock() {
        let sample = RELEASE_STALL_THRESHOLD / 2;
        assert_eq!(
            process_gap(sample, sample + Duration::from_secs(95_674)),
            Some(Duration::from_secs(95_674))
        );
        assert_eq!(process_gap(sample, sample + Duration::from_secs(3)), None);
        assert_eq!(process_gap(sample, Duration::ZERO), None);
        let line = gap_message(Duration::from_secs(95_674), Wall::Awake, Duration::ZERO);
        assert!(
            line.starts_with("this process was not running for 95674s while the system was awake"),
            "{line}"
        );

        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
        let back = t0 + Duration::from_secs(95_676);
        s.resync(back, 5, Duration::from_secs(95_674));
        assert!(
            s.poll_with(
                back + Duration::from_secs(1),
                5,
                Breadcrumb::UserEvent,
                None
            )
            .is_none(),
            "the gap is not a stall of the main thread"
        );
        assert!(
            s.poll_with(
                back + Duration::from_secs(6),
                5,
                Breadcrumb::UserEvent,
                None
            )
            .is_some(),
            "a real wedge after the gap still reports"
        );
    }

    /// One wake of `s`, driven as the sampler thread drives it (no CPU reading),
    /// gone `slept` since the wake before it on both clocks (the Mac awake),
    /// with the lines the thread logs for it appended to `lines`: the notices,
    /// then the stall line.
    fn wake_logged(
        s: &mut Sampler,
        lines: &mut Vec<String>,
        now: Instant,
        slept: Duration,
        beat: u64,
        bc: Breadcrumb,
    ) -> SamplerWake {
        let elapsed = Elapsed {
            mono: slept,
            wall: Some(slept),
            pass: Duration::ZERO,
        };
        wake_logged_on(s, lines, now, elapsed, beat, bc)
    }

    /// [`wake_logged`] with the two clocks given apart.
    fn wake_logged_on(
        s: &mut Sampler,
        lines: &mut Vec<String>,
        now: Instant,
        elapsed: Elapsed,
        beat: u64,
        bc: Breadcrumb,
    ) -> SamplerWake {
        let wake = s.wake(now, s.threshold / 2, elapsed, beat, bc, None);
        lines.extend(wake.sleep_note());
        lines.extend(wake.notices());
        if let Some(hit) = wake.hit {
            lines.push(stall_message(
                s.reports,
                hit.frozen,
                hit.root,
                Phase::None,
                Locus::Inside,
                hit.busy,
            ));
        }
        wake
    }

    /// A gap of `late` awake time with the Mac awake throughout, as
    /// [`wake_logged`] produces one: all of it in the sleep.
    fn awake_gap(late: Duration) -> Option<Gap> {
        Some(Gap {
            unscheduled: Some(late),
            wall: Wall::Awake,
            pass: Duration::ZERO,
        })
    }

    /// THE 2026-09-28 GAP. A stall reported after `NewEvents` returned, and then
    /// the whole process not running for 12130 s in the report pass that
    /// followed — after the sleep had been timed. Timed from the wake before it,
    /// as the sampler thread now times it, the next wake is 12130 s late: the
    /// gap line is written, and the ENDED line gives the main thread its own
    /// 7.5 s. Timed from its sleep alone, as the thread used to time it, the
    /// same wake is on time and the ENDED line charges the main thread with all
    /// of it: the incident's line read 12138 s. The Mac asleep in the same
    /// place is noted at info and changes nothing: the monotonic clock the
    /// accounting runs on stopped.
    #[test]
    fn the_2026_09_28_gap_inside_a_report_pass_is_not_charged_to_the_main_thread() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let pass = Duration::from_secs(12_130);
        let t0 = Instant::now();
        let reported = |lines: &mut Vec<String>| {
            let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
            let w = wake_logged(&mut s, lines, t0 + step, step, 5, Breadcrumb::NewEvents);
            assert_eq!(w.hit, None, "under the bar");
            let w = wake_logged(&mut s, lines, t0 + step * 2, step, 5, Breadcrumb::NewEvents);
            assert_eq!(w.hit.map(|h| h.root), Some(Breadcrumb::NewEvents));
            s
        };
        // The wake after the frozen pass and one more sleep; the main thread
        // beat in the meantime.
        let back = t0 + step * 3 + pass;

        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged_on(
            &mut s,
            &mut lines,
            back,
            Elapsed {
                mono: pass + step,
                wall: Some(pass + step),
                pass,
            },
            6,
            Breadcrumb::AboutToWait,
        );
        assert_eq!(
            w.gap,
            Some(Gap {
                unscheduled: Some(pass),
                wall: Wall::Awake,
                pass,
            })
        );
        assert_eq!(
            w.ended.map(|e| e.frozen),
            Some(step * 3),
            "the main thread's own 7.5 s, not the pass's 12130 s"
        );
        let gap_line = lines
            .iter()
            .find(|l| l.starts_with("this process was not running for 12130s"))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(
            gap_line.contains(" 12130s of it fell in the watchdog's own report pass"),
            "the line says where the gap fell: {gap_line}"
        );
        assert!(
            gap_line.contains("so the Mac did not sleep through it"),
            "{gap_line}"
        );
        assert!(
            lines
                .last()
                .is_some_and(|l| l.starts_with("MAIN-THREAD STALL ENDED:")),
            "{lines:#?}"
        );
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "{lines:#?}"
        );

        // The negative control: the same wake measured from its sleep alone.
        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged(&mut s, &mut lines, back, step, 6, Breadcrumb::AboutToWait);
        assert_eq!(w.gap, None, "a gap outside the timed sleep is invisible");
        assert_eq!(
            w.ended.map(|e| e.frozen),
            Some(step * 3 + pass),
            "and the main thread is charged with all of it"
        );

        // The Mac asleep in the same place: nothing to take out.
        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let before = lines.len();
        let w = wake_logged_on(
            &mut s,
            &mut lines,
            t0 + step * 3,
            Elapsed {
                mono: step,
                wall: Some(step + pass),
                pass: Duration::ZERO,
            },
            6,
            Breadcrumb::AboutToWait,
        );
        assert_eq!(
            w.gap,
            Some(Gap {
                unscheduled: None,
                wall: Wall::Slept(pass),
                pass: Duration::ZERO,
            })
        );
        assert_eq!(w.ended.map(|e| e.frozen), Some(step * 3));
        assert!(
            lines[before].starts_with("the Mac slept for about 12130s"),
            "{lines:#?}"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("this process was not running")),
            "a sleep is not a gap of awake time: {lines:#?}"
        );
    }

    /// Both clocks, read apart: lateness on the monotonic clock is awake time
    /// the process did not run, the wall clock's lead over it is the Mac
    /// asleep, and each line says which, fits its level's cap, and never reads
    /// as a stall to the recovery census.
    #[test]
    fn a_gap_is_read_on_both_clocks() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let hour = Duration::from_secs(3_600);
        let lost = Duration::from_secs(100);
        let on = |mono: Duration, wall: Option<Duration>| {
            classify_gap(
                step,
                Elapsed {
                    mono,
                    wall,
                    pass: Duration::ZERO,
                },
            )
        };
        let gap = |unscheduled: Option<Duration>, wall: Wall| {
            Some(Gap {
                unscheduled,
                wall,
                pass: Duration::ZERO,
            })
        };
        // Not scheduled while awake: the two clocks ran on together.
        assert_eq!(
            on(step + hour, Some(step + hour)),
            gap(Some(hour), Wall::Awake)
        );
        // Asleep: only the wall clock ran on.
        assert_eq!(on(step, Some(step + hour)), gap(None, Wall::Slept(hour)));
        // Both, one after the other.
        assert_eq!(
            on(step + lost, Some(step + lost + hour)),
            gap(Some(lost), Wall::Slept(hour))
        );
        // Scheduling jitter and clock slew are neither.
        let jitter = Duration::from_secs(3);
        assert_eq!(on(step + jitter, Some(step + jitter * 4)), None);
        assert_eq!(
            on(step, Some(step + PROCESS_GAP - Duration::from_millis(1))),
            None
        );
        // A clock set back says nothing, and hides nothing the monotonic clock saw;
        // a wall clock BEHIND the monotonic one is no sleep.
        assert_eq!(on(step, None), None);
        assert_eq!(on(step + hour, None), gap(Some(hour), Wall::Unknown));
        assert_eq!(on(step + hour, Some(step)), gap(Some(hour), Wall::Awake));

        // What the sampler thread feeds it: both spans from one pair of
        // readings, and the report pass up to the start of the sleep.
        let t0 = Instant::now();
        let w0 = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let reported_for = Duration::from_millis(2);
        let earlier = Clocks { mono: t0, wall: w0 };
        let later = Clocks {
            mono: t0 + reported_for + step,
            wall: w0 + reported_for + step + hour,
        };
        assert_eq!(
            later.since(earlier, t0 + reported_for),
            Elapsed {
                mono: reported_for + step,
                wall: Some(reported_for + step + hour),
                pass: reported_for,
            }
        );
        let set_back = Clocks {
            mono: t0 + step,
            wall: w0 - hour,
        };
        assert_eq!(
            set_back.since(earlier, t0),
            Elapsed {
                mono: step,
                wall: None,
                pass: Duration::ZERO,
            }
        );

        // A pass of milliseconds is not named; one that holds the gap is.
        let awake = gap_message(hour, Wall::Awake, Duration::from_millis(3));
        assert!(
            awake.contains("it was not scheduled (App Nap or CPU starvation"),
            "{awake}"
        );
        assert!(
            awake.contains("so the Mac did not sleep through it"),
            "{awake}"
        );
        assert!(!awake.contains("report pass"), "{awake}");
        let in_pass = gap_message(hour, Wall::Awake, hour);
        assert!(
            in_pass.contains(
                "suspended. 3600s of it fell in the watchdog's own report pass (the stall \
                 lines and the stack capture), where a log write stuck on the disk would read \
                 the same. The wall clock ran no further"
            ),
            "{in_pass}"
        );
        let both = gap_message(lost, Wall::Slept(hour), Duration::ZERO);
        assert!(
            both.starts_with("this process was not running for 100s while the system was awake"),
            "{both}"
        );
        assert!(
            both.contains("ran another 3600s past that, so the Mac also slept"),
            "{both}"
        );
        let unknown = gap_message(hour, Wall::Unknown, Duration::ZERO);
        assert!(
            unknown.contains("cannot say whether the Mac also slept"),
            "{unknown}"
        );
        let asleep = slept_message(hour);
        assert!(
            asleep.starts_with("the Mac slept for about 3600s"),
            "{asleep}"
        );
        // The widest figures a line can carry.
        let huge = Duration::from_secs(u64::MAX);
        for line in [
            gap_message(huge, Wall::Slept(huge), huge),
            gap_message(huge, Wall::Awake, huge),
            gap_message(huge, Wall::Unknown, huge),
        ] {
            assert!(
                line.len() <= aterm_log::record_cap(aterm_log::Level::Warn),
                "{} bytes: {line}",
                line.len()
            );
            assert!(!line.contains("MAIN-THREAD STALL"), "{line}");
        }
        let asleep = slept_message(huge);
        assert!(
            asleep.len() <= aterm_log::record_cap(aterm_log::Level::Info),
            "{} bytes: {asleep}",
            asleep.len()
        );
        assert!(!asleep.contains("MAIN-THREAD STALL"), "{asleep}");
    }

    /// THE 2026-09-28 STACK, WHOLE. Written as one line, a deep stack runs past
    /// the logger's 1 KiB ERROR body cap and is cut short — that day at frame
    /// #11. In parts, every frame reaches the log in order, every part passes
    /// the logger untouched, and every part is numbered and says when the stack
    /// was captured. (A synthetic stack: 64 frames sized so one line is cut
    /// where that day's was.)
    #[test]
    fn the_stack_is_logged_whole_in_numbered_parts_under_the_error_cap() {
        let cap = aterm_log::record_cap(aterm_log::Level::Error);
        let mut frames =
            vec!["load address 0x1004c8000, UUID 4C4C4453-5555-3144-A1B2-C3D4E5F60718".to_string()];
        frames.extend((0..64).map(|i| {
            format!(
                "#{i} AppKit + {} (-[NSSceneStatusItem _setupScene:] + {}) \
                 [a synthetic AppKit frame]",
                1_000_000 + 4_096 * i,
                60 + i
            )
        }));
        let captured = UNIX_EPOCH + Duration::from_millis(1_790_191_963_664);

        // The negative control: the one line the watchdog used to write keeps
        // frame #10 and loses #11 and everything deeper, as that day's did.
        let old = format!("{STACK_LINE} {}", frames.join(FRAME_SEPARATOR));
        let logged = aterm_log::sanitize_record_for(aterm_log::Level::Error, &old);
        assert!(
            old.contains("| #11 ") && logged.contains("| #10 ") && !logged.contains("| #11 "),
            "{logged}"
        );

        let lines = stack_lines(&frames, captured, cap);
        let parts = lines.len();
        assert!(parts > 1, "{lines:#?}");
        let mut rejoined: Vec<String> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let head = format!(
                "MAIN-THREAD STALL stack: (part {}/{parts}, captured at 1790191963.664) ",
                i + 1
            );
            let body = line
                .strip_prefix(head.as_str())
                .unwrap_or_else(|| panic!("numbered and stamped: {line}"));
            assert!(line.len() <= cap, "{} bytes: {line}", line.len());
            assert_eq!(
                aterm_log::sanitize_record_for(aterm_log::Level::Error, line),
                line.as_str(),
                "the logger passes every part untouched"
            );
            rejoined.extend(body.split(FRAME_SEPARATOR).map(str::to_string));
        }
        assert_eq!(rejoined, frames, "every frame, in order, none split");
    }

    /// A frame too long for a line of its own is clipped, on a character
    /// boundary, into a part of its own; control characters are counted at
    /// the width the logger writes them (U+FFFD), so no part is cut by it;
    /// no frames make no lines; and the stamp is the log's own form.
    #[test]
    fn an_oversized_frame_is_clipped_into_a_part_of_its_own() {
        let cap = aterm_log::record_cap(aterm_log::Level::Error);
        let captured = UNIX_EPOCH + Duration::from_millis(1_790_191_963_664);
        let frames = [
            "#0 libsystem_kernel.dylib + 4660 (mach_msg2_trap + 8)".to_string(),
            format!("#1 {}", "é".repeat(1_100)),
            format!("#2 {}", "\u{1}".repeat(600)),
            "#3 dyld + 24680 (start + 2360)".to_string(),
        ];
        let lines = stack_lines(&frames, captured, cap);
        assert_eq!(lines.len(), 4, "{lines:#?}");
        for line in &lines {
            assert!(
                logged_len(line) <= cap,
                "{} bytes: {line}",
                logged_len(line)
            );
            assert_eq!(
                aterm_log::sanitize_record_for(aterm_log::Level::Error, line).len(),
                logged_len(line),
                "the logger replaces control characters and cuts nothing: {line}"
            );
        }
        assert!(lines[0].ends_with(") #0 libsystem_kernel.dylib + 4660 (mach_msg2_trap + 8)"));
        assert!(
            lines[1].contains("(part 2/4, ") && lines[1].ends_with("é…"),
            "{}",
            lines[1]
        );
        assert!(lines[2].ends_with("\u{1}…"), "{}", lines[2]);
        assert!(lines[3].ends_with(") #3 dyld + 24680 (start + 2360)"));
        assert!(stack_lines(&[], captured, cap).is_empty());

        assert_eq!(epoch_stamp(captured), "1790191963.664");
        assert_eq!(epoch_stamp(UNIX_EPOCH + Duration::from_millis(5)), "0.005");
        assert!(epoch_stamp(UNIX_EPOCH - Duration::from_secs(1)).starts_with("an unknown time"));
    }

    /// THE PORT (keeper P0 onto the 2026-09-26 gap rule). A stall REPORTED
    /// before a process gap is still open after the resync, so its end is still
    /// said — and the recovery census, folding the lines the sampler thread
    /// logs, reads the run as having recovered, not as "stalled at its end".
    /// Both ways it can end: after the gap, when the heartbeat moves; and
    /// inside it, when the main thread beat before the sampler woke. The time
    /// the ENDED line gives is the main thread's own, never the gap's.
    #[test]
    fn a_stall_reported_before_a_process_gap_is_still_ended_after_it() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let gap = Duration::from_secs(95_674);
        let t0 = Instant::now();
        // Up to a reported stall inside `UserEvent`, heartbeat frozen at 5.
        let reported = |lines: &mut Vec<String>| {
            let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
            let w = wake_logged(&mut s, lines, t0 + step, step, 5, Breadcrumb::UserEvent);
            assert_eq!(w.hit, None, "under the bar");
            let w = wake_logged(&mut s, lines, t0 + step * 2, step, 5, Breadcrumb::UserEvent);
            assert_eq!(w.hit.map(|h| h.root), Some(Breadcrumb::UserEvent));
            s
        };
        let back = t0 + step * 3 + gap;

        // 1. Still frozen when the process runs again: the gap ends nothing, and
        //    the stall ends when the heartbeat moves.
        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged(
            &mut s,
            &mut lines,
            back,
            step + gap,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.gap, w.ended, w.hit), (awake_gap(gap), None, None));
        let w = wake_logged(
            &mut s,
            &mut lines,
            back + step,
            step,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.ended, w.hit), (None, None), "under the bar again");
        let w = wake_logged(
            &mut s,
            &mut lines,
            back + step * 2,
            step,
            6,
            Breadcrumb::AboutToWait,
        );
        assert_eq!(
            w.ended,
            Some(Ended {
                // Frozen 7.5 s before the wake that found the gap (the sleep it
                // asked for counts) and 5 s after it; the 26.6 h do not.
                frozen: step * 5,
                root: Breadcrumb::UserEvent,
                by: EndedBy::Beat,
            }),
            "the stall reported before the gap ends after it"
        );
        assert!(
            lines
                .last()
                .is_some_and(|l| l.starts_with("MAIN-THREAD STALL ENDED:")),
            "{lines:#?}"
        );
        assert_eq!(s.take_ended(), None, "said once");
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "a stall the run survived is not its last word: {lines:#?}"
        );

        // 2. The heartbeat moved while the process was stopped (the main thread
        //    ran first on resume): the wake that finds the gap ends the stall.
        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged(
            &mut s,
            &mut lines,
            back,
            step + gap,
            6,
            Breadcrumb::AboutToWait,
        );
        assert_eq!(w.gap, awake_gap(gap));
        assert_eq!(
            w.ended,
            Some(Ended {
                frozen: step * 3,
                root: Breadcrumb::UserEvent,
                by: EndedBy::Beat,
            })
        );
        assert_eq!(w.notices().len(), 2, "the gap line, then the ENDED line");
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "{lines:#?}"
        );
        assert_eq!(
            wake_logged(
                &mut s,
                &mut lines,
                back + step,
                step,
                7,
                Breadcrumb::UserEvent
            )
            .ended,
            None,
            "ended once, not again at the next beat"
        );

        // And the control: a stall that has NOT ended across the gap is still
        // the run's last word, as it must be.
        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        wake_logged(
            &mut s,
            &mut lines,
            back,
            step + gap,
            5,
            Breadcrumb::UserEvent,
        );
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::Yes,
            "{lines:#?}"
        );
    }

    /// A process gap never INVENTS a stall: a freeze under the bar before the
    /// gap is not reported across it, nothing that was not reported is ended
    /// after it, a stall that already ended is not ended twice, and the census
    /// reads the run as unstalled.
    #[test]
    fn a_process_gap_does_not_invent_a_stall() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let gap = Duration::from_secs(95_674);
        let t0 = Instant::now();
        let mut lines = Vec::new();
        let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
        // Frozen 2.5 s inside `UserEvent` — under the bar — when the process stops.
        let w = wake_logged(
            &mut s,
            &mut lines,
            t0 + step,
            step,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!(w.hit, None);
        let back = t0 + step * 2 + gap;
        let w = wake_logged(
            &mut s,
            &mut lines,
            back,
            step + gap,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.gap, w.ended, w.hit), (awake_gap(gap), None, None));
        // Still frozen after it, but only for 2.5 s of the main thread's own time.
        let w = wake_logged(
            &mut s,
            &mut lines,
            back + step,
            step,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!(
            (w.ended, w.hit),
            (None, None),
            "the gap is not charged to the main thread"
        );
        // It beats: nothing was reported, so nothing ends.
        let w = wake_logged(
            &mut s,
            &mut lines,
            back + step * 2,
            step,
            6,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.ended, w.hit), (None, None));
        assert_eq!(lines.len(), 1, "the gap line alone: {lines:#?}");
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "{lines:#?}"
        );

        // A stall that was reported AND ended before the gap is not ended again
        // by it, whether or not the heartbeat moves across it.
        for beat_after in [8, 9] {
            let mut lines = Vec::new();
            let mut s = Sampler::with_threshold(t0, 7, RELEASE_STALL_THRESHOLD);
            wake_logged(
                &mut s,
                &mut lines,
                t0 + step * 2,
                step,
                7,
                Breadcrumb::UserEvent,
            );
            let w = wake_logged(
                &mut s,
                &mut lines,
                t0 + step * 3,
                step,
                8,
                Breadcrumb::UserEvent,
            );
            assert!(w.ended.is_some());
            let w = wake_logged(
                &mut s,
                &mut lines,
                t0 + step * 4 + gap,
                step + gap,
                beat_after,
                Breadcrumb::UserEvent,
            );
            assert_eq!((w.ended, w.hit), (None, None), "beat {beat_after}");
            #[cfg(unix)]
            assert_eq!(
                crate::recovery_census::stall_at_end(&lines),
                aterm_update::recovery_ledger::Tri::No,
                "{lines:#?}"
            );
        }
    }

    /// The ported hand-off on the spin rule: a spin REPORTED at the idle park
    /// has ended once a CPU reading finds the main thread quiet again — its
    /// heartbeat never moves, so waiting for one would leave a run that went
    /// idle and was later killed reading as "stalled at its end". With no
    /// reading to go by it stays open until the heartbeat moves.
    #[test]
    fn a_spin_reported_at_the_idle_park_ends_when_a_reading_finds_it_quiet() {
        let t0 = Instant::now();
        let step = Duration::from_millis(250);
        let spin_to_report = |s: &mut Sampler| {
            let mut hit = None;
            for tick in 1..=3u32 {
                hit = hit.or(s.poll_with(
                    t0 + step * tick,
                    3,
                    Breadcrumb::AboutToWait,
                    Some(step * tick),
                ));
            }
            assert!(hit.is_some_and(|h| h.busy), "{hit:?}");
            assert_eq!(s.take_ended(), None);
        };
        // Quiet: 1 ms of CPU across the next interval.
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        spin_to_report(&mut s);
        let cpu = step * 3 + Duration::from_millis(1);
        assert_eq!(
            s.poll_with(t0 + step * 4, 3, Breadcrumb::AboutToWait, Some(cpu)),
            None
        );
        assert_eq!(
            s.take_ended(),
            Some(Ended {
                // On-CPU from the first reading's interval, quiet at the fourth.
                frozen: step * 3,
                root: Breadcrumb::AboutToWait,
                by: EndedBy::Quiet,
            })
        );
        assert!(
            s.poll_with(t0 + step * 5, 4, Breadcrumb::NewEvents, Some(cpu))
                .is_none()
                && s.take_ended().is_none(),
            "ended once: the next heartbeat ends nothing"
        );
        // No reading: still open, however long, until the heartbeat moves.
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        spin_to_report(&mut s);
        for tick in 4..=40u32 {
            assert_eq!(
                s.poll_with(t0 + step * tick, 3, Breadcrumb::AboutToWait, None),
                None
            );
            assert_eq!(s.take_ended(), None, "no reading is no evidence it ended");
        }
        s.poll_with(t0 + step * 41, 4, Breadcrumb::NewEvents, None);
        assert_eq!(
            s.take_ended().map(|e| (e.root, e.by)),
            Some((Breadcrumb::AboutToWait, EndedBy::Beat))
        );
    }

    /// The quiet rule ends only a SPIN reported at a park point. A stall
    /// reported at a WORK root while the main thread was parked on a lock
    /// (quiet readings, `busy` false) can meet a park point under an unmoved
    /// heartbeat only when a sample reads the next root's breadcrumb before
    /// its beat — two relaxed atomics, the breadcrumb stamped first — and that
    /// beat is what ends it. Ended as "quiet", its ENDED line would say the
    /// thread "stopped spinning … on-CPU" when it never was.
    #[test]
    fn a_work_root_stall_ends_by_its_beat_never_as_a_quiet_spin() {
        let t0 = Instant::now();
        let step = Duration::from_millis(250);
        let quiet = |tick: u32| Some(Duration::from_millis(tick.into()));
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        let mut hit = None;
        for tick in 1..=3u32 {
            hit = hit.or(s.poll_with(t0 + step * tick, 3, Breadcrumb::UserEvent, quiet(tick)));
        }
        assert!(
            hit.is_some_and(|h| h.root == Breadcrumb::UserEvent && !h.busy),
            "a park on a lock inside `UserEvent`, reported: {hit:?}"
        );
        // `AboutToWait` stamped, its heartbeat not yet read: nothing has ended.
        assert_eq!(
            s.poll_with(t0 + step * 4, 3, Breadcrumb::AboutToWait, quiet(4)),
            None
        );
        assert_eq!(s.take_ended(), None, "not a spin that went quiet");
        // The beat lands: the stall ended by it, once.
        assert_eq!(
            s.poll_with(t0 + step * 5, 4, Breadcrumb::AboutToWait, quiet(5)),
            None
        );
        assert_eq!(
            s.take_ended(),
            Some(Ended {
                frozen: step * 5,
                root: Breadcrumb::UserEvent,
                by: EndedBy::Beat,
            })
        );
        assert_eq!(s.take_ended(), None);
    }

    // ---- The refusal of a verb the main thread cannot take ----

    /// A process-clock reading long after launch, where a stall happens.
    const REFUSAL_T0: u64 = 40_000_000_000;

    /// One tick of the derived `MainThreadStallRefusal` machine: half the
    /// refusal bar, so the model's `Bar = 2` ticks is [`MAIN_STALL_BAR`].
    const REFUSAL_TICK_NS: u64 = 2_500_000_000;

    fn secs(n: u64) -> u64 {
        n * 1_000_000_000
    }

    /// THE 2026-09-28 INCIDENT, as the refusal reads it: `NewEvents` returned
    /// and the main thread went into AppKit's scene setup and did not come
    /// back. Past the bar every main-thread verb is refused, and the reason
    /// names the root and says its handler had returned; under the bar it is
    /// a slow turn and the verb is posted.
    #[test]
    fn a_work_root_with_no_beat_for_the_bar_is_refused_and_says_where() {
        let beat = REFUSAL_T0;
        assert_eq!(
            main_stall(beat + secs(4), beat, Breadcrumb::NewEvents, true, None),
            None,
            "under the bar is a slow turn, not a stall"
        );
        let stall = main_stall(beat + secs(12_138), beat, Breadcrumb::NewEvents, true, None)
            .expect("a work root frozen past the bar is stalled");
        assert_eq!(stall.stalled, Duration::from_secs(12_138));
        assert_eq!(
            stall.to_string(),
            "main thread stalled 12138s since `NewEvents` returned; retry"
        );
        let inside = main_stall(beat + secs(7), beat, Breadcrumb::UserEvent, false, None)
            .expect("a handler still on the stack is stalled too");
        assert_eq!(
            inside.to_string(),
            "main thread stalled 7s since `UserEvent`; retry"
        );
    }

    /// The idle park is no stall by itself: a thread asleep in its event wait
    /// has an old heartbeat by design. Nor is a hop that was only just posted
    /// to it, which is what a verb arriving while the first verb after ten
    /// idle minutes is answered sees. A hop that has waited the bar with no
    /// beat since it was posted IS a stall: the thread left its wait for
    /// something that is not aterm's and did not come back. The negative
    /// control judges on the heartbeat alone and refuses the fresh hop.
    #[test]
    fn the_idle_park_is_refused_only_once_a_hop_has_waited_the_bar() {
        let beat = REFUSAL_T0;
        let idle = beat + secs(600);
        let park = Breadcrumb::AboutToWait;
        assert_eq!(main_stall(idle, beat, park, true, None), None, "idle");
        assert_eq!(
            main_stall(idle, beat, park, true, Some(idle)),
            None,
            "a hop just posted"
        );
        assert_eq!(
            main_stall(idle + secs(4), beat, park, true, Some(idle)),
            None
        );
        let stall = main_stall(idle + secs(5), beat, park, true, Some(idle))
            .expect("a hop that waited the bar on an unmoved heartbeat");
        assert_eq!(
            stall.stalled,
            Duration::from_secs(5),
            "counted from the hop, not from the idle before it"
        );
        assert_eq!(
            stall.to_string(),
            "main thread stalled 5s since `AboutToWait` returned; retry"
        );
        // A beat after the hop was posted restarts the count from that beat.
        assert_eq!(
            main_stall(idle + secs(6), idle + secs(2), park, true, Some(idle)),
            None
        );
        // The heartbeat-only reading: the fresh hop is refused.
        assert!(main_stall(idle, beat, park, true, Some(1)).is_some());
    }

    /// The designed freezes never read as stalled, however long and whatever
    /// waits on them: a dialog has its own refusal, and startup and the update
    /// handoff end by themselves. Nor does a main thread that has not beaten.
    #[test]
    fn a_designed_freeze_or_a_thread_that_never_beat_is_never_refused() {
        let beat = REFUSAL_T0;
        let late = beat + secs(3_600);
        for root in [
            Breadcrumb::Modal,
            Breadcrumb::UpdateHandoff,
            Breadcrumb::Startup,
        ] {
            assert_eq!(
                main_stall(late, beat, root, false, Some(beat)),
                None,
                "{root:?}"
            );
        }
        assert_eq!(
            main_stall(late, 0, Breadcrumb::UserEvent, false, Some(beat)),
            None,
            "no beat yet is no evidence"
        );
    }

    /// ONE BAR in every build: the shipped 5 s, never the debug lane's 500 ms,
    /// so a debug build's slow-but-live turn is still waited for.
    #[test]
    fn the_refusal_bar_is_the_shipped_stall_bar_in_every_build() {
        assert_eq!(MAIN_STALL_BAR, Duration::from_secs(5));
        assert_eq!(MAIN_STALL_BAR, RELEASE_STALL_THRESHOLD);
        let beat = REFUSAL_T0;
        assert_eq!(
            main_stall(beat + secs(1), beat, Breadcrumb::ResizeSettle, false, None),
            None,
            "a debug-lane stall line, never a refusal"
        );
    }

    /// The real writer, reset, hop count and reader, driven beside the derived
    /// `MainThreadStallRefusal` machine: [`beat_into`] on this test's own
    /// ledger (the writer), [`TurnLedger::reset`] (what `metrics reset` runs),
    /// `control_media::Hops` (what `call_main` counts), `control_media::take_hop`
    /// (what `user_event` runs first on a posted hop) and [`main_stall`] (the
    /// reader). One model tick is [`REFUSAL_TICK_NS`] of the clock.
    struct RefusalLockstep<'a> {
        ledger: &'a TurnLedger,
        phase: &'a AtomicU8,
        /// `'static`, as the process's own is, so a real `Wake::Hop` can
        /// carry its mark.
        hops: &'static crate::control::control_media::Hops,
        /// The worker's side of the posted hop, alive while it waits.
        hop: Option<crate::control::control_media::HopGuard<'static>>,
        /// When `hop` was posted.
        posted: u64,
        now: u64,
        root: Breadcrumb,
        returned: bool,
        /// The census defect: the reader takes the turn census's open stamp,
        /// which `reset` zeroes, for the heartbeat's time.
        census: bool,
        /// The heartbeat-only defect: a waiting hop is judged as if it had
        /// waited forever, so only the heartbeat's age counts.
        beat_only: bool,
        /// The count-until-reply defect: a hop counts from its post for as
        /// long as its worker waits, taken by the main thread or not.
        until_reply: bool,
    }

    impl RefusalLockstep<'_> {
        fn enter_root(&mut self, root: Breadcrumb, returned: bool) {
            beat_into(self.ledger, self.phase, root, self.now);
            self.root = root;
            self.returned = returned;
        }

        /// The main thread takes the posted hop: the real `take_hop` on the
        /// `Wake::Hop` that `call_main` posts, which hands back the request.
        fn take(&self) {
            let hop = self.hop.as_ref().expect("a posted hop to take");
            let request = crate::control::control_media::take_hop(crate::Wake::Hop {
                mark: hop.mark(),
                wake: Box::new(crate::Wake::TitleSummaryReady),
            });
            assert!(
                matches!(request, crate::Wake::TitleSummaryReady),
                "{request:?}"
            );
        }

        /// The stamp the reader under test takes for the heartbeat's time.
        fn stamp(&self) -> u64 {
            if self.census {
                self.ledger.open_ns.load(Ordering::Relaxed)
            } else {
                self.ledger.last_beat_ns.load(Ordering::Relaxed)
            }
        }

        /// The real reader's verdict.
        fn refuses(&self) -> bool {
            let since = if self.until_reply {
                self.hop.as_ref().map(|_| self.posted)
            } else {
                self.hops.since_ns()
            };
            let since = since.map(|since| if self.beat_only { 1 } else { since });
            main_stall(self.now, self.stamp(), self.root, self.returned, since).is_some()
        }

        /// Take the model's `action` on the real code; `Some` is the reader's
        /// verdict when the action is `Probe`.
        fn step(&mut self, action: &str) -> Option<bool> {
            match action {
                "BeatIdle" => self.enter_root(Breadcrumb::AboutToWait, true),
                "BeatWork" => self.enter_root(Breadcrumb::NewEvents, true),
                "BeatPark" => self.enter_root(Breadcrumb::Modal, false),
                "Tick" => self.now += REFUSAL_TICK_NS,
                "Post" => {
                    let hops: &'static crate::control::control_media::Hops = self.hops;
                    self.hop = Some(hops.post(self.now));
                    self.posted = self.now;
                }
                // The main thread takes the hop, which is a root entry, and
                // answers it in the same turn: the worker stops waiting.
                "Answer" => {
                    self.enter_root(Breadcrumb::UserEvent, false);
                    self.take();
                    self.hop = None;
                }
                // The main thread takes the hop and queues its reply (a
                // `settings set` write): the worker goes on waiting.
                "Take" => {
                    self.enter_root(Breadcrumb::UserEvent, false);
                    self.take();
                }
                // The queued reply comes, from a later turn.
                "Reply" => {
                    self.enter_root(Breadcrumb::UserEvent, false);
                    self.hop = None;
                }
                // The worker's deadline passes and it stops waiting.
                "GiveUp" => self.hop = None,
                "Reset" => self.ledger.reset(),
                "Probe" => return Some(self.refuses()),
                other => panic!("no real step for model action `{other}`"),
            }
            None
        }
    }

    /// One schedule taken on `model` and on the real code in lockstep, with
    /// `mutation` (a `Buggy = 1` defect) fired on the model first and replayed
    /// on the real reader. After every action the worker's live guard projects
    /// onto `waiting`, the hop count onto `hop` and the reader's stamp onto
    /// `stamp`; at every `Probe` the real verdict must be the model's
    /// `refused`. Returns `(refused, stalled, real verdict)` per probe. The
    /// caller holds [`beat_serial`].
    fn refusal_lockstep(
        model: &aterm_spec::derive::Model,
        mutation: Option<&'static str>,
        schedule: &[&'static str],
    ) -> Vec<(i64, i64, bool)> {
        let ledger = TurnLedger::new();
        let phase = AtomicU8::new(Phase::None as u8);
        // Leaked, one small count per schedule, for the `'static` a real
        // `Wake::Hop` needs.
        let hops: &'static crate::control::control_media::Hops =
            Box::leak(Box::new(crate::control::control_media::Hops::new()));
        let mut real = RefusalLockstep {
            ledger: &ledger,
            phase: &phase,
            hops,
            hop: None,
            posted: 0,
            now: REFUSAL_T0,
            root: Breadcrumb::Startup,
            returned: false,
            census: mutation == Some("MutateCensusStamp"),
            beat_only: mutation == Some("MutateHeartbeatOnly"),
            until_reply: mutation == Some("MutateCountsUntilReply"),
        };
        let mut state = model.init_state();
        if let Some(mutation) = mutation {
            assert!(model.fire(mutation, &mut state), "{mutation}");
        }
        let mut probes = Vec::new();
        for &action in schedule {
            assert!(
                model.fire(action, &mut state),
                "{action} is not admitted at {state:?} ({schedule:?})"
            );
            let verdict = real.step(action);
            assert_eq!(
                state["waiting"],
                i64::from(real.hop.is_some()),
                "{action} in {schedule:?}: the worker's live guard projects onto `waiting`"
            );
            assert_eq!(
                state["hop"],
                i64::from(hops.since_ns().is_some()),
                "{action} in {schedule:?}: the hop count projects onto `hop`"
            );
            assert_eq!(
                state["stamp"],
                i64::from(real.stamp() != 0),
                "{action} in {schedule:?}: the reader's stamp projects onto `stamp`"
            );
            if let Some(refuses) = verdict {
                assert_eq!(
                    state["refused"],
                    i64::from(refuses),
                    "{action} in {schedule:?}: the real reader and the model disagree"
                );
                probes.push((state["refused"], state["stalled"], refuses));
            }
        }
        probes
    }

    /// TIER-1 for `MainThreadStallRefusal`. Each schedule is taken on the
    /// model and on the genuine writer, reset, hop count, take and reader in
    /// lockstep ([`refusal_lockstep`]), and at every probe the model proves
    /// the refusal was exactly the stall (`refused == stalled`). The three
    /// defects its `Buggy` replays are replayed on the real reader too and
    /// each is caught: a `metrics reset` under a stall read through the census
    /// stamp lets the stalled thread's verbs through, a heartbeat-only rule
    /// refuses a fresh hop after idleness, and a hop counted until its reply
    /// makes a healthy idle thread behind a queued `settings set` read as
    /// stuck.
    #[test]
    fn main_stall_conforms_to_the_stall_refusal_model() {
        let model = aterm_spec::derive::main_thread_stall_refusal_model();
        assert!(model.consts.contains(&("Bar", 2)), "two ticks are the bar");
        assert_eq!(Duration::from_nanos(2 * REFUSAL_TICK_NS), MAIN_STALL_BAR);

        let _serial = beat_serial();
        let schedules: &[&[&'static str]] = &[
            // The incident: a work root, then no beat.
            &[
                "BeatWork", "Probe", "Tick", "Probe", "Tick", "Probe", "Post", "Tick", "Probe",
            ],
            // Idle past the bar, then a hop: let through until it has waited.
            &[
                "BeatIdle", "Tick", "Tick", "Tick", "Post", "Probe", "Tick", "Probe", "Tick",
                "Probe",
            ],
            // The hop is answered, and the loop parks again.
            &[
                "BeatIdle", "Tick", "Tick", "Post", "Tick", "Answer", "Probe", "BeatIdle", "Tick",
                "Tick", "Probe",
            ],
            // A dialog stands over a waiting hop: never this refusal.
            &[
                "BeatWork", "BeatPark", "Post", "Tick", "Tick", "Tick", "Probe",
            ],
            // `metrics reset` under a stall: still refused.
            &[
                "BeatWork", "Tick", "Reset", "Tick", "Probe", "Reset", "Probe",
            ],
            // A hop given up on stops counting.
            &[
                "BeatIdle", "Post", "Tick", "Tick", "Probe", "GiveUp", "Probe",
            ],
            // A loop that turns while the hop waits restarts the count.
            &[
                "BeatIdle", "Post", "Tick", "BeatIdle", "Tick", "Probe", "Tick", "Probe",
            ],
            // No beat yet, however long: nothing is refused.
            &["Tick", "Tick", "Post", "Tick", "Tick", "Probe"],
            // A hop taken with its reply queued (`settings set`): the loop
            // parks while the worker still waits, and nothing waits on the
            // main thread, however long the write takes.
            &[
                "BeatIdle", "Post", "Take", "BeatIdle", "Tick", "Tick", "Probe", "Tick", "Probe",
                "Reply", "Probe",
            ],
            // Taken, then stuck in the handler that took it: a work root.
            &["BeatIdle", "Post", "Take", "Tick", "Tick", "Probe"],
        ];
        let mut refused = 0;
        for schedule in schedules {
            for (model_refused, stalled, _) in refusal_lockstep(&model, None, schedule) {
                assert_eq!(model_refused, stalled, "{schedule:?}");
                refused += model_refused;
            }
        }
        assert!(refused > 0, "the schedules reach a refusal");

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let reset_under_stall: &[&'static str] = &["BeatWork", "Tick", "Reset", "Tick", "Probe"];
        assert_eq!(
            refusal_lockstep(&model, None, reset_under_stall),
            [(1, 1, true)]
        );
        assert_eq!(
            refusal_lockstep(&buggy, Some("MutateCensusStamp"), reset_under_stall),
            [(0, 1, false)],
            "read through the census stamp, a reset lets a stalled thread's verbs through"
        );
        let fresh_hop: &[&'static str] = &["BeatIdle", "Tick", "Tick", "Tick", "Post", "Probe"];
        assert_eq!(refusal_lockstep(&model, None, fresh_hop), [(0, 0, false)]);
        assert_eq!(
            refusal_lockstep(&buggy, Some("MutateHeartbeatOnly"), fresh_hop),
            [(1, 0, true)],
            "judged on the heartbeat alone, a fresh hop after idleness is refused"
        );
        let queued_reply: &[&'static str] = &[
            "BeatIdle", "Post", "Take", "BeatIdle", "Tick", "Tick", "Probe",
        ];
        assert_eq!(
            refusal_lockstep(&model, None, queued_reply),
            [(0, 0, false)]
        );
        assert_eq!(
            refusal_lockstep(&buggy, Some("MutateCountsUntilReply"), queued_reply),
            [(1, 0, true)],
            "counted until its reply, a queued settings write makes a healthy idle thread \
             read as stuck"
        );

        // Leave the global breadcrumb where the rest of the suite expects it.
        beat(Breadcrumb::AboutToWait);
    }

    /// `metrics reset` clears the turn census and leaves the heartbeat's
    /// timestamp: the one fact the refusal must not lose to it.
    #[test]
    fn a_census_reset_keeps_the_heartbeat_timestamp() {
        let _serial = beat_serial();
        let ledger = TurnLedger::new();
        let phase = AtomicU8::new(Phase::None as u8);
        beat_into(&ledger, &phase, Breadcrumb::UserEvent, REFUSAL_T0);
        assert_eq!(ledger.open_ns.load(Ordering::Relaxed), REFUSAL_T0);
        assert_eq!(ledger.last_beat_ns.load(Ordering::Relaxed), REFUSAL_T0);
        ledger.reset();
        assert_eq!(ledger.open_ns.load(Ordering::Relaxed), 0);
        assert_eq!(ledger.last_beat_ns.load(Ordering::Relaxed), REFUSAL_T0);
        beat(Breadcrumb::AboutToWait);
    }
}
