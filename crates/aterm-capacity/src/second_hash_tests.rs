// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's record of second hashes ([`super`]): one row per pair of units, whichever of the
//! two a run built; the newest [`ROWS`] kept; the record kept in its file across runs.

use super::{ROWS, Row, SecondHashes};

fn row(at_ms: u64, kind: &str, hash: &str, other: &str, input: &str) -> Row {
    Row {
        at_ms,
        package: "aterm-grid".to_owned(),
        kind: kind.to_owned(),
        hash: hash.to_owned(),
        other: other.to_owned(),
        cause: "aterm-json".to_owned(),
        input: input.to_owned(),
        detail: "\"[]\" -> \"[\\\"std\\\"]\"".to_owned(),
    }
}

/// A PAIR OF UNITS IS ONE ROW ([`SecondHashes::push`], [`Row::same`]): built again, or the other
/// of the two built beside the first, it adds nothing, in memory and in the file across two
/// runs ([`SecondHashes::keep`]). NEGATIVE CONTROLS: a pair of another kind, and another pair
/// that shares one unit, are rows of their own.
#[test]
fn a_pair_of_units_is_one_row_whichever_was_built() {
    let mut record = SecondHashes::default();
    assert!(record.push(row(1, "lib", "a2", "a1", "features")));
    assert!(!record.push(row(2, "lib", "a2", "a1", "features")), "again");
    assert!(
        !record.push(row(3, "lib", "a1", "a2", "features")),
        "the other of the two built beside the first"
    );
    assert!(
        record.push(row(4, "test-lib", "a2", "a1", "features")),
        "another kind"
    );
    assert!(
        record.push(row(5, "lib", "a3", "a1", "deps")),
        "another pair"
    );
    assert_eq!(record.rows().len(), 3);
    assert!(
        record.listing().starts_with("second hashes: 3 "),
        "{}",
        record.listing()
    );

    let dir = aterm_tempfile::Builder::new()
        .prefix("second-hash-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let file = dir.path().join("verify/second-hashes.json");
    assert_eq!(
        SecondHashes::keep(&file, vec![row(1, "lib", "a2", "a1", "features")]).expect("kept"),
        1
    );
    assert_eq!(
        SecondHashes::keep(&file, vec![row(2, "lib", "a1", "a2", "features")]).expect("kept"),
        0,
        "a later run that built the other of the pair"
    );
    assert_eq!(
        SecondHashes::keep(&file, vec![row(3, "lib", "a3", "a2", "deps")]).expect("kept"),
        1
    );
    assert_eq!(SecondHashes::load(&file).rows().len(), 2);
}

/// THE RECORD KEEPS THE NEWEST [`ROWS`] ([`SecondHashes::push`]): one past them drops the oldest,
/// whatever order they came in. NEGATIVE CONTROL: at [`ROWS`] nothing goes.
#[test]
fn the_record_keeps_the_newest_rows() {
    let rows = u64::try_from(ROWS).expect("the bound fits");
    let mut record = SecondHashes::default();
    for n in 1..=rows {
        assert!(record.push(row(n, "lib", &format!("h{n}"), "h0", "features")));
    }
    assert_eq!(record.rows().len(), ROWS);
    assert_eq!(record.rows()[0].at_ms, 1, "at the bound nothing goes");
    // Past it, the oldest goes, though the newest came first.
    assert!(record.push(row(rows + 2, "lib", "late", "h0", "deps")));
    assert!(record.push(row(rows + 1, "lib", "early", "h0", "deps")));
    assert_eq!(record.rows().len(), ROWS);
    assert_eq!(
        (record.rows()[0].at_ms, record.rows()[ROWS - 1].at_ms),
        (3, rows + 2)
    );
    assert_eq!(
        SecondHashes::decode(&record.encode()),
        record,
        "the file holds what memory does"
    );
}
