// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SUB-ROW SCROLL TRANSLATE (M1b) — the display-only vertical shift that turns
//! M1's proven scroll KINEMATICS into pixel-true motion.
//!
//! M1 landed the pure motion core (`aterm-gui`'s `scroll_motion`): the model
//! banks a SIGNED fractional-pixel residual `scroll_frac_px ∈ (-cell_h, cell_h)`
//! below every whole-row scroll (the `frac` half of `decompose`'s Euclidean split
//! for the glide, the elastic-overscroll spring displacement at a history end).
//! This module is the render-side consumer: at PRESENT time it shifts the
//! TERMINAL-CONTENT pixel band by that residual — UP for a positive frac (the
//! glide's incoming row appears at the bottom), DOWN for a negative frac (the
//! rubber-band bounce sags the content, exposing a strip at the top) — so
//! scrolling glides by the pixel instead of jumping by the row and history-end
//! overscroll springs back visibly. Glyph rasters and glyph shaping are UNTOUCHED
//! — the translate is an integer memmove of already-rendered pixels, so text
//! stays raster-exact while it moves (`translate_grid_band_in_place`).
//!
//! # The chrome exemption (the key theorem)
//!
//! The windowed compose path splices non-terminal chrome into the SAME
//! `RenderInput` rows as terminal content: the tab strip is PREPENDED at the top
//! (`app_render::prepend_strip_rows`), transient bars can occupy an edge row, and
//! split-pane dividers sit between panes. Those rows must stay PINNED while the
//! grid glides underneath them. So the translate
//! is confined to the grid pixel band `[y0, y1)` derived from the frame's
//! `[grid_top_row, grid_bot_row)` partition ([`grid_band_px`]); every pixel
//! OUTSIDE that band is left byte-for-byte identical. That structural confinement
//! is exactly [`translate_grid_band_in_place`]'s contract and is proven below
//! (`chrome_pixels_are_invariant`) — the M1b chrome-invariance theorem.
//!
//! # Invariants (proven)
//!
//! 1. **Identity at `frac == 0`.** A zero residual moves nothing: the present
//!    buffer is byte-identical to the untranslated frame — so a frame at
//!    `scroll_frac_px == 0` equals the same viewport reached by a whole-row jump
//!    (`frac_zero_is_identity`). This is the raster-invariance PROVE bullet.
//! 2. **Chrome invariance.** For ANY `frac`, every pixel with a row index outside
//!    `[y0, y1)` is byte-identical to the input; only the grid band is rewritten
//!    (`chrome_pixels_are_invariant`, `only_the_band_is_written`). The abstract
//!    twin is `aterm_spec::derive::grid_translate_model` (a row shifts IFF it is
//!    in the grid band), proven by the real `ty` at `Buggy=0` and counterexampled
//!    by a mutant that leaks the shift onto a chrome row (`Buggy=1`).
//! 3. **Band derivation.** [`grid_band_px`] reproduces the renderer's own row→px
//!    mapping (`pad + row*cell_h`) and clamps to the framebuffer, so the band is
//!    exactly the terminal-content rows' pixels (`band_matches_row_mapping`).
//!
//! Division/multiplication in the row→px mapping are outside the `ty` `Expr`
//! language, so invariant 3 is proven by an exhaustive small-lattice cargo test
//! (the documented waiver, mirroring the box-drawing rounding law); the boolean
//! chrome-partition policy is what the `ty` twin carries.
//!
//! # Exposed strip: the incoming-row placeholder (DEFERRED, on purpose)
//!
//! When the band shifts, the `|frac|`-px strip at the exposed edge (BOTTOM for an
//! up-glide, TOP for a down-bounce) has no in-band source, so it retains the
//! band's own pixels as a PLACEHOLDER — identical on both backends, so parity
//! holds. For a down-bounce the placeholder is correct: overscroll past a history
//! end exposes empty rubber-band gap, and there IS no row beyond the end. For an
//! up-glide the strip should instead show the TOP `frac` px of the row sliding in
//! from just below the viewport (viewport row `grid_bot_row`).
//!
//! Rendering that incoming row RASTER-EXACT was scoped and DEFERRED — it is a
//! genuine cross-crate architectural change, not a local tweak here:
//!
//! * The surface is FIXED at `pad*2 + rows*cell_h` and CHROME lives IN the
//!   framebuffer (the tab strip sits above `grid_top_row`; transient edge bars
//!   can bound `grid_bot_row`). There is no spare band below the grid to hold an extra
//!   row's pixels; the incoming row would have to be rastered into an
//!   OFF-surface scratch (`w × cell_h`) and its top `frac` px composited into
//!   the strip.
//! * The engine (`aterm_core::Terminal::cell_frame_into`) supplies exactly `rows`
//!   rows; the incoming row is one PAST the viewport, so it would need a new
//!   `RenderInput` field (the row's cells + clusters/combining/line-size/images)
//!   threaded through the snapshot builder.
//! * BOTH backends would need a new one-row raster-into-strip path kept in
//!   CPU==GPU byte-parity — cheap on the CPU (`render_row` into a scratch), but on
//!   the GPU a SEPARATE one-row instanced pass into a scratch texture, wired to
//!   agree with the CPU strip bit-for-bit. That is the sacred-parity risk.
//!
//! The strip is only `frac ∈ (0, cell_h)` px tall and shows for a sub-second
//! glide, so the placeholder is a minor transient during fast motion; landing the
//! raster-exact strip is a follow-up that must not be half-wired (the dormant-code
//! gate would flag an unused engine field). Until then the placeholder is the
//! deliberate, parity-safe behaviour.

