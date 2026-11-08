// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn transaction(case: i64) -> Transaction {
    let mut tx = Transaction::new(10);
    assert!(!tx.execute(11));
    assert!(!tx.execution_granted);
    if case > 0 {
        assert!(tx.reserve(10));
        assert!(!tx.execute(11));
        assert!(!tx.execution_granted);
    }
    if (3..=5).contains(&case) || case == 7 {
        assert!(tx.execute(10));
        assert!(!tx.execute(11));
        assert!(tx.execution_granted);
    }
    if case == 4 || case == 5 {
        assert!(tx.load(11));
    }
    if case == 5 {
        assert!(tx.consume(11));
    }
    if case >= 6 {
        assert!(tx.cancel());
    }
    tx
}

fn revoke_case(state: &aterm_spec::interp::State, buggy: u8) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let case = state["case"];
    let mut tx = transaction(case);
    // The old monitor treated a write-ahead receipt marker as an issued Go.
    let prospective_marker = (2..=5).contains(&case) || case == 7;
    let terminate = match buggy {
        1 => {
            tx.cancel();
            prospective_marker && case != 5
        }
        2 => {
            if case == 5 {
                tx.revoke()
            } else {
                tx.cancel();
                tx.execution_granted = false;
                false
            }
        }
        3 if case == 2 => false,
        4 => {
            tx.stage = Stage::Cancelled;
            tx.revoke()
        }
        _ => tx.revoke(),
    };
    if buggy == 0 && case == 5 {
        assert_eq!(tx.stage(), Stage::Consumed(11));
        assert!(tx.consume(11));
    }
    next.insert("done", 1);
    next.insert("history", i64::from(tx.execution_granted));
    next.insert("cancelled", i64::from(tx.stage() == Stage::Cancelled));
    next.insert("terminate", i64::from(terminate));
    let late = case != 5 && (tx.reserve(10) || tx.execute(10) || tx.load(11) || tx.consume(11));
    next.insert("late", i64::from(late));
    next
}

pub(crate) fn revocation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    match action {
        "NextCase" => {
            let mut next = state.clone();
            next.insert("case", state["case"] + 1);
            next
        }
        "Revoke" => revoke_case(state, 0),
        _ => unreachable!(),
    }
}

#[test]
fn exec_transaction_revocation_refines_live_grant_history() {
    let model = aterm_models_harness::exec_revocation_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = revocation_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
            }
        }
    }
    for (buggy, case, law) in [
        (1, 2, "NoPrematureTermination"),
        (2, 3, "GrantHistory"),
        (2, 7, "GrantedRevocation"),
        (3, 2, "AdmissionClosed"),
        (4, 5, "ConsumedUnchanged"),
    ] {
        let mut state = model.init_state();
        state.insert("case", case);
        let next = revoke_case(&state, buggy);
        assert!(!model.check_invariant(law, &next), "{law}");
        assert!(!model.successors("Revoke", &state).contains(&next));
    }
}
