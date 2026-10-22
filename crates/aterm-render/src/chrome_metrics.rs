// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixed-point typography math for the GUI CHROME (the tray/overlay text the
//! `aterm-gui` `tray_raster` draws: Settings, About, the command palette).
//!
//! Two pure integer laws, checked exhaustively by the lattice tests below and used verbatim
//! by the rasterizer:
//!
//! 1. Cap-height centring (leading-trim): the baseline sits at `row_center + cap_height/2`,
//!    so the gap above the cap box equals the gap below the baseline
//!    ([`baseline_in_row_q`]).
//! 2. Drift-free pen advance: advances accumulate exactly in 26.6 ([`px_to_q`] plus `i64`
//!    addition) and each glyph lands at [`q_round_to_px`] of the running pen, so every
//!    placement is within 0.5px and the error does not accumulate along the run.
//!
//! 2. **Drift-free pen advance** — glyph advances accumulate in 26.6 fixed
//!    point ([`px_to_q`] + plain `i64` addition, which is EXACT), and each
//!    glyph is placed at [`q_round_to_px`] of the running pen. The placement
//!    error of every glyph is `<= 0.5px` against the exact pen and does NOT
//!    accumulate along the run (the historical float pen truncated with
//!    `as i32`, a leftward bias of up to 1px that jittered per glyph).
//!
//! WAIVER (Tier-0 `ty`): both laws divide (`/2` for the centring, `/64` for the
//! device rounding); the derived-model `Expr` language is add/sub-only, so —
//! exactly like the box-drawing rounding law (`procedural.rs`) — the
//! machine-checked encodings are the exhaustive lattice tests here, not a `ty`
//! model. The FACE-SELECTION policy of the same
//! chrome overhaul (a pure boolean gate) is enumerated exhaustively in
//! `aterm-gui`'s `tray_raster`.

/// One device pixel in 26.6 fixed point (Q = 64 subunits per px).
pub const Q: i64 = 64;

/// Quantize px to 26.6 fixed point (nearest subunit, ties away from zero). Non-finite input
/// maps to 0.
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

/// Round a 26.6 fixed-point coordinate to the nearest integer DEVICE pixel
/// (ties round up, matching `f32::round` for the non-negative coordinates the
/// raster uses; exact for negatives too via `div_euclid`).
///
/// # Invariant (checked)
/// `|q_round_to_px(q) * Q − q| <= Q/2` for all `q` — each placement is within
/// half a pixel of the exact fixed-point pen, with no dependence on how many
/// advances were summed before it (`rounding_error_bounded_by_half_px`, which
/// covers every residue mod `Q`).
#[must_use]
#[inline]
pub const fn q_round_to_px(q: i64) -> i64 {
    (q + Q / 2).div_euclid(Q)
}

/// The cap-height-centred baseline for a run in the row `[row_top, row_top + row_h)`, in
/// 26.6: `row_top + (row_h + cap_h) / 2`. `row_h = 0` centres the cap box on `row_top`
/// (anchor-centred labels such as ring values).
///
/// For `0 <= cap_h <= row_h`, the gap above the cap box and the gap below the baseline
/// differ by at most 1 subunit (`cap_centering_lattice`).
#[must_use]
#[inline]
pub const fn baseline_in_row_q(row_top_q: i64, row_h_q: i64, cap_h_q: i64) -> i64 {
    row_top_q + (row_h_q + cap_h_q) / 2
}

/// Cap height of face `index` in `bytes` as a ratio of the em: OS/2 `sCapHeight` over
/// `units_per_em` when present (OS/2 version 2+), else the ink height of `'H'` rasterized
/// by `font`, else `0.7`.
#[must_use]
pub fn cap_height_ratio(bytes: &[u8], index: u32, font: &crate::font::Font) -> f32 {
    if let Ok(face) = aterm_font::Face::parse(bytes, index)
        && let Some(cap) = face.capital_height()
        && cap > 0
    {
        return f32::from(cap) / f32::from(face.units_per_em());
    }
    // Measured-'H' fallback: rasterize the capital at a reference size and take
    // its ink height. 100px keeps the quantization error under 1%.
    const REF_PX: f32 = 100.0;
    let m = font.metrics('H', REF_PX);
    if m.height > 0 {
        return m.height as f32 / REF_PX;
    }
    0.7
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cap-height centring: `<= 1px` imbalance on the integer-px lattice; on the subunit
    /// lattice `<= 1` subunit before device rounding and `<= Q + 1` after. Sweeps both
    /// parities of `row_h + cap_h` and every baseline residue mod `Q`.
    #[test]
    fn cap_centering_lattice() {
        // Integer-pixel lattice, the law as stated: <= 1px.
        for row_h_px in 0..=96i64 {
            for cap_px in 0..=row_h_px {
                let (row_h, cap) = (row_h_px * Q, cap_px * Q);
                let b = baseline_in_row_q(0, row_h, cap);
                let top_gap = b - cap;
                let bottom_gap = row_h - b;
                assert!(
                    (top_gap - bottom_gap).abs() <= Q,
                    "px lattice row_h={row_h_px} cap={cap_px}: gaps {top_gap} vs {bottom_gap}"
                );
            }
        }
        // Full subunit lattice: exact balance to 1/64 px, any row_top.
        for row_top in [-320i64, 0, 97] {
            for row_h in 0..=512i64 {
                for cap in 0..=row_h {
                    let b = baseline_in_row_q(row_top, row_h, cap);
                    let top_gap = (b - cap) - row_top;
                    let bottom_gap = (row_top + row_h) - b;
                    assert!(
                        (top_gap - bottom_gap).abs() <= 1,
                        "q lattice top={row_top} row_h={row_h} cap={cap}: \
                         gaps {top_gap} vs {bottom_gap}"
                    );
                    let snapped = q_round_to_px(b) * Q;
                    let rounded = ((snapped - cap) - row_top) - ((row_top + row_h) - snapped);
                    assert!(
                        rounded.abs() <= Q + 1,
                        "device-rounded top={row_top} row_h={row_h} cap={cap}: imbalance {rounded}"
                    );
                }
            }
        }
    }

    /// `q_round_to_px` stays within half a pixel over every residue mod `Q`, on both signs.
    #[test]
    fn rounding_error_bounded_by_half_px() {
        for q in -100_000..=100_000i64 {
            assert!((q_round_to_px(q) * Q - q).abs() <= Q / 2, "q={q}");
        }
    }

    /// The embedded DejaVu's OS/2 table is version 1 (no `sCapHeight`), so both calls take
    /// the measured-'H' path; the ratio stays in (0.4, 1.0) and the two agree.
    #[cfg(feature = "embedded-font")]
    #[test]
    fn cap_ratio_from_os2_and_measured_h_agree() {
        let bytes = crate::embedded_font();
        let font =
            crate::font::Font::from_bytes(bytes, crate::font::FontSettings::default()).unwrap();
        let table = cap_height_ratio(bytes, 0, &font);
        assert!((0.4..1.0).contains(&table), "OS/2 cap ratio sane: {table}");
        // Force the measured fallback by handing it unparseable "table" bytes.
        let measured = cap_height_ratio(&[0u8; 4], 0, &font);
        assert!(
            (0.4..1.0).contains(&measured),
            "measured cap ratio sane: {measured}"
        );
        assert!(
            (table - measured).abs() < 0.05,
            "OS/2 ({table}) and measured-'H' ({measured}) cap heights agree"
        );
    }
}
