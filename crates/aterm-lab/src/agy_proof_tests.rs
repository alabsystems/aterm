// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const STUB_LOG: &str = "\
13:02:46.080 stub listening on 127.0.0.1:52886 (proxy black hole: 52887) mode=ok pid=38489
13:02:46.716 PROXY refused: CONNECT antigravity-unleash.goog:443 HTTP/1.1
13:02:46.716 PROXY refused: CONNECT antigravity-unleash.goog:443 HTTP/1.1
13:02:47.103 POST /v1beta/models/m:streamGenerateContent -> 200 body=738B mode=ok kind=gemini-aux auth=gemini-key model=m
13:02:47.113 POST /v1beta/models/m:streamGenerateContent -> 200 body=53718B mode=ok kind=gemini auth=gemini-key model=m
13:02:51.728 PROXY refused: CONNECT play.googleapis.com:443 HTTP/1.1
13:02:52.000 POST /v1beta/models/m:countTokens -> 200 body=10B mode=ok kind=gemini-count auth=gemini-key tokens=3
";

/// The stub's log says every Gemini API request carried the fake key.
#[test]
fn every_gemini_request_names_the_key_it_carried() {
    assert_eq!(gemini_auths(STUB_LOG), (3, Vec::new()));
    let login = format!(
        "{STUB_LOG}13:03:00.000 POST /v1beta/models/m:streamGenerateContent -> 200 mode=ok kind=gemini \
         auth=jwt:owner model=m\n13:03:01.000 POST /v1beta/models/m:generateContent -> 401 mode=ok \
         kind=gemini-json model=m\n"
    );
    assert_eq!(
        gemini_auths(&login),
        (5, vec!["jwt:owner".to_string(), "none".to_string()])
    );
    let claude_only = "13:00:00.000 POST /v1/messages -> 200 mode=ok kind=main auth=key\n";
    assert_eq!(gemini_auths(claude_only), (0, Vec::new()));
}

