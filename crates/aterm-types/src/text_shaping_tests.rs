// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn parse_font_features_short_tag_padded() {
    let f = parse_font_features("+cv1");
    assert_eq!(f.len(), 1);
    assert_eq!(&f[0].tag, b"cv1 ");
}

#[test]
fn parse_font_features_skips_malformed() {
    let f = parse_font_features("+toolong + -calt cv01=x zero");
    assert_eq!(f.len(), 2);
    assert_eq!(&f[0].tag, b"calt");
    assert_eq!(f[0].value, 0);
    assert_eq!(&f[1].tag, b"zero");
}
