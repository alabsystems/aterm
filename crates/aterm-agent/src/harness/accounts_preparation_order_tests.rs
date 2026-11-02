// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn number(kind: PreparationKind) -> i64 {
    match kind {
        PreparationKind::Native => 0,
        PreparationKind::Shared => 1,
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut order = PreparationOrder {
        active: state["active"] == 1,
        next: match state["next"] {
            0 => PreparationKind::Native,
            1 => PreparationKind::Shared,
            _ => panic!("bad class"),
        },
    };
    let mut observed = state.clone();
    let op = match action {
        "Begin" | "Busy" => {
            observed.insert("answer", order.begin().map_or(2, number));
            if action == "Begin" { 1 } else { 2 }
        }
        "Native" => {
            order.admitted(PreparationKind::Native);
            observed.insert("last", 0);
            3
        }
        "Shared" => {
            order.admitted(PreparationKind::Shared);
            observed.insert("last", 1);
            4
        }
        "End" => {
            order.end();
            observed.insert("answer", 2);
            5
        }
        _ => panic!("unknown preparation order action {action}"),
    };
    observed.insert("op", op);
    observed.insert("active", i64::from(order.active));
    observed.insert("next", number(order.next));
    observed
}

#[test]
fn preparation_order_conforms_and_rejects_wrong_priority() {
    let model = aterm_models_harness::preparation_order_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut wrong = observed.clone();
                wrong.insert("next", 1 - observed["next"]);
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
}

#[test]
fn discovery_pass_is_exclusive_and_refusal_keeps_its_priority() {
    let guard = Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let first = PreparationPass::claim(&guard).unwrap();
    assert_eq!(first.first, PreparationKind::Native);
    assert!(PreparationPass::claim(&guard).is_none());
    first.admitted(PreparationKind::Native);
    assert!(PreparationPass::claim(&guard).is_none());
    drop(first);
    let second = PreparationPass::claim(&guard).unwrap();
    assert_eq!(second.first, PreparationKind::Shared);
    drop(second);
    let retry = PreparationPass::claim(&guard).unwrap();
    assert_eq!(retry.first, PreparationKind::Shared);
    retry.admitted(PreparationKind::Native);
    drop(retry);
    let fallback = PreparationPass::claim(&guard).unwrap();
    assert_eq!(fallback.first, PreparationKind::Shared);
    fallback.admitted(PreparationKind::Shared);
    drop(fallback);
    let permit = AutoRenewPermit::claim(&guard, "other", Instant::now(), false).unwrap();
    assert!(PreparationPass::claim(&guard).is_none());
    drop(permit);
    assert_eq!(
        PreparationPass::claim(&guard).unwrap().first,
        PreparationKind::Native
    );
}
