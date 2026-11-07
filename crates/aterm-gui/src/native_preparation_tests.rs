// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

pub(super) fn transition(state: &State, action: &str) -> State {
    let mut preparation = Preparation {
        predecessor: None,
        recorded: state["recorded"] == 1,
        account_state: None,
        serial: state["serial"] as u64,
        active: (state["active"] == 1).then_some(state["serial"] as u64),
    };
    let mut next = state.clone();
    match action {
        "Begin" => {
            preparation.begin().expect("available writer");
        }
        "Collide" => {
            next.insert("collision", i64::from(preparation.begin().is_some()));
        }
        "Ack" => {
            next.insert("durable", 1);
            assert!(preparation.acknowledge(preparation.serial));
            next.insert("seen_ack", 1);
        }
        "LostAck" => {
            // The real journal committed, but the caller has no acknowledgement.
            next.insert("durable", 1);
        }
        "StaleAck" => assert!(!preparation.acknowledge(0)),
        "Release" => preparation.release(preparation.serial),
        "Invalidate" => {
            next.insert("valid", 0);
        }
        "Admit" => {
            let admitted = preparation.can_admit(state["valid"] == 1);
            next.insert("admitted", i64::from(admitted));
            next.insert("stale", i64::from(admitted && state["valid"] == 0));
        }
        "Publish" => {
            if preparation.can_publish() {
                next.insert("published", 1);
            }
        }
        _ => panic!("unknown preparation action {action}"),
    }
    next.insert("serial", preparation.serial as i64);
    next.insert("active", i64::from(preparation.active.is_some()));
    next.insert("recorded", i64::from(preparation.recorded));
    next
}

#[test]
fn preparation_publication_refines_each_reachable_model_transition() {
    let model = aterm_models_harness::preparation_publication_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn pretending_a_failed_append_was_acknowledged_breaks_the_model() {
    let model = aterm_models_harness::preparation_publication_model();
    let mut state = model.init_state();
    let mut preparation = Preparation::new(true, None);
    // Negative control: the old failure path treated an uncertain write as a
    // settled operation. No durable record exists in this modeled execution.
    assert!(preparation.acknowledge(1));
    assert!(preparation.can_admit(true));
    state.insert("recorded", 1);
    state.insert("admitted", 1);
    assert!(!model.check_invariant("AcknowledgementNeedsDurability", &state));
}

#[test]
fn stale_completions_and_serial_exhaustion_cannot_steal_a_writer() {
    let mut preparation = Preparation::new(true, None);
    assert_eq!(preparation.begin(), None);
    preparation.release(0);
    assert_eq!(preparation.begin(), None);
    preparation.release(1);
    assert_eq!(preparation.begin(), Some(2));
    assert!(!preparation.acknowledge(1));
    preparation.release(1);
    assert_eq!(preparation.begin(), None);
    assert!(preparation.acknowledge(2));
    preparation.release(2);
    preparation.serial = u64::MAX;
    assert_eq!(preparation.begin(), None);
    assert!(preparation.recorded);
    assert!(!preparation.can_admit(true));
}
