// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Materialize/fill round-trip tests — scrollback Line ↔ Grid row fidelity (#4216).
//!
//! Migrated from aterm-core as part of #6556 Batch 2.

use super::super::*;

#[test]
fn materialize_from_line_roundtrip_ascii() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let mut grid = Grid::new(3, 10);

    let red_fg = PackedColor::indexed(1);
    let default_bg = PackedColor::DEFAULT_BG;
    let bold_flag = CellFlags::BOLD;
    for (col, c) in "Hello".chars().enumerate() {
        let cell = Cell::with_style(c, red_fg, default_bg, bold_flag);
        grid.row_mut(0).unwrap().set(col as u16, cell);
    }

    let row = grid.row(0).unwrap();
    let line = Grid::row_to_line_static(row);
    let mat = materialize_from_line(&line, 10);
    assert_eq!(mat.cells.len(), 10);

    for (i, c) in "Hello".chars().enumerate() {
        assert_eq!(mat.cells[i].char(), c, "char mismatch at col {i}");
        assert_eq!(
            mat.cells[i].fg_color().unwrap(),
            red_fg,
            "fg mismatch at col {i}"
        );
        assert_eq!(
            mat.cells[i].bg_color().unwrap(),
            default_bg,
            "bg mismatch at col {i}"
        );
        assert!(
            mat.cells[i].flags().contains(CellFlags::BOLD),
            "BOLD flag lost at col {i}"
        );
    }

    for i in 5..10 {
        assert_eq!(mat.cells[i], Cell::default(), "non-default cell at col {i}");
    }
}

#[test]
fn materialize_from_line_roundtrip_wide_chars() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let mut grid = Grid::new(3, 10);

    let fg = PackedColor::indexed(2);
    let bg = PackedColor::DEFAULT_BG;
    let wide_cell = Cell::with_style('世', fg, bg, CellFlags::WIDE);
    let cont_cell = Cell::with_style(' ', fg, bg, CellFlags::WIDE_CONTINUATION);
    grid.row_mut(0).unwrap().set(0, wide_cell);
    grid.row_mut(0).unwrap().set(1, cont_cell);

    let row = grid.row(0).unwrap();
    let line = Grid::row_to_line_static(row);
    let mat = materialize_from_line(&line, 10);

    assert_eq!(mat.cells[0].char(), '世');
    assert!(mat.cells[0].flags().contains(CellFlags::WIDE));
    assert_eq!(mat.cells[0].fg_color().unwrap(), fg);
    assert!(mat.cells[1].flags().contains(CellFlags::WIDE_CONTINUATION));
}

/// East-Asian-Ambiguous width is a write-time terminal policy. Once printed,
/// the main cell's WIDE bit and continuation are the geometry authority: moving
/// the row through Line-backed scrollback must not reclassify it with the
/// default narrow Unicode table.
#[test]
fn line_roundtrip_preserves_write_time_ambiguous_width() {
    use crate::grid::scroll_materialize::materialize_from_line;

    const AMBIGUOUS: char = '\u{00B7}'; // MIDDLE DOT, East Asian Width=A
    assert_eq!(aterm_grapheme::char_width(AMBIGUOUS), 1);
    assert_eq!(aterm_grapheme::char_width_cjk(AMBIGUOUS), 2);

    let fg = PackedColor::indexed(2);
    let bg = PackedColor::DEFAULT_BG;
    let mut wide_grid = Grid::new(2, 6);
    wide_grid
        .row_mut(0)
        .unwrap()
        .set(0, Cell::with_style(AMBIGUOUS, fg, bg, CellFlags::WIDE));
    wide_grid.row_mut(0).unwrap().set(
        1,
        Cell::with_style(' ', fg, bg, CellFlags::WIDE_CONTINUATION),
    );
    wide_grid
        .row_mut(0)
        .unwrap()
        .set(2, Cell::with_style('Z', fg, bg, CellFlags::empty()));

    let wide_line = Grid::row_to_line_static(wide_grid.row(0).unwrap());
    let materialized = materialize_from_line(&wide_line, 6);
    assert_eq!(materialized.cells[0].char(), AMBIGUOUS);
    assert!(materialized.cells[0].is_wide());
    assert!(materialized.cells[1].is_wide_continuation());
    assert_eq!(materialized.cells[2].char(), 'Z');

    let mut restored = Grid::new(2, 6);
    restored.fill_row_from_line(0, &wide_line, 6);
    assert_eq!(restored.cell(0, 0).unwrap().char(), AMBIGUOUS);
    assert!(restored.cell(0, 0).unwrap().is_wide());
    assert!(restored.is_wide_continuation_at(0, 1));
    assert_eq!(restored.cell(0, 2).unwrap().char(), 'Z');

    // Negative control: the same ambiguous scalar written narrow has no stored
    // WIDE bit, so both Line consumers keep it at one column.
    let mut narrow_grid = Grid::new(2, 6);
    narrow_grid
        .row_mut(0)
        .unwrap()
        .set(0, Cell::with_style(AMBIGUOUS, fg, bg, CellFlags::empty()));
    narrow_grid
        .row_mut(0)
        .unwrap()
        .set(1, Cell::with_style('Z', fg, bg, CellFlags::empty()));
    let narrow_line = Grid::row_to_line_static(narrow_grid.row(0).unwrap());
    let narrow = materialize_from_line(&narrow_line, 6);
    assert!(!narrow.cells[0].is_wide());
    assert_eq!(narrow.cells[1].char(), 'Z');
}

