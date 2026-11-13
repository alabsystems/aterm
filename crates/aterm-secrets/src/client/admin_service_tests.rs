// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn admission_publishes_one_recipient_replays_and_respects_a_hold() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let server = crate::server::start_with_join_change_gap(
        std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        Duration::ZERO,
    )
    .unwrap();
    let url = format!("http://{}/v1/team", server.addr);
    let paths = Paths::for_home(&root.path().join("admin"));
    create(&paths, &url, None, Some("admin")).unwrap();
    let reported: aterm_json::Value = aterm_json::from_str(&status(&paths).unwrap()).unwrap();
    assert_eq!(reported["vault_admin"]["value"].as_bool(), Some(true));
    let queued = |host: &str| {
        let identity = machine_identity(&Paths::for_home(&root.path().join(host))).unwrap();
        let recipient = identity.recipient();
        let code = join_code(&recipient);
        transport::post_join(&url, &format!("{} {host} {code}", recipient.to_bech32())).unwrap();
        (
            recipient.to_bech32(),
            Request {
                code,
                nonce: "0123456789abcdef0123456789abcdef".into(),
            },
        )
    };
    let (first, request) = queued("first");
    let (held, held_request) = queued("held");
    let (raced, _) = queued("raced");
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_code_only(&held).unwrap();
    doc.add_code_only(&raced).unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    let pin = admin.config.signer.unwrap();
    assert!(
        approve_request_opened(&paths, &admin, &raced).is_err(),
        "a hold published after approval's baseline must win the source comparison"
    );
    assert!(approve(&paths, "fleet:test", &held_request).is_err());
    let wire = approve(&paths, "fleet:test", &request).unwrap();
    let receipt = verify_for_request(&wire, &pin, "team", "fleet:test", &request).unwrap();
    assert_eq!(receipt["recipient"].as_str(), Some(first.as_str()));
    assert!(verify_for_request(&wire, &pin, "team", "fleet:other", &request).is_err());
    assert!(verify_for_request(&wire, &pin, "another-vault", "fleet:test", &request).is_err());
    let mut changed = request.clone();
    changed.nonce = "abcdef0123456789abcdef0123456789".into();
    assert!(verify_for_request(&wire, &pin, "team", "fleet:test", &changed).is_err());
    changed = held_request.clone();
    assert!(verify_for_request(&wire, &pin, "team", "fleet:test", &changed).is_err());
    let before = admin_open(&paths).unwrap().seq;
    assert!(approve(&paths, "fleet:test", &request).is_ok());
    assert_eq!(
        admin_open(&paths).unwrap().seq,
        before,
        "a replay cannot republish"
    );
    assert!(
        !admin_open(&paths)
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|m| m.recipient == held)
    );
    let stranger = Paths::for_home(&root.path().join("stranger"));
    let absent: aterm_json::Value = aterm_json::from_str(&status(&stranger).unwrap()).unwrap();
    assert_eq!(absent["vault_admin"]["value"].as_bool(), Some(false));
    let held_lock = StateLock::take(&paths).unwrap();
    let busy: aterm_json::Value = aterm_json::from_str(&status(&paths).unwrap()).unwrap();
    assert_eq!(busy["vault_admin"]["state"].as_str(), Some("unknown"));
    assert!(busy["vault_admin"].get("value").is_none());
    drop(held_lock);
    assert!(approve(&stranger, "fleet:test", &request).is_err());
    server.stop();
    assert!(
        approve(&paths, "fleet:test", &request).is_err(),
        "a local cached success is not served-head proof"
    );
}

#[test]
fn admission_accepts_only_one_code_and_a_bounded_nonce() {
    let good = "approve abcd-efgh-ijkl-mnop 0123456789abcdef0123456789abcdef";
    assert!(Request::parse(good).is_ok());
    for bad in [
        "approve --all",
        "approve abcd-efgh-ijkl-mnop x",
        "edit",
        "approve abcd-efgh-ijkl-mnop 0123456789abcdef0123456789abcdef command=sh",
    ] {
        assert!(Request::parse(bad).is_err());
    }
}

#[test]
fn signed_receipt_rejects_changed_caller_recipient_nonce_and_trust_root() {
    let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let signer: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let payload = r#"{"caller":"fleet:m1","recipient":"age1receiver","nonce":"0123456789abcdef0123456789abcdef","host":"joining","vault":"team","sequence":5}"#;
    let signature = key.sign(&[DOMAIN, payload.as_bytes()].concat());
    let encode = |payload: &str| {
        aterm_json::to_string(&aterm_json::json!({
            "payload": payload, "signer": crate::b64::encode_padded(&signer),
            "signature": crate::b64::encode_padded(signature.as_ref()),
        }))
        .unwrap()
    };
    let wire = encode(payload);
    assert_eq!(
        verify_receipt(&wire, &signer).unwrap()["caller"].as_str(),
        Some("fleet:m1")
    );
    for changed in [
        payload.replace("m1", "m2"),
        payload.replace("receiver", "attacker"),
        payload.replace("012345", "abcdef"),
        payload.replace("joining", "other"),
        payload.replace("team", "other-vault"),
        payload.replace(":5", ":6"),
    ] {
        assert!(verify_receipt(&encode(&changed), &signer).is_err());
    }
    assert!(verify_receipt(&wire, &[8; 32]).is_err());
    let wrong_domain = key.sign(payload.as_bytes());
    let forged = aterm_json::to_string(&aterm_json::json!({"payload":payload,"signer":crate::b64::encode_padded(&signer),"signature":crate::b64::encode_padded(wrong_domain.as_ref())})).unwrap();
    assert!(verify_receipt(&forged, &signer).is_err());
}
