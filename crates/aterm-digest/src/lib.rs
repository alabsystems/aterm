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
