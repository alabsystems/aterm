// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A SESSION'S REPOSITORY HOLDS THAT NO REMOTE MAIN HAS: the probe behind
//! the turn-end policy's repository suffix on Cruise's done check and Land's
//! nudge (`supervise::policy::turn_end::repo_work_suffix`), and behind
//! Land's fourth landing condition: no defense lands a task over a finding
//! (Autopilot Land: all of the work in the remote main branch, no exceptions).
//!
//! [`probe`] runs in the host, never in the pure policy: `git` through
//! `aterm_uds::spawnfd::command`, each call bounded by what is left of one
//! limit, in the session's working directory as the host knows it (the
//! transcript's `cwd`, else the session's own). It looks for
//!
//! * tracked changes, staged or not (`git status --porcelain -z`, `XY` other
//!   than `??`);
//! * untracked files git does not ignore (`??`);
//! * stash entries made on `HEAD`'s branch (`git stash list`, by each
//!   entry's `WIP on <branch>:` or `On <branch>:` subject, [`stashes_on`];
//!   on a detached `HEAD`, `(no branch)`): a repository's stash is shared by
//!   all its worktrees, and another worktree's entry is another session's
//!   work;
//! * commits on `HEAD` that no remote-tracking ref contains (`git rev-list
//!   --count HEAD --not --remotes`) — counted only where the repository has
//!   a remote-tracking ref at all: a repository with no remote has nowhere
//!   to push, and every commit of it would read as unpushed;
//! * commits on `HEAD` that the remote's default branch does not contain
//!   (`git rev-list --count <default>..HEAD`, [`RepoWork::unlanded`]): a
//!   commit pushed only to a feature branch is pushed, and still not landed.
//!   The default branch is `refs/remotes/origin/HEAD`'s target, else
//!   `origin/main`, else `origin/master`, each only where it names a commit;
//!   with none of them the count is unknown and finds nothing, as a probe
//!   that cannot run does. Only `HEAD` is read: other branches and other
//!   worktrees of a shared repository are other sessions' work, never this
//!   one's. A branch landed by squash or rebase (other commits on main) reads
//!   as unlanded until `HEAD` moves onto the landed main.
//!
//! A probe that cannot run — no `git`, no repository, a folder macOS guards
//! with a consent dialog (`atpkg::protected`: the probe is unattended, and
//! must never put one on the owner's screen), a call past its limit — returns
//! `None`, as does a repository with nothing in it no remote has. `None` is
//! not evidence that the repository is clean. The reads take no optional lock
//! (`GIT_OPTIONAL_LOCKS=0`) and disable fsmonitor, hooks and pagers. Inherited
//! environment and system/global Git configuration are not passed through;
//! repository-local configuration, including clean/process filters, still applies.
//! Each read's `HOME` is the person's home, for the `~/` a repository's own
//! path values may hold (with none, Git dies on one at its first read), and
//! its `XDG_CONFIG_HOME` points at nothing, so Git's default files under it
//! are not read unchecked.
//! ONE GLOBAL SETTING IS: the person's global excludes file
//! ([`global_excludes`]: `core.excludesFile` from their global
//! configuration and the files it includes, else Git's default
//! `$XDG_CONFIG_HOME/git/ignore`), handed to the status read alone (`-c
//! core.excludesFile=`), so a file they ignore everywhere (`.DS_Store`) is no
//! untracked work here, as their own `git status` says — and only where the
//! repository sets no `core.excludesFile` of its own ([`own_excludes`]),
//! which Git reads instead of theirs. Only that one value is read from their
//! configuration, by `git config --global --includes --path --get` run in
//! the repository (its conditional includes judged against it), which
//! runs nothing.
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

/// The most files the work's state is read over ([`RepoWork::state`]):
/// past that it is not read.
pub const STATE_FILES: usize = 4096;

/// The longest path, in characters, a finding names whole: a longer one
/// keeps its last characters after `…`.
const PATH_CHARS: usize = 72;

/// WHAT ONE REPOSITORY HOLDS THAT NO REMOTE MAIN HAS ([`probe`]).
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
    /// Commits on `HEAD` the remote's default branch ([`Self::main_ref`])
    /// does not contain, pushed elsewhere or not at all; zero where no
    /// default branch is known.
    pub unlanded: usize,
    /// The remote default branch [`Self::unlanded`] counts against, as `git`
    /// names it short (`origin/main`); empty where none is known.
    pub main_ref: String,
    /// The first [`NAMED_PATHS`] changed or untracked paths, each printable
    /// ([`printable`]).
    pub paths: Vec<String>,
    /// THE WORK'S STATE, hashed ([`state_of`]): `HEAD`'s commit, the stash
    /// entries of its branch, and every changed or untracked file git does
    /// not ignore (an untracked directory's files each), by its status
    /// code, its size and its modification time. Two findings with the
    /// same counts and the same named paths can hold different work — an
    /// edit to a file already changed, a new file in a directory already
    /// untracked, a path past the named ones — and only this tells them
    /// apart ([`Self::same`]). `None`: not read (more than [`STATE_FILES`]
    /// files, a read that failed, a finding a test or an older tape made).
    pub state: Option<u64>,
}

