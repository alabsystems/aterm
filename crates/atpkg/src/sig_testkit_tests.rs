// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Anchor, TrustedRoster, VerifiedBytes, admit_roster};
use aterm_update_core::roster::{Machine, Roster};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Obviously synthetic, and shared with `sig`'s own tests so one fixture describes the whole crate's notion of "the owner".
pub(crate) const MASTER_SEED: [u8; 32] = [0xA1; 32];
/// The one machine on the test roster.
pub(crate) const MACHINE_SEED: [u8; 32] = [0xB1; 32];
/// A key that is on NO roster — the attacker.
pub(crate) const OUTSIDER_SEED: [u8; 32] = [0xC1; 32];
/// 2026-08-04T00:00:00Z.
pub(crate) const NOW: i64 = 1_785_801_600;
/// The roster generation every fixture below publishes at.
pub(crate) const SEQ: u64 = 3;
/// The machine id every fixture below signs as.
pub(crate) const MACHINE_ID: &str = "m3";

pub(crate) fn keypair(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).expect("synthetic 32-byte seed")
}

pub(crate) fn pubkey_b64(seed: &[u8; 32]) -> String {
    aterm_codec::base64::encode(keypair(seed).public_key().as_ref()).expect("32-byte key")
}

pub(crate) fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
    keypair(seed).sign(msg).as_ref().to_vec()
}

/// The one-machine roster the whole crate's tests publish under.
pub(crate) fn roster() -> Roster {
    Roster {
        schema: 1,
        roster_seq: SEQ,
        valid_until: "2099-01-01T00:00:00Z".into(),
        machines: vec![Machine {
            id: MACHINE_ID.into(),
            pubkey: pubkey_b64(&MACHINE_SEED),
            added_at: "2026-08-04T00:00:00Z".into(),
            not_after: None,
        }],
        revoked: vec![],
    }
}

/// The roster's published bytes + the paper master's detached signature.
pub(crate) fn published_roster() -> (Vec<u8>, Vec<u8>) {
    let bytes = roster()
        .to_toml()
        .expect("a valid roster emits")
        .into_bytes();
    let sig = sign(&MASTER_SEED, &bytes);
    (bytes, sig)
}

/// An anchor armed with the synthetic paper master, floor 0.
pub(crate) fn anchor() -> Anchor {
    Anchor::of(vec![pubkey_b64(&MASTER_SEED)], 0)
}

/// The admitted roster generation, through the real chain.
pub(crate) fn trusted_roster() -> TrustedRoster {
    let (bytes, sig) = published_roster();
    admit_roster(&anchor(), bytes, &sig, NOW).expect("the synthetic chain admits")
}

/// The index `raw`, signed by the test machine and authorized under the test roster re-issued at generation `seq`.
pub(crate) fn trusted_index_at(seq: u64, raw: Vec<u8>) -> super::TrustedIndex {
    let bytes = Roster {
        roster_seq: seq,
        ..roster()
    }
    .to_toml()
    .expect("a valid roster emits")
    .into_bytes();
    let roster_sig = sign(&MASTER_SEED, &bytes);
    let sig = sign(&MACHINE_SEED, &raw);
    admit_roster(&anchor(), bytes, &roster_sig, NOW)
        .expect("the synthetic chain admits")
        .authorize_index(raw, &sig)
        .expect("the roster's own machine signed this index")
        .0
}

/// `raw`, machine-signed and taken through the real authorization path.
pub(crate) fn machine_signed(raw: Vec<u8>) -> VerifiedBytes {
    let sig = sign(&MACHINE_SEED, &raw);
    trusted_roster()
        .authorize_bytes(raw, &sig)
        .expect("the roster's own machine signed these bytes")
        .0
}
