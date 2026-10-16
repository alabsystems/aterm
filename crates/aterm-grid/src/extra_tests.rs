// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn cell_extra_default_is_empty() {
    let extra = CellExtra::default();
    assert!(!extra.has_data());
}

#[test]
fn cell_extra_with_hyperlink() {
    let mut extra = CellExtra::default();
    let url: Arc<str> = "https://example.com".into();
    extra.set_hyperlink(Some(url.clone()));

    assert!(extra.has_data());
    assert_eq!(extra.hyperlink(), Some(&url));
}

#[test]
fn cell_extra_with_underline_color() {
    let mut extra = CellExtra::default();
    extra.set_underline_color(Some([255, 0, 0])); // Red

    assert!(extra.has_data());
    assert_eq!(extra.underline_color(), Some([255, 0, 0]));

    // Test clear
    extra.set_underline_color(None);
    assert!(!extra.has_data());
    assert_eq!(extra.underline_color(), None);
}

#[test]
fn cell_extra_packed_rgb_colors() {
    let mut extra = CellExtra::default();

    // Test all three RGB colors can be set independently
    extra.set_underline_color(Some([10, 20, 30]));
    extra.set_fg_rgb(Some([40, 50, 60]));
    extra.set_bg_rgb(Some([70, 80, 90]));

    assert!(extra.has_data());
    assert_eq!(extra.underline_color(), Some([10, 20, 30]));
    assert_eq!(extra.fg_rgb(), Some([40, 50, 60]));
    assert_eq!(extra.bg_rgb(), Some([70, 80, 90]));

    // Clear one, others remain
    extra.set_fg_rgb(None);
    assert!(extra.has_data());
    assert_eq!(extra.underline_color(), Some([10, 20, 30]));
    assert_eq!(extra.fg_rgb(), None);
    assert_eq!(extra.bg_rgb(), Some([70, 80, 90]));

    // Clear all
    extra.set_underline_color(None);
    extra.set_bg_rgb(None);
    assert!(!extra.has_data());
}

#[test]
fn cell_extra_extended_flags() {
    let mut extra = CellExtra::default();

    extra.set_extended_flags(0x1234);
    assert!(extra.has_data()); // flags != 0
    assert_eq!(extra.extended_flags(), 0x1234);

    // Extended flags preserve color presence
    extra.set_underline_color(Some([100, 100, 100]));
    assert_eq!(extra.extended_flags(), 0x1234);
    assert_eq!(extra.underline_color(), Some([100, 100, 100]));

    // Setting extended flags preserves color presence
    extra.set_extended_flags(0x0567);
    assert_eq!(extra.extended_flags(), 0x0567);
    assert_eq!(extra.underline_color(), Some([100, 100, 100]));
}

#[test]
fn cell_extra_with_combining() {
    let mut extra = CellExtra::default();
    extra.add_combining('\u{0301}'); // Combining acute accent
    extra.add_combining('\u{0308}'); // Combining diaeresis

    assert!(extra.has_data());
    assert_eq!(extra.combining(), &['\u{0301}', '\u{0308}']);
}