impl RepoWork {
    /// Whether anything was found.
    #[must_use]
    pub fn found(&self) -> bool {
        self.changed > 0
            || self.untracked > 0
            || self.stashes > 0
            || self.unpushed > 0
            || self.unlanded > 0
    }

    /// The finding of the reads' output: `repo` the top level, `status` the
    /// NUL-separated `git status --porcelain -z`, `stash` the stash entries
    /// of `HEAD`'s branch, one a line ([`stashes_on`]), `unpushed` the count
    /// of commits no remote-tracking ref
    /// contains, `unlanded` the count of commits the remote default branch
    /// `main_ref` (short, `origin/main`; empty where none is known) does not
    /// contain. `None` where nothing was found.
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

    /// WHETHER TWO PROBES FOUND THE SAME WORK, nothing in it touched
    /// between them: both found nothing, or both found the same finding and
    /// the same known state ([`Self::state`]). A state not read on either
    /// side says nothing of what changed between the two, so the work is
    /// not the same: an edit the counts cannot see is never ruled out.
    #[must_use]
    pub fn same(a: Option<&Self>, b: Option<&Self>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a.state.is_some() && a == b,
            _ => false,
        }
    }

    /// The finding in a few words, for the repository suffix and the
    /// journal: `/r: 2 changed, 1 untracked (a.rs, b/, …), 1 stash, 3
    /// unpushed commits, 4 commits not on origin/main`.
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

