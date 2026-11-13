// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut result = state.clone();
    let mut ledger = AccountLineages::default();
    if state["root"] == 1 {
        ledger
            .entries
            .insert("root".into(), (state["child"] == 1).then(|| "child".into()));
    }
    let occupied = (state["root"] + state["child"] + state["others"]) as usize + ledger.spares();
    match action {
        "Root" => {
            let _ = ledger.reserve(&AccountSlot::Root, "root", occupied);
        }
        "Other" => {
            if ledger
                .reserve(&AccountSlot::Plain, "other", occupied)
                .is_ok()
            {
                result.insert("others", state["others"] + 1);
            }
        }
        "Child" => {
            let accepted = ledger
                .reserve(&AccountSlot::Successor("root".into()), "child", occupied)
                .is_ok();
            result.insert("attempted", 1);
            result.insert("accepted", i64::from(accepted));
        }
        "Cancel" => {
            ledger.cancel(&AccountSlot::Successor("root".into()), "child");
            result.insert("attempted", 0);
            result.insert("accepted", 0);
        }
        "Transfer" => {
            ledger.advance("root", "child".into());
            result.insert("attempted", 0);
            result.insert("accepted", 0);
            result.insert("hops", state["hops"] + 1);
        }
        _ => unreachable!(),
    }
    result.insert("root", i64::from(!ledger.entries.is_empty()));
    result.insert(
        "child",
        i64::from(ledger.entries.values().any(Option::is_some)),
    );
    result.insert("spare", ledger.spares() as i64);
    result
}

#[test]
fn bounded_ledger_refines_production_and_catches_lost_spare() {
    let model = aterm_models_harness::account_lineage_budget_model();
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    assert!(model.fire("Root", &mut state));
    assert!(model.fire("Child", &mut state));
    let mut lost_spare = transition(&state, "Transfer");
    lost_spare.insert("spare", 0);
    assert!(!model.check_invariant("Reserved", &lost_spare));
}
