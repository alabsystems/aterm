// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A master is a base64 Ed25519 public key (32 bytes, 44 chars with one `=`); this
/// catches a truncated paste.
#[test]
fn the_master_is_a_well_formed_base64_ed25519_key() {
    for k in PAPER_MASTER_PUBKEYS {
        assert_eq!(k.len(), 44, "not a 44-char base64 Ed25519 key: {k}");
        assert!(k.ends_with('='), "missing base64 padding: {k}");
        assert!(
            k.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
            "non-base64 character: {k}"
        );
    }
}

/// The Windows anchor is the leaf's subject CN as `signtool verify /v` prints it, never a
/// DN and never padded: `targo --unverified winsign` compares it byte for byte.
#[test]
fn the_windows_anchor_is_a_bare_common_name_or_empty() {
    let a = WINDOWS_SIGNING_PUBLISHER;
    assert_eq!(a, a.trim(), "no leading/trailing whitespace");
    assert!(!a.contains('\n') && !a.contains('\r'));
    assert!(
        !a.starts_with("CN=") && !a.contains(", O=") && !a.contains(",O="),
        "commit the CN as signtool prints it, not the distinguished name"
    );
}

/// Only an empty slice unpins the master: an empty member would read as armed to
/// `roster_tier_armed()` while authorizing nobody.
#[test]
fn the_master_keyset_has_no_empty_members_and_no_duplicates() {
    for (i, key) in PAPER_MASTER_PUBKEYS.iter().enumerate() {
        assert!(
            !key.is_empty(),
            "PAPER_MASTER_PUBKEYS[{i}] is empty — use an empty SLICE to unpin, never \
             an empty member"
        );
        assert!(
            !PAPER_MASTER_PUBKEYS[..i].contains(key),
            "duplicate master key: {key}"
        );
    }
    assert!(
        PAPER_MASTER_PUBKEYS.len() <= 2,
        "master keyset has {} members; a rotation window holds at most two",
        PAPER_MASTER_PUBKEYS.len()
    );
}
