// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The write verbs on the command line: the outcome JSON, `--confirm` and `--dry-run` (WP6).

use std::path::PathBuf;
use std::sync::Arc;

use agh::Fx;
use agh::config::Dirs;
use agh_fake::{AssetSpec, FakeGithub, FakeRelease, Fault, Match, ReleaseSpec};
use aterm_json::Value;

use crate::common;

const APP: &str = "fixture-dev/app";
/// A sha no ref of the fixtures names.
const OTHER_SHA: &str = "89abcdef0123456789abcdef0123456789abcdef";
/// When the fixtures' releases were made.
const AT: &str = "2026-10-01T00:00:00Z";

/// A scratch root with the canonical agh.toml, both token files and the canonical repos.
struct World {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
}

/// One command line's exit and streams.
struct Ran {
    exit: u8,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// stdout's one JSON value.
    fn json(&self) -> Value {
        aterm_json::from_str(self.stdout.trim_end())
            .unwrap_or_else(|e| panic!("JSON on stdout ({e}): {} / {}", self.stdout, self.stderr))
    }

    /// stdout's report, which must be one object.
    fn report(&self) -> Value {
        let value = self.json();
        assert!(value.is_object(), "one report: {value}");
        value
    }

    /// stdout's reports: an array's items, or the one object.
    fn reports(&self) -> Vec<Value> {
        match self.json() {
            Value::Array(items) => items,
            value => vec![value],
        }
    }

    /// Each report's outcome, in order.
    fn outcomes(&self) -> Vec<String> {
        self.reports()
            .iter()
            .map(|r| r["outcome"].as_str().unwrap_or("?").to_string())
            .collect()
    }

    /// stderr's error object.
    fn error(&self) -> Value {
        common::error_json(&self.stderr)
    }

    /// Exit `exit`, or a panic naming both streams.
    fn exits(&self, exit: u8) -> &Ran {
        assert_eq!(
            self.exit, exit,
            "stdout {} / stderr {}",
            self.stdout, self.stderr
        );
        self
    }
}

impl World {
    fn new(label: &str) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, true);
        common::write_config(
            &Dirs::under(root.path()),
            &common::canonical_config(root.path()),
        );
        common::canonical_repos(&fake);
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

    /// `run` in the context of a run that pushes through the git runner (plan §1, rule 8's
    /// one exception): the host's `PATH`, `USER` and `LOGNAME`, the agh binary as the helper,
    /// and a simulated clock that starts at the host's.
    fn run_git(&self, args: &[&str]) -> Ran {
        let (ctx, captured) =
            common::ctx(self.root.path(), &self.fake, crate::git_runner::git_env());
        let mut ctx = ctx.with_git_helper(common::helper());
        ctx.fx = Arc::new(Fx::simulated(crate::git_runner::live_start()));
        let exit = common::run_cli(&mut ctx, args);
        Ran {
            exit,
            stdout: captured.stdout_text(),
            stderr: captured.stderr_text(),
        }
    }

    /// `bytes` as `{dir}/{name}` under the root; its path as text.
    fn file(&self, dir: &str, name: &str, bytes: &[u8]) -> String {
        let dir = self.root.path().join(dir);
        std::fs::create_dir_all(&dir).expect("the files' directory");
        common::file(&dir, name, bytes).display().to_string()
    }

    /// Each write the fake saw, `{METHOD} {path}`.
    fn writes(&self) -> Vec<String> {
        self.fake
            .seen()
            .into_iter()
            .filter(|s| s.method != "GET" && s.method != "HEAD")
            .map(|s| format!("{} {}", s.method, s.path_and_query))
            .collect()
    }

    /// The journal files under the scratch state directory.
    fn journals(&self) -> Vec<PathBuf> {
        agh::journal::journals(&Dirs::under(self.root.path()))
            .expect("the journals list")
            .into_iter()
            .map(|(_, path)| path)
            .collect()
    }

    /// `APP`'s release with `tag`.
    fn release(&self, tag: &str) -> FakeRelease {
        self.fake
            .releases_of(APP)
            .into_iter()
            .find(|r| r.tag == tag)
            .unwrap_or_else(|| panic!("no release {tag}"))
    }

    /// A draft `tag` of `APP` made through agh, so its intent is this journal's; its id.
    fn draft(&self, tag: &str) -> u64 {
        self.run(&["release", "create", APP, tag, "--draft"])
            .exits(0);
        self.release(tag).id
    }

    /// A draft `tag` of `APP` holding `a.bin` as `bytes`, made through agh; its id.
    fn draft_with_asset(&self, tag: &str, bytes: &[u8]) -> u64 {
        let id = self.draft(tag);
        let a = self.file(tag, "a.bin", bytes);
        self.run(&["release", "upload", APP, tag, &a]).exits(0);
        id
    }
}

/// `sha256` hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

#[test]
fn create_prints_one_created_report() {
    let w = World::new("cw-create");
    let ran = w.run(&["release", "create", APP, "v1", "--draft", "--notes", "n"]);
    let report = ran.exits(0).report();
    let keys: Vec<&String> = report.as_object().expect("an object").keys().collect();
    assert_eq!(
        keys,
        [
            "answer", "dry_run", "id", "intent", "key", "op", "outcome", "repo", "url"
        ]
    );
    assert_eq!(report["outcome"], Value::from("created"));
    assert_eq!(report["op"], Value::from("release.create"));
    assert_eq!(report["repo"], Value::from(APP));
    assert_eq!(report["key"], Value::from("v1"));
    let releases = w.fake.releases_of(APP);
    assert_eq!(releases.len(), 1, "{releases:?}");
    assert!(releases[0].draft);
    assert_eq!(report["id"], Value::from(releases[0].id));
    let intent = report["intent"].as_str().expect("an intent");
    let digits = intent.strip_prefix("0-").expect("0- and its digits");
    assert_eq!(digits.len(), 12, "{intent}");
    assert!(
        digits
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{intent}"
    );
    assert_eq!(report["dry_run"], Value::from(false));
    assert_eq!(report["answer"], Value::Null);
}

