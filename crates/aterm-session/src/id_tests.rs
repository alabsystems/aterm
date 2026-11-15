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
    assert!(is_minted_sid("tqn5r6e0148"), "the `t` form reads too");
    assert_eq!(
        SessionId::parse("tqn5r6e0148").map(|id| id.to_string()),
        Some("tqn5r6e0148".to_owned())
    );
    assert_eq!(
        SessionId::parse("s-0123456789abcdef0123").map(|id| id.to_string()),
        Some("s-0123456789abcdef0123".to_owned())
    );
    for chosen in ["s-1", "s-boss", "tqn5r6e014", "Tqn5r6e0148", "worker"] {
        assert_eq!(SessionId::parse(chosen), None, "NEGATIVE CONTROL: {chosen}");
    }
    // The two crates' readers give one answer.
    for s in [
        "s-0123456789abcdef0123",
        "tqn5r6e0148",
        "s-1",
        "tqn5r6e014u",
        "",
    ] {
        assert_eq!(
            is_minted_sid(s),
            aterm_types::control_verbs::is_minted_sid(s),
            "{s}"
        );
    }
}

#[test]
fn session_id_generate_mints_the_t_form() {
    use std::borrow::Borrow;

    let a = SessionId::generate();
    assert!(
        a.as_str().starts_with('t'),
        "a minted id is the t form: {a}"
    );
    assert_eq!(
        a.as_str().len(),
        aterm_types::tab_id::LEN,
        "t plus 10 Crockford base32 characters (50 bits)"
    );
    assert!(a.tab().is_some(), "every minted ID is a tab ID: {a}");
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

#[test]
fn mint_draws_again_while_taken_knows_the_id() {
    assert!(
        SessionId::mint(|_| false).tab().is_some(),
        "a mint nothing refuses is a tab id"
    );
    // `taken` names every ID it is asked about until the fourth draw: the mint refuses the first
    // three and returns the fourth, which is unlike each refused one.
    let mut refused = Vec::new();
    let sid = SessionId::mint(|id| {
        if refused.len() < 3 {
            refused.push(*id);
            true
        } else {
            false
        }
    });
    assert_eq!(refused.len(), 3);
    let kept = sid.tab().expect("a tab id");
    assert!(
        refused.iter().all(|r| r.as_str() != kept.as_str()),
        "NEGATIVE CONTROL: a taken ID is never the one minted: {sid} {refused:?}"
    );
    // A wrapped name that is no tab ID has none.
    assert_eq!(
        SessionId::new("human").tab().map(|t| t.as_str().to_owned()),
        None
    );
}
