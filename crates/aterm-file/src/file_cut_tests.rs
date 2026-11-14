// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

thread_local! {
    static LEFT: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Let this thread's next `writes` writes land and fail every one after, as a process
/// killed there makes none; `None` lets every write land again.
pub(crate) fn after(writes: Option<u32>) {
    LEFT.with(|left| left.set(writes));
}

pub(crate) fn stops() -> bool {
    LEFT.with(|left| match left.get() {
        None => false,
        Some(0) => true,
        Some(n) => {
            left.set(Some(n - 1));
            false
        }
    })
}
