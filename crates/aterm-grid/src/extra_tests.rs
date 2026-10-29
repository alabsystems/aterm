// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

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
    assert_eq!(extra.combining().len(), CellExtra::MAX_COMBINING);
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

    extra.set_underline_color_u32(Some(0x02_000005));
    assert_eq!(extra.underline_color_u32(), Some(0x02_000005));
    assert_eq!(extra.underline_color_index(), Some(5));
    assert_eq!(
        extra.underline_color(),
        None,
        "storing an index clears the RGB slot on purpose"
    );

    extra.set_underline_color_u32(Some(0x00_000009));
    assert_eq!(extra.underline_color_u32(), Some(0x02_000009));
    assert_eq!(extra.underline_color_index(), Some(9));
    assert_eq!(extra.underline_color(), None);

    extra.set_underline_color_u32(Some(0xFF_000000));
    assert_eq!(extra.underline_color_u32(), None);
    assert_eq!(extra.underline_color_index(), None);
    assert_eq!(extra.underline_color(), None);
    assert!(!extra.has_data());

    assert_eq!(crate::underline_color_sgr(0x00_000009), "58:5:9");
    assert_eq!(crate::underline_color_sgr(0x02_000005), "58:5:5");
    assert_eq!(crate::underline_color_sgr(0x01_FF8040), "58:2::255:128:64");
    assert_eq!(crate::underline_color_sgr(0xFF_000000), "59");
}

#[test]
fn has_data_tracks_every_field_independently() {
    let url: Arc<str> = Arc::from("https://example.com");
    let emoji: Arc<str> = Arc::from("\u{1F468}\u{200D}\u{1F469}");
    let fields = |e: &CellExtra| {
        (
            e.hyperlink().cloned(),
            e.complex_char().cloned(),
            e.underline_color(),
            e.fg_rgb(),
            e.bg_rgb(),
            e.extended_flags(),
        )
    };

    let mut extra = CellExtra::default();
    assert!(!extra.has_data());
    extra.set_hyperlink(Some(url.clone()));
    extra.set_complex_char(Some(emoji.clone()));
    extra.set_underline_color(Some([10, 20, 30]));
    extra.set_fg_rgb(Some([40, 50, 60]));
    extra.set_bg_rgb(Some([70, 80, 90]));
    extra.set_extended_flags(0xF234);
    extra.set_extended_flags(0xA567);
    assert_eq!(
        fields(&extra),
        (
            Some(url.clone()),
            Some(emoji.clone()),
            Some([10, 20, 30]),
            Some([40, 50, 60]),
            Some([70, 80, 90]),
            0xA567
        )
    );
    extra.set_hyperlink(None);
    extra.set_complex_char(None);
    extra.set_fg_rgb(None);
    assert_eq!(
        fields(&extra),
        (
            None,
            None,
            Some([10, 20, 30]),
            None,
            Some([70, 80, 90]),
            0xA567
        )
    );
    extra.set_underline_color(None);
    extra.set_bg_rgb(None);
    assert!(extra.has_data(), "the extended flags alone are data");
    extra.set_extended_flags(0);
    assert!(!extra.has_data());

    for high_flag in [crate::CellFlags::CURLY_UNDERLINE.bits(), 0x8000, u16::MAX] {
        let mut high = CellExtra::default();
        high.set_extended_flags(high_flag);
        assert_eq!(high.extended_flags(), high_flag);
        assert!(
            high.has_data(),
            "extended flag {high_flag:#06x} must survive"
        );
    }

    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let setters: [Box<dyn Fn(&mut CellExtra)>; 7] = [
        Box::new(|e| e.set_hyperlink(Some(url.clone()))),
        Box::new(|e| e.set_complex_char(Some(emoji.clone()))),
        Box::new(|e| e.set_underline_color(Some([1, 2, 3]))),
        Box::new(|e| e.set_fg_rgb(Some([1, 2, 3]))),
        Box::new(|e| e.set_bg_rgb(Some([1, 2, 3]))),
        Box::new(|e| e.set_extended_flags(1)),
        Box::new(|e| e.add_combining('\u{0301}')),
    ];
    for (i, set) in setters.iter().enumerate() {
        let mut extra = CellExtra::default();
        set(&mut extra);
        assert!(extra.has_data(), "field {i} alone is data");
    }
}

