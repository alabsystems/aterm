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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Generate a fresh Ed25519 keypair: returns `(pkcs8 private-key bytes, base64 raw
/// 32-byte public key)`. The pkcs8 bytes are the **secret** — write them `0600` and keep
/// them offline (the root key especially). The base64 public key is what goes into
/// `PINNED_PKG_ROOTKEY` (root) or the index's `[keys].release_key_pubkey` (release).
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

    // THE contract: a signature produced here is accepted by the actual atpkg client
    // verifier over the exact bytes, and a 1-byte tamper is rejected.
    #[test]
    fn produces_signatures_the_client_verifier_accepts() {
        let (key, pub_b64) = generate().unwrap();
        let manifest = b"schema = 1\nindex_build = 7\nvalid_until = \"2026-07-05T12:00:00Z\"\n";
        let sig = sign(&key, manifest).unwrap();

        // The real client verifier accepts it under the published pubkey.
        assert!(
            atpkg::sig::verify_index_with(&pub_b64, manifest.to_vec(), &sig).is_ok(),
            "the client must accept a signature this tool produced"
        );
        // A single-byte tamper is rejected.
        let mut bad = manifest.to_vec();
        bad[0] ^= 0x01;
        assert!(atpkg::sig::verify_index_with(&pub_b64, bad, &sig).is_err());
        // A different key's signature is rejected (no cross-key acceptance).
        let (other, _) = generate().unwrap();
        let other_sig = sign(&other, manifest).unwrap();
        assert!(atpkg::sig::verify_index_with(&pub_b64, manifest.to_vec(), &other_sig).is_err());
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
