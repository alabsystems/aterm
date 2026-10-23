// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS OCCUPANCY (`docs/DESIGN-harness-occupancy-2026-10-20.md`): what the harness measures
//! of a session's wait on its own background work, under `[harness] occupancy`
//! ([`super::config::Occupancy`]). Measures, judges and books, and types only under the
//! owner's `prompt`:
//!
//! * [`hold`]: the hold's clock — when the wait began, whether the agent declared it
//!   (`WAITING:`), when it ended — and its `fact-hold@v1` ledger rows, which a successor loop
//!   carries the hold by.
//! * [`reading`]: the machine as the window's facts board reads it for one session
//!   (`IdleHost::occupancy`, taped), the empty reading where none is.
//! * [`rules`]: the built-in rules, `wait-nudge@v1` and `hold-room@v1`, and [`decide`], the pure
//!   decider of whether one is due at a break and what holds it.
//! * [`templates`]: the closed lines they type.
//! * [`door`]: the pure parts of the write under `prompt`: the `status` read before it, the
//!   write's own holds, the host's pace, and the intent and outcome rows.
//! * [`said`]: what the loop tells its host once a change ([`OccupancySaid`],
//!   `IdleHost::occupancy_said`) and its `occupancy why` form.
//! * [`ring`]: the board's facts ring (`<state>/occupancy/facts.ring`) and the slot-time idle
//!   while a session was ready; [`replay`]: `occupancy check --replay`, the decider run again
//!   over it.
//!
//! The loop feeds the clock and asks the decider from `run::occupancy_loop` on hosted,
//! unattended loops only, and only while the key is not `off`; there it journals `OCCUPANCY`
//! lines and books the rows. Under `observe` what `prompt` would type is booked as
//! `fact-would@v1`, and that is all; only the owner's `occupancy = "prompt"` reaches the write
//! ([`super::config::Occupancy::grant`]), at a drawn break, fenced, after every guard.

pub mod door;
#[cfg(any(test, debug_assertions))]
mod door_environment;
pub mod hold;
pub mod reading;
pub mod replay;
pub mod ring;
pub mod rules;
pub mod said;
pub mod templates;

pub use door::{AtWrite, PaceGrant, PaceRefusal, StatusSeen, write_held};
pub use hold::{Close, HoldChange, HoldClock, HoldEvent, waiting_declared};
pub use reading::{OccupancyReading, SlotsSeen};
pub use rules::{Books, Guards, HoldSeen, RuleId, Verdict, decide};
pub use said::{OccupancySaid, SaidHold, SaidRule, SaidVerdict};
pub use templates::FactLine;

#[cfg(any(test, debug_assertions))]
pub use door_environment::occupancy_door_environment;

/// THE NEGATIVE CONTROLS' FAULT POINTS, debug and test builds only: each, armed on this thread,
/// plants one defect `OccupancyDoor`'s Tier-1 bind (`tests/conformance_occupancy.rs`) must catch,
/// as `aterm_keeper::core::fault` plants its. The shipped build's check is constant `false`.
#[cfg(any(test, debug_assertions))]
pub mod fault {
    use std::cell::Cell;

    /// A planted defect.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Fault {
        /// [`Books::hold`](super::Books::hold) keeps a closed hold's per-hold counts: a rule told
        /// in one hold is never due in the next, deduplicated by its name, not its episode.
        DedupeByRule,
        /// [`HoldClock::step`](super::HoldClock::step) restarts the open hold at each new break
        /// (on 10-19 a clock restarted at each short idle window would never have reached 2 h).
        RestartOnIdle,
        /// [`HoldClock::step`](super::HoldClock::step) ends the hold at the turn that answers a
        /// line the harness typed, as if no line were owed.
        CloseOnOurTurn,
        /// [`HoldClock::step`](super::HoldClock::step) judges the point that ends a harness
        /// line's answer as any other point.
        CloseAtTheAnswer,
        /// [`HoldClock::step`](super::HoldClock::step) ends the hold at a point that decides
        /// nothing while the work still shows (F5).
        CloseAtAnyPoint,
        /// [`FactsState::seed`](crate::supervise::facts::FactsState::seed) reads an intent with
        /// no outcome after it as told.
        SeedIntentAsTold,
        /// [`FactsState::seed`](crate::supervise::facts::FactsState::seed) reads a skipped
        /// episode as told.
        SeedSkipAsTold,
        /// The door's write under `prompt` makes no `status` read before it, judging the
        /// person, a driver's hand, the halt and unread input from the loop's last read alone.
        DoorSkipsStatus,
        /// The door's write books its outcome (`typed`) before the write.
        DoorOutcomeFirst,
        /// The door's line is taken as the host's notice (`TurnEndState::host_typed`), not a fact
        /// line of its own (`TurnEndState::fact_typed`).
        DoorHostTyped,
        /// The door's write falls back to the guarded submit on a read it cannot fence.
        DoorGuardedFallback,
        /// The door's write ignores a person's hand (their keys, a driver, the halt).
        DoorIgnoresPerson,
        /// The door's write ignores a draft and unread input.
        DoorIgnoresDraft,
        /// The door's write ignores the brakes and the host's withhold (a full disk).
        DoorIgnoresBrakes,
        /// [`write_held`](super::write_held) lets a nudge go while the upgrade's notice is due
        /// (F11's order turned).
        NudgeOverNotice,
        /// [`write_held`](super::write_held) lets the door's line go at the break the host's own
        /// line went at: a second line at one door.
        StackOnHostLine,
        /// The door's untaken-act guard leaves out the host's line awaiting its answer (the
        /// policy's acts alone, `TurnEndState::act_untaken`): a line typed over a host line the
        /// agent has not taken.
        OverUntakenHostLine,
        /// The door's write, held after its reads, makes them again at every settled read of the
        /// same episode, not once a board look.
        DoorRereads,
    }

    thread_local! {
        static ARMED: Cell<Option<Fault>> = const { Cell::new(None) };
    }

    /// Run `f` with `fault` armed on this thread, disarmed after it even on unwind.
    pub fn with<T>(fault: Fault, f: impl FnOnce() -> T) -> T {
        struct Disarm;
        impl Drop for Disarm {
            fn drop(&mut self) {
                ARMED.with(|a| a.set(None));
            }
        }
        ARMED.with(|a| a.set(Some(fault)));
        let _disarm = Disarm;
        f()
    }

    /// Whether `fault` is armed on this thread.
    pub(crate) fn armed(fault: Fault) -> bool {
        ARMED.with(|a| a.get() == Some(fault))
    }
}

#[cfg(not(any(test, debug_assertions)))]
pub(crate) mod fault {
    /// A planted defect: none is armed in the shipped build.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        DedupeByRule,
        RestartOnIdle,
        CloseOnOurTurn,
        CloseAtTheAnswer,
        CloseAtAnyPoint,
        SeedIntentAsTold,
        SeedSkipAsTold,
        DoorSkipsStatus,
        DoorOutcomeFirst,
        DoorHostTyped,
        DoorGuardedFallback,
        DoorIgnoresPerson,
        DoorIgnoresDraft,
        DoorIgnoresBrakes,
        NudgeOverNotice,
        StackOnHostLine,
        OverUntakenHostLine,
        DoorRereads,
    }

    /// The shipped build: never armed.
    #[inline]
    pub(crate) const fn armed(_fault: Fault) -> bool {
        false
    }
}
