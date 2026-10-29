// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Line feed / reverse line feed with scroll regions.

use super::*;

#[test]
fn single_row_scroll_region_rejected_per_vt510() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(3, 3);

    let region = grid.scroll_region();
    assert!(
        region.is_full(grid.rows()),
        "set_scroll_region(3,3) should reset to full screen, got top={} bottom={}",
        region.top,
        region.bottom,
    );

    grid.set_cursor(4, 0);
    grid.line_feed();
    assert_eq!(
        grid.cursor_row(),
        5,
        "cursor should move down (full-screen region), not stay at 4"
    );
    grid.assert_invariants();
}

fn column_zero(grid: &Grid) -> String {
    (0..grid.rows())
        .map(|r| grid.cell(r, 0).map_or(' ', |c| c.char()))
        .collect()
}

#[test]
fn scrolls_move_only_the_region_rows() {
    type Op = fn(&mut Grid);
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: &[(&str, u16, Option<(u16, u16)>, u16, Op, &str)] = &[
        ("SU 1", 3, None, 0, |g| g.scroll_up(1), "BC "),
        ("SD 1", 3, None, 0, |g| g.scroll_down(1), " AB"),
        ("SU 2", 4, None, 0, |g| g.scroll_up(2), "CD  "),
        ("SD 2", 4, None, 0, |g| g.scroll_down(2), "  AB"),
        ("SU 0", 3, None, 0, |g| g.scroll_up(0), "ABC"),
        (
            "full-screen region up",
            3,
            Some((0, 2)),
            0,
            |g| g.scroll_region_up(1),
            "BC ",
        ),
        (
            "region up past its size",
            3,
            Some((0, 2)),
            0,
            |g| g.scroll_region_up(100),
            "   ",
        ),
        (
            "region down past its size",
            3,
            Some((0, 2)),
            0,
            |g| g.scroll_region_down(100),
            "   ",
        ),
        (
            "region up",
            6,
            Some((2, 4)),
            0,
            |g| g.scroll_region_up(1),
            "ABDE F",
        ),
        (
            "region down",
            6,
            Some((2, 4)),
            0,
            |g| g.scroll_region_down(1),
            "AB CDF",
        ),
        (
            "region up n == size",
            6,
            Some((1, 3)),
            0,
            |g| g.scroll_region_up(3),
            "A   EF",
        ),
        (
            "region down n == size",
            6,
            Some((1, 3)),
            0,
            |g| g.scroll_region_down(3),
            "A   EF",
        ),
        (
            "region up n > size",
            6,
            Some((1, 3)),
            0,
            |g| g.scroll_region_up(100),
            "A   EF",
        ),
        (
            "two-row region up",
            5,
            Some((2, 3)),
            0,
            |g| g.scroll_region_up(1),
            "ABD E",
        ),
        (
            "LF at the bottom margin",
            10,
            Some((2, 5)),
            5,
            Grid::line_feed,
            "ABDEF GHIJ",
        ),
        (
            "RI at the top margin",
            10,
            Some((2, 5)),
            2,
            Grid::reverse_line_feed,
            "AB CDEGHIJ",
        ),
        (
            "LF in a two-row region",
            10,
            Some((3, 4)),
            4,
            Grid::line_feed,
            "ABCE FGHIJ",
        ),
        (
            "RI in a two-row region",
            10,
            Some((3, 4)),
            3,
            Grid::reverse_line_feed,
            "ABC DFGHIJ",
        ),
    ];
    for &(name, rows, region, cursor, op, want) in cases {
        let mut grid = Grid::new(rows, 10);
        fill_grid_rows(&mut grid, rows);
        if let Some((top, bottom)) = region {
            grid.set_scroll_region(top, bottom);
        }
        grid.set_cursor(cursor, 0);
        let total_before = grid.total_lines();
        op(&mut grid);
        assert_eq!(column_zero(&grid), want, "{name}");
        if name.starts_with("SD") || name.contains("down") {
            assert_eq!(grid.total_lines(), total_before, "{name}: total_lines");
        }
        grid.assert_invariants();
    }

    let degenerate: [(&str, Op); 4] = [
        ("up", |g| g.scroll_region_up(1)),
        ("down", |g| g.scroll_region_down(1)),
        ("up margined", |g| g.scroll_region_up_margined(1, 2, 5)),
        ("down margined", |g| g.scroll_region_down_margined(1, 2, 5)),
    ];
    for (name, op) in degenerate {
        let mut grid = Grid::new(6, 10);
        fill_grid_rows(&mut grid, 6);
        grid.storage.cursor_state.scroll_region = ScrollRegion { top: 3, bottom: 3 };
        op(&mut grid);
        assert_eq!(column_zero(&grid), "ABCDEF", "degenerate region {name}");
        grid.assert_invariants();
    }
}

