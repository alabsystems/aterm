// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The main lane's append-only, compare-and-swap Git journal. No verdict is a gate.

use aterm_uds::spawnfd::Command;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const REF: &str = "refs/aterm/judged";
pub const RETENTION: u64 = 14 * 86400;
pub const LEASE: u64 = 120;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub outcome: String,
    pub log: String,
    pub red_items: Vec<String>,
    pub suspects: Vec<String>,
    pub fixed_by: Option<String>,
    pub decisions: Vec<Decision>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decision {
    pub id: String,
    pub outcome: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub commit: String,
    pub tree: String,
    pub host: String,
    pub verifier: String,
    pub inputs: String,
    pub token: String,
    pub state: String,
    pub started: u64,
    pub updated: u64,
    pub lease_until: u64,
    pub rows: Vec<Row>,
}

impl Record {
    /// An expired lease is reclaimable; cancellation and infrastructure failures are retryable.
    #[must_use]
    pub fn occupied(&self, now: u64) -> bool {
        matches!(self.state.as_str(), "pass" | "red")
            || (self.state == "judging" && self.lease_until > now)
    }
}

pub fn command(root: &Path, args: &[&str]) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("git");
    cmd.current_dir(root).args(args).env_remove("GITHUB_TOKEN");
    cmd
}

pub fn deadline() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(60)
}

fn capture_until(
    cmd: &mut Command,
    input: Option<&[u8]>,
    until: std::time::Instant,
) -> Result<std::process::Output, String> {
    if std::time::Instant::now() >= until {
        return Err("journal operation exhausted its deadline".into());
    }
    aterm_uds::deadline::output_with_input_until(
        cmd,
        aterm_uds::secret::Secret::new(input.unwrap_or_default()),
        until,
        512 * 1024 * 1024,
    )
    .map_err(|e| e.to_string())
}

pub fn output(cmd: &mut Command, input: Option<&[u8]>) -> Result<String, String> {
    output_until(cmd, input, deadline())
}

fn output_until(
    cmd: &mut Command,
    input: Option<&[u8]>,
    until: std::time::Instant,
) -> Result<String, String> {
    let out = capture_until(cmd, input, until)?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

pub fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    output(&mut command(root, args), None).map(|s| s.trim().to_owned())
}

fn git_until(root: &Path, args: &[&str], until: std::time::Instant) -> Result<String, String> {
    output_until(&mut command(root, args), None, until).map(|s| s.trim().to_owned())
}

/// Refresh only this private journal, never the checkout's branch or index.
pub fn head(
    root: &Path,
    remote: &str,
    until: std::time::Instant,
) -> Result<Option<String>, String> {
    let git = |root: &Path, args: &[&str]| git_until(root, args, until);
    let named = git(root, &["ls-remote", remote, REF])?;
    let Some(sha) = named.split_whitespace().next() else {
        return Ok(None);
    };
    git(root, &["fetch", "--quiet", "--no-tags", remote, REF])?;
    Ok(Some(sha.to_owned()))
}

pub fn records(
    root: &Path,
    head: Option<&str>,
    until: std::time::Instant,
) -> Result<Vec<Record>, String> {
    let git = |root: &Path, args: &[&str]| git_until(root, args, until);
    let output = |cmd: &mut Command, input: Option<&[u8]>| output_until(cmd, input, until);
    let Some(head) = head else {
        return Ok(Vec::new());
    };
    let names = git(
        root,
        &["ls-tree", "-r", "--format=%(objectname)", head, "verdicts/"],
    )?;
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let objects = output(
        &mut command(root, &["cat-file", "--batch"]),
        Some(format!("{names}\n").as_bytes()),
    )?;
    decode_objects(objects.as_bytes())
}

pub fn record(
    root: &Path,
    head: Option<&str>,
    tree: &str,
    until: std::time::Instant,
) -> Result<Option<Record>, String> {
    let git = |root: &Path, args: &[&str]| git_until(root, args, until);
    let Some(head) = head else {
        return Ok(None);
    };
    let path = format!("verdicts/{tree}.toml");
    if git(root, &["ls-tree", head, &path])?.is_empty() {
        return Ok(None);
    }
    let text = git(root, &["show", &format!("{head}:{path}")])?;
    let record: Record = aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
    if record.tree != tree {
        return Err("journal record is filed under another tree".into());
    }
    Ok(Some(record))
}

fn decode_objects(mut bytes: &[u8]) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    while !bytes.is_empty() {
        let end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("truncated object header")?;
        let header = std::str::from_utf8(&bytes[..end]).map_err(|e| e.to_string())?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.len() != 3 || fields[1] != "blob" {
            return Err(format!("invalid journal object: {header}"));
        }
        let length = fields[2].parse::<usize>().map_err(|e| e.to_string())?;
        bytes = &bytes[end + 1..];
        let text = std::str::from_utf8(bytes.get(..length).ok_or("truncated journal blob")?)
            .map_err(|e| e.to_string())?;
        records.push(aterm_toml::from_str(text).map_err(|e| e.to_string())?);
        bytes = bytes
            .get(length..)
            .and_then(|b| b.strip_prefix(b"\n"))
            .ok_or("missing object terminator")?;
    }
    Ok(records)
}

