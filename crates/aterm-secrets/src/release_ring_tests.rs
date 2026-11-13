// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
