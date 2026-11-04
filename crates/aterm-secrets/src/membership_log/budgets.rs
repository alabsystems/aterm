// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record budgets a membership [`Log`](super::Log) holds its records within: two disjoint
//! ones by what the quorum weighing reads, and the held total.
//!
//! - **Weighed** records ([`Kind::weighed`]: `Genesis`, `Admit`, `Remove`) are what the quorum
//!   weighing reads. Its candidates and their 1 MiB relation payload (`eval/weigh/relations.rs`)
//!   are sized by [`MAX_WEIGHED_RECORDS`], so no flood of other records grows the weighing past
//!   it.
//! - **Keyed** records (every other kind: `Epoch`, `Wrap`, `Put`, `Delete`, `Node`, `Name`) carry
//!   keys, secrets and names; the weighing never reads them. They hold at most
//!   [`MAX_KEYED_RECORDS`].
//! - **Held** is both together, [`MAX_HELD_RECORDS`]: also the record bound of every stream a
//!   member reads (a records file, a pull's answer, a POST).
//!
//! The two budgets keep a flood of one side from taking the other's room (no flood of keyed
//! records takes the place of a `Remove`, and no flood of `Admit`s and `Remove`s shuts out a
//! rotation's `Wrap`s) only where the held total is the two together. While it is less, it binds
//! first and refuses every kind alike once full; at [`LEGACY_RECORDS`], no more than either
//! budget, neither budget refuses a record before it does.
//!
//! [`RecordBudgets::PRODUCTION`] is the one set every member holds; a test makes smaller ones
//! with [`RecordBudgets::within`], so each budget can be filled alone.

use super::{Kind, MAX_RECORDS_STREAM};

/// The most weighed records ([`Kind::weighed`]) a [`Log`](super::Log) holds, the genesis among
/// them. Each quorum candidate is a group of at least two `Remove`s, so the weighing has at most
/// `(MAX_WEIGHED_RECORDS - 1) / 2` candidates, whose relations its 1 MiB payload is sized for.
pub const MAX_WEIGHED_RECORDS: usize = 4096;
/// The most keyed records (every kind [`Kind::weighed`] is not) a [`Log`](super::Log) holds.
pub const MAX_KEYED_RECORDS: usize = 12_288;
/// The most records a 0.146-0.151 build takes: it reads a records stream (a pull's answer, a
/// POST, a records file) and a Commons control inventory (`commons::journal::MAX_NODES`) of at
/// most this many, and refuses a longer one whole.
pub const LEGACY_RECORDS: usize = 4096;
#[cfg(unix)]
const _: () = assert!(LEGACY_RECORDS == crate::commons::journal::MAX_NODES);
/// The most records a [`Log`](super::Log) holds in all, which a [`Store`](super::Store) counts
/// its waiting records in too, and the record bound of every stream it reads or writes.
///
/// It is [`LEGACY_RECORDS`] while members still serve and take records from 0.146-0.151 builds:
/// those refuse a whole stream past it, so a member's log past it would cut each of them off at
/// once, part-way through the batch that crossed it. The held total may pass it only once a
/// member refuses those builds before its log crosses (no stream of a crossed log reaches one)
/// and members trade records on a wire with no inventory bound; it is never more than the two
/// budgets together.
pub const MAX_HELD_RECORDS: usize = LEGACY_RECORDS;
const _: () = assert!(MAX_HELD_RECORDS <= MAX_WEIGHED_RECORDS + MAX_KEYED_RECORDS);

impl Kind {
    /// Whether the quorum weighing reads records of this kind, which count against the weighed
    /// budget ([`MAX_WEIGHED_RECORDS`]); every other kind counts against the keyed one
    /// ([`MAX_KEYED_RECORDS`]). Every kind is named, so a new one is classified where it is
    /// added.
    #[must_use]
    pub const fn weighed(self) -> bool {
        match self {
            Kind::Genesis | Kind::Admit | Kind::Remove => true,
            Kind::Name | Kind::Epoch | Kind::Wrap | Kind::Put | Kind::Delete | Kind::Node => false,
        }
    }
}

