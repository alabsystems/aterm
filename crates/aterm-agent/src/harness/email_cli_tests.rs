// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
fn options(args: &[&str]) -> Result<Option<Options>, String> {
    parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
}
#[test]
fn arguments_are_bounded_and_read_only() {
    assert!(options(&["send", "me@example.test"]).is_err());
    assert!(options(&["list", "me@example.test", "--limit", "101"]).is_err());
    assert!(options(&["watch", "me@example.test", "--timeout", "0"]).is_err());
    assert!(options(&["read", "me@example.test", "id", "--folder", "bad\nfolder"]).is_err());
    assert!(options(&["list", "me@example.test", "--limit", "2", "--limit", "3"]).is_err());
    assert!(options(&["list", "a@b@c"]).is_err());
    assert_eq!(
        options(&["session", "me@example.test"])
            .unwrap()
            .unwrap()
            .timeout,
        Duration::from_secs(3600)
    );
    assert!(options(&["read", "me@example.test", "-opaque-id"]).is_ok());
    assert!(options(&["source", "me@example.test", "-opaque-id"]).is_ok());
    assert!(options(&["magic-link", "me@example.test", "-opaque-id"]).is_ok());
    let parsed = options(&[
        "watch",
        "Me@Example.Test",
        "--timeout",
        "2",
        "--limit",
        "100",
    ])
    .unwrap()
    .unwrap();
    assert_eq!(parsed.account, "me@example.test");
    assert_eq!(parsed.timeout, Duration::from_secs(2));
}
#[test]
fn a_default_password_cannot_be_sent_to_an_unconfigured_account() {
    let config = headless_oauth::MailboxConfig {
        user: Some("me@proton.me".into()),
        password: Some("do-not-send-to-another-provider".into()),
        ..Default::default()
    };
    assert!(configured_account(&config, "ME@proton.me"));
    assert!(!configured_account(&config, "other@gmail.com"));
}

#[test]
fn expired_operation_is_retryable_without_network_work() {
    assert!(remaining(Instant::now()).unwrap_err().retry);
}

#[test]
#[cfg(unix)]
fn proton_read_recovery_keeps_classification_and_server_waits() {
    use aterm_mail::proton_error::{Kind, Operation};
    let now = Instant::now();
    for (kind, seconds, unparsed, retry, wait) in [
        (Kind::Network, None, false, true, Some(30)),
        (Kind::RateLimited, Some(47), false, true, Some(47)),
        (Kind::RateLimited, Some(0), false, true, Some(1)),
        (Kind::RateLimited, None, true, false, None),
        (Kind::Credentials, None, false, false, None),
        (Kind::Verification, None, false, false, None),
        (Kind::AccessDenied, None, false, false, None),
        (Kind::SessionExpired, None, false, true, None),
        (Kind::RateLimited, Some(u64::MAX), false, false, None),
    ] {
        let detail = aterm_mail::proton_error::Failure {
            operation: Operation::Addresses,
            kind,
            http_status: None,
            api_code: None,
            retry_after_seconds: seconds,
            retry_after_unparsed: unparsed,
            phase: None,
        };
        let failure = Failure::proton("owned read failure".into(), Some(&detail), now);
        assert_eq!(failure.retry, retry, "{kind:?}: {seconds:?}");
        assert_eq!(
            failure.retry_at,
            wait.map(|seconds| now + Duration::from_secs(seconds))
        );
        if let Some(seconds) = wait {
            assert!(reconnect_delay(0, failure.retry_at, now) >= Duration::from_secs(seconds));
        }
    }
    let unknown = Failure::from(super::super::mail_browser::MailboxOpenError::from(
        "owned network setup failure",
    ));
    assert!(!unknown.retry);
    assert_eq!(unknown.retry_at, None);
}

#[test]
fn proton_urls_without_trailing_slash_and_mixed_case_accounts_normalize_cleanly() {
    assert!(is_proton_webmail_url("https://mail.proton.me"));
    assert!(is_proton_webmail_url("HTTPS://mail.proton.me/u/0/inbox"));
    assert!(!is_proton_webmail_url(
        "https://mail.proton.me.evil.example.invalid"
    ));
    let config = headless_oauth::MailboxConfig {
        user: Some("Me@Example.Invalid".into()),
        ..Default::default()
    };
    assert_eq!(
        normalized_accounts(
            Some(&config),
            vec!["me@example.invalid".into(), "bad@@invalid".into()],
        ),
        vec!["me@example.invalid".to_owned()],
    );
}

#[test]
fn headers_exclude_body_and_output_remains_one_json_line() {
    let message = Message {
        id: "id".into(),
        from: "from".into(),
        to: "to".into(),
        subject: "line\nother".into(),
        date: String::new(),
        unread: true,
        text: "private-body".into(),
    };
    assert!(message_value(&message, false).get("text").is_none());
    let mut output = Vec::new();
    assert!(emit(&mut output, message_value(&message, true)).is_ok());
    assert_eq!(output.iter().filter(|b| **b == b'\n').count(), 1);
    let value: Value = aterm_json::from_slice(&output).unwrap();
    assert_eq!(value["text"].as_str(), Some("private-body"));
}
#[test]
#[cfg(unix)]
#[ignore = "requires ATERM_MAIL_TEST_ACCOUNT and its authenticated headless profile"]
fn live_commands_list_read_and_finish_a_quiet_watch() {
    aterm_uds::secret::no_core_dumps();
    let account = std::env::var("ATERM_MAIL_TEST_ACCOUNT").expect("live test account");
    let home = aterm_types::dirs::home_dir().unwrap();
    let mut output = Vec::new();
    let list = options(&["list", &account, "--limit", "1"])
        .unwrap()
        .unwrap();
    run(&home, &list, &mut output)
        .map_err(|error| error.message)
        .unwrap();
    let value: Value = aterm_json::from_slice(&output).unwrap();
    let id = value["messages"][0]["id"].as_str().unwrap();
    assert!(value["messages"][0].get("text").is_none());
    let read = options(&["read", &account, id]).unwrap().unwrap();
    let mut body = Vec::new();
    run(&home, &read, &mut body)
        .map_err(|error| error.message)
        .unwrap();
    let value: Value = aterm_json::from_slice(&body).unwrap();
    assert!(!value["message"]["text"].as_str().unwrap().is_empty());
    let watch = options(&["watch", &account, "--timeout", "30", "--limit", "1"])
        .unwrap()
        .unwrap();
    let mut events = Vec::new();
    run(&home, &watch, &mut events)
        .map_err(|error| error.message)
        .unwrap();
    let lines: Vec<Value> = events
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| aterm_json::from_slice(line).unwrap())
        .collect();
    assert!(
        lines
            .iter()
            .any(|line| line["type"].as_str() == Some("snapshot"))
    );
    assert_eq!(lines.last().unwrap()["type"].as_str(), Some("complete"));
}
