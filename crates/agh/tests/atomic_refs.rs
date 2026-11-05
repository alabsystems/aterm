// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Atomic ref updates across the real client, journal and loopback service.

use agh::github::refs::{AtomicRefBatch, RefUpdate};
use agh::journal::IntentStore as _;
use agh::policy::{RouteRequest, RouteTarget};
use agh::write::{Effect, Outcome};
use agh::{Github, Journal, Kind, SideEffect};
use agh_fake::{CommitSpec, FakeGithub, Fault, Match};
use aterm_json::{Value, json};

use crate::common;

const APP: &str = "fixture-dev/app";
const NEXT: &str = "89abcdef0123456789abcdef0123456789abcdef";
const ZERO: &str = "0000000000000000000000000000000000000000";
const RECORD: &str = "refs/attest/soundness/candidate/manifest";

struct World {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    gh: Github,
    journal: Journal,
}

fn world(label: &str) -> World {
    let root = common::scratch(label);
    let fake = FakeGithub::start();
    common::canonical_repos(&fake);
    common::canonical_tokens(root.path(), &fake, true, true);
    fake.add_commit(
        APP,
        CommitSpec {
            sha: NEXT.into(),
            message: "next".into(),
            author_login: None,
            author_name: "fixture".into(),
            date: "2026-10-01T00:00:00Z".into(),
            paths: Vec::new(),
        },
    );
    let (dirs, router) = common::file_router(root.path(), common::WRITERS);
    let gh = common::writer(router, &fake, &dirs);
    let journal = gh.journal(&common::slug(APP)).expect("journal");
    World {
        root,
        fake,
        gh,
        journal,
    }
}

fn update(reference: &str, before: &str, after: &str) -> RefUpdate {
    RefUpdate {
        reference: reference.into(),
        before: before.into(),
        after: after.into(),
    }
}

fn batch() -> AtomicRefBatch {
    AtomicRefBatch::new(vec![
        update("refs/heads/main", common::MAIN_SHA, NEXT),
        update(RECORD, ZERO, NEXT),
    ])
    .expect("a batch")
}

impl World {
    fn run(
        &self,
        batch: &AtomicRefBatch,
    ) -> agh::Result<Outcome<agh::write::Ensured<AtomicRefBatch>>> {
        let permit = common::permit(
            &self.gh,
            &RouteRequest::write(RouteTarget::Repo(common::slug(APP))),
        );
        self.gh
            .refs(&common::slug(APP))
            .update_atomic(&permit, batch, &self.journal)
    }

    fn posts(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|r| r.method == "POST" && r.path_and_query == "/graphql")
            .count()
    }

    fn input_file(&self, value: &Value) -> String {
        let file = self.root.path().join("updates.json");
        std::fs::write(&file, value.to_string()).expect("input");
        file.display().to_string()
    }

    fn cli(&self, extra: &[&str]) -> (u8, Value, String) {
        let (mut ctx, captured) = common::ctx(self.root.path(), &self.fake, common::env());
        let mut args = vec!["ref", "update-atomic", APP];
        args.extend_from_slice(extra);
        let exit = common::run_cli(&mut ctx, &args);
        let value = if captured.stdout_text().trim().is_empty() {
            Value::Null
        } else {
            common::stdout_json(&captured)
        };
        (exit, value, captured.stderr_text())
    }
}

#[test]
fn one_mutation_updates_main_and_custom_records_with_expected_oids_and_no_force() {
    let w = world("atomic-refs");
    let Outcome::Done(done) = w.run(&batch()).expect("an outcome") else {
        panic!("acknowledged");
    };
    assert_eq!(done.effect, Effect::Created);
    assert_eq!(w.posts(), 1);
    let refs = w.fake.git_refs(APP);
    assert!(refs.contains(&("refs/heads/main".into(), NEXT.into())));
    assert!(refs.contains(&(RECORD.into(), NEXT.into())));
    let body = w
        .fake
        .bodies()
        .into_iter()
        .find(|b| w.fake.seen()[b.seen].path_and_query == "/graphql")
        .expect("the mutation");
    let value: Value = aterm_json::from_str(body.json.as_deref().expect("JSON")).expect("JSON");
    let input = &value["variables"]["input"];
    assert!(
        input["repositoryId"]
            .as_str()
            .expect("node ID")
            .starts_with("R_")
    );
    let rows = input["refUpdates"].as_array().expect("refUpdates");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r["force"].as_bool() == Some(false) && r["afterOid"].as_str() == Some(NEXT))
    );
    assert!(
        rows.iter()
            .any(|r| r["name"].as_str() == Some("refs/heads/main")
                && r["beforeOid"].as_str() == Some(common::MAIN_SHA))
    );
    assert!(
        rows.iter()
            .any(|r| r["name"].as_str() == Some(RECORD) && r["beforeOid"].as_str() == Some(ZERO))
    );
    assert!(w.journal.open().expect("journal").is_empty());
}

