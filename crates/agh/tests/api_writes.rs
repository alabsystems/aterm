// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh api` writes: `-X POST|PATCH|PUT|DELETE`, `--confirm` and `--dry-run` (WP3).

use std::sync::Arc;

use agh::config::Dirs;
use agh::journal::JournalRecord;
use agh::{Fx, IntentId, Journal};
use agh_fake::{FakeGithub, Fault, Match, RepoSpec};
use aterm_json::Value;

use crate::common;

const APP: &str = "fixture-dev/app";
const REFS: &str = "/repos/fixture-dev/app/git/refs";

struct World {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
}

struct Ran {
    exit: u8,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// stdout's report.
    fn report(&self) -> Value {
        aterm_json::from_str(self.stdout.trim_end())
            .unwrap_or_else(|e| panic!("a report on stdout ({e}): {}", self.stdout))
    }

    /// stderr's error object.
    fn error(&self) -> Value {
        common::error_json(&self.stderr)
    }

    /// A usage error (exit 2) whose next is `agh help api`.
    fn usage(&self) -> Value {
        assert_eq!(self.exit, 2, "stderr {}", self.stderr);
        let error = self.error();
        assert_eq!(error["next"], Value::from("agh help api"), "{error}");
        error
    }
}

impl World {
    /// The canonical config and repos; the release token file only when `release`.
    fn new(label: &str, release: bool) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, release);
        common::write_config(
            &Dirs::under(root.path()),
            &common::canonical_config(root.path()),
        );
        fake.add_repo(APP, RepoSpec::private().push("fixture-dev-bot"));
        fake.add_repo(
            "alabsystems/aterm",
            RepoSpec::public().push("fixture-release-bot"),
        );
        fake.set_branch(APP, "main", common::MAIN_SHA);
        World { root, fake }
    }

    fn run(&self, args: &[&str]) -> Ran {
        let (mut ctx, captured) = common::ctx(self.root.path(), &self.fake, common::env());
        let exit = common::run_cli(&mut ctx, args);
        Ran {
            exit,
            stdout: captured.stdout_text(),
            stderr: captured.stderr_text(),
        }
    }

    /// The writes the fake saw.
    fn writes(&self) -> Vec<String> {
        self.fake
            .seen()
            .into_iter()
            .filter(|s| s.method != "GET" && s.method != "HEAD")
            .map(|s| format!("{} {}", s.method, s.path_and_query))
            .collect()
    }

    /// The ids of the journal's intents, in its order.
    fn intents(&self) -> Vec<String> {
        let (records, _) = self.journal().read().expect("the journal reads");
        records
            .into_iter()
            .filter_map(|(_, record)| match record {
                JournalRecord::Intent(intent) => Some(intent.id.to_string()),
                _ => None,
            })
            .collect()
    }

    fn journal(&self) -> Journal {
        let fx = Arc::new(Fx::simulated(common::SIM_START));
        Journal::open(&Dirs::under(self.root.path()), &common::slug(APP), &fx).expect("the journal")
    }
}

/// `api -X POST {REFS}` of tag `tag`.
fn post_tag(tag: &str) -> Vec<String> {
    post_to(REFS, tag)
}

/// `api -X POST {path}` of tag `tag`, `path` as typed.
fn post_to(path: &str, tag: &str) -> Vec<String> {
    vec![
        "api".into(),
        "-X".into(),
        "POST".into(),
        path.into(),
        "-f".into(),
        format!("ref=refs/tags/{tag}"),
        "-f".into(),
        format!("sha={}", common::MAIN_SHA),
    ]
}

fn args(owned: &[String]) -> Vec<&str> {
    owned.iter().map(String::as_str).collect()
}

