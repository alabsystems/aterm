// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `AccountUpkeepVerdict`: it holds with no mutant on, every action fires, and each
//! mutant breaks the one law it was written to break and no other. Tier 1 is `aterm-agent`'s
//! `accounts_upkeep_memo` tests (`Hit`, `Run`) and `aterm-gui`'s `accounts_keeper` tests
//! (`Look`). The non-vacuity sweep is `aterm-models`' `non_vacuity`.

use aterm_models_harness::account_upkeep_verdict_model;
use aterm_spec::interp;

/// Each mutant (its `Buggy` value) and the one law it breaks.
const MUTANTS: [(i64, &str); 6] = [
    (1, "HitImpliesReplayEqual"),
    (2, "HitWithinHorizon"),
    (3, "KeptIsClean"),
    (4, "UnsettledNotKept"),
    (5, "NoRunWithoutCause"),
    (6, "DueOnlyForCause"),
];

#[test]
fn the_upkeep_verdict_holds_and_each_mutant_breaks_its_own_law_alone() {
    let model = account_upkeep_verdict_model();
    assert!(interp::prove_and_catch(&model) > 1);
    let fired = interp::fired_actions(&interp::with_buggy(&model, 0));
    for action in &model.actions {
        assert!(fired.contains(action.name), "{} never fires", action.name);
    }
    for (buggy, law) in MUTANTS {
        let broken = interp::violated_invariants(&interp::with_buggy(&model, buggy));
        assert_eq!(
            broken.into_iter().collect::<Vec<_>>(),
            [law],
            "Buggy = {buggy}"
        );
    }
}
