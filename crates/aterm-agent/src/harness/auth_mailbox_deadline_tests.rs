// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;

#[cfg(unix)]
#[test]
fn expired_pending_credential_remains_owned_and_live_pending_is_reused_once() {
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let (commands, to_browser) = std::io::pipe().unwrap();
    let (from_browser, events) = std::io::pipe().unwrap();
    let mut cdp = CdpClient::new(to_browser, from_browser);
    let expected = MailboxCredential::Otp("918472".into());
    let mut pending = Some(expected.clone());
    let mut seen = BTreeSet::new();
    now.set(deadline.end());
    let result = poll_mailbox_or_pending_credential_until(
        &mut pending,
        &mut cdp,
        &MailboxConfig::default(),
        Some("one@example.test"),
        deadline,
        Duration::ZERO,
        &mut seen,
    );
    assert_eq!(result, Err(super::super::auth_deadline::EXPIRED));
    assert_eq!(pending, Some(expected.clone()));
    assert!(seen.is_empty());
    let live = Deadline::new(Duration::from_secs(1), &clock);
    let result = poll_mailbox_or_pending_credential_until(
        &mut pending,
        &mut cdp,
        &MailboxConfig::default(),
        Some("one@example.test"),
        live,
        Duration::ZERO,
        &mut seen,
    );
    assert_eq!(result, Ok(expected));
    assert_eq!(pending, None);
    assert!(seen.contains(&otp_flow_key("918472", Some("one@example.test"))));
    drop(cdp);
    drop(events);
    drop(commands);
}

#[test]
fn one_scan_retries_use_available_mail_without_renewing_the_outer_deadline() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let message = tmp.path().join("message");
    let first = "https://claude.ai/magic?token=first";
    let second = "https://claude.ai/magic?token=second";
    std::fs::write(&message, format!("To: one@example.test\r\n\r\nSign in to Claude: {first}\nAnother sign-in link: {second}\n")).unwrap();
    let cfg = MailboxConfig {
        dir: Some(tmp.path().to_owned()),
        seen_file: Some(tmp.path().join("mail.seen")),
        ..MailboxConfig::default()
    };
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let mut seen = BTreeSet::new();
    now.set(deadline.end());
    assert_eq!(
        poll_mailbox_config_until(&cfg, Some("one@example.test"), deadline, true, &mut seen),
        Err(super::super::auth_deadline::EXPIRED)
    );
    assert!(seen.is_empty());
    assert!(!tmp.path().join("message.clicked").exists());
    assert!(!cfg.seen_file.as_ref().unwrap().exists());
    let live = Deadline::new(Duration::from_secs(10), &clock);
    now.set(start + Duration::from_secs(13));
    let got =
        poll_mailbox_config_until(&cfg, Some("one@example.test"), live, true, &mut seen).unwrap();
    assert_eq!(got, MailboxCredential::MagicLink(first.into()));
    assert!(!tmp.path().join("message.clicked").exists());
    now.set(start + Duration::from_secs(18));
    let got =
        poll_mailbox_config_until(&cfg, Some("one@example.test"), live, true, &mut seen).unwrap();
    assert_eq!(got, MailboxCredential::MagicLink(second.into()));
    assert!(tmp.path().join("message.clicked").exists());
    assert_eq!(deadline.end(), start + Duration::from_secs(10));
    assert_eq!(live.end(), start + Duration::from_secs(20));
}

#[cfg(unix)]
#[test]
fn expired_profile_preparation_cannot_start_authorization() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let calls = Cell::new(0);
    let result = with_browser_profile_using_deadline(
        Some(tmp.path()),
        Some("one"),
        None,
        |path| {
            prepare_browser_profile(path)?;
            now.set(deadline.end());
            Ok(())
        },
        |_| {
            calls.set(calls.get() + 1);
            Ok(())
        },
        |_| panic!("an expired authorization cannot adopt the profile"),
        Some(deadline),
    );
    assert_eq!(result, Err(super::super::auth_deadline::EXPIRED));
    assert_eq!(calls.get(), 0);
    assert!(
        resolve_browser_profile_dir(tmp.path(), Some("one"), None)
            .join(PROFILE_UNPROVEN_MARK)
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn cleanup_after_timely_authorization_preserves_its_success() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let result = with_browser_profile_using_deadline(
        Some(tmp.path()),
        Some("completed"),
        None,
        prepare_browser_profile,
        |_| {
            assert!(deadline.remaining().is_ok());
            // The authorization completed while admitted; its cleanup then used the rest.
            now.set(deadline.end());
            Ok(())
        },
        |_| panic!("the completed profile should be adopted"),
        Some(deadline),
    );
    assert_eq!(finish_deadline(result, deadline), Ok(()));
    assert!(
        !resolve_browser_profile_dir(tmp.path(), Some("completed"), None)
            .join(PROFILE_UNPROVEN_MARK)
            .exists()
    );
}

#[test]
fn expiry_at_the_next_evaluate_keeps_an_observed_bot_check() {
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let mut bot = BotCheckClock::default();
    bot.check("https://claude.ai/login", start);
    now.set(deadline.end());
    let _ = take_met();
    assert_eq!(
        oauth_time_left(deadline, &bot, Some("claude.ai")),
        Err(BOT_CHECK_UNCLEARED)
    );
    assert_eq!(
        take_met(),
        Some(super::super::open_url::Met::new(
            MetWord::BotCheck,
            Some("claude.ai")
        ))
    );
    assert!(oauth_time_left(deadline, &BotCheckClock::default(), None).is_err());
    assert_eq!(
        take_met(),
        Some(super::super::open_url::Met::new(MetWord::Stall, None))
    );
}

#[cfg(unix)]
#[test]
fn a_late_relay_credential_is_not_marked_consumed() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let seen_file = tmp.path().join("seen");
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let credential = MailboxCredential::MagicLink("https://claude.ai/magic?token=relay".into());
    let mut seen = BTreeSet::new();
    let mut codes = BTreeSet::new();
    now.set(deadline.end());
    assert_eq!(
        accept_relay_credential(
            credential.clone(),
            Some("one@example.test"),
            Some(&seen_file),
            &mut seen,
            &mut codes,
            Some(deadline)
        ),
        None
    );
    assert!(seen.is_empty() && codes.is_empty());
    assert!(!seen_file.exists());
    let next = Deadline::new(Duration::from_secs(1), &clock);
    assert_eq!(
        accept_relay_credential(
            credential.clone(),
            Some("one@example.test"),
            Some(&seen_file),
            &mut seen,
            &mut codes,
            Some(next)
        ),
        Some(credential)
    );
    assert!(!seen.is_empty() && !codes.is_empty());
    assert!(seen_file.exists());
}
