// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kani proofs for [`crate::rand::hex_encode`], the encoding behind every
//! single-use auth token and handoff nonce in the workspace.
//!
//! 1. [`hex_encode_roundtrips_every_input`]: for every 16-byte input (the
//!    seamless-handoff nonce width) the output is exactly 32 lowercase ASCII
//!    hex bytes, and an independent decoder recovers the input, so encoding is
//!    injective and two distinct nonces never print alike.
//! 2. [`hex_encode_width_is_invariant`]: the `2N` length contract at a second
//!    width (8 bytes, the tempfile-suffix shape), so the const-generic
//!    arithmetic is proved per width rather than assumed.
//!
//! Runner: `KANI_CRATE=aterm-uds scripts/verify-kani-proofs.sh` (run by
//! `tools/verify.sh --full`), not stock `cargo kani`.
//!
//! Each harness binds its `kani::any()` calls directly in its body, not via a
//! helper, so trust-mc's proof-quality check classifies it as substantive.

#![cfg(kani)]

use crate::rand::hex_encode;

/// Inline hex-digit decoder, written independently of the encoder's nibble
/// table so the roundtrip check is two implementations agreeing, not one
/// implementation echoed.
fn undigit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        // The harness asserts the charset before decoding, so this arm being
        // reached is itself a proof failure.
        _ => unreachable!("non-hex byte escaped the charset assertion"),
    }
}

/// Property 1: over all 16-byte inputs, `hex_encode` emits exactly 32
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
/// const-generic length arithmetic is proved rather than inferred from the
/// 16-byte case.
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