#[test]
fn materialize_keeps_decsca_protected_text() {
    // PROTECTED (DECSCA) shares bit 10 with WIDE_CONTINUATION. Row→line
    // materialization SKIPS wide-continuation spacers; it must NOT skip
    // protected glyphs, or DECSCA-protected text vanishes from scrollback
    // (and from `line`/search/copy of history). Regression for that collision.
    let mut grid = Grid::new(3, 12);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    for (col, c) in "SECRET".chars().enumerate() {
        let cell = Cell::with_style(c, fg, bg, CellFlags::PROTECTED);
        grid.row_mut(0).unwrap().set(col as u16, cell);
    }
    let row = grid.row(0).unwrap();
    let line = Grid::row_to_line_static(row);
    let text: String = line.to_string().chars().take(6).collect();
    assert_eq!(
        text, "SECRET",
        "DECSCA-protected text must survive scrollback materialization"
    );
}

#[test]
fn materialize_drops_spacer_but_keeps_protected_neighbor() {
    // Disambiguation: a real spacer (after a WIDE lead) collapses away; a
    // protected cell that merely shares bit 10 is kept. Lays out 世<spacer>!
    // where `!` is PROTECTED — the line must read "世!".
    let mut grid = Grid::new(3, 12);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    grid.row_mut(0)
        .unwrap()
        .set(0, Cell::with_style('世', fg, bg, CellFlags::WIDE));
    grid.row_mut(0).unwrap().set(
        1,
        Cell::with_style(' ', fg, bg, CellFlags::WIDE_CONTINUATION),
    );
    grid.row_mut(0)
        .unwrap()
        .set(2, Cell::with_style('!', fg, bg, CellFlags::PROTECTED));
    let row = grid.row(0).unwrap();
    let line = Grid::row_to_line_static(row);
    let text: String = line.to_string().chars().take_while(|c| *c != ' ').collect();
    assert_eq!(
        text, "世!",
        "drop the real spacer, keep the protected glyph"
    );
}

#[test]
fn materialize_from_line_empty_line() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let line = Line::new();
    let mat = materialize_from_line(&line, 5);
    assert_eq!(mat.cells.len(), 5);
    for cell in &mat.cells {
        assert_eq!(*cell, Cell::default());
    }
}

#[test]
fn materialize_from_line_end_to_end_styled_scrollback() {
    let mut grid = Grid::with_scrollback(3, 10, 100);

    let blue_fg = PackedColor::indexed(4);
    let default_bg = PackedColor::DEFAULT_BG;
    let cell = Cell::with_style('A', blue_fg, default_bg, CellFlags::empty());
    grid.row_mut(0).unwrap().set(0, cell);

    for _ in 0..5 {
        grid.line_feed();
    }

    assert!(grid.scrollback_lines() > 0);

    let mut found = false;
    for rev_idx in 0..grid.scrollback_lines() {
        if let Some(mat) = grid.materialize_scrollback_row_full(rev_idx, 10)
            && mat.cells[0].char() == 'A'
        {
            assert_eq!(
                mat.cells[0].fg_color().unwrap(),
                blue_fg,
                "fg color lost in scrollback"
            );
            found = true;
            break;
        }
    }
    assert!(
        found,
        "styled 'A' cell not found in materialized scrollback"
    );
}

