// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Base64 encoding and decoding (RFC 4648).

use std::fmt;

/// Standard Base64 alphabet (A-Z, a-z, 0-9, +, /).
const STANDARD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// URL-safe Base64 alphabet (A-Z, a-z, 0-9, -, _).
const URL_SAFE_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

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
const URL_SAFE_DECODE: [u8; 256] = build_decode_table(URL_SAFE_ALPHABET);

/// Error during Base64 decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// An invalid character was encountered at the given position.
    InvalidByte(usize, u8),
    /// The input length is not valid for Base64 (not a multiple of 4 when padded).
    InvalidLength(usize),
    /// The input exceeds [`crate::MAX_INPUT_LEN`] (DoS guard); value is the actual length.
    InputTooLarge(usize),
    /// Strict decode only: the padding is not canonical — the input length is
    /// not an exact multiple of four. Value is the input length.
    InvalidPadding(usize),
    /// Strict decode only: the final data symbol carries bits the decode would
    /// discard, so the input is a NON-CANONICAL spelling of its own output
    /// (`"Zh=="` and `"Zg=="` both mean `b"f"`). Position + the offending byte.
    InvalidLastSymbol(usize, u8),
}

impl fmt::Display for DecodeError {
    // Formatted via `write_str` + the crate's digit writers instead of
    // `write!`: the `format_args!` expansion calls the unsafe
    // `fmt::Arguments` constructor, which Trust fails closed on. The output
    // is byte-identical to the previous `write!` format strings.
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
    Ok(encode_with_alphabet(input, STANDARD_ALPHABET, true))
}

/// Encode bytes to URL-safe Base64 without padding.
///
/// # Errors
///
/// Returns [`EncodeError::InputTooLarge`] if `input` is longer than
/// [`crate::MAX_INPUT_LEN`].
pub fn encode_url_safe_no_pad(input: &[u8]) -> Result<String, EncodeError> {
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(EncodeError::InputTooLarge(input.len()));
    }
    Ok(encode_with_alphabet(input, URL_SAFE_ALPHABET, false))
}

/// Encode bytes to standard Base64 without padding.
///
/// # Errors
///
/// Returns [`EncodeError::InputTooLarge`] if `input` is longer than
/// [`crate::MAX_INPUT_LEN`].
pub fn encode_no_pad(input: &[u8]) -> Result<String, EncodeError> {
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(EncodeError::InputTooLarge(input.len()));
    }
    Ok(encode_with_alphabet(input, STANDARD_ALPHABET, false))
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

/// Decode URL-safe Base64 without padding, under STRICT RFC 4648
/// canonicalisation — the exact acceptance set of `base64`'s
/// `URL_SAFE_NO_PAD` engine.
///
/// It used to be the LENIENT decoder wearing this name: it delegated to the
/// same body [`decode`] does, which strips trailing `=` and ignores the bits a
/// truncated quad discards. Measured over 200,000 random candidates against the
/// retired engine, that was 12,538 disagreements — every one of them
/// ours-accepts / oracle-refuses (`"B1"`, `"1b="`, a bare `"="`). Nothing in
/// the tree called it, so this is a trap closed before the next caller finds
/// it, not a behaviour change under anyone.
///
/// Concretely, and unlike [`decode`]:
///
/// * `=` is never accepted — the URL-safe no-pad form has no padding, so the
///   pad byte is simply outside the alphabet;
/// * the low bits of the final symbol that the decode discards must be zero, so
///   exactly ONE spelling maps to any given byte string.
///
/// # Errors
///
/// Returns [`DecodeError`] if the input is over [`crate::MAX_INPUT_LEN`],
/// contains a byte outside the URL-safe alphabet (`=` included), has a length
/// that leaves a one-symbol tail, or is a non-canonical spelling of its output.
pub fn decode_url_safe_no_pad(input: &str) -> Result<Vec<u8>, DecodeError> {
    decode_strict_no_pad_with_table(input.as_bytes(), &URL_SAFE_DECODE)
}

