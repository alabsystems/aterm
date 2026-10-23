// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for `ScrollbackStorage`.

use super::*;

#[test]
fn get_line_returns_borrowed_for_hot_tier() {
    use std::borrow::Cow;

    let mut sb = Scrollback::with_block_size(4, 10, 10_000_000, 2);

    for i in 0..6 {
        sb.push_str(&format!("line-{i}"));
    }
    assert_eq!(sb.line_count(), 6);
    assert_eq!(sb.hot_line_count(), 4, "4 lines in hot tier");
    assert_eq!(sb.warm_line_count(), 2, "2 lines promoted to warm");

    for idx in 0..2 {
        let cow = sb.get_line(idx).unwrap().expect("warm line present");
        assert!(
            matches!(cow, Cow::Owned(_)),
            "warm-tier line {idx} should be Cow::Owned, got Borrowed"
        );
        assert_eq!(cow.to_string(), format!("line-{idx}"));
    }

    for idx in 2..6 {
        let cow = sb.get_line(idx).unwrap().expect("hot line present");
        assert!(
            matches!(cow, Cow::Borrowed(_)),
            "hot-tier line {idx} should be Cow::Borrowed, got Owned"
        );
        assert_eq!(cow.to_string(), format!("line-{idx}"));
    }

    let newest = sb.get_line_rev(0).unwrap().expect("newest line present");
    assert!(
        matches!(newest, Cow::Borrowed(_)),
        "newest line (hot tier) should be Cow::Borrowed via get_line_rev"
    );
    assert_eq!(newest.to_string(), "line-5");
}

fn streaming_parity_line(i: u16) -> Line {
    use std::sync::Arc;

    let text = format!("line-{i:03}: 日本語 e\u{0301} — linked output");
    let mut attrs = Rle::new();
    for (col, _) in text.chars().enumerate() {
        attrs.push(if col < 4 {
            CellAttrs::new(0x01_11_22_33, CellAttrs::DEFAULT.bg, 0)
        } else {
            CellAttrs::DEFAULT
        });
    }
    let mut line = Line::with_hyperlinks_owned(
        text,
        attrs,
        vec![HyperlinkSpan::with_id(
            0,
            4,
            Arc::from("https://example.test/output"),
            Some(Arc::from(format!("line-{i}"))),
        )],
    );
    line.set_underline_colors(vec![UnderlineColorSpan::new(0, 4, 0x02_00_00_05)]);
    line.set_wrapped(i % 3 == 1);
    line
}

#[test]
fn streaming_iter_matches_get_line_walk_memory() {
    let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
    let mut original = Vec::new();
    for i in 0..60 {
        let line = streaming_parity_line(i);
        original.push(line.serialize());
        sb.push_line(line);
    }
    assert_eq!(
        sb.iter().map(|l| l.serialize()).collect::<Vec<_>>(),
        original
    );
    sb.truncate(53).unwrap();
    assert!(
        sb.cold_line_count() > 0,
        "oracle needs a populated cold tier"
    );
    assert!(
        sb.warm_line_count() > 0,
        "oracle needs a populated warm tier"
    );
    assert!(sb.hot_line_count() > 0, "oracle needs a populated hot tier");

    let via_get: Vec<_> = (0..sb.line_count())
        .map(|i| {
            sb.get_line(i)
                .expect("no error")
                .expect("line present")
                .serialize()
        })
        .collect();
    let iter = sb.iter();
    let via_iter: Vec<_> = iter.map(|l| l.serialize()).collect();
    assert_eq!(via_iter, via_get, "streaming iter must match get_line walk");
    assert_eq!(via_iter, original[7..], "trim must keep the exact suffix");

    let mut storage: ScrollbackStorage = sb.into();
    let via_storage: Vec<_> = storage.iter().map(|l| l.serialize()).collect();
    assert_eq!(via_storage, via_get);
    storage.set_line_limit(Some(13));
    assert_eq!(storage.cold_line_count(), 0);
    let via_storage: Vec<_> = storage.iter().map(|l| l.serialize()).collect();
    assert_eq!(via_storage, original[47..]);
}

