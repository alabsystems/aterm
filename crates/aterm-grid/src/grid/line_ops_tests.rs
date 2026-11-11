// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::grid::test_support::wrapped_pair;

/// Helper: read the character at (row, col), returning space for empty cells.
fn char_at(grid: &Grid, row: u16, col: u16) -> char {
    grid.resolved_char(row, col).unwrap_or(' ')
}

/// Helper: write ASCII text at the current cursor position.
fn write_text(grid: &mut Grid, text: &str) {
    for c in text.chars() {
        grid.write_char(c);
    }
}

/// Helper: read the trimmed text content of a row (trailing spaces stripped).
fn row_trimmed(grid: &Grid, row: u16) -> String {
    grid.row_text(row)
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

#[derive(Clone, Copy, Debug)]
enum CharOp {
    Ich,
    Dch,
    Ech,
}

/// ICH shifts the rest of the cursor row right (dropping what passes the
/// edge), DCH shifts it left (blank-filling the end), ECH blanks in place;
/// each clamps its count to the row, leaves the other rows alone and
/// consumes the pending wrap.
#[test]
fn char_ops_edit_only_the_cursor_row() {
    use CharOp::*;
    let cases: &[(CharOp, u16, &str, u16, u16, &str)] = &[
        (Ich, 10, "ABCDE", 2, 1, "AB CDE    "),
        (Ich, 10, "ABCDE", 1, 3, "A   BCDE  "),
        (Ich, 8, "HELLO", 0, 2, "  HELLO "),
        (Ich, 6, "ABCDEF", 4, 100, "ABCD  "),
        (Ich, 5, "ABCDE", 4, 1, "ABCD "),
        (Ich, 10, "", 0, 3, "          "),
        (Dch, 10, "ABCDE", 2, 1, "ABDE      "),
        (Dch, 10, "ABCDEFGH", 1, 3, "AEFGH     "),
        (Dch, 8, "ABCDEFGH", 0, 3, "DEFGH   "),
        (Dch, 6, "ABCDEF", 4, 100, "ABCD  "),
        (Dch, 5, "ABCDE", 4, 1, "ABCD "),
        (Dch, 10, "", 0, 3, "          "),
        (Ech, 10, "ABCDEFGH", 2, 3, "AB   FGH  "),
        (Ech, 10, "ABCDE", 2, 1, "AB DE     "),
        (Ech, 6, "ABCDEF", 3, 100, "ABC   "),
        (Ech, 8, "ABCDEFGH", 0, 4, "    EFGH"),
        (Ech, 10, "", 0, 5, "          "),
    ];
    for &(op, cols, text, col, count, want) in cases {
        let mut grid = Grid::new(3, cols);
        write_text(&mut grid, text);
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "ROW1");
        grid.move_cursor_to(0, col);
        grid.storage.mark_pending_wrap();
        match op {
            Ich => grid.insert_chars(count),
            Dch => grid.delete_chars(count),
            Ech => grid.erase_chars(count),
        }
        let got: String = (0..cols).map(|c| char_at(&grid, 0, c)).collect();
        let at = format!("{op:?} {count} at col {col} of {text:?}");
        assert_eq!(got, want, "{at}");
        assert_eq!(row_trimmed(&grid, 1), "ROW1", "{at}: another row changed");
        assert!(!grid.storage.pending_wrap, "{at}: the wrap survived");
    }
}

