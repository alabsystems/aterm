// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{Cell, CellFlags, PackedColor};
use aterm_scrollback::{Rle, Scrollback};

#[test]
fn viewport_restore_clears_once_and_matches_individual_rows() {
    let attrs = aterm_scrollback::CellAttrs::new(
        PackedColor::rgb(12, 34, 56).0,
        PackedColor::rgb(65, 43, 21).0,
        CellFlags::BOLD.bits(),
    );
    let old = vec![Line::with_attrs("abcdefghijklmnop", Rle::with_value(attrs, 16)); 8];
    let new = vec![Line::with_attrs("e\u{301}日👩‍💻", Rle::with_value(attrs, 16)); 8];
    for offset in [0, 1, 8] {
        for count in [0, 1, 8] {
            let mut actual = Grid::new(8, 16);
            let mut reference = Grid::new(8, 16);
            for grid in [&mut actual, &mut reference] {
                grid.restore_visible_from_lines(&old);
                grid.storage.ring_head = 3;
                grid.storage.display_offset = offset;
            }
            let entries = actual.storage.extras.len();
            assert!(entries > 0);
            crate::test_counters::take_extras_clear_ops();
            actual.restore_visible_from_lines(&new[..count]);
            assert!(
                crate::test_counters::take_extras_clear_ops() <= entries,
                "one viewport restore must not rescan the extras map for every row"
            );
            for row in 0..8 {
                if let Some(line) = new[..count].get(usize::from(row)) {
                    reference.fill_row_from_line(row, line, 16);
                } else if let Some(r) = reference.row_mut(row) {
                    r.clear();
                    reference.storage.extras.clear_row(row);
                }
            }
            let reference_ops = crate::test_counters::take_extras_clear_ops();
            if offset == 0 && count == 8 {
                assert!(
                    reference_ops > entries,
                    "individual clears must break the work bound"
                );
            }
            let actual_lines = actual.checkpoint_visible_lines();
            let expected_lines = reference.checkpoint_visible_lines();
            for (a, e) in actual_lines.iter().zip(&expected_lines) {
                assert_eq!(a.as_str(), e.as_str(), "offset={offset}, count={count}");
                let mut a_attrs = a.attr_cursor();
                let mut e_attrs = e.attr_cursor();
                for col in 0..16 {
                    assert_eq!(a_attrs.attr_at(col), e_attrs.attr_at(col));
                }
            }
            for row in 0..8 {
                for col in 0..16 {
                    assert_eq!(
                        actual.storage.extras.fg_rgb_for(row, col),
                        reference.storage.extras.fg_rgb_for(row, col)
                    );
                    assert_eq!(
                        actual.storage.extras.bg_rgb_for(row, col),
                        reference.storage.extras.bg_rgb_for(row, col)
                    );
                }
            }
        }
    }
}

/// Build a plain-text [`Line`] for scrollback fixtures.
fn plain_line(text: &str) -> Line {
    Line::with_attrs(text, Rle::new())
}

/// Round-trip a grid through the line codec the way
/// `Terminal::from_checkpoint` does: a fresh grid with the scrollback
/// re-attached (oldest first) and the visible rows restored.
fn rebuild_from_lines(rows: u16, cols: u16, lines: &[Line]) -> Grid {
    let total = lines.len();
    let visible_start = total.saturating_sub(rows as usize);
    let (scrollback_lines, visible_lines) = lines.split_at(visible_start);

    let mut sb = Scrollback::with_defaults();
    sb.set_line_limit(None);
    for line in scrollback_lines {
        sb.push_line(line.clone());
    }

    let mut grid = Grid::with_tiered_scrollback(rows, cols, 1000, sb);
    grid.restore_visible_from_lines(visible_lines);
    grid
}

/// Materialize a grid's visible rows to plain strings (trailing spaces
/// trimmed) for content comparison.
fn visible_text(grid: &Grid) -> Vec<String> {
    (0..grid.rows())
        .map(|r| {
            let row = grid.row(r).expect("row in range");
            let len = row.len() as usize;
            let cells = &row.as_slice()[..len];
            let mut s = String::new();
            let mut col = 0usize;
            while col < cells.len() {
                let cell = cells[col];
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }
                if cell.is_complex() {
                    if let Some(extra) = grid.extras().get(crate::CellCoord::new(r, col as u16))
                        && let Some(cx) = extra.complex_char()
                    {
                        s.push_str(cx);
                    }
                } else {
                    s.push(cell.char());
                    if let Some(extra) = grid.extras().get(crate::CellCoord::new(r, col as u16)) {
                        for m in extra.combining() {
                            s.push(*m);
                        }
                    }
                }
                col += 1;
            }
            s.trim_end().to_string()
        })
        .collect()
}

