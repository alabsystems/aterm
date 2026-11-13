// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::Cell;

thread_local! {
    static START: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

pub(super) fn set(at: Option<std::time::Instant>) {
    START.with(|start| start.set(at));
}

pub(super) fn get() -> Option<std::time::Instant> {
    START.with(Cell::get)
}
