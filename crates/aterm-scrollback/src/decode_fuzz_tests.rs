// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Robustness fuzz for the scrollback DECODE surfaces, as unit tests.
//!
//! Warm- and cold-tier blocks are read back from RAM/disk and may be CORRUPT
//! (truncated writes, crashed sessions, hostile `.dtrm` files). Every byte-level
//! decoder on that read path must NEVER panic on arbitrary input — a panic here
//! crashes the terminal when it tries to scroll back through a corrupt page. The
//! tests drive the decode entry points (`lz4::decompress_size_prepended`,
//! `Line::deserialize`, `deserialize_lines`) with crafted-plus-random bytes and
//! assert only that each call returns (Ok/Err/None/empty).
//!
//! These lived in `tests/fuzz_decode.rs` behind `#![cfg(any(fuzzing, feature =
//! "fuzz"))]`, so the workspace gate — which never enables `fuzz` — compiled an
//! EMPTY binary and ran none of them. As unit tests they need no feature (the
//! decoders are reachable crate-internally) and run on every push, at 10k
//! deterministic inputs per loop. The `fuzz` feature restores the pre-move
//! depth (90k/90k/90k/40k, see [`iters`]); the same seeds make the default run
//! exactly the first 10k inputs of each deep stream. The deep run:
//!
//! ```text
//! targo --unverified test -p aterm-scrollback --features fuzz --lib decode_fuzz_tests
//! ```
//!
//! The warm-tier read path is `WarmBlock::decompress` → `decompress_lz4_bounded`
//! → `stored_line_count` → `deserialize_lines`. `stored_line_count`'s
//! `try_into().expect("count header")` is preceded by a `len() < 4` guard, so it
//! is unreachable; the genuine panic-reachable surfaces are the byte decoders
//! below, which parse fully attacker-controlled lengths and offsets.

use crate::lz4;
use crate::{Line, deserialize_lines};

/// Inputs per fuzz loop: 10k by default (every push runs these), the pre-move
/// `deep` count under the `fuzz` feature.
const fn iters(deep: u32) -> u32 {
    if cfg!(feature = "fuzz") { deep } else { 10_000 }
}

/// Deterministic LCG: same generator shape as `aterm-parser`'s fuzz harness.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    #[inline]
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    /// Random byte vector of length `0..max_len`.
    fn bytes(&mut self, max_len: u32) -> Vec<u8> {
        let len = (self.next_u32() % max_len) as usize;
        (0..len).map(|_| (self.next_u32() & 0xFF) as u8).collect()
    }
}

/// LZ4 block decoder: fully attacker-controlled 4-byte size prefix +
/// arbitrary "compressed" body. Exercises the size-prefix bounds check, the
/// compression-bomb ratio gate, the back-reference offset validation, and the
/// final size-mismatch check. Must always return Ok/Err, never panic, and an
/// Ok is exactly the length the prefix claims.
#[test]
fn fuzz_lz4_decompress_never_panics() {
    let mut rng = Lcg::new(0x9E37_79B9_7F4A_7C15);
    for i in 0..iters(90_000) {
        // Random bytes, biased so the first four bytes (the LE size prefix)
        // sometimes claim huge sizes, sometimes match the body, sometimes are 0.
        let mut input = rng.bytes(96);

        if input.len() >= 4 {
            match i % 6 {
                // Honest-ish prefix: claim a size near the body length.
                0 => {
                    let claim = (input.len() as u32).saturating_sub(4);
                    input[..4].copy_from_slice(&claim.to_le_bytes());
                }
                // Compression-bomb shape: tiny body, enormous claim.
                1 => input[..4].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes()),
                // Over the page cap.
                2 => input[..4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()),
                // Zero-size prefix (empty-decode fast path when body is empty).
                3 => input[..4].copy_from_slice(&0u32.to_le_bytes()),
                // Leave the random prefix as-is for cases 4/5.
                _ => {}
            }
        }

        if let Ok(out) = lz4::decompress_size_prepended(&input) {
            let claim = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
            assert_eq!(out.len(), claim as usize, "{input:02x?}");
        }
    }

    // A handful of explicit edge inputs the random stream rarely lands on.
    for case in [
        &b""[..],
        &b"\x00"[..],
        &b"\x00\x00\x00"[..],     // < 4 bytes (InputTooShort)
        &b"\x00\x00\x00\x00"[..], // zero prefix, empty body (Ok empty)
        &[0x04, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF], // garbage body
        &[0x01, 0x00, 0x00, 0x00], // claims 1 byte, no body
    ] {
        let result = lz4::decompress_size_prepended(case);
        if case.len() < 4 {
            assert_eq!(result, Err(lz4::Lz4Error::InputTooShort), "{case:02x?}");
        } else if let Ok(out) = result {
            let claim = u32::from_le_bytes([case[0], case[1], case[2], case[3]]);
            assert_eq!(out.len(), claim as usize, "{case:02x?}");
        }
    }
}

