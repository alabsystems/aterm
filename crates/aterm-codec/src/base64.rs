// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Base64 encoding and decoding (RFC 4648).

use std::fmt;

/// Standard Base64 alphabet (A-Z, a-z, 0-9, +, /).
const STANDARD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Build a 256-byte decode lookup table from an alphabet.
/// Invalid characters map to 0xFF.
const fn build_decode_table(alphabet: &[u8; 64]) -> [u8; 256] {
    let mut table = [0xFFu8; 256];
    let mut i = 0;
    while i < 64 {
        table[alphabet[i] as usize] = i as u8;
        i += 1;
    }
    table
}

const STANDARD_DECODE: [u8; 256] = build_decode_table(STANDARD_ALPHABET);

/// Error during Base64 decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// An invalid character was encountered at the given position.
    InvalidByte(usize, u8),
    /// The data length is one modulo four, or lenient input has more than two
    /// padding bytes. The value is the length checked, after stripping at most
    /// two trailing padding bytes in the lenient data-length check.
    InvalidLength(usize),
    /// The input exceeds [`crate::MAX_INPUT_LEN`] (DoS guard); value is the actual length.
    InputTooLarge(usize),
    /// Strict decode only: the padding is not canonical — the input length is
    /// not an exact multiple of four. Value is the input length.
    InvalidPadding(usize),
    /// Strict decode only: the final data symbol has nonzero discarded bits,
    /// a non-canonical spelling (`"Zh=="` vs `"Zg=="`). Position + byte.
    InvalidLastSymbol(usize, u8),
}

impl fmt::Display for DecodeError {
    // `write_str` + the crate's digit writers instead of `write!`, whose
    // `format_args!` Trust fails closed on.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidByte(pos, byte) => {
                f.write_str("invalid base64 byte 0x")?;
                crate::write_hex_byte_upper(f, *byte)?;
                f.write_str(" at position ")?;
                crate::write_usize_decimal(f, *pos)
            }
            Self::InvalidLength(len) => {
                f.write_str("invalid base64 input length: ")?;
                crate::write_usize_decimal(f, *len)
            }
            Self::InputTooLarge(len) => {
                f.write_str("base64 input length ")?;
                crate::write_usize_decimal(f, *len)?;
                f.write_str(" exceeds maximum ")?;
                crate::write_usize_decimal(f, crate::MAX_INPUT_LEN)
            }
            Self::InvalidPadding(len) => {
                f.write_str("base64 input is not canonically padded: length ")?;
                crate::write_usize_decimal(f, *len)?;
                f.write_str(" is not a multiple of 4")
            }
            Self::InvalidLastSymbol(pos, byte) => {
                f.write_str("non-canonical base64: symbol 0x")?;
                crate::write_hex_byte_upper(f, *byte)?;
                f.write_str(" at position ")?;
                crate::write_usize_decimal(f, *pos)?;
                f.write_str(" has bits the decode discards")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Error during Base64 encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The input exceeds [`crate::MAX_INPUT_LEN`] (DoS guard); value is the actual length.
    InputTooLarge(usize),
}

impl fmt::Display for EncodeError {
    // `write_str`-based for the same Trust `format_args!` reason as
    // `DecodeError`; output is byte-identical.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge(len) => {
                f.write_str("base64 input length ")?;
                crate::write_usize_decimal(f, *len)?;
                f.write_str(" exceeds maximum ")?;
                crate::write_usize_decimal(f, crate::MAX_INPUT_LEN)
            }
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encode bytes to standard Base64 with padding.
///
/// # Errors
///
/// Returns [`EncodeError::InputTooLarge`] if `input` is longer than
/// [`crate::MAX_INPUT_LEN`].
pub fn encode(input: &[u8]) -> Result<String, EncodeError> {
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(EncodeError::InputTooLarge(input.len()));
    }
    Ok(encode_padded(input))
}

