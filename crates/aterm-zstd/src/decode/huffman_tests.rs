// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/huff0/huff0_decoder.rs, modified by the aterm
// project in 2026. See ../../LICENSE-MIT for the upstream MIT license.

use super::HuffmanTable;

#[test]
fn direct_weights_build_the_canonical_table() {
    let mut t = HuffmanTable::new();
    let used = t.build_decoder(&[129, 0x11]).unwrap();
    assert_eq!(used, 2);
    assert_eq!(t.max_num_bits, 2);
    let syms: Vec<(u8, u8)> = t.decode.iter().map(|e| (e.symbol, e.num_bits)).collect();
    assert_eq!(syms, [(0, 2), (1, 2), (2, 1), (2, 1)]);
}

#[test]
fn weights_that_complete_no_tree_are_refused() {
    let mut t = HuffmanTable::new();
    assert!(t.build_decoder(&[129, 0x22]).is_err());
    assert!(t.build_decoder(&[128]).is_err());
    assert!(t.build_decoder(&[]).is_err());
    assert!(t.build_decoder(&[129, 0x00]).is_err());
    assert!(t.build_decoder(&[128, 0xC0]).is_err());
    // FSE weight table (probs = [-1, 31], accuracy_log = 5) followed by a 0-bit stream (0x01)
    // lacks the 10 bits needed to initialize both FSE states and must be refused.
    assert!(t.build_decoder(&[3, 0x00, 0x7E, 0x01]).is_err());
}
