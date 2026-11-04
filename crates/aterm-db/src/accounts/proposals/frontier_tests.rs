// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline writers can exceed one envelope's parent bound without losing edits.

use super::*;
use crate::table::tests::frontier::{receive, wide_lab, wires};
use astream_wire::sync::MAX_PARENTS;
use aterm_json::json;
use std::collections::BTreeSet;

fn change(key: &str, plan: &str) -> Change {
    Change {
        collection: "accounts".into(),
        key: key.into(),
        value: Some(
            json!({"provider":"google", "method":"email-link", "plan":plan,
            "email":"frontier@example.test", "auth":[{"kind":"email-link",
            "mailbox":"inbox", "requirements":["email-access"]}]}),
        ),
    }
}

#[test]
fn more_than_one_parent_window_of_offline_writers_preserves_values_and_retries() {
    for covered_predecessor in [false, true] {
        wide_frontier(covered_predecessor);
    }
}

fn wide_frontier(covered_predecessor: bool) {
    let writers = MAX_PARENTS + 1;
    let lab = wide_lab(writers);
    let db = Proposals::at(lab.paths[0].clone());
    let scope = db.initialize().unwrap();
    let mut original = BTreeSet::new();
    for (writer, paths) in lab.paths[..writers].iter().enumerate() {
        let peer = Proposals::at(paths.clone());
        assert_eq!(peer.initialize().unwrap(), scope);
        let receipt = peer
            .write([1; 16], vec![change("one", &format!("offline-{writer}"))])
            .unwrap();
        original.insert(receipt.id);
    }
    if covered_predecessor {
        receive(&lab, 1, scope, &wires(&lab, 0, scope));
        Proposals::at(lab.paths[1].clone())
            .write([3; 16], vec![change("bridge", "covers-writer-zero")])
            .unwrap();
    }
    let incoming: Vec<_> = (1..writers)
        .flat_map(|writer| wires(&lab, writer, scope))
        .collect();
    receive(&lab, 0, scope, &incoming);
    let before = db.snapshot().unwrap();
    assert!(before.history_complete());
    assert_eq!(
        before.operations.len(),
        writers + usize::from(covered_predecessor)
    );
    assert_eq!(
        before.cells[&("accounts".into(), "one".into())]
            .candidates
            .len(),
        writers
    );
    let before_ids: BTreeSet<_> = before.operations.iter().map(|op| op.header.id).collect();
    let mut heads = before_ids.clone();
    for op in &before.operations {
        for parent in &op.header.parents {
            heads.remove(parent);
        }
    }
    assert_eq!(heads.len(), writers - usize::from(covered_predecessor));

    let edit = vec![change("after-merge", "still-writable")];
    let receipt = db.write([2; 16], edit.clone()).unwrap();
    let after = db.snapshot().unwrap();
    assert!(after.history_complete());
    assert_eq!(
        after.cells[&("accounts".into(), "one".into())],
        before.cells[&("accounts".into(), "one".into())],
        "joining causal history must leave every competing value intact"
    );
    let known: BTreeSet<_> = after.operations.iter().map(|op| op.header.id).collect();
    assert!(original.is_subset(&known));
    assert!(known.contains(&receipt.id));
    assert!(
        after
            .operations
            .iter()
            .all(|op| op.header.parents.len() <= MAX_PARENTS)
    );
    assert_eq!(db.write([2; 16], edit).unwrap(), receipt);
    assert_eq!(
        db.snapshot().unwrap().operations.len(),
        after.operations.len()
    );
    let merges: Vec<_> = known
        .difference(&before_ids)
        .filter(|id| **id != receipt.id)
        .copied()
        .collect();
    assert!(!merges.is_empty());
    for id in merges {
        assert!(
            db.revert([4; 16], id).is_err(),
            "a causal join has no value changes to undo"
        );
    }
    assert_eq!(
        db.snapshot().unwrap().operations.len(),
        after.operations.len()
    );

    receive(&lab, 1, scope, &wires(&lab, 0, scope));
    let peer = Proposals::at(lab.paths[1].clone()).snapshot().unwrap();
    assert!(peer.history_complete());
    assert_eq!(peer.cells, after.cells);
    assert_eq!(
        peer.operations
            .iter()
            .map(|op| op.header.id)
            .collect::<BTreeSet<_>>(),
        known
    );
}
