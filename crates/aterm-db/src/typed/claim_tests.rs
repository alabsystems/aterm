// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    alab::registry::MACHINE,
    database::{Candidate, Cell, Change},
};
use astream_wire::sync::Header;
use aterm_json::json;

fn provenance_with_past(count: usize) -> Value {
    let past: Vec<_> = (0..count)
        .map(|number| {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&(number as u64).to_be_bytes());
            aterm_codec::hex::encode_lower(&id)
        })
        .collect();
    json!({"v": 2, "s": [{"o": aterm_codec::hex::encode_lower(&[255; 32]), "p": past}], "e": [null]})
}

#[test]
fn provenance_validation_has_constant_ancestry_allocations() {
    let _ = aterm_heap_count::allocations_in(|| ());
    let allocations = |count| {
        let proof = provenance_with_past(count);
        let (valid, allocated) =
            aterm_heap_count::allocations_in(|| valid_context(std::hint::black_box(&proof)));
        assert!(valid);
        allocated
    };
    let empty = allocations(0);
    for count in [16, 64] {
        assert_eq!(
            allocations(count),
            empty,
            "validation must not allocate an owned ancestry index"
        );
    }
}

#[test]
fn provenance_codec_rejects_malformed_evidence() {
    let known = provenance_with_past(2);
    let start = known["s"][0]["o"].clone();
    let first = known["s"][0]["p"][0].clone();
    let second = known["s"][0]["p"][1].clone();
    assert_eq!(
        Provenance::decode(&known),
        Some(Provenance::Known(std::collections::BTreeMap::from([(
            [255; 32],
            std::collections::BTreeSet::from([digest(&first).unwrap(), digest(&second).unwrap()]),
        )])))
    );
    assert_eq!(Provenance::decode(&Value::Null), Some(Provenance::Unknown));
    assert_eq!(
        Provenance::decode(&Value::Bool(false)),
        Some(Provenance::Invalid)
    );
    assert_eq!(Provenance::decode(&known["s"]), Some(Provenance::Unknown));
    assert_eq!(
        Provenance::decode(&json!({"v": 2, "s": null, "e": [null]})),
        Some(Provenance::Unknown)
    );
    assert_eq!(
        Provenance::decode(&json!({"v": 2, "s": false, "e": [null]})),
        Some(Provenance::Invalid)
    );
    assert!(valid_context(&json!({"v": 2, "l": [null]})));
    for malformed in [
        json!({"v": 1, "s": known["s"], "e": [null]}),
        json!({"v": 2, "s": known["s"]}),
        json!({"v": 2, "s": known["s"], "e": [null], "extra": 0}),
        json!({"v": 2, "s": [], "e": [null]}),
        json!({"v": 2, "s": [{"o": start, "p": [start]}], "e": [null]}),
        json!({"v": 2, "s": [{"o": start, "p": [first, first]}], "e": [null]}),
        json!({"v": 2, "s": [{"o": start, "p": [second, first]}], "e": [null]}),
        json!({"v": 2, "s": [known["s"][0], known["s"][0]], "e": [null]}),
        json!({"v": 2, "s": [{"o": "FF".repeat(32), "p": []}], "e": [null]}),
        json!({"v": 2, "s": known["s"], "e": [null, null]}),
        json!({"v": 2, "s": known["s"], "e": [start, null]}),
        json!({"v": 2, "s": null, "e": [null, null]}),
        json!([{"o": start, "p": [second, first]}]),
    ] {
        assert_eq!(Provenance::decode(&malformed), None, "{malformed:?}");
        assert!(!valid_context(&malformed), "{malformed:?}");
    }
}

#[test]
fn provenance_codec_counts_epochs_and_ancestry_at_the_budget() {
    let at_limit = provenance_with_past(crate::database::MAX_RECORDS - 2);
    assert!(valid_context(&at_limit));
    assert!(Provenance::decode(&at_limit).is_some());
    let over_limit = provenance_with_past(crate::database::MAX_RECORDS - 1);
    assert!(!valid_context(&over_limit));
    assert!(Provenance::decode(&over_limit).is_none());
}

/// Isolate lifecycle tracking from otherwise identical all-live claim projection.
/// The control omits explicit live assertions; both views must expose identical labels.
#[test]
#[ignore = "manual paired claim projection allocation and instruction measurement"]
fn measure_initial_live_claim_projection() {
    use aterm_bench::measurement_clock::{counters, declare_interactive};
    use std::hint::black_box;
    let _ = declare_interactive();
    for count in [64_usize, 256, 1024] {
        let mut index = Index::new(count + 8);
        let mut operations = Vec::new();
        let mut previous = None;
        for row in 0..count {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&(row as u64 + 1).to_be_bytes());
            let mut actor = [0; 48];
            actor[..32].copy_from_slice(&id);
            index
                .insert(Header {
                    id,
                    actor,
                    sequence: 1,
                    previous: None,
                    parents: previous.into_iter().collect(),
                })
                .unwrap();
            previous = Some(id);
            let pk = format!("mc-{:016x}", row + 1);
            operations.push(Op {
                id,
                actor,
                sequence: 1,
                roster: [0; 32],
                period: 0,
                revert: None,
                changes: [("label", format!("m{}", row + 1)), ("row", "live".into())]
                    .into_iter()
                    .map(|(column, value)| Change {
                        collection: MACHINE.name.into(),
                        key: format!("{pk}.{column}"),
                        value: Some(CellValue::automatic(MACHINE.version, json!(value)).encode()),
                    })
                    .collect(),
            });
        }
        let raw = cells(&operations.iter().collect::<Vec<_>>());
        let mut implicit = operations.clone();
        for operation in &mut implicit {
            operation
                .changes
                .retain(|change| change.key.ends_with(".label"));
        }
        let run = |explicit| {
            let operations = if explicit { &operations } else { &implicit };
            black_box(project(
                &MACHINE,
                black_box(&raw),
                operations.iter(),
                &index,
            ))
        };
        for explicit in [false, true] {
            let read = run(explicit);
            assert_eq!(read.rows.len(), count);
            for row in 0..count {
                assert_eq!(
                    read.rows[&format!("mc-{:016x}", row + 1)].get("label"),
                    Some(&json!(format!("m{}", row + 1)))
                );
            }
        }
        for sample in 0..5 {
            for explicit in if sample % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let (_, allocations) = aterm_heap_count::allocations_in(|| drop(run(explicit)));
                let before = counters();
                drop(run(explicit));
                let after = counters();
                let work = before.zip(after).map(|(before, after)| after.since(before));
                eprintln!(
                    "CLAIM-PROJECTION rows={count} explicit_live={explicit} sample={sample} allocations={allocations} instructions={:?} cpu_ns={:?}",
                    work.map(|value| value.instructions),
                    work.map(|value| value.cpu_ns)
                );
            }
        }
    }
}

