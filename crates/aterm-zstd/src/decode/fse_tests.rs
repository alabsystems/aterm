// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/fse/fse_decoder.rs, modified by the aterm
// project in 2026. See ../../LICENSE-MIT for the upstream MIT license.

use super::FseTable;
use crate::decode::sequences::{LL_DEFAULT_ACC_LOG, LL_DEFAULT_DISTRIBUTION};

#[test]
fn predefined_literal_length_table() {
    let mut table = FseTable::new(35);
    table
        .build_from_probabilities(LL_DEFAULT_ACC_LOG, &LL_DEFAULT_DISTRIBUTION)
        .unwrap();
    assert_eq!(table.decode.len(), 64);
    let row = |i: usize| {
        let e = table.decode[i];
        (e.symbol, e.num_bits, e.base_line)
    };
    assert_eq!(row(0), (0, 4, 0));
    assert_eq!(row(1), (0, 4, 16));
    assert_eq!(row(19), (27, 6, 0));
    assert_eq!(row(39), (25, 4, 16));
    assert_eq!(row(59), (24, 5, 32));
    assert_eq!(row(60), (35, 6, 0));
    assert_eq!(row(63), (32, 6, 0));
}

#[test]
fn distributions_that_do_not_fill_the_table_are_refused() {
    let mut table = FseTable::new(35);
    assert!(table.build_from_probabilities(5, &[16, 15]).is_err());
    assert!(!table.is_built());
    assert!(table.build_from_probabilities(5, &[16, 17]).is_err());
    assert!(table.build_from_probabilities(5, &[-2, 16, 18]).is_err());
    assert!(table.build_from_probabilities(5, &[-1; 32]).is_ok());
    assert!(table.build_from_probabilities(5, &[1; 37]).is_err());
}

#[test]
fn byte_aligned_short_last_probability_decodes_without_overread() {
    let buf = 0x4F41_8810u32.to_le_bytes();
    let mut table = FseTable::new(35);
    let consumed = table.build_decoder(&buf, 9).unwrap();
    assert_eq!(consumed, 4);
    assert_eq!(table.probs, [0, 16, 0, 12, 2, 1, -1]);
}
