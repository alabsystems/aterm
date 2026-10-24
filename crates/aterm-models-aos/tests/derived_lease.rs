// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 state-space and negative-control checks for `aos_worker_lease_model`.

use std::collections::BTreeSet;

use aterm_models_aos::{aos_worker_lease_liveness, aos_worker_lease_model};
use aterm_spec::derive::{InvariantKind, Model};
use aterm_spec::interp::{self, State};
use aterm_spec::verify;

/// Each dial and the one law it is written to break.
const DIALS: [(i64, &str); 6] = [
    (1, "LeasedByBusy"),
    (2, "NoDuplicateAccept"),
    (3, "NoStaleAccept"),
    (4, "NoAcceptAfterCancel"),
    (5, "NoContradictionIgnored"),
    (6, "AttemptsBounded"),
];

/// The state after firing `steps` in order from the initial state, each of which must be enabled.
fn walk(model: &Model, steps: &[&str]) -> State {
    let mut state = model.init_state();
    for (done, step) in steps.iter().enumerate() {
        assert!(
            model.fire(step, &mut state),
            "`{step}` is enabled after {:?}",
            &steps[..done]
        );
    }
    state
}

fn at_most_one(model: &Model) -> Model {
    interp::with_consts(model, &[("MaxAttempts", 1)])
}

#[test]
fn the_lease_ledger_holds_and_each_law_has_its_own_mutant() {
    let model = aos_worker_lease_model();

    let actions: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    let mut live: BTreeSet<&str> = actions.clone();
    live.remove("ExpireKeepsLease");
    assert_eq!(interp::fired_actions(&model), live);
    assert_eq!(interp::fired_actions(&at_most_one(&model)), live);

    let dials: Vec<i64> = DIALS.iter().map(|&(dial, _)| dial).collect();
    assert_eq!(model.mutant_dials(), dials);
    let claims: BTreeSet<&str> = model
        .invariants
        .iter()
        .filter(|i| i.kind == InvariantKind::Claim)
        .map(|i| i.name)
        .collect();
    assert_eq!(claims, DIALS.iter().map(|&(_, law)| law).collect());

    for (dial, law) in DIALS {
        assert_eq!(
            interp::violated_invariants(&interp::with_buggy(&model, dial)),
            BTreeSet::from([law]),
            "Buggy {dial}"
        );
    }
    // At one attempt the first expiry exhausts the job, so no late result ever finds it open:
    // dial 3 has nothing to accept there, and every other dial still breaks its own law alone.
    for (dial, law) in DIALS {
        let caught = interp::violated_invariants(&interp::with_buggy(&at_most_one(&model), dial));
        let want = if dial == 3 {
            BTreeSet::new()
        } else {
            BTreeSet::from([law])
        };
        assert_eq!(caught, want, "MaxAttempts 1, Buggy {dial}");
    }

    let settles = aos_worker_lease_liveness();
    assert!(interp::nonprogress_under(&model, &settles).is_none());
    let kept = verify::buggy_baseline_with(&model, "ExpireKeepsLease");
    let cycle = interp::nonprogress_under(&kept, &settles).expect("dial 1 breaks Settles");
    assert_eq!(cycle.entry()["job"], 1, "the job stays leased: {cycle}");
    assert_eq!(verify::audit_liveness(&model, &settles), Ok(1));
}