#[test]
fn cell_extra_with_image() {
    use std::sync::Arc;
    let mut extra = CellExtra::default();
    assert!(extra.image().is_none(), "no image by default");
    assert!(!extra.has_data(), "empty extra has no data");

    let image = Arc::new(ImageData {
        bytes: vec![0x89, b'P', b'N', b'G'],
        format: ImageFormat::Png,
        cols: 3,
        rows: 2,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    extra.set_image(Some(ImageRef {
        image: Arc::clone(&image),
        cell_row: 1,
        cell_col: 2,
        kitty: None,
    }));

    assert!(extra.has_data(), "an image makes the extra non-empty");
    let got = extra.image().expect("image set");
    assert_eq!(got.cell_row, 1);
    assert_eq!(got.cell_col, 2);
    assert_eq!(got.image.cols, 3);
    assert_eq!(got.image.rows, 2);
    assert_eq!(got.image.format, ImageFormat::Png);
    assert!(Arc::ptr_eq(&got.image, &image), "shares the payload Arc");

    // Clearing the image drops the ref and returns to empty.
    extra.set_image(None);
    assert!(extra.image().is_none());
    assert!(!extra.has_data());
}

/// Kitty layers stack in draw order (z, then first-send time, then placement
/// order), at most `MAX_IMAGE_LAYERS` deep with the lowest dropped first;
/// `set_image` makes one image the only layer and `retain_images` drops layers
/// without reordering the rest.
#[test]
fn cell_extra_stacks_image_layers_in_draw_order() {
    use crate::KittyPlacementTag;
    use crate::extra::MAX_IMAGE_LAYERS;
    use std::num::NonZeroU32;
    use std::sync::Arc;
    let layer = |z: i32, id: u32, serial: u32| ImageRef {
        image: Arc::new(ImageData {
            bytes: Vec::new(),
            format: ImageFormat::Png,
            cols: 1,
            rows: 1,
            z_index: z,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        }),
        cell_row: 0,
        cell_col: 0,
        kitty: NonZeroU32::new(serial).map(|serial| KittyPlacementTag {
            image_id: id,
            placement_id: 0,
            // Each image here was first sent in the order of its id.
            image_serial: NonZeroU32::new(id).unwrap_or(NonZeroU32::MIN),
            serial,
        }),
    };
    let order = |extra: &CellExtra| -> Vec<(i32, u32, u32)> {
        extra.images().iter().map(ImageRef::draw_key).collect()
    };
    let mut extra = CellExtra::default();
    for (z, id, serial) in [(0, 5, 1), (0, 2, 2), (-3, 9, 3), (0, 5, 4), (7, 1, 5)] {
        extra.stack_image(layer(z, id, serial));
    }
    assert_eq!(
        order(&extra),
        [(-3, 9, 3), (0, 2, 2), (0, 5, 1), (0, 5, 4), (7, 1, 5)]
    );
    assert_eq!(
        extra.image().map(ImageRef::draw_key),
        Some((7, 1, 5)),
        "the top"
    );

    extra.retain_images(|image| image.draw_key().1 != 5);
    assert_eq!(order(&extra), [(-3, 9, 3), (0, 2, 2), (7, 1, 5)]);

    for serial in 10..10 + MAX_IMAGE_LAYERS as u32 {
        extra.stack_image(layer(1, 3, serial));
    }
    assert_eq!(extra.images().len(), MAX_IMAGE_LAYERS);
    assert_eq!(
        extra.images().first().map(ImageRef::draw_key),
        Some((1, 3, 11)),
        "the three lowest went: (-3, 9, 3), (0, 2, 2) and (1, 3, 10)"
    );
    assert_eq!(extra.image().map(ImageRef::draw_key), Some((7, 1, 5)));

    extra.set_image(Some(layer(0, 0, 0)));
    assert_eq!(order(&extra), [(0, 0, 0)], "an image that owns the cell");
    extra.retain_images(|_| false);
    assert!(!extra.has_data(), "no layer left, no image");
}

#[test]
fn cell_extra_max_combining() {
    let mut extra = CellExtra::default();
    for _ in 0..20 {
        extra.add_combining('\u{0301}');
    }
    // Should be capped at MAX_COMBINING
    assert_eq!(extra.combining().len(), CellExtra::MAX_COMBINING);
}

#[test]
fn cell_extras_storage() {
    let mut extras = CellExtras::new();
    let coord = CellCoord::new(5, 10);

    assert!(extras.get(coord).is_none());

    let extra = extras.get_or_create(coord);
    extra.set_hyperlink(Some("https://test.com".into()));

    let stored = extras
        .get(coord)
        .expect("entry should exist after get_or_create");
    assert_eq!(
        stored.hyperlink().map(|h| h.as_ref()),
        Some("https://test.com"),
        "stored hyperlink should match"
    );
    assert_eq!(extras.len(), 1);
}

#[test]
fn cell_extras_clear_row() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(0, 5))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(1, 0))
        .add_combining('\u{0303}');
    extras
        .get_or_create(CellCoord::new(2, 0))
        .add_combining('\u{0304}');

    assert_eq!(extras.len(), 4);

    extras.clear_row(0);

    assert_eq!(extras.len(), 2);
    assert!(
        extras.get(CellCoord::new(0, 0)).is_none(),
        "row 0 col 0 cleared"
    );
    assert!(
        extras.get(CellCoord::new(0, 5)).is_none(),
        "row 0 col 5 cleared"
    );
    let r1 = extras
        .get(CellCoord::new(1, 0))
        .expect("row 1 should survive clear_row(0)");
    assert_eq!(r1.combining(), &['\u{0303}'], "row 1 content preserved");
}

#[test]
fn cell_extras_shift_rows_up() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(1, 0))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(2, 0))
        .add_combining('\u{0303}');

    extras.shift_rows_up_by(1, 1);

    let r0 = extras.get(CellCoord::new(0, 0)).unwrap();
    assert_eq!(r0.combining(), &['\u{0301}'], "row 0 content preserved");

    let r1 = extras.get(CellCoord::new(1, 0)).unwrap();
    assert_eq!(
        r1.combining(),
        &['\u{0303}'],
        "row 2 content shifted to row 1"
    );

    assert!(extras.get(CellCoord::new(2, 0)).is_none());
}

#[test]
fn cell_extras_empty_removed() {
    let mut extras = CellExtras::new();
    let coord = CellCoord::new(0, 0);

    let mut extra = CellExtra::default();
    extra.add_combining('\u{0301}');
    extras.set(coord, extra);
    assert_eq!(extras.len(), 1);

    extras.set(coord, CellExtra::default());
    assert_eq!(extras.len(), 0);
}

#[test]
fn complex_char_roundtrip() {
    let mut extra = CellExtra::default();
    assert!(extra.complex_char().is_none());
    assert!(!extra.has_data());

    let emoji: Arc<str> = Arc::from("👨‍👩‍👧‍👦");
    extra.set_complex_char(Some(emoji.clone()));

    assert!(extra.has_data());
    let stored = extra.complex_char().expect("complex_char should be set");
    assert_eq!(
        stored.as_ref(),
        emoji.as_ref(),
        "stored complex char should preserve value"
    );

    extra.set_complex_char(None);
    assert!(extra.complex_char().is_none());
    assert!(!extra.has_data());
}

