// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sub-row scroll translation and incoming-strip painting for rendered grid bands.

/// Returns the terminal-content pixel band `[y0, y1)` for grid rows `[grid_top_row, grid_bot_row)`, clamped to `h`.
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
    let raw_y0 = pad.saturating_add(grid_top_row.saturating_mul(cell_h));
    let raw_y1 = pad.saturating_add(grid_bot_row.saturating_mul(cell_h));
    let y0 = if raw_y0 < h { raw_y0 } else { h };
    let y1 = if raw_y1 < h { raw_y1 } else { h };
    (y0, y1)
}

/// Shifts the rows of the grid band `[y0, y1)` of `buf` (`w`-wide, row-major) by `frac_px` device pixels in place; a band that runs past `buf`, or a shift wider than `usize`, moves nothing.
#[inline]
pub fn translate_grid_band_in_place(buf: &mut [u32], w: usize, y0: usize, y1: usize, frac_px: i64) {
    if w == 0 || y0 >= y1 || frac_px == 0 {
        return;
    }
    let Some(end) = y1.checked_mul(w).filter(|&end| end <= buf.len()) else {
        return;
    };
    let band_h = y1 - y0;
    let Ok(mag) = usize::try_from(frac_px.unsigned_abs()) else {
        return;
    };
    let moved = band_h.saturating_sub(mag);
    if moved == 0 {
        return;
    }
    if frac_px > 0 {
        // Shift up: rows `[y0, y0 + moved)` take `[y0 + mag, y1)`.
        buf.copy_within((y0 + mag) * w..end, y0 * w);
    } else {
        // Shift down: rows `[y0 + mag, y1)` take `[y0, y0 + moved)`.
        buf.copy_within(y0 * w..(y0 + moved) * w, (y0 + mag) * w);
    }
}

/// Returns whether a frame should paint the incoming-row strip at the bottom of the grid band.
#[must_use]
#[inline]
pub const fn incoming_row_applies(input: &aterm_core::render::RenderInput) -> bool {
    input.scroll_frac_px > 0
        && (input.apron_row.present || input.display_offset == 0)
        && input.grid_bot_row == input.rows
        && input.wallpaper.is_none()
}

/// Returns the incoming-row strip `(dst_y, rows)` for band `[y0, y1)` shifted up by `frac_px`.
///
/// Its subtractions are `wrapping_sub` behind the guards that make them exact: Trust refuses an
/// inlined body with an overflow assert after a branch, and aterm-gpu's `apron_strip_plan` lost a
/// proof to that (`docs/measured/trust-prover-2026-10-21.md`).
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
    // `y0 < y1`, so the band height is exact; `n < y1 - y0 <= y1`, so is the strip's top.
    if n >= y1.wrapping_sub(y0) {
        return None;
    }
    Some((y1.wrapping_sub(n), n))
}

/// Copies the incoming-row raster `apron` into the bottom strip of `[y0, y1)` exposed by `frac_px`, returning rows painted.
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
#[path = "scroll_translate_tests.rs"]
mod tests;
