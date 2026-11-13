// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_canonical_unauthorized_is_a_renewal_signal() {
    assert!(matches!(
        response(401, b"", "owner@gmail.com"),
        Err(Error::CredentialRejected)
    ));
    for status in [200, 302, 403] {
        assert!(matches!(
            response(status, b"", "owner@gmail.com"),
            Err(Error::Refused(_))
        ));
    }
    assert!(matches!(
        response(429, b"", "owner@gmail.com"),
        Err(Error::RateLimited)
    ));
    for status in [408, 500, 502, 503, 504, 599] {
        assert!(matches!(
            response(status, b"", "owner@gmail.com"),
            Err(Error::Unavailable)
        ));
    }
    let other = br#"{"sub":"1234","email":"other@gmail.com","email_verified":true}"#;
    assert!(matches!(
        response(200, other, "owner@gmail.com"),
        Err(Error::Refused(_))
    ));
    assert!(matches!(
        read("", "owner@gmail.com", Duration::ZERO),
        Err(Error::Refused(_))
    ));
}

#[test]
fn canonical_google_identity_requires_verified_exact_account() {
    let valid = br#"{"sub":"1234","email":"Owner+cli@gmail.com","email_verified":true}"#;
    let first = response(200, valid, "owner+cli@gmail.com").unwrap();
    assert!(response(200, valid, "owner@gmail.com").is_err());
    assert!(response(302, valid, "owner+cli@gmail.com").is_err());
    assert!(response(401, valid, "owner+cli@gmail.com").is_err());
    for invalid in [
        br#"{"sub":"1234","email":"owner+cli@gmail.com","email_verified":false}"#.as_slice(),
        br#"{"sub":"1234","email":"owner+cli@gmail.com","email_verified":"true"}"#,
        br#"{"sub":"","email":"owner+cli@gmail.com","email_verified":true}"#,
    ] {
        assert!(response(200, invalid, "owner+cli@gmail.com").is_err());
    }
    let changed = br#"{"sub":"5678","email":"owner+cli@gmail.com","email_verified":true}"#;
    assert_ne!(
        first.principal,
        response(200, changed, "owner+cli@gmail.com")
            .unwrap()
            .principal
    );
}
