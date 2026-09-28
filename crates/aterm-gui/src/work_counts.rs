// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic WORK counters for the many-tab / many-session passes whose
//! wins are structural (docs/PERF-REGRESSION-DEFENCE.md, W-4) — the
//! `aterm_grid::test_counters` idiom, for this crate.
//!
//! Each counter sits at the site that does the WORK a win removed, never at
//! the site that makes the decision: a counter on the decision reads the same
//! whether or not the work behind it came back. The asserting tests live in
//! `work_count_tests.rs` and ride the merge contract's test stage, which no
//! timing lane does.
//!
//! Thread-local, so parallel tests cannot bleed into each other; every counted
//! site runs on the calling (test) thread. Compiled only under `cfg(test)`.

use std::cell::Cell;

thread_local! {
    /// Layout plans built (`App::plan_tab`): the redraw plan-build
    /// cardinality, and MPT-2's discarded background-tab plans.
    static PLANS_BUILT: Cell<usize> = const { Cell::new(0) };
    /// MPT-3: output stamps touched (swapped or cleared) by the present walk.
    static LATENCY_STAMPS_TOUCHED: Cell<usize> = const { Cell::new(0) };
    /// MPT-4: sessions probed by the status sweep.
    static STATUS_PROBES: Cell<usize> = const { Cell::new(0) };
    /// MPT-5: tabs walked by the title-drift gate.
    static TITLE_TAB_TOUCHES: Cell<usize> = const { Cell::new(0) };
    /// The subscribe idle wake: retained ledger records (shell blocks, turn
    /// records, timeline events) an `events` digest touched.
    static RETAINED_RECORDS_TOUCHED: Cell<usize> = const { Cell::new(0) };
}

fn bump(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    counter.with(|c| c.set(c.get() + 1));
}

fn take(counter: &'static std::thread::LocalKey<Cell<usize>>) -> usize {
    counter.with(|c| c.replace(0))
}

pub(crate) fn plan_built() {
    bump(&PLANS_BUILT);
}
pub(crate) fn take_plans_built() -> usize {
    take(&PLANS_BUILT)
}
pub(crate) fn plans_built() -> usize {
    PLANS_BUILT.with(Cell::get)
}

pub(crate) fn latency_stamp_touched() {
    bump(&LATENCY_STAMPS_TOUCHED);
}
pub(crate) fn take_latency_stamps_touched() -> usize {
    take(&LATENCY_STAMPS_TOUCHED)
}

pub(crate) fn status_probe() {
    bump(&STATUS_PROBES);
}
pub(crate) fn take_status_probes() -> usize {
    take(&STATUS_PROBES)
}

pub(crate) fn title_tab_touched() {
    bump(&TITLE_TAB_TOUCHES);
}
pub(crate) fn take_title_tab_touches() -> usize {
    take(&TITLE_TAB_TOUCHES)
}

pub(crate) fn retained_record_touched() {
    bump(&RETAINED_RECORDS_TOUCHED);
}
pub(crate) fn take_retained_records_touched() -> usize {
    take(&RETAINED_RECORDS_TOUCHED)
}
