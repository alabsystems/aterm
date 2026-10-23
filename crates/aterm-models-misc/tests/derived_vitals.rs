// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_misc::{
    end_by_proof_model, explain_honesty_model, scope_balance_model, stuck_verdict_model,
};
use aterm_spec::{interp, verify};

/// THE STUCK VERDICT (`docs/DESIGN-system-awareness-2026-10-16.md` §17): it holds with no mutant
/// on, each law is broken by its own `Buggy` value alone, the environment mutants each break the
/// age law and the liveness alone, and the healthy lane reads as the design's curl row. Tier-1
/// against the real step is `aterm-vitals`' `conformance_stuck_verdict`.
#[test]
fn the_stuck_verdict_holds_and_each_law_has_its_own_mutant() {
    let model = stuck_verdict_model();

    for (dial, law) in [
        (1, "NeverStuckWhileProgressing"),
        (2, "LegitWaitNeverAccrues"),
        (3, "RefusedReadingIsNotZeroProgress"),
        (4, "UnnamedNeverEntersStuck"),
        (5, "NoFlap"),
        (6, "FlaggedOnTime"),
    ] {
        let mut m = interp::with_buggy(&model, dial);
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

    let mut s = model.init_state();
    assert!(model.fire("QuietTick", &mut s));
    assert!(model.fire("QuietTick", &mut s));
    assert_eq!((s["q"], s["v"]), (2, 1), "quiet at the T3 band");
    assert!(model.fire("QuietTick", &mut s));
    assert!(model.fire("QuietTick", &mut s));
    assert_eq!((s["q"], s["v"]), (4, 2), "stuck at the T8 band");
    assert!(model.fire("MuGrows", &mut s));
    assert!(model.fire("MuGrows", &mut s));
    assert_eq!((s["q"], s["v"]), (2, 2));
    assert!(model.fire("MuGrows", &mut s));
    assert_eq!((s["q"], s["v"]), (1, 0));
    let mut s = model.init_state();
    assert!(model.fire("Relabel", &mut s));
    for _ in 0..4 {
        assert!(model.fire("QuietTick", &mut s));
    }
    assert_eq!((s["cls"], s["q"], s["v"]), (3, 4, 1));
    assert!(model.fire("Relabel", &mut s));
    assert_eq!((s["cls"], s["v"]), (2, 2));
}

/// END BY PROOF (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.4): the protocol holds with
/// no mutant — no signal follows a read whose identity moved or whose tier was lost — and the
/// mutant that signals without the judgment breaks that law. Tier-1 against the real
/// `end_proven` is `aterm-verify`'s `conformance_end_by_proof`.
#[test]
fn the_end_by_proof_holds_and_its_mutant_signals_after_a_move() {
    let model = end_by_proof_model();

    let mut buggy = interp::with_buggy(&model, 1);
    buggy.invariants.retain(|i| i.name == "NoUnprovenSignal");
    assert!(
        interp::bmc(&buggy).is_err(),
        "Buggy 1 signals after a moved identity or a lost tier"
    );

    let mut s = model.init_state();
    assert!(!model.action_enabled("Term", &s), "no SIGTERM unjudged");
    assert!(model.fire("Move", &mut s));
    assert!(
        !model.action_enabled("Judge", &s),
        "no judgment holds after a move"
    );
    assert!(model.fire("Stop", &mut s));
    assert_eq!((s["v"], s["bad"]), (2, 0), "a moved read sends nothing");
    let mut s = model.init_state();
    assert!(model.fire("Judge", &mut s));
    assert!(model.fire("Term", &mut s));
    assert!(
        !model.action_enabled("Kill", &s),
        "the SIGTERM's judgment does not serve the SIGKILL"
    );
    assert!(model.fire("Lose", &mut s));
    assert!(
        !model.action_enabled("Judge", &s) && !model.action_enabled("Kill", &s),
        "no SIGKILL once the tier is lost"
    );
    assert!(model.fire("Stop", &mut s));
    assert_eq!((s["v"], s["bad"]), (2, 0), "a lost tier sends nothing more");
}

/// THE SCOPE BALANCE (`docs/DESIGN-system-awareness-2026-10-16.md` §7.7, §17): it holds with no
/// mutant on, each `Buggy` value breaks exactly the laws its row names, and a delegated member
/// read ahead of its launcher's counter moves no more than the launcher's own figure, the excess
/// shown as a negative residual. Tier-1 against the real split is `aterm-vitals`'
/// `conformance_scope_balance`.
#[test]
fn the_scope_balance_holds_and_each_law_has_its_own_mutant() {
    let model = scope_balance_model();

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
