// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SEED: [u8; 32] = [17; 32];
fn fixture() -> (Trust, age::Identity, Request) {
    let key = Ed25519KeyPair::from_seed_unchecked(&SEED).unwrap();
    let trust = Trust {
        network: [1; 16],
        authority: key.public_key().as_ref().try_into().unwrap(),
        genesis: [3; 32],
    };
    let identity = age::Identity::from_secret_bytes([19; 32]);
    let binding = super::super::binding::Request::create(
        trust.network,
        trust.authority,
        [5; 16],
        [6; 16],
        &identity,
        &[23; 32],
    )
    .unwrap()
    .binding()
    .clone();
    let request = Request {
        account: [7; 32],
        generation: 1,
        parents: vec![],
        action: Action::Prepare(Box::new(Preparation {
            table: [8; 32],
            key: "m3.2".into(),
            proposal: [9; 32],
            configuration: [10; 32],
            provider: "anthropic".into(),
            method: "cli".into(),
            device: binding,
            intent: None,
            secret: Some(SecretRef {
                vault: "lab".into(),
                signer: [11; 32],
                entry: "credential".into(),
            }),
        })),
    };
    (trust, identity, request)
}

#[test]
fn fleet_intent_seals_its_binding_to_the_issuer_and_each_device_lane() {
    let (trust, identity, request) = fixture();
    let Action::Prepare(legacy) = &request.action else {
        unreachable!()
    };
    let terms = Intent {
        table: legacy.table,
        key: legacy.key.clone(),
        proposal: legacy.proposal,
        configuration: legacy.configuration,
        provider: legacy.provider.clone(),
        method: legacy.method.clone(),
        issuer: legacy.device.clone(),
        secret: legacy.secret.clone(),
    };
    let hash = SecretBytes::new(vec![42; 32]);
    assert!(terms.issued_by(&legacy.device, 1));
    let successor = super::super::binding::Request::create(
        trust.network,
        trust.authority,
        *legacy.device.machine(),
        [88; 16],
        &identity,
        &[24; 32],
    )
    .unwrap()
    .binding()
    .clone();
    assert_ne!(successor.actor(), legacy.device.actor());
    assert!(terms.issued_by(&successor, 1));
    assert!(!terms.issued_by(&successor, 0));
    assert!(!terms.issued_by(&successor, 2));
    for (network, authority, machine) in [
        ([55; 16], trust.authority, *legacy.device.machine()),
        (trust.network, [55; 32], *legacy.device.machine()),
        (trust.network, trust.authority, [55; 16]),
    ] {
        let other = super::super::binding::Request::create(
            network, authority, machine, [88; 16], &identity, &[24; 32],
        )
        .unwrap()
        .binding()
        .clone();
        assert!(!terms.issued_by(&other, 1));
    }
    let mut intent_request = request.clone();
    intent_request.action = Action::Intent(Box::new(terms.clone()));
    let intent = Record::issue(
        &SEED,
        trust,
        intent_request.clone(),
        [12; 32],
        0,
        Some(&hash),
    )
    .unwrap();
    assert_eq!(
        Record::verify(intent.bytes(), trust).unwrap().request(),
        &intent_request
    );
    assert!(intent.matches_issuer_entry(&SEED, hash.as_slice()).unwrap());
    assert!(!intent.matches_issuer_entry(&SEED, &[43; 32]).unwrap());
    assert!(
        intent
            .matches_issuer_entry(&[18; 32], hash.as_slice())
            .is_err()
    );
    assert!(age::decrypt(std::slice::from_ref(&identity), &intent.capsule).is_err());
    assert!(
        !intent
            .bytes()
            .windows(32)
            .any(|part| part == hash.as_slice())
    );
    let reference = IntentRef {
        account: intent.request().account,
        decision: intent.id(),
    };
    let preparation = terms.for_device(reference, legacy.device.clone());
    let account = reference.device_account(&preparation.device);
    let mut later_reference = reference;
    later_reference.decision = [99; 32];
    assert_eq!(account, later_reference.device_account(&preparation.device));
    let child_request = Request {
        account,
        action: Action::Prepare(Box::new(preparation)),
        ..request
    };
    let child = Record::issue(
        &SEED,
        trust,
        child_request.clone(),
        [12; 32],
        0,
        Some(&hash),
    )
    .unwrap();
    assert_eq!(
        Record::verify(child.bytes(), trust).unwrap().request(),
        &child_request
    );
    assert!(child.matches_entry(&identity, hash.as_slice()).unwrap());
    assert!(child.matches_issuer_entry(&SEED, hash.as_slice()).unwrap());
    assert!(!child.matches_issuer_entry(&SEED, &[43; 32]).unwrap());
    assert!(
        !child
            .bytes()
            .windows(32)
            .any(|part| part == hash.as_slice())
    );
}
fn resign(bytes: &mut [u8]) {
    let n = bytes.len() - 64;
    let key = Ed25519KeyPair::from_seed_unchecked(&SEED).unwrap();
    let signature = key.sign(&[DOMAIN, &bytes[..n]].concat());
    bytes[n..].copy_from_slice(signature.as_ref());
}