#[test]
fn create_without_draft_is_usage() {
    let w = World::new("cw-no-draft");
    let ran = w.run(&["release", "create", APP, "v1"]);
    ran.exits(2);
    assert!(ran.stdout.is_empty(), "{}", ran.stdout);
    let why = ran.error()["why"].as_str().unwrap_or_default().to_string();
    assert!(why.contains("--draft"), "{why}");
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert!(w.journals().is_empty(), "{:?}", w.journals());
}

#[test]
fn a_second_create_converges() {
    let w = World::new("cw-converge");
    let first = w.run(&["release", "create", APP, "v1", "--draft"]);
    let first = first.exits(0).report();
    let again = w.run(&["release", "create", APP, "v1", "--draft"]);
    let again = again.exits(0).report();
    assert_eq!(again["outcome"], Value::from("converged"), "{again}");
    assert_eq!(again["id"], first["id"]);
    assert_eq!(again["intent"], first["intent"]);
    let posts: Vec<String> = w
        .writes()
        .into_iter()
        .filter(|s| s.starts_with("POST "))
        .collect();
    assert_eq!(posts.len(), 1, "{posts:?}");
}

#[test]
fn a_lost_create_is_unknown_and_never_posted_again() {
    let w = World::new("cw-lost");
    w.fake.fault(
        Match::api("/repos/fixture-dev/app/releases").method("POST"),
        Fault::Close,
    );
    let ran = w.run(&["release", "create", APP, "v1", "--draft"]);
    let report = ran.exits(13).report();
    assert_eq!(report["outcome"], Value::from("unknown"), "{report}");
    assert!(report["intent"].as_str().is_some(), "{report}");
    w.fake.set_listing_lag(APP, true);
    let again = w.run(&["release", "create", APP, "v1", "--draft"]);
    again.exits(13);
    assert_eq!(again.error()["sub"], Value::from("intent_open"));
    assert_eq!(again.report()["intent"], report["intent"]);
    let posts = w.writes();
    assert_eq!(posts.len(), 1, "{posts:?}");
}

#[test]
fn a_release_tier_create_takes_the_typed_literal() {
    let w = World::new("cw-release-tier");
    w.run(&["release", "create", "alabsystems/aterm", "v1", "--draft"])
        .exits(0);
    let post = w
        .fake
        .seen()
        .into_iter()
        .find(|s| s.method == "POST")
        .expect("one POST");
    assert_eq!(post.as_login.as_deref(), Some("fixture-release-bot"));
    w.fake.clear_seen();
    let ran = w.run(&["release", "create", "@channel", "v1", "--draft"]);
    ran.exits(1);
    assert_eq!(
        ran.error()["sub"],
        Value::from("policy.release_target_for_write")
    );
    // The permit's refusal prints the write's report (plan §4.6.20), outcome refused.
    assert_eq!(ran.report()["outcome"], Value::from("refused"));
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
}

/// Each report's `{outcome} {op} {key}`.
fn rows(ran: &Ran) -> Vec<String> {
    ran.reports()
        .iter()
        .map(|r| {
            format!(
                "{} {} {}",
                r["outcome"].as_str().unwrap_or("?"),
                r["op"].as_str().unwrap_or("?"),
                r["key"].as_str().unwrap_or("?")
            )
        })
        .collect()
}

#[test]
fn upload_prints_one_report_per_file() {
    let w = World::new("cw-upload");
    w.draft("v1");
    let a = w.file("files", "a.bin", b"alpha");
    let b = w.file("files", "b.bin", b"beta");
    let ran = w.run(&["release", "upload", APP, "v1", &a, &b]);
    ran.exits(0);
    assert_eq!(
        rows(&ran),
        ["created asset.upload a.bin", "created asset.upload b.bin"]
    );
    let held = w.release("v1").assets;
    for report in ran.reports() {
        let name = report["key"].as_str().expect("a key");
        let (id, ..) = held
            .iter()
            .find(|(_, n, ..)| n == name)
            .unwrap_or_else(|| panic!("{name} is held"));
        assert_eq!(report["id"], Value::from(*id), "{report}");
        let url = report["url"].as_str().expect("a url");
        assert!(
            url.ends_with(&format!("/{APP}/releases/download/v1/{name}")),
            "the asset's browser_download_url: {url}"
        );
    }
    let again = w.run(&["release", "upload", APP, "v1", &a, &b]);
    again.exits(0);
    assert_eq!(again.outcomes(), ["converged", "converged"]);
    let a = w.file("files", "a.bin", b"alpha, changed");
    let clobbered = w.run(&["release", "upload", APP, "v1", &a, &b, "--clobber"]);
    clobbered.exits(0);
    assert_eq!(clobbered.outcomes(), ["created", "unchanged"]);
}

#[test]
fn an_upload_failure_prints_the_reports_so_far() {
    let w = World::new("cw-upload-fail");
    w.draft("v1");
    let held = w.file("first", "b.bin", b"held");
    w.run(&["release", "upload", APP, "v1", &held]).exits(0);
    let a = w.file("second", "a.bin", b"alpha");
    let b = w.file("second", "b.bin", b"other bytes");
    let ran = w.run(&["release", "upload", APP, "v1", &a, &b]);
    ran.exits(9);
    assert_eq!(
        rows(&ran),
        ["created asset.upload a.bin", "refused asset.upload b.bin"]
    );
    assert_eq!(ran.error()["sub"], Value::from("already_exists"));
}

#[test]
fn two_files_of_one_name_send_nothing() {
    let w = World::new("cw-same-name");
    w.draft("v1");
    let x = w.file("x", "a.bin", b"alpha");
    let y = w.file("y", "a.bin", b"beta");
    let before = w.fake.seen().len();
    let upload = w.run(&["release", "upload", APP, "v1", &x, &y]);
    upload.exits(10);
    assert!(upload.stdout.is_empty(), "{}", upload.stdout);
    let ensure = w.run(&[
        "release",
        "ensure",
        APP,
        "v2",
        "--file",
        &x,
        "--file",
        &y,
        "--publish",
    ]);
    ensure.exits(10);
    assert!(ensure.stdout.is_empty(), "{}", ensure.stdout);
    assert_eq!(w.fake.seen().len(), before, "{:?}", w.fake.seen());
}

