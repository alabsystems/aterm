// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/blocks/literals_section.rs and
// src/decoding/literals_section_decoder.rs, modified by the aterm project in
// 2026. See ../../LICENSE-MIT for the upstream MIT license.

use super::{Kind, parse_header};

#[test]
fn header_sizes_at_each_format() {
    let h = parse_header(&[31 << 3]).unwrap();
    assert_eq!((h.kind, h.len, h.regenerated_size), (Kind::Raw, 1, 31));
    let h = parse_header(&[0b1100_0101, 0xAB]).unwrap();
    assert_eq!(
        (h.kind, h.len, h.regenerated_size, h.compressed_size),
        (Kind::Rle, 2, 0xABC, 1)
    );
    let h = parse_header(&[0x0C | 0x10, 0x23, 0x01]).unwrap();
    assert_eq!((h.len, h.regenerated_size), (3, 0x1231));
    let h = parse_header(&[0xF2, 0xFF, 0xFF]).unwrap();
    assert_eq!(
        (
            h.kind,
            h.len,
            h.regenerated_size,
            h.compressed_size,
            h.four_streams
        ),
        (Kind::Compressed, 3, 1023, 1023, false)
    );
    let h = parse_header(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).unwrap();
    assert_eq!(
        (
            h.kind,
            h.len,
            h.regenerated_size,
            h.compressed_size,
            h.four_streams
        ),
        (Kind::Treeless, 5, (1 << 18) - 1, (1 << 18) - 1, true)
    );
    let h = parse_header(&[0xCA, 0xAB, 0xD2, 0x48]).unwrap();
    assert_eq!(
        (
            h.kind,
            h.len,
            h.regenerated_size,
            h.compressed_size,
            h.four_streams
        ),
        (Kind::Compressed, 4, 0x2ABC, 0x1234, true)
    );
    let h = parse_header(&[0xBD, 0xDC, 0xFE]).unwrap();
    assert_eq!(
        (h.kind, h.len, h.regenerated_size, h.compressed_size),
        (Kind::Rle, 3, 0xF_EDCB, 1)
    );
    assert!(parse_header(&[]).is_err());
    assert!(parse_header(&[0b0100]).is_err());
    assert!(parse_header(&[0x0E, 0, 0]).is_err());
    assert!(parse_header(&[0xCA, 0xAB, 0xD2]).is_err());
    assert!(parse_header(&[0xBD, 0xDC]).is_err());
    assert!(parse_header(&[0x02, 0]).is_err());
}
