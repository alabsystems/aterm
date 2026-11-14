// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 state-space and negative-control checks for `aos_pane_faithful_replay_model`.

use std::collections::BTreeSet;

use aterm_models_aos::aos_pane_faithful_replay_model;
use aterm_spec::derive::{InvariantKind, Model};
use aterm_spec::interp::{self, State};
use aterm_spec::verify;

/// Each dial and the one law it is written to break.
const DIALS: [(i64, &str); 4] = [
    (1, "WitnessMatches"),
    (2, "RepliesMatch"),
    (3, "ClockRecorded"),
    (4, "FaithfulPasses"),
];

/// The verdicts, as the model numbers them.
const NO_VERDICT: i64 = 0;
const PASS: i64 = 1;
const FAIL_WITNESS: i64 = 2;
const FAIL_REPLIES: i64 = 3;
const FAIL_CLOCK: i64 = 4;

/// The state after firing `steps` in order from the initial state, each of which must be enabled.
fn walk(model: &Model, steps: &[&str]) -> State {
    let mut state = model.init_state();
    for (done, step) in steps.iter().enumerate() {
        assert!(
            model.fire(step, &mut state),
            "`{step}` is enabled after {:?}",
            &steps[..done]
        );
    }
    state
}

/// The verdict and the cursor it names after `steps`.
fn verdict(model: &Model, steps: &[&str]) -> (i64, i64) {
    let s = walk(model, steps);
    (s["verdict"], s["at"])
}

#[test]
fn the_pane_contract_holds_and_each_law_has_its_own_mutant() {
    let model = aos_pane_faithful_replay_model();

    let actions: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(interp::fired_actions(&model), actions);
    assert_eq!(
        interp::reachable_states(&model).len(),
        493,
        "the space aterm-aos-pane's Tier-1 test walks"
    );

    let dials: Vec<i64> = DIALS.iter().map(|&(dial, _)| dial).collect();
    assert_eq!(model.mutant_dials(), dials);
    let claims: BTreeSet<&str> = model
        .invariants
        .iter()
        .filter(|i| i.kind == InvariantKind::Claim)
        .map(|i| i.name)
        .collect();
    assert_eq!(claims, DIALS.iter().map(|&(_, law)| law).collect());

    for (dial, law) in DIALS {
        assert_eq!(
            interp::violated_invariants(&interp::with_buggy(&model, dial)),
            BTreeSet::from([law]),
            "Buggy {dial}"
        );
    }
    assert_eq!(
        verify::sweep_faults(std::slice::from_ref(&model)),
        Vec::<String>::new()
    );
    verify::prove_and_catch_scalar(
        &model,
        "pane.faithful-replay: each fault is named by its law at the first input it reaches, and a \
         chain with no fault passes",
    );
}

#[test]
fn a_faithful_chain_passes_with_or_without_a_witness() {
    let model = aos_pane_faithful_replay_model();
    assert_eq!(verdict(&model, &[]), (PASS, 0), "no input");
    assert_eq!(
        verdict(&model, &["Slice", "Resize", "Slice"]),
        (PASS, 0),
        "main passes a chain no witness checked; the R3 review's engine fix makes it unchecked"
    );
    let s = walk(&model, &["Slice", "Witness", "Resize", "Witness"]);
    assert_eq!((s["verdict"], s["w"], s["m"], s["z"]), (PASS, 2, 2, 1));

    let blind = interp::with_buggy(&model, 4);
    let s = walk(&blind, &["Slice", "Witness"]);
    assert_eq!(s["verdict"], NO_VERDICT);
    assert!(!blind.check_invariant("FaithfulPasses", &s));
}

#[test]
fn each_fault_is_named_by_its_law_at_its_own_input() {
    let model = aos_pane_faithful_replay_model();
    for (steps, want) in [
        (
            ["Slice", "SkewedSlice", "Witness"].as_slice(),
            (FAIL_WITNESS, 2),
        ),
        (["Slice", "ForgedWitness"].as_slice(), (FAIL_WITNESS, 1)),
        (["Witness", "AlteredReply"].as_slice(), (FAIL_REPLIES, 1)),
        (
            ["Witness", "Slice", "DroppedClock"].as_slice(),
            (FAIL_CLOCK, 2),
        ),
    ] {
        assert_eq!(verdict(&model, steps), want, "{steps:?}");
    }
    let s = walk(&model, &["SkewedSlice"]);
    assert_eq!(
        (s["div"], s["verdict"]),
        (1, PASS),
        "a skew shows only at the witness after it"
    );
}

#[test]
fn the_first_fault_decides_and_a_failed_law_does_not_stop_the_replay() {
    let model = aos_pane_faithful_replay_model();
    assert_eq!(
        verdict(&model, &["DroppedClock", "AlteredReply", "ForgedWitness"]),
        (FAIL_CLOCK, 0)
    );
    let s = walk(&model, &["AlteredReply", "Witness", "Witness"]);
    assert_eq!(
        (s["verdict"], s["m"]),
        (FAIL_REPLIES, 2),
        "the replay went on"
    );
}
