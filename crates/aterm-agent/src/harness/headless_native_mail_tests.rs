// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn routed_gmail_uses_its_saved_encrypted_grant_without_a_password() {
    let home = aterm_tempfile::tempdir().unwrap();
    let cfg = MailboxConfig::from_toml_str(
        "[[mailboxes]]\nlogin='saved@gmail.com'\nimap_url='imaps://imap.gmail.com/Archive'\nreceives=['alias@example.com']\n",
        None,
    ).unwrap();
    let target = Some("alias@example.com");
    assert!(!cfg.route_for(target).unwrap().ready());
    assert!(cfg.is_configured_for(target));
    assert_eq!(cfg.unready_words(target), None);
    aterm_mail::oauth::session::register(
        home.path(),
        "saved@gmail.com",
        "fixture-client",
        None,
        aterm_mail::Secret::new("fixture-refresh".into()),
    )
    .unwrap();
    let mut requested = Vec::new();
    let (account, endpoint, _session) = saved_google(&cfg, target, |account| {
        requested.push(account.to_owned());
        aterm_mail::oauth::session::shared(home.path(), account)
    })
    .unwrap()
    .unwrap();
    assert_eq!(account, "saved@gmail.com");
    assert_eq!(requested, ["saved@gmail.com"]);
    assert_eq!(endpoint.host, "imap.gmail.com");
    assert_eq!(endpoint.mailbox, "Archive");
    assert!(endpoint.password.is_none());
    let missing=MailboxConfig::from_toml_str("[[mailboxes]]\nlogin='missing@gmail.com'\nwebmail_url='https://mail.google.com/'\nreceives=['alias@example.com']\n",None).unwrap();
    assert!(
        saved_google(&missing, target, |account| {
            aterm_mail::oauth::session::shared(home.path(), account)
        })
        .unwrap()
        .is_none(),
        "another account's stored grant is never a fallback"
    );
    for text in [
        "[[mailboxes]]\nlogin='saved@gmail.com'\nimap_url='imaps://other@gmail.com@imap.gmail.com/INBOX'\nreceives=['alias@example.com']\n",
        "[[mailboxes]]\nlogin='saved@gmail.com'\nimap_url='imap://imap.gmail.com/INBOX'\nreceives=['alias@example.com']\n",
        "[[mailboxes]]\nlogin='saved@gmail.com'\nimap_url='imaps://imap.gmail.com.evil.test/INBOX'\nreceives=['alias@example.com']\n",
    ] {
        let other = MailboxConfig::from_toml_str(text, None).unwrap();
        assert!(
            saved_google::<()>(&other, target, |_| panic!(
                "unrelated or insecure endpoint reached the Google grant"
            ))
            .unwrap()
            .is_none()
        );
    }
}

#[test]
fn managed_google_mail_uses_the_configured_mailbox_identity() {
    let mut cfg = MailboxConfig {
        user: Some("mailbox-a@gmail.com".into()),
        imap_url: Some("imaps://imap.gmail.com/INBOX".into()),
        ..Default::default()
    };
    assert_eq!(
        managed_google_login(&cfg, Some("target-b@gmail.com")).as_deref(),
        Some("mailbox-a@gmail.com")
    );
    cfg.imap_url = Some("imaps://embedded-c@gmail.com@imap.gmail.com/Archive".into());
    assert_eq!(
        managed_google_login(&cfg, Some("target-b@gmail.com")).as_deref(),
        Some("embedded-c@gmail.com")
    );
    cfg.imap_url = None;
    cfg.webmail_url = Some("https://mail.proton.me/u/0/inbox".into());
    assert!(managed_google_login(&cfg, Some("target-b@gmail.com")).is_none());
    cfg.webmail_url = Some("https://mail.google.com.evil.test/".into());
    assert!(managed_google_login(&cfg, Some("target-b@gmail.com")).is_none());

    let routed = MailboxConfig::from_toml_str(
        r#"
[[mailboxes]]
login = "owner@proton.me"
webmail_url = "https://mail.proton.me/u/0/inbox"
receives = ["*@proton.me"]
[[mailboxes]]
login = "me@gmail.com"
imap_url = "imaps://imap.example.test/INBOX"
receives = ["me@gmail.com"]
[[mailboxes]]
login = "shared@workspace.example.test"
imap_url = "imaps://imap.gmail.com/Archive"
receives = ["*@workspace.example.test"]
"#,
        None,
    )
    .unwrap();
    assert!(managed_google_login(&routed, Some("alias@proton.me")).is_none());
    assert!(managed_google_login(&routed, Some("me@gmail.com")).is_none());
    let (login, imap) =
        managed_google_target(&routed, Some("worker@workspace.example.test")).unwrap();
    assert_eq!(login, "shared@workspace.example.test");
    assert_eq!(imap.mailbox, "Archive");
    assert!(
        managed_google_login(
            &routed.routed_for(Some("alias@proton.me")),
            Some("alias@proton.me")
        )
        .is_none()
    );
}