#[test]
fn an_expired_lease_reopens_the_job_and_its_late_result_is_compared_on_acceptance() {
    let model = aos_worker_lease_model();
    let s = walk(&model, &["LeaseA", "SendA", "ExpireA"]);
    assert_eq!((s["job"], s["attempts"], s["holder"]), (0, 1, 0));
    assert_eq!((s["wa"], s["ea"], s["ra"]), (2, 1, 1));
    assert!(!model.action_enabled("AcceptA", &s), "the lease is gone");
    assert!(
        !model.action_enabled("LeaseA", &s),
        "a draining worker takes no lease"
    );
    assert!(model.action_enabled("LeaseB", &s));

    let s = walk(
        &model,
        &["LeaseA", "SendA", "ExpireA", "LateA", "LeaseB", "SendB"],
    );
    assert_eq!((s["wa"], s["late"], s["job"], s["eb"]), (6, 1, 1, 2));
    let s = walk(
        &model,
        &[
            "LeaseA", "SendA", "ExpireA", "LateA", "LeaseB", "SendB", "AcceptB",
        ],
    );
    assert_eq!((s["job"], s["acc"], s["late"], s["out"]), (2, 1, 0, 0));
    assert!(model.action_enabled("Finish", &s));

    let s = walk(
        &model,
        &["LeaseA", "SendA", "ExpireA", "LeaseB", "SendB", "AcceptB"],
    );
    assert!(!model.action_enabled("Finish", &s), "A still drains");
    let s = walk(
        &model,
        &[
            "LeaseA", "SendA", "ExpireA", "LeaseB", "SendB", "AcceptB", "LateA", "Finish",
        ],
    );
    assert_eq!((s["wa"], s["out"]), (6, 1));
}

#[test]
fn a_lie_is_contradicted_by_the_honest_late_claim_in_either_order() {
    let model = aos_worker_lease_model();
    for steps in [
        [
            "LeaseA", "SendA", "ExpireA", "LeaseB", "LieB", "AcceptB", "LateA",
        ]
        .as_slice(),
        [
            "LeaseA", "SendA", "ExpireA", "LateA", "LeaseB", "LieB", "AcceptB",
        ]
        .as_slice(),
        [
            "LeaseB", "LieB", "ExpireB", "LeaseA", "SendA", "AcceptA", "LateB",
        ]
        .as_slice(),
        [
            "LeaseB", "LieB", "ExpireB", "LateB", "LeaseA", "SendA", "AcceptA",
        ]
        .as_slice(),
    ] {
        let s = walk(&model, steps);
        assert_eq!((s["job"], s["out"], s["ignored"]), (2, 2, 0), "{steps:?}");
    }
    // A sole lie is accepted and the search completes: re-judging it is §11.3 step 5's, not the
    // ledger's (design §11.7: the model does not cover it).
    let s = walk(&model, &["LeaseB", "LieB", "AcceptB", "Finish"]);
    assert_eq!((s["acc"], s["out"]), (2, 1));
}

#[test]
fn a_differing_resend_kills_its_sender_and_voids_even_a_finished_search() {
    let model = aos_worker_lease_model();
    let s = walk(&model, &["LeaseB", "SendB", "AcceptB", "LieB", "CompareB"]);
    assert_eq!((s["wb"], s["out"]), (7, 2), "killed, not drained");
    let s = walk(
        &model,
        &["LeaseB", "SendB", "AcceptB", "ResendB", "CompareB"],
    );
    assert_eq!((s["wb"], s["out"], s["twice"]), (0, 0, 0));
    assert!(
        !model.action_enabled("ResendB", &s) && !model.action_enabled("LieB", &s),
        "one resend"
    );
    let s = walk(
        &model,
        &["LeaseB", "SendB", "AcceptB", "Finish", "LieB", "CompareB"],
    );
    assert_eq!((s["wb"], s["out"]), (7, 2));
}

#[test]
fn the_last_lease_lost_or_expired_exhausts_the_job_and_voids() {
    let model = aos_worker_lease_model();
    for steps in [
        ["LeaseA", "ExpireA", "LeaseB", "ExpireB"].as_slice(),
        ["LeaseA", "LoseA", "LeaseB", "LoseB"].as_slice(),
    ] {
        let s = walk(&model, steps);
        assert_eq!((s["job"], s["attempts"], s["out"]), (4, 2, 2), "{steps:?}");
    }
    let one = at_most_one(&model);
    let s = walk(&one, &["LeaseA", "ExpireA"]);
    assert_eq!((s["job"], s["attempts"], s["out"]), (4, 1, 2));

    let reissue = interp::with_buggy(&model, 6);
    let s = walk(&reissue, &["LeaseA", "ExpireA", "LeaseB", "ExpireB"]);
    assert_eq!((s["job"], s["holder"], s["wb"], s["eb"]), (1, 2, 1, 3));
    assert!(!reissue.check_invariant("AttemptsBounded", &s));
    assert!(reissue.check_invariant("AttemptsBox", &s));
    assert!(reissue.check_invariant("LeasedByBusy", &s));
}

