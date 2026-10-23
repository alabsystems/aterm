// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Base64 encoding and decoding (RFC 4648).

use std::fmt;

const STANDARD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

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
    /// The data length is one modulo four, or lenient input has more than two padding bytes.
    InvalidLength(usize),
    /// The input exceeds [`crate::MAX_INPUT_LEN`] (DoS guard); value is the actual length.
    InputTooLarge(usize),
    /// Strict decode only: the padding is not canonical.
    InvalidPadding(usize),
    /// Strict decode only: the final data symbol has nonzero discarded bits, a non-canonical
    /// spelling (`"Zh=="` vs `"Zg=="`).
    InvalidLastSymbol(usize, u8),
}

impl fmt::Display for DecodeError {
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
/// # Errors
/// Returns [`EncodeError::InputTooLarge`] if `input` is longer than [`crate::MAX_INPUT_LEN`].
#[inline]
pub fn encode(input: &[u8]) -> Result<String, EncodeError> {
    let mut out = String::new();
    encode_into(&mut out, input)?;
    Ok(out)
}

/// Length of the padded encoding of `input_len` bytes, `4 * ceil(input_len / 3)`;
/// `None` above [`crate::MAX_INPUT_LEN`], exactly the lengths [`encode_into`]
/// refuses.
#[must_use]
#[inline]
pub const fn encoded_len(input_len: usize) -> Option<usize> {
    if input_len > crate::MAX_INPUT_LEN {
        return None;
    }
    #[expect(
        clippy::manual_div_ceil,
        reason = "the verifier bounds this arithmetic from the guard; div_ceil's result is opaque to it"
    )]
    let len = (input_len + 2) / 3 * 4;
    Some(len)
}

const MAX_ENCODED_LEN: usize = crate::MAX_INPUT_LEN.div_ceil(3) * 4;

/// [`encode`] appending to `out`, a byte-level fill with no buffer of its own.
/// # Errors
/// Returns [`EncodeError::InputTooLarge`], with `out` untouched, if `input` is longer than
/// [`crate::MAX_INPUT_LEN`].
pub fn encode_into(out: &mut String, input: &[u8]) -> Result<(), EncodeError> {
    let Some(len) = encoded_len(input.len()) else {
        return Err(EncodeError::InputTooLarge(input.len()));
    };
    out.reserve(len.min(MAX_ENCODED_LEN));
    // SAFETY: every byte appended is a `STANDARD_ALPHABET` entry or `b'='`, all ASCII
    // (`aterm-spec-models/proofs/ay/a2_codec/encoder_output_ascii.smt2`), so `out` stays UTF-8
    // after every push.
    let bytes = unsafe { out.as_mut_vec() };

    let (chunks, rem) = input.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        bytes.extend_from_slice(&sextets(n));
    }
    match *rem {
        [r0] => {
            let [s0, s1, _, _] = sextets(u32::from(r0) << 16);
            bytes.extend_from_slice(&[s0, s1, b'=', b'=']);
        }
        [r0, r1] => {
            let [s0, s1, s2, _] = sextets((u32::from(r0) << 16) | (u32::from(r1) << 8));
            bytes.extend_from_slice(&[s0, s1, s2, b'=']);
        }
        _ => {}
    }
    Ok(())
}

/// Decode standard Base64 (with or without padding).
/// # Errors
/// Returns [`DecodeError`] if the input contains invalid characters or has an invalid length.
#[inline]
pub fn decode(input: &str) -> Result<Vec<u8>, DecodeError> {
    decode_with_table(input.as_bytes(), &STANDARD_DECODE)
}

/// Decode standard Base64 under strict RFC 4648 canonicalisation.
/// # Errors
/// Returns [`DecodeError`] if the input is over [`crate::MAX_INPUT_LEN`], contains a byte outside
/// the standard alphabet, is not canonically padded, or is a non-canonical spelling of its output.
#[inline]
pub fn decode_strict(input: &[u8]) -> Result<Vec<u8>, DecodeError> {
    decode_strict_with_table(input, &STANDARD_DECODE)
}

#[inline]
const fn sextets(n: u32) -> [u8; 4] {
    let alphabet = STANDARD_ALPHABET;
    [
        alphabet[((n >> 18) & 0x3F) as usize],
        alphabet[((n >> 12) & 0x3F) as usize],
        alphabet[((n >> 6) & 0x3F) as usize],
        alphabet[(n & 0x3F) as usize],
    ]
}

fn decode_with_table(input: &[u8], table: &[u8; 256]) -> Result<Vec<u8>, DecodeError> {
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(DecodeError::InputTooLarge(input.len()));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let cap = input.len().min(crate::MAX_INPUT_LEN);

    let pad_count = input.iter().rev().take_while(|&&b| b == b'=').count();
    if pad_count > 2 {
        return Err(DecodeError::InvalidLength(input.len()));
    }
    let input = input
        .get(..input.len().saturating_sub(pad_count))
        .unwrap_or(input);

    let remainder = input.len() % 4;

    if remainder == 1 {
        return Err(DecodeError::InvalidLength(input.len()));
    }

    let mut out = Vec::with_capacity(cap);

    let mut i: usize = 0;

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
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(DecodeError::InputTooLarge(input.len()));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let remainder = input.len() % 4;
    if remainder != 0 {
        return Err(if remainder == 1 {
            DecodeError::InvalidLength(input.len())
        } else {
            DecodeError::InvalidPadding(input.len())
        });
    }

    let cap = input.len().min(crate::MAX_INPUT_LEN);
    let mut out = Vec::with_capacity(cap);

    let (chunks, _rem) = input.as_chunks::<4>();
    let last = chunks.len().saturating_sub(1);
    let mut i: usize = 0;
    for (idx, chunk) in chunks.iter().enumerate() {
        let &[c0, c1, c2, c3] = chunk;
        if idx == last && c3 == b'=' {
            if c2 == b'=' {
                let a = decode_byte(table, c0, i)?;
                let b = decode_byte(table, c1, i.saturating_add(1))?;
                if b & 0x0F != 0 {
                    return Err(DecodeError::InvalidLastSymbol(i.saturating_add(1), c1));
                }
                let n = (u32::from(a) << 18) | (u32::from(b) << 12);
                out.push(((n >> 16) & 0xFF) as u8);
            } else {
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
        i = i.saturating_add(4);
    }

    Ok(out)
}

#[inline]
const fn decode_byte(table: &[u8; 256], byte: u8, pos: usize) -> Result<u8, DecodeError> {
    let val = table[byte as usize];
    if val == 0xFF {
        Err(DecodeError::InvalidByte(pos, byte))
    } else {
        Ok(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_decode_refuses_malformed_input() {
        assert_eq!(decode("SGV!bG8="), Err(DecodeError::InvalidByte(3, b'!')));
        assert_eq!(decode("A"), Err(DecodeError::InvalidLength(1)));
        for bad in ["Zg=====", "Zg===", "====", "==="] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }
}
