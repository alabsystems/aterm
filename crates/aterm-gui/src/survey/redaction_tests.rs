// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::redact;
use super::*;

/// Whether [`redact`] changed `text`: it replaced a word or withheld the
/// line.
fn redacted(text: &str) -> bool {
    redact(text) != text
}

#[test]
fn credential_shapes_and_userinfo_lines_are_sensitive_and_prose_is_not() {
    let stripe_shaped = concat!("sk_", "live_A1B2C3D4E5F6G7H8I9J0K1L2M3N4");
    assert!(redacted("export API_KEY=very-secret"));
    assert!(redacted(stripe_shaped));
    assert!(redacted(&format!("Completed {stripe_shaped}")));
    assert!(!redacted("cargo test"));

    for secret_line in [
        "DATABASE_URL=postgres://alice:p4ssw0rd@db.example/prod",
        "export REDIS_URL=redis://cache.example/0",
        "connecting to postgres://alice:p4ssw0rd@db.example/prod",
        "API_KEY=very-secret cargo test",
        concat!("sk-", "proj-abcdefghijklmnopqrstuvwxyz0123456789"),
    ] {
        assert!(redacted(secret_line), "missed {secret_line}");
    }

    for credential in [
        concat!("sk-", "abcdefghijklmnopqrstuvwxyz012345"),
        concat!("sk", "_abcdefghijklmnopqrstuvwxyz012345"),
        "rk-abcdefghijklmnopqrstuvwxyz012345",
        "RK_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
        concat!("xoxb-", "1234567890-abcdefghijklmnopqrstuvwxyz"),
        "xoxb_1234567890_abcdefghijklmnopqrstuvwxyz",
        concat!("xoxp-", "1234567890-abcdefghijklmnopqrstuvwxyz"),
        "xoxp_1234567890_abcdefghijklmnopqrstuvwxyz",
        concat!("ghp_", "abcdefghijklmnopqrstuvwxyz0123456789"),
        concat!("github_pat_", "abcdefghijklmnopqrstuvwxyz0123456789"),
        concat!("AKIA", "IOSFODNN7EXAMPLE"),
        "ASIAIOSFODNN7EXAMPLE",
        concat!("AIza", "SyD-abcdefghijklmnopqrstuvwxyz012345"),
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "abcdefghijklmnopqrstuvwxyz0123456789",
        concat!(
            "eyJhbGciOiJIUzI1NiJ9",
            ".eyJzdWIiOiIxMjM0NTY3ODkwIn0",
            ".signature_value"
        ),
    ] {
        assert!(looks_like_raw_credential(credential), "missed {credential}");
        assert!(redacted(credential), "did not redact {credential}");
        for line in [
            format!("pushed with {credential}."),
            format!("cat /tmp/{credential}/env"),
            format!("({credential}),"),
        ] {
            let shown = redact(&line);
            assert!(!shown.contains(credential), "{line} -> {shown}");
        }
    }
    for benign in [
        "api-version",
        "running-rust-tests",
        "0123456789abcdef",
        "Ready in project2026",
        "three.part.words",
        "risk-analysis",
    ] {
        assert!(
            !looks_like_raw_credential(benign),
            "false positive {benign}"
        );
        assert!(!redacted(benign), "privacy false positive in {benign}");
    }
}

#[test]
fn structured_secret_labels_are_identifier_aware_and_bounded() {
    for sensitive in [
        "STRIPE_SECRET = sample-value",
        "export STRIPE_SECRET=sample-value",
        r#"{"STRIPE_SECRET":"sample-value"}"#,
        r#"{"stripeSecret":"sample-value"}"#,
        "dbPassword: sample-value",
        "service_passwd = sample-value",
        "refresh_token: sample-value",
        "api key: sample-value",
        r#"{"private key":"sample-value"}"#,
        "AWS_ACCESS_KEY_ID: sample-value",
        "GOOGLE_APPLICATION_CREDENTIALS=sample-value",
        "authorization: sample-value",
        "DATABASE_URL = sample-value",
    ] {
        assert!(
            has_sensitive_assignment_key(sensitive),
            "missed structured label in {sensitive}"
        );
        assert!(
            redacted(sensitive),
            "did not redact structured label in {sensitive}"
        );
    }

    for benign in [
        "keyboard_layout=ansi",
        "monkey: banana",
        r#"{"keyboard":"ansi"}"#,
        "tokenizer: enabled",
        "access_tokenizer: enabled",
        "auth_tokenizer: enabled",
        "apikeyboard=true",
        "bearerless=true",
        "passwordless=true",
        "secretary=present",
        "private keyboard: enabled",
        "access keyboard: enabled",
        "api keyboard: enabled",
        "A monkey uses a keyboard",
        "cargo test --package token-parser",
    ] {
        assert!(
            !has_sensitive_assignment_key(benign),
            "identifier false positive in {benign}"
        );
        assert!(!redacted(benign), "privacy false positive in {benign}");
    }

    let oversized = "keyboard ".repeat(MAX_SENSITIVE_SCAN_BYTES / 2);
    assert!(oversized.len() > MAX_SENSITIVE_SCAN_BYTES);
    assert!(redacted(&oversized));
    assert!(!redacted(&oversized[..MAX_SENSITIVE_SCAN_BYTES]));

    let excessive_fields = "safe=1:".repeat(MAX_SENSITIVE_FIELDS + 1);
    assert!(has_sensitive_assignment_key(&excessive_fields));
    assert!(redacted(&excessive_fields));
    let within = "safe=1:".repeat(MAX_SENSITIVE_FIELDS / 2);
    assert!(!has_sensitive_assignment_key(&within));
}
