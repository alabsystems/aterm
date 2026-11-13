// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which repository the build tree is a worktree of, decided when the tool
//! runs: never a path compiled in, which goes stale the day the checkout that
//! built the tool is moved or removed.
//!
//! In order: `--repo`; the path recorded beside the kept tool
//! (`%LOCALAPPDATA%\aterm\build\repo.txt`, written when the build tree is
//! made and when the task is scheduled); the repository the build tree
//! already belongs to; the aterm checkout the tool was started in. Whichever
//! answers, the repository is named by its main checkout, so one found
//! through a linked worktree outlives that worktree.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::proc::{self, Place};
use crate::smoke::one_line;
use crate::update::Layout;

/// The longest a git command that reads or writes only this machine may take.
pub(crate) const GIT_LOCAL: Duration = Duration::from_secs(120);
/// The longest a git command that talks to a remote may take.
pub(crate) const GIT_NETWORK: Duration = Duration::from_secs(1800);

/// `git -C dir args…`; its standard output, trimmed.
pub(crate) fn git(dir: &Path, args: &[&str], within: Duration) -> Result<String, String> {
    let mut all: Vec<&std::ffi::OsStr> = vec!["-C".as_ref(), dir.as_os_str()];
    all.extend(args.iter().map(|a| std::ffi::OsStr::new(*a)));
    let ran = proc::run(Path::new("git"), &all, Place::default(), within)?;
    if ran.ok() {
        Ok(ran.stdout.trim().to_string())
    } else {
        Err(format!(
            "git {} in {}: {} (exit {:?})",
            args.join(" "),
            dir.display(),
            one_line(&ran.text(), 240),
            ran.code
        ))
    }
}

/// Where the repository's name came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `--repo`.
    Flag,
    /// The record beside the kept tool.
    Recorded,
    /// The build tree, which is a worktree of it.
    Tree,
    /// The checkout the tool was started in.
    Here,
}

impl Source {
    /// The source, for a person.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Flag => "--repo",
            Self::Recorded => "the recorded repository",
            Self::Tree => "the build tree's repository",
            Self::Here => "the checkout this was started in",
        }
    }
}

/// The repository the build tree is a worktree of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// Its main checkout, absolute.
    pub dir: PathBuf,
    /// Where the name came from.
    pub from: Source,
}

/// The first `worktree <path>` of `git worktree list --porcelain`: the main one.
fn first_worktree(listing: &str) -> Option<&str> {
    listing
        .lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// The main checkout of the repository `dir` is in: `dir` itself, or the
/// checkout a linked worktree hangs off.
///
/// # Errors
/// `dir` is not in a git repository.
pub fn main_checkout(dir: &Path) -> Result<PathBuf, String> {
    let listing = git(dir, &["worktree", "list", "--porcelain"], GIT_LOCAL)?;
    let main = first_worktree(&listing)
        .ok_or_else(|| format!("git names no worktree for {}", dir.display()))?;
    std::path::absolute(main).map_err(|e| format!("{main}: {e}"))
}

/// The directory all of a repository's worktrees share, as the one spelling
/// two worktrees of it agree on.
fn common_dir(dir: &Path) -> Result<PathBuf, String> {
    let said = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        GIT_LOCAL,
    )?;
    std::fs::canonicalize(&said).map_err(|e| format!("{said}: {e}"))
}

/// Whether `a` and `b` are worktrees of one repository.
///
/// # Errors
/// Either is not in a git repository.
pub fn same(a: &Path, b: &Path) -> Result<bool, String> {
    Ok(common_dir(a)? == common_dir(b)?)
}

/// The recorded repository, where there is a record.
#[must_use]
pub fn recorded(layout: &Layout) -> Option<PathBuf> {
    let text = std::fs::read_to_string(layout.repo_record()).ok()?;
    let line = text.trim_start_matches('\u{feff}').trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// Records `repo` beside the kept tool, for the runs that are given no `--repo`.
///
/// # Errors
/// The record could not be written.
pub fn record(layout: &Layout, repo: &Path) -> Result<(), String> {
    let path = layout.repo_record();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    aterm_uds::atomic_file::write(&path, format!("{}\n", repo.display()).as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether `top` is an aterm checkout: it carries the Windows build script.
fn is_aterm(top: &Path) -> bool {
    top.join("apps")
        .join("aterm-win")
        .join("build.ps1")
        .is_file()
}

/// Decides the repository: `flag`, the record, the build tree's own, then the
/// aterm checkout `cwd` is in.
///
/// # Errors
/// `flag` or the record names something that is not a git repository, or
/// nothing names one.
pub fn resolve(flag: Option<&Path>, layout: &Layout, cwd: &Path) -> Result<Repo, String> {
    if let Some(flag) = flag {
        let dir = std::path::absolute(cwd.join(flag))
            .map_err(|e| format!("--repo {}: {e}", flag.display()))?;
        let dir = main_checkout(&dir)
            .map_err(|e| format!("--repo {} is not a git repository: {e}", dir.display()))?;
        return Ok(Repo {
            dir,
            from: Source::Flag,
        });
    }
    if let Some(named) = recorded(layout) {
        let dir = main_checkout(&named).map_err(|e| {
            format!(
                "the repository recorded in {} is not a git repository (name another with --repo <dir>): {e}",
                layout.repo_record().display()
            )
        })?;
        return Ok(Repo {
            dir,
            from: Source::Recorded,
        });
    }
    let src = layout.src();
    if src.join(".git").exists()
        && let Ok(dir) = main_checkout(&src)
    {
        return Ok(Repo {
            dir,
            from: Source::Tree,
        });
    }
    if let Ok(top) = git(cwd, &["rev-parse", "--show-toplevel"], GIT_LOCAL)
        && is_aterm(Path::new(&top))
        && let Ok(dir) = main_checkout(Path::new(&top))
    {
        return Ok(Repo {
            dir,
            from: Source::Here,
        });
    }
    Err(format!(
        "no repository: name it with --repo <dir> (none is recorded in {}, there is no build tree at {}, and {} is not in an aterm checkout)",
        layout.repo_record().display(),
        src.display(),
        cwd.display()
    ))
}

#[cfg(test)]
#[path = "repo_tests.rs"]
mod tests;
