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
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
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
}
