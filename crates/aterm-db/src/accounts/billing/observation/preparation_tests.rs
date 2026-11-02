// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::secret::Secret;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn preparation_transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(before, action, false)
}

fn transition_with(
    before: &aterm_spec::interp::State,
    action: &str,
    skip_validation: bool,
) -> aterm_spec::interp::State {
    let (f, broker, token, _) = super::broker_tests::setup();
    let parsed = AtomicUsize::new(0);
    let entered = AtomicUsize::new(0);
    let bytes = b"owned provider input";
    let prepare = |input: &[u8], until: Instant| {
        parsed.fetch_add(1, Ordering::SeqCst);
        assert_eq!(input, bytes);
        assert!(until <= deadline());
        match action {
            "Invalid" if !skip_validation => return Err(crate::Error::new("invalid input")),
            "RevokedDuring" => {
                broker.revoke(token.bytes()).unwrap();
            }
            "ExpiredDuring" => broker.test_expire(token.bytes()),
            "Panic" => panic!("synthetic preparation failure"),
            _ => {}
        }
        Ok(Secret::new(input))
    };
    let run = |prepared: Secret, request: &crate::resources::BoundRequest, credential: &[u8]| {
        assert_eq!(prepared.expose(), bytes);
        assert_eq!(request.payload(), prepared.expose());
        assert_eq!(credential, b"synthetic-private-credential");
        entered.fetch_add(1, Ordering::SeqCst);
        None
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        broker.dispatch_prepared_until(
            if action == "WrongAdapter" {
                "other-v1"
            } else {
                "example-v1"
            },
            if action == "WrongToken" {
                &[0; 32]
            } else {
                token.bytes()
            },
            ([92; 16], bytes),
            deadline(),
            prepare,
            run,
        )
    }));
    if action == "Panic" {
        assert!(result.is_err());
    } else if matches!(action, "Valid" | "Retry") || (action == "Invalid" && skip_validation) {
        assert!(result.unwrap().unwrap().entered);
    } else {
        assert!(result.unwrap().is_err());
    }
    if action == "Retry" {
        let repeated = broker
            .dispatch_prepared_until(
                "example-v1",
                token.bytes(),
                ([92; 16], bytes),
                deadline(),
                prepare,
                |_: Secret, _, _| panic!("preparation retry entered provider"),
            )
            .unwrap();
        assert!(!repeated.entered);
    }
    let debit = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .resources()
        .snapshot()
        .unwrap()
        .used
        .values()
        .sum::<u64>();
    let scenario = [
        "Valid",
        "Invalid",
        "WrongToken",
        "WrongAdapter",
        "RevokedDuring",
        "ExpiredDuring",
        "Retry",
        "Panic",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    let mut next = before.clone();
    next.insert("scenario", scenario as i64);
    next.insert("parsed", parsed.load(Ordering::SeqCst) as i64);
    next.insert("entered", entered.load(Ordering::SeqCst) as i64);
    next.insert("debit", debit as i64);
    next
}

#[test]
fn broker_preparation_conforms_and_invalid_input_cannot_consume_capacity() {
    let model = aterm_models_db::broker_preparation_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    aterm_spec::fs_lane::each(&model.actions, |action| {
        let next = super::preparation_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    let unsafe_next = transition_with(&initial, "Invalid", true);
    assert!(!model.check_invariant("NoUnpreparedEffect", &unsafe_next));
}
