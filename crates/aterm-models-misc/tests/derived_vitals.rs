// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_misc::{
    explain_honesty_model, scope_balance_model, stuck_verdict_liveness, stuck_verdict_model,
};
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

/// THE SCOPE BALANCE (`docs/DESIGN-system-awareness-2026-10-16.md` §7.7, §17): it holds with no
/// mutant on, each `Buggy` value breaks exactly the laws its row names, and a delegated member
/// read ahead of its launcher's counter moves no more than the launcher's own figure, the excess
/// shown as a negative residual. Tier-1 against the real split is `aterm-vitals`'
/// `conformance_scope_balance`.
#[test]
fn the_scope_balance_holds_and_each_law_has_its_own_mutant() {
    let model = scope_balance_model();
    assert_eq!(
        verify::sweep_faults(std::slice::from_ref(&model)),
        Vec::<String>::new()
    );
    verify::prove_and_catch_scalar(&model, "scope balance");

    for (dial, laws) in [
        (1, &["PartsSumToWhole", "ResidualIsShown"][..]),
        (2, &["DelegatedCountedOnce", "PartsSumToWhole"]),
        (3, &["PartsNeverNegative"]),
        (4, &["ResidualIsShown"]),
        (5, &["MatchedWindowsOnly"]),
    ] {
        let broken = interp::violated_invariants(&interp::with_buggy(&model, dial));
        assert_eq!(broken.into_iter().collect::<Vec<_>>(), laws, "Buggy {dial}");
    }

    // A delegated member read one unit ahead of its launcher's counter: the
    // launcher gives up its one unit, the tab shows both, and the residual
    // shows the unit no coalition counted.
    let mut s = model.init_state();
    for action in ["DelegatedWork", "DelegatedAhead", "Move", "Close"] {
        assert!(model.fire(action, &mut s), "{action}");
    }
    assert_eq!((s["g"], s["l"], s["d"]), (1, 1, 2));
    assert_eq!(
        (s["moved"], s["others"], s["tab"], s["rest"], s["residual"]),
        (1, 0, 2, 0, -1)
    );
}

/// THE EXPLANATION'S HONESTY (`docs/DESIGN-resource-details-2026-10-20.md` §17): it holds with no
/// mutant on, each `Buggy` value breaks exactly its own law, and the Deck's clause shows on the
/// third 5 s tick its candidate holds (10 s) and goes on the seventh it fails (30 s). Tier-1
/// against the real engine is `aterm-vitals`' `conformance_explain`.
#[test]
fn the_explanations_honesty_holds_and_each_law_has_its_own_mutant() {
    let model = explain_honesty_model();
    assert_eq!(
        verify::sweep_faults(std::slice::from_ref(&model)),
        Vec::<String>::new()
    );
    verify::prove_and_catch_scalar(&model, "explain honesty");

    for (dial, law) in [
        (1, "CauseOnlyWithEvidence"),
        (2, "ClaimNeverVouches"),
        (3, "StartIsEarliestBirth"),
        (4, "ContextOnlyWhereItHolds"),
        (5, "ContextOnlyWhereItHolds"),
        (6, "CauseNeedsFloor"),
        (7, "RoleNeedsBound"),
        (8, "UnknownNeverZero"),
        (9, "ClauseNeverFlickers"),
    ] {
        let broken = interp::violated_invariants(&interp::with_buggy(&model, dial));
        assert_eq!(
            broken.into_iter().collect::<Vec<_>>(),
            [law],
            "Buggy {dial}"
        );
    }

    let mut s = model.init_state();
    for (tick, clause) in [("Hold", 0), ("Hold", 0), ("Hold", 1)] {
        assert!(model.fire(tick, &mut s), "{tick}");
        assert_eq!(s["clause"], clause);
        assert!(model.check_invariant("ClauseNeverFlickers", &s));
    }
    for k in 1..=7 {
        assert!(model.fire("Fail", &mut s));
        assert_eq!(s["clause"], i64::from(k < 7), "failing tick {k}");
        assert!(model.check_invariant("ClauseNeverFlickers", &s));
    }

    // The init state is the 10:32 moment: Spotlight, after start-up; the
    // caller's tab minor, its work under the floor.
    let mut s = model.init_state();
    for action in ["Detect", "Attach"] {
        assert!(model.fire(action, &mut s), "{action}");
    }
    assert_eq!(
        (
            s["shown"],
            s["tabshown"],
            s["actstart"],
            s["tabstart"],
            s["role"],
            s["start"]
        ),
        (1, 0, 1, 0, 1, 0)
    );
}