/// The terminal-content PIXEL band `[y0, y1)` for a frame whose grid rows are
/// `[grid_top_row, grid_bot_row)`, given the grid content's Y-origin `pad` (the
/// renderer passes `grid_top()` = interior pad + head band; `pad + head == pad`
/// when headless) and cell height `cell_h`, clamped to the framebuffer height
/// `h`.
///
/// Reproduces the renderer's row→pixel mapping exactly: row `r`'s top device
/// pixel is `grid_top + r * cell_h` (see `render_row`). Returns an EMPTY band
/// (`y0 >= y1`) when there is no grid to translate — `grid_bot_row == 0` (the
/// default / no-partition case), a degenerate partition (`top >= bot`), or a band
/// entirely below the framebuffer — so the caller's `y0 < y1` guard collapses to
/// the byte-identical no-translate path.
#[must_use]
pub fn grid_band_px(
    pad: usize,
    cell_h: usize,
    grid_top_row: usize,
    grid_bot_row: usize,
    h: usize,
) -> (usize, usize) {
    if grid_bot_row <= grid_top_row {
        return (0, 0);
    }
    let y0 = (pad + grid_top_row.saturating_mul(cell_h)).min(h);
    let y1 = (pad + grid_bot_row.saturating_mul(cell_h)).min(h);
    (y0, y1)
}

