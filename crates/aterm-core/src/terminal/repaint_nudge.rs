// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clockless state machine for the PTY resize repaint nudge.

use std::collections::VecDeque;

/// Output quiet a nudge waits for before it starts.
pub const QUIET_WAIT_MS: u64 = 150;
/// The longest that quiet is waited for; then it goes unless a frame is open.
pub const QUIET_MAX_MS: u64 = 3_000;
/// Maximum wait for an open DEC 2026 frame after the quiet timeout.
pub const FRAME_WAIT_CAP_MS: u64 = 3_000;
/// Output quiet required after an answer before releasing the hold.
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
/// Hard deadline on any PTY hold, enforced by the host.
pub const PTY_HOLD_CAP_MS: u64 = 2 * ANSWER_TIMEOUT_MS + 1_000;

/// `[render] repaint_nudge`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Nothing runs.
    Off,
    /// Log what would have been done without writing to the PTY.
    Log,
    /// Perform the PTY repaint nudge when needed.
    #[default]
    On,
}

impl Mode {
    /// Parses the config mode word case-insensitively.
    #[inline]
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        [Self::Off, Self::Log, Self::On]
            .into_iter()
            .find(|mode| word.eq_ignore_ascii_case(mode.word()))
    }

    /// The config word.
    #[must_use]
    #[inline]
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
    #[inline]
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
    /// A repaint adoption left the engine blank and received no answer.
    Adoption,
}

impl Reason {
    /// The log word.
    #[must_use]
    #[inline]
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
    #[inline]
    const fn slot(self) -> usize {
        match self {
            Self::Lease => 0,
            Self::Selection => 1,
            Self::Busy => 2,
        }
    }

    /// The log word.
    #[must_use]
    #[inline]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Lease => "lease",
            Self::Selection => "selection",
            Self::Busy => "busy",
        }
    }
}

/// Reason an attempt ended without repairing a likely displaced screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiveUp {
    /// One of the two PTY sizes went unanswered before the timeout.
    NoAnswer,
    /// A gate deferred the attempt to its cap.
    Deferred(Gate),
    /// Three attempts in the hour are spent.
    Budget,
}

impl GiveUp {
    /// The log word.
    #[inline]
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
    /// The engine owes no rows because the program repainted on its own.
    NothingOwed,
    /// A resize moved the engine away from the held size before an answer arrived.
    Superseded,
    /// The second size was answered without the first: repainted anyway.
    Repainted,
    /// The engine is too small to hold one row short.
    TooSmall,
}

/// What the host must do after a [`RepaintNudge::step`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Do nothing now; re-evaluate on the next host event.
    Nothing,
    /// Nothing now; call [`RepaintNudge::step`] again at this time (host ms).
    WaitUntil(u64),
    /// Set the PTY to `engine.rows - 1` × `engine.cols` now.
    Hold,
    /// Restore the PTY to the engine's size now.
    Release,
    /// Drop the hold without writing to the PTY because another caller resized it.
    Forget,
    /// Mode `log`: the attempt would have started here. Finished.
    WouldNudge,
    /// Both PTY sizes were answered; carries first and second answer latencies in ms.
    Healed {
        /// Hold to first answer.
        first_ms: u64,
        /// Release to second answer.
        second_ms: u64,
    },
    /// Attempt ended without needing owner intervention.
    Dropped(Dropped),
    /// Finished without a repair the owner has one action for.
    GaveUp(GiveUp),
}

/// Per-session state sampled by the host at each [`RepaintNudge::step`].
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
    /// Milliseconds since a person last interacted with this session (or since attach).
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
    /// Returns baseline facts for a quiet, eligible session at `size`.
    #[inline]
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

/// Returns milliseconds since the last user gesture, falling back to `since_attach_ms`.
#[inline]
#[must_use]
pub const fn person_quiet_ms(since_gesture_ms: Option<u64>, since_attach_ms: u64) -> u64 {
    match since_gesture_ms {
        Some(ms) => ms,
        None => since_attach_ms,
    }
}

/// Per-session state machine for the PTY resize repaint nudge.
#[derive(Clone, Debug)]
pub struct RepaintNudge {
    phase: Phase,
    reason: Reason,
    epoch: u64,
    last_epoch: Option<u64>,
    deferred_since: [Option<u64>; 3],
    quiet_since: Option<u64>,
    attempts: VecDeque<u64>,
    nudged: u32,
    hold_ms: u64,
    hold_engine: (u16, u16),
    base_clears: u64,
    base_replaced: u64,
    release_ms: u64,
    first_answer_ms: Option<u64>,
    second_answer_ms: Option<u64>,
    rebase: bool,
    thaw: bool,
}

impl Default for RepaintNudge {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl RepaintNudge {
    /// A machine at rest with no history.
    #[inline]
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
    #[inline]
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Why the current or last attempt was armed.
    #[inline]
    #[must_use]
    pub const fn reason(&self) -> Reason {
        self.reason
    }

    /// The epoch of the current or last attempt.
    #[inline]
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// How many attempts have actually held a PTY (`nudged=`).
    #[inline]
    #[must_use]
    pub const fn nudged(&self) -> u32 {
        self.nudged
    }

    /// Milliseconds from the hold to the first answer, once seen.
    #[inline]
    #[must_use]
    pub const fn first_answer_ms(&self) -> Option<u64> {
        self.first_answer_ms
    }

    /// Milliseconds from the release to the second answer, once seen.
    #[inline]
    #[must_use]
    pub const fn second_answer_ms(&self) -> Option<u64> {
        self.second_answer_ms
    }

    /// When the hold began (host ms); meaningful while [`Phase::Short`].
    #[inline]
    #[must_use]
    pub const fn hold_started_ms(&self) -> u64 {
        self.hold_ms
    }

    /// The engine size the hold was taken against.
    #[inline]
    #[must_use]
    pub const fn hold_engine(&self) -> (u16, u16) {
        self.hold_engine
    }

    /// Arms the nudge for `epoch`, returning `false` if disabled, active, or already tried.
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

    /// Releases an active hold from the host side and transitions to [`Phase::Restoring`].
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

    #[inline]
    const fn finish(&mut self, act: Act) -> Act {
        self.phase = Phase::Rest;
        self.last_epoch = Some(self.epoch);
        act
    }

    #[inline]
    const fn drop_with(&mut self, why: Dropped) -> Act {
        self.phase = Phase::Rest;
        if matches!(why, Dropped::NothingOwed | Dropped::Repainted) {
            self.last_epoch = Some(self.epoch);
        }
        Act::Dropped(why)
    }

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

    const fn answered(&self, f: &Facts) -> bool {
        f.full_clears > self.base_clears || f.screen_replaced > self.base_replaced
    }

    const fn settled(f: &Facts) -> bool {
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
        if f.engine != self.hold_engine || f.pty != target {
            return self.forget(f, now_ms);
        }
        if !f.alt_screen {
            return self.release(f, now_ms, Phase::Rest);
        }
        let waited = now_ms.saturating_sub(self.hold_ms);
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
            self.thaw = true;
            return Act::Nothing;
        }
        if std::mem::take(&mut self.thaw) {
            self.release_ms = now_ms;
        }
        if std::mem::take(&mut self.rebase) {
            if self.answered(f) {
                self.first_answer_ms = Some(now_ms.saturating_sub(self.hold_ms));
            }
            self.base_clears = f.full_clears;
            self.base_replaced = f.screen_replaced;
        }
        let waited = now_ms.saturating_sub(self.release_ms);
        if self.answered(f) {
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
#[path = "repaint_nudge_tests.rs"]
mod tests;
