// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::dial_cadence_model;
use aterm_spec::interp;
use std::collections::BTreeSet;

/// The law each mutant of `DbDialCadence` is written to break, in its dial's order.
const LAWS: [&str; 8] = [
    "BeaconGrantsNothing",
    "SilenceProbesOnce",
    "PendingNeverRests",
    "HealthyVouchInert",
    "AuditWithin",
    "WalkEnds",
    "VouchEndsSilence",
    "OwedWalksOnce",
];

/// The model holds with no mutant on while every action fires, and each mutant breaks exactly
/// the law it is written for and no other.
#[test]
fn dial_cadence_holds_and_each_mutant_breaks_its_one_law() {
    let model = dial_cadence_model();
    let states = interp::bmc(&model)
        .unwrap_or_else(|(state, law)| panic!("{law} is broken with no mutant on, at {state:?}"));
    let fired = interp::fired_actions(&model);
    let dead: Vec<_> = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !fired.contains(name))
        .collect();
    assert!(dead.is_empty(), "actions that never fire: {dead:?}");
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<i64>>());
    for (dial, law) in (1..).zip(LAWS) {
        assert_eq!(
            interp::violated_invariants(&interp::with_buggy(&model, dial)),
            BTreeSet::from([law]),
            "mutant {dial} breaks {law} and nothing else"
        );
    }
    assert!(states > 1, "{states} states");
}
