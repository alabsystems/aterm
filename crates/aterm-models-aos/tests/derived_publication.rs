// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `aos_boundary_publication_model`: the laws hold over the whole bounded space, each
//! law is broken by its own `Buggy` dial and by no other, the registry sweep finds nothing in
//! either declared configuration, and the protocol's named cases (Aos plan §4) walk as designed.
//! Tier-1 against the real `Boundary` is `aterm-aos`'s `tests/conformance_publication.rs`.

use std::collections::BTreeSet;

use aterm_models_aos::aos_boundary_publication_model;
use aterm_spec::derive::{InvariantKind, Model};
use aterm_spec::interp::{self, State};

/// Each dial and the one law it is written to break.
const DIALS: [(i64, &str); 4] = [
    (1, "StaleNeverCommitted"),
    (2, "NeverMixed"),
    (3, "EachInputOnce"),
    (4, "DeliveredToTheLivePair"),
];

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

/// The healthy laws hold everywhere, every action fires with no mutant on, and each dial breaks
/// exactly the one law it names, so no law is a ghost and no dial carries another's catch.
#[test]
fn publication_holds_and_each_law_has_its_own_mutant() {
    let model = aos_boundary_publication_model();

    let actions: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(interp::fired_actions(&model), actions);

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
}

/// Commit publishes code, replayed state and a new revision together, then hands the queue to the
/// new pair; a reject that delivered nothing leaves the head where it was, so a candidate kept
/// across it is still current.
#[test]
fn a_candidate_prepared_against_the_paused_head_publishes_code_and_state_together() {
    let model = aos_boundary_publication_model();
    let s = walk(&model, &["Input", "Pause", "PrepareA", "Hold", "CommitA"]);
    assert_eq!(
        (s["paused"], s["rev"], s["code_v"], s["state_v"]),
        (0, 1, 1, 1)
    );
    assert_eq!(
        (s["arrived"], s["queued"], s["delivered"], s["cur"]),
        (2, 0, 2, 2)
    );
    assert_eq!(s["pa"], 0);

    let s = walk(&model, &["Pause", "PrepareA", "Reject", "Pause"]);
    assert!(model.action_enabled("CommitA", &s));
}

/// Two candidates prepared against one head: the first commits, and the second is stale from that
/// moment (the revision moved though no input did). Only the revision-blind dial publishes it.
#[test]
fn a_second_candidate_against_the_same_head_cannot_commit_after_the_first() {
    let model = aos_boundary_publication_model();
    let s = walk(
        &model,
        &["Pause", "PrepareA", "PrepareB", "CommitA", "Pause"],
    );
    assert_eq!((s["rev"], s["cur"], s["rb"], s["ib"]), (1, 0, 0, 0));
    assert!(!model.action_enabled("CommitB", &s));
    assert!(model.action_enabled("DiscardB", &s));

    let blind = interp::with_buggy(&model, 1);
    let mut next = s.clone();
    assert!(blind.fire("CommitB", &mut next));
    assert!(!model.check_invariant("StaleNeverCommitted", &next));
}

/// A candidate kept while an input reached the live pair (delivered from the queue by a reject,
/// or run while the task was not paused) is stale, under the revision-blind dial too.
#[test]
fn a_candidate_cannot_commit_once_an_input_moved_the_head() {
    let model = aos_boundary_publication_model();
    let blind = interp::with_buggy(&model, 1);
    for steps in [
        ["Pause", "PrepareA", "Hold", "Reject", "Pause"].as_slice(),
        ["Pause", "PrepareA", "Reject", "Input", "Pause"].as_slice(),
    ] {
        let s = walk(&model, steps);
        assert_eq!((s["pa"], s["rev"], s["ra"]), (1, 0, 0), "{steps:?}");
        assert!(s["ia"] < s["cur"], "{steps:?}");
        assert!(!model.action_enabled("CommitA", &s), "{steps:?}");
        assert!(!blind.action_enabled("CommitA", &s), "{steps:?}");
    }
}

/// An input that finds the queue full rejects the update before anything is dropped: the queue,
/// then that input, reach the old pair, and the candidate left behind is stale. With no queue at
/// all (`Cap = 0`) the first input while paused does the same.
#[test]
fn a_full_queue_rejects_the_update_and_delivers_everything_to_the_old_pair() {
    let model = aos_boundary_publication_model();
    let s = walk(&model, &["Pause", "PrepareA", "Hold", "Hold"]);
    assert!(!model.action_enabled("Hold", &s));
    let s = walk(
        &model,
        &["Pause", "PrepareA", "Hold", "Hold", "Overflow", "Pause"],
    );
    assert_eq!((s["rev"], s["code_v"], s["state_v"], s["pa"]), (0, 0, 0, 1));
    assert_eq!(
        (s["arrived"], s["queued"], s["delivered"], s["cur"]),
        (3, 0, 3, 3)
    );
    assert!(!model.action_enabled("CommitA", &s));

    let unqueued = interp::with_consts(&model, &[("Cap", 0)]);
    let s = walk(&unqueued, &["Pause", "PrepareA"]);
    assert!(!unqueued.action_enabled("Hold", &s));
    let s = walk(&unqueued, &["Pause", "PrepareA", "Overflow"]);
    assert_eq!(
        (s["paused"], s["arrived"], s["delivered"], s["cur"]),
        (0, 1, 1, 1)
    );
}
