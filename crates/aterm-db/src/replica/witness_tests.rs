// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn tables(n: u8) -> BTreeSet<Digest> {
    (1..=n).map(|n| [n; 32]).collect()
}

/// Chunks round-trip through a list, beside tables and a clock mark, and only the tables whose
/// witnesses differ (or that a side could not read, or that no chunk covers) read as differing.
#[test]
fn chunks_carry_witnesses_and_name_only_the_tables_that_differ() {
    let salt = [9; 16];
    let common = tables(12);
    let contents: Vec<_> = common.iter().map(|table| Some(*table)).collect();
    let ours: Vec<_> = common
        .iter()
        .zip(&contents)
        .map(|(table, content)| witness(&salt, table, *content))
        .collect();
    let mut theirs = ours.clone();
    theirs[3] = witness(&salt, &[4; 32], Some([99; 32]));
    theirs[7] = UNKNOWN;
    let mut list = common.clone();
    list.insert(crate::clock::mark(1_000));
    list.insert(ask([1; 16]));
    list.extend(chunks(&theirs, usize::MAX));
    assert_eq!(list.len(), 12 + 1 + 1 + chunk_count(12));
    let (back, heard) = split(list);
    let (back, clock) = crate::clock::split(&back);
    assert_eq!(back, common, "asks, chunks and marks are never tables");
    assert_eq!(clock, crate::clock::Heard::One(1_000));
    assert_eq!(heard.asks, vec![[1; 16]]);
    assert_eq!(
        differing(&common, &ours, &heard),
        Some(BTreeSet::from([[4; 32], [8; 32]]))
    );
    // A chunk that did not fit leaves its tables differing.
    let (_, partial) = split(chunks(&ours, 1));
    assert_eq!(
        differing(&common, &ours, &partial),
        Some(common.iter().skip(PER_CHUNK).copied().collect())
    );
    // No chunk, or chunks for another set of tables, say nothing.
    assert_eq!(differing(&common, &ours, &Heard::default()), None);
    let (_, other) = split(chunks(&ours[..4], usize::MAX));
    assert_eq!(differing(&common, &ours, &other), None);
    // Under another salt equal content still matches, and other content differs.
    assert_eq!(
        witness(&[2; 16], &[1; 32], Some([5; 32])),
        witness(&[2; 16], &[1; 32], Some([5; 32]))
    );
    assert_ne!(
        witness(&[2; 16], &[1; 32], Some([5; 32])),
        witness(&[2; 16], &[1; 32], Some([6; 32]))
    );
    assert_eq!(witness(&salt, &[1; 32], None), UNKNOWN);
}