/// Decode standard Base64 (with or without padding).
///
/// # Errors
///
/// Returns [`DecodeError`] if the input contains invalid characters or has
/// an invalid length.
pub fn decode(input: &str) -> Result<Vec<u8>, DecodeError> {
    decode_with_table(input.as_bytes(), &STANDARD_DECODE)
}

/// Decode standard Base64 under strict RFC 4648 canonicalisation.
///
/// Narrower than [`decode`]:
///
/// * the length must be a multiple of four (padding required);
/// * `=` may appear only as the last one or two bytes of the final quad;
/// * the discarded low bits of the final data symbol must be zero, so exactly
///   one spelling maps to any byte string.
///
/// Signature, roster and pinned-key paths use this, since keys are compared as
/// text (`aterm_release::publish::canonical_update_pubkey`); [`decode`] stays
/// lenient for terminal payloads whose emitters omit padding.
///
/// # Errors
///
/// Returns [`DecodeError`] if the input is over [`crate::MAX_INPUT_LEN`],
/// contains a byte outside the standard alphabet, is not canonically padded, or
/// is a non-canonical spelling of its output.
pub fn decode_strict(input: &[u8]) -> Result<Vec<u8>, DecodeError> {
    decode_strict_with_table(input, &STANDARD_DECODE)
}

// ── Internal ────────────────────────────────────────────────────────────────

fn encode_padded(input: &[u8]) -> String {
    let alphabet = STANDARD_ALPHABET;
    // Makes the `encoded_len` allocation bound locally provable; `encode`
    // already rejects larger input, so this only fails safe for other callers.
    if input.len() > crate::MAX_INPUT_LEN {
        return String::new();
    }
    if input.is_empty() {
        return String::new();
    }

    // Branch-free over-estimate `(n / 3 + 1) * 4 < 2^28` the verifier can
    // bound; the pushes below produce the exact length.
    let cap = input.len() / 3 * 4 + 4;
    let mut out = Vec::with_capacity(cap);

    // `as_chunks` yields `&[u8; 3]`, so chunk indices are compile-time bounded.
    let (chunks, rem) = input.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(alphabet[((n >> 18) & 0x3F) as usize]);
        out.push(alphabet[((n >> 12) & 0x3F) as usize]);
        out.push(alphabet[((n >> 6) & 0x3F) as usize]);
        out.push(alphabet[(n & 0x3F) as usize]);
    }

    // Process remainder
    match rem.len() {
        1 => {
            let n = u32::from(rem[0]) << 16;
            out.push(alphabet[((n >> 18) & 0x3F) as usize]);
            out.push(alphabet[((n >> 12) & 0x3F) as usize]);
            out.push(b'=');
            out.push(b'=');
        }
        2 => {
            let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
            out.push(alphabet[((n >> 18) & 0x3F) as usize]);
            out.push(alphabet[((n >> 12) & 0x3F) as usize]);
            out.push(alphabet[((n >> 6) & 0x3F) as usize]);
            out.push(b'=');
        }
        _ => {}
    }

    // SAFETY: all output bytes are ASCII from the alphabet or '='
    unsafe { String::from_utf8_unchecked(out) }
}

