// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS OCCUPANCY (`docs/DESIGN-harness-occupancy-2026-10-20.md`): what the harness measures
//! of a session's wait on its own background work, under `[harness] occupancy`
//! ([`super::config::Occupancy`]). Measures, judges and books, and types nothing:
//!
//! * [`hold`]: the hold's clock — when the wait began, whether the agent declared it
//!   (`WAITING:`), when it ended — and its `fact-hold@v1` ledger rows, which a successor loop
//!   carries the hold by.
//! * [`reading`]: the machine as the window's facts board reads it for one session
//!   (`IdleHost::occupancy`, taped), the empty reading where none is.
//! * [`rules`]: the built-in rules, `wait-nudge@v1` and `hold-room@v1`, and [`decide`], the pure
//!   decider of whether one is due at a break and what holds it.
//! * [`templates`]: the closed lines they would type.
//!
//! The loop feeds the clock and asks the decider from `run::occupancy_loop` on hosted,
//! unattended loops only, and only while the key is not `off`; there it journals `OCCUPANCY`
//! lines and books the rows. What `prompt` would type is booked as `fact-would@v1`: under
//! `observe` that is all, and there is no write path under `prompt` either.

#[cfg(any(test, debug_assertions))]
mod door_environment;
pub mod hold;
pub mod reading;
pub mod rules;
pub mod templates;

pub use hold::{Close, HoldChange, HoldClock, HoldEvent, waiting_declared};
pub use reading::{OccupancyReading, SlotsSeen};
pub use rules::{Books, Guards, HoldSeen, RuleId, Verdict, decide};
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
    }

    /// The shipped build: never armed.
    #[inline]
    pub(crate) const fn armed(_fault: Fault) -> bool {
        false
    }
}
