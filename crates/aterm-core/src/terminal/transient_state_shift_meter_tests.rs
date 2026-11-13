// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

thread_local! {
    static WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `n` events walked.
pub(super) fn charge(n: usize) {
    WALKED.with(|c| c.set(c.get() + n));
}

/// Read and reset, so each measured stretch starts from zero.
pub(in crate::terminal) fn take() -> usize {
    WALKED.with(|c| c.replace(0))
}