/// Decode standard Base64 under STRICT RFC 4648 canonicalisation.
///
/// This is the acceptance set every trust-critical decode in the tree needs,
/// and it is deliberately NARROWER than [`decode`]:
///
/// * the input length must be an exact multiple of four — canonical padding is
///   REQUIRED, where [`decode`] happily accepts an unpadded tail;
/// * `=` may appear only as the last one or two bytes of the final quad;
/// * the low bits of the final data symbol that the decode discards must be
///   zero, so exactly ONE Base64 spelling maps to any given byte string.
///
/// That last rule is the reason this entry point exists. Under [`decode`] both
/// `"Zg=="` and `"Zh=="` yield `b"f"`, so a pinned Ed25519 key would have
/// textual aliases that compare unequal as strings yet verify identically —
/// and the release pipeline compares those keys AS TEXT
/// (`aterm_release::publish::canonical_update_pubkey`). Signature, roster and
/// pinned-key paths therefore call `decode_strict`; [`decode`] stays lenient
/// for terminal payloads (OSC 52, OSC 1337) where real emitters omit padding.
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

fn encode_with_alphabet(input: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    // Dominating DoS guard: bound `input.len()` so the `encoded_len` allocation
    // below is provably below the verifier's per-allocation ceiling. Every public
    // caller (`encode`/`encode_url_safe_no_pad`/`encode_no_pad`) already rejects
    // input over `crate::MAX_INPUT_LEN`, so this guard is unreachable for any real
    // call — it only makes the bound LOCALLY visible to verification and fails
    // safe (empty output) for any future caller that forgets the cap.
    if input.len() > crate::MAX_INPUT_LEN {
        return String::new();
    }
    if input.is_empty() {
        return String::new();
    }

    // Capacity is a SINGLE, branch-free over-estimate computed directly from the
    // guarded `input.len()` so the verifier can bound the allocation: every Base64
    // encoding of `n` bytes is at most `(n / 3 + 1) * 4` chars (4 output chars per
    // 3 input bytes, plus one partial-chunk quad). With `input.len() <=
    // MAX_INPUT_LEN` (the dominating guard above) this is `< 2 * MAX_INPUT_LEN <
    // 2^28`, and the exact length still drives the pushes below, so the returned
    // string is byte-identical — only the reservation may be a few bytes larger.
    let cap = input.len() / 3 * 4 + 4;
    let mut out = Vec::with_capacity(cap);

    // Process full 3-byte chunks — `as_chunks` yields `&[u8; 3]` arrays, so
    // every chunk index below is compile-time bounded (no runtime obligations),
    // and the remainder falls out of the same call.
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
            if pad {
                out.push(b'=');
                out.push(b'=');
            }
        }
        2 => {
            let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
            out.push(alphabet[((n >> 18) & 0x3F) as usize]);
            out.push(alphabet[((n >> 12) & 0x3F) as usize]);
            out.push(alphabet[((n >> 6) & 0x3F) as usize]);
            if pad {
                out.push(b'=');
            }
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

    // Reservation upper bound computed DIRECTLY from the original, guarded
    // `input.len()` (before any re-slice below): a Base64 decode yields at most 3
    // bytes per 4 input chars, so the decoded length never exceeds the original
    // input length. With the dominating `input.len() > MAX_INPUT_LEN` guard above
    // this is `< MAX_INPUT_LEN < 2^28`. Taken here, off the un-shadowed `input`,
    // so the bound is visible to verification (the post-pad-strip re-slice length
    // is a fresh slice var the verifier cannot relate back to the guarded length).
    // `.min(MAX_INPUT_LEN)` restates the dominating guard as a local fact for
    // the verifier's allocation budget; identical value on every path that
    // reaches here (the guard already returned for larger inputs).
    let cap = input.len().min(crate::MAX_INPUT_LEN);

    // Count and validate padding (0, 1, or 2 trailing '=' allowed).
    let pad_count = input.iter().rev().take_while(|&&b| b == b'=').count();
    if pad_count > 2 {
        return Err(DecodeError::InvalidLength(input.len()));
    }
    // `take_while` counts at most `input.len()` bytes, so this never actually
    // saturates; `saturating_sub` just makes the bound locally visible, and the
    // total `get(..).unwrap_or(input)` fallback (never taken: end <= len by the
    // saturation) removes the slice obligation outright.
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

    // Process full 4-byte chunks. `as_chunks::<4>()` yields `&[u8; 4]` arrays,
    // handing the verifier the chunk length at compile time — no index or
    // add-overflow obligations remain, and the array pattern is irrefutable;
    // `i` only tracks the absolute position for error reporting. It advances
    // by 4 per consumed chunk and never exceeds `input.len()`, so the
    // saturating adds never actually saturate. Iteration count, decode order,
    // and error positions are identical to the previous `while i + 4 <= len`
    // loop.
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

    // Process the remainder (0, 2, or 3 trailing chars; 1 was rejected above).
    // Same bytes and positions as the previous `input.get(i..)` re-slice —
    // `i == input.len() - rem.len()` here — destructured by slice pattern for
    // the same zero-obligation reason as the main loop.
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

/// The unpadded counterpart of [`decode_strict_with_table`]: no `=` anywhere,
/// a tail of two or three symbols instead of a pad-filled quad, and the same
/// refusal of discarded bits that are not zero.
fn decode_strict_no_pad_with_table(
    input: &[u8],
    table: &[u8; 256],
) -> Result<Vec<u8>, DecodeError> {
    // Dominating DoS guard, identical in placement and effect to the two
    // decoders above: bound the input before any work, so the `with_capacity`
    // below is provably under the verifier's allocation ceiling.
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(DecodeError::InputTooLarge(input.len()));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    // A one-symbol tail carries six bits, which is less than a byte, so it can
    // never be the truncation of any encoding.
    if input.len() % 4 == 1 {
        return Err(DecodeError::InvalidLength(input.len()));
    }

    // At most 3 output bytes per 4 input bytes, so the decoded length never
    // exceeds the (already guarded) input length. `.min` restates the dominating
    // guard as a local fact for the verifier; identical value on every path.
    let cap = input.len().min(crate::MAX_INPUT_LEN);
    let mut out = Vec::with_capacity(cap);

    // `=` is outside the URL-safe alphabet, so `decode_byte` refuses it in any
    // position — which is the whole difference from the lenient body.
    let (chunks, rem) = input.as_chunks::<4>();
    let mut i: usize = 0;
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
            // 12 bits carried, 8 consumed: the low 4 bits of the second symbol
            // are discarded and must therefore be zero.
            let a = decode_byte(table, r0, i)?;
            let b = decode_byte(table, r1, i.saturating_add(1))?;
            if b & 0x0F != 0 {
                return Err(DecodeError::InvalidLastSymbol(i.saturating_add(1), r1));
            }
            let n = (u32::from(a) << 18) | (u32::from(b) << 12);
            out.push(((n >> 16) & 0xFF) as u8);
        }
        [r0, r1, r2] => {
            // 18 bits carried, 16 consumed: the low 2 bits of the third symbol
            // are discarded and must therefore be zero.
            let a = decode_byte(table, r0, i)?;
            let b = decode_byte(table, r1, i.saturating_add(1))?;
            let c = decode_byte(table, r2, i.saturating_add(2))?;
            if c & 0x03 != 0 {
                return Err(DecodeError::InvalidLastSymbol(i.saturating_add(2), r2));
            }
            let n = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6);
            out.push(((n >> 16) & 0xFF) as u8);
            out.push(((n >> 8) & 0xFF) as u8);
        }
        _ => {}
    }

    Ok(out)
}