#[test]
fn materialize_from_line_restores_hyperlinks() {
    use crate::grid::scroll_materialize::materialize_from_line;
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(4, 10, 10);

    let url: Arc<str> = Arc::from("https://example.com");
    grid.set_cursor(0, 0);
    for c in "Link!".chars() {
        grid.write_char(c);
    }
    for col in 0..5u16 {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }

    for _ in 0..4 {
        grid.line_feed();
    }

    let ring_sb = grid.storage.ring_buffer_scrollback();
    let line = grid.history_line_rev(ring_sb - 1).expect("scrollback line");
    let row = materialize_from_line(&line, 10);

    for col in 0..5u16 {
        let extra = row.get_extra(col);
        assert!(
            extra.is_some(),
            "column {col} should have extras with hyperlink"
        );
        assert_eq!(
            extra.unwrap().hyperlink().map(|u| &**u),
            Some("https://example.com"),
            "hyperlink URL mismatch at col {col}"
        );
    }
    let no_link = row.get_extra(5).and_then(|e| e.hyperlink()).map(|u| &**u);
    assert_eq!(no_link, None, "column 5 should not have hyperlink");
}

#[test]
fn materialize_from_line_restores_emoji() {
    use crate::grid::scroll_materialize::materialize_from_line;
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(4, 10, 10);

    grid.set_cursor(0, 0);
    grid.write_char('A');

    let emoji_str: Arc<str> = Arc::from("🎉");
    let mut cell = Cell::with_style(
        ' ',
        PackedColor::indexed(1),
        PackedColor::DEFAULT_BG,
        CellFlags::WIDE,
    );
    cell.set_overflow_index(0);
    grid.row_mut(0).unwrap().set(1, cell);
    let cont = Cell::with_style(
        ' ',
        PackedColor::indexed(1),
        PackedColor::DEFAULT_BG,
        CellFlags::WIDE_CONTINUATION,
    );
    grid.row_mut(0).unwrap().set(2, cont);
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 1))
        .set_complex_char(Some(emoji_str));

    for _ in 0..4 {
        grid.line_feed();
    }

    let ring_sb = grid.storage.ring_buffer_scrollback();
    let line = grid.history_line_rev(ring_sb - 1).expect("scrollback line");
    let row = materialize_from_line(&line, 10);

    let extra = row.get_extra(1);
    assert!(extra.is_some(), "col 1 should have extras for emoji");
    let complex = extra.unwrap().complex_char();
    assert_eq!(
        complex.map(|s| &**s),
        Some("🎉"),
        "emoji should be preserved in complex_char"
    );
    assert!(
        row.cells[1].is_complex(),
        "emoji cell should have COMPLEX flag"
    );
}

#[test]
fn materialize_from_line_restores_rgb_colors() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let mut grid = Grid::with_scrollback(4, 10, 10);

    let rgb_fg = PackedColor::rgb(255, 0, 0);
    let default_bg = PackedColor::DEFAULT_BG;
    let cell = Cell::with_style('R', rgb_fg, default_bg, CellFlags::empty());
    grid.row_mut(0).unwrap().set(0, cell);
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 0))
        .set_fg_rgb(Some([255, 0, 0]));

    for _ in 0..4 {
        grid.line_feed();
    }

    let ring_sb = grid.storage.ring_buffer_scrollback();
    let line = grid.history_line_rev(ring_sb - 1).expect("scrollback line");
    let row = materialize_from_line(&line, 10);

    let extra = row.get_extra(0);
    assert!(extra.is_some(), "col 0 should have extras for RGB");
    assert_eq!(
        extra.unwrap().fg_rgb(),
        Some([255, 0, 0]),
        "RGB red fg should be preserved"
    );
    assert!(
        row.cells[0].fg_needs_overflow(),
        "RGB cell fg should need overflow"
    );
}

