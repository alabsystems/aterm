// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The index filter tracks a dictionary, not occurrences or screen refreshes.

use super::*;

#[test]
fn repeated_output_and_unchanged_refreshes_do_not_grow_the_filter() {
    let text = "Compiling agent_worker: 日本語 ÉCHO e\u{301} ".repeat(32);
    let mut index = SearchIndex::with_capacity(100_000);
    index.index_line(0, &text);
    let inserted = index.bloom.item_count();
    let bits = index.bloom.num_bits();
    assert_eq!(inserted, index.trigrams.len());
    assert!(inserted > 10, "exercise real original and folded keys");

    for row in 1..2048 {
        index.index_line(row, &text);
    }
    for _ in 0..100 {
        for row in 2000..2048 {
            index.index_line(row, &text);
        }
    }
    assert_eq!(index.bloom.item_count(), inserted);
    assert_eq!(index.bloom.num_bits(), bits);
    assert!(bits <= 10_048, "row capacity cannot size the dictionary");
    let previous_insert_calls = (text.len() - 2 + lower_fold(&text).len() - 2) * (2048 + 100 * 48);
    eprintln!(
        "repeated_bloom: dictionary inserts={inserted}, previous occurrence inserts \
         (excluding rebuilds)={previous_insert_calls}, filter bytes={}",
        bits / 8
    );
    assert_eq!(index.search("agent_worker").count(), 2048);
    assert_eq!(
        index
            .search_case_insensitive("écho", SearchDirection::Forward)
            .len(),
        2048 * 32
    );

    index.rebuild_bloom();
    assert_eq!(index.bloom.item_count(), inserted);
    assert_eq!(index.bloom.num_bits(), bits);
}

#[test]
fn dictionary_growth_rebuilds_without_losing_live_trigrams() {
    let mut index = SearchIndex::new();
    let initial_bits = index.bloom.num_bits();
    // Unique three-byte words grow the dictionary enough to cross the initial
    // saturation threshold, unlike repeated lines with many occurrences.
    for row in 0..4096 {
        let word = [
            b'!' + (row / 256) as u8,
            b'!' + ((row / 16) % 16) as u8,
            b'!' + (row % 16) as u8,
        ];
        index.index_line(row, std::str::from_utf8(&word).unwrap());
    }
    assert!(index.bloom.num_bits() > initial_bits);
    assert!(!index.bloom.is_saturated());
    assert_eq!(index.bloom.item_count(), index.trigrams.len());
    for trigram in index.trigrams.keys() {
        assert!(index.bloom.might_contain_bytes(trigram));
    }

    // Eviction and edits leave stale bits, but rebuilding must use only the
    // live dictionary and cannot discard a replacement's Unicode fold.
    index.drop_history_below(4080);
    index.index_line(4095, "ÉCHO İSTANBUL 日本語");
    index.rebuild_bloom();
    assert_eq!(index.bloom.item_count(), index.trigrams.len());
    assert!(index.bloom.num_bits() <= 10_048);
    for trigram in index.trigrams.keys() {
        assert!(index.bloom.might_contain_bytes(trigram));
    }
    assert_eq!(
        index
            .search_case_insensitive("écho", SearchDirection::Forward)
            .len(),
        1
    );
    assert_eq!(
        index
            .search_case_insensitive("i\u{307}stanbul", SearchDirection::Forward)
            .len(),
        1
    );
    assert_eq!(index.search_with_positions("日本語").len(), 1);
}

#[test]
fn replacing_last_posting_and_reintroducing_it_preserves_all_search_modes() {
    let mut index = SearchIndex::new();
    let mut rows = [
        "shared ASCII NEEDLE",
        "shared ÉCHO 日本語",
        "shared İSTANBUL",
    ];
    for (row, text) in rows.iter().enumerate() {
        index.index_line(row, text);
    }
    for (row, text) in [
        (0, "shared ÉCHO 日本語"),
        (1, "replacement without the old keys"),
        (0, "shared ASCII NEEDLE"),
        (2, "shared ÉCHO 日本語"),
        (2, "shared İSTANBUL"),
    ] {
        index.index_line(row, text);
        rows[row] = text;
        index.rebuild_bloom();
        let mut fresh = SearchIndex::new();
        for (row, text) in rows.iter().enumerate() {
            fresh.index_line(row, text);
        }
        for (query, sensitive, regex) in [
            ("NEEDLE", true, false),
            ("écho", false, false),
            ("i\u{307}stanbul", false, false),
            ("日本語", true, false),
            ("shared .+", true, true),
            ("écho|needle", false, true),
        ] {
            assert_eq!(
                index.search_results_opts(query, sensitive, regex).unwrap(),
                fresh.search_results_opts(query, sensitive, regex).unwrap(),
                "row={row} query={query:?} sensitive={sensitive} regex={regex}"
            );
        }
    }
}
