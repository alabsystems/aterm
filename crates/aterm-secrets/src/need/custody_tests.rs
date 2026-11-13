// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    assert!(attest(&SIGNER_SEED, &[1; 16], record("", &[2; 16], body)).is_err());
    let over_kind = "k".repeat(usize::from(u16::MAX) + 1);
    assert!(attest(&SIGNER_SEED, &[1; 16], record(&over_kind, &[2; 16], body)).is_err());
    assert!(!attested(
        &public(&SIGNER_SEED),
        &[1; 16],
        record(&over_kind, &[2; 16], body),
        &signature,
    ));
}
