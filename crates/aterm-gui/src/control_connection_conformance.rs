// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for bounded control-socket connection admission.
//!
//! The trace drives the genuine shipping `BoundedDispatch` with real `CtlStream`
//! pairs, projects queued + running admissions plus observed outcomes into the
//! derived model, and checks every transition. Completion runs through
//! `BoundedDispatch::serve_next`, the one body every control and subscription
//! worker loops on, under a panicking handler; a `serve_next` that lost its
//! completion guard would fail the `Complete` step. A historical over-admission
//! projection and a completion that keeps its lane are independently rejected so
//! the test cannot pass vacuously.

#![cfg(test)]

use aterm_spec::derive::{Model, control_connection_admission_model};
use aterm_spec::interp::{State, admits};
use aterm_uds::CtlStream;

use crate::control::BoundedDispatch;

#[derive(Clone, Copy, Default)]
struct Facts {
    arrivals: i64,
    accepted: i64,
    rejected: i64,
    completed: i64,
}

fn project(model: &Model, dispatch: &BoundedDispatch<CtlStream>, facts: Facts) -> State {
    let mut state = model.init_state();
    state.insert(
        "outstanding",
        i64::try_from(dispatch.outstanding()).expect("bounded admission count fits i64"),
    );
    state.insert("arrivals", facts.arrivals);
    state.insert("accepted", facts.accepted);
    state.insert("rejected", facts.rejected);
    state.insert("completed", facts.completed);
    state
}

fn assert_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(
        admits(model, before, after),
        Some(action),
        "shipping transition must be admitted specifically as {action}"
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

fn admit(
    model: &Model,
    dispatch: &BoundedDispatch<CtlStream>,
    facts: &mut Facts,
    expect_accept: bool,
) -> Option<CtlStream> {
    let before = project(model, dispatch, *facts);
    let (_client, server) = CtlStream::pair().expect("real local control stream pair");
    facts.arrivals += 1;
    let rejected = match dispatch.try_submit(server) {
        Ok(()) => {
            assert!(
                expect_accept,
                "shipping dispatch unexpectedly admitted at capacity"
            );
            facts.accepted += 1;
            None
        }
        Err(stream) => {
            assert!(
                !expect_accept,
                "shipping dispatch unexpectedly rejected below capacity"
            );
            facts.rejected += 1;
            Some(stream)
        }
    };
    let after = project(model, dispatch, *facts);
    assert_transition(
        model,
        &before,
        &after,
        if expect_accept { "Admit" } else { "Reject" },
    );
    rejected
}

#[test]
fn shipping_connection_dispatch_conforms_and_rejects_overflow() {
    let model = control_connection_admission_model();
    let dispatch = BoundedDispatch::new(2);
    dispatch.set_capacity(2);
    let mut facts = Facts::default();
    assert_eq!(project(&model, &dispatch, facts), model.init_state());

    assert!(admit(&model, &dispatch, &mut facts, true).is_none());
    assert!(admit(&model, &dispatch, &mut facts, true).is_none());
    let full = project(&model, &dispatch, facts);

    // Negative control: the old unbounded admission would have retained a third
    // stream. The healthy model admits no such transition and QueueBounded fails.
    let mut overflow = full.clone();
    overflow.insert("outstanding", 3);
    overflow.insert("arrivals", 3);
    overflow.insert("accepted", 3);
    assert_eq!(admits(&model, &full, &overflow), None);
    assert!(!model.check_invariant("LaneBounded", &overflow));

    assert!(
        admit(&model, &dispatch, &mut facts, false).is_some(),
        "saturation returns ownership for the listener's bounded busy reply"
    );

    let before_complete = project(&model, &dispatch, facts);
    // The shipping worker body. The handler records what it sees while it runs,
    // then panics, so only the completion guard's `Drop` can release the lane.
    let mut while_serving = None;
    let returned = dispatch.serve_next(|stream| {
        while_serving = Some(project(&model, &dispatch, facts));
        drop(stream);
        panic!("synthetic control handler panic");
    });
    assert!(!returned, "the handler really panicked");
    assert_eq!(
        while_serving.as_ref(),
        Some(&before_complete),
        "popping starts work but must retain its admission lane"
    );
    facts.completed += 1;
    let after_complete = project(&model, &dispatch, facts);
    assert_transition(&model, &before_complete, &after_complete, "Complete");

    // Negative control: a worker that returns without releasing its lane (the
    // same panic with no guard) leaves the pool one lane short for good.
    let mut leaked = after_complete.clone();
    leaked.insert("outstanding", before_complete["outstanding"]);
    assert_eq!(admits(&model, &before_complete, &leaked), None);
    assert!(!model.check_invariant("AcceptedWorkAccounted", &leaked));

    // Capacity released by genuine worker completion is immediately reusable.
    assert!(admit(&model, &dispatch, &mut facts, true).is_none());
}
