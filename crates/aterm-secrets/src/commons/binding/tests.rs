// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SEED: [u8; 32] = [11; 32];
fn request(identity: &age::Identity) -> Request {
    Request::create([2; 16], [3; 32], [5; 16], [7; 16], identity, &SEED).unwrap()
}
fn issue(request: &Request) -> (Pending, Vec<u8>) {
    Pending::issue(request, [2; 16], [3; 32]).unwrap()
}

#[test]
fn both_keys_are_required_and_success_consumes_exactly_one_challenge() {
    let identity = age::Identity::from_secret_bytes([13; 32]);
    let request = request(&identity);
    let decoded = Request::decode(request.bytes()).unwrap();
    let (mut pending, challenge) = issue(&decoded);
    assert!(pending.is_live());
    let stranger = age::Identity::from_secret_bytes([17; 32]);
    assert!(request.answer(&stranger, &SEED, &challenge).is_err());
    assert!(request.answer(&identity, &[19; 32], &challenge).is_err());
    let answer = request.answer(&identity, &SEED, &challenge).unwrap();
    let proof = pending.verify(&answer).unwrap();
    assert_eq!(proof.binding(), request.binding());
    assert!(pending.verify(&answer).is_err());
    assert!(!pending.is_live());
    assert!(pending.plain.is_none());
}

#[test]
fn copied_recipient_and_writer_self_signature_do_not_prove_age_possession() {
    let victim = age::Identity::from_secret_bytes([13; 32]);
    let request = request(&victim);
    // All request fields, including the victim's recipient, are public. The
    // attacker even owns this request's writer; neither grants the age secret.
    let (mut pending, challenge) = issue(&request);
    let wrong = age::Identity::from_secret_bytes([17; 32]);
    assert!(age::decrypt(&[wrong], &challenge).is_err());
    let forged = pair(&SEED).unwrap().sign(&challenge);
    let mut response = b"ACP1".to_vec();
    response.extend_from_slice(&Sha256::digest(&challenge));
    response.extend_from_slice(forged.as_ref());
    assert!(pending.verify(&response).is_err());
    let real = request.answer(&victim, &SEED, &challenge).unwrap();
    assert!(pending.verify(&real).is_ok(), "failure must not consume it");
}

#[test]
fn every_bound_field_and_request_signature_resists_substitution() {
    let identity = age::Identity::from_secret_bytes([13; 32]);
    let request = request(&identity);
    assert!(Pending::issue(&request, [4; 16], [3; 32]).is_err());
    assert!(Pending::issue(&request, [2; 16], [4; 32]).is_err());
    for index in [4, 20, 52, 68, 100, 132, REQUEST_LEN - 1] {
        let mut wire = request.bytes().to_vec();
        wire[index] ^= 1;
        assert!(Request::decode(&wire).is_err(), "field at {index}");
    }
    for length in 0..REQUEST_LEN {
        assert!(Request::decode(&request.bytes()[..length]).is_err());
    }
    let mut extra = request.bytes().to_vec();
    extra.push(0);
    assert!(Request::decode(&extra).is_err());
    let writer = pair(&SEED).unwrap();
    assert!(
        Request::create(
            [2; 16],
            writer.public_key().as_ref().try_into().unwrap(),
            [5; 16],
            [7; 16],
            &identity,
            &SEED
        )
        .is_err(),
        "authority key must not become an ordinary writer"
    );
}

#[test]
fn equivalent_x25519_encodings_cannot_register_distinct_identities() {
    let identity = age::Identity::from_secret_bytes([13; 32]);
    let request = request(&identity);
    let mut alias = identity.recipient().0;
    alias[31] |= 0x80;
    assert_eq!(
        crate::x25519::x25519(&SEED, &alias),
        crate::x25519::x25519(&SEED, &identity.recipient().0)
    );
    let mut aliases = vec![alias];
    // Every noncanonical encoding in [p, 2^255) must be rejected, including
    // p+9, whose X25519 coordinate is the ordinary base point 9.
    for low in 0xed..=0xff {
        let mut point = [0xff; 32];
        point[0] = low;
        point[31] = 0x7f;
        aliases.push(point);
    }
    for alias in aliases {
        let mut wire = request.bytes().to_vec();
        wire[68..100].copy_from_slice(&alias);
        let end = REQUEST_LEN - 64;
        let signature = pair(&SEED)
            .unwrap()
            .sign(message(REQUEST_DOMAIN, &wire[..end]).as_slice());
        wire[end..].copy_from_slice(signature.as_ref());
        assert!(Request::decode(&wire).is_err());
    }
    let mut largest = [0xff; 32];
    largest[0] = 0xec;
    largest[31] = 0x7f;
    assert!(crate::x25519::canonical_u(&largest));
    assert!(crate::x25519::canonical_u(&identity.recipient().0));
}

