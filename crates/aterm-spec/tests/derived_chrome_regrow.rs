// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `ChromeRegrowFloor`: after any rise of a window's chrome rows, neither the
//! message band's quiet shrink nor the presence row's quiet fold re-grids the window back
//! inside the settle, whichever source rose, and a fall the floor holds is applied once its own
//! quiet and the floor have run. `Buggy = 1` lets each source fall once its own quiet has run
//! (the cross-source pairs) and re-anchors the floor on a step that is no rise. Tier-1, the real
//! App path, is `aterm-gui/src/chrome_regrow_conformance.rs`.

use crate::walk::walk;
use aterm_spec::derive::{chrome_regrow_floor_liveness, chrome_regrow_floor_model};
use aterm_spec::{interp, verify};

#[test]
fn no_source_falls_inside_the_settle_of_another_sources_rise() {
    let model = chrome_regrow_floor_model();
    verify::prove_and_catch_scalar(&model, "chrome regrow floor");
    let dead = ["BandFallOwnQuiet", "PresFallOwnQuiet", "Restart"];
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(dead.len()),
        "every mutant is a negative control caught alone"
    );

    // The presence row is born just before the band's quiet shrink: the band waits the floor
    // and shrinks one tick later.
    let mut state = walk(
        &model,
        &["BandRise", "Tick", "BandUnwant", "PresRise", "BandQuiet"],
    );
    assert_eq!((state["b"], state["since"]), (1, 0), "{state:?}");
    assert!(
        !model.action_enabled("BandFall", &state),
        "no shrink inside the floor: {state:?}"
    );
    for action in ["Tick", "BandFall"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!((state["b"], state["quick"]), (0, 0), "{state:?}");

    // A band row rises just before the presence row's quiet fold lands: the fold waits the
    // floor.
    let mut state = walk(
        &model,
        &["PresRise", "Tick", "PresUnwant", "PresQuiet", "BandRise"],
    );
    assert!(!model.action_enabled("PresFall", &state), "{state:?}");
    for action in ["Tick", "PresFall"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!((state["p"], state["quick"]), (0, 0), "{state:?}");

    // A band row rises just before a person reads a row that has stood its quiet: the read
    // waits the floor.
    let mut state = walk(&model, &["PresRise", "Tick", "BandRise", "PresRead"]);
    assert_eq!(state["pq"], 1, "the read's own due has passed: {state:?}");
    assert!(!model.action_enabled("PresFall", &state), "{state:?}");
    for action in ["Tick", "PresFall"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }

    // The model lets a source's own quiet run out before a floor tick (an over-approximation:
    // the real quiet, 1.5 s, outlasts the 400 ms floor), so the floor covers a source's own
    // pair too.
    let own = walk(&model, &["BandRise", "BandUnwant", "BandQuiet"]);
    assert!(!model.action_enabled("BandFall", &own), "{own:?}");

    // A want back while the fall waits re-grids nothing, and a rise is never
    // delayed: the benign order is immediate both ways.
    let back = walk(&model, &["BandRise", "BandUnwant", "BandBack"]);
    assert_eq!((back["bw"], back["b"]), (1, 1), "{back:?}");
    let benign = walk(
        &model,
        &[
            "BandRise",
            "Tick",
            "BandUnwant",
            "BandQuiet",
            "BandFall",
            "PresRise",
        ],
    );
    assert_eq!((benign["b"], benign["p"]), (0, 1), "{benign:?}");

    // The window's own shrink takes a waiting fall at once, inside the floor; it is exempt by
    // name.
    let mut cramped = walk(
        &model,
        &["BandRise", "BandUnwant", "BandQuiet", "PresRise", "Cramp"],
    );
    assert!(!model.action_enabled("BandFall", &cramped), "{cramped:?}");
    assert!(model.fire("BandTaken", &mut cramped), "{cramped:?}");
    assert_eq!((cramped["b"], cramped["quick"]), (0, 0), "{cramped:?}");

    // `Buggy = 1`: each source falls once its own quiet has run, the net-zero cross-source
    // pair, and `NoQuickRegrow` breaks.
    let buggy = interp::with_buggy(&model, 1);
    let raise = walk(
        &buggy,
        &[
            "BandRise",
            "Tick",
            "BandUnwant",
            "PresRise",
            "BandQuiet",
            "BandFallOwnQuiet",
        ],
    );
    assert!(!buggy.check_invariant("NoQuickRegrow", &raise), "{raise:?}");
    let fold = walk(
        &buggy,
        &[
            "PresRise",
            "Tick",
            "PresUnwant",
            "PresQuiet",
            "BandRise",
            "PresFallOwnQuiet",
        ],
    );
    assert!(!buggy.check_invariant("NoQuickRegrow", &fold), "{fold:?}");
    let read = walk(
        &buggy,
        &[
            "PresRise",
            "Tick",
            "BandRise",
            "PresRead",
            "PresFallOwnQuiet",
        ],
    );
    assert!(!buggy.check_invariant("NoQuickRegrow", &read), "{read:?}");
    // …and a floor re-anchored on a step that is no rise leaves the engine's
    // count behind the rise's real age.
    let restarted = walk(&buggy, &["BandRise", "Tick", "BandUnwant", "Restart"]);
    assert_eq!(
        (restarted["age"], restarted["since"]),
        (1, 0),
        "{restarted:?}"
    );
    assert!(!buggy.check_invariant("TheAnchorIsTheLastRise", &restarted));
}

#[test]
fn a_fall_the_floor_holds_is_applied_and_the_restart_mutant_starves_it() {
    let model = chrome_regrow_floor_model();
    let live = chrome_regrow_floor_liveness();
    verify::liveness_proves_and_catches_tiered(&model, &live, "ChromeRegrowFloor");

    // Restart re-anchors the floor on every step, so time passes and the fall never comes due;
    // no state is stuck, only the property sees it.
    let restarting = verify::buggy_baseline_with(&model, "Restart");
    let starved = interp::nonprogress_under(&restarting, &live)
        .expect("a floor re-anchored on every step never lets the grid grow back");
    let entry = starved.entry();
    assert!(
        entry["b"] > entry["bw"] || entry["p"] > entry["pw"],
        "the loop is a waiting fall:\n{starved}"
    );
    assert!(
        starved.moves.contains("Restart") && starved.moves.contains("Tick"),
        "time passes and the anchor keeps moving:\n{starved}"
    );
    assert!(
        !starved.moves.contains("BandFall") && !starved.moves.contains("PresFall"),
        "{starved}"
    );
}
