// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Owner-only POSIX tool (0600 key files; its consumer atpkg is unix-gated too): the crate
// compiles empty on Windows and the binary prints an "unsupported" stub.
#![cfg(unix)]

//! Owner-side Ed25519 signing for atpkg manifests: the producer half of the signature
//! contract whose verifier is `atpkg::sig`.
//!
//! The atpkg client is verify-only (`ring` without default features, so no RNG). Generating
//! the paper master and machine keys and signing `index.toml` / `pkg-*.toml` needs the full
//! `ring` crate, so it lives in this owner-only tool, never shipped to clients. The signed
//! unit is the exact asset bytes (no canonicalization), checked by `aterm_update_core::roster`,
//! the same verifier the app updater runs.
//!
//! [`master`] handles the paper master: 52 base32 characters, read back from a no-echo
//! `/dev/tty` and never written anywhere. [`roster_ops`] holds the roster edits as pure
//! functions over the shared `Roster` type the updater parses, so producer and client cannot
//! disagree on a valid roster. [`provision`] is the engine behind `setup` (first machine) and
//! `join` (each later one): through [`pins_edit`] it writes the public keys into
//! `aterm_update_core::pins` itself, and stops short of `git commit`, since arming a trust
//! anchor is a reviewed act. See `docs/RELEASE-KEYS.md`.

pub mod fsio;
pub mod master;
pub mod pins_edit;
pub mod provision;
pub mod roster_ops;
#[cfg(test)]
mod testdir;

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Generate a fresh Ed25519 keypair: `(pkcs8 private-key bytes, base64 raw 32-byte public
/// key)`. The pkcs8 bytes are the secret: write them `0600` and keep them offline.
pub fn generate() -> Result<(Vec<u8>, String), String> {
    let rng = SystemRandom::new();
    let pkcs8 =
        Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| "key generation failed".to_string())?;
    let pub_b64 = pubkey_b64(pkcs8.as_ref())?;
    Ok((pkcs8.as_ref().to_vec(), pub_b64))
}

/// The base64 raw public key of a pkcs8 private key.
pub fn pubkey_b64(pkcs8: &[u8]) -> Result<String, String> {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "invalid pkcs8 key".to_string())?;
    aterm_codec::base64::encode(kp.public_key().as_ref())
        .map_err(|_| "public key too large to encode".to_string())
}

/// Detached-sign `msg`'s exact bytes (the raw manifest asset) with the pkcs8 key, giving the
/// 64-byte Ed25519 signature the client verifies.
pub fn sign(pkcs8: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "invalid pkcs8 key".to_string())?;
    Ok(kp.sign(msg).as_ref().to_vec())
}

/// Who signed a file, as [`verify_signed`] proved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// The roster id of the machine whose key verified.
    pub machine_id: String,
    /// The roster generation that authorized it.
    pub roster_seq: u64,
}

