// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;
use std::slice::from_ref;

fn typed(operation: u8, x: Value) -> Candidate {
    Candidate {
        operation: [operation; 32],
        value: CellValue::owner(1, x).encode(),
    }
}

/// A cell's difference is read as the grid reads the cell: a clear beside a value is a side of a
/// conflict, so a concurrent clear arriving, or a conflict with a clear resolved, is
/// conflict-changed; only a clear alone retires a value; a revert back to absence (a stored
/// null) holds nothing; and a set's member taken out holds nothing beside one put in (add-wins).
/// NEGATIVE CONTROL: with `values` dropping every `x: null` (a clear read as absence wherever it
/// stands), the first assertion fails: `` left: None right: Some(ConflictChanged) ``.
#[test]
fn a_clear_beside_a_value_is_a_conflict_and_alone_retires_it() {
    let one = typed(1, json!(1));
    let cleared = typed(2, Value::Null);
    let two = typed(3, json!(2));
    let resolved = typed(4, json!(1));
    let reverted = Candidate {
        operation: [5; 32],
        value: Value::Null,
    };
    assert_eq!(
        delta(false, from_ref(&one), &[one.clone(), cleared.clone()]),
        Some(Delta::ConflictChanged),
        "a concurrent clear arrives"
    );
    assert_eq!(
        delta(false, &[one.clone(), cleared.clone()], from_ref(&resolved)),
        Some(Delta::ConflictChanged),
        "a conflict with a clear is resolved"
    );
    assert_eq!(
        delta(false, from_ref(&one), &[cleared.clone(), two]),
        Some(Delta::ConflictChanged)
    );
    assert_eq!(
        delta(false, from_ref(&one), from_ref(&cleared)),
        Some(Delta::Retired)
    );
    assert_eq!(
        delta(false, from_ref(&one), from_ref(&reverted)),
        Some(Delta::Retired)
    );
    assert_eq!(delta(false, &[], from_ref(&one)), Some(Delta::Added));
    assert_eq!(delta(false, from_ref(&one), from_ref(&one)), None);
    let put_in = typed(6, Value::Bool(true));
    let taken_out = typed(7, Value::Bool(false));
    assert_eq!(
        delta(
            true,
            from_ref(&put_in),
            &[put_in.clone(), taken_out.clone()]
        ),
        None,
        "add-wins: the member stays in"
    );
    assert_eq!(
        delta(true, from_ref(&put_in), from_ref(&taken_out)),
        Some(Delta::Retired)
    );
}
