// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Changed-path reads with no capture multiplied by the number of merge parents or commits.

use std::collections::BTreeSet;

/// What a merge owes the caller.
#[derive(Clone, Copy)]
pub enum MergeDiff {
    /// The resolution of a two-parent merge against Git's automatic merge. Git has no
    /// automatic remerge for an octopus, so every parent's difference is owed there.
    Resolutions,
    /// Every parent's difference, including paths an automatic merge imported.
    AllParents,
}

/// Union one commit's changed paths, consuming each parent's listing before reading the next.
/// Roots contribute their whole tree; renames contribute both names. Path bytes are preserved.
/// `read` must return complete output or an error, with its own capture and execution bounds.
///
/// # Errors
/// A Git read failed; no partial union is returned.
pub fn commit<E>(
    read: &mut impl FnMut(&[&str]) -> Result<Vec<u8>, E>,
    id: &str,
    parents: &[&str],
    mode: MergeDiff,
    restrict: &[&str],
) -> Result<BTreeSet<Vec<u8>>, E> {
    let mut paths = BTreeSet::new();
    let mut take = |prefix: &[&str]| {
        let mut args = prefix.to_vec();
        args.extend([
            "--name-only",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "--",
        ]);
        args.extend_from_slice(restrict);
        let listed = read(&args)?;
        paths.extend(
            listed
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(<[u8]>::to_vec),
        );
        Ok(())
    };
    if parents.len() == 2 && matches!(mode, MergeDiff::Resolutions) {
        take(&["show", "--format=", "--diff-merges=remerge", id])?;
    } else if parents.is_empty() {
        take(&["diff-tree", "--root", "--no-commit-id", "-r", id])?;
    } else {
        for parent in parents {
            take(&["diff", parent, id])?;
        }
    }
    Ok(paths)
}

/// Union changes made anywhere in `base..tip`, including changes subsequently reverted.
/// Enumerate the graph without path simplification, in bounded metadata pages, then read one
/// commit and parent at a time. `base` and `tip` must be immutable object IDs for the walk.
/// A name listing never contains repeated copies just because a merge has many parents.
///
/// # Errors
/// Git failed or wrote a malformed commit/parent row; no partial union is returned.
pub fn range(
    read: &mut impl FnMut(&[&str]) -> Result<Vec<u8>, String>,
    base: &str,
    tip: &str,
    mode: MergeDiff,
    restrict: &[&str],
) -> Result<BTreeSet<Vec<u8>>, String> {
    const PAGE: usize = 128;
    let range = format!("{base}..{tip}");
    let mut paths = BTreeSet::new();
    let mut skip = 0;
    loop {
        let listed = read(&[
            "rev-list",
            "--parents",
            "--max-count=128",
            &format!("--skip={skip}"),
            &range,
            "--",
        ])?;
        let listed = std::str::from_utf8(&listed).map_err(|e| e.to_string())?;
        let mut count = 0;
        for row in listed.lines() {
            let ids: Vec<_> = row.split_whitespace().collect();
            if ids.is_empty()
                || !ids
                    .iter()
                    .all(|id| aterm_types::land_trailer::object_id(id))
            {
                return Err("git rev-list wrote a malformed commit/parent row".into());
            }
            paths.extend(commit(read, ids[0], &ids[1..], mode, restrict)?);
            count += 1;
        }
        if count < PAGE {
            return Ok(paths);
        }
        skip += count;
    }
}

#[cfg(test)]
mod tests {
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
}
