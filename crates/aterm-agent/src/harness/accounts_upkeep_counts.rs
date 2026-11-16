// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the accounts' upkeep did in this process, counted: its passes, the other CLIs' pool
//! syncs it ran and how many of them held a receipt guard, and the receipt admissions asked of
//! a source. Each count only rises, from the process's start; a reader takes two readings and
//! subtracts. They decide nothing: no sync, admission or schedule reads them.

use std::sync::atomic::{AtomicU64, Ordering};

/// One counted event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// The upkeep was entered (`accounts_sync::upkeep`), whatever it came to: a pass that
    /// only renews logins and syncs no pool, and one the accounts' off switch ends, count too.
    Pass,
    /// The upkeep ran one other CLI's pool sync (`accounts_sync::upkeep_pools`): not one the
    /// quiet rule held back (`accounts_sync::default_sync_holds`).
    PoolSync,
    /// One of those syncs reached its roster's publication holding a receipt guard: the lab is
    /// that CLI's chosen source (`ProviderSource::receipt_guard`).
    PoolSyncGuarded,
    /// A publication asked its source's receipt guard to admit it
    /// (`accounts::Store::publish_plan_scoped`), whoever in this process published.
    ReceiptAdmission,
}

const EVENTS: usize = 4;

static COUNTS: [AtomicU64; EVENTS] = [const { AtomicU64::new(0) }; EVENTS];

#[cfg(test)]
thread_local! {
    /// The calling thread's own counts ([`counts_here`]).
    static HERE: [std::cell::Cell<u64>; EVENTS] = const { [const { std::cell::Cell::new(0) }; EVENTS] };
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
}

impl UpkeepCounts {
    fn of(read: impl Fn(Event) -> u64) -> Self {
        Self {
            passes: read(Event::Pass),
            pool_syncs: read(Event::PoolSync),
            pool_syncs_guarded: read(Event::PoolSyncGuarded),
            receipt_admissions: read(Event::ReceiptAdmission),
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
        (Event::PoolSync, by.pool_syncs),
        (Event::PoolSyncGuarded, by.pool_syncs_guarded),
        (Event::ReceiptAdmission, by.receipt_admissions),
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