#[test]
fn cell_extras_clear_range_real() {
    let mut extras = CellExtras::new();

    for col in 0..5u16 {
        extras
            .get_or_create(CellCoord::new(3, col))
            .add_combining('\u{0301}');
    }
    extras
        .get_or_create(CellCoord::new(4, 2))
        .add_combining('\u{0302}');

    assert_eq!(extras.len(), 6);
    extras.clear_range(3, 1, 4);

    assert_eq!(extras.len(), 3);
    let col0 = extras
        .get(CellCoord::new(3, 0))
        .expect("col 0 should survive clear_range");
    assert_eq!(col0.combining(), &['\u{0301}'], "col 0 content preserved");
    assert!(extras.get(CellCoord::new(3, 1)).is_none(), "col 1 cleared");
    assert!(extras.get(CellCoord::new(3, 2)).is_none(), "col 2 cleared");
    assert!(extras.get(CellCoord::new(3, 3)).is_none(), "col 3 cleared");
    let col4 = extras
        .get(CellCoord::new(3, 4))
        .expect("col 4 should survive clear_range");
    assert_eq!(col4.combining(), &['\u{0301}'], "col 4 content preserved");
    let other_row = extras
        .get(CellCoord::new(4, 2))
        .expect("row 4 should be untouched");
    assert_eq!(
        other_row.combining(),
        &['\u{0302}'],
        "row 4 content preserved"
    );
}

#[test]
fn cell_extras_clear_rect_real() {
    let mut extras = CellExtras::new();

    for row in 0..4u16 {
        for col in 0..4u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
    }

    extras.clear_rect(1..3, 1..3);

    for row in 0..4u16 {
        for col in 0..4u16 {
            let present = extras.get(CellCoord::new(row, col)).is_some();
            let inside = (1..3).contains(&row) && (1..3).contains(&col);
            assert_eq!(present, !inside, "row={row} col={col} presence mismatch");
        }
    }
}

#[test]
fn underline_color_u32_roundtrip() {
    let mut extra = CellExtra::default();

    extra.set_underline_color_u32(Some(0x01_FF8040));
    assert_eq!(extra.underline_color_u32(), Some(0x01_FF8040));
    assert_eq!(extra.underline_color(), Some([0xFF, 0x80, 0x40]));

    extra.set_underline_color(Some([0x12, 0x34, 0x56]));
    assert_eq!(extra.underline_color_u32(), Some(0x01_123456));

    extra.set_underline_color_u32(Some(0x01_000000));
    assert_eq!(extra.underline_color(), Some([0, 0, 0]));
    assert_eq!(extra.underline_color_u32(), Some(0x01_000000));

    extra.set_underline_color_u32(Some(0x01_FFFFFF));
    assert_eq!(extra.underline_color(), Some([0xFF, 0xFF, 0xFF]));

    extra.set_underline_color_u32(None);
    assert_eq!(extra.underline_color_u32(), None);
    assert_eq!(extra.underline_color(), None);
    assert!(!extra.has_data());

    // The indexed form (0x02) round-trips as an index, not a resolved triple:
    // an indexed SGR 58 colour re-resolves against the live palette at draw time.
    extra.set_underline_color_u32(Some(0x02_000005));
    assert_eq!(extra.underline_color_u32(), Some(0x02_000005));
    assert_eq!(extra.underline_color_index(), Some(5));
    assert_eq!(
        extra.underline_color(),
        None,
        "storing an index clears the RGB slot on purpose"
    );
}

/// `clear_range` removes hyperlinks within
/// the column span but preserves hyperlinks outside it.
#[test]
fn row_has_hyperlinks_after_clear_range() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(2, 5))
        .set_hyperlink(Some(Arc::from("https://inside.example.com")));
    extras
        .get_or_create(CellCoord::new(2, 15))
        .set_hyperlink(Some(Arc::from("https://outside.example.com")));
    assert!(extras.row_has_hyperlinks(2));

    // Clear columns 0..10 — removes the hyperlink at col 5
    extras.clear_range(2, 0, 10);
    assert!(
        extras.row_has_hyperlinks(2),
        "hyperlink at col 15 survives clear_range(0..10)"
    );

    // Clear columns 10..20 — removes the hyperlink at col 15
    extras.clear_range(2, 10, 20);
    assert!(
        !extras.row_has_hyperlinks(2),
        "no hyperlinks remain after clearing entire row range"
    );
}

#[test]
fn hyperlink_has_data_consistent() {
    let mut extra = CellExtra::default();

    assert!(!extra.has_data(), "empty extra has no data");
    assert!(extra.hyperlink().is_none(), "empty extra has no hyperlink");

    extra.set_hyperlink(Some(Arc::from("url")));
    assert!(extra.has_data(), "extra with hyperlink has data");

    extra.set_hyperlink(None);
    assert!(!extra.has_data(), "cleared extra has no data");

    extra.set_hyperlink(Some(Arc::from("url2")));
    extra.set_underline_color(Some([255, 0, 0]));
    assert!(extra.has_data(), "extra with underline has data");

    extra.set_hyperlink(None);
    assert!(extra.has_data(), "extra with underline still has data");
}

