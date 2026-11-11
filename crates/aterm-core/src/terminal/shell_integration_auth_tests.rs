// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn hex_of(nonce: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    for &b in nonce {
        out.push(to_hex_digit(b >> 4));
        out.push(to_hex_digit(b & 0xF));
    }
    out
}

fn to_hex_digit(v: u8) -> u8 {
    match v {
        0..=9 => b'0' + v,
        10..=15 => b'a' + (v - 10),
        _ => unreachable!("only called with 4-bit nibbles"),
    }
}

/// `verify_nonce` accepts only a 64-digit `id=` (either case) at parameter
/// 2 or later that decodes to the authorized nonce, and every refusal
/// counts one drop.
#[test]
fn verify_nonce_accepts_only_the_authorized_id() {
    let nonce: [u8; 32] = core::array::from_fn(|i| u8::try_from(i * 7).unwrap_or(0));
    let id = |hex: Vec<u8>| [b"id=".to_vec(), hex].concat();
    let ok = id(hex_of(&nonce));
    let upper = id(hex_of(&nonce).to_ascii_uppercase());
    let mut late = nonce;
    late[31] ^= 1;
    let late = id(hex_of(&late));
    let short = ok[..ok.len() - 1].to_vec();
    let mut not_hex = ok.clone();
    not_hex[3] = b'g';
    let cases: [(&str, bool, &[&[u8]], bool); 9] = [
        ("the authorized id", true, &[b"133", b"A", &ok], true),
        ("upper-case hex", true, &[b"133", b"A", &upper], true),
        (
            "after a 633 E payload",
            true,
            &[b"633", b"E", b"ls -la", &ok],
            true,
        ),
        ("none authorized", false, &[b"133", b"A", &ok], false),
        ("the last byte differs", true, &[b"133", b"A", &late], false),
        ("no id", true, &[b"133", b"A"], false),
        ("63 digits", true, &[b"133", b"A", &short], false),
        ("a non-hex digit", true, &[b"133", b"A", &not_hex], false),
        ("id in the subcommand slot", true, &[b"133", &ok], false),
    ];
    for (name, authorized, params, want) in cases {
        let mut auth = ShellIntegrationAuth::new();
        if authorized {
            auth.authorize(nonce);
        }
        assert_eq!(auth.verify_nonce(params), want, "{name}");
        assert_eq!(auth.dropped_count(), u64::from(!want), "{name}: drops");
    }
}
