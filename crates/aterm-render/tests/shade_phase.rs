// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phase-keyed shade dithers (`░▒▓`) tile with a uniform 2-pixel period across cell seams.

use aterm_render::procedural;

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