/// Compare storage layouts with the same signed origins and carried proof context.
#[test]
#[ignore = "manual paired carried claim allocation and instruction measurement"]
fn measure_carried_claim_layout() {
    use aterm_bench::measurement_clock::{counters, declare_interactive};
    use std::hint::black_box;
    let _ = declare_interactive();
    for count in [24_u8, 48, 96] {
        let mut native_index = Index::new(usize::from(count) + 8);
        let mut native = Vec::new();
        for id in 1..=count {
            let pk = format!("mc-{id:016x}");
            let parents = if id == 1 { vec![] } else { vec![id - 1] };
            let mut op = operation(
                id,
                &pk,
                "label",
                &format!("m{id}"),
                &parents,
                &mut native_index,
            );
            op.changes.push(Change {
                collection: MACHINE.name.into(),
                key: format!("{pk}.row"),
                value: Some(CellValue::automatic(MACHINE.version, json!("live")).encode()),
            });
            native.push(op);
        }
        let raw = cells(&native.iter().collect::<Vec<_>>());
        let proof = snapshot(&MACHINE, &raw, native.iter(), &native_index).unwrap();
        let mut combined_index = Index::new(8);
        let combined = vec![carried_operation(
            240,
            &raw,
            Some(&proof),
            &mut combined_index,
        )];
        let combined_cells = cells(&combined.iter().collect::<Vec<_>>());
        let mut split_index = Index::new(usize::from(count) + 8);
        let split: Vec<_> = (1..=count)
            .map(|id| {
                let pk = format!("mc-{id:016x}");
                let row = raw
                    .iter()
                    .filter(|((_, key), _)| cell::CellKey::parse(key).unwrap().pk == pk)
                    .map(|(coordinate, value)| (coordinate.clone(), value.clone()))
                    .collect();
                carried_operation(127 + id, &row, Some(&proof), &mut split_index)
            })
            .collect();
        let split_cells = cells(&split.iter().collect::<Vec<_>>());
        let layouts = [
            ("native", &raw, &native, &native_index),
            ("combined", &combined_cells, &combined, &combined_index),
            ("per_row", &split_cells, &split, &split_index),
        ];
        let run = |layout: usize| {
            let (_, cells, operations, index) = layouts[layout];
            black_box(project(
                &MACHINE,
                black_box(cells),
                operations.iter(),
                index,
            ))
        };
        let expected = run(0);
        for (layout, (_, cells, operations, index)) in layouts.iter().enumerate() {
            let read = run(layout);
            assert_eq!(read.rows.len(), usize::from(count));
            for id in 1..=count {
                let pk = format!("mc-{id:016x}");
                assert!(read.rows[&pk].live());
                assert_eq!(read.rows[&pk].get("label"), Some(&json!(format!("m{id}"))));
                assert_eq!(read.rows[&pk].fields, expected.rows[&pk].fields);
            }
            assert_eq!(
                snapshot(&MACHINE, cells, operations.iter(), index).unwrap(),
                proof
            );
        }
        for sample in 0..5 {
            for layout in if sample % 2 == 0 {
                [0, 1, 2]
            } else {
                [2, 1, 0]
            } {
                let (_, allocations) = aterm_heap_count::allocations_in(|| drop(run(layout)));
                let before = counters();
                drop(run(layout));
                let after = counters();
                let work = before.zip(after).map(|(before, after)| after.since(before));
                eprintln!(
                    "CARRIED-CLAIM-LAYOUT rows={count} layout={} sample={sample} allocations={allocations} instructions={:?} cpu_ns={:?}",
                    layouts[layout].0,
                    work.map(|value| value.instructions),
                    work.map(|value| value.cpu_ns)
                );
            }
        }
    }
}

pub(super) fn operation(
    id: u8,
    pk: &str,
    column: &str,
    value: &str,
    parents: &[u8],
    index: &mut Index,
) -> Op {
    index
        .insert(Header {
            id: [id; 32],
            actor: [id; 48],
            sequence: 1,
            previous: None,
            parents: parents.iter().map(|parent| [*parent; 32]).collect(),
        })
        .unwrap();
    Op {
        id: [id; 32],
        actor: [id; 48],
        sequence: 1,
        roster: [0; 32],
        period: 0,
        revert: None,
        changes: vec![Change {
            collection: MACHINE.name.into(),
            key: format!("{pk}.{column}"),
            value: Some(CellValue::automatic(MACHINE.version, json!(value)).encode()),
        }],
    }
}

#[test]
fn collapsed_initial_activity_matches_the_full_causal_walk() {
    let pk = "mc-0000000000000001";
    for concurrent in [false, true] {
        for variant in 0..8 {
            let mut index = Index::new(8);
            let first = operation(10, pk, "row", "live", &[], &mut index);
            let mut second = operation(
                20,
                pk,
                "row",
                "live",
                if concurrent { &[] } else { &[10] },
                &mut index,
            );
            second.changes[0].value = match variant {
                0 => first.changes[0].value.clone(),
                1 => Some(CellValue::owner(MACHINE.version, json!("live")).encode()),
                2 => Some(CellValue::automatic(MACHINE.version, json!("retired")).encode()),
                3 => Some(CellValue::automatic(MACHINE.version, Value::Null).encode()),
                4 => Some(CellValue::automatic(MACHINE.version + 1, json!("live")).encode()),
                5 => Some(Value::Null),
                6 => Some(
                    cell::carry(
                        &cell::origins(
                            &cells(&[&first])[&(MACHINE.name.into(), format!("{pk}.row"))],
                        )
                        .into_iter()
                        .collect(),
                    )
                    .unwrap(),
                ),
                _ => Some(json!({"future": true})),
            };
            let label = operation(30, pk, "label", "m7", &[10, 20], &mut index);
            let operations = [&first, &second, &label];
            let collapsed =
                activity::Activity::read(&MACHINE, &operations, &index, &index.project());
            let reference = activity::Activity::read_reference(&MACHINE, &operations, &index);
            assert_eq!(
                collapsed.reentries, reference.reentries,
                "variant={variant} concurrent={concurrent}"
            );
            for id in [10, 20, 30] {
                let a = collapsed.at(pk, &[id; 32]);
                let b = reference.at(pk, &[id; 32]);
                assert_eq!(
                    (a.live, &a.epochs),
                    (b.live, &b.epochs),
                    "variant={variant} concurrent={concurrent} id={id}"
                );
                assert_eq!(
                    collapsed.origin_proof(pk, &[id; 32]),
                    reference.origin_proof(pk, &[id; 32])
                );
            }
            let a = collapsed.current(pk);
            let b = reference.current(pk);
            assert_eq!(
                (a.live, &a.epochs),
                (b.live, &b.epochs),
                "variant={variant} concurrent={concurrent}"
            );
        }
    }
}

