// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The work record's reads ([`read_work`]): the repository probe's own runner, deadlines and
//! guards, so every git it starts runs as the probe's do (no fsmonitor, no hooks, no system or
//! global configuration, no optional locks, no prompt, each read under the hang detector), and
//! the reads that walk history or compare trees (the merge-base, the behind count, the committed
//! names and the status) run offline, so none of them fetches.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{
    Diagnostics, Reads, branch_unborn, git, git_lossy, git_offline_answer, is_head, main_ref,
    run_git_bytes, unattended_dir, work_status,
};
use crate::harness::work_record::{WorkRead, WorkRecord, changed_names};

/// Where the session working in `dir` works ([`WorkRecord`]), read with git under the probe's
/// rules, `home` the person's home and `xdg` their `XDG_CONFIG_HOME` (for their excludes alone).
/// Each read has `limit` from the end of the one before it, as [`super::probe`]'s do, and none
/// starts while `held` says new starts are held back. Nothing is read inside a folder macOS
/// guards, on a network or automounted volume, or with no `home` ([`WorkRead::None`], no git
/// started); a read the limit cut or `held` stopped, one git could not start, a listing or count
/// git refused or warned about, or a merge-base git refused while `HEAD` names a commit, is
/// [`WorkRead::Unread`].
#[must_use]
pub fn read_work(
    dir: &Path,
    home: Option<&Path>,
    xdg: Option<&Path>,
    limit: Duration,
    held: &dyn Fn() -> bool,
) -> WorkRead {
    let Some(home) = home else {
        return WorkRead::None;
    };
    if atpkg::protected::under_protected_root(home, dir) {
        return WorkRead::None;
    }
    let Some(deadline) = Instant::now().checked_add(limit) else {
        return WorkRead::None;
    };
    let reads = Reads {
        deadline: Cell::new(deadline),
        each: Some(limit),
        home,
        xdg: xdg.filter(|x| x.is_absolute()),
        cut: Cell::new(false),
        starts: Cell::new(0),
        held: Some(held),
        #[cfg(test)]
        refused: None,
    };
    work_reading(dir, &reads)
}

/// [`read_work`]'s answer over `reads`.
fn work_reading(dir: &Path, reads: &Reads<'_>) -> WorkRead {
    match work_reads(dir, reads) {
        Some(record) if !reads.cut.get() => WorkRead::Read(Box::new(WorkRecord {
            read_ms: aterm_time::unix_millis().unwrap_or(0),
            starts: reads.starts.get(),
            ..record
        })),
        _ if reads.cut.get() => WorkRead::Unread,
        _ => WorkRead::None,
    }
}

