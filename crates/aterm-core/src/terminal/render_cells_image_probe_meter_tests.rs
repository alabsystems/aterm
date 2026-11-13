// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

thread_local! {
    static PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One extras lookup.
pub(super) fn charge() {
    PROBES.with(|c| c.set(c.get() + 1));
}

/// Read and reset, so each measured stretch starts from zero.
pub(super) fn take() -> usize {
    PROBES.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}
