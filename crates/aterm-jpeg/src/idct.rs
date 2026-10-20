// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The 8x8 inverse DCT (T.81 A.3.3), separable, in single precision: the
//! definition itself, so its error is the float rounding alone (within one
//! level of the exact transform, as the integer IDCTs of other decoders
//! are). A row or column whose AC terms are all zero is a constant and costs
//! one multiply.

use std::sync::OnceLock;

/// `C(u)/2 * cos((2x + 1) u pi / 16)`, indexed `[u][x]`.
fn basis() -> &'static [[f32; 8]; 8] {
    static BASIS: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    BASIS.get_or_init(|| {
        let mut table = [[0.0f32; 8]; 8];
        for (u, row) in table.iter_mut().enumerate() {
            let scale = if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            } * 0.5;
            for (x, cell) in row.iter_mut().enumerate() {
                let angle = ((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0;
                *cell = (scale * angle.cos()) as f32;
            }
        }
        table
    })
}

/// Transform one block of dequantized coefficients (natural order) into
/// samples, level-shifted by 128 and clamped to 0..=255, written row by row
/// at `out[row * stride + column]`.
pub(crate) fn idct_block(coefficients: &[i32; 64], out: &mut [u8], stride: usize) {
    let basis = basis();
    // Columns first: for each column u of coefficients, the eight vertical
    // samples (by y).
    let mut columns = [0.0f32; 64];
    for u in 0..8 {
        let column = |v: usize| coefficients[v * 8 + u];
        if (1..8).all(|v| column(v) == 0) {
            let value = column(0) as f32 * basis[0][0];
            for y in 0..8 {
                columns[y * 8 + u] = value;
            }
            continue;
        }
        for y in 0..8 {
            let mut sum = 0.0f32;
            for (v, terms) in basis.iter().enumerate() {
                let c = column(v);
                if c != 0 {
                    sum += c as f32 * terms[y];
                }
            }
            columns[y * 8 + u] = sum;
        }
    }
    // Then each row across.
    for y in 0..8 {
        let row = &columns[y * 8..y * 8 + 8];
        let Some(target) = out.get_mut(y * stride..y * stride + 8) else {
            return;
        };
        if row[1..].iter().all(|&value| value == 0.0) {
            let sample = clamp_sample(row[0] * basis[0][0]);
            target.fill(sample);
            continue;
        }
        for (x, slot) in target.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            for (u, &value) in row.iter().enumerate() {
                sum += value * basis[u][x];
            }
            *slot = clamp_sample(sum);
        }
    }
}

fn clamp_sample(value: f32) -> u8 {
    (value + 128.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::idct_block;

    /// A lone DC coefficient is a flat block at DC / 8 above mid-grey.
    #[test]
    fn a_dc_only_block_is_flat() {
        let mut coefficients = [0i32; 64];
        coefficients[0] = 80;
        let mut out = [0u8; 64];
        idct_block(&coefficients, &mut out, 8);
        assert!(out.iter().all(|&sample| sample == 138), "{out:?}");
    }

    /// The first horizontal AC term is a left-to-right ramp, symmetric about
    /// the middle, and the transform clamps to the sample range.
    #[test]
    fn a_horizontal_term_ramps_across_the_row() {
        let mut coefficients = [0i32; 64];
        coefficients[1] = 200;
        let mut out = [0u8; 64];
        idct_block(&coefficients, &mut out, 8);
        assert!(out[0] > out[7], "{:?}", &out[..8]);
        assert_eq!(i32::from(out[0]) + i32::from(out[7]), 256);
        coefficients[0] = 4000;
        idct_block(&coefficients, &mut out, 8);
        assert!(out.iter().all(|&sample| sample == 255));
    }
}
