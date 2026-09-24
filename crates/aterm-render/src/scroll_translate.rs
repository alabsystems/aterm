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
//! # Exposed strip: the incoming-row apron
//!
//! When the band shifts, the `|frac|`-px strip at the exposed edge (BOTTOM for
//! an up-glide, TOP for a down-bounce) has no in-band source. The translate
//! itself leaves the band's own pixels there, and then:
//!
//! * **Up-glide (`frac > 0`)** — the strip is the row sliding IN from just
//!   below the viewport, and it is on glass for the WHOLE gesture under 1:1
//!   trackpad tracking. The engine carries that row as
//!   [`RenderInput::apron_row`](aterm_core::render::RenderInput::apron_row)
//!   (the apron at offset `d` IS the bottom viewport row at offset `d - 1`), the
//!   renderer rasters it into a one-row scratch through the SAME phase runner
//!   every viewport row takes ([`crate::ApronScratch`]), and
//!   [`paint_incoming_strip`] lays its TOP `frac` px over the strip — AFTER the
//!   translate, INSIDE the band (chrome stays pinned: invariant 2 is untouched).
//!   The GPU backends do not raster it a second time: they upload the CPU
//!   raster and COPY it onto the same strip, so the strip is byte-identical
//!   across backends by construction (`shift_present_copy_band` /
//!   `ApronStripPass`), and the frame that lands the row (`frac == 0`)
//!   re-rasters it as an ordinary viewport row.
//! * **Down-bounce (`frac < 0`)** — the placeholder is correct: overscroll past
//!   a history end exposes empty rubber-band gap, and there IS no row beyond
//!   the end. [`incoming_strip`] is `None` there.
//!
//! [`incoming_row_applies`] is the ONE predicate both backends consult for
//! WHETHER a frame owes a strip (a present apron, a positive residual, a band
//! that reaches the frame's last row so the row below the band is the row below
//! the viewport, no wallpaper), and [`incoming_strip`] the ONE geometry; a
//! backend that drifts from either is a byte diff, not a shared assumption.
//!
//! Known residuals, on purpose: the strip is one row rastered in isolation, so
//! a neighbouring row's glyph overshoot across the seam, a selection band that
//! reaches the incoming row, and an inline image on it appear only when the row
//! lands; a wallpaper frame keeps the placeholder (its backdrop is frame-fixed,
//! not row-fixed).

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
/// — the down-bounce's rubber-band gap, and the up-glide's bottom strip until
/// [`paint_incoming_strip`] lays the incoming row over it (the same order on
/// both backends). `|frac| >= band_h` exposes
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

/// WHETHER a frame owes the incoming-row strip — the ONE predicate both
/// backends consult, so they cannot disagree about the strip's existence.
///
/// * a POSITIVE residual (the up-glide; a bounce exposes rubber-band gap);
/// * a PRESENT apron (`display_offset > 0`: a row lies below the viewport);
/// * a band that reaches the frame's LAST row (`grid_bot_row == rows`), so the
///   row below the band is the row below the viewport — a bottom chrome row
///   inside the frame (the find bar floated to the bottom) would put the row
///   under that chrome instead, and the engine apron would be the wrong row;
/// * no wallpaper (its backdrop is frame-fixed, so a row rastered in isolation
///   would carry the wrong texels).
#[must_use]
pub fn incoming_row_applies(input: &aterm_core::render::RenderInput) -> bool {
    input.scroll_frac_px > 0
        && input.apron_row.present
        && input.grid_bot_row == input.rows
        && input.wallpaper.is_none()
}

/// The incoming-row strip's geometry `(dst_y, rows)` for a band `[y0, y1)`
/// shifted by `frac_px`: the BOTTOM `frac` rows of the band, `[y1 - frac, y1)`.
/// `None` when nothing is owed — a non-positive residual (no up-glide), an
/// empty band, or a residual that exposes the WHOLE band (`frac >= band_h`:
/// nothing moved, so there is no seam to fill — the same `None` the GPU's
/// `band_shift_ops` answers there).
#[must_use]
pub fn incoming_strip(y0: usize, y1: usize, frac_px: i64) -> Option<(usize, usize)> {
    if frac_px <= 0 || y0 >= y1 {
        return None;
    }
    let n = usize::try_from(frac_px).ok()?;
    if n >= y1 - y0 {
        return None;
    }
    Some((y1 - n, n))
}

