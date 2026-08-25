// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cryptographic digests for aterm: SHA-256 and HMAC-SHA256.
//!
//! This crate is a first-party, dependency-free replacement for the `sha2` and
//! `hmac` crates. Measured with `cargo forge survey --cell mac-arm`, dropping
//! them removed **8 packages / 49,474 lines** of third-party source from the
//! shipped graph (`sha2`, `hmac`, `digest`, `typenum`, `generic-array`,
//! `crypto-common`, `block-buffer`, `cpufeatures`) -- `digest` and its
//! const-generic tower dominate at 45,734 lines, of which `typenum` alone is
//! 40,601 -- to provide a surface aterm uses four methods of:
//! [`Sha256::new`], [`Sha256::digest`], `update` and `finalize`, plus HMAC's
//! `new_from_slice` / `update` / `finalize` / `verify_slice`.
//!
//! `subtle` is **not** removed by this crate and is not claimed above: `rustls`
//! depends on it directly (and again through `ureq` ->
//! `rustls-platform-verifier`), so it stays in the graph regardless. What
//! [`ct_eq`] replaces is aterm's *use* of it, not the package.
//!
//! Trading 49k lines of const-generic type arithmetic for ~250 lines of
//! transcribed FIPS 180-4 is the right side of the maintenance ledger: when
//! this breaks we can read it, and every release-signature check in the tree
//! runs through code we can audit in one sitting.
//!
//! # Exported types
//!
//! - [`Sha256`] -- FIPS 180-4 SHA-256, streaming or one-shot
//! - [`HmacSha256`] -- RFC 2104 HMAC keyed with SHA-256
//! - [`ct_eq`] -- constant-time byte-slice comparison (in-house, no `subtle`)
//! - [`InvalidLength`], [`MacError`] -- API-compatible error types
//!
//! # What this crate is not
//!
//! There is no SIMD, no SHA-NI, no assembly and no `unsafe`. Correctness and
//! auditability are the goals; aterm hashes release manifests and capability
//! tokens, not bulk traffic. It is not gratuitously slow either -- input is
//! consumed in 64-byte blocks straight out of the caller's slice, with a single
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

// Named `hmac_sha256`, not `hmac`: the dev-dependency oracle is the `hmac`
// crate, and a private module of the same name at the crate root makes a bare
// `hmac::` path ambiguous in edition 2024.
mod hmac_sha256;
mod sha256;

pub use hmac_sha256::{HmacSha256, InvalidLength, MacError};
pub use sha256::Sha256;

/// Compares two byte slices in constant time with respect to their *contents*.
///
/// Returns `true` when the slices have the same length and the same bytes.
///
/// # Why the obvious loop is wrong
///
/// The natural implementation --
///
/// ```text
/// for (x, y) in a.iter().zip(b) {
///     if x != y {
///         return false;      // WRONG
///     }
///}
/// ```
///
/// -- leaks the position of the first differing byte through how long it runs.
/// An attacker who can submit candidate tags and time the answer recovers a
/// 32-byte HMAC one byte at a time, in 256 * 32 guesses instead of 2^256. This
/// is the property `subtle`'s `ConstantTimeEq` provided, and it is why
/// [`HmacSha256::verify_slice`] must not be written with `==`.
///
/// So: fold every byte pair into one accumulator with `|=`, which has no
/// branch and no early exit, and run the whole length every time. Length is
/// *not* secret (it is visible in the wire format), so an unequal length may
/// short-circuit; the bytes may not.
///
/// The final [`std::hint::black_box`] is load-bearing. Without it the optimiser
/// is free to notice that `diff` can only grow and bail out of the loop as soon
/// as it is non-zero, reintroducing exactly the branch this function exists to
/// avoid.
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

/// Decodes an even-length lowercase hex string, for test vectors only.
#[cfg(test)]
pub(crate) fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "hex literal must be even length");
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let nibble = |c: u8| match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("bad hex digit {c:?}"),
            };
            (nibble(pair[0]) << 4) | nibble(pair[1])
        })
        .collect()
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
    fn ct_eq_stays_branch_free() {
        // Value tests cannot see this property: an early-return `ct_eq` passes
        // every assertion above (mutation-checked -- inserting
        // `if x != y { return false; }` into the fold leaves the whole suite
        // green) while leaking the first differing byte through timing. The
        // property lives in the SHAPE of the code, so pin the source, the way
        // `crates/xtask/src/gate.rs` pins the pre-push hook's markers.
        let src = include_str!("lib.rs");
        let body = src
            .split("pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {")
            .nth(1)
            .expect("ct_eq is defined here");
        let fold = body.split("for (x, y)").nth(1).expect("the fold loop");
        let fold = &fold[..fold.find('}').expect("the loop closes")];
        assert!(
            !fold.contains("return"),
            "ct_eq's fold must not early-exit: {fold}"
        );
        assert!(
            !fold.contains("if "),
            "ct_eq's fold must stay branch-free: {fold}"
        );
        assert!(
            body.contains("black_box"),
            "ct_eq lost its optimiser barrier"
        );
    }

    #[test]
    fn hex_helper_round_trips() {
        assert_eq!(hex(&[0x00, 0x0f, 0xf0, 0xff]), "000ff0ff");
        assert_eq!(unhex("000ff0ff"), vec![0x00, 0x0f, 0xf0, 0xff]);
        assert_eq!(unhex(&hex(&Sha256::digest(b"x"))), Sha256::digest(b"x"));
    }
}