/// THE KEY CHECK FAILS ON ANY CREDENTIAL BUT THE LAB'S KEY ALONE: the
/// selftest's own log passes it, and so does the catalog's GET under the
/// key; a Google login — a bearer token beside the key, one alone, a
/// request with no key, each labelled as the stub labels it
/// ([`crate::stub::gemini::credential_label`]) — fails it.
#[test]
fn the_key_check_fails_on_any_credential_but_the_labs_key_alone() {
    use crate::stub::gemini::credential_label;
    assert_eq!(key_verdict(STUB_LOG), Verdict::Ok);
    let catalog = |label: &str| {
        format!(
            "{STUB_LOG}13:03:00.000 GET /v1beta/models -> 200 body=0B mode=ok kind=gemini-models \
             auth={label}\n"
        )
    };
    let key = Some(crate::agy::LAB_GEMINI_KEY);
    assert_eq!(
        key_verdict(&catalog(&credential_label(key, None))),
        Verdict::Ok
    );
    let login = Some("Bearer ya29.a-google-login");
    for label in [
        credential_label(key, login),
        credential_label(None, login),
        credential_label(None, None),
    ] {
        assert_eq!(key_verdict(&catalog(&label)), Verdict::Fail, "{label}");
    }
    let quiet: String = STUB_LOG
        .lines()
        .filter(|l| !l.contains(" kind=gemini"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(key_verdict(&quiet), Verdict::Fail, "the control");
}

/// The witness's logs as the sampler writes them ([`crate::sampler`]): each
/// new endpoint `<ts> <line>` in `sockets.log`, and one whose host is not
/// loopback `<ts> EXTERNAL <line>` in `egress.log` too.
fn witness_logs(lsof_f: &str) -> (String, String) {
    let (mut sockets, mut egress) = (String::new(), String::new());
    for line in crate::sampler::endpoints(lsof_f) {
        sockets.push_str(&format!("13:02:47 {line}\n"));
        if line.ends_with("EXTERNAL") {
            egress.push_str(&format!("13:02:47 EXTERNAL {line}\n"));
        }
    }
    (sockets, egress)
}

/// THE EGRESS CHECK FAILS WHEN EGRESS HAPPENS: the witness's own lines —
/// the sampler's reading of `lsof` — for agy talking to the stub and the
/// proxy pass it; one socket of the lab's to a Google address, agy's own
/// or another process's, IPv4 or IPv6, fails it, and so does its line in
/// either log alone.
#[test]
fn the_egress_check_fails_when_any_socket_leaves_loopback() {
    let (pids, port) = (&[38564][..], "52886");
    let stub = "p38564\ncagy\nPTCP\nn127.0.0.1:50100->127.0.0.1:52886 (ESTABLISHED)\n\
                n127.0.0.1:50101->127.0.0.1:52887\n";
    let (sockets, egress) = witness_logs(stub);
    assert!(egress.is_empty(), "{sockets}");
    assert_eq!(egress_verdict(&sockets, &egress, pids, port), Verdict::Ok);

    for leak in [
        "n10.0.0.2:50102->142.250.72.10:443 (SYN_SENT)\n",
        "p38600\ncnode\nPTCP\nn[fd00::2]:50103->[2607:f8b0:4005::200e]:443\n",
    ] {
        let (sockets, egress) = witness_logs(&format!("{stub}{leak}"));
        assert!(!egress.is_empty(), "the witness marks it: {leak}");
        assert_eq!(
            egress_verdict(&sockets, &egress, pids, port),
            Verdict::Fail,
            "{leak}"
        );
        assert_eq!(
            egress_verdict(&sockets, "", pids, port),
            Verdict::Fail,
            "sockets.log alone: {leak}"
        );
        let (clean, _) = witness_logs(stub);
        assert_eq!(
            egress_verdict(&clean, &egress, pids, port),
            Verdict::Fail,
            "egress.log alone: {leak}"
        );
    }

    assert_eq!(
        egress_verdict(&sockets, &egress, &[38565], port),
        Verdict::Fail,
        "the agent never seen"
    );
    let (listen, quiet) = witness_logs("p38564\ncagy\nPTCP\nn127.0.0.1:52886\n");
    assert_eq!(
        egress_verdict(&listen, &quiet, pids, port),
        Verdict::Fail,
        "a listen"
    );
}

/// A refusal naming `~/.gemini` fails the refusals check whether or not
/// the log kept the control; with none, the check holds only on a kept
/// control.
#[test]
fn a_refusal_naming_gemini_fails_whether_or_not_the_control_was_kept() {
    let denied = ["deny(1) file-read-data /Users//owner/.gemini/settings.json".to_string()];
    assert_eq!(refusals_verdict(&denied, true), Verdict::Fail);
    assert_eq!(refusals_verdict(&denied, false), Verdict::Fail);
    assert_eq!(refusals_verdict(&[], true), Verdict::Ok);
    assert_eq!(refusals_verdict(&[], false), Verdict::Unproven);
    assert_eq!(proxy_verdict(&[]), Verdict::Unproven);
    assert_eq!(
        proxy_verdict(&["antigravity-unleash.goog".to_string()]),
        Verdict::Ok
    );
}

/// THE OWNER'S-`~/.gemini` CHECK FAILS ON A HELD FILE (a failed report's
/// included), a changed decoy, no agent process or no write of its own;
/// otherwise it holds only when every open-file report was complete.
#[test]
fn the_owner_gemini_check_fails_on_a_held_file_and_needs_complete_reports() {
    let held = vec!["n/Users//owner/.gemini/oauth_creds.json".to_string()];
    let wrote = vec!["settings.json.bak".to_string()];
    assert_eq!(
        owner_verdict(true, &held, &[7], &wrote, true),
        Verdict::Fail
    );
    assert_eq!(
        owner_verdict(true, &held, &[7], &wrote, false),
        Verdict::Fail
    );
    assert_eq!(owner_verdict(false, &[], &[7], &wrote, true), Verdict::Fail);
    assert_eq!(owner_verdict(true, &[], &[], &wrote, true), Verdict::Fail);
    assert_eq!(owner_verdict(true, &[], &[7], &[], true), Verdict::Fail);
    assert_eq!(owner_verdict(true, &[], &[7], &wrote, true), Verdict::Ok);
    assert_eq!(
        owner_verdict(true, &[], &[7], &wrote, false),
        Verdict::Unproven
    );
    let read = crate::selftest::open_files_report(
        7,
        Ok(crate::procs::OpenPaths {
            paths: vec!["/Users//owner/.gemini/oauth_creds.json".into()],
            complete: true,
        }),
    );
    assert!(read.complete);
    assert!(read.text.contains("n/Users//owner/.gemini/oauth_creds.json"));
    let unread =
        crate::selftest::open_files_report(7, Err(std::io::ErrorKind::PermissionDenied.into()));
    assert!(!unread.complete && unread.text.is_empty());
}

/// The black-hole proxy's refusals name each host once, in the order first
/// asked.
#[test]
fn the_proxy_names_each_refused_host_once() {
    assert_eq!(
        proxy_refused(STUB_LOG),
        ["antigravity-unleash.goog", "play.googleapis.com"]
    );
    let plain =
        "13:00:00 PROXY refused: GET http://metadata.google.internal/computeMetadata/v1 HTTP/1.1\n";
    assert_eq!(proxy_refused(plain), ["metadata.google.internal"]);
    for target in [
        "http://https://example.com",
        "https://http://example.com",
        "http://http://example.com",
    ] {
        assert!(proxy_refused(&format!("PROXY refused: {target}\n")).is_empty());
    }
    let redirects = "PROXY refused: GET https://example.com/path?next=http://other.example/x HTTP/1.1\n\
                     PROXY refused: GET http://query.example?next=https://other.example/x HTTP/1.1\n";
    assert_eq!(proxy_refused(redirects), ["example.com", "query.example"]);
    let quiet: String = STUB_LOG
        .lines()
        .filter(|l| !l.contains("PROXY"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(proxy_refused(&quiet).is_empty());
}

/// The witness's line for a pid's connection to the stub, any program's
/// name.
#[test]
fn the_witness_names_the_pid_that_talked_to_the_stub() {
    let log = "13:02:47 agy[38564] TCP conn 127.0.0.1:52886\n\
               13:02:47 agy[38564] TCP listen 127.0.0.1:52888\n\
               13:02:48 node[4021] TCP conn 127.0.0.1:52887\n";
    assert!(talked(log, &[38564], "52886"));
    assert!(!talked(log, &[38565], "52886"));
    assert!(!talked(log, &[4021], "52886"), "the proxy's port");
    assert!(!talked(log, &[38564], "52888"), "a listen");
    assert!(!talked(log, &[3856], "52886"));
}

/// What the agent wrote under its scratch `.gemini` is everything but the
/// lab's own seeds.
#[test]
fn the_agents_own_writes_are_what_is_not_a_seed() {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let agy = v(&[
        "antigravity-cli",
        "antigravity-cli/cache",
        "antigravity-cli/cache/onboarding.json",
        "antigravity-cli/log",
        "antigravity-cli/log/cli-20261016_130246.log",
        "antigravity-cli/settings.json",
    ]);
    assert_eq!(
        own_writes(&agy),
        v(&[
            "antigravity-cli/log",
            "antigravity-cli/log/cli-20261016_130246.log"
        ])
    );
    let seeded = v(&[
        "antigravity-cli",
        "antigravity-cli/cache",
        "antigravity-cli/cache/onboarding.json",
        "antigravity-cli/settings.json",
    ]);
    assert!(own_writes(&seeded).is_empty(), "the control");
}
