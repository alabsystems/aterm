// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Differential tests against the crates `aterm-digest` replaces.
//!
//! `sha2` and `hmac` are **dev-dependencies only** -- they never enter the
//! shipped graph, exactly as `crates/aterm-grapheme` keeps
//! `unicode-segmentation`/`unicode-width` purely to cross-validate its inline
//! tables. Here they are the oracle: for every input we generate, the
//! first-party bytes must equal the reference bytes exactly.
//!
//! Published FIPS 180-4 and RFC 4231 vectors live beside the implementation in
//! `src/sha256.rs` and `src/hmac_sha256.rs`. This file covers the space between the
//! vectors -- every message length from 0 to 200, several large ones, every
//! key length across the block boundary, and arbitrary streaming splits.

use aterm_digest::{HmacSha256, Sha256};
use hmac::{Hmac, Mac};
use sha2::Digest as _;

type OracleHmac = Hmac<sha2::Sha256>;

/// A deterministic 64-bit LCG (Knuth's MMIX constants).
///
/// Deliberately not `rand`: this whole crate exists to stop importing packages
/// for one job, and a fixed seed means a failure reproduces exactly rather than
/// once every few thousand CI runs.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // Return the high bits: the low bits of an LCG have short periods.
        self.0 >> 11
    }

    fn next_byte(&mut self) -> u8 {
        (self.next_u64() & 0xff) as u8
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_byte()).collect()
    }

    /// A value in `1..=max`, for chunk sizes.
    fn chunk_len(&mut self, max: usize) -> usize {
        assert!(max > 0);
        (self.next_u64() as usize % max) + 1
    }
}

fn oracle_sha256(bytes: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(bytes).into()
}

fn oracle_hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut m = OracleHmac::new_from_slice(key).expect("hmac accepts any key length");
    m.update(data);
    m.finalize().into_bytes().into()
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

#[test]
fn sha256_matches_sha2_for_every_length_up_to_200() {
    let mut rng = Lcg::new(0x005e_a11e_d0dd_beef);
    for len in 0..=200usize {
        let msg = rng.bytes(len);
        assert_eq!(
            Sha256::digest(&msg),
            oracle_sha256(&msg),
            "sha256 mismatch at length {len}"
        );
    }
}

#[test]
fn sha256_matches_sha2_for_large_inputs() {
    let mut rng = Lcg::new(0x00c0_ffee_cafe_f00d);
    // Sizes chosen around the block and buffer boundaries the padding logic
    // cares about, plus a couple of genuinely large buffers.
    for len in [
        255usize, 256, 511, 512, 1_000, 4_096, 8_192, 65_535, 65_536, 100_000,
    ] {
        let msg = rng.bytes(len);
        assert_eq!(
            Sha256::digest(&msg),
            oracle_sha256(&msg),
            "sha256 mismatch at length {len}"
        );
    }
}

#[test]
fn sha256_matches_sha2_when_fed_in_arbitrary_chunks() {
    let mut rng = Lcg::new(0x0123_4567_89ab_cdef);
    for len in 0..=200usize {
        let msg = rng.bytes(len);
        let expected = oracle_sha256(&msg);

        // Ten independent random splits of the SAME message must all reach the
        // same digest as the one-shot call. This is what catches a broken
        // partial-block buffer, which published vectors alone never would --
        // they are all one-shot.
        for split in 0..10 {
            let mut ours = Sha256::new();
            let mut theirs = sha2::Sha256::new();
            let mut rest: &[u8] = &msg;
            while !rest.is_empty() {
                let take = rng.chunk_len(rest.len().min(70));
                ours.update(&rest[..take]);
                theirs.update(&rest[..take]);
                rest = &rest[take..];
            }
            assert_eq!(ours.finalize(), expected, "length {len}, split {split}");
            let theirs: [u8; 32] = theirs.finalize().into();
            assert_eq!(theirs, expected, "oracle disagreed: length {len}");
        }
    }
}

