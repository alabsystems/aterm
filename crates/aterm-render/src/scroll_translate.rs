// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sub-row scroll translate: the display-only vertical shift that turns the scroll
//! kinematics' fractional residual into pixel motion.
//!
//! `aterm-gui`'s `scroll_motion` banks a signed residual `scroll_frac_px ∈ (-cell_h,
//! cell_h)` below every whole-row scroll (the glide's fractional part, or the
//! elastic-overscroll displacement at a history end). At present time this module
//! shifts the terminal-content pixel band by it: up for a positive residual (the
//! incoming row appears at the bottom), down for a negative one (the bounce exposes a
//! strip at the top). Glyph rasters are untouched; the translate is an integer memmove
//! of rendered pixels, so text stays raster-exact while it moves.
//!
//! # Chrome stays pinned
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
//! (`matches_out_of_place`, against a reference that writes only the band) —
//! the M1b chrome-invariance theorem.
//!
//! # The exposed strip
//!
//! 1. **Identity at `frac == 0`.** A zero residual moves nothing: the present
//!    buffer is byte-identical to the untranslated frame — so a frame at
//!    `scroll_frac_px == 0` equals the same viewport reached by a whole-row jump
//!    (`matches_out_of_place` at `frac == 0`). This is the raster-invariance
//!    PROVE bullet.
//! 2. **Chrome invariance.** For ANY `frac`, every pixel with a row index outside
//!    `[y0, y1)` is byte-identical to the input; only the grid band is rewritten
//!    (`matches_out_of_place`): a row shifts IFF it is in the grid band.
//! 3. **Band derivation.** [`grid_band_px`] reproduces the renderer's own row→px
//!    mapping (`pad + row*cell_h`) and clamps to the framebuffer, so the band is
//!    exactly the terminal-content rows' pixels (`band_matches_row_mapping`).
//!
//! Known limits: the incoming row is rastered in isolation, so a neighbour's glyph
//! overshoot across the seam, a selection reaching the row and an inline image on it
//! appear only when the row lands; a wallpaper frame keeps the placeholder (its
//! backdrop is frame-fixed).

/// The terminal-content pixel band `[y0, y1)` for grid rows `[grid_top_row,
/// grid_bot_row)`, row `r` starting at `pad + r * cell_h` (the renderer passes
/// `grid_top()` as `pad`), clamped to the framebuffer height `h`. Empty (`y0 >= y1`)
/// when there is no grid to translate: `grid_bot_row <= grid_top_row` (including the
/// no-partition default `grid_bot_row == 0`) or a band below the frame.
#[must_use]
#[inline]
pub const fn grid_band_px(
    pad: usize,
    cell_h: usize,
    grid_top_row: usize,
    grid_bot_row: usize,
    h: usize,
) -> (usize, usize) {
    if grid_bot_row <= grid_top_row {
        return (0, 0);
    }
    let raw_y0 = pad + grid_top_row.saturating_mul(cell_h);
    let raw_y1 = pad + grid_bot_row.saturating_mul(cell_h);
    let y0 = if raw_y0 < h { raw_y0 } else { h };
    let y1 = if raw_y1 < h { raw_y1 } else { h };
    (y0, y1)
}

/// Shift the rows of the grid band `[y0, y1)` of `buf` (`w`-wide, row-major) by
/// `frac_px` device pixels in place; rows outside the band are never touched.
///
/// * Positive shifts up (the glide residual, or the bounce past the live bottom): row
///   `dy` takes `dy + frac` while that is in the band; the bottom `frac` rows are the
///   exposed strip.
/// * Negative shifts down (the bounce past the history top): row `dy` takes
///   `dy - |frac|`; the top `|frac|` rows are exposed.
///
/// The exposed strip keeps the band's own pixels as a placeholder until
/// [`paint_incoming_strip`] covers it (up-glide only); `|frac| >= band_h` exposes the
/// whole band. Each direction is one `copy_within` (memmove), so overlap is safe
/// (`matches_out_of_place`). `frac_px == 0` is a no-op.
#[inline]
pub fn translate_grid_band_in_place(buf: &mut [u32], w: usize, y0: usize, y1: usize, frac_px: i64) {
    if w == 0 || y0 >= y1 || frac_px == 0 {
        return;
    }
    let band_h = y1 - y0;
    let mag = frac_px.unsigned_abs() as usize;
    // The number of destination rows that pull from a still-in-band source; the
    // remaining `min(mag, band_h)` rows at the exposed edge are the placeholder.
    let moved = band_h.saturating_sub(mag);
    if moved == 0 {
        return;
    }
    if frac_px > 0 {
        // Shift up: rows `[y0, y0 + moved)` take `[y0 + mag, y1)`.
        buf.copy_within((y0 + mag) * w..y1 * w, y0 * w);
    } else {
        // Shift down: rows `[y0 + mag, y1)` take `[y0, y0 + moved)`.
        buf.copy_within(y0 * w..(y0 + moved) * w, (y0 + mag) * w);
    }
}

