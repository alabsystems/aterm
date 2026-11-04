// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a session's repository holds that no remote main has, and its hygiene for the fact
//! menus.

use std::cell::Cell;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

/// How long each read of [`probe`] may take, the one-minute hang detector: a
/// read that ends gives the next this long, and a read cut by it makes the
/// probe [`RepoRead::Unread`].
pub const PROBE_LIMIT: Duration = Duration::from_secs(60);

/// How long one ask of a probe read beside its asker waits for the answer
/// ([`Probing::ask`]): the loop that asks renews its claim between asks, well
/// inside the claim's lease.
pub const PROBE_ASK: Duration = Duration::from_secs(5);

/// How often an ask that waits looks at its asker's stop ([`Probing::ask`]).
const STOP_LOOK: Duration = Duration::from_millis(100);

/// The most paths a finding names ([`RepoWork::paths`]).
pub const NAMED_PATHS: usize = 3;

/// The most files the work's state is read over ([`RepoWork::state`]): past that it is not read.
pub const STATE_FILES: usize = 4096;

pub(crate) const PATH_CHARS: usize = 72;

/// What one repository holds that no remote main has ([`probe`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoWork {
    /// The repository's top-level directory, as `git rev-parse --show-toplevel` names it.
    pub repo: String,
    /// Tracked paths with changes, staged or not (a submodule whose checkout only lags the commit
    /// recorded for it is none).
    pub changed: usize,
    /// Untracked paths git does not ignore (a directory with nothing tracked in it is one).
    pub untracked: usize,
    /// Stash entries.
    pub stashes: usize,
    /// Commits on `HEAD` no remote-tracking ref contains.
    pub unpushed: usize,
    /// Commits on `HEAD` the remote's default branch ([`Self::main_ref`]) does not contain.
    pub unlanded: usize,
    /// The remote default branch [`Self::unlanded`] counts against.
    pub main_ref: String,
    /// Commits a populated submodule's checkout, or the commit recorded for it, holds that no
    /// remote of the submodule has, at every level of submodules: a pushed record of one nobody
    /// can fetch.
    pub sub_unpushed: usize,
    /// The submodules [`Self::sub_unpushed`] counts, the first [`NAMED_PATHS`], each printable.
    pub subs: Vec<String>,
    /// Commits on another branch the session used ([`Used`]), local or `origin`'s of that name,
    /// that neither `HEAD` nor the remote main ([`Self::main_ref`]; with none, any remote) has.
    pub parked: usize,
    /// The branches [`Self::parked`] counts, the first [`NAMED_PATHS`] by name, each printable.
    pub parked_on: Vec<String>,
    /// The first [`NAMED_PATHS`] changed or untracked paths, each printable ([`printable`]).
    pub paths: Vec<String>,
    /// A hash of the work ([`state_of`]); `None` when not read (cut short, or past
    /// [`STATE_FILES`] paths), and then no other finding is the same ([`Self::same`]).
    pub state: Option<u64>,
}

impl RepoWork {
    /// Whether anything was found.
    #[must_use]
    pub const fn found(&self) -> bool {
        self.changed > 0
            || self.untracked > 0
            || self.stashes > 0
            || self.unpushed > 0
            || self.unlanded > 0
            || self.sub_unpushed > 0
            || self.parked > 0
    }

    /// The finding of `git status --porcelain=v1 -z` and stash-list output; `None` for nothing.
    #[must_use]
    pub fn of_git(
        repo: &str,
        status: &str,
        stash: &str,
        unpushed: usize,
        unlanded: usize,
        main_ref: &str,
    ) -> Option<Self> {
        let mut work = Self {
            repo: printable(repo.trim_end_matches(['\n', '\r'])),
            stashes: stash.lines().filter(|l| !l.trim().is_empty()).count(),
            unpushed,
            unlanded,
            main_ref: printable(main_ref),
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
                if moved(code) {
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

    /// Whether two probes found the same work: both nothing, or equal findings of a known state.
    #[must_use]
    pub fn same(a: Option<&Self>, b: Option<&Self>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a.state.is_some() && a == b,
            _ => false,
        }
    }

    /// The finding in a few words.
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
            let named = files_list(&self.paths, self.changed.saturating_add(self.untracked));
            text = format!("{text} ({named})");
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
        if self.sub_unpushed > 0 {
            tail.push(format!(
                "{} unpushed commit{} in {}",
                self.sub_unpushed,
                if self.sub_unpushed == 1 { "" } else { "s" },
                self.subs.join(", ")
            ));
        }
        if self.unlanded > 0 {
            tail.push(format!(
                "{} commit{} not on {}",
                self.unlanded,
                if self.unlanded == 1 { "" } else { "s" },
                if self.main_ref.is_empty() {
                    "the remote main"
                } else {
                    &self.main_ref
                }
            ));
        }
        if self.parked > 0 {
            tail.push(format!(
                "{} commit{} on {} not on {}",
                self.parked,
                if self.parked == 1 { "" } else { "s" },
                self.parked_on.join(", "),
                if self.main_ref.is_empty() {
                    "any remote"
                } else {
                    &self.main_ref
                }
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

/// THE FILES A PROBE NAMED, AS EVERY SURFACE LISTS THEM (one source: Land's prompt in
/// [`RepoWork::summary`], the hold's glass through `hold_said::files_words`): `a, b, c`, then
/// `, …` where it found `found` changed and untracked files, more than it named; nothing where
/// it named none.
#[must_use]
pub fn files_list(paths: &[String], found: usize) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let more = if found > paths.len() {
        ", \u{2026}"
    } else {
        ""
    };
    format!("{}{more}", paths.join(", "))
}

/// WHAT A CLEAN PROBE CHECKED ([`RepoRead::Checked`]; `docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §14.9, slice 9a): the repository it read, the commit its `HEAD` named, the remote main it
/// counted against and whether a remote was there to count against. The probe found nothing no
/// remote main has; these say what that was measured against, for a landing's receipt. No
/// decision reads them: a checked reading decides as a clean one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanAt {
    /// The repository's top-level directory, in [`RepoWork::repo`]'s form.
    pub repo: String,
    /// `HEAD`'s commit in full lowercase hex (`git rev-parse --verify --quiet HEAD`).
    pub head: String,
    /// The remote default branch commits off it were counted against (`origin/main`;
    /// [`RepoWork::main_ref`]); empty where the repository has none.
    pub main_ref: String,
    /// Whether the repository has a remote: one configured (`git remote`) or any remote-tracking
    /// ref (`for-each-ref refs/remotes`), each a read that answered (one git refused names no
    /// reading). `false`: there is nowhere to push, so no unpushed commit was counted.
    pub remote: bool,
    /// `HEAD`'s branch (`git symbolic-ref --short HEAD`, read lossily, in [`printable`]'s form),
    /// whose stashes the probe counted ([`stashes_on`]); [`DETACHED`] on a detached `HEAD`, and
    /// empty only in a reading taped before the branch was read (slice 9b's). A landing's receipt
    /// names it (slice 9c: `no stash on main`).
    pub branch: String,
}

/// A detached `HEAD`'s branch word: git's own, which its stash lines name (`On (no branch): …`,
/// [`stashes_on`]) and no branch can be named, a ref name holding no space.
pub const DETACHED: &str = "(no branch)";

/// What one probe of a session's repository read ([`probe`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RepoRead {
    /// Nothing no remote main has, and nothing checked to name: the probe
    /// cannot run here (no `git`, no `HOME`, no repository, a folder macOS
    /// guards, a network or automounted volume), so it read nothing; or it
    /// found nothing in a repository whose `HEAD` it could not name: no commit
    /// yet (an empty branch-ref read proves it when counts over `HEAD` fail),
    /// or a head read refused or cut once every count had answered. A refused read
    /// of a superproject, the configured remotes, a submodule or a branch the
    /// session used is [`Self::Unread`].
    #[default]
    Clean,
    /// Nothing no remote main has, in the repository and at the commit it names
    /// ([`CleanAt`]): the probe finished every read and found nothing. Every
    /// decision reads it as [`Self::Clean`] ([`Self::is_clean`], [`Self::same`]).
    Checked(CleanAt),
    /// What the repository holds that no remote main has.
    Found(RepoWork),
    /// Nothing to read: the session's folder is gone (`docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §14.17 item 4, slice 9c), named in [`printable`]'s form. Its path was absolute, outside
    /// every folder macOS guards and on a local volume, and it was not there to resolve
    /// ([`gone_dir`]). Every decision reads it as [`Self::Clean`] ([`Self::is_clean`],
    /// [`Self::same`]); only a landing's receipt names it (`the folder is gone`).
    Gone(String),
    /// A read of the probe did not end within its limit (a hung `git`), or
    /// the probe could not read the repository it found (a `git` that could not
    /// start for want of a file descriptor or process, one a signal ended,
    /// a status read a broken index or submodule fails, a count git refused at
    /// a commit `HEAD` names, as over a stash or remote-tracking ref to an
    /// object the repository lacks, or with no proof its unread `HEAD` is
    /// unborn): what the repository holds is not known.
    Unread,
}

impl RepoRead {
    /// `Some` finding as [`Self::Found`], `None` as [`Self::Clean`].
    #[must_use]
    pub fn of_found(found: Option<RepoWork>) -> Self {
        found.map_or(Self::Clean, Self::Found)
    }

    /// The finding, where the probe found work.
    #[must_use]
    pub const fn found(&self) -> Option<&RepoWork> {
        match self {
            Self::Found(work) => Some(work),
            Self::Clean | Self::Checked(_) | Self::Gone(_) | Self::Unread => None,
        }
    }

    /// The finding, owned ([`Self::found`]).
    #[must_use]
    pub fn into_found(self) -> Option<RepoWork> {
        match self {
            Self::Found(work) => Some(work),
            Self::Clean | Self::Checked(_) | Self::Gone(_) | Self::Unread => None,
        }
    }

    /// What a clean probe checked, where it named it ([`Self::Checked`]).
    #[must_use]
    pub const fn checked(&self) -> Option<&CleanAt> {
        match self {
            Self::Checked(at) => Some(at),
            Self::Clean | Self::Found(_) | Self::Gone(_) | Self::Unread => None,
        }
    }

    /// Whether the reading is clean: [`Self::Clean`] (nothing found, or no
    /// probe can run here), [`Self::Checked`] (nothing found at a named `HEAD`)
    /// or [`Self::Gone`] (no folder there to hold anything).
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        matches!(self, Self::Clean | Self::Checked(_) | Self::Gone(_))
    }

    /// Whether the probe could not read the repository ([`Self::Unread`]).
    #[must_use]
    pub const fn is_unread(&self) -> bool {
        matches!(self, Self::Unread)
    }

    /// Whether two probes read the same work: both clean ([`Self::is_clean`]: a
    /// checked or gone reading is the same as a clean one, whatever each names),
    /// or both the same finding of a known state ([`RepoWork::same`]). An unread
    /// probe never matches.
    #[must_use]
    pub fn same(a: &Self, b: &Self) -> bool {
        match (a, b) {
            (
                Self::Clean | Self::Checked(_) | Self::Gone(_),
                Self::Clean | Self::Checked(_) | Self::Gone(_),
            ) => true,
            (Self::Found(a), Self::Found(b)) => RepoWork::same(Some(a), Some(b)),
            _ => false,
        }
    }
}

/// Repository hygiene and drift from the main branch ([`probe_hygiene`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoHygiene {
    /// The main branch drift counts against: `origin/HEAD`'s target, `origin/main`,
    /// `origin/master`, else local `main` or `master`.
    pub main_ref: String,
    /// Commits on `HEAD` not in `main_ref`.
    pub ahead: usize,
    /// Commits on `main_ref` not in `HEAD`.
    pub behind: usize,
    /// Tracked paths with uncommitted changes.
    pub changed: usize,
    /// Untracked paths git does not ignore.
    pub untracked: usize,
    /// Stash entries on the current branch.
    pub stashes: usize,
    /// Commits on `HEAD` no remote-tracking ref contains.
    pub unpushed: usize,
    /// Total worktrees attached to this repository.
    pub worktrees: usize,
    /// Local branches with commits not yet merged into `main_ref`.
    pub unmerged_branches: Vec<String>,
    /// Local branches already merged into `main_ref` (besides `main`/`master` and the current
    /// branch) that can be cleaned up.
    pub stale_branches: Vec<String>,
    /// Bytes held across linked worktrees of this repository.
    pub worktree_bytes: u64,
    /// Bytes held in build and temp directories (`target`, `target-*`, `target.*`,
    /// `*.noindex`, `.tmp*`) of this repository.
    pub temp_bytes: u64,
    /// Free bytes available on the drive holding this repository.
    pub drive_free_bytes: Option<u64>,
}

fn join_named(names: &[String], cap: usize) -> String {
    let shown: Vec<&str> = names.iter().take(cap).map(String::as_str).collect();
    if names.len() > cap {
        format!("{}, \u{2026}", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

impl RepoHygiene {
    /// Parse `git` outputs into [`RepoHygiene`].
    #[must_use]
    pub fn of_git(
        main_ref: &str,
        branch: Option<&str>,
        ahead_behind: &str,
        worktree_list: &str,
        unmerged: &str,
        merged: &str,
    ) -> Self {
        let mut parts = ahead_behind.split_whitespace();
        let behind = parts.next().and_then(crate::parse_uint).unwrap_or(0);
        let ahead = parts.next().and_then(crate::parse_uint).unwrap_or(0);
        let worktrees = worktree_list
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count()
            .max(usize::from(!worktree_list.trim().is_empty()));
        let is_trunk = |b: &str| matches!(b, "main" | "master");
        let unmerged_branches = unmerged
            .lines()
            .map(str::trim)
            .filter(|b| !b.is_empty() && !is_trunk(b))
            .map(printable)
            .collect();
        let stale_branches = merged
            .lines()
            .map(str::trim)
            .filter(|b| !b.is_empty() && !is_trunk(b) && Some(*b) != branch)
            .map(printable)
            .collect();
        Self {
            main_ref: printable(main_ref.trim()),
            ahead,
            behind,
            changed: 0,
            untracked: 0,
            stashes: 0,
            unpushed: 0,
            worktrees,
            unmerged_branches,
            stale_branches,
            worktree_bytes: 0,
            temp_bytes: 0,
            drive_free_bytes: None,
        }
    }

    /// Attach local unsynced counts (`status`, `stash`, `unpushed`) and disk usage figures.
    #[must_use]
    pub fn with_local_and_disk(
        mut self,
        status: &str,
        stash: &str,
        unpushed: usize,
        worktree_bytes: u64,
        temp_bytes: u64,
        drive_free_bytes: Option<u64>,
    ) -> Self {
        if let Some(work) = RepoWork::of_git("/r", status, stash, unpushed, 0, "") {
            self.changed = work.changed;
            self.untracked = work.untracked;
            self.stashes = work.stashes;
            self.unpushed = work.unpushed;
        }
        self.worktree_bytes = worktree_bytes;
        self.temp_bytes = temp_bytes;
        self.drive_free_bytes = drive_free_bytes;
        self
    }

    fn unsynced_parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.changed > 0 {
            parts.push(format!("{} changed", self.changed));
        }
        if self.untracked > 0 {
            parts.push(format!("{} untracked", self.untracked));
        }
        if self.unpushed > 0 {
            parts.push(format!("{} unpushed", self.unpushed));
        }
        if self.stashes > 0 {
            parts.push(format!(
                "{} stash{}",
                self.stashes,
                if self.stashes == 1 { "" } else { "es" }
            ));
        }
        parts
    }

    fn disk_parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.worktree_bytes > 0 {
            parts.push(format!(
                "worktrees {}",
                super::disk::human_bytes(self.worktree_bytes)
            ));
        }
        if self.temp_bytes > 0 {
            parts.push(format!(
                "build/temp {}",
                super::disk::human_bytes(self.temp_bytes)
            ));
        }
        if let Some(free) = self.drive_free_bytes {
            parts.push(format!("{} free", super::disk::human_bytes(free)));
        }
        parts
    }

    /// Note rows for the branch fact menu (aterm-gui's `Fact::Branch`).
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        let unsynced = self.unsynced_parts();
        if !self.main_ref.is_empty() {
            let mut drift = match (self.ahead, self.behind) {
                (0, 0) if unsynced.is_empty() => {
                    format!("\u{2713}  in sync with {}", self.main_ref)
                }
                (0, 0) => format!("on {}", self.main_ref),
                (a, 0) => format!("\u{2191}{a} ahead of {}", self.main_ref),
                (0, b) => format!("\u{2193}{b} behind {}", self.main_ref),
                (a, b) => format!(
                    "\u{2191}{a} ahead \u{b7} \u{2193}{b} behind {}",
                    self.main_ref
                ),
            };
            if !unsynced.is_empty() {
                drift = format!("{drift} \u{b7} {}", unsynced.join(", "));
            }
            out.push(drift);
        } else if !unsynced.is_empty() {
            out.push(unsynced.join(" \u{b7} "));
        }
        let wt = self.worktrees.max(1);
        let mut hygiene = vec![format!("{wt} worktree{}", if wt == 1 { "" } else { "s" })];
        if !self.unmerged_branches.is_empty() {
            hygiene.push(format!(
                "{} unmerged ({})",
                self.unmerged_branches.len(),
                join_named(&self.unmerged_branches, 2)
            ));
        }
        if !self.stale_branches.is_empty() {
            hygiene.push(format!(
                "{} stale ({})",
                self.stale_branches.len(),
                join_named(&self.stale_branches, 2)
            ));
        } else if self.unmerged_branches.is_empty() {
            hygiene.push("no stale branches".to_string());
        }
        out.push(format!("\u{2387}  {}", hygiene.join(" \u{b7} ")));
        let disk = self.disk_parts();
        if !disk.is_empty() {
            out.push(format!("\u{26c1}  {}", disk.join(" \u{b7} ")));
        }
        out
    }

    /// Note rows for the folder fact menu (aterm-gui's `Fact::Path`).
    #[must_use]
    pub fn folder_notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        let wt = self.worktrees.max(1);
        let mut disk = vec![format!("{wt} worktree{}", if wt == 1 { "" } else { "s" })];
        disk.extend(self.disk_parts());
        out.push(format!("\u{26c1}  {}", disk.join(" \u{b7} ")));
        let unsynced = self.unsynced_parts();
        if !unsynced.is_empty() {
            out.push(format!("\u{2387}  {}", unsynced.join(" \u{b7} ")));
        }
        out
    }
}

