// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn empty_discovery_finishes_the_connecting_notice() {
    let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
    let (tx, rx) = mpsc::sync_channel(2);
    app.replies = rx;
    app.pending = Some(3);
    app.notice = "Connecting…".into();
    tx.send((3, Reply::Available(Vec::new()))).unwrap();
    assert!(app.poll());
    assert!(app.pending.is_none());
    assert_eq!(app.notice, "Add an account to get started");
}

#[test]
fn late_discovery_preserves_a_newer_request_and_selected_account() {
    let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
    let (tx, rx) = mpsc::sync_channel(2);
    app.replies = rx;
    app.account = Some(0);
    app.pending = Some(9);
    tx.send((2, Reply::Available(vec!["stored@example.test".into()])))
        .unwrap();
    assert!(app.poll());
    assert_eq!(app.pending, Some(9));
    assert_eq!(app.account, Some(0));
    assert_eq!(
        app.stored.front().map(String::as_str),
        Some("stored@example.test")
    );
}

#[test]
fn background_refresh_preserves_open_message_and_pending_draft() {
    let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
    let (tx, rx) = mpsc::sync_channel(2);
    app.replies = rx;
    app.account = Some(0);
    app.folder = "0".into();
    app.draft_dirty = true;
    app.pending = Some(7);
    app.message = Some(Message {
        id: "open".into(),
        from: String::new(),
        to: String::new(),
        subject: "Open".into(),
        date: String::new(),
        unread: false,
        text: "Body".into(),
    });
    tx.send((
        0,
        Reply::Synced {
            account: 0,
            folder: "0".into(),
            messages: Vec::new(),
        },
    ))
    .unwrap();
    app.poll();
    assert_eq!(app.message.as_ref().unwrap().id, "open");
    assert!(app.draft_dirty);
    assert_eq!(app.pending, Some(7));
    assert_eq!(retry_interval(0).as_secs(), 2);
    assert_eq!(retry_interval(1).as_secs(), 2);
    assert_eq!(retry_interval(u32::MAX).as_secs(), 900);
}

#[test]
fn send_acknowledgement_does_not_discard_newer_edits() {
    let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
    let (tx, rx) = mpsc::sync_channel(2);
    app.replies = rx;
    app.pending = Some(7);
    app.draft_revision = 4;
    app.draft_dirty = true;
    tx.send((7, Reply::Sent(3))).unwrap();
    assert!(app.poll());
    assert!(app.draft_dirty);
    app.pending = Some(8);
    tx.send((8, Reply::Sent(4))).unwrap();
    app.poll();
    assert!(!app.draft_dirty);
}

#[test]
#[ignore = "requires ATERM_MAIL_TEST_ACCOUNT and an authenticated headless profile"]
fn headless_account_connects_and_reads_through_native_worker() {
    aterm_uds::secret::no_core_dumps();
    let email = std::env::var("ATERM_MAIL_TEST_ACCOUNT").expect("set the live test account");
    let mut accounts = Vec::new();
    let connected = perform(Work::Stored(email.clone()), &mut accounts, &mut |_| {}).unwrap();
    let Reply::Connected {
        account,
        folder,
        messages,
        ..
    } = connected
    else {
        panic!("mail worker did not connect");
    };
    let message = messages.first().expect("the live test needs one message");
    let read = perform(
        Work::Read {
            account,
            folder,
            id: message.id.clone(),
        },
        &mut accounts,
        &mut |_| {},
    )
    .unwrap();
    let Reply::Read(read) = read else {
        panic!("mail worker did not read");
    };
    assert_eq!(read.id, message.id);
    assert!(read.subject == message.subject);
    assert!(!read.text.is_empty());
    let mut config = aterm_agent::harness::mail_recovery::configured().unwrap();
    config.senders = vec!["aterm-native-probe.invalid".into()];
    let outcome = aterm_agent::harness::headless_oauth::poll_mailbox_config_for_credential(
        &config,
        Some(&email),
        std::time::Duration::from_secs(2),
    );
    assert!(matches!(
        outcome,
        Err("timed out waiting for verification email in mailbox")
    ));
}

#[test]
fn browser_fallback_does_not_repeat_credentials_or_network_failures() {
    use aterm_mail::proton_error::Kind;
    for kind in [
        Kind::Verification,
        Kind::DeviceVerification,
        Kind::Scope,
        Kind::ClientRejected,
    ] {
        assert!(permits_browser_fallback(kind));
    }
    for kind in [
        Kind::Credentials,
        Kind::RateLimited,
        Kind::Network,
        Kind::Timeout,
        Kind::AccessDenied,
        Kind::Configuration,
        Kind::AccountRestricted,
        Kind::Service,
        Kind::SessionExpired,
        Kind::Protocol,
        Kind::Refused,
    ] {
        assert!(!permits_browser_fallback(kind));
    }
}

#[test]
fn late_connection_retains_account_without_replacing_selection() {
    let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
    let (tx, rx) = mpsc::sync_channel(2);
    app.replies = rx;
    app.pending = Some(9);
    app.account = Some(0);
    tx.send((
        8,
        Reply::Connected {
            account: 1,
            email: "second@example.test".into(),
            folders: vec![],
            folder: "INBOX".into(),
            messages: vec![],
        },
    ))
    .unwrap();
    app.poll();
    assert_eq!(app.account, Some(0));
    assert_eq!(app.pending, Some(9));
    assert_eq!(app.accounts[0].0, 1);
    assert!(!app.take_leave_setup());
}

