// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a claimed column's [`Settlement`] says a Write will make each row read as, against what
//! the read shows once that Write is one of its operations. Every value is synthetic.

use super::tests::{cells, operation};
use super::*;
use crate::{alab::registry::MACHINE, database::Change};
use aterm_json::json;

const A: &str = "mc-0000000000000001";
const B: &str = "mc-0000000000000002";
const C: &str = "mc-0000000000000003";
const D: &str = "mc-0000000000000004";
const N: &str = "mc-0000000000000009";

/// One operation `id`, after `parents`, writing each of `changes` (row, column, value).
fn write(id: u8, changes: &[(&str, &str, &str)], parents: &[u8], index: &mut Index) -> Op {
    let (pk, column, value) = changes[0];
    let mut op = operation(id, pk, column, value, parents, index);
    op.changes = changes
        .iter()
        .map(|(pk, column, value)| Change {
            collection: MACHINE.name.into(),
            key: format!("{pk}.{column}"),
            value: Some(CellValue::automatic(MACHINE.version, json!(value)).encode()),
        })
        .collect();
    op
}

/// A label staged in `pk`.
fn label(pk: &str, value: &str) -> (String, Staged) {
    (
        pk.to_owned(),
        Staged {
            value: Some(json!(value)),
            ..Staged::default()
        },
    )
}

/// A lifecycle staged in `pk`.
fn life(pk: &str, live: bool) -> (String, Staged) {
    (
        pk.to_owned(),
        Staged {
            live: Some(live),
            ..Staged::default()
        },
    )
}

/// The labels column's settlement of the read `current` (the operations whose values the cells
/// hold) of `history` holds.
fn settlement(current: &[&Op], history: &[&Op], index: &Index) -> Settlement {
    let (projected, mut settled) = settled(
        &MACHINE,
        &cells(current),
        history.iter().copied(),
        index,
        None,
    );
    assert_eq!(
        projected,
        project(
            &MACHINE,
            &cells(current),
            history.iter().copied(),
            index,
            None
        ),
        "keeping the settlement changes nothing of the read"
    );
    settled
        .remove("label")
        .expect("machines claim their labels")
}

/// That `predicted` is what the read of `current` in `history` shows, row for row: every live
/// row that claims a label reads as predicted, and no other does.
fn reads_as_predicted(
    predicted: &BTreeMap<String, Reading>,
    current: &[&Op],
    history: &[&Op],
    index: &Index,
) {
    let read = project(
        &MACHINE,
        &cells(current),
        history.iter().copied(),
        index,
        None,
    );
    let shown: BTreeMap<String, Option<String>> = read
        .rows
        .iter()
        .filter(|(_, row)| row.live() && row.fields.get("label").is_some_and(|f| !f.conflicted()))
        .map(|(pk, row)| {
            (
                pk.clone(),
                row.get("label").and_then(Value::as_str).map(str::to_owned),
            )
        })
        .collect();
    let said: BTreeMap<String, Option<String>> = predicted
        .iter()
        .map(|(pk, reading)| (pk.clone(), reading.reads.clone()))
        .collect();
    assert_eq!(said, shown, "the Write reads as its settlement said");
}

