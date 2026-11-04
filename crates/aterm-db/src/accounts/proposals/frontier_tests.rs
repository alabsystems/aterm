// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline writers can exceed one envelope's parent bound without losing edits.

use super::*;
use crate::table::tests::{
    Lab,
    frontier::{first_records, receive, wide_lab, wires},
};
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

/// 65 offline writers exceed one parent window: a later write adds a bounded merge whether or not
/// the writer's own predecessor is covered, values and history stay complete, a retry appends
/// nothing, a merge cannot be reverted and a replica converges. Mac 2 and replica Mac 3 hold the
/// uncovered case (65 heads), Mac 0 and replica Mac 1 the covered one (64 heads, and a predecessor
/// the bridge covers, so it is not itself a head and would add a 65th parent); neither pair hears
/// what the other does after the offline writes. Macs 0 to 3 and the last, past
/// `LEGACY_MAX_MEMBERS`, write through the product; the others seal what a first write appends, so
/// they run neither `initialize` nor `Proposals::write` here.
/// NEGATIVE CONTROLS: with the store's fit test counting heads alone, the covered case's write
/// fails `table frontier requires a bounded merge`; with `Proposals::write` calling
/// `commit_catalog`, the uncovered case's write at Mac 2 fails the same way.
#[test]
fn more_than_one_parent_window_of_offline_writers_preserves_values_and_retries() {
    let writers = MAX_PARENTS + 1;
    let lab = wide_lab(writers);
    let scope = Proposals::at(lab.paths[0].clone()).initialize().unwrap();
    let last = writers - 1;
    let (mut original, mut originals) = (BTreeSet::new(), Vec::new());
    for writer in [0, 1, 2, 3, last] {
        let peer = Proposals::at(lab.paths[writer].clone());
        assert_eq!(peer.initialize().unwrap(), scope);
        let receipt = peer
            .write([1; 16], vec![change("one", &format!("offline-{writer}"))])
            .unwrap();
        original.insert(receipt.id);
        originals.extend(wires(&lab, writer, scope));
    }
    originals.extend(first_records(&lab, 4..last, scope, |writer, _| {
        encode(&Command::Write {
            request: [1; 16],
            changes: vec![change("one", &format!("offline-{writer}"))],
        })
        .unwrap()
    }));
    receive(&lab, 1, scope, &wires(&lab, 0, scope));
    Proposals::at(lab.paths[1].clone())
        .write([3; 16], vec![change("bridge", "covers-writer-zero")])
        .unwrap();
    let bridged = wires(&lab, 1, scope);
    wide_frontier(&lab, scope, (2, 3), false, &originals, &original);
    originals.extend(bridged);
    wide_frontier(&lab, scope, (0, 1), true, &originals, &original);
}

fn wide_frontier(
    lab: &Lab,
    scope: Scope,
    (observer, replica): (usize, usize),
    covered_predecessor: bool,
    incoming: &[Vec<u8>],
    original: &BTreeSet<Digest>,
) {
    let writers = lab.paths.len();
    let db = Proposals::at(lab.paths[observer].clone());
    receive(lab, observer, scope, incoming);
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
    assert!(before_ids.is_subset(&known));
    assert!(original.is_subset(&before_ids));
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

    receive(lab, replica, scope, &wires(lab, observer, scope));
    let peer = Proposals::at(lab.paths[replica].clone())
        .snapshot()
        .unwrap();
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
