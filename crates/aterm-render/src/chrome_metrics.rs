// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixed-point typography math for the GUI CHROME (the tray/overlay text the
//! `aterm-gui` `tray_raster` draws: Settings, About, the command palette, the
//! system-performance card).
//!
//! Two laws live here as PURE integer functions so they can be checked
//! exhaustively (the lattice tests below cover every residue the laws depend
//! on) and then CONSUMED verbatim by the shipping rasterizer — the check is
//! about the exact arithmetic that places pixels:
//!
//! 1. **Cap-height centering (leading-trim)** — a text run is vertically
//!    centred in its row by placing the BASELINE at
//!    `row_center + cap_height/2`, so the gap above the cap box equals the gap
//!    below the baseline. The historical chrome placed the baseline at
//!    `y + font_px` (fontdue's em box bottom), which sits ~0.135 em low.
//!    [`baseline_in_row_q`] encodes the rule in 26.6 fixed point;
//!    `|top_gap − bottom_gap| <= 1` subunit before device rounding, and
//!    `<= 1px + 1` subunit after (`cap_centering_lattice`).
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
//! chrome overhaul (a pure boolean gate) IS `ty`-modelled: see
//! `aterm_spec::derive::chrome_face_gate_model` and its Tier-1 binding in
//! `aterm-gui`'s `tray_raster`.

/// One device pixel in 26.6 fixed point (Q = 64 subunits per px).
pub const Q: i64 = 64;

/// Quantize a fractional px length/coordinate to 26.6 fixed point (nearest
/// subunit, ties away from zero — `f32::round` semantics). Non-finite input
/// maps to 0 (a degenerate prim, drawn nowhere).
#[must_use]
pub fn px_to_q(px: f32) -> i64 {
    let q = (f64::from(px) * Q as f64).round();
    if q.is_finite() { q as i64 } else { 0 }
}

/// 26.6 fixed point back to fractional px (exact: 2^-6 is a binary fraction).
#[must_use]
pub fn q_to_px(q: i64) -> f32 {
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
/// covers every residue mod `Q`, + the `pen_is_drift_free` lattice test).
#[must_use]
pub fn q_round_to_px(q: i64) -> i64 {
    (q + Q / 2).div_euclid(Q)
}

/// The cap-height-centred BASELINE for a text run in the row box
/// `[row_top, row_top + row_h)`, all in 26.6 fixed point:
/// `baseline = row_top + (row_h + cap_h) / 2` — i.e. `row_center + cap_h/2`,
/// the leading-trim rule. `row_h = 0` degenerates to "centre the cap box on
/// `row_top`" (used for anchor-centred labels like ring values).
///
/// # Invariant (checked)
/// For `0 <= cap_h <= row_h`: with `top_gap = (baseline − cap_h) − row_top`
/// (space above the cap box) and `bottom_gap = (row_top + row_h) − baseline`
/// (space below the baseline), `|top_gap − bottom_gap| <= 1` subunit (1/64 px)
/// — and `<= 1px` on the integer-px lattice — for ALL inputs: the difference
/// depends only on the parity of `row_h + cap_h`, and `cap_centering_lattice`
/// sweeps both.
#[must_use]
pub fn baseline_in_row_q(row_top_q: i64, row_h_q: i64, cap_h_q: i64) -> i64 {
    row_top_q + (row_h_q + cap_h_q) / 2
}

/// Cap height of face `index` inside `bytes`, as a RATIO of the em size:
/// OS/2 `sCapHeight` (via aterm-font) over `units_per_em` when the table
/// carries it, else the measured height of an actual `'H'` (rasterized at a
/// reference size by the caller-supplied face). Falls back to `0.7`
/// (the Latin norm) only when both probes fail, so the centring rule is always
/// defined.
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

    /// L0 exhaustive lattice proof of the cap-height centering law.
    ///
    /// Integer-px lattice (`q = 64k` — the raster's observable grid): for every
    /// `(row_h, cap_h)` with `cap_h <= row_h`, `|top_gap − bottom_gap| <= 1px`.
    /// Sub-px lattice (every subunit): `|top_gap − bottom_gap| <= 1` subunit
    /// before device rounding, and `<= Q + 1` subunits (1px + 1/64) after the
    /// baseline snaps to the device grid, which moves both gaps oppositely by
    /// one rounding delta. Deliberately sweeps odd AND even sizes so the `/2`
    /// truncation is exercised on both parities, and every residue of the
    /// baseline mod `Q`, which is all the rounding depends on.
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

    /// L0 drift-freedom of the fixed-point pen: over a long pseudo-random run of
    /// fractional advances, EVERY prefix placement is within 0.5px of the exact
    /// fixed-point pen (which is an exact integer sum — checked against i128),
    /// and the error does not grow with the prefix length. Negative control:
    /// the historical float pen + `as i32` truncation exceeds 0.5px error.
    #[test]
    fn pen_is_drift_free() {
        // Deterministic LCG advances in [4.0, 12.0) px with fractional parts.
        let mut seed = 0x2545_F491u64;
        let mut lcg = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f32 / (u32::MAX >> 1) as f32) * 8.0 + 4.0
        };
        let advances: Vec<f32> = (0..4096).map(|_| lcg()).collect();

        let mut pen_q: i64 = 0;
        let mut exact: i128 = 0;
        let mut float_pen: f32 = 0.0;
        let mut trunc_err_seen: f32 = 0.0;
        for a in &advances {
            let aq = px_to_q(*a);
            pen_q += aq;
            exact += i128::from(aq);
            assert_eq!(
                i128::from(pen_q),
                exact,
                "fixed-point accumulation is exact"
            );
            let placed = q_round_to_px(pen_q);
            let err_subunits = (placed * Q - pen_q).abs();
            assert!(
                err_subunits <= Q / 2,
                "placement error {err_subunits} subunits exceeds 0.5px"
            );
            // The old path: f32 running pen, truncated per glyph.
            float_pen += *a;
            let old_placed = float_pen as i32; // the historical `as i32`
            trunc_err_seen = trunc_err_seen.max(float_pen - old_placed as f32);
        }
        // Negative control: truncation error genuinely exceeds the 0.5px bound
        // the rounded fixed-point pen guarantees (the pre-fix leftward bias).
        assert!(
            trunc_err_seen > 0.5,
            "the pre-fix truncating pen must show > 0.5px error (saw {trunc_err_seen})"
        );
    }

    /// `q_round_to_px` half-pixel bound over a dense signed lattice: the error
    /// depends only on `q mod Q`, and the lattice holds every residue many
    /// times over, on both signs.
    #[test]
    fn rounding_error_bounded_by_half_px() {
        for q in -100_000..=100_000i64 {
            assert!((q_round_to_px(q) * Q - q).abs() <= Q / 2, "q={q}");
        }
    }

    /// Cap-ratio extraction: the embedded DejaVu Sans Mono carries OS/2
    /// sCapHeight (729/2048 em ≈ 0.7115); the measured-'H' fallback lands within
    /// a few percent of it, and both stay in the sane (0.4, 1.0) band.
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
