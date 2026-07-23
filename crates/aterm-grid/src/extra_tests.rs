// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_hash::FxHashMap;

// =============================================================================
// Test-only helpers (moved from extra.rs to reduce #[cfg(test)] in production)
// =============================================================================

impl CellExtra {
    /// Get the underline color as legacy u32 format.
    #[must_use]
    fn underline_color_u32(&self) -> Option<u32> {
        self.underline_color().map(|[r, g, b]| {
            0x01_000000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        })
    }

    /// Clear all combining characters.
    fn clear_combining(&mut self) {
        self.combining.clear();
    }
}

impl CellExtras {
    /// Shift rows down (for scroll up).
    ///
    /// Rows >= start_row are shifted down by 1.
    /// Used when inserting a row at start_row.
    fn shift_rows_down(&mut self, start_row: u16, max_row: u16) {
        // Compact first so we operate on external coordinates.
        self.compact();
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row >= start_row && coord.row < max_row {
                new_data.insert(
                    CellCoord::new(coord.row.saturating_add(1), coord.col),
                    extra,
                );
            } else if coord.row < start_row {
                new_data.insert(coord, extra);
            }
            // Rows >= max_row are dropped (scrolled off)
        }
        self.data = new_data;
    }
}

/// Check if a character is a zero-width character.
#[must_use]
fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
    )
}

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
fn cell_extra_size_optimized() {
    let size = std::mem::size_of::<CellExtra>();
    assert!(
        size <= 72,
        "CellExtra should be <= 72 bytes, got {size} bytes"
    );
    // 72 bytes: 64 (after boxing the rare complex_char Arc<str>) plus the
    // niche-optimized `image: Option<Box<ImageRef>>` field (8 bytes) for inline
    // images (iTerm2 OSC 1337). CellExtra is allocated only for cells that have
    // extras at all (rare), so the 8-byte growth is paid by those cells only.
    assert_eq!(size, 72, "CellExtra should be exactly 72 bytes");
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
    });
    extra.set_image(Some(ImageRef {
        image: Arc::clone(&image),
        cell_row: 1,
        cell_col: 2,
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
fn cell_extras_clear_rows_basic() {
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
    extras
        .get_or_create(CellCoord::new(3, 0))
        .add_combining('\u{0304}');

    extras.clear_rows(1..3);

    assert!(extras.get(CellCoord::new(0, 0)).is_some(), "row 0 survives");
    assert!(extras.get(CellCoord::new(1, 0)).is_none(), "row 1 cleared");
    assert!(extras.get(CellCoord::new(2, 0)).is_none(), "row 2 cleared");
    assert!(extras.get(CellCoord::new(3, 0)).is_some(), "row 3 survives");
}

#[test]
fn cell_extras_shift_rows_down() {
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

    extras.shift_rows_down(1, 3);

    let r0 = extras.get(CellCoord::new(0, 0)).unwrap();
    assert_eq!(r0.combining(), &['\u{0301}'], "row 0 content preserved");

    assert!(extras.get(CellCoord::new(1, 0)).is_none());

    let r2 = extras.get(CellCoord::new(2, 0)).unwrap();
    assert_eq!(
        r2.combining(),
        &['\u{0302}'],
        "row 1 content shifted to row 2"
    );

    let r3 = extras.get(CellCoord::new(3, 0)).unwrap();
    assert_eq!(
        r3.combining(),
        &['\u{0303}'],
        "row 2 content shifted to row 3"
    );
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
fn is_combining_mark_basic() {
    assert!(is_combining_mark('\u{0301}'));
    assert!(is_combining_mark('\u{0308}'));
    assert!(is_combining_mark('\u{0327}'));
    assert!(!is_combining_mark('a'));
    assert!(!is_combining_mark(' '));
}

#[test]
fn is_combining_mark_all_ranges() {
    assert!(is_combining_mark('\u{0300}'));
    assert!(is_combining_mark('\u{036F}'));
    assert!(is_combining_mark('\u{1AB0}'));
    assert!(is_combining_mark('\u{1AFF}'));
    assert!(is_combining_mark('\u{1DC0}'));
    assert!(is_combining_mark('\u{1DFF}'));
    assert!(is_combining_mark('\u{20D0}'));
    assert!(is_combining_mark('\u{20FF}'));
    assert!(is_combining_mark('\u{FE20}'));
    assert!(is_combining_mark('\u{FE2F}'));

    assert!(!is_combining_mark('\u{02FF}'));
    assert!(!is_combining_mark('\u{0370}'));
    assert!(!is_combining_mark('\u{1AAF}'));
    assert!(!is_combining_mark('\u{1B00}'));
    assert!(!is_combining_mark('\u{1DBF}'));
    assert!(!is_combining_mark('\u{1E00}'));
    assert!(!is_combining_mark('\u{20CF}'));
    assert!(!is_combining_mark('\u{2100}'));
    assert!(!is_combining_mark('\u{FE1F}'));
    assert!(!is_combining_mark('\u{FE30}'));
}

#[test]
fn is_zero_width_basic() {
    assert!(is_zero_width('\u{200B}'));
    assert!(is_zero_width('\u{200C}'));
    assert!(is_zero_width('\u{200D}'));
    assert!(is_zero_width('\u{2060}'));
    assert!(is_zero_width('\u{FEFF}'));
    assert!(!is_zero_width('a'));
    assert!(!is_zero_width(' '));
    assert!(!is_zero_width('\u{200A}'));
    assert!(!is_zero_width('\u{200E}'));
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
fn clear_combining_removes_all() {
    let mut extra = CellExtra::default();
    extra.add_combining('\u{0301}');
    extra.add_combining('\u{0308}');
    extra.add_combining('\u{0327}');
    assert_eq!(extra.combining().len(), 3);
    assert!(extra.has_data());

    extra.clear_combining();
    assert!(extra.combining().is_empty());
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
}

#[test]
fn row_has_hyperlinks_detection() {
    let mut extras = CellExtras::new();

    assert!(!extras.row_has_hyperlinks(0));

    extras
        .get_or_create(CellCoord::new(0, 5))
        .add_combining('\u{0301}');
    assert!(!extras.row_has_hyperlinks(0));

    extras
        .get_or_create(CellCoord::new(1, 3))
        .set_hyperlink(Some(Arc::from("https://example.com")));
    assert!(!extras.row_has_hyperlinks(0));
    assert!(extras.row_has_hyperlinks(1));

    extras
        .get_or_create(CellCoord::new(0, 10))
        .set_hyperlink(Some(Arc::from("https://test.com")));
    assert!(extras.row_has_hyperlinks(0));

    extras
        .get_or_create(CellCoord::new(0, 10))
        .set_hyperlink(None);
    assert!(!extras.row_has_hyperlinks(0));
}

/// Behavioral test for #5816: after dead hyperlink_row_cache removal,
/// verify `row_has_hyperlinks` correctly handles partial removal of
/// multiple hyperlinks on the same row.
#[test]
fn row_has_hyperlinks_partial_removal() {
    let mut extras = CellExtras::new();

    // Two hyperlinks on the same row
    extras
        .get_or_create(CellCoord::new(3, 0))
        .set_hyperlink(Some(Arc::from("https://a.example.com")));
    extras
        .get_or_create(CellCoord::new(3, 10))
        .set_hyperlink(Some(Arc::from("https://b.example.com")));
    assert!(extras.row_has_hyperlinks(3));

    // Remove one — row still has a hyperlink from the other cell
    extras
        .get_or_create(CellCoord::new(3, 0))
        .set_hyperlink(None);
    assert!(
        extras.row_has_hyperlinks(3),
        "row should still report hyperlinks when only one of two is removed"
    );

    // Remove the second — now the row is clean
    extras
        .get_or_create(CellCoord::new(3, 10))
        .set_hyperlink(None);
    assert!(
        !extras.row_has_hyperlinks(3),
        "row should report no hyperlinks after both are removed"
    );
}

/// Behavioral test for #5816: `clear_range` removes hyperlinks within
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

// =============================================================================
// Batch clear tests (clear_rows, clear_rect)
// =============================================================================

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

// --- Column shift tests (ICH/DCH extras support, #4057) ---

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

// =============================================================================
// Column shift no-realloc tests (#5550)
// =============================================================================

/// Verify `shift_cols_right` correctness and that non-target rows are untouched.
///
/// The fix extracts only target-row entries and reinserts shifted versions
/// in-place. Non-target-row entries are never removed or reinserted.
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

/// Verify `shift_cols_left` respects the `max_col` boundary when DECLRMM is
/// active.  Extras at or beyond `max_col` must be preserved in their original
/// positions — only columns in `[start_col, max_col)` participate in the shift.
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

    // Delete 2 columns starting at col 3, margin boundary at col 10.
    // Deletion range: [3, 5) — col 4 is deleted.
    // Shift range: [5, 10) — col 5 shifts left by 2 → col 3, col 8 → col 6.
    // Cols >= 10 (12, 15) must be preserved at original positions.
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

// =============================================================================
// Memory shrink tests (#4376)
// =============================================================================

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

// =============================================================================
// Hyperlink limit enforcement tests (#7172)
// =============================================================================

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
        after, MAX_HYPERLINK_ENTRIES,
        "should evict exactly the excess"
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

    // The evicted entries should be from row 0 (the oldest row).
    // Row 0 had 200 entries. We evicted 100, so row 0 should have 100 left.
    let row0_hyperlinks = extras
        .data
        .iter()
        .filter(|(coord, extra)| coord.row == 0 && extra.hyperlink().is_some())
        .count();
    assert_eq!(
        row0_hyperlinks, 100,
        "100 of the 200 entries in row 0 should have been evicted"
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

#[path = "extra_tests/overflow.rs"]
mod overflow;
#[path = "extra_tests/remap_reflow_tests.rs"]
mod remap_reflow_tests;
#[path = "extra_tests/scaling_proofs.rs"]
mod scaling_proofs;
#[path = "extra_tests/shift_region_tests.rs"]
mod shift_region_tests;
