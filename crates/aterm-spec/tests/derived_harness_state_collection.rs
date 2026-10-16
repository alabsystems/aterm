// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::{
    harness_upgrade_state_collection_liveness, harness_upgrade_state_collection_model,
};
use aterm_spec::{interp, verify};

/// The Codex records' prune: only a closed tab's record goes, only once it is settled and only
/// after its week, and nothing writes a closed tab's settled record young again. Each invariant
/// is caught by its own `Buggy` member alone. Tier-1 against the real
/// `upgrade_codex_drive::prune` is in aterm-agent's `upgrade_codex_drive` tests.
#[test]
fn only_a_closed_tabs_settled_week_old_record_is_pruned() {
    let model = harness_upgrade_state_collection_model();
    verify::prove_and_catch_scalar(&model, "harness upgrade state collection");

    for (mutant, invariant) in [
        ("PruneLiveTab", "OnlyAGoneTabsRecordGoes"),
        ("PruneInFlight", "OnlyASettledRecordGoes"),
        ("PruneFresh", "OnlyAWeekOldRecordGoes"),
        ("Rewrite", "AClosedSettledRecordStaysAged"),
    ] {
        let mut alone = verify::buggy_baseline_with(&model, mutant);
        alone.invariants.retain(|inv| inv.name == invariant);
        assert!(
            interp::bmc(&alone).is_err(),
            "`{mutant}` alone breaks `{invariant}`"
        );
    }

    // The healthy lane, step by step: a settled record of a live tab stays,
    // however old; the tab closes; the week passes; the prune takes it.
    let mut s = model.init_state();
    assert!(model.fire("Settle", &mut s));
    assert!(model.fire("Week", &mut s));
    assert!(!model.action_enabled("Prune", &s), "its tab lives");
    assert!(model.fire("TabCloses", &mut s));
    assert!(
        model.action_enabled("Prune", &s),
        "closed, settled, a week old"
    );
    let mut fresh = s.clone();
    fresh.insert("old", 0);
    assert!(
        !model.action_enabled("Prune", &fresh),
        "not before its week"
    );
    let mut in_flight = s.clone();
    in_flight.insert("stage", 1);
    assert!(
        !model.action_enabled("Prune", &in_flight),
        "never a restart in flight"
    );
    assert!(model.fire("Prune", &mut s));
    assert_eq!(s["kept"], 0);
}

/// "A closed tab's settled record goes" under the clock and the prune's own weak fairness;
/// `Rewrite`, a pass that writes the record again at every look, breaks it alone. Without a
/// `Prune` the record is kept for ever while the invariants and the deadlock check see nothing
/// wrong: the property's negative control.
#[test]
fn a_closed_tabs_settled_record_goes_and_the_historical_lane_kept_it_for_ever() {
    let model = harness_upgrade_state_collection_model();
    let live = harness_upgrade_state_collection_liveness();
    verify::liveness_proves_and_catches_tiered(
        &model,
        &live,
        "HarnessUpgradeStateCollection collects",
    );

    let rewriting = verify::buggy_baseline_with(&model, "Rewrite");
    let cycle = interp::nonprogress_under(&rewriting, &live).expect("young for ever");
    assert_eq!(
        (cycle.entry()["tab"], cycle.entry()["stage"]),
        (0, 2),
        "{cycle}"
    );
    assert!(cycle.moves.contains("Rewrite"), "{cycle}");

    let mut historical = interp::with_buggy(&model, 0);
    historical.actions.retain(|a| a.name != "Prune");
    let mut unpruned = live.clone();
    unpruned.weak.retain(|a| *a != "Prune");
    assert!(
        interp::bmc(&historical).is_ok(),
        "no invariant sees a record that is never removed"
    );
    let kept = interp::nonprogress_under(&historical, &unpruned)
        .expect("without the prune a closed tab's settled record is kept for ever");
    assert_eq!(
        (
            kept.entry()["tab"],
            kept.entry()["stage"],
            kept.entry()["kept"]
        ),
        (0, 2, 1),
        "{kept}"
    );
}
