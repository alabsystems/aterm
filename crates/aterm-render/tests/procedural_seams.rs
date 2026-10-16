// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Procedural glyphs in adjacent cells meet exactly: composed cells (tmux pane
//! borders, half-blocks, shaded bars) have identical, non-empty lit pixels on
//! both sides of every interior seam, at odd and even cell sizes, where
//! centring rounding breaks seams.

use aterm_render::procedural;

/// Odd and even cell sizes.
const SIZES: &[(usize, usize)] = &[(7, 15), (8, 16), (9, 19), (10, 20), (11, 21), (12, 22)];

/// A composed multi-cell image of hard-coverage bits. `' '` composes as an
/// empty cell.
struct Grid {
    w: usize,
    h: usize,
    cw: usize,
    ch: usize,
    lit: Vec<bool>,
}

fn compose(rows: &[&[char]], cw: usize, ch: usize) -> Grid {
    let cols = rows[0].len();
    let (w, h) = (cols * cw, rows.len() * ch);
    let mut lit = vec![false; w * h];
    for (r, row) in rows.iter().enumerate() {
        assert_eq!(row.len(), cols, "ragged fixture");
        for (c, &g) in row.iter().enumerate() {
            if g == ' ' {
                continue;
            }
            let cov = procedural::coverage(g, cw, ch).expect("fixture glyphs are procedural");
            for y in 0..ch {
                for x in 0..cw {
                    if cov[y * cw + x] != 0 {
                        lit[(r * ch + y) * w + (c * cw + x)] = true;
                    }
                }
            }
        }
    }
    Grid { w, h, cw, ch, lit }
}

impl Grid {
    /// Lit rows of column `x`, restricted to cell-row `r`.
    fn col_rows(&self, x: usize, r: usize) -> Vec<usize> {
        (r * self.ch..(r + 1) * self.ch)
            .filter(|&y| self.lit[y * self.w + x])
            .collect()
    }

    /// Lit columns of row `y`, restricted to cell-column `c`.
    fn row_cols(&self, y: usize, c: usize) -> Vec<usize> {
        (c * self.cw..(c + 1) * self.cw)
            .map(|x| x - c * self.cw)
            .filter(|&dx| self.lit[y * self.w + c * self.cw + dx])
            .collect()
    }

    /// The seam between cell (r, c) and its right neighbour: the lit rows of the
    /// boundary columns are equal and non-empty (no gap, no jog).
    fn assert_h_seam(&self, r: usize, c: usize, what: &str) {
        let left = self.col_rows((c + 1) * self.cw - 1, r);
        let right = self.col_rows((c + 1) * self.cw, r);
        assert!(
            !left.is_empty(),
            "{what}: cell ({r},{c}) does not reach its right edge"
        );
        assert_eq!(
            left,
            right,
            "{what}: seam between cells ({r},{c}) and ({r},{}) gaps or jogs",
            c + 1
        );
    }

    /// The seam between cell (r, c) and the cell below it (column sets of the
    /// boundary rows, in cell-local x).
    fn assert_v_seam(&self, r: usize, c: usize, what: &str) {
        let upper: Vec<usize> = (0..self.cw)
            .filter(|&dx| self.lit[((r + 1) * self.ch - 1) * self.w + c * self.cw + dx])
            .collect();
        let lower: Vec<usize> = (0..self.cw)
            .filter(|&dx| self.lit[((r + 1) * self.ch) * self.w + c * self.cw + dx])
            .collect();
        assert!(
            !upper.is_empty(),
            "{what}: cell ({r},{c}) does not reach its bottom edge"
        );
        assert_eq!(
            upper,
            lower,
            "{what}: seam between cells ({r},{c}) and ({},{c}) gaps or jogs",
            r + 1
        );
    }
}

/// A run of the same horizontal-stroke glyph: every column of the composed
/// image, not just the seam, carries the identical lit-row set.
#[test]
fn horizontal_runs_are_column_uniform() {
    for &(cw, ch) in SIZES {
        for g in ['─', '━', '█', '▀', '▄', '═'] {
            let grid = compose(&[&[g, g, g]], cw, ch);
            let want = grid.col_rows(0, 0);
            assert!(!want.is_empty(), "{g:?} at {cw}x{ch}: blank stroke");
            for x in 1..grid.w {
                assert_eq!(
                    grid.col_rows(x, 0),
                    want,
                    "{g:?} at {cw}x{ch}: column {x} breaks the run"
                );
            }
        }
    }
}

/// A vertical stack of the same vertical-stroke glyph: every row carries the
/// identical lit-column set.
#[test]
fn vertical_stacks_are_row_uniform() {
    for &(cw, ch) in SIZES {
        for g in ['│', '┃', '█', '▌', '▐', '║'] {
            let grid = compose(&[&[g], &[g], &[g]], cw, ch);
            let want = grid.row_cols(0, 0);
            assert!(!want.is_empty(), "{g:?} at {cw}x{ch}: blank stroke");
            for y in 1..grid.h {
                assert_eq!(
                    grid.row_cols(y, 0),
                    want,
                    "{g:?} at {cw}x{ch}: row {y} breaks the stack"
                );
            }
        }
    }
}