/// One bound a [`Log`](super::Log) holds its records within: a [`RecordBudgets`] budget, or the
/// byte bound of [`MAX_RECORDS_STREAM`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Budget {
    /// Every held record ([`RecordBudgets::held`]).
    Held,
    /// The weighed records ([`RecordBudgets::weighed`]).
    Weighed,
    /// The keyed records ([`RecordBudgets::keyed`]).
    Keyed,
    /// The held records' wire bytes ([`MAX_RECORDS_STREAM`]).
    Bytes,
}

impl Budget {
    /// What the budget counts, as a refusal names it.
    #[must_use]
    pub const fn counts(self) -> &'static str {
        match self {
            Budget::Held => "records",
            Budget::Weighed => "weighed records (Genesis, Admit, Remove)",
            Budget::Keyed => "keyed records (Epoch, Wrap, Put, Delete, Node, Name)",
            Budget::Bytes => "record bytes",
        }
    }
}

/// How many records count against each budget: held ones, or a batch about to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordCounts {
    /// Records of a kind [`Kind::weighed`] names.
    pub weighed: usize,
    /// Records of every other kind.
    pub keyed: usize,
}

impl RecordCounts {
    /// One record of `kind`.
    #[must_use]
    pub const fn of(kind: Kind) -> Self {
        if kind.weighed() {
            Self {
                weighed: 1,
                keyed: 0,
            }
        } else {
            Self {
                weighed: 0,
                keyed: 1,
            }
        }
    }

    /// Both counts together.
    #[must_use]
    pub const fn held(self) -> usize {
        self.weighed.saturating_add(self.keyed)
    }

    /// These counts and `more`'s.
    #[must_use]
    pub const fn plus(self, more: Self) -> Self {
        Self {
            weighed: self.weighed.saturating_add(more.weighed),
            keyed: self.keyed.saturating_add(more.keyed),
        }
    }
}

/// The record budgets of one [`Log`](super::Log), which it keeps for its life (and its clones
/// keep): the weighed and keyed budgets, disjoint by [`Kind::weighed`], and the held total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordBudgets {
    /// The most weighed records held ([`MAX_WEIGHED_RECORDS`]).
    pub weighed: usize,
    /// The most keyed records held ([`MAX_KEYED_RECORDS`]).
    pub keyed: usize,
    /// The most records held in all ([`MAX_HELD_RECORDS`]); a [`Store`](super::Store) counts
    /// its records waiting on a parent in it too.
    pub held: usize,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The budgets [`RecordBudgets::within`] set on this thread.
    static WITHIN: std::cell::Cell<Option<RecordBudgets>> = const { std::cell::Cell::new(None) };
}

impl RecordBudgets {
    /// The budgets every member holds its log within.
    pub const PRODUCTION: Self = Self {
        weighed: MAX_WEIGHED_RECORDS,
        keyed: MAX_KEYED_RECORDS,
        held: MAX_HELD_RECORDS,
    };

    /// The budgets a log or store made now takes: [`RecordBudgets::PRODUCTION`], or in a test or
    /// debug build the ones [`RecordBudgets::within`] set on this thread.
    #[must_use]
    pub fn current() -> Self {
        #[cfg(any(test, debug_assertions))]
        if let Some(budgets) = WITHIN.with(std::cell::Cell::get) {
            return budgets;
        }
        Self::PRODUCTION
    }