#[test]
fn exact_root_signed_bytes_and_private_binding_round_trip() {
    let (trust, identity, request) = fixture();
    let secret_hash = SecretBytes::new(vec![42; 32]);
    let record = Record::issue(
        &SEED,
        trust,
        request.clone(),
        [12; 32],
        u64::MAX,
        Some(&secret_hash),
    )
    .unwrap();
    let decoded = Record::verify(record.bytes(), trust).unwrap();
    assert_eq!(decoded.request(), &request);
    assert_eq!(decoded.authored_ms(), u64::MAX);
    assert_eq!(decoded.id(), record.id());
    assert!(
        decoded
            .matches_entry(&identity, secret_hash.as_slice())
            .unwrap()
    );
    assert!(!decoded.matches_entry(&identity, &[43; 32]).unwrap());
    assert!(
        !decoded
            .matches_entry(
                &age::Identity::from_secret_bytes([20; 32]),
                secret_hash.as_slice()
            )
            .unwrap()
    );
    assert!(
        !record
            .bytes()
            .windows(32)
            .any(|part| part == secret_hash.as_slice())
    );
    assert!(!format!("{record:?}").contains(&format!("{secret_hash:?}")));
}

#[test]
fn malformed_forged_and_cross_context_decisions_are_refused() {
    let (trust, identity, request) = fixture();
    let hash = SecretBytes::new(vec![42; 32]);
    let record = Record::issue(&SEED, trust, request.clone(), [12; 32], 0, Some(&hash)).unwrap();
    for n in 0..record.bytes().len() {
        if n % 17 == 0 {
            assert!(Record::verify(&record.bytes()[..n], trust).is_err());
        }
    }
    for at in [
        0,
        4,
        20,
        52,
        84,
        116,
        125,
        record.bytes().len() - 65,
        record.bytes().len() - 1,
    ] {
        let mut wire = record.bytes().to_vec();
        wire[at] ^= 1;
        assert!(Record::verify(&wire, trust).is_err());
    }
    assert!(Record::verify(&vec![0; MAX_RECORD + 1], trust).is_err());
    let mut other = trust;
    other.genesis = [13; 32];
    assert!(Record::verify(record.bytes(), other).is_err());
    other = trust;
    other.authority = [14; 32];
    assert!(Record::verify(record.bytes(), other).is_err());
    // A root-signed outer context change cannot relabel the sealed inner binding.
    let mut relabeled = record.bytes().to_vec();
    relabeled[84] ^= 1;
    resign(&mut relabeled);
    let relabeled = Record::verify(&relabeled, trust).unwrap();
    assert!(!relabeled.matches_entry(&identity, hash.as_slice()).unwrap());
    let mut malformed = record.bytes().to_vec();
    malformed.extend_from_slice(&[0]);
    resign(&mut malformed);
    assert!(Record::verify(&malformed, trust).is_err());
}

#[test]
fn no_binding_means_no_credential_and_invalid_requests_cannot_be_signed() {
    let (trust, identity, mut request) = fixture();
    assert!(Record::issue(&SEED, trust, request.clone(), [12; 32], 0, None).is_err());
    let Action::Prepare(p) = &mut request.action else {
        unreachable!()
    };
    p.secret = None;
    let record = Record::issue(&SEED, trust, request.clone(), [12; 32], 0, None).unwrap();
    assert!(!record.matches_entry(&identity, &[42; 32]).unwrap());
    assert!(Record::issue(&[18; 32], trust, request.clone(), [12; 32], 0, None).is_err());
    for bad in 0..5 {
        let mut r = request.clone();
        match bad {
            0 => r.generation = 0,
            1 => r.parents = vec![[1; 32]],
            2 => {
                r.generation = 2;
                r.parents = vec![[1; 32], [1; 32]];
            }
            3 => r.account = [0; 32],
            _ => r.parents = vec![[1; 32]; MAX_PARENTS + 1],
        }
        assert!(Record::issue(&SEED, trust, r, [12; 32], 0, None).is_err());
    }
    request.action = Action::Stop;
    assert!(Record::issue(&SEED, trust, request, [12; 32], 0, None).is_ok());
}