/// Single-line decoder over arbitrary bytes. Drives the v0/v1/v2/v3 format
/// branches, the content-length / run-count / hyperlink-count / url-len /
/// id-len parsing (all attacker-controlled), and the UTF-8 validation. Biased
/// to start with valid version bytes so the deeper parse branches are reached.
#[test]
fn fuzz_line_deserialize_never_panics() {
    let mut rng = Lcg::new(0xD1B5_4A32_D192_ED03);
    for i in 0..iters(90_000) {
        let mut input = rng.bytes(128);
        if !input.is_empty() {
            // Force the version byte across {0,1,2,3,255} to cover every branch
            // (legacy, v1 attrs, v2 hyperlinks, v3 hyperlinks+ids, unknown).
            input[0] = match i % 5 {
                0 => 0,
                1 => 1,
                2 => 2,
                3 => 3,
                _ => 255,
            };
        }
        let _ = std::hint::black_box(Line::deserialize(&input));
    }
}

/// Block decoder (`deserialize_lines`) over arbitrary bytes — this is the
/// function fed by the warm-tier `stored_line_count` / `logical_suffix` read
/// path. The 4-byte count header plus a stream of variable-length line records
/// are all attacker-controlled. Biased so the first four bytes claim a line
/// count and the body looks like plausible v0..v3 records.
///
/// NOT a duplicate of `line_tests.rs`'s
/// `fuzz_deserialize_lines_never_panics_on_corrupt_disk_data`: that loop feeds
/// UNIFORM bytes, so the first record's version byte is in 0..3 about 4 times in
/// 256 and the version-specific framing helpers are rarely reached. This one
/// writes a version byte every 1-13 bytes so they are.
#[test]
fn fuzz_deserialize_lines_never_panics() {
    let mut rng = Lcg::new(0x2545_F491_4F6C_DD1D);
    for i in 0..iters(90_000) {
        let mut input = rng.bytes(160);

        if input.len() >= 4 {
            // Bias the claimed line count: sometimes absurdly large (stresses
            // pre-allocation clamping), sometimes small/honest.
            let count: u32 = match i % 4 {
                0 => 0xFFFF_FFFF,
                1 => rng.next_u32() % 8,
                2 => 0,
                _ => rng.next_u32(),
            };
            input[..4].copy_from_slice(&count.to_le_bytes());

            // Sprinkle plausible version bytes through the record area so the
            // size-computation helpers (line_size_v0 / line_size_v1v2 /
            // hyperlinks_size_*) take their non-trivial branches.
            let mut p = 4;
            while p < input.len() {
                input[p] = (rng.next_u32() % 4) as u8; // version 0..3
                p += 1 + (rng.next_u32() % 12) as usize;
            }
        }

        let decoded = std::hint::black_box(deserialize_lines(&input));
        // The count clamp: the minimum line record is 5 bytes, so a payload of
        // `len` bytes holds at most `(len - 4) / 5` lines (line_tests' bound).
        let max_possible = input.len().saturating_sub(4) / 5;
        assert!(
            decoded.len() <= max_possible,
            "decoded {} lines from {} bytes (max {max_possible})",
            decoded.len(),
            input.len()
        );
    }

    // Edge inputs the random stream rarely lands on.
    for case in [
        &b""[..],
        &b"\x00\x00\x00"[..],            // < 4 bytes -> empty
        &b"\x00\x00\x00\x00"[..],        // count 0 -> empty
        &[0xFF, 0xFF, 0xFF, 0xFF],       // huge count, no records
        &[0x01, 0x00, 0x00, 0x00, 0x00], // claims 1 line, truncated record
    ] {
        let _ = std::hint::black_box(deserialize_lines(case));
    }
}

/// Cross-feed: random bytes that survive LZ4 decode are handed straight to the
/// block decoder, mirroring the real warm-tier read path
/// (`decompress` -> `decompress_lz4_bounded` -> `deserialize_lines`). Catches
/// any panic that only manifests when a decompressed buffer of an unusual
/// length reaches the line parser.
#[test]
fn fuzz_lz4_then_deserialize_round_path_never_panics() {
    let mut rng = Lcg::new(0xA0761D6478BD642F);
    for _ in 0..iters(40_000) {
        // Build a genuine LZ4 frame from random plaintext, then decode it and
        // feed the result to deserialize_lines (as the warm tier does).
        let plaintext = rng.bytes(192);
        if let Ok(frame) = lz4::compress_prepend_size(&plaintext)
            && let Ok(buf) = lz4::decompress_size_prepended(&frame)
        {
            let _ = std::hint::black_box(deserialize_lines(&buf));
        }
        // Also feed raw random bytes (a corrupt page that still "decompressed").
        let raw = rng.bytes(192);
        let _ = std::hint::black_box(deserialize_lines(&raw));
    }
}