#[test]
fn set_hyperlink_clears_stale_id_on_url_replacement() {
    let mut extra = CellExtra::default();

    extra.set_hyperlink(Some(Arc::from("https://a.example.com")));
    extra.set_hyperlink_id(Some(Arc::from("link-a")));

    extra.set_hyperlink(Some(Arc::from("https://b.example.com")));
    assert_eq!(
        extra.hyperlink_id(),
        None,
        "stale ID from previous hyperlink must be cleared on URL replacement",
    );

    extra.set_hyperlink_id(Some(Arc::from("link-b")));
    assert_eq!(extra.hyperlink_id().map(Arc::as_ref), Some("link-b"));
}

// Batch clear tests (clear_rows, clear_rect)

#[test]
fn cell_extras_clear_rows_batch() {
    let mut extras = CellExtras::new();

    for row in 0..5u16 {
        extras
            .get_or_create(CellCoord::new(row, 0))
            .add_combining('\u{0301}');
        extras
            .get_or_create(CellCoord::new(row, 3))
            .add_combining('\u{0302}');
    }
    assert_eq!(extras.len(), 10);

    extras.clear_rows(1..4);

    assert_eq!(extras.len(), 4, "only rows 0 and 4 survive");
    assert!(extras.get(CellCoord::new(0, 0)).is_some(), "row 0 survives");
    assert!(extras.get(CellCoord::new(1, 0)).is_none(), "row 1 cleared");
    assert!(extras.get(CellCoord::new(4, 0)).is_some(), "row 4 survives");
}

#[test]
fn cell_extras_clear_rows_empty_range() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(5, 0))
        .add_combining('\u{0301}');

    extras.clear_rows(3..3);
    assert_eq!(extras.len(), 1, "empty range should be no-op");
}

#[test]
fn cell_extras_clear_rect_batch() {
    let mut extras = CellExtras::new();

    for row in 0..5u16 {
        for col in 0..5u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
    }
    assert_eq!(extras.len(), 25);

    extras.clear_rect(1..4, 1..4);
    assert_eq!(extras.len(), 16, "9 cells in rect should be cleared");
}

#[test]
fn cell_extras_clear_rect_empty_cols() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');

    extras.clear_rect(0..6, 3..3);
    assert_eq!(extras.len(), 1, "empty col range should be no-op");
}

// Column shift tests (ICH/DCH)

#[test]
fn cell_extras_shift_cols_right() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(1, 2))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(1, 5))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(1, 9))
        .add_combining('\u{0303}');
    extras
        .get_or_create(CellCoord::new(2, 3))
        .add_combining('\u{0304}');

    extras.shift_cols_right(1, 4, 3, 10);

    assert_eq!(extras.len(), 3);
    assert!(
        extras.get(CellCoord::new(1, 2)).is_some(),
        "col 2 preserved"
    );
    assert!(extras.get(CellCoord::new(1, 8)).is_some(), "col 5 -> col 8");
    assert!(extras.get(CellCoord::new(1, 9)).is_none(), "col 9 dropped");
    assert!(
        extras.get(CellCoord::new(2, 3)).is_some(),
        "other row untouched"
    );
}

#[test]
fn cell_extras_shift_cols_left() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(0, 1))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(0, 3))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(0, 7))
        .add_combining('\u{0303}');
    extras
        .get_or_create(CellCoord::new(5, 3))
        .add_combining('\u{0304}');

    extras.shift_cols_left(0, 2, 2, 10);

    assert_eq!(extras.len(), 3);
    assert!(
        extras.get(CellCoord::new(0, 1)).is_some(),
        "col 1 preserved"
    );
    assert!(extras.get(CellCoord::new(0, 3)).is_none(), "col 3 deleted");
    assert!(extras.get(CellCoord::new(0, 5)).is_some(), "col 7 -> col 5");
}

#[test]
fn cell_extras_shift_cols_zero_count_is_noop() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(0, 5))
        .add_combining('\u{0301}');

    extras.shift_cols_right(0, 3, 0, 10);
    assert_eq!(extras.len(), 1, "shift_cols_right with count=0 is no-op");

    extras.shift_cols_left(0, 3, 0, 10);
    assert_eq!(extras.len(), 1, "shift_cols_left with count=0 is no-op");
}

