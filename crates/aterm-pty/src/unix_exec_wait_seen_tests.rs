// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::time::Duration;

std::thread_local! {
    static LAST: Cell<Option<Duration>> = const { Cell::new(None) };
}

pub(super) fn note(wait: Duration) {
    LAST.with(|l| l.set(Some(wait)));
}

/// The last wait noted on this thread.
pub(crate) fn last() -> Option<Duration> {
    LAST.with(Cell::get)
}
