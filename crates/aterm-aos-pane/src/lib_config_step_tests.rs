// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static KEPT: Cell<bool> = const { Cell::new(false) };
}

/// Moves the state's config again when dropped.
pub(crate) struct Kept;

impl Drop for Kept {
    fn drop(&mut self) {
        KEPT.with(|k| k.set(false));
    }
}

/// Leave the state's config on every later config install on this thread, until the returned guard drops.
pub(crate) fn keep_old() -> Kept {
    KEPT.with(|k| k.set(true));
    Kept
}

pub(super) fn kept() -> bool {
    KEPT.with(Cell::get)
}