fn decode_with_table(input: &[u8], table: &[u8; 256]) -> Result<Vec<u8>, DecodeError> {
    // Dominating DoS guard: bound the input (and therefore the decoded length and
    // the with_capacity allocation below) before doing any work.
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(DecodeError::InputTooLarge(input.len()));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    // Decoded length never exceeds the input length. Taken from the guarded,
    // un-reshadowed `input` so the verifier can relate it to the guard; `.min`
    // restates the guard locally.
    let cap = input.len().min(crate::MAX_INPUT_LEN);

    // Count and validate padding (0, 1, or 2 trailing '=' allowed).
    let pad_count = input.iter().rev().take_while(|&&b| b == b'=').count();
    if pad_count > 2 {
        return Err(DecodeError::InvalidLength(input.len()));
    }
    // Never saturates, and the `get(..).unwrap_or(input)` fallback is never
    // taken; both remove obligations.
    let input = input
        .get(..input.len().saturating_sub(pad_count))
        .unwrap_or(input);

    let remainder = input.len() % 4;

    // Remainder of 1 is never valid (would encode only 6 bits, less than a byte)
    if remainder == 1 {
        return Err(DecodeError::InvalidLength(input.len()));
    }

    // The exact decoded length is still produced by the pushes below, so the
    // returned bytes are identical — only the reservation may be a few bytes larger.
    let mut out = Vec::with_capacity(cap);

    let mut i: usize = 0;

    // `as_chunks::<4>()` gives fixed-size arrays, so no index or overflow
    // obligations remain; `i` tracks the absolute position for errors and
    // never saturates.
    let (chunks, rem) = input.as_chunks::<4>();
    for chunk in chunks {
        let &[c0, c1, c2, c3] = chunk;
        let a = decode_byte(table, c0, i)?;
        let b = decode_byte(table, c1, i.saturating_add(1))?;
        let c = decode_byte(table, c2, i.saturating_add(2))?;
        let d = decode_byte(table, c3, i.saturating_add(3))?;

        let n = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6) | u32::from(d);
        out.push(((n >> 16) & 0xFF) as u8);
        out.push(((n >> 8) & 0xFF) as u8);
        out.push((n & 0xFF) as u8);
        i = i.saturating_add(4);
    }

    // Remainder: 0, 2 or 3 chars (1 was rejected above); `i` is its offset.
    match *rem {
        [r0, r1] => {
            let a = decode_byte(table, r0, i)?;
            let b = decode_byte(table, r1, i.saturating_add(1))?;
            let n = (u32::from(a) << 18) | (u32::from(b) << 12);
            out.push(((n >> 16) & 0xFF) as u8);
        }
        [r0, r1, r2] => {
            let a = decode_byte(table, r0, i)?;
            let b = decode_byte(table, r1, i.saturating_add(1))?;
            let c = decode_byte(table, r2, i.saturating_add(2))?;
            let n = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6);
            out.push(((n >> 16) & 0xFF) as u8);
            out.push(((n >> 8) & 0xFF) as u8);
        }
        _ => {}
    }

    Ok(out)
}

fn decode_strict_with_table(input: &[u8], table: &[u8; 256]) -> Result<Vec<u8>, DecodeError> {
    // Allocation guard, as in `decode_with_table`.
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(DecodeError::InputTooLarge(input.len()));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    // Canonical encodings are whole quads. A remainder of 1 is a length error
    // (six bits is less than a byte); any other means missing padding.
    let remainder = input.len() % 4;
    if remainder != 0 {
        return Err(if remainder == 1 {
            DecodeError::InvalidLength(input.len())
        } else {
            DecodeError::InvalidPadding(input.len())
        });
    }

    // Decoded length never exceeds the guarded input length; `.min` restates
    // the guard for the verifier.
    let cap = input.len().min(crate::MAX_INPUT_LEN);
    let mut out = Vec::with_capacity(cap);

    // Length is a nonzero multiple of 4, so the remainder is empty and the
    // loop covers every byte.
    let (chunks, _rem) = input.as_chunks::<4>();
    let last = chunks.len().saturating_sub(1);
    let mut i: usize = 0;
    for (idx, chunk) in chunks.iter().enumerate() {
        let &[c0, c1, c2, c3] = chunk;
        // Padding is legal only in the final quad; elsewhere `decode_byte`
        // rejects `=`, so `"Zg==Zg=="` is an error.
        if idx == last && c3 == b'=' {
            if c2 == b'=' {
                // `XX==` — 12 bits carried, 8 consumed; the low 4 bits of the
                // second symbol are discarded and must therefore be zero.
                let a = decode_byte(table, c0, i)?;
                let b = decode_byte(table, c1, i.saturating_add(1))?;
                if b & 0x0F != 0 {
                    return Err(DecodeError::InvalidLastSymbol(i.saturating_add(1), c1));
                }
                let n = (u32::from(a) << 18) | (u32::from(b) << 12);
                out.push(((n >> 16) & 0xFF) as u8);
            } else {
                // `XXX=` — 18 bits carried, 16 consumed; the low 2 bits of the
                // third symbol are discarded and must therefore be zero.
                let a = decode_byte(table, c0, i)?;
                let b = decode_byte(table, c1, i.saturating_add(1))?;
                let c = decode_byte(table, c2, i.saturating_add(2))?;
                if c & 0x03 != 0 {
                    return Err(DecodeError::InvalidLastSymbol(i.saturating_add(2), c2));
                }
                let n = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6);
                out.push(((n >> 16) & 0xFF) as u8);
                out.push(((n >> 8) & 0xFF) as u8);
            }
        } else {
            let a = decode_byte(table, c0, i)?;
            let b = decode_byte(table, c1, i.saturating_add(1))?;
            let c = decode_byte(table, c2, i.saturating_add(2))?;
            let d = decode_byte(table, c3, i.saturating_add(3))?;
            let n =
                (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6) | u32::from(d);
            out.push(((n >> 16) & 0xFF) as u8);
            out.push(((n >> 8) & 0xFF) as u8);
            out.push((n & 0xFF) as u8);
        }
        // Advances by 4 per consumed quad and never exceeds `input.len()`, so
        // the saturation never actually engages; it only makes the bound local.
        i = i.saturating_add(4);
    }

    Ok(out)
}

