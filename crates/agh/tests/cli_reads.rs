// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The read verbs through `run_cli`: exits, stdout values and the error line's fields.

// Every case routes through the canonical config's profiles, private token files (plan §1,
// rule 12).
#![cfg(unix)]

use std::path::Path;

use agh::config::Dirs;
use agh_fake::{
    AssetSpec, CommitSpec, CompareSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec,
};
use aterm_json::Value;

use crate::common;

/// Public, the dev bot holding push.
const APP: &str = "fixture-dev/app";
/// Public, the dev bot holding pull only.
const PULLED: &str = "fixture-dev/pulled";
/// Private, the dev bot holding push.
const SECRET: &str = "fixture-dev/secret";

struct World {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
}

/// One run: the exit, stdout and stderr.
struct Ran {
    exit: u8,
    stdout: Vec<u8>,
    stderr: String,
}

impl Ran {
    fn text(&self) -> &str {
        std::str::from_utf8(&self.stdout).expect("utf-8")
    }

    fn json(&self) -> Value {
        aterm_json::from_slice(&self.stdout)
            .unwrap_or_else(|e| panic!("{:?}: {e}; stderr {}", self.text(), self.stderr))
    }

    /// The second stderr line's `error`.
    fn error(&self) -> Value {
        let line = self
            .stderr
            .lines()
            .nth(1)
            .unwrap_or_else(|| panic!("two stderr lines: {}", self.stderr));
        aterm_json::from_str::<Value>(line)
            .expect("the error line")
            .get("error")
            .cloned()
            .expect("an error object")
    }

