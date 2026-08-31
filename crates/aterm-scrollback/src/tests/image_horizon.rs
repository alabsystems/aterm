// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE IMAGE RETENTION HORIZON, pinned at the TIER — where a picture stops
//! following its text into history.
//!
//! `line_tests.rs` pins the codec half (`Line::serialize` drops the image and
//! keeps everything else). These pin what the STORE does with that, which is
//! the part a person actually experiences: a picture is retained for exactly as
//! long as its line is held uncompressed, the crossing is COUNTED rather than
//! silent, and past it the row is the empty row it always was underneath —
//! never half-restored, never invented.
//!
//! Every test here also has to fail loudly if a future change gives the block
//! codec a payload table: at that point the horizon MOVES, and these are what
//! say so.

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
    })
}

/// One footprint row as a history line: an image row carries no glyph, which is
/// the shape the defect this horizon describes lives in.
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

    // Inside the hot tier the payload is the very same allocation the live grid
    // placed — pointer identity, not a copy, which is what the renderer's
    // decode cache is keyed on.
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
    // The predictability contract. If a compressed line came back with its text
    // mangled, its style dropped or a half-restored span, "the picture is gone"
    // would not be a description anybody could rely on.
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
    // The rule a person can hold in their head: history ages out oldest-first,
    // and a picture is history, so it is eaten from the TOP one row at a time —
    // never from the middle, never all at once, and never resurrected.
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
    // Non-vacuity for the counter itself: a session that never drew one must
    // never report a crossing, however much history it compresses.
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
    // Monotonic, like every other out-of-band loss signal (audit E10a): a host
    // polling it must be able to diff two reads without the count walking back
    // under it.
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
    // The horizon is also what keeps the budget honest: while a picture is
    // retained the store is charged its share, and when the rows cross that
    // charge has to go with it — otherwise the budget would evict text history
    // to make room for a raster it no longer holds.
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

// --- The disk-backed twin -------------------------------------------------
//
// Same horizon, same reason: the boundary is COMPRESSION, so a picture never
// reaches the `.dtrm` cold file at all. Pinned separately because the two
// stores promote through their own `promote_hot_to_warm`.

#[cfg(feature = "disk-tier")]
#[test]
fn the_disk_backed_store_ends_a_picture_at_compression_not_at_the_disk() {
    use crate::{DiskBackedScrollback, DiskBackedScrollbackConfig};

    let dir = aterm_tempfile::tempdir().expect("temp dir");
    let config = DiskBackedScrollbackConfig::new(dir.path().join("horizon.dtrm"))
        .with_hot_limit(4)
        .with_warm_limit(4)
        .with_block_size(4);
    let mut sb = DiskBackedScrollback::with_config(config).expect("create disk scrollback");

    let image = placement(2, 4, 8_192);
    for row in 0..2u16 {
        sb.push_line(image_row(&image, row, 4)).expect("push");
    }
    assert!(
        sb.get_line(0).expect("ok").expect("present").has_images(),
        "uncompressed, the picture is there"
    );
    assert_eq!(sb.image_rows_dropped_by_compression(), 0);

    // Enough to compress the first block and push it all the way to the disk
    // cold tier — the picture was already gone one tier earlier.
    for i in 0..32 {
        sb.push_line(Line::from(format!("d{i}").as_str()))
            .expect("push");
    }
    assert!(sb.cold_line_count() > 0, "history really reached disk");
    assert!(
        !sb.get_line(0).expect("ok").expect("present").has_images(),
        "a compressed line carries no picture, on disk or in RAM"
    );
    assert_eq!(
        sb.image_rows_dropped_by_compression(),
        2,
        "the disk store reports the crossing exactly as the memory store does"
    );
}
