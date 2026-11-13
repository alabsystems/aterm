// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::typed::{
    Column, Derive, Writers,
    secret::{Cell, MASK, Secret},
};

const COLLECTION: Collection = Collection {
    name: "mailbox",
    version: 2,
    kind: Kind::Cells,
    writers: Writers::Owner,
    prefix: "mb",
    derive: Derive::Random,
    columns: &[
        Column::new("row", Ty::Enum(&["live"])),
        Column::new("password", Ty::Secret),
    ],
    unique: &[],
    max_row: 1024,
};

#[test]
fn secret_get_scan_export_history_and_unknown_reader() {
    let password = "synthetic-display-password";
    let token = Secret::new(password.into())
        .seal(
            &[3; 32],
            &[4; 32],
            Cell {
                table: "alab/registry",
                collection: "mailbox",
                row: "mb-fixture",
                column: "password",
            },
        )
        .unwrap();
    let change = crate::typed::cell::change(
        &COLLECTION,
        CellKey {
            pk: "mb-fixture",
            col: "password",
            elem: None,
        },
        &CellValue::owner(2, Value::String(token.clone())),
    )
    .unwrap();
    let cells = std::collections::BTreeMap::from([(
        (change.collection.clone(), change.key.clone()),
        crate::database::Cell {
            stopped: false,
            candidates: vec![crate::database::Candidate {
                operation: [1; 32],
                value: change.value.clone().unwrap(),
            }],
        },
    )]);
    let view = View {
        ops: vec![],
        quota: Default::default(),
        cells,
        owned: Default::default(),
        complete: true,
        refused: vec![],
    };
    const OLD: Collection = Collection {
        columns: &[Column::new("row", Ty::Enum(&["live"]))],
        ..COLLECTION
    };
    let old_row = view.project(&OLD).rows.remove("mb-fixture").unwrap();
    assert!(old_row.raw.contains("password"));
    assert_eq!(
        old_row.get("password").and_then(Value::as_str),
        Some(token.as_str())
    );
    for reveal in [false, true] {
        for value in [
            get(&view, &COLLECTION, "mb-fixture", reveal).unwrap(),
            scan(&view, &COLLECTION, &[], reveal).unwrap(),
        ] {
            let text = aterm_json::to_string(&value).unwrap();
            assert!(text.contains(MASK));
            assert!(!text.contains(password));
            assert!(!text.contains(&token));
        }
        assert_eq!(
            grid::show(&Ty::Secret, &Value::String(token.clone()), reveal),
            MASK
        );
        // Unknown-column history uses this recursive renderer on the stored cell.
        let history = grid::raw_masked(change.value.as_ref().unwrap(), reveal);
        assert!(aterm_json::to_string(&history).unwrap().contains(MASK));
    }
}
