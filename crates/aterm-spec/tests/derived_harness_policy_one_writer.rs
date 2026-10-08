// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `HarnessPolicyOneWriter` (2026-09-29): the supervisor's
//! `[harness]` policy has one writer, the config watcher. Twin of the Clean
//! island in aterm-gui's `config_watcher_island.rs`; Tier-1 is
//! `config_watcher::tests::the_watcher_is_the_policys_one_writer`.

use aterm_spec::derive::{harness_policy_one_writer_liveness, harness_policy_one_writer_model};
use aterm_spec::verify;

/// SAFETY: proved at `Buggy = 0` by the interpreter and, where installed, by
/// `ty`; at `Buggy = 1` a counterexample to `Agrees` is required. Each mutant
/// is a negative control caught alone.
#[test]
fn harness_policy_one_writer_proves_and_catches_the_second_writer() {
    let model = harness_policy_one_writer_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the one-writer model must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness policy one writer");
    let dead = ["SecondWriterOff", "SecondWriterOn", "CommitWithoutForget"];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &dead),
        Ok(dead.len()),
        "every mutant is a negative control caught alone"
    );
}

/// LIVENESS: the window catches up with the file (`[]<>`) under weak fairness
/// of the watcher's poll, the window's delivery and the watcher's forget —
/// each load-bearing — proved by the interpreter and by `ty`, and broken by
/// each mutant alone.
#[test]
fn the_window_catches_up_with_the_file() {
    verify::liveness_proves_and_catches_tiered(
        &harness_policy_one_writer_model(),
        &harness_policy_one_writer_liveness(),
        "harness policy one writer",
    );
}
