// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session identity (design §4.1): a stable, pid-free [`SessionId`] and a
//! per-launch [`LaunchNonce`] an edge binds to so it fails closed if the target
//! restarts under the same name/pid.

use crate::{ct_eq, fill_random, from_hex, hex};

/// A stable session identity, `s-<20 hex>` (80 bits of entropy), minted once at
/// launch and recorded in `<id>/meta`. Not derived from a pid: pids are reused,
/// and an edge bound to a reused pid would silently target a stranger.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(String);

impl SessionId {
    /// Wrap an existing id string (e.g. read back from `<id>/meta`). Code minting a
    /// fresh identity uses [`SessionId::generate`].
    #[inline]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The id as a string slice (the on-wire / on-disk form).
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Mint a fresh random id, `s-<20 hex>` (80 bits from the OS CSPRNG).
    #[must_use]
    pub fn generate() -> Self {
        let mut b = [0u8; 10];
        fill_random(&mut b);
        // No `format!`: its `fmt::Arguments` path has a panic the verifier cannot
        // lower; plain pushes produce the same `s-<20 hex>` form.
        let mut id = String::from("s-");
        id.push_str(&hex(&b));
        Self(id)
    }

    /// Whether this id has the shape [`SessionId::generate`] mints
    /// ([`is_minted_sid`]).
    #[must_use]
    #[inline]
    pub fn is_minted(&self) -> bool {
        is_minted_sid(&self.0)
    }
}

/// Whether `s` has the shape [`SessionId::generate`] mints: `s-` and 20
/// lowercase hex digits. THE parser of a minted sid: a name someone chose
/// (an edge's `src=`, which `grant` takes as given) is a session only when
/// it reads as one, so a session log never attributes a record to a word
/// that was never a session's.
#[must_use]
#[inline]
pub fn is_minted_sid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 22
        && b[0] == b's'
        && b[1] == b'-'
        && b[2..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

/// A map keyed by `SessionId` is looked up with the id's text, so a parsed wire
/// selector finds its session without minting an owned id. The derived `Hash` and
/// `Eq` read the one `String`, which hashes and compares as its `str`.
impl std::borrow::Borrow<str> for SessionId {
    #[inline]
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A per-launch nonce an edge binds to. It is public anti-spoof state
/// (published as `meta.nonce`), not a secret; the secret is the
/// [`EdgeToken`](crate::EdgeToken). A restarted target mints a fresh nonce, so a
/// stale edge fails closed and is audited, closing the pid-reuse confused-deputy
/// hazard.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LaunchNonce([u8; 16]);

impl LaunchNonce {
    /// Wrap raw nonce bytes (e.g. parsed from `meta.nonce`).
    #[must_use]
    #[inline]
    pub const fn from_bytes(b: [u8; 16]) -> Self {
        Self(b)
    }

    /// Mint a fresh random per-launch nonce from the OS CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut b = [0u8; 16];
        fill_random(&mut b);
        Self(b)
    }

    /// Lowercase-hex (32 chars), the `meta.nonce` form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Parse from the 32-char hex `meta.nonce` form.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        let mut b = [0u8; 16];
        from_hex(s, &mut b)?;
        Some(Self(b))
    }

    /// Constant-time equality (used by the gate so nonce comparison is uniform).
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        ct_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
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
}
