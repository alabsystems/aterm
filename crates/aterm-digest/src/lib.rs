// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cryptographic digests for aterm ([`Sha256`], [`Sha256::new`], [`Sha256::digest`],
//! [`HmacSha256`], [`hmac_sha256_parts`], [`hkdf_extract`], [`hkdf_expand_32`], [`ct_eq`],
//! [`InvalidLength`], [`MacError`]).
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

pub use astream_digest::{
    HmacSha256, InvalidLength, MacError, Sha256, ct_eq, hkdf_expand_32, hkdf_extract,
    hmac_sha256_parts,
};

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

/// Decode a 16-byte SHA-256 digest envelope header produced by [`encode_digest_envelope_wire`].
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

/// Encode a 16-byte HMAC-SHA256 prefix header.
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

/// Decode a 16-byte HMAC-SHA256 prefix header produced by [`encode_hmac_prefix_wire`].
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The two headers round-trip, and a digest envelope whose tag is longer
    /// than SHA-256's 32 bytes is refused (the negative control).
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