#[test]
fn row_has_hyperlinks_tracks_the_links_on_the_row() {
    fn link(extras: &mut CellExtras, row: u16, col: u16, url: Option<&str>) {
        extras
            .get_or_create(CellCoord::new(row, col))
            .set_hyperlink(url.map(Arc::from));
    }
    let mut extras = CellExtras::new();
    assert!(!extras.row_has_hyperlinks(0));
    extras
        .get_or_create(CellCoord::new(0, 5))
        .add_combining('\u{0301}');
    link(&mut extras, 1, 3, Some("https://a.example"));
    assert!(!extras.row_has_hyperlinks(0), "a mark is not a link");
    assert!(extras.row_has_hyperlinks(1));

    link(&mut extras, 0, 0, Some("https://b.example"));
    link(&mut extras, 0, 10, Some("https://c.example"));
    link(&mut extras, 0, 0, None);
    assert!(extras.row_has_hyperlinks(0), "one of two links removed");
    link(&mut extras, 0, 10, None);
    assert!(!extras.row_has_hyperlinks(0));

    link(&mut extras, 2, 5, Some("https://inside.example"));
    link(&mut extras, 2, 15, Some("https://outside.example"));
    extras.clear_range(2, 0, 10);
    assert!(
        extras.row_has_hyperlinks(2),
        "col 15 survives clear_range(0..10)"
    );
    extras.clear_range(2, 10, 20);
    assert!(!extras.row_has_hyperlinks(2));
}

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
fn test_hyperlink_limit_evicts_oldest_rows_first() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;

    let mut extras = CellExtras::new();

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

#[test]
fn test_hyperlink_limit_preserves_non_hyperlink_extras() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;

    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');

    let entry = extras.get_or_create(CellCoord::new(0, 1));
    entry.set_hyperlink(Some(Arc::from("https://evict.me")));
    entry.add_combining('\u{0302}');

    for i in 2..(MAX_HYPERLINK_ENTRIES + 100) {
        let row = (i / 200) as u16;
        let col = (i % 200) as u16;
        let url: Arc<str> = Arc::from(format!("https://spam.com/{i}"));
        extras
            .get_or_create(CellCoord::new(row, col))
            .set_hyperlink(Some(url));
    }

    extras.enforce_hyperlink_limit();

    let entry_0_0 = extras.get(CellCoord::new(0, 0));
    assert!(entry_0_0.is_some(), "non-hyperlink entry should survive");
    assert_eq!(
        entry_0_0.map(|e| e.combining().len()),
        Some(1),
        "combining mark at (0,0) should be preserved"
    );

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

#[test]
fn hyperlink_guard_counts_links_not_total_entries() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;
    use crate::test_counters::take_hyperlink_walk_ops;

    let mut extras = CellExtras::new();
    for i in 0..14_000u32 {
        extras
            .get_or_create(CellCoord::new((i / 200) as u16, (i % 200) as u16))
            .set_fg_rgb(Some([1, 2, 3]));
    }
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

    assert_eq!(extras.hyperlink_entries_slow(), 600);
    assert_eq!(extras.len(), 14_600);
}

#[test]
fn small_map_never_walks_however_inflated_the_counter() {
    use crate::extra_collection::MAX_HYPERLINK_ENTRIES;
    use crate::test_counters::take_hyperlink_walk_ops;

    let mut extras = CellExtras::new();
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

    for col in 0..10u16 {
        extras
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }
    check(&extras, "per-cell handout");

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
    let before = extras.hyperlink_upper_bound();
    extras.set_range_uniform(1, 0, 10, &vals, 24, 80);
    assert_eq!(
        extras.hyperlink_upper_bound(),
        before,
        "a repaint of an already-linked run must not inflate the bound"
    );

    let mut carried = CellExtra::default();
    carried.set_hyperlink(Some(url.clone()));
    extras.set(CellCoord::new(2, 0), carried);
    check(&extras, "whole-value set");

    assert!(extras.remove(CellCoord::new(2, 0)));
    check(&extras, "single removal");

    extras.clear_row(1);
    check(&extras, "row clear");

    for _ in 0..300 {
        extras.shift_rows_up_by(1);
    }
    check(&extras, "offset scroll + compaction");

    extras.set_range_uniform(3, 0, 10, &vals, 24, 80);
    extras.shift_region_up_by(1, 23, 1);
    check(&extras, "region scroll");
    extras.shift_cols_right(2, 0, 2, 80);
    check(&extras, "ICH column shift");
    extras.shift_cols_left(2, 0, 2, 80);
    check(&extras, "DCH column shift");

    extras.clear();
    assert_eq!(extras.hyperlink_upper_bound(), 0);
    assert_eq!(extras.hyperlink_entries_slow(), 0);
}

