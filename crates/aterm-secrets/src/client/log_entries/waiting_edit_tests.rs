// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `[everyone]`'s `name` = `value`.
fn entry(name: &str, value: &[u8]) -> Entry {
    Entry {
        name: name.to_owned(),
        value: SecretBytes::from(value.to_vec()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    }
}

/// The one write `held` waits with, of `shared`.
fn held(write: WaitingWrite) -> Waiting {
    Waiting(BTreeMap::from([("shared".to_owned(), write)]))
}

/// A save that leaves a waiting write as the view showed it, editing another name, keeps what
/// that write meant. A settle of a conflict stays one: rewritten with its `resolve`, never
/// taken back as a plain write the log holds already. A write over the text's value, where
/// this Mac opens none of the newest epochs and reads that text as the log's, keeps the bases
/// of the write it replaces, so it still stands over the log's value the earlier write did.
///
/// NEGATIVE CONTROL: `everyone_edit` without the `kept` write's `resolve`, then without
/// `bases.extend` of each replaced write's bases (each cut by hand and restored with cp from a
/// backup plus touch, confirmed with cmp): the test fails "a left settle of a conflict stays
/// one", then "a write over the text's value stands over the log's value its earlier write
/// did".
#[test]
fn a_left_waiting_write_keeps_its_settle_and_its_bases() {
    let other = entry("other", b"x");
    let a = entry("shared", b"a");
    let settle = held(WaitingWrite {
        to: Some(a.clone()),
        bases: BTreeSet::from([Some(entry_digest(&a))]),
        resolve: true,
    });
    let log = BTreeMap::from([("shared".to_owned(), a.clone())]);
    let doc = BTreeMap::from([
        ("shared".to_owned(), a.clone()),
        ("other".to_owned(), other.clone()),
    ]);
    let edit = everyone_edit(
        Some(&log),
        Some(&settle),
        &settle,
        &BTreeMap::new(),
        Some(&doc),
        |_| false,
    );
    let write = edit
        .writes
        .0
        .get("shared")
        .expect("CONTROL: it is rewritten");
    assert!(write.resolve, "a left settle of a conflict stays one");
    assert!(
        write.stands_over(Some(&a)) && !write.done(Some(&a), true),
        "it stands, not done while the conflict does"
    );
    assert!(
        edit.shows.contains(&"shared".to_owned()),
        "it is named as waiting"
    );

    let (v0, v1) = (entry("shared", b"v0"), entry("shared", b"v1"));
    let earlier = held(WaitingWrite {
        to: Some(v1.clone()),
        bases: BTreeSet::from([Some(entry_digest(&v0))]),
        resolve: false,
    });
    let text = BTreeMap::from([("shared".to_owned(), v1.clone())]);
    let doc = BTreeMap::from([
        ("shared".to_owned(), v1.clone()),
        ("other".to_owned(), other),
    ]);
    let edit = everyone_edit(
        Some(&text),
        Some(&earlier),
        &earlier,
        &BTreeMap::new(),
        Some(&doc),
        |_| false,
    );
    let write = edit
        .writes
        .0
        .get("shared")
        .expect("CONTROL: it is rewritten");
    assert!(
        write.stands_over(Some(&v0)),
        "a write over the text's value stands over the log's value its earlier write did"
    );
}
