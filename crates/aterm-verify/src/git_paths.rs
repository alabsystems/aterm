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
#[path = "git_paths_tests.rs"]
mod tests;
