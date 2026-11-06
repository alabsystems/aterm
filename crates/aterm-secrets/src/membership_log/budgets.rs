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
/// The keyed records past which an automatic batch no writer waits on waits for checkpoints
/// (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §8, "The 8,500 stop"): the ranked tender's re-key
/// of an audience that no held `Remove` made due (a coalesce of concurrent heads after no
/// removal, an F21 reseal's re-`Put`s) is not signed where the log's keyed records and the
/// batch's planned ones together would pass it, or where it would take the room the re-keys a
/// held `Remove` made due still owe, and stays due. A heal, a person's act and every batch a held
/// `Remove` made due never wait on the stop: writers wait on those (one-mesh M4), and their room
/// is kept instead, as each way a person removes a Mac (`members remove`, `secrets revoke`, a
/// `code-only` line) refuses up front where its own batch and the worst case of every re-key owed
/// after it would pass [`MAX_KEYED_RECORDS`] or the byte bound (`client/remove.rs`'s
/// `RemovalFill`), a heal signs only the `Wrap`s that leave that room free, and a seat move is
/// signed only below the proof-admission stop. The stop leaves 3,788 keyed records under that bound,
/// about 57 removals' worth at the 66 records one costs once the fold lands; the proof-admission
/// stop (`client/readmit.rs`'s `PROVEN_FILL` of the keyed budget, 9,216) lies between.
/// [`RecordBudgets::automatic_keyed_stop`] scales it to a test's smaller budgets.
pub const AUTOMATIC_KEYED_STOP: usize = 8_500;
const _: () = assert!(AUTOMATIC_KEYED_STOP < MAX_KEYED_RECORDS);
/// The most records a 0.146-0.152 build takes: it reads a records stream (a pull's answer, a
/// POST, a records file) and a Commons control inventory (`commons::journal::MAX_NODES`) of at
/// most this many, and refuses a longer one whole.
pub const LEGACY_RECORDS: usize = 4096;
#[cfg(unix)]
const _: () = assert!(LEGACY_RECORDS == crate::commons::journal::MAX_NODES);
/// The most records a [`Log`](super::Log) holds in all, which a [`Store`](super::Store) counts
/// its waiting records in too, and the record bound of every stream it reads or writes.
///
/// Modern peers negotiate this capacity before exchanging crossed history. Legacy wires keep
/// [`LEGACY_RECORDS`], and the local storage layout pins its required reader capacity before
/// publishing larger custody. Both disjoint budgets can therefore use their full share.
pub const MAX_HELD_RECORDS: usize = MAX_WEIGHED_RECORDS + MAX_KEYED_RECORDS;

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

#[cfg(test)]
thread_local! {
    /// MUTANT of [`counted`], on for one test thread: every record counts as weighed, as a log
    /// that judged the keyed kinds by the weighed budget would.
    pub(crate) static KEYED_AS_WEIGHED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// What one record of `kind` adds to a [`Log`](super::Log)'s counts, held or judged against its
/// budgets: [`RecordCounts::of`]. Every count the log keeps, and every record it judges as it
/// inserts or lands one, goes through it; a batch a caller counts by hand and judges against
/// the log's counts ([`Log::passed_by`](super::Log::passed_by) for a proof's admission,
/// [`RecordBudgets::passed_by`] for a Commons recovery) does not.
pub(super) fn counted(kind: Kind) -> RecordCounts {
    #[cfg(test)]
    if KEYED_AS_WEIGHED.with(std::cell::Cell::get) {
        return RecordCounts::of(Kind::Admit);
    }
    RecordCounts::of(kind)
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
    /// The byte bound [`RecordBudgets::within_bytes`] set on this thread.
    static WITHIN_BYTES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The wire bytes a [`Log`](super::Log)'s held records stay within ([`Budget::Bytes`]):
/// [`MAX_RECORDS_STREAM`], the bound of every stream a member reads, or in a test or debug build
/// the smaller one [`RecordBudgets::within_bytes`] set on this thread, so a test fills it with a
/// few records.
#[must_use]
pub fn held_bytes_bound() -> usize {
    #[cfg(any(test, debug_assertions))]
    if let Some(bytes) = WITHIN_BYTES.with(std::cell::Cell::get) {
        return bytes;
    }
    MAX_RECORDS_STREAM
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
    /// The held total stays within the two budgets together; tests can choose a limit on either
    /// side of [`LEGACY_RECORDS`] (`crate::crossing`). Network streams stay bounded by
    /// [`MAX_HELD_RECORDS`]. Local-file recovery uses the receiving store's held budget, so such
    /// a log can be written and reopened without losing its tail.
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

    /// Run `run` with every [`Log`](super::Log) on this thread holding its records within
    /// `bytes` wire bytes ([`held_bytes_bound`]) rather than [`MAX_RECORDS_STREAM`]: a test's
    /// smaller byte bound, which a few large records fill. The bound before comes back when `run`
    /// returns or unwinds.
    ///
    /// # Panics
    /// `bytes` is larger than [`MAX_RECORDS_STREAM`].
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn within_bytes<T>(bytes: usize, run: impl FnOnce() -> T) -> T {
        struct Restore(Option<usize>);
        impl Drop for Restore {
            fn drop(&mut self) {
                WITHIN_BYTES.with(|within| within.set(self.0));
            }
        }
        assert!(
            bytes <= MAX_RECORDS_STREAM,
            "a test's byte bound stays within the production one"
        );
        let _restore = Restore(WITHIN_BYTES.with(|within| within.replace(Some(bytes))));
        run()
    }

    /// [`AUTOMATIC_KEYED_STOP`] scaled to this keyed budget as it is to [`MAX_KEYED_RECORDS`],
    /// rounded down: exactly 8,500 under [`RecordBudgets::PRODUCTION`], and the same share of a
    /// test's smaller budget ([`RecordBudgets::within`]), so the stop lies as far below the hard
    /// bound there as it does in the lab.
    #[must_use]
    pub const fn automatic_keyed_stop(self) -> usize {
        self.keyed.saturating_mul(AUTOMATIC_KEYED_STOP) / MAX_KEYED_RECORDS
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
    /// together pass [`held_bytes_bound`] ([`MAX_RECORDS_STREAM`]).
    #[must_use]
    pub fn passed_with_bytes(
        self,
        held: RecordCounts,
        more: RecordCounts,
        held_bytes: usize,
        more_bytes: usize,
    ) -> Option<Budget> {
        match self.passed_by(held, more) {
            Some(budget) => Some(budget),
            None if held_bytes.saturating_add(more_bytes) > held_bytes_bound() => {
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
