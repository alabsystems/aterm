// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const A: &str = "aaaaaaaaaaaa0000000000000000000000000000";
const B: &str = "bbbbbbbbbbbb0000000000000000000000000000";

fn pointer(full: &str, build_number: u64, smoke: Smoke) -> Pointer {
    Pointer {
        version: "0.199.0".to_string(),
        commit: full[..12].to_string(),
        commit_full: full.to_string(),
        build_number,
        dir: std::env::temp_dir().join(&full[..12]),
        staged_at: "2026-11-11T19:20:00Z".to_string(),
        smoke,
    }
}

fn head(full: &str, build_number: u64) -> Head<'_> {
    Head {
        commit_full: full,
        build_number,
    }
}

#[test]
fn plan_builds_only_what_is_not_staged() {
    let staged = pointer(A, 100, Smoke::Pass);
    assert_eq!(
        plan(None, false, head(A, 100), false),
        Plan::Build,
        "no pointer yet"
    );
    assert_eq!(
        plan(Some(&staged), true, head(A, 100), false),
        Plan::Staged(Smoke::Pass)
    );
    // Negative controls: the same pointer, one fact changed, builds.
    assert_eq!(
        plan(Some(&staged), true, head(B, 200), false),
        Plan::Build,
        "main moved"
    );
    assert_eq!(
        plan(Some(&staged), false, head(A, 100), false),
        Plan::Build,
        "the folder is gone"
    );
    assert_eq!(
        plan(Some(&staged), true, head(A, 100), true),
        Plan::Build,
        "--force"
    );
}

#[test]
fn plan_never_rebuilds_a_failed_commit_and_retries_one_not_smoked() {
    let failed = pointer(A, 100, Smoke::Fail);
    assert_eq!(
        plan(Some(&failed), true, head(A, 100), false),
        Plan::Staged(Smoke::Fail)
    );
    let not_run = pointer(A, 100, Smoke::NotRun);
    assert_eq!(plan(Some(&not_run), true, head(A, 100), false), Plan::Build);
    assert_eq!(
        plan(Some(&failed), true, head(B, 200), false),
        Plan::Build,
        "a new commit is tried"
    );
}

#[test]
fn plan_refuses_a_head_older_than_the_staged_build() {
    let staged = pointer(A, 200, Smoke::Pass);
    assert_eq!(
        plan(Some(&staged), true, head(B, 100), false),
        Plan::Older {
            staged: 200,
            head: 100
        }
    );
    // Controls: an equal or newer head builds, and --force overrides.
    assert_eq!(plan(Some(&staged), true, head(B, 200), false), Plan::Build);
    assert_eq!(plan(Some(&staged), true, head(B, 201), false), Plan::Build);
    assert_eq!(plan(Some(&staged), true, head(B, 100), true), Plan::Build);
}

#[test]
fn install_needed_only_for_a_passed_build_that_is_not_installed() {
    let staged = pointer(A, 100, Smoke::Pass);
    assert_eq!(install_needed(&staged, None, false), Install::Yes);
    assert_eq!(
        install_needed(&staged, Some((B, 50, false)), false),
        Install::Yes
    );
    assert_eq!(
        install_needed(&staged, Some((A, 100, false)), false),
        Install::Same,
        "already installed"
    );
    // A copy of the staged commit built from a changed tree carries its number and is not
    // that build: it is replaced (the control is the clean copy above).
    assert_eq!(
        install_needed(&staged, Some((A, 100, true)), false),
        Install::Yes
    );
    for smoke in [Smoke::Fail, Smoke::NotRun] {
        for force in [false, true] {
            assert_eq!(
                install_needed(&pointer(A, 100, smoke), Some((B, 50, false)), force),
                Install::Unproven,
                "{smoke:?} is never installed"
            );
        }
    }
}

#[test]
fn install_never_downgrades_a_newer_installed_build() {
    let staged = pointer(A, 100, Smoke::Pass);
    assert_eq!(
        install_needed(&staged, Some((B, 200, false)), false),
        Install::Newer {
            installed: 200,
            staged: 100
        }
    );
    // Controls: an equal or older installed build is replaced, and --force overrides.
    assert_eq!(
        install_needed(&staged, Some((B, 100, false)), false),
        Install::Yes
    );
    assert_eq!(
        install_needed(&staged, Some((B, 99, false)), false),
        Install::Yes
    );
    assert_eq!(
        install_needed(&staged, Some((B, 200, false)), true),
        Install::Yes
    );
    // The installed commit itself is never reinstalled, forced or not.
    assert_eq!(
        install_needed(&staged, Some((A, 100, false)), true),
        Install::Same
    );
}

#[test]
fn retention_keeps_the_named_builds_and_everything_that_is_not_a_dist() {
    let existing: Vec<String> = [
        "aaaaaaaaaaaa",
        "bbbbbbbbbbbb",
        "cccccccccccc",
        "dddddddddddd",
        "latest.json",
        "update.log",
        "my notes",
        "AAAAAAAAAAAA",
        "aaaaaaaaaaaa.smoke.txt",
    ]
    .map(String::from)
    .to_vec();
    let gone = retention(&existing, &["aaaaaaaaaaaa", "bbbbbbbbbbbb"]);
    assert_eq!(gone, ["cccccccccccc", "dddddddddddd"]);
    // Negative control: with nothing kept, every dist goes and still nothing else does.
    let all = retention(&existing, &[]);
    assert_eq!(
        all,
        [
            "aaaaaaaaaaaa",
            "bbbbbbbbbbbb",
            "cccccccccccc",
            "dddddddddddd"
        ]
    );
}

#[test]
fn leftovers_are_only_the_half_made_and_the_replaced_dists() {
    let existing: Vec<String> = [
        "aaaaaaaaaaaa",
        "aaaaaaaaaaaa.building",
        "bbbbbbbbbbbb.old",
        "notes.building",
        "aaaaaaaaaaaa.smoke.txt",
        "AAAAAAAAAAAA.old",
        ".building",
        "latest.json",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        leftovers(&existing),
        ["aaaaaaaaaaaa.building", "bbbbbbbbbbbb.old"]
    );
    // Neither is a dist to retention: a leftover is never kept or counted as one.
    assert_eq!(retention(&existing, &[]), ["aaaaaaaaaaaa"]);
}
