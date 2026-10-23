// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Build-number-to-commit resolution tests over git graphs.

use std::path::{Path, PathBuf};

use atpkg_publish::lane::resolve::resolve_commit;

use super::super::shared::scratch;
use super::fixture::{commit, git, repo};

fn clone_of(origin: &Path, at: &Path) -> PathBuf {
    let clone = at.join("clone");
    git(at, &["clone", "-q", &origin.display().to_string(), "clone"]);
    clone
}

fn count(dir: &Path, rev: &str) -> u64 {
    git(dir, &["rev-list", "--count", rev])
        .parse()
        .expect("count")
}

#[test]
fn a_build_on_the_first_parent_chain_is_found_by_binary_search() {
    let tmp = scratch("resolve-linear");
    let origin = tmp.path().join("origin");
    repo(&origin, 57);
    let clone = clone_of(&origin, tmp.path());
    for want in [1, 2, 28, 56, 57] {
        let sha = resolve_commit(&clone, want).expect("resolved");
        assert_eq!(count(&clone, &sha), want);
    }
}

/// main: A(1) B(2) C(3) M(7), where M merges a side branch S1(2) S2(3) S3(4) off A.
fn merged(at: &Path) -> (PathBuf, String) {
    let origin = at.join("origin");
    repo(&origin, 0);
    let a = commit(&origin, "A", None);
    commit(&origin, "B", None);
    git(&origin, &["checkout", "-q", "-b", "side", &a]);
    commit(&origin, "S1", None);
    commit(&origin, "S2", None);
    let s3 = commit(&origin, "S3", None);
    git(&origin, &["checkout", "-q", "main"]);
    commit(&origin, "C", None);
    git(&origin, &["merge", "-q", "--no-ff", "side", "-m", "M"]);
    (clone_of(&origin, at), s3)
}

#[test]
fn a_build_inside_a_merge_is_found_in_the_one_bracketing_span() {
    let tmp = scratch("resolve-merge");
    let (clone, s3) = merged(tmp.path());
    assert_eq!(count(&clone, "origin/main"), 7);
    assert_eq!(resolve_commit(&clone, 4).expect("in the merge"), s3);
    assert_eq!(
        count(&clone, &resolve_commit(&clone, 7).expect("the merge")),
        7
    );
    assert_eq!(count(&clone, &resolve_commit(&clone, 3).expect("C")), 3);
}

#[test]
fn a_count_no_commit_has_is_refused_naming_the_span() {
    let tmp = scratch("resolve-none");
    let (clone, _) = merged(tmp.path());
    let e = resolve_commit(&clone, 5).expect_err("no commit counts 5");
    assert!(e.to_string().contains("has rev-list count 5"), "{e}");
}

#[test]
fn a_pin_past_the_origin_head_is_refused_as_an_over_count() {
    let tmp = scratch("resolve-over");
    let origin = tmp.path().join("origin");
    repo(&origin, 5);
    let clone = clone_of(&origin, tmp.path());
    let e = resolve_commit(&clone, 6).expect_err("over-count");
    assert!(
        e.to_string()
            .contains("resolve: pin 6 exceeds origin head's count 5 (stale clone"),
        "{e}"
    );
}

#[test]
fn a_clone_with_no_origin_head_is_refused() {
    let tmp = scratch("resolve-no-origin");
    let lone = tmp.path().join("lone");
    repo(&lone, 3);
    let e = resolve_commit(&lone, 2).expect_err("no origin");
    assert!(e.to_string().contains("no origin head ref"), "{e}");
}
