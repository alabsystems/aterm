// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_required_mailbox_command_waits_for_its_native_pipe_reply() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    for (age, parent_expired) in [(31, false), (61, false), (0, true), (0, false)] {
        let (mut commands, command_write) = aterm_uds::pipe().unwrap();
        let (event_read, mut replies) = aterm_uds::pipe().unwrap();
        let peer = std::thread::spawn(move || {
            let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            let mut seen = Vec::new();
            loop {
                let mut bytes = Vec::new();
                loop {
                    assert!(
                        aterm_uds::ready::any_until(&[commands.as_fd()], until)
                            .is_ok_and(|ready| !ready.is_empty()),
                        "owned native mail command did not arrive"
                    );
                    let mut byte = [0];
                    if commands.read(&mut byte).unwrap() == 0 {
                        return seen;
                    }
                    if byte == [0] {
                        break;
                    }
                    bytes.push(byte[0]);
                }
                let request: Value = aterm_json::from_slice(&bytes).unwrap();
                assert_eq!(request["method"].as_str(), Some("Runtime.evaluate"));
                assert_eq!(request["sessionId"].as_str(), Some("owned-mail-session"));
                assert_eq!(
                    request["params"]["expression"].as_str(),
                    Some("owned-mail-expression")
                );
                let id = request["id"].as_u64().unwrap();
                seen.push(id);
                for (id, receipt) in [(id + 100, "wrong-id"), (id, "owned-mail-reply")] {
                    let mut response =
                        aterm_json::to_vec(&json!({"id":id,"result":{"receipt":receipt}})).unwrap();
                    response.push(0);
                    replies.write_all(&response).unwrap();
                }
            }
        });
        let mut client = browser::CdpClient::new(command_write, event_read);
        let call = |client: &mut browser::CdpClient, end, started| {
            command_remaining(end, started).and_then(|wait| {
                client
                    .call(
                        Some("owned-mail-session"),
                        "Runtime.evaluate",
                        json!({"expression":"owned-mail-expression"}),
                        wait,
                    )
                    .map_err(String::from)
            })
        };
        let parent = parent_expired.then(|| Instant::now() - Duration::from_secs(1));
        let got = call(
            &mut client,
            parent,
            Instant::now() - Duration::from_secs(age),
        );
        let expired = age == 61 || parent_expired;
        let fresh = expired.then(|| call(&mut client, None, Instant::now()));
        drop(client);
        let seen = peer.join().unwrap();
        assert_eq!(
            seen,
            [1],
            "expiry publishes no command and poisons no fresh call"
        );
        if expired {
            assert!(got.is_err());
            assert_eq!(
                fresh.unwrap().unwrap(),
                json!({"receipt":"owned-mail-reply"})
            );
        } else {
            assert_eq!(got.unwrap(), json!({"receipt":"owned-mail-reply"}));
        }
    }
}

/// The webmail reader's credentials come from the lab registry's mailbox rows with no
/// mailbox file or sheet present: its login, password, second password and TOTP seed.
/// NEGATIVE CONTROL: with no registry mailbox (an empty `with_mail`), discovery finds
/// nothing and the reader says `No stored mail credentials`.
#[test]
fn the_webmail_reader_signs_in_with_the_registry_mailbox() {
    use super::super::super::{alab_roster, headless_oauth::registry_tests as fixture};
    let home = aterm_tempfile::tempdir().unwrap();
    let email = "ai1@example.test";
    let stored = alab_roster::with_mail(vec![fixture::mailbox()], fixture::secrets(), || {
        HeadlessMail::stored_credentials(
            browser::discover_mailbox_config_files(Some(home.path()), None, None),
            email,
        )
    })
    .unwrap();
    let read = stored.borrowed();
    assert_eq!(read.login, "owner@example.test");
    assert_eq!(read.password, "fixture-login-pass");
    assert_eq!(read.mailbox_password, Some("fixture-second-pass"));
    assert_eq!(read.totp_secret, Some(fixture::SEED));
    let none = alab_roster::with_mail(Vec::new(), Vec::new(), || {
        HeadlessMail::stored_credentials(
            browser::discover_mailbox_config_files(Some(home.path()), None, None),
            email,
        )
    });
    assert_eq!(none.err().as_deref(), Some("No stored mail credentials"));
}

#[test]
#[ignore = "requires ATERM_MAIL_TEST_ACCOUNT and a live authenticated profile"]
fn crashed_browser_reconnects_for_a_read() {
    use aterm_mail::proton::ApiTransport;
    aterm_uds::secret::no_core_dumps();
    let email = std::env::var("ATERM_MAIL_TEST_ACCOUNT").unwrap();
    let home = aterm_types::dirs::home_dir().unwrap();
    let session = HeadlessMail::connect(&home, &email).unwrap();
    let mut transport = BrowserTransport::new(session).unwrap();
    let _ = transport.session.as_mut().unwrap().cdp.call(
        None,
        "Browser.close",
        json!({}),
        Duration::from_secs(2),
    );
    let response = transport
        .request(aterm_http::Method::Get, "/core/v4/addresses", None)
        .unwrap();
    assert_eq!(response.status, 200);
    let body: Value = aterm_json::from_slice(&response.body).unwrap();
    assert_eq!(body["Code"].as_u64(), Some(1000));
}

#[test]
fn transport_failures_separate_credentials_from_transient_disconnects() {
    use aterm_mail::proton_error::Kind;
    assert_eq!(
        transport_error("Mail operation deadline expired".into()).kind,
        Kind::Network
    );
    assert_eq!(
        transport_error("Proton rejected the headless browser credentials".into()).kind,
        Kind::Credentials
    );
    assert_eq!(
        transport_error("Proton requested additional verification in the headless browser".into())
            .kind,
        Kind::Verification
    );
    assert_eq!(
        transport_error("Reading mail page: browser closed its DevTools pipe prematurely".into())
            .kind,
        Kind::Network
    );
    assert_eq!(
        transport_error("API session belongs to a different mailbox".into()).kind,
        Kind::Refused
    );
}

#[test]
fn mailbox_identity_uses_complete_addresses() {
    assert!(identity_matches(
        "alice@proton.me",
        "Inbox | alice@pm.me | Proton Mail"
    ));
    assert!(!identity_matches("alice@proton.me", "malice@pm.me"));
    assert!(!identity_matches(
        "alice@proton.me",
        "alice@pm.me.evil.test"
    ));
    assert!(!identity_matches("alice@proton.me", "alice@evil.test"));
    assert!(!identity_matches("alice@proton.me", "Inbox"));
}
