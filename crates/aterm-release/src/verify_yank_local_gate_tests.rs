// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Yank runs from any clean checkout whose own cutter it is.

use super::*;

fn scratch_repo(label: &str) -> aterm_tempfile::TempDir {
    let dir = crate::test_scratch_dir(label);
    let root = dir.path();
    ledger::testing::init(
        root,
        &[
            ("user.name", "t"),
            ("user.email", "t@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    for args in [
        &["commit", "-q", "--allow-empty", "-m", "one"][..],
        &["commit", "-q", "--allow-empty", "-m", "two"],
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    ] {
        let out = ledger::testing::git()
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }
    dir
}

#[test]
fn a_detached_checkout_may_yank_and_a_dirty_one_may_not() {
    let scratch = scratch_repo("detached");
    let root = scratch.path();
    let git = ledger::testing::Git::new(root);
    ledger::git_ok(&git, &["checkout", "-q", "--detach", "HEAD~1"]).unwrap();
    let branch = ledger::git_ok(&git, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .stdout_utf8();
    assert_eq!(branch.trim(), "HEAD");
    let ours = |_: &dyn ledger::GitRunner| -> Result<()> { Ok(()) };
    yank_local_gates(&git, "o/r", &ours).expect("no branch rule");

    fs::write(root.join("stray"), "x").unwrap();
    assert!(yank_local_gates(&git, "o/r", &ours).is_err());
    fs::remove_file(root.join("stray")).unwrap();
    let stale = |_: &dyn ledger::GitRunner| -> Result<()> { Err(Error::new("stale cutter")) };
    assert!(yank_local_gates(&git, "o/r", &stale).is_err());
    assert!(yank_local_gates(&git, "someone/else", &ours).is_err());
}