    /// The receipt line's `key=value` pairs.
    fn receipt(&self) -> Vec<(String, String)> {
        let line = self
            .stderr
            .lines()
            .find(|line| line.starts_with("agh: ok"))
            .unwrap_or_else(|| panic!("a receipt: {}", self.stderr));
        line.trim_start_matches("agh: ok ")
            .split(' ')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn note(&self, key: &str) -> Option<String> {
        self.receipt()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// Exit `exit`, an error of `kind` (and `sub` when given), nothing on stdout.
    fn failed(&self, exit: u8, kind: &str, sub: Option<&str>) -> Value {
        assert_eq!(self.exit, exit, "stderr {}", self.stderr);
        assert!(self.stdout.is_empty(), "stdout {:?}", self.text());
        let error = self.error();
        assert_eq!(error["kind"], Value::from(kind), "{error}");
        assert_eq!(error["exit"], Value::from(exit), "{error}");
        assert_eq!(
            error["sub"],
            sub.map_or(Value::Null, Value::from),
            "{error}"
        );
        error
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
        fake.add_repo(APP, RepoSpec::public().push("fixture-dev-bot"));
        fake.add_repo(PULLED, RepoSpec::public().pull("fixture-dev-bot"));
        fake.add_repo(SECRET, RepoSpec::private().push("fixture-dev-bot"));
        World { root, fake }
    }

    fn run(&self, args: &[&str]) -> Ran {
        self.run_at(args, false)
    }

    /// A run whose stdout is a terminal when `terminal`.
    fn run_at(&self, args: &[&str], terminal: bool) -> Ran {
        let (ctx, captured) = common::ctx(self.root.path(), &self.fake, common::env());
        let mut ctx = ctx.with_terminals(terminal, false);
        let exit = common::run_cli(&mut ctx, args);
        Ran {
            exit,
            stdout: captured.stdout(),
            stderr: captured.stderr_text(),
        }
    }

    /// Requests seen whose path and query start with `prefix`.
    fn seen_under(&self, prefix: &str) -> Vec<agh_fake::Seen> {
        self.fake
            .seen()
            .into_iter()
            .filter(|s| s.path_and_query.starts_with(prefix))
            .collect()
    }

    fn files(&self) {
        for path in [
            "README.md",
            "Makefile",
            "src/lib.rs",
            "src/main.rs",
            "docs/guide.md",
        ] {
            self.fake.put_file(APP, "main", path, path.as_bytes());
        }
    }
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value
        .as_array()
        .expect("an array")
        .iter()
        .map(|item| item[key].as_str().expect("a string").to_string())
        .collect()
}

// ---------------------------------------------------------------- every usage error of plan §10.2

#[test]
fn every_usage_error_is_exit_2_with_the_verb_s_help() {
    let w = World::new("cli-usage");
    let dir = w.root.path().join("d");
    std::fs::create_dir(&dir).expect("a directory");
    let dir = dir.display().to_string();
    let missing = w.root.path().join("missing").display().to_string();
    let file = w.root.path().join("f").display().to_string();
    let part_file = w.root.path().join("f.agh-part").display().to_string();
    // The api origin's base with one more digit on its port: a prefix of it, another origin.
    let lookalike = format!(
        "http://127.0.0.1:{}0/repos/fixture-dev/app",
        w.fake.ports().api
    );
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["cat", APP], "cat"),
        (
            vec!["cat", "fixture-dev/app:README.md", "--max-bytes", "0"],
            "cat",
        ),
        (
            vec!["cat", "fixture-dev/app:README.md", "--max-bytes", "many"],
            "cat",
        ),
        (vec!["find", "fixture-dev/app:docs"], "find"),
        (vec!["find", APP, "--type", "file"], "find"),
        (vec!["find", APP, "--match", "("], "find"),
        (vec!["rev", APP], "rev"),
        (vec!["rev", "fixture-dev/app@main:README.md"], "rev"),
        (vec!["log", "fixture-dev/app:src"], "log"),
        (vec!["log", APP, "--since", "yesterday"], "log"),
        (vec!["compare", APP, "main..dev"], "compare"),
        (vec!["compare", APP, "...dev"], "compare"),
        (
            vec!["repo", "list", "fixture-dev", "--type", "mine"],
            "repo list",
        ),
        (vec!["repo", "list", "fixture_dev"], "repo list"),
        (
            vec!["release", "list", APP, "--prerelease", "--no-prerelease"],
            "release list",
        ),
        (
            vec!["release", "list", APP, "--tag-regex", "("],
            "release list",
        ),
        (
            vec!["release", "newest", APP, "--tag-regex", "("],
            "release newest",
        ),
        (vec!["release", "view", APP], "release view"),
        (
            vec!["release", "view", APP, "v1", "--id", "5000"],
            "release view",
        ),
        (vec!["release", "view", APP, "--id", "v1"], "release view"),
        (vec!["release", "view", APP, "v1/../v2"], "release view"),
        (vec!["release", "assets", APP], "release assets"),
        (vec!["release", "download", APP, "v1"], "release download"),
        (
            vec![
                "release", "download", APP, "v1", "--asset", "a", "--asset", "a",
            ],
            "release download",
        ),
        (
            vec![
                "release", "download", APP, "v1", "--asset", "a", "--dir", &dir, "--output", &file,
            ],
            "release download",
        ),
        (
            vec![
                "release", "download", APP, "v1", "--asset", "a", "--asset", "b", "--output", &file,
            ],
            "release download",
        ),
        (
            vec![
                "release", "download", APP, "v1", "--asset", "a", "--dir", &missing,
            ],
            "release download",
        ),
        (vec!["can", APP, "write"], "can"),
        (vec!["api", "/repos/fixture-dev/app", "-X", "POST"], "api"),
        (vec!["api", "/repos/fixture-dev/app", "-X", "delete"], "api"),
        (vec!["api", "/repos/fixture-dev/app", "-X", "TRACE"], "api"),
        (
            vec!["api", "https://example.invalid/repos/fixture-dev/app"],
            "api",
        ),
        (vec!["api", "/search/code", "-F", "q=@query.txt"], "api"),
        (vec!["api", "/search/code", "-f", "q"], "api"),
        (vec!["api", "/repos/fixture-dev/app", "-R", PULLED], "api"),
        (vec!["api", "/repositories/1000"], "api"),
        // PULLED is repository 1001: the path's id is not -R's.
        (vec!["api", "/repositories/1001", "-R", APP], "api"),
        (vec!["api", "/repositories/app", "-R", APP], "api"),
        (vec!["api", &lookalike], "api"),
        (vec!["api", "/repos/fixture-dev/app/../secret"], "api"),
        (vec!["api", "/repos/fixture-dev/app/%2e%2E/secret"], "api"),
        (
            vec!["api", "/repos/fixture-dev/app", "-X", "HEAD", "--all"],
            "api",
        ),
        (
            vec![
                "api",
                "/repos/fixture-dev/app/releases",
                "--all",
                "--include",
            ],
            "api",
        ),
        // `x.agh-part` is the part file of `x`, which the download of `x` removes first.
        (
            vec![
                "release",
                "download",
                APP,
                "v1",
                "--asset",
                "x.agh-part",
                "--asset",
                "x",
                "--dir",
                &dir,
            ],
            "release download",
        ),
        (
            vec![
                "release", "download", APP, "v1", "--asset", "x", "--output", &part_file,
            ],
            "release download",
        ),
        // An `--output` that names no file, as a separate value or after `=`.
        (
            vec![
                "release", "download", APP, "v1", "--asset", "x", "--output", "",
            ],
            "release download",
        ),
        (
            vec![
                "release",
                "download",
                APP,
                "v1",
                "--asset",
                "x",
                "--output=",
            ],
            "release download",
        ),
        (
            vec![
                "release", "download", APP, "v1", "--asset", "x", "--output", ".",
            ],
            "release download",
        ),
    ];
    for (args, verb) in cases {
        let ran = w.run(&args);
        let error = ran.failed(2, "usage", None);
        assert_eq!(
            error["next"],
            Value::from(format!("agh help {verb}")),
            "{args:?}: {error}"
        );
    }
    let terminal = w.run_at(
        &[
            "release", "download", APP, "v1", "--asset", "a", "--output", "-",
        ],
        true,
    );
    terminal.failed(2, "usage", None);
    w.fake.assert_clean();
}