/// `shift_cols_right` shifts target-row entries in place and leaves other rows
/// untouched.
#[test]
fn shift_cols_right_preserves_non_target_rows() {
    let mut extras = CellExtras::new();
    // Populate 200 entries across 10 rows
    for row in 0..10u16 {
        for col in 0..20u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
    }
    assert_eq!(extras.len(), 200);

    // Shift columns right on row 5 only: cols >= 10 shift right by 3
    extras.shift_cols_right(5, 10, 3, 20);

    // All 9 non-target rows must have every entry intact
    for row in [0u16, 1, 2, 3, 4, 6, 7, 8, 9] {
        for col in 0..20u16 {
            assert!(
                extras.get(CellCoord::new(row, col)).is_some(),
                "row {row} col {col} should be untouched"
            );
        }
    }

    // Target row: cols 0-9 unchanged
    for col in 0..10u16 {
        assert!(
            extras.get(CellCoord::new(5, col)).is_some(),
            "row 5 col {col} before start_col should be preserved"
        );
    }
    // cols 10-16 shifted to 13-19
    for col in 13..17u16 {
        assert!(
            extras.get(CellCoord::new(5, col)).is_some(),
            "row 5 col {col} should have shifted entry"
        );
    }
    // cols 10-12 are now blank (insertion gap)
    for col in 10..13u16 {
        assert!(
            extras.get(CellCoord::new(5, col)).is_none(),
            "row 5 col {col} should be cleared (insertion gap)"
        );
    }
    // cols 17-19 shifted past max_col=20, so dropped: total 200 - 3 = 197
    assert_eq!(
        extras.len(),
        197,
        "3 entries should be dropped past max_col"
    );
}

/// Verify `shift_cols_left` correctness and that non-target rows are untouched.
#[test]
fn shift_cols_left_preserves_non_target_rows() {
    let mut extras = CellExtras::new();
    for row in 0..10u16 {
        for col in 0..20u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
    }
    assert_eq!(extras.len(), 200);

    // Delete 3 columns starting at col 5 on row 3: cols 5-7 deleted, cols 8-19 shift left by 3
    extras.shift_cols_left(3, 5, 3, 20);

    // All 9 non-target rows must have every entry intact
    for row in [0u16, 1, 2, 4, 5, 6, 7, 8, 9] {
        for col in 0..20u16 {
            assert!(
                extras.get(CellCoord::new(row, col)).is_some(),
                "row {row} col {col} should be untouched"
            );
        }
    }

    // Target row: cols 0-4 preserved
    for col in 0..5u16 {
        assert!(
            extras.get(CellCoord::new(3, col)).is_some(),
            "row 3 col {col} before deletion should be preserved"
        );
    }
    // cols 8-19 shifted to 5-16
    for col in 5..17u16 {
        assert!(
            extras.get(CellCoord::new(3, col)).is_some(),
            "row 3 col {col} should have shifted entry (from col {})",
            col + 3
        );
    }
    // Total: 200 - 3 deleted = 197
    assert_eq!(extras.len(), 197, "3 entries should have been deleted");
}

/// `shift_cols_left` honours `max_col` (DECLRMM): extras at or beyond it stay
/// put; only `[start_col, max_col)` shifts.
#[test]
fn shift_cols_left_respects_max_col_boundary() {
    let mut extras = CellExtras::new();
    // Populate extras at columns 2, 4, 5, 8, 12, 15 on row 0
    for &col in &[2u16, 4, 5, 8, 12, 15] {
        extras
            .get_or_create(CellCoord::new(0, col))
            .add_combining('\u{0301}');
    }
    assert_eq!(extras.len(), 6);

    // Delete 2 columns at col 3 with the margin at col 10: col 4 is deleted,
    // cols 5 and 8 move to 3 and 6, cols 12 and 15 stay.
    extras.shift_cols_left(0, 3, 2, 10);

    // col 2: before start_col, preserved
    assert!(
        extras.get(CellCoord::new(0, 2)).is_some(),
        "col 2 before start_col should be preserved"
    );
    // col 4: in deletion range [3, 5), deleted
    assert!(
        extras.get(CellCoord::new(0, 4)).is_none(),
        "col 4 in deletion range should be removed"
    );
    // col 5: first column after the deletion range, shifted left by 2 → col 3
    assert!(
        extras.get(CellCoord::new(0, 3)).is_some(),
        "col 5 should shift to col 3"
    );
    assert!(
        extras.get(CellCoord::new(0, 5)).is_none(),
        "col 5 original position should be empty"
    );
    // col 8: shifted left by 2 → col 6
    assert!(
        extras.get(CellCoord::new(0, 6)).is_some(),
        "col 8 should shift to col 6"
    );
    assert!(
        extras.get(CellCoord::new(0, 8)).is_none(),
        "col 8 original position should be empty"
    );
    // col 12: beyond max_col, preserved at original position
    assert!(
        extras.get(CellCoord::new(0, 12)).is_some(),
        "col 12 beyond max_col must be preserved"
    );
    // col 15: beyond max_col, preserved at original position
    assert!(
        extras.get(CellCoord::new(0, 15)).is_some(),
        "col 15 beyond max_col must be preserved"
    );
    // Total: col 2 + col 3 (was 5) + col 6 (was 8) + col 12 + col 15 = 5 entries
    assert_eq!(
        extras.len(),
        5,
        "one entry deleted, rest preserved or shifted"
    );
}

// Memory shrink tests

#[test]
fn cell_extras_clear_releases_capacity() {
    let mut extras = CellExtras::new();
    for i in 0..200u16 {
        extras
            .get_or_create(CellCoord::new(i / 20, i % 20))
            .set_hyperlink(Some(Arc::from("https://example.com")));
    }
    assert_eq!(extras.len(), 200);
    let cap_before = extras.capacity();
    assert!(cap_before >= 200);

    extras.clear();
    assert_eq!(extras.len(), 0);
    assert!(
        extras.capacity() < cap_before,
        "clear() should release capacity: before={cap_before}, after={}",
        extras.capacity()
    );
}