    /// Run `run` with every [`Log`](super::Log) and [`Store`](super::Store) it makes on this
    /// thread held within these budgets: a test's small ones, so each budget fills alone, through
    /// whatever loads or builds a log (a records file, a removal plan, a proof's admission). The
    /// budgets before come back when `run` returns or unwinds.
    ///
    /// The held total may be raised past [`MAX_HELD_RECORDS`], up to the two budgets together
    /// (where the activation slice puts it), so a test can hold a log past [`LEGACY_RECORDS`]
    /// (`crate::crossing`). Network streams stay bounded by [`MAX_HELD_RECORDS`]. Local-file
    /// recovery uses the receiving store's held budget, so such a log can be written and
    /// reopened without losing its tail.
    ///
    /// # Panics
    /// The weighed or keyed budget is larger than [`RecordBudgets::PRODUCTION`]'s (the weighing
    /// is sized for those), or the held total is larger than the two together.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn within<T>(self, run: impl FnOnce() -> T) -> T {
        struct Restore(Option<RecordBudgets>);
        impl Drop for Restore {
            fn drop(&mut self) {
                WITHIN.with(|within| within.set(self.0));
            }
        }
        assert!(
            self.weighed <= MAX_WEIGHED_RECORDS
                && self.keyed <= MAX_KEYED_RECORDS
                && self.held <= MAX_WEIGHED_RECORDS + MAX_KEYED_RECORDS,
            "a test's record budgets stay within the production weighed and keyed ones"
        );
        let _restore = Restore(WITHIN.with(|within| within.replace(Some(self))));
        run()
    }

    /// The first budget `more` records past `held` would pass, the held total first; `None` where
    /// they fit every one. A budget `more` adds nothing to is not judged.
    #[must_use]
    pub const fn passed_by(self, held: RecordCounts, more: RecordCounts) -> Option<Budget> {
        let after = held.plus(more);
        if more.held() > 0 && after.held() > self.held {
            Some(Budget::Held)
        } else if more.weighed > 0 && after.weighed > self.weighed {
            Some(Budget::Weighed)
        } else if more.keyed > 0 && after.keyed > self.keyed {
            Some(Budget::Keyed)
        } else {
            None
        }
    }

    /// [`RecordBudgets::passed_by`], and [`Budget::Bytes`] where `held_bytes` and `more_bytes`
    /// together pass [`MAX_RECORDS_STREAM`].
    #[must_use]
    pub const fn passed_with_bytes(
        self,
        held: RecordCounts,
        more: RecordCounts,
        held_bytes: usize,
        more_bytes: usize,
    ) -> Option<Budget> {
        match self.passed_by(held, more) {
            Some(budget) => Some(budget),
            None if held_bytes.saturating_add(more_bytes) > MAX_RECORDS_STREAM => {
                Some(Budget::Bytes)
            }
            None => None,
        }
    }
}

/// A test's mutant of [`Log::land_waiting`](super::Log), on for one test thread through
/// [`LANDING`].
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Landing {
    /// The landing as written.
    Sound,
    /// A ready record that did not land goes back to waiting, as a landing that only skips a
    /// record would leave it.
    KeepUnfit,
    /// Each pass takes its ready records in reverse id order.
    Reversed,
}

#[cfg(test)]
thread_local! {
    /// The [`Landing`] this test thread's logs land waiting records by.
    pub(super) static LANDING: std::cell::Cell<Landing> =
        const { std::cell::Cell::new(Landing::Sound) };
}

/// The order a pass of [`Log::land_waiting`](super::Log) takes its `ready` records in, given in
/// id order: reversed under [`Landing::Reversed`].
#[cfg(test)]
pub(super) fn mutant_order(mut ready: Vec<super::RecordId>) -> Vec<super::RecordId> {
    if LANDING.with(std::cell::Cell::get) == Landing::Reversed {
        ready.reverse();
    }
    ready
}

/// Under [`Landing::KeepUnfit`], the ready `record` that did not land goes back to waiting.
#[cfg(test)]
pub(super) fn mutant_keeps_unfit(log: &mut super::Log, record: super::Record) {
    if LANDING.with(std::cell::Cell::get) == Landing::KeepUnfit {
        log.wait(record);
    }
}

impl Default for RecordBudgets {
    /// [`RecordBudgets::current`].
    fn default() -> Self {
        Self::current()
    }
}
