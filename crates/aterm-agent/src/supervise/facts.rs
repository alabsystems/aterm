// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS SEAM'S STATE (`docs/DESIGN-system-awareness-2026-10-16.md` §14.4;
//! `docs/DESIGN-harness-occupancy-2026-10-20.md` §8): which episodes of a
//! measured condition a session has been told, kept apart from the measuring and the typing.
//! Pure and clockless: every instant is the caller's milliseconds.
//!
//! An episode is told once, across every door that tells it. A successor loop (a live upgrade,
//! a seamless update) seeds the state from the ledger's `fact-*@v1` rows ([`FactsState::seed`]):
//! an episode whose last row is an intent with no outcome was never told (the loop may have died
//! between), and a kind whose episodes a restart cannot name again exactly ([`FactKind::Hold`],
//! keyed by a hold start the successor places anew) counts as told within [`SEED_HOLD_MS`] until
//! its condition has been seen clear once ([`FactsState::observe_closed`]), so a restart never
//! tells it twice.
//!
//! Two kinds, in priority order:
//!
//! * [`FactKind::NoProgress`]: a live background call of this session's agent that the host's
//!   jobs reading says has made no progress (the hung-call design's R2 and R3;
//!   [`crate::supervise::BackgroundCall`]), read through [`FactsReading`]
//!   (`IdleHost::facts`). Its episode is the call's run of no progress, keyed by its process
//!   group and its leader's birth ([`EpisodeKey::no_progress`]), a stable fold a successor reads
//!   back exactly. It rides a prompt the turn-end policy types anyway
//!   (`policy::turn_end::FACT_RIDES`) and, with `[harness] stuck_break_note` on, is typed at a
//!   break of the agent's own background work (the occupancy design's §2.1 rank 4). It is
//!   forgotten only on a present, fresh reading that no longer names it ([`FactsState::observe`]),
//!   so an absent or stale reading never makes a told fact new again.
//! * [`FactKind::Hold`]: the session's wait on its own background work, whose rows the hold
//!   clock writes (`occupancy::hold`), and the occupancy door's outcome rows under `prompt`
//!   (`occupancy::door`).
//!
//! [`FactKind`] shares its name with `crate::harness::footer::FactKind` (the footer's fact
//! lines): name this one by its path, never glob-import both.

use std::collections::BTreeMap;

use super::background::BackgroundCall;

/// What a fact is about, in priority order: where two open episodes are untold, the first kind
/// is told first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FactKind {
    /// A live background call of the agent's that has made no progress (the hung-call design's
    /// R2; `aterm_vitals::jobs::JobRow::no_progress`).
    NoProgress,
    /// The session's wait on its own background work: how long it has held, and whether the
    /// agent declared it (`WAITING:`; the occupancy design's §3.1 `hold`).
    Hold,
}

impl FactKind {
    /// Every kind, in priority order.
    pub const ALL: [FactKind; 2] = [FactKind::NoProgress, FactKind::Hold];

    /// The ledger rule a row of this kind is booked under, `fact-<kind>@v1`. Stable: a successor
    /// loop seeds by it ([`Self::of_rule`]).
    #[must_use]
    pub const fn rule(self) -> &'static str {
        match self {
            FactKind::NoProgress => "fact-no-progress@v1",
            FactKind::Hold => "fact-hold@v1",
        }
    }

    /// Its word, as the Harness page counts it and an intent row names it (`kind=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            FactKind::NoProgress => "no-progress",
            FactKind::Hold => "hold",
        }
    }

    /// The kind a ledger rule names, or `None` for a rule that is no fact's.
    #[must_use]
    pub fn of_rule(rule: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.rule() == rule)
    }

    /// The kind [`Self::word`] names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.word() == word)
    }

    /// Whether a seed holds the whole kind told ([`FactsState::seed`]): a kind whose episode ids
    /// a successor cannot name again exactly. A no-progress episode's id folds the call's group
    /// and its leader's birth, which survive a restart, so its own key carries it.
    const fn seeds_whole(self) -> bool {
        match self {
            FactKind::NoProgress => false,
            FactKind::Hold => true,
        }
    }
}