fn write_text(grid: &mut Grid, row: u16, text: &str) {
    for (col, ch) in text.chars().enumerate() {
        grid.set_cell(row, col as u16, Cell::new(ch));
    }
}

#[test]
fn visible_checkpoint_excludes_scrollback_and_restores_visible_rows() {
    let mut g = Grid::with_scrollback(3, 20, 100);
    let mut sb = Scrollback::with_defaults();
    sb.set_line_limit(None);
    for i in 0..40 {
        sb.push_line(plain_line(&format!("history-{i}")));
    }
    g.attach_scrollback(sb);
    write_text(&mut g, 0, "visible-a");
    write_text(&mut g, 2, "visible-c");

    let visible = g.checkpoint_visible_lines();
    assert_eq!(
        visible.len(),
        3,
        "only fixed-height visible rows are copied"
    );
    let mut rebuilt = Grid::new(3, 20);
    rebuilt.restore_visible_from_lines(&visible);
    assert_eq!(visible_text(&g), visible_text(&rebuilt));
    assert_eq!(rebuilt.scrollback_lines(), 0);
}

#[test]
fn visible_checkpoint_captures_live_rows_while_viewport_is_scrolled_up() {
    let mut g = Grid::with_scrollback(3, 20, 100);
    for i in 0..8 {
        g.carriage_return();
        for ch in format!("line-{i}").chars() {
            g.write_char(ch);
        }
        if i < 7 {
            g.line_feed();
        }
    }
    let live = aterm_scrollback::serialize_lines(&g.checkpoint_visible_lines());
    let live_top = g.row_at_screen(0).unwrap().to_string();
    g.scroll_display(2);
    assert!(g.display_offset() > 0);
    assert_ne!(
        g.row(0).unwrap().to_string(),
        live_top,
        "negative control: fixture must expose a different viewport row"
    );
    assert_eq!(
        aterm_scrollback::serialize_lines(&g.checkpoint_visible_lines()),
        live,
        "viewport offset must not alter seamless live-screen capture"
    );
}

#[test]
fn roundtrip_sgr_colors_and_attrs() {
    let mut g = Grid::new(3, 16);
    let flags = CellFlags::BOLD.union(CellFlags::UNDERLINE);
    let fg = PackedColor::indexed(202); // 256-color
    let bg = PackedColor::rgb(10, 20, 30); // truecolor
    let styled = Cell::with_style('X', fg, bg, flags);
    g.set_cell(0, 0, styled);
    let styled2 = Cell::with_style('Y', PackedColor::rgb(200, 100, 50), bg, flags);
    g.set_cell(0, 1, styled2);
    if g.row(0).is_some() {
        let e = g.extras_mut().get_or_create(crate::CellCoord::new(0, 1));
        e.set_fg_rgb(Some([200, 100, 50]));
        e.set_bg_rgb(Some([10, 20, 30]));
    }

    let lines = g.checkpoint_lines_bounded(usize::MAX);
    let rebuilt = rebuild_from_lines(3, 16, &lines);

    assert_eq!(visible_text(&rebuilt)[0], "XY");
    let c0 = *rebuilt.row(0).unwrap().get(0).unwrap();
    assert!(c0.flags().contains(CellFlags::BOLD), "bold preserved");
    assert!(
        c0.flags().contains(CellFlags::UNDERLINE),
        "underline preserved"
    );
    let extra = rebuilt
        .extras()
        .get(crate::CellCoord::new(0, 1))
        .expect("rgb extra restored");
    assert_eq!(extra.fg_rgb(), Some([200, 100, 50]), "fg rgb preserved");
}

#[test]
fn roundtrip_wide_char() {
    let mut g = Grid::new(2, 10);
    g.set_cell(
        0,
        0,
        Cell::with_style(
            '世',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::WIDE,
        ),
    );
    g.set_cell(
        0,
        1,
        Cell::with_style(
            ' ',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::WIDE_CONTINUATION,
        ),
    );
    write_text(&mut g, 0, ""); // no-op, keep wide cell

    let lines = g.checkpoint_lines_bounded(usize::MAX);
    let rebuilt = rebuild_from_lines(2, 10, &lines);

    let c0 = *rebuilt.row(0).unwrap().get(0).unwrap();
    assert_eq!(c0.char(), '世', "wide char preserved");
    assert!(c0.is_wide(), "wide flag preserved");
    let c1 = *rebuilt.row(0).unwrap().get(1).unwrap();
    assert!(c1.is_wide_continuation(), "wide continuation preserved");
}

