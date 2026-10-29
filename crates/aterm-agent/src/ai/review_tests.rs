// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::connector::{AGY, CLAUDE, CODEX, GROK, KIMI, MUSE};
use crate::ai::facts::{ClaudeAccounts, Managed};
use crate::ai::permission::Level;
use crate::ai::state::{Mark, State};
use crate::ai::store::Record;
use crate::ai::testing::{FakeRunner, machine, ran};

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
        env_credentials: Vec::new(),
        pool: None,
    }
}

fn row_of(c: &'static Connector, state: State) -> Row {
    Row {
        connector: c,
        wanted: true,
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
        facts_of(&CODEX, b.pkg.agents_dir().join("codex")),
        facts_of(&CLAUDE, b.pkg.agents_dir().join("claude")),
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
    assert!(rec.of("codex").ended() && rec.of("claude").used_at.is_some());
    let runner = FakeRunner::with(|call| {
        if call.program == "codex" {
            Err(RunFailed::Timeout)
        } else {
            Ok(ran(0, "x", ""))
        }
    });
    let host = Host {
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
    let mut claude = facts_of(&CLAUDE, b.pkg.agents_dir().join("claude"));
    claude.accounts = Some(ClaudeAccounts {
        label: "main".into(),
        listed: false,
        refused: None,
        unread: None,
        renews: true,
        login: Some("/state/accounts/logins/main+1".into()),
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
    });
    let facts = vec![claude, facts_of(&CODEX, b.pkg.agents_dir().join("codex"))];
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
    let facts = vec![facts_of(&CODEX, b.pkg.agents_dir().join("codex"))];
    let rows = [row_of(&CODEX, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![&CODEX];
    let cands = candidates(&reviewers, None, "claude", &rows, 0);
    let runner =
        FakeRunner::with(|_| Ok(ran(1, "", "Error: something broke with \x1b[31mcolour\n")));
    let var = |_: &str| None;
    let now = || 0;
    let host = Host {
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
    let facts = vec![facts_of(&CODEX, b.pkg.agents_dir().join("codex"))];
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
        runner: &runner,
        var: &other,
        vars: &[],
        store: &store,
        now: &now,
    };
    let _ = run_review(&host, &cands, &facts, &ask);
    assert_eq!(store.load().expect("load").of("codex"), Record::default());
    let none = |_: &str| None;
    let host = Host {
        runner: &runner,
        var: &none,
        vars: &[],
        store: &store,
        now: &now,
    };
    let _ = run_review(&host, &cands, &facts, &ask);
    assert!(store.load().expect("load").of("codex").ended());
}

#[test]
fn a_limit_keeps_the_cli_out_of_reviews_for_a_while() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let facts = vec![facts_of(&CODEX, b.pkg.agents_dir().join("codex"))];
    let rows = [row_of(&CODEX, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![&CODEX];
    let cands = candidates(&reviewers, None, "claude", &rows, 0);
    let runner = FakeRunner::with(|_| Ok(ran(1, "", "ERROR: You've hit your usage limit.")));
    let now = || 1_000;
    let none = |_: &str| None;
    let host = Host {
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
        store.load().expect("load").of("codex").limited_until,
        Some(1_000 + LIMIT_WAIT_S)
    );
}

fn review_auth_after_login(replacement_at: Option<i64>) -> (Record, Option<Record>) {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    store
        .note("codex", How::Login, Outcome::Worked, 100)
        .expect("initial login");
    let mut fact = facts_of(&CODEX, b.pkg.agents_dir().join("codex"));
    fact.record = store.load().expect("initial record").of("codex");
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
                .note("codex", How::Login, Outcome::Worked, at)
                .expect("new login");
            *seen.lock().expect("replacement") =
                Some(during_run.load().expect("new record").of("codex"));
        }
        Ok(ran(1, "", "ERROR: unexpected status 401 Unauthorized"))
    });
    let var = |_: &str| None;
    let now = || 300;
    let host = Host {
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
    (store.load().expect("final record").of("codex"), expected)
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
    let facts = vec![facts_of(c, b.pkg.agents_dir().join(c.id))];
    let rows = [row_of(c, State::Idle)];
    let reviewers: Vec<&'static Connector> = vec![c];
    let cands = candidates(&reviewers, Some(&[c.id]), "nobody", &rows, 0);
    let runner = FakeRunner::with(|_| Ok(ran(0, "fine", "")));
    let none = |_: &str| None;
    let now = || 0;
    let host = Host {
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
    assert!(yolo < exec, "{muse:?}");
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
    assert!(
        yolo[3].starts_with("Read the file /") && yolo[3].contains("request.txt"),
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
    let facts = vec![facts_of(&CLAUDE, b.pkg.agents_dir().join("claude"))];
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
    let facts = vec![facts_of(&CLAUDE, b.pkg.agents_dir().join("claude"))];
    let rows = [row_of(&CLAUDE, State::Ok)];
    let reviewers: Vec<&'static Connector> = vec![&CLAUDE];
    let cands = candidates(&reviewers, Some(&["claude"]), "codex", &rows, 0);
    let runner = FakeRunner::with(move |_| Ok(ran(exit, &stdout, &stderr)));
    let none = |_: &str| None;
    let now = || 1_000;
    let host = Host {
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
    store
        .note(c.id, How::Login, Outcome::Worked, 100)
        .expect("initial login");
    let mut fact = facts_of(c, b.pkg.agents_dir().join(c.id));
    fact.record = store.load().expect("record").of(c.id);
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
        store.load().expect("final record").of(c.id),
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
    let shim = b.pkg.agents_dir().join("codex");
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
    std::fs::write(
        b.pkg.agents_dir().join("codex"),
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", exe.display()),
    )
    .expect("launcher");
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
        let mut f = facts_of(&CODEX, b.pkg.agents_dir().join("codex"));
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
        std::fs::create_dir_all(staging).expect("staging");
        std::fs::write(staging.join("auth.json"), "\u{0}opaque").expect("auth");
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
        store.load().expect("ai store").of("codex").limited_until,
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
        store.load().expect("ai store").of("codex").used_at,
        Some(now_val)
    );
}

/// A pool of `cli`'s on a fresh machine: `main`, this machine's own login, and `work`, a login of
/// its own, both signed in here, beside its roster in `config`. For Codex, `reads` lays atpkg's
/// launcher onto a codex-alab that keeps `CODEX_AUTH_PATH`'s login (`true`) or one from before
/// it (`false`).
struct ReviewPool {
    b: crate::ai::testing::Box3,
    config: PathBuf,
    cli: crate::harness::accounts::Cli,
}

impl ReviewPool {
    const NOW: i64 = 1_800_000_000;

    fn new(cli: crate::harness::accounts::Cli, reads: Option<bool>) -> ReviewPool {
        let c = cli.connector();
        let b = machine(&[c.id]);
        if let Some(reads) = reads {
            alab_launcher(&b, reads);
        }
        let config = b.dir.path().join("config");
        std::fs::create_dir_all(&config).expect("config");
        let pool = ReviewPool { b, config, cli };
        let store = c.stores.first().copied().expect("a login file");
        let default = pool.b.home.join(cli.accounts().default_home).join(store);
        for login in [&default, &pool.work().join(store)] {
            std::fs::create_dir_all(login.parent().expect("parent")).expect("login home");
            std::fs::write(login, "{}").expect("stand-in login");
        }
        pool.roster(None);
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

    fn pin(&self, label: &str) {
        self.state(|state| {
            state.active = Some(label.to_string());
            state.pinned = true;
        });
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
        self.state(|state| {
            let _ = crate::harness::accounts::note_auth(
                state,
                "main",
                crate::harness::accounts::Auth::SignedOut,
                Self::NOW,
            );
        });
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
        let c = self.cli.connector();
        let home = self.b.home.display().to_string();
        let vars = vec![("HOME".to_string(), home.clone())];
        let var = |k: &str| (k == "HOME").then(|| home.clone());
        let store = Store::at(&self.b.state);
        let facts = vec![self.facts()];
        let rows = [crate::ai::state::assess(&facts[0], true, Self::NOW)];
        let reviewers: Vec<&'static Connector> = vec![c];
        let cands = candidates(&reviewers, Some(&[c.id]), "claude", &rows, Self::NOW);
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
        let now = || Self::NOW;
        let host = Host {
            runner: &runner,
            var: &var,
            vars: &vars,
            store: &store,
            now: &now,
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

/// One state of a pool a review is judged in: its name, and how it is set on a fresh pool.
type ReviewCase = (&'static str, fn(&ReviewPool));

/// THE STATES OF A POOL A REVIEW IS JUDGED IN, UNDER ROTATION (`early`) AND WITH ROTATION OFF: no
/// pin; `main` held for another Mac's warm set, which a launch passes over; the pin on either
/// row; on `work` with no login of it here; on a label the roster does not list; `main` walled;
/// both walled; `main` recorded signed out; and the pin on `work` while another Mac's claim
/// holds it.
fn review_cases() -> [ReviewCase; 10] {
    [
        ("no pin", |_| {}),
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
/// rotation and with it off, a launch that takes a row (`accounts_pool::launch_env_for`) has its
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
        for off in [false, true] {
            for (case, set) in review_cases() {
                let at = format!("{} on {reads:?}, off={off}, {case}", cli.id());
                let ready = || {
                    let pool = ReviewPool::new(cli, reads);
                    if off {
                        std::fs::write(
                            pool.config.join("aterm.toml"),
                            "[harness]\naccounts = \"off\"\n",
                        )
                        .expect("aterm.toml");
                    }
                    set(&pool);
                    pool
                };
                let reviewed = ready();
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
/// `main` alone walled or signed out, a launch (`accounts_pool::launch_env_for`) runs on that
/// login as it is, and the facts record no refusal (`launch_refused`). CONTROL: with `work`
/// listed beside it and walled too, the launch is refused and the facts record it.
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
            assert_eq!(pool.launch(), Ok(None), "{at}");
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
}
