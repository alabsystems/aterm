// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn explicit_and_standard_transport_modes() {
    assert_eq!(
        Endpoint::parse("mail.example:587", 465).unwrap().security,
        Security::StartTls
    );
    assert_eq!(
        Endpoint::parse("tls://mail.example:587", 465)
            .unwrap()
            .security,
        Security::Tls
    );
    assert_eq!(
        Endpoint::parse("TLS://mail.example:587", 465)
            .unwrap()
            .security,
        Security::Tls
    );
    assert_eq!(
        Endpoint::parse("STARTTLS://mail.example:993", 993)
            .unwrap()
            .security,
        Security::StartTls
    );
    assert_eq!(Endpoint::parse("[::1]:993", 993).unwrap().host, "::1");
    for invalid in ["", "host:0", "host:65536", "user@host", "host/path", "[::1"] {
        assert!(Endpoint::parse(invalid, 993).is_err(), "{invalid}");
    }
}

/// NEGATIVE CONTROL: without rejecting `:` in unbracketed hosts, `2001:db8::587` splits at its
/// final `:` into host `"2001:db8:"` on port `587` (`StartTls`), and without control-character
/// checks in `Authentication::{plain, xoauth2}`, `\0` or `\x01` splits SASL credential frames.
#[test]
fn unbracketed_ipv6_non_ipv6_brackets_and_sasl_control_delimiters_fail_closed() {
    for invalid in [
        "::1",
        "2001:db8::587",
        "[not-ipv6]:993",
        "[::1]:0",
        "smtp://mail.example:587",
        "host]name:993",
    ] {
        assert!(Endpoint::parse(invalid, 993).is_err(), "{invalid}");
    }

    for (user, secret) in [
        ("", "pw"),
        ("me@example.test", ""),
        ("admin\0me@example.test", "pw"),
        ("me@example.test", "pw\0extra"),
        ("me@example.test\r\n", "pw"),
    ] {
        let pw = Authentication::Password {
            username: user.into(),
            password: Secret::new(secret.into()),
        };
        assert!(pw.plain().is_none(), "{user:?} / {secret:?}");
    }

    for (user, token) in [
        ("", "tok"),
        ("me@example.test", ""),
        ("me@example.test\x01auth=Bearer other", "tok"),
        ("me@example.test", "tok\x01extra"),
        ("me@example.test", "tok\r\n"),
    ] {
        let oauth = Authentication::OAuth {
            username: user.into(),
            access_token: Secret::new(token.into()),
        };
        assert!(oauth.xoauth2().is_none(), "{user:?}");
    }
}
