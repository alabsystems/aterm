// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static OPENED: Cell<u64> = const { Cell::new(0) };
}

/// One `blitCommandEncoder` about to open on this thread.
pub(crate) fn note() {
    OPENED.with(|opened| opened.set(opened.get() + 1));
}

/// How many this thread has opened so far.
pub(crate) fn opened() -> u64 {
    OPENED.with(Cell::get)
}
