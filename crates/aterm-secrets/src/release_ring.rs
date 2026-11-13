// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authorization of a shared release signer, independent of the physical member's name.
//! A member's admitted vault may deliver the ring, but only the pinned release master's
//! roster can authorize its signing key. This module reads no files and exports no secrets.

use aterm_update_core::roster::{Attribution, Roster, RosterReject};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use zeroize::Zeroizing;

const CHALLENGE: &[u8] = b"aterm/release-ring/authorize/1\0";

/// Select the newest master-signed generation before checking a signer's authority. An
/// invalid supplied pair, malformed signed roster or fork at the newest generation refuses
/// selection. In particular, callers cannot ignore a newer revocation and use an old grant.
pub(crate) fn newest_roster<'a, T>(
    anchors: &[&str],
    candidates: impl IntoIterator<Item = (T, &'a [u8], &'a [u8])>,
) -> Option<(T, u64)> {
    let mut chosen: Option<(T, u64, &[u8])> = None;
    let mut fork = false;
    for (item, raw, signature) in candidates {
        let verified =
            aterm_update_core::roster::verify_roster(anchors, raw.to_vec(), signature).ok()?;
        let roster = Roster::parse(&verified).ok()?;
        if let Some((_, seq, old_raw)) = &chosen {
            if roster.roster_seq == *seq {
                fork |= raw != *old_raw;
                continue;
            }
            if roster.roster_seq < *seq {
                continue;
            }
        }
        chosen = Some((item, roster.roster_seq, raw));
        fork = false;
    }
    if fork {
        None
    } else {
        chosen.map(|(item, seq, _)| (item, seq))
    }
}

/// Why the delivered profile and key do not establish release authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// The profile lacks an unambiguous signing key or signer id.
    Profile,
    /// Either private key is not a canonical PKCS#8 Ed25519 key.
    Key,
    /// The profile and the ring's key file identify different keys.
    KeyMismatch,
    /// The signed roster is untrusted, stale, revoked, or does not authorize the key.
    Roster(RosterReject),
    /// The profile names a different signer than the key proves.
    SignerMismatch,
}

/// Prove that a profile and its key file share one currently authorized release identity.
/// `floor_seq` is the caller's retained roster floor; a freshly enrolled member may use zero.
/// No physical hostname is involved: a shared ring identifies its signer, while enrollment
/// independently decides which members may receive it.
///
/// # Errors
/// A malformed or mismatched key/profile, an untrusted or stale roster, or a signer the roster
/// does not authorize returns [`Reject`] without including secret values in the error.
pub fn authorize(
    profile: &[u8],
    private_key: &[u8],
    roster: Vec<u8>,
    roster_signature: &[u8],
    floor_seq: u64,
    now_unix: i64,
) -> Result<Attribution, Reject> {
    authorize_with(
        profile,
        private_key,
        roster,
        roster_signature,
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        floor_seq,
        now_unix,
    )
}

pub(crate) fn authorize_with(
    profile: &[u8],
    private_key: &[u8],
    roster: Vec<u8>,
    roster_signature: &[u8],
    anchors: &[&str],
    floor_seq: u64,
    now_unix: i64,
) -> Result<Attribution, Reject> {
    let text = std::str::from_utf8(profile).map_err(|_| Reject::Profile)?;
    let doc = aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|_| Reject::Profile)?;
    let encoded = doc
        .get("signing_key")
        .and_then(aterm_toml::Value::as_str)
        .ok_or(Reject::Profile)?;
    let claimed = doc
        .get("machine_id")
        .and_then(aterm_toml::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(Reject::Profile)?;
    let pkcs8 = Zeroizing::new(
        aterm_codec::base64::decode_strict(encoded.as_bytes()).map_err(|_| Reject::Key)?,
    );
    let profile_key = Ed25519KeyPair::from_pkcs8(pkcs8.as_slice()).map_err(|_| Reject::Key)?;
    let file_key = Ed25519KeyPair::from_pkcs8(private_key).map_err(|_| Reject::Key)?;
    if profile_key.public_key().as_ref() != file_key.public_key().as_ref() {
        return Err(Reject::KeyMismatch);
    }
    let verified = aterm_update_core::roster::verify_roster(anchors, roster, roster_signature)
        .map_err(Reject::Roster)?;
    let roster = Roster::parse(&verified).map_err(Reject::Roster)?;
    roster.admit(floor_seq, now_unix).map_err(Reject::Roster)?;
    roster.machine(claimed, now_unix).map_err(Reject::Roster)?;
    let proof = file_key.sign(CHALLENGE);
    let signer = roster
        .authorize_appcast(CHALLENGE, proof.as_ref(), now_unix)
        .map_err(Reject::Roster)?;
    if signer.machine_id != claimed {
        return Err(Reject::SignerMismatch);
    }
    Ok(signer)
}

#[cfg(test)]
#[path = "release_ring_tests.rs"]
mod tests;