/// Lay the incoming row's TOP rows over the strip an up-glide exposed: after
/// [`translate_grid_band_in_place`] has shifted `[y0, y1)` of `buf` (a
/// `w`-wide framebuffer) UP by `frac_px`, copy row `i` of `apron` (a `w`-wide,
/// row-major raster of the incoming row, its top row first) onto framebuffer
/// row `y1 - frac + i` for `i < n`, where `n` is [`incoming_strip`]'s row count
/// clamped to the rows `apron` actually holds — apron row 0 always lands on
/// the strip's FIRST row (the incoming row's top edge meets the band's bottom
/// row), so a short apron fills the strip's top rows and leaves the rest as
/// the placeholder. Writes ONLY inside `[y1 - frac, y1)` — the exposed strip —
/// so every moved interior row and every chrome row
/// outside the band is untouched (the chrome-invariance theorem still holds
/// with the apron painted). Returns the rows painted (`0` == a literal no-op:
/// a bounce, a frac-0 frame, an empty band, or an empty apron).
pub fn paint_incoming_strip(
    buf: &mut [u32],
    w: usize,
    y0: usize,
    y1: usize,
    frac_px: i64,
    apron: &[u32],
) -> usize {
    if w == 0 {
        return 0;
    }
    let Some((dst_y, n)) = incoming_strip(y0, y1, frac_px) else {
        return 0;
    };
    let n = n
        .min(apron.len() / w)
        .min((buf.len() / w).saturating_sub(dst_y));
    for i in 0..n {
        let dst = (dst_y + i) * w;
        buf[dst..dst + w].copy_from_slice(&apron[i * w..i * w + w]);
    }
    n
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

    /// INCOMING-ROW STRIP geometry law: an up-glide owes `[y1 - frac, y1)`; a
    /// bounce, a zero residual, an empty band and a fully-exposed band owe
    /// nothing — the same `None` the GPU's staged move answers there.
    #[test]
    fn incoming_strip_geometry_law() {
        assert_eq!(incoming_strip(3, 13, 4), Some((9, 4)));
        assert_eq!(incoming_strip(0, 10, 1), Some((9, 1)));
        assert_eq!(incoming_strip(3, 13, 0), None, "frac 0: nothing exposed");
        assert_eq!(
            incoming_strip(3, 13, -4),
            None,
            "a bounce exposes rubber-band gap"
        );
        assert_eq!(incoming_strip(5, 5, 2), None, "empty band");
        assert_eq!(incoming_strip(7, 3, 2), None, "degenerate band");
        assert_eq!(
            incoming_strip(3, 13, 10),
            None,
            "whole band exposed: nothing moved"
        );
        assert_eq!(
            incoming_strip(3, 13, 9),
            Some((4, 9)),
            "one row still moves"
        );
    }

    /// INCOMING-ROW STRIP painter: after the up-translate, the apron's TOP `frac`
    /// rows land on exactly the exposed strip; every interior (moved) row and
    /// every chrome row is byte-identical to the translate alone; a short apron
    /// never reads past its end; a bounce / frac 0 paints nothing. Against a
    /// translate WITHOUT the painter the strip holds the band's own pixels — the
    /// retired placeholder — which is the first assertion's negative control.
    #[test]
    fn paint_incoming_strip_fills_only_the_exposed_strip() {
        let (w, h) = (5usize, 20usize);
        let base = checkerboard(w, h);
        let (y0, y1, cell_h) = (2usize, 17usize, 6usize);
        // The apron: a recognisable one-row raster (`cell_h` rows) whose pixels
        // cannot collide with the checkerboard (a distinct high byte).
        let apron: Vec<u32> = (0..w * cell_h)
            .map(|i| 0x0A00_0000 | ((i / w) as u32) << 8 | (i % w) as u32)
            .collect();
        for frac in 1..cell_h as i64 {
            let mut only_shift = base.clone();
            translate_grid_band_in_place(&mut only_shift, w, y0, y1, frac);
            let mut painted = only_shift.clone();
            let n = paint_incoming_strip(&mut painted, w, y0, y1, frac, &apron);
            assert_eq!(n, frac as usize, "frac={frac}: every strip row painted");
            let strip_y0 = y1 - frac as usize;
            for y in 0..h {
                let row = &painted[y * w..y * w + w];
                if (strip_y0..y1).contains(&y) {
                    let i = y - strip_y0;
                    assert_eq!(
                        row,
                        &apron[i * w..i * w + w],
                        "frac={frac}: strip row {y} is apron row {i}"
                    );
                    assert_ne!(
                        row,
                        &only_shift[y * w..y * w + w],
                        "frac={frac}: the placeholder is gone at row {y}"
                    );
                } else {
                    assert_eq!(
                        row,
                        &only_shift[y * w..y * w + w],
                        "frac={frac}: row {y} untouched"
                    );
                }
            }
        }
        // A short apron (fewer rows than the strip) paints what it has, no more —
        // TOP-aligned: apron row 0 is the incoming row's top edge and meets the
        // band's bottom row at `y1 - frac`; the strip's remaining rows keep the
        // placeholder.
        let mut painted = base.clone();
        translate_grid_band_in_place(&mut painted, w, y0, y1, 4);
        let placeholder = painted.clone();
        let n = paint_incoming_strip(&mut painted, w, y0, y1, 4, &apron[..2 * w]);
        assert_eq!(n, 2, "a two-row apron paints two rows");
        assert_eq!(&painted[(y1 - 4) * w..(y1 - 2) * w], &apron[..2 * w]);
        assert_eq!(
            &painted[(y1 - 2) * w..y1 * w],
            &placeholder[(y1 - 2) * w..y1 * w],
            "the unfilled remainder of the strip keeps the placeholder"
        );
        // Nothing owed: a bounce, frac 0, an empty apron.
        for (frac, ap) in [(-3i64, &apron[..]), (0, &apron[..]), (3, &apron[..0])] {
            let mut buf = base.clone();
            translate_grid_band_in_place(&mut buf, w, y0, y1, frac);
            let before = buf.clone();
            assert_eq!(paint_incoming_strip(&mut buf, w, y0, y1, frac, ap), 0);
            assert_eq!(buf, before, "frac={frac}: a literal no-op");
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
