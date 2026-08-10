// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE trust anchors. Every one of them, in one file.
//!
//! Changing a value here is a reviewed commit. There is no other way to change what
//! a build trusts — no environment variable, no per-machine file, no build flag.
//!
//! # Why these are constants and not `option_env!`
//!
//! These were compiled in from the build environment (`ATERM_UPDATE_PUBKEY`,
//! `ATERM_PKG_ROOTKEY`, `ATERM_EXPECTED_TEAM_ID`), exported into the child cargo
//! build by the release cutter. That made **what a binary trusts a property of the
//! shell that compiled it rather than of the source**, with three costs paid in
//! practice:
//!
//! * A locally built `atpkg` had an EMPTY root key and refused every install with
//!   `atpkg: disabled (no root key pinned or overridden)`. Same commit, same machine,
//!   different trust, and no diff anywhere to review.
//! * An unset variable is silent: `option_env!` yields `None`, the pin becomes `""`,
//!   and the consumer goes inert without saying so.
//! * "What does this build trust?" could not be answered by reading the repository.
//!
//! As constants the answer is a `git diff`, identical on every machine, and carried
//! in history like any other reviewed change.
//!
//! # Empty means unpinned, and unpinned means inert
//!
//! An empty anchor is the fail-closed default: with nothing compiled in there is
//! nothing to trust, so the consumer stays inert rather than accepting anything. A
//! fork or private channel commits its OWN anchors here — the same deliberate,
//! reviewable act as changing `update_channel`.
//!
//! # Rotation
//!
//! [`UPDATE_CHANNEL_PUBKEYS`] is a LIST, not a single key, and clients accept any
//! member. That is load-bearing: replacing one key with another in a single release
//! instantly strands every client still holding the old one, and the keyset cannot
//! be retrofitted after a key is lost. Rotating means publishing a bridge release
//! signed by the outgoing key that carries both, then promoting the incoming key
//! and retiring the old one. See `docs/RELEASE-KEYS.md`.

/// Ed25519 public keys any of which may sign a release for the public channel.
///
/// ORDER IS A CONTRACT. Index 0 is the key THIS build signs with. Every other member
/// is accepted-but-never-signed-with, and is either an **incoming** key being
/// pre-seeded into clients ahead of a rotation, or an **outgoing** key inside its
/// retirement window. Verification accepts any member.
///
/// The only workable rotation order follows from that, because a client can only
/// learn a new key from a release it already accepts:
///
/// 1. append K2 as a NON-head member, and ship — clients now accept K1 and K2
/// 2. wait out the adoption window
/// 3. promote K2 to index 0, so new releases are signed with K2
/// 4. drop K1 once the window has closed
///
/// Empty slice = unpinned: signature verification is skipped and the channel is
/// unauthenticated (forks and private channels).
///
/// An EMPTY STRING MEMBER is never legal — it is not "unpinned", it is a brick:
/// `update_channel_signing_pubkey()` would return `""` (so the build stamps itself
/// unpinned) while this slice is non-empty (so the client demands a signature and
/// then rejects every one). The tests below refuse it.
pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &["cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8="];

/// The single key new releases are signed WITH — always `UPDATE_CHANNEL_PUBKEYS[0]`.
///
/// Verification must use [`UPDATE_CHANNEL_PUBKEYS`] (accept any); only the cutter,
/// which produces exactly one signature, cares which key is current.
#[must_use]
pub const fn update_channel_signing_pubkey() -> &'static str {
    if UPDATE_CHANNEL_PUBKEYS.is_empty() {
        ""
    } else {
        UPDATE_CHANNEL_PUBKEYS[0]
    }
}

/// Ed25519 public ROOT key anchoring the atpkg package index.
///
/// The root is offline and delegates to a release key named by the signed index, so
/// package signing rotates without touching this value. Empty = atpkg inert.
pub const PKG_ROOT_PUBKEY: &str = "a2Ieu1ll3Lcl8L5G0V1+uCQ2tqTdILCFjds7IdYTr6c=";

