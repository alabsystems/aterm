// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authorization of a shared release signer, independent of the physical member's name.
//! A member's admitted vault may deliver the ring, but only the pinned release master's
//! roster can authorize its signing key. This module reads no files and exports no secrets.

use aterm_update_core::roster::{Attribution, Roster, RosterReject};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use zeroize::Zeroizing;

const CHALLENGE: &[u8] = b"aterm/release-ring/authorize/1\0";

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
mod tests {
    use super::*;
    use aterm_update_core::roster::{Machine, SUPPORTED_SCHEMA};

    struct Fixture {
        key: Zeroizing<Vec<u8>>,
        profile: Zeroizing<String>,
        master: Ed25519KeyPair,
        anchor: String,
        roster: Roster,
    }

    impl Fixture {
        fn new() -> Self {
            let rng = ring::rand::SystemRandom::new();
            let master_der = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
            let master = Ed25519KeyPair::from_pkcs8(master_der.as_ref()).unwrap();
            let anchor = aterm_codec::base64::encode(master.public_key().as_ref()).unwrap();
            let key = Zeroizing::new(
                Ed25519KeyPair::generate_pkcs8(&rng)
                    .unwrap()
                    .as_ref()
                    .to_vec(),
            );
            let pair = Ed25519KeyPair::from_pkcs8(&key).unwrap();
            let profile = Zeroizing::new(format!(
                "signing_key = \"{}\"\nmachine_id = \"shared-ring\"\n",
                aterm_codec::base64::encode(&key).unwrap()
            ));
            let roster = Roster {
                schema: SUPPORTED_SCHEMA,
                roster_seq: 7,
                valid_until: "9999-12-31T00:00:00Z".into(),
                revoked: Vec::new(),
                machines: vec![Machine {
                    id: "shared-ring".into(),
                    pubkey: aterm_codec::base64::encode(pair.public_key().as_ref()).unwrap(),
                    added_at: "1970-01-01T00:00:00Z".into(),
                    not_after: None,
                }],
            };
            Self {
                key,
                profile,
                master,
                anchor,
                roster,
            }
        }

        fn check(&self, floor: u64, now: i64) -> Result<Attribution, Reject> {
            let bytes = self.roster.to_toml().unwrap().into_bytes();
            let signature = self.master.sign(&bytes);
            authorize_with(
                self.profile.as_bytes(),
                self.key.as_slice(),
                bytes,
                signature.as_ref(),
                &[&self.anchor],
                floor,
                now,
            )
        }
    }

    #[test]
    fn a_shared_ring_proves_its_authorized_signer_without_a_physical_host_name() {
        let f = Fixture::new();
        let signer = f.check(7, 1_000).unwrap();
        assert_eq!(signer.machine_id, "shared-ring");
        assert_eq!(signer.roster_seq, 7);
    }

    #[test]
    fn mismatched_keys_identity_and_roster_authority_are_refused() {
        let mut f = Fixture::new();
        let other = Fixture::new();
        let old = std::mem::replace(&mut f.key, other.key);
        assert_eq!(f.check(7, 1_000), Err(Reject::KeyMismatch));
        f.key = old;
        f.roster.revoked.push("shared-ring".into());
        assert_eq!(
            f.check(7, 1_000),
            Err(Reject::Roster(RosterReject::Revoked))
        );
        f.roster.revoked.clear();
        assert_eq!(
            f.check(8, 1_000),
            Err(Reject::Roster(RosterReject::Rollback))
        );
        f.roster.valid_until = "1970-01-01T00:00:01Z".into();
        assert_eq!(f.check(7, 1_000), Err(Reject::Roster(RosterReject::Stale)));
    }

    #[test]
    fn an_untrusted_roster_and_a_false_profile_signer_are_refused() {
        let mut f = Fixture::new();
        let other = Fixture::new();
        let bytes = f.roster.to_toml().unwrap().into_bytes();
        let signature = other.master.sign(&bytes);
        assert_eq!(
            authorize_with(
                f.profile.as_bytes(),
                f.key.as_slice(),
                bytes,
                signature.as_ref(),
                &[&f.anchor],
                0,
                1_000,
            ),
            Err(Reject::Roster(RosterReject::Verify))
        );
        f.roster.machines.push(Machine {
            id: "other-signer".into(),
            pubkey: other.roster.machines[0].pubkey.clone(),
            added_at: "1970-01-01T00:00:00Z".into(),
            not_after: None,
        });
        f.profile = Zeroizing::new(f.profile.replace("shared-ring", "other-signer"));
        assert_eq!(f.check(0, 1_000), Err(Reject::SignerMismatch));
    }
}
