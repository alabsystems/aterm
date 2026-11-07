// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A row's touched span ([`Grid::take_screen_rows_touched`]) against the cells themselves. A
//! generated stream of grid operations runs on one grid; after each, every screen row's cells
//! outside the span it reports must be the cells its storage held at the last take. The stream
//! writes narrow, wide and styled glyphs, one at a time and in runs, edits in place, scrolls
//! regions with and without side margins, inserts and deletes lines and columns, erases and
//! fills rectangles, copies them, recolours, sets line sizes and resizes, and takes runs of line
//! feeds and text lines at a region's or the screen's bottom through the line-run steps. Its
//! negative control plants writes that note nothing, which the comparison must catch.

use std::collections::HashMap;

use super::*;
use crate::page::forget_touches;
use crate::{CellFlags, LineSize, PackedColors};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u16) -> u16 {
        u16::try_from(self.next() % u64::from(n.max(1))).expect("below n")
    }
}

/// The line-run steps a generated operation took.
#[derive(Clone, Copy, Default)]
struct LineRuns {
    /// [`Grid::line_feeds_at_region_bottom`] calls.
    region_feeds: usize,
    /// Runs of [`Grid::screen_text_line`]s.
    screen_text: usize,
}

/// One generated operation, on a grid of `rows` by `cols`, adding the line-run steps it took to
/// `runs`.
fn step(grid: &mut Grid, rng: &mut Rng, runs: &mut LineRuns) {
    let (rows, cols) = (grid.rows(), grid.cols());
    let (row, col) = (rng.below(rows), rng.below(cols));
    let n = 1 + rng.below(4);
    let colors = PackedColors::DEFAULT;
    let mut last = None;
    grid.set_cursor(row, col);
    match rng.below(31) {
        0 => {
            grid.write_ascii_blast(b"plain text here");
        }
        1 => {
            grid.write_ascii_run_styled_packed(b"styled", colors, CellFlags::BOLD, &mut last);
        }
        2 => {
            grid.write_cell_run(
                b'=',
                usize::from(n) * 3,
                colors,
                CellFlags::empty(),
                &mut last,
            );
        }
        3 => {
            grid.write_wide_char_at_cursor_packed('\u{6f22}', colors, CellFlags::empty());
        }
        4 => grid.write_char('x'),
        5 => grid.write_char_wrap('y'),
        6 => grid.insert_chars(n),
        7 => grid.delete_chars(n),
        8 => grid.erase_chars(n),
        9 => grid.insert_lines(usize::from(n)),
        10 => grid.delete_lines(usize::from(n)),
        11 => {
            grid.set_scroll_region(row, rng.below(rows));
            if rng.below(2) == 0 {
                grid.scroll_region_up(usize::from(n));
            } else {
                grid.scroll_region_down(usize::from(n));
            }
            grid.reset_scroll_region();
        }
        12 => {
            let right = col + rng.below(cols - col);
            grid.set_horizontal_margins(col, right);
            match rng.below(5) {
                0 => grid.insert_lines_margined(usize::from(n), true),
                1 => grid.delete_lines_margined(usize::from(n), true),
                2 => grid.scroll_region_up_margined(usize::from(n), col, right),
                3 => grid.insert_chars(n),
                _ => grid.delete_chars(n),
            }
            grid.reset_horizontal_margins();
        }
        13 => grid.scroll_up(usize::from(n)),
        14 => match rng.below(6) {
            0 => grid.erase_to_end_of_line(),
            1 => grid.erase_from_start_of_line(),
            2 => grid.erase_line(),
            3 => grid.erase_to_end_of_screen(),
            4 => grid.erase_from_start_of_screen(),
            _ => grid.selective_erase_screen(),
        },
        15 => {
            let (bottom, right) = (row + rng.below(rows - row), col + rng.below(cols - col));
            grid.fill_rect(
                Cell::from_ascii_fast(b'#'),
                row,
                col,
                bottom,
                right,
                None,
                None,
            );
        }
        16 => {
            let (bottom, right) = (row + rng.below(rows - row), col + rng.below(cols - col));
            grid.erase_rect(row, col, bottom, right);
        }
        17 => {
            let (bottom, right) = (row + rng.below(rows - row), col + rng.below(cols - col));
            grid.change_attrs_rect(
                row,
                col,
                bottom,
                right,
                CellFlags::UNDERLINE,
                CellFlags::BOLD,
            );
        }
        18 => {
            let (bottom, right) = (row + rng.below(rows - row), col + rng.below(cols - col));
            grid.copy_rect(row, col, bottom, right, rng.below(rows), rng.below(cols));
        }
        19 => grid.insert_columns(n),
        20 => grid.delete_columns(n),
        21 => {
            let size = [LineSize::DoubleWidth, LineSize::SingleWidth][usize::from(rng.below(2))];
            grid.set_row_line_size(row, size);
        }
        22 => grid.set_cell_complex_char(row, col, "e\u{301}"),
        23 => {
            let run = ['\u{6f22}', '\u{5b57}', '\u{30c6}', '\u{30b9}'];
            grid.write_wide_run_autowrap(&run[..usize::from(n)], colors, CellFlags::empty());
        }
        24 => grid.write_mixed_wide_run_autowrap(
            &['\u{6f22}', '\u{1f600}', '\u{5b57}'][..usize::from(n).min(3)],
            colors,
            CellFlags::empty(),
            CellFlags::COMPLEX,
        ),
        25 => grid.write_narrow_run_autowrap(
            &['a', '\u{e9}', 'b', '\u{3b4}'][..usize::from(n)],
            colors,
            CellFlags::empty(),
        ),
        26 => grid.write_narrow_autowrap_fast('\u{e9}', colors, CellFlags::empty()),
        27 => {
            grid.write_wide_autowrap_fast('\u{6f22}', colors, CellFlags::empty());
        }
        29 => {
            // A history-free region that ends on the screen's last row: a region from row 0
            // archives into the history this grid keeps.
            let top = 1 + rng.below(rows - 2);
            grid.set_scroll_region(top, rows - 1);
            grid.set_cursor(rows - 1, col);
            if grid.line_run_region().is_some() {
                grid.line_feeds_at_region_bottom(usize::from(n));
                runs.region_feeds += 1;
            }
            grid.reset_scroll_region();
        }
        30 => {
            grid.set_cursor(rows - 1, 0);
            if let Some(mut run) = grid.begin_screen_text_lines() {
                for _ in 0..n {
                    grid.screen_text_line(&mut run, b"text line");
                    grid.carriage_return();
                }
                grid.settle_screen_text_lines(run, usize::from(n), true);
                runs.screen_text += 1;
            }
        }
        _ => {
            if rng.below(8) == 0 {
                grid.resize(6 + rng.below(6), 10 + rng.below(12));
            } else {
                grid.screen_alignment_pattern();
            }
        }
    }
}

