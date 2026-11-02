// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::tests::{account, causal_gap_fixture, retain_wire, retained_wire};
use super::*;
use aterm_spec::interp::State;

pub(crate) fn transition(state: &State, action: &str) -> State {
    assert_eq!(state["scenario"], 0);
    let (lab, parent, _) = causal_gap_fixture();
    let db = Proposals::at(lab.paths[0].clone());
    let value = account("conditional").value.unwrap();
    let blocked = matches!(action, "GapCreate" | "GapReplace");
    if action == "HealCreate" {
        let before = db.snapshot().unwrap();
        assert!(
            db.create_if_absent([74; 16], "accounts", "new", value.clone())
                .is_err()
        );
        let after = db.snapshot().unwrap();
        assert_eq!(after.retained, before.retained);
        assert_eq!(after.cells, before.cells);
    }
    if !blocked {
        retain_wire(&lab, 0, &parent);
    }
    let original = if action == "RetryGap" {
        let receipt = db
            .create_if_absent([74; 16], "accounts", "new", value.clone())
            .unwrap();
        let peer = Proposals::at(lab.paths[1].clone());
        let mut unrelated = account("another");
        unrelated.key = "unrelated".into();
        peer.write([75; 16], vec![unrelated.clone()]).unwrap();
        let child = peer.write([76; 16], vec![unrelated]).unwrap();
        retain_wire(&lab, 0, &retained_wire(&lab, 1, &child.id));
        Some(receipt)
    } else {
        None
    };
    let before = db.snapshot().unwrap();
    let incomplete = blocked || original.is_some();
    assert_eq!(before.withheld, usize::from(incomplete));
    let (scenario, result) = match action {
        "FreshCreate" => (1, db.create_if_absent([74; 16], "accounts", "new", value)),
        "FreshReplace" => (
            2,
            db.replace_if_current(
                [74; 16],
                "accounts",
                "one",
                before.cells[&("accounts".into(), "one".into())].candidates[0].operation,
                value,
            ),
        ),
        "GapCreate" => (3, db.create_if_absent([74; 16], "accounts", "new", value)),
        "GapReplace" => (
            4,
            db.replace_if_current(
                [74; 16],
                "accounts",
                "one",
                before.cells[&("accounts".into(), "one".into())].candidates[0].operation,
                value,
            ),
        ),
        "RetryGap" => (5, db.create_if_absent([74; 16], "accounts", "new", value)),
        "HealCreate" => (6, db.create_if_absent([74; 16], "accounts", "new", value)),
        _ => panic!("unknown conditional write action"),
    };
    let after = db.snapshot().unwrap();
    if result.is_err() || original.is_some() {
        assert_eq!(after.cells, before.cells);
    }
    let same = original
        .as_ref()
        .is_none_or(|receipt| result.as_ref().ok() == Some(receipt));
    State::from([
        ("scenario", scenario),
        ("incomplete", i64::from(incomplete)),
        ("accepted", i64::from(result.is_ok())),
        ("effects", (after.retained - before.retained) as i64),
        ("same", i64::from(same)),
    ])
}

#[test]
fn conditional_writes_require_complete_history_but_exact_retries_remain_available() {
    let model = aterm_models_db::proposal_conditional_model();
    aterm_spec::interp::prove_and_catch(&model);
    let initial = model.init_state();
    for action in &model.actions {
        let next = transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}",
            action.name
        );
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &next),
                "{}",
                invariant.name
            );
        }
        let mut wrong = next.clone();
        if next["incomplete"] == 1 {
            wrong.insert("effects", 1);
            assert!(!model.check_invariant("NoGapWrites", &wrong));
        } else {
            wrong.insert("accepted", 0);
            assert!(!model.check_invariant("EligibleProgress", &wrong));
        }
    }
}