fn decode_strict_with_table(input: &[u8], table: &[u8; 256]) -> Result<Vec<u8>, DecodeError> {
    // Dominating DoS guard, identical in placement and effect to
    // `decode_with_table`: bound the input before any work, so the
    // `with_capacity` below is provably under the verifier's allocation ceiling.
    if input.len() > crate::MAX_INPUT_LEN {
        return Err(DecodeError::InputTooLarge(input.len()));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    // Canonical padding: the encoded form of `n` bytes is always a whole number
    // of quads. A remainder of 1 can never be a truncated quad (six bits is less
    // than a byte) and is reported as a length error; any other remainder is an
    // input that dropped its `=` padding.
    let remainder = input.len() % 4;
    if remainder != 0 {
        return Err(if remainder == 1 {
            DecodeError::InvalidLength(input.len())
        } else {
            DecodeError::InvalidPadding(input.len())
        });
    }

    // At most 3 output bytes per 4 input bytes, so the decoded length never
    // exceeds the (already guarded) input length. `.min` restates the dominating
    // guard as a local fact for the verifier; identical value on every path.
    let cap = input.len().min(crate::MAX_INPUT_LEN);
    let mut out = Vec::with_capacity(cap);

    // `input.len() % 4 == 0` and `input` is non-empty, so `as_chunks::<4>()`
    // yields at least one `&[u8; 4]` and an EMPTY remainder — every byte of the
    // input is covered by the loop below, and each index is bounded at compile
    // time by the array pattern.
    let (chunks, _rem) = input.as_chunks::<4>();
    let last = chunks.len().saturating_sub(1);
    let mut i: usize = 0;
    for (idx, chunk) in chunks.iter().enumerate() {
        let &[c0, c1, c2, c3] = chunk;
        // Padding is legal ONLY in the final quad. In every other quad `=` is
        // just a byte outside the alphabet and `decode_byte` rejects it, which
        // is what makes `"Zg==Zg=="` an error rather than two concatenated
        // decodes.
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
        // base64 carries untrusted payloads (e.g. OSC 52 clipboard data from any
        // program), so `decode` must NEVER panic on arbitrary input — only
        // Ok/Err — and `encode` ∘ `decode` must round-trip every byte string.
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
    fn test_encode_empty() {
        assert_eq!(encode(b"").unwrap(), "");
    }

    #[test]
    fn test_encode_hello_world() {
        assert_eq!(encode(b"Hello, world!").unwrap(), "SGVsbG8sIHdvcmxkIQ==");
    }

    #[test]
    fn test_encode_padding_one() {
        // 1 byte remainder -> 2 padding chars
        assert_eq!(encode(b"f").unwrap(), "Zg==");
    }

    #[test]
    fn test_encode_padding_two() {
        // 2 byte remainder -> 1 padding char
        assert_eq!(encode(b"fo").unwrap(), "Zm8=");
    }

    #[test]
    fn test_encode_no_padding() {
        // 3 byte multiple -> no padding
        assert_eq!(encode(b"foo").unwrap(), "Zm9v");
    }

    #[test]
    fn test_decode_empty() {
        assert_eq!(decode("").unwrap(), b"");
    }

    #[test]
    fn test_decode_hello_world() {
        assert_eq!(decode("SGVsbG8sIHdvcmxkIQ==").unwrap(), b"Hello, world!");
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
    fn test_roundtrip_standard() {
        for input in [
            b"".as_slice(),
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            b"Hello, world!",
            &[0u8; 256],
            &(0..=255).collect::<Vec<u8>>(),
        ] {
            let encoded = encode(input).expect("encode");
            let decoded = decode(&encoded).expect("roundtrip decode failed");
            assert_eq!(decoded, input);
        }
    }

    #[test]
    fn test_roundtrip_url_safe() {
        for input in [
            b"".as_slice(),
            b"a",
            b"ab",
            b"abc",
            b"Hello, world!",
            &(0..=255).collect::<Vec<u8>>(),
        ] {
            let encoded = encode_url_safe_no_pad(input).expect("encode");
            let decoded = decode_url_safe_no_pad(&encoded).expect("roundtrip decode failed");
            assert_eq!(decoded, input);
        }
    }

    #[test]
    fn test_url_safe_alphabet() {
        // URL-safe should not contain + or /
        let encoded = encode_url_safe_no_pad(&[0xFF, 0xFF, 0xFF]).unwrap();
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
    }

    #[test]
    fn test_encode_no_pad_function() {
        assert_eq!(encode_no_pad(b"f").unwrap(), "Zg");
        assert_eq!(encode_no_pad(b"fo").unwrap(), "Zm8");
        assert_eq!(encode_no_pad(b"foo").unwrap(), "Zm9v");
    }

    #[test]
    fn test_rfc4648_vectors() {
        // Test vectors from RFC 4648 section 10
        assert_eq!(encode(b"").unwrap(), "");
        assert_eq!(encode(b"f").unwrap(), "Zg==");
        assert_eq!(encode(b"fo").unwrap(), "Zm8=");
        assert_eq!(encode(b"foo").unwrap(), "Zm9v");
        assert_eq!(encode(b"foob").unwrap(), "Zm9vYg==");
        assert_eq!(encode(b"fooba").unwrap(), "Zm9vYmE=");
        assert_eq!(encode(b"foobar").unwrap(), "Zm9vYmFy");
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