/// A label another row holds, staged in a row, reads once written as the clash label the read
/// settles it under, and the holder keeps it; two rows given one label nobody holds are settled
/// by the read's own tie-break; a holder that moves away leaves the label to the row that lost
/// it before, not to the one written later; a row given another row's clash label takes it and
/// moves that row on; a retire written with the label frees it; a restore re-claims the
/// restored row's label from the Write, after the row that took it meanwhile; a row born with a
/// held label loses it; and a row's own label staged again changes nothing. Each prediction is
/// the read itself once the Write is an operation of it.
/// NEGATIVE CONTROL: with `Settlement::written` beginning no tenure at the Write for a value
/// staged anew (its `entry.starts = …` for a changed value removed), the row born with a held
/// label is said to read as no label at all: `` called `Option::unwrap()` on a `None` value ``
/// at its `starts_with("m22-")``.
#[test]
fn a_settlement_says_what_a_write_will_read_as_by_the_reads_own_settling() {
    // A holder, and a row given its label.
    let mut index = Index::new(16);
    let a = operation(10, A, "label", "m22", &[], &mut index);
    let b = operation(20, B, "label", "m30", &[10], &mut index);
    let said = settlement(&[&a, &b], &[&a, &b], &index).written(&BTreeMap::from([label(B, "m22")]));
    let clash = said[B].reads.clone().unwrap();
    assert!(clash.starts_with("m22-"), "{clash}");
    assert_eq!(said[A].reads.as_deref(), Some("m22"), "the holder keeps it");
    assert_eq!(said[B].with, vec![A.to_owned()]);
    let w = operation(30, B, "label", "m22", &[10, 20], &mut index);
    reads_as_predicted(&said, &[&a, &w], &[&a, &b, &w], &index);

    // Two rows given one label nobody holds, in one Write.
    let mut index = Index::new(16);
    let c = operation(40, C, "label", "m40", &[], &mut index);
    let d = operation(41, D, "label", "m41", &[40], &mut index);
    let said = settlement(&[&c, &d], &[&c, &d], &index)
        .written(&BTreeMap::from([label(C, "m50"), label(D, "m50")]));
    assert_eq!(said[C].reads.as_deref(), Some("m50"));
    assert!(said[D].reads.as_deref().unwrap().starts_with("m50-"));
    let w = write(
        60,
        &[(C, "label", "m50"), (D, "label", "m50")],
        &[41],
        &mut index,
    );
    reads_as_predicted(&said, &[&w], &[&c, &d, &w], &index);

    // The holder moves away: the row that lost the label before keeps it, not the later one.
    let mut index = Index::new(16);
    let a = operation(10, A, "label", "m22", &[], &mut index);
    let b = operation(20, B, "label", "m22", &[10], &mut index);
    let c = operation(40, C, "label", "m40", &[20], &mut index);
    let said = settlement(&[&a, &b, &c], &[&a, &b, &c], &index)
        .written(&BTreeMap::from([label(A, "m30"), label(C, "m22")]));
    assert_eq!(said[B].reads.as_deref(), Some("m22"));
    assert!(said[C].reads.as_deref().unwrap().starts_with("m22-"));
    let w = write(
        70,
        &[(A, "label", "m30"), (C, "label", "m22")],
        &[40],
        &mut index,
    );
    reads_as_predicted(&said, &[&w, &b], &[&a, &b, &c, &w], &index);

    // A row given another row's clash label takes it, and moves that row on.
    let mut index = Index::new(16);
    let a = operation(10, A, "label", "m22", &[], &mut index);
    let b = operation(20, B, "label", "m22", &[10], &mut index);
    let c = operation(40, C, "label", "m40", &[20], &mut index);
    let held = settlement(&[&a, &b, &c], &[&a, &b, &c], &index);
    let taken = held.reads()[B].reads.clone().unwrap();
    let said = held.written(&BTreeMap::from([label(C, &taken)]));
    assert_eq!(said[C].reads.as_deref(), Some(taken.as_str()));
    assert_ne!(said[B].reads.as_deref(), Some(taken.as_str()));
    let w = operation(70, C, "label", &taken, &[40], &mut index);
    reads_as_predicted(&said, &[&a, &b, &w], &[&a, &b, &c, &w], &index);

    // A retire written with the label frees it.
    let mut index = Index::new(16);
    let a = operation(10, A, "label", "m22", &[], &mut index);
    let c = operation(40, C, "label", "m40", &[10], &mut index);
    let said = settlement(&[&a, &c], &[&a, &c], &index)
        .written(&BTreeMap::from([life(A, false), label(C, "m22")]));
    assert_eq!(said[C].reads.as_deref(), Some("m22"));
    assert!(!said.contains_key(A), "a retired row claims nothing");
    let w = write(
        80,
        &[(A, "row", "retired"), (C, "label", "m22")],
        &[40],
        &mut index,
    );
    reads_as_predicted(&said, &[&a, &w], &[&a, &c, &w], &index);

    // A restore re-claims the row's label from the Write, after the row that took it.
    let mut index = Index::new(16);
    let a = operation(10, A, "label", "m7", &[], &mut index);
    let retired = operation(50, A, "row", "retired", &[10], &mut index);
    let b = operation(20, B, "label", "m7", &[50], &mut index);
    let said = settlement(&[&a, &retired, &b], &[&a, &retired, &b], &index)
        .written(&BTreeMap::from([life(A, true)]));
    assert_eq!(said[B].reads.as_deref(), Some("m7"));
    assert!(said[A].reads.as_deref().unwrap().starts_with("m7-"));
    let w = operation(90, A, "row", "live", &[20], &mut index);
    reads_as_predicted(&said, &[&a, &b, &w], &[&a, &retired, &b, &w], &index);

    // A row born with a held label loses it.
    let mut index = Index::new(16);
    let a = operation(10, A, "label", "m22", &[], &mut index);
    let said = settlement(&[&a], &[&a], &index).written(&BTreeMap::from([(
        N.to_owned(),
        Staged {
            value: Some(json!("m22")),
            live: Some(true),
            record: 0,
        },
    )]));
    assert!(said[N].reads.as_deref().unwrap().starts_with("m22-"));
    let w = write(
        95,
        &[(N, "label", "m22"), (N, "row", "live")],
        &[10],
        &mut index,
    );
    reads_as_predicted(&said, &[&a, &w], &[&a, &w], &index);

    // A row's own label staged again changes nothing.
    let mut index = Index::new(16);
    let a = operation(10, A, "label", "m22", &[], &mut index);
    let b = operation(20, B, "label", "m22", &[10], &mut index);
    let held = settlement(&[&a, &b], &[&a, &b], &index);
    let said = held.written(&BTreeMap::from([label(B, "m22")]));
    assert_eq!(said, held.reads());
    let w = operation(30, B, "label", "m22", &[20], &mut index);
    reads_as_predicted(&said, &[&a, &w], &[&a, &b, &w], &index);
}