#[inline]
fn decode_byte(table: &[u8; 256], byte: u8, pos: usize) -> Result<u8, DecodeError> {
    let val = table[byte as usize];
    if val == 0xFF {
        Err(DecodeError::InvalidByte(pos, byte))
    } else {
        Ok(val)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzz_base64_roundtrip_and_decode_never_panics() {
        // `decode` must never panic on arbitrary input, and `encode` then
        // `decode` must round-trip every byte string.
        let mut state: u64 = 0xC2B2_AE3D_27D4_EB4F;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u32
        };
        for _ in 0..50_000 {
            // Round-trip arbitrary bytes.
            let len = (next() % 64) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
            let encoded = encode(&bytes).expect("encode");
            assert_eq!(decode(&encoded).expect("valid base64 must decode"), bytes);

            // Arbitrary (likely-invalid) string: must return cleanly, never panic.
            let slen = (next() % 80) as usize;
            let s: String = (0..slen)
                .map(|_| {
                    const ALPH: &[u8] =
                        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=-_ \n\t!";
                    ALPH[(next() as usize) % ALPH.len()] as char
                })
                .collect();
            let _ = decode(&s);
        }
    }

    #[test]
    fn test_decode_without_padding() {
        // Should work without padding too
        assert_eq!(decode("SGVsbG8sIHdvcmxkIQ").unwrap(), b"Hello, world!");
    }

    #[test]
    fn test_decode_invalid_char() {
        let result = decode("SGV!bG8=");
        assert!(result.is_err());
        if let Err(DecodeError::InvalidByte(pos, byte)) = result {
            assert_eq!(pos, 3);
            assert_eq!(byte, b'!');
        }
    }

    #[test]
    fn test_decode_invalid_length() {
        // Single char is never valid
        let result = decode("A");
        assert!(result.is_err());
        assert!(matches!(result, Err(DecodeError::InvalidLength(1))));
    }

    #[test]
    fn test_decode_excess_padding_rejected() {
        assert!(decode("Zg=====").is_err());
        assert!(decode("Zg===").is_err());
        // Valid padding counts still work.
        assert_eq!(decode("Zg==").unwrap(), b"f");
        assert_eq!(decode("Zm8=").unwrap(), b"fo");
        assert_eq!(decode("Zm9v").unwrap(), b"foo");
    }

    #[test]
    fn test_decode_all_padding_rejected() {
        assert!(decode("====").is_err());
        assert!(decode("===").is_err());
    }

    #[test]
    fn test_decode_error_display() {
        let err = DecodeError::InvalidByte(3, 0xFF);
        assert_eq!(err.to_string(), "invalid base64 byte 0xFF at position 3");

        let err = DecodeError::InvalidLength(5);
        assert_eq!(err.to_string(), "invalid base64 input length: 5");
    }
}
