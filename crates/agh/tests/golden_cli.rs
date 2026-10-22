// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Scenarios that cross packages, as sequences of `run_cli` calls on one fake: a
//! provisioning read as the routed identity, a token that disappears, a wait then two
//! downloads, and jq over pages. Each step asserts exits, JSON values, receipt fields and what
//! the fake saw, never prose.

use agh_fake::{FakeGithub, ReleaseSpec, RepoSpec};
#[cfg(unix)]
use aterm_json::Value;

use crate::common;

/// The public channel repository.
const CHANNEL: &str = "alabsystems/aterm";

/// One run: the exit, stdout and stderr.
struct Ran {
    exit: u8,
    stdout: String,
    stderr: String,
}

impl Ran {
    #[cfg(unix)]
    fn json(&self) -> Value {
        aterm_json::from_str(self.stdout.trim_end())
            .unwrap_or_else(|e| panic!("{:?}: {e}; stderr {}", self.stdout, self.stderr))
    }

    /// The receipt line's value for `key`.
    fn receipt(&self, key: &str) -> Option<String> {
        let line = self
            .stderr
            .lines()
            .find(|line| line.starts_with("agh: ok"))
            .unwrap_or_else(|| panic!("a receipt: {}", self.stderr));
        line.split(' ')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }

    fn ok(&self) -> &Ran {
        assert_eq!(self.exit, 0, "stderr {}", self.stderr);
        self
    }
}

/// `args` through `run_cli` in a fresh context over `fake`, rooted at `root`.
fn run(root: &std::path::Path, fake: &FakeGithub, args: &[&str]) -> Ran {
    let (mut ctx, captured) = common::ctx(root, fake, common::env());
    let exit = common::run_cli(&mut ctx, args);
    Ran {
        exit,
        stdout: captured.stdout_text(),
        stderr: captured.stderr_text(),
    }
}

/// A `--fields` value of each item of a JSON array.
#[cfg(unix)]
fn column(value: &Value, key: &str) -> Vec<Value> {
    value
        .as_array()
        .expect("an array")
        .iter()
        .map(|item| item[key].clone())
        .collect()
}

#[cfg(unix)]
#[test]
fn a_profile_provisioned_from_nothing_reads_as_the_routed_identity() {
    let root = common::scratch("golden-provision");
    let root = root.path();
    let fake = FakeGithub::start();
    let token = agh_fake::token("dev");
    fake.add_token(&token, "fixture-dev-bot");
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::public().push("fixture-dev-bot"),
    );
    let published = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"),
    );
    let draft = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::draft("v2", "2026-02-01T00:00:00Z"),
    );
    assert!(!agh::config::Dirs::under(root).config_file().exists());

    // A person at the keyboard (stderr a terminal) hands the token over on stdin.
    let (ctx, captured) = common::ctx(root, &fake, common::env());
    let mut ctx = ctx
        .with_terminals(false, true)
        .with_stdin(format!("{token}\n").into_bytes());
    let exit = common::run_cli(
        &mut ctx,
        &[
            "auth",
            "add",
            "dev",
            "--stdin",
            "--tier",
            "dev",
            "--owner",
            "fixture-dev",
            "--write",
        ],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let added: Value =
        aterm_json::from_str(captured.stdout_text().trim_end()).expect("the profile row");
    assert_eq!(added["profile"], Value::from("dev"));
    assert_eq!(added["login"], Value::from("fixture-dev-bot"));

    let route = run(
        root,
        &fake,
        &["whoami", "--for", "fixture-dev/app", "--op", "writer-read"],
    );
    let decision = route.ok().json()["decision"].clone();
    assert_eq!(
        decision["send"]["profile"],
        Value::from("dev"),
        "{decision}"
    );
    assert_eq!(decision["send"]["tier"], Value::from("dev"), "{decision}");

    let listed = run(
        root,
        &fake,
        &[
            "release",
            "list",
            "fixture-dev/app",
            "--drafts",
            "--fields",
            "id,draft",
        ],
    );
    let rows = listed.ok().json();
    assert_eq!(
        column(&rows, "id"),
        [Value::from(draft), Value::from(published)]
    );
    assert_eq!(
        column(&rows, "draft"),
        [Value::Bool(true), Value::Bool(false)]
    );

    let api: Vec<agh_fake::Seen> = fake
        .seen()
        .into_iter()
        .filter(|s| s.origin == agh_fake::FakeOrigin::Api)
        .collect();
    assert!(!api.is_empty());
    for seen in &api {
        assert_eq!(
            seen.as_login.as_deref(),
            Some("fixture-dev-bot"),
            "{} {}",
            seen.method,
            seen.path_and_query
        );
    }
    fake.assert_clean();
}

