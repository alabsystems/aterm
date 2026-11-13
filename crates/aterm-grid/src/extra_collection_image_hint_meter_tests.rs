// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

thread_local! {
    static VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn charge() {
    VISITS.with(|visits| visits.set(visits.get() + 1));
}

pub(crate) fn take() -> usize {
    VISITS.with(|visits| visits.replace(0))
}
