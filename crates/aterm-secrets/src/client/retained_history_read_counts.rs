// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Numeric diagnostics for `ReadCache::read`; no authority decisions or file observations.

use std::sync::atomic::{AtomicU64, Ordering};

/// Journal outcomes after one kind of decoded-history cache lookup.
/// Every attempt still calls `read_committed`, including an unchanged result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetainedHistoryReadOutcomes {
    pub lookups: u64,
    pub unchanged: u64,
    pub extended: u64,
    pub rebuilt: u64,
    /// Errors returned by `read_committed`, before decoded receipt replay.
    pub errors: u64,
}

impl RetainedHistoryReadOutcomes {
    fn checked_delta(self, before: Self) -> Option<Self> {
        Some(Self {
            lookups: self.lookups.checked_sub(before.lookups)?,
            unchanged: self.unchanged.checked_sub(before.unchanged)?,
            extended: self.extended.checked_sub(before.extended)?,
            rebuilt: self.rebuilt.checked_sub(before.rebuilt)?,
            errors: self.errors.checked_sub(before.errors)?,
        })
    }
}

/// Cumulative diagnostics owned by one retained-history decoded cache.
/// These count only `ReadCache::read`: enrollment checks in `read_history` and
/// the separately admitted writer replay path are outside this scope.
/// Snapshot fields are loaded separately; join all measured workers before
/// comparing them. The counts contain no paths, values or authority claims.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetainedHistoryReadCounts {
    pub empty: RetainedHistoryReadOutcomes,
    pub same_directory: RetainedHistoryReadOutcomes,
    pub other_directory: RetainedHistoryReadOutcomes,
    /// The cache mutex was unavailable; the existing read still runs uncached.
    pub unavailable: RetainedHistoryReadOutcomes,
    pub journal_read_attempts: u64,
    pub replay_records_attempted: u64,
    pub replay_records_accepted: u64,
    /// Failed `History::ingest_record` calls, not journal-read failures.
    pub replay_failures: u64,
    pub publications: u64,
    /// Successful publications that replaced an occupied different-directory slot.
    pub replacements: u64,
}

impl RetainedHistoryReadCounts {
    /// Subtract an earlier snapshot, returning `None` if any field decreased.
    #[must_use]
    pub fn checked_delta(self, before: Self) -> Option<Self> {
        Some(Self {
            empty: self.empty.checked_delta(before.empty)?,
            same_directory: self.same_directory.checked_delta(before.same_directory)?,
            other_directory: self.other_directory.checked_delta(before.other_directory)?,
            unavailable: self.unavailable.checked_delta(before.unavailable)?,
            journal_read_attempts: self
                .journal_read_attempts
                .checked_sub(before.journal_read_attempts)?,
            replay_records_attempted: self
                .replay_records_attempted
                .checked_sub(before.replay_records_attempted)?,
            replay_records_accepted: self
                .replay_records_accepted
                .checked_sub(before.replay_records_accepted)?,
            replay_failures: self.replay_failures.checked_sub(before.replay_failures)?,
            publications: self.publications.checked_sub(before.publications)?,
            replacements: self.replacements.checked_sub(before.replacements)?,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) enum Lookup {
    Empty,
    SameDirectory,
    OtherDirectory,
    Unavailable,
}

struct Outcomes {
    lookups: AtomicU64,
    unchanged: AtomicU64,
    extended: AtomicU64,
    rebuilt: AtomicU64,
    errors: AtomicU64,
}

impl Outcomes {
    const fn new() -> Self {
        Self {
            lookups: AtomicU64::new(0),
            unchanged: AtomicU64::new(0),
            extended: AtomicU64::new(0),
            rebuilt: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> RetainedHistoryReadOutcomes {
        RetainedHistoryReadOutcomes {
            lookups: self.lookups.load(Ordering::Relaxed),
            unchanged: self.unchanged.load(Ordering::Relaxed),
            extended: self.extended.load(Ordering::Relaxed),
            rebuilt: self.rebuilt.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

pub(super) struct Counters {
    empty: Outcomes,
    same_directory: Outcomes,
    other_directory: Outcomes,
    unavailable: Outcomes,
    journal_read_attempts: AtomicU64,
    replay_records_attempted: AtomicU64,
    replay_records_accepted: AtomicU64,
    replay_failures: AtomicU64,
    publications: AtomicU64,
    replacements: AtomicU64,
}

impl Counters {
    pub(super) const fn new() -> Self {
        Self {
            empty: Outcomes::new(),
            same_directory: Outcomes::new(),
            other_directory: Outcomes::new(),
            unavailable: Outcomes::new(),
            journal_read_attempts: AtomicU64::new(0),
            replay_records_attempted: AtomicU64::new(0),
            replay_records_accepted: AtomicU64::new(0),
            replay_failures: AtomicU64::new(0),
            publications: AtomicU64::new(0),
            replacements: AtomicU64::new(0),
        }
    }

    fn row(&self, lookup: Lookup) -> &Outcomes {
        match lookup {
            Lookup::Empty => &self.empty,
            Lookup::SameDirectory => &self.same_directory,
            Lookup::OtherDirectory => &self.other_directory,
            Lookup::Unavailable => &self.unavailable,
        }
    }

    pub(super) fn lookup(&self, lookup: Lookup) {
        self.row(lookup).lookups.fetch_add(1, Ordering::Relaxed);
        self.journal_read_attempts.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn journal_result(
        &self,
        lookup: Lookup,
        result: &std::io::Result<aterm_journal::Committed>,
    ) {
        let row = self.row(lookup);
        let counter = match result {
            Ok(aterm_journal::Committed::Unchanged) => &row.unchanged,
            Ok(aterm_journal::Committed::Extended { .. }) => &row.extended,
            Ok(aterm_journal::Committed::Rebuilt { .. }) => &row.rebuilt,
            Err(_) => &row.errors,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn replay_attempt(&self) {
        self.replay_records_attempted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn replay_result(&self, accepted: bool) {
        let counter = if accepted {
            &self.replay_records_accepted
        } else {
            &self.replay_failures
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn publish(&self, replaces_other_directory: bool) {
        self.publications.fetch_add(1, Ordering::Relaxed);
        if replaces_other_directory {
            self.replacements.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn snapshot(&self) -> RetainedHistoryReadCounts {
        RetainedHistoryReadCounts {
            empty: self.empty.snapshot(),
            same_directory: self.same_directory.snapshot(),
            other_directory: self.other_directory.snapshot(),
            unavailable: self.unavailable.snapshot(),
            journal_read_attempts: self.journal_read_attempts.load(Ordering::Relaxed),
            replay_records_attempted: self.replay_records_attempted.load(Ordering::Relaxed),
            replay_records_accepted: self.replay_records_accepted.load(Ordering::Relaxed),
            replay_failures: self.replay_failures.load(Ordering::Relaxed),
            publications: self.publications.load(Ordering::Relaxed),
            replacements: self.replacements.load(Ordering::Relaxed),
        }
    }
}
