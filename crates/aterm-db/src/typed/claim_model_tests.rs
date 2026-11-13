// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every bounded dependency/digest/mode case is evaluated by the actual claim projection.
//! The negative control removes real carried provenance before reading the new generation.

use super::{
    Provenance, cell, project, snapshot,
    tests::{cells, operation},
};
use crate::{alab::registry::MACHINE, database::Change, typed::View};
use astream_wire::sync::Index;
use aterm_json::json;
use aterm_spec::interp::State;
use std::collections::BTreeMap;

const ROWS: [&str; 3] = [
    "mc-0000000000000001",
    "mc-0000000000000002",
    "mc-0000000000000003",
];

fn revival_evaluate(
    model: &aterm_spec::derive::Model,
    lose_retirement: bool,
    lose_context: bool,
) -> State {
    use super::{interpreted_cells, tests::carried_operation};
    let mode = model
        .consts
        .iter()
        .find(|(name, _)| *name == "Mode")
        .unwrap()
        .1;
    let mut index = Index::new(8);
    let a = operation(10, ROWS[0], "label", "m7", &[], &mut index);
    let retired = operation(50, ROWS[0], "row", "retired", &[10], &mut index);
    let b = operation(20, ROWS[1], "label", "m7", &[50], &mut index);
    let revived = operation(5, ROWS[0], "row", "live", &[20], &mut index);
    let mut raw = cells(&[&a, &b, &revived]);
    let mut operations = vec![a, b, revived];
    if !lose_retirement {
        operations.push(retired);
    }
    raw = interpreted_cells(&MACHINE, &raw, operations.iter(), &index).into_owned();
    for id in [100, 200].into_iter().take(mode as usize) {
        let evidence = snapshot(&MACHINE, &raw, operations.iter(), &index).unwrap();
        let mut next = Index::new(8);
        let copied = carried_operation(id, &raw, (!lose_context).then_some(&evidence), &mut next);
        raw = cells(&[&copied]);
        operations = vec![copied];
        index = next;
    }
    let shown = election(&project(&MACHINE, &raw, operations.iter(), &index)).0;
    let evidence = snapshot(&MACHINE, &raw, operations.iter(), &index).unwrap();
    let coordinate = (MACHINE.name.into(), format!("{}.label", ROWS[0]));
    let origin = if lose_retirement { [10; 32] } else { [5; 32] };
    let tenure = match Provenance::decode(&evidence[&coordinate][&origin]).unwrap() {
        Provenance::Known(starts) => i64::from(starts.keys().next().unwrap()[0]),
        Provenance::Unknown | Provenance::Invalid => 0,
    };
    let mut next = model.init_state();
    next.insert("mode", mode);
    next.insert("read", 1);
    next.insert("shown", shown);
    next.insert("tenure", tenure);
    next
}

pub(crate) fn revival_transition(
    model: &aterm_spec::derive::Model,
    path: &[&'static str],
    action: &str,
) -> State {
    assert!(path.is_empty());
    assert_eq!(action, "Judge");
    revival_evaluate(model, false, false)
}

#[test]
fn claim_revival_conforms_for_native_carried_and_recarried_rows() {
    let configurations = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|models| models[0].name == "AlabClaimRevival")
        .unwrap();
    assert_eq!(configurations.len(), 3);
    for (mode, model) in configurations.iter().enumerate() {
        let successors = model.successors("Judge", &model.init_state());
        assert!(successors.contains(&revival_transition(model, &[], "Judge")));
        assert!(!successors.contains(&revival_evaluate(model, true, false)));
        if mode > 0 {
            assert!(!successors.contains(&revival_evaluate(model, false, true)));
        }
    }
}

