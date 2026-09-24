// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the supervisor's machines (aterm-agent `supervise`): each
//! proves at `Buggy = 0` and yields its counterexample at `Buggy = 1`. The
//! Tier-1 binds drive the real `Session` (aterm-agent
//! `supervise/run_engine_tests.rs`).

#[test]
fn the_supervisor_claim_proves_and_catches_a_press_on_a_stale_view() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::supervisor_claim_model(),
        "supervisor claim: renew before a press",
    );
}

#[test]
fn the_focus_choice_proves_and_catches_an_enter_on_an_unconfirmed_focus() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::supervisor_focus_choice_model(),
        "supervisor focus choice: confirm before Enter",
    );
}

#[test]
fn the_turn_end_policy_proves_and_catches_a_continuation_into_a_box() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::supervisor_turn_end_model(),
        "supervisor turn end: never type under a box or a wall's wait",
    );
}
