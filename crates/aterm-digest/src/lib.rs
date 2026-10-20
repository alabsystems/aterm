// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cryptographic digests for aterm: SHA-256 and HMAC-SHA256.
//!
//! A first-party replacement for the `sha2` and `hmac` crates, covering the
//! surface aterm uses: [`Sha256::new`], [`Sha256::digest`], `update` and
//! `finalize`, plus HMAC's `new_from_slice` / `update` / `finalize` /
//! `verify_slice`. [`ct_eq`] replaces aterm's use of `subtle` (the package
//! stays in the graph through `rustls`).
//!
//! # Exported types
//!
//! - [`Sha256`] -- FIPS 180-4 SHA-256, streaming or one-shot: astream's
//!   `astream_digest::Sha256`, re-exported (below)
//! - [`HmacSha256`] -- RFC 2104 HMAC keyed with SHA-256
//! - [`ct_eq`] -- constant-time byte-slice comparison
//! - [`InvalidLength`], [`MacError`] -- API-compatible error types
//!
//! ONE SHA-256. The hash is astream-digest's, the same type astream-cap hashes
//! aterm-link's capability grants and producer ids with: zero dependencies,
//! `forbid(unsafe)`, its streaming buffer's obligations discharged as a Clean
//! island. It began as this crate's own `sha256.rs`, ported to astream with its
//! tests; keeping both meant two 320-line transcriptions of FIPS 180-4 that
//! nothing held to each other, so a fix or a proof had to land twice. This crate
//! keeps what astream does not export: the HMAC construction and [`ct_eq`]
//! (astream-cap's own HMAC is private to it).
//!
//! No SIMD, SHA-NI, assembly or `unsafe`: this hashes release manifests and
//! capability tokens, not bulk traffic. atpkg's bulk content (archive digests and
//! the extract fold) runs on ring's hardware SHA-256 (`atpkg::tree::BulkSha256`).
//! Input is consumed in 64-byte blocks straight from the caller's slice with one
//! partial-block buffer.
//!
//! # Example
//!
//! ```rust
//! use aterm_digest::{HmacSha256, Sha256};
//!
//! // One-shot.
//! let d = Sha256::digest(b"abc");
//! assert_eq!(d[0], 0xba);
//!
//! // Streaming.
//! let mut h = Sha256::new();
//! h.update(b"a");
//! h.update(b"bc");
//! assert_eq!(h.finalize(), d);
//!
//! // Keyed authentication; verification is constant time.
//! let mut mac = HmacSha256::new_from_slice(b"key").expect("any key length");
//! mac.update(b"message");
//! let tag = mac.finalize();
//!
//! let mut check = HmacSha256::new_from_slice(b"key").expect("any key length");
//! check.update(b"message");
//! assert!(check.verify_slice(&tag).is_ok());
//! ```

#![forbid(unsafe_code)]

mod hmac_sha256;

pub use astream_digest::Sha256;
pub use hmac_sha256::{HmacSha256, InvalidLength, MacError};

/// Compares two byte slices in constant time with respect to their contents.
///
/// Returns `true` when the slices have the same length and the same bytes.
///
/// An early-exit loop would leak the position of the first differing byte
/// through timing, letting an attacker recover an HMAC tag byte by byte. Every
/// byte pair is folded into one accumulator with `|=`, with no branch and no
/// early exit. Length is not secret (it is visible in the wire format), so an
/// unequal length may short-circuit. The final [`std::hint::black_box`] keeps
/// the optimiser from reintroducing an early exit once `diff` is non-zero.
#[must_use]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    let mut i = 0usize;
    while i < a.len() && i < b.len() {
        diff |= a[i] ^ b[i];
        i += 1;
    }
    std::hint::black_box(diff) == 0
}

/// Wire size in bytes of an encoded digest envelope header (`"SH25"`).
pub const DIGEST_ENVELOPE_WIRE_BYTES: usize = 16;

/// Wire size in bytes of an encoded HMAC tag-prefix header (`"HMAC"`).
pub const HMAC_PREFIX_WIRE_BYTES: usize = 16;

/// Encode a 16-byte SHA-256 digest envelope header (`"SH25"`).
#[inline]
#[must_use]
pub const fn encode_digest_envelope_wire(
    alg_id: u8,
    tag_len: u8,
    flags: u16,
    payload_len: u64,
) -> [u8; DIGEST_ENVELOPE_WIRE_BYTES] {
    [
        b'S',
        b'H',
        b'2',
        b'5',
        alg_id,
        tag_len,
        flags as u8,
        (flags >> 8) as u8,
        payload_len as u8,
        (payload_len >> 8) as u8,
        (payload_len >> 16) as u8,
        (payload_len >> 24) as u8,
        (payload_len >> 32) as u8,
        (payload_len >> 40) as u8,
        (payload_len >> 48) as u8,
        (payload_len >> 56) as u8,
    ]
}