fn undo_evaluate(model: &aterm_spec::derive::Model, lose_undo: bool, lose_context: bool) -> State {
    use super::{restored_cells, tests::carried_operation};
    let mode = model
        .consts
        .iter()
        .find(|(name, _)| *name == "Mode")
        .unwrap()
        .1;
    let mut index = Index::new(8);
    let a = operation(10, ROWS[0], "label", "m7", &[], &mut index);
    let b = operation(20, ROWS[1], "label", "m7", &[], &mut index);
    let mut raw = cells(&[&a, &b]);
    let mut operations = vec![a, b];
    if mode > 0 {
        let evidence = snapshot(&MACHINE, &raw, operations.iter(), &index).unwrap();
        let mut next = Index::new(8);
        let carried = carried_operation(100, &raw, Some(&evidence), &mut next);
        raw = cells(&[&carried]);
        operations = vec![carried];
        index = next;
    }
    let coordinate = (MACHINE.name.into(), format!("{}.label", ROWS[0]));
    let beforeimage = raw[&coordinate].candidates[0].value.clone();
    let mut parents: Vec<_> = operations.iter().map(|operation| operation.id[0]).collect();
    parents.sort_unstable();
    let withdrawn = operation(50, ROWS[0], "label", "m8", &parents, &mut index);
    let mut restored = operation(5, ROWS[0], "label", "m7", &[50], &mut index);
    restored.revert = !lose_undo;
    restored.changes[0].value = Some(beforeimage);
    raw.extend(cells(&[&restored]));
    operations.extend([withdrawn, restored]);
    raw = restored_cells(&MACHINE, &raw, operations.iter()).into_owned();
    if mode == 2 {
        let evidence = snapshot(&MACHINE, &raw, operations.iter(), &index).unwrap();
        let mut next = Index::new(8);
        let carried = carried_operation(200, &raw, (!lose_context).then_some(&evidence), &mut next);
        raw = cells(&[&carried]);
        operations = vec![carried];
        index = next;
    }
    let shown = election(&project(&MACHINE, &raw, operations.iter(), &index)).0;
    let evidence = snapshot(&MACHINE, &raw, operations.iter(), &index).unwrap();
    let origin = if mode > 0 && lose_undo {
        [10; 32]
    } else {
        [5; 32]
    };
    let tenure = match Provenance::decode(&evidence[&coordinate][&origin]).unwrap() {
        Provenance::Known(starts) => i64::from(starts.keys().next().unwrap()[0]),
        Provenance::Unknown | Provenance::Invalid => 0,
    };
    let mut next = model.init_state();
    next.insert("mode", mode);
    next.insert("read", 1);
    next.insert("shown", shown);
    next.insert("tenure", tenure);
    next
}

pub(crate) fn undo_transition(
    model: &aterm_spec::derive::Model,
    path: &[&'static str],
    action: &str,
) -> State {
    assert!(path.is_empty());
    assert_eq!(action, "Judge");
    undo_evaluate(model, false, false)
}

#[test]
fn claim_undo_conforms_for_native_carried_and_recarried_beforeimages() {
    let configurations = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|models| models[0].name == "AlabClaimUndo")
        .unwrap();
    assert_eq!(configurations.len(), 3);
    for (mode, model) in configurations.iter().enumerate() {
        let successors = model.successors("Judge", &model.init_state());
        assert!(successors.contains(&undo_transition(model, &[], "Judge")));
        if mode > 0 {
            assert!(!successors.contains(&undo_evaluate(model, true, false)));
        }
        if mode == 2 {
            assert!(!successors.contains(&undo_evaluate(model, false, true)));
        }
    }
}

fn election(projection: &cell::Projection) -> (i64, i64) {
    let elected: Vec<_> = ROWS
        .iter()
        .enumerate()
        .filter(|(_, pk)| {
            projection.rows.get(**pk).and_then(|row| row.get("label")) == Some(&json!("m7"))
        })
        .map(|(index, _)| index as i64 + 1)
        .collect();
    (elected.first().copied().unwrap_or(0), elected.len() as i64)
}

