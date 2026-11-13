// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static LIVE: Cell<bool> = const { Cell::new(false) };
}

/// Puts replays back on the recorded clock when dropped.
pub(crate) struct Live;

impl Drop for Live {
    fn drop(&mut self) {
        LIVE.with(|l| l.set(false));
    }
}

/// Fold every later replay on this thread with plain `process` (the live clock), until the returned guard drops.
pub(crate) fn on_live_clock() -> Live {
    LIVE.with(|l| l.set(true));
    Live
}

pub(super) fn live() -> bool {
    LIVE.with(Cell::get)
}