#[test]
fn autowrap_at_the_region_bottom_scrolls_the_region_only() {
    type Write = fn(&mut Grid);
    let writes: [(&str, Write); 4] = [
        ("write_char_wrap", |g| {
            for c in "XXXXXZ".chars() {
                g.write_char_wrap(c);
            }
        }),
        ("write_char_wrap_styled", |g| {
            for c in "XXXXXZ".chars() {
                g.write_char_wrap_styled(
                    c,
                    PackedColor::indexed(196),
                    PackedColor::indexed(22),
                    CellFlags::BOLD,
                );
            }
        }),
        ("write_ascii_blast", |g| {
            assert_eq!(g.write_ascii_blast(b"XXXXXZ"), 6);
        }),
        ("write_ascii_run_styled", |g| {
            let mut last_byte = None;
            let written = g.write_ascii_run_styled(
                b"XXXXXZ",
                PackedColor::indexed(33),
                PackedColor::indexed(235),
                CellFlags::ITALIC,
                &mut last_byte,
            );
            assert_eq!((written, last_byte), (6, Some(b'Z')));
        }),
    ];
    for (name, write) in writes {
        let mut grid = build_autowrap_scroll_region_fixture();
        grid.set_cursor(3, 0);
        write(&mut grid);
        assert_rows_outside_scroll_region_preserved(&grid);
        assert_eq!(column_zero(&grid), "ACXZEF", "{name}");
        assert_eq!((grid.cursor_row(), grid.cursor_col()), (3, 1), "{name}");
    }
}

#[test]
fn a_region_scroll_from_a_scrolled_back_view_snaps_to_live() {
    type Op = fn(&mut Grid);
    let ops: [(&str, u16, u16, Op); 4] = [
        ("LF at the bottom margin", 7, 0, Grid::line_feed),
        ("RI at the top margin", 2, 0, Grid::reverse_line_feed),
        ("autowrap at the bottom margin", 7, 9, |g| {
            g.write_char_wrap('X');
            g.resolve_pending_wrap();
        }),
        ("scroll_region_up", 7, 0, |g| g.scroll_region_up(1)),
    ];
    for (name, row, col, op) in ops {
        let mut grid = Grid::with_scrollback(10, 10, 20);
        fill_grid_rows(&mut grid, 10);
        grid.set_cursor(9, 0);
        for _ in 0..5 {
            grid.line_feed();
        }
        grid.set_scroll_region(2, 7);
        grid.set_cursor(row, col);
        grid.scroll_display(3);
        assert!(grid.display_offset() > 0, "{name}: precondition");
        op(&mut grid);
        assert_eq!(grid.display_offset(), 0, "{name}");
        grid.assert_invariants();
    }
}

#[test]
fn write_wide_char_wrap_at_scroll_region_bottom_scrolls_region_only() {
    let mut grid = build_autowrap_scroll_region_fixture();

    grid.set_cursor(3, 4);
    let ok = grid.write_wide_char_wrap_styled(
        '好',
        PackedColor::indexed(45),
        PackedColor::indexed(232),
        CellFlags::BOLD,
    );
    assert!(ok);

    assert_rows_outside_scroll_region_preserved(&grid);
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(3, 0).unwrap().char(), '好');
    assert_eq!(grid.cursor_row(), 3);
    assert_eq!(grid.cursor_col(), 2);
}

#[test]
fn line_feed_full_screen_region_scrolls_with_scrollback() {
    let mut grid = Grid::with_scrollback(3, 5, 10);

    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.set_cursor(1, 0);
    grid.write_char('B');
    grid.set_cursor(2, 0);
    grid.write_char('C');

    grid.set_cursor(2, 0);
    grid.line_feed();

    assert_eq!(grid.scrollback_lines(), 1);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
    assert!(grid.row(2).unwrap().is_empty());
}

#[test]
fn write_char_wrap_below_scroll_region_at_screen_bottom_does_not_scroll() {
    let mut grid = Grid::with_scrollback(5, 5, 10);

    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(1, 3);
    grid.set_cursor(4, 4);

    grid.write_char_wrap('Z');
    grid.resolve_pending_wrap();

    assert_eq!(
        grid.scrollback_lines(),
        0,
        "no scrollback: display must not scroll"
    );
    for (row, ch) in ['A', 'B', 'C', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(
            grid.cell(row as u16, 0).unwrap().char(),
            ch,
            "row {row} content untouched by wrap below the region"
        );
    }
    assert_eq!(grid.cell(4, 4).unwrap().char(), 'Z', "'Z' written at (4,4)");
    assert_eq!(grid.cursor_row(), 4, "cursor stays on the bottom row");
    assert_eq!(grid.cursor_col(), 0, "cursor at col 0 after wrap");
    grid.assert_invariants();
}

