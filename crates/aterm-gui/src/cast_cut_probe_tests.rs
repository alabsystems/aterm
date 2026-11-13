// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static WAITS: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn waited() {
    WAITS.with(|w| w.set(w.get() + 1));
}

/// Number of [`CastRecorder::snapshot_at`] waits recorded on this thread.
pub(crate) fn waits() -> usize {
    WAITS.with(Cell::get)
}