/// The directory the session works in: the last `cwd` its transcript `tail` wrote, else
/// `started` (absolute paths only).
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

/// `text` with every control character as `?`, kept to [`PATH_CHARS`] characters (its end,
/// after `…`).
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

/// The most branches [`Used`] names: the newest.
const USED_BRANCHES: usize = 64;

/// What a session used beyond its folder's `HEAD` ([`probe_session`]): the branches it parked
/// work on are these, never another session's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Used {
    /// When the session began, in Unix seconds: the branches this worktree's `HEAD` reflog moved
    /// from or to after it are ones it used.
    pub since: Option<u64>,
    /// The branches its own commands created or checked out, newest first ([`branches_named`]).
    pub branches: Vec<String>,
}

impl Used {
    /// A session's: begun at the earlier of `started` (its agent's start, Unix seconds) and the
    /// first stamp its transcript `tail` holds, with the branches its Bash calls there named.
    #[must_use]
    pub fn of_session(started: Option<u64>, tail: Option<&str>) -> Self {
        let stamped = tail.and_then(|tail| {
            tail.lines()
                .filter(|line| line.contains("\"timestamp\""))
                .find_map(|line| {
                    let v = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
                    super::upgrade_models::parse_utc(v.get("timestamp")?.as_str()?)
                })
        });
        Self {
            since: started.into_iter().chain(stamped).min(),
            branches: tail.map(branches_named).unwrap_or_default(),
        }
    }
}

/// The branches the Bash calls in transcript `tail` created or checked out (`checkout`,
/// `switch`, `worktree add`, `branch`, through git's global options and chained commands),
/// newest first, each once, at most [`USED_BRANCHES`]; each a name [`branch_name_ok`] admits.
#[must_use]
pub fn branches_named(tail: &str) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    for line in tail.lines().filter(|l| l.contains("\"tool_use\"")) {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let Some(parts) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(aterm_json::Value::as_array)
        else {
            continue;
        };
        for part in parts {
            if part.get("type").and_then(aterm_json::Value::as_str) != Some("tool_use")
                || part.get("name").and_then(aterm_json::Value::as_str) != Some("Bash")
            {
                continue;
            }
            if let Some(command) = part
                .get("input")
                .and_then(|i| i.get("command"))
                .and_then(aterm_json::Value::as_str)
            {
                named.extend(git_branches_in(command));
            }
        }
    }
    let mut newest: Vec<String> = Vec::new();
    for name in named.into_iter().rev() {
        if newest.len() < USED_BRANCHES && branch_name_ok(&name) && !newest.contains(&name) {
            newest.push(name);
        }
    }
    newest
}

/// The words of each simple command in shell `text`, quotes and backslashes taken as the shell
/// takes them, split at `;`, `&`, `|`, parentheses and new lines.
fn shell_commands(text: &str) -> Vec<Vec<String>> {
    let (mut commands, mut words, mut word) = (Vec::new(), Vec::new(), String::new());
    let (mut quote, mut open) = (None::<char>, false);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => word.extend(chars.next()),
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                open = true;
            }
            (None, '\\') => {
                word.extend(chars.next());
                open = true;
            }
            (None, ';' | '&' | '|' | '(' | ')' | '\n') => {
                if open {
                    words.push(std::mem::take(&mut word));
                    open = false;
                }
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            (None, c) if c.is_whitespace() => {
                if open {
                    words.push(std::mem::take(&mut word));
                    open = false;
                }
            }
            (None, c) => {
                word.push(c);
                open = true;
            }
        }
    }
    if open {
        words.push(word);
    }
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

/// The branches the git commands in shell `command` create or check out, in order.
fn git_branches_in(command: &str) -> Vec<String> {
    let mut names = Vec::new();
    for words in shell_commands(command) {
        let mut words = words
            .iter()
            .map(String::as_str)
            .skip_while(|w| {
                matches!(*w, "env" | "command" | "exec" | "time")
                    || w.split_once('=').is_some_and(|(k, _)| {
                        !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    })
            })
            .peekable();
        if !words
            .next()
            .is_some_and(|w| w == "git" || w.ends_with("/git"))
        {
            continue;
        }
        while let Some(w) = words.next_if(|w| w.starts_with('-')) {
            if matches!(w, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
                words.next();
            }
        }
        let Some(verb) = words.next() else {
            continue;
        };
        let args: Vec<&str> = words.collect();
        names.extend(match verb {
            "checkout" | "switch" => checkout_branch(verb, &args),
            "worktree" => match args.split_first() {
                Some((&"add", rest)) => worktree_branch(rest),
                _ => None,
            },
            "branch" => created_branch(&args),
            _ => None,
        });
    }
    names
}

