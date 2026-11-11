// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual gate evaluation, formatted private transcripts and the status reader's causal boundary.
//! Production stdout uses the existing `say_to` transport and is not exercised here: synthetic
//! failure notices must not escape these fixtures into an enclosing release's status stream.

use super::*;
use crate::release::{Candidate, Refusal, Step};
use crate::release_steps::{CommitGateNotice, check_commit_gate};
use std::cell::{Cell, RefCell};
use std::io::Write as _;

fn record(notice: &CommitGateNotice<'_>, gate: &str, refusal: &Refusal) {
    let event = notice.event(gate, refusal);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(notice.transcript)
        .unwrap();
    writeln!(file, "release-event: {event}").unwrap();
}

fn candidate(commit: String) -> Candidate {
    Candidate {
        commit,
        version: "0.2.0".into(),
        why: "owned observation fixture".into(),
        bump_from: None,
        carry: Vec::new(),
    }
}

fn real_changelog(root: &Path) -> (Candidate, crate::ledger::testing::Git) {
    crate::ledger::testing::init(
        root,
        &[
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ],
    );
    std::fs::write(
        root.join("CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n",
    )
    .unwrap();
    let git = crate::ledger::testing::Git::new(root);
    for args in [
        &["add", "CHANGELOG.md"][..],
        &["commit", "-qm", "Empty actual notes"][..],
    ] {
        crate::ledger::git_ok(&git, args).unwrap();
    }
    let commit = crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    let c = candidate(commit);
    (c, git)
}

fn changelog_refusal(git: &dyn crate::ledger::GitRunner, c: &Candidate) -> Refusal {
    let failed = crate::gates::changelog_gate_at(git, &c.commit, &c.version).unwrap_err();
    Refusal::new(failed.to_string(), "write actual release notes")
        .detail(["retain this complete detail in the aggregate".into()])
}

#[test]
fn a_known_gate_failure_is_visible_inside_the_next_evaluator_without_closing_the_stage() {
    for gate_dry in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (c, git) = real_changelog(&dir.path().join("repo"));
        let first = changelog_refusal(&git, &c);
        let transcript = dir.path().join("transcript.log");
        let log = dir.path().join("01-checks.log");
        let start = aterm_json::json!({"schema":1,"event":"stage-started","stage_index":1,"stage":"commit checks"});
        let release = aterm_json::json!({"schema":1,"event":"release-stage","stage":"checks","outcome":"began","source":c.commit,"version":c.version});
        std::fs::write(
            &transcript,
            format!("verify-event: {start}\nrelease-event: {release}\n"),
        )
        .unwrap();
        let notice = CommitGateNotice {
            candidate: &c,
            step: Step::Checks,
            gate_dry,
            log: &log,
            transcript: &transcript,
        };
        let mut report = |gate: &str, refusal: &Refusal| record(&notice, gate, refusal);
        let mut blocks = Vec::new();
        assert!(
            check_commit_gate::<()>("changelog", &mut blocks, &mut report, || Err(
                changelog_refusal(&git, &c)
            ))
            .is_none()
        );
        let later_ran = Cell::new(false);
        let second = Refusal::new("prerequisite unavailable", "obtain the real prerequisite")
            .could_not_run();
        assert!(
            check_commit_gate::<()>("prerequisites", &mut blocks, &mut report, || {
                // This is the causal barrier: the later evaluator cannot proceed unless the
                // already known refusal traversed the real transcript and status parser.
                let observed = read_events(std::fs::File::open(&transcript).unwrap()).unwrap();
                let failure = observed
                    .first_failure
                    .as_ref()
                    .expect("earlier failure before later work");
                assert_eq!(failure["gate"].as_str(), Some("changelog"));
                assert_eq!(failure["detail"].as_str(), Some(first.why.as_str()));
                assert_eq!(failure["outcome"].as_str(), Some("FAIL"));
                assert_eq!(failure["source"].as_str(), Some(c.commit.as_str()));
                assert_eq!(failure["gate_dry"].as_bool(), Some(gate_dry));
                assert_eq!(failure["terminal"].as_bool(), Some(false));
                assert_eq!(observed.latest_release_stage, Some(release.clone()));
                assert_eq!(
                    observed.unfinished_stages(),
                    aterm_json::json!([{"stage_index":1,"stage":"commit checks"}])
                );
                later_ran.set(true);
                Err(second.clone())
            })
            .is_none()
        );
        let last_ran = Cell::new(false);
        assert_eq!(
            check_commit_gate("last success", &mut blocks, &mut report, || {
                last_ran.set(true);
                Ok(17)
            }),
            Some(17)
        );
        assert!(
            later_ran.get() && last_ran.get(),
            "the first failure does not skip later checks"
        );
        assert_eq!(
            blocks,
            vec![first.clone(), second],
            "full typed aggregate and order are unchanged"
        );
        let bytes = std::fs::read(&transcript).unwrap();
        let observed = read_events(bytes.as_slice()).unwrap();
        assert_eq!(
            observed.first_failure.unwrap()["detail"].as_str(),
            Some(first.why.as_str())
        );
        assert_eq!(observed.rejected_lines, 0);
        assert_eq!(observed.open_stages.len(), 1);
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .contains("\"outcome\":\"NOT RUN\"")
        );
    }
}

