// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::pet_surface_seam_model, interp, verify};

#[test]
fn pet_surface_seam_proves_and_catches_every_mutant() {
    let model = pet_surface_seam_model();
    verify::prove_and_catch_scalar(&model, "resident pet across a coordinate seam");
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(0),
        "every action fires at Buggy=0"
    );
    let declared: std::collections::BTreeSet<_> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(
        interp::fired_actions(&interp::with_buggy(&model, 1)),
        declared,
        "every action fires at Buggy=1"
    );
}

#[test]
fn a_regrid_keeps_the_body_and_a_replacement_crossfades_in_place() {
    let model = pet_surface_seam_model();
    let run = |m: &aterm_spec::derive::Model, actions: &[&str]| {
        let mut state = m.init_state();
        for action in actions {
            assert!(m.fire(action, &mut state), "{action}");
        }
        state
    };
    let regrid = run(&model, &["Draw", "Regrid", "Tick"]);
    assert_eq!((regrid["lit"], regrid["moved"]), (1, 0));
    let replaced = run(&model, &["Draw", "Replace"]);
    assert_eq!(
        (replaced["lit"], replaced["hit"]),
        (0, 0),
        "the boundary is dark"
    );
    let crossfade = run(&model, &["Draw", "Replace", "Tick"]);
    assert_eq!((crossfade["lit"], crossfade["ghost"]), (1, 1));
    let shifted = run(&model, &["Draw", "Replace", "Shift", "Tick"]);
    assert_eq!(
        (shifted["lit"], shifted["ghost"], shifted["ghost_moved"]),
        (1, 1, 0),
        "a split-pane focus switch crossfades: the ghost fades on its own glass"
    );
    for actions in [
        &["Draw", "Replace", "Lose", "Tick"][..],
        &["Draw", "Replace", "Shift", "Lose", "Tick"][..],
    ] {
        let lost = run(&model, actions);
        assert_eq!(
            (lost["ghost"], lost["ghost_moved"]),
            (0, 0),
            "{actions:?}: no ghost on glass that is gone"
        );
    }
    let blind = run(&model, &["Draw", "Blind", "Tick"]);
    assert_eq!(blind["lit"], 1, "a blind frame holds the body");
    let long = run(&model, &["Draw", "Blind", "Tick", "BlindLong"]);
    assert_eq!(long["lit"], 0, "…for a bounded grace");

    // `Buggy = 1`: every seam cuts the cat for a frame.
    let shipped = interp::with_buggy(&model, 1);
    for (actions, invariant) in [
        (&["Draw", "Regrid"][..], "RegridKeepsTheBody"),
        (&["Draw", "Regrid", "Tick"][..], "RegridKeepsTheBody"),
        (&["Draw", "Blind", "Tick"][..], "BlindHoldsInPlace"),
    ] {
        let state = run(&shipped, actions);
        assert_eq!(state["lit"], 0, "{actions:?}: the shipped cut");
        assert!(!model.check_invariant(invariant, &state), "{invariant}");
    }
    // Its replacement is dark at the boundary, as it must be, and dark again on the frame
    // after, with no crossfade.
    let mut state = run(&model, &["Draw", "Replace"]);
    let cut = shipped.successors("Tick", &state);
    assert_eq!(cut.len(), 1);
    assert_eq!((cut[0]["lit"], cut[0]["ghost"]), (0, 0));
    assert!(!model.check_invariant("ReplaceCrossfades", &cut[0]));
    assert!(model.fire("Tick", &mut state));
    assert!(model.check_invariant("ReplaceCrossfades", &state));

    // A split-pane focus switch under `PaneCut`: a replacement on the same glass crossfades,
    // but the moved pixel frame drops the ghost, and the frame after the switch has no cat.
    let pane_cut = interp::with_consts(&model, &[("PaneCut", 1)]);
    let same_glass = run(&pane_cut, &["Draw", "Replace", "Tick"]);
    assert!(model.check_invariant("ReplaceCrossfades", &same_glass));
    let mut state = run(&model, &["Draw", "Replace", "Shift"]);
    let cut = pane_cut.successors("Tick", &state);
    assert_eq!(cut.len(), 1);
    assert_eq!((cut[0]["lit"], cut[0]["ghost"]), (0, 0));
    assert!(!model.check_invariant("ReplaceCrossfades", &cut[0]));
    assert!(model.fire("Tick", &mut state));
    assert!(model.check_invariant("ReplaceCrossfades", &state));
    let Err((_, broken)) = interp::bmc(&pane_cut) else {
        panic!("PaneCut must break an invariant");
    };
    assert_eq!(broken, "ReplaceCrossfades");
    // …and `Buggy = 1` spends a ghost on glass that is gone.
    let state = run(&model, &["Draw", "Replace", "Lose"]);
    let spent = shipped.successors("Tick", &state);
    assert_eq!(spent.len(), 1);
    assert!(!model.check_invariant("LostGlassSpendsNoGhost", &spent[0]));
    assert!(!model.check_invariant("GhostNeverDisplaced", &spent[0]));

    // The world the ghost would fade in protects the cells under the old
    // body (`Refuse`): no ghost there, and the arriver carries its presence —
    // on the same glass, in the pane the focus left, and on a return.
    for actions in [
        &["Draw", "Replace", "Refuse", "Tick"][..],
        &["Draw", "Replace", "Shift", "Refuse", "Tick"][..],
        &["Draw", "Replace", "Tick", "Return", "Refuse", "Tick"][..],
    ] {
        let refused = run(&model, actions);
        assert_eq!(
            (refused["lit"], refused["ghost"], refused["dipped"]),
            (1, 0, 0),
            "{actions:?}: no ghost over protected cells, and no dip"
        );
    }
    // `Cull`: the refused ghost takes its presence with it, and the arriver stands alone at its
    // ramp's floor.
    let cull = interp::with_consts(&model, &[("Cull", 1)]);
    let state = run(&model, &["Draw", "Replace", "Refuse"]);
    let cut = cull.successors("Tick", &state);
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0]["dipped"], 1);
    assert!(!model.check_invariant("RefusedGhostHandsOver", &cut[0]));
    let Err((_, broken)) = interp::bmc(&cull) else {
        panic!("Cull must break an invariant");
    };
    assert_eq!(broken, "RefusedGhostHandsOver");
}