/// The full light box-drawing junction set, assembled the way tmux assembles
/// pane borders: every interior seam must be exact.
#[test]
fn light_box_grid_seams_are_exact() {
    #[rustfmt::skip]
    let rows: &[&[char]] = &[
        &['┌', '─', '┬', '─', '┐'],
        &['│', ' ', '│', ' ', '│'],
        &['├', '─', '┼', '─', '┤'],
        &['│', ' ', '│', ' ', '│'],
        &['└', '─', '┴', '─', '┘'],
    ];
    for &(cw, ch) in SIZES {
        let g = compose(rows, cw, ch);
        let what = format!("light box at {cw}x{ch}");
        for r in [0, 2, 4] {
            for c in 0..4 {
                g.assert_h_seam(r, c, &what);
            }
        }
        for c in [0, 2, 4] {
            for r in 0..4 {
                g.assert_v_seam(r, c, &what);
            }
        }
    }
}

/// The heavy junction set under the same assembly.
#[test]
fn heavy_box_grid_seams_are_exact() {
    #[rustfmt::skip]
    let rows: &[&[char]] = &[
        &['┏', '━', '┳', '━', '┓'],
        &['┃', ' ', '┃', ' ', '┃'],
        &['┣', '━', '╋', '━', '┫'],
        &['┃', ' ', '┃', ' ', '┃'],
        &['┗', '━', '┻', '━', '┛'],
    ];
    for &(cw, ch) in SIZES {
        let g = compose(rows, cw, ch);
        let what = format!("heavy box at {cw}x{ch}");
        for r in [0, 2, 4] {
            for c in 0..4 {
                g.assert_h_seam(r, c, &what);
            }
        }
        for c in [0, 2, 4] {
            for r in 0..4 {
                g.assert_v_seam(r, c, &what);
            }
        }
    }
}

/// The double-line junction set: both rails must continue exactly.
#[test]
fn double_box_grid_seams_are_exact() {
    #[rustfmt::skip]
    let rows: &[&[char]] = &[
        &['╔', '═', '╦', '═', '╗'],
        &['║', ' ', '║', ' ', '║'],
        &['╠', '═', '╬', '═', '╣'],
        &['║', ' ', '║', ' ', '║'],
        &['╚', '═', '╩', '═', '╝'],
    ];
    for &(cw, ch) in SIZES {
        let g = compose(rows, cw, ch);
        let what = format!("double box at {cw}x{ch}");
        for r in [0, 2, 4] {
            for c in 0..4 {
                g.assert_h_seam(r, c, &what);
            }
        }
        for c in [0, 2, 4] {
            for r in 0..4 {
                g.assert_v_seam(r, c, &what);
            }
        }
    }
}

/// Full/partial blocks butted against each other: ▐ then ▌ forms a solid
/// column pair across the seam; █ tiles solidly in both directions.
#[test]
fn block_adjacency_seams_are_exact() {
    for &(cw, ch) in SIZES {
        let what = format!("blocks at {cw}x{ch}");
        // ▐▌: both boundary columns are full-height.
        let g = compose(&[&['▐', '▌']], cw, ch);
        g.assert_h_seam(0, 0, &what);
        assert_eq!(
            g.col_rows(cw - 1, 0).len(),
            ch,
            "{what}: ▐ must fill its last column"
        );
        // █ over █ and █ beside █.
        let g = compose(&[&['█', '█']], cw, ch);
        g.assert_h_seam(0, 0, &what);
        let g = compose(&[&['█'], &['█']], cw, ch);
        g.assert_v_seam(0, 0, &what);
        // ▄ beside ▄ then ▀ beside ▀: the half-height strokes line up.
        let g = compose(&[&['▄', '▄']], cw, ch);
        g.assert_h_seam(0, 0, &what);
        let g = compose(&[&['▀', '▀']], cw, ch);
        g.assert_h_seam(0, 0, &what);
    }
}

/// Diagonals light both cell corners they run between, so a chain of them
/// continues through the cell corners.
#[test]
fn diagonals_meet_cell_corners() {
    for &(cw, ch) in SIZES {
        let cov = procedural::coverage('╲', cw, ch).expect("diagonal");
        assert!(cov[0] != 0, "╲ at {cw}x{ch} must light its top-left corner");
        assert!(
            cov[(ch - 1) * cw + (cw - 1)] != 0,
            "╲ at {cw}x{ch} must light its bottom-right corner"
        );
        let cov = procedural::coverage('╱', cw, ch).expect("diagonal");
        assert!(
            cov[cw - 1] != 0,
            "╱ at {cw}x{ch} must light its top-right corner"
        );
        assert!(
            cov[(ch - 1) * cw] != 0,
            "╱ at {cw}x{ch} must light its bottom-left corner"
        );
    }
}