#[test]
fn streaming_iter_skips_corrupt_block_with_same_totals() {
    let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
    for i in 0..24 {
        sb.push_str(&format!("line-{i:02}"));
    }
    let corrupt_lines = 5;
    sb.inject_corrupted_warm_block(corrupt_lines);
    let total = sb.line_count();

    let mut iter = sb.iter();
    let yielded = iter.by_ref().count();
    assert_eq!(
        yielded,
        total - corrupt_lines,
        "all healthy lines must still be yielded"
    );
    assert_eq!(
        iter.skipped_lines(),
        corrupt_lines,
        "skip total must equal the corrupt block's line count"
    );
}

fn dense_oracle(storage: &ScrollbackStorage, start: usize) -> Vec<Option<Vec<u8>>> {
    (start..storage.line_count())
        .map(|i| storage.get_line(i).ok().flatten().map(|l| l.serialize()))
        .collect()
}

fn dense_walk(storage: &ScrollbackStorage, start: usize) -> Vec<Option<Vec<u8>>> {
    let iter = storage.dense_from(start);
    assert_eq!(
        iter.len(),
        storage.line_count().saturating_sub(start),
        "the dense walk announces one item per remaining line"
    );
    iter.map(|l| l.map(|l| l.serialize())).collect()
}

#[test]
fn dense_walk_matches_per_line_oracle_across_tiers_and_front_truncation() {
    let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
    for i in 0..60 {
        sb.push_line(streaming_parity_line(i));
    }
    let mut storage: ScrollbackStorage = sb.into();
    for keep in [60, 53, 13] {
        storage.set_line_limit(Some(keep));
        assert_eq!(storage.line_count(), keep);
        for start in 0..=keep + 2 {
            assert_eq!(
                dense_walk(&storage, start),
                dense_oracle(&storage, start),
                "keep={keep} start={start}"
            );
        }
        let mut iter = storage.dense_from(0);
        assert!(iter.by_ref().all(|l| l.is_some()));
        assert_eq!(iter.placeholders(), 0);
    }
}

#[test]
fn dense_walk_borrows_hot_lines() {
    let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
    for i in 0..30 {
        sb.push_line(streaming_parity_line(i));
    }
    let hot = sb.hot_line_count();
    assert!(hot > 0);
    let storage: ScrollbackStorage = sb.into();
    let n = storage.line_count();
    let items: Vec<_> = storage.dense_from(0).collect();
    for (i, item) in items.iter().enumerate() {
        let borrowed = matches!(item, Some(std::borrow::Cow::Borrowed(_)));
        assert_eq!(
            borrowed,
            i >= n - hot,
            "line {i}: hot lines borrow, decoded lines own"
        );
    }
}

#[test]
fn dense_walk_yields_placeholders_for_a_corrupt_segment_and_keeps_coordinates() {
    let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
    for i in 0..40 {
        sb.push_line(streaming_parity_line(i));
    }
    let cold_before = sb.cold_line_count();
    assert!(
        cold_before > 0,
        "the corrupt block must sit after a cold tier"
    );
    let corrupt_lines = 5;
    sb.inject_corrupted_warm_block(corrupt_lines);
    let storage: ScrollbackStorage = sb.into();
    let total = storage.line_count();

    for start in 0..=total {
        let dense = dense_walk(&storage, start);
        assert_eq!(dense, dense_oracle(&storage, start), "start={start}");
    }
    let mut iter = storage.dense_from(0);
    let items: Vec<_> = iter.by_ref().collect();
    assert_eq!(iter.placeholders(), corrupt_lines);
    let holes: Vec<usize> = (0..total).filter(|&i| items[i].is_none()).collect();
    assert_eq!(
        holes,
        (cold_before..cold_before + corrupt_lines).collect::<Vec<_>>(),
        "the placeholders sit exactly on the corrupt block's rows"
    );

    let skipping: Vec<_> = storage.iter().map(|l| Some(l.serialize())).collect();
    let oracle = dense_oracle(&storage, 0);
    assert_eq!(skipping.len(), total - corrupt_lines);
    assert_ne!(
        skipping[cold_before], oracle[cold_before],
        "a skipping walk shifts the first line after the corrupt block onto its row"
    );
}
