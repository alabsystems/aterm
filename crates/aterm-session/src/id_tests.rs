// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_minted_sid_parses_and_a_chosen_name_does_not() {
    for _ in 0..64 {
        assert!(SessionId::generate().is_minted());
    }
    for chosen in [
        "harness",
        "ctl",
        "human",
        "s-1",
        "s-boss",
        "s-0123456789ABCDEF0123",
        "s-0123456789abcdef01234",
        "x-0123456789abcdef0123",
        "",
    ] {
        assert!(!is_minted_sid(chosen), "{chosen}");
    }
    assert!(is_minted_sid("s-0123456789abcdef0123"));
}

#[test]
fn session_id_generate_is_prefixed_and_minted() {
    use std::borrow::Borrow;

    let a = SessionId::generate();
    assert!(a.as_str().starts_with("s-"), "id must be s-prefixed: {a}");
    assert_eq!(a.as_str().len(), 2 + 20, "s- plus 20 hex chars (80 bits)");
    assert!(
        a.is_minted(),
        "generated id must satisfy is_minted_sid: {a}"
    );
    let borrowed: &str = a.borrow();
    assert_eq!(borrowed, a.as_str());
    assert_eq!(a.to_string(), a.as_str());
    let chosen = SessionId::new("human");
    assert!(!chosen.is_minted());
    assert_ne!(a, chosen);
}

#[test]
fn launch_nonce_hex_roundtrips_and_ct_eq() {
    let n = LaunchNonce::generate();
    let hex = n.to_hex();
    assert_eq!(hex.len(), 32);
    assert!(
        hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "to_hex must emit 32 lowercase hex digits: {hex}"
    );
    let round = LaunchNonce::from_hex(&hex).expect("hex roundtrip");
    assert_eq!(n, round);
    assert!(n.ct_eq(&round));
    let mut flipped = n.0;
    flipped[0] ^= 0xff;
    let other = LaunchNonce::from_bytes(flipped);
    assert!(!n.ct_eq(&other));
    // Malformed hex is rejected, never silently zeroed; uppercase hex decodes canonically.
    assert!(LaunchNonce::from_hex("xyz").is_none());
    assert!(LaunchNonce::from_hex(&"a".repeat(31)).is_none());
    assert!(LaunchNonce::from_hex(&"a".repeat(33)).is_none());
    assert!(LaunchNonce::from_hex(&"g".repeat(32)).is_none());
    assert_eq!(
        LaunchNonce::from_hex(&"A".repeat(32)),
        Some(LaunchNonce::from_bytes([0xaa; 16]))
    );
}
