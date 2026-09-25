// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_index_publish_walk_model, interp, verify};

/// Fire `actions` in order from the model's initial state, each one required to be enabled.
fn run(model: &aterm_spec::derive::Model, actions: &[&str]) -> interp::State {
    let mut state = model.init_state();
    for action in actions {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

#[test]
fn the_index_channel_is_one_run_the_walk_lands_and_nothing_is_overwritten() {
    let model = atpkg_index_publish_walk_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the index publish/walk pair must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "atpkg index publish/walk");
    assert!(
        verify::uncaught_invariants(&model).is_empty(),
        "every invariant has its own Buggy = 1 counterexample"
    );
    // Strictly non-vacuous: every action fires in the committed model.
    let fired = interp::fired_actions(&model);
    for action in &model.actions {
        assert!(fired.contains(action.name), "{} never fires", action.name);
    }

    // Healthy races. A killed publish leaves A's draft at 2; B reads the newest PUBLISHED
    // index (1), so it aims at 2 too and is refused there — never building 3 on the draft.
    let orphaned = run(&model, &["ReadA", "CreateA", "DieA", "ReadB"]);
    assert_eq!((orphaned["t2"], orphaned["nb"]), (1, 2));
    assert!(model.action_enabled("RefuseB", &orphaned));
    assert!(!model.action_enabled("CreateB", &orphaned));
    assert!(!model.action_enabled("ConvergeB", &orphaned));
    // A's own re-run converges its draft and publishes it; the walk from 1 then lands 2.
    let mut healed = run(&model, &["ReadA", "CreateA", "DieA", "ReadA"]);
    assert!(model.action_enabled("ConvergeA", &healed));
    assert!(model.fire("ConvergeA", &mut healed));
    assert!(model.fire("Walk", &mut healed));
    assert_eq!((healed["t2"], healed["floor"]), (2, 2));
    // Two publishers on one baseline: the first create wins N, the second is refused.
    let raced = run(&model, &["ReadA", "ReadB", "CreateA", "FinishA"]);
    assert!(model.action_enabled("RefuseB", &raced));
    assert!(!model.action_enabled("ConvergeB", &raced));

    // Buggy = 1, each invariant's own counterexample. The skipped number: A publishes 3
    // over an absent 2, a hole the walk from 1 cannot cross.
    let buggy = interp::with_buggy(&model, 1);
    let skipped = run(&buggy, &["ReadA", "CreateA", "FinishA"]);
    assert_eq!((skipped["t2"], skipped["t3"]), (0, 2));
    assert!(!buggy.check_invariant("OneRun", &skipped));
    assert!(!buggy.check_invariant("WalkLandsNewest", &skipped));
    // The draft baseline: A's killed publish leaves a draft at 3, B's read counts it and
    // publishes 4 on it — two numbers missing directly above the floor.
    let on_draft = run(
        &buggy,
        &["ReadA", "CreateA", "DieA", "ReadB", "CreateB", "FinishB"],
    );
    assert_eq!((on_draft["t2"], on_draft["t3"], on_draft["t4"]), (0, 1, 4));
    assert!(!buggy.check_invariant("WalkLandsNewest", &on_draft));
    // The healthy writer takes neither step: its read never counts the draft, and its
    // number is the baseline's successor.
    let healthy_b = run(&model, &["ReadA", "CreateA", "DieA", "ReadB"]);
    assert_eq!((healthy_b["bb"], healthy_b["nb"]), (1, 2));
    // B converging onto A's tag (the skip puts both on 3) overwrites it.
    let overwrite = run(&buggy, &["ReadA", "ReadB", "CreateA", "ConvergeB"]);
    assert_eq!(overwrite["overwrote"], 1);
    assert!(!buggy.check_invariant("NoOverwrite", &overwrite));
}