/// The branch `git checkout|switch <args>` creates or moves to: the one a create option names,
/// else its first word that is no option (none after `--`).
fn checkout_branch(verb: &str, args: &[&str]) -> Option<String> {
    let mut it = args.iter();
    let mut first = None;
    while let Some(&w) = it.next() {
        let create = match verb {
            "checkout" => matches!(w, "-b" | "-B" | "--orphan"),
            _ => matches!(w, "-c" | "-C" | "--create" | "--force-create" | "--orphan"),
        };
        if create {
            return it.next().map(|n| (*n).to_string());
        }
        let prefixes: &[&str] = match verb {
            "checkout" => &["-b", "-B", "--orphan="],
            _ => &["-c", "-C", "--create=", "--force-create=", "--orphan="],
        };
        if let Some(name) = prefixes
            .iter()
            .find_map(|p| w.strip_prefix(p))
            .filter(|s| !s.is_empty())
        {
            return Some(name.to_string());
        }
        if matches!(w, "--conflict" | "--pathspec-from-file") {
            it.next();
            continue;
        }
        if w == "--" {
            break;
        }
        if !w.starts_with('-') && first.is_none() {
            first = Some(w.to_string());
        }
    }
    first
}

/// The branch `git worktree add <args>` checks out: `-b`'s or `-B`'s, else the commit named after
/// the folder, else (not `--detach`ed) the folder's name.
fn worktree_branch(args: &[&str]) -> Option<String> {
    let (mut named, mut detached, mut words) = (None, false, Vec::new());
    let mut it = args.iter();
    while let Some(&w) = it.next() {
        match w {
            "--" => {
                words.extend(it.copied());
                break;
            }
            "-b" | "-B" => named = it.next().map(|n| (*n).to_string()),
            "-d" | "--detach" => detached = true,
            "--reason" => {
                it.next();
            }
            w if w.starts_with("-b") || w.starts_with("-B") => {
                named = Some(w[2..].to_string());
            }
            w if w.starts_with('-') => {}
            w => words.push(w),
        }
    }
    named
        .or_else(|| words.get(1).map(|w| (*w).to_string()))
        .or_else(|| {
            words
                .first()
                .filter(|_| !detached)
                .and_then(|path| Path::new(path).file_name())
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
}

/// The branch `git branch <args>` creates, or renames or copies to: none where it lists,
/// deletes or sets an upstream.
fn created_branch(args: &[&str]) -> Option<String> {
    let mut words = Vec::new();
    let mut moved = false;
    for &w in args {
        match w {
            "-m" | "-M" | "--move" | "-c" | "-C" | "--copy" => moved = true,
            "-d" | "-D" | "--delete" | "-l" | "--list" | "-a" | "--all" | "-r" | "--remotes"
            | "-v" | "-vv" | "--verbose" | "--merged" | "--no-merged" | "--contains"
            | "--no-contains" | "--points-at" | "--show-current" | "--edit-description" | "-u"
            | "--set-upstream-to" | "--unset-upstream" | "--format" | "--sort" | "--column" => {
                return None;
            }
            w if w.starts_with('-') => {
                if [
                    "--set-upstream-to=",
                    "--format=",
                    "--sort=",
                    "--column=",
                    "--merged=",
                    "--no-merged=",
                    "--contains=",
                    "--no-contains=",
                    "--points-at=",
                ]
                .iter()
                .any(|p| w.starts_with(p))
                {
                    return None;
                }
            }
            w => words.push(w),
        }
    }
    let named = if moved { words.last() } else { words.first() };
    named.map(|w| (*w).to_string())
}

/// Whether `name` is a branch name the probe reads as `refs/heads/<name>`: one `git
/// check-ref-format --branch` would take, never an option or a revision expression.
fn branch_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with(['-', '.', '/'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("@{")
        && !name.contains("//")
        && name != "@"
        && name != "HEAD"
        && !name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
}

/// [`probe`] of a session that used `used` beyond its folder's `HEAD`: the work it parked on
/// those branches counts too ([`RepoWork::parked`], their stash entries in [`RepoWork::stashes`]).
#[must_use]
pub fn probe_session(dir: &Path, home: Option<&Path>, used: &Used, limit: Duration) -> RepoRead {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    probe_in(dir, home, xdg.as_deref(), used, limit)
}

/// What the repository `dir` is in holds that no remote main has.
/// Each read has `limit` from the end of the one before it (the first, from
/// the call), so reads that each end in time are read whole however long they
/// take together. A read cut by its limit, and a repository whose reads could
/// not be made, is [`RepoRead::Unread`], not a clean result.
#[must_use]
pub fn probe(dir: &Path, home: Option<&Path>, limit: Duration) -> RepoRead {
    probe_session(dir, home, &Used::default(), limit)
}

/// [`probe`] with the person's `XDG_CONFIG_HOME` given (`None`: unset).
#[must_use]
pub fn probe_with(
    dir: &Path,
    home: Option<&Path>,
    xdg: Option<&Path>,
    limit: Duration,
) -> RepoRead {
    probe_in(dir, home, xdg, &Used::default(), limit)
}

fn probe_in(
    dir: &Path,
    home: Option<&Path>,
    xdg: Option<&Path>,
    used: &Used,
    limit: Duration,
) -> RepoRead {
    let Some(home) = home else {
        return RepoRead::Clean;
    };
    if atpkg::protected::under_protected_root(home, dir) {
        return RepoRead::Clean;
    }
    let Some(deadline) = Instant::now().checked_add(limit) else {
        return RepoRead::Clean;
    };
    let reads = Reads {
        deadline: Cell::new(deadline),
        each: Some(limit),
        home,
        xdg: xdg.filter(|x| x.is_absolute()),
        cut: Cell::new(false),
        starts: Cell::new(0),
        held: None,
        #[cfg(test)]
        refused: None,
    };
    probe_reading(dir, used, &reads)
}

fn probe_reading(dir: &Path, used: &Used, reads: &Reads<'_>) -> RepoRead {
    match probe_reads(dir, used, reads) {
        Some(read) => read,
        None if reads.cut.get() => RepoRead::Unread,
        None => RepoRead::Clean,
    }
}

/// An answer a [`Probing`] read gives.
pub trait ProbeAnswer: Send + 'static {
    /// The answer of a read whose thread could not start, or that panicked: nothing is known.
    fn unread() -> Self;
}

impl ProbeAnswer for RepoRead {
    fn unread() -> Self {
        Self::Unread
    }
}

/// A PROBE READ BESIDE ITS ASKER ([`crate::supervise::IdleHost::repo_work`], and the host's work
/// record): its reads go on on a thread of their own, and an ask waits a bounded time for their
/// answer, so the loop that asks keeps its claim and sees its stop between asks however long the
/// reads take together ([`PROBE_LIMIT`] bounds each read).
#[derive(Debug)]
pub struct Probing<T = RepoRead> {
    /// The answer of the probe that reads now, until an ask takes it.
    reading: Mutex<Option<mpsc::Receiver<T>>>,
    /// The name of the thread each read runs on.
    thread: String,
}

impl<T> Default for Probing<T> {
    fn default() -> Self {
        Self::named("aterm-repo-probe")
    }
}

impl<T> Probing<T> {
    /// A probing whose reads run on a thread named `thread`.
    #[must_use]
    pub fn named(thread: impl Into<String>) -> Self {
        Self {
            reading: Mutex::new(None),
            thread: thread.into(),
        }
    }
}

impl<T: ProbeAnswer> Probing<T> {
    /// The answer of the probe that reads now, or else of the one `begin`
    /// hands it, once that probe ends within `wait`; `None` while it reads on
    /// past `wait`, or once `stop` is raised, the probe left reading for the
    /// next ask. A probe whose thread could not start, or that panicked, is
    /// [`ProbeAnswer::unread`] ([`RepoRead::Unread`]).
    pub fn ask<F>(&self, begin: impl FnOnce() -> F, wait: Duration, stop: &AtomicBool) -> Option<T>
    where
        F: FnOnce() -> T + Send + 'static,
    {
        let mut reading = self.reading.lock().unwrap_or_else(PoisonError::into_inner);
        let answer = if let Some(answer) = reading.take() {
            answer
        } else {
            let read = begin();
            let (said, answer) = mpsc::sync_channel(1);
            let started = std::thread::Builder::new()
                .name(self.thread.clone())
                .spawn(move || {
                    if let Ok(found) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read))
                    {
                        let _ = said.send(found);
                    }
                });
            if started.is_err() {
                return Some(T::unread());
            }
            answer
        };
        let by = Instant::now().checked_add(wait);
        loop {
            let left = by.map_or(STOP_LOOK, |by| by.saturating_duration_since(Instant::now()));
            match answer.recv_timeout(left.min(STOP_LOOK)) {
                Ok(found) => return Some(found),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Some(T::unread()),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if stop.load(Ordering::SeqCst) || by.is_some_and(|by| Instant::now() >= by) {
                        *reading = Some(answer);
                        return None;
                    }
                }
            }
        }
    }
}

/// [`probe_in`]'s reads of the outermost project `dir` is in ([`outermost`]) and, where `dir`'s
/// repository is a submodule of it, of that one too: the first finding ([`RepoRead::Found`]),
/// or, where neither found anything, what the outermost read checked ([`RepoRead::Checked`]);
/// `None` for nothing to name. A finding whose state read was cut keeps its counts, its state
/// unknown ([`RepoWork::state`]).
fn probe_reads(dir: &Path, used: &Used, reads: &Reads<'_>) -> Option<RepoRead> {
    let home = reads.home;
    let table = aterm_sysprobe::volumes::table();
    let Some(dir) = unattended_dir(dir, Some(home), &table) else {
        return gone_dir(dir, home, &table).map(RepoRead::Gone);
    };
    let top = git(&dir, &["rev-parse", "--show-toplevel"], reads)?;
    let top = top.trim_end_matches(['\n', '\r']).to_string();
    if top.is_empty() {
        return None;
    }
    let root = unattended_dir(Path::new(&top), Some(home), &table)?;
    let (outer_top, outer_root) = outermost(&top, &root, &table, reads)?;
    let outer = repo_reads(&outer_top, &outer_root, used, &table, reads);
    if outer_root == root || matches!(outer, Some(RepoRead::Found(_))) {
        return outer;
    }
    // A session inside a submodule: its own commits that remote's main lacks are work the
    // project around it never counts. Either read cut is no clean reading.
    let cut = reads.cut.replace(false);
    let inner = repo_reads(&top, &root, used, &table, reads);
    if matches!(inner, Some(RepoRead::Found(_))) {
        return inner;
    }
    if cut {
        reads.cut.set(true);
    }
    if reads.cut.get() {
        return None;
    }
    inner.and(outer)
}

