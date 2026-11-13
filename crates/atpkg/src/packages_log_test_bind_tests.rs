// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::path::PathBuf;

thread_local! {
    static BOUND: RefCell<Option<(PathBuf, PathBuf)>> = const { RefCell::new(None) };
}

/// Log `prefix`'s events into `dir` on this thread until the guard drops.
pub(crate) fn bind(dir: PathBuf, prefix: PathBuf) -> Guard {
    BOUND.with(|b| *b.borrow_mut() = Some((dir, prefix)));
    Guard
}

pub(super) fn get() -> Option<(PathBuf, PathBuf)> {
    BOUND.with(|b| b.borrow().clone())
}

pub(crate) struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        BOUND.with(|b| *b.borrow_mut() = None);
    }
}