#[test]
fn materialize_wide_char_at_last_col_does_not_corrupt_rgb() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let blue_fg = CellAttrs::new(0x01_00_00_FF, 0xFF_00_00_00, 0);
    let default_attrs = CellAttrs::DEFAULT;
    let red_fg = CellAttrs::new(0x01_FF_00_00, 0xFF_00_00_00, 0);

    let attrs: Rle<CellAttrs> = [blue_fg, default_attrs, red_fg].into_iter().collect();
    let line = Line::with_attrs("AB全", attrs);

    let row = materialize_from_line(&line, 3);

    let extra0 = row.get_extra(0);
    assert!(extra0.is_some(), "col 0 should have extras for blue RGB");
    assert_eq!(
        extra0.unwrap().fg_rgb(),
        Some([0, 0, 255]),
        "col 0 should have blue RGB, not red from dropped wide char"
    );

    let extra1 = row.get_extra(1);
    let has_rgb = extra1.is_some_and(|e| e.fg_rgb().is_some());
    assert!(!has_rgb, "col 1 should not have RGB extras");

    let extra2 = row.get_extra(2);
    let has_rgb2 = extra2.is_some_and(|e| e.fg_rgb().is_some());
    assert!(
        !has_rgb2,
        "col 2 should not have RGB from dropped wide char"
    );
}

/// VS16 presentation width survives materialization: a scrolled-back `❤️Z`
/// keeps the heart as the 2-column emoji-presentation cell the live writer
/// made it (WIDE + continuation), so `Z` stays at column 2 — no column shift
/// after the line enters scrollback.
#[test]
fn materialize_preserves_vs16_widened_cell() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let attrs: Rle<CellAttrs> = [CellAttrs::DEFAULT; 3].into_iter().collect();
    let line = Line::with_attrs("\u{2764}\u{FE0F}Z", attrs);
    let row = materialize_from_line(&line, 10);

    assert!(
        row.cells[0].is_wide(),
        "❤️ must materialize WIDE (the live VS16 widening)"
    );
    assert!(
        row.cells[1].is_wide_continuation(),
        "col 1 must be the wide continuation spacer"
    );
    assert_eq!(row.cells[2].char(), 'Z', "Z stays at col 2 as it was live");
    let complex = row.get_extra(0).and_then(|e| e.complex_char().cloned());
    assert_eq!(
        complex.as_deref(),
        Some("\u{2764}\u{FE0F}"),
        "the full heart + VS16 cluster rides the complex char"
    );
}

/// The live writer's row-edge exception survives materialization: a VS16 heart
/// whose base sits at the LAST column was never widened live (no room for the
/// continuation spacer — `widen_previous_cell_for_vs16` bails), so the restore
/// must keep it as that narrow cell instead of dropping an "unfittable" wide.
#[test]
fn materialize_keeps_vs16_heart_narrow_at_last_column() {
    use crate::grid::scroll_materialize::materialize_from_line;

    // 3-wide row: "AB❤️" — the heart's base is at the last column (col 2).
    let attrs: Rle<CellAttrs> = [CellAttrs::DEFAULT; 4].into_iter().collect();
    let line = Line::with_attrs("AB\u{2764}\u{FE0F}", attrs);
    let row = materialize_from_line(&line, 3);

    assert_eq!(row.cells[0].char(), 'A');
    assert_eq!(row.cells[1].char(), 'B');
    assert!(
        !row.cells[2].is_wide(),
        "the edge-pinned heart stays NARROW, as it was live"
    );
    let complex = row.get_extra(2).and_then(|e| e.complex_char().cloned());
    assert_eq!(
        complex.as_deref(),
        Some("\u{2764}\u{FE0F}"),
        "the heart is PRESENT (not dropped as an unfittable wide glyph)"
    );
}

/// VS15 presentation width survives materialization: a scrolled-back `⌚︎Z`
/// keeps the watch NARROW (the live VS15 narrowing), so `Z` stays at column 1.
#[test]
fn materialize_preserves_vs15_narrowed_cell() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let attrs: Rle<CellAttrs> = [CellAttrs::DEFAULT; 3].into_iter().collect();
    let line = Line::with_attrs("\u{231A}\u{FE0E}Z", attrs);
    let row = materialize_from_line(&line, 10);

    assert!(
        !row.cells[0].is_wide(),
        "⌚︎ must materialize NARROW (the live VS15 narrowing)"
    );
    assert!(
        !row.cells[1].is_wide_continuation(),
        "no continuation spacer after a narrowed cell"
    );
    assert_eq!(row.cells[1].char(), 'Z', "Z stays at col 1 as it was live");
}

