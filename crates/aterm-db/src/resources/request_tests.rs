// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::resources::tests::{allocation, resources, rounds};
use crate::table::tests::Lab;
use aterm_secrets::commons::roster::Role;

fn binding() -> RequestBinding {
    RequestBinding {
        principal: [41; 32],
        account: [1; 32],
        revision: [2; 32],
        credential: [3; 32],
        policy: [4; 32],
        adapter: "example-adapter-v1".into(),
    }
}

fn request() -> BoundRequest {
    BoundRequest::new(binding(), [1; 16], 1, b"exact private request").unwrap()
}

fn changed(field: usize) -> BoundRequest {
    let mut changed = request();
    match field {
        0 => changed.binding.principal = [91; 32],
        1 => changed.binding.account = [91; 32],
        2 => changed.binding.revision = [91; 32],
        3 => changed.binding.credential = [91; 32],
        4 => changed.binding.policy = [91; 32],
        5 => changed.binding.adapter = "example-adapter-v2".into(),
        6 => changed.payload = Secret::new(b"different request"),
        7 => changed.cost = 2,
        _ => unreachable!(),
    }
    changed
}

#[test]
fn bound_inputs_are_owned_and_lent_only_after_durable_admission() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[1]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    let mut payload = b"exact private request".to_vec();
    let request = BoundRequest::new(binding(), [1; 16], 1, &payload).unwrap();
    payload.fill(b'x');
    let first = db
        .dispatch_bound(request, |input| {
            assert_eq!(input.payload(), b"exact private request");
            assert_eq!(input.binding(), &binding());
            let retained = db.attempt(input.id()).unwrap().unwrap();
            assert_eq!(retained.operation, input.operation(&allocation).unwrap());
            assert_eq!(db.snapshot().unwrap().used.values().sum::<u64>(), 1);
            None
        })
        .unwrap();
    assert!(first.invoked);
    let retry = db
        .dispatch_bound(self::request(), |_| panic!("retry reentered adapter"))
        .unwrap();
    assert!(!retry.invoked);
    assert_eq!(retry.receipt, first.receipt);
    assert!(
        db.dispatch_bound(
            BoundRequest::new(binding(), [2; 16], 1, b"next").unwrap(),
            |_| panic!("exhausted share entered adapter")
        )
        .is_err()
    );
}

#[test]
fn changing_any_bound_input_cannot_reuse_a_retained_attempt() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    db.dispatch_bound(request(), |_| None).unwrap();
    for field in 0..8 {
        assert!(
            db.dispatch_bound(changed(field), |_| panic!("changed input entered adapter"))
                .is_err(),
            "field {field}"
        );
    }
    assert_eq!(db.snapshot().unwrap().used.values().sum::<u64>(), 1);
}

#[test]
fn bound_requests_have_closed_identifiers_and_bounded_private_payloads() {
    for field in 0..5 {
        let mut binding = binding();
        match field {
            0 => binding.principal = [0; 32],
            1 => binding.account = [0; 32],
            2 => binding.revision = [0; 32],
            3 => binding.credential = [0; 32],
            4 => binding.policy = [0; 32],
            _ => unreachable!(),
        }
        assert!(BoundRequest::new(binding, [1; 16], 1, b"").is_err());
    }
    for adapter in ["".to_owned(), "bad route".into(), "x".repeat(65)] {
        let mut binding = binding();
        binding.adapter = adapter;
        assert!(BoundRequest::new(binding, [1; 16], 1, b"").is_err());
    }
    assert!(BoundRequest::new(binding(), [0; 16], 1, b"").is_err());
    assert!(BoundRequest::new(binding(), [1; 16], 0, b"").is_err());
    assert!(BoundRequest::new(binding(), [1; 16], 1, &vec![0; MAX_PAYLOAD + 1]).is_err());
    assert!(BoundRequest::new(binding(), [1; 16], 1, &vec![0; MAX_PAYLOAD]).is_ok());
}

