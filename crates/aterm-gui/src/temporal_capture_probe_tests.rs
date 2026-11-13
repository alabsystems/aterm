// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static CAPTURED: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn captured() {
    CAPTURED.with(|n| n.set(n.get() + 1));
}

/// Keyframes captured on this thread so far.
pub(super) fn count() -> usize {
    CAPTURED.with(Cell::get)
}
