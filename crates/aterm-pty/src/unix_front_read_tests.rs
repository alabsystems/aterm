// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static STAND_IN: Cell<Option<libc::pid_t>> = const { Cell::new(None) };
}

/// This thread's next [`super::end_session_on_pty`] of a running leader reads its pty's
/// foreground group as `group`.
pub(crate) fn reads_as(group: libc::pid_t) {
    STAND_IN.with(|stand_in| stand_in.set(Some(group)));
}

/// The stand-in [`reads_as`] set, taken.
pub(super) fn stand_in() -> Option<libc::pid_t> {
    STAND_IN.with(Cell::take)
}