fn evaluate(input: &State, drop_provenance: bool) -> State {
    let ranks = [
        [10, 20, 30],
        [10, 30, 20],
        [20, 10, 30],
        [30, 10, 20],
        [20, 30, 10],
        [30, 20, 10],
    ][input["order"] as usize];
    let mode = input["mode"];
    let mut index = Index::new(16);
    let a = operation(ranks[0], ROWS[0], "label", "m7", &[], &mut index);
    let parents_b = if input["ba"] == 1 {
        vec![ranks[0]]
    } else {
        Vec::new()
    };
    let b = operation(ranks[1], ROWS[1], "label", "m7", &parents_b, &mut index);
    let mut parents_c = Vec::new();
    if input["ca"] == 1 {
        parents_c.push(ranks[0]);
    }
    if input["cb"] == 1 {
        parents_c.push(ranks[1]);
    }
    parents_c.sort_unstable();
    let c = operation(ranks[2], ROWS[2], "label", "m7", &parents_c, &mut index);
    let mut heads = [a.clone(), b.clone(), c.clone()];
    let mut operations = vec![a, b, c];
    let original = project(
        &MACHINE,
        &cells(&heads.iter().collect::<Vec<_>>()),
        operations.iter(),
        &index,
    );
    let mut expected = election(&original).0;
    if mode == 2 || mode == 3 {
        let mut parents = ranks.to_vec();
        parents.sort_unstable();
        let id = if mode == 2 { 40 } else { 50 };
        let value = if mode == 2 { "m7" } else { "m8" };
        let changed = operation(id, ROWS[0], "label", value, &parents, &mut index);
        heads[0] = changed.clone();
        operations.push(changed);
        if mode == 3 {
            let returned = operation(5, ROWS[0], "label", "m7", &[50], &mut index);
            heads[0] = returned.clone();
            operations.push(returned);
            expected = election(&project(
                &MACHINE,
                &cells(&heads.iter().collect::<Vec<_>>()),
                operations.iter(),
                &index,
            ))
            .0;
        }
    }
    let mut raw = cells(&heads.iter().collect::<Vec<_>>());
    let mut origin_a = heads[0].id;
    if mode == 1 || mode == 4 {
        let evidence = snapshot(&MACHINE, &raw, operations.iter(), &index).unwrap();
        let mut target = Index::new(4);
        let mut carried = operation(100, ROWS[0], "label", "m7", &[], &mut target);
        carried.changes = heads
            .iter()
            .map(|original| {
                let change = &original.changes[0];
                let mut encoded = cell::carry(&BTreeMap::from([(
                    original.id,
                    change.value.clone().unwrap(),
                )]))
                .unwrap();
                if mode == 1 && !drop_provenance {
                    encoded
                        .as_object_mut()
                        .unwrap()
                        .get_mut("k")
                        .unwrap()
                        .as_array_mut()
                        .unwrap()[0]
                        .as_object_mut()
                        .unwrap()
                        .insert(
                            "c".into(),
                            evidence[&(change.collection.clone(), change.key.clone())]
                                [&original.id]
                                .clone(),
                        );
                }
                Change {
                    collection: change.collection.clone(),
                    key: change.key.clone(),
                    value: Some(encoded),
                }
            })
            .collect();
        raw = cells(&[&carried]);
        origin_a = heads[0].id;
        operations = vec![carried];
        index = target;
    }
    if mode == 6 {
        let mut parents: Vec<_> = operations.iter().map(|operation| operation.id[0]).collect();
        parents.sort_unstable();
        let mut unreadable = operation(77, ROWS[0], "future_identity", "m7", &parents, &mut index);
        unreadable.changes[0].value =
            Some(cell::CellValue::owner(MACHINE.version + 1, json!("m7")).encode());
        raw.extend(cells(&[&unreadable]));
        operations.push(unreadable);
    }
    let view = View {
        causal: index.clone(),
        ops: operations.clone(),
        quota: Default::default(),
        cells: raw.clone(),
        owned: Default::default(),
        complete: mode != 5,
        refused: Vec::new(),
    };
    let accepted = view
        .require_roll_source(&crate::alab::registry::REGISTRY)
        .is_ok();
    let (shown, holders, tenure) = if accepted {
        let read = project(&MACHINE, &raw, operations.iter(), &index);
        let (shown, holders) = election(&read);
        let evidence = snapshot(&MACHINE, &raw, operations.iter(), &index).unwrap();
        let encoded = &evidence[&(MACHINE.name.into(), format!("{}.label", ROWS[0]))][&origin_a];
        let tenure = match Provenance::decode(encoded).unwrap() {
            Provenance::Known(starts) => i64::from(starts.keys().next().unwrap()[0]),
            Provenance::Unknown | Provenance::Invalid => 0,
        };
        (shown, holders, tenure)
    } else {
        (0, 0, 0)
    };
    let mut next = input.clone();
    next.insert("read", 1);
    next.insert("expected", expected);
    next.insert("shown", shown);
    next.insert("holders", holders);
    next.insert("tenure", tenure);
    next.insert("accepted", i64::from(accepted));
    next
}

