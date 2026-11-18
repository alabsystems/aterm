// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the accounts' upkeep did in this process, counted: its passes, all of them and those of
//! each caller (the window's accounts' keeper, a session's thread, the network move's thread), the
//! other CLIs' pool syncs it ran and how many of them held a receipt guard, the receipt
//! admissions asked of a source, and what the keeper's verdicts made of each duty of its passes
//! (`accounts_upkeep_memo`). Each count only rises, from the process's start; a reader takes two
//! readings and subtracts. They decide nothing: no sync, admission, schedule or verdict reads
//! them.

use std::sync::atomic::{AtomicU64, Ordering};

/// One counted event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// The upkeep was entered (`accounts_sync::upkeep`), whatever it came to: a pass that
    /// only renews logins and syncs no pool, and one the accounts' off switch ends, count too.
    Pass,
    /// Of those, one entered on the keeper's thread (`aterm_uds::trace::in_keeper`).
    PassKeeper,
    /// Of those, one entered on the thread that answers the window's network moves
    /// ([`mark_moved_thread`]).
    PassMoved,
    /// Of those, one entered on any other thread: a session's, a verb's.
    PassSession,
    /// The upkeep ran one other CLI's pool sync (`accounts_sync::upkeep_pools`): not one whose
    /// duty's kept verdict stood (`accounts_upkeep_memo`).
    PoolSync,
    /// One of those syncs reached its roster's publication holding a receipt guard: the lab is
    /// that CLI's chosen source (`ProviderSource::receipt_guard`).
    PoolSyncGuarded,
    /// A publication asked its source's receipt guard to admit it
    /// (`accounts::Store::publish_plan_scoped`), whoever in this process published.
    ReceiptAdmission,
    /// A duty of a pass on the keeper's thread ran its work (`accounts_upkeep_memo::run`): it
    /// had no kept verdict, or its verdict no longer stood.
    DutyRun,
    /// A duty of a pass on the keeper's thread was skipped: its kept verdict stood.
    DutySkipped,
    /// A duty's run was kept as its verdict: it did nothing and read only what its trace
    /// replays.
    DutyKept,
    /// A duty's run was poisoned: it used something its trace cannot replay (a raw read of the
    /// clock among them), so it runs at every pass.
    DutyPoisoned,
    /// An account store's writer lock was taken (`accounts::Store::acquire` and its bounded
    /// forms), by any caller in this process ([`store_locks`]).
    StoreLock,
}

const EVENTS: usize = 12;

static COUNTS: [AtomicU64; EVENTS] = [const { AtomicU64::new(0) }; EVENTS];

#[cfg(test)]
thread_local! {
    /// The calling thread's own counts ([`counts_here`]).
    static HERE: [std::cell::Cell<u64>; EVENTS] = const { [const { std::cell::Cell::new(0) }; EVENTS] };
}

thread_local! {
    /// Whether this thread answers the window's network moves ([`mark_moved_thread`]).
    static MOVED_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the calling thread as the one that answers the window's network moves, for its life: a
/// pass it enters counts as [`Event::PassMoved`].
pub fn mark_moved_thread() {
    MOVED_THREAD.set(true);
}

/// Count one pass of the upkeep entered on this thread: [`Event::Pass`], and the caller's
/// ([`Event::PassKeeper`], [`Event::PassMoved`] or [`Event::PassSession`]).
pub(crate) fn count_pass() {
    count(Event::Pass);
    count(if aterm_uds::trace::in_keeper() {
        Event::PassKeeper
    } else if MOVED_THREAD.get() {
        Event::PassMoved
    } else {
        Event::PassSession
    });
}

/// Count `event` once.
pub(crate) fn count(event: Event) {
    COUNTS[event as usize].fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    HERE.with(|here| {
        let cell = &here[event as usize];
        cell.set(cell.get() + 1);
    });
}

/// The accounts' upkeep's counts in this process since it started ([`counts`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpkeepCounts {
    /// Upkeep passes entered, whatever each came to: those that only renew logins and those
    /// the accounts' off switch ends are among them, so this is not the number of passes that
    /// synced.
    pub passes: u64,
    /// Of the passes, those the window's accounts' keeper entered: at its first turn, at each
    /// forced one, and at a turn where its last pass's trace no longer stood (something it read
    /// moved, or the wall clock reached a second it compared) or was not kept and its retry came
    /// due; never at a turn where that trace stands.
    pub passes_keeper: u64,
    /// Of the passes, those a session's thread (or any other caller's) entered.
    pub passes_session: u64,
    /// Of the passes, those the network move's thread entered ([`mark_moved_thread`]).
    pub passes_moved: u64,
    /// Pool syncs of the other CLIs the upkeep ran.
    pub pool_syncs: u64,
    /// Those of them that reached their roster's publication holding a receipt guard. A sync
    /// that ends before that publication (a pool with no row and no roster here, a busy lock,
    /// a refused source) is not among them, though it may have held the guard.
    pub pool_syncs_guarded: u64,
    /// Publications that asked their source's receipt guard to admit them, by any caller in
    /// this process. In a window the guard is the lab cache's, whose admission reads the
    /// registry strictly (`aterm_db::alab::admission_counts` counts what it read). It is not
    /// every strict read a sync can cause: the capture of its source refreshes the lab view,
    /// which reads the registry's mail metadata strictly where its held lab does not stand
    /// (`aterm_db::alab::AdmissionCounts::observations`).
    pub receipt_admissions: u64,
    /// Duties of the keeper's passes whose work ran: those with no kept verdict, or whose
    /// verdict no longer stood.
    pub duty_runs: u64,
    /// Duties of the keeper's passes skipped because their kept verdict stood.
    pub duty_skips: u64,
    /// Of the duty runs, those kept as their duty's verdict.
    pub duty_kept: u64,
    /// Of the duty runs, those poisoned: each such duty runs at every pass.
    pub duty_poisoned: u64,
}