/// One episode of a kind's condition: the kind and an id that survives a restart (a job's
/// `(pgid, leader birth)` folded, [`Self::no_progress`]; for the hold, the episode's start in
/// Unix milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpisodeKey {
    /// What the episode is of.
    pub kind: FactKind,
    /// Which episode of it.
    pub id: u64,
}

impl EpisodeKey {
    /// The no-progress episode of the call whose process group is `pgid` and whose leader was born
    /// at `birth`: FNV-1a over the two, little endian, never std's `DefaultHasher`, whose output
    /// may change between builds, since a key is read back from the ledger by a later one.
    #[must_use]
    pub fn no_progress(pgid: u32, birth: u64) -> Self {
        let mut bytes = [0u8; 12];
        bytes[..4].copy_from_slice(&pgid.to_le_bytes());
        bytes[4..].copy_from_slice(&birth.to_le_bytes());
        Self {
            kind: FactKind::NoProgress,
            id: aterm_hash::fnv1a_64(&bytes),
        }
    }

    /// The episode of `call`; `None` where its leader's birth was not read, which nothing could
    /// tell from a later process in a reused group.
    #[must_use]
    pub fn of_call(call: &BackgroundCall) -> Option<Self> {
        call.birth.map(|birth| Self::no_progress(call.pgid, birth))
    }
}

/// The longest fact sentence, in characters: one sentence, filled from a closed template.
pub const FACT_CAP: usize = 200;

/// How old the reading a fact comes from may be when it is typed.
pub const FACT_FRESH_MS: i64 = 5_000;

/// How old a no-progress reading may be and still forget a told episode it no longer names: the
/// jobs board's own bound (`aterm-gui`'s `vitals_jobs::JOBS_FRESH`). A fact is told only from a
/// reading [`FACT_FRESH_MS`] old ([`FactsReading::tells_at`]), counted from when the probe read
/// it, which the board meets: its view of a call at no progress is read about every 2 s, a window
/// in view or none (`vitals_jobs::told_after`, `windowless_after`), and the host re-proves at the
/// ask that every leader it names still lives with the same birth.
pub const NO_PROGRESS_FRESH_MS: i64 = 60_000;

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

/// A fact to tell: its episode, its one sentence, and when the reading it came from was taken
/// (unix ms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// Its episode.
    pub key: EpisodeKey,
    /// One sentence from a closed template, no imperative, at most [`FACT_CAP`] characters.
    pub text: String,
    /// When the reading it came from was taken, unix ms.
    pub as_of_ms: i64,
}

/// WHAT THE HOST READ FOR THIS SESSION'S AGENT (`IdleHost::facts`): the live background calls of
/// THIS session's agent that made no progress, each regardless of the others, worst (quiet
/// longest) first, as of `as_of_ms` (unix ms). Plain values only: this crate takes no dependency
/// on the engine's types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactsReading {
    /// When the reading was taken, unix ms.
    pub as_of_ms: i64,
    /// Each call at no progress.
    pub no_progress: Vec<BackgroundCall>,
}

impl FactsReading {
    /// No older than [`NO_PROGRESS_FRESH_MS`] at `now_ms` (a reading from the future, a clock
    /// stepped back, is no older).
    #[must_use]
    pub fn fresh_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= NO_PROGRESS_FRESH_MS
    }

    /// No older than [`FACT_FRESH_MS`] at `now_ms`: a reading a fact may be told from (system
    /// awareness §14.4's gate), as [`Self::fresh_at`] is one that may forget.
    #[must_use]
    pub fn tells_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= FACT_FRESH_MS
    }

    /// Every open episode the reading names, with its sentence and its quiet: in priority order,
    /// each kind's worst first. A call with no birth names none.
    fn open(&self) -> Vec<(EpisodeKey, String, u64)> {
        let mut open: Vec<(EpisodeKey, String, u64)> = self
            .no_progress
            .iter()
            .filter_map(|c| Some((EpisodeKey::of_call(c)?, c.told(), c.quiet_ms)))
            .collect();
        open.sort_by(|a, b| a.0.kind.cmp(&b.0.kind).then(b.2.cmp(&a.2)));
        open
    }
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
    /// Each episode the occupancy loop's task door booked (`door=task`), with when: a fact word
    /// noted on a managed session's task record, a host's answer that took none, or under
    /// `observe` the note that would have been. A mark apart from [`Self::told`]: a note tells
    /// the session's manager, never the session, so a line the managed guard no longer holds is
    /// still the session's to be told.
    noted: BTreeMap<EpisodeKey, i64>,
}

