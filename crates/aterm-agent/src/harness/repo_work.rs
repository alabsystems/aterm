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

const PATH_CHARS: usize = 72;

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
        let all: Vec<String> = [text]
            .into_iter()
            .filter(|t| !t.is_empty())
            .chain(tail)
            .collect();
        format!("{}: {}", self.repo, all.join(", "))
    }
}

/// WHAT A CLEAN PROBE CHECKED ([`RepoRead::Checked`]; `docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §14.9, slice 9a): the repository it read, the commit its `HEAD` named, the remote main it
/// counted against and whether any remote-tracking ref was there to count against. The probe
/// found nothing no remote main has; these say what that was measured against, for a landing's
/// receipt. No decision reads them: a checked reading decides as a clean one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanAt {
    /// The repository's top-level directory, in [`RepoWork::repo`]'s form.
    pub repo: String,
    /// `HEAD`'s commit in full lowercase hex (`git rev-parse --verify --quiet HEAD`).
    pub head: String,
    /// The remote default branch commits off it were counted against (`origin/main`;
    /// [`RepoWork::main_ref`]); empty where the repository has none.
    pub main_ref: String,
    /// Whether the repository holds any remote-tracking ref (`for-each-ref refs/remotes`, a
    /// read that answered: one git refused names no reading): `false`, nothing was pushed
    /// anywhere, so no unpushed commit was counted.
    pub remote: bool,
}