#[test]
fn a_hidden_dot_segment_or_an_empty_tag_part_is_usage_with_nothing_sent() {
    let w = World::new("cli-hidden-dots");
    w.files();
    let cases: Vec<Vec<&str>> = vec![
        // A `..` behind an encoded `/`, which a front end that decodes `%2F` before it removes
        // dot-segments would resolve onto another path than the one routed.
        vec!["api", "user/..%2Fusers%2Fx"],
        vec!["api", "/repos/fixture-dev/app/..%2F..%2Fx"],
        vec!["api", "/repos/fixture-dev/app/..%5C..%5Cx"],
        // A tag with an empty part, which a slash-merging server reads as another tag, on
        // both of `view`'s branches.
        vec!["release", "view", APP, "a//b"],
        vec!["release", "view", APP, "v1/"],
        vec!["release", "assets", APP, "a//b"],
        vec!["release", "view", APP, "a//b", "--include-drafts"],
        vec!["release", "view", APP, "v1/../v2", "--include-drafts"],
    ];
    for args in cases {
        w.run(&args).failed(2, "usage", None);
        assert!(w.fake.seen().is_empty(), "{args:?}: {:?}", w.fake.seen());
    }
    // An encoded `/` that hides no dot-segment is no refusal.
    let fine = w.run(&["api", "/repos/fixture-dev/app/contents/a%2Fb.md"]);
    assert_ne!(fine.exit, 2, "{}", fine.stderr);
    w.fake.assert_clean();
}

// ---------------------------------------------------------------- content

#[test]
fn cat_prints_the_bytes_and_judges_absence() {
    let w = World::new("cli-cat");
    w.files();
    w.fake.put_file(SECRET, "main", "README.md", b"secret");
    let ran = w.run(&["cat", "fixture-dev/app:README.md"]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, b"README.md");

    let missing = w.run(&["cat", "fixture-dev/app:nothing.md"]);
    let error = missing.failed(6, "not_found", None);
    assert_eq!(error["tried"].as_array().map(Vec::len), Some(2), "{error}");

    let hidden = w.run(&["cat", "fixture-dev/secret:README.md", "--anonymous"]);
    hidden.failed(7, "not_visible", None);
    w.fake.assert_clean();
}