impl UpkeepCounts {
    fn of(read: impl Fn(Event) -> u64) -> Self {
        Self {
            passes: read(Event::Pass),
            passes_keeper: read(Event::PassKeeper),
            passes_session: read(Event::PassSession),
            passes_moved: read(Event::PassMoved),
            pool_syncs: read(Event::PoolSync),
            pool_syncs_guarded: read(Event::PoolSyncGuarded),
            receipt_admissions: read(Event::ReceiptAdmission),
            duty_runs: read(Event::DutyRun),
            duty_skips: read(Event::DutySkipped),
            duty_kept: read(Event::DutyKept),
            duty_poisoned: read(Event::DutyPoisoned),
        }
    }
}

/// The counts now.
#[must_use]
pub fn counts() -> UpkeepCounts {
    UpkeepCounts::of(|event| COUNTS[event as usize].load(Ordering::Relaxed))
}

/// Add `by` to the process's counts ([`counts`]), so a test of a reader in another crate (the
/// window's `metrics`) can tell each count from the others. The calling thread's own counts
/// are left as they were: nothing happened.
#[cfg(any(test, debug_assertions))]
pub fn add_for_test(by: UpkeepCounts) {
    for (event, n) in [
        (Event::Pass, by.passes),
        (Event::PassKeeper, by.passes_keeper),
        (Event::PassSession, by.passes_session),
        (Event::PassMoved, by.passes_moved),
        (Event::PoolSync, by.pool_syncs),
        (Event::PoolSyncGuarded, by.pool_syncs_guarded),
        (Event::ReceiptAdmission, by.receipt_admissions),
        (Event::DutyRun, by.duty_runs),
        (Event::DutySkipped, by.duty_skips),
        (Event::DutyKept, by.duty_kept),
        (Event::DutyPoisoned, by.duty_poisoned),
    ] {
        COUNTS[event as usize].fetch_add(n, Ordering::Relaxed);
    }
}

/// [`counts`] of what the calling thread itself did, so a test reads its own counts beside the
/// tests that sync in the same binary: each event counts on the thread that made it.
#[cfg(test)]
pub(crate) fn counts_here() -> UpkeepCounts {
    UpkeepCounts::of(|event| HERE.with(|here| here[event as usize].get()))
}

/// The account store writer locks taken in this process since it started ([`Event::StoreLock`]),
/// kept beside [`counts`] and not in [`UpkeepCounts`]; the window's `metrics` row reports it as
/// `accounts_store_locks`. A pass whose duties all stood takes none.
#[must_use]
pub fn store_locks() -> u64 {
    COUNTS[Event::StoreLock as usize].load(Ordering::Relaxed)
}

/// [`store_locks`] taken by the calling thread itself ([`counts_here`]).
#[cfg(test)]
pub(crate) fn store_locks_here() -> u64 {
    HERE.with(|here| here[Event::StoreLock as usize].get())
}

#[cfg(test)]
#[path = "accounts_upkeep_counts_tests.rs"]
mod tests;
