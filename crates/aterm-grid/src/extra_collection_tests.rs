// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn sparse_refill_cleanup_work_follows_heap_slots_not_unrelated_extras() {
    use std::sync::Arc;
    for cols in [80_u16, 256] {
        for kind in ["image", "complex", "combining"] {
            let mut extras = CellExtras::new();
            let image = Arc::new(crate::ImageData {
                bytes: vec![7; 256],
                format: crate::ImageFormat::Png,
                cols,
                rows: 24,
                z_index: 0,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: crate::ImageScaling::Fit,
                source_rect: None,
            });
            for row in 0..24 {
                for col in 0..cols {
                    extras
                        .get_or_create(CellCoord::new(row, col))
                        .set_fg_rgb(Some([1, 2, 3]));
                }
            }
            let mut stale = Vec::new();
            for col in 0..4 {
                let coord = CellCoord::new(0, col);
                if kind == "complex" {
                    let payload: Arc<str> = Arc::from("👩‍💻");
                    stale.push(Arc::downgrade(&payload));
                    extras.get_or_create(coord).set_complex_char(Some(payload));
                } else if kind == "combining" {
                    for mark in ['\u{301}', '\u{302}', '\u{303}', '\u{304}', '\u{305}'] {
                        extras.get_or_create(coord).add_combining(mark);
                    }
                } else {
                    extras.set_image(
                        CellCoord::new(0, col),
                        crate::ImageRef {
                            image: Arc::clone(&image),
                            cell_row: 0,
                            cell_col: col,
                            kitty: None,
                        },
                    );
                }
            }
            let refill = extras.prepare_rows_for_restore(0..24);
            for row in 0..24 {
                for col in 0..cols {
                    extras
                        .get_or_create(CellCoord::new(row, col))
                        .set_fg_rgb(Some([4, 5, 6]));
                }
            }
            crate::test_counters::take_extras_clear_ops();
            extras.finish_rows_restore(refill, false);
            let work = crate::test_counters::take_extras_clear_ops();
            assert!(
                work <= 4,
                "{cols} columns: sparse cleanup visited {work} entries"
            );
            assert!(!extras.any_image());
            assert!(extras.refill_scratch.is_empty());
            assert_eq!(extras.refill_scratch.capacity(), 0);
            assert_eq!(Arc::strong_count(&image), 1);
            assert!(stale.iter().all(|weak| weak.upgrade().is_none()));
            for row in 0..24 {
                for col in 0..cols {
                    assert_eq!(extras.fg_rgb_for(row, col), Some([4, 5, 6]));
                }
            }
        }
    }
}

#[test]
fn image_refill_clears_stale_payloads_and_preserves_outside_rows_at_offsets() {
    use std::sync::Arc;
    for offset in [0_u16, 1, 128] {
        let image = Arc::new(crate::ImageData {
            bytes: vec![7; 256],
            format: crate::ImageFormat::Png,
            cols: 4,
            rows: 3,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: crate::ImageScaling::Fit,
            source_rect: None,
        });
        let mut extras = CellExtras::new();
        for row in [offset, offset + 2] {
            extras.set_image(
                CellCoord::new(row, 0),
                crate::ImageRef {
                    image: Arc::clone(&image),
                    cell_row: 0,
                    cell_col: 0,
                    kitty: None,
                },
            );
        }
        extras.shift_rows_up_by(offset);
        assert_eq!(extras.row_offset, offset);
        let old = extras.get_or_create(CellCoord::new(0, 0));
        old.set_hyperlink(Some(Arc::from("https://old.example")));
        old.set_complex_char(Some(Arc::from("old")));
        old.add_combining('\u{0301}');
        old.set_fg_rgb(Some([8, 9, 10]));
        // Unrelated outside entries keep both refills in sparse mode.
        for col in 1..4 {
            extras
                .get_or_create(CellCoord::new(2, col))
                .set_fg_rgb(Some([11, 12, 13]));
        }
        for row in [0, 2] {
            extras.set_complex_char_ring(row, 0, '\u{1F680}', 3, 4);
            extras.set_rgb_ring_range(row, 0, 1, Some([1, 2, 3]), Some([4, 5, 6]), 3, 4);
        }
        let refill = extras.prepare_rows_for_restore(0..1);
        let reset = extras.get(CellCoord::new(0, 0)).unwrap();
        assert!(reset.image().is_none());
        assert!(reset.hyperlink().is_none());
        assert!(reset.complex_char().is_none());
        assert!(reset.combining().is_empty());
        assert!(reset.fg_rgb().is_none());
        assert_eq!(extras.fg_rgb_for(0, 0), None);
        assert_eq!(extras.fg_rgb_for(2, 0), Some([1, 2, 3]));
        for col in 1..4 {
            assert_eq!(extras.fg_rgb_for(2, col), Some([11, 12, 13]));
        }
        let ring = extras.complex_ring.as_ref().unwrap();
        assert_eq!(ring.get(0, 0), None);
        assert_eq!(ring.get(2, 0), Some('\u{1F680}'));
        extras.finish_rows_restore(refill, false);
        assert!(extras.get(CellCoord::new(0, 0)).is_none());
        assert!(extras.get(CellCoord::new(2, 0)).unwrap().image().is_some());
        assert!(extras.any_image());
        assert_eq!(Arc::strong_count(&image), 2);
        let refill = extras.prepare_rows_for_restore(2..3);
        extras.finish_rows_restore(refill, false);
        assert!(extras.is_empty());
        assert!(!extras.any_image());
        assert_eq!(Arc::strong_count(&image), 1);
    }
}