/// What one probe of a session's repository read ([`probe`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RepoRead {
    /// Nothing no remote main has, and nothing checked to name: the probe
    /// cannot run here (no `git`, no `HOME`, no repository, a folder macOS
    /// guards, a network or automounted volume), so it read nothing; or it
    /// found nothing in a repository whose `HEAD` it could not name (no commit
    /// yet, or that one read cut after every other read finished) or where git
    /// refused a read its counts rest on (the stash list, the remote-tracking
    /// refs, an unpushed or unlanded count).
    #[default]
    Clean,
    /// Nothing no remote main has, in the repository and at the commit it names
    /// ([`CleanAt`]): the probe finished every read and found nothing. Every
    /// decision reads it as [`Self::Clean`] ([`Self::is_clean`], [`Self::same`]).
    Checked(CleanAt),
    /// What the repository holds that no remote main has.
    Found(RepoWork),
    /// A read of the probe did not end within its limit (a hung `git`), or
    /// the probe could not read the repository it found (a `git` that could not
    /// start for want of a file descriptor or process, one a signal ended,
    /// a status read a broken index or submodule fails): what the repository
    /// holds is not known.
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
            Self::Clean | Self::Checked(_) | Self::Unread => None,
        }
    }

    /// The finding, owned ([`Self::found`]).
    #[must_use]
    pub fn into_found(self) -> Option<RepoWork> {
        match self {
            Self::Found(work) => Some(work),
            Self::Clean | Self::Checked(_) | Self::Unread => None,
        }
    }

    /// What a clean probe checked, where it named it ([`Self::Checked`]).
    #[must_use]
    pub const fn checked(&self) -> Option<&CleanAt> {
        match self {
            Self::Checked(at) => Some(at),
            Self::Clean | Self::Found(_) | Self::Unread => None,
        }
    }

    /// Whether the reading is clean: [`Self::Clean`] (nothing found, or no
    /// probe can run here) or [`Self::Checked`] (nothing found at a named `HEAD`).
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        matches!(self, Self::Clean | Self::Checked(_))
    }

    /// Whether the probe could not read the repository ([`Self::Unread`]).
    #[must_use]
    pub const fn is_unread(&self) -> bool {
        matches!(self, Self::Unread)
    }

    /// Whether two probes read the same work: both clean ([`Self::is_clean`]: a
    /// checked reading is the same as a clean one, whatever `HEAD` each names),
    /// or both the same finding of a known state ([`RepoWork::same`]). An unread
    /// probe never matches.
    #[must_use]
    pub fn same(a: &Self, b: &Self) -> bool {
        match (a, b) {
            (Self::Clean | Self::Checked(_), Self::Clean | Self::Checked(_)) => true,
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
        let behind = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let ahead = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
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

/// What the repository `dir` is in holds that no remote main has.
/// Each read has `limit` from the end of the one before it (the first, from
/// the call), so reads that each end in time are read whole however long they
/// take together. A read cut by its limit, and a repository whose reads could
/// not be made, is [`RepoRead::Unread`], not a clean result.
#[must_use]
pub fn probe(dir: &Path, home: Option<&Path>, limit: Duration) -> RepoRead {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    probe_with(dir, home, xdg.as_deref(), limit)
}

/// [`probe`] with the person's `XDG_CONFIG_HOME` given (`None`: unset).
#[must_use]
pub fn probe_with(
    dir: &Path,
    home: Option<&Path>,
    xdg: Option<&Path>,
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
    };
    match probe_reads(dir, &reads) {
        Some(read) => read,
        None if reads.cut.get() => RepoRead::Unread,
        None => RepoRead::Clean,
    }
}

/// A PROBE READ BESIDE ITS ASKER ([`crate::supervise::IdleHost::repo_work`]):
/// its reads go on on a thread of their own, and an ask waits a bounded time
/// for their answer, so the loop that asks keeps its claim and sees its stop
/// between asks however long the reads take together ([`PROBE_LIMIT`] bounds
/// each read).
#[derive(Debug, Default)]
pub struct Probing {
    /// The answer of the probe that reads now, until an ask takes it.
    reading: Mutex<Option<mpsc::Receiver<RepoRead>>>,
}

impl Probing {
    /// The answer of the probe that reads now, or else of the one `begin`
    /// hands it, once that probe ends within `wait`; `None` while it reads on
    /// past `wait`, or once `stop` is raised, the probe left reading for the
    /// next ask. A probe whose thread could not start, or that panicked, is
    /// [`RepoRead::Unread`].
    pub fn ask<F>(
        &self,
        begin: impl FnOnce() -> F,
        wait: Duration,
        stop: &AtomicBool,
    ) -> Option<RepoRead>
    where
        F: FnOnce() -> RepoRead + Send + 'static,
    {
        let mut reading = self.reading.lock().unwrap_or_else(PoisonError::into_inner);
        let answer = if let Some(answer) = reading.take() {
            answer
        } else {
            let read = begin();
            let (said, answer) = mpsc::sync_channel(1);
            let started = std::thread::Builder::new()
                .name("aterm-repo-probe".into())
                .spawn(move || {
                    if let Ok(found) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read))
                    {
                        let _ = said.send(found);
                    }
                });
            if started.is_err() {
                return Some(RepoRead::Unread);
            }
            answer
        };
        let by = Instant::now().checked_add(wait);
        loop {
            let left = by.map_or(STOP_LOOK, |by| by.saturating_duration_since(Instant::now()));
            match answer.recv_timeout(left.min(STOP_LOOK)) {
                Ok(found) => return Some(found),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Some(RepoRead::Unread),
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

/// [`probe_with`]'s reads: the finding ([`RepoRead::Found`]), or, where it
/// found nothing, what it checked ([`RepoRead::Checked`]); `None` for nothing
/// to name. A finding whose state read was cut keeps its counts, its state
/// unknown ([`RepoWork::state`]).
fn probe_reads(dir: &Path, reads: &Reads<'_>) -> Option<RepoRead> {
    let home = reads.home;
    let table = aterm_sysprobe::volumes::table();
    let dir = unattended_dir(dir, Some(home), &table)?;
    let top = git(&dir, &["rev-parse", "--show-toplevel"], reads)?;
    let top = top.trim_end_matches(['\n', '\r']).to_string();
    if top.is_empty() {
        return None;
    }
    let root = unattended_dir(Path::new(&top), Some(home), &table)?;
    let excludes = (!own_excludes(&root, reads))
        .then(|| global_excludes(&root, &table, reads))
        .flatten()
        .and_then(|p| p.to_str().map(|p| format!("core.excludesFile={p}")));
    let mut status_args: Vec<&str> = Vec::new();
    if let Some(flag) = &excludes {
        status_args.extend(["-c", flag]);
    }
    status_args.extend(["status", "--porcelain=v1", "-z", "--untracked-files=normal"]);
    let Some(status) = git(&root, &status_args, reads) else {
        // A repository whose state cannot be read holds what it holds: never nothing.
        reads.cut.set(true);
        return None;
    };
    let status = without_lagging_submodules(&root, &status, excludes.as_deref(), reads);
    let branch = git(
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        reads,
    )
    .map(|b| b.trim().to_string())
    .filter(|b| !b.is_empty());
    let listed = git(&root, &["stash", "list", "--format=%gs"], reads);
    let stash = stashes_on(listed.as_deref().unwrap_or_default(), branch.as_deref());
    let remotes = git(
        &root,
        &[
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "refs/remotes",
        ],
        reads,
    );
    let remote = remotes.as_deref().is_some_and(|r| !r.trim().is_empty());
    let pushed = remote.then(|| {
        git(
            &root,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
            reads,
        )
        .and_then(|n| n.trim().parse().ok())
    });
    let unpushed = pushed.flatten().unwrap_or(0);
    let main = main_ref(&root, reads);
    let landed = main.as_ref().map(|full| {
        git(
            &root,
            &["rev-list", "--count", &format!("{full}..HEAD")],
            reads,
        )
        .and_then(|n| n.trim().parse().ok())
    });
    let (unlanded, main_ref) = match (&main, landed) {
        (Some(full), Some(Some(n))) => (n, full.trim_start_matches("refs/remotes/").to_string()),
        _ => (0, String::new()),
    };
    // Whether every read a clean finding's counts rest on gave an answer: a
    // read git refused (an unreadable ref, output that is no text) counted
    // nothing, so a clean reading over it names nothing it did not read.
    let counted = listed.is_some()
        && remotes.is_some()
        && pushed.is_none_or(|n| n.is_some())
        && landed.is_none_or(|n| n.is_some());
    let cut_before_head = reads.cut.get();
    let head = git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"], reads);
    let Some(mut work) = RepoWork::of_git(&top, &status, &stash, unpushed, unlanded, &main_ref)
    else {
        // Nothing found. The head read is the clean path's last and only adds a
        // name: a reading cut before it stays unread, and a head read cut or
        // refused leaves the reading what it was before the head was read. A
        // count git refused names nothing either: clean, as it read before.
        reads.cut.set(cut_before_head);
        if cut_before_head || !counted {
            return None;
        }
        let head = head.as_deref().map(str::trim).filter(|h| is_head(h))?;
        return Some(RepoRead::Checked(CleanAt {
            repo: printable(&top),
            head: head.to_ascii_lowercase(),
            main_ref: printable(&main_ref),
            remote,
        }));
    };
    let mut all_args: Vec<&str> = Vec::new();
    if let Some(flag) = &excludes {
        all_args.extend(["-c", flag]);
    }
    all_args.extend(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let head = head.unwrap_or_default();
    work.state = git(&root, &all_args, reads)
        .and_then(|listed| state_of(&root, head.trim(), &stash, &listed));
    Some(RepoRead::Found(work))
}

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
            "--format=%(refname)",
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
        .and_then(|n| n.trim().parse().ok())
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
        if code.starts_with(['R', 'C']) {
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
        let renamed = entry.starts_with(['R', 'C']);
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
    let mut file = std::fs::File::open(path).ok()?;
    let len = file
        .metadata()
        .ok()
        .filter(std::fs::Metadata::is_file)?
        .len();
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
        if code.starts_with(['R', 'C']) {
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
    if !path.is_absolute() || atpkg::protected::under_protected_root(home, &path) {
        return None;
    }
    let path = super::disk::idle::resolve_local(&path, table)?;
    (!atpkg::protected::under_protected_root(home, &path) && path.is_file()).then_some(path)
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
    (!atpkg::protected::under_protected_root(&home, &dir)).then_some(dir)
}

fn git(dir: &Path, args: &[&str], reads: &Reads<'_>) -> Option<String> {
    git_as(dir, args, reads, false)
}

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
    run_git(dir, args, reads, global, false, None)
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
    run_git(dir, args, reads, false, true, feed)
}

/// [`git_as`], or with `offline` [`git_offline`].
fn run_git(
    dir: &Path,
    args: &[&str],
    reads: &Reads<'_>,
    global: bool,
    offline: bool,
    feed: Option<std::fs::File>,
) -> Option<String> {
    let deadline = reads.deadline.get();
    #[cfg(test)]
    let deadline = deadline
        .checked_sub(read_stall::spent())
        .unwrap_or_else(Instant::now);
    if Instant::now() >= deadline {
        reads.cut.set(true);
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
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
        return None;
    }
    String::from_utf8(output.stdout).ok()
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

#[cfg(test)]
#[path = "repo_work_tests.rs"]
pub(crate) mod tests;
