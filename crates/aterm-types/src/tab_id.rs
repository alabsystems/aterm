// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A tab's ID: `t` and 10 lower-case Crockford base32 characters (50 random bits), such as
//! `tqn5r6e0148`, shown as its first 5 (`tqn5r`). aterm mints it when the tab opens and the tab
//! keeps it until it closes, across a relaunch, an update and a cold restore. The minter draws
//! again while the ID is one the fleet already knows ([`TabId::mint`]).
//!
//! A tab ID is only this form. A tab opened before it took `s-` and 20 lower-case hex digits
//! (`s-1a2b3c4d5e6f70819203`), which [`TabId::parse`] refuses: a cold restore gives such a tab a
//! minted ID and records the `s-` one as the ID it had. A shell still running in such a tab holds
//! the `s-` text in its environment, which nothing can rewrite, so the readers of a request from
//! a still-running shell (a requester's `sid=`, a local `@<sid>` selector, a keeper's link) also
//! take it ([`is_legacy`]). Requester metadata can carry that exact legacy ID over the fleet;
//! it does not become a canonical tab ID, and nothing mints it.
//!
//! [`TabId::parse`] reads the canonical form. Its text grammar (the alphabet, lengths and
//! [`is_tab_id`]) is [`alab_policy::tab_id`]'s, shared with the reply readers; this module builds
//! the ID on it. Local request and requester-metadata readers use
//! [`crate::control_request::Sid`] or [`crate::control_verbs::is_minted_sid`] to also accept the
//! exact legacy form. Both grammars are strict: lower case only, exact lengths, and none of
//! Crockford's aliases (`i`, `l`, `o`, `u`) in the `t` form. One spelling per ID also keeps two
//! IDs from folding onto one file name on a case-insensitive volume.

use core::fmt;

use alab_policy::tab_id::digit;
/// The grammar of a tab ID's text ([`alab_policy::tab_id`]).
pub use alab_policy::tab_id::{ALPHABET, BITS, LEN, SHORT_LEN, is_tab_id};

/// The bytes of a legacy tab's ID ([`is_legacy`]): `s-` and 20 hex digits.
pub const LEGACY_LEN: usize = 22;

const MASK: u64 = (1 << BITS) - 1;

/// The draws [`TabId::mint`] makes before it gives up on finding an ID nobody knows. Each draw
/// clashes with odds of (known IDs) / 2^50, so a mint that needs more than one already means the
/// known set is wrong; the bound only keeps a `taken` that answers yes to everything from
/// spinning forever.
pub const MINT_DRAWS: u32 = 64;

/// A tab's ID, held as its 11 ASCII bytes. Hash,
/// equality and order are those of its text, so a map keyed by `TabId` (or by a type that
/// borrows as one) is looked up with a `&str`.
#[derive(Clone, Copy)]
pub struct TabId {
    bytes: [u8; LEN],
}

const fn is_lower_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

impl TabId {
    /// `s` as a tab ID, or `None` unless it is exactly `t` and 10 characters of [`ALPHABET`].
    #[must_use]
    pub const fn parse(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != LEN || b[0] != b't' {
            return None;
        }
        let mut bytes = [0u8; LEN];
        let mut i = 0;
        while i < LEN {
            if i > 0 && digit(b[i]).is_none() {
                return None;
            }
            bytes[i] = b[i];
            i += 1;
        }
        Some(Self { bytes })
    }

    /// The ID whose 50 bits are the low 50 of `v` (the higher bits are dropped): the inverse of
    /// [`TabId::bits`]. Minting draws `v` at random; a fixture or a simulation names an ID by
    /// its number.
    #[must_use]
    pub const fn from_bits(v: u64) -> Self {
        let v = v & MASK;
        let mut bytes = [0u8; LEN];
        bytes[0] = b't';
        let mut i = 1;
        while i < LEN {
            let shift = 5 * (LEN - 1 - i);
            bytes[i] = ALPHABET[((v >> shift) & 31) as usize];
            i += 1;
        }
        Self { bytes }
    }

    /// The ID's 50 bits: the inverse of [`TabId::from_bits`].
    #[must_use]
    pub const fn bits(self) -> u64 {
        let mut v = 0u64;
        let mut i = 1;
        while i < LEN {
            // Every byte passed `digit` when the ID was made.
            let d = match digit(self.bytes[i]) {
                Some(d) => d,
                None => 0,
            };
            v = (v << 5) | d as u64;
            i += 1;
        }
        v
    }

    /// Mint an ID: draw 50 bits from `draw` until the ID they spell is not `taken` (the IDs
    /// this Mac and the fleet already know), at most [`MINT_DRAWS`] times. After that many
    /// clashes the known set is wrong, not the draw, and the last draw is returned. It is always
    /// the `t` form.
    #[must_use]
    pub fn mint(mut draw: impl FnMut() -> u64, mut taken: impl FnMut(&Self) -> bool) -> Self {
        let mut id = Self::from_bits(draw());
        let mut tries = 1;
        while tries < MINT_DRAWS && taken(&id) {
            id = Self::from_bits(draw());
            tries += 1;
        }
        id
    }

    /// The ID as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Every byte is ASCII by construction, so this never falls back.
        core::str::from_utf8(&self.bytes).unwrap_or_default()
    }

    /// The ID as it is shown: its first [`SHORT_LEN`] characters (`tqn5r`).
    #[must_use]
    pub fn short(&self) -> &str {
        self.as_str().get(..SHORT_LEN).unwrap_or_default()
    }
}

/// Whether `s` is a legacy tab's ID: `s-` and 20 lower-case hex digits, which a tab opened
/// before the `t` form still holds in its shell's environment until aterm restores or closes it.
/// Only a reader of a request from this Mac's own shells takes it, as the ID of a session this
/// Mac holds; [`TabId::parse`] refuses it and nothing mints it.
#[must_use]
pub const fn is_legacy(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != LEGACY_LEN || b[0] != b's' || b[1] != b'-' {
        return false;
    }
    let mut i = 2;
    while i < LEGACY_LEN {
        if !is_lower_hex(b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// `s` as it is shown: a tab ID's short form ([`TabId::short`]), and any other word (`ctl`,
/// `human`, a name) unchanged.
#[must_use]
pub fn short(s: &str) -> &str {
    match TabId::parse(s) {
        Some(_) => s.get(..SHORT_LEN).unwrap_or(s),
        None => s,
    }
}

impl PartialEq for TabId {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for TabId {}

impl PartialOrd for TabId {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TabId {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl core::hash::Hash for TabId {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl core::borrow::Borrow<str> for TabId {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for TabId {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl core::str::FromStr for TabId {
    type Err = NotATabId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(NotATabId)
    }
}

impl PartialEq<str> for TabId {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for TabId {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// A word that is not a tab ID ([`TabId::from_str`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotATabId;

impl fmt::Display for NotATabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a tab ID (`t` and 10 of 0-9 a-z without i l o u)")
    }
}

impl std::error::Error for NotATabId {}

#[cfg(test)]
#[path = "tab_id_tests.rs"]
mod tests;