#[test]
fn ring_columns_past_the_stride_never_alias_the_next_row() {
    let mut complex = ComplexCharRing::new(4, 10);
    complex.set(1, 0, '\u{1F680}');
    complex.set(0, 15, '\u{2764}');
    assert_eq!(complex.get(1, 5), None, "a write past the stride aliased");
    for col in [10, 100] {
        assert_eq!(complex.get(0, col), None, "col {col}");
    }
    complex.set(0, 10, '\u{1F389}');
    assert_eq!(complex.get(1, 0), Some('\u{1F680}'));

    let mut rgb = RgbColorRing::new(4, 10);
    rgb.set_fg(1, 0, [0, 0, 255]);
    rgb.set_bg(1, 0, [128, 128, 0]);
    rgb.set_fg(0, 15, [255, 0, 0]);
    rgb.set_bg(0, 15, [0, 255, 0]);
    assert_eq!((rgb.fg(1, 5), rgb.bg(1, 5)), (None, None));
    for col in [10, 200] {
        assert_eq!((rgb.fg(0, col), rgb.bg(0, col)), (None, None), "col {col}");
    }
    rgb.set_fg(0, 10, [1, 1, 1]);
    assert_eq!(rgb.fg(1, 0), Some([0, 0, 255]));
}

#[test]
fn ring_overscroll_recycles_and_resets_the_offset() {
    for n in [4, 5, 65535] {
        let mut complex = ComplexCharRing::new(4, 10);
        complex.set(3, 1, '\u{1F389}');
        complex.scroll_up(1);
        complex.scroll_up(n);
        assert_eq!(complex.ring_offset, 0, "scroll {n}");
        assert_eq!(complex.get(2, 1), None, "scroll {n}");
        complex.set(2, 3, '\u{1F680}');
        assert_eq!(complex.get(2, 3), Some('\u{1F680}'), "scroll {n}");

        let mut rgb = RgbColorRing::new(4, 10);
        rgb.set_fg(3, 0, [255, 0, 0]);
        rgb.set_bg(3, 0, [0, 255, 0]);
        rgb.scroll_up(1);
        rgb.scroll_up(n);
        assert_eq!(rgb.ring_offset, 0, "scroll {n}");
        assert_eq!((rgb.fg(2, 0), rgb.bg(2, 0)), (None, None), "scroll {n}");
        rgb.set_fg(2, 2, [10, 20, 30]);
        rgb.set_bg(2, 2, [40, 50, 60]);
        assert_eq!(rgb.fg(2, 2), Some([10, 20, 30]), "scroll {n}");
        assert_eq!(rgb.bg(2, 2), Some([40, 50, 60]), "scroll {n}");
    }
}