#[test]
fn lifecycle_frontiers_match_retained_parent_walks_across_merges() {
    let rows = ["mc-0000000000000001", "mc-0000000000000002"];
    let mut index = Index::new(160);
    let mut ops = vec![operation(1, rows[0], "row", "live", &[], &mut index)];
    // Each diamond introduces concurrent writes and joins the preceding diamond's history.
    // With a different actor per operation, later joins outrun the sparse-vector budget.
    for round in 0..50_u8 {
        let previous = 1 + round * 3;
        let left = previous + 1;
        let right = previous + 2;
        let pk = rows[usize::from(round) % rows.len()];
        ops.push(operation(
            left,
            pk,
            "row",
            "retired",
            &[previous],
            &mut index,
        ));
        let mut live = operation(right, pk, "row", "live", &[previous], &mut index);
        if round % 7 == 0 {
            live.revert = Some([previous; 32]);
        }
        ops.push(live);
        // Including the earlier ancestor exercises removal of a dominated lifecycle write.
        ops.push(operation(
            previous + 3,
            pk,
            "label",
            "m7",
            &[previous, left, right],
            &mut index,
        ));
    }
    assert_activity_matches(&rows, &ops, &index);
}

#[test]
fn lifecycle_frontiers_follow_retained_parents_at_a_checkpoint_floor() {
    use astream_wire::sync::FloorRecord;

    let pk = "mc-0000000000000001";
    let mut index = Index::new(4);
    let retired = operation(1, pk, "row", "retired", &[], &mut index);
    // The checkpoint attests that it saw the retirement, but its parent chain is absent.
    index
        .insert_floor(FloorRecord {
            header: Header {
                id: [2; 32],
                actor: [2; 48],
                sequence: 1,
                previous: None,
                parents: vec![[1; 32]],
            },
            past: BTreeMap::from([([1; 48], 1), ([2; 48], 1)]),
        })
        .unwrap();
    let live = operation(3, pk, "row", "live", &[2], &mut index);
    let merge = operation(4, pk, "label", "m7", &[1, 3], &mut index);
    assert!(index.project().observes(&index, &live.id, &retired.id));
    assert!(!index.observes(&live.id, &retired.id));
    assert_activity_matches(&[pk], &[retired, live, merge], &index);
}

fn assert_activity_matches(rows: &[&str], ops: &[Op], index: &Index) {
    let operations: Vec<_> = ops.iter().collect();
    let actual = activity::Activity::read(&MACHINE, &operations, index, &index.project());
    let expected = activity::Activity::read_reference(&MACHINE, &operations, index);
    assert_eq!(actual.reentries, expected.reentries);
    for pk in rows {
        let a = actual.current(pk);
        let b = expected.current(pk);
        assert_eq!((a.live, &a.epochs), (b.live, &b.epochs));
        for op in ops {
            let a = actual.at(pk, &op.id);
            let b = expected.at(pk, &op.id);
            assert_eq!((a.live, &a.epochs), (b.live, &b.epochs));
            assert_eq!(
                actual.origin_proof(pk, &op.id),
                expected.origin_proof(pk, &op.id)
            );
        }
    }
}

pub(super) fn cells(operations: &[&Op]) -> Cells {
    let mut cells = Cells::new();
    for operation in operations {
        for change in &operation.changes {
            cells
                .entry((change.collection.clone(), change.key.clone()))
                .or_insert_with(|| Cell {
                    stopped: false,
                    candidates: Vec::new(),
                })
                .candidates
                .push(Candidate {
                    operation: operation.id,
                    value: change.value.clone().unwrap(),
                });
        }
    }
    cells
}

pub(super) fn carried_operation(
    id: u8,
    source: &Cells,
    evidence: Option<&CarriedClaims>,
    index: &mut Index,
) -> Op {
    let mut carried = operation(id, "mc-0000000000000001", "label", "m7", &[], index);
    carried.changes = source
        .iter()
        .map(|(coord, register)| {
            let origins = cell::origins(register).into_iter().collect();
            let mut encoded = cell::carry(&origins).unwrap();
            for item in encoded
                .as_object_mut()
                .unwrap()
                .get_mut("k")
                .unwrap()
                .as_array_mut()
                .unwrap()
            {
                let origin = digest(&item["o"]).unwrap();
                if let Some(context) = evidence
                    .and_then(|all| all.get(coord))
                    .and_then(|all| all.get(&origin))
                {
                    item.as_object_mut()
                        .unwrap()
                        .insert("c".into(), context.clone());
                }
            }
            Change {
                collection: coord.0.clone(),
                key: coord.1.clone(),
                value: Some(encoded),
            }
        })
        .collect();
    carried
}