#[test]
fn sha256_streaming_a_large_message_matches_one_shot() {
    let mut rng = Lcg::new(0xdead_beef_1234_5678);
    let msg = rng.bytes(200_000);
    let expected = oracle_sha256(&msg);

    let mut h = Sha256::new();
    let mut rest: &[u8] = &msg;
    while !rest.is_empty() {
        // Chunks that straddle block boundaries in every possible alignment.
        let take = rng.chunk_len(rest.len().min(4_097));
        h.update(&rest[..take]);
        rest = &rest[take..];
    }
    assert_eq!(h.finalize(), expected);
}

// ---------------------------------------------------------------------------
// HMAC-SHA256
// ---------------------------------------------------------------------------

#[test]
fn hmac_matches_the_hmac_crate_for_every_message_length_up_to_200() {
    let mut rng = Lcg::new(0xa5a5_5a5a_a5a5_5a5a);
    let key = rng.bytes(32);
    for len in 0..=200usize {
        let msg = rng.bytes(len);
        let mut ours = HmacSha256::new_from_slice(&key).expect("any key length");
        ours.update(&msg);
        assert_eq!(
            ours.finalize(),
            oracle_hmac(&key, &msg),
            "hmac mismatch at message length {len}"
        );
    }
}

#[test]
fn hmac_matches_the_hmac_crate_for_every_key_length_up_to_200() {
    let mut rng = Lcg::new(0x1357_9bdf_2468_ace0);
    let msg = rng.bytes(97);
    // 0..=200 spans the 64-byte block boundary in both directions: keys at 64
    // are used verbatim, keys at 65 and above are hashed down first.
    for len in 0..=200usize {
        let key = rng.bytes(len);
        let mut ours = HmacSha256::new_from_slice(&key).expect("any key length");
        ours.update(&msg);
        assert_eq!(
            ours.finalize(),
            oracle_hmac(&key, &msg),
            "hmac mismatch at key length {len}"
        );
    }
}

#[test]
fn hmac_matches_the_hmac_crate_for_large_keys_and_messages() {
    let mut rng = Lcg::new(0x2b2b_2b2b_7f7f_7f7f);
    for (klen, mlen) in [
        (0usize, 0usize),
        (1, 1),
        (63, 63),
        (64, 64),
        (65, 65),
        (128, 4_096),
        (1_000, 1),
        (4_096, 65_536),
    ] {
        let key = rng.bytes(klen);
        let msg = rng.bytes(mlen);
        let mut ours = HmacSha256::new_from_slice(&key).expect("any key length");
        ours.update(&msg);
        assert_eq!(
            ours.finalize(),
            oracle_hmac(&key, &msg),
            "hmac mismatch at key {klen} / message {mlen}"
        );
    }
}

#[test]
fn hmac_matches_the_hmac_crate_when_fed_in_arbitrary_chunks() {
    let mut rng = Lcg::new(0x9e37_79b9_7f4a_7c15);
    for len in 0..=200usize {
        let key = rng.bytes(len % 97);
        let msg = rng.bytes(len);
        let expected = oracle_hmac(&key, &msg);

        for split in 0..5 {
            let mut ours = HmacSha256::new_from_slice(&key).expect("any key length");
            let mut rest: &[u8] = &msg;
            while !rest.is_empty() {
                let take = rng.chunk_len(rest.len().min(33));
                ours.update(&rest[..take]);
                rest = &rest[take..];
            }
            assert_eq!(ours.finalize(), expected, "length {len}, split {split}");
        }
    }
}

#[test]
fn hmac_verify_slice_agrees_with_the_oracle_tag() {
    let mut rng = Lcg::new(0x4242_4242_dead_c0de);
    for len in 0..=200usize {
        let key = rng.bytes(len % 71);
        let msg = rng.bytes(len);
        let tag = oracle_hmac(&key, &msg);

        let mut ok = HmacSha256::new_from_slice(&key).expect("any key length");
        ok.update(&msg);
        assert!(ok.verify_slice(&tag).is_ok(), "length {len}");

        // Corrupt one pseudorandom bit of the oracle's tag; it must not verify.
        let mut bad = tag;
        let idx = (rng.next_u64() as usize) % bad.len();
        bad[idx] ^= 1 << (rng.next_u64() % 8);
        let mut nope = HmacSha256::new_from_slice(&key).expect("any key length");
        nope.update(&msg);
        assert!(nope.verify_slice(&bad).is_err(), "length {len}");
    }
}
