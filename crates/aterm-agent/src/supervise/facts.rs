// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS SEAM'S STATE (`docs/DESIGN-system-awareness-2026-10-16.md` §14.4;
//! `docs/DESIGN-harness-occupancy-2026-10-20.md` §8): which episodes of a
//! measured condition a session has been told, kept apart from the measuring and the typing.
//! Pure and clockless: every instant is the caller's milliseconds.
//!
//! An episode is told once. A successor loop (a live upgrade, a seamless update) seeds the state
//! from the ledger's `fact-*@v1` rows ([`FactsState::seed`]): an episode whose last row is an
//! intent with no outcome was never told (the loop may have died between), and a kind told
//! within [`SEED_HOLD_MS`] counts as told until its condition has been seen clear once
//! ([`FactsState::observe_closed`]), so a restart never tells it twice.
//!
//! Currently holds one kind, [`FactKind::Hold`], whose rows the hold clock writes
//! (`occupancy::hold`), and the occupancy door's outcome rows under `prompt` (`occupancy::door`):
//! the loop's door reads this state before it types.
//!
//! [`FactKind`] shares its name with `crate::harness::footer::FactKind` (the footer's fact
//! lines): name this one by its path, never glob-import both.

use std::collections::BTreeMap;

/// What a fact is about, in priority order: where two open episodes are untold, the first kind
/// rides. Currently holds [`FactKind::Hold`] alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FactKind {
    /// The session's wait on its own background work: how long it has held, and whether the
    /// agent declared it (`WAITING:`; the occupancy design's §3.1 `hold`).
    Hold,
}

impl FactKind {
    /// Every kind, in priority order.
    pub const ALL: [FactKind; 1] = [FactKind::Hold];

    /// The ledger rule a row of this kind is booked under, `fact-<kind>@v1`. Stable: a successor
    /// loop seeds by it ([`Self::of_rule`]).
    #[must_use]
    pub const fn rule(self) -> &'static str {
        match self {
            FactKind::Hold => "fact-hold@v1",
        }
    }

    /// The kind a ledger rule names, or `None` for a rule that is no fact's.
    #[must_use]
    pub fn of_rule(rule: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.rule() == rule)
    }
}

/// One episode of a kind's condition: the kind and an id that survives a restart (a job's
/// `(pgid, leader start)` hashed; for the hold and the machine's own conditions, the episode's
/// start in Unix milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpisodeKey {
    /// What the episode is of.
    pub kind: FactKind,
    /// Which episode of it.
    pub id: u64,
}

/// The longest fact sentence, in characters: one sentence, filled from a closed template.
pub const FACT_CAP: usize = 200;

/// How old the reading a fact comes from may be when it is typed.
pub const FACT_FRESH_MS: i64 = 5_000;

/// After a restart, a kind told this recently waits for its condition to clear once before any
/// episode of it is told again.
pub const SEED_HOLD_MS: i64 = 30 * 60_000;

/// What a ledger row says of an episode, for [`FactsState::seed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The loop meant to tell it: written before the act, so an intent with no outcome after it
    /// is an episode the loop may never have told.
    Intent,
    /// It was told.
    Typed,
    /// The loop meant to and did not (a guard held it at the last moment): untold.
    Skipped,
}

/// Which episodes this session has been told ([`FactsState::told`]), and which kinds a seed holds
/// told until their condition clears.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactsState {
    /// Each episode told, with when.
    told: BTreeMap<EpisodeKey, i64>,
    /// Each kind a seed holds told (with the last time it was told), until its condition has
    /// been seen clear once.
    seeded: Vec<(FactKind, i64)>,
}

impl FactsState {
    /// The episodes open now, from one reading: a told episode not among them closed, so it is
    /// forgotten (a new episode of its kind is a new key), and a seeded kind with no open episode
    /// has been seen clear, so the seed no longer holds it.
    pub fn observe_closed(&mut self, open: &[EpisodeKey]) {
        self.told.retain(|key, _| open.contains(key));
        self.seeded
            .retain(|(kind, _)| open.iter().any(|key| key.kind == *kind));
    }

    /// `key` was told at `at_ms`.
    pub fn told(&mut self, key: EpisodeKey, at_ms: i64) {
        self.told.insert(key, at_ms);
    }

    /// Whether `key` has been told: the episode itself, or any episode of a kind a seed holds.
    #[must_use]
    pub fn is_told(&self, key: EpisodeKey) -> bool {
        self.told.contains_key(&key) || self.seeded.iter().any(|(kind, _)| *kind == key.kind)
    }

    /// A successor loop's state from the ledger's fact rows, oldest first, at `now_ms`: each
    /// episode whose LAST row says it was typed is told, and its kind is held told while that
    /// row is younger than [`SEED_HOLD_MS`]. An intent with no outcome after it, or a skip, leaves
    /// the episode untold (`OccupancyDoor`'s `Crash`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OccupancyDoor",
            action = "Crash",
            project = "conformance_occupancy::project"
        )
    )]
    pub fn seed(&mut self, rows: &[(EpisodeKey, i64, SeedOutcome)], now_ms: i64) {
        use crate::supervise::occupancy::fault::{Fault, armed};
        let mut last: BTreeMap<EpisodeKey, (i64, SeedOutcome)> = BTreeMap::new();
        for &(key, at_ms, outcome) in rows {
            last.insert(key, (at_ms, outcome));
        }
        for (key, (at_ms, outcome)) in last {
            let told = outcome == SeedOutcome::Typed
                || (outcome == SeedOutcome::Intent && armed(Fault::SeedIntentAsTold))
                || (outcome == SeedOutcome::Skipped && armed(Fault::SeedSkipAsTold));
            if !told {
                continue;
            }
            self.told.insert(key, at_ms);
            if now_ms.saturating_sub(at_ms) < SEED_HOLD_MS {
                match self.seeded.iter_mut().find(|(kind, _)| *kind == key.kind) {
                    Some((_, when)) => *when = (*when).max(at_ms),
                    None => self.seeded.push((key.kind, at_ms)),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