#[test]
fn cell_extras_clear_rows_shrinks_when_excess() {
    let mut extras = CellExtras::new();
    for row in 0..10u16 {
        for col in 0..20u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .set_hyperlink(Some(Arc::from("https://example.com")));
        }
    }
    assert_eq!(extras.len(), 200);
    let cap_before = extras.capacity();

    extras.clear_rows(0..10);
    assert_eq!(extras.len(), 0);
    assert!(
        extras.capacity() < cap_before,
        "clear_rows should shrink: before={cap_before}, after={}",
        extras.capacity()
    );
}

#[test]
fn cell_extras_retain_no_shrink_on_small_map() {
    let mut extras = CellExtras::new();
    for col in 0..10u16 {
        extras
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(Arc::from("https://example.com")));
    }
    let cap_before = extras.capacity();

    extras.clear_row(0);
    assert_eq!(extras.len(), 0);
    assert_eq!(extras.capacity(), cap_before, "small map should not shrink");
}

// Hyperlink limit enforcement tests

/// Hyperlinks work normally when under the limit.
#[test]
fn test_hyperlink_limit_normal_usage_under_limit() {
    let mut extras = CellExtras::new();
    let url: Arc<str> = Arc::from("https://example.com");

    // Add 100 hyperlinks — well under the 10,000 limit.
    for i in 0..100u16 {
        extras
            .get_or_create(CellCoord::new(i / 10, i % 10))
            .set_hyperlink(Some(url.clone()));
    }
    extras.enforce_hyperlink_limit();

    // All 100 should survive.
    let hyperlink_count = extras
        .data
        .values()
        .filter(|e| e.hyperlink().is_some())
        .count();
    assert_eq!(
        hyperlink_count, 100,
        "all hyperlinks should survive under limit"
    );
}

/// When the limit is exceeded, oldest hyperlink entries are evicted.
#[test]
fn test_hyperlink_limit_evicts_oldest_when_exceeded() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;

    let mut extras = CellExtras::new();

    // Fill to MAX + 500 entries, each with a unique hyperlink URL.
    // Use rows 0..N with col 0 so internal ordering is clear.
    let total = MAX_HYPERLINK_ENTRIES + 500;
    for i in 0..total {
        let row = (i / 256) as u16;
        let col = (i % 256) as u16;
        let url: Arc<str> = Arc::from(format!("https://evil.com/{i}"));
        extras
            .get_or_create(CellCoord::new(row, col))
            .set_hyperlink(Some(url));
    }

    // Before enforcement, we have total entries with hyperlinks.
    let before = extras
        .data
        .values()
        .filter(|e| e.hyperlink().is_some())
        .count();
    assert_eq!(before, total, "all hyperlinks present before enforcement");

    // Enforce the limit.
    extras.enforce_hyperlink_limit();

    // After enforcement, hyperlink count should be at most MAX_HYPERLINK_ENTRIES.
    let after = extras
        .data
        .values()
        .filter(|e| e.hyperlink().is_some())
        .count();
    assert!(
        after <= MAX_HYPERLINK_ENTRIES,
        "hyperlink count should be at most {MAX_HYPERLINK_ENTRIES}, got {after}"
    );
    assert_eq!(
        after,
        MAX_HYPERLINK_ENTRIES * 3 / 4,
        "should evict down to the low-water mark, buying headroom for later inserts"
    );
}

/// Eviction removes from oldest rows first (lowest internal row coordinate).
#[test]
fn test_hyperlink_limit_evicts_oldest_rows_first() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;

    let mut extras = CellExtras::new();

    // Create MAX+100 entries across rows. Row 0 is oldest.
    let total = MAX_HYPERLINK_ENTRIES + 100;
    for i in 0..total {
        let row = (i / 200) as u16;
        let col = (i % 200) as u16;
        let url: Arc<str> = Arc::from(format!("https://test.com/{i}"));
        extras
            .get_or_create(CellCoord::new(row, col))
            .set_hyperlink(Some(url));
    }

    extras.enforce_hyperlink_limit();

    // Eviction is oldest first: entries sort monotonically in `i`, so the
    // survivors are exactly the newest suffix.
    let low_water = MAX_HYPERLINK_ENTRIES * 3 / 4;
    let mut surviving: Vec<usize> = extras
        .data
        .iter()
        .filter(|(_, extra)| extra.hyperlink().is_some())
        .map(|(coord, _)| usize::from(coord.row) * 200 + usize::from(coord.col))
        .collect();
    surviving.sort_unstable();
    assert_eq!(
        surviving.len(),
        low_water,
        "one pass trims to the low-water mark"
    );
    let expected: Vec<usize> = (total - low_water..total).collect();
    assert_eq!(
        surviving, expected,
        "eviction must remove the oldest rows first, as an unbroken prefix"
    );
}

