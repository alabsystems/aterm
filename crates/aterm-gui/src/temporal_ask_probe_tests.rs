// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static RACE: Cell<bool> = const { Cell::new(false) };
}

/// Have the next ask a writer on this thread stores race a reader that counts a keyframe right after it.
pub(super) fn race_next() {
    RACE.with(|r| r.set(true));
}

/// Whether this ask races one; once per [`race_next`].
pub(super) fn races() -> bool {
    RACE.with(|r| r.replace(false))
}