#[test]
fn notes_follow_their_three_rules() {
    let w = World::new("cw-notes");
    w.draft("v1");
    let notes = w.file("notes", "n.md", b"n");
    let both = [
        "release",
        "edit",
        APP,
        "v1",
        "--notes",
        "n",
        "--notes-file",
        &notes,
    ];
    let ran = w.run(&both);
    ran.exits(2);
    assert!(ran.stdout.is_empty(), "{}", ran.stdout);
    let before = w.fake.seen().len();
    let missing = w.root.path().join("absent.md").display().to_string();
    let ran = w.run(&[
        "release",
        "create",
        APP,
        "v2",
        "--draft",
        "--notes-file",
        &missing,
    ]);
    ran.exits(3);
    assert_eq!(ran.error()["sub"], Value::from("local"));
    let device = w.run(&["release", "edit", APP, "v1", "--notes-file", "/dev/zero"]);
    device.exits(10);
    assert_eq!(w.fake.seen().len(), before, "{:?}", w.fake.seen());
    let long = w.file("notes", "long.md", &[b'n'; 125_001]);
    let ran = w.run(&["release", "edit", APP, "v1", "--notes-file", &long]);
    ran.exits(10);
    assert_eq!(ran.report()["outcome"], Value::from("refused"));
    assert!(
        w.writes().iter().all(|s| !s.starts_with("PATCH ")),
        "{:?}",
        w.writes()
    );
}

#[test]
fn a_ref_found_elsewhere_after_a_lost_create_names_its_intent() {
    let w = World::new("cw-ref-lost");
    w.fake.set_tag(APP, "v1", OTHER_SHA);
    w.fake.fault(
        Match::api("/repos/fixture-dev/app/git/refs").method("POST"),
        Fault::Close,
    );
    let ran = w.run(&["ref", "create", APP, "refs/tags/v1", common::MAIN_SHA]);
    let report = ran.exits(9).report();
    assert_eq!(report["outcome"], Value::from("unknown"), "{report}");
    let intent = report["intent"].as_str().expect("the intent left open");
    assert!(intent.starts_with("0-"), "{intent}");
}

#[test]
fn an_unopenable_file_sends_nothing() {
    let w = World::new("cw-unopenable");
    let long = w.root.path().join("x".repeat(256)).display().to_string();
    let ran = w.run(&["release", "upload", APP, "v1", &long]);
    ran.exits(10);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
}

#[test]
fn upload_by_id_takes_every_positional_as_a_file() {
    let w = World::new("cw-upload-id");
    let id = w.draft("v1").to_string();
    let a = w.file("files", "a.bin", b"alpha");
    let ran = w.run(&["release", "upload", APP, "--id", &id, &a]);
    ran.exits(0);
    assert!(ran.json().is_array(), "{}", ran.stdout);
    assert_eq!(rows(&ran), ["created asset.upload a.bin"]);
    assert!(
        w.release("v1").assets.iter().any(|(_, n, ..)| n == "a.bin"),
        "{:?}",
        w.release("v1")
    );
    let before = w.fake.seen().len();
    let none = w.run(&["release", "upload", APP, "--id", &id]);
    none.exits(2);
    assert!(none.stdout.is_empty(), "{}", none.stdout);
    let why = none.error()["why"].as_str().unwrap_or_default().to_string();
    assert!(why.contains("FILE"), "{why}");
    assert_eq!(w.fake.seen().len(), before);
}

#[test]
fn publish_checks_the_expected_assets() {
    let w = World::new("cw-publish");
    w.draft_with_asset("v1", b"alpha");
    let expect = format!("a.bin={}", hex(b"alpha"));
    let ran = w.run(&["release", "publish", APP, "v1", "--expect-asset", &expect]);
    assert_eq!(ran.exits(0).report()["outcome"], Value::from("created"));
    assert!(!w.release("v1").draft);
    let again = w.run(&["release", "publish", APP, "v1", "--expect-asset", &expect]);
    assert_eq!(again.exits(0).report()["outcome"], Value::from("converged"));
    w.draft_with_asset("v2", b"alpha");
    let before = w.writes();
    let other = format!("a.bin={}", hex(b"other"));
    let refused = w.run(&["release", "publish", APP, "v2", "--expect-asset", &other]);
    refused.exits(9);
    assert_eq!(refused.error()["sub"], Value::from("guard_failed"));
    assert_eq!(w.writes(), before, "no PATCH");
    let bad = w.run(&[
        "release",
        "publish",
        APP,
        "v2",
        "--expect-asset",
        "a.bin=xyz",
    ]);
    bad.exits(2);
}

#[test]
fn edit_without_a_change_is_usage() {
    let w = World::new("cw-edit");
    w.draft("v1");
    w.run(&["release", "edit", APP, "v1"]).exits(2);
    let ran = w.run(&["release", "edit", APP, "v1", "--title", "v1-final"]);
    let report = ran.exits(0).report();
    assert_eq!(report["outcome"], Value::from("created"), "{report}");
    assert_eq!(report["op"], Value::from("release.update"));
    let again = w.run(&["release", "edit", APP, "v1", "--title", "v1-final"]);
    assert_eq!(again.exits(0).report()["outcome"], Value::from("unchanged"));
}

#[test]
fn ensure_reports_each_step() {
    let w = World::new("cw-ensure");
    let a = w.file("files", "a.bin", b"alpha");
    let b = w.file("files", "b.bin", b"beta");
    let line = [
        "release",
        "ensure",
        APP,
        "v2",
        "--file",
        &a,
        "--file",
        &b,
        "--publish",
    ];
    let ran = w.run(&line);
    ran.exits(0);
    assert_eq!(
        rows(&ran),
        [
            "created release.create v2",
            "created asset.upload a.bin",
            "created asset.upload b.bin",
            "created release.publish v2",
        ]
    );
    let writes = w.writes();
    let again = w.run(&line);
    again.exits(0);
    assert_eq!(
        again.outcomes(),
        ["converged", "converged", "converged", "converged"]
    );
    assert_eq!(w.writes(), writes, "no write request is added");
}

