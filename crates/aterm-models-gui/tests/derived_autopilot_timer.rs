// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_gui::autopilot_timer_model;
use aterm_spec::{interp, verify};

#[test]
fn a_timer_only_takes_power_away_and_the_hand_always_wins() {
    let model = autopilot_timer_model();

    for (mutant, invariant) in [
        ("WireLandKeepsTimer", "TheTimerGoesWithCruise"),
        ("TrustingFire", "LandOncePerArm"),
        ("TrustingFire", "TheHandWins"),
        ("SameWordKeepsTimer", "TheHandWins"),
        ("MonotonicFire", "NeverEarly"),
        ("CommitWithoutRebuild", "TheIndexTrailsTheStore"),
        ("CloseKeepsEntry", "TheEntryGoesWithItsSession"),
    ] {
        let mut alone = verify::buggy_baseline_with(&model, mutant);
        alone.invariants.retain(|inv| inv.name == invariant);
        assert!(
            interp::bmc(&alone).is_err(),
            "`{mutant}` alone breaks `{invariant}`"
        );
    }

    let mut s = model.init_state();
    assert!(model.fire("SetCruiseUntil", &mut s));
    assert_eq!((s["word"], s["land_at"], s["idx"]), (1, 2, 2));
    assert!(!model.action_enabled("Fire", &s), "not due yet");
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Fire", &mut s));
    assert_eq!((s["word"], s["land_at"], s["idx"]), (2, 0, 0), "{s:?}");

    let mut s = model.init_state();
    assert!(model.fire("SetCruiseUntil", &mut s));
    assert!(model.fire("WireOff", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Fire", &mut s));
    assert_eq!((s["word"], s["land_at"], s["idx"]), (3, 0, 0), "{s:?}");

    let mut s = model.init_state();
    assert!(model.fire("SetCruiseUntil", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("ClockBack", &mut s));
    assert!(model.fire("Fire", &mut s));
    assert_eq!((s["word"], s["idx"], s["wait"]), (1, 2, 1), "{s:?}");

    let mut s = model.init_state();
    assert!(model.fire("SetCruiseUntil", &mut s));
    assert!(model.fire("Sleep", &mut s));
    assert!(model.fire("Sleep", &mut s));
    assert!(
        !model.action_enabled("Fire", &s),
        "the monotonic clock stopped"
    );
    assert!(model.fire("WallMoved", &mut s));
    assert!(model.fire("Fire", &mut s));
    assert_eq!(s["word"], 2, "{s:?}");
}