#[test]
fn delaying_notification_until_aggregation_fails_the_early_observation_oracle() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (c, git) = real_changelog(&dir.path().join("repo"));
    let first = changelog_refusal(&git, &c);
    let transcript = dir.path().join("transcript.log");
    std::fs::write(&transcript, "").unwrap();
    let log = dir.path().join("01-checks.log");
    let notice = CommitGateNotice {
        candidate: &c,
        step: Step::Checks,
        gate_dry: true,
        log: &log,
        transcript: &transcript,
    };
    let pending = RefCell::new(Vec::new());
    let mut delayed = |gate: &str, refusal: &Refusal| {
        pending
            .borrow_mut()
            .push((gate.to_owned(), refusal.clone()))
    };
    let mut blocks = Vec::new();
    let _ = check_commit_gate::<()>("changelog", &mut blocks, &mut delayed, || {
        Err(changelog_refusal(&git, &c))
    });
    let saw_early = check_commit_gate("later", &mut blocks, &mut delayed, || {
        Ok(read_events(std::fs::File::open(&transcript).unwrap())
            .unwrap()
            .first_failure
            .is_some())
    })
    .unwrap();
    assert!(
        !saw_early,
        "delayed notification cannot satisfy the causal oracle"
    );
    for (gate, refusal) in pending.into_inner() {
        record(&notice, &gate, &refusal);
    }
    assert!(
        read_events(std::fs::File::open(&transcript).unwrap())
            .unwrap()
            .first_failure
            .is_some()
    );
    assert_eq!(blocks, vec![first]);
}

#[test]
fn gate_notification_bounds_escaped_text_but_retains_the_complete_typed_refusal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let transcript = dir.path().join("transcript.log");
    let c = candidate("é🦀".repeat(512));
    let log = Path::new("x").join("é🦀".repeat(4096));
    let refusal = Refusal::new("\u{1}é🦀".repeat(4096), "\u{2}".repeat(4096))
        .detail((0..7).map(|_| "\u{3}é🦀".repeat(4096)));
    let notice = CommitGateNotice {
        candidate: &c,
        step: Step::Checks,
        gate_dry: true,
        log: &log,
        transcript: &transcript,
    };
    let mut report = |gate: &str, why: &Refusal| record(&notice, gate, why);
    let mut blocks = Vec::new();
    let _ = check_commit_gate::<()>(
        "\u{4}".repeat(4096).as_str(),
        &mut blocks,
        &mut report,
        || Err(refusal.clone()),
    );
    assert_eq!(blocks, vec![refusal]);
    let bytes = std::fs::read(&transcript).unwrap();
    assert!(
        bytes.len() < LINE_LIMIT,
        "escaped UTF-8 event fits the actual reader budget"
    );
    let read = read_events(bytes.as_slice()).unwrap();
    assert_eq!(read.rejected_lines, 0);
    assert!(!read.incomplete_tail);
    let event = read.first_failure.unwrap();
    assert_eq!(event["detail_truncated"].as_bool(), Some(true));
    assert_eq!(event["context_truncated"].as_bool(), Some(true));
    assert_eq!(event["log_unavailable"].as_bool(), Some(true));
    assert!(event["log"].is_null());
}
