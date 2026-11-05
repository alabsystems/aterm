// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `SupervisorUsageWall`'s person's own `/login` over the wall (`Dialog = 1`): with
//! the dialog in the space the wall still ends (`TheUsageWallEnds`, minimal and caught), the
//! dialog's moves fire with no mutant on, and the 2026-10-31 loop (`LoginLeftOpen`), alone,
//! breaks the liveness that the committed model keeps; and of its managed worker
//! (`Managed = 1`), whose wall past the horizon concludes its work. Tier 1 is
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

/// A MANAGED WORKER'S WALL PAST THE HORIZON (`Managed = 1`, the model's scenario): with no mutant
/// on, its work concludes (`ManagedConcludes`) and `NoManagedWaitPastHorizon` holds, while a wall
/// whose clock reached the horizon (time passed under a hidden wall) is still waited out to its
/// act, and the wall still ends or concludes (`TheUsageWallEnds`). NEGATIVE CONTROL: the 2026-11-04 claim
/// worker's wait (`ManagedWaitsOut`), the one mutant on, breaks the law; at `Managed = 0` it has no
/// state to fire in.
#[test]
fn a_managed_worker_concludes_at_a_far_wall_and_waits_out_a_near_one() {
    let base = supervisor_usage_wall_model();
    let managed = interp::with_consts(&base, &[("Managed", 1)]);
    let fired = interp::fired_actions(&managed);
    for action in ["ManagedConcludes", "Act", "Taken", "WallHides", "WallShows"] {
        assert!(fired.contains(action), "{action} fires: {fired:?}");
    }
    assert!(interp::violated_invariants(&managed).is_empty());
    let states = interp::reachable_states(&managed);
    // `ResetAt = 2`, `Horizon = 1`: the shown wall at minute 1 resets inside the horizon.
    let near = states
        .iter()
        .filter(|st| st["wall"] == 1 && st["view"] == 0 && st["cut"] == 0 && st["clock"] == 1)
        .collect::<Vec<_>>();
    assert!(!near.is_empty(), "a near wall is reached");
    for st in near {
        assert!(
            managed.action_enabled("Tick", st) && !managed.action_enabled("ManagedConcludes", st),
            "a near wall is waited out: {st:?}"
        );
    }
    assert!(
        states.iter().any(|st| st["wall"] == 7),
        "a far wall concludes"
    );
    // The conclusion is the decider's own act, as weakly fair as `Act`; the committed liveness
    // cannot name it, since it never fires at `Managed = 0`.
    let mut live = supervisor_usage_wall_liveness();
    live.weak.push("ManagedConcludes");
    let cycle = interp::nonprogress_under(&managed, &live);
    assert!(cycle.is_none(), "{cycle:#?}");

    let only = |m: &aterm_spec::derive::Model| {
        let fired = interp::fired_actions(m);
        let mut one = interp::with_buggy(m, 1);
        one.actions
            .retain(|a| fired.contains(a.name) || a.name == "ManagedWaitsOut");
        one
    };
    assert!(
        interp::violated_invariants(&only(&managed)).contains("NoManagedWaitPastHorizon"),
        "the claim worker's wait breaks the law"
    );
    assert!(
        interp::violated_invariants(&only(&base)).is_empty(),
        "no managed worker, nothing to wait out"
    );
}
