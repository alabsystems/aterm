// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_scrollback::{CellAttrs, Line, Rle};

fn compare_fillers(line: &Line, cols: u16, expect_fast: bool) {
    let mut fast = Grid::new(1, cols.max(1));
    let mut reference = Grid::new(1, cols.max(1));
    let text = line.as_str().expect("text fixture");
    let mut actual = Vec::new();
    let direct = fill_ascii_row_cells(
        fast.row_mut(0).unwrap(),
        text,
        line,
        cols,
        &mut |_, extra| actual.push(extra),
    );
    assert_eq!(direct.is_some(), expect_fast, "{text:?}, cols={cols}");
    if direct.is_none() {
        assert!(
            actual.is_empty(),
            "a rejected fast path must emit no extras"
        );
    }
    let filled = direct.unwrap_or_else(|| {
        fill_row_cells(
            fast.row_mut(0).unwrap(),
            text,
            line,
            cols,
            &mut |_, extra| actual.push(extra),
        )
    });
    let mut expected = Vec::new();
    let expected_filled = fill_row_cells_general(
        reference.row_mut(0).unwrap(),
        text,
        line,
        cols,
        &mut |_, extra| expected.push(extra),
    );
    assert_eq!(filled, expected_filled);
    assert_eq!(
        actual, expected,
        "restored extras for {text:?}, cols={cols}"
    );
    let (actual, expected) = (fast.row(0).unwrap(), reference.row(0).unwrap());
    assert_eq!(actual.as_slice(), expected.as_slice());
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual.flags(), expected.flags());
}

#[test]
fn scalar_ring_and_owned_graphemes_replace_without_stale_payloads() {
    let mut grid = Grid::new(2, 8);
    grid.restore_visible_from_lines(&[Line::from("👩‍💻"), Line::from("guard")]);
    let old = Arc::downgrade(grid.extras().complex_char_arc_for(0, 0).unwrap());
    grid.restore_visible_from_lines(&[Line::from("👩‍💻"), Line::from("guard")]);
    assert!(std::sync::Weak::ptr_eq(
        &old,
        &Arc::downgrade(grid.extras().complex_char_arc_for(0, 0).unwrap())
    ));
    grid.restore_visible_from_lines(&[Line::from("👨‍💻"), Line::from("guard")]);
    assert!(old.upgrade().is_none());
    let old = Arc::downgrade(grid.extras().complex_char_arc_for(0, 0).unwrap());
    grid.restore_visible_from_lines(&[Line::from("😀𐐀"), Line::from("guard")]);
    assert!(old.upgrade().is_none());
    assert_eq!(grid.row_text(0).as_deref(), Some("😀𐐀"));
    assert_eq!(grid.complex_char_at(0, 0), Some('😀'));
    assert_eq!(grid.complex_char_at(0, 2), Some('𐐀'));
    assert!(grid.extras().complex_char_arc_for(0, 0).is_none());
    grid.fill_row_from_line(0, &Line::from("👩‍💻"), 8);
    assert_eq!(grid.row_text(0).as_deref(), Some("👩‍💻"));
    assert_eq!(grid.complex_char_at(0, 2), None);
    let replaced = Arc::downgrade(grid.extras().complex_char_arc_for(0, 0).unwrap());
    grid.fill_row_from_line(0, &Line::from("plain"), 8);
    assert!(replaced.upgrade().is_none());
    for col in 0..8 {
        assert_eq!(grid.complex_char_at(0, col), None);
    }
    assert_eq!(grid.row_text(0).as_deref(), Some("plain"));
    assert_eq!(grid.row_text(1).as_deref(), Some("guard"));
}

#[test]
fn replacing_a_styled_row_with_plain_text_releases_excess_storage() {
    let cols = 80_u16;
    let attrs = CellAttrs::new(
        crate::PackedColor::rgb(12, 34, 56).0,
        crate::PackedColor::rgb(65, 43, 21).0,
        crate::CellFlags::BOLD.bits(),
    );
    let styled = Line::with_attrs(
        &"x".repeat(usize::from(cols)),
        Rle::with_value(attrs, u32::from(cols)),
    );
    let mut grid = Grid::new(1, cols);
    grid.restore_visible_from_lines(&[styled]);
    let full_capacity = grid.storage.extras.capacity();
    assert!(full_capacity >= usize::from(cols));

    grid.restore_visible_from_lines(&[Line::from("plain")]);
    assert_eq!(grid.storage.extras.len(), 0);
    assert!(grid.storage.extras.capacity() < full_capacity);
    let restored = grid.checkpoint_visible_lines();
    assert_eq!(restored[0].as_str(), Some("plain"));
    let mut attrs = restored[0].attr_cursor();
    for col in 0..5 {
        assert_eq!(attrs.attr_at(col), CellAttrs::DEFAULT);
    }
}

