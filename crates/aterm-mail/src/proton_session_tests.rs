// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn message_recipients_and_body_projection_handle_cc_bcc_and_parameterized_mime() {
    let parsed = message(&json!({
        "ID": "m1",
        "Sender": {"Address": "login@anthropic.com"},
        "ToList": [{"Address": "primary@proton.me"}],
        "CCList": [{"Address": "PRIMARY@proton.me"}, {"Address": "cc@proton.me"}],
        "BCCList": [{"Address": "bcc@proton.me"}],
        "Subject": "Code",
        "Time": 1_800_000_001,
        "Unread": 1,
    }))
    .unwrap();
    assert_eq!(parsed.to, "primary@proton.me, cc@proton.me, bcc@proton.me");
    assert_eq!(
        project_body(
            "TEXT/HTML; charset=utf-8",
            "<p>Code <b>123456</b></p>".into()
        ),
        "Code 123456\n"
    );
    let multipart = "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nHello MIME\r\n--b--\r\n";
    assert_eq!(
        project_body("multipart/mixed", multipart.into()),
        "Hello MIME"
    );
}

#[test]
fn bulk_success_requires_every_item_and_is_not_a_read_success() {
    let ok = br#"{"Code":1001,"Responses":[{"ID":"a","Response":{"Code":1000}}]}"#;
    assert!(
        Proton::empty()
            .process_response("/mail/v4/messages/delete", 200, None, ok)
            .is_ok()
    );
    assert!(
        Proton::empty()
            .process_response("/mail/v4/messages", 200, None, ok)
            .is_err()
    );
    for bad in [
        br#"{"Code":1001,"Responses":[]}"#.as_slice(),
        br#"{"Code":1001,"Responses":[{"Response":{"Code":1000}},{"Response":{"Code":2001}}]}"#,
    ] {
        assert!(
            Proton::empty()
                .process_response("/mail/v4/messages/read", 200, None, bad)
                .is_err()
        );
    }
}

fn cached_message(id: &str, text: &str) -> Message {
    Message {
        id: id.into(),
        from: String::new(),
        to: String::new(),
        subject: String::new(),
        date: String::new(),
        unread: false,
        text: text.into(),
    }
}

#[test]
fn message_cache_bounds_bytes_and_evicts_least_recently_read() {
    let mut cache = MessageCache {
        max_entries: 2,
        max_bytes: 20,
        ..Default::default()
    };
    cache.insert(cached_message("a", "one"));
    cache.insert(cached_message("b", "two"));
    assert!(cache.get("a").is_some());
    cache.insert(cached_message("c", "three"));
    assert!(cache.get("b").is_none());
    assert!(cache.get("a").is_some());
    cache.insert(cached_message("huge", &"x".repeat(21)));
    assert_eq!(cache.entries.len(), 2);
    assert!(cache.bytes <= cache.max_bytes);
    cache.insert(cached_message("a", "updated"));
    assert_eq!(cache.get("a").unwrap().text, "updated");
    cache.clear();
    assert_eq!((cache.entries.len(), cache.bytes), (0, 0));
}

#[test]
fn cached_body_avoids_network_and_folder_refresh_invalidates_it() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
        responses: [reply(200, r#"{"Code":1000,"Messages":[]}"#)].into(),
        ..Default::default()
    }));
    let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
    proton.cache.insert(cached_message("one", "cached body"));
    assert_eq!(proton.read("0", "one").unwrap().text, "cached body");
    assert_eq!(state.lock().unwrap().requests, 0);
    proton.messages("0", 50).unwrap();
    assert!(proton.cache.get("one").is_none());
    assert_eq!(state.lock().unwrap().requests, 1);
}

#[derive(Default)]
struct ChannelState {
    requests: usize,
    renewals: usize,
    deadlines: Vec<Option<Instant>>,
    responses: std::collections::VecDeque<Result<ApiResponse, ApiError>>,
}

struct Channel(std::sync::Arc<std::sync::Mutex<ChannelState>>);

#[test]
fn a_failed_folder_refresh_keeps_the_last_verified_body() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
        responses: [Err("offline".into())].into(),
        ..Default::default()
    }));
    let mut proton = Proton::with_transport(Box::new(Channel(state)));
    proton.cache.insert(cached_message("one", "verified body"));
    assert!(proton.messages("0", 50).is_err());
    assert_eq!(proton.read("0", "one").unwrap().text, "verified body");
}

