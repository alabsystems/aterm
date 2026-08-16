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
//! verifier enforces), so a detached signature here is byte-for-byte what
//! `atpkg::sig::verify_index` / `atpkg::sig::verify_pkg` check. A test signs a manifest and
//! verifies it with the **actual client verifier**, pinning the contract.
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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
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
    Ok(STANDARD.encode(kp.public_key().as_ref()))
}

/// Detached-sign `msg`'s exact bytes with the pkcs8 key → the 64-byte Ed25519 signature
/// the client verifies. `msg` must be the exact raw manifest asset bytes.
pub fn sign(pkcs8: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "invalid pkcs8 key".to_string())?;
    Ok(kp.sign(msg).as_ref().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(STANDARD.decode(&pub_b64).unwrap().len(), 32);
    }

    #[test]
    fn invalid_key_fails_closed() {
        assert!(sign(b"not a pkcs8 key", b"x").is_err());
        assert!(pubkey_b64(b"not a pkcs8 key").is_err());
    }
}