#[test]
fn reviving_a_retired_row_does_not_reclaim_an_observed_incumbents_label() {
    let a_pk = "mc-0000000000000001";
    let b_pk = "mc-0000000000000002";
    let mut index = Index::new(8);
    let a = operation(10, a_pk, "label", "m7", &[], &mut index);
    let retired = operation(50, a_pk, "row", "retired", &[10], &mut index);
    let b = operation(20, b_pk, "label", "m7", &[50], &mut index);
    let before = project(
        &MACHINE,
        &cells(&[&a, &retired, &b]),
        [&a, &retired, &b].into_iter(),
        &index,
    );
    assert_eq!(before.rows[b_pk].get("label"), Some(&json!("m7")));
    assert!(!before.rows[a_pk].live());
    let revived = operation(5, a_pk, "row", "live", &[20], &mut index);
    let after = project(
        &MACHINE,
        &cells(&[&a, &b, &revived]),
        [&a, &retired, &b, &revived].into_iter(),
        &index,
    );
    assert!(after.rows[a_pk].live());
    assert_eq!(
        after.rows[b_pk].get("label"),
        Some(&json!("m7")),
        "retirement released the label before this row observed its new holder"
    );
    let raw = cells(&[&a, &b, &revived]);
    let operations = [&a, &retired, &b, &revived];
    let normalized = interpreted_cells(&MACHINE, &raw, operations.into_iter(), &index);
    let evidence = snapshot(&MACHINE, &normalized, operations.into_iter(), &index).unwrap();
    let mut next = Index::new(8);
    let carried = carried_operation(100, &normalized, Some(&evidence), &mut next);
    let copied = cells(&[&carried]);
    let read = project(&MACHINE, &copied, [&carried].into_iter(), &next);
    assert_eq!(read.rows[b_pk].get("label"), Some(&json!("m7")));
    assert_eq!(read.rows[a_pk].get("label"), after.rows[a_pk].get("label"));
    let unproven = carried_operation(101, &normalized, None, &mut next);
    let hydrated = project(
        &MACHINE,
        &cells(&[&carried, &unproven]),
        [&carried, &unproven].into_iter(),
        &next,
    );
    assert_eq!(hydrated.rows[b_pk].get("label"), Some(&json!("m7")));
    assert_eq!(
        hydrated.rows[a_pk].get("label"),
        after.rows[a_pk].get("label")
    );
    let evidence = snapshot(&MACHINE, &copied, [&carried].into_iter(), &next).unwrap();
    let mut last = Index::new(8);
    let recarried = carried_operation(200, &copied, Some(&evidence), &mut last);
    let read = project(
        &MACHINE,
        &cells(&[&recarried]),
        [&recarried].into_iter(),
        &last,
    );
    assert_eq!(read.rows[b_pk].get("label"), Some(&json!("m7")));
    assert_eq!(read.rows[a_pk].get("label"), after.rows[a_pk].get("label"));
}

#[test]
fn a_live_reassertion_preserves_tenure_and_a_concurrent_edit_stays_conflicted() {
    let a_pk = "mc-0000000000000001";
    let b_pk = "mc-0000000000000002";
    let mut index = Index::new(8);
    let a = operation(10, a_pk, "label", "m7", &[], &mut index);
    let b = operation(20, b_pk, "label", "m7", &[], &mut index);
    let live = operation(5, a_pk, "row", "live", &[10, 20], &mut index);
    let read = project(
        &MACHINE,
        &cells(&[&a, &b, &live]),
        [&a, &b, &live].into_iter(),
        &index,
    );
    assert_eq!(read.rows[a_pk].get("label"), Some(&json!("m7")));
    let retired = operation(50, a_pk, "row", "retired", &[5], &mut index);
    let revived = operation(60, a_pk, "row", "live", &[50], &mut index);
    let edited = operation(70, a_pk, "label", "m8", &[50], &mut index);
    let raw = cells(&[&edited, &b, &revived]);
    let read = project(
        &MACHINE,
        &raw,
        [&a, &b, &live, &retired, &revived, &edited].into_iter(),
        &index,
    );
    assert!(read.rows[a_pk].fields["label"].conflicted());
    assert_eq!(read.rows[a_pk].get("label"), None);
    assert_eq!(read.rows[b_pk].get("label"), Some(&json!("m7")));
}

#[test]
fn a_partial_carry_cannot_pair_an_old_claim_with_a_new_activity_period() {
    let a_pk = "mc-0000000000000001";
    let b_pk = "mc-0000000000000002";
    let mut source = Index::new(8);
    let a = operation(10, a_pk, "label", "m7", &[], &mut source);
    let initial = cells(&[&a]);
    let first_proof = snapshot(&MACHINE, &initial, [&a].into_iter(), &source).unwrap();
    let retired = operation(50, a_pk, "row", "retired", &[10], &mut source);
    let b = operation(20, b_pk, "label", "m7", &[50], &mut source);
    let revived = operation(5, a_pk, "row", "live", &[20], &mut source);
    let raw = cells(&[&a, &b, &revived]);
    let operations = [&a, &retired, &b, &revived];
    let normalized = interpreted_cells(&MACHINE, &raw, operations.into_iter(), &source);
    let proof = snapshot(&MACHINE, &normalized, operations.into_iter(), &source).unwrap();
    let mut target = Index::new(8);
    let old = carried_operation(100, &initial, Some(&first_proof), &mut target);
    let mut partial = normalized.into_owned();
    partial.remove(&(MACHINE.name.into(), format!("{a_pk}.label")));
    let rest = carried_operation(101, &partial, Some(&proof), &mut target);
    let read = project(
        &MACHINE,
        &cells(&[&old, &rest]),
        [&old, &rest].into_iter(),
        &target,
    );
    assert_ne!(read.rows[a_pk].get("label"), Some(&json!("m7")));
    assert_eq!(read.rows[b_pk].get("label"), Some(&json!("m7")));
}

