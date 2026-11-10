// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::tests::{account, raw, retain_wire, retained_wire};
use super::*;
use crate::accounts::publication::Draft;
use crate::table::tests::Lab;
use aterm_secrets::commons::roster::Role;
use aterm_spec::interp::State;

fn draft(db: &Proposals, plan: &str) -> Draft {
    Draft::new(
        db.publication_destination().unwrap(),
        "accounts".into(),
        "chain".into(),
        None,
        account(plan).value.unwrap(),
    )
    .unwrap()
}

fn change(plan: &str) -> Change {
    let mut value = account(plan);
    value.key = "chain".into();
    value
}

fn gap(lab: &Lab) {
    let peer = Proposals::at(lab.paths[1].clone());
    peer.initialize().unwrap();
    let mut value = account("unrelated");
    value.key = "unrelated".into();
    peer.write([80; 16], vec![value.clone()]).unwrap();
    let child = peer.write([81; 16], vec![value]).unwrap();
    retain_wire(lab, 0, &retained_wire(lab, 1, &child.id));
}

fn check(scenario: i64, mutant: u8) -> State {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let db = Proposals::at(lab.paths[0].clone());
    db.initialize().unwrap();
    let first = draft(&db, "first").allocate().unwrap();
    let mut next = Draft::after(&first, account("second").value.unwrap())
        .unwrap()
        .allocate()
        .unwrap();
    if scenario != 1 {
        db.deliver_publication(&first).unwrap();
    }
    let mut original = None;
    match scenario {
        2 => {
            db.write([82; 16], vec![change("intervening")]).unwrap();
        }
        3 => gap(&lab),
        4 => {
            original = Some(db.deliver_publication(&next).unwrap());
            db.write([82; 16], vec![change("intervening")]).unwrap();
            gap(&lab);
        }
        5 => {
            raw(
                &lab,
                0,
                &Command::Write {
                    request: first.request().unwrap(),
                    changes: vec![change("conflicting")],
                }
                .encode()
                .unwrap(),
            );
        }
        6 => {
            raw(&lab, 0, &first.validate().unwrap().encode().unwrap());
        }
        7 => {
            let command = Command::Write {
                request: [83; 16],
                changes: vec![change("one"), account("another-key")],
            };
            raw(&lab, 0, &command.encode().unwrap());
            next = draft(&db, "second")
                .with_reference_for_test(
                    command.request(),
                    aterm_digest::Sha256::digest(command.encode().unwrap()),
                )
                .allocate()
                .unwrap();
        }
        8 => {
            next = draft(&db, "second")
                .with_reference_for_test(first.request().unwrap(), [9; 32])
                .allocate()
                .unwrap();
        }
        _ => {}
    }
    let before = db.snapshot().unwrap();
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CHAIN_MUTANT.set(0);
        }
    }
    let _reset = Reset;
    CHAIN_MUTANT.set(mutant);
    let result = db.deliver_publication(&next);
    let after = db.snapshot().unwrap();
    if result.is_err() {
        assert_eq!(after.cells, before.cells);
    }
    let same = original.as_ref().is_none_or(|old| {
        result
            .as_ref()
            .is_ok_and(|new| new.operation() == old.operation())
    });
    State::from([
        ("scenario", scenario),
        ("done", 1),
        ("accepted", i64::from(result.is_ok())),
        ("effects", (after.retained - before.retained) as i64),
        ("same", i64::from(same)),
    ])
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    if action == "Scenario" {
        let mut next = state.clone();
        next.insert("scenario", state["scenario"] + 1);
        next
    } else {
        assert_eq!(action, "Check");
        check(state["scenario"], 0)
    }
}

#[test]
fn publication_chain_refines_current_predecessor_and_retry_laws() {
    let model = aterm_models_db::publication_chain_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                checked += usize::from(action.name == "Check");
            }
        }
    }
    assert_eq!(checked, 9);
    for (mutant, scenario, law) in [
        (1, 2, "NoUnadmittedWrite"),
        (2, 8, "RefuseUnadmitted"),
        (3, 4, "AvailableProgress"),
    ] {
        let wrong = check(scenario, mutant);
        assert!(!model.check_invariant(law, &wrong), "mutant{mutant}: {law}");
    }
}

#[test]
fn three_offline_revisions_apply_once_after_predecessor_entries_are_gone() {
    let lab = Lab::new(&[Role::Writer]);
    let db = Proposals::at(lab.paths[0].clone());
    db.initialize().unwrap();
    let first = draft(&db, "A").allocate().unwrap();
    let second = Draft::after(&first, account("B").value.unwrap())
        .unwrap()
        .allocate()
        .unwrap();
    let third = Draft::after(&second, account("A").value.unwrap())
        .unwrap()
        .allocate()
        .unwrap();
    assert_ne!(first.request().unwrap(), third.request().unwrap());
    let a = db.deliver_publication(&first).unwrap();
    // Delivery needs no local predecessor entry: signed table history is enough.
    drop(first);
    let b = db.deliver_publication(&second).unwrap();
    drop(second);
    let c = db.deliver_publication(&third).unwrap();
    let before = db.snapshot().unwrap();
    assert_eq!(
        db.deliver_publication(&third).unwrap().operation(),
        c.operation()
    );
    assert_eq!(db.snapshot().unwrap().retained, before.retained);
    let cell = &before.cells[&("accounts".into(), "chain".into())];
    assert_eq!(cell.candidates.len(), 1);
    assert_eq!(cell.candidates[0].operation, *c.operation());
    assert_ne!(a.operation(), b.operation());
    assert_ne!(a.operation(), c.operation());
}
