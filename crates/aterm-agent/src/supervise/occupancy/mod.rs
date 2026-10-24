// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS OCCUPANCY (`docs/DESIGN-harness-occupancy-2026-10-20.md`).

pub mod declarer;
pub mod door;
#[cfg(any(test, debug_assertions))]
mod door_environment;
pub mod due;
pub mod fact_word;
pub mod hold;
pub mod reading;
pub mod replay;
pub mod ring;
pub mod rules;
pub mod said;
pub mod templates;
pub mod wake_atoms;
pub mod wakes;
#[cfg(any(test, debug_assertions))]
mod wakes_environment;
pub mod wakes_seen;

pub use door::{
    AtWrite, BreakLine, PaceGrant, PaceRefusal, StatusSeen, StuckVerdict, stuck_at_break,
    write_held,
};
pub use fact_word::{FactWord, NoteOutcome};
pub use hold::{Close, HoldChange, HoldClock, HoldEvent, waiting_declared};
pub use reading::{OccupancyReading, SlotsSeen};
pub use rules::{Books, Guards, HoldSeen, RuleId, Verdict, decide};
pub use said::{OccupancySaid, SaidHold, SaidReadyIdle, SaidRule, SaidVerdict};
pub use templates::FactLine;
pub use wakes_seen::{WakeClaim, WakeDoor, WakeSeen, WakesSeen};

#[cfg(any(test, debug_assertions))]
pub use door_environment::occupancy_door_environment;
#[cfg(any(test, debug_assertions))]
pub use wakes_environment::wake_book_environment;

/// THE NEGATIVE CONTROLS' FAULT POINTS.
#[cfg(any(test, debug_assertions))]
pub mod fault {
    use std::cell::Cell;

    /// A planted defect.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Fault {
        /// [`Books::hold`](super::Books::hold) keeps a closed hold's per-hold counts.
        DedupeByRule,
        /// [`HoldClock::step`](super::HoldClock::step) restarts the open hold at each new break.
        RestartOnIdle,
        /// [`HoldClock::step`](super::HoldClock::step) ends the hold at the turn that answers a
        /// line the harness typed.
        CloseOnOurTurn,
        /// [`HoldClock::step`](super::HoldClock::step) judges the point that ends a harness line's
        /// answer as any other point.
        CloseAtTheAnswer,
        /// [`HoldClock::step`](super::HoldClock::step) ends the hold at a point that decides
        /// nothing while the work still shows.
        CloseAtAnyPoint,
        /// [`FactsState::seed`](crate::supervise::facts::FactsState::seed) reads an intent with no
        /// outcome after it as told.
        SeedIntentAsTold,
        /// [`FactsState::seed`](crate::supervise::facts::FactsState::seed) reads a skipped episode
        /// as told.
        SeedSkipAsTold,
        /// The door's write under `prompt` makes no `status` read before it.
        DoorSkipsStatus,
        /// The door's write books its outcome (`typed`) before the write.
        DoorOutcomeFirst,
        /// The door's line is taken as the host's notice (`TurnEndState::host_typed`).
        DoorHostTyped,
        /// The door's write falls back to the guarded submit on a read it cannot fence.
        DoorGuardedFallback,
        /// The door's write ignores a person's hand (their keys, a driver, the halt).
        DoorIgnoresPerson,
        /// The door's write ignores a draft and unread input.
        DoorIgnoresDraft,
        /// The door's write ignores the brakes and the host's withhold (a full disk).
        DoorIgnoresBrakes,
        /// [`write_held`](super::write_held) lets a nudge go while the upgrade's notice is due.
        NudgeOverNotice,
        /// [`write_held`](super::write_held) lets the door's line go at the break the host's own
        /// line went at.
        StackOnHostLine,
        /// The door's untaken-act guard leaves out the host's line awaiting its answer.
        OverUntakenHostLine,
        /// The door's write, held after its reads, makes them again at every settled read of the
        /// same episode.
        DoorRereads,
        /// The door books a `fact-held@v1` row at every change of a rule's word to a held one.
        HeldEachChange,
        /// `Session::drive` tells its host no hold on its return alone.
        UntoldOnReturnOnly,
        /// The door keeps the held verdicts it booked past a change of hold.
        HeldAcrossHolds,
        /// [`write_held`](super::write_held) lets the stuck fact go while the upgrade's notice
        /// is due (§2.1's ranks 3 and 4 turned).
        StuckOverNotice,
        /// [`stuck_at_break`](super::door::stuck_at_break) judges the stuck fact with no
        /// `[harness] stuck_break_note` grant, as if the key were on.
        StuckWithoutKey,
        /// [`DueClock::observe`](super::due::DueClock::observe) restarts a key's clock at each
        /// change of the guard that holds it, as a clock from the rule's last word would.
        KFromLastWord,
        /// [`DueClock::seed`](super::due::DueClock::seed) drops a successor's seed: its clock
        /// starts at its first look.
        KUnseeded,
        /// [`DueClock::sample`](super::due::DueClock::sample) raises a look past K.
        LateRaise,
        /// [`WakeBook::on_source`](super::wakes::WakeBook::on_source) ignores an edge: only a
        /// level a look reads makes a wake ready.
        LevelOnly,
        /// [`WakeBook::claim`](super::wakes::WakeBook::claim) grants a told wake again.
        ClaimTwice,
        /// [`ParkBook::park`](super::wakes::ParkBook::park) admits a second park.
        ParkSecond,
        /// [`WakeBook::answer_done`](super::wakes::WakeBook::answer_done) keeps a wake answered
        /// whose reply was never written.
        ToldBeforeWrite,
        /// [`reconcile_claim`](super::wakes::reconcile_claim) releases a claim whose last row
        /// says typed.
        ReleaseAfterTyped,
        /// [`declarer::prove`](super::declarer::prove) admits a peer whose ancestry never meets
        /// the session's tab: a wake declared, awaited or taken from anyone.
        AwaitFromAnyone,
        /// [`wake_atoms::admit`](super::wake_atoms::admit) probes the path before it judges it.
        CanonicalizeAtRead,
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
        HeldEachChange,
        UntoldOnReturnOnly,
        HeldAcrossHolds,
        StuckOverNotice,
        StuckWithoutKey,
        KFromLastWord,
        KUnseeded,
        LateRaise,
        LevelOnly,
        ClaimTwice,
        ParkSecond,
        ToldBeforeWrite,
        ReleaseAfterTyped,
        AwaitFromAnyone,
        CanonicalizeAtRead,
    }

    /// The shipped build: never armed.
    #[inline]
    pub(crate) const fn armed(_fault: Fault) -> bool {
        false
    }
}