/// A normal fast-forward push is the CAS: a competing append rejects this parent.
/// The caller rereads and redecides; it never force-pushes a stale decision.
pub fn append(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    record: &Record,
    until: std::time::Instant,
) -> Result<bool, String> {
    // One absolute deadline bounds the whole append, including CAS races and pruning.
    let git = |root: &Path, args: &[&str]| git_until(root, args, until);
    let output = |cmd: &mut Command, input: Option<&[u8]>| output_until(cmd, input, until);
    let capture = |cmd: &mut Command, input: Option<&[u8]>| capture_until(cmd, input, until);
    let index = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
    let index_path = index.path().join("index");
    let mut read = command(root, &["read-tree", parent.unwrap_or("--empty")]);
    read.env("GIT_INDEX_FILE", &index_path);
    output(&mut read, None)?;
    let last_pruned = parent
        .and_then(|p| git(root, &["show", &format!("{p}:pruned-at")]).ok())
        .and_then(|s| s.parse::<u64>().ok());
    let prune = last_pruned.is_none_or(|day| day < record.updated / 86400);
    for old in if prune {
        records(root, parent, until)?
    } else {
        Vec::new()
    } {
        if old.updated.saturating_add(RETENTION) < record.updated {
            let mut remove = command(
                root,
                &[
                    "update-index",
                    "--force-remove",
                    &format!("verdicts/{}.toml", old.tree),
                ],
            );
            remove.env("GIT_INDEX_FILE", &index_path);
            output(&mut remove, None)?;
        }
    }
    if prune {
        let blob = output(
            &mut command(root, &["hash-object", "-w", "--stdin"]),
            Some((record.updated / 86400).to_string().as_bytes()),
        )?;
        let mut add = command(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "100644",
                blob.trim(),
                "pruned-at",
            ],
        );
        add.env("GIT_INDEX_FILE", &index_path);
        output(&mut add, None)?;
    }
    let text = aterm_toml::to_string(record).map_err(|e| e.to_string())?;
    let blob = output(
        &mut command(root, &["hash-object", "-w", "--stdin"]),
        Some(text.as_bytes()),
    )?;
    let path = format!("verdicts/{}.toml", record.tree);
    let mut add = command(
        root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "100644",
            blob.trim(),
            &path,
        ],
    );
    add.env("GIT_INDEX_FILE", &index_path);
    output(&mut add, None)?;
    let mut write = command(root, &["write-tree"]);
    write.env("GIT_INDEX_FILE", &index_path);
    let tree = output(&mut write, None)?;
    let mut commit = command(
        root,
        &[
            "commit-tree",
            tree.trim(),
            "-m",
            &format!("verify watch: {} {}", record.state, record.commit),
        ],
    );
    if let Some(parent) = parent {
        commit.args(["-p", parent]);
    }
    let sha = output(&mut commit, None)?;
    let pushed = capture(
        &mut command(
            root,
            &[
                "push",
                "--porcelain",
                remote,
                &format!("{}:{REF}", sha.trim()),
            ],
        ),
        None,
    )?;
    if pushed.status.success() {
        return Ok(true);
    }
    // A race is retryable only when the remote actually moved. Authentication/network failures
    // remain errors, rather than turning into a tight retry loop.
    if head(root, remote, until)?.as_deref() != parent {
        return Ok(false);
    }
    Err(String::from_utf8_lossy(&pushed.stderr).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_decided_verdicts_or_unexpired_claims_exclude_another_sample() {
        let mut r = Record {
            commit: "c".into(),
            tree: "t".into(),
            host: "h".into(),
            verifier: "v".into(),
            inputs: "i".into(),
            token: "one".into(),
            state: "judging".into(),
            started: 0,
            updated: 0,
            lease_until: 120,
            rows: vec![],
        };
        assert!(r.occupied(119));
        assert!(!r.occupied(120));
        for state in ["cancelled", "could-not-run"] {
            r.state = state.into();
            assert!(!r.occupied(1));
        }
        for state in ["pass", "red"] {
            r.state = state.into();
            assert!(r.occupied(1000));
        }
    }

    #[test]
    fn competing_append_cannot_replace_a_claim_and_pruning_keeps_recent_rows() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "--bare", "remote"]).unwrap();
        git(root, &["clone", "remote", "one"]).unwrap();
        let one = root.join("one");
        git(&one, &["config", "user.name", "Andrew Yates"]).unwrap();
        git(&one, &["config", "user.email", "test@example.invalid"]).unwrap();
        let mut r = Record {
            commit: "c".into(),
            tree: "t".into(),
            host: "h".into(),
            verifier: "v".into(),
            inputs: "i".into(),
            token: "one".into(),
            state: "judging".into(),
            started: 0,
            updated: 1,
            lease_until: 120,
            rows: vec![],
        };
        assert!(append(&one, "origin", None, &r, deadline()).unwrap());
        let first = head(&one, "origin", deadline()).unwrap().unwrap();
        assert!(
            append(
                &one,
                "origin",
                Some(&first),
                &r,
                std::time::Instant::now() - std::time::Duration::from_secs(1)
            )
            .is_err()
        );
        assert_eq!(
            head(&one, "origin", deadline()).unwrap().as_deref(),
            Some(first.as_str())
        );
        let blob = git(&one, &["rev-parse", &format!("{first}:verdicts/t.toml")]).unwrap();
        let input = format!("{blob}\n").repeat(3000);
        let objects = output(
            &mut command(&one, &["cat-file", "--batch"]),
            Some(input.as_bytes()),
        )
        .unwrap();
        assert_eq!(
            decode_objects(objects.as_bytes()).unwrap().len(),
            3000,
            "large journals must drain output while feeding more object requests"
        );
        r.token = "two".into();
        assert!(!append(&one, "origin", None, &r, deadline()).unwrap());
        assert_eq!(
            records(&one, Some(&first), deadline()).unwrap()[0].token,
            "one"
        );
        r.tree = "new".into();
        r.updated = RETENTION + 2;
        assert!(append(&one, "origin", Some(&first), &r, deadline()).unwrap());
        let last = head(&one, "origin", deadline()).unwrap().unwrap();
        assert_eq!(records(&one, Some(&last), deadline()).unwrap(), vec![r]);
    }
}
