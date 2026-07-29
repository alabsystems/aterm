// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for exact-instance capture-namespace retention.
//!
//! Every lease-state × PID-liveness input is passed to the genuine shipping
//! decision function, then independently projected onto the drift-free model.
//! A destructive held-lease mutant is rejected so the binding cannot pass
//! merely because the `Decide` guard is enabled.

#![cfg(test)]

use aterm_spec::derive::{Model, exact_instance_retention_model};
use aterm_spec::interp::State;
use aterm_spec::verify::validate_transition_tiered;
use std::collections::BTreeSet;

use crate::control_auth::{
    InstanceLeaseState, InstanceSweepDecision, decide_instance_namespace_sweep,
};

const DECIDE_ACTION: &str = "Decide";

fn lease_code(lease: InstanceLeaseState) -> i64 {
    match lease {
        InstanceLeaseState::Missing => 0,
        InstanceLeaseState::Held => 1,
        InstanceLeaseState::Acquirable => 2,
        InstanceLeaseState::Invalid => 3,
    }
}

fn decision_code(decision: InstanceSweepDecision) -> i64 {
    match decision {
        InstanceSweepDecision::Keep => 1,
        InstanceSweepDecision::Remove => 2,
    }
}

pub(crate) fn project_before(model: &Model, lease: InstanceLeaseState, pid_alive: bool) -> State {
    let mut state = model.init_state();
    state.insert("lease", lease_code(lease));
    state.insert("pid_alive", i64::from(pid_alive));
    state
}

fn project_after(before: &State, decision: InstanceSweepDecision) -> State {
    let mut state = before.clone();
    state.insert("decision", decision_code(decision));
    state
}

fn assert_invariants(model: &Model, state: &State, context: &str) {
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, state),
            "{context}: post-state violates {}::{}: {state:?}",
            model.name,
            invariant.name,
        );
    }
}

// These four actions select facts supplied by the filesystem/OS environment;
// no shipping state transition implements the observation itself. The real
// sweeper produces those facts and `Decide` consumes them. Keep that boundary
// explicit rather than attaching a refinement to a function that does not own
// the transition.
#[allow(dead_code)]
#[aterm_spec::spec_unmodeled(
    machine = "ExactInstanceRetention",
    action = "SelectHeld",
    reason = "environment observation: advisory-lock contention supplies Held; the exhaustive \
              Tier-1 matrix projects that real decision input and validates its consumption"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ExactInstanceRetention",
    action = "SelectFree",
    reason = "environment observation: successful advisory-lock acquisition supplies Free; the \
              exhaustive Tier-1 matrix projects that real decision input and validates it"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ExactInstanceRetention",
    action = "SelectMalformed",
    reason = "environment observation: link/type/I/O failures supply Invalid; the exhaustive \
              Tier-1 matrix projects that fail-closed decision input and validates it"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ExactInstanceRetention",
    action = "ObservePidAlive",
    reason = "environment observation: the OS liveness probe supplies this compatibility fact \
              only for a missing legacy lease; the Tier-1 matrix covers both values"
)]
fn explicit_environment_scope_waivers() {}

#[test]
fn real_instance_sweep_decision_conforms_for_entire_lease_pid_matrix() {
    use InstanceLeaseState::{Acquirable, Held, Invalid, Missing};
    use InstanceSweepDecision::{Keep, Remove};

    let model = exact_instance_retention_model();
    let mut visited = 0_u8;
    let mut kept = 0_u8;
    let mut removed = 0_u8;

    let anchored: BTreeSet<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "ExactInstanceRetention")
        .map(|anchor| anchor.action)
        .collect();
    assert_eq!(
        anchored,
        BTreeSet::from(["Decide"]),
        "only the genuine shipping decision is refinement-bound"
    );
    let waived: BTreeSet<_> = aterm_spec::xref::waivers()
        .filter(|waiver| waiver.machine == "ExactInstanceRetention")
        .map(|waiver| waiver.action)
        .collect();
    assert_eq!(
        waived,
        BTreeSet::from([
            "ObservePidAlive",
            "SelectFree",
            "SelectHeld",
            "SelectMalformed",
        ]),
        "all environment-only setup actions need explicit scope boundaries"
    );

    for lease in [Missing, Held, Acquirable, Invalid] {
        for pid_alive in [false, true] {
            let before = project_before(&model, lease, pid_alive);
            let mut reachable = model.init_state();
            let selector = match lease {
                Missing => None,
                Held => Some("SelectHeld"),
                Acquirable => Some("SelectFree"),
                Invalid => Some("SelectMalformed"),
            };
            if let Some(selector) = selector {
                assert!(model.fire(selector, &mut reachable));
            }
            if pid_alive {
                assert!(model.fire("ObservePidAlive", &mut reachable));
            }
            assert_eq!(
                before, reachable,
                "projection must be reachable through the modeled observation actions"
            );
            assert!(
                model.action_enabled(DECIDE_ACTION, &before),
                "Decide must cover lease={lease:?}, pid_alive={pid_alive}"
            );

            let real_decision = decide_instance_namespace_sweep(lease, pid_alive);
            let after = project_after(&before, real_decision);
            assert!(
                model.successors(DECIDE_ACTION, &before).contains(&after),
                "shipping decision has no exact model successor: \
                 lease={lease:?}, pid_alive={pid_alive}, decision={real_decision:?}"
            );

            let label = format!("exact-instance retention lease={lease:?} pid_alive={pid_alive}");
            let (conforms, evidence) = validate_transition_tiered(
                &model,
                &[],
                &before,
                &after,
                Some(DECIDE_ACTION),
                &label,
            );
            assert!(conforms, "{label}: {evidence}");
            assert_invariants(&model, &after, &label);

            visited |= 1 << (lease_code(lease) * 2 + i64::from(pid_alive));
            match real_decision {
                Keep => kept += 1,
                Remove => removed += 1,
            }
        }
    }

    assert_eq!(visited, u8::MAX, "all eight input rows must be exercised");
    assert_eq!(
        (kept, removed),
        (5, 3),
        "matrix must include both retention outcomes"
    );

    // Negative control: removing a held exact-instance lease recreates the
    // destructive false-stale policy. The same enabled guard must reject this
    // post-state and its named invariant must fail.
    let held = project_before(&model, Held, false);
    assert!(model.action_enabled(DECIDE_ACTION, &held));
    let destructive = project_after(&held, Remove);
    assert!(
        !model
            .successors(DECIDE_ACTION, &held)
            .contains(&destructive)
    );
    let (mutant_conforms, evidence) = validate_transition_tiered(
        &model,
        &[],
        &held,
        &destructive,
        Some(DECIDE_ACTION),
        "exact-instance retention held-lease negative control",
    );
    assert!(
        !mutant_conforms,
        "held-lease removal mutant unexpectedly conformed: {evidence}"
    );
    assert!(!model.check_invariant("HeldNeverRemoved", &destructive));
}