/// The skin-tone fold gates on effective width end-to-end: a text-presentation
/// `☝🏽Z` materializes as the SPLIT layout the live writer produced (narrow ☝ at
/// col 0, the modifier as its own wide cell at cols 1-2, Z at col 3) — not a
/// folded single cell that would pull `Z` two columns left.
#[test]
fn materialize_keeps_unfolded_skin_tone_split() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let attrs: Rle<CellAttrs> = [CellAttrs::DEFAULT; 3].into_iter().collect();
    let line = Line::with_attrs("\u{261D}\u{1F3FD}Z", attrs);
    let row = materialize_from_line(&line, 10);

    assert!(!row.cells[0].is_wide(), "text-presentation ☝ stays narrow");
    assert!(
        row.cells[1].is_wide(),
        "the standalone modifier is its own wide cell (live fall-through)"
    );
    assert!(row.cells[2].is_wide_continuation());
    assert_eq!(row.cells[3].char(), 'Z', "Z stays at col 3 as it was live");

    // And the VS16-widened variant folds into ONE wide cell, Z at col 2.
    let attrs: Rle<CellAttrs> = [CellAttrs::DEFAULT; 4].into_iter().collect();
    let line = Line::with_attrs("\u{261D}\u{FE0F}\u{1F3FD}Z", attrs);
    let row = materialize_from_line(&line, 10);
    assert!(
        row.cells[0].is_wide(),
        "VS16-widened ☝️🏽 folds into one wide cell"
    );
    assert!(row.cells[1].is_wide_continuation());
    assert_eq!(row.cells[2].char(), 'Z');
}

// ============================================================================
// materialize_from_line untested code paths (#4234)
// ============================================================================

#[test]
fn materialize_from_line_zwj_family_emoji() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let zwj_text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let attr_count = zwj_text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(zwj_text, attrs);

    let row = materialize_from_line(&line, 10);

    let extra = row.get_extra(0);
    assert!(extra.is_some(), "col 0 should have extras for ZWJ emoji");
    let complex = extra.unwrap().complex_char();
    assert_eq!(
        complex.map(|s| &**s),
        Some(zwj_text),
        "ZWJ family emoji should be preserved as single complex_char"
    );
    assert!(
        row.cells[0].flags().contains(CellFlags::WIDE),
        "ZWJ emoji cell should be WIDE"
    );
    assert!(
        row.cells[1].flags().contains(CellFlags::WIDE_CONTINUATION),
        "ZWJ emoji continuation should be WIDE_CONTINUATION"
    );
}

#[test]
fn materialize_from_line_combining_mark() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let text = "e\u{0301}X";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);

    let row = materialize_from_line(&line, 10);

    let extra = row.get_extra(0);
    assert!(
        extra.is_some(),
        "col 0 should have extras for combining mark"
    );
    let complex = extra.unwrap().complex_char();
    assert_eq!(
        complex.map(|s| &**s),
        Some("e\u{0301}"),
        "combining mark should be preserved in complex_char"
    );

    assert_eq!(row.cells[1].char(), 'X', "col 1 should be 'X'");
}

#[test]
fn materialize_from_line_rgb_background() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let green_bg = CellAttrs::new(0xFF_FF_FF_FF, 0x01_00_FF_00, 0);
    let attrs: Rle<CellAttrs> = std::iter::once(green_bg).collect();
    let line = Line::with_attrs("B", attrs);

    let row = materialize_from_line(&line, 5);

    let extra = row.get_extra(0);
    assert!(extra.is_some(), "col 0 should have extras for RGB bg");
    assert_eq!(
        extra.unwrap().bg_rgb(),
        Some([0, 255, 0]),
        "RGB green background should be preserved"
    );
    assert_eq!(
        extra.unwrap().fg_rgb(),
        None,
        "default fg should not produce RGB extras"
    );
}