/// Runs one generated stream, with each write noting nothing when `forget` is set, and says
/// where a cell outside its row's touched span first changed, how many such cells it compared
/// and how many line-run steps it took.
fn run(seed: u64, forget: bool) -> (Option<String>, usize, LineRuns) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut grid = Grid::with_scrollback(8, 16, 20);
    // Each row storage's cells at its last take.
    let mut seen: HashMap<usize, Vec<Cell>> = HashMap::new();
    let mut found = None;
    let mut compared = 0;
    let mut line_runs = LineRuns::default();
    for k in 0..400 {
        forget_touches(forget);
        step(&mut grid, &mut rng, &mut line_runs);
        forget_touches(false);
        let mut now = HashMap::new();
        grid.take_screen_rows_touched(|r, cells, _, touched| {
            let addr = cells.as_ptr().addr();
            if let Some(before) = seen.get(&addr).filter(|b| b.len() == cells.len()) {
                compared += cells.len() - touched.len();
                if let Some(col) =
                    (0..cells.len()).find(|&c| !touched.contains(&c) && cells[c] != before[c])
                    && found.is_none()
                {
                    found = Some(format!(
                        "seed {seed}, step {k}: row {r} col {col} outside {touched:?}"
                    ));
                }
            }
            now.insert(addr, cells.to_vec());
        });
        seen = now;
    }
    (found, compared, line_runs)
}

#[test]
fn a_cell_outside_the_touched_span_is_as_the_last_take_left_it() {
    let (mut compared, mut region_feeds, mut screen_text) = (0, 0, 0);
    for seed in 1..=60 {
        let (found, cells, runs) = run(seed, false);
        if let Some(why) = found {
            panic!("a write the row did not note: {why}");
        }
        compared += cells;
        region_feeds += runs.region_feeds;
        screen_text += runs.screen_text;
    }
    assert!(
        compared > 100_000 && region_feeds > 300 && screen_text > 10,
        "only {compared} untouched cells were compared, over {region_feeds} coalesced region \
         feeds and {screen_text} screen text runs"
    );
}

#[test]
fn a_planted_write_that_notes_nothing_is_caught() {
    let caught = (1..=10).filter(|&seed| run(seed, true).0.is_some()).count();
    assert!(
        caught >= 8,
        "writes that note nothing were caught on only {caught} of 10 streams"
    );
}
