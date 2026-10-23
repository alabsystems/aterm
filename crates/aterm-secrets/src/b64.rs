// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical padded and unpadded standard Base64 (RFC 4648 §4) encoding and decoding.

use aterm_codec::base64;

/// Unpadded base64 of `bytes` (age: "the standard alphabet, without padding").
pub fn encode_nopad(bytes: &[u8]) -> String {
    let mut text = encode_padded(bytes);
    while text.ends_with('=') {
        text.pop();
    }
    text
}

/// Decodes unpadded Base64 strictly, rejecting padding, whitespace, invalid lengths, and non-zero trailing bits.
pub fn decode_nopad(text: &str) -> Option<Vec<u8>> {
    if text.contains('=') {
        return None;
    }
    let pad = match text.len() % 4 {
        0 => "",
        2 => "==",
        3 => "=",
        _ => return None,
    };
    let mut padded = String::with_capacity(text.len() + pad.len());
    padded.push_str(text);
    padded.push_str(pad);
    base64::decode_strict(padded.as_bytes()).ok()
}

/// Encodes `bytes` as padded Base64.
#[inline]
pub fn encode_padded(bytes: &[u8]) -> String {
    base64::encode(bytes).unwrap_or_default()
}

/// Decode padded base64 strictly (required padding, zero trailing bits, no whitespace).
#[inline]
pub fn decode_padded(text: &str) -> Option<Vec<u8>> {
    base64::decode_strict(text.as_bytes()).ok()
}

#[cfg(test)]
mod tests {
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
}