impl FactsState {
    /// The episodes of `kind` open now, from one reading of it: a told episode of that kind not
    /// among them closed, so it is forgotten (the condition's next episode is told again), and a
    /// seed's hold on the kind, with no open episode of it, has been seen clear. The other kinds
    /// are not this reading's to judge, and keep what they hold.
    pub fn observe_closed(&mut self, kind: FactKind, open: &[EpisodeKey]) {
        self.told
            .retain(|key, _| key.kind != kind || open.contains(key));
        self.noted
            .retain(|key, _| key.kind != kind || open.contains(key));
        self.seeded
            .retain(|(seeded, _)| *seeded != kind || open.iter().any(|key| key.kind == kind));
    }

    /// FOLD IN WHAT THE HOST READ at `now_ms` ([`FactKind::NoProgress`]): on a reading that is
    /// present and fresh, a told episode it no longer names is over (the call made progress or
    /// ended) and forgotten. An absent or stale reading forgets nothing.
    pub fn observe(&mut self, reading: Option<&FactsReading>, now_ms: i64) {
        let Some(r) = reading.filter(|r| r.fresh_at(now_ms)) else {
            return;
        };
        let open: Vec<EpisodeKey> = r.open().into_iter().map(|(key, ..)| key).collect();
        self.observe_closed(FactKind::NoProgress, &open);
    }

    /// THE FACT TO TELL NOW: the highest-priority open episode of a present, fresh reading that
    /// was not told, its sentence within [`FACT_CAP`]; else `None`. The others wait, and one whose
    /// episode closes meanwhile is never told late. Told only from a reading at most
    /// [`FACT_FRESH_MS`] old ([`FactsReading::tells_at`]).
    #[must_use]
    pub fn pending(&self, reading: Option<&FactsReading>, now_ms: i64) -> Option<Fact> {
        let r = reading.filter(|r| r.tells_at(now_ms))?;
        r.open()
            .into_iter()
            .find(|(key, text, _)| {
                !self.is_told(*key) && !text.is_empty() && text.chars().count() <= FACT_CAP
            })
            .map(|(key, text, _)| Fact {
                key,
                text,
                as_of_ms: r.as_of_ms,
            })
    }

    /// `key` was told at `at_ms`: where its act was taken, never where it was decided.
    pub fn told(&mut self, key: EpisodeKey, at_ms: i64) {
        self.told.insert(key, at_ms);
    }

    /// The task door booked `key` at `at_ms` ([`Self::noted`]).
    pub fn note(&mut self, key: EpisodeKey, at_ms: i64) {
        self.noted.insert(key, at_ms);
    }

    /// Whether the task door booked `key` ([`Self::note`]): forgotten with the episode
    /// ([`Self::observe_closed`]). A successor seeds none: the host's edit of a word standing
    /// already moves nothing.
    #[must_use]
    pub fn is_noted(&self, key: EpisodeKey) -> bool {
        self.noted.contains_key(&key)
    }

    /// Whether `key` has been told: the episode itself, or any episode of a kind a seed holds.
    #[must_use]
    pub fn is_told(&self, key: EpisodeKey) -> bool {
        self.told.contains_key(&key) || self.seeded.iter().any(|(kind, _)| *kind == key.kind)
    }

    /// A successor loop's state from the ledger's fact rows, oldest first, at `now_ms`: each
    /// episode whose LAST row says it was typed is told, and a kind whose ids a restart does not
    /// carry exactly ([`FactKind::Hold`]) is held told while that row is younger than
    /// [`SEED_HOLD_MS`]. An intent with no outcome after it, or a skip, leaves the episode untold
    /// (`OccupancyDoor`'s `Crash`).
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
            if key.kind.seeds_whole() && now_ms.saturating_sub(at_ms) < SEED_HOLD_MS {
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
