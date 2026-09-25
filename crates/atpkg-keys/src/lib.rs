// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Owner-only POSIX tool (0600 secret-key files; its sole consumer, atpkg, is
// itself unix-gated): the crate compiles empty on Windows and the binary
// prints an honest "unsupported" stub instead.
#![cfg(unix)]

//! Owner-side Ed25519 signing for atpkg manifests (§8/§12) — the producer half of the
//! signature contract whose verifier is `atpkg::sig`.
//!
//! The atpkg client is **verify-only** (`ring` with `default-features = false`, no RNG/
//! alloc). The owner, by contrast, must *generate* the offline root + rotatable release
//! keys and *sign* `index.toml` / `pkg-*.toml` over their **exact raw bytes**. That needs
//! the full `ring` crate, so it lives in this **separate, owner-only** tool — never shipped
//! to clients, so the client's minimal crypto surface is unaffected.
//!
//! The unit signed is the exact asset bytes (no canonicalization — same discipline the
//! verifier enforces), so a detached signature here is byte-for-byte what the client
//! verifier checks: `atpkg::sig::TrustedRoster::authorize_bytes` (and
//! `atpkg::sig::TrustedIndex::verify_pkg`, the wrapper that calls it), which delegate to
//! `aterm_update_core::roster` — the same verifier the app updater runs. There is
//! exactly one; the single-root `verify_index`/`verify_index_with` pair this line
//! used to name was retired with the package-specific root, as the note on
//! `produces_signatures_the_client_verifier_accepts` below records. A test signs a
//! manifest and verifies it with that **actual client verifier**, pinning the contract.
//!
//! # The paper master and the machine roster
//!
//! This tool is also where the owner's **paper master** lives operationally. [`master`]
//! generates the 52 base32 characters that go on paper, reads them back from a
//! no-echo `/dev/tty` prompt with a public fingerprint the owner can eyeball, and derives
//! the master identity — without ever writing the secret anywhere. [`roster_ops`] holds
//! the three edits the master authorizes (add a machine, revoke a machine, start a fresh
//! roster) as pure functions over the shared `aterm_update_core::roster::Roster` type the
//! updater client parses, so producer and client cannot disagree about what a valid
//! roster is. See `docs/SIGNING-KEY-DESIGN.md` for the decision and `docs/RELEASE-KEYS.md`
//! for the runbook.
//!
//! # Provisioning: the human's only step is writing the phrase on paper
//!
//! [`provision`] is the engine behind the two verbs that removed every hand-transcription
//! from arming the release channel — `atpkg-keys setup` on the first machine and
//! `atpkg-keys join` on every later one. It derives the master's public identity, mints
//! the machine keypair, edits the roster and, through [`pins_edit`], WRITES the two public
//! keys into `aterm_update_core::pins` itself, so no one ever copies 44 base64 characters
//! into the file that decides what the fleet trusts. It stops short of `git commit` on
//! purpose: arming a trust anchor is a reviewed act.

pub mod fsio;
pub mod master;
pub mod pins_edit;
pub mod provision;
pub mod roster_ops;

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Generate a fresh Ed25519 keypair: returns `(pkcs8 private-key bytes, base64 raw
/// 32-byte public key)`. The pkcs8 bytes are the **secret** — write them `0600` and keep
/// them offline (the root key especially). The base64 public key is what goes into
/// the committed paper-master anchor, or a roster-named machine key. (The retired
/// two-tier world verified under `PINNED_PKG_ROOTKEY` / `[keys].release_key_pubkey`.)
pub fn generate() -> Result<(Vec<u8>, String), String> {
    let rng = SystemRandom::new();
    let pkcs8 =
        Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| "key generation failed".to_string())?;
    let pub_b64 = pubkey_b64(pkcs8.as_ref())?;
    Ok((pkcs8.as_ref().to_vec(), pub_b64))
}

/// The base64 raw public key for a pkcs8 private key (so the owner can recover the
/// publishable pubkey from a stored key file without re-generating).
pub fn pubkey_b64(pkcs8: &[u8]) -> Result<String, String> {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "invalid pkcs8 key".to_string())?;
    aterm_codec::base64::encode(kp.public_key().as_ref())
        .map_err(|_| "public key too large to encode".to_string())
}

/// Detached-sign `msg`'s exact bytes with the pkcs8 key → the 64-byte Ed25519 signature
/// the client verifies. `msg` must be the exact raw manifest asset bytes.
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
/// master-signed roster authorizes — the client's own rule for an appcast
/// (`aterm_update_core::roster`): the roster verifies under one of `master_pubkeys`, parses,
/// is fresh at `now_unix`, and one of its LIVE machines (listed, not revoked, not expired)
/// signed the file.
///
/// The ALab lane (`tools/atpkg-auto-alab.sh`) runs this over an ALab source release's
/// `SHA256SUMS` before it builds that source, against the roster the channel's verified
/// index ships — so a release signed by no rostered machine, or by one revoked since, is
/// never built beside the machine key. The refusal is the verifier's own reason, never an
/// oracle about which key was close.
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

    /// A master-signed one-machine roster (plus an optional revoked id and a validity
    /// deadline), minted in memory with keys this tool generates.
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

    // `verify-signed`: the release-side twin of the client's appcast rule. The positive
    // case names the machine; every negative control is one link of the chain broken.
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

    // THE contract: a signature produced here is accepted by the actual client verifier
    // over the exact bytes, and a 1-byte tamper is rejected.
    //
    // The verifier is `aterm_update_core::roster` — the SAME one atpkg's index chain runs
    // (`atpkg::sig::TrustedRoster::authorize_bytes` delegates to it) and the same one the
    // app updater runs. It used to be `atpkg::sig::verify_index_with`, a single-key check
    // against a package-specific root; that root is retired, and there is exactly one
    // verifier left to prove this tool's output against. The FULL owner→client chain,
    // through `atpkg::flow::install` and a real archive, is `tests/owner_to_client.rs`.
    #[test]
    fn produces_signatures_the_client_verifier_accepts() {
        use aterm_update_core::roster::{Machine, Roster, RosterReject};

        let (key, pub_b64) = generate().unwrap();
        let manifest = b"schema = 2\nindex_build = 7\nvalid_until = \"2099-01-01T00:00:00Z\"\n";
        let sig = sign(&key, manifest).unwrap();
        // A one-machine roster naming the key this tool just minted. Building it here (in
        // memory) keeps this a unit test of the SIGNATURE; the master-signature half is
        // proved wherever a roster is published.
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
        // A different key's signature is rejected (no cross-key acceptance).
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
        // A 32-byte base64 pubkey decodes to exactly 32 bytes (the client's BadKey gate).
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
