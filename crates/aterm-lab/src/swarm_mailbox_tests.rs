// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::align::Captured;

#[test]
fn standin_auth_login_enrolls_via_mailbox_and_refuses_when_mailbox_withheld() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
    let home = root.join("home");
    let staging = root.join("staging");
    let ctl = root.join("ctl");
    let store_dir = root.join("store");
    std::fs::create_dir_all(home.join(".claude")).expect("claude dir");
    let marker = J::obj([
        ("control_dir", J::s(ctl.to_string_lossy().into_owned())),
        ("store_dir", J::s(store_dir.to_string_lossy().into_owned())),
        ("expected_otp", J::s(EXPECTED_OTP)),
    ]);
    atomic_write(&home.join(STANDIN_MARKER), marker.dump(COMPACT).as_bytes()).expect("marker");

    let login_args = vec![
        "login".to_owned(),
        "--claudeai".to_owned(),
        "--email".to_owned(),
        EMAIL_MBX.to_owned(),
    ];
    let status_args = vec!["status".to_owned(), "--json".to_owned()];

    // Negative control 1: mailbox config withheld -> login exits 1 and status exits 1.
    let (code, _) = run_standin_auth(&login_args, &home, Some(&staging));
    assert_eq!(
        code, 1,
        "login must refuse when ai-mailbox.toml is withheld"
    );
    assert!(!staging.join(".credentials.json").exists());
    let (st_code, st_out) = run_standin_auth(&status_args, &home, Some(&staging));
    assert_eq!(st_code, 1);
    assert!(st_out.contains("\"loggedIn\":false"), "{st_out}");

    // Negative control 2: ai-mailbox.toml configured, but email addressed to a different user.
    let maildir = root.join("maildir");
    write_verification_email(&maildir, "someone-else@example.test", EXPECTED_OTP)
        .expect("wrong recipient mail");
    write_hermetic_ai_mailbox_toml(&home, &maildir, EMAIL_MBX).expect("mailbox toml");
    let (wrong_code, _) = run_standin_auth(&login_args, &home, Some(&staging));
    assert_eq!(
        wrong_code, 1,
        "login must refuse when maildir holds no OTP for target email"
    );

    // Positive: verification email addressed to EMAIL_MBX with EXPECTED_OTP -> enrolls and
    // `claude auth status --json` satisfies `cli::status_facts(...).renews()`.
    write_verification_email(&maildir, EMAIL_MBX, EXPECTED_OTP).expect("target mail");
    let (ok_code, _) = run_standin_auth(&login_args, &home, Some(&staging));
    assert_eq!(
        ok_code, 0,
        "login must succeed once mailbox holds target OTP"
    );
    let (ok_st_code, ok_st_out) = run_standin_auth(&status_args, &home, Some(&staging));
    assert_eq!(ok_st_code, 0);
    let captured = Captured {
        code: Some(ok_st_code),
        stdout: ok_st_out.into_bytes(),
    };
    let facts = aterm_agent::harness::cli::status_facts(&captured, EMAIL_MBX)
        .expect("status JSON must parse");
    assert!(
        facts.renews(),
        "status facts must qualify row renewal: {facts:?}"
    );
}

#[test]
fn mailbox_judge_accepts_enrolled_served_run_and_rejects_controls() {
    let good = MailboxEvidence {
        withheld_outcome: Some(AttemptOutcome::NoRoute.as_str().into()),
        enrolled_while_withheld: false,
        signin_outcome: Some(AttemptOutcome::SignedIn.as_str().into()),
        signin_route: Some("mailbox".into()),
        enrolled_after: BTreeSet::from([LABEL_MBX.to_owned()]),
        bound_dir: Some("/tmp/logins/swarm-mbx+1".into()),
        turns_served: 1,
        account_ran: true,
        login_served: true,
        withheld_ms: 45,
        signin_ms: 120,
        turn_ms: 1500,
    };
    assert_eq!(
        judge_mailbox_evidence(&good, LABEL_MBX),
        Vec::<String>::new()
    );

    let missing_no_route = MailboxEvidence {
        withheld_outcome: None,
        ..good.clone()
    };
    assert!(
        judge_mailbox_evidence(&missing_no_route, LABEL_MBX)
            .iter()
            .any(|m| m.contains("withheld mailbox negative control"))
    );

    let not_served = MailboxEvidence {
        account_ran: false,
        login_served: false,
        turns_served: 0,
        ..good
    };
    let bad = judge_mailbox_evidence(&not_served, LABEL_MBX);
    assert!(bad.iter().any(|m| m.contains("served 0 turns")));
    assert!(bad.iter().any(|m| m.contains("accounts/state.json")));
    assert!(bad.iter().any(|m| m.contains("accounts/logins.json")));
}

#[test]
fn mailbox_cli_parser_handles_flags_and_rejects_unknown_args() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        parse_mailbox_args(&s(&[])).unwrap(),
        MailboxSigninOpts::default()
    );
    assert_eq!(
        parse_mailbox_args(&s(&[
            "--live",
            "--email",
            "a@example.com",
            "--label",
            "m22-1",
            "--mailbox",
            "/tmp/ai-mailbox.toml",
            "--record",
            "--aterm",
            "/tmp/aterm",
            "--keep"
        ]))
        .unwrap(),
        MailboxSigninOpts {
            live: true,
            email: Some("a@example.com".into()),
            label: Some("m22-1".into()),
            mailbox: Some("/tmp/ai-mailbox.toml".into()),
            record: true,
            aterm: Some("/tmp/aterm".into()),
            keep: true,
        }
    );
    assert!(parse_mailbox_args(&s(&["--help"])).is_err());
    assert!(parse_mailbox_args(&s(&["--bogus"])).is_err());
}