#[test]
fn ls_projects_names_as_tsv() {
    let w = World::new("cli-ls");
    w.files();
    let ran = w.run(&["ls", APP, "--fields", "name", "--format", "tsv"]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(ran.text(), "Makefile\nREADME.md\ndocs\nsrc\n");
    w.fake.assert_clean();
}

#[test]
fn find_filters_and_a_truncated_tree_needs_partial() {
    let w = World::new("cli-find");
    w.files();
    let ran = w.run(&["find", APP, "src", "--type", "blob", "--match", "lib"]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(strings(&ran.json(), "path"), ["src/lib.rs"]);
    let trees = w.run(&["find", APP, "--type", "tree"]);
    assert_eq!(strings(&trees.json(), "path"), ["docs", "src"]);
    let limited = w.run(&["find", APP, "--limit", "2"]);
    assert_eq!(strings(&limited.json(), "path"), ["Makefile", "README.md"]);

    w.fake.set_tree_truncated(APP, true);
    let truncated = w.run(&["find", APP]);
    truncated.failed(14, "incomplete", Some("truncated_tree"));
    let partial = w.run(&["find", APP, "--partial"]);
    assert_eq!(partial.exit, 0, "{}", partial.stderr);
    assert_eq!(strings(&partial.json(), "path").len(), 7);
    assert_eq!(partial.note("complete").as_deref(), Some("0"));
    w.fake.assert_clean();
}

// ---------------------------------------------------------------- history

#[test]
fn compare_of_unrelated_refs_is_exit_0_unrelated() {
    let w = World::new("cli-compare");
    w.fake.add_commit(
        APP,
        CommitSpec {
            sha: "c1".into(),
            message: "c1".into(),
            author_login: None,
            author_name: "someone".into(),
            date: "2026-01-01T00:00:00Z".into(),
            paths: vec!["README.md".into()],
        },
    );
    w.fake
        .set_compare(APP, "main", "orphan", CompareSpec::NoCommonAncestor);
    let ran = w.run(&["compare", APP, "main...orphan"]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    let value = ran.json();
    assert_eq!(value["status"], Value::from("unrelated"));
    assert_eq!(value["base"], Value::from("main"));
    assert_eq!(value["head"], Value::from("orphan"));
    assert_eq!(value["merge_base"], Value::Null);
    assert_eq!(value["ahead_by"], Value::from(0u64));
    assert_eq!(value["behind_by"], Value::from(0u64));
    assert_eq!(value["total_commits"], Value::from(0u64));
    w.fake.assert_clean();
}

// ---------------------------------------------------------------- releases

#[test]
fn release_list_reads_latest_only_when_projected_and_drafts_need_push() {
    let w = World::new("cli-release-list");
    w.fake
        .add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    let v2 = w
        .fake
        .add_release(APP, ReleaseSpec::published("v2", "2026-02-01T00:00:00Z"));
    w.fake
        .add_release(PULLED, ReleaseSpec::draft("v1", "2026-01-01T00:00:00Z"));
    let latest_path = format!("/repos/{APP}/releases/latest");

    let plain = w.run(&["release", "list", APP, "--fields", "tag_name"]);
    assert_eq!(plain.exit, 0, "{}", plain.stderr);
    assert_eq!(strings(&plain.json(), "tag_name"), ["v2", "v1"]);
    assert!(w.seen_under(&latest_path).is_empty());

    let with_latest = w.run(&["release", "list", APP, "--fields", "tag_name,latest,repo"]);
    assert_eq!(with_latest.exit, 0, "{}", with_latest.stderr);
    let value = with_latest.json();
    assert_eq!(value[0]["latest"], Value::Bool(true));
    assert_eq!(value[1]["latest"], Value::Bool(false));
    assert_eq!(value[0]["repo"], Value::from(APP));
    assert_eq!(w.seen_under(&latest_path).len(), 1);

    let viewed = w.run(&["release", "view", APP, "v2", "--fields", "id,latest"]);
    assert_eq!(viewed.json()["id"], Value::from(v2));
    assert_eq!(viewed.json()["latest"], Value::Bool(true));

    let drafts = w.run(&["release", "list", PULLED, "--drafts"]);
    drafts.failed(5, "forbidden", Some("forbid.no_push"));
    w.fake.assert_clean();
}

#[test]
fn a_writer_outside_the_read_chain_still_reads_latest() {
    // alabsystems reads through dev and anonymous and writes as release: `--as release` names
    // the writer, who reads the latest pointer too (a WriterRead, where release is at home).
    let w = World::new("cli-release-writer");
    let channel = "alabsystems/aterm";
    w.fake
        .add_repo(channel, RepoSpec::public().push("fixture-release-bot"));
    let published = w.fake.add_release(
        channel,
        ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"),
    );
    let draft = w
        .fake
        .add_release(channel, ReleaseSpec::draft("v2", "2026-02-01T00:00:00Z"));
    let viewed = w.run(&[
        "release",
        "view",
        channel,
        "v2",
        "--include-drafts",
        "--as",
        "release",
        "--fields",
        "id,latest",
    ]);
    assert_eq!(viewed.exit, 0, "{}", viewed.stderr);
    assert_eq!(viewed.json()["id"], Value::from(draft));
    assert_eq!(viewed.json()["latest"], Value::Bool(false));

    let listed = w.run(&[
        "release",
        "list",
        channel,
        "--drafts",
        "--as",
        "release",
        "--fields",
        "id,latest",
    ]);
    assert_eq!(listed.exit, 0, "{}", listed.stderr);
    let value = listed.json();
    assert_eq!(value[0]["id"], Value::from(draft));
    assert_eq!(value[0]["latest"], Value::Bool(false));
    assert_eq!(value[1]["id"], Value::from(published));
    assert_eq!(value[1]["latest"], Value::Bool(true));
    w.fake.assert_clean();
}

#[test]
fn a_private_repository_s_latest_is_read_as_the_writer() {
    // Only the release bot sees this private repository; its read chain's dev bot gets a 404.
    let w = World::new("cli-release-private-latest");
    let channel = "alabsystems/hidden";
    w.fake
        .add_repo(channel, RepoSpec::private().push("fixture-release-bot"));
    let published = w.fake.add_release(
        channel,
        ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"),
    );
    let draft = w
        .fake
        .add_release(channel, ReleaseSpec::draft("v2", "2026-02-01T00:00:00Z"));
    let latest_path = format!("/repos/{channel}/releases/latest");
    let listed = w.run(&[
        "release",
        "list",
        channel,
        "--drafts",
        "--as",
        "release",
        "--fields",
        "id,latest",
    ]);
    assert_eq!(listed.exit, 0, "{}", listed.stderr);
    let value = listed.json();
    assert_eq!(value[0]["id"], Value::from(draft));
    assert_eq!(value[0]["latest"], Value::Bool(false));
    assert_eq!(value[1]["id"], Value::from(published));
    assert_eq!(value[1]["latest"], Value::Bool(true));
    let latest = w.seen_under(&latest_path);
    assert_eq!(latest.len(), 1, "{latest:?}");
    assert_eq!(latest[0].as_login.as_deref(), Some("fixture-release-bot"));

    let viewed = w.run(&[
        "release",
        "view",
        channel,
        "v1",
        "--include-drafts",
        "--as",
        "release",
    ]);
    assert_eq!(viewed.exit, 0, "{}", viewed.stderr);
    assert_eq!(viewed.json()["latest"], Value::Bool(true));
    w.fake.assert_clean();
}

#[test]
fn release_view_of_a_tag_two_drafts_share_is_ambiguous() {
    let w = World::new("cli-ambiguous");
    w.fake
        .add_release(APP, ReleaseSpec::draft("v1", "2026-01-01T00:00:00Z"));
    w.fake
        .add_release(APP, ReleaseSpec::draft("v1", "2026-01-02T00:00:00Z"));
    let ran = w.run(&["release", "view", APP, "v1", "--include-drafts"]);
    let error = ran.failed(9, "conflict", Some("ambiguous_tag"));
    assert!(
        error["next"]
            .as_str()
            .is_some_and(|next| next.contains("--id")),
        "{error}"
    );
    w.fake.assert_clean();
}

#[test]
fn release_download_writes_files_or_stdout() {
    let w = World::new("cli-download");
    let release = w
        .fake
        .add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.fake
        .add_asset(release, AssetSpec::uploaded("a.bin", b"first asset"));
    w.fake
        .add_asset(release, AssetSpec::uploaded("b.bin", b"second asset"));
    let dir = w.root.path().join("out");
    std::fs::create_dir(&dir).expect("the directory");
    let dir_text = dir.display().to_string();

    let ran = w.run(&[
        "release", "download", APP, "v1", "--asset", "a.bin", "--asset", "b.bin", "--dir",
        &dir_text, "--meta",
    ]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(
        std::fs::read(dir.join("a.bin")).expect("a.bin"),
        b"first asset"
    );
    assert_eq!(
        std::fs::read(dir.join("b.bin")).expect("b.bin"),
        b"second asset"
    );
    let rows = ran.json();
    assert_eq!(
        rows.as_array()
            .expect("rows")
            .iter()
            .map(|row| (row["bytes"].as_u64(), row["written"].as_bool()))
            .collect::<Vec<_>>(),
        [(Some(11), Some(true)), (Some(12), Some(true))]
    );
    assert_eq!(ran.note("files").as_deref(), Some("2"));

    let one = w.run(&[
        "release", "download", APP, "v1", "--asset", "a.bin", "--dir", &dir_text, "--meta",
    ]);
    assert_eq!(one.exit, 0, "{}", one.stderr);
    assert_eq!(one.note("written").as_deref(), Some("0"));
    assert!(one.note("digest").is_some_and(|d| d.starts_with("sha256:")));

    let stdout = w.run(&[
        "release", "download", APP, "v1", "--asset", "b.bin", "--output", "-",
    ]);
    assert_eq!(stdout.exit, 0, "{}", stdout.stderr);
    assert_eq!(stdout.stdout, b"second asset");

    let absent = w.run(&[
        "release", "download", APP, "v1", "--asset", "a.bin", "--asset", "c.bin", "--dir",
        &dir_text,
    ]);
    absent.failed(6, "not_found", None);
    assert!(!Path::new(&dir).join("c.bin").exists());
    w.fake.assert_clean();
}

// ---------------------------------------------------------------- can

#[test]
fn can_is_exit_5_when_not_granted_and_granted_otherwise() {
    let w = World::new("cli-can");
    let refused = w.run(&["can", PULLED, "push"]);
    refused.failed(5, "forbidden", Some("forbid.permission"));
    let granted = w.run(&["can", APP, "push"]);
    assert_eq!(granted.exit, 0, "{}", granted.stderr);
    let value = granted.json();
    assert_eq!(value["granted"], Value::Bool(true));
    assert_eq!(value["op"], Value::from("push"));
    assert_eq!(value["repo"], Value::from(APP));
    let pull = w.run(&["can", PULLED, "pull"]);
    assert_eq!(pull.exit, 0, "{}", pull.stderr);
    assert_eq!(pull.json()["permissions"]["pull"], Value::Bool(true));
    w.fake.assert_clean();
}

// ---------------------------------------------------------------- api

#[test]
fn api_routes_each_path_class() {
    let w = World::new("cli-api-route");
    let login_of = |args: &[&str], path: &str| -> Option<String> {
        w.fake.clear_seen();
        let ran = w.run(args);
        assert_eq!(ran.exit, 0, "{args:?}: {}", ran.stderr);
        let seen = w.seen_under(path);
        assert_eq!(seen.len(), 1, "{args:?}");
        seen[0].as_login.clone()
    };
    let dev = Some("fixture-dev-bot".to_string());
    assert_eq!(
        login_of(&["api", "/repos/fixture-dev/app"], "/repos/fixture-dev/app"),
        dev
    );
    assert_eq!(
        login_of(&["api", "repos/fixture-dev/app"], "/repos/fixture-dev/app"),
        dev
    );
    assert_eq!(
        login_of(&["api", "/users/fixture-dev"], "/users/fixture-dev"),
        dev
    );
    assert_eq!(login_of(&["api", "/rate_limit"], "/rate_limit"), dev);
    assert_eq!(
        login_of(&["api", "/user", "--as", "release"], "/user"),
        Some("fixture-release-bot".to_string())
    );
    assert_eq!(
        login_of(
            &["api", "/repos/fixture-dev/app", "--anonymous"],
            "/repos/fixture-dev/app"
        ),
        None
    );
    assert_eq!(
        login_of(
            &["api", "/repositories/1000", "-R", APP],
            "/repositories/1000"
        ),
        dev
    );
    w.fake.assert_clean();
}

#[test]
fn api_reads_a_by_id_user_path_through_the_read_chain_never_as_a_release_profile() {
    // `/user/{account_id}/repos` (GitHub's next link for `/users/{login}/repos`) names
    // fixture-dev, a dev-tier owner, by id: the release token never goes there.
    let w = World::new("cli-api-user-id");
    let owner = w.run(&["api", "/users/fixture-dev"]);
    assert_eq!(owner.exit, 0, "{}", owner.stderr);
    let id = owner.json()["id"].as_u64().expect("an owner id");
    let path = format!("/user/{id}/repos");

    w.fake.clear_seen();
    let refused = w.run(&["api", &path, "--as", "release"]);
    refused.failed(1, "refused", Some("policy.as_not_in_chain"));
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());

    let read = w.run(&["api", &path]);
    assert_eq!(read.exit, 0, "{}", read.stderr);
    let seen = w.seen_under(&path);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].as_login.as_deref(), Some("fixture-dev-bot"));
    w.fake.assert_clean();
}

#[test]
fn api_include_all_and_the_path_s_query() {
    let w = World::new("cli-api");
    for (i, day) in ["01", "02", "03"].iter().enumerate() {
        w.fake.add_release(
            APP,
            ReleaseSpec::published(&format!("v{i}"), &format!("2026-01-{day}T00:00:00Z")),
        );
    }
    let included = w.run(&["api", "/repos/fixture-dev/app", "--include"]);
    assert_eq!(included.exit, 0, "{}", included.stderr);
    let value = included.json();
    let keys: Vec<&String> = value.as_object().expect("an object").keys().collect();
    assert_eq!(keys, ["body", "headers", "status"]);
    assert_eq!(value["status"], Value::from(200u64));
    assert_eq!(value["body"]["full_name"], Value::from(APP));
    assert!(
        value["headers"]["content-type"].as_str().is_some(),
        "{value}"
    );

    let objects = w.run(&["api", "/repos/fixture-dev/app", "--all"]);
    objects.failed(2, "usage", None);
    let ndjson = w.run(&[
        "api",
        "/repos/fixture-dev/app",
        "--all",
        "--format",
        "ndjson",
    ]);
    assert_eq!(ndjson.exit, 0, "{}", ndjson.stderr);
    assert_eq!(ndjson.text().lines().count(), 1);

    w.fake.clear_seen();
    let paged = w.run(&[
        "api",
        "/repos/fixture-dev/app/releases?per_page=2",
        "-f",
        "page=1",
    ]);
    assert_eq!(paged.exit, 0, "{}", paged.stderr);
    assert_eq!(paged.json().as_array().map(Vec::len), Some(2));
    let sent: Vec<String> = w
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.origin == FakeOrigin::Api)
        .map(|s| s.path_and_query)
        .collect();
    assert_eq!(sent, [format!("/repos/{APP}/releases?per_page=2&page=1")]);

    let all = w.run(&["api", "/repos/fixture-dev/app/releases?per_page=2", "--all"]);
    assert_eq!(all.exit, 0, "{}", all.stderr);
    assert_eq!(all.json().as_array().map(Vec::len), Some(3));

    let head = w.run(&["api", "/repos/fixture-dev/app", "-X", "head"]);
    assert_eq!(head.exit, 0, "{}", head.stderr);
    assert!(head.stdout.is_empty());
    w.fake.assert_clean();
}

