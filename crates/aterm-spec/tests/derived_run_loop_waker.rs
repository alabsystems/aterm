// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the event loop's run-loop wake timer (the 2026-09-26 freeze).
//! Tier-1: `crates/aterm-objc/tests/conformance_wake_timer.rs`.

#[test]
fn run_loop_waker_proves_and_catches_the_spin_and_the_lost_wake() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::run_loop_waker_model(),
        "run-loop waker",
    );
}

/// Each invariant is caught by its OWN defect, not by the other one: the
/// historical interval spins but never loses a wake (the timer stays due), and
/// a plan deaf to the callout loses a wake but never spins.
#[test]
fn each_waker_invariant_is_caught_by_its_own_defect() {
    assert!(
        aterm_spec::verify::uncaught_invariants(&aterm_spec::derive::run_loop_waker_model())
            .is_empty(),
        "every run-loop waker invariant must be falsified by a Buggy = 1 member"
    );
}
