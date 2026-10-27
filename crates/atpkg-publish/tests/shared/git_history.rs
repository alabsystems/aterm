// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Publication numbers come from complete history, never a shallow boundary.

use std::path::Path;

use atpkg_publish::{git::rev_count, testkit};

fn git(at: &Path, args: &[&str]) -> String {
    let output = testkit::git()
        .arg("-C")
        .arg(at)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .output()
        .expect("start fixture Git");
    assert!(
        output.status.success(),
        "Git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_owned()
}

fn history(at: &Path) -> String {
    git(at, &["init", "--quiet", "--initial-branch=main"]);
    git(at, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
    git(
        at,
        &["commit", "--quiet", "--allow-empty", "-m", "sealed source"],
    );
    let seal = git(at, &["rev-parse", "HEAD"]);
    git(
        at,
        &["commit", "--quiet", "--allow-empty", "-m", "main advances"],
    );
    seal
}

#[test]
fn full_history_counts_the_seal_commit_after_main_advances() {
    let directory = super::scratch("full-publication-history");
    let seal = history(directory.path());
    assert_eq!(
        rev_count(directory.path(), &seal).expect("complete seal history"),
        2
    );
    assert_eq!(
        rev_count(directory.path(), "HEAD").expect("complete main history"),
        3
    );
    git(
        directory.path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "main advances again",
        ],
    );
    assert_eq!(
        rev_count(directory.path(), &seal).expect("same complete seal history"),
        2
    );
    assert_eq!(
        rev_count(directory.path(), "HEAD").expect("new complete main history"),
        4
    );
}

#[test]
fn shallow_history_cannot_mint_a_truncated_publication_number() {
    let directory = super::scratch("shallow-publication-history");
    let full = directory.path().join("full");
    std::fs::create_dir(&full).expect("create full repository");
    history(&full);
    git(
        directory.path(),
        &[
            "clone",
            "--quiet",
            "--no-local",
            "--depth",
            "1",
            "full",
            "shallow",
        ],
    );
    let shallow = directory.path().join("shallow");
    assert_eq!(
        git(&shallow, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    assert_eq!(git(&shallow, &["rev-list", "--count", "HEAD"]), "1");
    let refusal =
        rev_count(&shallow, "HEAD").expect_err("a shallow count is not a publication number");
    assert!(refusal.to_string().contains("shallow"), "{refusal}");
    git(&shallow, &["fetch", "--quiet", "--unshallow"]);
    assert_eq!(
        rev_count(&shallow, "HEAD").expect("fetched complete history"),
        3
    );
}
