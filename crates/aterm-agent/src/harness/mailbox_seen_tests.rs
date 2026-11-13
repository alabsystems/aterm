// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A mailbox that was never unlocked has no list to note, a held one is passed over rather
/// than waited on, and an unlocked one's list is noted with the login and target; what is
/// noted shows no address in its `Debug`.
/// NEGATIVE CONTROL: with `of_open` noting an empty list where the mailbox has none
/// (`address_list().cloned().unwrap_or_default()`), the never-unlocked mailbox is noted:
/// `` assertion failed: of_open(&mailbox, LOGIN, None).is_none() ``.
#[test]
fn only_an_unlocked_mailbox_free_now_is_noted() {
    const LOGIN: &str = "owner@proton.example";
    let mailbox = std::sync::Mutex::new(aterm_mail::proton::Proton::with_transport(Box::new(
        Refuses,
    )));
    assert!(of_open(&mailbox, LOGIN, None).is_none());
    mailbox
        .lock()
        .unwrap()
        .read_address_list(&aterm_json::json!({"Addresses": [
            {"Email": LOGIN, "Type": 1, "Status": 1, "Receive": 1},
            {"Email": "ai@example.test", "Type": 3, "Status": 1, "Receive": 1}
        ]}));
    let held = mailbox.lock().unwrap();
    assert!(of_open(&mailbox, LOGIN, None).is_none());
    drop(held);
    let seen = of_open(&mailbox, LOGIN, Some("ai@example.test")).unwrap();
    assert_eq!(seen.login, LOGIN);
    assert_eq!(seen.target.as_deref(), Some("ai@example.test"));
    assert_eq!(
        seen.addresses
            .iter()
            .map(|a| (a.own, a.receives))
            .collect::<Vec<_>>(),
        [(true, true), (false, true)]
    );
    assert!(seen.complete);
    assert!(!format!("{seen:?}").contains('@'));
    let ((), noted) = captured(|| note(&seen));
    assert_eq!(noted, vec![seen.clone()]);
    let ((), none) = captured(|| ());
    assert!(none.is_empty());
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        captured(|| {
            note(&seen);
            panic!("boom");
        })
    }));
    assert!(unwound.is_err());
    note(&seen);
    let ((), after_panic) = captured(|| ());
    assert!(after_panic.is_empty());
}

struct Refuses;

impl aterm_mail::proton::ApiTransport for Refuses {
    fn set_deadline(&mut self, _: Option<std::time::Instant>) {}
    fn request(
        &mut self,
        _: aterm_http::Method,
        _: &str,
        _: Option<aterm_json::Value>,
    ) -> Result<aterm_mail::proton::ApiResponse, aterm_mail::proton::ApiError> {
        Err(aterm_mail::proton::ApiError {
            kind: aterm_mail::proton_error::Kind::Refused,
            message: "no network in a test".into(),
        })
    }
    fn renew(&mut self) -> Result<(), aterm_mail::proton::ApiError> {
        Ok(())
    }
}