/// Whether a frame owes the incoming-row strip, the one predicate both backends use:
///
/// * a positive residual (the top bounce exposes rubber-band gap, not a row);
/// * a present apron, or the live bottom (`display_offset == 0`), where the strip is
///   painted blank; an absent apron above the bottom (a reordered BiDi row) keeps the
///   placeholder;
/// * a band reaching the frame's last row (`grid_bot_row == rows`): under bottom chrome
///   inside the frame (a find bar) the row below the band is not the engine's apron;
/// * no wallpaper (its frame-fixed backdrop would be wrong on a row rastered alone).
#[must_use]
#[inline]
pub const fn incoming_row_applies(input: &aterm_core::render::RenderInput) -> bool {
    input.scroll_frac_px > 0
        && (input.apron_row.present || input.display_offset == 0)
        && input.grid_bot_row == input.rows
        && input.wallpaper.is_none()
}

/// The incoming-row strip `(dst_y, rows)` for band `[y0, y1)` shifted by `frac_px`: the
/// band's bottom `frac` rows. `None` for a non-positive residual, an empty band, or
/// `frac >= band_h` (nothing moved, so no seam; the GPU's `band_shift_ops` agrees).
#[must_use]
#[inline]
pub const fn incoming_strip(y0: usize, y1: usize, frac_px: i64) -> Option<(usize, usize)> {
    if frac_px <= 0 || y0 >= y1 {
        return None;
    }
    if frac_px as i128 > usize::MAX as i128 {
        return None;
    }
    let n = frac_px as usize;
    if n >= y1 - y0 {
        return None;
    }
    Some((y1 - n, n))
}

/// After [`translate_grid_band_in_place`] shifted `[y0, y1)` of `buf` up by `frac_px`,
/// copy row `i` of `apron` (the incoming row's `w`-wide raster, top row first) onto
/// framebuffer row `y1 - frac + i`, for as many rows as [`incoming_strip`] owes and
/// `apron` holds; a short apron fills the strip's top rows and leaves the rest as the
/// placeholder. Writes only inside the strip. Returns the rows painted (`0` for a
/// bounce, frac 0, an empty band or an empty apron).
#[inline]
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
    //! M1b PROVE bullets, always-on layer (real pixel buffers): the
    //! grid-vs-chrome partition and, by lattice/exhaustive tests, the band
    //! derivation (`pad + r*cell_h`).

    use super::*;

    /// A framebuffer whose pixels encode (row, col), so any stray move shows.
    fn checkerboard(w: usize, h: usize) -> Vec<u32> {
        (0..w * h)
            .map(|i| {
                let (y, x) = (i / w, i % w);
                ((y as u32) << 16) | (x as u32) | 0x0100_0000
            })
            .collect()
    }

    /// Out-of-place reference translate that reads only the pristine source; the
    /// in-place routine must match it for both signs.
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

    /// The in-place move equals the pristine-source reference: no row is read after it
    /// is overwritten.
    #[test]
    fn matches_out_of_place() {
        let (w, h) = (6usize, 24usize);
        let base = checkerboard(w, h);
        for y0 in 0..h {
            for y1 in y0..=h {
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

    /// A positive frac exposes the bottom strip and pulls the rest up; a negative one
    /// exposes the top strip and pushes the rest down, each by exactly `|frac|` rows.
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

    /// An up-glide owes `[y1 - frac, y1)`; a bounce, a zero residual, an empty band and
    /// a fully exposed band owe nothing.
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
        for frac in [i64::from(u32::MAX) + 1, i64::from(u32::MAX) + 2, i64::MAX] {
            assert_eq!(incoming_strip(0, 10, frac), None, "oversized residual");
        }
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

    /// The painter lays the apron's top `frac` rows on exactly the exposed strip and
    /// leaves every other row as the translate left it; a short apron paints only what
    /// it has; a bounce or frac 0 paints nothing.
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
        // A short apron is top-aligned: its rows meet the band's bottom row at
        // `y1 - frac`, and the rest of the strip keeps the placeholder.
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

    /// The band derivation matches `pad + r*cell_h` clamped to `h` over a lattice,
    /// including the empty-band cases.
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
