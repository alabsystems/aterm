// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `NativeUpdateSettingsDraftCarry`
//! (`aterm_spec::derive::native_update_settings_draft_carry_model`): an unsaved
//! Settings draft rides a seamless self-update (plan P2-2). Proved at
//! `Buggy = 0` and caught at `Buggy = 1`; `Buggy = 2` (the cold lane spends a
//! carried token) and `Buggy = 3` (a draft handed to an older successor) each
//! lose the draft too. The update's paths are walked as the person meets them.
//! Tier-1 is `aterm-gui/src/settings_draft_carry_conformance.rs`.

use aterm_spec::derive::native_update_settings_draft_carry_model;
use aterm_spec::{interp, verify};

#[test]
fn settings_draft_carry_proves_catches_and_has_no_dead_action() {
    let model = native_update_settings_draft_carry_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the Settings draft carry must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "Settings draft carry: the successor reopens the outgoing draft exactly, or says which it could not",
    );
    assert!(verify::uncaught_invariants(&model).is_empty());
}

/// The update as the person meets it. A draft typed before the update rides it.
/// A key typed while the successor boots holds the Commit, and the next attempt
/// carries the newer draft. With no terminal open, or toward an older build, a
/// draft holds the update as it always did; a desk with no draft goes either
/// way.
#[test]
fn settings_draft_carry_walks_the_update_the_way_the_person_meets_it() {
    let m = native_update_settings_draft_carry_model();

    let mut carried = m.init_state();
    for action in ["Type", "Start", "Park", "Restore", "Commit"] {
        assert!(m.fire(action, &mut carried), "{action}: {carried:?}");
    }
    assert_eq!(
        (carried["phase"], carried["succ"], carried["draft"]),
        (3, 1, 1),
        "the successor holds the draft"
    );

    let mut typed = m.init_state();
    for action in ["Type", "Start", "Park", "Restore", "Type"] {
        assert!(m.fire(action, &mut typed), "{action}: {typed:?}");
    }
    assert!(
        !m.action_enabled("Commit", &typed),
        "the person typed while the successor booted: no Commit"
    );
    assert!(m.fire("Rollback", &mut typed));
    for action in ["Start", "Park", "Restore", "Commit"] {
        assert!(m.fire(action, &mut typed), "{action}: {typed:?}");
    }
    assert_eq!(
        (typed["succ"], typed["draft"]),
        (2, 2),
        "the next attempt carried the newer draft"
    );

    let mut cold = m.init_state();
    for action in ["Type", "Start"] {
        assert!(m.fire(action, &mut cold), "{action}: {cold:?}");
    }
    assert!(
        !m.action_enabled("ColdExec", &cold),
        "the cold lane reopens nothing: a draft holds it"
    );
    assert!(
        !m.action_enabled("ParkOlder", &cold),
        "an older successor ignores the carry: a draft holds it"
    );
    assert!(m.fire("StandDown", &mut cold));

    let mut clean = m.init_state();
    for action in ["Start", "ColdExec"] {
        assert!(m.fire(action, &mut clean), "{action}: {clean:?}");
    }
    assert_eq!(
        clean["phase"], 3,
        "a desk with no draft takes the cold lane"
    );

    let mut unreadable = m.init_state();
    for action in ["Type", "Start", "Park", "RestoreUnreadable", "Commit"] {
        assert!(m.fire(action, &mut unreadable), "{action}: {unreadable:?}");
    }
    assert_eq!(
        unreadable["succ"], 9,
        "a carry the successor could not reopen is said, never refused"
    );
}

/// Each defect loses the draft on its own schedule: `Buggy = 1` commits a
/// successor holding the older draft, `Buggy = 2` execs cold over a draft,
/// `Buggy = 3` hands a draft to a successor that ignores it.
#[test]
fn settings_draft_carry_every_defect_loses_the_draft() {
    for buggy in [1, 2, 3] {
        let m = interp::with_buggy(&native_update_settings_draft_carry_model(), buggy);
        let Err((lost, _)) = interp::bmc(&m) else {
            panic!("Buggy = {buggy} must lose a draft");
        };
        assert_eq!(lost["phase"], 3, "Buggy = {buggy}: {lost:?}");
        assert_ne!(lost["succ"], lost["draft"], "Buggy = {buggy}: {lost:?}");
        assert_ne!(lost["succ"], 9, "Buggy = {buggy}: {lost:?}");
    }
}
