// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn unit() -> Unit {
    Unit {
        program: "/inert/aterm".into(),
        args: vec!["secrets".into(), "serve".into()],
        definition: "inert test definition".into(),
        image: "a".repeat(64),
        executable_sha256: "b".repeat(64),
    }
}
fn health() -> HealthRequirement {
    HealthRequirement {
        vault: "team".into(),
        signer: "c".repeat(64),
        records: vec!["d".repeat(64)],
        frontier: 17,
    }
}
fn proof() -> NativeProof {
    // Private fixture for checking custody/accessor behavior. This never reaches a
    // production callback and is not evidence that native operations ran in the test.
    NativeProof {
        job: Job::Server,
        unit: unit(),
        pid: 42,
        process_identity: "42@7:3".into(),
        challenge: "e".repeat(64),
        health: health(),
        server_endpoint: Some("http://127.0.0.1:7484/v1/team".into()),
    }
}
fn foreign_backend() -> NativeBackend {
    NativeBackend::new(
        Paths::for_home(Path::new("/__aterm_foreign_service_proof__")),
        None,
        149,
    )
}

#[test]
fn native_proof_is_bound_to_the_complete_unit_and_cleared_before_failed_attempts() {
    let mut native = foreign_backend();
    native.proof = Some(proof());
    assert!(native.proof_for(Job::Server, &unit()).is_some());
    assert!(native.proof_for(Job::Subscriber, &unit()).is_none());
    let mut changed = unit();
    changed.definition.push_str(" changed");
    assert!(native.proof_for(Job::Server, &changed).is_none());
    // Foreign configured paths refuse before reading services or materialized secrets.
    assert!(native.prove(Job::Server, &unit(), &health()).is_err());
    assert!(native.proof_for(Job::Server, &unit()).is_none());
    native.proof = Some(proof());
    assert!(native.install(Job::Server, &unit()).is_err());
    assert!(native.proof_for(Job::Server, &unit()).is_none());
}

#[test]
fn native_request_change_discards_previous_proof_and_constructor_keeps_explicit_floor() {
    let mut native = foreign_backend();
    assert_eq!(native.running_build, 149);
    native.proof = Some(proof());
    native.set_signed_request(json!({"request":"different"}));
    assert!(native.proof_for(Job::Server, &unit()).is_none());
}

fn outcome_fixture() -> (aterm_net::fleet::NodeIdentity, Outcome) {
    let node = "0123456789abcdef0123456789abcdef";
    let identity = aterm_net::fleet::NodeIdentity::from_seed(node, &[7; 32]).unwrap();
    let outcome = Outcome {
        request: Request {
            node: node.into(),
            requester: node.into(),
            nonce: "outcome-signing-test".into(),
            job: Job::Server,
            current_image: "old".into(),
            desired_image: "new".into(),
            precondition: Observed {
                unit: Some(unit()),
                off: false,
                pin: None,
            },
            unit: unit(),
            health: health(),
            required_protocols: vec![1],
        },
        verdict: transaction::Verdict::RolledBack,
        detail: "lab caller rejection after positive native proof".into(),
    };
    (identity, outcome)
}

#[test]
fn shared_outcome_signature_binds_exact_native_result_and_target() {
    let (identity, mut outcome) = outcome_fixture();
    let signed = signed_outcome_as(&outcome, &identity).unwrap();
    let body = aterm_json::to_vec(&signed["outcome"]).unwrap();
    assert!(aterm_net::fleet::verifies_record(
        &identity.key(),
        "aterm-service-outcome/1",
        &body,
        &signature(&signed).unwrap()
    ));
    outcome.detail.push_str(" changed");
    assert!(!aterm_net::fleet::verifies_record(
        &identity.key(),
        "aterm-service-outcome/1",
        &aterm_json::to_vec(&transaction::outcome_value(&outcome)).unwrap(),
        &signature(&signed).unwrap()
    ));
    outcome.request.node = "another-node".into();
    assert!(signed_outcome_as(&outcome, &identity).is_err());
}