/// IL inserts blank rows at the cursor row and DL deletes rows there,
/// shifting only the rest of the scroll region (rows past it are pushed
/// out or blank-filled), clamping the count to the region, and homing the
/// cursor column; a zero count or a cursor outside the region is a no-op.
#[test]
fn line_ops_shift_rows_within_the_region() {
    type Case<'a> = (
        bool,
        u16,
        Option<(u16, u16)>,
        (u16, u16),
        u16,
        &'a [&'a str],
    );
    let cases: &[Case] = &[
        (true, 5, None, (2, 0), 1, &["L0", "L1", "", "L2", "L3"]),
        (true, 6, None, (1, 0), 2, &["L0", "", "", "L1", "L2", "L3"]),
        (true, 4, None, (0, 0), 1, &["", "L0", "L1", "L2"]),
        (true, 4, None, (1, 5), 100, &["L0", "", "", ""]),
        (true, 5, None, (3, 0), 2, &["L0", "L1", "L2", "", ""]),
        (true, 1, None, (0, 0), 1, &[""]),
        (true, 4, None, (0, 3), 0, &["L0", "L1", "L2", "L3"]),
        (
            true,
            6,
            Some((1, 4)),
            (2, 0),
            1,
            &["L0", "L1", "", "L2", "L3", "L5"],
        ),
        (
            true,
            6,
            Some((1, 3)),
            (1, 0),
            1,
            &["L0", "", "L1", "L2", "L4", "L5"],
        ),
        (
            true,
            6,
            Some((2, 4)),
            (0, 3),
            1,
            &["L0", "L1", "L2", "L3", "L4", "L5"],
        ),
        (false, 5, None, (2, 0), 1, &["L0", "L1", "L3", "L4", ""]),
        (false, 6, None, (1, 0), 3, &["L0", "L4", "L5", "", "", ""]),
        (false, 4, None, (0, 0), 1, &["L1", "L2", "L3", ""]),
        (false, 4, None, (1, 5), 100, &["L0", "", "", ""]),
        (false, 5, None, (3, 0), 1, &["L0", "L1", "L2", "L4", ""]),
        (false, 1, None, (0, 0), 1, &[""]),
        (false, 4, None, (0, 3), 0, &["L0", "L1", "L2", "L3"]),
        (
            false,
            6,
            Some((1, 4)),
            (2, 0),
            1,
            &["L0", "L1", "L3", "L4", "", "L5"],
        ),
        (
            false,
            6,
            Some((1, 3)),
            (3, 0),
            1,
            &["L0", "L1", "L2", "", "L4", "L5"],
        ),
        (
            false,
            6,
            Some((2, 4)),
            (5, 3),
            1,
            &["L0", "L1", "L2", "L3", "L4", "L5"],
        ),
    ];
    for &(insert, rows, region, (row, col), count, want) in cases {
        let mut grid = Grid::new(rows, 10);
        for r in 0..rows {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        if let Some((top, bottom)) = region {
            grid.set_scroll_region(top, bottom);
        }
        grid.move_cursor_to(row, col);
        if insert {
            grid.insert_lines(usize::from(count));
        } else {
            grid.delete_lines(usize::from(count));
        }
        let at = format!(
            "{} {count} at {:?} in {rows} rows, region {region:?}",
            if insert { "IL" } else { "DL" },
            (row, col)
        );
        let got: Vec<String> = (0..rows).map(|r| row_trimmed(&grid, r)).collect();
        assert_eq!(got, want, "{at}");
        let unchanged = want.iter().enumerate().all(|(r, w)| *w == format!("L{r}"));
        let want_col = if unchanged { col } else { 0 };
        assert_eq!(grid.cursor_col(), want_col, "{at}: cursor column");
    }
}

#[test]
fn test_ech_through_the_last_column_breaks_the_wrap_as_el0_does() {
    for col in [0u16, 2, 4] {
        for count in [5 - col, 99] {
            let mut grid = wrapped_pair();
            grid.move_cursor_to(0, col);
            grid.erase_chars(count);
            assert!(
                !grid.row(1).unwrap().is_wrapped(),
                "ECH {count} at col {col} breaks the link"
            );
            assert_eq!(row_trimmed(&grid, 1), "FG", "row 1's cells survive");
        }
    }
}

#[test]
fn test_ech_short_of_the_last_column_keeps_the_wrap() {
    for (col, count) in [(0u16, 3u16), (1, 1), (2, 2)] {
        let mut grid = wrapped_pair();
        grid.move_cursor_to(0, col);
        grid.erase_chars(count);
        assert!(
            grid.row(1).unwrap().is_wrapped(),
            "ECH {count} at col {col} keeps the link"
        );
        assert_eq!(char_at(&grid, 0, 4), 'E');
    }
}

#[test]
fn test_ech_under_margins_runs_to_the_row_end_and_breaks_the_wrap_as_el0_does() {
    let mut grid = wrapped_pair();
    grid.set_horizontal_margins(0, 3);
    grid.move_cursor_to(0, 1);
    grid.erase_chars(99);
    assert_eq!(char_at(&grid, 0, 3), ' ', "erased through the margin");
    assert_eq!(char_at(&grid, 0, 4), ' ', "and past it, to the row end");
    assert!(!grid.row(1).unwrap().is_wrapped());
}

