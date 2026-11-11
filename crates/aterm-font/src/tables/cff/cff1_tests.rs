// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cff/cff1.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use super::*;

#[test]
fn private_dict_size_overflow() {
    let data = &[
        0x00, 0x01, // count: 1
        0x01, // offset size: 1
        0x01, // index [0]: 1
        0x0C, // index [1]: 14
        0x1D, 0x7F, 0xFF, 0xFF, 0xFF, // length: i32::MAX
        0x1D, 0x7F, 0xFF, 0xFF, 0xFF, // offset: i32::MAX
        0x12, // operator: 18 (private)
    ];

    let top_dict = parse_top_dict(&mut Stream::new(data)).unwrap();
    assert_eq!(top_dict.private_dict_range, Some(2147483647..4294967294));
}

#[test]
fn private_dict_negative_char_strings_offset() {
    let data = &[
        0x00, 0x01, // count: 1
        0x01, // offset size: 1
        0x01, // index [0]: 1
        0x03, // index [1]: 3
        0x8A, // offset: -1
        0x11, // operator: 17 (char_string)
    ];

    assert!(parse_top_dict(&mut Stream::new(data)).is_none());
}

#[test]
fn private_dict_no_char_strings_offset_operand() {
    let data = &[
        0x00, 0x01, // count: 1
        0x01, // offset size: 1
        0x01, // index [0]: 1
        0x02, // index [1]: 2
        0x11, // operator: 17 (char_string)
    ];

    assert!(parse_top_dict(&mut Stream::new(data)).is_none());
}

#[test]
fn negative_private_dict_offset_and_size() {
    let data = &[
        0x00, 0x01, // count: 1
        0x01, // offset size: 1
        0x01, // index [0]: 1
        0x04, // index [1]: 4
        0x8A, // length: -1
        0x8A, // offset: -1
        0x12, // operator: 18 (private)
    ];

    let top_dict = parse_top_dict(&mut Stream::new(data)).unwrap();
    assert!(top_dict.private_dict_range.is_none());
}
