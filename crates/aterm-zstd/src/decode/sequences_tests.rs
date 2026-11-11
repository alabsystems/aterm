// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/blocks/sequence_section.rs,
// src/decoding/sequence_section_decoder.rs and
// src/decoding/sequence_execution.rs, modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

use super::{INITIAL_OFFSETS, parse_header, resolve_offset};

#[test]
fn sequence_counts_at_each_header_width() {
    assert_eq!(parse_header(&[0]).unwrap().0.num_sequences, 0);
    let (h, len) = parse_header(&[127, 0]).unwrap();
    assert_eq!((h.num_sequences, len), (127, 2));
    let (h, len) = parse_header(&[0x81, 0x02, 0]).unwrap();
    assert_eq!((h.num_sequences, len), (0x102, 3));
    let (h, len) = parse_header(&[128, 0]).unwrap();
    assert_eq!((h.num_sequences, len), (0, 2));
    let (h, len) = parse_header(&[255, 0x34, 0x12, 0]).unwrap();
    assert_eq!((h.num_sequences, len), (0x1234 + 0x7F00, 4));
    assert!(parse_header(&[]).is_err());
    assert!(parse_header(&[5]).is_err());
    assert!(parse_header(&[255, 1]).is_err());
}

/// RFC 8878 §3.1.2.5's rules, step by step from the initial offsets.
#[test]
fn repeat_offsets() {
    let mut h = INITIAL_OFFSETS;
    assert_eq!(resolve_offset(1, 5, &mut h), 1);
    assert_eq!(h, [1, 4, 8]);
    assert_eq!(resolve_offset(2, 5, &mut h), 4);
    assert_eq!(h, [4, 1, 8]);
    assert_eq!(resolve_offset(3, 5, &mut h), 8);
    assert_eq!(h, [8, 4, 1]);
    assert_eq!(resolve_offset(103, 5, &mut h), 100);
    assert_eq!(h, [100, 8, 4]);
    assert_eq!(resolve_offset(1, 0, &mut h), 8);
    assert_eq!(h, [8, 100, 4]);
    assert_eq!(resolve_offset(2, 0, &mut h), 4);
    assert_eq!(h, [4, 8, 100]);
    assert_eq!(resolve_offset(3, 0, &mut h), 3);
    assert_eq!(h, [3, 4, 8]);
    let mut one = [1, 2, 3];
    assert_eq!(resolve_offset(3, 0, &mut one), 0);
}