#[test]
fn an_expired_claim_cannot_block_a_sole_unproven_claimant() {
    let a_pk = "mc-0000000000000001";
    let b_pk = "mc-0000000000000002";
    let c_pk = "mc-0000000000000003";
    for (column, value) in [("label", "m7"), ("node", "0123456789abcdef")] {
        let mut source = Index::new(8);
        let a = operation(10, a_pk, column, value, &[], &mut source);
        let initial = cells(&[&a]);
        let initial_proof = snapshot(&MACHINE, &initial, [&a].into_iter(), &source).unwrap();
        let retired = operation(50, a_pk, "row", "retired", &[10], &mut source);
        let revived = operation(5, a_pk, "row", "live", &[50], &mut source);
        let operations = [&a, &retired, &revived];
        let raw = cells(&[&a, &revived]);
        let mut partial =
            interpreted_cells(&MACHINE, &raw, operations.into_iter(), &source).into_owned();
        let proof = snapshot(&MACHINE, &partial, operations.into_iter(), &source).unwrap();
        partial.remove(&(MACHINE.name.into(), format!("{a_pk}.{column}")));
        let b = operation(20, b_pk, column, value, &[], &mut source);
        let c = operation(21, c_pk, column, value, &[], &mut source);
        let mut target = Index::new(8);
        let old = carried_operation(100, &initial, Some(&initial_proof), &mut target);
        let rest = carried_operation(101, &partial, Some(&proof), &mut target);
        let unknown = carried_operation(102, &cells(&[&b]), None, &mut target);
        let alone = project(
            &MACHINE,
            &cells(&[&unknown]),
            [&unknown].into_iter(),
            &target,
        );
        assert_eq!(alone.rows[b_pk].get(column), Some(&json!(value)));
        let operations = [&old, &rest, &unknown];
        let combined = cells(&operations);
        let read = project(&MACHINE, &combined, operations.into_iter(), &target);
        assert_eq!(
            read.rows[a_pk].get(column),
            None,
            "expired tenure stays absent"
        );
        assert_eq!(read.rows[a_pk].fields[column].value(), Some(&json!(value)));
        assert_eq!(
            read.rows[b_pk].get(column),
            Some(&json!(value)),
            "a proved-empty contender cannot block the sole remaining claim"
        );
        let proof = snapshot(&MACHINE, &combined, operations.into_iter(), &target).unwrap();
        let recarried = carried_operation(103, &combined, Some(&proof), &mut target);
        let read = project(
            &MACHINE,
            &cells(&[&recarried]),
            [&recarried].into_iter(),
            &target,
        );
        assert_eq!(read.rows[a_pk].get(column), None);
        assert_eq!(read.rows[b_pk].get(column), Some(&json!(value)));
        let other = carried_operation(104, &cells(&[&c]), None, &mut target);
        let operations = [&old, &rest, &unknown, &other];
        let read = project(
            &MACHINE,
            &cells(&operations),
            operations.into_iter(),
            &target,
        );
        for pk in [a_pk, b_pk, c_pk] {
            assert_eq!(
                read.rows[pk].get(column),
                None,
                "unknown rivals stay withheld"
            );
        }
    }
}

#[test]
fn one_native_reactivation_can_restore_multiple_previously_carried_rows() {
    let pks = ["mc-0000000000000001", "mc-0000000000000002"];
    let mut source = Index::new(8);
    let a = operation(10, pks[0], "label", "m7", &[], &mut source);
    let c = operation(11, pks[1], "label", "m8", &[10], &mut source);
    let mut retired = operation(50, pks[0], "row", "retired", &[11], &mut source);
    let mut second = retired.changes[0].clone();
    second.key = format!("{}.row", pks[1]);
    retired.changes.push(second);
    let raw = cells(&[&a, &c, &retired]);
    let proof = snapshot(&MACHINE, &raw, [&a, &c, &retired].into_iter(), &source).unwrap();
    let mut target = Index::new(8);
    let carried = carried_operation(100, &raw, Some(&proof), &mut target);
    let mut revived = operation(5, pks[0], "row", "live", &[100], &mut target);
    let mut second = revived.changes[0].clone();
    second.key = format!("{}.row", pks[1]);
    revived.changes.push(second);
    let mut raw = cells(&[&carried]);
    raw.extend(cells(&[&revived]));
    let operations = [&carried, &revived];
    let normalized = interpreted_cells(&MACHINE, &raw, operations.into_iter(), &target);
    let proof = snapshot(&MACHINE, &normalized, operations.into_iter(), &target).unwrap();
    let read = project(&MACHINE, &raw, operations.into_iter(), &target);
    for (pk, label) in pks.into_iter().zip(["m7", "m8"]) {
        assert_eq!(read.rows[pk].get("label"), Some(&json!(label)));
        let coordinate = (MACHINE.name.into(), format!("{pk}.label"));
        assert_eq!(normalized[&coordinate].candidates[0].operation, [5; 32]);
        let Provenance::Known(starts) = Provenance::decode(&proof[&coordinate][&[5; 32]]).unwrap()
        else {
            panic!("known reactivation tenure")
        };
        assert_eq!(starts.keys().copied().collect::<Vec<_>>(), vec![[5; 32]]);
    }
}

#[test]
#[cfg(unix)]
fn the_importer_adopts_the_canonical_node_holder() {
    use crate::alab::import::{
        self, Input, KnownMac,
        tests::{Stand, store},
    };
    let mut index = Index::new(8);
    let node = "0011223344556677";
    let mut a = operation(30, "mc-0000000000000001", "node", node, &[], &mut index);
    let mut b = operation(10, "mc-0000000000000002", "node", node, &[30], &mut index);
    for (op, label) in [(&mut a, "m12"), (&mut b, "m40")] {
        let pk = op.changes[0].key.split('.').next().unwrap().to_owned();
        for (column, value) in [("label", label), ("row", "live")] {
            op.changes.push(Change {
                collection: MACHINE.name.into(),
                key: format!("{pk}.{column}"),
                value: Some(CellValue::owner(MACHINE.version, json!(value)).encode()),
            });
        }
    }
    let view = crate::typed::View {
        table: None,
        cells: cells(&[&a, &b]),
        causal: index,
        ops: vec![a, b],
        quota: Default::default(),
        inert: Default::default(),
        owned: Default::default(),
        logs: Default::default(),
        complete: true,
        awaiting: crate::typed::Awaiting::NONE,
        refused: Vec::new(),
        refusals: Vec::new(),
        cells_from: None,
    };
    let macs = [KnownMac {
        label: None,
        aliases: vec![crate::machines::Label::plain(70)],
        node: Some(node.into()),
        fleet_node: None,
        hosts: Vec::new(),
    }];
    let input = Input {
        sheets: &[],
        macs: &macs,
        vendors: &Stand,
        store: store(),
        now_ms: 0,
        births: crate::alab::import::Births::None,
        unread: &[],
    };
    let plan = import::plan(&input, &view).unwrap();
    assert!(
        plan.planned
            .iter()
            .any(|row| row.pk == "mc-0000000000000001")
    );
    assert!(
        plan.planned
            .iter()
            .all(|row| row.pk != "mc-0000000000000002")
    );
    assert!(
        !plan
            .findings
            .iter()
            .any(|finding| matches!(finding.kind, import::Kind::NodeAmbiguous))
    );
}

