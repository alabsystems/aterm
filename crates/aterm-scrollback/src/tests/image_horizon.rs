// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Image retention horizon tests across hot and compressed scrollback tiers.

use std::sync::Arc;

use super::*;
use crate::ImageSpan;

fn placement(rows: u16, cols: u16, payload: usize) -> Arc<aterm_types::ImageData> {
    Arc::new(aterm_types::ImageData {
        bytes: vec![0xA5; payload],
        format: aterm_types::ImageFormat::Png,
        cols,
        rows,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: aterm_types::ImageScaling::Fit,
        source_rect: None,
    })
}

fn image_row(image: &Arc<aterm_types::ImageData>, row: u16, cols: u16) -> Line {
    let mut line = Line::from("");
    line.set_images(vec![ImageSpan::new(0, cols, row, 0, Arc::clone(image))]);
    line
}

fn store(hot: usize) -> Scrollback {
    Scrollback::with_block_size(hot, 1_000, 100_000_000, hot)
}

#[test]
fn retaining_hot_images_preserves_order_and_reconciles_payload_accounting() {
    let image = placement(4, 2, 4096);
    let mut line = Line::from("kept text");
    line.set_images(
        (0..4)
            .map(|row| ImageSpan::new(0, 2, row, 0, Arc::clone(&image)))
            .collect(),
    );
    let mut storage: ScrollbackStorage = store(16).into();
    storage.push_line(line).unwrap();
    assert_eq!(Arc::strong_count(&image), 5);
    let before = storage.total_memory_used();
    let budget = storage.budgeted_memory_used();
    let pointer = storage
        .get_line(0)
        .unwrap()
        .unwrap()
        .images()
        .unwrap()
        .as_ptr();
    assert!(!storage.retain_images(&mut |_| true));
    assert_eq!(storage.total_memory_used(), before);
    assert_eq!(storage.budgeted_memory_used(), budget);
    assert_eq!(
        storage
            .get_line(0)
            .unwrap()
            .unwrap()
            .images()
            .unwrap()
            .as_ptr(),
        pointer,
        "a no-match delete keeps the allocation"
    );
    assert!(storage.retain_images(&mut |span| span.image_row % 2 == 1));
    assert_eq!(Arc::strong_count(&image), 3);
    let line = storage.get_line(0).unwrap().unwrap();
    assert_eq!(line.to_string(), "kept text");
    assert_eq!(
        line.images()
            .unwrap()
            .iter()
            .map(|span| span.image_row)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    drop(line);
    assert!(storage.total_memory_used() < before);
    assert!(storage.budgeted_memory_used() < budget);
    assert!(storage.retain_images(&mut |_| false));
    assert_eq!(
        Arc::strong_count(&image),
        1,
        "all history payload references were released"
    );
    assert!(!storage.any_image(&|_| true));
    let ScrollbackStorage::Memory(sb) = &storage;
    sb.assert_bytes_used_invariant();
    assert_eq!(sb.bytes_used, sb.recompute_total_memory_used());
    assert_eq!(sb.budgeted_bytes, sb.recompute_budgeted_bytes());
}

#[test]
fn a_picture_is_retained_for_exactly_as_long_as_its_line_is_uncompressed() {
    let image = placement(2, 4, 8_192);
    let mut sb = store(4);
    for row in 0..2u16 {
        sb.push_line(image_row(&image, row, 4));
    }

    for idx in 0..2 {
        let line = sb.get_line(idx).expect("ok").expect("present");
        let span = line.get_image(0).expect("still covered");
        assert!(
            Arc::ptr_eq(&span.image, &image),
            "hot line {idx} must hold the ONE payload, not a copy"
        );
    }
    assert_eq!(
        sb.image_rows_dropped_by_compression(),
        0,
        "nothing has been compressed, so nothing has crossed the horizon"
    );

    for i in 0..8 {
        sb.push_str(&format!("after {i}"));
    }
    assert_eq!(sb.line_count(), 10, "every line is still retained");
    for idx in 0..2 {
        let line = sb.get_line(idx).expect("ok").expect("present");
        assert!(
            !line.has_images(),
            "line {idx} crossed the horizon and must not still claim a picture"
        );
    }
    assert_eq!(
        sb.image_rows_dropped_by_compression(),
        2,
        "both footprint rows crossed, and both are reported"
    );
}

#[test]
fn crossing_the_horizon_costs_the_line_its_picture_and_nothing_else() {
    let image = placement(1, 3, 4_096);
    let mut sb = store(2);

    let mut line = Line::with_hyperlinks(
        "caption under the picture",
        Rle::new(),
        vec![HyperlinkSpan::with_id(
            0,
            7,
            "https://example.invalid/".into(),
            Some("cap".into()),
        )],
    );
    line.set_images(vec![ImageSpan::new(0, 3, 0, 0, Arc::clone(&image))]);
    sb.push_line(line);

    for i in 0..6 {
        sb.push_str(&format!("filler {i}"));
    }

    let back = sb.get_line(0).expect("ok").expect("present");
    assert_eq!(
        back.as_str(),
        Some("caption under the picture"),
        "the text is untouched by the horizon"
    );
    assert_eq!(
        back.hyperlinks().map(<[HyperlinkSpan]>::len),
        Some(1),
        "the hyperlink is untouched by the horizon"
    );
    assert!(!back.has_images(), "only the picture is gone");
    assert!(
        back.get_image(0).is_none(),
        "no half-restored span survives the crossing"
    );
    assert_eq!(sb.image_rows_dropped_by_compression(), 1);
}

#[test]
fn a_footprint_straddling_the_horizon_ages_out_from_its_top() {
    const ROWS: u16 = 6;
    let image = placement(ROWS, 4, 12_288);
    let mut sb = store(4);
    for row in 0..ROWS {
        sb.push_line(image_row(&image, row, 4));
    }
    assert_eq!(
        sb.image_rows_dropped_by_compression(),
        4,
        "the four oldest footprint rows crossed"
    );

    let surviving: Vec<u16> = (0..ROWS)
        .filter_map(|idx| {
            let line = sb.get_line(usize::from(idx)).expect("ok").expect("present");
            line.get_image(0).map(|span| span.image_row)
        })
        .collect();
    assert_eq!(
        surviving,
        vec![4, 5],
        "the picture must survive as a suffix of its footprint rows, not a hole in the middle"
    );
}

#[test]
fn the_horizon_is_reported_only_for_lines_that_actually_carried_a_picture() {
    let mut sb = store(4);
    for i in 0..200 {
        sb.push_str(&format!("plain {i}"));
    }
    assert!(sb.warm_line_count() > 0, "history really was compressed");
    assert_eq!(
        sb.image_rows_dropped_by_compression(),
        0,
        "text-only history must never report an image crossing"
    );
}

#[test]
fn the_horizon_count_never_decreases_and_survives_a_clear() {
    let image = placement(1, 2, 2_048);
    let mut sb = store(2);
    sb.push_line(image_row(&image, 0, 2));
    for i in 0..6 {
        sb.push_str(&format!("f{i}"));
    }
    let crossed = sb.image_rows_dropped_by_compression();
    assert_eq!(crossed, 1);
    sb.clear();
    assert_eq!(sb.line_count(), 0);
    assert_eq!(
        sb.image_rows_dropped_by_compression(),
        crossed,
        "clearing history does not un-lose what was already lost"
    );
}

#[test]
fn a_footprints_payload_leaves_the_byte_budget_when_its_rows_cross() {
    const PAYLOAD: usize = 400_000;
    const ROWS: u16 = 4;
    let image = placement(ROWS, 4, PAYLOAD);
    let mut sb = store(4);
    for row in 0..ROWS {
        sb.push_line(image_row(&image, row, 4));
    }
    let with_picture = sb.total_memory_used();
    assert!(
        with_picture >= PAYLOAD,
        "a retained footprint must be charged its raster: {with_picture} < {PAYLOAD}"
    );

    for i in 0..8 {
        sb.push_str(&format!("g{i}"));
    }
    assert_eq!(sb.image_rows_dropped_by_compression(), ROWS.into());
    let after = sb.total_memory_used();
    assert!(
        after < PAYLOAD / 2,
        "the raster must leave the budget with the rows that crossed: {after}"
    );
}
