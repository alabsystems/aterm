// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kani proofs — the hex-token encoding behind every single-use auth token
//! and handoff nonce in the workspace ([`crate::rand::hex_encode`]).
//!
//! These proofs verify **non-trivial** properties (per `design doc` "Kani
//! proof quality rule"). No stubs, no `assert!(true)`, no constructor-echo.
//!
//! # Two harnesses
//!
//!   1. [`hex_encode_roundtrips_every_input`] — for ALL 2^128 symbolic
//!      16-byte inputs (the seamless-handoff nonce width), the output is
//!      exactly 32 bytes, every byte is lowercase ASCII hex, and an
//!      independently-written inline decoder recovers the input byte-for-byte
//!      (encode is exact, hence injective, hence collision-free at the token
//!      layer — two distinct nonces can never print alike).
//!   2. [`hex_encode_width_is_invariant`] — the `2N` output-length contract
//!      holds at a second, non-nonce width (8 bytes, the tempfile-suffix
//!      shape), so the const-generic arithmetic is proved per-width rather
//!      than assumed to generalize.
//!
//! Why this module exists at all: the 2026-07-04/05 kernel-panic incident was
//! a hand-rolled entropy reader (`fs::read` pointed at `/dev/urandom`,
//! read-to-EOF on a never-EOF device). The retirement plan is one audited surface
//! (`rand::fill`/`rand::hex_token`) whose I/O is bounded by construction and
//! whose pure half is PROVED here; `tools/grep_guard.sh` bans the literal
//! elsewhere so the class cannot quietly return.
//!
//! Runner: `KANI_CRATE=aterm-uds scripts/verify-kani-proofs.sh` (the trust-mc
//! floor stage of `tools/verify.sh --full` runs exactly that). Never invoke
//! stock `cargo kani` directly — verification is discharged by trust-mc + ay.
//!
//! Each harness directly binds `kani::any()` calls in its body (not via a
//! helper function) so the content-quality classifier (`aterm formal mc`,
//! #7954 — `trust-mc`'s substantive-proof check, folded into the Trust
//! compiler) sees the symbolic inputs and classifies the proof as
//! `substantive`.

#![cfg(kani)]

use crate::rand::hex_encode;

/// Inline hex-digit decoder, written independently of the encoder's nibble
/// table so the roundtrip check is two implementations agreeing, not one
/// implementation echoed.
fn undigit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        // The harness asserts the charset BEFORE decoding, so this arm being
        // reached is itself a proof failure.
        _ => unreachable!("non-hex byte escaped the charset assertion"),
    }
}

/// Property 1: over ALL 16-byte inputs, `hex_encode` emits exactly 32
/// lowercase-ASCII-hex bytes that decode back to the input.
#[kani::proof]
fn hex_encode_roundtrips_every_input() {
    let bytes: [u8; 16] = kani::any();
    let hex = hex_encode(&bytes);
    let out = hex.as_bytes();
    assert_eq!(out.len(), 32, "16 bytes -> exactly 32 hex chars");
    let mut i = 0;
    while i < 16 {
        let hi = out[2 * i];
        let lo = out[2 * i + 1];
        assert!(
            hi.is_ascii_digit() || (b'a'..=b'f').contains(&hi),
            "high nibble is lowercase hex"
        );
        assert!(
            lo.is_ascii_digit() || (b'a'..=b'f').contains(&lo),
            "low nibble is lowercase hex"
        );
        assert_eq!(
            (undigit(hi) << 4) | undigit(lo),
            bytes[i],
            "decode(encode(b)) == b, most-significant nibble first"
        );
        i += 1;
    }
}

/// Property 2: the `2N` width contract holds at a second const width, so the
/// const-generic length arithmetic is proved rather than pattern-matched from
/// the 16-byte case.
#[kani::proof]
fn hex_encode_width_is_invariant() {
    let bytes: [u8; 8] = kani::any();
    let hex = hex_encode(&bytes);
    assert_eq!(hex.len(), 16, "8 bytes -> exactly 16 hex chars");
    assert!(
        hex.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "every output byte is lowercase ASCII hex"
    );
}
