// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::{accounts, accounts_sync};
use aterm_spec::interp::State;

fn row(value: i64, variation: usize) -> accounts::Row {
    let mut row = accounts::Row {
        label: "current".into(),
        email: Some("old@example.test".into()),
        identity_email: Some("old-provider@example.test".into()),
        browser: Some("Profile 1".into()),
        dir: Some("/old/login".into()),
        ..accounts::Row::default()
    };
    if value == 1 {
        match variation {
            0 => row.email = Some("new@example.test".into()),
            1 => row.identity_email = Some("new-provider@example.test".into()),
            2 => row.browser = Some("Profile 2".into()),
            3 => row.dir = Some("/new/login".into()),
            _ => unreachable!(),
        }
    }
    row
}

fn inputs(value: i64, variation: usize) -> String {
    accounts_sync::row_retry_inputs(&row(value, variation), "same route and files")
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_for(state, action, 0)
}

fn transition_for(state: &State, action: &str, variation: usize) -> State {
    let mut next = state.clone();
    match action {
        "RowA" | "RowB" => {
            next.insert("row", i64::from(action == "RowB"));
            next.insert("result", 0);
            next.insert("isolated", 0);
            return next;
        }
        "Expire" => {
            next.insert("fresh", 0);
            next.insert("result", 0);
            next.insert("isolated", 0);
            return next;
        }
        _ => {}
    }
    let now = Instant::now();
    let mut ladder = Ladder::default();
    if state["cached"] == 1 {
        ladder.backoff.insert(
            "current".into(),
            Backoff {
                next: if state["fresh"] == 1 {
                    now + BACKOFF_FIRST
                } else {
                    now
                },
                step: 1,
                inputs: inputs(state["origin"], variation),
            },
        );
    }
    match action {
        "Probe" => {
            let held = ladder.held("current", &inputs(state["row"], variation), now);
            next.insert("result", if held { 1 } else { 2 });
            if state["cached"] == 1 && state["origin"] != state["row"] {
                next.insert("obsolete", 1);
            }
            next.insert("isolated", 0);
        }
        "Fail" => {
            ladder.failed("current", &inputs(state["row"], variation), now);
            next.insert("origin", state["row"]);
            next.insert("obsolete", 0);
            next.insert("result", 0);
            next.insert("isolated", 0);
        }
        "OtherFailure" => {
            ladder.failed("other", "another account", now);
            next.insert(
                "isolated",
                i64::from(
                    state["cached"] == 1 && state["fresh"] == 1 && state["origin"] == state["row"],
                ),
            );
            next.insert("result", 0);
        }
        _ => panic!("unexpected retry action {action}"),
    }
    let cached = ladder.backoff.get("current");
    next.insert("cached", i64::from(cached.is_some()));
    next.insert("fresh", i64::from(cached.is_some_and(|b| b.next > now)));
    next
}

#[test]
fn row_retry_inputs_conform_for_changed_identity_and_route() {
    let model = aterm_models_harness::account_retry_inputs_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(model.mutant_dials(), (1..=5).collect::<Vec<_>>());
    let states = aterm_spec::interp::reachable_states(&model);
    for variation in 0..4 {
        for state in &states {
            for action in &model.actions {
                if model.action_enabled(action.name, state) {
                    let next = transition_for(state, action.name, variation);
                    assert!(
                        model.successors(action.name, state).contains(&next),
                        "variation {variation}, {}: {state:?} -> {next:?}",
                        action.name,
                    );
                }
            }
        }
    }

    // Negative control: the real retry engine is given only the unchanged coarse
    // observation, dropping the row identity. Its stale hold must fail conformance.
    let state = states
        .iter()
        .find(|s| s["cached"] == 1 && s["fresh"] == 1 && s["origin"] == 0 && s["row"] == 1)
        .unwrap();
    let now = Instant::now();
    let mut ladder = Ladder::default();
    ladder.failed("current", "same route and files", now);
    assert!(ladder.held("current", "same route and files", now));
    let mut bad = transition(state, "Probe");
    bad.insert("cached", 1);
    bad.insert("fresh", 1);
    bad.insert("result", 1);
    assert!(!model.successors("Probe", state).contains(&bad));
    assert!(!model.check_invariant("HeldUsesCurrentRow", &bad));
}