/// [`probe_reads`] of the repository at top level `top`, the folder `root`.
fn repo_reads(
    top: &str,
    root: &Path,
    used: &Used,
    table: &aterm_sysprobe::volumes::Table,
    reads: &Reads<'_>,
) -> Option<RepoRead> {
    let (top, root) = (top.to_string(), root.to_path_buf());
    let (status, excludes) = work_status(&root, table, reads)?;
    // The branch and the stash list are read lossily, alike: a branch name or
    // stash message in bytes that are no UTF-8 still names its branch, so a
    // stash written on this branch is counted and one written on another is
    // not ([`stashes_on`]).
    let branch = git_lossy(
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        reads,
        Diagnostics::Permit,
    )
    .map(|b| b.trim().to_string())
    .filter(|b| !b.is_empty());
    let listed = git_lossy(
        &root,
        &["stash", "list", "--format=%gs"],
        reads,
        Diagnostics::Refuse,
    );
    let used_on = used_branches(&root, used, branch.as_deref(), reads);
    let mut stash = stashes_on(listed.as_deref().unwrap_or_default(), branch.as_deref());
    for name in used_on.iter().flatten() {
        stash.push_str(&stashes_on(
            listed.as_deref().unwrap_or_default(),
            Some(name),
        ));
    }
    let remotes = git_count(
        &root,
        &[
            "for-each-ref",
            "--count=1",
            "--format=remote",
            "refs/remotes",
        ],
        reads,
    );
    // A remote configured but never fetched from or pushed to has no tracking ref, and every
    // commit on `HEAD` is one it lacks.
    let configured = git_count(&root, &["remote"], reads);
    let remote = [&remotes, &configured]
        .iter()
        .any(|r| r.as_deref().is_some_and(|r| !r.trim().is_empty()));
    let pushed = remote.then(|| {
        git_count(
            &root,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
            reads,
        )
        .and_then(|n| crate::parse_uint(n.trim()))
    });
    let unpushed = pushed.flatten().unwrap_or(0);
    let main = main_ref(&root, reads);
    let landed = main.as_ref().map(|full| {
        git_count(
            &root,
            &["rev-list", "--count", &format!("{full}..HEAD")],
            reads,
        )
        .and_then(|n| crate::parse_uint(n.trim()))
    });
    let (unlanded, main_ref) = match (&main, landed) {
        (Some(full), Some(Some(n))) => (
            n,
            full.strip_prefix("refs/remotes/")
                .unwrap_or(full)
                .to_string(),
        ),
        _ => (0, String::new()),
    };
    let subs = if remote {
        submodules_unpushed(&root, table, reads)
    } else {
        Some(Beyond::default())
    };
    let parked = match &used_on {
        Some(names) if remote => parked_on(&root, names, main.as_deref(), reads),
        Some(_) => Some(Beyond::default()),
        None => None,
    };
    // A read of the configured remotes, a superproject's submodules or the branches the session
    // used that git refused is never a clean reading.
    if configured.is_none() || subs.is_none() || parked.is_none() {
        reads.cut.set(true);
    }
    let (subs, parked) = (subs.unwrap_or_default(), parked.unwrap_or_default());
    // Whether every read a clean finding's counts rest on gave an answer: a
    // read git refused (an unreadable ref, output that is no text) counted
    // nothing, so a reading that found nothing over it is not clean.
    let independent = listed.is_some() && remotes.is_some();
    let counted =
        independent && pushed.is_none_or(|n| n.is_some()) && landed.is_none_or(|n| n.is_some());
    let cut_before_head = reads.cut.get();
    let head = git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"], reads);
    let head_cut = reads.cut.get() && !cut_before_head;
    let found =
        RepoWork::of_git(&top, &status, &stash, unpushed, unlanded, &main_ref).or_else(|| {
            (subs.count > 0 || parked.count > 0).then(|| RepoWork {
                repo: printable(top.trim_end_matches(['\n', '\r'])),
                main_ref: printable(&main_ref),
                ..RepoWork::default()
            })
        });
    let Some(mut work) = found else {
        // Nothing found. A reading cut before HEAD stays unread. Where every
        // count answered, HEAD only adds a name and its refusal leaves the
        // reading as before; failed counts need the unborn proof below.
        reads.cut.set(cut_before_head);
        if cut_before_head {
            return None;
        }
        let head = head.as_deref().map(str::trim).filter(|h| is_head(h));
        if !counted {
            // Stash and remote enumeration do not depend on HEAD. A refused
            // HEAD count proves nothing unless an ordinary branch-ref read
            // confirms there is no commit yet ([`branch_unborn`]); a refused HEAD
            // read alone is no such proof. A failed confirmation stays unread too.
            let unborn = independent && head.is_none() && !head_cut && branch_unborn(&root, reads);
            reads.cut.set(!unborn);
            return None;
        }
        let head = head?;
        return Some(RepoRead::Checked(CleanAt {
            repo: printable(&top),
            head: head.to_ascii_lowercase(),
            main_ref: printable(&main_ref),
            remote,
            branch: branch
                .as_deref()
                .map_or_else(|| DETACHED.to_owned(), printable),
        }));
    };
    // Offline and with no rename found, as [`work_status`] reads: nothing is fetched.
    let mut all_args: Vec<&str> = vec!["-c", "status.renames=false"];
    if let Some(flag) = &excludes {
        all_args.extend(["-c", flag]);
    }
    all_args.extend(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    (work.sub_unpushed, work.subs) = (subs.count, subs.names);
    (work.parked, work.parked_on) = (parked.count, parked.names);
    let head = head.unwrap_or_default();
    let beyond = format!("{}{}", subs.listing, parked.listing);
    work.state = git_offline(&root, &all_args, reads, None)
        .and_then(|listed| state_of(&root, head.trim(), &stash, &listed))
        .map(|state| {
            if beyond.is_empty() {
                state
            } else {
                aterm_hash::fnv1a_64(format!("{state:016x}\n{beyond}").as_bytes())
            }
        });
    Some(RepoRead::Found(work))
}

/// Whether the repository at `root` is on a branch with no commit yet: `HEAD` names a
/// `refs/heads/` ref, read strictly, and no ref of that name names a commit. The full ref is read
/// strictly where the display branch is read lossily, so a replacement character never names a ref
/// for this absence proof; a read git refused proves nothing.
fn branch_unborn(root: &Path, reads: &Reads<'_>) -> bool {
    git_count(root, &["symbolic-ref", "--quiet", "HEAD"], reads)
        .map(|full| full.trim().to_string())
        .filter(|full| full.starts_with("refs/heads/"))
        .is_some_and(|full| {
            git_count(
                root,
                &["for-each-ref", "--count=1", "--format=commit", &full],
                reads,
            )
            .is_some_and(|refs| refs.trim().is_empty())
        })
}

/// The status of the repository at `root` the probe and the work record read
/// (`git -c status.renames=false status --porcelain=v1 -z --untracked-files=normal`),
/// with the person's excludes as their own `git status` reads them, without each
/// submodule whose checkout only lags its record ([`without_lagging_submodules`]),
/// and the `core.excludesFile=` setting it passed. It runs offline
/// ([`git_offline`]) and finds no rename, so it never needs an object a partial
/// clone lacks: a move reads as its two paths, one deleted and one added. Names in
/// bytes that are no UTF-8 are decoded lossily, as [`git_lossy`]'s are. `None`,
/// with [`Reads::cut`] set, where it could not be read: a repository whose state
/// cannot be read holds what it holds, never nothing.
fn work_status(
    root: &Path,
    table: &aterm_sysprobe::volumes::Table,
    reads: &Reads<'_>,
) -> Option<(String, Option<String>)> {
    let excludes = (!own_excludes(root, reads))
        .then(|| global_excludes(root, table, reads))
        .flatten()
        .and_then(|p| p.to_str().map(|p| format!("core.excludesFile={p}")));
    let mut status_args: Vec<&str> = vec!["-c", "status.renames=false"];
    if let Some(flag) = &excludes {
        status_args.extend(["-c", flag]);
    }
    status_args.extend(["status", "--porcelain=v1", "-z", "--untracked-files=normal"]);
    let Some(status) = run_git_bytes(
        root,
        &status_args,
        reads,
        false,
        true,
        None,
        Diagnostics::Permit,
    ) else {
        reads.cut.set(true);
        return None;
    };
    let status = String::from_utf8_lossy(&status);
    let status = without_lagging_submodules(root, &status, excludes.as_deref(), reads);
    Some((status, excludes))
}

/// Whether a `git status --porcelain=v1 -z` entry's two-letter `code` renames or copies, in the
/// index's column or the working tree's (` R`, a move whose new path is intent-to-add): its
/// source path follows as a field of its own.
pub(crate) fn moved(code: &str) -> bool {
    code.contains(['R', 'C'])
}

/// The most superprojects [`outermost`] climbs through.
const SUPER_CLIMB: usize = 8;

/// The top level and the folder of the outermost project the repository at top level `top`,
/// folder `root`, is a submodule of (`git rev-parse --show-superproject-working-tree`, at most
/// [`SUPER_CLIMB`] up; itself where it is none): a session that moved into a submodule works on
/// the project around it too. `None` where a folder is one the probe never enters, or a read is
/// refused or cut, or the climb goes on past [`SUPER_CLIMB`] (then unread).
fn outermost(
    top: &str,
    root: &Path,
    table: &aterm_sysprobe::volumes::Table,
    reads: &Reads<'_>,
) -> Option<(String, PathBuf)> {
    let (mut top, mut root) = (top.to_string(), root.to_path_buf());
    for _ in 0..=SUPER_CLIMB {
        let Some(outer) = git(
            &root,
            &["rev-parse", "--show-superproject-working-tree"],
            reads,
        ) else {
            reads.cut.set(true);
            return None;
        };
        let outer = outer.trim_end_matches(['\n', '\r']);
        if outer.is_empty() {
            return Some((top, root));
        }
        if !Path::new(outer).is_absolute() {
            reads.cut.set(true);
            return None;
        }
        top = outer.to_string();
        root = unattended_dir(Path::new(&top), Some(reads.home), table)?;
    }
    reads.cut.set(true);
    None
}

/// The most paths one `ls-files` read of [`submodule_records`] names.
const RECORD_PATHS: usize = 256;

/// The most of a shallow clone's `shallow` file [`shallow_roots`] reads.
const SHALLOW_BYTES: u64 = 16 * 1024 * 1024;

/// The most of a repository's `FETCH_HEAD` file [`remote_fetch_heads`] reads.
const FETCH_HEAD_BYTES: u64 = 4 * 1024 * 1024;

/// The most configured remotes [`remote_fetch_heads`] queries for fetch URLs.
const FETCH_REMOTES: usize = 32;

/// The most fetched commit tips [`remote_fetch_heads`] passes to `rev-list`.
const FETCH_HEAD_COMMITS: usize = 256;

/// The most commits one read of [`submodules_unpushed`] or [`parked_on`] lists.
const BEYOND_COMMITS: usize = 4096;

/// What [`submodules_unpushed`] or [`parked_on`] counted: the commits, the first
/// [`NAMED_PATHS`] submodules or branches holding them, and their listing for the state.
#[derive(Debug, Default)]
struct Beyond {
    count: usize,
    names: Vec<String>,
    listing: String,
}

/// The commits each populated submodule of the repository at `root` (a `.gitmodules` path below
/// it holding its own `.git`), and each of theirs in turn, holds at its checkout or at the commit
/// its superproject's index records for it that no remote of that submodule has, git's own check
/// before a push that recurses: a superproject that pushes its record of one leaves the remote
/// main pointing at a commit nobody can fetch. A record the submodule never fetched is not read
/// ([`record_available`]); its checkout and remote refs must still answer without omissions.
/// A shallow clone's boundary commits ([`shallow_roots`]) and commits `FETCH_HEAD` records as
/// fetched from a configured remote of that submodule ([`remote_fetch_heads`], as
/// `git submodule update --depth N` writes) came from a remote,
/// and nothing is fetched ([`git_offline`]). Every submodule is read, each read under its own
/// limit; each level is a folder below the last, so the walk ends. `None` where a read is refused
/// or cut.
fn submodules_unpushed(
    root: &Path,
    table: &aterm_sysprobe::volumes::Table,
    reads: &Reads<'_>,
) -> Option<Beyond> {
    let mut beyond = Beyond::default();
    let mut repos: Vec<(String, PathBuf)> = vec![(String::new(), root.to_path_buf())];
    let max = format!("--max-count={BEYOND_COMMITS}");
    let mut at = 0;
    while let Some((prefix, repo)) = repos.get(at).cloned() {
        at += 1;
        for (path, dir, record) in submodule_records(&repo, table, reads)? {
            if repos.iter().any(|(_, read)| *read == dir) {
                continue;
            }
            let mut args = vec!["rev-list", max.as_str(), "HEAD"];
            if let Some(record) = record.as_deref()
                && record_available(&dir, record, reads)?
            {
                args.push(record);
            }
            args.extend(["--not", "--remotes"]);
            let mut listing = git_offline_count(&dir, &args, reads)?;
            let mut commits: Vec<&str> = listing.lines().filter(|l| !l.trim().is_empty()).collect();
            if !commits.is_empty() {
                let shallow = shallow_roots(&dir, reads)?;
                commits.retain(|c| !shallow.contains(c.trim()));
                if !commits.is_empty() {
                    let fetched = remote_fetch_heads(&dir, reads)?;
                    if !fetched.is_empty() {
                        args.extend(fetched.iter().map(String::as_str));
                        listing = git_offline_count(&dir, &args, reads)?;
                        commits = listing
                            .lines()
                            .filter(|l| !l.trim().is_empty())
                            .filter(|c| !shallow.contains(c.trim()))
                            .collect();
                    }
                }
            }
            let named = format!("{prefix}{path}");
            if !commits.is_empty() {
                beyond.count = beyond.count.saturating_add(commits.len());
                if beyond.names.len() < NAMED_PATHS {
                    beyond.names.push(printable(&named));
                }
                beyond
                    .listing
                    .push_str(&format!("{named}\n{}\n", commits.join("\n")));
            }
            repos.push((format!("{named}/"), dir));
        }
    }
    Some(beyond)
}

/// Whether the index's gitlink `record` is a commit this submodule has fetched. Only Git's
/// structured `missing` answer makes it optional; an error, diagnostic or other object type
/// leaves the count unread. The private input file feeds one bounded object name and is removed
/// after the query has finished.
fn record_available(dir: &Path, record: &str, reads: &Reads<'_>) -> Option<bool> {
    let input = aterm_tempfile::tempdir().ok()?;
    let path = input.path().join("record");
    std::fs::write(&path, format!("{record}\n")).ok()?;
    let feed = std::fs::File::open(&path).ok()?;
    let reply = run_git(
        dir,
        &["cat-file", "--batch-check=%(objecttype)"],
        reads,
        false,
        true,
        Some(feed),
        Diagnostics::Refuse,
    )?;
    match reply.trim_end_matches(['\n', '\r']) {
        "commit" => Some(true),
        missing if missing == format!("{record} missing") => Some(false),
        _ => None,
    }
}

/// The populated submodules of the repository at `root`: each `.gitmodules` path whose folder,
/// below `root`, holds its own `.git`, its folder and the commit the index records for it.
/// `None` where a read is refused or cut.
fn submodule_records(
    root: &Path,
    table: &aterm_sysprobe::volumes::Table,
    reads: &Reads<'_>,
) -> Option<Vec<(String, PathBuf, Option<String>)>> {
    if !std::fs::symlink_metadata(root.join(".gitmodules")).is_ok_and(|m| m.is_file()) {
        return Some(Vec::new());
    }
    let config = git_offline(
        root,
        &["config", "-z", "--file", ".gitmodules", "--list"],
        reads,
        None,
    )?;
    let mut paths: Vec<(String, PathBuf)> = Vec::new();
    for entry in config.split('\0') {
        let Some((key, path)) = entry.split_once('\n') else {
            continue;
        };
        if !(key.starts_with("submodule.") && key.ends_with(".path"))
            || path.is_empty()
            || Path::new(path)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            continue;
        }
        let Some(dir) = unattended_dir(&root.join(path), Some(reads.home), table) else {
            continue;
        };
        if dir != root
            && dir.starts_with(root)
            && dir.join(".git").exists()
            && !paths.iter().any(|(p, d)| p == path || *d == dir)
        {
            paths.push((path.to_string(), dir));
        }
    }
    let mut records: Vec<(String, String)> = Vec::new();
    for chunk in paths.chunks(RECORD_PATHS) {
        let mut args = vec!["--literal-pathspecs", "ls-files", "--stage", "-z", "--"];
        args.extend(chunk.iter().map(|(p, _)| p.as_str()));
        let staged = git_offline(root, &args, reads, None)?;
        // `<mode> <commit> <stage>\t<path>`: a gitlink's mode is `160000`.
        records.extend(staged.split('\0').filter_map(|e| {
            let (meta, path) = e.split_once('\t')?;
            let mut meta = meta.split(' ');
            (meta.next()? == "160000").then_some(())?;
            let commit = meta.next().filter(|c| is_head(c))?;
            Some((path.to_string(), commit.to_string()))
        }));
    }
    Some(
        paths
            .into_iter()
            .map(|(path, dir)| {
                let record = records
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, c)| c.clone());
                (path, dir, record)
            })
            .collect(),
    )
}

