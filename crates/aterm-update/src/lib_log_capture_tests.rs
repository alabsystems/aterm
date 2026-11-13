// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;

thread_local! {
    static LINES: RefCell<Vec<(aterm_log::Level, String)>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn record(level: aterm_log::Level, msg: &str) {
    LINES.with(|lines| lines.borrow_mut().push((level, msg.to_string())));
}

/// Drain this thread's captured lines.
pub(crate) fn take() -> Vec<(aterm_log::Level, String)> {
    LINES.with(|lines| std::mem::take(&mut *lines.borrow_mut()))
}
