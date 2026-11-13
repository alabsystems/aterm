// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MailboxConfig, MailboxCredential};

#[test]
fn mailbox_credential_debug_hides_codes_and_magic_links() {
    for credential in [
        MailboxCredential::Otp("674291".into()),
        MailboxCredential::MagicLink("https://example.invalid/synthetic-private-link".into()),
    ] {
        for output in [format!("{credential:?}"), format!("{credential:#?}")] {
            assert!(!output.contains("674291"));
            assert!(!output.contains("synthetic-private-link"));
            assert!(output.contains("<redacted>"));
        }
    }
}

#[test]
fn mailbox_debug_does_not_disclose_credentials_or_identity() {
    let marker = "synthetic-private-mailbox-value";
    let config = MailboxConfig {
        bearer_token: Some(marker.into()),
        password: Some(marker.into()),
        mailbox_password: Some(marker.into()),
        totp_secret: Some(marker.into()),
        user: Some(marker.into()),
        http_url: Some(marker.into()),
        passwords: [(marker.into(), marker.into())].into(),
        mailbox_passwords: [(marker.into(), marker.into())].into(),
        totp_secrets: [(marker.into(), marker.into())].into(),
        ..MailboxConfig::default()
    };
    for output in [format!("{config:?}"), format!("{config:#?}")] {
        assert!(!output.contains(marker));
        assert!(output.contains("MailboxConfig"));
    }
}