#[test]
fn a_partial_import_keeps_the_order_of_a_claim_the_source_already_renamed() {
    let mut source = Index::new(8);
    let a = operation(30, "mc-0000000000000002", "label", "m7", &[], &mut source);
    let initial = snapshot(&MACHINE, &cells(&[&a]), [&a].into_iter(), &source).unwrap();
    let b = operation(10, "mc-0000000000000001", "label", "m7", &[30], &mut source);
    let away = operation(
        50,
        "mc-0000000000000002",
        "label",
        "m8",
        &[10, 30],
        &mut source,
    );
    let current = cells(&[&away, &b]);
    let evidence = snapshot(&MACHINE, &current, [&a, &b, &away].into_iter(), &source).unwrap();
    let mut target = Index::new(8);
    let first = carried_operation(100, &cells(&[&a]), Some(&initial), &mut target);
    // The renamed row has not arrived yet. Reads keep held records rather than waiting.
    let second = carried_operation(101, &cells(&[&b]), Some(&evidence), &mut target);
    let partial = project(
        &MACHINE,
        &cells(&[&first, &second]),
        [&first, &second].into_iter(),
        &target,
    );
    assert_eq!(
        partial.rows["mc-0000000000000002"].get("label"),
        Some(&json!("m7")),
        "an observed later claim must not steal the held older claim during import"
    );
    let held = cells(&[&first, &second]);
    let view = crate::typed::View {
        table: None,
        cells: held.clone(),
        causal: target.clone(),
        ops: vec![first.clone(), second.clone()],
        quota: Default::default(),
        inert: Default::default(),
        owned: Default::default(),
        logs: Default::default(),
        complete: true,
        awaiting: crate::typed::Awaiting::NONE,
        refused: Vec::new(),
        refusals: Vec::new(),
        cells_from: None,
    };
    let empty = crate::typed::owned::OwnedRead::default();
    let lab = crate::alab::view::LabView::read_view(&view, &empty, &empty, None);
    assert_eq!(lab.machine_label("mc-0000000000000002"), Some("m7"));
    assert!(
        lab.machine_label("mc-0000000000000001")
            .unwrap()
            .starts_with("m7-")
    );
    let evidence = snapshot(&MACHINE, &held, [&first, &second].into_iter(), &target).unwrap();
    let mut next = Index::new(8);
    let copied = carried_operation(110, &held, Some(&evidence), &mut next);
    let again = project(&MACHINE, &cells(&[&copied]), [&copied].into_iter(), &next);
    assert_eq!(
        again.rows["mc-0000000000000002"].get("label"),
        Some(&json!("m7")),
        "another retirement must preserve the withdrawn ancestor's ordering evidence"
    );
}

#[test]
fn a_causally_later_claim_cannot_create_a_three_way_comparator_cycle() {
    let mut causal = Index::new(8);
    let a = operation(30, "mc-0000000000000001", "label", "m7", &[], &mut causal);
    let b = operation(10, "mc-0000000000000002", "label", "m7", &[30], &mut causal);
    let c = operation(20, "mc-0000000000000003", "label", "m7", &[], &mut causal);
    let raw = cells(&[&a, &b, &c]);
    for operations in [
        [&a, &b, &c],
        [&a, &c, &b],
        [&b, &a, &c],
        [&b, &c, &a],
        [&c, &a, &b],
        [&c, &b, &a],
    ] {
        let read = project(&MACHINE, &raw, operations.into_iter(), &causal);
        assert_eq!(
            read.rows["mc-0000000000000003"].get("label"),
            Some(&json!("m7"))
        );
        let labels: BTreeSet<_> = read
            .rows
            .values()
            .map(|row| row.get("label").unwrap().as_str().unwrap())
            .collect();
        assert_eq!(labels.len(), 3);
        for row in read.rows.values() {
            assert_eq!(
                row.fields["label"].value(),
                Some(&json!("m7")),
                "signed candidates remain unchanged"
            );
        }
    }
}

#[test]
fn repeating_a_number_preserves_tenure_but_leaving_and_returning_does_not() {
    let mut causal = Index::new(8);
    let a = operation(10, "mc-0000000000000001", "label", "m7", &[], &mut causal);
    let b = operation(20, "mc-0000000000000002", "label", "m7", &[], &mut causal);
    let repeat = operation(
        30,
        "mc-0000000000000001",
        "label",
        "m7",
        &[10, 20],
        &mut causal,
    );
    let first = project(
        &MACHINE,
        &cells(&[&repeat, &b]),
        [&a, &b, &repeat].into_iter(),
        &causal,
    );
    assert_eq!(
        first.rows["mc-0000000000000001"].get("label"),
        Some(&json!("m7"))
    );
    let away = operation(40, "mc-0000000000000001", "label", "m8", &[30], &mut causal);
    let back = operation(5, "mc-0000000000000001", "label", "m7", &[40], &mut causal);
    let returned = project(
        &MACHINE,
        &cells(&[&back, &b]),
        [&a, &b, &repeat, &away, &back].into_iter(),
        &causal,
    );
    assert_eq!(
        returned.rows["mc-0000000000000002"].get("label"),
        Some(&json!("m7"))
    );
}

#[test]
fn a_losing_node_claim_is_withheld_without_fabricating_a_principal() {
    let mut causal = Index::new(4);
    let a = operation(
        10,
        "mc-0000000000000001",
        "node",
        "0123456789abcdef",
        &[],
        &mut causal,
    );
    let b = operation(
        20,
        "mc-0000000000000002",
        "node",
        "0123456789abcdef",
        &[],
        &mut causal,
    );
    let raw = cells(&[&a, &b]);
    let read = project(&MACHINE, &raw, [&a, &b].into_iter(), &causal);
    assert_eq!(
        read.rows["mc-0000000000000001"].get("node"),
        Some(&json!("0123456789abcdef"))
    );
    assert_eq!(read.rows["mc-0000000000000002"].get("node"), None);
    assert_eq!(
        read.rows["mc-0000000000000002"].fields["node"].value(),
        Some(&json!("0123456789abcdef"))
    );
    let unknown = project(&MACHINE, &raw, [&a, &b].into_iter(), &Index::new(0));
    assert!(
        unknown.rows.values().all(|row| row.get("node").is_none()),
        "missing ancestry must not invent concurrency"
    );
}

