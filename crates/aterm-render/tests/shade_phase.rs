// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phase-keyed ░▒▓ dithers tile with a uniform 2-pixel period across cell
//! seams at any cell width. With cell-local parity, an odd width (9) lights
//! the last column of one cell and the first of the next: a doubled line at
//! every seam.
//!
//! This file checks that the pattern is a function of absolute column parity,
//! not cell-local parity, on the real rasterizer and key plumbing
//! (`coverage_phased` with the phase `shade_phase_key` derives), over odd/even
//! widths and pads.

use aterm_render::procedural;

/// Compose `cells` horizontal repeats of `ch` at `cell_w x cell_h`, each keyed
/// with the phase of its absolute pixel origin (`pad + col * cell_w`), as the
/// CPU blit and GPU quad emission do. Returns the `cells*cell_w x cell_h` strip.
fn compose_row(ch: char, cell_w: usize, cell_h: usize, cells: usize, pad: usize) -> Vec<u8> {
    let w = cells * cell_w;
    let mut strip = vec![0u8; w * cell_h];
    for c in 0..cells {
        let x0 = pad + c * cell_w;
        let (px, py) = (x0 & 1 == 1, pad & 1 == 1);
        let cov = procedural::coverage_phased(ch, cell_w, cell_h, px, py).expect("shade glyph");
        for y in 0..cell_h {
            for x in 0..cell_w {
                strip[y * w + c * cell_w + x] = cov[y * cell_w + x];
            }
        }
    }
    strip
}

/// At every width and pad parity, a run of shade cells is globally 2-periodic
/// in x: every column equals the column two to its right, across seams.
#[test]
fn adjacent_shade_cells_tile_with_uniform_period() {
    for ch in ['░', '▒', '▓'] {
        for &(cw, chh) in &[(9usize, 19usize), (7, 15), (11, 3), (8, 16), (10, 20)] {
            for pad in [0usize, 1] {
                let cells = 3;
                let w = cells * cw;
                let strip = compose_row(ch, cw, chh, cells, pad);
                for y in 0..chh {
                    for x in 0..w - 2 {
                        assert_eq!(
                            strip[y * w + x],
                            strip[y * w + x + 2],
                            "{ch:?} cw={cw} pad={pad}: column {x} vs {} differ at row {y} — \
                             the dither period is not uniform across the seam",
                            x + 2
                        );
                    }
                }
            }
        }
    }
}

/// End-to-end on a CPU frame: a rendered ▒ run is globally 2-periodic in x,
/// seams included, at the host font's cell width and an odd pad (which flips
/// every cell's absolute phase).
#[test]
fn rendered_shade_run_is_globally_periodic() {
    use aterm_core::terminal::Terminal;
    let mut r = aterm_render::Renderer::from_system(18.0, aterm_render::Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    r.set_pad(1); // odd pad: every cell origin parity flips
    let (cw, ch) = r.cell_size();
    let (rows, cols) = (2usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l▒▒▒▒▒▒▒▒▒▒▒▒".as_bytes());
    let input = term.cell_frame(rows, cols);
    let frame = r.render_input(&input);
    // The shade band: rows [1, 1+ch), columns [1, 1+cols*cw).
    for y in 1..1 + ch {
        for x in 1..1 + cols * cw - 2 {
            assert_eq!(
                frame.pixels[y * frame.width + x],
                frame.pixels[y * frame.width + x + 2],
                "rendered ▒ run not 2-periodic at ({x},{y}) with cw={cw} — \
                 seam phase broken in the real blit path"
            );
        }
    }
}