#[test]
fn an_open_job_with_no_worker_left_to_take_it_voids() {
    let model = aos_worker_lease_model();
    for (steps, wa, wb) in [
        (["LeaseA", "ExpireA", "LoseB"].as_slice(), 2, 4),
        (["LoseA", "LeaseB", "ExpireB"].as_slice(), 4, 2),
        (["LoseA", "LoseB"].as_slice(), 4, 4),
    ] {
        let s = walk(&model, steps);
        assert_eq!(
            (s["job"], s["out"], s["wa"], s["wb"]),
            (3, 2, wa, wb),
            "{steps:?}"
        );
    }
    let s = walk(&model, &["LeaseA", "SendA", "AcceptA", "LoseB"]);
    assert_eq!(s["out"], 0, "a claimed job needs no worker");
}

#[test]
fn an_interrupt_cancels_the_holder_whose_result_is_then_dropped() {
    let model = aos_worker_lease_model();
    let s = walk(&model, &["LeaseA", "SendA", "Cancel"]);
    assert_eq!((s["job"], s["holder"], s["wa"], s["out"]), (3, 0, 3, 2));
    assert!(!model.action_enabled("AcceptA", &s));
    assert!(
        !model.action_enabled("AckA", &s),
        "its result is read first"
    );
    let s = walk(&model, &["LeaseA", "SendA", "Cancel", "DropA", "AckA"]);
    assert_eq!((s["wa"], s["post"]), (6, 0));

    let holding = interp::with_buggy(&model, 4);
    let s = walk(&holding, &["LeaseA", "SendA", "Cancel"]);
    assert_eq!((s["job"], s["wa"], s["out"]), (1, 1, 2));
    let s = walk(&holding, &["LeaseA", "SendA", "Cancel", "AcceptA"]);
    assert!(!holding.check_invariant("NoAcceptAfterCancel", &s));
}

#[test]
fn drains_end_in_the_order_they_began_and_finish_waits_for_them() {
    let model = aos_worker_lease_model();
    let s = walk(&model, &["LeaseA", "ExpireA", "LeaseB", "Cancel"]);
    assert_eq!((s["wa"], s["ea"], s["wb"], s["eb"]), (2, 1, 3, 2));
    assert!(model.action_enabled("DrainTimeoutA", &s));
    assert!(!model.action_enabled("DrainTimeoutB", &s), "B began later");
    let s = walk(
        &model,
        &["LeaseA", "ExpireA", "LeaseB", "Cancel", "DrainTimeoutA"],
    );
    assert!(model.action_enabled("DrainTimeoutB", &s));

    let s = walk(&model, &["LeaseA", "ExpireA", "LeaseB", "SendB", "AcceptB"]);
    assert!(!model.action_enabled("Finish", &s));
    for end in ["DrainTimeoutA", "AckA", "LoseA"] {
        let s = walk(
            &model,
            &["LeaseA", "ExpireA", "LeaseB", "SendB", "AcceptB", end],
        );
        assert!(model.action_enabled("Finish", &s), "{end}");
    }
}

#[test]
fn a_late_result_for_an_open_job_is_never_its_claim() {
    let model = aos_worker_lease_model();
    let steps = ["LeaseA", "SendA", "ExpireA", "LateA"];
    let s = walk(&model, &steps);
    assert_eq!((s["job"], s["late"], s["stale"]), (0, 1, 0));
    let fenceless = interp::with_buggy(&model, 3);
    let s = walk(&fenceless, &steps);
    assert_eq!((s["job"], s["acc"], s["stale"]), (2, 1, 1));
    assert!(!fenceless.check_invariant("NoStaleAccept", &s));
}
