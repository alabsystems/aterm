// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::fmt::Write as _;

#[test]
fn every_byte_and_digit_matches_std() {
    let all: Vec<u8> = (0..=255).collect();
    let mut lower = String::new();
    let mut upper = String::new();
    for b in &all {
        let _ = write!(lower, "{b:02x}");
        let _ = write!(upper, "{b:02X}");
    }
    assert_eq!(encode_lower(&all), lower);
    assert_eq!(encode_upper(&all), upper);
    let mut pushed = String::from("n-");
    push_lower(&mut pushed, &all);
    assert_eq!(pushed, format!("n-{lower}"));

    for c in 0..=255u8 {
        let std = char::from(c).to_digit(16).map(|d| d as u8);
        assert_eq!(nibble(c), std, "digit {c:#04x}");
        let s = char::from(c).to_string();
        if c.is_ascii() {
            assert_eq!(is_hex(&s, 1), std.is_some());
            assert_eq!(is_lower(&s, 1), std.is_some() && !c.is_ascii_uppercase());
        }
    }

    assert_eq!(decode_vec(lower.as_bytes()), Some(all.clone()));
    assert_eq!(decode_vec(upper.as_bytes()), Some(all.clone()));
    let mut out = vec![0u8; 256];
    assert!(decode_into(upper.as_bytes(), &mut out));
    assert_eq!(out, all);
    assert!(is_lower(&lower, 512) && !is_lower(&upper, 512));
    assert!(is_hex(&lower, 512) && is_hex(&upper, 512));
}

#[test]
fn decoders_refuse_what_is_not_exactly_hex() {
    assert_eq!(decode_vec(b""), Some(Vec::new()));
    assert_eq!(decode_vec(b"5"), None);
    assert_eq!(decode_vec(b"5G"), None);
    assert_eq!(decode_vec(b"+a"), None);
    assert_eq!(decode_vec(b"544E"), Some(b"TN".to_vec()));

    let mut two = [0u8; 2];
    assert!(!decode_into(b"abc", &mut two));
    assert!(!decode_into(b"abcdef", &mut two));
    assert!(!decode_into(b"ab-d", &mut two));
    assert!(decode_into(b"aBcD", &mut two));
    assert_eq!(two, [0xab, 0xcd]);

    assert!(!is_lower("abc", 2) && !is_hex("abc", 2));
    assert!(!is_lower("+a", 2) && !is_hex("+a", 2));
}