#[test]
fn api_post_prints_the_report_with_the_answer() {
    let w = World::new("api-post", true);
    let ran = w.run(&args(&post_tag("v1")));
    assert_eq!(ran.exit, 0, "stderr {}", ran.stderr);
    let report = ran.report();
    assert_eq!(report["outcome"], Value::from("created"), "{report}");
    assert_eq!(report["op"], Value::from("api"), "{report}");
    assert_eq!(
        report["key"],
        Value::from(format!("api POST {REFS}")),
        "{report}"
    );
    let intent = report["intent"].as_str().expect("the intent's id");
    assert_eq!(
        Some(intent.to_string()),
        w.intents().first().cloned(),
        "{report}"
    );
    assert!(
        report["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/git/refs/tags/v1")),
        "{report}"
    );
    assert_eq!(
        report["answer"]["ref"],
        Value::from("refs/tags/v1"),
        "{report}"
    );

    // Under `--include` the answer is `{body, headers, status}`, and the report's object
    // is still read from its body.
    let mut owned = post_tag("v4");
    owned.push("--include".into());
    let ran = w.run(&args(&owned));
    assert_eq!(ran.exit, 0, "stderr {}", ran.stderr);
    let report = ran.report();
    assert_eq!(report["answer"]["status"], Value::from(201_u64), "{report}");
    assert!(
        report["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/git/refs/tags/v4")),
        "{report}"
    );
}

#[test]
fn a_second_post_after_a_lost_answer_is_refused() {
    let w = World::new("api-post-lost", true);
    w.fake.fault(Match::api(REFS).method("POST"), Fault::Close);
    // The plan's command, its path typed with no leading slash.
    let typed = "repos/fixture-dev/app/git/refs";
    let first = w.run(&args(&post_to(typed, "v2")));
    assert_eq!(first.exit, 13, "stderr {}", first.stderr);
    let report = first.report();
    assert_eq!(report["outcome"], Value::from("unknown"), "{report}");
    let intent = report["intent"]
        .as_str()
        .expect("the intent's id")
        .to_string();

    // The same command, and every other spelling GitHub reads as the same path, is the same
    // key: each is refused at `begin`, and nothing is sent.
    for path in [
        typed,
        "/repos/Fixture-Dev/App/git/refs",
        "/repos/fixture-dev/app/git/ref%73",
    ] {
        let second = w.run(&args(&post_to(path, "v2")));
        assert_eq!(second.exit, 13, "{path}: stderr {}", second.stderr);
        let error = second.error();
        assert_eq!(error["sub"], Value::from("intent_open"), "{path}: {error}");
        assert!(
            error["why"]
                .as_str()
                .is_some_and(|why| why.contains(&intent)),
            "{path}: {error}"
        );
        // The refusal prints its report too, naming the intent that stands open.
        let report = second.report();
        assert_eq!(
            report["outcome"],
            Value::from("unknown"),
            "{path}: {report}"
        );
        assert_eq!(
            report["intent"],
            Value::from(intent.as_str()),
            "{path}: {report}"
        );
    }
    assert_eq!(w.writes().len(), 1, "{:?}", w.writes());

    let id = IntentId::parse(&intent).expect("an id");
    w.journal().retract(&id).expect("the retract");
    let third = w.run(&args(&post_to(typed, "v2")));
    assert_eq!(third.exit, 0, "stderr {}", third.stderr);
    assert_eq!(third.report()["outcome"], Value::from("created"));
    assert_eq!(w.writes().len(), 2, "{:?}", w.writes());
}

#[test]
fn api_delete_needs_confirm() {
    let w = World::new("api-delete", true);
    w.fake.set_tag(APP, "v1", common::MAIN_SHA);
    let path = "/repos/fixture-dev/app/git/refs/tags/v1";
    let ran = w.run(&["api", "-X", "DELETE", path]);
    assert_eq!(ran.exit, 1, "stderr {}", ran.stderr);
    let other = "/repos/fixture-dev/app/git/refs/tags/v2";
    let ran = w.run(&["api", "-X", "DELETE", path, "--confirm", other]);
    assert_eq!(ran.exit, 1, "stderr {}", ran.stderr);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    let ran = w.run(&["api", "-X", "DELETE", path, "--confirm", path]);
    assert_eq!(ran.exit, 0, "stderr {}", ran.stderr);
    assert_eq!(ran.report()["outcome"], Value::from("created"));
    assert_eq!(w.writes(), vec![format!("DELETE {path}")]);
    // A DELETE is never journaled, so it opens no journal: not even the owner's directory.
    let owner = Dirs::under(w.root.path()).journal().join("fixture-dev");
    assert!(!owner.exists(), "{}", owner.display());
}