#[test]
fn delete_needs_confirm() {
    let w = World::new("cw-delete");
    w.draft("v1");
    w.fake.clear_seen();
    w.run(&["release", "delete", APP, "v1"]).exits(1);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    w.run(&["release", "delete", APP, "v1", "--confirm", "v2"])
        .exits(1);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    let ran = w.run(&["release", "delete", APP, "v1", "--confirm", "v1"]);
    let report = ran.exits(0).report();
    assert_eq!(report["outcome"], Value::from("deleted"), "{report}");
    assert_eq!(report["op"], Value::from("release.delete"));
    let again = w.run(&["release", "delete", APP, "v1", "--confirm", "v1"]);
    assert_eq!(again.exits(0).report()["outcome"], Value::from("unchanged"));
}

#[test]
fn delete_by_id_confirms_against_the_tag() {
    let w = World::new("cw-delete-id");
    let id = w.draft("v1");
    let text = id.to_string();
    w.fake.clear_seen();
    w.run(&["release", "delete", APP, "--id", &text, "--confirm", "v9"])
        .exits(1);
    // The guard read's push check reads the repository, then the one GET of the release, and
    // nothing else: no listing, no second read, no DELETE.
    let seen: Vec<String> = w
        .fake
        .seen()
        .into_iter()
        .map(|s| format!("{} {}", s.method, s.path_and_query))
        .collect();
    assert_eq!(
        seen,
        [
            format!("GET /repos/{APP}"),
            format!("GET /repos/{APP}/releases/{id}"),
        ]
    );
    let line = ["release", "delete", APP, "--id", &text, "--confirm", "v1"];
    assert_eq!(
        w.run(&line).exits(0).report()["outcome"],
        Value::from("deleted")
    );
    // A rerun, as after a lost DELETE answer, has nothing to do.
    assert_eq!(
        w.run(&line).exits(0).report()["outcome"],
        Value::from("unchanged")
    );
}

/// A tag no release among the newest 500 holds is nothing to delete only when the listing ends
/// there: past that window an older draft may hold it, so `delete TAG` is NotFound naming
/// `--id`, and `--id` deletes it.
#[test]
fn a_delete_past_the_draft_window_is_not_found_and_names_the_id() {
    let w = World::new("cw-delete-window");
    let old = w.draft("v-old");
    for n in 0..500 {
        w.fake.add_release(
            APP,
            ReleaseSpec::published(&format!("v{n}"), "2026-11-02T00:00:00Z"),
        );
    }
    w.fake.clear_seen();
    let ran = w.run(&["release", "delete", APP, "v-old", "--confirm", "v-old"]);
    ran.exits(6);
    let why = ran.error()["why"].as_str().unwrap_or_default().to_string();
    assert!(why.contains("--id ID"), "{why}");
    assert!(w.writes().is_empty(), "{:?}", w.writes());
    let text = old.to_string();
    let line = [
        "release",
        "delete",
        APP,
        "--id",
        &text,
        "--confirm",
        "v-old",
    ];
    assert_eq!(
        w.run(&line).exits(0).report()["outcome"],
        Value::from("deleted")
    );
    // The control: a tag no release holds, on a listing that ends inside the window, is
    // nothing to delete.
    let fresh = World::new("cw-delete-window-control");
    fresh.draft("v1");
    let again = fresh.run(&["release", "delete", APP, "v2", "--confirm", "v2"]);
    assert_eq!(again.exits(0).report()["outcome"], Value::from("unchanged"));
}

#[test]
fn asset_delete_confirms_the_name() {
    let w = World::new("cw-asset-delete");
    w.draft_with_asset("v1", b"alpha");
    w.run(&["asset", "delete", APP, "v1", "a.bin"]).exits(1);
    let line = ["asset", "delete", APP, "v1", "a.bin", "--confirm", "a.bin"];
    let report = w.run(&line).exits(0).report();
    assert_eq!(report["outcome"], Value::from("deleted"), "{report}");
    assert_eq!(report["op"], Value::from("asset.delete"));
    assert_eq!(
        w.run(&line).exits(0).report()["outcome"],
        Value::from("unchanged")
    );
}

/// `next`'s command line, `agh` dropped and `ID` replaced by `id`.
fn hinted(next: &str, id: u64) -> Vec<String> {
    let line = next.strip_prefix("name one: ").unwrap_or(next);
    line.split(' ')
        .skip(1)
        .map(|word| {
            if word == "ID" {
                id.to_string()
            } else {
                word.to_string()
            }
        })
        .collect()
}

#[test]
fn an_ambiguous_tag_hints_a_line_the_verb_takes() {
    let w = World::new("cw-ambiguous");
    let first = w.fake.add_release(APP, ReleaseSpec::draft("v1", AT));
    w.fake.add_release(APP, ReleaseSpec::draft("v1", AT));
    // `asset delete` first: `release delete`'s hinted line ends the ambiguity.
    for words in [
        vec!["asset", "delete", APP, "v1", "a.bin", "--confirm", "a.bin"],
        vec!["release", "delete", APP, "v1", "--confirm", "v1"],
    ] {
        let ran = w.run(&words);
        ran.exits(9);
        let error = ran.error();
        let next = error["next"].as_str().expect("a next");
        // The hint runs as typed: no flag the verb does not take, none it requires missing.
        let line = hinted(next, first);
        let args: Vec<&str> = line.iter().map(String::as_str).collect();
        w.run(&args).exits(0);
    }
    assert!(
        w.fake.releases_of(APP).iter().all(|r| r.id != first),
        "{:?}",
        w.fake.releases_of(APP)
    );
}