#[test]
fn unchanged_observations_keep_verified_bodies_and_changes_invalidate_them() {
    let header = json!({"ID":"one","Subject":"mail","Unread":1});
    let changed = json!({"ID":"one","Subject":"mail","Unread":0});
    let body =
        |header: Value| aterm_json::to_string(&json!({"Code":1000,"Messages":[header]})).unwrap();
    let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
        responses: [
            reply(200, &body(header.clone())),
            reply(200, &body(changed)),
        ]
        .into(),
        ..Default::default()
    }));
    let mut proton = Proton::with_transport(Box::new(Channel(state)));
    proton.cache.insert(cached_message("one", "verified body"));
    let first = message(&header).unwrap();
    let observed = proton.observe_messages("0", 50, &[first]).unwrap();
    assert_eq!(proton.cache.get("one").unwrap().text, "verified body");
    assert!(!proton.observe_messages("0", 50, &observed).unwrap()[0].unread);
    assert!(proton.cache.get("one").is_none());
}

impl ApiTransport for Channel {
    fn set_deadline(&mut self, end: Option<Instant>) {
        self.0.lock().unwrap().deadlines.push(end);
    }
    fn request(&mut self, _: Method, _: &str, _: Option<Value>) -> Result<ApiResponse, ApiError> {
        let mut state = self.0.lock().unwrap();
        state.requests += 1;
        state
            .responses
            .pop_front()
            .expect("unexpected repeated request")
    }

    fn renew(&mut self) -> Result<(), ApiError> {
        self.0.lock().unwrap().renewals += 1;
        Ok(())
    }
}

fn reply(status: u16, body: &str) -> Result<ApiResponse, ApiError> {
    Ok(ApiResponse {
        status,
        retry_after: None,
        body: body.as_bytes().to_vec(),
    })
}

#[test]
fn expired_and_nested_operation_budgets_restore_the_shared_transport() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
        responses: [reply(200, r#"{"Code":1000,"Messages":[]}"#)].into(),
        ..Default::default()
    }));
    let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
    let expired = Instant::now();
    assert!(proton.run_until(expired, |p| p.messages("0", 10)).is_err());
    assert_eq!(state.lock().unwrap().requests, 0);
    let end = Instant::now() + Duration::from_secs(60);
    proton
        .run_until(end, |p| {
            p.run_until(end + Duration::from_secs(60), |p| p.messages("0", 10))
        })
        .unwrap();
    assert!(proton.request_deadline.is_none());
    let state = state.lock().unwrap();
    assert_eq!(
        state.deadlines,
        vec![Some(expired), None, Some(end), Some(end), Some(end), None]
    );
}

#[test]
fn external_session_renews_once_without_rotating_browser_credentials() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
        responses: [
            reply(401, r#"{"Code":401}"#),
            reply(200, r#"{"Code":1000,"Messages":[]}"#),
        ]
        .into(),
        ..Default::default()
    }));
    let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
    assert!(!proton.refresh_if_needed().unwrap());
    assert!(
        proton
            .with_verification(|p| p.messages("0", 10), |_| panic!("not a human challenge"))
            .unwrap()
            .is_empty()
    );
    let state = state.lock().unwrap();
    assert_eq!((state.requests, state.renewals), (2, 1));
    assert!(
        proton
            .persist(std::path::Path::new("/unavailable"), "test@example.test")
            .is_err()
    );
}

#[test]
fn external_verification_renews_the_browser_session_before_requesting_a_code() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
        responses: [reply(422, r#"{"Code":9001,"Details":{"HumanVerificationMethods":["ownership-email"],"HumanVerificationToken":"private-token"}}"#),
            reply(200, r#"{"Code":1000,"Messages":[]}"#)].into(),
        ..Default::default()
    }));
    let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
    proton
        .with_verification(
            |p| p.messages("0", 10),
            |_| panic!("browser renewal should resolve the challenge"),
        )
        .unwrap();
    let state = state.lock().unwrap();
    assert_eq!((state.requests, state.renewals), (2, 1));
}

#[test]
fn rejected_external_credentials_stop_background_retries() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
        responses: [Err(ApiError {
            kind: crate::proton_error::Kind::Credentials,
            message: "credentials rejected".into(),
        })]
        .into(),
        ..Default::default()
    }));
    let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
    assert!(proton.messages("0", 10).is_err());
    assert!(proton.messages("0", 10).is_err());
    assert_eq!(state.lock().unwrap().requests, 1);
    assert_eq!(
        proton.last_failure().unwrap().kind,
        crate::proton_error::Kind::Credentials
    );
}