fn configured(model: &aterm_spec::derive::Model, state: &State) -> State {
    let mut input = state.clone();
    for (variable, constant) in [
        ("ba", "BA"),
        ("ca", "CA"),
        ("cb", "CB"),
        ("order", "Order"),
        ("mode", "Mode"),
    ] {
        let value = model
            .consts
            .iter()
            .find(|(name, _)| *name == constant)
            .expect("declared scenario constant")
            .1;
        input.insert(variable, value);
    }
    input
}

/// Constant scenarios configure the environment; each Judge reads the production projection.
pub(crate) fn claim_transition(
    model: &aterm_spec::derive::Model,
    path: &[&'static str],
    action: &str,
) -> State {
    let mut state = model.init_state();
    for step in path {
        assert_eq!(*step, "Judge");
        state = evaluate(&configured(model, &state), false);
    }
    assert_eq!(action, "Judge");
    evaluate(&configured(model, &state), false)
}

#[test]
fn identity_claims_conform_for_every_dependency_digest_and_generation_case() {
    let default = aterm_models_db::alab_identity_claims_model();
    let configurations = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|models| models[0].name == default.name)
        .expect("registered identity model");
    assert_eq!(configurations.len(), 2usize.pow(3) * 6 * 7);
    let mut judged = 0;
    for model in &configurations {
        let init = model.init_state();
        let mut reached = BTreeMap::from([(init.clone(), Vec::new())]);
        let mut queue = vec![init];
        while let Some(state) = queue.pop() {
            let path = reached[&state].clone();
            for action in &model.actions {
                if !model.action_enabled(action.name, &state) {
                    continue;
                }
                let actual = claim_transition(model, &path, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{} with {:?} from {state:?} produced {actual:?}",
                    action.name,
                    model.consts
                );
                judged += 1;
                if !reached.contains_key(&actual) {
                    let mut next_path = path.clone();
                    next_path.push(action.name);
                    reached.insert(actual.clone(), next_path);
                    queue.push(actual);
                }
            }
        }
        assert_eq!(
            reached.len(),
            aterm_spec::interp::reachable_states(model).len()
        );
    }
    assert_eq!(judged, configurations.len());
    let model = configurations
        .iter()
        .find(|model| {
            let input = configured(model, &model.init_state());
            input["mode"] == 1
                && input["order"] == 3
                && input["ba"] == 1
                && input["ca"] == 0
                && input["cb"] == 0
        })
        .unwrap();
    let source = configured(model, &model.init_state());
    let lost = evaluate(&source, true);
    assert_eq!(lost["shown"], 0, "removed provenance cannot elect a holder");
    assert!(
        !model
            .successors("Judge", &model.init_state())
            .contains(&lost),
        "dropping real carry evidence is no legal transition"
    );
}
