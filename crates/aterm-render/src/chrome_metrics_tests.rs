// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Cap-height centring: `<= 1px` imbalance on the integer-px lattice; on the subunit
/// lattice `<= 1` subunit before device rounding and `<= Q + 1` after. Sweeps both
/// parities of `row_h + cap_h` and every baseline residue mod `Q`.
#[test]
fn cap_centering_lattice() {
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

/// The embedded DejaVu's OS/2 table is version 1 (no `sCapHeight`).
#[cfg(feature = "embedded-font")]
#[test]
fn cap_ratio_from_os2_and_measured_h_agree() {
    let bytes = crate::embedded_font();
    let font = crate::font::Font::from_bytes(bytes, crate::font::FontSettings::default()).unwrap();
    let table = cap_height_ratio(bytes, 0, &font);
    assert!((0.4..1.0).contains(&table), "OS/2 cap ratio sane: {table}");
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