#[test]
fn materialize_from_line_mixed_content() {
    use crate::grid::scroll_materialize::materialize_from_line;
    use std::sync::Arc;

    let rgb_red = CellAttrs::new(0x01_FF_00_00, 0xFF_00_00_00, 0);
    let default_a = CellAttrs::DEFAULT;
    let rgb_blue = CellAttrs::new(0x01_00_00_FF, 0xFF_00_00_00, 0);
    let attrs: Rle<CellAttrs> = [rgb_red, default_a, rgb_blue].into_iter().collect();

    let url: Arc<str> = Arc::from("https://example.com");
    let hyperlinks = vec![HyperlinkSpan::new(2, 4, url)];
    let line = Line::with_hyperlinks("\u{1F389}AB", attrs, hyperlinks);

    let row = materialize_from_line(&line, 10);

    let e0 = row.get_extra(0).expect("col 0 should have extras");
    assert!(
        e0.complex_char().is_some(),
        "col 0 should have complex_char for emoji"
    );
    assert_eq!(e0.fg_rgb(), Some([255, 0, 0]), "col 0 should have red RGB");

    assert!(
        row.cells[1].flags().contains(CellFlags::WIDE_CONTINUATION),
        "col 1 should be wide continuation"
    );

    let e2 = row
        .get_extra(2)
        .expect("col 2 should have hyperlink extras");
    assert_eq!(
        e2.hyperlink().map(|u| &**u),
        Some("https://example.com"),
        "col 2 should have hyperlink"
    );
    assert_eq!(e2.fg_rgb(), None, "col 2 should not have RGB");

    let e3 = row.get_extra(3).expect("col 3 should have extras");
    assert_eq!(
        e3.hyperlink().map(|u| &**u),
        Some("https://example.com"),
        "col 3 should have hyperlink"
    );
    assert_eq!(e3.fg_rgb(), Some([0, 0, 255]), "col 3 should have blue RGB");
}

#[test]
fn materialize_from_line_preserves_hyperlink_id() {
    use crate::grid::scroll_materialize::materialize_from_line;
    use std::sync::Arc;

    let url: Arc<str> = Arc::from("https://example.com");
    let id: Option<Arc<str>> = Some(Arc::from("link-42"));
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, 4).collect();
    let hyperlinks = vec![HyperlinkSpan::with_id(0, 4, url.clone(), id.clone())];
    let line = Line::with_hyperlinks("ABCD", attrs, hyperlinks);

    let row = materialize_from_line(&line, 10);

    for col in 0..4u16 {
        let extra = row.get_extra(col).expect("col should have extras");
        assert_eq!(
            extra.hyperlink().map(|u| &**u),
            Some("https://example.com"),
            "col {col} should have hyperlink URL"
        );
        assert_eq!(
            extra.hyperlink_id().map(|id| &**id),
            Some("link-42"),
            "col {col} should have hyperlink ID preserved"
        );
    }
}

#[test]
fn fill_row_from_line_preserves_hyperlink_id() {
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(4, 10, 0);
    let url: Arc<str> = Arc::from("https://example.com");
    let id: Option<Arc<str>> = Some(Arc::from("link-99"));
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, 3).collect();
    let hyperlinks = vec![HyperlinkSpan::with_id(0, 3, url.clone(), id.clone())];
    let line = Line::with_hyperlinks("XYZ", attrs, hyperlinks);

    grid.fill_row_from_line(0, &line, 10);

    for col in 0..3u16 {
        let extra = grid.storage.extras.get(CellCoord::new(0, col));
        let extra = extra.expect("col should have extras");
        assert_eq!(
            extra.hyperlink().map(|u| &**u),
            Some("https://example.com"),
            "col {col} should have hyperlink URL"
        );
        assert_eq!(
            extra.hyperlink_id().map(|id| &**id),
            Some("link-99"),
            "col {col} should have hyperlink ID preserved"
        );
    }
}