#[test]
fn uncertain_external_write_is_not_replayed_and_failed_channel_is_throttled() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
        responses: [Err("disconnected".into())].into(),
        ..Default::default()
    }));
    let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
    assert!(proton.mark_read("0", "one", true).is_err());
    assert!(proton.mark_read("0", "one", true).is_err());
    let state = state.lock().unwrap();
    assert_eq!((state.requests, state.renewals), (1, 0));
}

#[test]
fn revoked_access_token_renews_once_and_retries_only_the_read() {
    let mut proton = Proton::empty();
    proton.access = Secret::new("old-access".into());
    let mut reads = 0;
    let mut renewals = 0;
    let result = proton.with_recovery(
        |session| {
            reads += 1;
            session.process_response("/core/v4/addresses", 401, None, br#"{"Code":401}"#)
        },
        |_| {
            renewals += 1;
            Ok(())
        },
        |_| panic!("Expired tokens are not ownership challenges"),
    );
    assert!(result.is_err());
    assert_eq!((reads, renewals), (2, 1));
}

#[test]
fn refresh_challenge_is_verified_before_retrying_the_original_read() {
    let mut proton = Proton::empty();
    proton.access = Secret::new("old-access".into());
    let mut reads = 0;
    let mut renewals = 0;
    let result = proton.with_recovery(
        |session| {
            reads += 1;
            if reads == 1 {
                session.process_response("/core/v4/addresses", 401, None, br#"{"Code":401}"#)
            } else {
                assert!(
                    session.verification.is_none(),
                    "consumed refresh proof must not leak onto the retried read"
                );
                session.process_response("/core/v4/addresses", 200, None, br#"{"Code":1000}"#)
            }
        },
        |session| {
            renewals += 1;
            if renewals == 1 {
                session.process_response("/auth/v4/refresh", 422, None, br#"{"Code":9001,"Details":{"HumanVerificationMethods":["ownership-email"],"HumanVerificationToken":"private-token"}}"#).map(|_| ())
            } else {
                assert_eq!(
                    session.verification.as_ref().map(|v| v.token.expose()),
                    Some("proof")
                );
                Ok(())
            }
        },
        |_| Ok(crate::human::Verified { token: Secret::new("proof".into()), method: "ownership-email".into() }),
    );
    assert!(result.is_ok());
    assert!(proton.verification.is_none());
    assert_eq!((reads, renewals), (2, 2));
}

#[test]
fn invalid_refresh_token_does_not_loop_through_another_refresh() {
    let mut proton = Proton::empty();
    proton.access = Secret::new("old-access".into());
    assert!(
        proton
            .with_recovery(
                |session| session.process_response(
                    "/auth/v4/refresh",
                    422,
                    None,
                    br#"{"Code":10013}"#
                ),
                |_| panic!("A rejected refresh token must not trigger another renewal"),
                |_| panic!("A rejected refresh token is not a verification challenge"),
            )
            .is_err()
    );
}

#[test]
fn valid_saved_access_token_does_not_force_a_refresh_or_verification() {
    let credentials = crate::session::Credentials {
        uid: Secret::new("uid".into()),
        access: Secret::new("access".into()),
        refresh: Secret::new("refresh".into()),
        expires: aterm_time::unix_secs().unwrap().saturating_add(3600),
    };
    let session = Proton::resume(credentials, |_| {
        panic!("An unexpired token needs no verification")
    })
    .unwrap()
    .unwrap();
    assert_eq!(session.access.expose(), "access");
    assert!(!session.credentials_dirty);
}

#[test]
fn verification_failure_keeps_the_original_operation_and_code() {
    let mut proton = Proton::empty();
    let error = proton.with_verification(
        |proton| proton.process_response("/core/v4/addresses", 422, None, br#"{"Code":9001,"Details":{"HumanVerificationMethods":["ownership-email"],"HumanVerificationToken":"private-token"}}"#),
        |_| Err("Recovery mailbox unavailable".into()),
    ).unwrap_err();
    assert!(error.contains("account addresses"));
    assert!(error.contains("9001"));
    assert!(error.contains("Recovery mailbox unavailable"));
    assert!(!error.contains("private-token"));
    assert_eq!(
        proton.last_failure().unwrap().kind,
        crate::proton_error::Kind::Verification
    );
}

#[test]
fn verification_retries_once_and_never_retries_credentials() {
    let mut proton = Proton::empty();
    let mut calls = 0;
    let result = proton.with_verification(
        |proton| {
            calls += 1;
            if calls == 2 {
                assert_eq!(
                    proton.verification.as_ref().map(|v| v.token.expose()),
                    Some("proof")
                );
            }
            proton.process_response("/auth/v4", 422, None, br#"{"Code":9001,"Details":{"HumanVerificationMethods":["ownership-email"],"HumanVerificationToken":"private-token"}}"#)
        },
        |_| Ok(crate::human::Verified { token: Secret::new("proof".into()), method: "ownership-email".into() }),
    );
    assert!(result.is_err());
    assert_eq!(calls, 2);
    assert!(
        proton.verification.is_none(),
        "single-use verification proof must be cleared once recovery finishes"
    );
    let result = proton.with_verification(
        |proton| proton.process_response("/auth/v4", 422, None, br#"{"Code":8002}"#),
        |_| panic!("Incorrect passwords must not trigger ownership verification"),
    );
    assert!(result.unwrap_err().contains("credentials"));
    let result = proton.with_verification(
        |proton| {
            proton.process_response(
                "/auth/v4",
                422,
                None,
                br#"{"Code":9001,"Details":{"HumanVerificationMethods":["ownership-email"],"HumanVerificationToken":"bad\r\ntoken"}}"#,
            )
        },
        |_| panic!("Control-character challenge tokens must not trigger verification"),
    );
    assert!(
        result
            .unwrap_err()
            .contains("no supported verification challenge")
    );
}

#[test]
fn non_json_rate_limit_preserves_retry_and_never_becomes_bad_credentials() {
    let proton = Proton::empty();
    assert!(
        proton
            .process_response("/auth/v4/refresh", 429, Some("60"), b"unavailable")
            .is_err()
    );
    let failure = proton.last_failure().unwrap();
    assert_eq!(failure.kind, crate::proton_error::Kind::RateLimited);
    assert_eq!(failure.retry_after_seconds, Some(60));
    assert!(proton.ready().is_err());
    proton.retry_at.set(Some(std::time::Instant::now()));
    assert!(proton.ready().is_ok());
}

/// The address list keeps each address with Proton's type and whether it receives, and an
/// entry with no address or an unknown `Type` makes it partial, and an unknown `Type` fails
/// closed as the account's own; a mailbox that was never unlocked has none.
/// NEGATIVE CONTROL: with `receives` reading `Status` alone, the disabled-receive address
/// reads as receiving: `` left: [true, true, true, false] right: [true, false, true, false] ``;
/// with `AddressKind::own` matching only 1/4/5, `AddressKind::Other.own()` is `false` and an
/// untyped or future-typed login can enter Commons.
#[test]
fn the_address_list_reads_types_and_receiving() {
    assert!(Proton::empty().address_list().is_none());
    let list = AddressList::read(&json!({"Code": 1000, "Addresses": [
        {"Email": "own@proton.example", "Type": 1, "Status": 1, "Receive": 1},
        {"Email": "quiet@proton.example", "Type": 2, "Status": 1, "Receive": 0},
        {"Email": "ai@custom.example", "Type": 3, "Status": 1},
        {"Email": "off@proton.example", "Type": 2, "Status": 0, "Receive": 1},
    ]}));
    assert!(list.complete);
    assert_eq!(
        list.addresses
            .iter()
            .map(|a| a.receives)
            .collect::<Vec<_>>(),
        [true, false, true, false]
    );
    assert_eq!(
        list.addresses.iter().map(|a| a.kind).collect::<Vec<_>>(),
        [
            AddressKind::Original,
            AddressKind::Alias,
            AddressKind::Custom,
            AddressKind::Alias
        ]
    );
    assert!(
        AddressKind::Original.own()
            && AddressKind::Premium.own()
            && AddressKind::External.own()
            && AddressKind::Other.own()
    );
    assert!(!AddressKind::Custom.own() && !AddressKind::Alias.own());
    assert!(!format!("{list:?}").contains('@'));
    let unknown_type = AddressList::read(&json!({"Addresses": [
        {"Email": "future@proton.example", "Type": 99, "Status": 1, "Receive": 1}
    ]}));
    assert!(!unknown_type.complete);
    assert_eq!(unknown_type.addresses[0].kind, AddressKind::Other);
    assert!(unknown_type.addresses[0].kind.own());
    let partial = AddressList::read(&json!({"Addresses": [{"Type": 2}, {"Email": "a@x.example"}]}));
    assert!(!partial.complete);
    assert_eq!(partial.addresses.len(), 1);
    assert!(partial.addresses[0].kind.own());
    assert!(!AddressList::read(&json!({"Code": 1000})).complete);
}

#[test]
fn paused_connections_do_not_repeat_authentication_or_verification() {
    let proton = Proton::empty();
    assert!(
        proton
            .process_response("/auth/v4", 422, None, br#"{"Code":8002}"#)
            .is_err()
    );
    assert!(proton.ready().unwrap_err().contains("paused"));
    // An attempt made while paused fails before touching the transport.
    assert!(
        proton
            .call(Method::Get, "/core/v4/addresses", None)
            .unwrap_err()
            .contains("paused")
    );
    assert_eq!(proton.last_failure().unwrap().api_code, Some(8002));
}

#[test]
fn token_rotation_validates_every_field_before_replacing_credentials() {
    let mut proton = Proton::empty();
    proton.install_tokens(&json!({"UID":"uid", "AccessToken":"old-access", "RefreshToken":"old-refresh", "ExpiresIn":3600})).unwrap();
    assert!(
        proton
            .install_tokens(
                &json!({"UID":"uid", "AccessToken":"new-access", "RefreshToken":"bad\r\nheader"})
            )
            .is_err()
    );
    assert_eq!(proton.access.expose(), "old-access");
    assert_eq!(proton.refresh_token.expose(), "old-refresh");
    proton.install_tokens(&json!({"UID":"uid", "AccessToken":"new-access", "RefreshToken":"new-refresh", "ExpiresIn":3600})).unwrap();
    assert_eq!(proton.refresh_token.expose(), "new-refresh");
    assert!(proton.credentials_dirty);
}

#[test]
fn token_rotation_survives_an_unavailable_response_clock() {
    let mut proton = Proton::empty();
    proton.install_tokens_at(&json!({"UID":"uid", "AccessToken":"new-access", "RefreshToken":"new-refresh", "ExpiresIn":3600}), None).unwrap();
    assert_eq!(proton.refresh_token.expose(), "new-refresh");
    assert_eq!(proton.access.expose(), "new-access");
    assert_eq!(
        proton.expires, 0,
        "unknown expiry cannot reuse a cached session"
    );
    assert!(proton.credentials_dirty);
}

#[cfg(unix)]
#[test]
fn failed_session_write_retries_without_losing_rotated_tokens() {
    let mut nonce = [0; 8];
    aterm_uds::rand::fill(&mut nonce).unwrap();
    let home = std::env::temp_dir().join(format!(
        "aterm-mail-rotation-{}-{:x}",
        std::process::id(),
        u64::from_le_bytes(nonce)
    ));
    std::fs::write(&home, b"not a directory").unwrap();
    let mut proton = Proton::empty();
    proton.install_tokens_at(&json!({"UID":"uid", "AccessToken":"new-access", "RefreshToken":"new-refresh", "ExpiresIn":3600}), None).unwrap();
    proton.stored = Some((home.clone(), "a@example.test".into()));
    assert!(
        proton
            .refresh_if_needed_with(|_| panic!("save the pending rotation first"))
            .is_err()
    );
    assert!(proton.credentials_dirty);
    assert_eq!(proton.refresh_token.expose(), "new-refresh");
    std::fs::remove_file(&home).unwrap();
    assert!(
        proton
            .refresh_if_needed_with(|session| {
                assert!(!session.credentials_dirty);
                assert_eq!(
                    crate::session::load(&home, "a@example.test")
                        .unwrap()
                        .unwrap()
                        .refresh
                        .expose(),
                    "new-refresh"
                );
                session.expires = u64::MAX;
                Ok(())
            })
            .unwrap()
    );
    assert!(
        !proton
            .refresh_if_needed_with(|_| panic!("cached session is still valid"))
            .unwrap()
    );
    assert!(!proton.credentials_dirty);
    assert_eq!(
        crate::session::load(&home, "a@example.test")
            .unwrap()
            .unwrap()
            .refresh
            .expose(),
        "new-refresh"
    );
    std::fs::remove_dir_all(home).unwrap();
}
