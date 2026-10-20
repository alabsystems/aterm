// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A SESSION'S REPOSITORY HOLDS THAT NO REMOTE HAS: the probe behind the
//! turn-end policy's uncommitted-work challenge
//! (`supervise::policy::turn_end::RULE_UNCOMMITTED`; the owner's rule of
//! 2026-10-19: `ATERM-TASK-DONE` never ends a job while any uncommitted work
//! or follow-up is left).
//!
//! [`probe`] runs in the host, never in the pure policy: `git` through
//! `aterm_uds::spawnfd::command`, each call bounded by what is left of one
//! limit, in the session's working directory as the host knows it (the
//! transcript's `cwd`, else the session's own). It looks for
//!
//! * tracked changes, staged or not (`git status --porcelain -z`, `XY` other
//!   than `??`);
//! * untracked files git does not ignore (`??`);
//! * stash entries (`git stash list`);
//! * commits on `HEAD` that no remote-tracking ref contains (`git rev-list
//!   --count HEAD --not --remotes`) — counted only where the repository has
//!   a remote-tracking ref at all: a repository with no remote has nowhere
//!   to push, and every commit of it would read as unpushed.
//!
//! A probe that cannot run — no `git`, no repository, a folder macOS guards
//! with a consent dialog (`atpkg::protected`: the probe is unattended, and
//! must never put one on the owner's screen), a call past its limit — returns
//! `None`, as does a repository with nothing in it no remote has. `None` is
//! not evidence that the repository is clean. The reads take no optional lock
//! (`GIT_OPTIONAL_LOCKS=0`) and disable fsmonitor, hooks and pagers. Inherited
//! environment and system/global Git configuration are not passed through;
//! repository-local configuration, including clean/process filters, still applies.
//! Before Git runs, working and top-level paths are resolved component by
//! component on mounts a complete table calls local and non-automounted.
//! Missing home, unknown resolution and guarded physical paths refuse the
//! probe. This does not fence paths local Git configuration or filters access,
//! or make path lookup atomic against a concurrent replacement.
//!
//! [`RepoWork`] is the pure value the policy reads
//! (`TurnEndReading::repo`), and [`RepoWork::of_git`] its parse of the
//! reads' output, which the tests drive directly.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// How long the whole probe may take ([`probe`]): every call shares it.
pub const PROBE_LIMIT: Duration = Duration::from_secs(10);

/// The most paths a finding names ([`RepoWork::paths`]).
pub const NAMED_PATHS: usize = 3;

/// The longest path, in characters, a finding names whole: a longer one
/// keeps its last characters after `…`.
const PATH_CHARS: usize = 72;

/// WHAT ONE REPOSITORY HOLDS THAT NO REMOTE HAS ([`probe`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoWork {
    /// The repository's top-level directory, as `git rev-parse
    /// --show-toplevel` names it.
    pub repo: String,
    /// Tracked paths with changes, staged or not.
    pub changed: usize,
    /// Untracked paths git does not ignore (a directory with nothing tracked
    /// in it is one).
    pub untracked: usize,
    /// Stash entries.
    pub stashes: usize,
    /// Commits on `HEAD` no remote-tracking ref contains.
    pub unpushed: usize,
    /// The first [`NAMED_PATHS`] changed or untracked paths, each printable
    /// ([`printable`]).
    pub paths: Vec<String>,
}

impl RepoWork {
    /// Whether anything was found.
    #[must_use]
    pub fn found(&self) -> bool {
        self.changed > 0 || self.untracked > 0 || self.stashes > 0 || self.unpushed > 0
    }

    /// The finding of the reads' output: `repo` the top level, `status` the
    /// NUL-separated `git status --porcelain -z`, `stash` the lines of `git
    /// stash list`, `unpushed` the count of commits no remote-tracking ref
    /// contains. `None` where nothing was found.
    #[must_use]
    pub fn of_git(repo: &str, status: &str, stash: &str, unpushed: usize) -> Option<Self> {
        let mut work = Self {
            repo: printable(repo.trim_end_matches(['\n', '\r'])),
            stashes: stash.lines().filter(|l| !l.trim().is_empty()).count(),
            unpushed,
            ..Self::default()
        };
        let mut fields = status.split('\0');
        while let Some(entry) = fields.next() {
            let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else {
                continue;
            };
            if code == "??" {
                work.untracked += 1;
            } else if code != "!!" {
                work.changed += 1;
                // A rename or a copy names its source in the next field.
                if code.starts_with(['R', 'C']) {
                    fields.next();
                }
            } else {
                continue;
            }
            if work.paths.len() < NAMED_PATHS && !path.is_empty() {
                work.paths.push(printable(path));
            }
        }
        work.found().then_some(work)
    }