#[test]
fn hyperlink_id_survives_full_grid_scrollback_grid_roundtrip() {
    use crate::grid::scroll_materialize::materialize_from_line;
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(4, 10, 10);

    let url: Arc<str> = Arc::from("https://example.com/roundtrip");
    let id: Arc<str> = Arc::from("rt-id-7");
    grid.set_cursor(0, 0);
    for c in "OSC8".chars() {
        grid.write_char(c);
    }
    for col in 0..4u16 {
        let extra = grid.extras_mut().get_or_create(CellCoord::new(0, col));
        extra.set_hyperlink(Some(url.clone()));
        extra.set_hyperlink_id(Some(id.clone()));
    }

    for _ in 0..4 {
        grid.line_feed();
    }

    let ring_sb = grid.storage.ring_buffer_scrollback();
    let line = grid.history_line_rev(ring_sb - 1).expect("scrollback line");
    let row = materialize_from_line(&line, 10);

    for col in 0..4u16 {
        let extra = row
            .get_extra(col)
            .unwrap_or_else(|| panic!("column {col} should have extras"));
        assert_eq!(
            extra.hyperlink().map(|u| &**u),
            Some("https://example.com/roundtrip"),
            "hyperlink URL lost at col {col}",
        );
        assert_eq!(
            extra.hyperlink_id().map(|id| &**id),
            Some("rt-id-7"),
            "hyperlink ID lost at col {col} — forward or reverse path bug",
        );
    }
    let no_link = row.get_extra(4).and_then(|e| e.hyperlink());
    assert!(no_link.is_none(), "column 4 should not have hyperlink");
}

#[test]
fn materialize_from_line_truncation() {
    use crate::grid::scroll_materialize::materialize_from_line;
    use std::sync::Arc;

    let rgb_fg = CellAttrs::new(0x01_FF_00_00, 0xFF_00_00_00, 0);
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(rgb_fg, 8).collect();
    let url: Arc<str> = Arc::from("https://example.com");
    let hyperlinks = vec![HyperlinkSpan::new(2, 7, url)];
    let line = Line::with_hyperlinks("ABCDEFGH", attrs, hyperlinks);

    let row = materialize_from_line(&line, 4);

    assert_eq!(row.cells.len(), 4, "row should have 4 cells");

    for (i, expected) in ['A', 'B', 'C', 'D'].iter().enumerate() {
        assert_eq!(row.cells[i].char(), *expected, "char mismatch at col {i}");
    }

    for i in 0..4 {
        let extra = row.get_extra(i as u16);
        assert!(extra.is_some(), "col {i} should have RGB extras");
        assert_eq!(
            extra.unwrap().fg_rgb(),
            Some([255, 0, 0]),
            "col {i} should have red RGB"
        );
    }

    for col in 0..2u16 {
        let has_link = row.get_extra(col).and_then(|e| e.hyperlink()).is_some();
        assert!(!has_link, "col {col} should not have hyperlink");
    }
    for col in 2..4u16 {
        let link = row.get_extra(col).and_then(|e| e.hyperlink()).map(|u| &**u);
        assert_eq!(
            link,
            Some("https://example.com"),
            "col {col} should have clamped hyperlink"
        );
    }
}

/// Wide chars + hyperlinks: scrollback round-trip must use physical columns.
#[test]
fn scrollback_hyperlink_wide_char_round_trip() {
    use crate::grid::scroll_materialize::materialize_from_line;
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(4, 10, 10);
    let url: Arc<str> = Arc::from("https://wide.test");

    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.write_wide_char_wrap_with_style_id('\u{4E2D}', StyleId::default(), CellFlags::empty());
    grid.write_char('D');

    for col in [0u16, 1, 3] {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }

    for _ in 0..4 {
        grid.line_feed();
    }

    let ring_sb = grid.storage.ring_buffer_scrollback();
    let line = grid.history_line_rev(ring_sb - 1).expect("scrollback line");
    let row = materialize_from_line(&line, 10);

    for col in 0..4u16 {
        let link = row.get_extra(col).and_then(|e| e.hyperlink()).map(|u| &**u);
        assert_eq!(
            link,
            Some("https://wide.test"),
            "physical col {col} should have hyperlink after wide char round-trip"
        );
    }
    let no_link = row.get_extra(4).and_then(|e| e.hyperlink());
    assert!(no_link.is_none(), "col 4 should not have hyperlink");
}