#[test]
fn api_head_with_include_answers_the_status_and_headers() {
    let w = World::new("cli-api-head");
    let ran = w.run(&["api", "/repos/fixture-dev/app", "-X", "HEAD", "--include"]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    let value = ran.json();
    assert_eq!(value["status"], Value::from(200u64));
    assert!(
        value["headers"]["content-type"].as_str().is_some(),
        "{value}"
    );
    assert_eq!(value["body"], Value::from(""));
    w.fake.assert_clean();
}

#[test]
fn api_reads_an_absolute_url_on_the_api_origin() {
    let w = World::new("cli-api-absolute");
    let url = format!(
        "http://127.0.0.1:{}/repos/fixture-dev/app",
        w.fake.ports().api
    );
    let ran = w.run(&["api", &url]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(ran.json()["full_name"], Value::from(APP));
    w.fake.assert_clean();
}

#[test]
fn api_reads_a_query_s_plus_as_a_space() {
    let w = World::new("cli-api-plus");
    w.fake.clear_seen();
    let ran = w.run(&[
        "api",
        "/repos/fixture-dev/app/releases?q=a+b%2Bc&per_page=2",
    ]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    let sent: Vec<String> = w
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.origin == FakeOrigin::Api)
        .map(|s| s.path_and_query)
        .collect();
    assert_eq!(
        sent,
        [format!("/repos/{APP}/releases?q=a%20b%2Bc&per_page=2")]
    );
    w.fake.assert_clean();
}

#[test]
fn api_all_holds_limit_to_items_and_a_partial_cap_is_incomplete() {
    let w = World::new("cli-api-limit");
    for (i, day) in ["01", "02", "03"].iter().enumerate() {
        w.fake.add_release(
            APP,
            ReleaseSpec::published(&format!("v{i}"), &format!("2026-01-{day}T00:00:00Z")),
        );
    }
    let path = "/repos/fixture-dev/app/releases?per_page=2";
    let limited = w.run(&["api", path, "--all", "--limit", "1", "--meta"]);
    assert_eq!(limited.exit, 0, "{}", limited.stderr);
    assert_eq!(limited.json().as_array().map(Vec::len), Some(1));
    assert_eq!(limited.note("limit").as_deref(), Some("1"));

    let one_call = w.run(&["api", path, "--limit", "1", "--meta"]);
    assert_eq!(one_call.exit, 0, "{}", one_call.stderr);
    assert_eq!(one_call.json().as_array().map(Vec::len), Some(1));
    assert_eq!(one_call.note("limit").as_deref(), Some("1"));

    let capped = w.run(&["api", path, "--all", "--max-pages", "1"]);
    capped.failed(14, "incomplete", Some("capped"));
    let partial = w.run(&[
        "api",
        path,
        "--all",
        "--partial",
        "--max-pages",
        "1",
        "--meta",
    ]);
    assert_eq!(partial.exit, 0, "{}", partial.stderr);
    assert_eq!(partial.json().as_array().map(Vec::len), Some(2));
    assert_eq!(partial.note("complete").as_deref(), Some("0"));
    w.fake.assert_clean();
}

// ---------------------------------------------------------------- stream, limit and route edges

/// stdout whose reader goes away after its first write (`| head -c N`).
struct GoneAfterFirst {
    took: usize,
}

impl std::io::Write for GoneAfterFirst {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.took > 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        }
        self.took = buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn release_download_to_a_reader_that_went_away_is_exit_0_and_quiet() {
    let w = World::new("cli-download-gone");
    let release = w
        .fake
        .add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    // Larger than one 64 KiB chunk, so a second write meets the closed pipe.
    let big = vec![b'x'; 200 * 1024];
    w.fake
        .add_asset(release, AssetSpec::uploaded("big.bin", &big));
    w.fake
        .add_asset(release, AssetSpec::uploaded("next.bin", b"next"));
    let (ctx, captured) = common::ctx(w.root.path(), &w.fake, common::env());
    let mut ctx = ctx.with_terminals(false, false);
    ctx.stdout = Box::new(GoneAfterFirst { took: 0 });
    let exit = common::run_cli(
        &mut ctx,
        &[
            "release", "download", APP, "v1", "--asset", "big.bin", "--asset", "next.bin",
            "--output", "-",
        ],
    );
    let stderr = captured.stderr_text();
    assert_eq!(exit, 0, "{stderr}");
    assert_eq!(stderr, "");
    w.fake.assert_clean();
}

#[test]
fn an_asset_named_like_a_part_file_is_usage_under_dir_only() {
    let w = World::new("cli-download-part");
    let release = w
        .fake
        .add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.fake
        .add_asset(release, AssetSpec::uploaded("x.agh-part", b"first"));
    w.fake
        .add_asset(release, AssetSpec::uploaded("x", b"second"));
    let dir = w.root.path().join("out");
    std::fs::create_dir(&dir).expect("the directory");
    let dir_text = dir.display().to_string();
    w.fake.clear_seen();
    let ran = w.run(&[
        "release",
        "download",
        APP,
        "v1",
        "--asset",
        "x.agh-part",
        "--asset",
        "x",
        "--dir",
        &dir_text,
    ]);
    ran.failed(2, "usage", None);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert!(!dir.join("x").exists());
    // Only `--dir` makes the asset's name a file's: to stdout, or to a file the caller
    // names, such an asset downloads.
    let ran = w.run(&[
        "release",
        "download",
        APP,
        "v1",
        "--asset",
        "x.agh-part",
        "--output",
        "-",
    ]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, b"first");
    let named = dir.join("named");
    let named_text = named.display().to_string();
    let ran = w.run(&[
        "release",
        "download",
        APP,
        "v1",
        "--asset",
        "x.agh-part",
        "--output",
        &named_text,
    ]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(std::fs::read(&named).expect("the file"), b"first");
    w.fake.assert_clean();
}

#[test]
fn release_list_capped_under_partial_stays_incomplete_when_limited() {
    let w = World::new("cli-release-cap-limit");
    // 101 releases: one more than a page.
    for i in 0..101u32 {
        let day = i % 28 + 1;
        let month = i / 28 + 1;
        w.fake.add_release(
            APP,
            ReleaseSpec::published(
                &format!("v{i}"),
                &format!("2026-{month:02}-{day:02}T00:00:00Z"),
            ),
        );
    }
    let ran = w.run(&[
        "release",
        "list",
        APP,
        "--max-pages",
        "1",
        "--partial",
        "--limit",
        "1",
    ]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(ran.json().as_array().map(Vec::len), Some(1));
    assert_eq!(ran.note("complete").as_deref(), Some("0"), "{}", ran.stderr);
    assert!(ran.note("partial").is_some(), "{}", ran.stderr);
    assert_eq!(ran.note("limit"), None, "{}", ran.stderr);
    w.fake.assert_clean();
}

#[test]
fn ls_holds_its_limit() {
    let w = World::new("cli-ls-limit");
    w.files();
    let ran = w.run(&["ls", APP, "--limit", "1", "--meta"]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(strings(&ran.json(), "name"), ["Makefile"]);
    assert_eq!(ran.note("limit").as_deref(), Some("1"));
    w.fake.assert_clean();
}

#[test]
fn api_prints_a_raw_file_s_bytes_even_when_they_parse() {
    let w = World::new("cli-api-raw-file");
    let body = "{\"b\": 1}\n";
    let labelled = || Fault::Status {
        status: 200,
        body: body.to_string(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/vnd.github.raw+json; charset=utf-8".to_string(),
        )],
    };
    let file = format!("/repos/{APP}/contents/F.json");
    w.fake.fault(Match::api(&file), labelled());
    let ran = w.run(&["api", &file, "--accept", "application/vnd.github.raw+json"]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(ran.text(), body);

    // Elsewhere GitHub labels JSON objects `raw+json` too: those stay JSON.
    let issue = format!("/repos/{APP}/issues/1");
    w.fake.fault(Match::api(&issue), labelled());
    let ran = w.run(&["api", &issue, "--accept", "application/vnd.github.raw+json"]);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(ran.json()["b"], Value::from(1u64));
    w.fake.assert_clean();
}

#[test]
fn api_judges_the_route_on_the_path_a_fragment_leaves() {
    // A `#fragment` is never sent, so the route is judged on the path GitHub receives.
    let root = common::scratch("cli-api-fragment");
    let fake = FakeGithub::start();
    common::canonical_tokens(root.path(), &fake, true, true);
    let text = common::canonical_config(root.path()).replacen(
        "[default]",
        "[[owners]]\nname = \"fixture-anon\"\ntier = \"dev\"\nread = [\"anonymous\"]\nwrite = \"dev\"\n\n[default]",
        1,
    );
    common::write_config(&Dirs::under(root.path()), &text);
    fake.add_repo(APP, RepoSpec::public().push("fixture-dev-bot"));
    fake.add_repo(PULLED, RepoSpec::public().pull("fixture-dev-bot"));
    fake.add_repo("fixture-anon/app", RepoSpec::public());
    let w = World { root, fake };

    // `/user#` is the Identity path `/user`: refused without `--as`, nothing sent.
    let identity = w.run(&["api", "/user#"]);
    identity.failed(2, "usage", Some("policy.identity_needs_as"));
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());

    // `-R` is held to the path's repository with its fragment cut.
    let other = w.run(&["api", "/repos/fixture-dev/app#", "-R", PULLED]);
    other.failed(2, "usage", None);
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());

    // An owner whose read chain is anonymous only still reads with no credential.
    let anon = w.run(&["api", "/repos/fixture-anon/app#x"]);
    assert_eq!(anon.exit, 0, "{}", anon.stderr);
    let seen = w.fake.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].path_and_query, "/repos/fixture-anon/app");
    assert!(!seen[0].had_authorization, "{seen:?}");
    w.fake.assert_clean();
}

#[test]
fn api_honours_no_probe() {
    let w = World::new("cli-api-no-probe");
    w.files();
    let path = format!("/repos/{APP}/contents/nothing");
    let unprobed = w.run(&["api", &path, "--no-probe"]);
    unprobed.failed(7, "not_visible", None);
    assert_eq!(w.fake.seen().len(), 1, "{:?}", w.fake.seen());

    w.fake.clear_seen();
    let probed = w.run(&["api", &path]);
    probed.failed(6, "not_found", None);
    assert_eq!(w.fake.seen().len(), 2, "{:?}", w.fake.seen());
    w.fake.assert_clean();
}

#[test]
fn raw_get_refuses_a_fragment_before_any_request() {
    // A library caller's `#` would be cut by the wire: the route would be judged for one path
    // and another sent, so `Raw::get` refuses it.
    let w = World::new("cli-raw-fragment");
    let router = common::router(&common::canonical_config(w.root.path()), w.root.path());
    let gh = common::github(router, &w.fake);
    let request = agh::github::RawGet {
        method: agh::wire::Method::Get,
        path: "/rate_limit#x".to_string(),
        query: Vec::new(),
        accept: None,
        route: agh::policy::RouteRequest::read(agh::policy::RouteTarget::Unscoped),
        all: false,
        include: false,
        opts: agh::page::ListOpts::STANDARD,
    };
    let error = gh.raw().get(&request).expect_err("a fragment");
    assert_eq!(error.kind, agh::Kind::Usage);
    // A dot-segment, typed or encoded, in the path or in `all`'s owner-id lookup, likewise.
    for (path, all) in [
        ("/users/../repos", false),
        ("/users/%2e%2e/repos", true),
        ("/orgs/./repos", true),
    ] {
        let request = agh::github::RawGet {
            path: path.to_string(),
            all,
            ..request.clone()
        };
        let error = gh.raw().get(&request).expect_err("a dot-segment");
        assert_eq!(error.kind, agh::Kind::Usage, "{path}");
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    w.fake.assert_clean();
}

#[test]
fn a_download_destination_outside_dir_or_not_a_file_is_usage_with_nothing_sent() {
    let w = World::new("cli-download-dest");
    let release = w
        .fake
        .add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.fake
        .add_asset(release, AssetSpec::uploaded("x", b"bytes"));
    let dir = w.root.path().join("out");
    std::fs::create_dir(&dir).expect("the directory");
    let dir_text = dir.display().to_string();
    let escape = w.root.path().join("escaped").display().to_string();
    let cases: Vec<Vec<&str>> = vec![
        // `--output` naming an existing directory.
        vec!["--asset", "x", "--output", &dir_text],
        // Names `dir.join` would carry outside `--dir`.
        vec!["--asset", "../x", "--dir", &dir_text],
        vec!["--asset", "a/x", "--dir", &dir_text],
        vec!["--asset", &escape, "--dir", &dir_text],
        vec!["--asset", "x/", "--dir", &dir_text],
    ];
    for case in cases {
        w.fake.clear_seen();
        let mut args = vec!["release", "download", APP, "v1"];
        args.extend(case.iter().copied());
        let ran = w.run(&args);
        let error = ran.failed(2, "usage", None);
        assert_eq!(
            error["next"],
            Value::from("agh help release download"),
            "{case:?}"
        );
        assert!(w.fake.seen().is_empty(), "{case:?}: {:?}", w.fake.seen());
    }
    // A directory where `--dir`'s file would go.
    std::fs::create_dir(dir.join("x")).expect("a directory where x would go");
    w.fake.clear_seen();
    let ran = w.run(&[
        "release", "download", APP, "v1", "--asset", "x", "--dir", &dir_text,
    ]);
    let error = ran.failed(2, "usage", None);
    assert_eq!(error["next"], Value::from("agh help release download"));
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    assert!(!w.root.path().join("escaped").exists());
    w.fake.assert_clean();
}