#[test]
fn operation_identity_covers_allocation_and_separates_variable_fields() {
    let lab = Lab::new(&[Role::Writer]);
    let one = allocation(&lab, &[1]);
    let two = allocation(&lab, &[2]);
    assert_ne!(
        request().operation(&one).unwrap(),
        request().operation(&two).unwrap()
    );
    let mut left = request();
    left.binding.adapter = "ab".into();
    left.payload = Secret::new(b"c");
    let mut right = request();
    right.binding.adapter = "a".into();
    right.payload = Secret::new(b"bc");
    assert_ne!(
        left.operation(&one).unwrap(),
        right.operation(&one).unwrap()
    );
    let mut next = request();
    next.request = [2; 16];
    assert_ne!(
        request().operation(&one).unwrap(),
        next.operation(&one).unwrap()
    );
}

#[test]
fn account_aliases_share_one_allocation_on_two_disconnected_writers() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let allocation = allocation(&lab, &[2, 1]);
    let a = resources(&lab, 0, &allocation);
    let b = resources(&lab, 1, &allocation);
    a.initialize().unwrap();
    b.initialize().unwrap();
    for (db, id, account) in [(&a, 1, 11), (&a, 2, 12), (&b, 1, 13)] {
        let mut alias = binding();
        alias.account = [account; 32];
        let request = BoundRequest::new(alias, [id; 16], 1, b"request").unwrap();
        assert!(db.dispatch_bound(request, |_| None).unwrap().invoked);
    }
    for db in [&a, &b] {
        let mut alias = binding();
        alias.account = [14; 32];
        assert!(
            db.dispatch_bound(
                BoundRequest::new(alias, [3; 16], 1, b"another alias").unwrap(),
                |_| panic!("another account cannot expand its writer's share")
            )
            .is_err()
        );
    }
    rounds(&lab);
    assert_eq!(a.snapshot().unwrap().used, b.snapshot().unwrap().used);
    assert_eq!(a.snapshot().unwrap().used.values().sum::<u64>(), 3);
}

pub(crate) fn bound_transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(before, action, false)
}

fn transition_with(
    before: &aterm_spec::interp::State,
    action: &str,
    omit_payload: bool,
) -> aterm_spec::interp::State {
    let actions = [
        "Fresh",
        "Retry",
        "Principal",
        "Account",
        "Revision",
        "Credential",
        "Policy",
        "Adapter",
        "Payload",
        "Cost",
    ];
    let scenario = actions.iter().position(|name| *name == action).unwrap() + 1;
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    if action != "Fresh" {
        db.dispatch_bound(request(), |_| None).unwrap();
    }
    let requested = if scenario >= 3 {
        changed(scenario - 3)
    } else {
        request()
    };
    let expected_binding = requested.binding().clone();
    let expected_payload = requested.payload().to_vec();
    let mut entered = 0;
    let mut exact = true;
    let mut run = |input: &BoundRequest| {
        entered += 1;
        exact = input.binding() == &expected_binding && input.payload() == expected_payload;
        None
    };
    let result = if omit_payload && action == "Payload" {
        // Executable negative control: the old fingerprint with new bytes
        // incorrectly reports success for a different retained request.
        db.dispatch_once(
            requested.id(),
            requested.cost(),
            request().operation(&allocation).unwrap(),
            || run(&requested),
        )
    } else {
        db.dispatch_bound(requested, run)
    };
    let mut after = before.clone();
    after.insert("scenario", scenario as i64);
    after.insert("accepted", i64::from(result.is_ok()));
    after.insert("entered", entered);
    after.insert("exact", i64::from(exact));
    after
}

#[test]
fn bound_request_model_conforms_and_detects_an_omitted_payload() {
    let model = aterm_models_db::bound_request_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    let results = aterm_spec::fs_lane::each(&model.actions, |action| {
        let next = bound_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    assert_eq!(results.len(), 10);
    let unsafe_next = transition_with(&initial, "Payload", true);
    assert!(!model.check_invariant("ChangedBindingRefused", &unsafe_next));
}
