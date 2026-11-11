// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn only_recovery_methods_and_valid_proofs_are_accepted() {
    assert!(supported("ownership-email"));
    assert!(supported("ownership-sms"));
    assert!(!supported("captcha"));
    assert!(!supported("payment"));
    assert_eq!(
        verified(&aterm_json::json!({"Token":"proof"}), "ownership-email")
            .unwrap()
            .token
            .expose(),
        "proof"
    );
    assert!(verified(&aterm_json::json!({"Token":""}), "ownership-email").is_err());
    assert!(
        verified(
            &aterm_json::json!({"Token":"bad\r\nheader"}),
            "ownership-email"
        )
        .is_err()
    );
    let poisoned = Challenge {
        token: Secret::new("tok\r\ninjected".into()),
        methods: vec!["ownership-email".into()],
    };
    assert!(Recovery::prepare(&poisoned).is_err());
    assert!(
        decode_details(&aterm_json::json!({
            "ChallengeDestination": "user\x1b[2J@proton.example",
            "ChallengeType": "login"
        }))
        .is_err()
    );
    assert_eq!(
        decode_details(&aterm_json::json!({
            "ChallengeDestination": "u***@proton.example",
            "ChallengeType": "login"
        }))
        .unwrap(),
        ("u***@proton.example".into(), Purpose::Login)
    );
}
