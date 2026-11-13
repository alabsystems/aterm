// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static POOL_VISITS: Cell<u64> = const { Cell::new(0) };
    static ADMISSION_WINDOWS: Cell<u64> = const { Cell::new(0) };
    static AT_ADMISSION: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
}

/// One session yielded by `SessionPool::iter`.
pub(crate) fn note_pool_visit() {
    POOL_VISITS.with(|c| c.set(c.get().wrapping_add(1)));
}

/// One window examined by redraw admission.
pub(crate) fn note_admission_window() {
    ADMISSION_WINDOWS.with(|c| c.set(c.get().wrapping_add(1)));
}

/// Admission starts: record the pool walks and terminal locks so far.
pub(crate) fn note_admission_entry() {
    let locks = super::term_lock_acquisitions_on_this_thread();
    AT_ADMISSION.with(|c| c.set(Some((pool_visits(), locks))));
}

pub(crate) fn pool_visits() -> u64 {
    POOL_VISITS.with(Cell::get)
}

pub(crate) fn admission_windows() -> u64 {
    ADMISSION_WINDOWS.with(Cell::get)
}

/// `(pool visits, terminal locks)` as of the last admission entry, and forget it.
pub(crate) fn take_at_admission() -> Option<(u64, u64)> {
    AT_ADMISSION.with(Cell::take)
}