/// WHAT THE REPOSITORY `dir` IS IN HOLDS THAT NO REMOTE MAIN HAS, by `git`, the
/// whole probe within `limit`: `None` where nothing is found or the probe
/// cannot run (the module's rules). `home` is the person's home, for the
/// folders macOS guards ([`atpkg::protected::under_protected_root`]) and
/// their global excludes ([`global_excludes`], `XDG_CONFIG_HOME` as this
/// process has it).
#[must_use]
pub fn probe(dir: &Path, home: Option<&Path>, limit: Duration) -> Option<RepoWork> {
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
) -> Option<RepoWork> {
    let home = home?;
    if atpkg::protected::under_protected_root(home, dir) {
        return None;
    }
    let reads = Reads {
        deadline: Instant::now().checked_add(limit)?,
        home,
        xdg: xdg.filter(|x| x.is_absolute()),
    };
    let reads = &reads;
    let table = aterm_sysprobe::volumes::table();
    let dir = unattended_dir(dir, Some(home), &table)?;
    let top = git(&dir, &["rev-parse", "--show-toplevel"], reads)?;
    let top = top.trim_end_matches(['\n', '\r']).to_string();
    if top.is_empty() {
        return None;
    }
    let root = unattended_dir(Path::new(&top), Some(home), &table)?;
    // The repository's own `core.excludesFile` wins over the person's, as in
    // their own `git status`: theirs is handed over only where it sets none.
    let excludes = (!own_excludes(&root, reads))
        .then(|| global_excludes(&root, &table, reads))
        .flatten()
        .and_then(|p| p.to_str().map(|p| format!("core.excludesFile={p}")));
    let mut status_args: Vec<&str> = Vec::new();
    if let Some(flag) = &excludes {
        status_args.extend(["-c", flag]);
    }
    status_args.extend(["status", "--porcelain=v1", "-z", "--untracked-files=normal"]);
    let status = git(&root, &status_args, reads)?;
    let branch = git(
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        reads,
    )
    .map(|b| b.trim().to_string())
    .filter(|b| !b.is_empty());
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
    let (unlanded, main_ref) = main_ref(&root, reads)
        .and_then(|full| {
            let n = git(
                &root,
                &["rev-list", "--count", &format!("{full}..HEAD")],
                reads,
            )?
            .trim()
            .parse()
            .ok()?;
            Some((n, full.trim_start_matches("refs/remotes/").to_string()))
        })
        .unwrap_or_default();
    let mut work = RepoWork::of_git(&top, &status, &stash, unpushed, unlanded, &main_ref)?;
    // The work's state: every untracked file listed alone (a directory's
    // files each), by the same excludes, with `HEAD`'s commit.
    let mut all_args: Vec<&str> = Vec::new();
    if let Some(flag) = &excludes {
        all_args.extend(["-c", flag]);
    }
    all_args.extend(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let head = git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"], reads).unwrap_or_default();
    work.state = git(&root, &all_args, reads)
        .and_then(|listed| state_of(&root, head.trim(), &stash, &listed));
    Some(work)
}

/// THE WORK'S STATE ([`RepoWork::state`]) of the repository at `root`:
/// `head` its `HEAD` commit (empty where it has none), `stash` its branch's
/// stash entries ([`stashes_on`]), `listed` the NUL-separated `git status
/// --porcelain -z --untracked-files=all`, each entry hashed with its status
/// code and the file's size and modification time (`-` for a path with no
/// file, a deletion). `None` past [`STATE_FILES`] entries. Reads no file's
/// contents, and follows no symbolic link.
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

/// THE STASH ENTRIES OF ONE BRANCH among `list` (`git stash list
/// --format=%gs`: each entry's subject, one a line), one a line: those Git
/// wrote on `branch` (`WIP on <branch>: …`, or `On <branch>: …` for a
/// message given), `None` for a detached `HEAD` (`(no branch)`). A
/// repository's stash is shared by its worktrees; an entry made on another
/// branch is another worktree's — another session's — work.
#[must_use]
pub fn stashes_on(list: &str, branch: Option<&str>) -> String {
    let name = branch.unwrap_or("(no branch)");
    let (wip, on) = (format!("WIP on {name}: "), format!("On {name}: "));
    list.lines()
        .filter(|l| l.starts_with(&wip) || l.starts_with(&on))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// WHETHER THE REPOSITORY AT `root` SETS ITS OWN `core.excludesFile`: its
/// own configuration (`git config --includes --get`, the system and global
/// files shut out as for every read here, so the local file, a worktree's and
/// what they include answer). Where it does, Git reads that one and never
/// the person's global one ([`global_excludes`]), and the probe hands theirs
/// over not at all: a `-c` would override the repository's own.
fn own_excludes(root: &Path, reads: &Reads<'_>) -> bool {
    git(
        root,
        &["config", "--includes", "--get", "core.excludesFile"],
        reads,
    )
    .is_some_and(|p| !p.trim().is_empty())
}

/// THE PERSON'S GLOBAL EXCLUDES FILE, which the probe's `git` — run with no
/// global configuration (the module's rules) — would not read:
/// `core.excludesFile` as their global configuration sets it, the files it
/// includes followed (`git config --global --includes --path --get`, with
/// their home and `XDG_CONFIG_HOME` and nothing else), else Git's default,
/// `<xdg>/git/ignore` (`<home>/.config/git/ignore` where `xdg` is unset).
/// The read runs in the repository at `root`, so a conditional include
/// (`includeIf "gitdir:…"`, `"onbranch:…"`) is judged against it as in
/// their own `git status` (run from `/`, none matched). Only a file that
/// resolves on a local mount ([`super::disk::idle::resolve_local`]) outside
/// the folders macOS guards; `None` otherwise.
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

/// THE REMOTE'S DEFAULT BRANCH in the repository at `root`, as a full ref,
/// for [`RepoWork::unlanded`]: `refs/remotes/origin/HEAD`'s target, else
/// `refs/remotes/origin/main`, else `refs/remotes/origin/master` — the first
/// that names a commit. `None` where none does (the count is unknown).
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
/// Every child shares `reads`' deadline; expired calls do not start. The existing
/// capture caps stdout and stderr memory and refuses missing stdout EOF instead
/// of joining a reader forever. Its drain grace and bounded kill/reap reserve
/// may extend past the execution deadline; refusal does not prove descendant exit.
fn git(dir: &Path, args: &[&str], reads: &Reads<'_>) -> Option<String> {
    git_as(dir, args, reads, false)
}

/// What every read of one probe shares ([`git`]): its deadline, and the
/// person's home and `XDG_CONFIG_HOME`.
struct Reads<'a> {
    /// Every child shares it; expired calls do not start.
    deadline: Instant,
    /// The person's home: every read's `HOME`, so a `~/` in a path the
    /// repository's own configuration names expands as in their own `git`
    /// (with none, Git dies on it at the first read: `fatal: failed to
    /// expand user dir`, and the probe found nothing).
    home: &'a Path,
    /// The person's `XDG_CONFIG_HOME` (absolute), for [`global_excludes`]'s
    /// read alone.
    xdg: Option<&'a Path>,
}

/// [`git`], or with `global` the one read of the person's global
/// configuration [`global_excludes`] makes: their `XDG_CONFIG_HOME` set, the
/// global configuration not shut out. Every other read has their `HOME`
/// (for `~/` alone) with the global configuration shut out and
/// `XDG_CONFIG_HOME` pointed at nothing, so Git's own default files under it
/// (`git/ignore`, `git/attributes`) are never read unchecked: the person's
/// default excludes reach the status read only through
/// [`global_excludes`]' checks.
fn git_as(dir: &Path, args: &[&str], reads: &Reads<'_>, global: bool) -> Option<String> {
    let deadline = reads.deadline;
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
    let output = aterm_uds::deadline::output_until_complete_stdout(&mut cmd, deadline).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
#[path = "repo_work_tests.rs"]
mod tests;