/// Apple Developer **Team ID** for the optional Tier APPLE anchor.
///
/// Empty does NOT disable the updater — it skips the codesign/notarization anchor,
/// leaving signature + hash verification intact. Non-empty is a promise the release
/// pipeline must keep: a Developer-ID-signed AND notarized artifact.
pub const APPLE_TEAM_ID: &str = "";

/// Whether an anchor is active. Fail-closed: an empty anchor is never active.
///
/// Unlike the `pin_active` it replaces, this takes NO opt-out environment variable.
/// A build either has an anchor or it does not, and no ambient state can turn one
/// off — which is the entire point of moving these into source.
#[must_use]
pub const fn anchor_active(anchor: &str) -> bool {
    !anchor.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signing key is the head of the keyset — not a second, separately edited
    /// constant that could silently disagree with it.
    #[test]
    fn signing_key_is_the_head_of_the_keyset() {
        assert_eq!(update_channel_signing_pubkey(), UPDATE_CHANNEL_PUBKEYS[0]);
        assert!(
            !update_channel_signing_pubkey().is_empty(),
            "the public channel is pinned; an empty signing key would silently unpin it"
        );
    }

    /// A keyset with duplicates means a rotation was recorded wrong: the outgoing
    /// key was re-added rather than retired, so retiring it later would be a no-op.
    #[test]
    fn keyset_has_no_duplicates() {
        for (i, key) in UPDATE_CHANNEL_PUBKEYS.iter().enumerate() {
            assert!(
                !UPDATE_CHANNEL_PUBKEYS[..i].contains(key),
                "duplicate key in the rotation set: {key}"
            );
        }
    }

    /// An empty keyset member is a BRICK, not "unpinned": the build would stamp
    /// itself unpinned (`update_channel_signing_pubkey()` == "") while the client
    /// sees a non-empty keyset, demands a signature, and rejects every one. Only the
    /// whole slice being empty means unpinned.
    #[test]
    fn keyset_has_no_empty_members() {
        for (i, key) in UPDATE_CHANNEL_PUBKEYS.iter().enumerate() {
            assert!(
                !key.is_empty(),
                "UPDATE_CHANNEL_PUBKEYS[{i}] is empty — use an empty SLICE to unpin, \
                 never an empty member"
            );
        }
    }

    /// A keyset is a rotation window, not an accumulator. An unbounded list means an
    /// old key was never retired, which is the failure rotation exists to avoid.
    #[test]
    fn keyset_is_bounded() {
        assert!(
            UPDATE_CHANNEL_PUBKEYS.len() <= 4,
            "keyset has {} members; retire outgoing keys instead of accumulating them",
            UPDATE_CHANNEL_PUBKEYS.len()
        );
    }

    /// Anchors are base64 Ed25519 public keys: 32 bytes -> 44 chars with one `=`.
    /// Catches a truncated paste, which would otherwise fail closed at runtime with
    /// no hint that the VALUE, not the state, is wrong.
    #[test]
    fn anchors_are_well_formed_base64_ed25519() {
        // NOTE the asymmetry: an empty PKG_ROOT_PUBKEY is legal (atpkg inert), but an
        // empty keyset MEMBER is not — see `keyset_has_no_empty_members`.
        let check = |k: &str, what: &str| {
            if k.is_empty() {
                return; // unpinned is legal for a whole anchor
            }
            assert_eq!(k.len(), 44, "{what}: not a 44-char base64 Ed25519 key: {k}");
            assert!(k.ends_with('='), "{what}: missing base64 padding: {k}");
            assert!(
                k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
                "{what}: non-base64 character: {k}"
            );
        };
        for key in UPDATE_CHANNEL_PUBKEYS {
            check(key, "UPDATE_CHANNEL_PUBKEYS");
        }
        check(PKG_ROOT_PUBKEY, "PKG_ROOT_PUBKEY");
    }

    #[test]
    fn empty_anchor_is_never_active() {
        assert!(!anchor_active(""));
        assert!(anchor_active(PKG_ROOT_PUBKEY));
    }
}
