// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Hexadecimal encode and decode shared by the workspace.

/// Lowercase hex digit for the low four bits of `n`.
#[inline]
pub(crate) const fn digit_lower(n: u8) -> char {
    match n & 0x0f {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        _ => 'f',
    }
}

/// Uppercase hex digit for a nibble, as [`digit_lower`].
#[inline]
const fn digit_upper(n: u8) -> char {
    match n & 0x0f {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'A',
        11 => 'B',
        12 => 'C',
        13 => 'D',
        14 => 'E',
        _ => 'F',
    }
}

/// Append the lowercase hex of `bytes` to `out`.
#[inline]
pub fn push_lower(out: &mut String, bytes: &[u8]) {
    for &b in bytes {
        out.push(digit_lower(b / 16));
        out.push(digit_lower(b % 16));
    }
}

/// The lowercase hex spelling of `bytes` (`2 * bytes.len()` characters).
#[must_use]
#[inline]
pub fn encode_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    push_lower(&mut out, bytes);
    out
}

/// The uppercase hex spelling of `bytes`: byte-identical to `{b:02X}` per byte.
#[must_use]
#[inline]
pub fn encode_upper(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &b in bytes {
        out.push(digit_upper(b / 16));
        out.push(digit_upper(b % 16));
    }
    out
}

/// The value (`0..=15`) of one ASCII hex digit of either case, or `None`.
#[must_use]
#[inline]
pub const fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// The byte two hex digits spell, high digit first.
#[inline]
const fn byte(hi: u8, lo: u8) -> Option<u8> {
    match (nibble(hi), nibble(lo)) {
        (Some(hi), Some(lo)) => Some(((hi & 0x0f) << 4) | (lo & 0x0f)),
        _ => None,
    }
}

/// Decode `2 * out.len()` hex digits of either case into `out`, returning `false` on mismatch.
#[must_use]
#[inline]
pub fn decode_into(text: &[u8], out: &mut [u8]) -> bool {
    if Some(text.len()) != out.len().checked_mul(2) {
        return false;
    }
    for (slot, &[hi, lo]) in out.iter_mut().zip(text.as_chunks::<2>().0) {
        let Some(b) = byte(hi, lo) else {
            return false;
        };
        *slot = b;
    }
    true
}

/// Decode an even number of hex digits of either case.
#[must_use]
#[inline]
pub fn decode_vec(text: &[u8]) -> Option<Vec<u8>> {
    let (pairs, rest) = text.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(pairs.len());
    for &[hi, lo] in pairs {
        out.push(byte(hi, lo)?);
    }
    Some(out)
}

/// Whether `text` is exactly `len` lowercase hex digits (`0-9a-f`).
#[must_use]
#[inline]
pub fn is_lower(text: &str, len: usize) -> bool {
    if text.len() != len {
        return false;
    }
    for &c in text.as_bytes() {
        if !matches!(c, b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
    }
    true
}

/// Whether `text` is exactly `len` hex digits of either case.
#[must_use]
#[inline]
pub fn is_hex(text: &str, len: usize) -> bool {
    if text.len() != len {
        return false;
    }
    for &c in text.as_bytes() {
        if nibble(c).is_none() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
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
}