#[test]
fn a_stale_oid_in_one_row_rejects_every_row_even_after_the_guard_read() {
    let w = world("atomic-stale");
    w.fake.set_branch(APP, "main", NEXT);
    // Model a read that saw the old value before another writer moved main.
    w.fake.fault(
        Match::api("/repos/fixture-dev/app/git/ref/heads/main"),
        Fault::Status {
            status: 200,
            headers: Vec::new(),
            body: json!({"ref":"refs/heads/main","object":{"sha":common::MAIN_SHA}}).to_string(),
        },
    );
    assert!(matches!(
        w.run(&batch()).expect("outcome"),
        Outcome::Unknown { .. }
    ));
    assert_eq!(w.posts(), 1);
    assert!(!w.fake.git_refs(APP).iter().any(|(r, _)| r == RECORD));
    assert_eq!(w.journal.open().expect("journal").len(), 1);
}

#[test]
fn a_known_stale_guard_sends_no_mutation() {
    let w = world("atomic-guard");
    w.fake.set_branch(APP, "main", NEXT);
    assert_eq!(w.run(&batch()).expect_err("stale").kind, Kind::Conflict);
    assert_eq!(w.posts(), 0);
    assert!(w.journal.open().expect("journal").is_empty());
}

#[test]
fn graphql_errors_override_even_the_matching_acknowledgement_and_never_leak_their_message() {
    let control = world("atomic-echo-control");
    assert!(matches!(
        control.run(&batch()).expect("ack"),
        Outcome::Done(_)
    ));
    let body = control
        .fake
        .bodies()
        .into_iter()
        .find(|b| control.fake.seen()[b.seen].path_and_query == "/graphql")
        .expect("request");
    let input: Value = aterm_json::from_str(body.json.as_deref().expect("JSON")).expect("JSON");
    let echo = input["variables"]["input"]["clientMutationId"].clone();
    let w = world("atomic-ack-error");
    let token = agh_fake::token("dev");
    w.fake.fault(
        Match::api("/graphql").method("POST"),
        Fault::Status {
            status: 200,
            headers: Vec::new(),
            body: json!({"data":{"updateRefs":{"clientMutationId":echo}},
            "errors":[{"message":token}]})
            .to_string(),
        },
    );
    let Outcome::Unknown { error, .. } = w.run(&batch()).expect("outcome") else {
        panic!("errors win");
    };
    assert_eq!(error.side_effect, SideEffect::Unknown);
    assert!(!error.why.contains(&token));
    assert!(!format!("{:?}", w.journal.read().expect("records")).contains(&token));
    assert_eq!(w.posts(), 1);
}

#[test]
fn an_unacknowledged_mutation_is_unknown_never_retried_and_blocks_other_batches() {
    for (label, fault) in [
        ("lost", Fault::LandThenClose),
        (
            "errors",
            Fault::Status {
                status: 200,
                body: json!({"errors":[{"message":"no"}]}).to_string(),
                headers: Vec::new(),
            },
        ),
        (
            "malformed",
            Fault::Status {
                status: 200,
                body: "not JSON".into(),
                headers: Vec::new(),
            },
        ),
        (
            "wrong-echo",
            Fault::Status {
                status: 200,
                body: json!({"data":{"updateRefs":{"clientMutationId":"another request"}}})
                    .to_string(),
                headers: Vec::new(),
            },
        ),
        (
            "server-error",
            Fault::Status {
                status: 503,
                body: "unavailable".into(),
                headers: Vec::new(),
            },
        ),
    ] {
        let w = world(label);
        w.fake.fault(Match::api("/graphql").method("POST"), fault);
        let Outcome::Unknown { error, .. } = w.run(&batch()).expect("outcome") else {
            panic!("unknown: {label}");
        };
        assert_eq!(error.side_effect, SideEffect::Unknown, "{label}");
        assert_eq!(w.posts(), 1, "{label}");
        let other =
            AtomicRefBatch::new(vec![update("refs/attest/another", ZERO, NEXT)]).expect("batch");
        let error = w
            .run(&other)
            .expect_err("the open intent blocks another batch");
        assert_eq!(error.kind, Kind::OutcomeUnknown, "{label}");
        assert_eq!(w.posts(), 1, "{label}");
        assert_eq!(w.journal.open().expect("journal").len(), 1);
    }
}