#[test]
fn every_clear_removes_exactly_its_cells_from_map_and_rgb_ring() {
    type Clear = fn(&mut CellExtras);
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: &[(&str, Clear, fn(u16, u16) -> bool)] = &[
        ("clear_row(1)", |e| e.clear_row(1), |r, _| r == 1),
        (
            "clear_range(3, 1..4)",
            |e| e.clear_range(3, 1, 4),
            |r, c| r == 3 && (1..4).contains(&c),
        ),
        (
            "clear_range(3, 4..4)",
            |e| e.clear_range(3, 4, 4),
            |_, _| false,
        ),
        (
            "clear_rows(1..3)",
            |e| e.clear_rows(1..3),
            |r, _| (1..3).contains(&r),
        ),
        ("clear_rows(3..3)", |e| e.clear_rows(3..3), |_, _| false),
        (
            "clear_rect(1..3, 3..7)",
            |e| e.clear_rect(1..3, 3..7),
            |r, c| (1..3).contains(&r) && (3..7).contains(&c),
        ),
        (
            "clear_rect(0..4, 3..3)",
            |e| e.clear_rect(0..4, 3..3),
            |_, _| false,
        ),
    ];
    for &(name, clear, cleared) in cases {
        for dense in [true, false] {
            let cells: Vec<(u16, u16)> = (0..4u16)
                .flat_map(|r| (0..10u16).map(move |c| (r, c)))
                .filter(|&(r, c)| dense || (r + c) % 9 == 0)
                .collect();
            let mut extras = CellExtras::new();
            for &(r, c) in &cells {
                let mut extra = CellExtra::default();
                extra.set_fg_rgb(Some([r as u8, c as u8, 1]));
                extras.set(CellCoord::new(r, c), extra);
                extras.set_rgb_ring_range(r, c, c + 1, None, Some([r as u8, c as u8, 2]), 4, 10);
                let emoji = char::from_u32(0x1F600 + u32::from(r) * 16 + u32::from(c))
                    .expect("valid emoji");
                extras.set_complex_char_ring(r, c, emoji, 4, 10);
            }
            clear(&mut extras);
            assert_eq!(
                extras.is_empty(),
                usize::eq(&extras.len(), &0),
                "{name} (dense {dense}): is_empty vs len"
            );
            for &(r, c) in &cells {
                let kept = !cleared(r, c);
                let map = extras.get(CellCoord::new(r, c)).and_then(CellExtra::fg_rgb);
                assert_eq!(
                    map.is_some(),
                    kept,
                    "{name} (dense {dense}): map ({r}, {c})"
                );
                let ring = extras.bg_rgb_for(r, c);
                assert_eq!(
                    ring.is_some(),
                    kept,
                    "{name} (dense {dense}): rgb_ring ({r}, {c})"
                );
                let complex = extras.complex_codepoint_for(r, c);
                assert_eq!(
                    complex.is_some(),
                    kept,
                    "{name} (dense {dense}): complex_ring ({r}, {c})"
                );
            }
        }
    }
}

#[test]
fn fill_run_matches_per_cell_set() {
    let rgb_f = [17u8, 34, 51];
    let rgb_b = [68u8, 85, 102];
    let cases: &[(u16, u16, u16, u16, u16)] = &[
        (4, 10, 0, 0, 10),
        (4, 10, 2, 3, 7),
        (4, 10, 1, 0, 1),
        (4, 10, 3, 5, 5),
        (4, 10, 1, 7, 3),
        (4, 10, 2, 6, 20),
        (4, 10, 0, 10, 12),
        (4, 10, 0, 12, 15),
        (24, 8, 23, 0, 8),
        (24, 8, 5, 2, 6),
    ];
    for &(vis, cols, row, cs, ce) in cases {
        let mut per_cell = RgbColorRing::new(vis, cols);
        let mut bulk = RgbColorRing::new(vis, cols);
        for col in cs..ce {
            per_cell.set_fg(row, col, rgb_f);
            per_cell.set_bg(row, col, rgb_b);
        }
        bulk.fill_runs(row, cs, ce, Some(rgb_f), Some(rgb_b));
        for r in 0..vis {
            for c in 0..cols {
                assert_eq!(
                    per_cell.fg(r, c),
                    bulk.fg(r, c),
                    "fg mismatch at ({r},{c}) for case vis={vis} cols={cols} row={row} {cs}..{ce}"
                );
                assert_eq!(
                    per_cell.bg(r, c),
                    bulk.bg(r, c),
                    "bg mismatch at ({r},{c}) for case vis={vis} cols={cols} row={row} {cs}..{ce}"
                );
            }
        }
        assert_eq!(
            per_cell.heap_bytes(),
            bulk.heap_bytes(),
            "plane-allocation mismatch for case vis={vis} cols={cols} row={row} {cs}..{ce}"
        );
    }
}