fn open_git_file(path: &Path) -> Option<(std::fs::File, std::fs::Metadata)> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?
    };
    #[cfg(not(unix))]
    let file = {
        if !std::fs::symlink_metadata(path).ok()?.is_file() {
            return None;
        }
        std::fs::File::open(path).ok()?
    };
    let meta = file.metadata().ok()?;
    meta.is_file().then_some((file, meta))
}

/// The boundary commits of the shallow clone at `dir` (its `shallow` file, the first
/// [`SHALLOW_BYTES`] of it), none where it is no shallow clone: a fetch cut each one's history
/// there, so each came from a remote, though no remote-tracking ref reaches it. `None` where a
/// read is refused or cut.
fn shallow_roots(dir: &Path, reads: &Reads<'_>) -> Option<std::collections::HashSet<String>> {
    let at = git_offline(dir, &["rev-parse", "--git-path", "shallow"], reads, None)?;
    let mut text = String::new();
    if let Some((file, meta)) = open_git_file(&dir.join(at.trim_end_matches(['\n', '\r']))) {
        text.reserve(
            usize::try_from(meta.len().min(SHALLOW_BYTES))
                .unwrap_or(0)
                .saturating_add(1),
        );
        let _ =
            std::io::Read::read_to_string(&mut std::io::Read::take(file, SHALLOW_BYTES), &mut text);
    }
    Some(
        text.lines()
            .map(str::trim)
            .filter(|c| is_head(c))
            .map(str::to_string)
            .collect(),
    )
}

/// The commits `FETCH_HEAD` at `dir` (its first [`FETCH_HEAD_BYTES`]) records as fetched from one
/// of `dir`'s configured remotes, resolved to present commit objects ([`commits_available`]) and
/// capped at [`FETCH_HEAD_COMMITS`]. `git submodule update --depth N` (`N > 1`) at a commit no
/// remote-tracking ref reaches writes the fetched tip here rather than to `refs/remotes/*`, while
/// `shallow` holds only the boundary `N - 1` commits below it. `None` where a read is refused or
/// cut.
fn remote_fetch_heads(dir: &Path, reads: &Reads<'_>) -> Option<Vec<String>> {
    let at = git_offline(dir, &["rev-parse", "--git-path", "FETCH_HEAD"], reads, None)?;
    let Some((file, meta)) = open_git_file(&dir.join(at.trim_end_matches(['\n', '\r']))) else {
        return Some(Vec::new());
    };
    let mut bytes = Vec::with_capacity(
        usize::try_from(meta.len().min(FETCH_HEAD_BYTES))
            .unwrap_or(0)
            .saturating_add(1),
    );
    let _ =
        std::io::Read::read_to_end(&mut std::io::Read::take(file, FETCH_HEAD_BYTES), &mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    if text.trim().is_empty() {
        return Some(Vec::new());
    }
    let remotes = git_offline_count(dir, &["remote"], reads)?;
    let mut urls: Vec<String> = Vec::new();
    for remote in remotes
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .take(FETCH_REMOTES)
    {
        if let Some(configured) =
            git_offline(dir, &["remote", "get-url", "--all", remote], reads, None)
        {
            for raw in configured.lines().map(str::trim).filter(|u| !u.is_empty()) {
                let display = fetch_head_display_url(raw);
                if !display.is_empty() && !urls.contains(&display) {
                    urls.push(display);
                }
            }
        } else if reads.cut.get() {
            return None;
        }
    }
    if urls.is_empty() {
        return Some(Vec::new());
    }
    let mut shas: Vec<String> = Vec::new();
    for line in text.lines() {
        if shas.len() >= FETCH_HEAD_COMMITS {
            break;
        }
        let Some((sha, rest)) = line.split_once('\t') else {
            continue;
        };
        let Some((merge, note)) = rest.split_once('\t') else {
            continue;
        };
        if !is_head(sha) || !matches!(merge, "" | "not-for-merge") {
            continue;
        }
        let note = note.trim_end_matches('\r');
        let from_remote = urls
            .iter()
            .any(|u| note == u || note.ends_with(&format!("' of {u}")));
        if from_remote && !shas.iter().any(|s| s == sha) {
            shas.push(sha.to_string());
        }
    }
    commits_available(dir, &shas, reads)
}

/// Those of `shas` that resolve in `dir` to present commit objects (`<sha>^{commit}`); missing or
/// non-commit objects are skipped, and any malformed or incomplete `cat-file` reply returns
/// `None`.
fn commits_available(dir: &Path, shas: &[String], reads: &Reads<'_>) -> Option<Vec<String>> {
    if shas.is_empty() {
        return Some(Vec::new());
    }
    let input = aterm_tempfile::tempdir().ok()?;
    let path = input.path().join("fetch-heads");
    let mut query = String::new();
    for sha in shas {
        query.push_str(sha);
        query.push_str("^{commit}\n");
    }
    std::fs::write(&path, query).ok()?;
    let feed = std::fs::File::open(&path).ok()?;
    let reply = run_git(
        dir,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        reads,
        false,
        true,
        Some(feed),
        Diagnostics::Refuse,
    )?;
    let lines: Vec<&str> = reply.lines().collect();
    if lines.len() != shas.len() {
        return None;
    }
    let mut commits: Vec<String> = Vec::new();
    for (sha, line) in shas.iter().zip(lines) {
        let line = line.trim_end_matches('\r');
        if let Some(resolved) = line.strip_suffix(" commit")
            && is_head(resolved)
        {
            if !commits.iter().any(|c| c == resolved) {
                commits.push(resolved.to_string());
            }
        } else if line == format!("{sha}^{{commit}} missing") {
            continue;
        } else {
            return None;
        }
    }
    Some(commits)
}

/// The anonymized, suffix-trimmed URL Git's `store_updated_refs` writes into `FETCH_HEAD` for a
/// configured remote URL `raw`: `scheme://user@host/...` without `user@`, trailing `/` stripped
/// once, then a trailing `.git` stripped once.
fn fetch_head_display_url(raw: &str) -> String {
    let anon = match raw.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest
                .split_once('/')
                .map_or((rest, None), |(a, p)| (a, Some(p)));
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            match path {
                Some(p) => format!("{scheme}://{host}/{p}"),
                None => format!("{scheme}://{host}"),
            }
        }
        None => raw.to_string(),
    };
    let trimmed = anon.strip_suffix('/').unwrap_or(&anon);
    let stripped = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    if stripped.is_empty() {
        anon
    } else {
        stripped.to_string()
    }
}

