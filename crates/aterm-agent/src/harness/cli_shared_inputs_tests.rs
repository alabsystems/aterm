// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;

fn input() -> Value {
    json!({"schema":1,"kind":"mailbox","target":"a@example.test",
        "transport":"imaps","endpoint":"imaps://mail.example.test/INBOX",
        "user":"mail@example.test","password":"synthetic"})
}
fn decode(value: &Value, target: &str) -> Result<MailboxInputs, String> {
    MailboxInputs::decode(&aterm_json::to_vec(value).unwrap(), target)
}
#[test]
fn bound_mailbox_has_one_exact_target_and_no_ambient_fallback() {
    let input = decode(&input(), "A@example.test").unwrap().mailbox();
    assert!(input.routed && input.dir.is_none() && !input.relay_enabled);
    assert!(input.passwords.is_empty() && input.mailboxes.is_empty());
    assert_eq!(input.user.as_deref(), Some("mail@example.test"));
    assert!(decode(&self::input(), "other@example.test").is_err());
}
#[test]
fn bound_mailbox_refuses_session_blobs_cleartext_and_ambiguous_credentials() {
    for (field, value) in [
        ("endpoint", Value::String("imap://mail.example.test".into())),
        ("refresh_token", Value::String("synthetic".into())),
        ("bearer_token", Value::String("synthetic".into())),
        ("kind", Value::String("oauth-session".into())),
    ] {
        let mut input = input();
        input.as_object_mut().unwrap().insert(field.into(), value);
        assert!(decode(&input, "a@example.test").is_err(), "{field}");
    }
}

#[test]
fn bound_https_mailbox_requires_one_tls_authentication_method() {
    let value = json!({"schema":1,"kind":"mailbox","target":"a@example.test",
        "transport":"https","endpoint":"https://mail.example.test/inbox",
        "bearer_token":"synthetic"});
    let mailbox = decode(&value, "a@example.test").unwrap().mailbox();
    assert!(mailbox.routed && mailbox.http_url.is_some() && mailbox.user.is_none());
    for endpoint in [
        "http://mail.example.test/inbox",
        "https://user:pass@mail.example.test",
        "https://mail.example.test/inbox?token=x",
    ] {
        let mut bad = value.clone();
        bad.as_object_mut()
            .unwrap()
            .insert("endpoint".into(), Value::String(endpoint.into()));
        assert!(decode(&bad, "a@example.test").is_err());
    }
}