#[test]
fn roundtrip_combining_mark() {
    let mut g = Grid::new(2, 10);
    g.set_cell(0, 0, Cell::new('e'));
    g.extras_mut()
        .get_or_create(crate::CellCoord::new(0, 0))
        .add_combining('\u{0301}');
    if let Some(cell) = g.row_mut(0).and_then(|r| r.get_mut(0)) {
        cell.set_has_extras(true);
    }

    let lines = g.checkpoint_lines_bounded(usize::MAX);
    let rebuilt = rebuild_from_lines(2, 10, &lines);

    let extra = rebuilt
        .extras()
        .get(crate::CellCoord::new(0, 0))
        .expect("combining extra restored");
    assert_eq!(
        extra.combining(),
        ['\u{0301}'].as_slice(),
        "combining mark preserved"
    );
    assert_eq!(visible_text(&rebuilt)[0], "e\u{0301}");
}

#[test]
fn roundtrip_complex_char() {
    let mut g = Grid::new(2, 10);
    g.set_cell(0, 2, Cell::new('Z'));
    g.set_cell(
        0,
        0,
        Cell::with_style(
            'X',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::WIDE,
        ),
    );
    g.set_cell(
        0,
        1,
        Cell::with_style(
            ' ',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::WIDE_CONTINUATION,
        ),
    );
    g.set_cell_complex_char(0, 0, "👨‍👩‍👧");

    let lines = g.checkpoint_lines_bounded(usize::MAX);
    let rebuilt = rebuild_from_lines(2, 10, &lines);

    let extra = rebuilt
        .extras()
        .get(crate::CellCoord::new(0, 0))
        .expect("complex extra restored");
    assert_eq!(
        extra.complex_char().map(|s| s.to_string()),
        Some("👨‍👩‍👧".to_string()),
        "complex char preserved"
    );
    assert!(
        rebuilt.row(0).unwrap().get(0).unwrap().is_complex(),
        "complex flag preserved"
    );
    let c2 = *rebuilt.row(0).unwrap().get(2).unwrap();
    assert_eq!(
        c2.char(),
        'Z',
        "trailing normal char preserved after wide complex"
    );
}

#[test]
fn roundtrip_hyperlink() {
    use std::sync::Arc;
    let mut g = Grid::new(2, 12);
    write_text(&mut g, 0, "link");
    let url: Arc<str> = Arc::from("https://example.com");
    for col in 0..4u16 {
        let e = g.extras_mut().get_or_create(crate::CellCoord::new(0, col));
        e.set_hyperlink(Some(url.clone()));
        if let Some(cell) = g.row_mut(0).and_then(|r| r.get_mut(col)) {
            cell.set_has_extras(true);
        }
    }

    let lines = g.checkpoint_lines_bounded(usize::MAX);
    let rebuilt = rebuild_from_lines(2, 12, &lines);

    let extra = rebuilt
        .extras()
        .get(crate::CellCoord::new(0, 0))
        .expect("hyperlink extra restored");
    assert_eq!(
        extra.hyperlink().map(|u| u.to_string()),
        Some("https://example.com".to_string()),
        "hyperlink url preserved"
    );
}

#[test]
fn roundtrip_wrapped_line() {
    let mut g = Grid::new(3, 6);
    write_text(&mut g, 0, "abcdef"); // fills the row
    if let Some(row) = g.row_mut(0) {
        row.set_wrapped(true);
    }
    write_text(&mut g, 1, "ghi");

    let lines = g.checkpoint_lines_bounded(usize::MAX);
    let rebuilt = rebuild_from_lines(3, 6, &lines);

    assert!(
        rebuilt.row(0).unwrap().is_wrapped(),
        "wrapped flag preserved"
    );
    assert!(
        !rebuilt.row(1).unwrap().is_wrapped(),
        "non-wrapped flag preserved"
    );
    assert_eq!(visible_text(&rebuilt)[0], "abcdef");
    assert_eq!(visible_text(&rebuilt)[1], "ghi");
}

