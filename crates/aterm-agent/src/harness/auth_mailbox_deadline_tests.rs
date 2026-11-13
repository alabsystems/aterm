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
    let (commands, to_browser) = aterm_uds::pipe().unwrap();
    let (from_browser, events) = aterm_uds::pipe().unwrap();
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

/// A SIGN-IN'S MAILBOX POLL ENDS WHEN ITS DEADLINE PASSES ON AWAKE TIME: an empty Maildir polled
/// within a sign-in deadline 50 ms off ends with the deadline's words once that passes, with no
/// sleep at all. NEGATIVE CONTROL: a poll clock that stopped at the sign-in's end, a few
/// nanoseconds short of the end the poll took when it began, never reached it, and the poll spun
/// until the one-minute hang detector reported it.
#[cfg(unix)]
#[test]
fn a_sign_ins_mailbox_poll_ends_when_its_deadline_passes() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let cfg = MailboxConfig {
        dir: Some(tmp.path().to_owned()),
        seen_file: Some(tmp.path().join("mail.seen")),
        ..MailboxConfig::default()
    };
    let (said, polled) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (_commands, to_browser) = aterm_uds::pipe().unwrap();
        let (from_browser, _events) = aterm_uds::pipe().unwrap();
        let mut cdp = CdpClient::new(to_browser, from_browser);
        let mut seen = BTreeSet::new();
        let _ = said.send(poll_mailbox_or_webmail_for_credential_by(
            &mut cdp,
            &cfg,
            Some("one@example.test"),
            WakeDeadline::after(Duration::from_millis(50)),
            Duration::from_millis(400),
            &mut seen,
        ));
    });
    let result = polled
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the poll ended once its sign-in's deadline passed");
    assert_eq!(result, Err(super::super::auth_deadline::EXPIRED));
}

/// A SIGN-IN'S MAILBOX POLL ASKED FOR AFTER ITS HOST SLEPT PAST ITS DEADLINE READS NO MAIL: a
/// Maildir that holds the sign-in's link, polled within an hour of which the host slept two hours
/// before the poll began, ends with the deadline's words and takes nothing. CONTROL: the same
/// Maildir polled within a live deadline gives that link. NEGATIVE CONTROL: a passed deadline
/// taken as a zero timeout scanned the mailbox once, on no deadline at all, and took the link.
#[cfg(unix)]
#[test]
fn a_mailbox_poll_its_host_slept_past_reads_no_mail() {
    const HOUR: Duration = Duration::from_secs(3600);
    let tmp = aterm_tempfile::tempdir().unwrap();
    let link = "https://claude.ai/magic?token=slept";
    std::fs::write(
        tmp.path().join("message"),
        format!("To: one@example.test\r\n\r\nSign in to Claude: {link}\n"),
    )
    .unwrap();
    let cfg = MailboxConfig {
        dir: Some(tmp.path().to_owned()),
        seen_file: Some(tmp.path().join("mail.seen")),
        ..MailboxConfig::default()
    };
    let (_commands, to_browser) = aterm_uds::pipe().unwrap();
    let (from_browser, _events) = aterm_uds::pipe().unwrap();
    let mut cdp = CdpClient::new(to_browser, from_browser);
    let mut seen = BTreeSet::new();
    let _seam = super::super::wake_deadline::SleepSeam::hold();
    let slept_past = WakeDeadline::after(HOUR);
    super::super::wake_deadline::host_slept(2 * HOUR);
    assert!(slept_past.passed(), "CONTROL: the host slept past it");
    assert_eq!(
        poll_mailbox_or_webmail_for_credential_by(
            &mut cdp,
            &cfg,
            Some("one@example.test"),
            slept_past,
            Duration::ZERO,
            &mut seen,
        ),
        Err(super::super::auth_deadline::EXPIRED)
    );
    assert!(seen.is_empty());
    assert_eq!(
        poll_mailbox_or_webmail_for_credential_by(
            &mut cdp,
            &cfg,
            Some("one@example.test"),
            WakeDeadline::after(HOUR),
            Duration::ZERO,
            &mut seen,
        ),
        Ok(MailboxCredential::MagicLink(link.into())),
        "CONTROL: a live deadline reads the link"
    );
}