#[test]
fn scroll_to_bottom_targeted_damage_small_offset() {
    let mut grid = Grid::with_scrollback(24, 80, 100);

    for i in 0..30u16 {
        grid.set_cursor(23, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }

    grid.scroll_display(3);
    assert_eq!(grid.display_offset(), 3);

    grid.clear_damage();
    grid.scroll_to_bottom();
    assert_eq!(grid.display_offset(), 0);

    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "targeted damage: scroll_to_bottom with offset=3 should not mark_full on a 24-row grid"
    );

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    let dirty_rows: Vec<u16> = bounds.iter().map(|b| b.line).collect();
    assert_eq!(
        dirty_rows,
        vec![21, 22, 23],
        "expected exactly rows 21-23 dirty, got {dirty_rows:?}"
    );
}

#[test]
fn scroll_to_bottom_full_damage_large_offset() {
    let mut grid = Grid::with_scrollback(5, 10, 100);

    for i in 0..20u16 {
        grid.set_cursor(4, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }

    grid.scroll_to_top();
    assert!(grid.display_offset() >= 5);

    grid.clear_damage();
    grid.scroll_to_bottom();

    assert!(grid.damage().has_damage());
    assert!(
        grid.damage().is_full(),
        "scroll_to_bottom with offset >= visible_rows should mark_full"
    );
}

#[test]
fn scroll_region_up_targeted_damage() {
    let mut grid = Grid::new(24, 80);
    grid.set_scroll_region(5, 15);

    for row in 5..=15 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + (row - 5) as u8) as char);
    }

    grid.clear_damage();
    grid.scroll_region_up(1);

    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "scroll_region_up should use targeted damage, not mark_full"
    );

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    for b in &bounds {
        assert!(
            b.line >= 5 && b.line <= 15,
            "dirty row {} should be within scroll region [5, 15]",
            b.line
        );
    }
}

#[test]
fn erase_to_end_of_screen_targeted_damage() {
    let mut grid = Grid::new(24, 80);

    for row in 0..24u16 {
        grid.set_cursor(row, 0);
        grid.write_char('X');
    }

    grid.set_cursor(20, 0);
    grid.clear_damage();
    grid.erase_to_end_of_screen();

    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "erase_to_end_of_screen from row 20 should use targeted damage"
    );

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    for b in &bounds {
        assert!(
            b.line >= 20,
            "dirty row {} should be at or below cursor row 20",
            b.line
        );
    }
}

#[test]
fn scroll_up_mixed_phase1_phase2_single_call() {
    let mut grid = Grid::with_scrollback(4, 10, 3);

    fill_grid_rows(&mut grid, 4);

    grid.scroll_up(2);
    assert_eq!(
        grid.cell(0, 0).unwrap().char(),
        'C',
        "after scroll(2): row 0 = old row 2"
    );
    assert_eq!(
        grid.cell(1, 0).unwrap().char(),
        'D',
        "after scroll(2): row 1 = old row 3"
    );
    assert!(
        grid.row(2).unwrap().is_empty(),
        "after scroll(2): row 2 blank"
    );
    assert!(
        grid.row(3).unwrap().is_empty(),
        "after scroll(2): row 3 blank"
    );

    grid.set_cursor(2, 0);
    grid.write_char('E');
    grid.set_cursor(3, 0);
    grid.write_char('F');

    grid.scroll_up(3);

    assert_eq!(
        grid.cell(0, 0).unwrap().char(),
        'F',
        "mixed scroll: row 0 = old row 3 ('F')"
    );
    assert!(grid.row(1).unwrap().is_empty(), "mixed scroll: row 1 blank");
    assert!(grid.row(2).unwrap().is_empty(), "mixed scroll: row 2 blank");
    assert!(grid.row(3).unwrap().is_empty(), "mixed scroll: row 3 blank");

    let ring_sb = grid.storage.ring_buffer_scrollback();
    let extras_len = grid.storage.ring_extras.len();
    assert_eq!(
        extras_len, ring_sb,
        "ring_extras.len()={extras_len} != ring_buffer_scrollback()={ring_sb}"
    );

    grid.assert_invariants();
}

#[test]
fn scroll_up_mixed_phase_scrollback_content() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 10, 2, scrollback);

    fill_grid_rows(&mut grid, 3);

    grid.scroll_up(1);

    grid.set_cursor(2, 0);
    grid.write_char('D');

    grid.scroll_up(3);

    assert!(
        grid.row(0).unwrap().is_empty(),
        "row 0 blank after mixed scroll"
    );
    assert!(
        grid.row(1).unwrap().is_empty(),
        "row 1 blank after mixed scroll"
    );
    assert!(
        grid.row(2).unwrap().is_empty(),
        "row 2 blank after mixed scroll"
    );

    let line_count = grid.tiered_scrollback_lines();
    assert!(
        line_count >= 2,
        "scrollback should have at least 2 lines from Phase 2 eviction, got {line_count}"
    );

    grid.assert_invariants();
}