#[test]
fn clear_range_matches_per_cell_clear() {
    fn clear_range_per_cell(ring: &mut RgbColorRing, row: u16, start_col: u16, end_col: u16) {
        if row >= ring.visible_rows {
            return;
        }
        for col in start_col..end_col {
            if col >= ring.stride {
                break;
            }
            let idx = ring.index(row, col);
            if let Some(fg) = ring.fg.as_deref_mut()
                && let Some(entry) = fg.get_mut(idx)
            {
                *entry = 0;
            }
            if let Some(bg) = ring.bg.as_deref_mut()
                && let Some(entry) = bg.get_mut(idx)
            {
                *entry = 0;
            }
        }
    }

    let cases: &[(u16, u16, u16, u16, u16, u16)] = &[
        (4, 10, 1, 3, 6, 0),
        (4, 10, 0, 0, 10, 0),
        (4, 10, 2, 5, 5, 0),
        (4, 10, 2, 7, 3, 0),
        (4, 10, 3, 6, 20, 0),
        (4, 10, 0, 10, 12, 0),
        (4, 10, 0, 12, 15, 0),
        (4, 10, 5, 2, 5, 0),
        (4, 10, 9, 2, 5, 0),
        (4, 10, u16::MAX, 0, 10, 2),
        (4, 10, 1, 0, 10, 2),
        (4, 10, 3, 2, 8, 3),
        (24, 8, 23, 0, 8, 0),
        (24, 8, 5, 2, 6, 7),
    ];
    for &(vis, cols, row, cs, ce, scroll) in cases {
        let mut seed = RgbColorRing::new(vis, cols);
        for r in 0..vis {
            for c in 0..cols {
                seed.set_fg(r, c, [r as u8, c as u8, 1]);
                seed.set_bg(r, c, [r as u8, c as u8, 2]);
            }
        }
        if scroll > 0 {
            seed.scroll_up(scroll);
        }
        let mut per_cell = seed.clone();
        let mut bulk = seed;
        clear_range_per_cell(&mut per_cell, row, cs, ce);
        bulk.clear_range(row, cs, ce);
        assert_eq!(
            per_cell.fg, bulk.fg,
            "fg plane mismatch for vis={vis} cols={cols} row={row} {cs}..{ce} scroll={scroll}"
        );
        assert_eq!(
            per_cell.bg, bulk.bg,
            "bg plane mismatch for vis={vis} cols={cols} row={row} {cs}..{ce} scroll={scroll}"
        );
        assert_eq!(
            per_cell.heap_bytes(),
            bulk.heap_bytes(),
            "plane-allocation mismatch for vis={vis} cols={cols} row={row} {cs}..{ce}"
        );
    }
}

#[test]
fn complex_clear_range_matches_per_cell_clear() {
    fn clear_range_per_cell(ring: &mut ComplexCharRing, row: u16, left: u16, right_excl: u16) {
        if row >= ring.visible_rows {
            return;
        }
        let base = (ring.ring_row(row) as usize) * (ring.stride as usize);
        for col in left..right_excl.min(ring.stride) {
            let idx = base + col as usize;
            if let Some(e) = ring.entries.get_mut(idx) {
                *e = '\0';
            }
        }
    }

    let cases: &[(u16, u16, u16, u16, u16, u16)] = &[
        (4, 10, 1, 3, 6, 0),
        (4, 10, 0, 0, 10, 0),
        (4, 10, 2, 5, 5, 0),
        (4, 10, 2, 7, 3, 0),
        (4, 10, 3, 6, 20, 0),
        (4, 10, 0, 10, 12, 0),
        (4, 10, 5, 2, 5, 0),
        (4, 10, 9, 2, 5, 0),
        (4, 10, u16::MAX, 0, 10, 2),
        (4, 10, 1, 0, 10, 2),
        (24, 8, 23, 0, 8, 0),
    ];
    for &(vis, cols, row, left, right, scroll) in cases {
        let mut seed = ComplexCharRing::new(vis, cols);
        for r in 0..vis {
            for c in 0..cols {
                let ch =
                    char::from_u32(0x1F600 + u32::from(r) * 32 + u32::from(c)).expect("valid char");
                seed.set(r, c, ch);
            }
        }
        if scroll > 0 {
            seed.scroll_up(scroll);
        }
        let mut per_cell = seed.clone();
        let mut bulk = seed;
        clear_range_per_cell(&mut per_cell, row, left, right);
        bulk.clear_range(row, left, right);
        assert_eq!(
            per_cell.entries, bulk.entries,
            "entries mismatch for vis={vis} cols={cols} row={row} {left}..{right} scroll={scroll}"
        );
    }
}

#[test]
fn test_render_data_for_cell_reads_hashmap_fallbacks() {
    let mut extras = CellExtras::new();
    let coord = CellCoord::new(1, 2);
    let entry = extras.get_or_create(coord);
    entry.set_complex_char(Some(std::sync::Arc::<str>::from("x\u{0301}")));
    entry.set_fg_rgb(Some([10, 20, 30]));
    entry.set_bg_rgb(Some([40, 50, 60]));
    entry.add_combining('\u{0301}');

    let cell = Cell::from_raw_parts(
        0,
        crate::PackedColors::new()
            .with_rgb_fg()
            .with_rgb_bg()
            .with_extras_flag(),
        crate::CellFlags::COMPLEX,
    );

    let data = extras.render_data_for_cell(1, 2, cell);
    let extra = data
        .cell_extra()
        .expect("HAS_EXTRAS should expose CellExtra");
    assert_eq!(data.complex_char(), Some('x'));
    assert_eq!(data.fg_rgb(), Some([10, 20, 30]));
    assert_eq!(data.bg_rgb(), Some([40, 50, 60]));
    assert_eq!(extra.combining(), &['\u{0301}']);
}

