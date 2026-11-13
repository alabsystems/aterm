// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::Cell;

thread_local! {
    static RUN_ITERATIONS: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn count_run_iteration() {
    RUN_ITERATIONS.with(|counter| counter.set(counter.get() + 1));
}

pub(super) fn reset_run_iterations() {
    RUN_ITERATIONS.with(|counter| counter.set(0));
}

pub(super) fn take_run_iterations() -> usize {
    RUN_ITERATIONS.with(|counter| {
        let value = counter.get();
        counter.set(0);
        value
    })
}
