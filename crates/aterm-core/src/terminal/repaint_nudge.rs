// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The repaint nudge: repairs a full-screen program that did not repaint a screen a resize
//! left displaced. A pure, clockless state machine; the host (`aterm-gui`'s `app_nudge`)
//! stamps `now_ms`, samples the [`Facts`], and does the PTY writes the machine asks for.
//!
//! A program that repaints only when its SIGWINCH handler reads a changed size misses a flap
//! (it reads the same size twice), and every later frame lands one row off. So the machine
//! holds the PTY one row short, waits for the program's answer (a full clear), releases, and
//! waits for the second answer, so the handler reads each size. Measured: back to back (the
//! adoption pulse) 0/25, on a timer 9/25, closed loop (release after [`SETTLE_MS`] of quiet)
//! 25/25. No input byte is sent, so a draft, a paste and an approval box are untouched.
//!
//! The acts are about the PTY only; the engine is never resized. It runs only for the agent
//! class (Claude Code, the one program whose repaint contract was measured; an editor with
//! point on the last row re-scrolls 38 of 41 rows), and never while a halt stands, a driver
//! may type, a person touched the session within [`PERSON_GATE_MS`], text is selected, or a
//! DEC 2026 frame is open. A missing answer is a bounded wait, a release, then
//! [`GiveUp::NoAnswer`]: one try per epoch, at most [`MAX_PER_HOUR`] attempts per
//! session-hour, [`COOLDOWN_MS`] apart.
//!
//! A spent wait is judged by what the engine still owes ([`Facts::owed`]), not by answers
//! counted: rows owed is the give-up the owner hears, none is [`Dropped::NothingOwed`]. An
//! adoption owes nothing, so a size it left unanswered is a give-up (a log line only). A hold
//! someone else ends ([`Act::Forget`]) or the host lets go ([`RepaintNudge::let_go`]) waits
//! for the answer as after a release; only a resize that left the engine elsewhere is
//! [`Dropped::Superseded`]. Every attempt that held ends in a judged outcome.
//!
//! The model is `RepaintNudge` in `aterm-spec` (`models_resize_render.rs`); the Tier-1 bind is
//! `tests/conformance_repaint_nudge.rs`.

use std::collections::VecDeque;

/// Output quiet a nudge waits for before it starts.
pub const QUIET_WAIT_MS: u64 = 150;
/// The longest that quiet is waited for; then it goes unless a frame is open.
pub const QUIET_MAX_MS: u64 = 3_000;
/// After the last open frame's end, how much longer an open DEC 2026 frame is
/// waited for before the attempt is abandoned.
pub const FRAME_WAIT_CAP_MS: u64 = 3_000;
/// Output quiet after an answer before the hold is released: the program's
/// repaint is over, so a release never lands mid-frame.
pub const SETTLE_MS: u64 = 40;
/// How long each of the two answers is waited for.
pub const ANSWER_TIMEOUT_MS: u64 = 1_500;
/// The gap between two attempts on one session.
pub const COOLDOWN_MS: u64 = 60_000;
/// The window `MAX_PER_HOUR` is counted in.
pub const HOUR_MS: u64 = 3_600_000;
/// At most this many attempts per session per [`HOUR_MS`].
pub const MAX_PER_HOUR: usize = 3;
/// A person's gesture on the session postpones a nudge this long.
pub const PERSON_GATE_MS: u64 = 1_500;
/// A standing selection (the repaint clears it) defers a nudge this long.
pub const SELECTION_DEFER_CAP_MS: u64 = 30_000;
/// A lease held on the session defers a nudge this long.
pub const LEASE_DEFER_CAP_MS: u64 = 120_000;
/// How often a gate that ends without a wake is looked at again.
pub const POLL_MS: u64 = 250;
/// The hard deadline of a PTY hold, whoever holds it: two answer timeouts plus
/// the settle. Enforced by the host, outside this machine.
pub const PTY_HOLD_CAP_MS: u64 = 2 * ANSWER_TIMEOUT_MS + 1_000;

/// `[render] repaint_nudge`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Nothing runs.
    Off,
    /// Everything runs but the two PTY writes; what would have been done is
    /// logged. Also what an unreadable `[render] repaint_nudge` word means.
    Log,
    /// The repair acts (the render-integrity design, section 8).
    #[default]
    On,
}

impl Mode {
    /// Parse the config word, in any case: the config analyzer accepts `"OFF"`,
    /// so a parse that refused it would read an opt-out as a word it cannot read.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        [Self::Off, Self::Log, Self::On]
            .into_iter()
            .find(|mode| word.eq_ignore_ascii_case(mode.word()))
    }

    /// The config word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Log => "log",
            Self::On => "on",
        }
    }
}

/// Where the machine is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing owed.
    Idle,
    /// An epoch is at risk; waiting for a moment the gates allow.
    Armed,
    /// The PTY is held one row short; waiting for the program's answer.
    Short,
    /// The PTY is back at the engine's size; waiting for the second answer.
    Restoring,
    /// Finished (healed, given up, dropped); the outcome is on record.
    Rest,
}

impl Phase {
    /// The wire word (`resizes` `nudge=`).
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Armed => "armed",
            Self::Short => "short",
            Self::Restoring => "restoring",
            Self::Rest => "rest",
        }
    }
}