#[test]
fn a_credential_shaped_value_is_refused_before_anything_is_read() {
    let w = World::new("cw-shaped");
    let shaped = agh_fake::token("shaped-tag");
    let reference = format!("refs/tags/{shaped}");
    // A repository name may hold `_`, so a token's shape is a legal one. The journal judges the
    // lowercased key, so an upper-case one is refused too.
    let shaped_repo = format!("fixture-dev/{}", agh_fake::token("shaped-repo"));
    let upper_repo = format!(
        "fixture-dev/{}",
        agh_fake::token("shaped-repo").to_ascii_uppercase()
    );
    let mut lines: Vec<Vec<&str>> = vec![
        vec!["release", "create", APP, &shaped, "--draft"],
        vec![
            "release", "create", APP, "v1", "--draft", "--target", &shaped,
        ],
        vec!["release", "ensure", APP, &shaped],
        vec!["release", "ensure", APP, "v1", "--target", &shaped],
        vec!["ref", "create", APP, &reference, common::MAIN_SHA],
    ];
    for repo in [&shaped_repo, &upper_repo] {
        lines.push(vec!["release", "create", repo, "v1", "--draft"]);
        lines.push(vec!["release", "ensure", repo, "v1"]);
        lines.push(vec![
            "ref",
            "create",
            repo,
            "refs/tags/v1",
            common::MAIN_SHA,
        ]);
    }
    for words in lines {
        for dry in [true, false] {
            let mut args = words.clone();
            if dry {
                args.push("--dry-run");
            }
            w.fake.clear_seen();
            let ran = w.run(&args);
            ran.exits(2);
            assert!(ran.stdout.is_empty(), "{args:?}: {}", ran.stdout);
            assert!(w.fake.seen().is_empty(), "{args:?}: {:?}", w.fake.seen());
            assert!(w.journals().is_empty(), "{args:?}: {:?}", w.journals());
        }
    }
}

/// A world whose `dev` profile covers every repository, so a repository GitHub answers 404 for
/// is NotFound, as a missing object in it is.
fn covering(label: &str) -> World {
    let w = World::new(label);
    let config = common::canonical_config(w.root.path()).replacen(
        "login = \"fixture-dev-bot\"",
        "login = \"fixture-dev-bot\"\ncovers_all_repos = true",
        1,
    );
    common::write_config(&Dirs::under(w.root.path()), &config);
    w
}

#[test]
fn a_missing_repository_is_never_a_delete_with_nothing_to_do() {
    let w = covering("cw-covering");
    let nope = "fixture-dev/nope";
    let delete = ["release", "delete", nope, "--id", "1", "--confirm", "v1"];
    let ref_delete = [
        "ref",
        "delete",
        nope,
        "refs/tags/v1",
        "--expect",
        common::MAIN_SHA,
        "--confirm",
        "refs/tags/v1",
    ];
    let ref_create = ["ref", "create", nope, "refs/tags/v1", common::MAIN_SHA];
    // The dry runs, then the real deletes: none of them is a write.
    for (words, dry) in [
        (&delete[..], true),
        (&ref_delete[..], true),
        (&ref_create[..], true),
        (&delete[..], false),
        (&ref_delete[..], false),
    ] {
        let mut args = words.to_vec();
        if dry {
            args.push("--dry-run");
        }
        let ran = w.run(&args);
        ran.exits(6);
        assert_eq!(ran.report()["outcome"], Value::from("refused"), "{args:?}");
        assert!(w.writes().is_empty(), "{args:?}: {:?}", w.writes());
    }
    // The real `ref create` is refused by GitHub's 404, as its dry run predicted.
    w.run(&ref_create).exits(6);
}

/// A repository answer with no permissions block fails the push check (design §6.4) in
/// `ref delete` and in its dry run alike: Forbidden `other` (exit 5), the report refused,
/// nothing pushed, so the dry run predicts the real run.
#[test]
fn a_ref_delete_without_a_permissions_block_is_forbidden_dry_or_not() {
    let w = World::new("cw-ref-no-permissions");
    w.fake.set_tag(APP, "v1", common::MAIN_SHA);
    let bare = "{\"id\":1,\"name\":\"app\",\"full_name\":\"fixture-dev/app\",\
                \"owner\":{\"login\":\"fixture-dev\",\"id\":2,\"type\":\"Organization\"},\
                \"private\":true}";
    let line = [
        "ref",
        "delete",
        APP,
        "refs/tags/v1",
        "--expect",
        common::MAIN_SHA,
        "--confirm",
        "refs/tags/v1",
    ];
    let mut answers = Vec::new();
    for dry in [true, false] {
        w.fake.fault(
            Match::api("/repos/fixture-dev/app").method("GET"),
            Fault::Status {
                status: 200,
                body: bare.into(),
                headers: Vec::new(),
            },
        );
        let mut args = line.to_vec();
        if dry {
            args.push("--dry-run");
        }
        let ran = w.run(&args);
        ran.exits(5);
        assert_eq!(ran.report()["outcome"], Value::from("refused"), "{args:?}");
        answers.push(ran.error()["sub"].clone());
        assert!(w.writes().is_empty(), "{args:?}: {:?}", w.writes());
    }
    assert_eq!(answers, vec![Value::from("forbid.other"); 2]);
}

#[test]
fn ref_create_prints_a_ref_report() {
    let w = World::new("cw-ref-create");
    let line = ["ref", "create", APP, "refs/tags/v1", common::MAIN_SHA];
    let report = w.run(&line).exits(0).report();
    assert_eq!(report["outcome"], Value::from("created"), "{report}");
    assert_eq!(report["op"], Value::from("ref.create"));
    assert_eq!(report["id"], Value::Null);
    let url = report["url"].as_str().expect("a url");
    assert!(
        url.ends_with("/repos/fixture-dev/app/git/refs/tags/v1"),
        "{url}"
    );
    assert!(report["intent"].as_str().is_some(), "{report}");
    assert_eq!(
        w.run(&line).exits(0).report()["outcome"],
        Value::from("converged")
    );
}

#[test]
fn ref_delete_needs_expect_and_confirm() {
    let w = World::new("cw-ref-delete");
    w.fake.set_tag(APP, "v1", common::MAIN_SHA);
    w.run(&[
        "ref",
        "delete",
        APP,
        "refs/tags/v1",
        "--confirm",
        "refs/tags/v1",
    ])
    .exits(2);
    w.run(&[
        "ref",
        "delete",
        APP,
        "refs/tags/v1",
        "--expect",
        common::MAIN_SHA,
    ])
    .exits(1);
    let line = [
        "ref",
        "delete",
        APP,
        "refs/tags/v1",
        "--expect",
        common::MAIN_SHA,
        "--confirm",
        "refs/tags/v1",
    ];
    let report = w.run_git(&line).exits(0).report();
    assert_eq!(report["outcome"], Value::from("deleted"), "{report}");
    assert_eq!(report["op"], Value::from("ref.delete"));
    assert!(
        !w.fake
            .git_refs(APP)
            .iter()
            .any(|(name, _)| name == "refs/tags/v1"),
        "{:?}",
        w.fake.git_refs(APP)
    );
    assert_eq!(
        w.run_git(&line).exits(0).report()["outcome"],
        Value::from("unchanged")
    );
}

