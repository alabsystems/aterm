// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/bit_io/bit_reader.rs and
// src/bit_io/bit_reader_reverse.rs, modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

use super::{BitReader, BitReaderReversed};

#[test]
fn reversed_reads_from_the_last_byte_down_then_zeros() {
    let data = [0b1010_1010, 0b0101_0101];
    let mut br = BitReaderReversed::new(&data);
    assert_eq!(br.get_bits(1), 0);
    assert_eq!(br.get_bits(1), 1);
    assert_eq!(br.get_bits(1), 0);
    assert_eq!(br.get_bits(4), 0b1010);
    assert_eq!(br.get_bits(4), 0b1101);
    assert_eq!(br.get_bits(4), 0b0101);
    assert_eq!(br.get_bits(4), 0b0000);
    assert_eq!(br.get_bits(4), 0b0000);
    assert_eq!(br.bits_remaining(), -7);
}

#[test]
fn reversed_triple_matches_three_single_reads() {
    let data: Vec<u8> = (0u8..40).map(|i| i.wrapping_mul(37) ^ 0x5A).collect();
    for (n1, n2, n3) in [(0, 0, 0), (5, 9, 16), (31, 16, 16), (31, 0, 9), (1, 1, 1)] {
        let mut a = BitReaderReversed::new(&data);
        let mut b = BitReaderReversed::new(&data);
        for _ in 0..4 {
            let want = (a.get_bits(n1), a.get_bits(n2), a.get_bits(n3));
            assert_eq!(b.get_bits_triple(n1, n2, n3), want, "{n1} {n2} {n3}");
        }
        assert_eq!(a.bits_remaining(), b.bits_remaining());
    }
}

#[test]
fn padding_needs_a_marker_in_the_last_byte() {
    let mut br = BitReaderReversed::new(&[0xFF, 0x01]);
    br.skip_padding().unwrap();
    assert_eq!(br.bits_remaining(), 8);
    let mut br = BitReaderReversed::new(&[0xFF, 0x00]);
    assert!(br.skip_padding().is_err());
    let mut br = BitReaderReversed::new(&[]);
    assert!(br.skip_padding().is_err());
}

#[test]
fn forward_reads_low_bits_first_across_bytes() {
    let data = [0b1011_0110, 0b1100_0011, 0xFF];
    let mut br = BitReader::new(&data);
    assert_eq!(br.get_bits(3).unwrap(), 0b110);
    assert_eq!(br.get_bits(7).unwrap(), 0b111_0110);
    assert_eq!(br.bits_read(), 10);
    assert_eq!(br.get_bits(14).unwrap(), 0b11_1111_1111_0000);
    assert!(br.get_bits(1).is_err());
    assert!(br.get_bits(usize::MAX).is_err());
    assert_eq!(BitReader::new(&[]).get_bits(0).unwrap(), 0);
}