/// Why the machine was armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The engine owes rows and the epoch judge reads `desync-risk`.
    Debt,
    /// A repaint adoption left the engine blank on purpose and the program
    /// gave no answer to the adoption pulse.
    Adoption,
}

impl Reason {
    /// The log word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Debt => "debt",
            Self::Adoption => "adoption",
        }
    }
}

/// A gate that defers for a bounded time, then abandons the attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// A driver may type into the session.
    Lease,
    /// Text is selected.
    Selection,
    /// A DEC 2026 frame stayed open.
    Busy,
}

impl Gate {
    const fn slot(self) -> usize {
        match self {
            Self::Lease => 0,
            Self::Selection => 1,
            Self::Busy => 2,
        }
    }

    /// The log word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Lease => "lease",
            Self::Selection => "selection",
            Self::Busy => "busy",
        }
    }
}

/// An attempt that ended with the tab probably still displaced: the owner's
/// one action (drag the window edge) is worth an item in the digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiveUp {
    /// The program did not answer one of the two sizes and the engine still owes rows when
    /// the wait ends; for an adoption, which owes none, either size unanswered.
    NoAnswer,
    /// A gate deferred the attempt to its cap.
    Deferred(Gate),
    /// Three attempts in the hour are spent.
    Budget,
}

impl GiveUp {
    /// The log word.
    #[must_use]
    pub fn word(self) -> String {
        match self {
            Self::NoAnswer => "no-answer".to_string(),
            Self::Deferred(gate) => format!("deferred-{}", gate.word()),
            Self::Budget => "budget".to_string(),
        }
    }
}

/// An attempt that ended because there was nothing left to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// Mode `off`.
    Off,
    /// The program is not the agent class.
    NotAgent,
    /// The alternate screen was left.
    LeftAltScreen,
    /// The engine owes nothing any more: the program repainted by itself
    /// before the hold, or by the time a debt attempt's wait was spent
    /// (whatever answers were counted).
    NothingOwed,
    /// A real resize left the engine away from where the hold found it, and the program had
    /// not answered when the wait ended: it was given a size it reads as changed, so what it
    /// draws is the epoch judge's. A flap that put the engine back is not this; nor is a
    /// resize that comes back after the wait (the epoch judge reports that give-up).
    Superseded,
    /// The second size was answered without the first: repainted anyway.
    Repainted,
    /// The engine is too small to hold one row short.
    TooSmall,
}

/// What the host must do after a [`RepaintNudge::step`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Nothing, and nothing is scheduled: the wait ends with an event the host
    /// sees anyway (a halt lifting, a handoff finishing, a drag ending, a view
    /// closing) and its next tick re-evaluates.
    Nothing,
    /// Nothing now; call [`RepaintNudge::step`] again at this time (host ms).
    WaitUntil(u64),
    /// Set the PTY to `engine.rows - 1` × `engine.cols` now.
    Hold,
    /// Set the PTY to the engine's size now. The phase says whether the
    /// machine is now waiting for the second answer or is finished.
    Release,
    /// Someone else set the PTY while it was held (the layout passes of a flap or of a
    /// real resize, a cell-box sync): the hold is over and the PTY is theirs, so write
    /// nothing. The machine waits for the program's answer as after its own release.
    Forget,
    /// Mode `log`: the attempt would have started here. Finished.
    WouldNudge,
    /// Both answers arrived. Finished. Milliseconds from the hold to the
    /// first answer and from the release to the second.
    Healed {
        /// Hold to first answer.
        first_ms: u64,
        /// Release to second answer.
        second_ms: u64,
    },
    /// Finished without a repair the owner should not have to hear about
    /// (something else took care of it, or it never applied).
    Dropped(Dropped),
    /// Finished without a repair the owner has one action for.
    GaveUp(GiveUp),
}

/// What the host reads from the world at each step, every field about the target session
/// (not its window, not the machine).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a snapshot of independent gate facts: each bool is one gate the machine reads, and a bitset would hide the names"
)]
pub struct Facts {
    /// `[render] repaint_nudge`.
    pub mode: Mode,
    /// The presence classifier reads Claude Code on this session.
    pub agent: bool,
    /// The engine shows the alternate screen.
    pub alt_screen: bool,
    /// The engine's render debt is not `(0, 0)`.
    pub owed: bool,
    /// More than one view shows the session.
    pub shared: bool,
    /// A seamless update is freezing the rows.
    pub frozen: bool,
    /// A live drag of the hosting window.
    pub dragging: bool,
    /// A halt (`hold=1`) stands on the session.
    pub halted: bool,
    /// A lease or turn on this session may still type.
    pub driver_may_type: bool,
    /// Another session's PTY is held right now (one hold at a time across sessions).
    pub other_hold: bool,
    /// Milliseconds since a person last touched this session, or since it attached when
    /// never touched ([`person_quiet_ms`]).
    pub person_quiet_ms: u64,
    /// Text is selected.
    pub selection: bool,
    /// Milliseconds since the last PTY output.
    pub output_quiet_ms: u64,
    /// A DEC 2026 synchronized-output frame is open.
    pub frame_open: bool,
    /// `Terminal::full_clear_count`.
    pub full_clears: u64,
    /// `Terminal::screen_replaced_count`.
    pub screen_replaced: u64,
    /// The engine's `(rows, cols)`.
    pub engine: (u16, u16),
    /// The PTY's `(rows, cols)` as `TIOCGWINSZ` reads it.
    pub pty: (u16, u16),
}