#[test]
fn streamed_visible_extras_match_detached_restoration() {
    let attrs = CellAttrs::new(
        crate::PackedColor::rgb(12, 34, 56).0,
        crate::PackedColor::rgb(65, 43, 21).0,
        crate::CellFlags::BOLD.bits(),
    );
    for text in [
        "ASCII",
        "😀𐐀👩‍💻e\u{301}",
        "e\u{301}日👩‍💻Z",
        "日e\u{301}\u{302}\u{303}\u{304}\u{305}X",
    ] {
        let mut line = Line::with_attrs(text, Rle::with_value(attrs, 32));
        line.set_wrapped(true);
        for cols in [1, 2, 3, 7, 16] {
            let mut detached = Grid::new(1, cols);
            let row = detached.row_mut(0).expect("fresh row");
            let extras = fill_row_into(row, &line, cols);
            let expected = Grid::row_to_line_with_stored_extras(row, &extras);
            let mut visible = Grid::new(1, cols);
            visible.restore_visible_from_lines(std::slice::from_ref(&line));
            let actual = visible.checkpoint_visible_lines();
            assert_eq!(actual[0].as_str(), expected.as_str(), "{text:?}, {cols}");
            assert_eq!(actual[0].is_wrapped(), expected.is_wrapped());
            let mut actual_attrs = actual[0].attr_cursor();
            let mut expected_attrs = expected.attr_cursor();
            for col in 0..usize::from(cols) {
                assert_eq!(
                    actual_attrs.attr_at(col),
                    expected_attrs.attr_at(col),
                    "{text:?}, width {cols}, column {col}"
                );
            }
        }
    }
}

#[test]
fn ascii_restore_matches_general_cells_styles_lengths_and_holes() {
    let palette = [
        CellAttrs::DEFAULT,
        CellAttrs::new(0x01_112233, 0x01_aabbcc, CellAttrs::OPENS_CELL),
        CellAttrs::new(
            crate::PackedColor::indexed(10).0,
            crate::PackedColor::indexed(3).0,
            crate::CellFlags::BOLD.bits(),
        ),
        CellAttrs::new(0x03_abcdef, 0x04_123456, crate::CellFlags::PROTECTED.bits()),
        CellAttrs::new(0xFF_123456, 0xFF_654321, crate::CellFlags::COMPLEX.bits()),
    ];
    for cols in [0_u16, 1, 2, 7, 80, 256] {
        for len in [0, cols.saturating_sub(1), cols] {
            let text: String = "A~ 09".chars().cycle().take(usize::from(len)).collect();
            compare_fillers(&Line::from(text.as_str()), cols, true);
            let mut attrs = Rle::new();
            for i in 0..usize::from(len) {
                attrs.push(palette[(i / 3) % palette.len()]);
            }
            compare_fillers(&Line::with_attrs(&text, attrs), cols, true);
            let mut short_attrs = Rle::new();
            for _ in 0..len / 2 {
                short_attrs.push(palette[1]);
            }
            compare_fillers(&Line::with_attrs(&text, short_attrs), cols, true);
        }
    }
}

#[test]
fn ascii_restore_does_not_scan_attributes_past_the_text() {
    let mut attrs = Rle::with_value(CellAttrs::DEFAULT, 1);
    for i in 0..4096 {
        attrs.push(CellAttrs::new(i % 2, 0, 0));
    }
    attrs.push(CellAttrs::new(0, 0, crate::CellFlags::WIDE.bits()));
    compare_fillers(&Line::with_attrs("a", attrs.clone()), 3, true);
    compare_fillers(&Line::with_attrs("", attrs), 3, true);
}

#[test]
fn ascii_restore_keeps_unicode_wide_controls_and_truncation_on_general_walk() {
    for text in [
        "a\u{0301}",
        "界",
        "a\u{200d}👩",
        "\t",
        "\x7f",
        "a\nb",
        "longer-than-row",
    ] {
        compare_fillers(&Line::from(text), 5, false);
    }
    let mut wide = Rle::new();
    wide.push(CellAttrs::new(
        0xFF_FFFFFF,
        0xFF_000000,
        crate::CellFlags::WIDE.bits(),
    ));
    compare_fillers(&Line::with_attrs("a", wide), 5, false);
}