#[test]
fn api_dry_run_sends_nothing() {
    let w = World::new("api-dry-run", true);
    let mut owned = post_tag("v1");
    owned.push("--dry-run".into());
    let ran = w.run(&args(&owned));
    assert_eq!(ran.exit, 0, "stderr {}", ran.stderr);
    assert_eq!(ran.report()["outcome"], Value::from("would_send"));
    assert!(w.writes().is_empty(), "{:?}", w.writes());
    let file = Dirs::under(w.root.path())
        .journal()
        .join("fixture-dev")
        .join("app.steps");
    assert!(!file.exists(), "{}", file.display());
}

#[test]
fn api_refusals_print_the_report_and_exit_by_kind() {
    let w = World::new("api-refusals", true);
    w.fake.fault(
        Match::api(REFS).method("POST"),
        Fault::Status {
            status: 422,
            body: "{\"message\":\"Validation Failed\"}".into(),
            headers: Vec::new(),
        },
    );
    let ran = w.run(&args(&post_tag("v1")));
    assert_eq!(ran.exit, 1, "stderr {}", ran.stderr);
    assert_eq!(ran.report()["outcome"], Value::from("refused"));

    w.fake
        .fault(Match::api(REFS).method("POST"), Fault::LandThenClose);
    let ran = w.run(&args(&post_tag("v3")));
    assert_eq!(ran.exit, 13, "stderr {}", ran.stderr);
    assert_eq!(ran.report()["outcome"], Value::from("unknown"));

    let tag = "/repos/fixture-dev/app/git/refs/tags/v1";
    w.fake.fault(
        Match::api(tag).method("PATCH"),
        Fault::Status {
            status: 502,
            body: "{\"message\":\"Bad Gateway\"}".into(),
            headers: Vec::new(),
        },
    );
    let ran = w.run(&["api", "-X", "PATCH", tag, "-f", "sha=x"]);
    assert_eq!(ran.exit, 13, "stderr {}", ran.stderr);
    assert_eq!(ran.report()["outcome"], Value::from("unknown"));

    // An answered 403 is refused, and keeps its own exit (5), which a script reads.
    w.fake.fault(
        Match::api(tag).method("PATCH"),
        Fault::Status {
            status: 403,
            body: "{\"message\":\"Resource not accessible by integration\"}".into(),
            headers: Vec::new(),
        },
    );
    let ran = w.run(&["api", "-X", "PATCH", tag, "-f", "sha=x"]);
    assert_eq!(ran.exit, 5, "stderr {}", ran.stderr);
    assert_eq!(ran.report()["outcome"], Value::from("refused"));

    let bare = World::new("api-no-writer", false);
    let ran = bare.run(&[
        "api",
        "-X",
        "POST",
        "/repos/alabsystems/aterm/git/refs",
        "-f",
        "ref=refs/tags/v1",
    ]);
    assert_eq!(ran.exit, 4, "stderr {}", ran.stderr);
    assert!(bare.fake.seen().is_empty(), "{:?}", bare.fake.seen());
}

#[test]
fn api_x_rejects_other_methods() {
    let w = World::new("api-trace", true);
    let ran = w.run(&["api", "/repos/fixture-dev/app", "-X", "TRACE"]);
    let error = ran.usage();
    assert!(
        error["why"]
            .as_str()
            .is_some_and(|why| why.contains("GET, HEAD, POST, PATCH, PUT or DELETE")),
        "{error}"
    );
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
}

#[test]
fn api_all_is_a_read_flag() {
    let w = World::new("api-post-all", true);
    let mut owned = post_tag("v1");
    owned.push("--all".into());
    w.run(&args(&owned)).usage();
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
}

#[test]
fn api_dry_run_is_a_write_flag() {
    let w = World::new("api-read-flags", true);
    let path = "/repos/fixture-dev/app";
    for args in [
        vec!["api", path, "--dry-run"],
        vec!["api", path, "-X", "HEAD", "--dry-run"],
        vec!["api", path, "--confirm", path],
    ] {
        let error = w.run(&args).usage();
        assert_eq!(error["kind"], Value::from("usage"), "{args:?}: {error}");
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
}
