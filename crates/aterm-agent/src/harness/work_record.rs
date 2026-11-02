// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where an agent session works ([`WorkRecord`]): its repository, worktree and branch, the
//! branch's base on origin's default branch and how far behind that it is, and the names of the
//! files the branch changes. The host reads it with git under the repository probe's own rules
//! ([`crate::harness::repo_work::read_work`]) and shows it in `survey`'s content rows.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_phase::Program;

/// The most file names a record holds ([`WorkRecord::paths`]); [`WorkRecord::files`] counts on
/// past them.
pub const WORK_PATHS: usize = 64;

/// A session's record is read at most once per this at its turn breaks, after the read at its
/// first look.
pub const WORK_FLOOR: Duration = Duration::from_secs(20);

/// Where one agent session works, as one read found it ([`WorkRead::Read`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkRecord {
    /// The host name of the aterm that read it, set where the record is published: a repository
    /// is one host's ([`RepoKey`]).
    pub host: String,
    /// Git's common directory, absolute and canonical: one per repository, shared by its
    /// worktrees.
    pub common_dir: PathBuf,
    /// The worktree's top level, canonical.
    pub tree: PathBuf,
    /// The branch checked out; `None` while `HEAD` is detached or names no branch.
    pub branch: Option<String>,
    /// The commit a detached `HEAD` names, full lowercase hex; `None` on a branch.
    pub detached: Option<String>,
    /// The remote default branch the base is on (`origin/main`); empty where the repository has
    /// none.
    pub main_ref: String,
    /// The merge-base of `HEAD` and [`Self::main_ref`], full lowercase hex; `None` with no main
    /// ref, no commit yet, or no commit in common.
    pub base: Option<String>,
    /// Commits on [`Self::main_ref`] that `HEAD` lacks, counted where there is a [`Self::base`];
    /// `None` with none.
    pub behind: Option<u32>,
    /// The names of the files the branch changes, sorted: those changed since [`Self::base`] and
    /// those the working tree changes or adds, repository-relative and lossily decoded, at most
    /// [`WORK_PATHS`].
    pub paths: Vec<String>,
    /// How many distinct files it changes, past [`WORK_PATHS`] too.
    pub files: u32,
    /// When the read ended, in Unix milliseconds.
    pub read_ms: u64,
    /// The git processes the read started itself. The gits those start are not in it: a status
    /// starts one more in each populated submodule.
    pub starts: u32,
}

/// What "one repository" means: one host's canonical common dir.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoKey {
    /// [`WorkRecord::host`].
    pub host: String,
    /// [`WorkRecord::common_dir`].
    pub common_dir: PathBuf,
}

impl WorkRecord {
    /// The repository this record is in ([`RepoKey`]).
    #[must_use]
    pub fn key(&self) -> RepoKey {
        RepoKey {
            host: self.host.clone(),
            common_dir: self.common_dir.clone(),
        }
    }
}

/// What one read of a session's work record found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WorkRead {
    /// The record.
    Read(Box<WorkRecord>),
    /// Nothing to show: no repository, a folder macOS guards or one the probe never enters, a
    /// summary worker's folder, no `git`. A record shown before is cleared.
    #[default]
    None,
    /// The read did not finish: one of its reads the hang detector cut or a hold on new starts
    /// stopped, one git could not start or a signal ended, a listing or count git refused or
    /// warned about. The last record stands, and its age grows.
    Unread,
}

impl crate::harness::repo_work::ProbeAnswer for WorkRead {
    fn unread() -> Self {
        Self::Unread
    }
}

/// What the host knows of where a session's agent works ([`work_dir`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Folders<'a> {
    /// The directory the agent was started in, as its relaunch snapshot names it.
    pub started: Option<&'a str>,
    /// The tail of Claude Code's transcript, whose last `cwd` is where it works now
    /// ([`crate::harness::repo_work::session_dir`]).
    pub transcript: Option<&'a str>,
    /// The cwd codex's rollout names.
    pub rollout: Option<&'a Path>,
    /// The terminal's own working directory.
    pub engine: Option<&'a Path>,
}

/// The folder a session of `agent` works in: Claude Code's transcript's last `cwd`, else where it
/// started; codex's rollout cwd, else where it started; any other agent's start folder; and for
/// every agent, else the terminal's own. Only an absolute path is chosen.
#[must_use]
pub fn work_dir(agent: Program, at: Folders<'_>) -> Option<PathBuf> {
    let absolute = |p: &Path| p.is_absolute().then(|| p.to_path_buf());
    let started = at.started.map(Path::new).and_then(absolute);
    let chosen = match agent {
        Program::Claude => {
            crate::harness::repo_work::session_dir(at.started.unwrap_or_default(), at.transcript)
        }
        Program::Codex => at.rollout.and_then(absolute).or(started),
        Program::Agy | Program::Muse | Program::Grok | Program::Kimi | Program::Generic => started,
    };
    chosen.or_else(|| at.engine.and_then(absolute))
}

/// The names of the files a branch changes, sorted, at most [`WORK_PATHS`], and how many there
/// are: the union of `committed` (`git diff-tree -r -z --name-only` output) and the paths
/// `status` (`git status --porcelain=v1 -z`) names, a rename's or copy's both paths (in either
/// column, [`crate::harness::repo_work::moved`]), an ignored entry none.
#[must_use]
pub fn changed_names(committed: &str, status: &str) -> (Vec<String>, u32) {
    let mut names = std::collections::BTreeSet::new();
    names.extend(committed.split('\0').filter(|n| !n.is_empty()));
    let mut fields = status.split('\0');
    while let Some(entry) = fields.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else {
            continue;
        };
        if crate::harness::repo_work::moved(code)
            && let Some(from) = fields.next()
            && !from.is_empty()
        {
            names.insert(from);
        }
        if code != "!!" && !path.is_empty() {
            names.insert(path);
        }
    }
    let files = u32::try_from(names.len()).unwrap_or(u32::MAX);
    let paths = names
        .into_iter()
        .take(WORK_PATHS)
        .map(str::to_string)
        .collect();
    (paths, files)
}

#[cfg(test)]
#[path = "work_record_tests.rs"]
mod tests;
