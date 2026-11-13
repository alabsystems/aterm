// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::typed::cell::{Candidate, CellValue, Field, Projection, Row};
use aterm_json::json;

fn field(value: &str) -> Field {
    Field::Value(Candidate {
        operation: [1; 32],
        value: CellValue::owner(2, json!(value)),
    })
}

fn registry() -> Projection {
    let mut rows = Projection::default();
    rows.rows.insert(
        "mc-0000000000000007".into(),
        Row {
            fields: [
                ("label".into(), field("m7")),
                ("node".into(), field("0123456789abcdef")),
            ]
            .into(),
            ..Row::default()
        },
    );
    rows
}

fn seat() -> LabelSeat {
    LabelSeat {
        actor: [7; 48],
        node: "0123456789abcdef".into(),
        name: "log-name-seven".into(),
        mac: Some("mc-0000000000000007".into()),
        transport: "00112233445566778899aabbccddeeff".into(),
    }
}

#[test]
fn label_follows_registry_and_uses_book_bound_old_transport() {
    let mut registry = registry();
    assert_eq!(
        resolve_registry_label("m7", &registry, &[seat()]).unwrap(),
        "00112233445566778899aabbccddeeff"
    );
    let mut seat = seat();
    registry
        .rows
        .values_mut()
        .next()
        .unwrap()
        .fields
        .insert("label".into(), field("m22"));
    seat.transport = "n-0123456789abcdef".into();
    assert_eq!(
        resolve_registry_label("m22", &registry, &[seat]).unwrap(),
        "n-0123456789abcdef"
    );
    assert!(resolve_registry_label("m7", &registry, &[]).is_err());
}

#[test]
fn dispute_prints_both_registry_node_and_signed_seat_name() {
    let registry = registry();
    let mut seat = seat();
    seat.mac = Some("mc-0000000000000008".into());
    let why = resolve_registry_label("m7", &registry, &[seat]).unwrap_err();
    for evidence in [
        "0123456789abcdef",
        "log-name-seven",
        "mc-0000000000000007",
        "mc-0000000000000008",
    ] {
        assert!(why.contains(evidence), "missing evidence {evidence}: {why}");
    }
}

#[test]
fn conflicted_label_and_duplicate_adoption_refuse() {
    let mut registry = registry();
    assert!(resolve_registry_label("m7", &registry, &[seat(), seat()]).is_err());
    let mut collision = seat();
    collision.actor = [8; 48];
    collision.mac = Some("mc-0000000000000008".into());
    assert!(resolve_registry_label("m7", &registry, &[seat(), collision]).is_err());
    registry.rows.values_mut().next().unwrap().fields.insert(
        "label".into(),
        Field::Conflict {
            candidates: ["m7", "m8"]
                .into_iter()
                .map(|value| Candidate {
                    operation: [1; 32],
                    value: CellValue::owner(2, json!(value)),
                })
                .collect(),
            provisional: Some(json!("m7")),
        },
    );
    assert!(resolve_registry_label("m7", &registry, &[seat()]).is_err());
}

#[test]
fn raw_node_diagnostics_never_print_untyped_values() {
    let mut registry = registry();
    let row = registry.rows.values_mut().next().unwrap();
    row.fields
        .insert("node".into(), field("untyped-private-fixture"));
    row.raw.insert("node".into());
    let why = resolve_registry_label("m7", &registry, &[seat()]).unwrap_err();
    assert!(!why.contains("untyped-private-fixture"));
    assert!(why.contains("log-name-seven"));
}

#[test]
fn typed_label_words_are_canonical_before_lookup() {
    let label = registry_label("M7-3FA2").expect("a typed clash label");
    let mut registry = registry();
    registry
        .rows
        .values_mut()
        .next()
        .unwrap()
        .fields
        .insert("label".into(), field(&label));
    assert_eq!(
        resolve_registry_label(&label, &registry, &[seat()]).unwrap(),
        "00112233445566778899aabbccddeeff"
    );
    assert!(registry_label("m7.local").is_none());
}
