// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A clean detached submodule can still own the only copy of other Git history.

use std::path::{Path, PathBuf};

use aterm_uds::holders::PathScan;

use super::super::super::fence::Fence;
use super::super::{Git as _, retire_on};
use super::{Gate, GitCli, for_git, gate, git};

fn read(dir: &Path, args: &[&str]) -> String {
    let answer = GitCli.run(dir, args);
    assert_eq!(answer.code, Some(0), "git {args:?}: {}", answer.err);
    answer.out.trim().to_owned()
}

fn private_module(g: &Gate) -> (PathBuf, PathBuf, PathBuf, String) {
    let sub = g.snapshot().join("vendor/astream");
    let common = PathBuf::from(read(
        &sub,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    assert!(common.starts_with(g.checkout().join(".git/worktrees")));
    let witness = g.checkout().join("vendor/astream");
    let pin = read(&sub, &["rev-parse", "HEAD"]);
    assert_eq!(read(&witness, &["rev-parse", "HEAD"]), pin);
    (sub, common, witness, pin)
}

fn absent(dir: &Path, object: &str) {
    let answer = GitCli.run(dir, &["cat-file", "-e", object]);
    assert!(answer.code.is_some(), "Git must run: {}", answer.err);
    assert_ne!(
        answer.code,
        Some(0),
        "{object} must not exist in {}",
        dir.display()
    );
}

fn clean_at_pin(g: &Gate, sub: &Path, pin: &str) {
    assert_eq!(read(sub, &["rev-parse", "HEAD"]), pin);
    assert_eq!(read(sub, &["status", "--porcelain"]), "");
    assert_eq!(
        read(
            &g.snapshot(),
            &["status", "--porcelain", "--ignore-submodules=none"]
        ),
        "",
    );
    assert_eq!(
        read(sub, &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "the private store has no extra worktree to trigger the registry guard",
    );
}

#[test]
fn a_clean_private_submodule_keeps_its_unpublished_branch_and_blob() {
    let g = gate();
    let (sub, common, witness, pin) = private_module(&g);
    git(&sub, &["checkout", "-q", "-b", "retained-private"]);
    let payload = b"// committed only on the private branch\n";
    std::fs::write(sub.join("lib.rs"), payload).unwrap();
    git(&sub, &["add", "lib.rs"]);
    git(&sub, &["commit", "-qm", "retain private branch source"]);
    let unique = read(&sub, &["rev-parse", "HEAD"]);
    let blob = read(&sub, &["rev-parse", "HEAD:lib.rs"]);
    git(&sub, &["checkout", "-q", "--detach", &pin]);
    clean_at_pin(&g, &sub, &pin);
    let origin = g.home().join("astream");
    for elsewhere in [&witness, &origin] {
        absent(elsewhere, &unique);
        absent(elsewhere, &blob);
    }

    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = retire_on(&g.roots, g.now, &GitCli, &Fence::new(&clear));
    assert!(
        g.snapshot().join("Cargo.toml").is_file() && g.cells().is_dir() && common.is_dir(),
        "retirement must keep the sole private branch and its source: {done:#?}",
    );
    assert!(
        done.iter()
            .any(|(row, result)| row.path == g.snapshot() && result.is_err()),
        "{done:#?}"
    );
    assert_eq!(
        read(&sub, &["rev-parse", "refs/heads/retained-private"]),
        unique
    );
    let saved = GitCli.run(&sub, &["show", "refs/heads/retained-private:lib.rs"]);
    assert_eq!(saved.code, Some(0), "{}", saved.err);
    assert_eq!(saved.out.as_bytes(), payload);
    clean_at_pin(&g, &sub, &pin);
}

#[test]
fn an_older_private_stash_stays_when_only_the_newest_is_retained_elsewhere() {
    let g = gate();
    let (sub, common, witness, pin) = private_module(&g);
    let older_payload = b"// older private stash is the only copy of this source\n";
    std::fs::write(sub.join("lib.rs"), older_payload).unwrap();
    git(&sub, &["stash", "push", "-q", "-m", "older private source"]);
    let older = read(&sub, &["rev-parse", "refs/stash"]);
    let older_blob = read(&sub, &["rev-parse", "refs/stash:lib.rs"]);
    std::fs::write(
        sub.join("lib.rs"),
        b"// newest independently retained stash\n",
    )
    .unwrap();
    git(
        &sub,
        &["stash", "push", "-q", "-m", "newest retained source"],
    );
    let newest = read(&sub, &["rev-parse", "refs/stash"]);
    assert_ne!(older, newest);
    assert_eq!(read(&sub, &["rev-parse", "refs/stash@{1}"]), older);
    assert_eq!(
        GitCli
            .run(&sub, &["merge-base", "--is-ancestor", &older, &newest])
            .code,
        Some(1)
    );
    clean_at_pin(&g, &sub, &pin);

    // A normal file-transport fetch copies only the newest stash's closure into
    // the independent parent module. Its sibling stash is not an ancestor.
    git(
        &witness,
        &[
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "-q",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            &for_git(&sub),
            "refs/stash:refs/heads/retained-newest-stash",
        ],
    );
    assert_eq!(
        read(&witness, &["rev-parse", "refs/heads/retained-newest-stash"]),
        newest
    );
    assert_eq!(
        GitCli.run(&witness, &["cat-file", "-e", &newest]).code,
        Some(0)
    );
    let origin = g.home().join("astream");
    for elsewhere in [&witness, &origin] {
        absent(elsewhere, &older);
        absent(elsewhere, &older_blob);
    }
    clean_at_pin(&g, &sub, &pin);
    let reflog = std::fs::read(common.join("logs/refs/stash")).unwrap();
    assert_eq!(
        reflog
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count(),
        2
    );

    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = retire_on(&g.roots, g.now, &GitCli, &Fence::new(&clear));
    assert!(
        g.snapshot().join("Cargo.toml").is_file() && g.cells().is_dir() && common.is_dir(),
        "the newest saved stash does not preserve the older private stash: {done:#?}",
    );
    assert!(
        done.iter()
            .any(|(row, result)| row.path == g.snapshot() && result.is_err()),
        "{done:#?}"
    );
    assert_eq!(
        std::fs::read(common.join("logs/refs/stash")).unwrap(),
        reflog
    );
    assert_eq!(read(&sub, &["rev-parse", "refs/stash@{1}"]), older);
    assert_eq!(read(&sub, &["rev-parse", "refs/stash"]), newest);
    let saved = GitCli.run(&sub, &["show", &format!("{older}:lib.rs")]);
    assert_eq!(saved.code, Some(0), "{}", saved.err);
    assert_eq!(saved.out.as_bytes(), older_payload);
    clean_at_pin(&g, &sub, &pin);
}