#[test]
fn test_render_data_for_cell_prefers_ring_values() {
    let mut extras = CellExtras::new();
    let coord = CellCoord::new(0, 1);
    let entry = extras.get_or_create(coord);
    entry.set_complex_char(Some(std::sync::Arc::<str>::from("A")));
    entry.set_fg_rgb(Some([90, 91, 92]));
    entry.set_bg_rgb(Some([93, 94, 95]));
    entry.add_combining('\u{0308}');

    extras.set_complex_char_ring(0, 1, '\u{1F680}', 2, 4);
    extras.set_rgb_ring_range(0, 1, 2, Some([1, 2, 3]), Some([4, 5, 6]), 2, 4);

    let cell = Cell::from_raw_parts(
        0,
        crate::PackedColors::new()
            .with_rgb_fg()
            .with_rgb_bg()
            .with_extras_flag(),
        crate::CellFlags::COMPLEX,
    );

    let data = extras.render_data_for_cell(0, 1, cell);
    let extra = data
        .cell_extra()
        .expect("HAS_EXTRAS should expose CellExtra");
    assert_eq!(data.complex_char(), Some('\u{1F680}'));
    assert_eq!(data.fg_rgb(), Some([1, 2, 3]));
    assert_eq!(data.bg_rgb(), Some([4, 5, 6]));
    assert_eq!(extra.combining(), &['\u{0308}']);
}

#[test]
fn per_cell_hyperlink_edit_does_not_leak_across_the_run() {
    const RUN_URL: &str = "https://example.com/run";
    let url: Arc<str> = Arc::from(RUN_URL);
    let id: Arc<str> = Arc::from("run-1");
    let mut extras = CellExtras::new();
    extras.set_range_uniform(
        0,
        0,
        4,
        &crate::extra::UniformExtras {
            fg_rgb: None,
            bg_rgb: None,
            underline_color: None,
            extended_flags: 0,
            hyperlink: Some(&Arc::new(crate::extra::HyperlinkData {
                url: url.clone(),
                id: Some(id.clone()),
            })),
        },
        24,
        80,
    );
    for col in 0..4 {
        let extra = extras
            .get(CellCoord::new(0, col))
            .expect("the bulk run must create an entry per covered cell");
        assert_eq!(extra.hyperlink().map(|u| &**u), Some(RUN_URL));
        assert_eq!(extra.hyperlink_id().map(|i| &**i), Some("run-1"));
    }

    let other: Arc<str> = Arc::from("https://example.com/other");
    extras
        .get_or_create(CellCoord::new(0, 1))
        .set_hyperlink(Some(other));
    let other_id: Arc<str> = Arc::from("run-2");
    extras
        .get_or_create(CellCoord::new(0, 2))
        .set_hyperlink_id(Some(other_id));

    let c1 = extras.get(CellCoord::new(0, 1)).expect("entry 1");
    assert_eq!(
        c1.hyperlink().map(|u| &**u),
        Some("https://example.com/other")
    );
    assert_eq!(c1.hyperlink_id(), None, "set_hyperlink clears the stale id");

    let c2 = extras.get(CellCoord::new(0, 2)).expect("entry 2");
    assert_eq!(c2.hyperlink().map(|u| &**u), Some(RUN_URL));
    assert_eq!(c2.hyperlink_id().map(|i| &**i), Some("run-2"));

    for col in [0u16, 3] {
        let extra = extras.get(CellCoord::new(0, col)).expect("untouched entry");
        assert_eq!(
            extra.hyperlink().map(|u| &**u),
            Some(RUN_URL),
            "col {col} lost its url to a neighbour's edit"
        );
        assert_eq!(
            extra.hyperlink_id().map(|i| &**i),
            Some("run-1"),
            "col {col} lost its id to a neighbour's edit"
        );
    }
}

#[test]
fn memory_used_counts_complex_char_ring_heap_bytes() {
    let mut extras = CellExtras::new();
    let before = extras.memory_used();
    extras.set_complex_char_ring(0, 0, '\u{1F680}', 24, 80);
    let after = extras.memory_used();
    assert_eq!(
        after - before,
        24 * 80 * std::mem::size_of::<char>(),
        "allocating the complex-char ring must account for its backing entries"
    );
}