#[test]
fn challenge_and_response_cannot_move_between_bindings_or_issuances() {
    let identity = age::Identity::from_secret_bytes([13; 32]);
    let request = request(&identity);
    let (mut first, first_wire) = issue(&request);
    let (mut second, second_wire) = issue(&request);
    assert_ne!(first_wire, second_wire);
    assert!(
        request
            .answer(&identity, &SEED, age::armor(&first_wire).as_bytes())
            .is_err()
    );
    let mut tampered = first_wire.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(request.answer(&identity, &SEED, &tampered).is_err());
    let answer = request.answer(&identity, &SEED, &first_wire).unwrap();
    assert!(second.verify(&answer).is_err());
    // Even an identical plaintext re-encrypted to the same recipient is another
    // challenge. Moving its response to the old ID must invalidate the signature.
    let plain = age::decrypt(std::slice::from_ref(&identity), &first_wire).unwrap();
    let rewrapped = age::encrypt(&[identity.recipient()], plain.as_slice()).unwrap();
    let mut rebound = request.answer(&identity, &SEED, &rewrapped).unwrap();
    rebound[4..36].copy_from_slice(&Sha256::digest(&first_wire));
    assert!(first.verify(&rebound).is_err());
    for variant in 0..4 {
        let mut network = [2; 16];
        let mut authority = [3; 32];
        let mut machine = [5; 16];
        let mut incarnation = [7; 16];
        match variant {
            0 => network[0] ^= 1,
            1 => authority[0] ^= 1,
            2 => machine[0] ^= 1,
            _ => incarnation[0] ^= 1,
        }
        let other =
            Request::create(network, authority, machine, incarnation, &identity, &SEED).unwrap();
        assert!(other.answer(&identity, &SEED, &first_wire).is_err());
    }
    let mut trailing = answer.clone();
    trailing.push(0);
    assert!(first.verify(&trailing).is_err());
    assert!(first.verify(&answer).is_ok());
}

#[test]
fn expiry_is_monotonic_and_oversized_inputs_never_consume_a_live_challenge() {
    let identity = age::Identity::from_secret_bytes([13; 32]);
    let request = request(&identity);
    let (mut pending, challenge) = issue(&request);
    let answer = request.answer(&identity, &SEED, &challenge).unwrap();
    assert!(
        request
            .answer(&identity, &SEED, &vec![0; MAX_CHALLENGE + 1])
            .is_err()
    );
    assert!(pending.verify(&[0; RESPONSE_LEN + 1]).is_err());
    assert!(pending.verify_elapsed(&answer, CHALLENGE_LIFETIME).is_err());
    assert!(
        pending.verify_elapsed(&answer, Duration::ZERO).is_err(),
        "elapsed time cannot revive it"
    );
    assert!(pending.plain.is_none());
    assert!(!pending.is_live());
}

fn possession_transition(
    state: &aterm_spec::interp::State,
    action: &str,
    malformed: usize,
) -> aterm_spec::interp::State {
    let identity = age::Identity::from_secret_bytes([13; 32]);
    let request = request(&identity);
    let (mut pending, wire) = issue(&request);
    let response = request.answer(&identity, &SEED, &wire).unwrap();
    if state["proofs"] > 0 {
        pending.verify_elapsed(&response, Duration::ZERO).unwrap();
    }
    if state["expired"] > 0 {
        assert!(pending.verify_elapsed(&[], CHALLENGE_LIFETIME).is_err());
    }
    let mut next = state.clone();
    match action {
        "Issue" => {
            next.insert("issued", 1);
        }
        "ValidProof" => {
            if pending.verify_elapsed(&response, Duration::ZERO).is_ok() {
                *next.get_mut("proofs").unwrap() += 1;
            }
        }
        "InvalidProof" => {
            let mut bad = response.clone();
            match malformed {
                0 => bad.clear(),
                1 => bad[4] ^= 1,
                _ => bad[36] ^= 1,
            }
            if pending.verify_elapsed(&bad, Duration::ZERO).is_ok() {
                next.insert("invalid", 1);
            }
        }
        "Expire" => {
            if pending
                .verify_elapsed(&response, CHALLENGE_LIFETIME)
                .is_ok()
            {
                *next.get_mut("proofs").unwrap() += 1;
            }
            if state["proofs"] == 0 {
                next.insert("expired", 1);
            }
        }
        other => panic!("unknown action {other}"),
    }
    next.insert("pending", i64::from(pending.plain.is_some()));
    next
}

#[test]
fn actual_challenge_transitions_conform_to_every_reachable_model_state() {
    use aterm_spec::interp::{State, reachable_states};
    let model = aterm_models_db::device_possession_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in reachable_states(&model) {
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let variants = if action.name == "InvalidProof" { 3 } else { 1 };
            for malformed in 0..variants {
                let next = possession_transition(&state, action.name, malformed);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {} -> {next:?}",
                    action.name
                );
            }
        }
    }
    // Independent negative controls: accepting a forged answer, reusing a valid
    // proof, ignoring expiry, or burning an invalid attempt each breaks a law.
    let bad = |pending, proofs, invalid, expired| -> State {
        [
            ("issued", 1),
            ("pending", pending),
            ("proofs", proofs),
            ("invalid", invalid),
            ("expired", expired),
        ]
        .into_iter()
        .collect()
    };
    assert!(!model.check_invariant("InvalidNeverProves", &bad(1, 0, 1, 0)));
    assert!(!model.check_invariant("SingleUse", &bad(0, 2, 0, 0)));
    assert!(!model.check_invariant("ExpiredNeverProves", &bad(0, 1, 0, 1)));
    assert!(!model.check_invariant("FailurePreservesChallenge", &bad(0, 0, 0, 0)));
    let live = bad(1, 0, 0, 0);
    assert!(
        !model
            .successors("InvalidProof", &live)
            .contains(&bad(1, 0, 1, 0))
    );
    assert!(
        !model
            .successors("InvalidProof", &live)
            .contains(&bad(0, 0, 0, 0))
    );
    assert!(
        !model
            .successors("ValidProof", &bad(0, 1, 0, 0))
            .contains(&bad(0, 2, 0, 0))
    );
    assert!(
        !model
            .successors("ValidProof", &bad(0, 0, 0, 1))
            .contains(&bad(0, 1, 0, 1))
    );
}
