// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 desktop-alert cancellation. The GUI's separate Tier-1 bind
//! projects real herald decisions; this model alone proves nothing about Rust.

use aterm_spec::{derive::desktop_alert_debt_model, interp, verify};

#[test]
fn desktop_alert_debt_is_cancelled_without_forgetting_seen_escalations() {
    let model = desktop_alert_debt_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the desktop-alert debt model must remain registered"
    );
    verify::prove_and_catch_scalar(&model, "desktop alerts cancel work and preserve seen state");
    for (fault, label) in [
        (1, "desktop alert disable cancels existing debt"),
        (2, "desktop alert disabled observations create no work"),
        (3, "desktop alert disable preserves the seen escalation"),
    ] {
        let selected = interp::with_consts(&model, &[("Fault", fault)]);
        verify::prove_and_catch_scalar(&selected, label);
    }
    assert!(verify::uncaught_invariants(&model).is_empty());
}

#[test]
fn enable_replays_neither_cancelled_debt_nor_observations_made_while_disabled() {
    let model = desktop_alert_debt_model();
    let mut state = model.init_state();
    for action in ["Enable", "ObserveNewFree", "ObserveNewLimited"] {
        assert!(model.fire(action, &mut state));
    }
    assert_eq!((state["label"], state["seen"], state["pending"]), (2, 2, 1));
    for action in ["Disable", "Enable", "Reobserve", "Tick"] {
        assert!(model.fire(action, &mut state));
        assert_eq!((state["pending"], state["notice"]), (0, 0), "{action}");
        assert_eq!(state["seen"], 2);
    }
    for action in [
        "Disable",
        "ObserveNewLimited",
        "ObserveNewFree",
        "Enable",
        "Reobserve",
    ] {
        assert!(model.fire(action, &mut state));
        assert_eq!((state["pending"], state["notice"]), (0, 0), "{action}");
        assert_eq!(state["seen"], state["label"]);
    }
    assert!(model.fire("ObserveNewFree", &mut state));
    assert_eq!(
        state["notice"], 1,
        "a fresh enabled transition still notifies"
    );
    assert!(model.fire("ObserveNewLimited", &mut state));
    assert_eq!((state["pending"], state["notice"]), (1, 0));
    assert!(model.fire("Tick", &mut state));
    assert_eq!(
        (state["pending"], state["notice"]),
        (0, 1),
        "enabled debt still drains"
    );
}

#[test]
fn each_mutant_exposes_its_own_failure_and_forgotten_state_replays() {
    let model = desktop_alert_debt_model();
    let retained = interp::with_consts(&model, &[("Buggy", 1), ("Fault", 1)]);
    let mut state = retained.init_state();
    for action in ["Enable", "ObserveNewLimited", "Disable"] {
        assert!(retained.fire(action, &mut state));
    }
    assert!(!retained.check_invariant("DisabledOwnsNoDebt", &state));
    assert!(retained.check_invariant("SeenTracksLabel", &state));
    assert!(retained.check_invariant("OnlyEnabledNotices", &state));

    let ignored = interp::with_consts(&model, &[("Buggy", 1), ("Fault", 2)]);
    let mut state = ignored.init_state();
    assert!(ignored.fire("ObserveNewLimited", &mut state));
    assert!(!ignored.check_invariant("DisabledOwnsNoDebt", &state));
    assert!(ignored.check_invariant("SeenTracksLabel", &state));
    assert!(ignored.fire("ObserveNewFree", &mut state));
    assert!(!ignored.check_invariant("OnlyEnabledNotices", &state));

    let forgotten = interp::with_consts(&model, &[("Buggy", 1), ("Fault", 3)]);
    let mut state = forgotten.init_state();
    for action in ["Enable", "ObserveNewFree", "Disable"] {
        assert!(forgotten.fire(action, &mut state));
    }
    assert!(forgotten.check_invariant("DisabledOwnsNoDebt", &state));
    assert!(!forgotten.check_invariant("SeenTracksLabel", &state));
    assert!(forgotten.check_invariant("OnlyEnabledNotices", &state));
    for action in ["Enable", "Reobserve"] {
        assert!(forgotten.fire(action, &mut state));
    }
    assert_eq!(
        state["notice"], 1,
        "forgetting seen state replays the same escalation"
    );
}