/// Two different URLs separated by a wide char must produce physical-column spans.
#[test]
fn extract_row_extras_hyperlink_spans_use_physical_columns() {
    use crate::grid::scroll_convert::ScrolledRowExtras;
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(4, 10, 10);
    let url1: Arc<str> = Arc::from("https://first.test");
    let url2: Arc<str> = Arc::from("https://second.test");

    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.write_wide_char_wrap_with_style_id('\u{4E2D}', StyleId::default(), CellFlags::empty());
    grid.write_char('D');

    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 0))
        .set_hyperlink(Some(url1));
    for col in [1u16, 3] {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url2.clone()));
    }

    let row = grid.row(0).expect("row 0 should exist");
    let extracted: ScrolledRowExtras =
        Grid::extract_row_extras(row, grid.extras(), 0, grid.styles());
    assert_eq!(extracted.hyperlinks.len(), 2, "expected 2 hyperlink spans");

    let s1 = &extracted.hyperlinks[0];
    assert_eq!((s1.start_col, s1.end_col), (0, 1), "span1 should be [0,1)");
    assert_eq!(&*s1.url, "https://first.test");

    let s2 = &extracted.hyperlinks[1];
    assert_eq!(
        (s2.start_col, s2.end_col),
        (1, 4),
        "span2 should be [1,4) not [1,3)"
    );
    assert_eq!(&*s2.url, "https://second.test");
}

// ============================================================================
// advance_grapheme_unit optimization tests (#5951)
// ============================================================================

/// Mixed-content round-trip after allocation elimination (#5951).
#[test]
fn materialize_mixed_content_allocation_free_path() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let text = "A中🎉e\u{0301}B";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);

    let row = materialize_from_line(&line, 20);

    assert_eq!(row.cells[0].char(), 'A', "col 0 should be 'A'");
    assert!(
        row.get_extra(0).is_none() || row.get_extra(0).unwrap().complex_char().is_none(),
        "col 0 should have no complex_char"
    );

    assert!(
        row.cells[1].flags().contains(CellFlags::WIDE),
        "col 1 should be WIDE for '中'"
    );
    assert!(
        row.cells[2].flags().contains(CellFlags::WIDE_CONTINUATION),
        "col 2 should be WIDE_CONTINUATION"
    );

    let emoji_extra = row.get_extra(3);
    assert!(emoji_extra.is_some(), "col 3 should have extras for emoji");
    assert_eq!(
        emoji_extra.unwrap().complex_char().map(|s| &**s),
        Some("🎉"),
        "col 3 complex_char should be 🎉"
    );
    assert!(
        row.cells[3].flags().contains(CellFlags::WIDE),
        "col 3 should be WIDE"
    );
    assert!(
        row.cells[4].flags().contains(CellFlags::WIDE_CONTINUATION),
        "col 4 should be WIDE_CONTINUATION"
    );

    let combining_extra = row.get_extra(5);
    assert!(
        combining_extra.is_some(),
        "col 5 should have extras for combining mark"
    );
    assert_eq!(
        combining_extra.unwrap().complex_char().map(|s| &**s),
        Some("e\u{0301}"),
        "col 5 complex_char should be e + combining acute"
    );

    assert_eq!(row.cells[6].char(), 'B', "col 6 should be 'B'");

    assert_eq!(row.len(), 7, "row should have 7 occupied columns");
}

/// `fill_row_from_line` round-trip with mixed content (#5951).
#[test]
fn fill_row_mixed_content_allocation_free_path() {
    let text = "A中🎉e\u{0301}B";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);

    let mut grid = Grid::new(3, 20);
    grid.fill_row_from_line(0, &line, 20);

    let row = grid.row(0).expect("row 0 should exist");

    assert_eq!(
        row.get(0).expect("col 0").char(),
        'A',
        "col 0 should be 'A'"
    );

    assert!(
        row.get(1).expect("col 1").flags().contains(CellFlags::WIDE),
        "col 1 should be WIDE for '中'"
    );
    assert!(
        row.get(2)
            .expect("col 2")
            .flags()
            .contains(CellFlags::WIDE_CONTINUATION),
        "col 2 should be WIDE_CONTINUATION"
    );

    let emoji_extra = grid.storage.extras.get(CellCoord::new(0, 3));
    assert!(emoji_extra.is_some(), "col 3 should have extras for emoji");
    assert_eq!(
        emoji_extra.unwrap().complex_char().map(|s| &**s),
        Some("🎉"),
        "col 3 complex_char should be 🎉"
    );

    let combining_extra = grid.storage.extras.get(CellCoord::new(0, 5));
    assert!(
        combining_extra.is_some(),
        "col 5 should have extras for combining mark"
    );

    assert_eq!(
        row.get(6).expect("col 6").char(),
        'B',
        "col 6 should be 'B'"
    );
}
