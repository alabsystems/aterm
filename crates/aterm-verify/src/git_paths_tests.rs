// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::path::Path;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let out = aterm_uds::fixture_git::git()
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
    out.stdout
}

fn text(root: &Path, args: &[&str]) -> String {
    String::from_utf8(git(root, args)).unwrap().trim().into()
}

/// The old one-capture `-m` query exceeds the very same ceiling that admits each
/// parent's listing. Combined and first-parent diffs both omit the archived paths.
#[test]
fn octopus_and_range_reads_do_not_multiply_the_capture() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["commit", "-qm", "empty", "--allow-empty"]);
    let empty = text(root, &["rev-parse", "HEAD^{tree}"]);
    let a = text(root, &["commit-tree", &empty, "-m", "a"]);
    let b = text(root, &["commit-tree", &empty, "-m", "b"]);
    for i in 0..128 {
        std::fs::write(root.join(format!("path-{i:03}-{}", "x".repeat(80))), "kept").unwrap();
    }
    git(root, &["add", "."]);
    let tree = text(root, &["write-tree"]);
    let base = text(
        root,
        &["commit-tree", &tree, "-p", &a, "-p", &b, "-m", "released"],
    );
    std::fs::write(root.join("manifest"), "preserved").unwrap();
    git(root, &["add", "."]);
    let tree = text(root, &["write-tree"]);
    let archive = text(
        root,
        &[
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-p",
            &a,
            "-p",
            &b,
            "-m",
            "archive",
        ],
    );
    let limit = git(root, &["diff", "--name-only", "-z", &a, &archive]).len();
    let old = git(
        root,
        &[
            "diff-tree",
            "-r",
            "-m",
            "--no-commit-id",
            "--name-only",
            "-z",
            &archive,
        ],
    );
    assert!(
        old.len() > limit,
        "negative control: one -m capture overflows"
    );
    let mut read = |args: &[&str]| {
        let bytes = git(root, args);
        if bytes.len() > limit {
            Err("capture exceeded".into())
        } else {
            Ok(bytes)
        }
    };
    for mode in [MergeDiff::AllParents, MergeDiff::Resolutions] {
        let found = range(&mut read, &base, &archive, mode, &[]).unwrap();
        assert_eq!(found.len(), 129);
        assert!(found.contains(b"manifest".as_slice()));
    }
    for flag in ["--diff-merges=first-parent", "--diff-merges=combined"] {
        let old = git(
            root,
            &["show", "--format=", "--name-only", "-z", flag, &archive],
        );
        assert_eq!(
            old, b"manifest\0",
            "negative control: {flag} misses 128 paths"
        );
    }
    let restricted = range(
        &mut read,
        &base,
        &archive,
        MergeDiff::Resolutions,
        &["manifest"],
    )
    .unwrap();
    assert_eq!(restricted, BTreeSet::from([b"manifest".to_vec()]));
    let mut refused = |_: &[&str]| -> Result<Vec<u8>, String> { Err("not complete".into()) };
    assert!(range(&mut refused, &base, &archive, MergeDiff::AllParents, &[]).is_err());

    // Cross a metadata page with alternating trees: a whole range's repeated
    // names would exceed the ceiling, while every single-commit listing fits.
    let mut tip = archive;
    for n in 0..130 {
        let next = if n % 2 == 0 { &empty } else { &tree };
        tip = text(
            root,
            &[
                "commit-tree",
                next,
                "-p",
                &tip,
                "-m",
                &format!("change {n}"),
            ],
        );
    }
    assert!(
        git(
            root,
            &[
                "log",
                "--format=",
                "--name-only",
                "-z",
                &format!("{base}..{tip}")
            ]
        )
        .len()
            > limit
    );
    assert_eq!(
        range(&mut read, &base, &tip, MergeDiff::AllParents, &[])
            .unwrap()
            .len(),
        129
    );
}
