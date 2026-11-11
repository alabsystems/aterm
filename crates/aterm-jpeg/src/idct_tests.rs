// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::idct_block;

#[test]
fn a_dc_only_block_is_flat() {
    let mut coefficients = [0i32; 64];
    coefficients[0] = 80;
    let mut out = [0u8; 64];
    idct_block(&coefficients, &mut out, 8);
    assert!(out.iter().all(|&sample| sample == 138), "{out:?}");
}

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
