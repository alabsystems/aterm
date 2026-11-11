// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cff/dict.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use super::*;

#[test]
fn parse_dict_number() {
    assert_eq!(
        parse_number(0xFA, &mut Stream::new(&[0x7C])).unwrap(),
        1000.0
    );
    assert_eq!(
        parse_number(0xFE, &mut Stream::new(&[0x7C])).unwrap(),
        -1000.0
    );
    assert_eq!(
        parse_number(0x1C, &mut Stream::new(&[0x27, 0x10])).unwrap(),
        10000.0
    );
    assert_eq!(
        parse_number(0x1C, &mut Stream::new(&[0xD8, 0xF0])).unwrap(),
        -10000.0
    );
    assert_eq!(
        parse_number(0x1D, &mut Stream::new(&[0x00, 0x01, 0x86, 0xA0])).unwrap(),
        100000.0
    );
    assert_eq!(
        parse_number(0x1D, &mut Stream::new(&[0xFF, 0xFE, 0x79, 0x60])).unwrap(),
        -100000.0
    );
}