#[test]
fn batch_validation_refuses_duplicates_deletion_bad_names_and_bad_oids() {
    for updates in [
        vec![],
        vec![update("refs/heads/a", ZERO, NEXT); 129],
        vec![update("refs/heads/a", ZERO, NEXT); 2],
        vec![update("refs/attest/a", common::MAIN_SHA, ZERO)],
        vec![update("heads/main", ZERO, NEXT)],
        vec![update("refs/heads/a..b", ZERO, NEXT)],
        vec![update("refs/heads/a", "short", NEXT)],
        vec![update("refs/heads/a", ZERO, &NEXT.to_uppercase())],
    ] {
        assert!(AtomicRefBatch::new(updates).is_err());
    }
    let valid: Vec<_> = (0..128)
        .map(|i| update(&format!("refs/attest/{i}"), ZERO, NEXT))
        .collect();
    assert_eq!(
        AtomicRefBatch::new(valid)
            .expect("128 distinct refs")
            .updates()
            .len(),
        128
    );
}

#[test]
fn another_repository_or_mismatched_repository_metadata_cannot_authorize_the_batch() {
    let w = world("atomic-bound");
    let other = common::permit(
        &w.gh,
        &RouteRequest::write(RouteTarget::Repo(common::slug("fixture-dev/other"))),
    );
    assert_eq!(
        w.gh.refs(&common::slug(APP))
            .update_atomic(&other, &batch(), &w.journal)
            .expect_err("wrong permit")
            .kind,
        Kind::Usage
    );
    assert!(w.fake.seen().is_empty());
    w.fake.fault(
        Match::api("/repos/fixture-dev/app"),
        Fault::Status {
            status: 200,
            headers: Vec::new(),
            body: json!({"id":1,"name":"app","full_name":"other/repo","node_id":"R_1",
            "owner":{"id":1,"login":"other"},"permissions":{"push":true}})
            .to_string(),
        },
    );
    assert!(w.run(&batch()).is_err());
    assert_eq!(w.posts(), 0);
}

#[test]
fn the_cli_dry_run_checks_guards_but_posts_nothing_and_a_real_run_journals() {
    let w = world("atomic-cli");
    let input = w.input_file(&json!([
        {"ref":"refs/heads/main","before":common::MAIN_SHA,"after":NEXT},
        {"ref":RECORD,"before":ZERO,"after":NEXT},
    ]));
    let (exit, report, stderr) = w.cli(&["--input", &input, "--dry-run"]);
    assert_eq!(exit, 0, "{stderr}");
    assert_eq!(report["outcome"].as_str(), Some("would_send"));
    assert_eq!(w.posts(), 0);
    assert!(w.journal.open().expect("journal").is_empty());
    let (exit, report, stderr) = w.cli(&["--input", &input]);
    assert_eq!(exit, 0, "{stderr}");
    assert_eq!(report["op"].as_str(), Some("ref.update-atomic"));
    assert_eq!(report["outcome"].as_str(), Some("created"));
    assert!(report["intent"].as_str().is_some());
    assert_eq!(w.posts(), 1);
}

#[test]
fn malformed_typed_input_is_rejected_before_routing_or_reads() {
    for value in [
        json!([{"ref":"refs/heads/main","before":ZERO,"after":NEXT,"force":true}]),
        json!({"repositoryId":"R_another_repo","refUpdates":[]}),
        json!([{"ref":"refs/heads/main","before":ZERO,"after":0}]),
    ] {
        let w = world("atomic-input");
        let input = w.input_file(&value);
        let (exit, _, _) = w.cli(&["--input", &input]);
        assert_ne!(exit, 0);
        assert!(w.fake.seen().is_empty());
    }
}

#[test]
fn duplicate_json_fields_are_rejected_before_routing() {
    let w = world("atomic-duplicate-json");
    let path = w.root.path().join("duplicates.json");
    std::fs::write(&path, format!(
        "[{{\"ref\":\"refs/heads/ignored\",\"ref\":\"refs/heads/main\",\"before\":\"{}\",\"after\":\"{NEXT}\"}}]", common::MAIN_SHA,
    )).expect("input");
    let (exit, _, _) = w.cli(&["--input", path.to_str().expect("path")]);
    assert_ne!(exit, 0);
    assert!(w.fake.seen().is_empty());
}

#[test]
fn a_bare_replay_cannot_read_an_input_file_named_by_a_tape() {
    let w = world("atomic-replay-file");
    let input =
        w.input_file(&json!([{"ref":"refs/heads/main","before":common::MAIN_SHA,"after":NEXT}]));
    let tape = w.root.path().join("atomic.atape");
    let (exit, _, stderr) = w.cli(&[
        "--input",
        &input,
        "--dry-run",
        "--record",
        tape.to_str().expect("path"),
    ]);
    assert_eq!(exit, 0, "{stderr}");
    std::fs::remove_file(input).expect("the tape must not read the file");
    let (mut ctx, captured) = common::ctx(w.root.path(), &w.fake, common::env());
    let exit = common::run_cli(&mut ctx, &["--replay", tape.to_str().expect("path")]);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(
        captured
            .stderr_text()
            .contains("a bare --replay reads no file")
    );
    assert_eq!(w.posts(), 0);
}