#[test]
fn exporting_a_clash_label_and_saving_unchanged_preserves_the_signed_claim() {
    use crate::alab::registry::REGISTRY;
    let mut causal = Index::new(4);
    let a = operation(10, "mc-0000000000000001", "label", "m7", &[], &mut causal);
    let b = operation(20, "mc-0000000000000002", "label", "m7", &[], &mut causal);
    let raw = cells(&[&a, &b]);
    let read = project(&MACHINE, &raw, [&a, &b].into_iter(), &causal);
    let bare = super::super::grid::Where::parse("label=m7").unwrap();
    assert_eq!(
        read.rows
            .iter()
            .filter(|(pk, row)| bare.matches(&MACHINE, pk, row).unwrap())
            .count(),
        1,
        "canonical number lookup must select one holder"
    );
    let shown =
        super::super::grid::export_projected(&REGISTRY, &MACHINE, &raw, &read, &[]).unwrap();
    let clash = read.rows["mc-0000000000000002"]
        .get("label")
        .unwrap()
        .as_str()
        .unwrap();
    assert_ne!(clash, "m7");
    assert!(shown.text.contains(clash));
    let plan = super::super::grid::plan(&REGISTRY, &MACHINE, &shown.base, &shown.text, &|_, _| {
        panic!("an unchanged export births no row")
    })
    .unwrap();
    assert!(plan.is_empty());
    assert!(plan.born.is_empty());
    assert_eq!(
        read.rows["mc-0000000000000002"].fields["label"].value(),
        Some(&json!("m7"))
    );
}

#[test]
fn long_same_value_histories_keep_the_first_tenure_without_recursive_ancestry() {
    let count = 4096u64;
    let mut causal = Index::new(count as usize);
    let mut operations = Vec::new();
    let mut previous = None;
    for sequence in 1..=count {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&sequence.to_be_bytes());
        causal
            .insert(Header {
                id,
                actor: [250; 48],
                sequence,
                previous,
                parents: previous.into_iter().collect(),
            })
            .unwrap();
        operations.push(Op {
            id,
            actor: [250; 48],
            sequence,
            roster: [0; 32],
            period: 0,
            revert: None,
            changes: vec![Change {
                collection: MACHINE.name.into(),
                key: "mc-0000000000000001.label".into(),
                value: Some(CellValue::automatic(MACHINE.version, json!("m7")).encode()),
            }],
        });
        previous = Some(id);
    }
    let last = operations.last().unwrap();
    let provenance = snapshot(&MACHINE, &cells(&[last]), operations.iter(), &causal).unwrap();
    let encoded = &provenance[&(MACHINE.name.into(), "mc-0000000000000001.label".into())][&last.id];
    let Provenance::Known(starts) = Provenance::decode(encoded).unwrap() else {
        panic!("native history is known")
    };
    assert_eq!(
        starts.keys().copied().collect::<Vec<_>>(),
        vec![operations[0].id]
    );
}

#[test]
#[cfg(unix)]
fn restoring_a_withdrawn_claim_has_the_same_tenure_before_and_after_a_roll() {
    let a_pk = "mc-0000000000000001";
    let b_pk = "mc-0000000000000002";
    let mut source = Index::new(4);
    let a = operation(10, a_pk, "label", "m7", &[], &mut source);
    let b = operation(20, b_pk, "label", "m7", &[], &mut source);
    let original = cells(&[&a, &b]);
    let evidence = snapshot(&MACHINE, &original, [&a, &b].into_iter(), &source).unwrap();

    let withdrawn = operation(50, a_pk, "label", "m8", &[10, 20], &mut source);
    let mut restored = operation(5, a_pk, "label", "m7", &[50], &mut source);
    restored.revert = Some(withdrawn.id);
    // Compensation restores the exact beforeimage. Native cells contain their typed value;
    // after a roll the same value is enclosed in its original candidate and claim evidence.
    restored.changes[0].value = a.changes[0].value.clone();
    let native = project(
        &MACHINE,
        &cells(&[&restored, &b]),
        [&a, &b, &withdrawn, &restored].into_iter(),
        &source,
    );
    assert_eq!(native.rows[b_pk].get("label"), Some(&json!("m7")));

    let mut destination = Index::new(4);
    let carried = carried_operation(100, &original, Some(&evidence), &mut destination);
    let withdrawn = operation(50, a_pk, "label", "m8", &[100], &mut destination);
    let mut restored = operation(5, a_pk, "label", "m7", &[50], &mut destination);
    restored.revert = Some(withdrawn.id);
    let key = format!("{a_pk}.label");
    restored.changes[0].value = carried
        .changes
        .iter()
        .find(|change| change.key == key)
        .unwrap()
        .value
        .clone();
    let mut final_cells = cells(&[&carried]);
    final_cells.extend(cells(&[&restored]));
    let rolled = project(
        &MACHINE,
        &final_cells,
        [&carried, &withdrawn, &restored].into_iter(),
        &destination,
    );
    for pk in [a_pk, b_pk] {
        assert_eq!(
            rolled.rows[pk].get("label"),
            native.rows[pk].get("label"),
            "an exact compensation must not depend on the beforeimage's carry encoding"
        );
    }
    let operations = [&carried, &withdrawn, &restored];
    let normalized = restored_cells(&MACHINE, &final_cells, operations.into_iter());
    assert_eq!(
        normalized[&(MACHINE.name.into(), key)].candidates[0].value,
        a.changes[0].value.clone().unwrap(),
        "the restored typed value keeps its import and automatic metadata"
    );
    let evidence = snapshot(&MACHINE, &normalized, operations.into_iter(), &destination).unwrap();
    let mut next = Index::new(4);
    let carried_again = carried_operation(200, &normalized, Some(&evidence), &mut next);
    let after = project(
        &MACHINE,
        &cells(&[&carried_again]),
        [&carried_again].into_iter(),
        &next,
    );
    for pk in [a_pk, b_pk] {
        assert_eq!(after.rows[pk].get("label"), native.rows[pk].get("label"));
    }
}

#[test]
fn restoring_conflicted_or_opaque_claims_preserves_their_unresolved_evidence() {
    let pk = "mc-0000000000000001";
    for opaque in [false, true] {
        let mut source = Index::new(4);
        let mut a = operation(10, pk, "label", "m7", &[], &mut source);
        let b = operation(20, pk, "label", "m8", &[], &mut source);
        if opaque {
            a.changes[0].value = Some(json!({"future": "m7"}));
        }
        let mut destination = Index::new(4);
        let mut restored = carried_operation(100, &cells(&[&a, &b]), None, &mut destination);
        restored.revert = Some(a.id);
        let raw = cells(&[&restored]);
        let interpreted = restored_cells(&MACHINE, &raw, [&restored].into_iter());
        assert!(matches!(interpreted, std::borrow::Cow::Borrowed(_)));
        assert_eq!(*interpreted, raw);
        let projected = project(&MACHINE, &raw, [&restored].into_iter(), &destination);
        let row = &projected.rows[pk];
        if opaque {
            assert!(row.raw.contains("label"));
        } else {
            assert!(row.fields["label"].conflicted());
        }
    }
}

