// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn rfc4648_vectors_round_trip_in_both_spellings() {
    for (plain, padded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(encode_padded(plain.as_bytes()), padded);
        assert_eq!(decode_padded(padded).as_deref(), Some(plain.as_bytes()));
        let bare = padded.trim_end_matches('=');
        assert_eq!(encode_nopad(plain.as_bytes()), bare);
        assert_eq!(decode_nopad(bare).as_deref(), Some(plain.as_bytes()));
    }
}

#[test]
fn unpadded_refuses_padding_bad_length_and_trailing_bits() {
    assert_eq!(decode_nopad("Zg=="), None);
    assert_eq!(decode_nopad("Z"), None);
    assert_eq!(decode_nopad("Zh"), None, "non-zero trailing bits");
    assert_eq!(decode_nopad("Zm9 v"), None);
    assert_eq!(decode_nopad("Zm9v\n"), None);
}

#[test]
fn padded_refuses_missing_padding_and_trailing_bits() {
    assert_eq!(decode_padded("Zg"), None);
    assert_eq!(decode_padded("Zh=="), None);
}
