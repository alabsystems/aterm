// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_misc::{stuck_verdict_liveness, stuck_verdict_model};
use aterm_spec::{interp, verify};

/// THE STUCK VERDICT (`docs/DESIGN-system-awareness-2026-10-16.md` §17): it holds with no mutant
/// on, each law is broken by its own `Buggy` value alone, the environment mutants each break the
/// age law and the liveness alone, and the healthy lane reads as the design's curl row. Tier-1
/// against the real step is `aterm-vitals`' `conformance_stuck_verdict`.
#[test]
fn the_stuck_verdict_holds_and_each_law_has_its_own_mutant() {
    let model = stuck_verdict_model();
    verify::prove_and_catch_scalar(&model, "stuck verdict");

    for (dial, law) in [
        (1, "NeverStuckWhileProgressing"),
        (2, "LegitWaitNeverAccrues"),
        (3, "RefusedReadingIsNotZeroProgress"),
        (4, "UnnamedNeverEntersStuck"),
        (5, "NoFlap"),
        (6, "FlaggedOnTime"),
    ] {
        let mut m = interp::with_buggy(&model, dial);
        // The environment mutants (dial 1's own actions) are not this law's.
        m.actions
            .retain(|a| !a.name.ends_with("AsProgress") && a.name != "RefusedResetsAge");
        for other in &model.invariants {
            if other.name == law {
                continue;
            }
            let mut only = m.clone();
            only.invariants.retain(|i| i.name == other.name);
            assert!(
                dial == 1 || interp::bmc(&only).is_ok(),
                "Buggy {dial} breaks `{}` as well as `{law}`",
                other.name
            );
        }
        m.invariants.retain(|i| i.name == law);
        assert!(interp::bmc(&m).is_err(), "Buggy {dial} breaks `{law}`");
    }
    for mutant in [
        "CrRedrawAsProgress",
        "IdleChurnAsProgress",
        "RefusedResetsAge",
    ] {
        let mut alone = verify::buggy_baseline_with(&model, mutant);
        alone
            .invariants
            .retain(|i| i.name == "AgeResetsOnlyOnProgress");
        assert!(
            interp::bmc(&alone).is_err(),
            "`{mutant}` alone breaks the age law"
        );
    }
    verify::liveness_proves_and_catches_tiered(&model, &stuck_verdict_liveness(), "stuck verdict");

    // The healthy lane, as the design's curl row: quiet at φ 3, stuck at φ 8.
    let mut s = model.init_state();
    assert!(model.fire("QuietTick", &mut s));
    assert!(model.fire("QuietTick", &mut s));
    assert_eq!((s["q"], s["v"]), (2, 1), "quiet at the T3 band");
    assert!(model.fire("QuietTick", &mut s));
    assert!(model.fire("QuietTick", &mut s));
    assert_eq!((s["q"], s["v"]), (4, 2), "stuck at the T8 band");
    // μ grows: stuck holds while φ ≥ 3, then falls to fine.
    assert!(model.fire("MuGrows", &mut s));
    assert!(model.fire("MuGrows", &mut s));
    assert_eq!((s["q"], s["v"]), (2, 2));
    assert!(model.fire("MuGrows", &mut s));
    assert_eq!((s["q"], s["v"]), (1, 0));
    // An unnamed wait stays capped at quiet however long it waits.
    let mut s = model.init_state();
    assert!(model.fire("Relabel", &mut s));
    for _ in 0..4 {
        assert!(model.fire("QuietTick", &mut s));
    }
    assert_eq!((s["cls"], s["q"], s["v"]), (3, 4, 1));
    // Named at φ ≥ 8: stuck at once.
    assert!(model.fire("Relabel", &mut s));
    assert_eq!((s["cls"], s["v"]), (2, 2));
}
