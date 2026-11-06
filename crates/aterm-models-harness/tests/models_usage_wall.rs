// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `SupervisorUsageWall`'s person's own `/login` over the wall (`Dialog = 1`): with
//! the dialog in the space the wall still ends (`TheUsageWallEnds`, minimal and caught), the
//! dialog's moves fire with no mutant on, and the 2026-10-31 loop (`LoginLeftOpen`), alone,
//! breaks the liveness that the committed model keeps; and of its managed worker
//! (`Managed = 1`), whose usage wall waits for quota. Tier 1 is
//! `aterm-agent/tests/supervise_conformance_usage_wall.rs`; the registry-wide sweep is
//! `aterm-models`' `non_vacuity`.

use aterm_models_harness::{supervisor_usage_wall_liveness, supervisor_usage_wall_model};
use aterm_spec::interp;

const DIALOG_MOVES: [&str; 4] = [
    "PersonOpensLogin",
    "DialogUnused",
    "LoginClosed",
    "PersonClosesLogin",
];

/// Audit the complete mutant space, which the committed-state and isolated-mutant
/// checks do not walk. A terminal conclusion must not multiply irrelevant quota history.
#[test]
fn usage_wall_mutants_remain_auditable_in_both_scenarios() {
    let base = supervisor_usage_wall_model();
    let managed = interp::with_consts(&base, &[("Managed", 1)]);
    let configs = [base, managed];
    let faults = aterm_spec::verify::sweep_faults(&configs);
    assert!(faults.is_empty(), "{faults:?}");
    aterm_spec::verify::audit_dead_negative_controls(&configs).expect("audited negative controls");
}

#[test]
fn the_usage_wall_ends_under_a_persons_own_login_and_its_audit_is_clean() {
    let m = supervisor_usage_wall_model();
    let fired = interp::fired_actions(&m);
    for action in DIALOG_MOVES {
        assert!(fired.contains(action), "{action} fires: {fired:?}");
    }
    assert!(
        !fired.contains("LoginLeftOpen"),
        "the mutant is dead at Buggy = 0"
    );
    let live = supervisor_usage_wall_liveness();
    if let Err(why) = aterm_spec::verify::audit_liveness(&m, &live) {
        panic!("{why}");
    }
}

/// THE NEGATIVE CONTROL: the committed model's moves with `LoginLeftOpen` their one mutant
/// break `TheUsageWallEnds` at `Buggy = 1`, and with the dialog dialled out (`Dialog = 0`) the
/// same baseline keeps it, so the catch is the dialog's.
#[test]
fn a_persons_login_left_open_over_the_wall_keeps_the_wall_from_ending() {
    let live = supervisor_usage_wall_liveness();
    let base = supervisor_usage_wall_model();
    // The moves that fire with no mutant on, the dialog's own kept where they cannot (the
    // fairness names them), and the one mutant.
    let alone = |m: &aterm_spec::derive::Model| {
        let fired = interp::fired_actions(m);
        let mut one = interp::with_buggy(m, 1);
        one.actions.retain(|a| {
            fired.contains(a.name) || DIALOG_MOVES.contains(&a.name) || a.name == "LoginLeftOpen"
        });
        one
    };
    assert!(
        interp::nonprogress_under(&alone(&base), &live).is_some(),
        "LoginLeftOpen breaks TheUsageWallEnds"
    );
    let undialled = interp::with_consts(&base, &[("Dialog", 0)]);
    assert!(
        interp::nonprogress_under(&alone(&undialled), &live).is_none(),
        "with no dialog, the mutant has nothing to leave open"
    );
}

/// Managed workers wait for quota; the former conclusion is a negative control.
#[test]
fn a_managed_worker_waits_for_its_reset_without_concluding() {
    let base = supervisor_usage_wall_model();
    let managed = interp::with_consts(&base, &[("Managed", 1)]);
    let fired = interp::fired_actions(&managed);
    for action in ["Tick", "Act", "Taken", "WallHides", "WallShows"] {
        assert!(fired.contains(action), "{action}: {fired:?}");
    }
    assert!(!fired.contains("ManagedConcludes"));
    assert!(interp::violated_invariants(&managed).is_empty());
    assert!(interp::nonprogress_under(&managed, &supervisor_usage_wall_liveness()).is_none());
    let only = |m: &aterm_spec::derive::Model| {
        let fired = interp::fired_actions(m);
        let mut one = interp::with_buggy(m, 1);
        one.actions
            .retain(|a| fired.contains(a.name) || a.name == "ManagedConcludes");
        one
    };
    assert!(interp::violated_invariants(&only(&managed)).contains("NoUsageWallConclusion"));
    assert!(interp::violated_invariants(&only(&base)).is_empty());
}