#[test]
fn a_quick_return_continues_one_body_and_every_ghost_leaves_at_full_presence() {
    let model = pet_surface_seam_model();
    let run = |m: &aterm_spec::derive::Model, actions: &[&str]| {
        let mut state = m.init_state();
        for action in actions {
            assert!(m.fire(action, &mut state), "{action}");
        }
        state
    };
    for first in [&["Draw", "Replace"][..], &["Draw", "Replace", "Shift"][..]] {
        let toggle = [first, &["Tick", "Return"][..]].concat();
        let boundary = run(&model, &toggle);
        assert_eq!(
            (boundary["lit"], boundary["hit"]),
            (0, 0),
            "{toggle:?}: the second boundary is dark too"
        );
        let back = run(&model, &[&toggle[..], &["Tick"][..]].concat());
        assert_eq!(
            (back["lit"], back["ghost"], back["restarted"]),
            (1, 1, 0),
            "{toggle:?}: one body continues at the seat it comes back to"
        );
        let done = run(&model, &[&toggle[..], &["Tick", "Spend"][..]].concat());
        assert_eq!(done["tail"], 0, "{toggle:?}: no ghost leaves early");
    }
    // A single crossfade's ghosts already left on the arriver's full frame.
    let single = interp::with_consts(&model, &[("OwnClock", 1)]);
    let state = run(&single, &["Draw", "Replace", "Tick", "Spend"]);
    assert_eq!(state["tail"], 0);

    // `Restart`: the arriver restarts at the floor at the seat it comes back to, beside its own
    // ghost still fading there (a split pane), or after that ghost was cut at the boundary (the
    // same glass).
    let restart = interp::with_consts(&model, &[("Restart", 1)]);
    let state = run(&model, &["Draw", "Replace", "Tick", "Return"]);
    let doubled = restart.successors("Tick", &state);
    assert_eq!(doubled.len(), 1);
    assert_eq!(doubled[0]["restarted"], 1);
    assert!(!model.check_invariant("ReturnContinuesOneBody", &doubled[0]));
    let Err((_, broken)) = interp::bmc(&restart) else {
        panic!("Restart must break an invariant");
    };
    assert_eq!(broken, "ReturnContinuesOneBody");

    // `OwnClock`: a carried ghost keeps its old clock and leaves glass while the restarted
    // arriver comes up.
    let own_clock = interp::with_consts(&model, &[("OwnClock", 1)]);
    let state = run(&model, &["Draw", "Replace", "Tick", "Return", "Tick"]);
    let early = own_clock.successors("Spend", &state);
    assert_eq!(early.len(), 1);
    assert_eq!(early[0]["tail"], 1);
    assert!(!model.check_invariant("GhostLeavesAtFullPresence", &early[0]));
    let Err((_, broken)) = interp::bmc(&own_clock) else {
        panic!("OwnClock must break an invariant");
    };
    assert_eq!(broken, "GhostLeavesAtFullPresence");
}

#[test]
fn a_breed_leaver_crosses_a_boundary_and_leaves_at_full_presence() {
    let model = pet_surface_seam_model();
    let run = |m: &aterm_spec::derive::Model, actions: &[&str]| {
        let mut state = m.init_state();
        for action in actions {
            assert!(m.fire(action, &mut state), "{action}");
        }
        state
    };
    for seam in [&["Replace"][..], &["Replace", "Shift"][..]] {
        let crossed = [&["Draw", "Handoff"][..], seam, &["Tick"][..]].concat();
        let state = run(&model, &crossed);
        assert_eq!((state["lit"], state["ghost"]), (1, 1), "{crossed:?}");
        let done = run(&model, &[&crossed[..], &["Spend"][..]].concat());
        assert_eq!(done["tail"], 0, "{crossed:?}: the leaver fades on the ramp");
    }
    // `LeaverCut`: the boundary drops the leaver, which leaves glass while its arriver is still
    // coming up.
    let leaver_cut = interp::with_consts(&model, &[("LeaverCut", 1)]);
    let state = run(&model, &["Draw", "Handoff", "Replace", "Tick"]);
    let cut = leaver_cut.successors("Spend", &state);
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0]["tail"], 1);
    assert!(!model.check_invariant("GhostLeavesAtFullPresence", &cut[0]));
    // A crossfade with no leaver is never cut.
    let state = run(&leaver_cut, &["Draw", "Replace", "Tick", "Spend"]);
    assert_eq!(state["tail"], 0);
    let Err((_, broken)) = interp::bmc(&leaver_cut) else {
        panic!("LeaverCut must break an invariant");
    };
    assert_eq!(broken, "GhostLeavesAtFullPresence");
}
