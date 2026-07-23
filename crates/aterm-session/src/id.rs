// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session identity (design §4.1): a stable, pid-free [`SessionId`] and a
//! per-launch [`LaunchNonce`] an edge binds to so it fails closed if the target
//! restarts under the same name/pid.

use crate::{ct_eq, fill_random, from_hex, hex};

/// A stable session identity. Default form `s-<20 hex>` (80 bits of entropy),
/// minted ONCE at launch and recorded in `<id>/meta`. **Not** derived from a pid:
/// pids reuse, and an edge bound to a reused pid would silently target a stranger.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(String);

impl SessionId {
    /// Wrap an existing id string (e.g. read back from `<id>/meta`). Code minting a
    /// fresh identity uses [`SessionId::generate`].
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The id as a string slice (the on-wire / on-disk form).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Mint a fresh random id, `s-<20 hex>` (80 bits from the OS CSPRNG).
    #[must_use]
    pub fn generate() -> Self {
        let mut b = [0u8; 10];
        fill_random(&mut b);
        // Concatenate without `format!`: the `fmt::Arguments` machinery has an
        // unmodeled panic path the verifier cannot lower; plain `String`
        // pushes are verifiable and produce the identical `s-<20 hex>` form.
        let mut id = String::from("s-");
        id.push_str(&hex(&b));
        Self(id)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A per-LAUNCH nonce an edge binds to (design §4.1). It is **public** anti-spoof
/// state (published as `meta.nonce`), not a secret: the unforgeable secret is the
/// [`EdgeToken`](crate::EdgeToken). Its job is to invalidate an edge when the target
/// restarts — a fresh launch mints a fresh nonce, so a stale edge fails closed and
/// is audited, closing the pid-reuse confused-deputy hazard.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LaunchNonce([u8; 16]);

impl LaunchNonce {
    /// Wrap raw nonce bytes (e.g. parsed from `meta.nonce`).
    #[must_use]
    pub fn from_bytes(b: [u8; 16]) -> Self {
        Self(b)
    }

    /// The raw nonce bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
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
    fn session_id_generate_is_prefixed_and_distinct() {
        let a = SessionId::generate();
        let b = SessionId::generate();
        assert!(a.as_str().starts_with("s-"), "id must be s-prefixed: {a}");
        assert_eq!(a.as_str().len(), 2 + 20, "s- plus 20 hex chars (80 bits)");
        assert_ne!(a, b, "two fresh ids must differ (80 bits of entropy)");
    }

    #[test]
    fn launch_nonce_hex_roundtrips_and_ct_eq() {
        let n = LaunchNonce::generate();
        let round = LaunchNonce::from_hex(&n.to_hex()).expect("hex roundtrip");
        assert_eq!(n, round);
        assert!(n.ct_eq(&round));
        // A different nonce must not compare equal.
        let other = LaunchNonce::generate();
        assert!(!n.ct_eq(&other));
        // Malformed hex is rejected, never silently zeroed.
        assert!(LaunchNonce::from_hex("xyz").is_none());
        assert!(LaunchNonce::from_hex(&"a".repeat(31)).is_none());
    }
}