#[test]
fn test_ech_under_margins_short_of_the_row_end_keeps_the_wrap() {
    let mut grid = wrapped_pair();
    grid.set_horizontal_margins(0, 3);
    grid.move_cursor_to(0, 1);
    grid.erase_chars(3);
    assert_eq!(char_at(&grid, 0, 3), ' ', "erased through the margin");
    assert_eq!(char_at(&grid, 0, 4), 'E', "the last column keeps its text");
    assert!(grid.row(1).unwrap().is_wrapped());
}

/// IL and DL with DECLRMM active are no-ops when the cursor column is
/// outside the horizontal margins, even when the cursor row is inside the
/// vertical scroll region.
#[test]
fn test_il_and_dl_margined_cursor_col_outside_margins_are_noops() {
    type Op = fn(&mut Grid);
    let ops: [(&str, Op); 2] = [
        ("IL", |g| g.insert_lines_margined(1, true)),
        ("DL", |g| g.delete_lines_margined(1, true)),
    ];
    for (name, op) in ops {
        let mut grid = Grid::new(24, 80);
        for r in 0..5u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("ROW{r}"));
        }
        grid.set_horizontal_margins(10, 50);
        grid.move_cursor_to(2, 70);
        op(&mut grid);
        assert!(row_trimmed(&grid, 2).starts_with("ROW2"), "{name}");
        assert!(row_trimmed(&grid, 3).starts_with("ROW3"), "{name}");
        assert_eq!(
            grid.cursor_col(),
            70,
            "no-op {name} must not move cursor col"
        );
        assert_eq!(grid.cursor_row(), 2, "{name}");
    }
}

/// Inside both margins and region, margined IL still shifts (guard does not
/// over-fire).
#[test]
fn test_il_margined_cursor_inside_margins_still_shifts() {
    let mut grid = Grid::new(24, 80);
    grid.move_cursor_to(0, 20);
    write_text(&mut grid, "AAA");
    grid.move_cursor_to(1, 20);
    write_text(&mut grid, "BBB");
    grid.set_horizontal_margins(10, 50);
    grid.move_cursor_to(0, 30);
    grid.insert_lines_margined(1, true);
    assert_eq!(grid.cursor_col(), 10);
    assert!(row_trimmed(&grid, 1).contains("AAA"));
}

/// Margined IL/DL and scroll_region_*_margined write cells via get_mut,
/// bypassing len maintenance; when the margined copy blanks a row's tail,
/// Row.len must still shrink, or row_text/search/scrollback emit phantom
/// trailing spaces.
#[test]
fn test_dl_margined_shrinks_len_when_blanks_cover_tail() {
    let mut grid = Grid::new(24, 80);
    grid.move_cursor_to(0, 48);
    write_text(&mut grid, "END");
    assert_eq!(
        grid.row_len(0),
        Some(51),
        "precondition: row 0 content ends at col 50"
    );

    grid.set_horizontal_margins(10, 50);
    grid.move_cursor_to(0, 20);
    grid.delete_lines_margined(1, true);

    assert_eq!(
        grid.row_len(0),
        Some(0),
        "len must shrink to 0, not stay stale-high at 51 (phantom trailing spaces)"
    );
}

/// DECIC and DECDC shift the rows of the vertical scroll region (rows
/// 0..=10) and are no-ops when the cursor row is outside it, even when the
/// cursor column is within the horizontal margins; inside it DECIC still
/// inserts (the guard does not over-fire).
#[test]
fn test_decic_and_decdc_act_only_inside_the_vertical_region() {
    type Op = fn(&mut Grid);
    let cases: [(&str, Op, u16, [&str; 2]); 3] = [
        (
            "DECIC outside",
            |g| g.insert_columns(3),
            15,
            ["ABCDE", "ABCDE"],
        ),
        (
            "DECDC outside",
            |g| g.delete_columns(3),
            15,
            ["ABCDE", "ABCDE"],
        ),
        (
            "DECIC inside",
            |g| g.insert_columns(2),
            5,
            ["AB  C", "ABCDE"],
        ),
    ];
    for (name, op, row, want) in cases {
        let mut grid = Grid::new(24, 80);
        for text_row in [5, 15] {
            grid.move_cursor_to(text_row, 0);
            write_text(&mut grid, "ABCDE");
        }
        grid.set_scroll_region(0, 10);
        grid.move_cursor_to(row, 2);
        op(&mut grid);
        let got = [5, 15].map(|r| (0..5).map(|col| char_at(&grid, r, col)).collect::<String>());
        assert_eq!(got, want.map(String::from), "{name}: rows 5 and 15");
    }
}
