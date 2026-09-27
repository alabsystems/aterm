// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `NativeUpdateEditorCarry`
//! (`aterm_spec::derive::native_update_editor_carry_model`): an unsaved editor
//! tab rides a self-update (gap #32). Proved at `Buggy = 0` and caught at
//! `Buggy = 1` on every invariant; `Buggy = 2` and `Buggy = 3` are the two
//! subtler losses, each caught on `NoDraftLost`. The update's paths are walked
//! as the person meets them. Tier-1 is
//! `aterm-gui/src/editor_carry_conformance.rs`.

use aterm_spec::derive::{Model, native_update_editor_carry_model};
use aterm_spec::{interp, verify};

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

#[test]
fn editor_carry_proves_catches_and_has_no_dead_action() {
    let model = native_update_editor_carry_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the editor carry must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "editor carry: the successor holds the draft (or its journal does), and the journal is re-seated after a rollback",
    );
}

/// The update as the person meets it. A draft typed and journaled rides the
/// update. Typing during the overlap holds the Commit and the rollback gives
/// the journal back. A draft that is not durable yet holds the start.
#[test]
fn editor_carry_walks_the_update_the_way_the_person_meets_it() {
    let m = native_update_editor_carry_model();

    let mut carried = m.init_state();
    for action in ["Type", "Plan", "Land", "Start", "Park", "Restore", "Commit"] {
        assert!(m.fire(action, &mut carried), "{action}: {carried:?}");
    }
    assert_eq!(
        (carried["phase"], carried["succ"], carried["head"]),
        (3, 1, 1),
        "the successor holds the draft"
    );

    let mut typed = m.init_state();
    for action in ["Type", "Plan", "Land", "Start", "Park", "Restore", "Type"] {
        assert!(m.fire(action, &mut typed), "{action}: {typed:?}");
    }
    assert!(
        !m.action_enabled("Commit", &typed),
        "the person typed during the overlap: no Commit"
    );
    assert!(m.fire("Plan", &mut typed));
    assert!(
        m.fire("Refuse", &mut typed),
        "the append fails against the successor's image"
    );
    assert!(m.fire("Rollback", &mut typed));
    assert!(
        !m.action_enabled("Plan", &typed),
        "no append while the re-seat is owed: it would fail against the successor's image"
    );
    assert!(m.fire("Reseat", &mut typed));
    assert_eq!(
        (
            typed["owner"],
            typed["mem"],
            typed["journal"],
            typed["head"]
        ),
        (0, 2, 2, 2),
        "the rollback's re-seat gave the journal back, holding the latest edit"
    );
    assert!(
        m.action_enabled("Start", &typed),
        "and the next attempt carries it"
    );

    let mut writing = m.init_state();
    for action in ["Type", "Plan"] {
        assert!(m.fire(action, &mut writing), "{action}: {writing:?}");
    }
    assert!(
        !m.action_enabled("Start", &writing),
        "an append on the worker holds the start"
    );

    let mut failed = m.init_state();
    for action in ["Type", "Plan", "Land", "Start", "Park", "RestoreFails"] {
        assert!(m.fire(action, &mut failed), "{action}: {failed:?}");
    }
    assert!(
        m.action_enabled("Commit", &failed),
        "a failed restore leaves a Recovery tab over the journal that holds the draft"
    );

    let mut clean = m.init_state();
    for action in ["Start", "Park", "RestoreIdentical", "Commit"] {
        assert!(m.fire(action, &mut clean), "{action}: {clean:?}");
    }
    assert_eq!(clean["succ"], 0, "a clean document is the file");
}

/// `Buggy = 1` — a Commit that trusts a dirty document without the fresh
/// check, and a rollback that forgets the successor's image — is caught on
/// each invariant by its own schedule. `Buggy = 2` (a re-seat while parked)
/// and `Buggy = 3` (no `late` check) each lose the draft.
#[test]
fn editor_carry_the_defects_are_caught_on_every_invariant() {
    let m = interp::with_buggy(&native_update_editor_carry_model(), 1);
    let (lost, _) = interp::bmc(&only(&m, "NoDraftLost")).expect_err("blind Commit");
    assert_eq!(lost["phase"], 3, "{lost:?}");
    assert_ne!(lost["succ"], lost["head"], "{lost:?}");
    let (stale, _) =
        interp::bmc(&only(&m, "JournalOwnedOutsideHandoff")).expect_err("forgotten image");
    assert_eq!(
        (stale["phase"], stale["reseat"], stale["owner"]),
        (0, 0, 1),
        "{stale:?}"
    );

    for buggy in [2, 3] {
        let m = interp::with_buggy(&native_update_editor_carry_model(), buggy);
        let Err((lost, _)) = interp::bmc(&only(&m, "NoDraftLost")) else {
            panic!("Buggy = {buggy} must lose a draft");
        };
        assert_eq!(lost["phase"], 3, "Buggy = {buggy}: {lost:?}");
        assert_ne!(lost["succ"], lost["head"], "Buggy = {buggy}: {lost:?}");
        assert!(
            interp::bmc(&only(&m, "JournalOwnedOutsideHandoff")).is_ok(),
            "Buggy = {buggy} is a Commit-side loss only"
        );
    }
    assert!(verify::uncaught_invariants(&native_update_editor_carry_model()).is_empty());
}
