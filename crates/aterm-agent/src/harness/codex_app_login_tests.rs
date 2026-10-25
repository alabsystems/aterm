// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for Codex's app-server sign-in, replayed over an in-process pipe pair: the server's
//! side of lab gate (d) (`fixtures/signin/codex-0.164.0-app-server-login.jsonl`, codex-old
//! 0.164.0 in a scratch `CODEX_HOME`, its ids, host name and URL parameters fabricated, its
//! completion and account those of a login that went through).

use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::sync::Mutex;

use super::*;

pub(crate) const REPLAY: &str =
    include_str!("fixtures/signin/codex-0.164.0-app-server-login.jsonl");

/// A stand-in app server answering each request with the next response of `script` (and the
/// notifications after it); the methods it was sent are kept in `heard`.
pub(crate) fn serve(script: String, heard: Arc<Mutex<Vec<String>>>) -> AppServer {
    let (from_client, to_server) = std::io::pipe().expect("pipe");
    let (from_server, mut to_client) = std::io::pipe().expect("pipe");
    std::thread::spawn(move || {
        let mut lines: std::collections::VecDeque<String> =
            script.lines().map(str::to_owned).collect();
        for request in BufReader::new(from_client).lines() {
            let Ok(request) = request else { break };
            let message: Value = aterm_json::from_str(&request).expect("json request");
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                heard.lock().expect("heard").push(method.to_owned());
            }
            if message.get("id").is_none() {
                continue;
            }
            let mut wrote = false;
            while let Some(line) = lines.front() {
                let is_response = line.contains("\"id\":");
                if is_response && wrote {
                    break;
                }
                let line = lines.pop_front().unwrap_or_default();
                wrote |= is_response;
                if writeln!(to_client, "{line}").is_err() {
                    return;
                }
            }
            if !wrote || lines.is_empty() {
                break;
            }
        }
    });
    AppServer {
        from: Box::new(from_server),
        to: Box::new(to_server),
    }
}

/// The pages handed over: each one's host, code and completion.
type Handed = Vec<(Option<String>, Option<String>, Completion)>;

#[derive(Default)]
struct Seen {
    handed: RefCell<Handed>,
}

impl Approver for &Seen {
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        self.handed.borrow_mut().push((
            https_host(handoff.url),
            handoff.code.map(str::to_owned),
            handoff.completion,
        ));
        Approval::Approved { page: false }
    }
}

const HOSTS: &[&str] = &["auth.openai.com"];