/// One row of body 9's table: its setup, its command line, the exit its real run ends with
/// (so a row whose setup went wrong cannot pass by both runs agreeing on something else), and
/// whether its real run pushes through the git runner.
struct Row {
    label: &'static str,
    exit: u8,
    setup: fn(&World) -> Vec<String>,
    git: bool,
}

/// What a dry run must leave as it found.
#[derive(Debug, PartialEq)]
struct State {
    /// The writes the fake saw.
    writes: Vec<String>,
    /// Its releases.
    releases: Vec<FakeRelease>,
    /// Its refs.
    refs: Vec<(String, String)>,
    /// The journal files.
    journals: Vec<PathBuf>,
}

/// `w`'s state now.
fn state(w: &World) -> State {
    State {
        writes: w.writes(),
        releases: w.fake.releases_of(APP),
        refs: w.fake.git_refs(APP),
        journals: w.journals(),
    }
}

/// The command line `words`, owned.
fn line(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// A draft `v1` added to the fake directly, holding `a.bin` as `bytes` in `state`; its id.
fn fake_draft(w: &World, bytes: &[u8], state: &str) -> u64 {
    let id = w.fake.add_release(APP, ReleaseSpec::draft("v1", AT));
    w.fake.add_asset(
        id,
        AssetSpec {
            state: state.to_string(),
            ..AssetSpec::uploaded("a.bin", bytes)
        },
    );
    id
}

const ROWS: &[Row] = &[
    Row {
        label: "create-begin",
        exit: 0,
        setup: |_| line(&["release", "create", APP, "v1", "--draft"]),
        git: false,
    },
    Row {
        label: "create-converge",
        exit: 0,
        setup: |w| {
            w.draft("v1");
            line(&["release", "create", APP, "v1", "--draft"])
        },
        git: false,
    },
    Row {
        label: "create-refuse",
        exit: 9,
        setup: |w| {
            w.fake.add_release(APP, ReleaseSpec::published("v1", AT));
            line(&["release", "create", APP, "v1", "--draft"])
        },
        git: false,
    },
    Row {
        label: "create-still-open",
        exit: 13,
        setup: |w| {
            w.fake.fault(
                Match::api("/repos/fixture-dev/app/releases").method("POST"),
                Fault::Close,
            );
            w.run(&["release", "create", APP, "v1", "--draft"])
                .exits(13);
            w.fake.set_listing_lag(APP, true);
            line(&["release", "create", APP, "v1", "--draft"])
        },
        git: false,
    },
    Row {
        label: "upload-new",
        exit: 0,
        setup: |w| {
            w.draft("v1");
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "upload", APP, "v1", &a])
        },
        git: false,
    },
    Row {
        label: "upload-same",
        exit: 0,
        setup: |w| {
            fake_draft(w, b"alpha", "uploaded");
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "upload", APP, "v1", &a])
        },
        git: false,
    },
    Row {
        label: "upload-same-clobber",
        exit: 0,
        setup: |w| {
            fake_draft(w, b"alpha", "uploaded");
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "upload", APP, "v1", &a, "--clobber"])
        },
        git: false,
    },
    Row {
        label: "upload-other",
        exit: 9,
        setup: |w| {
            fake_draft(w, b"alpha", "uploaded");
            let a = w.file("files", "a.bin", b"other");
            line(&["release", "upload", APP, "v1", &a])
        },
        git: false,
    },
    Row {
        label: "upload-other-clobber",
        exit: 0,
        setup: |w| {
            fake_draft(w, b"alpha", "uploaded");
            let a = w.file("files", "a.bin", b"other");
            line(&["release", "upload", APP, "v1", &a, "--clobber"])
        },
        git: false,
    },
    Row {
        label: "upload-starter",
        exit: 9,
        setup: |w| {
            fake_draft(w, b"alpha", "starter");
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "upload", APP, "v1", &a])
        },
        git: false,
    },
    Row {
        label: "upload-starter-clobber",
        exit: 0,
        setup: |w| {
            fake_draft(w, b"alpha", "starter");
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "upload", APP, "v1", &a, "--clobber"])
        },
        git: false,
    },
    Row {
        label: "upload-clobber-long-name",
        exit: 10,
        setup: |w| {
            // 250 bytes: a name GitHub keeps, whose clobber's temporary name it would not.
            let name = format!("{}.bin", "a".repeat(246));
            let id = w.fake.add_release(APP, ReleaseSpec::draft("v1", AT));
            w.fake.add_asset(id, AssetSpec::uploaded(&name, b"alpha"));
            let a = w.file("files", &name, b"alpha, changed");
            line(&["release", "upload", APP, "v1", &a, "--clobber"])
        },
        git: false,
    },
    Row {
        label: "upload-clobber-long-name-unchanged",
        exit: 0,
        setup: |w| {
            // The same bytes: the clobber writes nothing, so its temporary name is not judged.
            let name = format!("{}.bin", "a".repeat(246));
            let id = w.fake.add_release(APP, ReleaseSpec::draft("v1", AT));
            w.fake.add_asset(id, AssetSpec::uploaded(&name, b"alpha"));
            let a = w.file("files", &name, b"alpha");
            line(&["release", "upload", APP, "v1", &a, "--clobber"])
        },
        git: false,
    },
    Row {
        label: "upload-full",
        exit: 10,
        setup: |w| {
            let id = w.fake.add_release(APP, ReleaseSpec::draft("v1", AT));
            for n in 0..1000 {
                w.fake
                    .add_asset(id, AssetSpec::uploaded(&format!("held-{n}.bin"), b"held"));
            }
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "upload", APP, "v1", &a])
        },
        git: false,
    },
    Row {
        label: "upload-fills",
        exit: 10,
        setup: |w| {
            let id = w.fake.add_release(APP, ReleaseSpec::draft("v1", AT));
            for n in 0..999 {
                w.fake
                    .add_asset(id, AssetSpec::uploaded(&format!("held-{n}.bin"), b"held"));
            }
            let a = w.file("files", "a.bin", b"alpha");
            let b = w.file("files", "b.bin", b"beta");
            line(&["release", "upload", APP, "v1", &a, &b])
        },
        git: false,
    },
    Row {
        label: "ensure-fills",
        exit: 10,
        setup: |w| {
            let id = w.draft("v1");
            for n in 0..999 {
                w.fake
                    .add_asset(id, AssetSpec::uploaded(&format!("held-{n}.bin"), b"held"));
            }
            let a = w.file("files", "a.bin", b"alpha");
            let b = w.file("files", "b.bin", b"beta");
            line(&["release", "ensure", APP, "v1", "--file", &a, "--file", &b])
        },
        git: false,
    },
    Row {
        label: "upload-immutable",
        exit: 1,
        setup: |w| {
            let id = w.fake.add_release(APP, ReleaseSpec::published("v1", AT));
            w.fake.set_immutable(id, true);
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "upload", APP, "v1", &a])
        },
        git: false,
    },
    Row {
        label: "publish-draft",
        exit: 0,
        setup: |w| {
            w.draft_with_asset("v1", b"alpha");
            let expect = format!("a.bin={}", hex(b"alpha"));
            line(&["release", "publish", APP, "v1", "--expect-asset", &expect])
        },
        git: false,
    },
    Row {
        label: "publish-published",
        exit: 0,
        setup: |w| {
            w.draft_with_asset("v1", b"alpha");
            w.run(&["release", "publish", APP, "v1"]).exits(0);
            line(&["release", "publish", APP, "v1"])
        },
        git: false,
    },
    Row {
        label: "publish-guard",
        exit: 9,
        setup: |w| {
            w.draft_with_asset("v1", b"alpha");
            let expect = format!("a.bin={}", hex(b"other"));
            line(&["release", "publish", APP, "v1", "--expect-asset", &expect])
        },
        git: false,
    },
    Row {
        label: "edit-unchanged",
        exit: 0,
        setup: |w| {
            w.draft("v1");
            line(&["release", "edit", APP, "v1", "--title", "v1"])
        },
        git: false,
    },
    Row {
        label: "edit-send",
        exit: 0,
        setup: |w| {
            w.draft("v1");
            line(&["release", "edit", APP, "v1", "--title", "v1-final"])
        },
        git: false,
    },
    Row {
        label: "edit-long-notes",
        exit: 10,
        setup: |w| {
            w.draft("v1");
            let notes = w.file("notes", "long.md", &[b'n'; 125_001]);
            line(&["release", "edit", APP, "v1", "--notes-file", &notes])
        },
        git: false,
    },
    Row {
        label: "ensure-refuse",
        exit: 9,
        setup: |w| {
            w.fake.add_release(APP, ReleaseSpec::published("v1", AT));
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "ensure", APP, "v1", "--file", &a, "--publish"])
        },
        git: false,
    },
    Row {
        label: "ensure-fresh",
        exit: 0,
        setup: |w| {
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "ensure", APP, "v1", "--file", &a, "--publish"])
        },
        git: false,
    },
    Row {
        label: "ensure-again",
        exit: 0,
        setup: |w| {
            let a = w.file("files", "a.bin", b"alpha");
            let words = ["release", "ensure", APP, "v1", "--file", &a, "--publish"];
            w.run(&words).exits(0);
            line(&words)
        },
        git: false,
    },
    Row {
        label: "ensure-upload",
        exit: 0,
        setup: |w| {
            w.draft("v1");
            let a = w.file("files", "a.bin", b"alpha");
            line(&["release", "ensure", APP, "v1", "--file", &a, "--publish"])
        },
        git: false,
    },
    Row {
        label: "delete-none",
        exit: 0,
        setup: |_| line(&["release", "delete", APP, "v1", "--confirm", "v1"]),
        git: false,
    },
    Row {
        label: "delete-there",
        exit: 0,
        setup: |w| {
            w.draft("v1");
            line(&["release", "delete", APP, "v1", "--confirm", "v1"])
        },
        git: false,
    },
    Row {
        label: "delete-id-gone",
        exit: 0,
        setup: |w| {
            let id = w.draft("v1").to_string();
            let words = ["release", "delete", APP, "--id", &id, "--confirm", "v1"];
            w.run(&words).exits(0);
            line(&words)
        },
        git: false,
    },
    Row {
        label: "asset-delete-none",
        exit: 0,
        setup: |w| {
            w.draft("v1");
            line(&["asset", "delete", APP, "v1", "a.bin", "--confirm", "a.bin"])
        },
        git: false,
    },
    Row {
        label: "asset-delete-there",
        exit: 0,
        setup: |w| {
            w.draft_with_asset("v1", b"alpha");
            line(&["asset", "delete", APP, "v1", "a.bin", "--confirm", "a.bin"])
        },
        git: false,
    },
    Row {
        label: "ref-create-new",
        exit: 0,
        setup: |_| line(&["ref", "create", APP, "refs/tags/v1", common::MAIN_SHA]),
        git: false,
    },
    Row {
        label: "ref-create-same",
        exit: 0,
        setup: |w| {
            w.fake.set_tag(APP, "v1", common::MAIN_SHA);
            line(&["ref", "create", APP, "refs/tags/v1", common::MAIN_SHA])
        },
        git: false,
    },
    Row {
        label: "ref-create-other",
        exit: 9,
        setup: |w| {
            w.fake.set_tag(APP, "v1", OTHER_SHA);
            line(&["ref", "create", APP, "refs/tags/v1", common::MAIN_SHA])
        },
        git: false,
    },
    Row {
        label: "ref-delete-none",
        exit: 0,
        setup: |_| {
            line(&[
                "ref",
                "delete",
                APP,
                "refs/tags/v1",
                "--expect",
                common::MAIN_SHA,
                "--confirm",
                "refs/tags/v1",
            ])
        },
        git: true,
    },
    Row {
        label: "ref-delete-other",
        exit: 9,
        setup: |w| {
            w.fake.set_tag(APP, "v1", OTHER_SHA);
            line(&[
                "ref",
                "delete",
                APP,
                "refs/tags/v1",
                "--expect",
                common::MAIN_SHA,
                "--confirm",
                "refs/tags/v1",
            ])
        },
        git: true,
    },
    Row {
        label: "ref-delete-there",
        exit: 0,
        setup: |w| {
            w.fake.set_tag(APP, "v1", common::MAIN_SHA);
            line(&[
                "ref",
                "delete",
                APP,
                "refs/tags/v1",
                "--expect",
                common::MAIN_SHA,
                "--confirm",
                "refs/tags/v1",
            ])
        },
        git: true,
    },
];