impl Facts {
    /// Facts of a quiet, healthy, eligible session at `size` (a test and rig
    /// starting point; the host builds its own).
    #[must_use]
    pub const fn eligible(mode: Mode, size: (u16, u16)) -> Self {
        Self {
            mode,
            agent: true,
            alt_screen: true,
            owed: true,
            shared: false,
            frozen: false,
            dragging: false,
            halted: false,
            driver_may_type: false,
            other_hold: false,
            person_quiet_ms: u64::MAX,
            selection: false,
            output_quiet_ms: u64::MAX,
            frame_open: false,
            full_clears: 0,
            screen_replaced: 0,
            engine: size,
            pty: size,
        }
    }
}

/// The person gate's input: milliseconds since a person's last gesture on the session, or,
/// when none reached it in this process (the stamp is process-local, so a session adopted
/// across an update has none), since it attached. "Never" is unknown, not quiet.
#[must_use]
pub const fn person_quiet_ms(since_gesture_ms: Option<u64>, since_attach_ms: u64) -> u64 {
    match since_gesture_ms {
        Some(ms) => ms,
        None => since_attach_ms,
    }
}

/// The machine. One per session, kept for the session's life; it is re-armed
/// per epoch and remembers the attempts it made.
#[derive(Clone, Debug)]
pub struct RepaintNudge {
    phase: Phase,
    reason: Reason,
    epoch: u64,
    /// The epoch of the last attempt started or logged: one try per epoch.
    last_epoch: Option<u64>,
    /// When each gate first deferred this attempt.
    deferred_since: [Option<u64>; 3],
    /// When the quiet wait began.
    quiet_since: Option<u64>,
    /// Attempts started with mode `on`, newest last.
    attempts: VecDeque<u64>,
    nudged: u32,
    hold_ms: u64,
    hold_engine: (u16, u16),
    base_clears: u64,
    base_replaced: u64,
    release_ms: u64,
    first_answer_ms: Option<u64>,
    second_answer_ms: Option<u64>,
    /// The host let the hold go with no step that read the world
    /// ([`Self::let_go`]): the counters are read at the next step.
    rebase: bool,
    /// A handoff freeze held the wait for the answer: it starts again at the
    /// first step after the freeze.
    thaw: bool,
}

impl Default for RepaintNudge {
    fn default() -> Self {
        Self::new()
    }
}

