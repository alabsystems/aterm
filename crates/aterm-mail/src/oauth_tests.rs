// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn pkce_is_fresh_and_callback_rejects_wrong_state_before_network() {
    let first = begin("client", 1234, Some("me@gmail.com")).unwrap();
    let second = begin("client", 1234, None).unwrap();
    assert_ne!(first.state, second.state);
    assert!(first.authorize_url.contains("code_challenge_method=S256"));
    assert!(exchange("client", None, &first, "wrong", "unused").is_err());
}
#[test]
fn token_responses_validate_scope_type_expiry_and_secrets() {
    let valid = br#"{"token_type":"Bearer","access_token":"access","expires_in":3600}"#;
    assert_eq!(
        decode_response(200, valid).unwrap().access_token.expose(),
        "access"
    );
    for body in [
        br#"{"token_type":"Bearer","access_token":"","expires_in":3600}"#.as_slice(),
        br#"{"token_type":"Basic","access_token":"access","expires_in":3600}"#,
        br#"{"token_type":"Bearer","access_token":"access","expires_in":0}"#,
        br#"{"token_type":"Bearer","access_token":"access","expires_in":3600,"scope":123}"#,
    ] {
        assert!(matches!(
            decode_response(200, body),
            Err(Error {
                kind: ErrorKind::InvalidResponse
            })
        ));
    }
    assert!(matches!(decode_response(200, br#"{"token_type":"Bearer","access_token":"access","expires_in":3600,"scope":"profile"}"#), Err(Error { kind: ErrorKind::Rejected })));
    let with_nulls = br#"{"token_type":"Bearer","access_token":"access","expires_in":3600,"refresh_token":null,"scope":null}"#;
    let decoded_nulls = decode_response(200, with_nulls).unwrap();
    assert_eq!(decoded_nulls.access_token.expose(), "access");
    assert!(decoded_nulls.refresh_token.is_none());
    assert!(
        Error {
            kind: ErrorKind::InvalidResponse
        }
        .retryable()
    );
    for status in [407, 408, 425, 429, 500, 503] {
        assert!(
            decode_response(status, b"private")
                .err()
                .unwrap()
                .retryable()
        );
    }
    assert!(
        decode_response(400, br#"{"error":"temporarily_unavailable"}"#)
            .err()
            .unwrap()
            .retryable()
    );
    let rejected = decode_response(400, b"sensitive-token").err().unwrap();
    assert!(!rejected.retryable());
    assert!(!format!("{rejected:?} {rejected}").contains("sensitive"));
}

#[test]
fn expired_refresh_deadline_never_reaches_network() {
    let result = refresh_until(
        "client",
        None,
        &Secret::new("refresh".into()),
        std::time::Instant::now(),
    );
    assert!(matches!(
        result,
        Err(Error {
            kind: ErrorKind::Transport
        })
    ));
}