#[cfg(unix)]
#[test]
fn a_token_that_disappears_falls_to_the_chains_anonymous_step() {
    let root = common::scratch("golden-disappears");
    let root = root.path();
    let fake = FakeGithub::start();
    common::canonical_tokens(root, &fake, true, false);
    common::write_config(
        &agh::config::Dirs::under(root),
        &common::canonical_config(root),
    );
    fake.add_repo(CHANNEL, RepoSpec::public());
    fake.add_release(
        CHANNEL,
        ReleaseSpec::published("v1.0.0", "2026-01-01T00:00:00Z"),
    );
    let args = ["release", "list", CHANNEL, "--meta"];

    let as_dev = run(root, &fake, &args);
    assert_eq!(as_dev.ok().receipt("as").as_deref(), Some("dev"));
    assert!(fake.seen().iter().all(|s| s.had_authorization));

    std::fs::remove_file(root.join("tokens").join("dev.token")).expect("remove the token");
    fake.clear_seen();
    let anonymous = run(root, &fake, &args);
    assert_eq!(anonymous.ok().receipt("as").as_deref(), Some("anonymous"));
    assert_eq!(anonymous.json(), as_dev.json());
    assert!(fake.seen().iter().all(|s| !s.had_authorization));
    fake.assert_clean();
}

#[cfg(unix)]
#[test]
fn a_wait_then_a_routed_and_an_anonymous_download_agree() {
    use agh_fake::{AssetSpec, Change, Match};

    let root = common::scratch("golden-wait");
    let root = root.path();
    let fake = FakeGithub::start();
    common::canonical_tokens(root, &fake, true, false);
    common::write_config(
        &agh::config::Dirs::under(root),
        &common::canonical_config(root),
    );
    fake.add_repo(CHANNEL, RepoSpec::public());
    let tags = format!("/repos/{CHANNEL}/releases/tags/");
    let id = fake.add_release(
        CHANNEL,
        ReleaseSpec::draft("v1.1.0", "2026-10-01T00:00:00Z"),
    );
    fake.after(Match::api(&tags), 1, Change::Publish { release_id: id });
    fake.after(
        Match::api(&tags),
        2,
        Change::AddAsset {
            release_id: id,
            spec: AssetSpec::uploaded("SHA256SUMS", b"sums of the release\n"),
        },
    );

    let waited = run(
        root,
        &fake,
        &[
            "release",
            "wait",
            CHANNEL,
            "v1.1.0",
            "published",
            "asset=SHA256SUMS",
        ],
    );
    let state = waited.ok().json();
    let holds: Vec<Value> = column(&state["conditions"], "holds");
    assert_eq!(holds, [Value::Bool(true), Value::Bool(true)], "{state}");
    assert_eq!(state["conditions"][0]["observed"]["id"], Value::from(id));

    let mut digests = Vec::new();
    let mut credentialed = Vec::new();
    for (dir, anonymous) in [("D1", false), ("D2", true)] {
        let dir = root.join(dir);
        std::fs::create_dir(&dir).expect("the download directory");
        let dir = dir.display().to_string();
        let mut args = vec![
            "release",
            "download",
            CHANNEL,
            "v1.1.0",
            "--asset",
            "SHA256SUMS",
            "--dir",
            &dir,
        ];
        if anonymous {
            args.insert(0, "--anonymous");
        }
        fake.clear_seen();
        let rows = run(root, &fake, &args).ok().json();
        assert_eq!(rows[0]["digest"], Value::from("matched"), "{rows}");
        assert_eq!(rows[0]["written"], Value::Bool(true), "{rows}");
        digests.push(rows[0]["sha256"].clone());
        credentialed.push(fake.seen().iter().any(|s| s.had_authorization));
    }
    assert_eq!(digests[0], digests[1]);
    assert_eq!(
        digests[0],
        Value::from(
            agh::model::Sha256Digest::from_bytes(aterm_digest::Sha256::digest(
                b"sums of the release\n"
            ))
            .to_string()
        )
    );
    assert_eq!(credentialed, [true, false]);
    fake.assert_clean();
}

#[test]
fn jq_runs_over_every_page_of_a_listing() {
    let root = common::scratch("golden-jq");
    let root = root.path();
    let fake = FakeGithub::start();
    fake.add_repo(CHANNEL, RepoSpec::public());
    for n in 0..201u32 {
        let created = format!(
            "2026-{:02}-{:02}T{:02}:{:02}:00Z",
            1 + n / 28 % 12,
            1 + n % 28,
            n / 60 % 24,
            n % 60
        );
        fake.add_release(
            CHANNEL,
            ReleaseSpec::published(&format!("v0.{n}.0"), &created),
        );
    }
    let ran = run(
        root,
        &fake,
        &[
            "api",
            "/repos/alabsystems/aterm/releases",
            "--all",
            "--jq",
            ".[].tag_name",
            "--meta",
        ],
    );
    ran.ok();
    let lines: Vec<&str> = ran.stdout.lines().collect();
    assert_eq!(lines.len(), 201, "{}", ran.stderr);
    let mut tags: Vec<&str> = lines.iter().map(|l| l.trim_matches('"')).collect();
    tags.sort_unstable();
    tags.dedup();
    assert_eq!(tags.len(), 201);
    assert_eq!(ran.receipt("pages").as_deref(), Some("3"));
    assert_eq!(ran.receipt("complete").as_deref(), Some("1"));
    fake.assert_clean();
}
