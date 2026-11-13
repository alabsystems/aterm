// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;
#[test]
fn remote_options_admit_only_explicit_paired_fields() {
    let args = vec!["--job".into(), "server".into()];
    assert!(options(&args, &["--job"]).is_ok());
    assert!(options(&["--job".into()], &["--job"]).is_err());
    assert!(
        options(
            &[
                "--job".into(),
                "server".into(),
                "--job".into(),
                "subscriber".into()
            ],
            &["--job"]
        )
        .is_err()
    );
    assert!(options(&["--program".into(), "/bin/sh".into()], &["--job"]).is_err());
}
#[test]
fn tampered_result_signature_is_rejected() {
    let identity =
        aterm_net::fleet::NodeIdentity::from_seed("0123456789abcdef0123456789abcdef", &[7; 32])
            .unwrap();
    let payload = json!({"state":"clean"});
    let signature = identity.sign_record(RESULT_DOMAIN, &aterm_json::to_vec(&payload).unwrap());
    let mut reply = json!({"payload":payload,"node":identity.node(),"key":identity.key().hex(),"signature":aterm_codec::hex::encode_lower(&signature)});
    assert!(reply_payload(&reply).is_ok());
    reply
        .as_object_mut()
        .unwrap()
        .insert("payload".into(), json!({"state":"committed"}));
    assert!(reply_payload(&reply).is_err());
}

#[test]
fn authentic_results_still_must_match_the_requested_operation() {
    let input = json!({"job":"server","image":"new-image","nonce":"requested-nonce"});
    let result = json!({"job":aterm_secrets::service::Job::Server.label(),"desired_image":"new-image","nonce":"requested-nonce"});
    assert!(result_matches("prepare", &input, &result).is_ok());
    for field in ["job", "image", "nonce"] {
        let mut other = input.clone();
        other
            .as_object_mut()
            .unwrap()
            .insert(field.into(), json!("different"));
        assert!(result_matches("prepare", &other, &result).is_err());
    }
    let activate = json!({"signed":{"request":result.clone()}});
    assert!(
        result_matches(
            "activate",
            &activate,
            &json!({"schema":1,"verdict":"committed","request":result})
        )
        .is_ok()
    );
    assert!(result_matches("activate", &activate, &json!({"request":{"nonce":"other"}})).is_err());
    assert!(result_matches("recover", &input, &json!({"state":"clean"})).is_ok());
    assert!(
        result_matches(
            "recover",
            &json!({"job":"bogus"}),
            &json!({"state":"clean"})
        )
        .is_err()
    );
    assert!(
        result_matches(
            "recover",
            &input,
            &json!({"state":"clean","job":aterm_secrets::service::Job::Subscriber.label()})
        )
        .is_err()
    );
    assert!(
        result_matches(
            "recover",
            &input,
            &json!({"request":{"job":aterm_secrets::service::Job::Subscriber.label()}})
        )
        .is_err()
    );
    assert!(
        result_matches(
            "inspect",
            &input,
            &json!({"job":aterm_secrets::service::Job::Server.label(),"unit":null,"off":false,"pin":null})
        )
        .is_ok()
    );
    assert!(result_matches("inspect", &input, &json!({"state":"clean"})).is_err());
    assert!(
        result_matches(
            "inspect",
            &input,
            &json!({"job":aterm_secrets::service::Job::Subscriber.label(),"unit":null,"off":false,"pin":null})
        )
        .is_err()
    );
    // Invalid local options refuse before dialing any remote host.
    assert!(
        execute(&[
            "--host".into(),
            "unreachable.invalid".into(),
            "inspect".into(),
            "--job".into(),
            "bogus".into(),
        ])
        .is_err()
    );
    assert!(
        execute(&[
            "--host".into(),
            "unreachable.invalid".into(),
            "prepare".into(),
            "--job".into(),
            "server".into(),
            "--image".into(),
            "not-hex".into(),
            "--nonce".into(),
            "n".into(),
            "--out".into(),
            "/tmp/out".into(),
        ])
        .is_err()
    );
    assert!(
        execute(&[
            "--host".into(),
            "unreachable.invalid".into(),
            "prepare".into(),
            "--job".into(),
            "server".into(),
            "--image".into(),
            "a".repeat(64),
            "--nonce".into(),
            "n".into(),
            "--out".into(),
            String::new(),
        ])
        .is_err()
    );
    // Standalone `--json` does not turn valid remote flags into an option-count error, while
    // invalid `--job` still fails locally before dialing.
    let json_err = execute(&[
        "--json".into(),
        "--host".into(),
        "unreachable.invalid".into(),
        "inspect".into(),
        "--job".into(),
        "bogus".into(),
        "--json".into(),
    ])
    .unwrap_err();
    assert_eq!(json_err, "job must be server or subscriber");
}
