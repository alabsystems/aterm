// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static CALLS: Cell<u64> = const { Cell::new(0) };
}

/// One `commandBuffer` mint or `nextDrawable` about to run on this thread.
pub(crate) fn note() {
    CALLS.with(|calls| calls.set(calls.get() + 1));
}

/// How many this thread has made so far.
pub(crate) fn calls() -> u64 {
    CALLS.with(Cell::get)
}