/// The record's reads, in order, each stopping the read where it gives nothing: the folder
/// checked, then git's common dir and the worktree's top level (each checked as the folder is),
/// the branch (or a detached `HEAD`'s commit), the main ref, the base and the behind count, the
/// names committed since the base and the working tree's.
fn work_reads(dir: &Path, reads: &Reads<'_>) -> Option<WorkRecord> {
    let home = reads.home;
    let table = aterm_sysprobe::volumes::table();
    let dir = unattended_dir(dir, Some(home), &table)?;
    let located = git(
        &dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--show-toplevel",
        ],
        reads,
    )?;
    let (common, top) = located_dirs(&located, &dir)?;
    let root = unattended_dir(&top, Some(home), &table)?;
    let common_dir = unattended_dir(&common, Some(home), &table)?;
    // A branch name in bytes that are no UTF-8 still names its branch, as the probe reads it.
    let branch = git_lossy(
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        reads,
        Diagnostics::Permit,
    )
    .map(|b| b.trim().to_string())
    .filter(|b| !b.is_empty());
    let detached = if branch.is_none() {
        git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"], reads)
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| is_head(h))
    } else {
        None
    };
    let main = main_ref(&root, reads);
    if reads.cut.get() {
        return None;
    }
    let (base, behind) = match main.as_deref() {
        Some(full) => ancestry(&root, full, reads)?,
        None => (None, None),
    };
    let committed = match &base {
        // Names alone, between two commits: no patch is made, so no diff driver or text filter
        // the repository's configuration names runs.
        Some(base) => {
            let names = run_git_bytes(
                &root,
                &[
                    "diff-tree",
                    "-r",
                    "-z",
                    "--name-only",
                    "--no-renames",
                    "--no-ext-diff",
                    "--no-textconv",
                    base,
                    "HEAD",
                ],
                reads,
                false,
                true,
                None,
                Diagnostics::Refuse,
                None,
            );
            let Some(names) = names else {
                // A listing git refused could name fewer files than the branch changes.
                reads.cut.set(true);
                return None;
            };
            String::from_utf8_lossy(&names).into_owned()
        }
        None => String::new(),
    };
    let (status, _) = work_status(&root, &table, reads)?;
    let (paths, files) = changed_names(&committed, &status);
    Some(WorkRecord {
        host: String::new(),
        common_dir,
        tree: root,
        branch,
        detached,
        main_ref: main
            .as_deref()
            .map(|full| {
                full.strip_prefix("refs/remotes/")
                    .unwrap_or(full)
                    .to_string()
            })
            .unwrap_or_default(),
        base,
        behind,
        paths,
        files,
        read_ms: 0,
        starts: 0,
    })
}

/// The merge-base of `HEAD` and the main ref `full`, full lowercase hex, and how many commits on
/// `full` `HEAD` lacks, both read offline (they walk commits alone, so a partial clone fetches
/// nothing). With no commit in common (git's quiet "none") there is no base and so nothing behind
/// it, and neither is there where `HEAD`'s branch has no commit yet ([`branch_unborn`]), whose
/// merge-base git refuses. `None`, with [`Reads::cut`] set, where git refused either otherwise:
/// a base or count it could not make would hide every commit the branch changes.
fn ancestry(root: &Path, full: &str, reads: &Reads<'_>) -> Option<(Option<String>, Option<u32>)> {
    let Some(base) = git_offline_answer(root, &["merge-base", full, "HEAD"], reads) else {
        if reads.cut.get() {
            if !branch_unborn(root, reads) {
                return None;
            }
            reads.cut.set(false);
        }
        return Some((None, None));
    };
    let base = base.trim().to_ascii_lowercase();
    let behind = git_offline_answer(
        root,
        &["rev-list", "--count", &format!("HEAD..{full}")],
        reads,
    )
    .and_then(|n| crate::parse_uint::<u32>(n.trim()));
    if !is_head(&base) || behind.is_none() {
        reads.cut.set(true);
        return None;
    }
    Some((Some(base), behind))
}

/// Git's common dir and the worktree's top level from `rev-parse --path-format=absolute
/// --git-common-dir --show-toplevel` run in `dir` (canonical). A git before 2.31 knows no
/// `--path-format`: it prints the option back first and the common dir as it lies from `dir`
/// (`.git`, `../.git`), which is joined to `dir` here, each `..` taking `dir`'s parent: `dir` is
/// canonical, so that is where it leads.
fn located_dirs(located: &str, dir: &Path) -> Option<(PathBuf, PathBuf)> {
    use std::path::Component;
    let mut lines = located
        .lines()
        .filter(|line| *line != "--path-format=absolute");
    let (common, top) = (lines.next()?, lines.next()?);
    if common.is_empty() || top.is_empty() {
        return None;
    }
    let common = Path::new(common);
    let common = if common.is_absolute() {
        common.to_path_buf()
    } else {
        let mut joined = dir.to_path_buf();
        for part in common.components() {
            match part {
                Component::ParentDir => {
                    if !joined.pop() {
                        return None;
                    }
                }
                Component::Normal(name) => joined.push(name),
                Component::CurDir => {}
                Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        joined
    };
    Some((common, PathBuf::from(top)))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "repo_work_record_tests.rs"]
mod tests;