/// Shift the pixel rows of the grid band `[y0, y1)` of `buf` (a `w`-wide,
/// row-major framebuffer) by `frac_px` device pixels, IN PLACE — BIDIRECTIONAL.
/// Rows outside the band are never touched — the chrome-invariance contract.
///
/// The sign of `frac_px` selects the direction (the SIGNED `scroll_frac_px` the
/// kinematics bank — a smooth-scroll glide residual OR the elastic-overscroll
/// spring bounce):
///
/// * **POSITIVE — shift UP** (the whole-row glide residual: content glides up,
///   the row scrolling in from BELOW appears at the bottom). Destination row
///   `dy ∈ [y0, y1)` takes source row `dy + |frac|` when that source is still
///   within the band; the bottom `|frac|` rows are the exposed strip. The copy
///   walks the band top-DOWN and every source row (`dy + |frac|`) sits strictly
///   BELOW the destination it feeds, so no source row is read after being
///   overwritten.
/// * **NEGATIVE — shift DOWN** (the elastic overscroll bounce at the history
///   TOP, and the snap-to-bottom rubber-band: content sags down, exposing a strip
///   at the TOP). Destination row `dy` takes source row `dy - |frac|` when that
///   source is still within the band; the TOP `|frac|` rows are the exposed
///   strip. The copy walks the band bottom-UP so every source row (`dy - |frac|`,
///   strictly ABOVE the destination) is read before being overwritten.
///
/// Either way the exposed strip retains the band's own content as a placeholder
/// (identical on both backends, so parity holds — the raster-exact incoming row
/// is documented-deferred; see the crate PROVE notes). `|frac| >= band_h` exposes
/// the whole band (no in-band source survives). Because the in-place walk order
/// matches the shift direction, the move equals a copy from a pristine snapshot
/// (proven by `matches_out_of_place`, both signs).
///
/// `frac_px == 0` is a literal no-op (every row copies onto itself, none exposed)
/// — the identity that makes a whole-row-jump frame byte-identical.
pub fn translate_grid_band_in_place(buf: &mut [u32], w: usize, y0: usize, y1: usize, frac_px: i64) {
    if w == 0 || y0 >= y1 || frac_px == 0 {
        return;
    }
    let band_h = y1 - y0;
    let mag = frac_px.unsigned_abs() as usize;
    // The number of destination rows that pull from a still-in-band source; the
    // remaining `min(mag, band_h)` rows at the exposed edge are the placeholder.
    let moved = band_h.saturating_sub(mag);
    if frac_px > 0 {
        // Shift UP: dst pulls from `dst + mag` (below). Walk top-DOWN — every
        // source sits strictly below its destination, read before overwrite.
        for i in 0..moved {
            let dst = y0 + i;
            let src = dst + mag; // < y1 by construction (i < band_h - mag)
            debug_assert!(src < y1, "source row must stay inside the band");
            // Rows are disjoint (dst < src) but `copy_within` tolerates overlap anyway.
            buf.copy_within(src * w..src * w + w, dst * w);
        }
        // The bottom `band_h - moved` rows keep their existing pixels (placeholder).
    } else {
        // Shift DOWN: dst pulls from `dst - mag` (above). Walk bottom-UP — every
        // source sits strictly above its destination, read before overwrite.
        for i in 0..moved {
            let dst = y1 - 1 - i;
            let src = dst - mag; // >= y0 by construction (i < band_h - mag)
            debug_assert!(src >= y0, "source row must stay inside the band");
            buf.copy_within(src * w..src * w + w, dst * w);
        }
        // The top `band_h - moved` rows keep their existing pixels (placeholder).
    }
}

#[cfg(test)]
mod tests {
    //! M1b PROVE bullets, always-on layer (real pixel buffers). The boolean
    //! grid-vs-chrome partition additionally carries a `ty` twin
    //! (`aterm_spec::derive::grid_translate_model`); these tests bind the pixel
    //! behaviour to that policy. Row→px arithmetic (`pad + r*cell_h`) is outside
    //! the `ty` `Expr` language, so the lattice/exhaustive tests here are the
    //! proof layer for the band derivation (the documented waiver).

    use super::*;

    /// A recognisable framebuffer: pixel value encodes its (row, col) so any
    /// stray move is detectable. `0xRRRRCCCC`-ish packing over a small grid.
    fn checkerboard(w: usize, h: usize) -> Vec<u32> {
        (0..w * h)
            .map(|i| {
                let (y, x) = (i / w, i % w);
                ((y as u32) << 16) | (x as u32) | 0x0100_0000
            })
            .collect()
    }

    /// Out-of-place reference translate (BIDIRECTIONAL): build the shifted band
    /// into a fresh copy by reading ONLY from the pristine source. The in-place
    /// routine must equal this for every input (proves the direction-matched walk
    /// never reads an overwritten row), for both signs of `frac`.
    fn reference(src: &[u32], w: usize, y0: usize, y1: usize, frac: i64) -> Vec<u32> {
        let mut out = src.to_vec();
        if w == 0 || y0 >= y1 || frac == 0 {
            return out;
        }
        let mag = frac.unsigned_abs() as usize;
        let moved = (y1 - y0).saturating_sub(mag);
        for i in 0..moved {
            // UP: dst = y0+i pulls from below (dst+mag); DOWN: dst pulls from above
            // (dst-mag). Reading only `src`, iteration order is irrelevant here.
            let (dst, s) = if frac > 0 {
                (y0 + i, y0 + i + mag)
            } else {
                (y0 + mag + i, y0 + i)
            };
            out[dst * w..dst * w + w].copy_from_slice(&src[s * w..s * w + w]);
        }
        out
    }