/// The branches the session used ([`Used`]) besides `HEAD`'s own: those this worktree's `HEAD`
/// reflog (its last [`LAG_REFLOG`] bytes) moved from or to after [`Used::since`], then those its
/// commands named, each once and at most [`USED_BRANCHES`]. Another worktree's moves are in its
/// own reflog, so its branches are never this session's. `None` where a read is refused or cut.
fn used_branches(
    root: &Path,
    used: &Used,
    head: Option<&str>,
    reads: &Reads<'_>,
) -> Option<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    if let Some(since) = used.since {
        let at = git_offline(root, &["rev-parse", "--git-path", "logs/HEAD"], reads, None)?;
        // No reflog file (none kept, or a `reftable` repository): no move is recorded.
        if let Some(mut file) = reflog_tail(&root.join(at.trim_end_matches(['\n', '\r']))) {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            for line in text.lines().rev() {
                // `<old> <new> <who> <when> <zone>\t<why>`
                let Some((who, why)) = line.split_once('\t') else {
                    continue;
                };
                let mut stamp = who.rsplitn(3, ' ');
                let when = stamp.nth(1).and_then(crate::parse_uint::<u64>);
                if !when.is_some_and(|when| when > since) {
                    continue;
                }
                if let Some((from, to)) = why
                    .strip_prefix("checkout: moving from ")
                    .and_then(|moved| moved.rsplit_once(" to "))
                {
                    names.extend([to, from].map(str::to_string));
                } else if let Some(back) =
                    why.strip_prefix("rebase (finish): returning to refs/heads/")
                {
                    names.push(back.to_string());
                }
            }
        }
    }
    names.extend(used.branches.iter().cloned());
    let mut kept: Vec<String> = Vec::new();
    for name in names {
        if kept.len() < USED_BRANCHES
            && branch_name_ok(&name)
            && Some(name.as_str()) != head
            && !kept.contains(&name)
        {
            kept.push(name);
        }
    }
    Some(kept)
}

/// The commits on the branches `names` that neither `HEAD` nor `main` (the remote main's full
/// ref; with none, any remote-tracking ref) has, and the branches holding them by name: each
/// local branch, and where `main` is `origin`'s, `origin`'s branch of that name, which a session
/// that pushed its branch there and deleted its own still parked its work on. A branch since
/// deleted counts nothing after exact ref enumeration; a present unreadable branch is not
/// deleted. `None` where the read is refused or cut, or Git omits reference data with a warning.
fn parked_on(
    root: &Path,
    names: &[String],
    main: Option<&str>,
    reads: &Reads<'_>,
) -> Option<Beyond> {
    if names.is_empty() {
        return Some(Beyond::default());
    }
    let origin = main.is_some_and(|m| m.starts_with(ORIGIN_REFS));
    let mut refs: Vec<(String, &str)> = names
        .iter()
        .map(|n| (format!("refs/heads/{n}"), n.as_str()))
        .collect();
    if origin {
        refs.extend(
            names
                .iter()
                .map(|n| (format!("{ORIGIN_REFS}{n}"), n.as_str())),
        );
    }
    let mut enumerate = vec!["for-each-ref", "--format=%(refname)"];
    enumerate.extend(refs.iter().map(|(name, _)| name.as_str()));
    let existing = git_offline_count(root, &enumerate, reads)?;
    refs.retain(|(name, _)| existing.lines().any(|line| line == name));
    if refs.is_empty() {
        return Some(Beyond::default());
    }
    let max = format!("--max-count={BEYOND_COMMITS}");
    let mut args = vec![
        "log",
        "--no-show-signature",
        "--no-color",
        max.as_str(),
        "--format=%H%x09%S",
    ];
    args.extend(refs.iter().map(|(r, _)| r.as_str()));
    args.extend(["--not", main.unwrap_or("--remotes"), "HEAD"]);
    let listing = git_offline_count(root, &args, reads)?;
    let mut beyond = Beyond::default();
    let mut on: Vec<&str> = Vec::new();
    for line in listing.lines().filter(|l| !l.trim().is_empty()) {
        beyond.count = beyond.count.saturating_add(1);
        if let Some(&(_, name)) = line
            .split_once('\t')
            .and_then(|(_, source)| refs.iter().find(|(r, _)| r == source))
            && !on.contains(&name)
        {
            on.push(name);
        }
    }
    on.sort_unstable();
    beyond.names = on.into_iter().take(NAMED_PATHS).map(printable).collect();
    beyond.listing = listing;
    Some(beyond)
}

/// Where `origin`'s remote-tracking branches are, the remote [`main_ref`] reads.
const ORIGIN_REFS: &str = "refs/remotes/origin/";

/// Whether `text` is a commit name `git rev-parse --verify` prints: 40 hex
/// digits (SHA-1) or 64 (SHA-256).
fn is_head(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Probe repository hygiene and drift from the main branch ([`RepoHygiene::main_ref`]) at `dir`.
#[must_use]
pub fn probe_hygiene(dir: &Path, home: Option<&Path>, limit: Duration) -> Option<RepoHygiene> {
    let home = home?;
    if atpkg::protected::under_protected_root(home, dir) {
        return None;
    }
    let reads = Reads {
        deadline: Cell::new(Instant::now().checked_add(limit)?),
        each: None,
        home,
        xdg: None,
        cut: Cell::new(false),
        starts: Cell::new(0),
        held: None,
        #[cfg(test)]
        refused: None,
    };
    let reads = &reads;
    let table = aterm_sysprobe::volumes::table();
    let dir = unattended_dir(dir, Some(home), &table)?;
    let top = git(&dir, &["rev-parse", "--show-toplevel"], reads)?;
    let top = top.trim_end_matches(['\n', '\r']);
    if top.is_empty() {
        return None;
    }
    let root = unattended_dir(Path::new(top), Some(home), &table)?;
    let branch = git(
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        reads,
    )
    .map(|b| b.trim().to_string())
    .filter(|b| !b.is_empty());
    let target_full = main_ref(&root, reads).or_else(|| {
        ["refs/heads/main", "refs/heads/master"]
            .into_iter()
            .map(str::to_string)
            .find(|full| {
                git(
                    &root,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("{full}^{{commit}}"),
                    ],
                    reads,
                )
                .is_some()
            })
    });
    let target_short = target_full
        .as_deref()
        .map(|f| {
            f.strip_prefix("refs/remotes/")
                .or_else(|| f.strip_prefix("refs/heads/"))
                .unwrap_or(f)
        })
        .unwrap_or_default();
    let ahead_behind = target_full
        .as_deref()
        .and_then(|full| {
            git(
                &root,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("{full}...HEAD"),
                ],
                reads,
            )
        })
        .unwrap_or_default();
    let worktree_list = git(&root, &["worktree", "list", "--porcelain"], reads).unwrap_or_default();
    let (unmerged, merged) = match target_full.as_deref() {
        Some(full) => (
            git(
                &root,
                &[
                    "for-each-ref",
                    &format!("--no-merged={full}"),
                    "--format=%(refname:short)",
                    "refs/heads",
                ],
                reads,
            )
            .unwrap_or_default(),
            git(
                &root,
                &[
                    "for-each-ref",
                    &format!("--merged={full}"),
                    "--format=%(refname:short)",
                    "refs/heads",
                ],
                reads,
            )
            .unwrap_or_default(),
        ),
        None => (String::new(), String::new()),
    };
    let status = git(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        reads,
    )
    .unwrap_or_default();
    let stash = git(&root, &["stash", "list", "--format=%gs"], reads).unwrap_or_default();
    let stash = stashes_on(&stash, branch.as_deref());
    let remotes = git(
        &root,
        &[
            "for-each-ref",
            "--count=1",
            "--format=remote",
            "refs/remotes",
        ],
        reads,
    )
    .unwrap_or_default();
    let unpushed = if remotes.trim().is_empty() {
        0
    } else {
        git(
            &root,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
            reads,
        )
        .and_then(|n| crate::parse_uint(n.trim()))
        .unwrap_or(0)
    };
    let (worktree_bytes, temp_bytes) = probe_hygiene_disk(&root, &worktree_list, &table, reads);
    // Last, so its reads, which share the one deadline, starve none of the others: one that runs
    // out leaves a lagging submodule counted.
    let status = without_lagging_submodules(&root, &status, None, reads);
    let drive_free_bytes = super::disk::volume_of(&root, &table).ok().map(|r| r.free);
    Some(
        RepoHygiene::of_git(
            target_short,
            branch.as_deref(),
            &ahead_behind,
            &worktree_list,
            &unmerged,
            &merged,
        )
        .with_local_and_disk(
            &status,
            &stash,
            unpushed,
            worktree_bytes,
            temp_bytes,
            drive_free_bytes,
        ),
    )
}

const HYGIENE_WALK_CAP: usize = 8_000;

fn is_temp_or_build_dir_name(name: &str) -> bool {
    name == "target"
        || name.starts_with("target-")
        || name.starts_with("target.")
        || name.ends_with(".noindex")
        || name.starts_with(".tmp")
}

