// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phase-keyed ░▒▓ dithers tile with a uniform 2-pixel period across cell
//! seams at any cell width. With cell-local parity, an odd width (9) lights
//! the last column of one cell and the first of the next: a doubled line at
//! every seam.
//!
//! The ty-checked twin is `aterm_spec::derive::shade_phase_model()` (invariant
//! `lit + parity = 1`, the pattern a function of absolute column parity;
//! `Buggy = 1` is cell-local parity). This file checks the same law on the real
//! rasterizer and key plumbing (`coverage_phased` with the phase
//! `shade_phase_key` derives), over odd/even widths and pads.

use aterm_render::{FaceId, GlyphClass, GlyphKey, StyleBits, procedural};

/// Compose `cells` horizontal repeats of `ch` at `cell_w x cell_h`, each keyed
/// with the phase of its absolute pixel origin (`pad + col * cell_w`), as the
/// CPU blit and GPU quad emission do. Returns the `cells*cell_w x cell_h` strip.
fn compose_row(
    ch: char,
    cell_w: usize,
    cell_h: usize,
    cells: usize,
    pad: usize,
    phased: bool,
) -> Vec<u8> {
    let w = cells * cell_w;
    let mut strip = vec![0u8; w * cell_h];
    for c in 0..cells {
        let x0 = pad + c * cell_w;
        let (px, py) = if phased {
            (x0 & 1 == 1, pad & 1 == 1)
        } else {
            (false, false)
        };
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
                let strip = compose_row(ch, cw, chh, cells, pad, true);
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

/// Negative control: phase-less cells at `cell_w = 9` double the ▒ line at the
/// seam and break the uniform-period law.
#[test]
fn cell_local_parity_would_double_the_seam_line() {
    let (cw, chh) = (9usize, 19usize);
    let w = 2 * cw;
    let strip = compose_row('▒', cw, chh, 2, 0, false);
    // Columns 8 and 9 are both "even" under cell-local parity.
    let same_at_seam = (0..chh).all(|y| strip[y * w + cw - 1] == strip[y * w + cw]);
    assert!(
        same_at_seam,
        "pre-fix reproduction changed: expected the doubled line at the seam"
    );
    // And that breaks the 2-period law the phased composition satisfies.
    let violates = (0..chh).any(|y| (0..w - 2).any(|x| strip[y * w + x] != strip[y * w + x + 2]));
    assert!(
        violates,
        "phase-less composition unexpectedly satisfies the uniform period — \
         the negative control is vacuous"
    );
}

/// A phase variant is the base pattern re-anchored, not a new dither: phase
/// (1,0) is the unphased pattern one absolute column over. Shades stay hard
/// 0/255 at every phase (the CPU==GPU exactness domain).
#[test]
fn phase_variants_are_shifted_not_reinvented() {
    for ch in ['░', '▒', '▓'] {
        let (w, h) = (9usize, 19usize);
        let base = procedural::coverage_phased(ch, w + 1, h + 1, false, false).unwrap();
        for (px, py) in [(false, false), (true, false), (false, true), (true, true)] {
            let cov = procedural::coverage_phased(ch, w, h, px, py).unwrap();
            assert!(
                cov.iter().all(|&b| b == 0 || b == 255),
                "{ch:?} phase ({px},{py}): shades must stay hard 0/255"
            );
            let (dx, dy) = (usize::from(px), usize::from(py));
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        cov[y * w + x],
                        base[(y + dy) * (w + 1) + (x + dx)],
                        "{ch:?} phase ({px},{py}) at ({x},{y}): not the shifted base pattern"
                    );
                }
            }
        }
    }
}

/// `shade_phase_key` folds the phase bits into shade keys only, keyed on the
/// pixel-origin parity, and leaves every other key untouched, so the blit and
/// quad sites apply it unconditionally.
#[test]
fn shade_phase_key_folds_only_shades() {
    let px_q = GlyphKey::quantize_px(18.0);
    let shade = GlyphKey::mono_char(FaceId::Procedural, '▒', StyleBits::REGULAR, px_q);
    // Even origin: untouched. Odd x / odd y: the documented bits.
    assert_eq!(aterm_render::shade_phase_key(shade, 0, 0), shade);
    assert_eq!(
        aterm_render::shade_phase_key(shade, 9, 0).ch_or_id,
        0x2592 | aterm_render::SHADE_PHASE_X_BIT
    );
    assert_eq!(
        aterm_render::shade_phase_key(shade, 18, 19).ch_or_id,
        0x2592 | aterm_render::SHADE_PHASE_Y_BIT
    );
    assert_eq!(
        aterm_render::shade_phase_key(shade, 9, 19).ch_or_id,
        0x2592 | aterm_render::SHADE_PHASE_X_BIT | aterm_render::SHADE_PHASE_Y_BIT
    );
    // The base code point stays recoverable above the phase bits.
    let folded = aterm_render::shade_phase_key(shade, 9, 19);
    assert_eq!(
        folded.ch_or_id & !(aterm_render::SHADE_PHASE_X_BIT | aterm_render::SHADE_PHASE_Y_BIT),
        0x2592
    );
    assert_eq!(folded.glyph_class, GlyphClass::Mono);
    // Non-shade procedural, non-procedural, and gid-class keys pass through.
    let boxch = GlyphKey::mono_char(FaceId::Procedural, '─', StyleBits::REGULAR, px_q);
    assert_eq!(aterm_render::shade_phase_key(boxch, 9, 19), boxch);
    let primary = GlyphKey::mono_char(FaceId::Primary, '▒', StyleBits::REGULAR, px_q);
    assert_eq!(aterm_render::shade_phase_key(primary, 9, 19), primary);
    let gid = GlyphKey::mono_gid(0x2592, StyleBits::REGULAR, px_q);
    assert_eq!(aterm_render::shade_phase_key(gid, 9, 19), gid);
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
