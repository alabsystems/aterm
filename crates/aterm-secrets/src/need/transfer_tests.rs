// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn delivery_transition(
    state: &std::collections::BTreeMap<&'static str, i64>,
    action: &str,
) -> std::collections::BTreeMap<&'static str, i64> {
    delivery_transition_with_binding(state, action, false)
}

fn delivery_transition_with_binding(
    state: &std::collections::BTreeMap<&'static str, i64>,
    action: &str,
    bypass_binding: bool,
) -> std::collections::BTreeMap<&'static str, i64> {
    let identity = || age::Identity::from_secret_bytes([4; 32]);
    let mut exchange = Exchange {
        vault: "fixture".into(),
        network: [1; 16],
        request: [2; 16],
        offer: [3; 32],
        custodian_nonce: [5; 32],
        holder_nonce: [6; 32],
        custodian: [7; 48],
        holder: [8; 48],
        recipient: identity().recipient(),
    };
    let expected_actor = exchange.custodian;
    let expected_recipient = exchange.recipient;
    let signer = crate::envelope::signer_public(&[9; 32]).unwrap();
    let mut writer = crate::envelope::signer_public(&[10; 32]).unwrap();
    let mut proof = exchange.prove(&[9; 32]).unwrap();
    let values = Values(vec![(
        "fixture_password".into(),
        SecretBytes::from(b"sentinel".as_slice()),
    )]);
    let mut delivery = exchange.seal(&[10; 32], &values).unwrap();
    let mut recipient_identity = identity();
    let mut elapsed = std::time::Duration::ZERO;
    let scenario = match action {
        "Valid" => 1,
        "NonCustodian" => {
            proof = exchange.prove(&[11; 32]).unwrap();
            2
        }
        "OtherRecipient" => {
            exchange.recipient = age::Identity::from_secret_bytes([12; 32]).recipient();
            // Even a valid vault signature cannot override the current binding.
            proof = exchange.prove(&[9; 32]).unwrap();
            delivery = exchange.seal(&[10; 32], &values).unwrap();
            recipient_identity = age::Identity::from_secret_bytes([12; 32]);
            3
        }
        "ReplayNonce" => {
            exchange.custodian_nonce[0] ^= 1;
            proof = exchange.prove(&[9; 32]).unwrap();
            4
        }
        "OtherCustodian" => {
            exchange.custodian[0] ^= 1;
            proof = exchange.prove(&[9; 32]).unwrap();
            delivery = exchange.seal(&[10; 32], &values).unwrap();
            5
        }
        "OtherOffer" => {
            exchange.offer[0] ^= 1;
            proof = exchange.prove(&[9; 32]).unwrap();
            6
        }
        "Expired" => {
            elapsed = LIFETIME;
            7
        }
        "WrongWriter" => {
            writer = crate::envelope::signer_public(&[13; 32]).unwrap();
            8
        }
        _ => panic!("unknown delivery action"),
    };
    let proven = if bypass_binding {
        exchange.proven(&signer, &proof)
    } else {
        exchange.proven_for(&signer, &proof, expected_actor, expected_recipient)
    };
    let accepted = proven
        && exchange
            .open(&writer, recipient_identity, elapsed, &delivery)
            .is_ok();
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next
}

#[test]
fn delivery_conforms_to_its_model_and_detects_a_recipient_bypass() {
    let model = aterm_models_db::credential_delivery_model();
    aterm_spec::interp::prove_and_catch(&model);
    let initial = model.init_state();
    for action in &model.actions {
        let actual = delivery_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    let bypassed = delivery_transition_with_binding(&initial, "OtherRecipient", true);
    assert_eq!(
        bypassed["accepted"], 1,
        "a real proof verifier without the binding check opens the redirected delivery"
    );
    assert!(
        !model
            .successors("OtherRecipient", &initial)
            .contains(&bypassed)
    );
    assert!(!model.check_invariant("OnlyBoundDelivery", &bypassed));
}

#[test]
fn custody_and_delivery_bind_every_exchange_field_and_the_lifetime() {
    let identity = || age::Identity::from_secret_bytes([4; 32]);
    let exchange = Exchange {
        vault: "fixture".into(),
        network: [1; 16],
        request: [2; 16],
        offer: [3; 32],
        custodian_nonce: [5; 32],
        holder_nonce: [6; 32],
        custodian: [7; 48],
        holder: [8; 48],
        recipient: identity().recipient(),
    };
    let signer = crate::envelope::signer_public(&[9; 32]).unwrap();
    let writer = crate::envelope::signer_public(&[10; 32]).unwrap();
    let proof = exchange.prove(&[9; 32]).unwrap();
    assert!(exchange.proven(&signer, &proof));
    assert!(
        !exchange.proven(&writer, &proof),
        "a member is no custodian"
    );
    let values = Values(vec![(
        "fixture_password".into(),
        SecretBytes::from(b"sentinel".as_slice()),
    )]);
    let delivery = exchange.seal(&[10; 32], &values).unwrap();
    assert_eq!(
        exchange
            .open(&writer, identity(), std::time::Duration::ZERO, &delivery)
            .unwrap()
            .0[0]
            .1
            .as_slice(),
        b"sentinel"
    );
    for field in 0..9 {
        let mut changed = exchange.clone();
        match field {
            0 => changed.network[0] ^= 1,
            1 => changed.request[0] ^= 1,
            2 => changed.offer[0] ^= 1,
            3 => changed.custodian_nonce[0] ^= 1,
            4 => changed.holder_nonce[0] ^= 1,
            5 => changed.custodian[0] ^= 1,
            6 => changed.holder[0] ^= 1,
            7 => changed.recipient.0[0] ^= 1,
            _ => changed.vault.push('x'),
        }
        assert!(!changed.proven(&signer, &proof), "field {field}");
        assert!(
            changed
                .open(&writer, identity(), std::time::Duration::ZERO, &delivery)
                .is_err(),
            "field {field}"
        );
    }
    assert!(
        exchange
            .open(&writer, identity(), LIFETIME, &delivery)
            .is_err()
    );
    assert!(
        exchange
            .open(&signer, identity(), std::time::Duration::ZERO, &delivery)
            .is_err()
    );
    let mut corrupted = delivery.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(
        exchange
            .open(&writer, identity(), std::time::Duration::ZERO, &corrupted)
            .is_err()
    );
    assert!(
        Values(vec![(
            "x".into(),
            SecretBytes::from(vec![1; MAX_VALUE + 1])
        )])
        .encode()
        .is_err()
    );
    assert!(Values::decode(&[255]).is_err());
    let mut trailing = values.encode().unwrap().as_slice().to_vec();
    trailing.push(0);
    assert!(Values::decode(&trailing).is_err());
}

/// Two entries encode into the one buffer sized for them (`encode` asserts it never grew, so
/// no first value is copied out of an outgrown buffer), and decode back as given, byte for
/// byte.
#[test]
fn two_values_encode_in_one_buffer_and_decode_as_given() {
    let values = Values(vec![
        ("first_token".into(), SecretBytes::from(vec![0xa5; 300])),
        ("second".into(), SecretBytes::from(b"s".as_slice())),
    ]);
    let encoded = values.encode().unwrap();
    assert_eq!(encoded.len(), 1 + (2 + 11 + 4 + 300) + (2 + 6 + 4 + 1));
    let decoded = Values::decode(encoded.as_slice()).unwrap();
    assert_eq!(decoded.0.len(), 2);
    for ((name, value), (want_name, want)) in decoded.0.iter().zip(&values.0) {
        assert_eq!((name, value.as_slice()), (want_name, want.as_slice()));
    }
}
