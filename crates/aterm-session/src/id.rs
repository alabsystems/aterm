// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session identity (design §4.1): a stable, pid-free [`SessionId`] and a
//! per-launch [`LaunchNonce`] an edge binds to so it fails closed if the target
//! restarts under the same name/pid.

use aterm_types::tab_id::TabId;

use crate::{fill_random, from_hex, hex};

/// A session's stable identity: its tab's ID ([`TabId`], `t` and 10 lower-case Crockford base32
/// characters, 50 bits), minted when the tab opens and kept until it closes, across a relaunch,
/// an update and a cold restore. A tab opened before that form keeps its earlier ID (`s-` and 20
/// lower-case hex digits) until it closes, so every reader takes both; only the `t` form is
/// minted. Recorded in `<id>/meta`. Not derived from a pid: pids are reused, and an edge bound
/// to a reused pid would silently target a stranger.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(String);

impl SessionId {
    /// Wrap an existing id string (e.g. read back from `<id>/meta`). Code minting a
    /// fresh identity uses [`SessionId::mint`].
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

    /// Mint a fresh tab ID from the OS CSPRNG, drawing again while `taken` knows it (the IDs
    /// this Mac and the fleet already hold: [`TabId::mint`]). Always the `t` form.
    #[must_use]
    pub fn mint(taken: impl FnMut(&TabId) -> bool) -> Self {
        let id = TabId::mint(
            || {
                let mut b = [0u8; 8];
                fill_random(&mut b);
                u64::from_le_bytes(b)
            },
            taken,
        );
        Self(id.as_str().to_owned())
    }

    /// A fresh random tab ID that consults no known set ([`SessionId::mint`] with nothing
    /// taken): for a caller with no view of the IDs this Mac and the fleet hold. A tab's shell
    /// is minted by the caller that knows them.
    #[must_use]
    pub fn generate() -> Self {
        Self::mint(|_| false)
    }

    /// The tab ID this names, or `None` for a name that is no tab ID (one [`SessionId::new`]
    /// wrapped as given).
    #[must_use]
    #[inline]
    pub fn tab(&self) -> Option<TabId> {
        TabId::parse(&self.0)
    }

    /// `s` as a session's ID, or `None` unless it is a tab ID or a legacy tab's ID ([`is_minted_sid`]).
    /// The one way to read an ID back from text that must name a session.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        is_minted_sid(s).then(|| Self(s.to_owned()))
    }

    /// Whether this id is a tab ID [`SessionId::mint`] minted, or a legacy tab's `s-` ID
    /// ([`is_minted_sid`]).
    #[must_use]
    #[inline]
    pub fn is_minted(&self) -> bool {
        is_minted_sid(&self.0)
    }
}

/// Whether `s` is a session ID aterm minted: a tab ID ([`aterm_types::tab_id::is_tab_id`]), the
/// `t` and 10 Crockford base32 characters [`SessionId::mint`] mints, or a legacy tab's `s-` and 20
/// lowercase hex digits ([`aterm_types::tab_id::is_legacy`]), which a shell opened before the `t`
/// form still holds until aterm restores or closes its tab. THE parser of a minted sid: a name someone chose (an edge's
/// `src=`, which `grant` takes as given) is a session only when it reads as one, so a session log
/// never attributes a record to a word that was never a session's.
#[must_use]
#[inline]
pub fn is_minted_sid(s: &str) -> bool {
    aterm_types::tab_id::is_tab_id(s) || aterm_types::tab_id::is_legacy(s)
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
        aterm_digest::ct_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
#[path = "id_tests.rs"]
mod tests;
