// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static LINEAR: Cell<bool> = const { Cell::new(false) };
}

/// Puts the lookups back on their sorted search when dropped.
pub(crate) struct Linear;

impl Drop for Linear {
    fn drop(&mut self) {
        LINEAR.with(|l| l.set(false));
    }
}

/// Look keyframes and witnesses up by linear scan on this thread until the returned guard drops.
pub(crate) fn scan_linearly() -> Linear {
    LINEAR.with(|l| l.set(true));
    Linear
}

pub(super) fn linear() -> bool {
    LINEAR.with(Cell::get)
}