#[test]
fn fill_row_from_line_replaces_exactly_its_row() {
    let text = |grid: &Grid, row: u16| -> String {
        (0..grid.cols())
            .map(|c| grid.cell(row, c).map_or('?', |cell| cell.char()))
            .collect()
    };
    let cases: &[(&str, &[u8], bool, &str)] = &[
        ("empty", b"", false, "     "),
        ("ascii", b"Hello", true, "Hello"),
        ("short", b"AB", false, "AB   "),
        ("truncated", b"LongTextThatExceedsColumns", false, "LongT"),
    ];
    for &(name, bytes, wrapped, want) in cases {
        let mut grid = Grid::new(3, 5);
        for row in 0..3 {
            grid.set_cursor(row, 0);
            grid.write_char('X');
        }
        let mut line = Line::from_bytes(bytes);
        line.set_wrapped(wrapped);
        grid.fill_row_from_line(1, &line, 5);
        assert_eq!(text(&grid, 1), want, "{name}");
        assert_eq!(grid.row(1).expect("row").is_wrapped(), wrapped, "{name}");
        assert_eq!(text(&grid, 0), "X    ", "{name}: row above");
        assert_eq!(text(&grid, 2), "X    ", "{name}: row below");
        grid.fill_row_from_line(99, &line, 5);
        assert_eq!(text(&grid, 2), "X    ", "{name}: out-of-range target");
    }
    let mut grid = Grid::new(1, 5);
    let line = Line::with_attrs("Hello", Rle::with_value(CellAttrs::DEFAULT, 5));
    grid.fill_row_from_line(0, &line, 5);
    assert_eq!(text(&grid, 0), "Hello", "default attrs");
}

#[test]
fn fill_row_hyperlinks_disjoint_are_correct_and_overlap_is_bounded() {
    use aterm_scrollback::HyperlinkSpan;
    let cols = 4u16;

    let mut grid = Grid::new(1, cols);
    let disjoint = vec![
        HyperlinkSpan::new(0, 2, Arc::from("a")),
        HyperlinkSpan::new(2, 4, Arc::from("b")),
    ];
    let line = Line::with_hyperlinks("wxyz", Rle::<CellAttrs>::new(), disjoint);
    grid.fill_row_from_line(0, &line, cols);
    let url = |g: &Grid, c: u16| {
        g.extras()
            .get(CellCoord::new(0, c))
            .and_then(|e| e.hyperlink())
            .map(|u| u.to_string())
    };
    assert_eq!(url(&grid, 0).as_deref(), Some("a"));
    assert_eq!(url(&grid, 1).as_deref(), Some("a"));
    assert_eq!(url(&grid, 2).as_deref(), Some("b"));
    assert_eq!(url(&grid, 3).as_deref(), Some("b"));

    let mut grid2 = Grid::new(1, cols);
    let overlap = vec![
        HyperlinkSpan::new(0, cols, Arc::from("first")),
        HyperlinkSpan::new(0, cols, Arc::from("second")),
        HyperlinkSpan::new(0, cols, Arc::from("third")),
    ];
    let line2 = Line::with_hyperlinks("wxyz", Rle::<CellAttrs>::new(), overlap);
    grid2.fill_row_from_line(0, &line2, cols);
    for c in 0..cols {
        assert_eq!(
            url(&grid2, c).as_deref(),
            Some("first"),
            "overlapping spans must stop after the budget (cols) is spent at col {c}"
        );
    }
}

#[test]
fn a_glyph_the_row_cannot_hold_takes_its_link_and_underline_colour() {
    use aterm_scrollback::{HyperlinkSpan, UnderlineColorSpan};
    for (text, cols, end, held) in [("\u{1F680}", 1u16, 2u16, 0u16), ("a\u{4E2D}", 2, 3, 1)] {
        let mut line = Line::with_hyperlinks(
            text,
            Rle::<CellAttrs>::new(),
            vec![HyperlinkSpan::new(
                0,
                end,
                Arc::from("https://example.com/"),
            )],
        );
        line.set_underline_colors(vec![UnderlineColorSpan::new(0, end, 0x0100_0001)]);

        let mut grid = Grid::new(1, cols);
        grid.fill_row_from_line(0, &line, cols);
        for col in 0..cols {
            let extra = grid.extras().get(CellCoord::new(0, col));
            let linked = extra.is_some_and(|e| e.hyperlink().is_some());
            let coloured = extra.is_some_and(|e| e.underline_color_u32().is_some());
            let claimed = grid.cell(0, col).is_some_and(crate::Cell::has_extras);
            let holds = col < held;
            assert_eq!(
                (linked, coloured, claimed),
                (holds, holds, holds),
                "fill_row_from_line {text:?} at {cols} columns, column {col}"
            );
        }

        let mut pages = crate::PageStore::new();
        // SAFETY: `pages` outlives `row`, both dropped at the end of the loop body.
        let mut row = unsafe { Row::new(cols, &mut pages) };
        let extras = fill_row_into(&mut row, &line, cols);
        let links: Vec<_> = extras
            .hyperlinks
            .iter()
            .map(|s| (s.start_col, s.end_col))
            .collect();
        let colours: Vec<u16> = extras.underline_colors.iter().map(|&(c, _)| c).collect();
        let want_links = if held > 0 { vec![(0, held)] } else { vec![] };
        assert_eq!(links, want_links, "fill_row_into {text:?} links");
        assert_eq!(
            colours,
            (0..held).collect::<Vec<_>>(),
            "fill_row_into {text:?} underline colours"
        );
    }
}
