// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CUSTODY (docs/DESIGN-credential-requests-2026-10-27.md §5, §7.1): a custodian is a Mac that
//! can sign with the vault's current signing key, which every member already pins, so no list,
//! record or new pin says who the admins are. Its *attestation* over a custodian-written record
//! (`Published`, `NeedsPerson`, a custodian's `Retired`, `Policy`) signs the record's kind, its
//! key (the request id) and its body, under a domain nothing else that key signs begins with
//! (envelopes and handoffs begin `alab-vault/1`, Commons records `aterm/commons/…`). A member
//! checks it before the record counts. The kind and key are bound, so a decision attested for
//! one request never counts for another whose body reads the same.
//!
//! These are pure: callers supply keys and the facts their replica holds.

use crate::Error;
use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};

/// The domain of a custodian's attestation of a record body.
pub const RECORD_DOMAIN: &[u8] = b"aterm/credentials/custodian-record/1\0";

/// The most a record body attested takes (§5).
pub const MAX_BODY: usize = 4 * 1024;

fn sha256(bytes: &[u8]) -> [u8; 32] {
    aterm_digest::Sha256::digest(bytes)
}

fn key_pair(seed: &[u8; 32]) -> crate::Result<Ed25519KeyPair> {
    Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("custody: the signing seed is not a usable Ed25519 seed"))
}

fn sign(seed: &[u8; 32], message: &[u8]) -> crate::Result<[u8; 64]> {
    let signature = key_pair(seed)?.sign(message);
    <[u8; 64]>::try_from(signature.as_ref())
        .map_err(|_| Error::new("custody: an Ed25519 signature is not 64 bytes"))
}

fn verifies(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    UnparsedPublicKey::new(&ED25519, key)
        .verify(message, signature)
        .is_ok()
}

/// The record an attestation covers: its kind (`published`, `needs-person`, `retired`…), its
/// key (the request id) and its canonical body without the attestation.
#[derive(Debug, Clone, Copy)]
pub struct Attested<'a> {
    /// The record's kind.
    pub kind: &'a str,
    /// The record's key: the request it answers.
    pub key: &'a [u8; 16],
    /// The canonical body.
    pub body: &'a [u8],
}

/// What an attestation signs: the domain, the Commons network, the record's kind and key, and
/// the body's digest. Binding the kind and key keeps a decision about one request from
/// counting for another whose body reads the same (a `needs-person` body is one template id).
#[must_use]
pub fn attestation_message(network: &[u8; 16], record: Attested<'_>) -> Vec<u8> {
    let mut message = RECORD_DOMAIN.to_vec();
    message.extend_from_slice(network);
    message.extend_from_slice(
        &u16::try_from(record.kind.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    message.extend_from_slice(record.kind.as_bytes());
    message.extend_from_slice(record.key);
    message.extend_from_slice(&sha256(record.body));
    message
}

/// A custodian's attestation of `record` on `network`, by the vault signing seed `seed`.
///
/// # Errors
/// The body is over [`MAX_BODY`], or `seed` is not a usable seed.
pub fn attest(
    seed: &[u8; 32],
    network: &[u8; 16],
    record: Attested<'_>,
) -> crate::Result<[u8; 64]> {
    if record.body.len() > MAX_BODY {
        return Err(Error::new(format!(
            "custody: a record body over {MAX_BODY} bytes is never attested"
        )));
    }
    sign(seed, &attestation_message(network, record))
}

/// Whether `signature` attests `record` on `network` by `signer`, the vault signing key this
/// Mac pins (and follows through the handoffs it applied). A record whose attestation fails is
/// inert.
#[must_use]
pub fn attested(
    signer: &[u8; 32],
    network: &[u8; 16],
    record: Attested<'_>,
    signature: &[u8; 64],
) -> bool {
    record.body.len() <= MAX_BODY
        && verifies(signer, &attestation_message(network, record), signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNER_SEED: [u8; 32] = [7; 32];
    const OTHER_SEED: [u8; 32] = [8; 32];

    fn public(seed: &[u8; 32]) -> [u8; 32] {
        crate::envelope::signer_public(seed).unwrap()
    }

    #[test]
    fn an_attestation_verifies_against_the_pinned_signer_and_fails_against_another_key() {
        let record = |kind, key, body| Attested { kind, key, body };
        let body: &[u8] = b"{\"template\":\"vault-lacks\"}";
        let signed = record("needs-person", &[2; 16], body);
        let signature = attest(&SIGNER_SEED, &[1; 16], signed).unwrap();
        assert!(attested(
            &public(&SIGNER_SEED),
            &[1; 16],
            signed,
            &signature
        ));
        assert!(!attested(
            &public(&OTHER_SEED),
            &[1; 16],
            signed,
            &signature
        ));
        assert!(!attested(
            &public(&SIGNER_SEED),
            &[2; 16],
            signed,
            &signature
        ));
        assert!(!attested(
            &public(&SIGNER_SEED),
            &[1; 16],
            record("needs-person", &[2; 16], b"{\"template\":\"holder-away\"}"),
            &signature
        ));
        // A decision about one request never counts for another, nor as another kind.
        assert!(!attested(
            &public(&SIGNER_SEED),
            &[1; 16],
            record("needs-person", &[3; 16], body),
            &signature
        ));
        assert!(!attested(
            &public(&SIGNER_SEED),
            &[1; 16],
            record("retired", &[2; 16], body),
            &signature
        ));
        // The domain is its own: an envelope signature over the same bytes never attests.
        let bare = sign(&SIGNER_SEED, body).unwrap();
        assert!(!attested(&public(&SIGNER_SEED), &[1; 16], signed, &bare));
        assert!(
            attest(
                &SIGNER_SEED,
                &[1; 16],
                record("published", &[2; 16], &[0; MAX_BODY + 1])
            )
            .is_err()
        );
    }
}