#[test]
fn dry_run_predicts_the_real_run() {
    for row in ROWS {
        let w = World::new(&format!("cw-dry-{}", row.label));
        let words = (row.setup)(&w);
        let real: Vec<&str> = words.iter().map(String::as_str).collect();
        let mut dry = real.clone();
        dry.push("--dry-run");
        let run = |args: &[&str]| {
            if row.git {
                w.run_git(args)
            } else {
                w.run(args)
            }
        };
        let before = state(&w);
        let predicted = run(&dry);
        assert_eq!(
            state(&w),
            before,
            "{}: the dry run changed something",
            row.label
        );
        let ran = run(&real);
        let label = row.label;
        assert_eq!(
            ran.exit, row.exit,
            "{label}: the real run: {} / {}",
            ran.stdout, ran.stderr
        );
        assert_eq!(
            predicted.exit == 0,
            ran.exit == 0,
            "{label}: dry {} / {} real {} / {}",
            predicted.stdout,
            predicted.stderr,
            ran.stdout,
            ran.stderr
        );
        assert_eq!(
            predicted.json().is_array(),
            ran.json().is_array(),
            "{label}: the dry run's shape is the real run's: {} / {}",
            predicted.stdout,
            ran.stdout
        );
        let (dry_reports, real_reports) = (predicted.reports(), ran.reports());
        assert_eq!(
            dry_reports.len(),
            real_reports.len(),
            "{label}: {} / {}",
            predicted.stdout,
            ran.stdout
        );
        for (dry, real) in dry_reports.iter().zip(&real_reports) {
            assert_eq!(dry["dry_run"], Value::from(true), "{label}: {dry}");
            assert_eq!(dry["intent"], Value::Null, "{label}: {dry}");
            assert_eq!(dry["op"], real["op"], "{label}");
            assert_eq!(dry["key"], real["key"], "{label}");
            let said = dry["outcome"].as_str().unwrap_or("?");
            let did = real["outcome"].as_str().unwrap_or("?");
            if said == "would_send" {
                assert!(
                    did == "created" || did == "deleted",
                    "{label}: would_send, then {did}: {}",
                    ran.stdout
                );
            } else {
                assert_eq!(said, did, "{label}: {} / {}", predicted.stdout, ran.stdout);
            }
        }
        if predicted.exit != 0 {
            assert_eq!(predicted.exit, ran.exit, "{label}");
            let (said, did) = (predicted.error(), ran.error());
            assert_eq!(said["kind"], did["kind"], "{label}: {said} / {did}");
            assert_eq!(said["sub"], did["sub"], "{label}: {said} / {did}");
            // The dry run's refusal is the library's own: its words, not a copy's.
            assert_eq!(said["why"], did["why"], "{label}: {said} / {did}");
        }
    }
}