fn probe_hygiene_disk(
    root: &Path,
    worktree_list: &str,
    table: &aterm_sysprobe::volumes::Table,
    reads: &Reads<'_>,
) -> (u64, u64) {
    let mut budget = HYGIENE_WALK_CAP;
    let mut wt_total = 0u64;
    let mut temp_total = 0u64;

    if let Ok(rd) = std::fs::read_dir(root) {
        for entry in rd.flatten() {
            if Instant::now() >= reads.deadline.get() || budget == 0 {
                break;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if is_temp_or_build_dir_name(name)
                && let Some(safe) = unattended_dir(&path, Some(reads.home), table)
            {
                temp_total = temp_total.saturating_add(bounded_dir_bytes(
                    &safe,
                    reads.deadline.get(),
                    &mut budget,
                    false,
                    &mut 0,
                ));
            }
        }
    }

    for wt_path in worktree_list
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .skip(1)
    {
        if Instant::now() >= reads.deadline.get() || budget == 0 {
            break;
        }
        let Some(safe) = unattended_dir(Path::new(wt_path.trim()), Some(reads.home), table) else {
            continue;
        };
        let mut wt_temp = 0u64;
        let bytes = bounded_dir_bytes(&safe, reads.deadline.get(), &mut budget, true, &mut wt_temp);
        wt_total = wt_total.saturating_add(bytes);
        if wt_temp > 0 {
            temp_total = temp_total.saturating_add(wt_temp);
        }
    }

    (wt_total, temp_total)
}

fn bounded_dir_bytes(
    dir: &Path,
    deadline: Instant,
    budget: &mut usize,
    split_temp: bool,
    temp_out: &mut u64,
) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![(dir.to_path_buf(), false)];
    while let Some((d, in_temp)) = stack.pop() {
        if Instant::now() >= deadline || *budget == 0 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            if *budget == 0 || Instant::now() >= deadline {
                break;
            }
            *budget -= 1;
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                let path = entry.path();
                let child_temp = in_temp
                    || (split_temp
                        && path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(is_temp_or_build_dir_name));
                stack.push((path, child_temp));
            } else if in_temp {
                *temp_out = temp_out.saturating_add(meta.len());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// The most submodules [`without_lagging_submodules`] reads one by one: any past them stay.
const LAG_READS: usize = 8;

/// How much of the end of `HEAD`'s reflog [`without_lagging_submodules`] reads: a thousand or
/// more of its moves.
const LAG_REFLOG: u64 = 256 * 1024;

/// `status` (`git status --porcelain=v1 -z` of the repository at `root`) without each submodule
/// whose checkout only lags its record: nothing edited or untracked in it (as far as the
/// repository's own submodule settings let git see), and its `HEAD` the commit this checkout
/// recorded for it at a point its own `HEAD` moved from or to, in the last [`LAG_REFLOG`] bytes of
/// that reflog. That is what a pull, a merge or a checkout that did not update its submodules
/// leaves: `git submodule update` loses nothing such a checkout holds, and the one commit its
/// entry asks for would record the older commit again. A checkout at a commit this one never
/// recorded there (a bump not yet committed, a branch switched to, a commit made in it) stays, as
/// does every entry past the first [`LAG_READS`] submodules, every entry where that reflog is off,
/// gone or no file (a `reftable` repository), and every entry when a read fails or is cut. The
/// reads cost the same at any length of history, none looks into a submodule's objects, and none
/// fetches ([`git_offline`]).
fn without_lagging_submodules(
    root: &Path,
    status: &str,
    excludes: Option<&str>,
    reads: &Reads<'_>,
) -> String {
    let mut submodules = Vec::new();
    let mut fields = status.split('\0');
    while let Some(entry) = fields.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else {
            continue;
        };
        if moved(code) {
            fields.next();
        }
        if code == " M"
            && submodules.len() < LAG_READS
            && !path.is_empty()
            && root.join(path).join(".git").exists()
        {
            submodules.push(path);
        }
    }
    if submodules.is_empty() {
        return status.to_string();
    }
    let mut args: Vec<&str> = Vec::new();
    if let Some(flag) = excludes {
        args.extend(["-c", flag]);
    }
    args.extend([
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=normal",
        "--",
    ]);
    args.extend(submodules);
    let Some(described) = git_offline(root, &args, reads, None) else {
        return status.to_string();
    };
    // `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`: `SC..` is a submodule whose commit
    // alone moved, `hI` the commit the index records for it.
    let mut moved = Vec::new();
    let mut fields = described.split('\0');
    while let Some(entry) = fields.next() {
        if entry.starts_with("2 ") {
            fields.next();
            continue;
        }
        let Some(rest) = entry.strip_prefix("1 ") else {
            continue;
        };
        let parts: Vec<&str> = rest.splitn(8, ' ').collect();
        if let &[".M", "SC..", _, _, _, _, recorded, path] = parts.as_slice() {
            moved.push((path, recorded));
        }
    }
    if moved.is_empty() {
        return status.to_string();
    }
    let Some(reflog) = git_offline(root, &["rev-parse", "--git-path", "logs/HEAD"], reads, None)
        .and_then(|at| reflog_tail(&root.join(at.trim_end_matches(['\n', '\r']))))
    else {
        return status.to_string();
    };
    // Each reflog line, `<old> <new> <who> <when>\t<why>`, is to `diff-tree --stdin` the commit
    // `<old>` with `<new>` as its one parent (the list of parents ends at `<who>`): its records
    // are the submodules' commits at both ends of that move. A line whose commit is gone gives
    // nothing.
    let mut args = vec![
        "diff-tree",
        "--stdin",
        "-r",
        "--raw",
        "-z",
        "--no-abbrev",
        "--",
    ];
    args.extend(moved.iter().map(|(path, _)| *path));
    let Some(history) = git_offline(root, &args, reads, Some(reflog)) else {
        return status.to_string();
    };
    let before = records_of(&history);
    let mut lagging = Vec::new();
    for (path, recorded) in moved {
        let Some(head) = git_offline(
            &root.join(path),
            &["rev-parse", "--verify", "--quiet", "HEAD"],
            reads,
            None,
        ) else {
            continue;
        };
        let head = head.trim();
        if is_head(head) && head != recorded && before.contains(&(path, head)) {
            lagging.push(path);
        }
    }
    if lagging.is_empty() || reads.cut.get() {
        return status.to_string();
    }
    let mut kept = String::with_capacity(status.len());
    let mut fields = status.split('\0');
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        let renamed = entry.get(..2).is_some_and(self::moved);
        let gone = entry
            .strip_prefix(" M ")
            .is_some_and(|path| lagging.contains(&path));
        if !gone {
            kept.push_str(entry);
            kept.push('\0');
        }
        if renamed && let Some(from) = fields.next() {
            kept.push_str(from);
            kept.push('\0');
        }
    }
    kept
}

/// The reflog file at `path`, read-only, at the first whole line of its last [`LAG_REFLOG`]
/// bytes; `None` where it is no file or cannot be read.
fn reflog_tail(path: &Path) -> Option<std::fs::File> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (mut file, meta) = open_git_file(path)?;
    let len = meta.len();
    let from = len.saturating_sub(LAG_REFLOG);
    if from > 0 {
        // From the byte before `from`: a line that starts at `from` is whole.
        file.seek(SeekFrom::Start(from - 1)).ok()?;
        let mut head = [0_u8; 4096];
        let read = file.read(&mut head).ok()?;
        let newline = head.get(..read)?.iter().position(|&b| b == b'\n')?;
        file.seek(SeekFrom::Start(from + u64::try_from(newline).ok()?))
            .ok()?;
    }
    Some(file)
}

/// The `(path, commit)` records `history` (`git diff-tree --raw -z --no-abbrev`) names: a
/// gitlink's commit on each side of each change (`:160000 160000 <before> <after> M`, then its
/// path, or a rename's two).
fn records_of(history: &str) -> std::collections::HashSet<(&str, &str)> {
    let mut records = std::collections::HashSet::new();
    let mut fields = history.split('\0');
    while let Some(field) = fields.next() {
        let Some(meta) = field.trim_start_matches('\n').strip_prefix(':') else {
            continue;
        };
        let &[from_mode, to_mode, from, to, change] =
            meta.split(' ').collect::<Vec<_>>().as_slice()
        else {
            continue;
        };
        let Some(from_path) = fields.next() else {
            break;
        };
        let to_path = if change.starts_with(['R', 'C']) {
            fields.next().unwrap_or(from_path)
        } else {
            from_path
        };
        if from_mode == "160000" {
            records.insert((from_path, from));
        }
        if to_mode == "160000" {
            records.insert((to_path, to));
        }
    }
    records
}

/// The work's state ([`RepoWork::state`]) of the repository at `root`: a hash of `head`, `stash`
/// and each listed path's status, size and mtime; `None` past [`STATE_FILES`] paths.
#[must_use]
pub fn state_of(root: &Path, head: &str, stash: &str, listed: &str) -> Option<u64> {
    let mut text = format!("{head}\n{stash}\n");
    let mut fields = listed.split('\0');
    let mut count = 0usize;
    while let Some(entry) = fields.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else {
            continue;
        };
        if moved(code) {
            fields.next();
        }
        count += 1;
        if count > STATE_FILES {
            return None;
        }
        let stat = std::fs::symlink_metadata(root.join(path)).ok().map_or_else(
            || "-".to_string(),
            |m| {
                let modified = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                format!("{} {modified}", m.len())
            },
        );
        text.push_str(&format!("{code} {path} {stat}\n"));
    }
    Some(aterm_hash::fnv1a_64(text.as_bytes()))
}

/// The entries of `list` (`git stash list --format=%gs`) stashed on `branch`, one per line.
#[must_use]
pub fn stashes_on(list: &str, branch: Option<&str>) -> String {
    let name = branch.unwrap_or("(no branch)");
    let (wip, on) = (format!("WIP on {name}: "), format!("On {name}: "));
    list.lines()
        .filter(|l| l.starts_with(&wip) || l.starts_with(&on))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn own_excludes(root: &Path, reads: &Reads<'_>) -> bool {
    git(
        root,
        &["config", "--includes", "--get", "core.excludesFile"],
        reads,
    )
    .is_some_and(|p| !p.trim().is_empty())
}

fn global_excludes(
    root: &Path,
    table: &aterm_sysprobe::volumes::Table,
    reads: &Reads<'_>,
) -> Option<PathBuf> {
    let (home, xdg) = (reads.home, reads.xdg);
    let set = git_as(
        root,
        &[
            "config",
            "--global",
            "--includes",
            "--path",
            "--get",
            "core.excludesFile",
        ],
        reads,
        true,
    )
    .map(|p| p.trim_end_matches(['\n', '\r']).to_string())
    .filter(|p| !p.is_empty())
    .map(PathBuf::from);
    let path = set.unwrap_or_else(|| {
        xdg.map_or_else(|| home.join(".config"), Path::to_path_buf)
            .join("git")
            .join("ignore")
    });
    if !path.is_absolute()
        || path.components().any(|c| c == Component::ParentDir)
        || atpkg::protected::under_protected_root(home, &path)
    {
        return None;
    }
    let real = super::disk::idle::resolve_local(&path, table)?;
    if atpkg::protected::under_protected_root(home, &real) {
        return None;
    }
    let real_home = super::disk::idle::resolve_local(home, table)?;
    canonical_outside(&real, &real_home).filter(|p| p.is_file())
}

fn main_ref(root: &Path, reads: &Reads<'_>) -> Option<String> {
    let head = git(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        reads,
    )
    .map(|r| r.trim().to_string())
    .filter(|r| r.starts_with("refs/remotes/"));
    head.into_iter()
        .chain(["refs/remotes/origin/main", "refs/remotes/origin/master"].map(str::to_string))
        .find(|full| {
            git(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{full}^{{commit}}"),
                ],
                reads,
            )
            .is_some()
        })
}

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
    if atpkg::protected::under_protected_root(&home, &dir) {
        return None;
    }
    canonical_outside(&dir, &home)
}

/// THE SESSION'S FOLDER IS GONE (§14.17 item 4, slice 9c): `dir`, which [`unattended_dir`] did
/// not resolve, in [`printable`]'s form, where it is absolute with no `..`, outside every folder
/// macOS guards (by its path, then by its local volume's), and a read of its own entry finds
/// nothing there. `None` for every other folder [`unattended_dir`] leaves alone: a guarded one is
/// never read, and a network or automounted volume is never touched.
fn gone_dir(dir: &Path, home: &Path, table: &aterm_sysprobe::volumes::Table) -> Option<String> {
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
    let real_home = super::disk::idle::resolve_local(home, table)?;
    let local = super::disk::idle::resolve_dangling(dir, table)?;
    if atpkg::protected::under_protected_root(&real_home, &local) {
        return None;
    }
    std::fs::symlink_metadata(&local)
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        .then(|| printable(&dir.to_string_lossy()))
}

fn canonical_outside(real: &Path, real_home: &Path) -> Option<PathBuf> {
    let canon = std::fs::canonicalize(real).ok()?;
    let canon_home = std::fs::canonicalize(real_home).ok()?;
    (!atpkg::protected::under_protected_root(&canon_home, &canon)).then_some(canon)
}

fn git(dir: &Path, args: &[&str], reads: &Reads<'_>) -> Option<String> {
    git_as(dir, args, reads, false)
}