#[test]
fn expired_wait_never_acquires_a_busy_shared_mailbox() {
    struct UnusedTransport;
    impl aterm_mail::proton::ApiTransport for UnusedTransport {
        fn request(
            &mut self,
            _: aterm_http::Method,
            _: &str,
            _: Option<Value>,
        ) -> Result<aterm_mail::proton::ApiResponse, aterm_mail::proton::ApiError> {
            panic!("expired read reached transport")
        }
        fn renew(&mut self) -> Result<(), aterm_mail::proton::ApiError> {
            panic!("expired read renewed transport")
        }
    }
    let mailbox = std::sync::Arc::new(std::sync::Mutex::new(
        aterm_mail::proton::Proton::with_transport(Box::new(UnusedTransport)),
    ));
    let held = mailbox.lock().unwrap();
    let clock = Instant::now;
    assert_eq!(
        read(&mailbox, Deadline::new(Duration::ZERO, &clock), |_| Ok(())),
        Err(false)
    );
    drop(held);
}

#[test]
fn expired_native_wait_never_connects_or_consumes_a_credential() {
    let clock = Instant::now;
    let expired = Deadline::new(Duration::ZERO, &clock);
    for (webmail_url, imap_url) in [
        (Some("https://mail.proton.me/u/0/inbox".into()), None),
        (None, Some("imaps://imap.gmail.com/INBOX".into())),
    ] {
        let cfg = MailboxConfig {
            webmail_url,
            imap_url,
            since: Some(1),
            ..Default::default()
        };
        let mut seen = BTreeSet::new();
        assert_eq!(
            poll(&cfg, None, Some(expired), false, &mut seen, "expired"),
            Some(Err("expired"))
        );
        assert!(seen.is_empty());
    }
}

#[test]
fn gmail_raw_mime_uses_the_same_freshness_recipient_and_consumption_rules() {
    let cfg = MailboxConfig {
        senders: vec!["anthropic".into()],
        since: Some(1_800_000_000),
        ..Default::default()
    };
    let raw = "From: login@anthropic.com\r\nTo: me@gmail.com\r\nDate: Tue, 01 Jan 2030 00:00:00 +0000\r\nSubject: Verification code\r\nContent-Type: text/plain\r\n\r\nYour verification code is 123456\r\n";
    let mut seen = BTreeSet::new();
    let mut otps = BTreeSet::new();
    assert!(
        matches!(accept_raw(&cfg, Some("me@gmail.com"), raw, &mut seen, &mut otps), Some(MailboxCredential::Otp(code)) if code == "123456")
    );
    assert!(accept_raw(&cfg, Some("me@gmail.com"), raw, &mut seen, &mut otps).is_none());
    assert!(
        accept_raw(
            &cfg,
            Some("other@gmail.com"),
            raw,
            &mut BTreeSet::new(),
            &mut BTreeSet::new()
        )
        .is_none()
    );
}

