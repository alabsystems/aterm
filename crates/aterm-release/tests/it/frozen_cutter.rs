// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real frozen cutter execs, then starts a step child through cut_command_of.

use crate::ledger::{GitCli, git_ok};
use crate::release_tools;
use std::time::Duration;

#[test]
fn a_frozen_cutters_own_child_proves_itself_without_the_freezer_envelope() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let source = root.join("source");
    crate::ledger::testing::init(&source, &[]);
    let git = GitCli::new(&source);
    std::fs::write(source.join("README"), "fixture release\n").unwrap();
    git_ok(&git, &["add", "README"]).unwrap();
    git_ok(&git, &["commit", "-qm", "recorded fixture commit"]).unwrap();
    let commit = git_ok(&git, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    let repo = root.join("clone");
    git_ok(
        &git,
        &[
            "clone",
            "-q",
            source.to_str().unwrap(),
            repo.to_str().unwrap(),
        ],
    )
    .unwrap();
    release_tools::clean_at(&repo, &commit).unwrap();
    let release = repo.join(".git/aterm-release");
    let binary = aterm_tempfile::bin_exe("aterm-release");
    // A failed first exec must retain the copied Mach-O and its directory for assessment.
    // stage_cutter proves successful --version output through EOF before cleanup is safe.
    let retained = dir.keep();
    let cutter = release_tools::stage_cutter(&release, &commit, &binary).unwrap();
    release_tools::publish_cutter(&release, &cutter).unwrap();
    for mode in ["proved", "scrubbed", "foreign", "envelope"] {
        let mut cmd = aterm_uds::spawnfd::command(&binary);
        cmd.current_dir(&repo)
            .args(["--cutter-child-probe", "exec", mode])
            .arg(&cutter.tool.path)
            .arg(&binary);
        let out = aterm_uds::deadline::output_within(&mut cmd, Duration::from_secs(60)).unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stdout.contains(&format!("parent Proved {commit}")),
            "{mode}: {stdout}\n{stderr}"
        );
        if mode == "proved" {
            assert!(out.status.success(), "{stdout}\n{stderr}");
            assert!(
                stdout.contains(&format!("child Proved {commit}; freezer envelope absent")),
                "{stdout}"
            );
        } else {
            assert!(
                !out.status.success(),
                "negative control {mode} accepted: {stdout}"
            );
            let reason = if mode == "scrubbed" {
                "cannot read"
            } else if mode == "foreign" {
                "not the recorded frozen cutter"
            } else {
                "freezer envelope leaked"
            };
            assert!(stderr.contains(reason), "{mode}: {stderr}");
        }
    }
    std::fs::remove_dir_all(retained).unwrap();
}