/// A Write the save appends as more than one record settles a label two of its rows are given
/// by those records' order, not by key: each record observes the one appended before it, so the
/// row an earlier record carries keeps the label though the row a later one carries has the
/// smaller key (a born row lands in the last, as the grid text holds it after every row it
/// shows); only rows one record carries tie by key.
/// NEGATIVE CONTROL: with `Settlement::written` beginning every staged claim at one record
/// (`Start::Written(0)` for `Start::Written(change.record)`), the earlier record's row is said to
/// lose it: `` left: Some("m50-2d71")  right: Some("m50") ``.
#[test]
fn a_write_of_several_records_settles_a_label_by_their_order() {
    let mut index = Index::new(16);
    let d = operation(41, D, "label", "m41", &[], &mut index);
    let held = settlement(&[&d], &[&d], &index);
    let staged = |born: usize| {
        BTreeMap::from([
            (
                D.to_owned(),
                Staged {
                    value: Some(json!("m50")),
                    ..Staged::default()
                },
            ),
            (
                A.to_owned(),
                Staged {
                    value: Some(json!("m50")),
                    live: Some(true),
                    record: born,
                },
            ),
        ])
    };
    // One record: the smaller key keeps it.
    let one = held.written(&staged(0));
    assert_eq!(one[A].reads.as_deref(), Some("m50"));
    // Two: the earlier record's row keeps it.
    let said = held.written(&staged(1));
    assert_eq!(said[D].reads.as_deref(), Some("m50"));
    assert!(said[A].reads.as_deref().unwrap().starts_with("m50-"));
    // The later record's digest is the smaller, so only its order settles it.
    let first = write(70, &[(D, "label", "m50")], &[41], &mut index);
    let second = write(
        65,
        &[(A, "label", "m50"), (A, "row", "live")],
        &[70],
        &mut index,
    );
    reads_as_predicted(&said, &[&first, &second], &[&d, &first, &second], &index);
}