impl RepaintNudge {
    /// A machine at rest with no history.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            reason: Reason::Debt,
            epoch: 0,
            last_epoch: None,
            deferred_since: [None; 3],
            quiet_since: None,
            attempts: VecDeque::new(),
            nudged: 0,
            hold_ms: 0,
            hold_engine: (0, 0),
            base_clears: 0,
            base_replaced: 0,
            release_ms: 0,
            first_answer_ms: None,
            second_answer_ms: None,
            rebase: false,
            thaw: false,
        }
    }

    /// Where the machine is.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Why the current or last attempt was armed.
    #[must_use]
    pub const fn reason(&self) -> Reason {
        self.reason
    }

    /// The epoch of the current or last attempt.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// How many attempts have actually held a PTY (`nudged=`).
    #[must_use]
    pub const fn nudged(&self) -> u32 {
        self.nudged
    }

    /// Milliseconds from the hold to the first answer, once seen.
    #[must_use]
    pub const fn first_answer_ms(&self) -> Option<u64> {
        self.first_answer_ms
    }

    /// Milliseconds from the release to the second answer, once seen.
    #[must_use]
    pub const fn second_answer_ms(&self) -> Option<u64> {
        self.second_answer_ms
    }

    /// When the hold began (host ms); meaningful while [`Phase::Short`].
    #[must_use]
    pub const fn hold_started_ms(&self) -> u64 {
        self.hold_ms
    }

    /// The engine size the hold was taken against.
    #[must_use]
    pub const fn hold_engine(&self) -> (u16, u16) {
        self.hold_engine
    }

    /// Arm for `epoch` (`u64::MAX` for an adoption). `false` when the machine will not start:
    /// mode `off`, an attempt in flight, or this epoch already tried or logged (new debt is a
    /// new epoch).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "Arm",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    pub fn arm(&mut self, mode: Mode, reason: Reason, epoch: u64, _now_ms: u64) -> bool {
        if mode == Mode::Off {
            return false;
        }
        if matches!(self.phase, Phase::Armed | Phase::Short | Phase::Restoring) {
            return false;
        }
        if self.last_epoch == Some(epoch) {
            return false;
        }
        self.phase = Phase::Armed;
        self.reason = reason;
        self.epoch = epoch;
        self.deferred_since = [None; 3];
        self.quiet_since = None;
        self.first_answer_ms = None;
        self.second_answer_ms = None;
        self.rebase = false;
        self.thaw = false;
        true
    }

    /// The host let the hold go (at [`PTY_HOLD_CAP_MS`] when no step could end it, or before
    /// a handoff freeze): the PTY is the engine's again and the attempt goes on as after a
    /// release (the model's `ReleaseByDeadline`). The world may not be readable now, so the
    /// counters are read at the next step. `false` when no hold was in flight.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "ReleaseByDeadline",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    pub fn let_go(&mut self, now_ms: u64) -> bool {
        if self.phase != Phase::Short {
            return false;
        }
        self.phase = Phase::Restoring;
        self.release_ms = now_ms;
        self.rebase = true;
        true
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Debt",
            reason = "The world's: rows come to be owed when a resize the program does not repaint for is booked. The machine reads it as `Facts::owed` (the engine's render debt), never causes it."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "AltLeft",
            reason = "The world's: the program leaves the alternate screen or repaints from scratch. The machine reads it as `Facts::alt_screen` / `Facts::owed` and stands down."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Quiet",
            reason = "The world's: the output goes quiet. The machine reads it as `Facts::output_quiet_ms` (`QUIET_WAIT_MS`), a clock it is handed."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Busy",
            reason = "The world's: output arrives or a DEC 2026 frame opens. The machine reads it as `Facts::output_quiet_ms` / `Facts::frame_open`."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "App1",
            reason = "The program's: it answers the short size with a full clear. The machine reads it as `Facts::full_clears` / `Facts::screen_replaced` advancing past the counts at the hold (`answered`)."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "App2",
            reason = "The program's: it answers the true size. The machine reads the same counters after the release."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Flap",
            reason = "The window's: a flap's layout passes (or a cell-box sync) set the PTY while it is held and leave the engine where the hold found it. The machine reads it as `Facts::pty` no longer the short size it set."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Resize",
            reason = "The window's: a real resize moves the engine, and its layout pass the PTY. The machine reads it as `Facts::engine` against the size the hold was taken at."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Return",
            reason = "The window's: a real resize that superseded an attempt did not last, and the engine is back where the hold found it. The machine is at rest by then; the epoch judge (`aterm_gui::resize_epoch`) reads it as the epoch back at `desync-risk` at its start geometry."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "ReturnUnwatched",
            reason = "A mutant of the `Buggy = 1` bundle: the shipped bug of 2026-10-03, nothing watched a superseded attempt's epoch, so a resize that came back left the tab displaced with its one try spent and nothing said. The epoch judge owes that end now, and the window's Tier-1 (`aterm_gui::nudge_conformance`) holds it to a give-up."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "DropOnFlap",
            reason = "A mutant of the `Buggy = 1` bundle: the shipped bug of 2026-10-02, any write by someone else read as a real resize and the attempt dropped at once. `forget` waits for the answer instead, and the conformance's busy stand-in holds it to a give-up."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Halt",
            reason = "The fabric's: a standing hold (`hold=1`) begins. The machine reads it as `Facts::halted`."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Lift",
            reason = "The fabric's: the standing hold ends. The machine reads it as `Facts::halted` and re-evaluates."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Tick",
            reason = "Time passing: the machine is clockless, and reads the host's `now_ms` at every `step` (the answer timeouts, the quiet wait and the deferral caps are comparisons against it)."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "DriverVanishes",
            reason = "The host's record of the attempt is lost while the PTY is short (a panic in the tick, a teardown race). Nothing in this machine can do it; the host's `PtyGuard` drop and the app-level held map (`aterm_gui::app_nudge`) are what the model's ReleaseByDeadline binds."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "Age",
            reason = "Time passing while the PTY is short: the host compares `now_ms` with the hold's start against `PTY_HOLD_CAP_MS` (`aterm_gui::app_nudge::NudgeHost::tick`)."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "LoseTheHold",
            reason = "A mutant of the `Buggy = 1` bundle: an exit path that forgets to lift the hold. No shipping code takes it; the Tier-1 (`conformance_repaint_nudge`) and the host tests hold that every exit releases."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "HealOnFirstAnswer",
            reason = "A mutant of the `Buggy = 1` bundle: healed on the first answer. No shipping code takes it; `Act::Healed` needs both answers."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "HoldTheEngine",
            reason = "A mutant of the `Buggy = 1` bundle: the hold resizes the ENGINE. The machine has no act that asks for it; the Tier-1 asserts the engine is untouched."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "NoCap",
            reason = "A mutant of the `Buggy = 1` bundle: no hard deadline on a hold. The host enforces `PTY_HOLD_CAP_MS`."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RepaintNudge",
            action = "RetryForever",
            reason = "A mutant of the `Buggy = 1` bundle: retried without a budget. The machine tries once per epoch and three times an hour."
        )
    )]
    /// One evaluation. Idempotent for a moment that changes nothing.
    pub fn step(&mut self, f: &Facts, now_ms: u64) -> Act {
        match self.phase {
            Phase::Idle | Phase::Rest => Act::Nothing,
            Phase::Armed => self.step_armed(f, now_ms),
            Phase::Short => self.step_short(f, now_ms),
            Phase::Restoring => self.step_restoring(f, now_ms),
        }
    }

    fn finish(&mut self, act: Act) -> Act {
        self.phase = Phase::Rest;
        self.last_epoch = Some(self.epoch);
        act
    }

    fn drop_with(&mut self, why: Dropped) -> Act {
        // A drop about the world (not the program) leaves the epoch untried: a later,
        // eligible moment may arm it again.
        self.phase = Phase::Rest;
        if matches!(why, Dropped::NothingOwed | Dropped::Repainted) {
            self.last_epoch = Some(self.epoch);
        }
        Act::Dropped(why)
    }

    /// Gate `gate` defers this attempt at `now_ms`: the give-up when its cap is spent, else
    /// the wait to schedule.
    fn defer(&mut self, gate: Gate, cap_ms: u64, now_ms: u64) -> Act {
        let since = *self.deferred_since[gate.slot()].get_or_insert(now_ms);
        self.quiet_since = None;
        if now_ms.saturating_sub(since) >= cap_ms {
            return self.finish(Act::GaveUp(GiveUp::Deferred(gate)));
        }
        Act::WaitUntil(
            now_ms
                .saturating_add(POLL_MS)
                .min(since.saturating_add(cap_ms)),
        )
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "Hold",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    fn step_armed(&mut self, f: &Facts, now_ms: u64) -> Act {
        if f.mode == Mode::Off {
            return self.drop_with(Dropped::Off);
        }
        if !f.agent {
            return self.drop_with(Dropped::NotAgent);
        }
        if !f.alt_screen {
            return self.drop_with(Dropped::LeftAltScreen);
        }
        if self.reason == Reason::Debt && !f.owed {
            return self.drop_with(Dropped::NothingOwed);
        }
        if f.engine.0 < 2 || f.engine.1 == 0 {
            return self.drop_with(Dropped::TooSmall);
        }
        // Gates that end with an event the host sees anyway: no timer, no cap.
        if f.halted || f.frozen || f.dragging || f.shared {
            self.quiet_since = None;
            return Act::Nothing;
        }
        if f.mode == Mode::On {
            self.prune(now_ms);
            if self.attempts.len() >= MAX_PER_HOUR {
                return self.finish(Act::GaveUp(GiveUp::Budget));
            }
            if let Some(last) = self.attempts.back() {
                let ready = last.saturating_add(COOLDOWN_MS);
                if ready > now_ms {
                    self.quiet_since = None;
                    return Act::WaitUntil(ready);
                }
            }
        }
        if f.driver_may_type {
            return self.defer(Gate::Lease, LEASE_DEFER_CAP_MS, now_ms);
        }
        self.deferred_since[Gate::Lease.slot()] = None;
        if f.person_quiet_ms < PERSON_GATE_MS {
            self.quiet_since = None;
            return Act::WaitUntil(now_ms.saturating_add(PERSON_GATE_MS - f.person_quiet_ms));
        }
        if f.selection {
            return self.defer(Gate::Selection, SELECTION_DEFER_CAP_MS, now_ms);
        }
        self.deferred_since[Gate::Selection.slot()] = None;
        if f.other_hold {
            self.quiet_since = None;
            return Act::WaitUntil(now_ms.saturating_add(POLL_MS));
        }
        // The moment: output quiet, no frame open.
        let began = *self.quiet_since.get_or_insert(now_ms);
        let waited = now_ms.saturating_sub(began);
        if f.frame_open {
            if waited >= QUIET_MAX_MS + FRAME_WAIT_CAP_MS {
                return self.finish(Act::GaveUp(GiveUp::Deferred(Gate::Busy)));
            }
            return Act::WaitUntil(now_ms.saturating_add(50));
        }
        if f.output_quiet_ms < QUIET_WAIT_MS && waited < QUIET_MAX_MS {
            let more = QUIET_WAIT_MS - f.output_quiet_ms;
            return Act::WaitUntil(now_ms.saturating_add(more.min(50)));
        }
        // Go.
        if f.mode == Mode::Log {
            return self.finish(Act::WouldNudge);
        }
        self.attempts.push_back(now_ms);
        self.nudged = self.nudged.saturating_add(1);
        self.phase = Phase::Short;
        self.last_epoch = Some(self.epoch);
        self.hold_ms = now_ms;
        self.hold_engine = f.engine;
        self.base_clears = f.full_clears;
        self.base_replaced = f.screen_replaced;
        self.first_answer_ms = None;
        self.second_answer_ms = None;
        Act::Hold
    }

    fn prune(&mut self, now_ms: u64) {
        while self
            .attempts
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= HOUR_MS)
        {
            self.attempts.pop_front();
        }
    }

    fn answered(&self, f: &Facts) -> bool {
        f.full_clears > self.base_clears || f.screen_replaced > self.base_replaced
    }

    fn settled(f: &Facts) -> bool {
        !f.frame_open && f.output_quiet_ms >= SETTLE_MS
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "Release",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    fn release(&mut self, f: &Facts, now_ms: u64, phase: Phase) -> Act {
        self.phase = phase;
        self.release_ms = now_ms;
        self.base_clears = f.full_clears;
        self.base_replaced = f.screen_replaced;
        Act::Release
    }

    /// Someone else set the PTY while it was held: the hold is theirs, and the attempt goes
    /// on as after a release. A real resize and a flap cannot be told apart yet (a step may
    /// see a flap's middle), so [`Self::step_restoring`] judges it when the wait ends.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "Forget",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    fn forget(&mut self, f: &Facts, now_ms: u64) -> Act {
        if self.answered(f) {
            // Whatever was drawn by now answered the short size; only a later answer
            // counts for the size that stands.
            self.first_answer_ms = Some(now_ms.saturating_sub(self.hold_ms));
        }
        self.phase = Phase::Restoring;
        self.release_ms = now_ms;
        self.base_clears = f.full_clears;
        self.base_replaced = f.screen_replaced;
        Act::Forget
    }

    fn step_short(&mut self, f: &Facts, now_ms: u64) -> Act {
        let target = (self.hold_engine.0.saturating_sub(1), self.hold_engine.1);
        // Someone else wrote the PTY: a resize's layout pass (real or a flap's) or a
        // cell-box sync.
        if f.engine != self.hold_engine || f.pty != target {
            return self.forget(f, now_ms);
        }
        // The alternate screen left: nothing to repair, so lift the hold and stand down.
        if !f.alt_screen {
            return self.release(f, now_ms, Phase::Rest);
        }
        let waited = now_ms.saturating_sub(self.hold_ms);
        // A halt or a handoff freeze lifts the hold at once; the attempt (its try spent)
        // is still judged by the program's answers, as after any release.
        if f.halted || f.frozen {
            if self.answered(f) {
                self.first_answer_ms = Some(waited);
            }
            return self.release(f, now_ms, Phase::Restoring);
        }
        if self.answered(f) {
            if Self::settled(f) || waited >= ANSWER_TIMEOUT_MS {
                self.first_answer_ms = Some(waited);
                return self.release(f, now_ms, Phase::Restoring);
            }
            let more = SETTLE_MS.saturating_sub(f.output_quiet_ms).max(10);
            return Act::WaitUntil(now_ms.saturating_add(more));
        }
        if waited >= ANSWER_TIMEOUT_MS {
            return self.release(f, now_ms, Phase::Restoring);
        }
        Act::WaitUntil(self.hold_ms.saturating_add(ANSWER_TIMEOUT_MS))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "Finish",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "GiveUp",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "Supersede",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    fn step_restoring(&mut self, f: &Facts, now_ms: u64) -> Act {
        if f.frozen {
            // A handoff freeze parks the readers, so no answer can be seen: the wait
            // restarts at the first step after it lifts.
            self.thaw = true;
            return Act::Nothing;
        }
        if std::mem::take(&mut self.thaw) {
            self.release_ms = now_ms;
        }
        if std::mem::take(&mut self.rebase) {
            // After `let_go`: what the program drew by now answered the short size.
            if self.answered(f) {
                self.first_answer_ms = Some(now_ms.saturating_sub(self.hold_ms));
            }
            self.base_clears = f.full_clears;
            self.base_replaced = f.screen_replaced;
        }
        let waited = now_ms.saturating_sub(self.release_ms);
        if self.answered(f) {
            // Answered. A program that streams through its repaint (never quiet, or a
            // frame open at every look) answered all the same: the timeout ends the wait for
            // quiet, as in `step_short`.
            if Self::settled(f) || waited >= ANSWER_TIMEOUT_MS {
                self.second_answer_ms = Some(waited);
                return match self.first_answer_ms {
                    Some(first_ms) => self.finish(Act::Healed {
                        first_ms,
                        second_ms: waited,
                    }),
                    None => self.finish(Act::Dropped(Dropped::Repainted)),
                };
            }
            let more = SETTLE_MS.saturating_sub(f.output_quiet_ms).max(10);
            return Act::WaitUntil(now_ms.saturating_add(more));
        }
        if waited >= ANSWER_TIMEOUT_MS {
            // Unanswered. An engine moved by a real resize got a size of its own; otherwise
            // the end is judged by what the engine still owes, never by answers counted
            // (two before one step read as one). An adoption owes nothing: a give-up.
            if f.engine != self.hold_engine {
                return self.finish(Act::Dropped(Dropped::Superseded));
            }
            if self.reason == Reason::Debt && !f.owed {
                return self.finish(Act::Dropped(Dropped::NothingOwed));
            }
            return self.finish(Act::GaveUp(GiveUp::NoAnswer));
        }
        Act::WaitUntil(self.release_ms.saturating_add(ANSWER_TIMEOUT_MS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u16, u16) = (61, 200);
    const SHORT: (u16, u16) = (60, 200);

    /// A machine, a clock and a program that answers as told.
    struct Rig {
        m: RepaintNudge,
        f: Facts,
        now: u64,
    }

    impl Rig {
        fn new(mode: Mode) -> Self {
            let mut rig = Self {
                m: RepaintNudge::new(),
                f: Facts::eligible(mode, SIZE),
                now: 100_000,
            };
            assert!(rig.m.arm(mode, Reason::Debt, 1, rig.now));
            rig
        }

        fn step(&mut self) -> Act {
            self.m.step(&self.f, self.now)
        }

        /// Advance the clock and step.
        fn at(&mut self, ms: u64) -> Act {
            self.now += ms;
            self.step()
        }

        /// The program clears its screen (an answer) and goes quiet.
        fn answer(&mut self) {
            self.f.full_clears += 1;
        }

        fn hold(&mut self) {
            assert_eq!(self.step(), Act::Hold);
            self.f.pty = SHORT;
        }

        fn release(&mut self) {
            self.f.pty = SIZE;
        }
    }

    /// Each deferring gate (a driver lease, a standing selection, an open DEC
    /// 2026 frame, another tab's hold) keeps the machine armed while it stands
    /// and lets the hold go once it clears; the first three are abandoned with
    /// their own give-up rather than waited on for ever.
    #[test]
    fn a_standing_gate_defers_the_hold_and_a_capped_one_is_abandoned() {
        type Set = fn(&mut Facts, bool);
        let gates: [(&str, Set, Option<Gate>); 4] = [
            ("lease", |f, on| f.driver_may_type = on, Some(Gate::Lease)),
            ("selection", |f, on| f.selection = on, Some(Gate::Selection)),
            ("frame", |f, on| f.frame_open = on, Some(Gate::Busy)),
            ("other hold", |f, on| f.other_hold = on, None),
        ];
        for (name, set, gate) in gates {
            let mut r = Rig::new(Mode::On);
            set(&mut r.f, true);
            for _ in 0..20 {
                assert!(matches!(r.at(100), Act::WaitUntil(_)), "{name}");
            }
            assert_eq!(r.m.phase(), Phase::Armed, "{name}");
            set(&mut r.f, false);
            assert_eq!(r.at(1), Act::Hold, "{name}");
            let Some(gate) = gate else { continue };
            let mut r = Rig::new(Mode::On);
            set(&mut r.f, true);
            let mut last = Act::Nothing;
            for _ in 0..600 {
                last = r.at(250);
                if matches!(last, Act::GaveUp(_)) {
                    break;
                }
            }
            assert_eq!(last, Act::GaveUp(GiveUp::Deferred(gate)), "{name}");
            assert_eq!(r.m.phase(), Phase::Rest, "{name}");
        }
    }

    #[test]
    fn nudge_never_runs_within_1500ms_of_a_persons_key() {
        let mut r = Rig::new(Mode::On);
        r.f.person_quiet_ms = 100;
        assert_eq!(r.step(), Act::WaitUntil(r.now + PERSON_GATE_MS - 100));
        r.f.person_quiet_ms = PERSON_GATE_MS;
        assert_eq!(r.step(), Act::Hold);
    }

    #[test]
    fn a_session_never_stamped_waits_1500ms_from_attach() {
        assert_eq!(person_quiet_ms(None, 300), 300);
        assert_eq!(person_quiet_ms(Some(40), 9_000), 40);
        let mut r = Rig::new(Mode::On);
        r.f.person_quiet_ms = person_quiet_ms(None, 300);
        assert!(matches!(r.step(), Act::WaitUntil(_)));
        r.f.person_quiet_ms = person_quiet_ms(None, 1_500);
        assert_eq!(r.step(), Act::Hold);
    }

    #[test]
    fn nudge_stops_at_one_try_per_epoch_and_three_per_hour() {
        let mut r = Rig::new(Mode::On);
        for epoch in 1..=3u64 {
            if epoch > 1 {
                r.now += COOLDOWN_MS;
                assert!(r.m.arm(Mode::On, Reason::Debt, epoch, r.now));
            }
            r.hold();
            r.at(ANSWER_TIMEOUT_MS);
            r.release();
            r.at(ANSWER_TIMEOUT_MS);
            assert_eq!(r.m.phase(), Phase::Rest);
            // One try per epoch: the same epoch does not re-arm.
            assert!(!r.m.arm(Mode::On, Reason::Debt, epoch, r.now));
        }
        r.now += COOLDOWN_MS;
        assert!(r.m.arm(Mode::On, Reason::Debt, 4, r.now));
        assert_eq!(r.step(), Act::GaveUp(GiveUp::Budget));
        // An hour later the budget is back.
        r.now += HOUR_MS;
        assert!(r.m.arm(Mode::On, Reason::Debt, 5, r.now));
        assert_eq!(r.step(), Act::Hold);
    }

    #[test]
    fn the_cooldown_spaces_two_attempts_by_a_minute() {
        let mut r = Rig::new(Mode::On);
        r.hold();
        r.at(ANSWER_TIMEOUT_MS);
        r.release();
        r.at(ANSWER_TIMEOUT_MS);
        assert!(r.m.arm(Mode::On, Reason::Debt, 2, r.now));
        assert!(matches!(r.step(), Act::WaitUntil(t) if t > r.now));
    }

    #[test]
    fn nudge_skips_a_shared_session_and_a_dragged_window() {
        for shared in [true, false] {
            let mut r = Rig::new(Mode::On);
            r.f.shared = shared;
            r.f.dragging = !shared;
            assert_eq!(r.step(), Act::Nothing);
            assert_eq!(r.m.phase(), Phase::Armed);
        }
    }

    #[test]
    fn the_release_waits_for_40ms_of_quiet() {
        let mut r = Rig::new(Mode::On);
        r.hold();
        r.answer();
        r.f.output_quiet_ms = 10;
        assert_eq!(r.step(), Act::WaitUntil(r.now + 30));
        r.f.output_quiet_ms = 40;
        assert_eq!(r.step(), Act::Release);
        // Nor is a frame that carried the answer released mid-frame.
        let mut r = Rig::new(Mode::On);
        r.hold();
        r.answer();
        r.f.frame_open = true;
        assert!(matches!(r.step(), Act::WaitUntil(_)));
        r.f.frame_open = false;
        assert_eq!(r.step(), Act::Release);
    }

    #[test]
    fn a_program_that_is_not_the_agent_class_is_never_held_whatever_the_mode() {
        for mode in [Mode::On, Mode::Log] {
            let mut r = Rig::new(mode);
            r.f.agent = false;
            assert_eq!(r.step(), Act::Dropped(Dropped::NotAgent));
            assert_eq!(r.m.nudged(), 0);
        }
    }

    #[test]
    fn mode_log_writes_would_nudge_and_touches_no_pty() {
        let mut r = Rig::new(Mode::Log);
        assert_eq!(r.step(), Act::WouldNudge);
        assert_eq!(r.m.phase(), Phase::Rest);
        assert_eq!(r.m.nudged(), 0);
        // Once per epoch.
        assert!(!r.m.arm(Mode::Log, Reason::Debt, 1, r.now));
        assert!(r.m.arm(Mode::Log, Reason::Debt, 2, r.now));
    }

    #[test]
    fn mode_off_runs_nothing() {
        let mut m = RepaintNudge::new();
        assert!(!m.arm(Mode::Off, Reason::Debt, 1, 0));
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.step(&Facts::eligible(Mode::Off, SIZE), 0), Act::Nothing);
        // A mode turned off under an armed machine drops it.
        let mut r = Rig::new(Mode::On);
        r.f.mode = Mode::Off;
        assert_eq!(r.step(), Act::Dropped(Dropped::Off));
    }

    #[test]
    fn a_program_that_repainted_by_itself_is_not_nudged() {
        let mut r = Rig::new(Mode::On);
        r.f.owed = false;
        assert_eq!(r.step(), Act::Dropped(Dropped::NothingOwed));
        assert_eq!(r.m.nudged(), 0);
        // An adoption arm has no debt to wait on.
        let mut m = RepaintNudge::new();
        let mut f = Facts::eligible(Mode::On, SIZE);
        f.owed = false;
        assert!(m.arm(Mode::On, Reason::Adoption, u64::MAX, 0));
        assert_eq!(m.step(&f, 0), Act::Hold);
    }

    #[test]
    fn every_exit_path_lifts_the_hold() {
        // Alternate screen left.
        let mut r = Rig::new(Mode::On);
        r.hold();
        r.f.alt_screen = false;
        assert_eq!(r.at(1), Act::Release);
        // A handoff freeze.
        let mut r = Rig::new(Mode::On);
        r.hold();
        r.f.frozen = true;
        assert_eq!(r.at(1), Act::Release);
        // The host lets it go: the attempt waits for its answer as after a release.
        let mut r = Rig::new(Mode::On);
        r.hold();
        assert!(r.m.let_go(r.now));
        assert_eq!(r.m.phase(), Phase::Restoring);
        // An armed machine holds nothing to let go.
        let mut r = Rig::new(Mode::On);
        assert!(!r.m.let_go(r.now));
    }

    #[test]
    fn the_second_size_answered_without_the_first_is_a_repaint_not_a_heal() {
        let mut r = Rig::new(Mode::On);
        r.hold();
        r.at(ANSWER_TIMEOUT_MS);
        r.release();
        r.at(200);
        r.answer();
        assert_eq!(r.at(1), Act::Dropped(Dropped::Repainted));
    }

    #[test]
    fn a_first_answer_with_no_second_is_judged_by_what_the_engine_owes() {
        // The program answered the short size only. Rows still owed are a give-up, a paid
        // debt is not reported, and an adoption (owes nothing) is a give-up.
        for (reason, owed, end) in [
            (Reason::Debt, true, Act::GaveUp(GiveUp::NoAnswer)),
            (Reason::Debt, false, Act::Dropped(Dropped::NothingOwed)),
            (Reason::Adoption, false, Act::GaveUp(GiveUp::NoAnswer)),
        ] {
            let mut r = Rig::new(Mode::On);
            r.m = RepaintNudge::new();
            assert!(r.m.arm(Mode::On, reason, 1, r.now));
            r.hold();
            r.answer();
            assert_eq!(r.at(5), Act::Release);
            r.release();
            r.f.owed = owed;
            assert_eq!(r.at(ANSWER_TIMEOUT_MS), end, "{reason:?} owed={owed}");
        }
    }

    #[test]
    fn a_screen_replacement_is_an_answer_too() {
        let mut r = Rig::new(Mode::On);
        r.hold();
        r.f.screen_replaced += 1;
        assert_eq!(r.at(1), Act::Release);
    }

    #[test]
    fn the_quiet_wait_is_bounded() {
        let mut r = Rig::new(Mode::On);
        r.f.output_quiet_ms = 0;
        assert!(matches!(r.at(1), Act::WaitUntil(_)));
        // A program that never goes quiet still gets its repair once the wait
        // is spent (no frame open).
        assert_eq!(r.at(QUIET_MAX_MS), Act::Hold);
    }
}