#[test]
fn offline_outcome_pins_real_signature_target_domain_and_every_request_field() {
    fn set(value: &mut Value, path: &[&str], replacement: Value) {
        let (last, parents) = path.split_last().unwrap();
        let mut target = value;
        for key in parents {
            target = target.as_object_mut().unwrap().get_mut(*key).unwrap();
        }
        target
            .as_object_mut()
            .unwrap()
            .insert((*last).into(), replacement);
    }
    let (identity, outcome) = outcome_fixture();
    let signed = signed_outcome_as(&outcome, &identity).unwrap();
    let request = transaction::request_value(&outcome.request);
    let bytes = aterm_json::to_vec(&signed).unwrap();
    let check = |signed: &Value, request: &Value, node: &str, key: &str| {
        verify_outcome(
            &aterm_json::to_vec(signed).unwrap(),
            &aterm_json::to_vec(request).unwrap(),
            node,
            key,
        )
    };
    assert_eq!(
        check(&signed, &request, identity.node(), &identity.key().hex()).unwrap(),
        outcome
    );
    // Existing signed-request files are accepted as expected input, never as trust roots.
    let wrapped = json!({"request":request,"key":"untrusted metadata","signature":"ignored"});
    let lab_report = json!({"signed_outcome":signed,"acceptance_passed":false});
    assert_eq!(
        check(
            &lab_report,
            &wrapped,
            identity.node(),
            &identity.key().hex()
        )
        .unwrap(),
        outcome
    );
    let mut bad_report = lab_report;
    set(
        &mut bad_report,
        &["signed_outcome", "outcome", "detail"],
        json!("forged lab result"),
    );
    assert!(
        check(
            &bad_report,
            &request,
            identity.node(),
            &identity.key().hex()
        )
        .is_err()
    );
    assert_eq!(
        check(&signed, &wrapped, identity.node(), &identity.key().hex()).unwrap(),
        outcome
    );
    let stranger = aterm_net::fleet::NodeIdentity::from_seed(identity.node(), &[8; 32]).unwrap();
    assert!(check(&signed, &request, identity.node(), &stranger.key().hex()).is_err());
    // Substituting the envelope key too must still fail against a valid pinned key.
    let mut changed = signed.clone();
    set(&mut changed, &["key"], json!(stranger.key().hex()));
    assert!(check(&changed, &request, identity.node(), &stranger.key().hex()).is_err());
    assert!(
        check(
            &signed,
            &request,
            "fedcba9876543210fedcba9876543210",
            &identity.key().hex()
        )
        .is_err()
    );
    let mut changed = signed.clone();
    set(
        &mut changed,
        &["signature"],
        json!(hex(&identity.sign_record(
            REQUEST_DOMAIN,
            &aterm_json::to_vec(&signed["outcome"]).unwrap()
        ))),
    );
    assert!(check(&changed, &request, identity.node(), &identity.key().hex()).is_err());
    for field in [
        "nonce",
        "unit",
        "health",
        "requester",
        "precondition",
        "required_protocols",
    ] {
        let mut expected = request.clone();
        match field {
            "nonce" => set(&mut expected, &["nonce"], json!("different-nonce-123456")),
            "unit" => set(&mut expected, &["unit", "program"], json!("/another/aterm")),
            "health" => set(&mut expected, &["health", "frontier"], json!(99999)),
            "requester" => set(&mut expected, &["requester"], json!("another-requester")),
            "precondition" => set(&mut expected, &["precondition", "off"], json!(true)),
            _ => set(&mut expected, &["required_protocols"], json!([2])),
        }
        assert!(
            check(&signed, &expected, identity.node(), &identity.key().hex()).is_err(),
            "{field}"
        );
        let mut changed = signed.clone();
        set(&mut changed, &["outcome", "request"], expected);
        assert!(
            check(&changed, &request, identity.node(), &identity.key().hex()).is_err(),
            "{field}"
        );
    }
    for (field, value) in [
        ("verdict", json!("committed")),
        ("detail", json!("altered")),
        ("schema", json!(2)),
        ("extra", json!(true)),
    ] {
        let mut changed = signed.clone();
        set(&mut changed, &["outcome", field], value);
        assert!(
            check(&changed, &request, identity.node(), &identity.key().hex()).is_err(),
            "{field}"
        );
    }
    let request_bytes = aterm_json::to_vec(&request).unwrap();
    let mut at_limit = bytes.clone();
    at_limit.resize(MAX_SIGNED_OUTCOME_BYTES, b' ');
    let mut request_at_limit = request_bytes.clone();
    request_at_limit.resize(MAX_EXPECTED_REQUEST_BYTES, b' ');
    assert_eq!(
        verify_outcome(
            &at_limit,
            &request_at_limit,
            identity.node(),
            &identity.key().hex()
        )
        .unwrap(),
        outcome
    );
    for malformed in [b"{".as_slice(), b"null", b"[]", b"{}"] {
        assert!(
            verify_outcome(
                malformed,
                &request_bytes,
                identity.node(),
                &identity.key().hex()
            )
            .is_err()
        );
        assert!(verify_outcome(&bytes, malformed, identity.node(), &identity.key().hex()).is_err());
    }
    assert!(
        verify_outcome(
            &vec![b' '; MAX_SIGNED_OUTCOME_BYTES + 1],
            &request_bytes,
            identity.node(),
            &identity.key().hex()
        )
        .is_err()
    );
    assert!(
        verify_outcome(
            &bytes,
            &vec![b' '; MAX_EXPECTED_REQUEST_BYTES + 1],
            identity.node(),
            &identity.key().hex()
        )
        .is_err()
    );
    let mut changed = signed;
    set(&mut changed, &["signature"], json!("not-hex"));
    assert!(check(&changed, &request, identity.node(), &identity.key().hex()).is_err());
}