#[test]
fn native_messages_keep_auth_links_and_reject_wrong_sender_recipient_and_old_mail() {
    let cfg = MailboxConfig {
        senders: vec!["anthropic".into()],
        since: Some(1_800_000_000),
        ..Default::default()
    };
    let mut message = aterm_mail::Message {
        id: "a".into(),
        from: "login@anthropic.com".into(),
        to: "me@proton.me".into(),
        subject: "Verification code".into(),
        date: "1800000001".into(),
        unread: true,
        text: "Your verification code is 123456".into(),
    };
    let mut seen = BTreeSet::new();
    let mut otps = BTreeSet::new();
    assert!(
        matches!(accept(&cfg, Some("me@proton.me"), &message, &mut seen, &mut otps), Some(MailboxCredential::Otp(code)) if code == "123456")
    );
    assert!(accept(&cfg, Some("me@proton.me"), &message, &mut seen, &mut otps).is_none());
    for (from, to, date) in [
        ("bad@evil.test", "me@proton.me", "1800000001"),
        ("login@anthropic.com", "other@proton.me", "1800000001"),
        ("login@anthropic.com", "me@proton.me", "1700000000"),
    ] {
        message.from = from.into();
        message.to = to.into();
        message.date = date.into();
        assert!(
            accept(
                &cfg,
                Some("me@proton.me"),
                &message,
                &mut BTreeSet::new(),
                &mut BTreeSet::new()
            )
            .is_none()
        );
    }
    message.from = "login@anthropic.com".into();
    message.to = "me@proton.me".into();
    message.date = "1800000001".into();
    message.date = "Tue, 01 Jan 2030 00:00:00 +0000".into();
    assert!(MailFilter::of(&cfg).admits(&envelope(&message)));
    message.text = "<a href=\"https://claude.ai/magic-link?token=secret\">Sign in</a>".into();
    assert!(matches!(
        accept(
            &cfg,
            Some("me@proton.me"),
            &message,
            &mut BTreeSet::new(),
            &mut BTreeSet::new()
        ),
        Some(MailboxCredential::MagicLink(_))
    ));
}

#[test]
fn missing_passwords_and_non_gmail_imap_routes_defer_without_dialling() {
    let clock = Instant::now;
    let active = Deadline::new(Duration::from_secs(5), &clock);
    let mut seen = BTreeSet::new();

    // Native Gmail reports missing saved authorization; Proton retains its unready
    // password path. Neither dials without a credential, and the outer poll can still relay.
    for (cfg, expected) in [
        MailboxConfig {
            imap_url: Some("imaps://imap.gmail.com/INBOX".into()),
            user: Some("unit-test-missing-pw@example.invalid".into()),
            password: None,
            relay_enabled: true,
            ..Default::default()
        },
        MailboxConfig {
            imap_url: Some("imaps://imap.gmail.com/INBOX".into()),
            user: Some("unit-test-missing-pw@example.invalid".into()),
            password: Some("   ".into()),
            ..Default::default()
        },
        MailboxConfig {
            webmail_url: Some("https://mail.proton.me/u/0/inbox".into()),
            user: Some("unit-test-missing-pw@example.invalid".into()),
            password: None,
            relay_enabled: true,
            ..Default::default()
        },
        MailboxConfig {
            webmail_url: Some("https://mail.proton.me".into()),
            user: Some("unit-test-missing-pw@example.invalid".into()),
            password: Some("  ".into()),
            ..Default::default()
        },
    ]
    .into_iter()
    .zip([
        Some(Err(GOOGLE_AUTH_MISSING)),
        Some(Err(GOOGLE_AUTH_MISSING)),
        None,
        None,
    ]) {
        assert_eq!(
            poll(
                &cfg,
                Some("unit-test-missing-pw@example.invalid"),
                Some(active),
                true,
                &mut seen,
                "expired"
            ),
            expected
        );
    }

    // A local ProtonMail Bridge or loopback IMAP route must not be preempted by Proton webmail.
    let bridge_cfg = MailboxConfig {
        imap_url: Some("imap://127.0.0.1:1143/INBOX".into()),
        webmail_url: Some("https://mail.proton.me/u/0/inbox".into()),
        user: Some("unit-test-bridge@example.invalid".into()),
        password: Some("bridge-pw".into()),
        ..Default::default()
    };
    assert_eq!(
        poll(
            &bridge_cfg,
            Some("unit-test-bridge@example.invalid"),
            Some(active),
            true,
            &mut seen,
            "expired"
        ),
        None
    );

    assert!(is_proton_webmail_url("https://mail.proton.me"));
    assert!(is_proton_webmail_url("HTTPS://MAIL.PROTON.ME/u/0/inbox"));
    assert!(!is_proton_webmail_url("https://mail.proton.me.evil.test/"));
}
