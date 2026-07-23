// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Optional Ed25519 verification of the update **manifest** (`aterm-appcast.toml`) —
//! the Apple-free "signed channel" tier of the updater's trust model.
//!
//! When a public key is compiled into the binary ([`crate::PINNED_UPDATE_PUBKEY`],
//! baked from `ATERM_UPDATE_PUBKEY` at build time), every release manifest MUST carry a
//! detached Ed25519 signature (`aterm-appcast.toml.sig`) that verifies against it. The
//! manifest pins the DMG's `sha256`, so a valid signature over the manifest —
//! transitively, once the sha256 is checked — authenticates the whole artifact WITHOUT
//! any Apple Developer ID: even an attacker with repo write cannot forge it (the
//! private key stays offline / in CI secrets). With NO pubkey pinned this tier is
//! simply absent and the updater falls back to repo-trust + sha256 (see `verify.rs`).
//!
//! This is the exact primitive `atpkg` pins for its signed index (`atpkg::sig`), reused
//! here: `ring`'s `UnparsedPublicKey::new(&ED25519, pk).verify(msg, sig)`, cheapest
//! gate first (empty-pin → key length → sig length → the crypto), fail-closed.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{ED25519, UnparsedPublicKey};

/// Why a manifest signature was rejected. Every crypto failure collapses to
/// [`SigReject::Verify`] (`ring`'s error is opaque) — no per-reason oracle.
#[derive(Debug, PartialEq, Eq)]
pub enum SigReject {
    /// No public key is pinned — the signed-channel tier is disabled. (Callers only
    /// invoke verification when a key IS pinned, so this is a defensive default.)
    Disabled,
    /// The pinned key did not base64-decode, or was not exactly 32 bytes.
    BadKey,
    /// The detached signature was not exactly 64 bytes.
    BadSig,
    /// The Ed25519 signature did not verify over these bytes with this key.
    Verify,
}

/// Verify that `sig` is a valid Ed25519 signature by `pubkey_b64` over `msg`.
/// Cheapest-first, fail-closed: empty pin → decode+length-check the key → length-check
/// the signature → the actual crypto, last.
pub fn verify_detached(pubkey_b64: &str, msg: &[u8], sig: &[u8]) -> Result<(), SigReject> {
    if pubkey_b64.is_empty() {
        return Err(SigReject::Disabled);
    }
    let pk = STANDARD.decode(pubkey_b64).map_err(|_| SigReject::BadKey)?;
    if pk.len() != 32 {
        return Err(SigReject::BadKey);
    }
    if sig.len() != 64 {
        return Err(SigReject::BadSig);
    }
    UnparsedPublicKey::new(&ED25519, &pk)
        .verify(msg, sig)
        .map_err(|_| SigReject::Verify)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const SEED: [u8; 32] = [7u8; 32];

    fn keypair() -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(&SEED).unwrap()
    }

    #[test]
    fn accepts_a_valid_signature_rejects_tamper_and_wrong_key() {
        let kp = keypair();
        let pk = B64.encode(kp.public_key().as_ref());
        let msg = b"schema = 1\nbuild_number = 42\n";
        let sig = kp.sign(msg);

        // valid.
        assert_eq!(verify_detached(&pk, msg, sig.as_ref()), Ok(()));
        // one flipped manifest byte ⇒ Verify (this is what stops a repo-write attacker
        // editing build_number / sha256 in the manifest).
        let mut tampered = msg.to_vec();
        tampered[13] ^= 1;
        assert_eq!(
            verify_detached(&pk, &tampered, sig.as_ref()),
            Err(SigReject::Verify)
        );
        // a different key ⇒ Verify.
        let other = B64.encode(
            Ed25519KeyPair::from_seed_unchecked(&[9u8; 32])
                .unwrap()
                .public_key()
                .as_ref(),
        );
        assert_eq!(
            verify_detached(&other, msg, sig.as_ref()),
            Err(SigReject::Verify)
        );
    }

    #[test]
    fn fails_closed_on_empty_key_bad_key_and_bad_sig() {
        let kp = keypair();
        let pk = B64.encode(kp.public_key().as_ref());
        let sig = kp.sign(b"m");
        assert_eq!(
            verify_detached("", b"m", sig.as_ref()),
            Err(SigReject::Disabled)
        );
        assert_eq!(
            verify_detached("not-base64!!", b"m", sig.as_ref()),
            Err(SigReject::BadKey)
        );
        assert_eq!(
            verify_detached(&B64.encode([0u8; 16]), b"m", sig.as_ref()),
            Err(SigReject::BadKey)
        );
        assert_eq!(
            verify_detached(&pk, b"m", &[0u8; 10]),
            Err(SigReject::BadSig)
        );
    }
}