/// Non-hyperlink extras are preserved during eviction.
#[test]
fn test_hyperlink_limit_preserves_non_hyperlink_extras() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;

    let mut extras = CellExtras::new();

    // Add a non-hyperlink extra (combining mark) on row 0, col 0.
    extras
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');

    // Also add a hyperlink + combining mark on row 0, col 1.
    let entry = extras.get_or_create(CellCoord::new(0, 1));
    entry.set_hyperlink(Some(Arc::from("https://evict.me")));
    entry.add_combining('\u{0302}');

    // Fill the rest to exceed the limit.
    for i in 2..(MAX_HYPERLINK_ENTRIES + 100) {
        let row = (i / 200) as u16;
        let col = (i % 200) as u16;
        let url: Arc<str> = Arc::from(format!("https://spam.com/{i}"));
        extras
            .get_or_create(CellCoord::new(row, col))
            .set_hyperlink(Some(url));
    }

    extras.enforce_hyperlink_limit();

    // The non-hyperlink extra at (0, 0) should be untouched.
    let entry_0_0 = extras.get(CellCoord::new(0, 0));
    assert!(entry_0_0.is_some(), "non-hyperlink entry should survive");
    assert_eq!(
        entry_0_0.map(|e| e.combining().len()),
        Some(1),
        "combining mark at (0,0) should be preserved"
    );

    // The entry at (0, 1) should have lost its hyperlink but kept combining.
    let entry_0_1 = extras.get(CellCoord::new(0, 1));
    assert!(
        entry_0_1.is_some(),
        "entry with other extras should survive"
    );
    assert!(
        entry_0_1.map(|e| e.hyperlink().is_none()).unwrap_or(false),
        "hyperlink at (0,1) should be evicted (oldest)"
    );
    assert_eq!(
        entry_0_1.map(|e| e.combining().len()),
        Some(1),
        "combining mark at (0,1) should be preserved after hyperlink eviction"
    );
}

/// When map is large but has few hyperlinks, no eviction occurs.
#[test]
fn test_hyperlink_limit_large_map_few_hyperlinks_no_eviction() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;

    let mut extras = CellExtras::new();

    // Create MAX+1000 entries, but only 100 have hyperlinks.
    for i in 0..(MAX_HYPERLINK_ENTRIES + 1000) {
        let row = (i / 200) as u16;
        let col = (i % 200) as u16;
        let entry = extras.get_or_create(CellCoord::new(row, col));
        if i < 100 {
            entry.set_hyperlink(Some(Arc::from("https://real-link.com")));
        } else {
            // Non-hyperlink extra (RGB color).
            entry.set_fg_rgb(Some([255, 0, 0]));
        }
    }

    extras.enforce_hyperlink_limit();

    // All 100 hyperlinks should survive since we're under the hyperlink limit.
    let hyperlink_count = extras
        .data
        .values()
        .filter(|e| e.hyperlink().is_some())
        .count();
    assert_eq!(hyperlink_count, 100, "no hyperlinks should be evicted");
}

/// The O(1) guard tests the hyperlink population, not the whole map: with
/// 14_600 entries of which 600 carry a link, the first call repairs the bound
/// and later calls take the early-out, evicting nothing.
#[test]
fn hyperlink_guard_counts_links_not_total_entries() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;
    use crate::test_counters::take_hyperlink_walk_ops;

    let mut extras = CellExtras::new();
    // 14_000 entries without a link (an fg RGB stands in for SGR 58).
    for i in 0..14_000u32 {
        extras
            .get_or_create(CellCoord::new((i / 200) as u16, (i % 200) as u16))
            .set_fg_rgb(Some([1, 2, 3]));
    }
    // 600 hyperlinks — 6 % of the limit, nowhere near it.
    let url: Arc<str> = Arc::from("https://example.com");
    for i in 0..600u32 {
        extras
            .get_or_create(CellCoord::new(70 + (i / 200) as u16, (i % 200) as u16))
            .set_hyperlink(Some(url.clone()));
    }
    assert!(
        extras.len() > MAX_HYPERLINK_ENTRIES,
        "precondition: the map must be OVER the limit, or the old guard would \
         have taken its early-out and this test would prove nothing"
    );
    assert_eq!(extras.hyperlink_entries_slow(), 600);

    let _ = take_hyperlink_walk_ops(); // reset
    extras.enforce_hyperlink_limit();
    let first = take_hyperlink_walk_ops();
    assert!(
        first >= 14_600,
        "the first call walks once to repair the bound, got {first} entries"
    );

    for _ in 0..30 {
        extras.enforce_hyperlink_limit();
    }
    assert_eq!(
        take_hyperlink_walk_ops(),
        0,
        "every later call must take the O(1) early-out: 600 hyperlinks is far \
         under the {MAX_HYPERLINK_ENTRIES} limit and the map's other 14_000 \
         entries are not the bounded quantity"
    );

    // Behaviour identity: still nothing evicted, still every entry present.
    assert_eq!(extras.hyperlink_entries_slow(), 600);
    assert_eq!(extras.len(), 14_600);
}

