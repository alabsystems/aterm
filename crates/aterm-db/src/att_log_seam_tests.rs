// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A log append that keys each entry by the millisecond its row states (`at`), as a writer
//! that reads its keys off a clock would: the negative control of the exit test of
//! `lab/tracker`'s logs (`file::tests`).

use std::cell::Cell;

thread_local! {
    static BY_CLOCK: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn keys_by_clock() -> bool {
    BY_CLOCK.with(Cell::get)
}

/// Key this thread's log appends by their rows' `at` until the guard drops.
pub(crate) fn key_by_clock() -> ByClock {
    BY_CLOCK.with(|on| on.set(true));
    ByClock(())
}

pub(crate) struct ByClock(());

impl Drop for ByClock {
    fn drop(&mut self) {
        BY_CLOCK.with(|on| on.set(false));
    }
}
