// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixed-point typography math for GUI chrome ([`baseline_in_row_q`], [`px_to_q`],
//! [`q_round_to_px`]).

/// One device pixel in 26.6 fixed point (Q = 64 subunits per px).
pub const Q: i64 = 64;

/// Quantize px to 26.6 fixed point (nearest subunit, ties away from zero).
#[must_use]
#[inline]
pub fn px_to_q(px: f32) -> i64 {
    let q = (f64::from(px) * Q as f64).round();
    if q.is_finite() { q as i64 } else { 0 }
}

/// 26.6 fixed point back to fractional px (exact: 2^-6 is a binary fraction).
#[must_use]
#[inline]
pub const fn q_to_px(q: i64) -> f32 {
    (q as f64 / Q as f64) as f32
}

/// Round a 26.6 fixed-point coordinate to the nearest integer device pixel (ties round up).
#[must_use]
#[inline]
pub const fn q_round_to_px(q: i64) -> i64 {
    (q + Q / 2).div_euclid(Q)
}

/// Cap-height-centred baseline in 26.6 fixed point for a run in `row_top..row_top + row_h`.
#[must_use]
#[inline]
pub const fn baseline_in_row_q(row_top_q: i64, row_h_q: i64, cap_h_q: i64) -> i64 {
    row_top_q + (row_h_q + cap_h_q) / 2
}

/// Cap height of face `index` in `bytes` as a ratio of the em.
#[must_use]
pub fn cap_height_ratio(bytes: &[u8], index: u32, font: &crate::font::Font) -> f32 {
    if let Ok(face) = aterm_font::Face::parse(bytes, index)
        && let Some(cap) = face.capital_height()
        && cap > 0
    {
        return f32::from(cap) / f32::from(face.units_per_em());
    }
    const REF_PX: f32 = 100.0;
    let m = font.metrics('H', REF_PX);
    if m.height > 0 {
        return m.height as f32 / REF_PX;
    }
    0.7
}

#[cfg(test)]
#[path = "chrome_metrics_tests.rs"]
mod tests;