#[test]
fn a_new_claim_after_a_carry_does_not_outrank_its_observed_incumbent() {
    let mut source_index = Index::new(4);
    let a = operation(
        10,
        "mc-0000000000000001",
        "label",
        "m7",
        &[],
        &mut source_index,
    );
    let b = operation(
        20,
        "mc-0000000000000002",
        "label",
        "m7",
        &[],
        &mut source_index,
    );
    let raw = cells(&[&a, &b]);
    let provenance = snapshot(&MACHINE, &raw, [&a, &b].into_iter(), &source_index).unwrap();
    let mut target_index = Index::new(8);
    let mut rolled = operation(
        100,
        "mc-0000000000000001",
        "label",
        "m7",
        &[],
        &mut target_index,
    );
    rolled.changes = [&a, &b]
        .iter()
        .map(|original| {
            let change = &original.changes[0];
            let mut value = cell::carry(&BTreeMap::from([(
                original.id,
                change.value.clone().unwrap(),
            )]))
            .unwrap();
            value
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
                    provenance[&(change.collection.clone(), change.key.clone())][&original.id]
                        .clone(),
                );
            Change {
                collection: change.collection.clone(),
                key: change.key.clone(),
                value: Some(value),
            }
        })
        .collect();
    let late = operation(
        1,
        "mc-0000000000000003",
        "label",
        "m7",
        &[100],
        &mut target_index,
    );
    let after = project(
        &MACHINE,
        &cells(&[&rolled, &late]),
        [&rolled, &late].into_iter(),
        &target_index,
    );
    assert_eq!(
        after.rows["mc-0000000000000001"].get("label"),
        Some(&json!("m7"))
    );
    assert_ne!(
        after.rows["mc-0000000000000003"].get("label"),
        Some(&json!("m7"))
    );
}

/// A registry 0.199.0 rolled carries every Mac's lifecycle with no proof of its period. A claim
/// written in the new generation after that carry has its whole history there and keeps its
/// tenure: a carried Mac set to the label and node of a Mac born after the roll, by a write that
/// observes that birth, takes a clash label and no node, and the newer Mac keeps both, though
/// the carried Mac's write has the lower digest. The next roll, which carries the claims with
/// their periods and the lifecycle still with none, keeps that election. A lifecycle carried with
/// a proof that contradicts itself (`false`) is no carry without a proof: it names no period, so
/// the carried Mac's claims stay unknown and the clash withholds the label and the node from both.
/// NEGATIVE CONTROL: with `Activity::read` giving a lifecycle carried with no proof no period
/// (`None`, as 804b63029c7 did), both Macs lose the label and the node; with it naming a period
/// for a contradicted proof too, the carried Mac's claim is judged as though it were proved.
#[test]
fn a_claim_written_after_a_lifecycle_carried_with_no_proof_keeps_its_tenure() {
    let (x, y) = ("mc-0000000000000001", "mc-0000000000000002");
    let birth = |id: u8, pk: &str, label: &str, node: &str, parents: &[u8], index: &mut Index| {
        let mut born = operation(id, pk, "label", label, parents, index);
        for (column, value) in [("node", node), ("row", "live")] {
            born.changes.push(Change {
                collection: MACHINE.name.into(),
                key: format!("{pk}.{column}"),
                value: Some(CellValue::automatic(MACHINE.version, json!(value)).encode()),
            });
        }
        born
    };
    let mut source = Index::new(8);
    let carried = cells(&[&birth(60, x, "m3", "00000000000000a3", &[], &mut source)]);
    let lifecycle = (MACHINE.name.into(), format!("{x}.row"));
    // The roll, with the lifecycle's proof `evidence` gives (none, as 0.199.0's), then the newer
    // Mac's birth and the carried Mac's relabel that observes it.
    let rolled = |evidence: Option<&CarriedClaims>| {
        let mut index = Index::new(8);
        let roll = carried_operation(100, &carried, evidence, &mut index);
        let newer = birth(20, y, "m9", "00000000000000b9", &[100], &mut index);
        let mut relabel = operation(5, x, "label", "m9", &[20], &mut index);
        relabel.changes.push(Change {
            collection: MACHINE.name.into(),
            key: format!("{x}.node"),
            value: Some(CellValue::automatic(MACHINE.version, json!("00000000000000b9")).encode()),
        });
        let mut raw = cells(&[&roll, &newer]);
        raw.extend(cells(&[&relabel]));
        (raw, [roll, newer, relabel], index)
    };
    let (raw, ops, index) = rolled(None);
    let operations = [&ops[0], &ops[1], &ops[2]];
    let elected = |read: &cell::Projection| {
        assert_eq!(read.rows[y].get("label"), Some(&json!("m9")));
        assert_eq!(read.rows[y].get("node"), Some(&json!("00000000000000b9")));
        let clash = read.rows[x].get("label").and_then(Value::as_str).unwrap();
        assert!(clash.starts_with("m9-"), "{clash}");
        assert_eq!(read.rows[x].get("node"), None);
    };
    elected(&project(&MACHINE, &raw, operations.into_iter(), &index));

    let normalized = interpreted_cells(&MACHINE, &raw, operations.into_iter(), &index);
    let evidence = snapshot(&MACHINE, &normalized, operations.into_iter(), &index).unwrap();
    assert_eq!(
        evidence[&lifecycle][&[60; 32]],
        Value::Null,
        "the next roll still carries no proof of the lifecycle's period"
    );
    let mut next = Index::new(8);
    let rerolled = carried_operation(200, &normalized, Some(&evidence), &mut next);
    elected(&project(
        &MACHINE,
        &cells(&[&rerolled]),
        [&rerolled].into_iter(),
        &next,
    ));

    let contradicted =
        CarriedClaims::from([(lifecycle, BTreeMap::from([([60; 32], Value::Bool(false))]))]);
    let (raw, ops, index) = rolled(Some(&contradicted));
    let read = project(&MACHINE, &raw, ops.iter(), &index);
    for pk in [x, y] {
        assert_eq!(read.rows[pk].get("label"), None, "{pk}");
        assert_eq!(read.rows[pk].get("node"), None, "{pk}");
    }
}