/// Names read for display and matching, decoded alike: invalid UTF-8 becomes U+FFFD.
/// Counts still reject diagnostic output when `diagnostics` is [`Diagnostics::Refuse`].
fn git_lossy(
    dir: &Path,
    args: &[&str],
    reads: &Reads<'_>,
    diagnostics: Diagnostics,
) -> Option<String> {
    run_git_bytes(dir, args, reads, false, false, None, diagnostics)
        .map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// Count/proof queries need all the data they asked for: Git can return success and empty
/// stdout while warning that it ignored a broken ref. Such a read proves no absence.
fn git_count(dir: &Path, args: &[&str], reads: &Reads<'_>) -> Option<String> {
    run_git(dir, args, reads, false, false, None, Diagnostics::Refuse)
}

/// A count made without fetching: omitted reference data is no complete count.
fn git_offline_count(dir: &Path, args: &[&str], reads: &Reads<'_>) -> Option<String> {
    run_git(dir, args, reads, false, true, None, Diagnostics::Refuse)
}

/// A count made without fetching that must answer ([`Diagnostics::Answer`]): git's quiet "none"
/// (exit 1 with nothing on stderr, as `merge-base` says no commit is in common) is `None`, and
/// every other refusal is no answer.
fn git_offline_answer(dir: &Path, args: &[&str], reads: &Reads<'_>) -> Option<String> {
    run_git(dir, args, reads, false, true, None, Diagnostics::Answer)
}

#[derive(Clone, Copy)]
enum Diagnostics {
    /// The ordinary metadata reads keep their existing directory-warning guard.
    Permit,
    /// A count or absence proof rejects any diagnostic output.
    Refuse,
    /// A read that must answer: as [`Self::Refuse`], and a refusal is no answer either
    /// ([`Reads::cut`]), but git's quiet "none" (exit 1, nothing on stderr).
    Answer,
}

#[cfg(test)]
type RefusalProbe<'a> = &'a dyn Fn(&[&str]) -> bool;
struct Reads<'a> {
    /// What the next read must end by; a call past it does not start.
    deadline: Cell<Instant>,
    /// What a read that ends gives the next ([`Self::ended`]); `None` where
    /// every read shares [`Self::deadline`] ([`probe_hygiene`]).
    each: Option<Duration>,
    /// The person's home.
    home: &'a Path,
    /// The person's `XDG_CONFIG_HOME` (absolute), for [`global_excludes`]'s read alone.
    xdg: Option<&'a Path>,
    /// Whether a read did not start, or was cut off, at the deadline, or could
    /// not be made ([`git_as`]), or the repository found could not be read
    /// ([`probe_reads`]).
    cut: Cell<bool>,
    /// The git processes started so far ([`run_git_bytes`]).
    starts: Cell<u32>,
    /// Whether new starts are held back now (the host's to say: macOS's program check reads
    /// full or stuck): a read then starts no git, and is cut. `None` holds nothing back.
    held: Option<&'a dyn Fn() -> bool>,
    /// An ordinary nonzero Git exit for a selected query; every other read
    /// remains real in the internal transport-failure controls.
    #[cfg(test)]
    refused: Option<RefusalProbe<'a>>,
}

impl Reads<'_> {
    /// A read ended: the next has [`Self::each`] from now. A read its limit
    /// cut never ends, so none after it starts.
    fn ended(&self) {
        let Some(each) = self.each else {
            return;
        };
        // A test's starved reads have run the probe's clock on ([`read_stall::lost`]).
        #[cfg(test)]
        let each = each.saturating_add(read_stall::lost());
        if let Some(next) = Instant::now().checked_add(each) {
            self.deadline.set(next);
        }
    }
}

fn git_as(dir: &Path, args: &[&str], reads: &Reads<'_>, global: bool) -> Option<String> {
    run_git(dir, args, reads, global, false, None, Diagnostics::Permit)
}

/// [`git`] for [`without_lagging_submodules`]: nothing it reads is fetched (`GIT_NO_LAZY_FETCH`;
/// a git before 2.45 knows no lazy fetch to stop and ignores it), so an object a partial clone
/// lacks fails the read, and `feed`, where given, is its standard input.
fn git_offline(
    dir: &Path,
    args: &[&str],
    reads: &Reads<'_>,
    feed: Option<std::fs::File>,
) -> Option<String> {
    run_git(dir, args, reads, false, true, feed, Diagnostics::Permit)
}

/// [`git_as`], or with `offline` [`git_offline`]: [`run_git_bytes`]'s output
/// where it is UTF-8, `None` where it is not.
fn run_git(
    dir: &Path,
    args: &[&str],
    reads: &Reads<'_>,
    global: bool,
    offline: bool,
    feed: Option<std::fs::File>,
    diagnostics: Diagnostics,
) -> Option<String> {
    run_git_bytes(dir, args, reads, global, offline, feed, diagnostics)
        .and_then(|out| String::from_utf8(out).ok())
}

/// The output of `git <args>` in `dir`. `None` where there is no `git`, or
/// where git refused (it exited with a failure); `None` with [`Reads::cut`]
/// set where the read did not start before its deadline or while new starts
/// are held back ([`Reads::held`]), could not be made or was cut off, a signal
/// ended it, git could not open a directory, or git refused a read that must
/// answer ([`Diagnostics::Answer`]).
fn run_git_bytes(
    dir: &Path,
    args: &[&str],
    reads: &Reads<'_>,
    global: bool,
    offline: bool,
    feed: Option<std::fs::File>,
    diagnostics: Diagnostics,
) -> Option<Vec<u8>> {
    let deadline = reads.deadline.get();
    #[cfg(test)]
    let deadline = deadline
        .checked_sub(read_stall::spent())
        .unwrap_or_else(Instant::now);
    if Instant::now() >= deadline || reads.held.is_some_and(|held| held()) {
        reads.cut.set(true);
        return None;
    }
    #[cfg(test)]
    if reads.refused.is_some_and(|refused| refused(args)) {
        reads.ended();
        return None;
    }
    let path = std::env::var_os("PATH")?;
    // On a Mac without Apple's Command Line Tools the `git` on PATH may be Apple's `/usr/bin`
    // stub, which raises their install dialog: there is no git here, read as quietly as none at
    // all.
    let mut cmd = git_command(
        &path,
        dir,
        Path::new("/usr/bin"),
        aterm_types::clt_shim::developer_git,
    )?;
    cmd.arg("--no-pager")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("HOME", reads.home);
    if global {
        if let Some(xdg) = reads.xdg {
            cmd.env("XDG_CONFIG_HOME", xdg);
        }
    } else {
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("XDG_CONFIG_HOME", "/dev/null");
    }
    if offline {
        cmd.env("GIT_NO_LAZY_FETCH", "1");
    }
    let stdin = feed.map_or_else(
        aterm_uds::spawnfd::Stdio::null,
        aterm_uds::spawnfd::Stdio::from,
    );
    reads.starts.set(reads.starts.get().saturating_add(1));
    let output =
        match aterm_uds::deadline::output_until_complete_stdout_from(&mut cmd, stdin, deadline) {
            Ok(output) => output,
            // No `git` here: the probe cannot run, and reads clean.
            Err(aterm_uds::deadline::CompleteOutputError::Run(
                aterm_uds::deadline::RunError::Spawn(e),
            )) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => {
                reads.cut.set(true);
                return None;
            }
        };
    reads.ended();
    if output.status.code().is_none() {
        // A signal ended it: what it would have said is not known.
        reads.cut.set(true);
        return None;
    }
    if !output.status.success() {
        if matches!(diagnostics, Diagnostics::Answer)
            && (output.status.code() != Some(1) || !output.stderr.is_empty())
        {
            reads.cut.set(true);
        }
        return None;
    }
    if (matches!(diagnostics, Diagnostics::Refuse | Diagnostics::Answer)
        && !output.stderr.is_empty())
        || output
            .stderr
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"warning: could not open directory '"))
    {
        reads.cut.set(true);
        return None;
    }
    Some(output.stdout)
}

/// The `git` a read starts in `dir` with `path` as its whole PATH: exactly the `git` that
/// [`git_to_run`] judged, by its absolute path; `None` where no `git` may run.
fn git_command(
    path: &std::ffi::OsStr,
    dir: &Path,
    stubs: &Path,
    developer_git: impl FnOnce() -> bool,
) -> Option<aterm_uds::spawnfd::Command> {
    let git = git_to_run(path, dir, stubs, developer_git)?;
    let mut cmd = aterm_uds::spawnfd::command(git);
    cmd.current_dir(dir).env_clear().env("PATH", path);
    Some(cmd)
}

/// The `git` a read runs: the first runnable `git` on the child's `path`, by its absolute path,
/// when it may run. The macOS search follows empty/relative entries from `child_dir` and skips
/// files that cannot execute, as spawnfd does. A canonical path or inode alias of the Apple stub
/// requires the developer tools' own Git; no candidate is executed. Unknown selected identity
/// refuses, and so does a `path` with no `git` at all (nothing then runs `/usr/bin/git` instead).
/// This is a metadata preflight, not an atomic guarantee against replacement between it and the
/// spawn. Other platforms run [`aterm_uds::os_path!`]'s `git`, the spawn's to answer.
#[cfg(target_os = "macos")]
fn git_to_run(
    path: &std::ffi::OsStr,
    child_dir: &Path,
    stubs: &Path,
    developer_git: impl FnOnce() -> bool,
) -> Option<std::path::PathBuf> {
    use std::os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _};
    let base = if child_dir.is_absolute() {
        child_dir.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(child_dir)
    } else {
        return None;
    };
    for dir in std::env::split_paths(path) {
        let candidate = base.join(dir).join("git");
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(name) = std::ffi::CString::new(candidate.as_os_str().as_bytes()) else {
            return None;
        };
        // SAFETY: `name` is a live NUL-terminated path. X_OK asks only about access;
        // access retains no pointer and neither opens nor executes the candidate.
        if unsafe { libc::access(name.as_ptr(), libc::X_OK) } != 0 {
            continue;
        }
        let Ok(selected) = candidate.canonicalize() else {
            return None;
        };
        let stub = match stubs.join("git").canonicalize() {
            Ok(stub) => stub,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(candidate),
            Err(_) => return None,
        };
        let Ok(stub_metadata) = std::fs::metadata(&stub) else {
            return None;
        };
        let same = selected == stub
            || (metadata.dev() == stub_metadata.dev() && metadata.ino() == stub_metadata.ino());
        return (!same || developer_git()).then_some(candidate);
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn git_to_run(
    _path: &std::ffi::OsStr,
    _child_dir: &Path,
    _stubs: &Path,
    _developer_git: impl FnOnce() -> bool,
) -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(aterm_uds::os_path!("git")))
}

/// A starved `git` for this thread's probes (tests).
#[cfg(test)]
pub(crate) mod read_stall {
    use std::cell::Cell;
    use std::time::Duration;

    std::thread_local! {
        static SKIP: Cell<u32> = const { Cell::new(0) };
        static NEXT: Cell<u64> = const { Cell::new(0) };
        static EVERY: Cell<u64> = const { Cell::new(0) };
        static SPENT: Cell<u64> = const { Cell::new(0) };
    }

    /// The next read this thread starts is starved `ms` (0: none).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn set(ms: u64) {
        set_after(0, ms);
    }

    /// Starve the read after `skip` reads, clearing the time already spent.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn set_after(skip: u32, ms: u64) {
        SKIP.with(|k| k.set(skip));
        NEXT.with(|n| n.set(ms));
        EVERY.with(|e| e.set(0));
        SPENT.with(|s| s.set(0));
    }

    /// Starve every read this thread starts `ms`, clearing the time already spent.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn every(ms: u64) {
        set_after(0, 0);
        EVERY.with(|e| e.set(ms));
    }

    /// The time this thread's reads have lost so far: how far the probe's clock
    /// runs ahead of the machine's, which every deadline is set on
    /// ([`super::Reads::ended`]).
    pub(super) fn lost() -> Duration {
        Duration::from_millis(SPENT.with(Cell::get))
    }

    /// The time this thread's reads have lost so far, the read starting now included.
    pub(super) fn spent() -> Duration {
        let skipped = SKIP.with(|k| {
            let left = k.get();
            k.set(left.saturating_sub(1));
            left > 0
        });
        let next = if skipped {
            0
        } else {
            NEXT.with(|n| n.replace(0))
        };
        let next = next.saturating_add(EVERY.with(Cell::get));
        SPENT.with(|s| {
            s.set(s.get().saturating_add(next));
            Duration::from_millis(s.get())
        })
    }
}

#[path = "repo_work_record.rs"]
mod record;
pub use record::read_work;

#[cfg(test)]
#[path = "repo_work_tests.rs"]
pub(crate) mod tests;