#[test]
fn roundtrip_scrollback_fills() {
    let mut g = Grid::with_tiered_scrollback(3, 12, 1000, Scrollback::with_defaults());
    let mut sb = Scrollback::with_defaults();
    sb.set_line_limit(None);
    for i in 0..10 {
        sb.push_line(plain_line(&format!("history-{i}")));
    }
    g.attach_scrollback(sb);
    write_text(&mut g, 0, "visible0");
    write_text(&mut g, 1, "visible1");
    write_text(&mut g, 2, "visible2");

    let lines = g.checkpoint_lines_bounded(usize::MAX);
    assert_eq!(lines.len(), 10 + 3);
    assert_eq!(lines[0].as_str(), Some("history-0"));
    assert_eq!(lines[9].as_str(), Some("history-9"));

    let rebuilt = rebuild_from_lines(3, 12, &lines);
    assert_eq!(rebuilt.scrollback_lines(), 10, "scrollback line count");
    assert_eq!(
        rebuilt.try_get_history_line(0).unwrap().unwrap().as_str(),
        Some("history-0"),
        "oldest scrollback line preserved (order)"
    );
    assert_eq!(
        rebuilt.try_get_history_line(9).unwrap().unwrap().as_str(),
        Some("history-9"),
        "newest scrollback line preserved (order)"
    );
    assert_eq!(visible_text(&rebuilt)[0], "visible0");
    assert_eq!(visible_text(&rebuilt)[2], "visible2");
}

/// The visible rows' facts are the line sizes and the protected runs, a wide
/// glyph's spacer is protected only with its lead, and the Lines carry neither.
#[test]
fn visible_cells_carry_line_sizes_and_protected_runs() {
    use crate::LineSize;
    let (fg, bg) = (PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG);
    let wide = |flags| Cell::with_style('世', fg, bg, flags);
    let spacer = Cell::with_style(' ', fg, bg, CellFlags::WIDE_CONTINUATION);
    let mut g = Grid::new(4, 10);
    write_text(&mut g, 0, "plain");
    g.set_cell(1, 0, wide(CellFlags::WIDE));
    g.set_cell(1, 1, spacer);
    g.set_cell(2, 0, wide(CellFlags::WIDE.union(CellFlags::PROTECTED)));
    g.set_cell(2, 1, spacer);
    g.set_cell(2, 2, Cell::new('x'));
    write_text(&mut g, 3, "ab");
    if let Some(row) = g.row_mut(3) {
        if let Some(cell) = row.get_mut(1) {
            cell.set_flags(cell.flags().union(CellFlags::PROTECTED));
        }
        row.set_line_size(LineSize::DoubleWidth);
    }

    let cells = g.checkpoint_visible_cells();
    assert_eq!(cells.protected, vec![(2, 0, 2), (3, 1, 2)]);
    assert_eq!(cells.line_sizes, vec![(3, LineSize::DoubleWidth)]);

    let mut back = Grid::new(4, 10);
    back.restore_visible_from_lines(&g.checkpoint_visible_lines());
    assert!(
        back.checkpoint_visible_cells().is_empty(),
        "NEGATIVE CONTROL: the Lines carry neither fact"
    );
    back.restore_visible_cells(&cells);
    assert_eq!(back.checkpoint_visible_cells(), cells);
}

/// A crafted list (out of order, overlapping, out of the grid) is applied
/// where it fits and skipped where it does not, and the cells keep their text.
#[test]
fn crafted_visible_cells_apply_where_they_fit() {
    use crate::LineSize;
    let lines = [
        plain_line("abcdef"),
        plain_line("ghijkl"),
        plain_line("mnopqr"),
    ];
    let cells = VisibleCells {
        line_sizes: vec![
            (1, LineSize::DoubleWidth),
            (1, LineSize::DoubleHeightTop),
            (0, LineSize::DoubleWidth),
            (9, LineSize::DoubleWidth),
        ],
        protected: vec![
            (0, 1, 3),
            (0, 2, 5),
            (0, 4, u16::MAX),
            (2, 0, 1),
            (1, 0, 2),
            (7, 0, 2),
        ],
    };
    let mut g = Grid::new(3, 8);
    g.restore_visible_from_lines(&lines);
    g.restore_visible_cells(&cells);

    let sizes: Vec<_> = (0..3).map(|r| g.row(r).unwrap().line_size()).collect();
    assert_eq!(
        sizes,
        [
            LineSize::SingleWidth,
            LineSize::DoubleWidth,
            LineSize::SingleWidth,
        ]
    );
    let protected: Vec<(u16, u16)> = (0..3)
        .flat_map(|r| (0..8).map(move |c| (r, c)))
        .filter(|&(r, c)| g.row(r).unwrap().is_cell_protected(c))
        .collect();
    assert_eq!(
        protected,
        [(0, 1), (0, 2), (0, 4), (0, 5), (0, 6), (0, 7), (2, 0)]
    );
    let text: Vec<String> = (0..3)
        .map(|r| {
            let row = g.row(r).unwrap();
            (0..6).map(|c| row.get(c).unwrap().char()).collect()
        })
        .collect();
    assert_eq!(text, ["abcdef", "ghijkl", "mnopqr"]);
}