    /// The finding in a few words, for the challenge and the journal:
    /// `/r: 2 changed, 1 untracked (a.rs, b/, …), 1 stash, 3 unpushed
    /// commits`.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.changed > 0 {
            parts.push(format!("{} changed", self.changed));
        }
        if self.untracked > 0 {
            parts.push(format!("{} untracked", self.untracked));
        }
        let mut text = parts.join(", ");
        if !self.paths.is_empty() {
            let more = if self.changed.saturating_add(self.untracked) > self.paths.len() {
                ", …"
            } else {
                ""
            };
            text = format!("{text} ({}{more})", self.paths.join(", "));
        }
        let mut tail: Vec<String> = Vec::new();
        if self.stashes > 0 {
            tail.push(format!(
                "{} stash{}",
                self.stashes,
                if self.stashes == 1 { "" } else { "es" }
            ));
        }
        if self.unpushed > 0 {
            tail.push(format!(
                "{} unpushed commit{}",
                self.unpushed,
                if self.unpushed == 1 { "" } else { "s" }
            ));
        }
        let all: Vec<String> = [text]
            .into_iter()
            .filter(|t| !t.is_empty())
            .chain(tail)
            .collect();
        format!("{}: {}", self.repo, all.join(", "))
    }
}

/// THE DIRECTORY THE SESSION WORKS IN, for [`probe`]: the last `cwd` the
/// conversation's transcript tail `tail` records (Claude Code writes the
/// shell's working directory on each row, so a `cd` into a worktree moves
/// it), else the directory the agent was started in (`started`, the
/// process's own). `None` where neither names an absolute directory.
#[must_use]
pub fn session_dir(started: &str, tail: Option<&str>) -> Option<std::path::PathBuf> {
    let written = tail.and_then(|t| {
        t.lines().rev().find_map(|line| {
            if !line.contains("\"cwd\"") {
                return None;
            }
            let v = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
            v.get("cwd")
                .and_then(aterm_json::Value::as_str)
                .map(str::to_string)
        })
    });
    [written.as_deref(), Some(started)]
        .into_iter()
        .flatten()
        .map(Path::new)
        .find(|p| p.is_absolute())
        .map(Path::to_path_buf)
}

/// `text` with every control character, and every character that is no
/// printable one, as `?`, and kept to [`PATH_CHARS`] characters (its end,
/// after `…`): a path a typed line names can neither end the line nor fill
/// the composer.
#[must_use]
pub fn printable(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    let n = clean.chars().count();
    if n <= PATH_CHARS {
        return clean;
    }
    let tail: String = clean.chars().skip(n - (PATH_CHARS - 1)).collect();
    format!("…{tail}")
}

/// WHAT THE REPOSITORY `dir` IS IN HOLDS THAT NO REMOTE HAS, by `git`, the
/// whole probe within `limit`: `None` where nothing is found or the probe
/// cannot run (the module's rules). `home` is the person's home, for the
/// folders macOS guards ([`atpkg::protected::under_protected_root`]).
#[must_use]
pub fn probe(dir: &Path, home: Option<&Path>, limit: Duration) -> Option<RepoWork> {
    if home.is_none() || home.is_some_and(|h| atpkg::protected::under_protected_root(h, dir)) {
        return None;
    }
    let deadline = Instant::now().checked_add(limit)?;
    let table = aterm_sysprobe::volumes::table();
    let dir = unattended_dir(dir, home, &table)?;
    let top = git(&dir, &["rev-parse", "--show-toplevel"], deadline)?;
    let top = top.trim_end_matches(['\n', '\r']).to_string();
    if top.is_empty() {
        return None;
    }
    let root = unattended_dir(Path::new(&top), home, &table)?;
    let status = git(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        deadline,
    )?;
    let stash = git(&root, &["stash", "list"], deadline).unwrap_or_default();
    let remotes = git(
        &root,
        &[
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "refs/remotes",
        ],
        deadline,
    )
    .unwrap_or_default();
    let unpushed = if remotes.trim().is_empty() {
        0
    } else {
        git(
            &root,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
            deadline,
        )
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
    };
    RepoWork::of_git(&top, &status, &stash, unpushed)
}

/// Resolve the home and path before applying the lexical protected-root
/// predicate. The existing resolver refuses incomplete, remote or automounted
/// placement before each metadata/readlink; local OS calls can still stall and
/// a mount table or path can change after the check. No file data is read here.
fn unattended_dir(
    dir: &Path,
    home: Option<&Path>,
    table: &aterm_sysprobe::volumes::Table,
) -> Option<PathBuf> {
    let home = home?;
    if !home.is_absolute()
        || !dir.is_absolute()
        || home
            .components()
            .chain(dir.components())
            .any(|c| c == Component::ParentDir)
        || atpkg::protected::under_protected_root(home, dir)
    {
        return None;
    }
    let home = super::disk::idle::resolve_local(home, table)?;
    let dir = super::disk::idle::resolve_local(dir, table)?;
    (!atpkg::protected::under_protected_root(&home, &dir)).then_some(dir)
}

/// `git <args>` in `dir`, with complete UTF-8 stdout and a successful exit.
/// Every child shares `deadline`; expired calls do not start. The existing
/// capture caps stdout and stderr memory and refuses missing stdout EOF instead
/// of joining a reader forever. Its drain grace and bounded kill/reap reserve
/// may extend past the execution deadline; refusal does not prove descendant exit.
fn git(dir: &Path, args: &[&str], deadline: Instant) -> Option<String> {
    if Instant::now() >= deadline {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let mut cmd = aterm_uds::spawnfd::command("git");
    cmd.arg("--no-pager")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    let output = aterm_uds::deadline::output_until_complete_stdout(&mut cmd, deadline).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
#[path = "repo_work_tests.rs"]
mod tests;
