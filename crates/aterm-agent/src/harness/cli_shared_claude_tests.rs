// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;
use std::cell::Cell;

#[test]
fn canonical_profile_outage_is_distinct_from_a_rejected_token() {
    let valid = br#"{"account":{"uuid":"id","email":"approved@example.test"},"organization":{"uuid":"org"}}"#;
    assert!(profile_response(200, valid).is_ok());
    assert!(matches!(
        profile_response(401, valid),
        Err(ObservationError::RecoveryRequired(_))
    ));
    for status in [201, 302, 400, 403, 404, 408, 429, 500, 502, 503] {
        assert!(
            matches!(
                profile_response(status, valid),
                Err(ObservationError::Unavailable(_))
            ),
            "status {status}"
        );
    }
    for body in [b"not json".as_slice(), b"{}".as_slice()] {
        assert!(matches!(
            profile_response(200, body),
            Err(ObservationError::Unavailable(_))
        ));
    }
    assert!(matches!(
        read_claude_profile("synthetic", Duration::ZERO),
        Err(ObservationError::Unavailable(_))
    ));
    assert!(matches!(
        read_claude_profile("bad\r\nheader", Duration::from_secs(1)),
        Err(ObservationError::RecoveryRequired(_))
    ));
}

#[test]
fn shared_claude_requires_fresh_provider_identity_not_local_status_metadata() {
    let bytes = aterm_json::to_vec(
        &json!({"claudeAiOauth":{"accessToken":"synthetic", "expiresAt":4102444800000_u64}}),
    )
    .unwrap();
    assert!(
        verify_credentials(&bytes, "approved@example.test", 1_800_000_000, |_| Err(
            ObservationError::RecoveryRequired("rejected by provider".into())
        ))
        .is_err()
    );
    let wrong = profile_value(&json!({"account":{"uuid":"id", "email":"other@example.test"},"organization":{"uuid":"org"}})).unwrap();
    assert!(
        profile_value(&json!({"account":{"uuid":"id", "email":"approved@example.test"},"organization":{"uuid":"org\n"}})).is_err()
    );
    assert!(
        verify_credentials(&bytes, "approved@example.test", 1_800_000_000, |_| Ok(
            wrong
        ))
        .is_err()
    );
    let valid = profile_value(&json!({"account":{"uuid":"id", "email":"approved@example.test"},"organization":{"uuid":"org"}})).unwrap();
    let got = verify_credentials(&bytes, "APPROVED@example.test", 1_800_000_000, |_| {
        Ok(valid)
    })
    .unwrap();
    assert_eq!(got.principal, profile::principal_id("id").unwrap());
}

#[test]
fn shared_claude_missing_expired_or_header_invalid_token_never_reaches_provider() {
    for oauth in [
        json!({}),
        json!({"accessToken":"synthetic", "expiresAt":1}),
        json!({"accessToken":"synthetic\r\nheader", "expiresAt":4102444800000_u64}),
    ] {
        let called = Cell::new(false);
        let bytes = aterm_json::to_vec(&json!({"claudeAiOauth":oauth})).unwrap();
        assert!(
            verify_credentials(&bytes, "approved@example.test", 1_800_000_000, |_| {
                called.set(true);
                Err(ObservationError::Unavailable("must not execute".into()))
            })
            .is_err()
        );
        assert!(!called.get());
    }
}