fn dummy_standard_account(username: &str) -> Account {
    Account::Standard(std::sync::Arc::new(std::sync::Mutex::new(
        StandardAccount {
            imap: aterm_mail::imap::Imap {
                endpoint: Endpoint::parse("imap.example.test", 993).unwrap(),
                authentication: Authentication::Password {
                    username: username.into(),
                    password: Secret::new("test-pass".into()),
                },
            },
            smtp: Endpoint::parse("smtp.example.test", 465).unwrap(),
            refresh: None,
            generation: 0,
        },
    )))
}

#[test]
fn second_account_and_case_variant_reconnect_leave_setup_without_duplicates() {
    assert_eq!(
        dedup_accounts(vec![
            " Alice@Example.Test ".into(),
            "alice@example.test".into(),
            "bob@example.test".into(),
            "   ".into(),
        ]),
        vec![
            "Alice@Example.Test".to_string(),
            "bob@example.test".to_string()
        ]
    );

    let mut worker_accounts = Vec::new();
    let (first_idx, first_email) = upsert_account(
        &mut worker_accounts,
        "alice@example.test".into(),
        dummy_standard_account("alice"),
    );
    assert_eq!((first_idx, first_email.as_str()), (0, "alice@example.test"));
    let (reconnected_idx, reconnected_email) = upsert_account(
        &mut worker_accounts,
        " Alice@Example.Test ".into(),
        dummy_standard_account("alice2"),
    );
    assert_eq!(
        (reconnected_idx, reconnected_email.as_str()),
        (0, "Alice@Example.Test")
    );
    assert_eq!(worker_accounts.len(), 1);
    let (second_idx, _) = upsert_account(
        &mut worker_accounts,
        "bob@example.test".into(),
        dummy_standard_account("bob"),
    );
    assert_eq!(second_idx, 1);

    let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
    let (tx, rx) = mpsc::sync_channel(4);
    app.replies = rx;
    app.account = Some(0);
    app.accounts = vec![(0, "alice@example.test".into(), Vec::new())];
    app.pending = Some(4);
    tx.send((
        4,
        Reply::Connected {
            account: 1,
            email: "bob@example.test".into(),
            folders: vec![Folder {
                id: "INBOX".into(),
                name: "Inbox".into(),
            }],
            folder: "INBOX".into(),
            messages: Vec::new(),
        },
    ))
    .unwrap();
    assert!(app.poll());
    assert!(app.take_leave_setup());
    assert!(!app.take_leave_setup());
    assert_eq!(app.account, Some(1));
    assert_eq!(app.accounts.len(), 2);

    app.pending = Some(5);
    tx.send((
        5,
        Reply::Connected {
            account: 0,
            email: "Alice@Example.Test".into(),
            folders: Vec::new(),
            folder: "INBOX".into(),
            messages: Vec::new(),
        },
    ))
    .unwrap();
    assert!(app.poll());
    assert!(app.take_leave_setup());
    assert_eq!(app.accounts.len(), 2);
    assert_eq!(app.accounts[0].0, 0);
    assert_eq!(app.accounts[0].1, "Alice@Example.Test");

    tx.send((
        0,
        Reply::Available(vec![
            "alice@example.test".into(),
            "BOB@example.test".into(),
            "carol@example.test".into(),
        ]),
    ))
    .unwrap();
    app.pending = Some(6);
    assert!(app.poll());
    assert_eq!(
        app.stored.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["carol@example.test"]
    );

    let mut view = MailViewState::default();
    view.setup = true;
    view.scroll = 96.0;
    view.leave_setup();
    assert!(!view.setup);
    assert_eq!(view.scroll, 0.0);
}

#[test]
fn stale_reply_rearms_active_folder_watch_once_foreground_work_settles() {
    let mut app = MailApp::with_stored(|| {}, Default::default()).unwrap();
    let (work_tx, work_rx) = mpsc::sync_channel(4);
    let (reply_tx, reply_rx) = mpsc::sync_channel(4);
    app.work = work_tx;
    app.replies = reply_rx;
    app.account = Some(0);
    app.folder = "INBOX".into();
    app.messages = vec![Message {
        id: "1".into(),
        from: "sender@example.test".into(),
        to: "alice@example.test".into(),
        subject: "Current".into(),
        date: String::new(),
        unread: true,
        text: String::new(),
    }];
    app.pending = Some(11);

    reply_tx
        .send((
            10,
            Reply::Connected {
                account: 1,
                email: "second@example.test".into(),
                folders: Vec::new(),
                folder: "Archive".into(),
                messages: Vec::new(),
            },
        ))
        .unwrap();
    assert!(app.poll());
    assert!(work_rx.try_recv().is_err());

    reply_tx.send((11, Reply::Sent(0))).unwrap();
    assert!(app.poll());
    let Ok((
        0,
        Work::Watch {
            account,
            folder,
            baseline,
        },
    )) = work_rx.try_recv()
    else {
        panic!("expected active folder watch to re-arm after foreground send");
    };
    assert_eq!(account, 0);
    assert_eq!(folder, "INBOX");
    assert_eq!(baseline.len(), 1);
    assert_eq!(baseline[0].id, "1");
}
