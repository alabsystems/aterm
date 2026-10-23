// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Procedural glyph seam-continuity tests across odd and even cell sizes.

use aterm_render::procedural;

const SIZES: &[(usize, usize)] = &[(7, 15), (8, 16), (9, 19), (10, 20), (11, 21), (12, 22)];

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
    fn col_rows(&self, x: usize, r: usize) -> Vec<usize> {
        (r * self.ch..(r + 1) * self.ch)
            .filter(|&y| self.lit[y * self.w + x])
            .collect()
    }

    fn row_cols(&self, y: usize, c: usize) -> Vec<usize> {
        (c * self.cw..(c + 1) * self.cw)
            .map(|x| x - c * self.cw)
            .filter(|&dx| self.lit[y * self.w + c * self.cw + dx])
            .collect()
    }

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

#[test]
fn box_grid_seams_are_exact() {
    #[rustfmt::skip]
    let sets: [(&str, &[&[char]]); 3] = [
        ("light", &[
            &['┌', '─', '┬', '─', '┐'],
            &['│', ' ', '│', ' ', '│'],
            &['├', '─', '┼', '─', '┤'],
            &['│', ' ', '│', ' ', '│'],
            &['└', '─', '┴', '─', '┘'],
        ]),
        ("heavy", &[
            &['┏', '━', '┳', '━', '┓'],
            &['┃', ' ', '┃', ' ', '┃'],
            &['┣', '━', '╋', '━', '┫'],
            &['┃', ' ', '┃', ' ', '┃'],
            &['┗', '━', '┻', '━', '┛'],
        ]),
        ("double", &[
            &['╔', '═', '╦', '═', '╗'],
            &['║', ' ', '║', ' ', '║'],
            &['╠', '═', '╬', '═', '╣'],
            &['║', ' ', '║', ' ', '║'],
            &['╚', '═', '╩', '═', '╝'],
        ]),
    ];
    for (name, rows) in sets {
        for &(cw, ch) in SIZES {
            let g = compose(rows, cw, ch);
            let what = format!("{name} box at {cw}x{ch}");
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
}

#[test]
fn block_adjacency_seams_are_exact() {
    for &(cw, ch) in SIZES {
        let what = format!("blocks at {cw}x{ch}");
        let g = compose(&[&['▐', '▌']], cw, ch);
        g.assert_h_seam(0, 0, &what);
        assert_eq!(
            g.col_rows(cw - 1, 0).len(),
            ch,
            "{what}: ▐ must fill its last column"
        );
        let g = compose(&[&['█', '█']], cw, ch);
        g.assert_h_seam(0, 0, &what);
        let g = compose(&[&['█'], &['█']], cw, ch);
        g.assert_v_seam(0, 0, &what);
        let g = compose(&[&['▄', '▄']], cw, ch);
        g.assert_h_seam(0, 0, &what);
        let g = compose(&[&['▀', '▀']], cw, ch);
        g.assert_h_seam(0, 0, &what);
    }
}

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
