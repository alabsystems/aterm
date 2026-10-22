// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::connector::{AGY, CLAUDE, CODEX, MUSE};
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
    // Split so the export's secret scanners do not read the fixtures as secrets.
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
    // An answer that quotes the wall's words is an answer.
    let quoted = Ok(ran(
        0,
        "Finding: the string \"Please run /login\" is matched too loosely.",
        "",
    ));
    assert_eq!(
        verdict_of(&CLAUDE, &quoted, Some("Finding: …"), "n", "").0,
        Verdict::Served
    );
    // The control: the same words from a run that failed are a lost login.
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
        home_exists: true,
        login_file: None,
        hidden_store: false,
        profile_signed_in: true,
        plan: None,
        accounts: None,
        limited_until: None,
        record: Record::default(),
        env_credentials: Vec::new(),
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
    // Named with --with, the asker may be asked too.
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
    // codex fails to authenticate; claude answers.
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
    // A timeout does not give way.
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
        renews: true,
        login: Some("/state/accounts/logins/main+1".into()),
        made: true,
        state: Some("ok"),
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
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
        // The asker's own environment, with its tools' PATH and agent socket.
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
        // Less the outer agent's own variables (the tab rule).
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
    // Sanitized answers may consist entirely of bytes JSON must escape.
    // Their encoded document exceeds the former four-answer raw-byte cap.
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
    // Codex echoes the request (and its own talk of it) on stderr before failing otherwise.
    let echoed = format!(
        "user\n{request}\ncodex\nThe change raises not logged in.\nERROR: stream disconnected before completion"
    );
    let failed = Ok(ran(1, "", &echoed));
    assert_eq!(
        verdict_of(&CODEX, &failed, None, "abc", &request).0,
        Verdict::Failed
    );
    // The control: the same run that ends on Codex's own lost-login line is a lost login.
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
    // The control: on the default home the end is recorded.
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
    let _ = run_review(&host, &cands, &facts, &ask);
    runner.calls().pop().expect("ran").args
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
    ] {
        let args = args_at(c, level);
        assert!(has(&args, want), "{} at {}: {args:?}", c.id, level.word());
    }
    // The control: no reviewer is handed a bypass below yolo.
    for c in [&CLAUDE, &CODEX, &AGY, &MUSE] {
        let args = args_at(c, Level::Ask);
        assert!(
            !args
                .iter()
                .any(|a| a.contains("dangerously") || a == "--yolo"),
            "{} at ask: {args:?}",
            c.id
        );
    }
    // Muse Code's are global flags, before its `exec`.
    let muse = args_at(&MUSE, Level::Yolo);
    let yolo = muse.iter().position(|a| a == "--yolo").expect("--yolo");
    let exec = muse.iter().position(|a| a == "exec").expect("exec");
    assert!(yolo < exec, "{muse:?}");
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
    // Split so the export's secret scanners do not read the fixtures as secrets.
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
    // The control: once it has passed, the due login reviews.
    let got = candidates(&reviewers, None, "codex", &rows, 1_800_001_000);
    assert!(got[0].1.is_none());
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

/// Runs the actual review/env/Store attribution path; the injected vendor uses a different
/// synthetic login when its overriding key is present. No vendor or real key is read.
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
