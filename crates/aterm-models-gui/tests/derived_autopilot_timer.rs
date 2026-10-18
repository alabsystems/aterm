// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_gui::{autopilot_timer_liveness, autopilot_timer_model};
use aterm_spec::{interp, verify};

/// AUTOPILOT'S TIMER (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.8): the timer goes
/// with Cruise, Land is written at most once per arm and never early, a hand write stored
/// first always wins, the index trails the store by at most a wake once committed, and an
/// entry goes with its session. Each invariant is broken by its own `Buggy` member alone.
/// Tier-1 against the real index, fire and restore paths is aterm-gui's
/// `autopilot_timers_conformance_tests`.
#[test]
fn a_timer_only_takes_power_away_and_the_hand_always_wins() {
    let model = autopilot_timer_model();
    verify::prove_and_catch_scalar(&model, "autopilot timer");

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

    // The healthy lane, step by step: armed, the time comes, Land.
    let mut s = model.init_state();
    assert!(model.fire("SetCruiseUntil", &mut s));
    assert_eq!((s["word"], s["land_at"], s["idx"]), (1, 2, 2));
    assert!(!model.action_enabled("Fire", &s), "not due yet");
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Fire", &mut s));
    assert_eq!((s["word"], s["land_at"], s["idx"]), (2, 0, 0), "{s:?}");

    // The race: an Off stored over the wire before the App applied its wake;
    // the fire reads the store and writes nothing.
    let mut s = model.init_state();
    assert!(model.fire("SetCruiseUntil", &mut s));
    assert!(model.fire("WireOff", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Fire", &mut s));
    assert_eq!((s["word"], s["land_at"], s["idx"]), (3, 0, 0), "{s:?}");

    // The clock set back under a due entry: no Land; the remainder indexed.
    let mut s = model.init_state();
    assert!(model.fire("SetCruiseUntil", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("ClockBack", &mut s));
    assert!(model.fire("Fire", &mut s));
    assert_eq!((s["word"], s["idx"], s["wait"]), (1, 2, 1), "{s:?}");

    // Asleep through the time: the monotonic due is stale until the wall is
    // read again.
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

/// A timer whose time came is landed (`[]<>`) under the fire's, the wall's re-derivation's, the
/// wake's and the Commit's weak fairness, each load-bearing; a Commit that does not rebuild
/// the index breaks it alone.
#[test]
fn a_stored_timer_whose_time_came_lands() {
    let model = autopilot_timer_model();
    let live = autopilot_timer_liveness();
    verify::liveness_proves_and_catches_tiered(&model, &live, "AutopilotTimer lands");
}