    /// PROVE (1) — identity at `frac == 0`: the present buffer is byte-identical
    /// to the untranslated frame, over a band × geometry lattice. This is the
    /// raster-invariance bullet: a `scroll_frac_px == 0` frame equals the
    /// whole-row-jump frame exactly.
    #[test]
    fn frac_zero_is_identity() {
        for (w, h) in [(1usize, 1usize), (4, 10), (7, 33)] {
            let base = checkerboard(w, h);
            for y0 in 0..h {
                for y1 in y0..=h {
                    let mut buf = base.clone();
                    translate_grid_band_in_place(&mut buf, w, y0, y1, 0);
                    assert_eq!(
                        buf, base,
                        "frac=0 must be a literal no-op (w={w} h={h} y0={y0} y1={y1})"
                    );
                }
            }
        }
    }

    /// PROVE (2) — chrome invariance (the KEY theorem): for ANY frac and ANY
    /// band, every pixel OUTSIDE `[y0, y1)` is byte-identical to the input. The
    /// translate touches only the grid band, so chrome (rows `< y0` and `>= y1`)
    /// is pinned. Non-vacuity: at least one in-band pixel genuinely moves.
    #[test]
    fn chrome_pixels_are_invariant() {
        let (w, h) = (5usize, 20usize);
        let base = checkerboard(w, h);
        let mut any_moved = false;
        for y0 in 0..h {
            for y1 in y0..=h {
                // BIDIRECTIONAL: sweep the residual across BOTH signs (up-glide and
                // down-bounce) past the band height on each side.
                let span = (y1 - y0 + 2) as i64;
                for frac in -span..=span {
                    let mut buf = base.clone();
                    translate_grid_band_in_place(&mut buf, w, y0, y1, frac);
                    for y in 0..h {
                        if y < y0 || y >= y1 {
                            assert_eq!(
                                buf[y * w..y * w + w],
                                base[y * w..y * w + w],
                                "chrome row {y} must be invariant (y0={y0} y1={y1} frac={frac})"
                            );
                        }
                    }
                    if buf != base {
                        any_moved = true;
                    }
                }
            }
        }
        assert!(
            any_moved,
            "non-vacuity: some band/frac genuinely shifts pixels"
        );
    }

    /// PROVE (2), companion: the in-place move equals the out-of-place reference
    /// that reads only pristine source rows — so the top-down walk is correct
    /// (never reads an already-overwritten row) AND writes exactly the band.
    #[test]
    fn matches_out_of_place() {
        let (w, h) = (6usize, 24usize);
        let base = checkerboard(w, h);
        for y0 in 0..h {
            for y1 in y0..=h {
                // BOTH signs: the direction-matched in-place walk (top-down for up,
                // bottom-up for down) must equal the pristine-source reference.
                let span = (y1 - y0 + 1) as i64;
                for frac in -span..=span {
                    let mut buf = base.clone();
                    translate_grid_band_in_place(&mut buf, w, y0, y1, frac);
                    assert_eq!(
                        buf,
                        reference(&base, w, y0, y1, frac),
                        "in-place must equal pristine-source reference (y0={y0} y1={y1} frac={frac})"
                    );
                }
            }
        }
    }

