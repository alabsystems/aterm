// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

std::thread_local! {
    static LAST: Cell<Option<super::ExecStatusCarrier>> = const { Cell::new(None) };
}

pub(super) fn note(carrier: super::ExecStatusCarrier) {
    LAST.with(|last| last.set(Some(carrier)));
}

/// The carrier noted last on this thread; none is noted after it.
pub(super) fn take() -> Option<super::ExecStatusCarrier> {
    LAST.with(Cell::take)
}
