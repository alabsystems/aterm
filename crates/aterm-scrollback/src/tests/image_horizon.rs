// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The image retention horizon at the tier: a picture is retained exactly as
//! long as its line is uncompressed, the crossing is counted, and past it the
//! row is the empty row it always was. `line_tests.rs` pins the codec half. A
//! block-codec payload table would move the horizon and fail these tests.

use std::sync::Arc;

use super::*;
use crate::ImageSpan;

/// A `rows`-tall placement whose payload is `payload` bytes — big enough that
/// the tier accounting has something to see.
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

/// One footprint row as a history line: an image row carries no glyph.
fn image_row(image: &Arc<aterm_types::ImageData>, row: u16, cols: u16) -> Line {
    let mut line = Line::from("");
    line.set_images(vec![ImageSpan::new(0, cols, row, 0, Arc::clone(image))]);
    line
}

/// A store whose hot tier holds exactly `hot` lines and compresses in blocks of
/// `hot`, so "crossed the horizon" is a precise, countable event.
fn store(hot: usize) -> Scrollback {
    Scrollback::with_block_size(hot, 1_000, 100_000_000, hot)
}

#[test]
fn a_picture_is_retained_for_exactly_as_long_as_its_line_is_uncompressed() {
    let image = placement(2, 4, 8_192);
    let mut sb = store(4);
    for row in 0..2u16 {
        sb.push_line(image_row(&image, row, 4));
    }

    // In the hot tier the payload is the very allocation the grid placed
    // (pointer identity, which the renderer's decode cache keys on).
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

    // Push past the hot tier so those two lines are compressed into a warm
    // block. They are still history — only the picture ends here.
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
    // Only the picture goes: text and links come back intact, with no
    // half-restored span.
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
    // History ages out oldest-first, and so does a picture: from its top, one
    // row at a time, never from the middle and never resurrected.
    const ROWS: u16 = 6;
    let image = placement(ROWS, 4, 12_288);
    let mut sb = store(4);
    for row in 0..ROWS {
        sb.push_line(image_row(&image, row, 4));
    }
    // Exactly one block of 4 is compressed once the 5th line arrives; rows 4
    // and 5 stay uncompressed.
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
    // Non-vacuity: a session that never drew a picture never reports a crossing.
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
    // Monotonic like every out-of-band loss signal, so a host can diff two reads.
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
    // A retained picture is charged its share; when its rows cross the charge
    // goes too, or the budget would evict text for a raster it no longer holds.
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
