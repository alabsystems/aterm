// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::connector::{AGY, CLAUDE, CODEX, GROK, KIMI, MUSE};
use crate::ai::facts::{ClaudeAccounts, Managed};
use crate::ai::permission::Level;
use crate::ai::state::{Mark, State};
use crate::ai::store::Record;
use crate::ai::testing::{FakeRunner, machine, ran};
use crate::harness::accounts_overlay::links_here;

fn change() -> Change {
    Change {
        root: PathBuf::from("/work/repo"),
        describes: "the uncommitted change".into(),
        diff: "diff --git a/a.rs b/a.rs\n+fn a() {}\n".into(),
        files: 1,
        held_back: vec![".env".into()],
        links: vec!["notes [old]".into()],
    }
}

#[test]
fn a_block_keeps_lines_and_drops_every_escape() {
    let raw = "ok\n\x1b]52;c;ZXZpbA==\x07next\tcol\r\n\u{202E}evil\u{2028}x\u{9b}31m\x7f";
    assert_eq!(
        sanitize_block(raw, 1000),
        "ok\n]52;c;ZXZpbA==next\tcol\nevil\nx31m"
    );
    assert_eq!(sanitize_block("ééé", 4), "éé");
}

#[test]
fn credential_shaped_tokens_are_redacted_and_words_are_not() {
    let text = concat!(
        "key sk-",
        "ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA and sk-proj-BBBBBBBBBBBBBBBBBBBBBBBB, jwt ",
        "eyJhbGciOiJIUzI1NiJ9",
        ".eyJzdWIiOiIxMjM0NTY3ODkwIn0",
        ".dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U aws AKIA",
        "IOSFODNN7EXAMPLE\n-----BEGIN OPENSSH ",
        "PRIVATE KEY-----\nb3BlbnNz\n-----END OPENSSH ",
        "PRIVATE KEY-----\ndone"
    );
    let (out, n) = redact(text);
    assert_eq!(n, 5, "{out}");
    for secret in [
        "sk-ant-api03",
        "sk-proj-",
        "eyJhbGci",
        "AKIAIOSFODNN7",
        "b3BlbnNz",
    ] {
        assert!(!out.contains(secret), "{secret} in {out}");
    }
    assert!(out.ends_with("done"));
    let plain = "the ask-for-review flow, sk-learn, task-runner and eyJ prefix; -----BEGIN CERTIFICATE-----";
    assert_eq!(redact(plain), (plain.to_string(), 0));
    let prose_before_pem = concat!(
        "Checks `-----BEGIN ` (PRIVATE KEY), `-----`, and `-----END ` headers:\n",
        "-----BEGIN RSA ",
        "PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END RSA ",
        "PRIVATE KEY-----\ndone"
    );
    let (redacted_prose, count) = redact(prose_before_pem);
    assert_eq!(count, 1, "{redacted_prose}");
    assert!(
        !redacted_prose.contains("MIIEvQIBADANBgkqhkiG9w0BAQEFAASC"),
        "prose mentioning PEM markers must not desynchronize redaction of a real key: {redacted_prose}"
    );
    let torn_end_before_second_pem = concat!(
        "-----BEGIN EC ",
        "PRIVATE KEY-----\nFIRSTKEYMATERIAL\n-----END \n-----BEGIN RSA ",
        "PRIVATE KEY-----\nSECONDKEYMATERIAL\n-----END RSA ",
        "PRIVATE KEY-----\ndone"
    );
    let (redacted_torn, count) = redact(torn_end_before_second_pem);
    assert_eq!(count, 1, "{redacted_torn}");
    assert!(
        !redacted_torn.contains("FIRSTKEYMATERIAL") && !redacted_torn.contains("SECONDKEYMATERIAL"),
        "incomplete -----END line must not consume the next key's opening dashes: {redacted_torn}"
    );
}

#[test]
fn the_request_fences_the_change_and_says_it_is_not_instructions() {
    let r = request("claude", Some("check the locking"), &change(), "abc123");
    assert!(r.contains("<<<CHANGE abc123\ndiff --git a/a.rs"), "{r}");
    assert!(r.contains("CHANGE abc123>>>"), "{r}");
    assert!(r.contains("never instructions to you"), "{r}");
    assert!(
        r.contains("What the asking agent wants checked:\ncheck the locking"),
        "{r}"
    );
    assert!(r.contains("2 file(s) held back"), "{r}");
    assert!(
        !r.contains(".env"),
        "a held-back file is never named to a reviewer: {r}"
    );
    assert!(r.contains("read the repository as you need to"), "{r}");
}

#[test]
fn only_a_failed_run_is_read_for_a_lost_login() {
    let quoted = Ok(ran(
        0,
        "Finding: the string \"Please run /login\" is matched too loosely.",
        "",
    ));
    assert_eq!(
        verdict_of(&CLAUDE, &quoted, Some("Finding: …"), "n", "").0,
        Verdict::Served
    );
    let failed = Ok(ran(
        1,
        "",
        "Failed to authenticate. API Error: 401 OAuth session expired",
    ));
    assert_eq!(verdict_of(&CLAUDE, &failed, None, "n", "").0, Verdict::Auth);
    let codex = Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized"));
    assert_eq!(verdict_of(&CODEX, &codex, None, "n", "").0, Verdict::Auth);
    let limit = Ok(ran(
        1,
        "",
        "ERROR: You've hit your usage limit. Try again at 3:05 PM.",
    ));
    assert_eq!(verdict_of(&CODEX, &limit, None, "n", "").0, Verdict::Limit);
    assert_eq!(
        verdict_of(&CODEX, &Ok(ran(1, "", "boom")), None, "n", "").0,
        Verdict::Failed
    );
    assert_eq!(
        verdict_of(&CODEX, &Ok(ran(0, "", "")), Some("  "), "n", "").0,
        Verdict::Failed
    );
    assert_eq!(
        verdict_of(&CODEX, &Err(RunFailed::Timeout), None, "n", "").0,
        Verdict::Timeout
    );

    // Synthetic failed runs: partial review text and argument errors are not quota evidence.
    for (stdout, stderr) in [
        ("", "error: unknown option '--spending-limit'"),
        (
            "Finding: the '--spending-limit' flag is not defined here.",
            "API error (status 500): upstream request failed",
        ),
        (
            "Finding: the 'spending limit' label is unclear.",
            "API error (status 500): upstream request failed",
        ),
        (
            "",
            "error: cannot hit your team's API endpoint: connection refused",
        ),
        (
            "",
            "error: cannot hit your team\u{2019}s API endpoint: connection refused",
        ),
    ] {
        assert_eq!(
            verdict_of(
                &GROK,
                &Ok(ran(1, stdout, stderr)),
                None,
                "n",
                "review the change"
            )
            .0,
            Verdict::Failed,
            "{stdout}\n{stderr}"
        );
    }
    for wall in [
        "API error (status 429): You've hit your team's API rate limit.",
        "API error (status 429): You\u{2019}ve hit your team\u{2019}s API rate limit.",
        "You hit your weekly limit.",
        "out of credits or over your spending limit. Add credits and retry.",
        "error: usage balance exhausted",
        "error: usage limit reached",
        "error_code: subscription:free-usage-exhausted",
        "error_code: insufficient_quota",
    ] {
        assert_eq!(
            verdict_of(&GROK, &Ok(ran(1, "", wall)), None, "n", "").0,
            Verdict::Limit,
            "{wall}"
        );
    }
    let quoted_wall = "Finding: the string \"You hit your weekly limit.\" is quoted here.";
    assert_eq!(
        verdict_of(
            &GROK,
            &Ok(ran(0, quoted_wall, "")),
            Some(quoted_wall),
            "n",
            ""
        )
        .0,
        Verdict::Served
    );
}

fn facts_of(c: &'static Connector, shim: PathBuf) -> Facts {
    Facts {
        connector: c,
        kept: None,
        managed: Some(Managed {
            path: shim,
            version: Some("1.0".into()),
        }),
        foreign: None,
        home: Ok(PathBuf::from("/home/me/.x")),
        home_var: None,
        home_exists: true,
        login_file: None,
        hidden_store: false,
        profile_signed_in: true,
        plan: None,
        accounts: None,
        limited_until: None,
        record: Record::default(),
        record_scope: (c.id == "codex").then_some("file"),
        credential_store_error: None,
        env_credentials: Vec::new(),
        pool: None,
        review_source: None,
    }
}

fn row_of(c: &'static Connector, state: State) -> Row {
    Row {
        connector: c,
        wanted: true,
        asked: true,
        state,
        mark: Mark::Ok,
        fact: "signed in".into(),
        fix: None,
        mend: None,
        actor: None,
        version: Some("1.0".into()),
        plan: None,
        used: None,
        renew_by: None,
        limited_until: None,
    }
}

fn claude_account(login: Option<&str>) -> ClaudeAccounts {
    ClaudeAccounts {
        pool_picks: false,
        store: None,
        roster: None,
        label: "main".into(),
        listed: false,
        refused: None,
        unread: None,
        launch_refused: None,
        sign_in_refused: None,
        renews: true,
        login: login.map(str::to_owned),
        made: true,
        state: Some("ok"),
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
        signed_out_at: None,
        next_sign_in: None,
        tried: None,
        unattended: true,
        off: None,
    }
}

#[test]
fn candidates_skip_the_asker_and_the_limited_and_keep_the_unmeasured() {
    let rows = [
        row_of(&CLAUDE, State::Idle),
        row_of(&CODEX, State::Limited(2_000_000_000)),
        row_of(&AGY, State::Ok),
    ];
    let reviewers: Vec<&'static Connector> = vec![&CLAUDE, &CODEX, &AGY];
    let got = candidates(&reviewers, None, "claude", &rows, 1_800_000_000);
    let ids: Vec<(&str, bool)> = got
        .iter()
        .map(|(r, s)| (r.connector.id, s.is_some()))
        .collect();
    assert_eq!(
        ids,
        [("codex", true), ("agy", false)],
        "an unmeasured CLI reviews like any other"
    );
    let got = candidates(
        &reviewers,
        Some(&["claude"]),
        "claude",
        &rows,
        1_800_000_000,
    );
    assert_eq!(got.len(), 1);
    assert!(got[0].1.is_none());
}