#[test]
fn notify_parses_where_a_verb_takes_it() {
    let w = World::new("cw-notify");
    let ran = w.run(&["release", "create", APP, "v1", "--draft", "--notify"]);
    assert_eq!(ran.exits(0).report()["outcome"], Value::from("created"));
    let before = w.fake.seen().len();
    let edit = w.run(&["release", "edit", APP, "v1", "--title", "T", "--notify"]);
    edit.exits(2);
    assert!(edit.stdout.is_empty(), "{}", edit.stdout);
    assert_eq!(w.fake.seen().len(), before);
}

#[test]
fn a_usage_error_prints_no_report() {
    let w = World::new("cw-usage");
    let ran = w.run(&["release", "upload", APP, "v1"]);
    ran.exits(2);
    assert!(ran.stdout.is_empty(), "{}", ran.stdout);
}

#[test]
fn a_projection_never_loses_a_write() {
    let w = World::new("cw-projection");
    w.draft("v1");
    w.fake.clear_seen();
    // A field the report lacks is Usage before anything is read or sent.
    let bogus = [
        "release",
        "delete",
        APP,
        "v1",
        "--confirm",
        "v1",
        "--fields",
        "bogus",
    ];
    let ran = w.run(&bogus);
    ran.exits(2);
    assert!(ran.stdout.is_empty(), "{}", ran.stdout);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert_eq!(w.fake.releases_of(APP).len(), 1);
    // A field it has projects the done write's report.
    let fields = [
        "release",
        "delete",
        APP,
        "v1",
        "--confirm",
        "v1",
        "--fields",
        "outcome",
    ];
    assert_eq!(
        w.run(&fields).exits(0).stdout,
        "{\"outcome\":\"deleted\"}\n"
    );
    // A `--jq` program that fails once the write was sent still prints the report whole, and
    // its error says the write took effect.
    let ran = w.run(&[
        "release",
        "create",
        APP,
        "v2",
        "--draft",
        "--jq",
        ".id + \"x\"",
    ]);
    ran.exits(2);
    let report = ran.report();
    assert_eq!(report["outcome"], Value::from("created"), "{report}");
    assert_eq!(report["key"], Value::from("v2"));
    assert_eq!(ran.error()["side_effect"], Value::from("applied"));
    assert_eq!(w.release("v2").id, report["id"].as_u64().expect("an id"));
}

#[test]
fn another_repository_s_journal_never_decides_a_dry_run() {
    use std::os::unix::fs::PermissionsExt as _;
    let w = World::new("cw-other-journal");
    w.run(&["release", "create", APP, "v1", "--draft"]).exits(0);
    let other = Dirs::under(w.root.path()).journal().join("someone-else");
    std::fs::create_dir_all(&other).expect("another owner's journals");
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o000))
        .expect("the directory closed");
    let ran = w.run(&["release", "create", APP, "v2", "--draft", "--dry-run"]);
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o700))
        .expect("the directory opened again");
    assert_eq!(ran.exits(0).report()["outcome"], Value::from("would_send"));
}
