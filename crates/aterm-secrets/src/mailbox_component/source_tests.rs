// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn inline_passwords_keep_significant_spaces() {
    let source = InlineSource::parse("user = 'owner@example'\nwebmail_url = 'https://mail.example'\npassword = '  fixture password  '\n").unwrap();
    let component =
        Component::for_mailbox("https://mail.example", "owner@example", Part::Password).unwrap();
    assert_eq!(
        source.value(component).unwrap().as_slice(),
        b"  fixture password  "
    );
}

#[test]
fn identical_shared_routes_coalesce_and_imap_userinfo_is_not_an_identity() {
    let source = InlineSource::parse(
        r#"
[[mailboxes]]
login = "owner@example"
imap_url = "imaps://owner%40example:fixture@imap.example/INBOX"
password = "fixture"
receives = ["first@example"]
[[mailboxes]]
login = "OWNER@example"
imap_url = "imaps://imap.example:993/Other"
password = "fixture"
receives = ["second@example"]
"#,
    )
    .unwrap();
    let component =
        Component::for_mailbox("imaps://imap.example", "owner@example", Part::Password).unwrap();
    assert_eq!(source.components().count(), 1);
    assert!(source.recognizes(component));
    assert_eq!(source.value(component).unwrap().as_slice(), b"fixture");
}

#[test]
fn shared_and_default_sources_are_bound_to_their_actual_mailboxes() {
    let source = InlineSource::parse(
        r#"
user = "owner@proton.me"
password = "fixture-password"
webmail_url = "https://mail.proton.me/u/0/inbox"
[[mailboxes]]
login = "other@example.test"
webmail_url = "https://mail.example.test/"
password = "other-password"
mailbox_password = "other-second"
receives = ["alias@example.test"]
"#,
    )
    .unwrap();
    let own = Component::for_mailbox("https://mail.proton.me", "owner@proton.me", Part::Password)
        .unwrap();
    assert_eq!(source.value(own).unwrap().as_slice(), b"fixture-password");
    let other = Component::for_mailbox(
        "https://mail.example.test",
        "other@example.test",
        Part::SecondPassword,
    )
    .unwrap();
    assert_eq!(source.value(other).unwrap().as_slice(), b"other-second");
    assert!(
        source
            .value(
                Component::for_mailbox(
                    "https://mail.example.test",
                    "alias@example.test",
                    Part::Password
                )
                .unwrap()
            )
            .is_none()
    );
    assert!(!format!("{source:?}").contains("password"));
}

#[test]
fn conflicting_fields_and_file_pointers_never_supply_credentials() {
    let source = InlineSource::parse(
        r#"
[[mailboxes]]
login = "owner@example.test"
webmail_url = "https://mail.example.test/"
password = "first"
[[mailboxes]]
login = "owner@example.test"
webmail_url = "https://mail.example.test/"
password = "second"
[[mailboxes]]
login = "file@example.test"
webmail_url = "https://mail.example.test/"
password_file = "/must/not/be/read"
"#,
    )
    .unwrap();
    assert_eq!(source.components().count(), 0);
}

/// NEGATIVE CONTROL: non-table `mailboxes` elements and cross-route defaults stay isolated
/// while top-level `[[mailboxes]]` entries beside `[mailbox]` and `username` routes are kept.
#[test]
fn nested_mailbox_table_keeps_top_level_mailboxes_and_username_alias() {
    let source = InlineSource::parse(
        r#"
mailboxes = [
  "ignored-non-table",
  { username = "embedded-c@gmail.com", imap_url = "imaps://embedded-c@gmail.com@imap.gmail.com/Archive", password = "fixture-gmail" },
]
[mailbox]
user = "owner@proton.me"
webmail_url = "https://mail.proton.me/u/0/inbox"
password = "fixture-proton"
"#,
    )
    .unwrap();
    let proton =
        Component::for_mailbox("https://mail.proton.me", "owner@proton.me", Part::Password)
            .unwrap();
    let gmail = Component::for_mailbox(
        "imaps://imap.gmail.com:993",
        "embedded-c@gmail.com",
        Part::Password,
    )
    .unwrap();
    assert!(source.recognizes(proton));
    assert!(source.recognizes(gmail));
    assert_eq!(source.value(proton).unwrap().as_slice(), b"fixture-proton");
    assert_eq!(source.value(gmail).unwrap().as_slice(), b"fixture-gmail");
}