/// Decode a 16-byte SHA-256 digest envelope header produced by
/// [`encode_digest_envelope_wire`].
///
/// Rejects envelopes whose `tag_len` is zero or exceeds 32 bytes.
#[inline]
#[must_use]
pub const fn decode_digest_envelope_wire(bytes: &[u8]) -> Option<(u8, u8, u16, u64)> {
    if bytes.len() < DIGEST_ENVELOPE_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'S' || bytes[1] != b'H' || bytes[2] != b'2' || bytes[3] != b'5' {
        return None;
    }
    let alg_id = bytes[4];
    let tag_len = bytes[5];
    if tag_len == 0 || tag_len > 32 {
        return None;
    }
    let flags = (bytes[6] as u16) | ((bytes[7] as u16) << 8);
    let payload_len = (bytes[8] as u64)
        | ((bytes[9] as u64) << 8)
        | ((bytes[10] as u64) << 16)
        | ((bytes[11] as u64) << 24)
        | ((bytes[12] as u64) << 32)
        | ((bytes[13] as u64) << 40)
        | ((bytes[14] as u64) << 48)
        | ((bytes[15] as u64) << 56);
    Some((alg_id, tag_len, flags, payload_len))
}

/// Encode a 16-byte HMAC-SHA256 prefix header (`"HMAC"` + `key_len: u32` +
/// 8-byte `tag_prefix: u64`).
#[inline]
#[must_use]
pub const fn encode_hmac_prefix_wire(
    key_len: u32,
    tag_prefix: u64,
) -> [u8; HMAC_PREFIX_WIRE_BYTES] {
    [
        b'H',
        b'M',
        b'A',
        b'C',
        key_len as u8,
        (key_len >> 8) as u8,
        (key_len >> 16) as u8,
        (key_len >> 24) as u8,
        tag_prefix as u8,
        (tag_prefix >> 8) as u8,
        (tag_prefix >> 16) as u8,
        (tag_prefix >> 24) as u8,
        (tag_prefix >> 32) as u8,
        (tag_prefix >> 40) as u8,
        (tag_prefix >> 48) as u8,
        (tag_prefix >> 56) as u8,
    ]
}

/// Decode a 16-byte HMAC-SHA256 prefix header produced by
/// [`encode_hmac_prefix_wire`].
#[inline]
#[must_use]
pub const fn decode_hmac_prefix_wire(bytes: &[u8]) -> Option<(u32, u64)> {
    if bytes.len() < HMAC_PREFIX_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'H' || bytes[1] != b'M' || bytes[2] != b'A' || bytes[3] != b'C' {
        return None;
    }
    let key_len = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let tag_prefix = (bytes[8] as u64)
        | ((bytes[9] as u64) << 8)
        | ((bytes[10] as u64) << 16)
        | ((bytes[11] as u64) << 24)
        | ((bytes[12] as u64) << 32)
        | ((bytes[13] as u64) << 40)
        | ((bytes[14] as u64) << 48)
        | ((bytes[15] as u64) << 56);
    Some((key_len, tag_prefix))
}

/// Lowercase hex, for test vectors only.
#[cfg(test)]
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_accepts_identical_slices() {
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"a", b"a"));
        assert!(ct_eq(&[0u8; 32], &[0u8; 32]));
        let tag = Sha256::digest(b"the quick brown fox");
        assert!(ct_eq(&tag, &tag));
    }

    #[test]
    fn ct_eq_rejects_same_length_differences() {
        // First byte differs.
        assert!(!ct_eq(b"abcdef", b"Xbcdef"));
        // Last byte differs -- the case an early-return loop would answer
        // slowest, and the one a timing attacker walks toward.
        assert!(!ct_eq(b"abcdef", b"abcdeX"));
        // A single bit differs.
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        b[31] = 0x01;
        assert!(!ct_eq(&a, &b));
        a[0] = 0x80;
        b[31] = 0x00;
        assert!(!ct_eq(&a, &b));
    }

    #[test]
    fn ct_eq_rejects_length_mismatch() {
        assert!(!ct_eq(b"", b"a"));
        assert!(!ct_eq(b"a", b""));
        // A prefix is not a match, in either direction.
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"abcd", b"abc"));
        // A truncated tag must never validate against a full one.
        let tag = Sha256::digest(b"payload");
        assert!(!ct_eq(&tag[..16], &tag));
    }

    #[test]
    fn digest_envelope_and_hmac_prefix_wire_roundtrip() {
        let ew = encode_digest_envelope_wire(1, 32, 0x0003, 65536);
        assert_eq!(decode_digest_envelope_wire(&ew), Some((1, 32, 3, 65536)));
        let bad_ew = encode_digest_envelope_wire(1, 33, 0, 0);
        assert!(decode_digest_envelope_wire(&bad_ew).is_none());

        let hw = encode_hmac_prefix_wire(64, 0x0102_0304_0506_0708);
        assert_eq!(
            decode_hmac_prefix_wire(&hw),
            Some((64, 0x0102_0304_0506_0708))
        );
    }
}
