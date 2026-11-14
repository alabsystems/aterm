// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static UNFLUSHED: Cell<bool> = const { Cell::new(false) };
}

/// The next outbox write on this thread lands and reports a failed directory flush.
pub(crate) fn unflush_next() {
    UNFLUSHED.with(|slot| slot.set(true));
}

pub(super) fn unflushed() -> bool {
    UNFLUSHED.with(|slot| slot.replace(false))
}