    /// BIDIRECTIONAL non-vacuity + exposed-edge law: a POSITIVE frac exposes the
    /// BOTTOM strip (the incoming-row placeholder) and pulls the rest up; a
    /// NEGATIVE frac of equal magnitude exposes the TOP strip and pushes the rest
    /// down. The two are genuinely different frames (the shift is signed, not just
    /// a magnitude), and each moves the band's interior by exactly `|frac|` rows.
    #[test]
    fn negative_frac_shifts_down_positive_shifts_up() {
        let (w, h) = (4usize, 16usize);
        let base = checkerboard(w, h);
        let (y0, y1, mag) = (3usize, 13usize, 4usize);
        let mut up = base.clone();
        translate_grid_band_in_place(&mut up, w, y0, y1, mag as i64);
        let mut down = base.clone();
        translate_grid_band_in_place(&mut down, w, y0, y1, -(mag as i64));
        assert_ne!(up, down, "opposite signs produce opposite shifts");
        // UP: interior dst row y0 shows source y0+mag; the bottom `mag` rows are the
        // exposed placeholder (unchanged from base).
        assert_eq!(
            up[y0 * w..y0 * w + w],
            base[(y0 + mag) * w..(y0 + mag) * w + w]
        );
        for y in (y1 - mag)..y1 {
            assert_eq!(
                up[y * w..y * w + w],
                base[y * w..y * w + w],
                "up: bottom strip exposed"
            );
        }
        // DOWN: interior dst row y1-1 shows source y1-1-mag; the top `mag` rows are
        // the exposed placeholder (unchanged from base).
        assert_eq!(
            down[(y1 - 1) * w..(y1 - 1) * w + w],
            base[(y1 - 1 - mag) * w..(y1 - 1 - mag) * w + w]
        );
        for y in y0..(y0 + mag) {
            assert_eq!(
                down[y * w..y * w + w],
                base[y * w..y * w + w],
                "down: top strip exposed"
            );
        }
    }

    /// NEGATIVE CONTROL for (2): a NAIVE translate that shifts the WHOLE
    /// framebuffer (ignoring the band) genuinely disturbs chrome rows — so the
    /// chrome-invariance assertion above has teeth.
    #[test]
    fn whole_frame_shift_would_break_chrome() {
        let (w, h) = (5usize, 20usize);
        let base = checkerboard(w, h);
        // Band is the middle; chrome occupies the first and last rows.
        let (y0, y1, frac) = (2usize, h - 2, 3usize);
        // Naive: shift EVERY row up by frac.
        let mut naive = base.clone();
        for y in 0..h - frac {
            naive.copy_within((y + frac) * w..(y + frac) * w + w, y * w);
        }
        // The correct band translate leaves chrome row 0 and the last row alone.
        let mut correct = base.clone();
        translate_grid_band_in_place(&mut correct, w, y0, y1, frac as i64);
        assert_ne!(
            naive[0..w],
            base[0..w],
            "control: the naive whole-frame shift DID disturb the top chrome row"
        );
        assert_eq!(
            correct[0..w],
            base[0..w],
            "the band translate leaves the top chrome row pinned"
        );
    }

    /// PROVE (3) — band derivation matches the renderer's row→px mapping
    /// (`pad + r*cell_h`), clamped to `h`, over a pad × cell_h × partition
    /// lattice. Includes the no-band cases (`grid_bot_row == 0`, degenerate,
    /// below-frame) that collapse to the no-translate path.
    #[test]
    fn band_matches_row_mapping() {
        for pad in [0usize, 1, 2, 8] {
            for cell_h in [1usize, 7, 16, 33] {
                for top in [0usize, 1, 3] {
                    for bot in [0usize, 1, 4, 40] {
                        let h = pad * 2 + 50 * cell_h;
                        let (y0, y1) = grid_band_px(pad, cell_h, top, bot, h);
                        if bot <= top {
                            assert!(y0 >= y1, "degenerate/no-band partition ⇒ empty band");
                        } else {
                            assert_eq!(y0, (pad + top * cell_h).min(h), "y0 == pad + top*cell_h");
                            assert_eq!(y1, (pad + bot * cell_h).min(h), "y1 == pad + bot*cell_h");
                            assert!(y1 <= h, "band clamps to the framebuffer");
                        }
                    }
                }
            }
        }
        // Non-vacuity: a normal partition yields a genuine, in-frame band.
        assert_eq!(
            grid_band_px(4, 16, 1, 25, 4 + 26 * 16),
            (4 + 16, 4 + 25 * 16)
        );
        // No-band default (grid_bot_row == 0) ⇒ empty.
        let (a, b) = grid_band_px(4, 16, 0, 0, 500);
        assert!(a >= b, "grid_bot_row==0 is the no-translate default");
    }
}
