// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fold that takes the reading whose Mac's clock read last: the negative control of the
//! `SharedFile` conformance's `Fold` and of `ai/limits`' exit test.

use std::cell::Cell;

thread_local! {
    static BY_CLOCK: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn by_clock() -> bool {
    BY_CLOCK.with(Cell::get)
}

/// Fold by clock on this thread until the guard drops.
pub(crate) fn fold_by_clock() -> ByClock {
    BY_CLOCK.with(|on| on.set(true));
    ByClock(())
}

pub(crate) struct ByClock(());

impl Drop for ByClock {
    fn drop(&mut self) {
        BY_CLOCK.with(|on| on.set(false));
    }
}