fn replay(script: String) -> (Drove, Vec<String>, usize) {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let server = serve(script, Arc::clone(&heard));
    let seen = Seen::default();
    let drove = login(
        server,
        &mut &seen,
        HOSTS,
        Some("codex-one@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    let heard = heard.lock().expect("heard").clone();
    let handed = seen.handed.borrow().len();
    (drove, heard, handed)
}

#[test]
fn the_app_servers_login_hands_its_pinned_page_over_and_reads_the_account_it_signed_in() {
    let (drove, heard, handed) = replay(REPLAY.to_owned());
    assert_eq!(
        drove,
        Drove {
            outcome: Outcome::SignedIn {
                email: Some("codex-one@example.com".to_owned()),
                page: false
            },
            host: Some("auth.openai.com".to_owned()),
        }
    );
    assert_eq!(
        heard,
        [
            "initialize",
            "initialized",
            "account/login/start",
            "account/read"
        ]
    );
    assert_eq!(handed, 1);
    assert_eq!(
        drove.why(crate::ai::connector::Flow::Browser),
        "auth.openai.com browser signed-in",
        "the ledger's line keeps no URL, login id or email"
    );
}

#[test]
fn an_off_pin_auth_url_is_cancelled_unopened_and_a_failed_login_is_no_sign_in() {
    let (drove, heard, handed) = replay(REPLAY.replace(
        "https://auth.openai.com/oauth/",
        "https://auth.openai.com.example.net/oauth/",
    ));
    assert_eq!(drove.outcome, Outcome::OffPin);
    assert_eq!(handed, 0, "nothing was opened");
    assert!(
        heard.iter().any(|m| m == "account/login/cancel"),
        "{heard:?}"
    );

    let (drove, heard, _) = replay(REPLAY.replace("\"success\":true", "\"success\":false"));
    assert_eq!(drove.outcome, Outcome::Failed);
    assert!(!heard.iter().any(|m| m == "account/read"), "{heard:?}");

    let (drove, _, _) = replay(REPLAY.replace("codex-one@example.com", "someone-else@example.com"));
    assert_eq!(
        drove.outcome,
        Outcome::SignedIn {
            email: Some("someone-else@example.com".to_owned()),
            page: false
        },
        "the account it names is handed back, for the renewal to refuse"
    );
}

#[test]
fn account_read_names_only_a_chatgpt_account() {
    let read: Value = aterm_json::from_str(
        r#"{"account":{"type":"chatgpt","email":"a@example.com","planType":"pro"}}"#,
    )
    .expect("json");
    assert_eq!(
        account_of(&read),
        Some(Account {
            email: Some("a@example.com".to_owned()),
            plan: Some("pro".to_owned())
        })
    );
    let none: Value =
        aterm_json::from_str(r#"{"account":null,"requiresOpenaiAuth":true}"#).expect("json");
    assert_eq!(account_of(&none), None);
    let key: Value = aterm_json::from_str(r#"{"account":{"type":"apiKey"}}"#).expect("json");
    assert_eq!(account_of(&key), None);
}

pub(crate) const RATE_LIMITS: &str =
    include_str!("fixtures/signin/codex-0.164.0-app-server-rate-limits.jsonl");

#[test]
fn a_rate_limits_read_gives_the_rows_sample_and_a_full_window_its_wall() {
    use crate::harness::accounts::{SESSION_LIMIT, WEEK_LIMIT};
    let now = 1_790_000_000;
    let heard = Arc::new(Mutex::new(Vec::new()));
    let usage = read_usage(
        serve(RATE_LIMITS.to_owned(), Arc::clone(&heard)),
        Instant::now() + Duration::from_secs(20),
        now,
    )
    .expect("a reading");
    assert_eq!(
        *heard.lock().expect("heard"),
        ["initialize", "initialized", "account/rateLimits/read"]
    );
    assert_eq!(usage.sample.session_pct, Some(95), "94.2% read up");
    assert_eq!(usage.sample.session_reset, Some(1_790_010_800));
    assert_eq!(usage.sample.week_pct, Some(31));
    assert_eq!(usage.sample.week_reset, Some(1_790_259_200));
    assert_eq!(usage.sample.at, now);
    assert_eq!(usage.wall, None, "ordinary use allowed, no window full");

    let answer = RATE_LIMITS.lines().nth(1).expect("the answer");
    let with = |from: &str, to: &str| {
        assert!(answer.contains(from), "{from}");
        let r: Value = aterm_json::from_str(&answer.replacen(from, to, 1)).expect("json");
        usage_of(r.get("result").expect("result"), now).expect("usage")
    };
    let full = with("\"usedPercent\":94.2", "\"usedPercent\":100.0");
    assert_eq!(full.wall, Some((SESSION_LIMIT, Some(1_790_010_800))));
    let week = with("\"usedPercent\":31", "\"usedPercent\":100");
    assert_eq!(week.wall, Some((WEEK_LIMIT, Some(1_790_259_200))));
    let refused = with(
        "\"ordinaryUsageAllowed\":true",
        "\"ordinaryUsageAllowed\":false",
    );
    assert_eq!(refused.wall, Some((SESSION_LIMIT, Some(1_790_010_800))));
    let reached = with(
        "\"rateLimitReachedType\":null",
        "\"rateLimitReachedType\":\"workspace_member_credits_depleted\"",
    );
    assert!(reached.wall.is_some());
    assert_eq!(usage_of(&json!({"accountId": "x"}), now), None);
    let week_only = with(
        "\"primary\":{\"usedPercent\":94.2,\"windowDurationMins\":300,\"resetsAt\":1790010800},\
         \"secondary\":{\"usedPercent\":31,\"windowDurationMins\":10080,\"resetsAt\":1790259200}",
        "\"primary\":{\"usedPercent\":0,\"windowDurationMins\":10080,\"resetsAt\":1793427434},\
         \"secondary\":null",
    );
    assert_eq!(
        (
            week_only.sample.session_pct,
            week_only.sample.week_pct,
            week_only.wall
        ),
        (None, Some(0), None)
    );
}

#[test]
fn finished_ignores_unrelated_login_id_and_wakes_on_matching_completion_or_server_close() {
    struct Probe<F>(F);
    impl<F: FnMut(&Handoff<'_>) -> Approval> Approver for Probe<F> {
        fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
            (self.0)(handoff)
        }
    }

    // 1. Stale `account/login/completed` with a different loginId does not flip `finished()`,
    //    whereas the matching loginId flips `finished()` to true.
    let stale_note = r#"{"method":"account/login/completed","params":{"loginId":"00000000-0000-4000-8000-999999999999","success":false,"error":"cancelled","onboardingEntrypoint":null},"emittedAtMs":1792808199415}"#;
    let lines: Vec<&str> = REPLAY.lines().collect();
    // Rpc reading response id 2 proves the reader already processed the preceding stale note.
    let script_stale_only = [lines[0], lines[1], stale_note, lines[2], lines[4]].join("\n");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let server = serve(script_stale_only, Arc::clone(&heard));
    let mut stale_finished = true;
    let mut probe = Probe(|handoff: &Handoff<'_>| {
        stale_finished = (handoff.finished)();
        Approval::Refused("stop after stale check".to_owned())
    });
    let drove = login(
        server,
        &mut probe,
        HOSTS,
        Some("codex-one@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    assert!(
        !stale_finished,
        "stale loginId completion must not mark current handoff finished"
    );
    assert_eq!(
        drove.outcome,
        Outcome::Refused("stop after stale check".to_owned())
    );

    // Control: matching loginId in REPLAY marks `finished()` true.
    let heard = Arc::new(Mutex::new(Vec::new()));
    let server = serve(REPLAY.to_owned(), Arc::clone(&heard));
    let mut matching_finished = false;
    let mut probe = Probe(|handoff: &Handoff<'_>| {
        let start = Instant::now();
        while !(handoff.finished)() && start.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(5));
        }
        matching_finished = (handoff.finished)();
        Approval::Approved { page: true }
    });
    let drove = login(
        server,
        &mut probe,
        HOSTS,
        Some("codex-one@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    assert!(
        matching_finished,
        "matching loginId completion marks finished"
    );
    assert_eq!(
        drove.outcome,
        Outcome::SignedIn {
            email: Some("codex-one@example.com".to_owned()),
            page: true
        }
    );

    // 2. Server closing stdout right after `account/login/start` flips `finished()` to true
    //    and yields `Outcome::Exited(None)`.
    let script_truncated = [lines[0], lines[1], lines[2]].join("\n");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let server = serve(script_truncated, Arc::clone(&heard));
    let mut closed_finished = false;
    let mut probe = Probe(|handoff: &Handoff<'_>| {
        let start = Instant::now();
        while !(handoff.finished)() && start.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(5));
        }
        closed_finished = (handoff.finished)();
        Approval::Approved { page: true }
    });
    let drove = login(
        server,
        &mut probe,
        HOSTS,
        Some("codex-one@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    assert!(closed_finished, "server stdout EOF marks finished");
    assert_eq!(drove.outcome, Outcome::Exited(None));

    // 3. Next-line `Want::Url` in `Scan::feed` rejects lines containing spaces/control chars.
    let route = crate::ai::connector::CODEX_SIGNIN
        .route(Some(crate::ai::connector::Flow::Device))
        .expect("codex device route");
    let crate::ai::connector::Drive::Lines { grammar: g, .. } = &route.drive else {
        panic!("expected line route");
    };
    let mut bad_scan = crate::harness::login_drive::Scan::default();
    bad_scan.feed(g, "Follow these steps to sign in:");
    bad_scan.feed(
        g,
        "1. Open this link in your browser and sign in to your account",
    );
    bad_scan.feed(g, "   https://auth.openai.com/codex/device (copy link)");
    assert_eq!(
        bad_scan.url, None,
        "multi-word line after URL lead must not be captured as URL"
    );
    let mut good_scan = crate::harness::login_drive::Scan::default();
    good_scan.feed(
        g,
        "1. Open this link in your browser and sign in to your account",
    );
    good_scan.feed(g, "   https://auth.openai.com/codex/device");
    assert_eq!(
        good_scan.url.as_deref(),
        Some("https://auth.openai.com/codex/device")
    );
}

#[test]
fn app_server_env_directs_sqlite_to_default_home_for_overlays_only() {
    use crate::harness::accounts::Cli;
    use std::ffi::OsString;

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let user_home = tmp.path().join("home");
    let overlay = tmp.path().join("logins/codex/codex-1");
    std::fs::create_dir_all(&user_home).expect("user home");
    std::fs::create_dir_all(&overlay).expect("overlay");
    let lookup = |name: &str| match name {
        "PATH" => Some(OsString::from("/usr/bin:/bin")),
        "HOME" => Some(user_home.as_os_str().to_os_string()),
        _ => None,
    };

    // 1. Overlay home sets `CODEX_SQLITE_HOME` to `user_home/.codex` and creates that directory.
    let default_home = user_home.join(".codex");
    assert!(!default_home.exists());
    let env = app_server_env(Cli::Codex, &overlay, &user_home, &lookup).expect("overlay env");
    assert!(
        default_home.is_dir(),
        "default home created for shared sqlite"
    );
    let get = |k: &str| {
        env.iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    };
    assert_eq!(
        get("CODEX_HOME").as_deref(),
        Some(overlay.to_string_lossy().as_ref())
    );
    assert_eq!(
        get("CODEX_SQLITE_HOME").as_deref(),
        Some(default_home.to_string_lossy().as_ref())
    );

    // 2. CONTROL: default home itself omits `CODEX_SQLITE_HOME`; non-Codex CLIs return `None`.
    let default_env =
        app_server_env(Cli::Codex, &default_home, &user_home, &lookup).expect("default env");
    assert!(
        !default_env.iter().any(|(n, _)| *n == "CODEX_SQLITE_HOME"),
        "default home must not set CODEX_SQLITE_HOME"
    );
    assert!(app_server_env(Cli::Grok, &overlay, &user_home, &lookup).is_none());
    assert!(app_server_env(Cli::Kimi, &overlay, &user_home, &lookup).is_none());
}
