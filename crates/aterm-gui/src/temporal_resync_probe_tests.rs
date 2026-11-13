// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static PROJECTED: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn projected() {
    PROJECTED.with(|n| n.set(n.get() + 1));
}

/// Keyframes projected on this thread so far.
pub(super) fn count() -> usize {
    PROJECTED.with(Cell::get)
}