#[test]
fn a_reviewer_that_cannot_authenticate_gives_way_to_the_next_and_a_timeout_does_not() {
    let b = machine(&["claude", "codex"]);
    let store = Store::at(&b.state);
    let facts = vec![
        facts_of(&CODEX, b.twin("codex")),
        facts_of(&CLAUDE, b.twin("claude")),
    ];
    let rows = [row_of(&CODEX, State::Idle), row_of(&CLAUDE, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![&CODEX, &CLAUDE];
    let cands = candidates(&reviewers, None, "agy", &rows, 0);
    let var = |k: &str| match k {
        "HOME" => Some("/home/me".to_string()),
        "OPENAI_API_KEY" => Some("sk-should-not-pass".to_string()),
        _ => None,
    };
    let now = || 1_800_000_000;
    let host = Host {
        config: None,
        runner: &FakeRunner::quiet(),
        var: &var,
        vars: &[],
        store: &store,
        now: &now,
    };
    let runner = FakeRunner::with(|call| {
        Ok(if call.program == "codex" {
            ran(1, "", "ERROR: could not be refreshed")
        } else {
            ran(0, "LGTM: no findings", "")
        })
    });
    let host = Host {
        config: None,
        runner: &runner,
        ..host
    };
    let answers = run_review(
        &host,
        &cands,
        &facts,
        &Ask {
            change: &change(),
            asker: "agy",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    let got: Vec<(&str, Verdict)> = answers.iter().map(|a| (a.id, a.verdict)).collect();
    assert_eq!(got, [("codex", Verdict::Auth), ("claude", Verdict::Served)]);
    assert_eq!(answers[1].text.as_deref(), Some("LGTM: no findings"));
    let rec = store.load().expect("load");
    assert!(rec.of_scope("codex", Some("file")).ended() && rec.of("claude").used_at.is_some());
    let runner = FakeRunner::with(|call| {
        if call.program == "codex" {
            Err(RunFailed::Timeout)
        } else {
            Ok(ran(0, "x", ""))
        }
    });
    let host = Host {
        config: None,
        runner: &runner,
        ..host
    };
    let answers = run_review(
        &host,
        &cands,
        &facts,
        &Ask {
            change: &change(),
            asker: "agy",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    assert_eq!(
        answers.iter().map(|a| a.verdict).collect::<Vec<_>>(),
        [Verdict::Timeout]
    );
}

#[test]
fn a_reviewer_runs_from_its_shim_in_the_repository_with_the_askers_environment_and_level() {
    let b = machine(&["claude", "codex"]);
    let store = Store::at(&b.state);
    let mut claude = facts_of(&CLAUDE, b.twin("claude"));
    claude.accounts = Some(claude_account(Some("/state/accounts/logins/main+1")));
    let facts = vec![claude, facts_of(&CODEX, b.twin("codex"))];
    let rows = [row_of(&CLAUDE, State::Ok), row_of(&CODEX, State::Ok)];
    let reviewers: Vec<&'static Connector> = vec![&CLAUDE, &CODEX];
    let cands = candidates(&reviewers, Some(&["claude", "codex"]), "agy", &rows, 0);
    let home = b.home.display().to_string();
    let vars: Vec<(String, String)> = [
        ("HOME", home.as_str()),
        ("PATH", "/opt/tools/bin:/usr/bin"),
        ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("ANTHROPIC_API_KEY", "secret"),
        ("CODEX_THREAD_ID", "t-1"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    let var = |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let runner = FakeRunner::with(|_| Ok(ran(0, "fine", "")));
    let now = || 0;
    let host = Host {
        config: None,
        runner: &runner,
        var: &var,
        vars: &vars,
        store: &store,
        now: &now,
    };
    let _ = run_review(
        &host,
        &cands,
        &facts,
        &Ask {
            change: &change(),
            asker: "agy",
            words: Some("look"),
            count: 2,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    for call in &calls {
        let keys: Vec<&str> = call.env.iter().map(|(k, _)| k.as_str()).collect();
        for (k, v) in [
            ("PATH", "/opt/tools/bin:/usr/bin"),
            ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
        ] {
            assert!(
                call.env.contains(&(k.to_string(), v.to_string())),
                "{} lacks {k}",
                call.program
            );
        }
        for k in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "ANTHROPIC_API_KEY",
            "CODEX_THREAD_ID",
        ] {
            assert!(!keys.contains(&k), "{} got {k}", call.program);
        }
        assert!(
            call.env
                .contains(&(REVIEW_ENV.to_string(), "1".to_string()))
        );
        assert_eq!(
            call.cwd.as_deref(),
            Some(Path::new("/work/repo")),
            "{} reviews in the repository",
            call.program
        );
        assert!(
            call.stdin
                .as_deref()
                .is_some_and(|s| s.contains("<<<CHANGE")),
            "{}",
            call.program
        );
    }
    let claude = calls
        .iter()
        .find(|c| c.program == "claude")
        .expect("claude");
    assert!(claude.env.contains(&(
        crate::harness::accounts::LOGIN_ENV.to_string(),
        "/state/accounts/logins/main+1".to_string()
    )));
    assert!(
        claude
            .args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"),
        "{:?}",
        claude.args
    );
    let codex = calls.iter().find(|c| c.program == "codex").expect("codex");
    assert!(
        codex
            .args
            .iter()
            .any(|a| a == "--dangerously-bypass-approvals-and-sandbox"),
        "{:?}",
        codex.args
    );
    assert!(
        !codex.args.iter().any(|a| a.starts_with("features.")),
        "batteries included: no tool is switched off: {:?}",
        codex.args
    );
    assert_eq!(codex.args.last().map(String::as_str), Some("-"));
}

#[test]
fn the_primers_marker_never_reaches_the_asker_and_a_failure_says_why() {
    assert_eq!(
        strip_markers("no findings\n\nATERM-TASK-DONE\n"),
        "no findings"
    );
    assert_eq!(
        strip_markers("a ATERM-TASK-DONE b"),
        "a ATERM-TASK-DONE b",
        "only a line of its own"
    );
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let facts = vec![facts_of(&CODEX, b.twin("codex"))];
    let rows = [row_of(&CODEX, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![&CODEX];
    let cands = candidates(&reviewers, None, "claude", &rows, 0);
    let runner =
        FakeRunner::with(|_| Ok(ran(1, "", "Error: something broke with \x1b[31mcolour\n")));
    let var = |_: &str| None;
    let now = || 0;
    let host = Host {
        config: None,
        runner: &runner,
        var: &var,
        vars: &[],
        store: &store,
        now: &now,
    };
    let a = run_review(
        &host,
        &cands,
        &facts,
        &Ask {
            change: &change(),
            asker: "claude",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    assert_eq!(a[0].verdict, Verdict::Failed);
    assert_eq!(
        a[0].why.as_deref(),
        Some("it exited 1: Error: something broke with [31mcolour")
    );
}

#[test]
fn a_kept_review_reads_back_and_a_bad_id_reads_nothing() {
    let b = machine(&[]);
    let store = Store::at(&b.state);
    let answers = vec![Answer {
        id: "codex",
        product: "Codex CLI",
        version: Some("0.163.0".into()),
        verdict: Verdict::Served,
        secs: 3,
        text: Some("no findings".into()),
        why: None,
        fix: None,
    }];
    let id = new_id();
    let dir = reviews_dir(&store).join(&id);
    let encoded = json(&id, &dir, "claude", &change(), &answers, 1);
    let recording = aterm_durable::seam::Recording::start();
    keep(&store, &id, &encoded).expect("kept");
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "a kept review is only ordered"
    );
    let (got_id, _, got) = answers_of(&kept(&store, &id).expect("read")).expect("parse");
    assert_eq!((got_id.as_str(), got), (id.as_str(), answers));
    let error = keep(&store, &id, "replacement").expect_err("ids cannot overwrite reviews");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(kept(&store, &id).as_deref(), Some(encoded.as_str()));
    let text = "\\\"\n\t".repeat(ANSWER_MAX / 4);
    let answers: Vec<Answer> = super::super::connector::ROWS
        .iter()
        .take(super::super::config::REVIEW_COUNT_MAX.into())
        .map(|c| Answer {
            id: c.id,
            product: c.product,
            version: None,
            verdict: Verdict::Served,
            secs: 1,
            text: Some(text.clone()),
            why: None,
            fix: None,
        })
        .collect();
    let id = new_id();
    let dir = reviews_dir(&store).join(&id);
    let encoded = json(&id, &dir, "claude", &change(), &answers, 1);
    assert!(encoded.len() > 4 * ANSWER_MAX);
    keep(&store, &id, &encoded).expect("escaped answers kept");
    let (_, _, got) = answers_of(&kept(&store, &id).expect("escaped answers read")).expect("parse");
    assert_eq!(got, answers);
    let oversized_id = new_id();
    let error = keep(&store, &oversized_id, &"x".repeat(KEPT_MAX_BYTES + 1))
        .expect_err("an unreadable document must never be installed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(kept(&store, &oversized_id).is_none());
    for bad in ["../../etc", "r-zzzzzzzzzz", "r-123", "x-0123456789"] {
        assert!(kept(&store, bad).is_none(), "{bad}");
        assert_eq!(
            keep(&store, bad, "{}").expect_err(bad).kind(),
            std::io::ErrorKind::InvalidInput,
            "{bad}"
        );
    }
}

#[test]
fn the_text_names_each_reviewer_and_prefixes_every_line() {
    let answers = vec![
        Answer {
            id: "codex",
            product: "Codex CLI",
            version: Some("0.163.0".into()),
            verdict: Verdict::Served,
            secs: 48,
            text: Some("one\ntwo".into()),
            why: None,
            fix: None,
        },
        Answer {
            id: "claude",
            product: "Claude Code",
            version: None,
            verdict: Verdict::Skipped,
            secs: 0,
            text: None,
            why: Some("usage limit until 2026-10-16 15:05 UTC".into()),
            fix: None,
        },
    ];
    let mut out = Vec::new();
    render(
        &mut out,
        "r-0123456789",
        Path::new("/s/ai/reviews/r-0123456789"),
        &answers,
    );
    assert_eq!(
        String::from_utf8(out).expect("utf8"),
        "review r-0123456789: /s/ai/reviews/r-0123456789\n\
         ── codex 0.163.0 · served in 48 s · untrusted ──\n│ one\n│ two\n\
         ── claude · skipped: usage limit until 2026-10-16 15:05 UTC ──\n"
    );
}

#[test]
fn a_change_about_logins_never_reads_as_a_lost_login() {
    let request = request(
        "claude",
        None,
        &change_with("+    raise Err(\"not logged in\")\n"),
        "abc",
    );
    let echoed = format!(
        "user\n{request}\ncodex\nThe change raises not logged in.\nERROR: stream disconnected before completion"
    );
    let failed = Ok(ran(1, "", &echoed));
    assert_eq!(
        verdict_of(&CODEX, &failed, None, "abc", &request).0,
        Verdict::Failed
    );
    let lost =
        format!("{echoed}\nERROR: Your access token could not be refreshed. Please sign in again.");
    assert_eq!(
        verdict_of(&CODEX, &Ok(ran(1, "", &lost)), None, "abc", &request).0,
        Verdict::Auth
    );
}

fn change_with(diff: &str) -> Change {
    Change {
        diff: diff.to_string(),
        ..change()
    }
}

#[test]
fn an_outcome_under_another_home_is_recorded_nowhere() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let facts = vec![facts_of(&CODEX, b.twin("codex"))];
    let rows = [row_of(&CODEX, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![&CODEX];
    let cands = candidates(&reviewers, None, "claude", &rows, 0);
    let runner = FakeRunner::with(|_| Ok(ran(1, "", "ERROR: could not be refreshed")));
    let now = || 0;
    let ask = Ask {
        change: &change(),
        asker: "claude",
        words: None,
        count: 1,
        budget: Duration::from_secs(5),
        level: Level::Yolo,
    };
    let other = |k: &str| (k == "CODEX_HOME").then(|| "/profiles/w/.codex".to_string());
    let host = Host {
        config: None,
        runner: &runner,
        var: &other,
        vars: &[],
        store: &store,
        now: &now,
    };
    let _ = run_review(&host, &cands, &facts, &ask);
    assert_eq!(
        store.load().expect("load").of_scope("codex", Some("file")),
        Record::default()
    );
    let none = |_: &str| None;
    let host = Host {
        config: None,
        runner: &runner,
        var: &none,
        vars: &[],
        store: &store,
        now: &now,
    };
    let _ = run_review(&host, &cands, &facts, &ask);
    assert!(
        store
            .load()
            .expect("load")
            .of_scope("codex", Some("file"))
            .ended()
    );
}

#[test]
fn a_limit_keeps_the_cli_out_of_reviews_for_a_while() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let facts = vec![facts_of(&CODEX, b.twin("codex"))];
    let rows = [row_of(&CODEX, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![&CODEX];
    let cands = candidates(&reviewers, None, "claude", &rows, 0);
    let runner = FakeRunner::with(|_| Ok(ran(1, "", "ERROR: You've hit your usage limit.")));
    let now = || 1_000;
    let none = |_: &str| None;
    let host = Host {
        config: None,
        runner: &runner,
        var: &none,
        vars: &[],
        store: &store,
        now: &now,
    };
    let ask = Ask {
        change: &change(),
        asker: "claude",
        words: None,
        count: 1,
        budget: Duration::from_secs(5),
        level: Level::Yolo,
    };
    let _ = run_review(&host, &cands, &facts, &ask);
    assert_eq!(
        store
            .load()
            .expect("load")
            .of_scope("codex", Some("file"))
            .limited_until,
        Some(1_000 + LIMIT_WAIT_S)
    );
}

fn review_auth_after_login(replacement_at: Option<i64>) -> (Record, Option<Record>) {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    store
        .note_scope("codex", Some("file"), How::Login, Outcome::Worked, 100)
        .expect("initial login");
    let mut fact = facts_of(&CODEX, b.twin("codex"));
    fact.record = store
        .load()
        .expect("initial record")
        .of_scope("codex", Some("file"));
    let facts = [fact];
    let rows = [row_of(&CODEX, State::Ok)];
    let reviewers: Vec<&'static Connector> = vec![&CODEX];
    let cands = candidates(&reviewers, None, "claude", &rows, 100);
    let replacement = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = std::sync::Arc::clone(&replacement);
    let during_run = store.clone();
    let runner = FakeRunner::with(move |_| {
        if let Some(at) = replacement_at {
            during_run
                .note_scope("codex", Some("file"), How::Login, Outcome::Worked, at)
                .expect("new login");
            *seen.lock().expect("replacement") = Some(
                during_run
                    .load()
                    .expect("new record")
                    .of_scope("codex", Some("file")),
            );
        }
        Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized"))
    });
    let var = |_: &str| None;
    let now = || 300;
    let host = Host {
        config: None,
        runner: &runner,
        var: &var,
        vars: &[],
        store: &store,
        now: &now,
    };
    let change = change();
    let answers = run_review(
        &host,
        &cands,
        &facts,
        &Ask {
            change: &change,
            asker: "claude",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    assert_eq!(
        runner.calls().len(),
        1,
        "the actual review used the injected runner"
    );
    assert_eq!(answers.len(), 1);
    assert_eq!(
        answers[0].verdict,
        Verdict::Auth,
        "the old run's own answer stays truthful"
    );
    let expected = replacement.lock().expect("replacement").clone();
    (
        store
            .load()
            .expect("final record")
            .of_scope("codex", Some("file")),
        expected,
    )
}

#[test]
fn a_review_auth_completion_cannot_end_a_newer_successful_login() {
    let (record, expected) = review_auth_after_login(Some(200));
    assert_eq!(record.used_at, Some(200));
    assert_eq!(
        Some(record),
        expected,
        "old completion must preserve the newer login record"
    );
}

#[test]
fn a_review_auth_completion_cannot_end_a_same_second_replacement_login() {
    let (record, expected) = review_auth_after_login(Some(100));
    assert_eq!(record.used_at, Some(100));
    assert_eq!(
        Some(record),
        expected,
        "equal timestamps do not give an old run the new login"
    );
}

#[test]
fn a_review_auth_completion_still_records_the_login_it_started_under() {
    let (record, replacement) = review_auth_after_login(None);
    assert!(replacement.is_none());
    assert_eq!(record.used_at, Some(100));
    assert_eq!(record.ended_at, Some(300));
    assert_eq!(record.ended_why, Some(Ended::ReviewAuth));
    assert!(record.ended());
}

/// The arguments `c` reviews with at `level`.
fn args_at(c: &'static Connector, level: Level) -> Vec<String> {
    let (_, mut calls) = review_at(c, level);
    calls.pop().expect("ran")
}

/// What a review at `level` that asks `c` alone answers, and the arguments of each run it made.
fn review_at(c: &'static Connector, level: Level) -> (Vec<Answer>, Vec<Vec<String>>) {
    let b = machine(&[c.id]);
    let store = Store::at(&b.state);
    let facts = vec![facts_of(c, b.twin(c.id))];
    let rows = [row_of(c, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![c];
    let cands = candidates(&reviewers, Some(&[c.id]), "nobody", &rows, 0);
    let runner = FakeRunner::with(|_| Ok(ran(0, "fine", "")));
    let none = |_: &str| None;
    let now = || 0;
    let host = Host {
        config: None,
        runner: &runner,
        var: &none,
        vars: &[],
        store: &store,
        now: &now,
    };
    let ask = Ask {
        change: &change(),
        asker: "nobody",
        words: None,
        count: 1,
        budget: Duration::from_secs(5),
        level,
    };
    let answers = run_review(&host, &cands, &facts, &ask);
    (
        answers,
        runner.calls().into_iter().map(|call| call.args).collect(),
    )
}

#[test]
fn each_reviewer_runs_at_the_level_in_its_own_flags() {
    let has = |args: &[String], want: &[&str]| args.windows(want.len()).any(|w| w == want);
    for (c, level, want) in [
        (
            &CLAUDE,
            Level::Yolo,
            &["--dangerously-skip-permissions"][..],
        ),
        (
            &CLAUDE,
            Level::Edits,
            &["--permission-mode", "acceptEdits"][..],
        ),
        (&CLAUDE, Level::Plan, &["--permission-mode", "plan"][..]),
        (
            &CODEX,
            Level::Yolo,
            &["--dangerously-bypass-approvals-and-sandbox"][..],
        ),
        (&CODEX, Level::Edits, &["--sandbox", "workspace-write"][..]),
        (&CODEX, Level::Plan, &["--sandbox", "read-only"][..]),
        (&AGY, Level::Yolo, &["--dangerously-skip-permissions"][..]),
        (&AGY, Level::Plan, &["--mode", "plan"][..]),
        (&MUSE, Level::Yolo, &["--yolo"][..]),
        (&MUSE, Level::Plan, &["--disable-write"][..]),
        (&GROK, Level::Yolo, &["--always-approve"][..]),
        (
            &GROK,
            Level::Edits,
            &["--permission-mode", "acceptEdits"][..],
        ),
        (&GROK, Level::Ask, &["--permission-mode", "dontAsk"][..]),
        (&GROK, Level::Plan, &["--permission-mode", "plan"][..]),
    ] {
        let args = args_at(c, level);
        assert!(has(&args, want), "{} at {}: {args:?}", c.id, level.word());
    }
    for c in crate::ai::connector::ROWS
        .iter()
        .copied()
        .filter(|c| c.review.only_at.is_none_or(|only| only == Level::Ask))
    {
        let args = args_at(c, Level::Ask);
        assert!(
            !args
                .iter()
                .any(|a| a.contains("dangerously") || a == "--yolo" || a == "--always-approve"),
            "{} at ask: {args:?}",
            c.id
        );
    }
    let grok = args_at(
        crate::ai::connector::connector("grok").expect("Grok Build is a connection a review asks"),
        Level::Plan,
    );
    assert!(has(&grok, &["--output-format", "plain"]), "{grok:?}");
    let file = grok
        .iter()
        .position(|a| a == "--prompt-file")
        .expect("Grok Build reads its request from a file");
    assert!(
        grok.get(file + 1)
            .is_some_and(|path| path.ends_with("request.txt")),
        "the request file follows its flag: {grok:?}"
    );
    assert!(
        !grok.iter().any(|a| a == "login" || a == "-p"),
        "a review runs no sign-in and no prompt operand: {grok:?}"
    );
    let muse = args_at(&MUSE, Level::Yolo);
    let yolo = muse.iter().position(|a| a == "--yolo").expect("--yolo");
    let exec = muse.iter().position(|a| a == "exec").expect("exec");
    assert_eq!(exec, 0, "{muse:?}");
    assert!(exec < yolo, "{muse:?}");
    assert!(
        has(&muse, &["--contributor-mode", "off"]),
        "a Muse review runs contributor mode off: {muse:?}"
    );
    assert!(
        has(&muse, &["--model", "muse-wonder-full-1.0"]),
        "a Muse review runs the default model: {muse:?}"
    );
    assert!(
        has(&muse, &["--reasoning-effort", "xhigh"]),
        "a Muse review runs at xhigh: {muse:?}"
    );
}

/// KIMI CODE REVIEWS AT YOLO ALONE, WITH NO LEVEL FLAG: 2.3.3's `-p` forces Never Ask and exits 1
/// on `--yolo`, `--auto` or `--plan` beside it (`docs/measured/kimi-review-2026-10-27.md` §4),
/// so at yolo it runs `--output-format text -p <pointer to request.txt>`, and at edits, ask or
/// plan, which nothing would hold it to, it is skipped with why and never run.
#[test]
fn kimi_reviews_at_yolo_alone_with_no_level_flag() {
    let yolo = args_at(&KIMI, Level::Yolo);
    assert_eq!(yolo.len(), 4, "{yolo:?}");
    assert_eq!(yolo[..3], ["--output-format", "text", "-p"], "{yolo:?}");
    let request = yolo[3]
        .strip_prefix("Read the file ")
        .and_then(|rest| rest.split_once(" and do what it asks"))
        .map(|(path, _)| Path::new(path));
    assert!(
        request.is_some_and(|p| p.is_absolute() && p.ends_with("request.txt")),
        "the prompt points at the request by its absolute path: {yolo:?}"
    );
    for flag in ["--yolo", "--auto", "--plan", "-y", "--add-dir"] {
        assert!(!yolo.iter().any(|a| a == flag), "{flag}: {yolo:?}");
    }
    for level in [Level::Edits, Level::Ask, Level::Plan] {
        let (answers, calls) = review_at(&KIMI, level);
        assert!(calls.is_empty(), "{} ran kimi: {calls:?}", level.word());
        assert_eq!(answers.len(), 1, "{answers:?}");
        assert_eq!(answers[0].verdict, Verdict::Skipped, "{answers:?}");
        assert_eq!(
            answers[0].why.as_deref(),
            Some(
                format!(
                    "its one-shot mode runs at yolo alone, and this review is at {}",
                    level.word()
                )
                .as_str()
            )
        );
    }
    // CONTROL: a row its flags hold to each level runs at every one.
    for level in [Level::Yolo, Level::Edits, Level::Ask, Level::Plan] {
        assert_eq!(review_at(&GROK, level).1.len(), 1, "{}", level.word());
    }
}

/// KIMI CODE'S REVIEW VERDICTS READ 2.3.3'S OWN FAILURES (`kimi-review-2026-10-27.md` §3): a
/// managed login with no token and a home with no model are its login (Auth), a quota kimi gave
/// up on is its limit (Limit), and a hand-written provider's missing key or 401 is a failure of
/// its own (Failed). A served answer that quotes a wall is served.
#[test]
fn kimi_review_verdicts_read_its_measured_failures() {
    let failed = |line: &str| {
        Ok(ran(
            1,
            "",
            &format!(
                "kimi version 2.3.3\nerror: failed to run prompt: {line}\nSee log: \
                 /h/.kimi-code/logs/kimi-code.log\n"
            ),
        ))
    };
    for (line, want) in [
        (
            "provider managed:kimi-code has no credential configured",
            Verdict::Auth,
        ),
        (
            "No model configured. Run `kimi` and use /login to sign in, then retry; or set \
             default_model in config.toml.",
            Verdict::Auth,
        ),
        (
            "Stored token for \"kimi-code\" was rejected; re-login required.",
            Verdict::Auth,
        ),
        (
            "provider.rate_limit: 429 You exceeded your current quota, please check your plan \
             and billing details.",
            Verdict::Limit,
        ),
        (
            "provider home has no credential configured",
            Verdict::Failed,
        ),
        (
            "provider.auth_error: 401 Invalid Authentication",
            Verdict::Failed,
        ),
    ] {
        assert_eq!(
            verdict_of(&KIMI, &failed(line), None, "n", "review the change").0,
            want,
            "{line}"
        );
    }
    let quoted = "\u{2022} Finding: \"You exceeded your current quota\" is matched too loosely.";
    assert_eq!(
        verdict_of(&KIMI, &Ok(ran(0, quoted, "")), Some(quoted), "n", "").0,
        Verdict::Served
    );
}

#[test]
fn claude_under_a_home_of_its_own_gets_no_login_variable() {
    let f = facts_of(&CLAUDE, PathBuf::from("/pkg/agents/claude"));
    let own = |k: &str| (k == "CLAUDE_CONFIG_DIR").then(|| "/me/.config/claude".to_string());
    let env = reviewer_env(&CLAUDE, &f, &own, &[]);
    assert!(
        env.iter()
            .all(|(k, _)| k != crate::harness::accounts::LOGIN_ENV),
        "{env:?}"
    );
    assert!(env.contains(&(
        "CLAUDE_CONFIG_DIR".to_string(),
        "/me/.config/claude".to_string()
    )));
    let none = |_: &str| None;
    let env = reviewer_env(&CLAUDE, &f, &none, &[]);
    assert!(
        env.contains(&(
            crate::harness::accounts::LOGIN_ENV.to_string(),
            String::new()
        )),
        "the control"
    );
}

/// A REVIEW RUNS ON THE LOGIN IT WAS STARTED WITH WHERE THE POOL PICKS NOTHING, AND ON THE POOL'S
/// CHOICE WHERE IT PICKS: rotation off leaves a chooser's login as a new session does. CONTROL: a
/// pool that picks, with no row, runs on none of that start's.
#[test]
fn a_review_keeps_the_login_it_was_started_with_only_where_the_pool_picks_nothing() {
    let mut f = facts_of(&CLAUDE, PathBuf::from("/pkg/agents/claude"));
    let started = |k: &str| {
        (k == crate::harness::accounts::LOGIN_ENV).then(|| "/me/logins/started".to_string())
    };
    let login_of = |f: &crate::ai::facts::Facts| {
        reviewer_env(&CLAUDE, f, &started, &[])
            .into_iter()
            .find(|(k, _)| k == crate::harness::accounts::LOGIN_ENV)
            .map(|(_, v)| v)
    };
    f.accounts = Some(claude_account(None));
    assert_eq!(login_of(&f).as_deref(), Some("/me/logins/started"));
    f.accounts.as_mut().unwrap().pool_picks = true;
    assert_eq!(login_of(&f).as_deref(), Some(""), "CONTROL");
    f.accounts.as_mut().unwrap().login = Some("/me/logins/picked".into());
    assert_eq!(login_of(&f).as_deref(), Some("/me/logins/picked"));
}

/// A REVIEW'S REFRESHED ROW SAYS WHETHER THE POOL PICKS, which is what keeps the login a review was
/// started with: rotation on picks, and rotation off does not.
#[test]
fn a_reviews_refreshed_row_says_whether_the_pool_picks() {
    let pool = ReviewPool::new(crate::harness::accounts::Cli::Claude, None);
    let config = pool.config.join("aterm.toml");
    let picks = |text: &str| {
        aterm_uds::atomic_file::write(&config, text.as_bytes(), 0o600).unwrap();
        let mut facts = pool.facts();
        review_row(&mut facts, ReviewPool::NOW, Some(&config)).unwrap();
        facts.accounts.unwrap().pool_picks
    };
    assert!(picks(""));
    assert!(!picks("[harness]\naccounts = \"off\"\n"));
}

#[test]
fn escaped_keys_and_more_token_shapes_are_redacted() {
    let json = concat!(
        r#"{"private_key": "-----BEGIN "#,
        r#"PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0B\n-----END "#,
        r#"PRIVATE KEY-----\n", "x": 1}"#
    );
    let (out, n) = redact(json);
    assert_eq!(n, 1, "{out}");
    assert!(!out.contains("MIIEvQ") && out.contains("\"x\": 1"), "{out}");
    for token in [
        concat!("sk_", "live_4eC39HqLyjWDarjtT1zdp7dc"),
        concat!("AI", "zaSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY"),
        concat!("gh", "s_16C7e42F292c6912E7710c838347Ae178B4a"),
        concat!("xo", "xp-1234567890-abcdefghij"),
    ] {
        let (out, n) = redact(&format!("token {token} end"));
        assert_eq!((out.as_str(), n), ("token [redacted] end", 1), "{token}");
    }
}

#[test]
fn a_live_limit_skips_a_cli_whatever_its_state() {
    let mut due = row_of(&CLAUDE, State::Due(2_000_000_000));
    due.limited_until = Some(1_800_000_600);
    let rows = [due];
    let reviewers: Vec<&'static Connector> = vec![&CLAUDE];
    let got = candidates(&reviewers, None, "codex", &rows, 1_800_000_000);
    assert!(
        got[0]
            .1
            .as_deref()
            .is_some_and(|w| w.starts_with("usage limit")),
        "{got:?}"
    );
    let got = candidates(&reviewers, None, "codex", &rows, 1_800_001_000);
    assert!(got[0].1.is_none());
}

/// A SKIPPED REVIEWER NAMES WHAT A PERSON DOES FIRST: a signed-out Claude Code row whose sign-in
/// starts nothing until a person mends accounts.toml ([`Row::mend`], no [`Row::fix`]) is skipped
/// with that step as its fix, and runs nothing. CONTROL: a signed-out row with a command keeps
/// the command.
#[test]
fn a_skipped_reviewer_names_what_a_person_does_first() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let facts = vec![facts_of(&CLAUDE, b.twin("claude"))];
    let step = "mend accounts.toml (`aterm harness accounts` says how), then sign claude in";
    for (fix, mend, want) in [
        (None, Some(step), step),
        (Some("aterm ai login claude"), None, "aterm ai login claude"),
    ] {
        let mut row = row_of(&CLAUDE, State::SignedOut);
        row.fix = fix.map(str::to_string);
        row.mend = mend.map(str::to_string);
        let rows = [row];
        let reviewers: Vec<&'static Connector> = vec![&CLAUDE];
        let cands = candidates(&reviewers, None, "codex", &rows, 0);
        let runner = FakeRunner::with(|_| Ok(ran(0, "fine", "")));
        let none = |_: &str| None;
        let now = || 0;
        let host = Host {
            config: None,
            runner: &runner,
            var: &none,
            vars: &[],
            store: &store,
            now: &now,
        };
        let ask = Ask {
            change: &change(),
            asker: "codex",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        };
        let answers = run_review(&host, &cands, &facts, &ask);
        assert!(runner.calls().is_empty(), "{want}: {:?}", runner.calls());
        assert_eq!(answers[0].verdict, Verdict::Skipped, "{answers:?}");
        assert_eq!(answers[0].fix.as_deref(), Some(want), "{answers:?}");
    }
}

fn scripted_claude_answer(exit: i32, stdout: String, stderr: String) -> Answer {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let facts = vec![facts_of(&CLAUDE, b.twin("claude"))];
    let rows = [row_of(&CLAUDE, State::Ok)];
    let reviewers: Vec<&'static Connector> = vec![&CLAUDE];
    let cands = candidates(&reviewers, Some(&["claude"]), "codex", &rows, 0);
    let runner = FakeRunner::with(move |_| Ok(ran(exit, &stdout, &stderr)));
    let none = |_: &str| None;
    let now = || 1_000;
    let host = Host {
        config: None,
        runner: &runner,
        var: &none,
        vars: &[],
        store: &store,
        now: &now,
    };
    let mut answers = run_review(
        &host,
        &cands,
        &facts,
        &Ask {
            change: &change(),
            asker: "codex",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    assert_eq!(runner.calls().len(), 1);
    assert_eq!(answers.len(), 1);
    answers.pop().expect("one answer")
}

#[test]
fn a_served_answer_redacts_a_key_before_its_display_cut() {
    let key = format!("sk-proj-{}", "A".repeat(48));
    let control = scripted_claude_answer(0, format!("normal {key} end"), String::new());
    assert_eq!(control.verdict, Verdict::Served);
    assert_eq!(control.text.as_deref(), Some("normal [redacted] end"));

    let raw = format!("{} {key}", "é".repeat((ANSWER_MAX - 12) / 2));
    let answer = scripted_claude_answer(0, raw, String::new());
    assert_eq!(answer.verdict, Verdict::Served);
    let text = answer.text.expect("served text");
    assert!(text.len() <= ANSWER_MAX);
    assert!(!text.contains("sk-proj-"), "a cut key prefix survived");
    assert!(text.contains("[redacted]"));
}

#[test]
fn a_failure_reason_redacts_a_key_before_its_display_cut() {
    let key = format!("sk-proj-{}", "B".repeat(48));
    let control = scripted_claude_answer(1, String::new(), format!("Error: {key}"));
    assert_eq!(control.verdict, Verdict::Failed);
    let why = control.why.expect("failure reason");
    assert!(!why.contains("sk-proj-"));
    assert!(why.contains("[redacted]"));

    let raw = format!("{} {key}", "é".repeat(94));
    let answer = scripted_claude_answer(1, String::new(), raw);
    assert_eq!(answer.verdict, Verdict::Failed);
    let why = answer.why.expect("failure reason");
    assert!(why.len() <= 400);
    assert!(!why.contains("sk-proj-"), "a cut key prefix survived");
    assert!(why.contains("[redacted]"));
}

/// Runs the review attribution path with an optional environment credential override.
fn review_on_frozen_login_with_env(
    c: &'static Connector,
    key: Option<&'static str>,
) -> (Answer, Record, Vec<crate::ai::testing::Call>) {
    let b = machine(&[c.id]);
    let store = Store::at(&b.state);
    let scope = (c.id == "codex").then_some("file");
    store
        .note_scope(c.id, scope, How::Login, Outcome::Worked, 100)
        .expect("initial login");
    let mut fact = facts_of(c, b.twin(c.id));
    fact.record = store.load().expect("record").of_scope(c.id, scope);
    fact.env_credentials = key.into_iter().collect();
    let facts = [fact];
    let rows = [row_of(c, State::Ok)];
    let reviewers = [c];
    let candidates = candidates(&reviewers, Some(&[c.id]), "another-cli", &rows, 100);
    let mut vars = vec![
        ("HOME".to_string(), b.home.display().to_string()),
        ("PATH".to_string(), "/private/synthetic/tools".to_string()),
        (
            "SSH_AUTH_SOCK".to_string(),
            "/private/synthetic/socket".to_string(),
        ),
    ];
    if let Some(key) = key {
        vars.push((key.to_string(), "synthetic-credential".to_string()));
    }
    let var = |key: &str| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let owned_runs = store.dir().to_path_buf();
    let runner = FakeRunner::with(move |call| {
        let override_present = call
            .env
            .iter()
            .any(|(k, _)| c.credential_env.contains(&k.as_str()));
        if override_present {
            return Ok(ran(1, "", "ERROR: unauthorized; unexpected status 401"));
        }
        if let AnswerVia::File(flag) = c.review.answer {
            let at = call
                .args
                .iter()
                .position(|a| a == flag)
                .expect("answer flag");
            let answer = Path::new(call.args.get(at + 1).expect("answer file"));
            assert!(
                answer.starts_with(&owned_runs),
                "only the actual private answer file"
            );
            std::fs::write(answer, "frozen-login review").expect("private answer");
        }
        Ok(ran(0, "frozen-login review", ""))
    });
    let now = || 300;
    let host = Host {
        config: None,
        runner: &runner,
        var: &var,
        vars: &vars,
        store: &store,
        now: &now,
    };
    let change = change();
    let answers = run_review(
        &host,
        &candidates,
        &facts,
        &Ask {
            change: &change,
            asker: "another-cli",
            words: None,
            count: 1,
            budget: Duration::from_secs(60),
            level: Level::Plan,
        },
    );
    assert_eq!(answers.len(), 1, "actual review reached");
    let calls = runner.calls();
    assert_eq!(calls.len(), 1, "actual injected runner reached");
    for (key, value) in [
        ("PATH", "/private/synthetic/tools"),
        ("SSH_AUTH_SOCK", "/private/synthetic/socket"),
    ] {
        assert!(
            calls[0].env.contains(&(key.to_string(), value.to_string())),
            "ordinary environment {key} stays"
        );
    }
    (
        answers.into_iter().next().expect("answer"),
        store.load().expect("final record").of_scope(c.id, scope),
        calls,
    )
}

#[test]
fn reviewers_keep_the_frozen_login_instead_of_inherited_credential_overrides() {
    let mut wrong = Vec::new();
    for (c, key) in [(&CODEX, "OPENAI_API_KEY"), (&MUSE, "META_API_KEY")] {
        let (control, record, _) = review_on_frozen_login_with_env(c, None);
        assert_eq!(
            control.verdict,
            Verdict::Served,
            "{} admitted control",
            c.id
        );
        assert_eq!(
            record.used_at,
            Some(300),
            "{} actual Store credit control",
            c.id
        );
        assert!(!record.ended());
        let (answer, record, calls) = review_on_frozen_login_with_env(c, Some(key));
        let passed = calls[0].env.iter().any(|(k, _)| k == key);
        if passed || answer.verdict != Verdict::Served || record.ended() {
            wrong.push((c.id, key, passed, answer.verdict, record.ended_why));
        }
    }
    assert!(
        wrong.is_empty(),
        "inherited credential changed the frozen-login review/Store record: {wrong:?}"
    );
}

#[test]
fn a_pooled_codex_reviewer_exports_serving_auth_path_unless_custom_home_or_keyring() {
    use crate::ai::facts::CliAccounts;

    let b = machine(&["codex"]);
    let shim = b.twin("codex");
    let home = b.home.display().to_string();
    let staged_auth = "/state/logins/codex/work+1/auth.json";

    let mut pooled = facts_of(&CODEX, shim.clone());
    pooled.pool = Some(CliAccounts {
        roster: PathBuf::from("/config/accounts-codex.toml"),
        rows: 2,
        enrolled: 1,
        serving_login: Some(("CODEX_AUTH_PATH", Some(staged_auth.to_string()))),
        ..CliAccounts::default()
    });
    let base_vars: Vec<(String, String)> = [
        ("HOME", home.as_str()),
        ("PATH", "/usr/bin"),
        ("CODEX_AUTH_PATH", "/stale/inherited/auth.json"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    let var_default = |k: &str| {
        base_vars
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    };
    let env_staged = reviewer_env(&CODEX, &pooled, &var_default, &base_vars);
    let auth_vals: Vec<&str> = env_staged
        .iter()
        .filter(|(k, _)| k == "CODEX_AUTH_PATH")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(auth_vals, [staged_auth]);

    // Default-home serving row (`None` value) unsets CODEX_AUTH_PATH cleanly.
    let mut default_row = pooled.clone();
    if let Some(p) = &mut default_row.pool {
        p.serving_login = Some(("CODEX_AUTH_PATH", None));
    }
    let env_default_row = reviewer_env(&CODEX, &default_row, &var_default, &base_vars);
    assert!(
        !env_default_row.iter().any(|(k, _)| k == "CODEX_AUTH_PATH"),
        "{env_default_row:?}"
    );

    // Custom CODEX_HOME keeps CODEX_HOME and does not inject a default-home pool's CODEX_AUTH_PATH.
    let mut custom_vars = base_vars.clone();
    custom_vars.push(("CODEX_HOME".to_string(), "/custom/codex".to_string()));
    let var_custom = |k: &str| {
        custom_vars
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    };
    let env_custom = reviewer_env(&CODEX, &pooled, &var_custom, &custom_vars);
    assert!(
        env_custom.contains(&("CODEX_HOME".to_string(), "/custom/codex".to_string()))
            && !env_custom.iter().any(|(k, _)| k == "CODEX_AUTH_PATH"),
        "{env_custom:?}"
    );

    // Keyring store suppresses CODEX_AUTH_PATH injection as well.
    let mut keyring = pooled;
    keyring.hidden_store = true;
    let env_keyring = reviewer_env(&CODEX, &keyring, &var_default, &base_vars);
    assert!(
        !env_keyring.iter().any(|(k, _)| k == "CODEX_AUTH_PATH"),
        "{env_keyring:?}"
    );
}

/// Writes `b`'s atpkg launcher for Codex to exec a codex-alab package laid beside it
/// ([`crate::harness::codex_alab::lay_package`]), one that keeps `CODEX_AUTH_PATH`'s login where
/// `reads`.
fn alab_launcher(b: &crate::ai::testing::Box3, reads: bool) {
    let exe = crate::harness::codex_alab::lay_package(&b.dir.path().join("store/codex"), reads);
    std::fs::write(b.twin("codex"), crate::harness::codex_alab::twin_text(&exe)).expect("launcher");
}

/// A POOLED CODEX REVIEW ON A ROW OF ITS OWN RUNS ONLY ON A BUILD THAT KEEPS `CODEX_AUTH_PATH`'S
/// LOGIN ITS OWN: one that does not (OpenAI's, an older codex-alab, or a launcher that names no
/// build) would review on the default login while the pool counted the outcome as the row's, so
/// nothing is run, and the answer names the pin of the default row as its fix where that row can
/// serve. Where it stands at a wall, the answer says so and offers no pin (the next read of the
/// wall would end it). The default row's review needs no such build.
#[test]
fn a_pooled_codex_review_on_a_row_of_its_own_needs_a_build_that_reads_codex_auth_path() {
    use crate::ai::facts::CliAccounts;
    use crate::harness::codex_alab::DefaultRow;

    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let serves = DefaultRow::Serves("main".into());
    let on_with = |login: Option<&str>, default: &DefaultRow| {
        let mut f = facts_of(&CODEX, b.twin("codex"));
        f.pool = Some(CliAccounts {
            roster: PathBuf::from("/config/accounts-codex.toml"),
            rows: 2,
            enrolled: 1,
            serving_label: Some("work".into()),
            default_label: Some("main".into()),
            default_row: Some(default.clone()),
            serving_login: Some(("CODEX_AUTH_PATH", login.map(str::to_string))),
            ..CliAccounts::default()
        });
        vec![f]
    };
    let on = |login: Option<&str>| on_with(login, &serves);
    let rows = [row_of(&CODEX, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![&CODEX];
    let cands = candidates(&reviewers, None, "claude", &rows, 0);
    let var = |k: &str| (k == "HOME").then(|| "/home/me".to_string());
    let now = || 1_800_000_000;
    let runner = FakeRunner::with(|call| {
        if let Some(out) = call
            .args
            .windows(2)
            .find_map(|w| (w[0] == "-o").then_some(&w[1]))
        {
            std::fs::write(out, "no findings").expect("write answer");
        }
        Ok(ran(0, "", ""))
    });
    let host = Host {
        config: None,
        runner: &runner,
        var: &var,
        vars: &[],
        store: &store,
        now: &now,
    };
    let review = |facts: &[Facts]| {
        run_review(
            &host,
            &cands,
            facts,
            &Ask {
                change: &change(),
                asker: "claude",
                words: None,
                count: 1,
                budget: Duration::from_secs(5),
                level: Level::Yolo,
            },
        )
    };
    let row_login = "/state/logins/codex/work/auth.json";
    for (what, reads) in [("names no build", None), ("old codex-alab", Some(false))] {
        if let Some(reads) = reads {
            alab_launcher(&b, reads);
        }
        let answers = review(&on(Some(row_login)));
        assert_eq!(answers[0].verdict, Verdict::Failed, "{what}");
        assert_eq!(
            answers[0].fix.as_deref(),
            Some("aterm harness accounts use main --cli codex"),
            "{what}: {:?}",
            answers[0].why
        );
        assert!(runner.calls().is_empty(), "{what}: nothing runs");
    }
    let walled = DefaultRow::Walled {
        label: "main".into(),
        window: "five_hour".into(),
        reset: Some(1_800_003_600),
        until: 1_800_003_750,
    };
    let answers = review(&on_with(Some(row_login), &walled));
    let why = answers[0].why.as_deref().unwrap_or_default();
    assert_eq!(answers[0].verdict, Verdict::Failed);
    assert_eq!(answers[0].fix, None, "no pin of a walled row: {why}");
    assert!(
        why.contains("aterm pkg update codex") && !why.contains("accounts use"),
        "{why}"
    );
    assert!(runner.calls().is_empty(), "nothing runs");
    // Where the owner's pin holds the review to `work`, `main` serves only once that pin ends
    // (its own pin, after its wall ends, or the pin's end) and its wall ends, never by itself.
    let mut pinned = on_with(Some(row_login), &walled);
    if let Some(pool) = pinned[0].pool.as_mut() {
        pool.pinned = Some("work".into());
    }
    let answers = review(&pinned);
    let why = answers[0].why.as_deref().unwrap_or_default();
    assert_eq!(answers[0].verdict, Verdict::Failed);
    assert_eq!(
        answers[0].fix, None,
        "no pin of a walled row before its wall ends: {why}"
    );
    assert!(
        why.contains(&crate::harness::codex_alab::unread_fix(Some("main")))
            && why.contains(&crate::harness::usage::rfc3339_utc(1_800_003_750)),
        "{why}"
    );
    assert!(runner.calls().is_empty(), "nothing runs");
    let answers = review(&on(None));
    assert_eq!(
        answers[0].verdict,
        Verdict::Served,
        "the default row's review"
    );
    assert_eq!(runner.calls().len(), 1);
    alab_launcher(&b, true);
    let answers = review(&on(Some(row_login)));
    assert_eq!(answers[0].verdict, Verdict::Served, "{:?}", answers[0].why);
    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    assert!(
        calls[1]
            .env
            .contains(&("CODEX_AUTH_PATH".to_string(), row_login.to_string())),
        "{:?}",
        calls[1].env
    );
}

#[test]
fn a_pooled_codex_review_limit_marks_only_the_serving_account_so_the_next_account_serves() {
    use crate::ai::facts::{Machine, gather};
    use crate::ai::state::assess;
    use crate::ai::store::Records;
    use crate::harness::accounts::{self, Cli, Store as AccountStore};

    let b = machine(&["codex"]);
    alab_launcher(&b, true);
    let now_val = 1_800_000_000_i64;
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    let claude_roster = config.join("accounts.toml");
    let work_1_base = b.home.join(".aterm/logins/codex/work-1");
    let work_2_base = b.home.join(".aterm/logins/codex/work-2");
    std::fs::write(
        config.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"work-1\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work-1@example.test\"\n\n\
             [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = {:?}\nemail = \"work-2@example.test\"\n",
            work_1_base.display().to_string(),
            work_2_base.display().to_string(),
        ),
    )
    .expect("codex roster");
    let acct_store = AccountStore::at_for(&b.state, Cli::Codex, &b.home);
    let staging_1 = b.home.join(".aterm/logins/codex/work-1+1");
    let staging_2 = b.home.join(".aterm/logins/codex/work-2+1");
    for (label, base, staging) in [
        ("work-1", &work_1_base, &staging_1),
        ("work-2", &work_2_base, &staging_2),
    ] {
        codex_fixture_login(staging, &format!("{label}@example.test"), now_val);
        let mut note = None;
        acct_store
            .transact_logins(now_val, &mut note, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    label,
                    Some(&base.display().to_string()),
                    &staging.display().to_string(),
                    None,
                    now_val - 60,
                ))
            })
            .expect("renew");
    }
    // Pin work-1 active (unpinned) so work-1 serves first.
    let mut note = None;
    acct_store
        .transact(now_val, &mut note, |state| {
            state.active = Some("work-1".to_string());
            state.pinned = false;
            Ok(Vec::new())
        })
        .expect("active work-1");

    let home_str = b.home.display().to_string();
    let vars = vec![("HOME".to_string(), home_str.clone())];
    let var = |k: &str| (k == "HOME").then(|| home_str.clone());
    let store = Store::at(&b.state);

    let m1 = Machine {
        now: now_val,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: store.load().unwrap_or_else(|_| Records::default()),
        harness_state: Some(b.state.clone()),
        roster: Some(claude_roster.clone()),
        accounts_mode: None,
        config: None,
    };
    let f1 = gather(&m1, &CODEX);
    assert_eq!(
        f1.pool.as_ref().and_then(|p| p.serving_label.as_deref()),
        Some("work-1")
    );
    let r1 = assess(&f1, true, now_val);
    assert_eq!(r1.state, State::Ok);

    // Run review: work-1 hits usage limit -> recorded on work-1 in accounts-codex, not CLI-wide store.
    let runner_limit = FakeRunner::with(|_| Ok(ran(1, "", "ERROR: You've hit your usage limit")));
    let now = || now_val;
    let host_limit = Host {
        config: None,
        runner: &runner_limit,
        var: &var,
        vars: &vars,
        store: &store,
        now: &now,
    };
    let rows1 = [r1];
    let reviewers: Vec<&'static Connector> = vec![&CODEX];
    let cands1 = candidates(&reviewers, None, "claude", &rows1, now_val);
    let facts1 = vec![f1];
    let answers1 = run_review(
        &host_limit,
        &cands1,
        &facts1,
        &Ask {
            change: &change(),
            asker: "claude",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    assert_eq!(answers1[0].verdict, Verdict::Limit);
    assert_eq!(
        store
            .load()
            .expect("ai store")
            .of_scope("codex", Some("file"))
            .limited_until,
        None,
        "isolated pool account limit does not poison CLI-wide connections.json"
    );

    // Next gather + assess immediately rotates to work-2 and serves cleanly!
    let m2 = Machine {
        records: store.load().expect("ai store"),
        ..m1
    };
    let f2 = gather(&m2, &CODEX);
    assert_eq!(
        f2.pool.as_ref().and_then(|p| p.serving_label.as_deref()),
        Some("work-2")
    );
    let r2 = assess(&f2, true, now_val);
    assert_eq!(r2.state, State::Ok, "{}", r2.fact);
    let expected_auth_2 = staging_2.join("auth.json").display().to_string();
    let runner_ok = FakeRunner::with(move |call| {
        assert!(
            call.env
                .contains(&("CODEX_AUTH_PATH".to_string(), expected_auth_2.clone(),))
        );
        let out_file = call
            .args
            .windows(2)
            .find_map(|w| (w[0] == "-o").then_some(&w[1]))
            .expect("-o path");
        std::fs::write(out_file, "no findings").expect("write answer");
        Ok(ran(0, "", ""))
    });
    let host_ok = Host {
        config: None,
        runner: &runner_ok,
        ..host_limit
    };
    let rows2 = [r2];
    let cands2 = candidates(&reviewers, None, "claude", &rows2, now_val);
    let facts2 = vec![f2];
    let answers2 = run_review(
        &host_ok,
        &cands2,
        &facts2,
        &Ask {
            change: &change(),
            asker: "claude",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    assert_eq!(answers2[0].verdict, Verdict::Served);
    assert_eq!(
        store
            .load()
            .expect("ai store")
            .of_scope("codex", Some("file"))
            .used_at,
        None
    );
    assert_eq!(
        acct_store
            .load_logins(now_val)
            .unwrap()
            .0
            .get("work-2")
            .unwrap()
            .served_at,
        Some(now_val)
    );
}

/// A pool of `cli`'s on a fresh machine: `main`, this machine's own login, and `work`, a login of
/// its own, both signed in here, beside its roster in `config`. For Codex, `reads` lays atpkg's
/// launcher onto a codex-alab that keeps `CODEX_AUTH_PATH`'s login (`true`) or one from before
/// it (`false`).
fn codex_fixture_login(home: &Path, email: &str, now: i64) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store='file'\n",
    )
    .unwrap();
    let token =
        crate::harness::codex_app_login::tests::make_chatgpt_jwt(Some(email), "plus", now + 3600);
    std::fs::write(
        home.join("auth.json"),
        aterm_json::json!({
            "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token}
        })
        .to_string(),
    )
    .unwrap();
}

struct ReviewPool {
    b: crate::ai::testing::Box3,
    config: PathBuf,
    cli: crate::harness::accounts::Cli,
}

impl ReviewPool {
    const NOW: i64 = 1_800_000_000;

    fn new(cli: crate::harness::accounts::Cli, reads: Option<bool>) -> ReviewPool {
        Self::with_scope(cli, reads, true)
    }

    fn with_scope(
        cli: crate::harness::accounts::Cli,
        reads: Option<bool>,
        listed: bool,
    ) -> ReviewPool {
        let c = cli.connector();
        let b = machine(&[c.id]);
        if let Some(reads) = reads {
            alab_launcher(&b, reads);
        }
        let config = b.dir.path().join("config");
        std::fs::create_dir_all(&config).expect("config");
        let pool = ReviewPool { b, config, cli };
        let store = if cli == crate::harness::accounts::Cli::Claude {
            std::fs::write(
                pool.b.home.join(".claude.json"),
                r#"{"oauthAccount":{"emailAddress":"owner@example.com"}}"#,
            )
            .expect("Claude profile");
            ".credentials.json"
        } else {
            c.stores.first().copied().expect("a login file")
        };
        let default = pool.b.home.join(cli.accounts().default_home).join(store);
        for login in [&default, &pool.work().join(store)] {
            std::fs::create_dir_all(login.parent().expect("parent")).expect("login home");
            std::fs::write(login, "{}").expect("stand-in login");
        }
        if cli == crate::harness::accounts::Cli::Codex {
            codex_fixture_login(default.parent().unwrap(), "owner@example.com", Self::NOW);
            codex_fixture_login(&pool.work(), "work@example.com", Self::NOW);
        }
        if listed {
            pool.roster(None);
        }
        pool
    }

    /// `work`'s own login home.
    fn work(&self) -> PathBuf {
        self.b
            .home
            .join(".aterm/logins")
            .join(self.cli.id())
            .join("work")
    }

    /// The roster of `main` and `work`, `main` held for the warm set of the Mac `held_by` names.
    fn roster(&self, held_by: Option<&str>) {
        use crate::harness::accounts;
        // Kimi Code's rows name the account system their email belongs to.
        let vendor = accounts::Vendor {
            region: (self.cli == accounts::Cli::Kimi)
                .then_some(crate::ai::connector::Region::Global),
            ..accounts::Vendor::default()
        };
        let rows = [
            accounts::Row {
                label: "main".into(),
                email: Some("owner@example.com".into()),
                held_by: held_by.map(str::to_string),
                vendor,
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "work".into(),
                email: Some("work@example.com".into()),
                dir: Some(self.work().display().to_string()),
                vendor,
                ..accounts::Row::default()
            },
        ];
        let text = accounts::append_rows_for(self.cli, "", &rows).expect("the rows admit");
        std::fs::write(self.config.join(self.cli.accounts().roster_file), text).expect("roster");
    }

    fn store(&self) -> crate::harness::accounts::Store {
        crate::harness::accounts::Store::at_for(&self.b.state, self.cli, &self.b.home)
    }

    fn state(&self, change: impl FnOnce(&mut crate::harness::accounts::State)) {
        self.store()
            .transact(Self::NOW, &mut None, |state| {
                change(state);
                Ok(Vec::new())
            })
            .expect("state");
    }

    fn auth(&self, label: &str, auth: crate::harness::accounts::Auth) {
        use crate::harness::accounts::{self, Cli};
        let store = self.store();
        let path = self.config.join(self.cli.accounts().roster_file);
        let records = store.read_account_records(Some(&path), Self::NOW).unwrap();
        let raw = records.raw_roster.row(label).unwrap();
        let context = store.capture_auth_context(records.roster.row(label).unwrap());
        store
            .transact_logins(Self::NOW, &mut None, |state, logins| {
                if self.cli == Cli::Codex {
                    accounts::stage_auth(state, raw, logins, context.unwrap(), auth, Self::NOW);
                    Ok(Vec::new())
                } else {
                    Ok(accounts::note_auth(state, label, auth, Self::NOW))
                }
            })
            .unwrap();
    }

    fn pin(&self, label: &str) {
        self.state(|state| {
            state.active = Some(label.to_string());
            state.pinned = true;
        });
    }

    fn share_work(&self) {
        self.store()
            .transact_logins(Self::NOW, &mut None, |_, logins| {
                logins.shared.begin(
                    crate::harness::accounts::shared::Intent {
                        source: crate::harness::accounts::shared::Source {
                            network: [1; 32],
                            account: [2; 32],
                        },
                        decision: [3; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: "work".into(),
                        identity: "work@example.com".into(),
                        staging: self
                            .b
                            .dir
                            .path()
                            .join("shared-staging")
                            .to_str()
                            .unwrap()
                            .into(),
                        before_generation: None,
                        begun_at: Self::NOW,
                    },
                    None,
                )?;
                Ok(Vec::new())
            })
            .unwrap();
    }

    fn wall(&self, label: &str) {
        let now = Self::NOW;
        self.state(|state| {
            crate::harness::accounts::note_wall(
                state,
                label,
                "five_hour",
                Some(now + 3600),
                now - 60,
                now,
            );
        });
    }

    /// The roster of `main` alone.
    fn main_alone(&self) {
        use crate::harness::accounts;
        let row = accounts::Row {
            label: "main".into(),
            email: Some("owner@example.com".into()),
            vendor: accounts::Vendor {
                region: (self.cli == accounts::Cli::Kimi)
                    .then_some(crate::ai::connector::Region::Global),
                ..accounts::Vendor::default()
            },
            ..accounts::Row::default()
        };
        let text = accounts::append_rows_for(self.cli, "", &[row]).expect("the row admits");
        std::fs::write(self.config.join(self.cli.accounts().roster_file), text).expect("roster");
    }

    /// `main` signed out here.
    fn main_signed_out(&self) {
        self.auth("main", crate::harness::accounts::Auth::SignedOut);
    }

    /// `work` with no login here, as `accounts add` leaves it.
    fn work_signed_out(&self) {
        let work = self.work();
        std::fs::remove_dir_all(&work).expect("work's login");
        std::fs::create_dir_all(&work).expect("an empty home");
    }

    /// The Codex a launch aterm makes runs on this machine (`worker::codex_launch_program`, from
    /// its atpkg layout); `None` for another CLI.
    fn program(&self) -> Option<PathBuf> {
        (self.cli == crate::harness::accounts::Cli::Codex).then(|| {
            crate::harness::worker::codex_launch_program_in(Some(&self.b.pkg.agents_dir()))
        })
    }

    /// What a launch of the pool's CLI takes (`accounts_pool::launch_env_for`), reading no usage.
    fn launch(&self) -> Result<Option<crate::harness::accounts_pool::LoginEnv>, String> {
        let seat = crate::harness::accounts_pool::Seat {
            config_dir: &self.config,
            state: &self.b.state,
            home: &self.b.home,
            now: Self::NOW,
        };
        let unread = |_: crate::harness::accounts::Cli, _: &Path, _: Option<&str>, _: i64| None;
        crate::harness::accounts_pool::launch_env_for(
            &seat,
            self.cli,
            &unread,
            self.program().as_deref(),
        )
    }

    /// The facts `aterm ai review --with <cli>` reads of the pool's CLI.
    fn facts(&self) -> Facts {
        use crate::ai::facts::{Machine, gather};
        let home = self.b.home.display().to_string();
        let var = |k: &str| (k == "HOME").then(|| home.clone());
        let m = Machine {
            now: Self::NOW,
            home: Some(self.b.home.clone()),
            var: &var,
            pkg: Some(self.b.pkg.clone()),
            exclude: Vec::new(),
            records: Store::at(&self.b.state).load().unwrap_or_default(),
            harness_state: Some(self.b.state.clone()),
            roster: Some(self.config.join("accounts.toml")),
            accounts_mode: None,
            config: None,
        };
        gather(&m, self.cli.connector())
    }

    /// What `aterm ai review --with <cli>` answers, and the environment of each run it made.
    fn review(&self) -> (Answer, Vec<Vec<(String, String)>>) {
        let runner = FakeRunner::with(|call| {
            if let Some(out) = call
                .args
                .windows(2)
                .find_map(|w| (w[0] == "-o").then_some(&w[1]))
            {
                std::fs::write(out, "no findings").expect("write answer");
            }
            Ok(ran(0, "no findings", ""))
        });
        self.review_using(&runner)
    }

    fn review_using(&self, runner: &FakeRunner) -> (Answer, Vec<Vec<(String, String)>>) {
        self.review_using_clock(runner, &|| Self::NOW)
    }

    fn review_using_clock(
        &self,
        runner: &FakeRunner,
        now: &(dyn Fn() -> i64 + Sync),
    ) -> (Answer, Vec<Vec<(String, String)>>) {
        let c = self.cli.connector();
        let home = self.b.home.display().to_string();
        let vars = vec![("HOME".to_string(), home.clone())];
        let var = |k: &str| (k == "HOME").then(|| home.clone());
        let store = Store::at(&self.b.state);
        let facts = vec![self.facts()];
        let rows = [crate::ai::state::assess(&facts[0], true, Self::NOW)];
        let reviewers: Vec<&'static Connector> = vec![c];
        let cands = candidates(&reviewers, Some(&[c.id]), "claude", &rows, Self::NOW);
        let host = Host {
            config: None,
            runner,
            var: &var,
            vars: &vars,
            store: &store,
            now,
        };
        let mut answers = run_review(
            &host,
            &cands,
            &facts,
            &Ask {
                change: &change(),
                asker: "claude",
                words: None,
                count: 1,
                budget: Duration::from_secs(5),
                level: Level::Yolo,
            },
        );
        assert_eq!(answers.len(), 1, "{answers:?}");
        let envs = runner.calls().into_iter().map(|call| call.env).collect();
        (answers.remove(0), envs)
    }
}

#[test]
fn a_review_result_cannot_heal_or_limit_a_replacement_login() {
    use crate::harness::accounts::{self, Auth, Cli};
    for outcome in 0..3 {
        for (label, change) in [
            ("main", 0),
            ("main", 1),
            ("main", 2),
            ("work", 0),
            ("work", 1),
            ("work", 2),
        ] {
            let pool = ReviewPool::new(Cli::Codex, Some(true));
            pool.pin(label);
            if label == "main" {
                let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
                    Some("owner@example.com"),
                    "plus",
                    ReviewPool::NOW + 3600,
                );
                std::fs::write(
                    pool.b.home.join(".codex/auth.json"),
                    aterm_json::json!({"tokens":{"access_token":token}}).to_string(),
                )
                .unwrap();
            }
            let account_store = pool.store();
            let raw = account_store
                .read_account_records(
                    Some(&pool.config.join(Cli::Codex.accounts().roster_file)),
                    ReviewPool::NOW,
                )
                .unwrap()
                .raw_roster
                .row(label)
                .unwrap()
                .clone();
            let replacement = pool.b.home.join(".aterm/logins/codex/replacement");
            let runner = FakeRunner::with(move |call| {
                if change == 1 {
                    codex_fixture_login(
                        &replacement,
                        raw.expected_identity_email().unwrap(),
                        ReviewPool::NOW,
                    );
                }
                account_store
                    .transact_logins(ReviewPool::NOW, &mut None, |state, logins| {
                        let mut events = Vec::new();
                        if change == 1 {
                            events.extend(accounts::note_renewed(
                                state,
                                logins,
                                label,
                                None,
                                replacement.to_str().unwrap(),
                                None,
                                ReviewPool::NOW - 1,
                            ));
                        }
                        let effective = accounts::bound(
                            &accounts::Roster {
                                rows: vec![raw.clone()],
                            },
                            logins,
                        );
                        let context = account_store
                            .capture_auth_context(&effective.rows[0])
                            .unwrap();
                        if change == 2 {
                            accounts::stage_auth(
                                state,
                                &raw,
                                logins,
                                context.clone(),
                                Auth::SignedOut,
                                ReviewPool::NOW - 1,
                            );
                            accounts::stage_signed_back(
                                state,
                                &raw,
                                logins,
                                context,
                                ReviewPool::NOW - 1,
                            );
                        } else if outcome == 0 {
                            accounts::stage_auth(
                                state,
                                &raw,
                                logins,
                                context,
                                Auth::SignedOut,
                                ReviewPool::NOW - 1,
                            );
                        }
                        Ok(events)
                    })
                    .unwrap();
                match outcome {
                    0 => {
                        let out = call
                            .args
                            .windows(2)
                            .find_map(|w| (w[0] == "-o").then_some(&w[1]))
                            .unwrap();
                        std::fs::write(out, "no findings").unwrap();
                        Ok(ran(0, "", ""))
                    }
                    1 => Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized")),
                    2 => Ok(ran(1, "", "ERROR: You've hit your usage limit")),
                    _ => unreachable!(),
                }
            });
            let (answer, calls) = pool.review_using(&runner);
            assert_eq!(calls.len(), 1, "{label}/{outcome}/{change}: {answer:?}");
            assert_eq!(
                answer.verdict,
                [Verdict::Served, Verdict::Auth, Verdict::Limit][outcome]
            );
            let state = pool.store().load(ReviewPool::NOW).unwrap().0;
            let logins = pool.store().load_logins(ReviewPool::NOW).unwrap().0;
            let account = state.accounts.get(label).unwrap();
            let record = Store::at(&pool.b.state)
                .load()
                .unwrap()
                .of_scope("codex", Some("file"));
            assert_eq!(record.ended_at, None);
            assert_eq!(record.limited_until, None);
            if outcome == 0 {
                assert_eq!(
                    account.auth,
                    if change == 1 {
                        Auth::SignedOut
                    } else {
                        Auth::Ok
                    }
                );
                assert_eq!(logins.sign_in_receipts.contains_key(label), change != 1);
                assert_eq!(record.used_at, None);
            } else if outcome == 1 {
                assert_eq!(
                    account.auth,
                    if change != 0 {
                        Auth::Ok
                    } else {
                        Auth::SignedOut
                    }
                );
            } else {
                assert_eq!(account.walls.contains_key("five_hour"), change == 0);
            }
            if change == 2 {
                assert_eq!(
                    logins.sign_in_clock, 1,
                    "the prior sign-in receipt is preserved"
                );
            }
        }
    }
}

#[test]
fn an_in_flight_review_cannot_restore_a_cleared_quota_wall() {
    use crate::harness::accounts::{self, Cli};
    use std::sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    };
    for clear_at in [None, Some(ReviewPool::NOW), Some(ReviewPool::NOW + 1)] {
        let pool = ReviewPool::new(Cli::Codex, Some(true));
        pool.pin("main");
        let store = pool.store();
        let roster = pool.config.join(Cli::Codex.accounts().roster_file);
        let clock = Arc::new(AtomicI64::new(ReviewPool::NOW));
        let written_clock = Arc::clone(&clock);
        let runner = FakeRunner::with(move |_| {
            if let Some(cleared_at) = clear_at {
                store
                    .transact(cleared_at, &mut None, |state| {
                        Ok(vec![accounts::pin(
                            state,
                            &accounts::Roster::load(&roster)?,
                            &["main".to_owned()].into_iter().collect(),
                            "main",
                            cleared_at,
                        )?])
                    })
                    .unwrap();
            }
            written_clock.store(ReviewPool::NOW + 2, Ordering::SeqCst);
            Ok(ran(1, "", "ERROR: You've hit your usage limit"))
        });
        let (answer, calls) = pool.review_using_clock(&runner, &|| clock.load(Ordering::SeqCst));
        assert_eq!(answer.verdict, Verdict::Limit);
        assert_eq!(calls.len(), 1);
        let state = pool.store().load(ReviewPool::NOW + 2).unwrap().0;
        assert_eq!(
            state.accounts["main"].walls.contains_key("five_hour"),
            clear_at.is_none()
        );
        let record = Store::at(&pool.b.state)
            .load()
            .unwrap()
            .of_scope("codex", Some("file"));
        assert_eq!(record.limited_until, None);
    }
}

#[test]
fn an_isolated_review_cannot_heal_or_extend_default_login_health() {
    use crate::ai::store::{Ended, How, Outcome};
    let pool = ReviewPool::new(crate::harness::accounts::Cli::Codex, Some(true));
    pool.pin("work");
    let store = Store::at(&pool.b.state);
    store
        .note_scope(
            "codex",
            Some("file"),
            How::Review,
            Outcome::Limited(Some(ReviewPool::NOW + 3600)),
            ReviewPool::NOW - 2,
        )
        .unwrap();
    store
        .note_scope(
            "codex",
            Some("file"),
            How::Review,
            Outcome::Ended(Ended::ReviewAuth),
            ReviewPool::NOW - 1,
        )
        .unwrap();
    let before = store.load().unwrap().of_scope("codex", Some("file"));
    assert!(before.ended());
    let (answer, calls) = pool.review();
    assert_eq!(answer.verdict, Verdict::Served, "{answer:?}");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        store.load().unwrap().of_scope("codex", Some("file")),
        before
    );
    assert_eq!(
        pool.store()
            .load_logins(ReviewPool::NOW)
            .unwrap()
            .0
            .get("work")
            .unwrap()
            .served_at,
        Some(ReviewPool::NOW)
    );
}

#[test]
fn unlisted_claude_reviews_leave_no_retained_implicit_account() {
    for empty_file in [false, true] {
        for outcome in 0..3 {
            let pool = ReviewPool::with_scope(crate::harness::accounts::Cli::Claude, None, false);
            if empty_file {
                std::fs::write(pool.config.join("accounts.toml"), "").unwrap();
            }
            let runner = FakeRunner::with(move |_| match outcome {
                0 => Ok(ran(0, "no findings", "")),
                1 => Ok(ran(
                    1,
                    "",
                    "Failed to authenticate. API Error: 401 OAuth session expired",
                )),
                _ => Ok(ran(1, "", "You've hit your limit")),
            });
            let (answer, calls) = pool.review_using(&runner);
            assert_eq!(calls.len(), 1, "{answer:?}");
            assert_eq!(
                answer.verdict,
                [Verdict::Served, Verdict::Auth, Verdict::Limit][outcome]
            );
            assert!(!pool.store().state_file().exists());
            assert!(!pool.store().logins_file().exists());
            let record = Store::at(&pool.b.state).load().unwrap().of("claude");
            assert_eq!(record.ended_at.is_some(), outcome == 1);
            assert_eq!(record.limited_until.is_some(), outcome == 2);
            assert_eq!(record.used_at.is_some(), outcome != 1);
        }
    }
}

#[test]
fn unlisted_claude_uses_default_health_without_rebinding_retained_history() {
    use crate::harness::accounts::{self, Auth, Cli};
    for isolated in [false, true] {
        let pool = ReviewPool::with_scope(Cli::Claude, None, false);
        let account_store = pool.store();
        let old_login = pool.work();
        account_store
            .transact_logins(ReviewPool::NOW, &mut None, |state, logins| {
                let mut events = if isolated {
                    accounts::note_renewed(
                        state,
                        logins,
                        "this-mac",
                        None,
                        old_login.to_str().unwrap(),
                        None,
                        ReviewPool::NOW - 20,
                    )
                } else {
                    accounts::note_served_login(logins, "this-mac", ReviewPool::NOW - 20);
                    Vec::new()
                };
                events.extend(accounts::note_auth_in_context(
                    state,
                    logins,
                    "this-mac",
                    Auth::SignedOut,
                    ReviewPool::NOW - 10,
                ));
                events.extend(accounts::note_wall(
                    state,
                    "this-mac",
                    "five_hour",
                    Some(ReviewPool::NOW + 600),
                    ReviewPool::NOW - 10,
                    ReviewPool::NOW - 10,
                ));
                Ok(events)
            })
            .unwrap();
        let images = || {
            [account_store.state_file(), account_store.logins_file()]
                .map(|path| std::fs::read(path).unwrap())
        };
        let before = images();
        let fact = pool.facts();
        let account = fact.accounts.as_ref().unwrap();
        assert!(!account.listed);
        assert_eq!(account.login, None);
        assert_eq!(account.state, None);
        assert_eq!(account.served_at, None);
        assert!(fact.default_record().is_some());
        assert!(
            crate::ai::state::assess(&fact, true, ReviewPool::NOW)
                .state
                .usable()
        );
        let runner = FakeRunner::with(|_| {
            Ok(ran(
                1,
                "",
                "Failed to authenticate. API Error: 401 OAuth session expired",
            ))
        });
        let (answer, calls) = pool.review_using(&runner);
        assert_eq!(answer.verdict, Verdict::Auth);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            crate::ai::state::assess(&pool.facts(), true, ReviewPool::NOW).state,
            State::SignedOut
        );
        assert_eq!(images(), before);
    }
}

#[test]
fn an_unlisted_claude_review_cannot_report_over_newly_adopted_accounts() {
    use crate::harness::accounts;
    for outcome in 0..3 {
        let pool = ReviewPool::with_scope(accounts::Cli::Claude, None, false);
        let path = pool.config.join("accounts.toml");
        let roster = accounts::append_rows_for(
            accounts::Cli::Claude,
            "",
            &[accounts::Row {
                label: "main".into(),
                email: Some("owner@example.com".into()),
                ..accounts::Row::default()
            }],
        )
        .unwrap();
        let account_store = pool.store();
        let runner = FakeRunner::with(move |_| {
            std::fs::write(&path, &roster).unwrap();
            account_store
                .transact(ReviewPool::NOW, &mut None, |state| {
                    Ok(accounts::note_auth(
                        state,
                        "main",
                        accounts::Auth::Ok,
                        ReviewPool::NOW,
                    ))
                })
                .unwrap();
            match outcome {
                0 => Ok(ran(0, "no findings", "")),
                1 => Ok(ran(
                    1,
                    "",
                    "Failed to authenticate. API Error: 401 OAuth session expired",
                )),
                _ => Ok(ran(1, "", "You've hit your limit")),
            }
        });
        let (_, calls) = pool.review_using(&runner);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            pool.store().load(ReviewPool::NOW).unwrap().0.accounts["main"].auth,
            accounts::Auth::Ok
        );
        assert_eq!(
            Store::at(&pool.b.state).load().unwrap().of("claude"),
            Record::default()
        );
    }
}

#[test]
fn claude_review_results_follow_the_observed_account_and_receipt() {
    use crate::harness::accounts::{self, Auth, Cli};
    for label in ["main", "work"] {
        for outcome in 0..3 {
            for change in 0..3 {
                let pool = ReviewPool::new(Cli::Claude, None);
                pool.pin(label);
                let store = pool.store();
                store
                    .transact_logins(ReviewPool::NOW, &mut None, |state, logins| {
                        accounts::note_served_login(logins, label, ReviewPool::NOW - 20);
                        Ok(accounts::note_auth(
                            state,
                            label,
                            Auth::Ok,
                            ReviewPool::NOW - 20,
                        ))
                    })
                    .unwrap();
                let observed_store = store.clone();
                let replacement = pool.b.home.join("replacement");
                let runner = FakeRunner::with(move |_| {
                    observed_store
                        .transact_logins(ReviewPool::NOW, &mut None, |state, logins| {
                            let mut events = Vec::new();
                            if change == 1 {
                                std::fs::create_dir_all(&replacement).unwrap();
                                std::fs::write(replacement.join(".credentials.json"), "{}")
                                    .unwrap();
                                events.extend(accounts::note_renewed(
                                    state,
                                    logins,
                                    label,
                                    None,
                                    replacement.to_str().unwrap(),
                                    None,
                                    ReviewPool::NOW - 1,
                                ));
                            }
                            if change == 2 {
                                events.extend(accounts::note_auth_in_context(
                                    state,
                                    logins,
                                    label,
                                    Auth::SignedOut,
                                    ReviewPool::NOW - 1,
                                ));
                                events.extend(accounts::note_signed_back(
                                    state,
                                    logins,
                                    label,
                                    ReviewPool::NOW - 1,
                                ));
                            } else if outcome == 0 {
                                events.extend(accounts::note_auth_in_context(
                                    state,
                                    logins,
                                    label,
                                    Auth::SignedOut,
                                    ReviewPool::NOW - 1,
                                ));
                            }
                            Ok(events)
                        })
                        .unwrap();
                    match outcome {
                        0 => Ok(ran(0, "no findings", "")),
                        1 => Ok(ran(
                            1,
                            "",
                            "Failed to authenticate. API Error: 401 OAuth session expired",
                        )),
                        _ => Ok(ran(1, "", "You've hit your limit")),
                    }
                });
                let (answer, calls) = pool.review_using(&runner);
                assert_eq!(calls.len(), 1, "{label}/{outcome}/{change}: {answer:?}");
                assert_eq!(
                    answer.verdict,
                    [Verdict::Served, Verdict::Auth, Verdict::Limit][outcome]
                );
                let state = store.load(ReviewPool::NOW).unwrap().0;
                let logins = store.load_logins(ReviewPool::NOW).unwrap().0;
                let account = &state.accounts[label];
                let expected_auth = match (outcome, change) {
                    (0, 1) | (1, 0) => Auth::SignedOut,
                    _ => Auth::Ok,
                };
                assert_eq!(account.auth, expected_auth, "{label}/{outcome}/{change}");
                assert_eq!(
                    account.walls.contains_key("five_hour"),
                    outcome == 2 && change == 0
                );
                assert_eq!(
                    logins.sign_in_receipts.contains_key(label),
                    change == 2 || (outcome == 0 && change == 0)
                );
                assert_eq!(
                    Store::at(&pool.b.state).load().unwrap().of("claude"),
                    Record::default()
                );
            }
        }
    }
}

#[test]
fn a_claude_review_cannot_heal_a_different_default_identity() {
    use crate::harness::accounts::{self, Auth, Cli};
    let pool = ReviewPool::new(Cli::Claude, None);
    pool.pin("main");
    let account_store = pool.store();
    let profile = pool.b.home.join(".claude.json");
    let runner = FakeRunner::with(move |_| {
        account_store
            .transact(ReviewPool::NOW, &mut None, |state| {
                Ok(accounts::note_auth(
                    state,
                    "main",
                    Auth::SignedOut,
                    ReviewPool::NOW - 1,
                ))
            })
            .unwrap();
        std::fs::write(
            &profile,
            r#"{"oauthAccount":{"emailAddress":"different@example.com"}}"#,
        )
        .unwrap();
        Ok(ran(0, "no findings", ""))
    });
    let (answer, calls) = pool.review_using(&runner);
    assert_eq!(calls.len(), 1);
    assert_eq!(answer.verdict, Verdict::Served);
    assert_eq!(
        pool.store().load(ReviewPool::NOW).unwrap().0.accounts["main"].auth,
        Auth::SignedOut
    );
    assert_eq!(
        Store::at(&pool.b.state).load().unwrap().of("claude"),
        Record::default()
    );
}

#[test]
fn a_review_that_heals_an_unbound_login_retains_its_served_time() {
    use crate::harness::accounts::{self, Auth, Cli};
    for (label, plain) in [
        ("main", false),
        ("work", false),
        ("main", true),
        ("work", true),
    ] {
        let pool = ReviewPool::new(Cli::Claude, None);
        if plain {
            let path = pool.config.join("accounts.toml");
            let mut roster = accounts::Roster::load(&path).unwrap();
            for row in &mut roster.rows {
                row.email = None;
            }
            std::fs::write(
                path,
                accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
            )
            .unwrap();
        }
        pool.pin(label);
        assert!(
            pool.store()
                .load_logins(ReviewPool::NOW)
                .unwrap()
                .0
                .get(label)
                .is_none()
        );
        let account_store = pool.store();
        let runner = FakeRunner::with(move |_| {
            account_store
                .transact(ReviewPool::NOW, &mut None, |state| {
                    Ok(accounts::note_auth(
                        state,
                        label,
                        Auth::SignedOut,
                        ReviewPool::NOW - 1,
                    ))
                })
                .unwrap();
            Ok(ran(0, "no findings", ""))
        });
        let (answer, calls) = pool.review_using(&runner);
        assert_eq!(calls.len(), 1, "{answer:?}");
        assert_eq!(answer.verdict, Verdict::Served);
        let records = pool
            .store()
            .read_account_records(Some(&pool.config.join("accounts.toml")), ReviewPool::NOW)
            .unwrap();
        assert_eq!(records.state.accounts[label].auth, Auth::Ok);
        assert_eq!(
            records.logins.get(label).unwrap().served_at,
            Some(ReviewPool::NOW)
        );
        assert!(records.logins.sign_in_receipts.contains_key(label));
        assert_eq!(
            crate::ai::state::assess(&pool.facts(), true, ReviewPool::NOW).state,
            State::Ok
        );
        assert_eq!(
            Store::at(&pool.b.state).load().unwrap().of("claude"),
            Record::default()
        );
    }
}

#[test]
fn a_later_review_wave_uses_current_selection_and_refuses_unusable_accounts() {
    use crate::harness::accounts::{self, Auth, Cli};
    for refuse in [false, true] {
        let codex = ReviewPool::new(Cli::Codex, Some(true));
        let claude = ReviewPool::new(Cli::Claude, None);
        claude.pin("main");
        let facts = vec![codex.facts(), claude.facts()];
        let rows: Vec<_> = facts
            .iter()
            .map(|f| crate::ai::state::assess(f, true, ReviewPool::NOW))
            .collect();
        let cands = candidates(
            &[&CODEX, &CLAUDE],
            Some(&["codex", "claude"]),
            "muse",
            &rows,
            ReviewPool::NOW,
        );
        let account_store = claude.store();
        let runner = FakeRunner::with(move |call| {
            if call.program == "codex" {
                if refuse {
                    account_store
                        .transact(ReviewPool::NOW, &mut None, |state| {
                            let mut events = accounts::note_auth(
                                state,
                                "main",
                                Auth::SignedOut,
                                ReviewPool::NOW,
                            );
                            events.extend(accounts::note_auth(
                                state,
                                "work",
                                Auth::SignedOut,
                                ReviewPool::NOW,
                            ));
                            Ok(events)
                        })
                        .unwrap();
                } else {
                    account_store
                        .transact(ReviewPool::NOW, &mut None, |state| {
                            state.active = Some("work".into());
                            state.pinned = true;
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                Ok(ran(1, "", "ERROR: You've hit your usage limit"))
            } else {
                assert!(!refuse, "a refused wave must never launch");
                Ok(ran(0, "no findings", ""))
            }
        });
        let home = claude.b.home.display().to_string();
        let vars = vec![("HOME".into(), home.clone())];
        let var = |key: &str| (key == "HOME").then(|| home.clone());
        let store = Store::at(&claude.b.state);
        let host = Host {
            config: None,
            runner: &runner,
            var: &var,
            vars: &vars,
            store: &store,
            now: &|| ReviewPool::NOW,
        };
        let answers = run_review(
            &host,
            &cands,
            &facts,
            &Ask {
                change: &change(),
                asker: "muse",
                words: None,
                count: 1,
                budget: Duration::from_secs(5),
                level: Level::Yolo,
            },
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(answers.len(), 2, "{answers:?}");
        assert_eq!(calls.len(), if refuse { 1 } else { 2 }, "{answers:?}");
        assert_eq!(
            answers[1].verdict,
            if refuse {
                Verdict::Failed
            } else {
                Verdict::Served
            }
        );
        if !refuse {
            assert!(calls[1].env.contains(&(
                "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
                claude.work().display().to_string()
            )));
            let logins = claude.store().load_logins(ReviewPool::NOW).unwrap().0;
            assert_eq!(logins.get("work").unwrap().served_at, Some(ReviewPool::NOW));
            assert!(
                logins
                    .get("main")
                    .is_none_or(|binding| binding.served_at.is_none())
            );
        }
    }
}

#[test]
fn a_roster_created_between_review_waves_controls_the_later_login() {
    use crate::harness::accounts::Cli;
    let first = ReviewPool::new(Cli::Claude, None);
    let later = std::sync::Arc::new(ReviewPool::with_scope(Cli::Codex, Some(true), false));
    let facts = vec![first.facts(), later.facts()];
    assert!(facts[1].pool.is_none(), "an absent roster is not a UI pool");
    let rows: Vec<_> = facts
        .iter()
        .map(|fact| crate::ai::state::assess(fact, true, ReviewPool::NOW))
        .collect();
    let cands = candidates(
        &[&CLAUDE, &CODEX],
        Some(&["claude", "codex"]),
        "muse",
        &rows,
        ReviewPool::NOW,
    );
    let changed = std::sync::Arc::clone(&later);
    let runner = FakeRunner::with(move |call| {
        if call.program == "claude" {
            changed.roster(None);
            changed.pin("work");
            Ok(ran(1, "", "You've hit your limit"))
        } else {
            assert_eq!(call.program, "codex");
            if let Some(out) = call
                .args
                .windows(2)
                .find_map(|words| (words[0] == "-o").then_some(&words[1]))
            {
                std::fs::write(out, "no findings").unwrap();
            }
            Ok(ran(0, "no findings", ""))
        }
    });
    let home = later.b.home.display().to_string();
    let vars = vec![("HOME".into(), home.clone())];
    let var = |key: &str| (key == "HOME").then(|| home.clone());
    let store = Store::at(&later.b.state);
    let host = Host {
        config: None,
        runner: &runner,
        var: &var,
        vars: &vars,
        store: &store,
        now: &|| ReviewPool::NOW,
    };
    let answers = run_review(
        &host,
        &cands,
        &facts,
        &Ask {
            change: &change(),
            asker: "muse",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 2, "{answers:?}");
    assert_eq!(answers[1].verdict, Verdict::Served, "{answers:?}");
    assert!(calls[1].env.contains(&(
        "CODEX_AUTH_PATH".into(),
        later.work().join("auth.json").display().to_string()
    )));
    let logins = later.store().load_logins(ReviewPool::NOW).unwrap().0;
    assert_eq!(logins.get("work").unwrap().served_at, Some(ReviewPool::NOW));
    assert!(logins.get("main").is_none());
    assert_eq!(
        store.load().unwrap().of_scope("codex", Some("file")),
        Record::default()
    );
}

#[test]
fn a_roster_created_during_an_unmanaged_review_rejects_its_old_health_result() {
    use crate::harness::accounts::{Auth, Cli};
    let pool = std::sync::Arc::new(ReviewPool::with_scope(Cli::Codex, Some(true), false));
    let changed = std::sync::Arc::clone(&pool);
    let runner = FakeRunner::with(move |_| {
        changed.roster(None);
        changed.pin("work");
        Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized"))
    });
    let (answer, calls) = pool.review_using(&runner);
    assert_eq!(answer.verdict, Verdict::Auth);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        Store::at(&pool.b.state)
            .load()
            .unwrap()
            .of_scope("codex", Some("file")),
        Record::default(),
        "the old unmanaged result cannot mark the new account scope signed out"
    );
    assert!(
        pool.store()
            .load(ReviewPool::NOW)
            .unwrap()
            .0
            .accounts
            .get("work")
            .is_none_or(|account| account.auth != Auth::SignedOut)
    );
}

fn prepare_from(pool: &ReviewPool, original: &Facts) -> PreparedReview {
    let home = pool.b.home.display().to_string();
    let var = |key: &str| (key == "HOME").then(|| home.clone());
    let runner = FakeRunner::quiet();
    let store = Store::at(&pool.b.state);
    prepare_review(
        &Host {
            config: None,
            runner: &runner,
            var: &var,
            vars: &[],
            store: &store,
            now: &|| ReviewPool::NOW,
        },
        original,
    )
}

fn codex_storage(pool: &ReviewPool, mode: &str) {
    std::fs::write(
        pool.b.home.join(".codex/config.toml"),
        format!("cli_auth_credentials_store = {mode:?}\n"),
    )
    .unwrap();
}

#[test]
fn completed_codex_health_does_not_cross_credential_backends() {
    use crate::harness::accounts::Cli;
    for limited in [false, true] {
        for observed in ["file", "keyring", "auto", "ephemeral"] {
            for current in ["file", "keyring", "auto", "ephemeral"] {
                let pool = ReviewPool::with_scope(Cli::Codex, Some(true), false);
                codex_storage(&pool, observed);
                let runner = FakeRunner::with(move |_| {
                    Ok(if limited {
                        ran(1, "", "ERROR: You've hit your usage limit")
                    } else {
                        ran(1, "", "ERROR: unexpected status 401 Unauthorized")
                    })
                });
                let (answer, calls) = pool.review_using(&runner);
                assert_eq!(calls.len(), 1);
                assert_eq!(
                    answer.verdict,
                    if limited {
                        Verdict::Limit
                    } else {
                        Verdict::Auth
                    }
                );
                codex_storage(&pool, current);
                let row = crate::ai::state::assess(&pool.facts(), true, ReviewPool::NOW);
                assert_eq!(
                    row.state.usable(),
                    observed != current,
                    "limited={limited}, {observed} -> {current}: {row:?}"
                );
                // Switching back must recover the original observation, not erase it.
                codex_storage(&pool, observed);
                let original = crate::ai::state::assess(&pool.facts(), true, ReviewPool::NOW);
                assert!(!original.state.usable());
            }
        }
    }
}

#[test]
fn legacy_health_is_retained_without_guessing_a_codex_backend() {
    use crate::harness::accounts::Cli;
    let pool = ReviewPool::with_scope(Cli::Codex, Some(true), false);
    let store = Store::at(&pool.b.state);
    store
        .note(
            "codex",
            How::Review,
            Outcome::Ended(Ended::ReviewAuth),
            ReviewPool::NOW,
        )
        .unwrap();
    let legacy = store.load().unwrap().of("codex");
    for backend in ["file", "keyring", "auto", "ephemeral"] {
        codex_storage(&pool, backend);
        let facts = pool.facts();
        assert_eq!(facts.record, Record::default());
        assert_eq!(facts.record_scope, Some(backend));
        assert!(
            crate::ai::state::assess(&facts, true, ReviewPool::NOW)
                .state
                .usable()
        );
        assert_eq!(store.load().unwrap().of("codex"), legacy);
    }
}

#[test]
fn an_unreadable_preparation_cannot_publish_into_a_restored_health_store() {
    use crate::harness::accounts::Cli;
    for raw in ["not JSON", r#"{"schema":3,"clis":{}}"#] {
        let pool = ReviewPool::with_scope(Cli::Codex, Some(true), false);
        codex_storage(&pool, "keyring");
        let store = Store::at(&pool.b.state);
        store.ensure().unwrap();
        let path = store.dir().join(crate::ai::store::RECORD_FILE);
        std::fs::write(&path, raw).unwrap();
        let prepared = prepare_from(&pool, &pool.facts());
        assert!(!prepared.health_readable);
        let runner = FakeRunner::with(move |_| {
            std::fs::write(&path, r#"{"schema":2,"clis":{},"scopes":{}}"#).unwrap();
            Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized"))
        });
        let (answer, calls) = pool.review_using(&runner);
        assert_eq!(answer.verdict, Verdict::Auth);
        assert_eq!(calls.len(), 1);
        assert_eq!(store.load().unwrap(), crate::ai::store::Records::default());
    }
}

#[test]
fn review_preparation_refreshes_codex_storage_mode() {
    use crate::harness::accounts::Cli;
    for initial in ["file", "keyring", "auto", "ephemeral"] {
        for current in ["file", "keyring", "auto", "ephemeral"] {
            let pool = ReviewPool::new(Cli::Codex, Some(true));
            pool.pin("work");
            codex_storage(&pool, initial);
            let original = pool.facts();
            codex_storage(&pool, current);
            let prepared = prepare_from(&pool, &original);
            assert!(prepared.refused.is_none(), "{initial} -> {current}");
            assert_eq!(prepared.fact.hidden_store, current != "file");
            let isolated = prepared.env.iter().any(|(key, value)| {
                key == "CODEX_AUTH_PATH"
                    && value == &pool.work().join("auth.json").display().to_string()
            });
            assert_eq!(isolated, current == "file", "{initial} -> {current}");
            assert_eq!(prepared.fact.default_record().is_some(), current != "file");
            assert_eq!(prepared.observation.is_some(), current == "file");
        }
    }
}

#[test]
fn review_outcomes_do_not_cross_codex_storage_changes() {
    use crate::harness::accounts::Cli;
    for initial in ["file", "keyring", "auto", "ephemeral"] {
        for current in ["file", "keyring", "auto", "ephemeral"] {
            if initial == current {
                continue;
            }
            let pool = std::sync::Arc::new(ReviewPool::new(Cli::Codex, Some(true)));
            pool.pin("work");
            codex_storage(&pool, initial);
            let account_store = pool.store();
            let before = std::fs::read(account_store.logins_file()).ok();
            let changed = std::sync::Arc::clone(&pool);
            let runner = FakeRunner::with(move |_| {
                codex_storage(&changed, current);
                Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized"))
            });
            let (answer, calls) = pool.review_using(&runner);
            assert_eq!(calls.len(), 1, "{initial} -> {current}: {answer:?}");
            assert_eq!(answer.verdict, Verdict::Auth);
            assert_eq!(
                std::fs::read(account_store.logins_file()).ok(),
                before,
                "{initial} -> {current}"
            );
            assert_eq!(
                Store::at(&pool.b.state)
                    .load()
                    .unwrap()
                    .of_scope("codex", Some("file")),
                Record::default(),
                "{initial} -> {current}"
            );
        }
    }
}

#[test]
fn a_codex_storage_change_after_preparation_refuses_the_child() {
    use crate::harness::accounts::Cli;
    for initial in ["file", "keyring"] {
        let pool = ReviewPool::new(Cli::Codex, Some(true));
        pool.pin("work");
        codex_storage(&pool, initial);
        let prepared = prepare_from(&pool, &pool.facts());
        codex_storage(&pool, if initial == "file" { "keyring" } else { "file" });
        let runner = FakeRunner::quiet();
        let store = Store::at(&pool.b.state);
        let (answer, outcome) = run_prepared(
            &Host {
                config: None,
                runner: &runner,
                var: &|_| None,
                vars: &[],
                store: &store,
                now: &|| ReviewPool::NOW,
            },
            &prepared,
            &Ask {
                change: &change(),
                asker: "claude",
                words: None,
                count: 1,
                budget: Duration::from_secs(5),
                level: Level::Yolo,
            },
        );
        assert_eq!(answer.verdict, Verdict::Failed);
        assert!(outcome.is_none());
        assert!(runner.calls().is_empty(), "{initial}: {answer:?}");
    }
}

fn review_storage_case(initial: i64, selected: i64, current: i64) -> [bool; 5] {
    use crate::harness::accounts::{Auth, Cli};
    let modes = ["file", "keyring", "auto", "ephemeral"];
    let mode = |index: i64| modes[usize::try_from(index).unwrap()];
    let pool = std::sync::Arc::new(ReviewPool::new(Cli::Codex, Some(true)));
    pool.pin("work");
    codex_storage(&pool, mode(initial));
    let original = pool.facts();
    codex_storage(&pool, mode(selected));
    let prepared = prepare_from(&pool, &original);
    assert!(prepared.refused.is_none());
    let hidden = prepared.fact.hidden_store;
    let file_evidence = prepared.fact.login_file.is_some();
    let isolated = prepared.env.iter().any(|(key, value)| {
        key == "CODEX_AUTH_PATH" && value == &pool.work().join("auth.json").display().to_string()
    });
    codex_storage(&pool, mode(current));
    let runner = FakeRunner::quiet();
    let store = Store::at(&pool.b.state);
    let _ = run_prepared(
        &Host {
            config: None,
            runner: &runner,
            var: &|_| None,
            vars: &[],
            store: &store,
            now: &|| ReviewPool::NOW,
        },
        &prepared,
        &Ask {
            change: &change(),
            asker: "claude",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    let launches = !runner.calls().is_empty();

    // Exercise completion separately: the change happens inside the child,
    // after it has demonstrably passed the launch fence.
    codex_storage(&pool, mode(selected));
    let changed = std::sync::Arc::clone(&pool);
    let mode_after = mode(current);
    let runner = FakeRunner::with(move |_| {
        codex_storage(&changed, mode_after);
        Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized"))
    });
    let rows = [crate::ai::state::assess(&original, true, ReviewPool::NOW)];
    let cands = candidates(
        &[&CODEX],
        Some(&["codex"]),
        "claude",
        &rows,
        ReviewPool::NOW,
    );
    let home = pool.b.home.display().to_string();
    let var = |key: &str| (key == "HOME").then(|| home.clone());
    let answers = run_review(
        &Host {
            config: None,
            runner: &runner,
            var: &var,
            vars: &[("HOME".into(), home.clone())],
            store: &store,
            now: &|| ReviewPool::NOW,
        },
        &cands,
        &[original],
        &Ask {
            change: &change(),
            asker: "claude",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    let calls = runner.calls();
    assert_eq!(
        calls.len(),
        1,
        "{initial}, {selected}, {current}: {answers:?}"
    );
    let account_recorded = pool
        .store()
        .load(ReviewPool::NOW)
        .unwrap()
        .0
        .accounts
        .get("work")
        .is_some_and(|account| account.auth == Auth::SignedOut);
    let default_recorded = store
        .load()
        .unwrap()
        .of_scope("codex", Some(mode(selected)))
        != Record::default();
    let records = account_recorded || default_recorded;
    if records {
        assert_eq!(
            account_recorded,
            selected == 0,
            "only the file account owns file health"
        );
        assert_eq!(
            default_recorded,
            selected != 0,
            "only the default login owns hidden-backend health"
        );
    }
    [hidden, isolated, launches, records, file_evidence]
}

pub(crate) fn review_storage_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Initial" => {
            next.insert("initial", state["initial"] + 1);
        }
        "Select" => {
            next.insert("selected", state["selected"] + 1);
        }
        "Current" => {
            next.insert("current", state["current"] + 1);
        }
        "Check" => {
            next.insert("phase", 1);
            for (field, value) in ["hidden", "isolated", "launches", "records", "file_evidence"]
                .into_iter()
                .zip(review_storage_case(
                    state["initial"],
                    state["selected"],
                    state["current"],
                ))
            {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn review_storage_conforms_to_current_backend() {
    let model = aterm_models_harness::account_review_storage_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 128);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = review_storage_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (field, law) in [
        ("hidden", "CurrentBackend"),
        ("isolated", "CurrentSeam"),
        ("launches", "LaunchFence"),
        ("records", "OutcomeFence"),
    ] {
        let mut bad = review_storage_transition(&model.init_state(), "Check");
        bad.insert(field, 1 - bad[field]);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut hidden = model.init_state();
    hidden.insert("selected", 1);
    let mut bad = review_storage_transition(&hidden, "Check");
    bad.insert("file_evidence", 1);
    assert!(!model.check_invariant("FileEvidence", &bad));
}

#[test]
fn invalid_codex_storage_configuration_refuses_reviews_and_health() {
    use crate::harness::accounts::Cli;
    for invalid in [
        "not toml",
        "cli_auth_credentials_store = 1",
        "cli_auth_credentials_store = \"unknown\"",
    ] {
        let pool = std::sync::Arc::new(ReviewPool::new(Cli::Codex, Some(true)));
        pool.pin("work");
        let original = pool.facts();
        let config = pool.b.home.join(".codex/config.toml");
        std::fs::write(&config, invalid).unwrap();
        assert!(prepare_from(&pool, &original).refused.is_some());
        codex_storage(&pool, "file");
        let config_during_run = config.clone();
        let runner = FakeRunner::with(move |_| {
            std::fs::write(&config_during_run, invalid).unwrap();
            Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized"))
        });
        let (answer, calls) = pool.review_using(&runner);
        assert_eq!(answer.verdict, Verdict::Auth);
        assert_eq!(calls.len(), 1);
        assert!(
            !pool
                .store()
                .load(ReviewPool::NOW)
                .unwrap()
                .0
                .accounts
                .contains_key("work")
        );
        assert_eq!(
            Store::at(&pool.b.state)
                .load()
                .unwrap()
                .of_scope("codex", Some("file")),
            Record::default()
        );
    }
}

#[test]
fn explicit_codex_homes_use_their_own_backend_fence() {
    use crate::harness::accounts::Cli;
    for invalid in ["not TOML", "cli_auth_credentials_store = 'future'"] {
        let pool = ReviewPool::with_scope(Cli::Codex, Some(true), false);
        let explicit = pool.b.dir.path().join("explicit-codex");
        std::fs::create_dir_all(&explicit).unwrap();
        let config = explicit.join("config.toml");
        let custom = explicit.display().to_string();
        let var = |key: &str| (key == "CODEX_HOME").then(|| custom.clone());
        let store = Store::at(&pool.b.state);
        let runner = FakeRunner::quiet();
        let host = Host {
            config: None,
            runner: &runner,
            var: &var,
            vars: &[],
            store: &store,
            now: &|| ReviewPool::NOW,
        };
        let facts = pool.facts();
        let ask = Ask {
            change: &change(),
            asker: "claude",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        };
        std::fs::write(&config, invalid).unwrap();
        let refused = prepare_review(&host, &facts);
        assert!(refused.refused.is_some());
        assert_eq!(refused.fact.home.as_ref().unwrap(), &explicit);
        let _ = run_prepared(&host, &refused, &ask);
        assert!(runner.calls().is_empty());
        std::fs::write(&config, "cli_auth_credentials_store = 'file'").unwrap();
        let prepared = prepare_review(&host, &facts);
        assert!(prepared.refused.is_none());
        assert_eq!(prepared.fact.home.as_ref().unwrap(), &explicit);
        std::fs::write(&config, invalid).unwrap();
        let (answer, outcome) = run_prepared(&host, &prepared, &ask);
        assert_eq!(answer.verdict, Verdict::Failed);
        assert!(outcome.is_none());
        assert!(runner.calls().is_empty());
        assert_eq!(store.load().unwrap(), crate::ai::store::Records::default());
    }
}

#[test]
fn a_review_checks_the_same_explicit_home_it_passes_to_the_child() {
    use crate::harness::accounts::Cli;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let pool = ReviewPool::with_scope(Cli::Codex, Some(true), false);
    let good = pool.b.dir.path().join("good-home");
    let bad = pool.b.dir.path().join("bad-home");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(
        good.join("config.toml"),
        "cli_auth_credentials_store = 'file'",
    )
    .unwrap();
    std::fs::write(bad.join("config.toml"), "not TOML").unwrap();
    for first_good in [false, true] {
        let reads = AtomicUsize::new(0);
        let first = if first_good { &good } else { &bad };
        let second = if first_good { &bad } else { &good };
        let var = |key: &str| {
            (key == "CODEX_HOME").then(|| {
                if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    first
                } else {
                    second
                }
                .display()
                .to_string()
            })
        };
        let store = Store::at(&pool.b.state);
        let runner = FakeRunner::quiet();
        let prepared = prepare_review(
            &Host {
                config: None,
                runner: &runner,
                var: &var,
                vars: &[],
                store: &store,
                now: &|| ReviewPool::NOW,
            },
            &pool.facts(),
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(prepared.fact.home.as_ref().unwrap(), first);
        assert_eq!(prepared.refused.is_none(), first_good);
        assert!(
            prepared
                .env
                .contains(&("CODEX_HOME".into(), first.display().to_string()))
        );
    }
}

#[test]
fn removing_a_review_home_override_requires_fresh_default_account_context() {
    use crate::harness::accounts::Cli;
    let pool = ReviewPool::new(Cli::Codex, Some(true));
    pool.pin("work");
    let mut original = pool.facts();
    original.home_var = Some("/old-explicit-home".into());
    original.home = Ok("/old-explicit-home".into());
    original.pool = None;
    original.review_source = None;
    let fresh = pool.b.home.display().to_string();
    let var = |key: &str| (key == "HOME").then(|| fresh.clone());
    let store = Store::at(&pool.b.state);
    let runner = FakeRunner::quiet();
    let prepared = prepare_review(
        &Host {
            config: None,
            runner: &runner,
            var: &var,
            vars: &[("HOME".into(), "/stale-home".into())],
            store: &store,
            now: &|| ReviewPool::NOW,
        },
        &original,
    );
    assert!(prepared.refused.is_some());
    assert!(prepared.observation.is_none());
    let _ = run_prepared(
        &Host {
            config: None,
            runner: &runner,
            var: &var,
            vars: &[],
            store: &store,
            now: &|| ReviewPool::NOW,
        },
        &prepared,
        &Ask {
            change: &change(),
            asker: "claude",
            words: None,
            count: 1,
            budget: Duration::from_secs(5),
            level: Level::Yolo,
        },
    );
    assert!(runner.calls().is_empty());
    assert_eq!(store.load().unwrap(), crate::ai::store::Records::default());
}

#[test]
fn a_broken_roster_is_ignored_only_by_current_non_file_storage() {
    use crate::harness::accounts::Cli;
    for initial in ["file", "keyring"] {
        for current in ["file", "keyring"] {
            let pool = ReviewPool::new(Cli::Codex, Some(true));
            std::fs::write(
                pool.config.join(Cli::Codex.accounts().roster_file),
                "broken toml",
            )
            .unwrap();
            codex_storage(&pool, initial);
            let original = pool.facts();
            assert!(original.pool.as_ref().unwrap().refused.is_some());
            codex_storage(&pool, current);
            let prepared = prepare_from(&pool, &original);
            assert_eq!(
                prepared.refused.is_some(),
                current == "file",
                "{initial} -> {current}"
            );
        }
    }
}

#[test]
fn a_roster_discovered_by_the_cli_respects_the_configured_rotation_mode() {
    use crate::harness::accounts::Cli;
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        let pool = ReviewPool::with_scope(cli, (cli == Cli::Codex).then_some(true), false);
        let config = pool.config.join("review-policy.toml");
        std::fs::write(&config, "[harness]\naccounts = \"off\"\n").unwrap();
        let env = crate::ai::cli::Env {
            now: ReviewPool::NOW,
            home: Some(pool.b.home.clone()),
            vars: vec![("HOME".into(), pool.b.home.display().to_string())],
            config: Some(config),
            aterm_state: Some(pool.b.state.clone()),
            harness_state: Some(pool.b.state.clone()),
            pkg: Some(pool.b.pkg.clone()),
            exclude: Vec::new(),
            cwd: pool.b.dir.path().to_path_buf(),
            stdin_tty: false,
            exe: None,
            asker: crate::ai::permission::Asker::Person,
        };
        let original = crate::ai::cli::look(&env)
            .facts
            .into_iter()
            .find(|fact| fact.connector.id == cli.id())
            .unwrap();
        assert!(original.pool.is_none());
        pool.roster(None);
        pool.state(|state| {
            state.active = Some("work".into());
            state.pinned = false;
        });
        let prepared = prepare_from(&pool, &original);
        assert!(prepared.refused.is_none(), "{:?}", prepared.refused);
        assert_eq!(
            prepared
                .observation
                .as_ref()
                .unwrap()
                .1
                .row()
                .unwrap()
                .label,
            "main",
            "{cli:?}: rotation off keeps the default login after discovery"
        );
        let (key, _) = prepared.observation.as_ref().unwrap().1.login(cli);
        assert!(
            !prepared
                .env
                .iter()
                .any(|(name, value)| name == key && !value.is_empty())
        );
    }
}

#[test]
fn a_discovered_codex_roster_keeps_current_recovery_advice() {
    use crate::harness::{accounts::Cli, codex_alab::DefaultRow};
    for walled in [false, true] {
        let pool = ReviewPool::with_scope(Cli::Codex, Some(true), false);
        let mut original = pool.facts();
        let review_program = pool.b.dir.path().join("stock-codex");
        std::fs::write(&review_program, "a program with no isolated-auth contract").unwrap();
        original.managed.as_mut().unwrap().path = review_program;
        pool.roster(None);
        pool.pin("work");
        if walled {
            pool.wall("main");
        }
        let prepared = prepare_from(&pool, &original);
        assert!(prepared.refused.is_none(), "{:?}", prepared.refused);
        let runner = FakeRunner::quiet();
        let store = Store::at(&pool.b.state);
        let home = pool.b.home.display().to_string();
        let var = |key: &str| (key == "HOME").then(|| home.clone());
        let (answer, outcome) = run_prepared(
            &Host {
                config: None,
                runner: &runner,
                var: &var,
                vars: &[],
                store: &store,
                now: &|| ReviewPool::NOW,
            },
            &prepared,
            &Ask {
                change: &change(),
                asker: "claude",
                words: None,
                count: 1,
                budget: Duration::from_secs(5),
                level: Level::Yolo,
            },
        );
        assert_eq!(answer.verdict, Verdict::Failed);
        assert!(outcome.is_none());
        assert!(runner.calls().is_empty());
        assert_eq!(
            answer.fix.as_deref(),
            (!walled).then_some("aterm harness accounts use main --cli codex")
        );
        let selected = prepared.fact.pool.as_ref().unwrap();
        assert_eq!(selected.pinned.as_deref(), Some("work"));
        assert!(
            matches!(
                selected.default_row.as_ref(),
                Some(DefaultRow::Serves(label)) if !walled && label == "main"
            ) || matches!(selected.default_row.as_ref(), Some(DefaultRow::Walled { label, .. }) if walled && label == "main")
        );
    }
}

#[test]
fn an_absent_pooled_roster_has_a_review_source_without_account_history() {
    use crate::harness::accounts::Cli;
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        let pool = ReviewPool::with_scope(cli, (cli == Cli::Codex).then_some(true), false);
        let account_store = pool.store();
        let files = [account_store.state_file(), account_store.logins_file()];
        assert!(files.iter().all(|path| !path.exists()));
        let original = pool.facts();
        assert!(original.pool.is_none());
        assert!(original.review_source.is_some());
        let prepared = prepare_from(&pool, &original);
        assert!(
            prepared.refused.is_none(),
            "{cli:?}: {:?}",
            prepared.refused
        );
        assert!(prepared.fact.pool.is_none());
        assert!(prepared.fact.default_record().is_some());
        assert!(prepared.observation.as_ref().unwrap().1.row().is_none());
        assert!(files.iter().all(|path| !path.exists()));
    }
}

#[test]
fn a_directory_or_dangling_link_roster_cannot_launch_a_pooled_review() {
    use crate::harness::accounts::Cli;
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for dangling in [false, true] {
            #[cfg(not(unix))]
            if dangling {
                continue;
            }
            let pool = ReviewPool::with_scope(cli, (cli == Cli::Codex).then_some(true), false);
            let path = pool.config.join(cli.accounts().roster_file);
            if dangling {
                #[cfg(unix)]
                std::os::unix::fs::symlink(pool.config.join("missing.toml"), &path).unwrap();
            } else {
                std::fs::create_dir(&path).unwrap();
            }
            let facts = pool.facts();
            assert!(facts.pool.as_ref().unwrap().refused.is_some());
            let prepared = prepare_from(&pool, &facts);
            assert!(prepared.refused.is_some(), "{cli:?}, dangling={dangling}");
            let (answer, runs) = pool.review();
            assert_ne!(answer.verdict, Verdict::Served);
            assert!(runs.is_empty(), "{cli:?}, dangling={dangling}");
        }
    }
}

#[cfg(unix)]
#[test]
fn a_linked_roster_created_after_gather_selects_its_witnessed_login() {
    use crate::harness::accounts::Cli;
    let pool = ReviewPool::with_scope(Cli::Codex, Some(true), false);
    let original = pool.facts();
    assert!(original.pool.is_none());
    pool.roster(None);
    pool.pin("work");
    let path = pool.config.join(Cli::Codex.accounts().roster_file);
    let target = pool.config.join("linked-accounts.toml");
    std::fs::rename(&path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let prepared = prepare_from(&pool, &original);
    assert!(prepared.refused.is_none(), "{:?}", prepared.refused);
    assert!(prepared.env.contains(&(
        "CODEX_AUTH_PATH".into(),
        pool.work().join("auth.json").display().to_string()
    )));
    assert_eq!(
        prepared
            .observation
            .as_ref()
            .unwrap()
            .1
            .row()
            .unwrap()
            .label,
        "work"
    );
    assert!(prepared.fact.default_record().is_none());
}

fn preparation_case_for(cli: crate::harness::accounts::Cli, scope: i64, changed: i64) -> [bool; 4] {
    use crate::harness::accounts::{Auth, Cli};
    let pool = ReviewPool::with_scope(cli, (cli == Cli::Codex).then_some(true), scope != 0);
    if scope != 0 {
        pool.pin(if scope == 2 { "work" } else { "main" });
    }
    let mut original = pool.facts();
    if changed == 1 || changed == 2 {
        pool.roster(None);
        pool.pin("work");
        if changed == 2 {
            pool.auth("main", Auth::SignedOut);
            pool.auth("work", Auth::SignedOut);
        }
    }
    let account_store = pool.store();
    if changed == 3 {
        std::fs::create_dir_all(account_store.state_file().parent().unwrap()).unwrap();
        std::fs::write(account_store.state_file(), "damaged account state").unwrap();
        original = pool.facts();
        if cli == Cli::Claude {
            assert!(original.accounts.as_ref().unwrap().unread.is_some());
        } else {
            assert!(original.review_source.is_some());
        }
    }
    if changed == 4 {
        pool.roster(None);
        pool.state(|state| {
            state
                .prepared_accounts
                .extend(["main".into(), "work".into()]);
        });
    }
    let images = || {
        [account_store.state_file(), account_store.logins_file()]
            .map(|path| std::fs::read(path).ok())
    };
    let before = images();
    let store = Store::at(&pool.b.state);
    let runner = FakeRunner::quiet();
    let home = pool.b.home.display().to_string();
    let var = |key: &str| (key == "HOME").then(|| home.clone());
    let host = Host {
        config: None,
        runner: &runner,
        var: &var,
        vars: &[],
        store: &store,
        now: &|| ReviewPool::NOW,
    };
    let prepared = prepare_review(&host, &original);
    if prepared.refused.is_some() {
        let (answer, outcome) = run_prepared(
            &host,
            &prepared,
            &Ask {
                change: &change(),
                asker: "codex",
                words: None,
                count: 1,
                budget: Duration::from_secs(5),
                level: Level::Yolo,
            },
        );
        assert_eq!(answer.verdict, Verdict::Failed);
        assert!(outcome.is_none());
        assert!(runner.calls().is_empty());
    }
    let (managed, isolated) =
        prepared
            .observation
            .as_ref()
            .map_or((false, false), |(_, witness)| {
                let (key, expected) = witness.login(cli);
                let actual = prepared
                    .env
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value)
                    .filter(|value| !value.is_empty());
                assert_eq!(actual, expected.as_ref(), "the child uses its witness");
                let managed = prepared.fact.default_record().is_none();
                assert_eq!(
                    managed,
                    witness.row().is_some(),
                    "health follows its witness"
                );
                (managed, actual.is_some())
            });
    [
        prepared.refused.is_none(),
        managed,
        isolated,
        before == images(),
    ]
}

fn preparation_case(scope: i64, changed: i64) -> [bool; 4] {
    use crate::harness::accounts::Cli;
    let observed = preparation_case_for(Cli::Claude, scope, changed);
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        if !links_here(cli) && (scope == 2 || changed == 1) {
            continue;
        }
        assert_eq!(
            preparation_case_for(cli, scope, changed),
            observed,
            "{cli:?}, scope={scope}, change={changed}"
        );
    }
    observed
}

pub(crate) fn review_preparation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Scope" => {
            next.insert("scope", state["scope"] + 1);
        }
        "Change" => {
            next.insert("change", state["change"] + 1);
        }
        "Prepare" => {
            next.insert("phase", 1);
            for (field, value) in ["ready", "managed", "isolated", "preserved"]
                .into_iter()
                .zip(preparation_case(state["scope"], state["change"]))
            {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn review_preparation_conforms_to_current_account_scope() {
    let model = aterm_models_harness::account_review_preparation_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 30);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = review_preparation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (changed, field, law) in [
        (0, "ready", "ReadyRuns"),
        (2, "ready", "RefusalStops"),
        (0, "managed", "ImplicitStaysUnmanaged"),
        (1, "isolated", "CurrentSeam"),
        (0, "preserved", "ReadPreservesHistory"),
        (4, "ready", "PreparationHeld"),
    ] {
        let mut initial = model.init_state();
        initial.insert("change", changed);
        let mut bad = review_preparation_transition(&initial, "Prepare");
        bad.insert(field, 1 - bad[field]);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn a_prepared_credential_needs_execution_authority_before_review_or_launch() {
    use crate::harness::accounts::Cli;
    for (cli, reads) in [
        (Cli::Claude, None),
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
        (Cli::Codex, Some(false)),
    ] {
        if !atpkg::vendor_direct::served_here(cli.id()) {
            continue;
        }
        for off in [false, true] {
            let pool = ReviewPool::new(cli, reads);
            pool.main_alone();
            if off {
                std::fs::write(
                    pool.config.join("aterm.toml"),
                    "[harness]\naccounts = \"off\"\n",
                )
                .unwrap();
            }
            let (answer, calls) = pool.review();
            assert_eq!(calls.len(), 1, "positive control {cli:?}/{off}: {answer:?}");
            pool.state(|state| {
                state.prepared_accounts.insert("main".into());
            });
            let before = std::fs::read(pool.store().state_file()).unwrap();
            let (answer, calls) = pool.review();
            assert!(calls.is_empty(), "held {cli:?}/{off}: {answer:?}");
            assert_ne!(answer.verdict, Verdict::Served);
            assert_eq!(std::fs::read(pool.store().state_file()).unwrap(), before);
            if cli != Cli::Claude {
                assert!(pool.launch().is_err(), "held launch {cli:?}/{off}");
            }
            pool.state(|state| {
                state.prepared_accounts.remove("main");
            });
            let (answer, calls) = pool.review();
            assert_eq!(calls.len(), 1, "released {cli:?}/{off}: {answer:?}");
        }
    }
}

#[test]
fn a_claude_review_skips_a_prepared_credential_for_an_authorized_one() {
    use crate::harness::accounts::Cli;
    let pool = ReviewPool::new(Cli::Claude, None);
    pool.state(|state| {
        state.active = Some("work".into());
        state.prepared_accounts.insert("work".into());
    });
    let facts = pool.facts();
    assert_eq!(facts.accounts.as_ref().unwrap().label, "main");
    let (answer, calls) = pool.review();
    assert_eq!(calls.len(), 1, "{answer:?}");
    assert!(calls[0].contains(&(crate::harness::accounts::LOGIN_ENV.into(), String::new(),)));
    assert!(
        pool.store()
            .load(ReviewPool::NOW)
            .unwrap()
            .0
            .prepared_accounts
            .contains("work")
    );
}

#[test]
fn a_prepared_pin_never_falls_through_to_another_account() {
    use crate::harness::accounts::Cli;
    // Where aterm makes no overlay home, a pin on Kimi's row of its own is refused before any
    // preparation (`accounts_overlay::links_here`).
    for (cli, reads) in [
        (Cli::Claude, None),
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
    ] {
        if !atpkg::vendor_direct::served_here(cli.id()) || !links_here(cli) {
            continue;
        }
        let pool = ReviewPool::new(cli, reads);
        pool.pin("work");
        let (answer, calls) = pool.review();
        assert_eq!(calls.len(), 1, "control {cli:?}: {answer:?}");
        pool.state(|state| {
            state.prepared_accounts.insert("work".into());
        });
        let (answer, calls) = pool.review();
        assert!(calls.is_empty(), "held pin {cli:?}: {answer:?}");
        if cli == Cli::Claude {
            let facts = pool.facts();
            let account = facts.accounts.as_ref().unwrap();
            assert_eq!(account.label, "work");
            assert!(account.launch_refused.is_some());
            let row = crate::ai::state::assess(&facts, true, ReviewPool::NOW);
            assert_eq!(row.mark, crate::ai::state::Mark::Note);
        } else {
            assert!(pool.launch().is_err(), "held pin {cli:?}");
        }
        pool.state(|state| {
            state.prepared_accounts.remove("work");
        });
        let (answer, calls) = pool.review();
        assert_eq!(calls.len(), 1, "released pin {cli:?}: {answer:?}");
    }
}

#[test]
fn a_prepared_fallback_does_not_hide_an_authorized_accounts_health() {
    use crate::harness::accounts::{Auth, Cli};
    for (cli, reads) in [
        (Cli::Claude, None),
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
        (Cli::Codex, Some(false)),
    ] {
        if !atpkg::vendor_direct::served_here(cli.id()) {
            continue;
        }
        for expired in [false, true] {
            let pool = ReviewPool::new(cli, reads);
            pool.state(|state| {
                state.prepared_accounts.insert("work".into());
            });
            if expired {
                pool.auth("main", Auth::SignedOut);
            }
            if !expired {
                pool.wall("main");
            }
            let facts = pool.facts();
            let label = facts
                .accounts
                .as_ref()
                .map(|a| a.label.as_str())
                .or_else(|| facts.pool.as_ref().and_then(|p| p.serving_label.as_deref()));
            assert_eq!(label, Some("main"), "{cli:?}/{expired}");
            let row = crate::ai::state::assess(&facts, true, ReviewPool::NOW);
            if expired {
                assert_eq!(row.state, State::SignedOut, "{cli:?}: {row:?}");
            } else {
                assert!(matches!(row.state, State::Limited(_)), "{cli:?}: {row:?}");
            }
        }
    }
}

#[test]
fn prepared_and_shared_accounts_are_not_local_sign_in_alternatives() {
    use crate::harness::accounts::{Auth, Cli};
    for (cli, reads) in [
        (Cli::Claude, None),
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
    ] {
        if !atpkg::vendor_direct::served_here(cli.id()) {
            continue;
        }
        for shared in [false, true] {
            let pool = ReviewPool::new(cli, reads);
            pool.wall("main");
            pool.auth("work", Auth::SignedOut);
            let before = pool.facts();
            let next = before
                .accounts
                .as_ref()
                .and_then(|a| a.next_sign_in.as_deref())
                .or_else(|| before.pool.as_ref().and_then(|p| p.next_sign_in.as_deref()));
            assert_eq!(next, Some("work"), "positive control {cli:?}");
            if shared {
                pool.share_work();
            } else {
                pool.state(|state| {
                    state.prepared_accounts.insert("work".into());
                });
            }
            let facts = pool.facts();
            let next = facts
                .accounts
                .as_ref()
                .and_then(|a| a.next_sign_in.as_deref())
                .or_else(|| facts.pool.as_ref().and_then(|p| p.next_sign_in.as_deref()));
            assert_eq!(next, None, "{cli:?}/{shared}");
            if let Some(p) = &facts.pool {
                assert_ne!(p.next.as_deref(), Some("work"));
            }
            let row = crate::ai::state::assess(&facts, true, ReviewPool::NOW);
            assert!(
                matches!(row.state, State::Limited(_)),
                "{cli:?}/{shared}: {row:?}"
            );
        }
    }
}

#[test]
fn a_prepared_expired_pin_explains_its_hold_without_offering_sign_in() {
    use crate::harness::accounts::{Auth, Cli};
    for (cli, reads) in [
        (Cli::Claude, None),
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
        (Cli::Codex, Some(false)),
    ] {
        if !atpkg::vendor_direct::served_here(cli.id()) {
            continue;
        }
        let pool = ReviewPool::new(cli, reads);
        pool.state(|state| {
            state.prepared_accounts.insert("work".into());
        });
        pool.auth("work", Auth::SignedOut);
        pool.pin("work");
        let facts = pool.facts();
        let row = crate::ai::state::assess(&facts, true, ReviewPool::NOW);
        assert_eq!(row.mark, crate::ai::state::Mark::Note, "{cli:?}: {row:?}");
        assert!(
            row.fact.contains("awaiting execution authority"),
            "{cli:?}: {row:?}"
        );
        assert!(row.fix.is_none() && row.mend.is_none(), "{cli:?}: {row:?}");
        assert!(crate::ai::login::refusal(&facts, &row).is_some(), "{cli:?}");
        let (_, calls) = pool.review();
        assert!(calls.is_empty(), "{cli:?}");
    }
}

#[test]
fn a_lone_default_keeps_its_existing_sign_out_and_lease_policy() {
    use crate::harness::accounts::{AccountLease, Auth, Cli};
    for (cli, reads) in [
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
        (Cli::Codex, Some(false)),
    ] {
        let pool = ReviewPool::new(cli, reads);
        pool.main_alone();
        pool.auth("main", Auth::SignedOut);
        pool.state(|state| {
            state.leases.insert(
                "main".into(),
                AccountLease {
                    holder: "other-mac".into(),
                    until: ReviewPool::NOW + 3600,
                    at: ReviewPool::NOW,
                },
            );
        });
        assert_eq!(pool.launch().unwrap(), None, "{cli:?}");
    }
}

#[test]
fn healthy_shared_accounts_remain_healthy_without_local_sign_in() {
    use crate::harness::accounts::Cli;
    // Where aterm makes no overlay home, a pin on Kimi's row of its own is refused, which the
    // doctor names (`accounts_overlay::links_here`).
    for (cli, reads) in [
        (Cli::Claude, None),
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
    ] {
        if !atpkg::vendor_direct::served_here(cli.id()) || !links_here(cli) {
            continue;
        }
        let pool = ReviewPool::new(cli, reads);
        pool.pin("work");
        pool.share_work();
        let facts = pool.facts();
        let row = crate::ai::state::assess(&facts, true, ReviewPool::NOW);
        assert_eq!(row.mark, crate::ai::state::Mark::Ok, "{cli:?}: {row:?}");
        assert!(
            !row.fact.contains("local sign-in is refused"),
            "{cli:?}: {row:?}"
        );
        assert!(crate::ai::login::refusal(&facts, &row).is_some());
    }
}

#[test]
fn a_legacy_codex_held_pin_preserves_its_repin_guidance() {
    use crate::harness::accounts::Cli;
    let pool = ReviewPool::new(Cli::Codex, Some(false));
    pool.pin("work");
    pool.state(|state| {
        state.prepared_accounts.insert("work".into());
    });
    let row = crate::ai::state::assess(&pool.facts(), true, ReviewPool::NOW);
    assert_eq!(row.mark, crate::ai::state::Mark::Warn, "{row:?}");
    assert!(
        row.mend
            .as_deref()
            .is_some_and(|step| step.contains("re-pin")),
        "{row:?}"
    );
}

#[test]
fn isolated_claude_review_outcomes_preserve_default_health() {
    let b = machine(&["claude"]);
    let home = b.home.display().to_string();
    let var = |key: &str| (key == "HOME").then(|| home.clone());
    for isolated in [false, true] {
        for outcome in 0..3 {
            let root = aterm_tempfile::TempDir::new().unwrap();
            let store = Store::at(root.path());
            let mut fact = facts_of(&CLAUDE, b.twin("claude"));
            fact.accounts = Some(claude_account(isolated.then_some("/isolated/login")));
            let rows = [row_of(&CLAUDE, State::Ok)];
            let cands = candidates(&[&CLAUDE], Some(&["claude"]), "codex", &rows, 0);
            let runner = FakeRunner::with(move |_| match outcome {
                0 => Ok(ran(0, "no findings", "")),
                1 => Ok(ran(
                    1,
                    "",
                    "Failed to authenticate. API Error: 401 OAuth session expired",
                )),
                2 => Ok(ran(1, "", "You've hit your limit")),
                _ => unreachable!(),
            });
            let host = Host {
                config: None,
                runner: &runner,
                var: &var,
                vars: &[],
                store: &store,
                now: &|| ReviewPool::NOW,
            };
            let answers = run_review(
                &host,
                &cands,
                &[fact],
                &Ask {
                    change: &change(),
                    asker: "codex",
                    words: None,
                    count: 1,
                    budget: Duration::from_secs(5),
                    level: Level::Yolo,
                },
            );
            assert_eq!(runner.calls().len(), 1);
            assert_eq!(
                answers[0].verdict,
                [Verdict::Served, Verdict::Auth, Verdict::Limit][outcome]
            );
            let record = store.load().unwrap().of("claude");
            assert_eq!(record.ended_at.is_some(), !isolated && outcome == 1);
            assert_eq!(record.limited_until.is_some(), !isolated && outcome == 2);
            assert_eq!(
                record.used_at.is_some(),
                !isolated && (outcome == 0 || outcome == 2)
            );
        }
    }
}

/// One state of a pool a review is judged in: its name, and how it is set on a fresh pool.
type ReviewCase = (&'static str, fn(&ReviewPool));

/// THE STATES OF A POOL A REVIEW IS JUDGED IN, UNDER ROTATION (`early`) AND WITH ROTATION OFF: no
/// pin; `main` held for another Mac's warm set, which a launch passes over; the pin on either
/// row; on `work` with no login of it here; on a label the roster does not list; `main` walled;
/// both walled; `main` recorded signed out; the pin on `work` while another Mac's claim holds it;
/// and `work` active with no pin, which rotation takes and rotation off does not.
fn review_cases() -> [ReviewCase; 11] {
    [
        ("no pin", |_| {}),
        ("work active, no pin", |pool| {
            pool.state(|state| {
                state.active = Some("work".to_string());
                state.proven = true;
            });
        }),
        ("main held for another Mac", |pool| {
            pool.roster(Some("other-mac"));
        }),
        ("pin on work", |pool| pool.pin("work")),
        ("pin on main", |pool| pool.pin("main")),
        ("pin on work, not signed in here", |pool| {
            pool.pin("work");
            pool.work_signed_out();
        }),
        ("pin on a label not listed", |pool| pool.pin("gone")),
        ("main walled", |pool| pool.wall("main")),
        ("both walled", |pool| {
            pool.wall("main");
            pool.wall("work");
        }),
        ("main signed out", ReviewPool::main_signed_out),
        ("pin on work, which another Mac claims", |pool| {
            pool.pin("work");
            let now = ReviewPool::NOW;
            pool.state(|state| state.note_lease("work", "other-mac", now + 3600, now));
        }),
    ]
}

/// `aterm ai review` OF A POOLED CLI RUNS UNDER THE ROW A LAUNCH TAKES, AND NOT AT ALL WHERE THE
/// LAUNCH IS REFUSED: for Kimi Code, Grok Build and Codex (on a codex-alab that keeps
/// `CODEX_AUTH_PATH`'s login and on one from before it), in each of [`review_cases`] under
/// rotation and with it off (by `[harness] accounts`, the harness's switch or the agent's, each as
/// `accounts_for` reads it), a launch that takes a row (`accounts_pool::launch_env_for`) has its
/// review run once under that row's login (the seam's variable naming it, or unset for this
/// machine's own login); one that takes no row has it run on this machine's own login; and one
/// that is refused runs no reviewer, whose answer gives the launch's reason (or, at a wall, the
/// limit or sign-out it is skipped for). With rotation off, which reads no usage, a launch runs
/// into a wall or a sign-out the records show on the login it runs on (`main`'s, with no pin),
/// and the review is skipped for that limit or sign-out; and a pin another Mac's claim holds is
/// refused there as the launch refuses it. Before, a pin on a row not signed in here, or on a
/// label not listed,
/// refused every launch while the review ran on another row or on this machine's own login, and
/// with `main` held for another Mac a launch took `work` while the review ran on `main`.
///
/// Both read every switch through `accounts_for`, whose forms `supervise::config` holds, so the
/// harness's switch and the agent's run at the two cases that tell rotation off from on (each takes
/// another row by rotation). THE GAP: a new read of `enabled` that goes around `accounts_for`, in
/// either path, shows only at those two, and a wall or a sign-out under those two switches no
/// longer runs.
#[test]
fn a_pooled_review_takes_what_a_launch_takes() {
    use crate::harness::accounts::Cli;
    use crate::harness::accounts_pool::NO_WORKER;
    let mut seen = std::collections::BTreeSet::new();
    for (cli, reads) in [
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
        (Cli::Codex, Some(false)),
    ] {
        if !atpkg::vendor_direct::served_here(cli.id()) {
            continue;
        }
        let agent_off = format!("[harness.{}]\nenabled = false\n", cli.program().name());
        for (switch, every_case) in [
            ("", true),
            ("[harness]\naccounts = \"off\"\n", true),
            ("[harness]\nenabled = false\n", false),
            (agent_off.as_str(), false),
        ] {
            for (case, set) in review_cases() {
                if !every_case
                    && !matches!(case, "work active, no pin" | "main held for another Mac")
                {
                    continue;
                }
                let at = format!("{} on {reads:?}, {switch:?}, {case}", cli.id());
                let ready = || {
                    let pool = ReviewPool::new(cli, reads);
                    std::fs::write(pool.config.join("aterm.toml"), switch).expect("aterm.toml");
                    set(&pool);
                    pool
                };
                let reviewed = ready();
                let off = {
                    let (config, _) = crate::supervise::config::SupervisorConfig::from_path(Some(
                        &reviewed.config.join("aterm.toml"),
                    ));
                    config.accounts_for(cli.program()) == crate::harness::accounts::Mode::Off
                };
                let (answer, envs) = reviewed.review();
                let launched = ready();
                let launch = launched.launch();
                // Each pool's own folder, so the two pools' words compare.
                let rooted = |pool: &ReviewPool, text: &str| {
                    text.replace(&pool.b.dir.path().display().to_string(), "<root>")
                };
                let var = cli.accounts().seam.var();
                let carried = |env: &[(String, String)]| {
                    env.iter()
                        .find(|(name, _)| name == var)
                        .map(|(_, value)| value.clone())
                };
                // What the records show on the login a launch takes: a wall, or a sign-out.
                let stops = |pool: &ReviewPool| {
                    let facts = pool.facts();
                    if facts.limited_until.is_some() {
                        Some("a row at a wall")
                    } else {
                        (crate::ai::state::assess(&facts, true, ReviewPool::NOW).state
                            == crate::ai::state::State::SignedOut)
                            .then_some("a row signed out")
                    }
                };
                match &launch {
                    // Rotation off reads no usage, so its launch runs into a wall or a sign-out
                    // the records show on the login it runs on; the review is skipped for it.
                    Ok(_) if off && stops(&reviewed).is_some() => {
                        seen.insert(stops(&reviewed).unwrap_or_default());
                        assert_eq!(answer.verdict, Verdict::Skipped, "{at}: {answer:?}");
                        assert!(envs.is_empty(), "{at}: no reviewer runs: {envs:?}");
                    }
                    Ok(pick) => {
                        seen.insert(if pick.is_some() { "a row" } else { "no row" });
                        assert_eq!(answer.verdict, Verdict::Served, "{at}: {answer:?}");
                        assert_eq!(envs.len(), 1, "{at}: one run");
                        // The row's login as the reviewed pool's roster names it.
                        let want = pick.as_ref().and_then(|pick| {
                            let roster = crate::harness::accounts::Roster::load_for(
                                cli,
                                &reviewed.config.join(cli.accounts().roster_file),
                            )
                            .expect("roster");
                            let row = roster.row(&pick.label).expect("the launch's row");
                            crate::harness::accounts::login_env_for(cli, row).1
                        });
                        assert_eq!(carried(&envs[0]), want, "{at}: {launch:?}");
                    }
                    Err(refused) => {
                        seen.insert("refused");
                        assert!(envs.is_empty(), "{at}: no reviewer runs: {envs:?}");
                        assert_ne!(answer.verdict, Verdict::Served, "{at}");
                        let why = answer.why.as_deref().unwrap_or_default();
                        if answer.verdict == Verdict::Skipped {
                            assert!(
                                stops(&reviewed).is_some(),
                                "{at}: skipped for its limit or sign-out: {why}"
                            );
                        } else {
                            let refused = refused.strip_suffix(NO_WORKER).unwrap_or(refused);
                            let (why, refused) =
                                (rooted(&reviewed, why), rooted(&launched, refused));
                            assert!(why.contains(&refused), "{at}: {why} / {refused}");
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        seen.len(),
        5,
        "a launch took a row, took none, ran into a wall, ran into a sign-out and was refused: \
         {seen:?}"
    );
}

/// A LAUNCH OF A POOL WHOSE ONE ROW IS THIS MACHINE'S OWN LOGIN TAKES NO ROW WHILE NO PIN STANDS,
/// AND THE FACTS A REVIEW READS AGREE: for Kimi Code, Grok Build and Codex on either build, with
/// `main` alone signed out, and for the first two with it walled, a launch
/// (`accounts_pool::launch_env_for`) runs on that login as it is, and the facts record no refusal
/// (`launch_refused`). A wall on a lone Codex row refuses the launch and the facts record that,
/// until the read a launch or a review makes first (`accounts_pool::lift_lone_wall`) shows it gone.
/// CONTROL: with `work` listed beside it and walled too, the launch is refused and the facts record
/// it.
/// A READ THAT LIFTED A LONE ROW'S WALL ALSO ANSWERS FOR ITS ROLLOUTS: nothing rewrites a rollout and
/// a session stopped at its wall writes no later reading, so the full reading it left stays the
/// newest thing on disk and the review's facts would skip the row a launch now runs on. A later
/// rollout's lower share leaves that window's largest share where it was, read when it was read.
/// CONTROL: a full reading from after the read speaks, and so does the first before any read.
#[test]
fn a_read_with_room_outranks_a_full_rollout_reading_older_than_it() {
    use crate::harness::accounts::Cli;
    use crate::harness::codex_rollout::fixtures::{meta_as, rate};
    let now = ReviewPool::NOW;
    let reset = now + 3600;
    let pool = ReviewPool::new(Cli::Codex, Some(true));
    pool.main_alone();
    let codex = pool.b.home.join(".codex");
    let day = codex.join("sessions/2027/01/15");
    std::fs::create_dir_all(&day).unwrap();
    let write = |thread: &str, at: i64, used: f64| {
        let log = day.join(format!("rollout-2027-01-15T10-00-00-{thread}.jsonl"));
        let body = [
            meta_as(
                (at - 1) * 1000,
                thread,
                thread,
                None,
                Some(crate::harness::codex_rollout::PAGINATED),
                Some(("account", "user")),
            ),
            rate(at * 1000, "codex", Some((used, 300, reset)), None),
        ]
        .concat();
        std::fs::write(&log, body).unwrap();
        std::fs::File::open(&log)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(at as u64))
            .unwrap();
    };
    write("01900000-0000-7000-8000-00000000a001", now - 30, 100.0);
    assert_eq!(
        pool.facts().limited_until,
        Some(reset),
        "CONTROL: with no read after it the rollout's wall stands"
    );

    pool.wall("main");
    let seat = crate::harness::accounts_pool::Seat {
        config_dir: &pool.config,
        state: &pool.b.state,
        home: &pool.b.home,
        now,
    };
    let room = |_: Cli, _: &Path, _: Option<&str>, _: i64| {
        Some(crate::harness::codex_app_login::Usage {
            sample: crate::harness::accounts::Sample {
                session_pct: Some(10),
                at: now,
                ..crate::harness::accounts::Sample::default()
            },
            walls: Vec::new(),
        })
    };
    assert!(crate::harness::accounts_pool::lift_lone_wall(
        &seat,
        Cli::Codex,
        &room,
        pool.program().as_deref()
    ));
    assert_eq!(pool.facts().limited_until, None, "lifted");

    write("01900000-0000-7000-8000-00000000a002", now, 5.0);
    assert_eq!(
        pool.facts().limited_until,
        None,
        "a later rollout's lower share does not bring the older full one back"
    );

    // A later read rounding a session up to full speaks for itself; the older reading stays gone.
    pool.state(|state| {
        crate::harness::accounts::note_sample(
            state,
            "main",
            crate::harness::accounts::Sample {
                session_pct: Some(100),
                week_pct: Some(12),
                at: now + 1,
                ..crate::harness::accounts::Sample::default()
            },
            now + 1,
        );
    });
    assert_eq!(pool.facts().limited_until, None, "a read kept its word");

    write("01900000-0000-7000-8000-00000000a003", now, 100.0);
    assert_eq!(
        pool.facts().limited_until,
        Some(reset),
        "CONTROL: a wall read since speaks"
    );
}

/// A review's first wave holds its reviewer only if a limit is all that keeps it out and enough
/// room is left when those that will run have gone first.
#[test]
fn a_reviewer_is_read_only_where_its_first_wave_would_start_it() {
    let shim = PathBuf::from("/x");
    let now = 1_800_000_000;
    let ready = |c: &'static Connector| {
        let mut facts = facts_of(c, shim.clone());
        facts.login_file = Some((PathBuf::from("/x/login"), Some(now - 60)));
        crate::ai::state::assess(&facts, true, now)
    };
    let claude = ready(&CLAUDE);
    let mut codex = ready(&CODEX);
    codex.limited_until = Some(now + 60);
    let rows = [claude, codex];
    let reviewers = [&CLAUDE, &CODEX];
    let cands = candidates(&reviewers, None, "agy", &rows, now);
    assert_eq!(
        cands
            .iter()
            .map(|(_, skip)| skip.is_some())
            .collect::<Vec<_>>(),
        [false, true],
        "CONTROL: Claude runs and Codex sits out: {cands:?}"
    );
    let first = |id, count| first_wave_but_for_its_limit(&cands, id, count, Level::Yolo, now);
    assert!(first("codex", 2), "room beside Claude");
    assert!(!first("codex", 1), "Claude takes the only place");
    assert!(!first("kimi", 2), "not among them");
    assert!(!first("claude", 2), "no limit keeps it out");
    assert!(
        !first_wave_but_for_its_limit(&cands, "codex", 2, Level::Yolo, now + 60),
        "a limit that has ended keeps nothing out"
    );
}

/// THE READ A REVIEW MAKES HOLDS AN ACCOUNT IN USE, as a launch's does ([`crate::harness::provider_read`]):
/// it comes after every refusal, and only for the row the review would start on, so a command that
/// starts nothing there asks the provider nothing. CONTROL: one that would start on it reads once,
/// the wall lifts, and it asks that reviewer.
#[test]
fn a_review_reads_a_lone_walled_row_only_on_its_way_to_start_on_it() {
    use crate::ai::facts::LiveRunner;
    use crate::ai::permission::Asker;
    use crate::harness::accounts::Cli;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Git as it is, in the scratch repository, and a reviewer that answers.
    struct OnlyGit;
    impl Runner for OnlyGit {
        fn run(&self, job: &Job<'_>) -> Result<Ran, RunFailed> {
            if job.program.file_name().is_some_and(|name| name == "git") {
                LiveRunner.run(job)
            } else {
                Ok(ran(0, "fine", ""))
            }
        }
        fn terminal(
            &self,
            _: &Path,
            _: &[String],
            _: &[(String, String)],
            _: Option<&Path>,
        ) -> Result<i32, String> {
            Ok(0)
        }
    }

    let pool = ReviewPool::new(Cli::Codex, Some(true));
    pool.main_alone();
    pool.wall("main");
    let repo = pool.b.dir.path().join("repo");
    let git = |args: &[&str]| {
        let status = aterm_uds::spawnfd::command(crate::test_program::git())
            .args(args)
            .current_dir(&repo)
            .env("HOME", &pool.b.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    };
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "-q"]);
    git(&["config", "user.email", "a@example.test"]);
    git(&["config", "user.name", "a"]);
    std::fs::write(repo.join("a.rs"), "fn a() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-q", "-m", "a"]);
    std::fs::write(repo.join("a.rs"), "fn a() { b() }\n").unwrap();
    let outside = pool.b.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    let reads = AtomicUsize::new(0);
    let room = |_: Cli, _: &Path, _: Option<&str>, _: i64| {
        reads.fetch_add(1, Ordering::SeqCst);
        Some(crate::harness::codex_app_login::Usage {
            sample: crate::harness::accounts::Sample {
                session_pct: Some(10),
                at: ReviewPool::NOW,
                ..crate::harness::accounts::Sample::default()
            },
            walls: Vec::new(),
        })
    };
    let run = |config: &str, cwd: &Path, asker: Asker, args: &[&str]| {
        let cfg = pool.config.join("aterm.toml");
        aterm_uds::atomic_file::write(&cfg, config.as_bytes(), 0o600).unwrap();
        let env = crate::ai::cli::Env {
            now: ReviewPool::NOW,
            home: Some(pool.b.home.clone()),
            vars: vec![
                ("HOME".to_string(), pool.b.home.display().to_string()),
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ],
            config: Some(cfg),
            aterm_state: Some(pool.b.state.clone()),
            harness_state: Some(pool.b.state.clone()),
            pkg: Some(pool.b.pkg.clone()),
            exclude: Vec::new(),
            cwd: cwd.to_path_buf(),
            stdin_tty: false,
            exe: Some(PathBuf::from("/app/aterm")),
            asker,
        };
        let words: Vec<String> = args.iter().map(ToString::to_string).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let _ = crate::ai::cli::review_reading(&env, &OnlyGit, &words, &mut out, &mut err, &room);
        String::from_utf8(err).expect("UTF-8")
    };

    for (config, cwd, args, why) in [
        (
            "[ai]\nreviews = false\n",
            &repo,
            &["--with", "codex"][..],
            "reviews are off",
        ),
        (
            "[ai]\n",
            &repo,
            &["--with", "codex,gpt"][..],
            "is no AI CLI",
        ),
        (
            "[ai]\n",
            &outside,
            &["--with", "codex"][..],
            "git repository",
        ),
    ] {
        let said = run(config, cwd, Asker::Person, args);
        assert!(said.contains(why), "{args:?}: {said}");
        assert_eq!(reads.load(Ordering::SeqCst), 0, "refused: {args:?}: {said}");
    }
    for (config, asker, args) in [
        (
            "[ai]\nreviewers = [\"claude\"]\n",
            Asker::Person,
            &["--all"][..],
        ),
        ("[ai]\n[ai.codex]\nuse = false\n", Asker::Person, &[][..]),
        ("[ai]\n", Asker::Agent("codex", Level::Yolo), &[][..]),
    ] {
        let said = run(config, &repo, asker, args);
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "{config:?} {args:?} starts nothing on it: {said}"
        );
    }

    let said = run("[ai]\n", &repo, Asker::Person, &["--with", "codex"]);
    assert_eq!(reads.load(Ordering::SeqCst), 1, "CONTROL: {said}");
    assert!(said.contains("asking codex"), "lifted, so asked: {said}");
}

#[test]
fn a_lone_default_row_launch_takes_no_row_and_the_facts_refuse_none() {
    use crate::harness::accounts::Cli;
    for (cli, reads) in [
        (Cli::Kimi, None),
        (Cli::Grok, None),
        (Cli::Codex, Some(true)),
        (Cli::Codex, Some(false)),
    ] {
        let walled: fn(&ReviewPool) = |pool| pool.wall("main");
        let signed_out: fn(&ReviewPool) = ReviewPool::main_signed_out;
        for (case, set) in [("walled", walled), ("signed out", signed_out)] {
            let at = format!("{} on {reads:?}, `main` alone {case}", cli.id());
            let pool = ReviewPool::new(cli, reads);
            pool.main_alone();
            set(&pool);
            if cli == Cli::Codex && case == "walled" {
                let refused = pool.launch().expect_err(&at);
                assert!(refused.contains("no usable account"), "{at}: {refused}");
                let facts = pool.facts();
                let accounts = facts.pool.as_ref().expect("the pool");
                assert!(
                    accounts.launch_refused.is_some(),
                    "{at}: the facts agree: {accounts:?}"
                );
                // What the review's own read does first where the wall has gone.
                let seat = crate::harness::accounts_pool::Seat {
                    config_dir: &pool.config,
                    state: &pool.b.state,
                    home: &pool.b.home,
                    now: ReviewPool::NOW,
                };
                let room = |_: Cli, _: &Path, _: Option<&str>, _: i64| {
                    Some(crate::harness::codex_app_login::Usage {
                        sample: crate::harness::accounts::Sample {
                            session_pct: Some(10),
                            at: ReviewPool::NOW,
                            ..crate::harness::accounts::Sample::default()
                        },
                        walls: Vec::new(),
                    })
                };
                assert!(
                    crate::harness::accounts_pool::lift_lone_wall(
                        &seat,
                        cli,
                        &room,
                        pool.program().as_deref()
                    ),
                    "{at}"
                );
                assert_eq!(pool.launch(), Ok(None), "{at}: lifted");
                let facts = pool.facts();
                let accounts = facts.pool.as_ref().expect("the pool");
                assert_eq!(accounts.launch_refused, None, "{at}: lifted: {accounts:?}");
                assert_eq!(facts.limited_until, None, "{at}: lifted");
                continue;
            }
            assert_eq!(pool.launch(), Ok(None), "{at}");
            let witness = crate::harness::accounts_sync::capture_current_review(
                &pool.store(),
                cli,
                Some(&pool.config.join(cli.accounts().roster_file)),
                crate::harness::accounts::Mode::Early,
                pool.program().as_deref(),
                ReviewPool::NOW,
            )
            .expect("a no-row launch still runs on the default login")
            .witness;
            assert_eq!(witness.row().map(|row| row.label.as_str()), Some("main"));
            let facts = pool.facts();
            let accounts = facts.pool.as_ref().expect("the pool");
            assert_eq!(accounts.launch_refused, None, "{at}: {accounts:?}");
        }
        let pool = ReviewPool::new(cli, reads);
        pool.wall("main");
        pool.wall("work");
        assert!(pool.launch().is_err(), "CONTROL {}", cli.id());
        let facts = pool.facts();
        let accounts = facts.pool.as_ref().expect("the pool");
        assert!(
            accounts.launch_refused.is_some(),
            "CONTROL {}: {accounts:?}",
            cli.id()
        );
    }

    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let run = crate::ai::run::RunDir::make(scratch.path()).expect("run dir");
    for bad in ["", ".", "..", "../escape", "sub/file", "sub\\file"] {
        assert!(run.write(bad, "x").is_err(), "{bad:?}");
        assert!(run.subdir(bad).is_err(), "{bad:?}");
    }
    assert!(run.write("prompt.txt", "ok").is_ok());
    assert!(run.subdir("work").is_ok());
}