#[test]
fn scroll_compound_cost_amortized_o1() {
    use crate::test_counters::take_extras_shift_ops;
    let _ = take_extras_shift_ops(); // reset

    let mut extras = CellExtras::new();
    let extras_per_row = 50u16;
    let num_rows = 10u16;
    for row in 0..num_rows {
        for col in 0..extras_per_row {
            extras
                .get_or_create(CellCoord::new(row, col))
                .set_fg_rgb(Some([255, 128, 0]));
        }
    }
    let initial_e = extras.len();
    assert_eq!(initial_e, (num_rows * extras_per_row) as usize);

    let _ = take_extras_shift_ops(); // reset
    let scroll_count = 50;
    for _ in 0..scroll_count {
        extras.shift_rows_up_by(1);
    }
    let total_ops = take_extras_shift_ops();

    assert_eq!(
        total_ops, 0,
        "50 scrolls below compact threshold should cost 0 shift ops, got {total_ops}"
    );

    assert_eq!(extras.row_offset(), 50);

    assert_eq!(extras.len(), 0, "all entries should have scrolled off");
    assert!(
        extras.is_empty(),
        "is_empty must agree with len() == 0 once every entry has scrolled off"
    );
}

#[test]
fn is_empty_agrees_with_len_across_offset_scrolls_and_removals() {
    let mut extras = CellExtras::new();
    assert!(extras.is_empty());
    extras
        .get_or_create(CellCoord::new(0, 0))
        .set_fg_rgb(Some([1, 2, 3]));
    extras
        .get_or_create(CellCoord::new(6, 2))
        .add_combining('\u{0301}');

    extras.shift_rows_up_by(3);
    assert_eq!(extras.row_offset(), 3);
    assert_eq!(extras.len(), 1);
    assert!(!extras.is_empty());
    assert!(
        extras.get(CellCoord::new(u16::MAX, 0)).is_none(),
        "external row u16::MAX must never wrap onto a stale scrolled-off entry"
    );

    assert!(extras.remove(CellCoord::new(3, 2)));
    assert_eq!(extras.len(), 0);
    assert!(
        extras.is_empty(),
        "removing the last live entry while stale entries remain must report empty"
    );
    assert!(
        extras.clone().is_empty(),
        "cloning preserves the max-row cache state"
    );

    extras
        .get_or_create(CellCoord::new(u16::MAX, 4))
        .set_bg_rgb(Some([9, 8, 7]));
    assert_eq!(extras.row_offset(), 0, "writing u16::MAX compacts first");
    assert_eq!(extras.len(), 1);
    assert!(!extras.is_empty());
    assert_eq!(
        extras
            .get(CellCoord::new(u16::MAX, 4))
            .and_then(CellExtra::bg_rgb),
        Some([9, 8, 7])
    );
}

#[test]
fn complex_ring_clears_and_overwrites_never_shadow_restored_clusters() {
    let mut grid = crate::Grid::new(4, 10);
    grid.extras_mut()
        .set_complex_char_ring(1, 0, '\u{1F680}', 4, 10);
    grid.extras_mut().clear_row(1);
    grid.write_char('x');
    grid.set_cell_complex_char(1, 0, "\u{1F469}\u{200D}\u{1F4BB}");
    let mut out = String::new();
    assert!(grid.extras().push_complex_char(1, 0, &mut out));
    assert_eq!(out, "\u{1F469}\u{200D}\u{1F4BB}");

    grid.extras_mut()
        .set_complex_char_ring(0, 0, '\u{1F680}', 4, 10);
    grid.set_cell_complex_char(0, 0, "e\u{0301}");
    assert_eq!(grid.extras().complex_codepoint_for(0, 0), Some('e'));

    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 2))
        .set_hyperlink(Some(Arc::from("https://stale-row0.example")));
    grid.extras_mut()
        .get_or_create(CellCoord::new(2, 0))
        .set_hyperlink(Some(Arc::from("https://stale-row2.example")));
    grid.restore_visible_from_lines(&[aterm_scrollback::Line::from("clean")]);
    assert!(
        grid.extras().get(CellCoord::new(0, 2)).is_none(),
        "fill_row_from_line must clear prior extras on overwritten rows"
    );
    assert!(
        grid.extras().get(CellCoord::new(2, 0)).is_none(),
        "restore_visible_from_lines must clear extras on trailing blank rows"
    );
    assert!(grid.extras().is_empty());
}

#[path = "extra_tests/overflow.rs"]
mod overflow;
