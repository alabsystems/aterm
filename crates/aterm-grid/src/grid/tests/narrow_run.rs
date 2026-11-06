// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `write_narrow_run_autowrap` against the per-glyph writer it batches
//! (`write_narrow_autowrap_fast` once per character), from generated grid states: scroll regions
//! and DECSLRM margins, line-feed runs into history, wide glyphs and emoji a run overwrites half
//! of, hyperlinks and marks on the cells it lands on, double-width rows and pending wraps.

use super::*;
use std::fmt::Write as _;
use std::sync::Arc;

/// A seeded xorshift stream.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % n.max(1) as u64).unwrap_or(0)
    }

    fn below_u16(&mut self, n: u16) -> u16 {
        u16::try_from(self.below(usize::from(n))).unwrap_or(0)
    }
}

/// Width-1 BMP glyphs a run holds: ASCII, Latin-1, Greek, Cyrillic, a symbol.
const NARROW: &[char] = &[
    'a', 'Z', ' ', '~', '\u{e9}', '\u{df}', '\u{3bb}', '\u{3c9}', '\u{416}', '\u{44f}', '\u{20ac}',
];

const WIDE: &[char] = &['\u{4e2d}', '\u{6587}', '\u{d55c}', '\u{3042}'];

/// One generated step: a state change both grids take alike, or a run (`Run`) the grid under
/// test writes in one call and the reference one glyph at a time.
#[derive(Debug, Clone)]
enum Step {
    Cursor(u16, u16),
    Wide(char),
    Emoji,
    Narrow(char),
    Link(u16, u16),
    Mark(u16, u16),
    Region(u16, u16),
    Margins(u16, u16),
    DoubleWidth(u16),
    PendingWrap(u16),
    LineFeeds(u16),
    Run(Vec<char>, PackedColors, CellFlags),
}

fn step(rng: &mut Rng, rows: u16, cols: u16) -> Step {
    match rng.below(16) {
        0 | 1 => Step::Cursor(rng.below_u16(rows), rng.below_u16(cols)),
        2 => Step::Wide(WIDE[rng.below(WIDE.len())]),
        3 => Step::Emoji,
        4 => Step::Narrow(NARROW[rng.below(NARROW.len())]),
        5 => Step::Link(rng.below_u16(rows), rng.below_u16(cols)),
        6 => Step::Mark(rng.below_u16(rows), rng.below_u16(cols)),
        7 => {
            let top = rng.below_u16(rows);
            Step::Region(top, top + rng.below_u16(rows - top))
        }
        8 => {
            let left = rng.below_u16(cols);
            Step::Margins(left, left + rng.below_u16(cols - left))
        }
        9 => Step::DoubleWidth(rng.below_u16(rows)),
        10 => Step::PendingWrap(rng.below_u16(rows)),
        11 => Step::LineFeeds(1 + rng.below_u16(4)),
        _ => {
            let len = 1 + rng.below(usize::from(cols) * 3);
            let run = (0..len).map(|_| NARROW[rng.below(NARROW.len())]).collect();
            let fg = match rng.below(3) {
                0 => PackedColor::DEFAULT_FG,
                _ => PackedColor::indexed(u8::try_from(rng.below(256)).unwrap_or(0)),
            };
            let bg = match rng.below(3) {
                0 => PackedColor::indexed(4),
                _ => PackedColor::DEFAULT_BG,
            };
            let flags = match rng.below(3) {
                0 => CellFlags::BOLD,
                1 => CellFlags::UNDERLINE,
                _ => CellFlags::empty(),
            };
            Step::Run(run, Cell::convert_colors(fg, bg), flags)
        }
    }
}

/// How a grid writes a run.
type RunWriter = fn(&mut Grid, &[char], PackedColors, CellFlags);

fn batched(grid: &mut Grid, run: &[char], colors: PackedColors, flags: CellFlags) {
    grid.write_narrow_run_autowrap(run, colors, flags);
}

fn per_glyph(grid: &mut Grid, run: &[char], colors: PackedColors, flags: CellFlags) {
    for &c in run {
        grid.write_narrow_autowrap_fast(c, colors, flags);
    }
}

/// A planted row-edge bug for the negative control: the batched writer, then the deferred wrap
/// a run that fills its row to the last column leaves is dropped.
fn planted_row_edge_bug(grid: &mut Grid, run: &[char], colors: PackedColors, flags: CellFlags) {
    grid.write_narrow_run_autowrap(run, colors, flags);
    grid.set_pending_wrap(false);
}

