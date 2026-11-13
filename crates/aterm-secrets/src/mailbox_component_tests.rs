// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn identities_bind_authority_and_login_without_disclosing_them() {
    assert_eq!(
        canonical_authority("imaps://[2001:0db8:0:0:0:0:0:1]/INBOX"),
        canonical_authority("imaps://[2001:db8::1]:993/other")
    );
    assert_eq!(
        configured_authority("imaps://owner:fixture@[2001:db8::1]/INBOX"),
        canonical_authority("imaps://[2001:db8::1]:993")
    );
    assert_eq!(
        configured_authority("imaps://embedded-c@gmail.com@imap.gmail.com/Archive"),
        canonical_authority("imaps://imap.gmail.com:993")
    );
    assert_eq!(
        configured_login(
            "imaps://embedded-c@gmail.com@imap.gmail.com/Archive",
            "fallback@example.test"
        )
        .as_deref(),
        Some("embedded-c@gmail.com")
    );
    assert_eq!(
        configured_login(
            "imaps://owner:p@ss@imap.example.test/INBOX",
            "fallback@example.test"
        )
        .as_deref(),
        Some("owner")
    );
    assert!(
        configured_login(
            "imaps://:fixture@imap.example.test/INBOX",
            "fallback@example.test"
        )
        .is_none()
    );
    assert!(configured_authority("imaps://@imap.example.test/INBOX").is_none());
    assert!(configured_authority("https://owner:fixture@mail.example").is_none());
    assert!(configured_authority("imaps://owner:fixture@bad host/").is_none());
    let one = Component::for_mailbox(
        "HTTPS://MAIL.EXAMPLE.:443/inbox?x=1",
        " User@Example ",
        Part::Password,
    )
    .unwrap();
    let same = Component::for_mailbox(
        "https://mail.example/elsewhere",
        "user@example",
        Part::Password,
    )
    .unwrap();
    assert_eq!(one, same);
    for (source, login) in [
        ("https://other.example", "user@example"),
        ("https://mail.example:444", "user@example"),
        ("imaps://mail.example:443", "user@example"),
        ("https://mail.example", "other@example"),
    ] {
        assert_ne!(
            one,
            Component::for_mailbox(source, login, Part::Password).unwrap()
        );
    }
    assert!(!one.key().contains("user") && !one.entry().contains("example"));
    assert!(!format!("{one:?}").contains("user@example"));
    assert_eq!(Component::parse(&one.key()), Some(one));
    assert_eq!(Component::from_entry(&one.entry()), Some(one));
    for part in [Part::Password, Part::SecondPassword, Part::Totp] {
        let component =
            Component::for_mailbox("https://mail.example", "user@example", part).unwrap();
        assert_eq!(Component::parse(&component.key()), Some(component));
        assert_eq!(Component::from_entry(&component.entry()), Some(component));
    }
}

#[test]
fn unknown_or_ambiguous_identities_and_noncanonical_keys_are_refused() {
    for source in [
        "mail.example",
        "ftp://mail.example",
        "https://user:password@mail.example",
        "https://",
        "https://mail.example:0",
    ] {
        assert!(Component::for_mailbox(source, "user@example", Part::Password).is_none());
    }
    for login in ["", " ", "user\n@example", "two users"] {
        assert!(Component::for_mailbox("https://mail.example", login, Part::Password).is_none());
    }
    for word in [
        "mailbox-field:00/password",
        "mailbox-field:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff/unknown",
        "mailbox-field:FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF/password",
    ] {
        assert!(Component::parse(word).is_none());
    }
}
