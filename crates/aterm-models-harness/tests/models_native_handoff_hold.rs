// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_harness::native_handoff_hold_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;

#[test]
fn each_handoff_hold_law_has_a_falsifying_control() {
    let base = native_handoff_hold_model();
    aterm_spec::interp::prove_and_catch(&base);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&base));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    let granted: &[&str] = &["Reserve", "GrantAck", "Deliver"];
    for (bug, law, tail) in [
        (
            1,
            "NoCommitPastClose",
            &["Expire", "PassClose", "Commit"][..],
        ),
        (2, "NoCloseBeforeClose", &["Sweep"][..]),
        (2, "ReleaseOnlyAnswerable", &["Sweep"][..]),
        (
            3,
            "ReleaseOnlyAnswerable",
            &["Expire", "ReleaseUngranted"][..],
        ),
        (
            3,
            "HandoffNeverCutsAnOpenGrant",
            &["Expire", "ReleaseUngranted", "Latch", "Handoff"][..],
        ),
        (4, "HandoffNeverCutsAnOpenGrant", &["Latch", "Handoff"][..]),
        (
            5,
            "ClosedAnswerOnlyPastClose",
            &["Commit", "Latch", "Handoff", "Answer"][..],
        ),
        (
            6,
            "SealedEndReleases",
            &["Expire", "PassClose", "Sweep"][..],
        ),
    ] {
        let path: Vec<&str> = granted.iter().chain(tail).copied().collect();
        let mutant = aterm_spec::interp::with_buggy(&base, bug);
        let mut bad = mutant.init_state();
        let mut caught = false;
        for action in &path {
            assert!(mutant.fire(action, &mut bad), "mutant {bug}: {action}");
            caught |= !base.check_invariant(law, &bad);
        }
        assert!(caught, "mutant {bug} must violate {law}");
        let mut good = base.init_state();
        let refused = path.iter().any(|action| !base.fire(action, &mut good));
        assert!(
            refused || base.check_invariant(law, &good),
            "the correct host refuses mutant {bug}'s step or keeps {law}"
        );
    }
}

/// Past the close, a hold whose grant append is a fact always has a release enabled: the close
/// seals one whose grant may have run, and one that never granted starts nothing more. A slow
/// commit is enabled no longer.
fn release_enabled_past_close(model: &Model) -> Result<usize, State> {
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(model) {
        if state["epoch"] != 1 || state["held"] != 1 || state["closed"] != 1 {
            continue;
        }
        if state["writing"] == 1 {
            assert!(model.action_enabled("GrantAck", &state), "{state:?}");
            continue;
        }
        checked += 1;
        assert!(!model.action_enabled("Commit", &state), "{state:?}");
        let enabled = |name: &str| {
            model.actions.iter().any(|action| action.name == name)
                && model.action_enabled(name, &state)
        };
        if !enabled("Sweep") && !enabled("ReleaseUngranted") {
            return Err(state);
        }
    }
    Ok(checked)
}

#[test]
fn past_its_close_every_hold_has_a_release_enabled() {
    let base = native_handoff_hold_model();
    let checked = release_enabled_past_close(&base).expect("a release past the close");
    assert!(checked > 0, "some hold reaches its close");
    // Negative control: without the close's seal, a granted operation past its close holds the
    // handoff with nothing left to release it, the defect of ATERM-KI-1112-151.
    let mut unclosed = base.clone();
    unclosed.actions.retain(|action| action.name != "Sweep");
    let stuck = release_enabled_past_close(&unclosed).expect_err("a hold nothing releases");
    assert_eq!((stuck["granted"], stuck["outcome"]), (1, 0), "{stuck:?}");
}