/// Verify a detached Ed25519 signature over `file`'s exact bytes by a machine the
/// master-signed roster authorizes, by the client's own appcast rule: the roster verifies
/// under one of `master_pubkeys`, parses, is fresh at `now_unix`, and one of its live machines
/// (listed, not revoked, not expired) signed the file.
///
/// The ALab lane (`atpkg-publish lane run alab`) runs this over a source release's
/// `SHA256SUMS` before building it, so a release signed by no rostered machine, or by one
/// since revoked, is never built.
///
/// # Errors
///
/// The [`aterm_update_core::roster::RosterReject`] of the first step that refused.
pub fn verify_signed(
    master_pubkeys: &[&str],
    roster: Vec<u8>,
    roster_sig: &[u8],
    file: &[u8],
    sig: &[u8],
    now_unix: i64,
) -> Result<Signed, aterm_update_core::roster::RosterReject> {
    use aterm_update_core::roster::{Roster, verify_roster};
    let verified = verify_roster(master_pubkeys, roster, roster_sig)?;
    let parsed = Roster::parse(&verified)?;
    parsed.admit(0, now_unix)?;
    let who = parsed.authorize_appcast(file, sig, now_unix)?;
    Ok(Signed {
        machine_id: who.machine_id,
        roster_seq: who.roster_seq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A master-signed one-machine roster, with optional revoked ids and a deadline.
    fn signed_roster(
        master: &[u8],
        machine_pub: &str,
        revoked: &[&str],
        valid_until: &str,
    ) -> (Vec<u8>, Vec<u8>) {
        use aterm_update_core::roster::{Machine, Roster};
        let roster = Roster {
            schema: 1,
            roster_seq: 4,
            valid_until: valid_until.into(),
            machines: vec![Machine {
                id: "m2".into(),
                pubkey: machine_pub.into(),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: revoked.iter().map(|r| (*r).to_string()).collect(),
        };
        let bytes = roster
            .to_toml()
            .expect("a valid roster serializes")
            .into_bytes();
        let sig = sign(master, &bytes).expect("the master signs");
        (bytes, sig)
    }

    // Each negative control breaks one link of the chain.
    #[test]
    fn verify_signed_accepts_a_rostered_signature_and_nothing_else() {
        use aterm_update_core::roster::RosterReject;
        let now = 1_785_801_600i64; // 2026-08-04
        let (master, master_pub) = generate().unwrap();
        let (machine, machine_pub) = generate().unwrap();
        let file = b"# alab-release SHA256SUMS \xe2\x80\x94 schema 1\n# tag: v0.25.0\n";
        let sig = sign(&machine, file).unwrap();
        let (roster, roster_sig) =
            signed_roster(&master, &machine_pub, &[], "2099-01-01T00:00:00Z");

        let who = verify_signed(&[&master_pub], roster.clone(), &roster_sig, file, &sig, now)
            .expect("a rostered machine's signature verifies");
        assert_eq!(
            who,
            Signed {
                machine_id: "m2".into(),
                roster_seq: 4
            }
        );

        // A tampered file (one byte) is refused.
        let mut bad = file.to_vec();
        bad[2] ^= 0x01;
        assert_eq!(
            verify_signed(&[&master_pub], roster.clone(), &roster_sig, &bad, &sig, now),
            Err(RosterReject::Verify)
        );
        // A signature by a key the roster does not list is refused.
        let (stranger, _) = generate().unwrap();
        let stranger_sig = sign(&stranger, file).unwrap();
        assert_eq!(
            verify_signed(
                &[&master_pub],
                roster.clone(),
                &roster_sig,
                file,
                &stranger_sig,
                now
            ),
            Err(RosterReject::Verify)
        );
        // A roster the pinned master did not sign is refused before any file crypto.
        let (other_master, _) = generate().unwrap();
        let forged_sig = sign(&other_master, &roster).unwrap();
        assert_eq!(
            verify_signed(&[&master_pub], roster.clone(), &forged_sig, file, &sig, now),
            Err(RosterReject::Verify)
        );
        // A machine revoked on the roster authorizes nothing.
        let (revoked, revoked_sig) =
            signed_roster(&master, &machine_pub, &["m2"], "2099-01-01T00:00:00Z");
        assert!(verify_signed(&[&master_pub], revoked, &revoked_sig, file, &sig, now).is_err());
        // A lapsed roster authorizes nothing.
        let (stale, stale_sig) = signed_roster(&master, &machine_pub, &[], "2026-01-01T00:00:00Z");
        assert_eq!(
            verify_signed(&[&master_pub], stale, &stale_sig, file, &sig, now),
            Err(RosterReject::Stale)
        );
    }

    // The contract: the client verifier (`aterm_update_core::roster`) accepts this tool's
    // signature over the exact bytes and rejects a 1-byte tamper. The full owner→client chain
    // through `atpkg::flow::install` is `tests/owner_to_client.rs`.
    #[test]
    fn produces_signatures_the_client_verifier_accepts() {
        use aterm_update_core::roster::{Machine, Roster, RosterReject};

        let (key, pub_b64) = generate().unwrap();
        let manifest = b"schema = 2\nindex_build = 7\nvalid_until = \"2099-01-01T00:00:00Z\"\n";
        let sig = sign(&key, manifest).unwrap();
        // An in-memory roster keeps this a test of the signature; the master-signature half
        // is `verify_signed_accepts_a_rostered_signature_and_nothing_else`.
        let roster = Roster {
            schema: 1,
            roster_seq: 1,
            valid_until: "2099-01-01T00:00:00Z".into(),
            machines: vec![Machine {
                id: "m3".into(),
                pubkey: pub_b64.clone(),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: vec![],
        };
        let now = 1_785_801_600i64;

        // The real client verifier accepts it, and attributes it to the minting machine.
        let who = roster
            .authorize_appcast(manifest, &sig, now)
            .expect("the client must accept a signature this tool produced");
        assert_eq!(who.machine_id, "m3");
        assert_eq!(who.pubkey_b64, pub_b64);
        // A single-byte tamper is rejected.
        let mut bad = manifest.to_vec();
        bad[0] ^= 0x01;
        assert_eq!(
            roster.authorize_appcast(&bad, &sig, now).err(),
            Some(RosterReject::Verify)
        );
        // Another key's signature is rejected.
        let (other, _) = generate().unwrap();
        let other_sig = sign(&other, manifest).unwrap();
        assert_eq!(
            roster.authorize_appcast(manifest, &other_sig, now).err(),
            Some(RosterReject::Verify)
        );
    }

    #[test]
    fn pubkey_b64_recovers_the_published_key() {
        let (key, pub_b64) = generate().unwrap();
        assert_eq!(pubkey_b64(&key).unwrap(), pub_b64);
        // The client's BadKey gate: the pubkey decodes to exactly 32 bytes.
        assert_eq!(
            aterm_codec::base64::decode_strict(pub_b64.as_bytes())
                .unwrap()
                .len(),
            32
        );
    }

    #[test]
    fn invalid_key_fails_closed() {
        assert!(sign(b"not a pkcs8 key", b"x").is_err());
        assert!(pubkey_b64(b"not a pkcs8 key").is_err());
    }
}