fn apply(grid: &mut Grid, step: &Step, write_run: RunWriter) {
    let plain = PackedColors::new();
    match *step {
        Step::Cursor(row, col) => grid.set_cursor(row, col),
        Step::Wide(c) => {
            grid.write_wide_autowrap_fast(c, plain, CellFlags::empty());
        }
        Step::Emoji => grid.write_emoji_autowrap_fast('\u{1f600}', plain, CellFlags::COMPLEX),
        Step::Narrow(c) => grid.write_narrow_autowrap_fast(c, plain, CellFlags::empty()),
        Step::Link(row, col) => {
            grid.cell_extra_mut(row, col)
                .set_hyperlink(Some(Arc::from("https://example.com/")));
        }
        Step::Mark(row, col) => grid.cell_extra_mut(row, col).add_combining('\u{301}'),
        Step::Region(top, bottom) => grid.set_scroll_region(top, bottom),
        Step::Margins(left, right) => grid.set_horizontal_margins(left, right),
        Step::DoubleWidth(row) => {
            grid.set_row_line_size(row, crate::LineSize::DoubleWidth);
        }
        Step::PendingWrap(row) => {
            grid.set_cursor(row, grid.cols().saturating_sub(1));
            grid.set_pending_wrap(true);
        }
        Step::LineFeeds(n) => {
            for _ in 0..n {
                grid.line_feed();
            }
        }
        Step::Run(ref run, colors, flags) => write_run(grid, run, colors, flags),
    }
}

/// Everything a write can change that a reader of the grid sees: each visible row's cells,
/// length, flags and line size, the extras and complex glyph behind each cell, its damage, the
/// cursor and pending wrap, the history lines, the selection damage, and whether the content
/// generation moved.
fn observe(grid: &mut Grid, gen_before: u64) -> String {
    let mut out = String::new();
    let cursor = grid.cursor();
    let _ = writeln!(
        out,
        "cursor=({},{}) wrap={} gen_moved={} selection={:?} history={}",
        cursor.row,
        cursor.col,
        grid.pending_wrap(),
        grid.content_gen() != gen_before,
        grid.take_selection_damage(),
        grid.history_line_count(),
    );
    for row in 0..grid.rows() {
        let Some(r) = grid.row(row) else {
            let _ = writeln!(out, "row {row}: none");
            continue;
        };
        let _ = writeln!(
            out,
            "row {row}: len={} flags={:?} size={:?} damaged={}",
            r.len(),
            r.flags(),
            r.line_size(),
            grid.damage().is_row_damaged(row),
        );
        for col in 0..grid.cols() {
            let Some(&cell) = grid.cell(row, col) else {
                continue;
            };
            let mut glyph = String::new();
            if cell.is_complex() {
                grid.push_complex_char_at(row, col, &mut glyph);
            }
            let _ = writeln!(
                out,
                "  {col}: {cell:?} {glyph:?} {:?}",
                grid.cell_extra(row, col)
            );
        }
    }
    for idx in 0..grid.history_line_count() {
        let _ = writeln!(out, "history {idx}: {:?}", grid.get_history_line(idx));
    }
    out
}

/// Whether a run written now takes the batched path: no DECSLRM margins, no double-width row.
fn batches(grid: &Grid) -> bool {
    !grid.has_horizontal_margins()
        && (0..grid.rows()).all(|row| {
            grid.row(row)
                .is_none_or(|r| r.line_size() == crate::LineSize::SingleWidth)
        })
}

/// The first case on which `write_run` leaves a grid other than the per-glyph writer does, as
/// the seed, the steps up to it and both observations; else how many runs took the batched path.
fn first_divergence(write_run: RunWriter, cases: u64) -> Result<usize, String> {
    let mut batched_runs = 0;
    for seed in 1..=cases {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
        let rows = 2 + rng.below_u16(5);
        let cols = 1 + rng.below_u16(12);
        let mut under_test = Grid::with_scrollback(rows, cols, 64);
        let mut reference = Grid::with_scrollback(rows, cols, 64);
        let mut steps = Vec::new();
        for _ in 0..24 {
            let s = step(&mut rng, rows, cols);
            steps.push(s.clone());
            let (gen_a, gen_b) = (under_test.content_gen(), reference.content_gen());
            if matches!(s, Step::Run(..)) && batches(&under_test) {
                batched_runs += 1;
            }
            apply(&mut under_test, &s, write_run);
            apply(&mut reference, &s, per_glyph);
            let (a, b) = (
                observe(&mut under_test, gen_a),
                observe(&mut reference, gen_b),
            );
            if a != b {
                return Err(format!(
                    "seed {seed}, {rows}x{cols}, steps {steps:?}\nunder test:\n{a}\nreference:\n{b}"
                ));
            }
        }
    }
    Ok(batched_runs)
}

#[test]
fn a_batched_narrow_run_leaves_the_grid_the_per_glyph_writer_leaves() {
    match first_divergence(batched, 3000) {
        Ok(batched_runs) => assert!(
            batched_runs >= 3000,
            "only {batched_runs} runs took the batched path: the cases hardly test it"
        ),
        Err(divergence) => {
            panic!("write_narrow_run_autowrap diverges from per-glyph writes: {divergence}")
        }
    }
}

#[test]
fn the_differential_catches_a_planted_row_edge_bug() {
    assert!(
        first_divergence(planted_row_edge_bug, 3000).is_err(),
        "a run writer that drops the deferred wrap at a full row passed the differential"
    );
}