/// A map under the limit never walks, however inflated the counter.
///
/// `hyperlink_upper_bound` over-charges on every handout to a link-free entry,
/// so repainting 1_600 SGR 58 cells drives it past MAX_HYPERLINK_ENTRIES.
/// `data.len()` is a second upper bound (links <= entries) and says 1_600, so
/// no walk happens.
#[test]
fn small_map_never_walks_however_inflated_the_counter() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;
    use crate::test_counters::take_hyperlink_walk_ops;

    let mut extras = CellExtras::new();
    // A status line's worth of coloured underlines: 10 rows x 160 cols.
    for row in 0..10u16 {
        for col in 0..160u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .set_underline_color(Some([7, 7, 7]));
        }
    }
    assert_eq!(extras.len(), 1_600);
    assert!(
        extras.len() <= MAX_HYPERLINK_ENTRIES,
        "precondition: the map must be UNDER the limit"
    );

    let _ = take_hyperlink_walk_ops(); // reset

    // Redraw the same cells until the counter is well past the limit. Each
    // handout charges again — the entries already exist and hold no link.
    for _ in 0..20 {
        for row in 0..10u16 {
            for col in 0..160u16 {
                extras
                    .get_or_create(CellCoord::new(row, col))
                    .set_underline_color(Some([7, 7, 7]));
            }
        }
        extras.enforce_hyperlink_limit();
    }
    assert!(
        extras.hyperlink_upper_bound() > MAX_HYPERLINK_ENTRIES,
        "precondition: the counter must be inflated past the limit ({} <= {}), \
         or this test proves nothing",
        extras.hyperlink_upper_bound(),
        MAX_HYPERLINK_ENTRIES
    );
    assert_eq!(
        take_hyperlink_walk_ops(),
        0,
        "a map of {} entries cannot hold more than {MAX_HYPERLINK_ENTRIES} \
         hyperlinks, so the cold walk is provably pointless and must be skipped",
        extras.len()
    );
    assert_eq!(extras.hyperlink_entries_slow(), 0);
}

/// The bound never sits below the true hyperlink population, through every
/// mutation family: per-cell handout, batch run, whole-value set, removal, row
/// clear, offset scroll + compaction, region scroll, and column shifts. Upward
/// drift costs one self-repairing walk; downward drift would disable eviction.
#[test]
fn hyperlink_upper_bound_never_under_counts() {
    fn check(extras: &CellExtras, stage: &str) {
        assert!(
            extras.hyperlink_upper_bound() >= extras.hyperlink_entries_slow(),
            "{stage}: bound {} is BELOW the true hyperlink count {} — the limit \
             would stop being enforced",
            extras.hyperlink_upper_bound(),
            extras.hyperlink_entries_slow()
        );
    }

    let mut extras = CellExtras::new();
    let url: Arc<str> = Arc::from("https://example.com");

    // 1. Per-cell handout (`handler_write`'s per-character OSC 8 path).
    for col in 0..10u16 {
        extras
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }
    check(&extras, "per-cell handout");

    // 2. Batch run (`write_ascii_run_with_extras` -> `set_range_uniform`).
    let vals = UniformExtras {
        fg_rgb: None,
        bg_rgb: None,
        underline_color: None,
        extended_flags: 0,
        hyperlink: Some(&url),
        hyperlink_id: None,
    };
    extras.set_range_uniform(1, 0, 10, &vals, 24, 80);
    check(&extras, "batch run");
    // Re-painting the same run must not charge again: the cells already carry
    // the link, so the population is unchanged.
    let before = extras.hyperlink_upper_bound();
    extras.set_range_uniform(1, 0, 10, &vals, 24, 80);
    assert_eq!(
        extras.hyperlink_upper_bound(),
        before,
        "a repaint of an already-linked run must not inflate the bound"
    );

    // 3. Whole-value set (DECCRA's rect copy).
    let mut carried = CellExtra::default();
    carried.set_hyperlink(Some(url.clone()));
    extras.set(CellCoord::new(2, 0), carried);
    check(&extras, "whole-value set");

    // 4. Single removal refunds.
    assert!(extras.remove(CellCoord::new(2, 0)));
    check(&extras, "single removal");

    // 5. Row clear (retain-based; no refund by design — must stay above).
    extras.clear_row(1);
    check(&extras, "row clear");

    // 6. O(1) offset scroll past the compaction threshold.
    for _ in 0..300 {
        extras.shift_rows_up_by(0, 1);
    }
    check(&extras, "offset scroll + compaction");

    // 7. Region scroll (drain-rebuild) and column shifts.
    extras.set_range_uniform(3, 0, 10, &vals, 24, 80);
    extras.shift_rows_up_by(1, 1);
    check(&extras, "region scroll");
    extras.shift_cols_right(2, 0, 2, 80);
    check(&extras, "ICH column shift");
    extras.shift_cols_left(2, 0, 2, 80);
    check(&extras, "DCH column shift");

    // 8. `clear()` is exact, not conservative.
    extras.clear();
    assert_eq!(extras.hyperlink_upper_bound(), 0);
    assert_eq!(extras.hyperlink_entries_slow(), 0);
}

#[path = "extra_tests/overflow.rs"]
mod overflow;
#[path = "extra_tests/scaling_proofs.rs"]
mod scaling_proofs;
#[path = "extra_tests/shift_region_tests.rs"]
mod shift_region_tests;
