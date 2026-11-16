// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK LOOK'S WORKTREE STEP (DC-02a, `docs/DESIGN-disk-custody-2026-11-12.md` §3.2): a
//! linked worktree an agent made that `origin/main` contains, that is clean and that no session
//! uses goes, and with it its merged local branch, with no command from anyone. Its shared build
//! cache goes at the next cache pass, as the cache of a workspace whose folder is gone.
//!
//! Only an agent's worktree is a candidate, read off its path: a folder named `*.noindex` with a
//! `.ubd` folder above it (the shape AGENTS.md gives an agent's worktree), or a folder of its
//! checkout's `.claude/worktrees/` (Claude Code's own isolation worktrees). A person's worktree
//! anywhere else (`~/myapp-develop`), and every worktree of a bare repository, stays whatever it
//! holds (`atpkg::build_cache::retired::judge_worktree` refuses it, and so does each remover).
//! Of those, a person's worktree that is merged, clean and quiet gives back its shared build
//! directories and nothing else (below).
//!
//! The rule needs no receipt of a session's start, so it covers every worktree standing today.
//! No session uses a worktree when no session's working directory is inside it, no open Claude
//! Code session's transcript or any of its subagents' names its folder (one whose transcript
//! cannot be named or read through keeps every worktree), nothing in it, in git's own files for
//! it or in its build folder was written for seven days, and no process uses it (the removal's
//! fence). The judgment and the removal are the retired verify gate's, opened to any linked
//! worktree (`atpkg::build_cache::retired::judge_worktree`, `remove_worktree`): a main checkout,
//! a locked worktree, one with any edit, untracked file, submodule change or commit
//! `origin/main` lacks stays, and so do the release's cut tree and the gate's snapshot. Each
//! removal is journalled before it starts and after it ends; one whose intent cannot be
//! journalled does not happen. The step never runs `fetch`, `stash`, `reset`,
//! `submodule deinit` or `worktree prune`, and it is bounded: judging stops at half its wall,
//! the transcripts are read by three quarters of it, no removal starts after it, and the git it
//! is handed (`retired::GitUntil`) kills a run and ends a walk that outlives its own bound.
//!
//! A MERGED, QUIET WORKTREE OUTSIDE AN AGENT'S SHAPE (DC-21) stays, and gives back its shared
//! build directories: one the merged rule would take but for where it sits (every clause of it
//! but the shape, `retired::judge_worktree_anywhere`; no session's working directory in it, no
//! open transcript naming it, nothing in it, in git's own files for it or in its build folder
//! written for the window) keeps its folder, its branch, its index and every file, and each
//! build directory of the shared cache that mirrors it or a workspace inside it goes whole
//! through the cache's own remover and fences (`atpkg::build_cache::reclaim_merged_worktree`:
//! cargo's locks, custody, the process check), journalled as `build-cache-sweep` rows before
//! and after; one whose intent cannot be journalled does not go. The rule runs only while
//! `[disk]` lets a standing checkout's idle build directory go whole
//! (`Config::idle_whole_on`), its window the longer of seven days and `disk.idle_build_days`.
//! A build-start line that names the worktree within that window keeps the directory (a build
//! that compiled nothing writes no file there), a worktree the cache holds no mirror of is not
//! judged at all, and the judgment is made again, once no process uses the worktree, just
//! before anything goes. A worktree outside the shape with commits `origin/main` lacks keeps
//! its build directory: the pinned rule below is the agents' alone. That second judgment reads
//! the build-start record again ([`Hooks::starts`]: the lines the step was handed are as old as
//! its look), whether a process uses the worktree is asked once more just before each build
//! directory's intent, and the remover's read of a directory's files ends with the step's
//! git. A worktree whose build directory was judged and stays is not judged for it again for
//! a day ([`BUILD_DIR_EVERY`], [`Schedule::rest`]): the judgment runs git in a person's
//! worktree, and a directory the remover keeps would otherwise cost it every hour.
//!
//! A WORKTREE WHOSE SESSION IS GONE (DC-02b) goes too, whatever it holds that git keeps a copy
//! of once it is on a ref: its commits `origin/main` lacks, its edits and untracked files, the
//! commits only its reflogs or its submodules hold, and the files it ignores that are no tagged
//! cache folder go on refs of its checkout first ([`pins`]), pushed to the repository's own
//! origin (the ignored files' ref never), and its removal is held to those pins
//! (`retired::remove_pinned_worktree`); its branch stays. Whether its session is gone is read
//! off the records alone: where every build-start line that names it (a build of a workspace at
//! or under it) names a session the window holds as exited (`SessionState::Exited`), it goes
//! once idle for [`RECORDS_QUIET`]; where no line names it, or a line names a session the
//! window does not hold (another window's, or one from before its start), once idle for seven
//! days; and where the window's sessions are not known (the headless watch), a worktree a line
//! names goes by the merged rule alone. A worktree a live session's line names stays on either
//! rule. IDLE: nothing in it, in git's own files for it or in its build folder was modified
//! within that window, no session's working directory is inside it, no open session's
//! transcript names it, and no process uses it (asked before its pins are written and again by
//! its removal, whose last read also finds nothing in it modified or changed since its pins
//! began to read it). It stays too while it hides an edit from git, a submodule checked out in
//! it holds a change, what its snapshot reads is more than [`pins::MAX_BYTES`] (the reason is
//! in [`pins`]), or what its pins owe an origin cannot be recorded; a push that fails is owed,
//! not a reason to keep it, and is tried again at a later step.
//!
//! ITS PINS AND PUSHES WAIT WHILE THE HOME VOLUME READS UNDER ITS EMERGENCY LINE (DC-06d,
//! `aterm_capacity::floor::custody_writes_wait`, [`Hooks::custody`]): a snapshot writes git
//! objects and a push writes a pack, so at zero free either fails or spends what recovery has
//! just freed. Such a worktree stays, nothing of it written, and one `deferred` row goes on
//! record for it ([`super::worktree_deferred_row`]); the owed pushes wait too, with one
//! `deferred` push row. The merged rule's removals, which pin nothing, still run. The step is
//! due again at the first look that reads the volume at or above the line ([`Schedule::due`]),
//! not an hour on: the wait is for a measurement, never for a person.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_sysprobe::volumes::Table;
use atpkg::build_cache::retired::{self, Admitted, Git, Pinned};
use atpkg::build_cache::{Leave, Unused};
use atpkg::build_start::Start;

use super::idle::{self, SessionCwd};
use super::{Refusal, SweepAction, WorktreeRow};

/// Pinning a gone session's worktree on refs, and pushing those refs.
#[path = "disk_worktree_git.rs"]
pub mod pins;

/// How long nothing in a worktree, nor in git's own files for it, was written before it is
/// pinned and goes, once the build-start record says its sessions are gone.
pub const RECORDS_QUIET: Duration = Duration::from_secs(60 * 60);

/// How often the step runs on one host at most.
pub const STEP_EVERY: Duration = Duration::from_secs(60 * 60);

/// How often at most the step judges one worktree that stays for its shared build directory,
/// once it judged it and the directory stayed ([`Schedule::rest`]).
pub const BUILD_DIR_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The whole step's wall-clock bound: no judgment and no removal starts after it.
pub const STEP_WALL: Duration = Duration::from_secs(120);

/// How long past [`STEP_WALL`] the git runs of the judgment or removal under way at it may go
/// before they are killed (and the worktree stays, or its removal resumes from its receipt).
pub const GIT_GRACE: Duration = Duration::from_secs(60);

/// How much of a transcript one read holds.
const SCAN_CHUNK: usize = 8 << 20;

/// The most subagent transcripts one session's folder may hold before the step reads none of
/// them and keeps every worktree. One workflow may run a thousand agents, and a session runs
/// several workflows.
const MAX_SUBAGENTS: usize = 8192;

/// How many levels of folders under a session's `subagents/` the step reads: a subagent's
/// transcript lies at the first, a workflow's agents' at the third (`workflows/<run>/`).
const SUBAGENT_DEPTH: usize = 3;

/// The ends of the names of the worktrees the step never judges: the retired gate's snapshot,
/// which `aterm pkg machine apply` judges, and the release's cut tree, which each release reuses
/// (`aterm_release::gates::cut_tree_path`).
const NOT_JUDGED: &[&str] = &["-verify.noindex", "-cut.noindex"];

/// The most entries of a checkout's git directories one branch check lists.
const MAX_ENTRIES: usize = 4096;

/// What a folder is to git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A main checkout: its `.git` is a folder.
    Main,
    /// A linked worktree of the main checkout `checkout`: its `.git` is a file naming
    /// `<checkout>/.git/worktrees/<id>`.
    Linked {
        /// Its main checkout, resolved.
        checkout: PathBuf,
    },
    /// Anything else: a submodule's checkout, another repository's gitfile, a link, no `.git`.
    Other,
}

/// What `dir` is to git, read off its `.git` alone.
#[must_use]
pub fn kind(dir: &Path) -> Kind {
    let gitfile = dir.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&gitfile) else {
        return Kind::Other;
    };
    if meta.is_dir() {
        return Kind::Main;
    }
    if !meta.is_file() {
        return Kind::Other;
    }
    linked_checkout(dir, &gitfile).map_or(Kind::Other, |checkout| Kind::Linked { checkout })
}

/// The main checkout whose worktree entry `gitfile`, the `.git` file of `dir`, names.
fn linked_checkout(dir: &Path, gitfile: &Path) -> Option<PathBuf> {
    let entry = entry_of(dir, gitfile)?;
    let worktrees = entry.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = std::fs::canonicalize(worktrees.parent()?).ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    let checkout = common.parent()?;
    let own = checkout.join(".git");
    (std::fs::symlink_metadata(&own).is_ok_and(|m| m.is_dir())
        && std::fs::canonicalize(&own).ok().as_ref() == Some(&common))
    .then(|| checkout.to_path_buf())
}

/// The git entry the `.git` file `gitfile` of `dir` names (its `gitdir:` line).
fn entry_of(dir: &Path, gitfile: &Path) -> Option<PathBuf> {
    let text = super::discover::small_text(gitfile)?;
    let named = Path::new(text.lines().next()?.strip_prefix("gitdir:")?.trim());
    Some(if named.is_absolute() {
        named.to_path_buf()
    } else {
        dir.join(named)
    })
}

/// When git registered `wt`: its entry's `gitdir` file's time, the epoch when it cannot be read
/// (a worktree whose folder is gone sorts first, and its removal is finished first).
fn registered_at(wt: &Path) -> SystemTime {
    entry_of(wt, &wt.join(".git"))
        .and_then(|entry| std::fs::metadata(entry.join("gitdir")).ok())
        .and_then(|meta| meta.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// One open Claude Code session as the step reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSession {
    /// Its process (0: the census itself could not be read).
    pub pid: u32,
    /// Its working directory, when the kernel says it.
    pub cwd: Option<PathBuf>,
    /// The transcript it writes, or why it cannot be named.
    pub transcript: Result<PathBuf, String>,
}

/// Every Claude Code session open now under `home` (`disk::discover::running_claude_census`),
/// each with the transcript it writes: one `pid 0` row whose transcript is an `Err` when the
/// census could not be read, so the step keeps everything.
#[must_use]
pub fn open_sessions(home: &Path) -> Vec<OpenSession> {
    let running = match super::discover::running_claude_census(Some(home)) {
        Ok(running) => running,
        Err(why) => {
            return vec![OpenSession {
                pid: 0,
                cwd: None,
                transcript: Err(why),
            }];
        }
    };
    running
        .claudes
        .into_iter()
        .map(|claude| {
            let transcript = match &claude.started {
                None => Err("its start time could not be read".to_owned()),
                Some(start) => transcript_of(home, claude.pid, start),
            };
            OpenSession {
                pid: claude.pid,
                cwd: claude.cwd,
                transcript,
            }
        })
        .collect()
}

/// The transcript the Claude Code process `pid`, started at `start`, writes, read through its
/// own environment as `upgrade_drive::snapshot_transcript_end` reads it.
fn transcript_of(home: &Path, pid: u32, start: &str) -> Result<PathBuf, String> {
    let args = atpkg::caller_shell::process_args(pid);
    let (config_dir, user_home, started_in) = match &args {
        Some(args) => (
            args.env_var(crate::harness::accounts::HOME_ENV),
            args.env_var(crate::harness::accounts::HOMEDIR_ENV),
            args.env_var("PWD"),
        ),
        None => (None, home.to_str(), None),
    };
    crate::harness::upgrade_drive::process_transcript(config_dir, user_home, started_in, pid, start)
        .map(|(path, _)| path)
        .ok_or_else(|| "no single transcript of it was found".to_owned())
}

/// One session the window holds, as the step reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSeen {
    /// Its session id.
    pub sid: String,
    /// Whether it is live (spawning or alive) rather than exited.
    pub live: bool,
}

/// What the step reads beyond the worktrees themselves.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The home whose children are the checkouts.
    pub home: &'a Path,
    /// Where transcripts live: nothing at, under or over it goes.
    pub transcripts_root: Option<&'a Path>,
    /// Every session's working directory, each as clause 4a reads it.
    pub cwds: &'a [SessionCwd],
    /// Why some session's working directory is not known: while it stands nothing goes.
    pub cwd_unknown: Option<&'a str>,
    /// The open Claude Code sessions.
    pub open: &'a [OpenSession],
    /// The mount table.
    pub table: Option<&'a Table>,
    /// Every checkout and worktree discovery found: one inside a candidate keeps it.
    pub known: &'a [PathBuf],
    /// The sessions the window holds, `None` where they are not known (the headless watch):
    /// a build-start line then shortens no worktree's quiet.
    pub roster: Option<&'a [SessionSeen]>,
    /// The build-start record's lines ([`atpkg::build_start::read`]).
    pub starts: &'a [Start],
    /// The file of pushes the pins owe ([`pins::OWED_FILE`] in aterm's state folder), `None`
    /// where there is none: a worktree whose session is gone then goes only by the merged rule.
    pub owed: Option<&'a Path>,
    /// How long a kept worktree's shared build directory must be unwritten before it may go,
    /// `None` where `[disk]` turns that off (`Config::idle_whole_on`).
    pub build_dirs: Option<Duration>,
    /// The worktrees whose build directory a step judged within the last day, and it stayed
    /// ([`Schedule::rested`]): none is judged for it again yet.
    pub build_rested: &'a [PathBuf],
}

/// A candidate the judgment admitted, by the rule it goes by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Merged into `origin/main`, clean and unused for seven days.
    Merged(Admitted),
    /// Outside an agent's shape, merged, clean and quiet: it stays, and its shared build
    /// directory goes.
    BuildDir(Admitted),
    /// Its session gone, quiet for `quiet`: it goes once its pins hold what it holds.
    Pinned {
        /// The pinned rule's admission.
        admitted: Admitted,
        /// How long it was quiet.
        quiet: Duration,
    },
}

impl Verdict {
    /// The admission.
    #[must_use]
    pub const fn admitted(&self) -> &Admitted {
        match self {
            Self::Merged(admitted) | Self::BuildDir(admitted) | Self::Pinned { admitted, .. } => {
                admitted
            }
        }
    }
}

/// One worktree the step removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    /// The worktree.
    pub path: PathBuf,
    /// Its main checkout.
    pub checkout: PathBuf,
    /// Its branch (the full `refs/heads/…`), when it was on one.
    pub branch: Option<String>,
    /// Whether its branch went, or why it stays.
    pub branch_deleted: Result<(), String>,
    /// Its bytes, walked before its removal.
    pub bytes: u64,
    /// The refs its commits and edits went on first, when it went by the pinned rule.
    pub pins: Option<String>,
}

/// One shared build directory a worktree that stays gave back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GivenBack {
    /// The worktree, which stays.
    pub worktree: PathBuf,
    /// The build directory, or the stage of it a removal left part of.
    pub path: PathBuf,
    /// Why it went, or what stayed of it (`Unused::Stayed`, `Unused::Revoked`).
    pub why: Unused,
    /// The bytes of it that went.
    pub bytes: u64,
}

/// What one step did.
#[derive(Debug, Default)]
pub struct Done {
    /// What went.
    pub removed: Vec<Removed>,
    /// The shared build directories the worktrees that stay gave back.
    pub given_back: Vec<GivenBack>,
    /// What stays, each with why.
    pub kept: Vec<(PathBuf, String)>,
    /// The worktrees that stay whose shared build directory this step could have judged, and
    /// it stays too: each rests a day before it is judged for it again ([`Schedule::rest`]).
    pub build_kept: Vec<PathBuf>,
    /// What was refused before anything of it was touched (an intent that could not be
    /// journalled).
    pub refused: Vec<Refusal>,
    /// Why the step stopped before it was through, the rest left for its next run.
    pub unfinished: Option<String>,
    /// How many owed pushes went to their origins.
    pub pushed: usize,
    /// The pushes still owed, each with why its last try failed.
    pub owed: Vec<pins::Owed>,
    /// The pushes origin refuses for good, each with why: tried no more.
    pub parked: Vec<pins::Owed>,
    /// Why what the pins owe could not be read or written, when it could not.
    pub unrecorded: Option<String>,
    /// The worktrees whose pins wait for the volume to read above its emergency line, each with
    /// why ([`Hooks::custody`]): each stays, nothing of it written.
    pub deferred: Vec<(PathBuf, String)>,
    /// Why the owed pushes wait for the same, when some were owed and they did.
    pub pushes_deferred: Option<String>,
}

impl Done {
    /// Whether some write this step owed waits for the volume to read above its emergency line
    /// ([`Self::deferred`], [`Self::pushes_deferred`]): the step is then due again at the first
    /// look that reads it there ([`Schedule::ran`]).
    #[must_use]
    pub fn waits(&self) -> bool {
        !self.deferred.is_empty() || self.pushes_deferred.is_some()
    }

    /// The bytes the removed worktrees held, and those of the build directories given back.
    #[must_use]
    pub fn freed(&self) -> u64 {
        self.removed_bytes().saturating_add(self.given_back_bytes())
    }

    /// The bytes the removed worktrees held.
    #[must_use]
    pub fn removed_bytes(&self) -> u64 {
        self.removed
            .iter()
            .map(|r| r.bytes)
            .fold(0, u64::saturating_add)
    }

    /// The bytes of the build directories the worktrees that stay gave back.
    #[must_use]
    pub fn given_back_bytes(&self) -> u64 {
        self.given_back
            .iter()
            .map(|g| g.bytes)
            .fold(0, u64::saturating_add)
    }
}

/// When the step runs next on this host, the candidate it reached last, what it last put
/// on record of each worktree it kept once admitted, when it last judged each worktree
/// that stays for its build directory and kept that, and whether its last run left writes
/// waiting for the volume to read above its emergency line.
#[derive(Debug, Default)]
pub struct Schedule {
    next: Option<Instant>,
    after: Option<PathBuf>,
    said: BTreeMap<PathBuf, String>,
    rested: BTreeMap<PathBuf, Instant>,
    waiting: bool,
}

impl Schedule {
    /// A step not yet run: due at once.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next: None,
            after: None,
            said: BTreeMap::new(),
            rested: BTreeMap::new(),
            waiting: false,
        }
    }

    /// What the step last journalled of each worktree it kept once admitted ([`Hooks::said`]).
    pub const fn said(&mut self) -> &mut BTreeMap<PathBuf, String> {
        &mut self.said
    }

    /// Whether the step is due at `at`: an hour after its last run ([`STEP_EVERY`]), or at once
    /// where that run left writes waiting ([`Self::ran`]) and `clear` says the look reads the
    /// home volume at or above its emergency line (`aterm_capacity::floor::custody_writes_wait`
    /// false). `clear` is asked only then.
    #[must_use]
    pub fn due(&self, at: Instant, clear: impl FnOnce() -> bool) -> bool {
        self.next.is_none_or(|next| at >= next) || (self.waiting && clear())
    }

    /// Whether a run at `at` stands down at once: one before its hour, which only the writes
    /// waiting for the volume made due, while `under` says the volume, read again now, is still
    /// under its emergency line. `under` is asked only then.
    #[must_use]
    pub fn stands_down(&self, at: Instant, under: impl FnOnce() -> bool) -> bool {
        self.waiting && self.next.is_some_and(|next| at < next) && under()
    }

    /// The candidate the last run reached before it stopped short: the next run starts after it.
    #[must_use]
    pub fn after(&self) -> Option<&Path> {
        self.after.as_deref()
    }

    /// The step ran at `at`, reaching `reached` when it stopped short, and left writes waiting
    /// for the volume (`waits`, [`Done::waits`]) or none. Writes the run before left waiting
    /// still wait where this run stopped short, since it may not have reached them: the step
    /// stays due at the next look above the line, starting after `reached`.
    pub fn ran(&mut self, at: Instant, reached: Option<PathBuf>, waits: bool) {
        self.next = Some(at.checked_add(STEP_EVERY).unwrap_or(at));
        self.waiting = waits || (self.waiting && reached.is_some());
        self.after = reached;
    }

    /// The worktrees whose build directory a step judged, and it stayed, less than
    /// [`BUILD_DIR_EVERY`] before `at` ([`Facts::build_rested`]).
    #[must_use]
    pub fn rested(&self, at: Instant) -> Vec<PathBuf> {
        self.rested
            .iter()
            .filter(|(_, since)| at.saturating_duration_since(**since) < BUILD_DIR_EVERY)
            .map(|(wt, _)| wt.clone())
            .collect()
    }

    /// The step at `at` judged each of `kept` for its build directory, which stays
    /// ([`Done::build_kept`]): none is judged for it again within [`BUILD_DIR_EVERY`]. A rest
    /// that has passed is forgotten.
    pub fn rest(&mut self, at: Instant, kept: &[PathBuf]) {
        self.rested
            .retain(|_, since| at.saturating_duration_since(*since) < BUILD_DIR_EVERY);
        for wt in kept {
            self.rested.insert(wt.clone(), at);
        }
    }
}

/// The receipt-free rule's own clauses for the worktree `wt` (the rest is the judge's): (a) no
/// session works in it; (b) no other checkout or worktree discovery found lies inside it; (c)
/// it is not at, under or over the transcripts root.
fn unused_here(facts: &Facts<'_>, wt: &Path) -> Result<(), String> {
    idle::sessions_clear(facts.cwds, facts.cwd_unknown, wt, wt, facts.table)?;
    if let Some(inside) = facts
        .known
        .iter()
        .find(|k| k.as_path() != wt && k.starts_with(wt))
    {
        return Err(format!(
            "{} lies inside it, and would go with it",
            inside.display()
        ));
    }
    if let Some(root) = facts.transcripts_root
        && (root.starts_with(wt) || wt.starts_with(root))
    {
        return Err(Refusal::Transcript(wt.to_path_buf()).describe());
    }
    Ok(())
}

/// The receipt-free rule's build-folder clause (d): the shared build cache's mirror of `wt`
/// (`<cache root><wt>`, under the current root and the one builds moved from) holds nothing
/// written within `quiet` (seven days by the merged rule).
fn build_folder_quiet(
    facts: &Facts<'_>,
    wt: &Path,
    now: SystemTime,
    until: Option<Instant>,
    quiet: Duration,
) -> Result<(), String> {
    let since = now
        .checked_sub(quiet)
        .ok_or("the clock reads too early to date how long it was quiet")?;
    let mut spellings = vec![wt.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(wt)
        && real != wt
    {
        spellings.push(real);
    }
    for root in [
        atpkg::build_cache::cache_root(facts.home),
        atpkg::build_cache::moved_from(facts.home),
    ] {
        for spelled in &spellings {
            let dir = atpkg::build_cache::build_dir(&root, spelled);
            let Some(mirror) = dir.parent() else {
                continue;
            };
            if std::fs::symlink_metadata(mirror).is_err() {
                continue;
            }
            if let Some(hit) = retired::written_since(mirror, since, until)? {
                return Err(format!(
                    "its build folder {} was written within {}",
                    hit.display(),
                    span(quiet)
                ));
            }
        }
    }
    Ok(())
}

/// `d` as a person reads a wait: `1h`, `7d`, `30d`.
fn span(d: Duration) -> String {
    let hours = d.as_secs() / 3600;
    if hours < 48 {
        format!("{hours}h")
    } else {
        format!("{}d", hours / 24)
    }
}

/// Whether the shared build cache holds a mirror of `wt` (`<cache root><wt>`), a real folder
/// and no link, under its path as given or as resolved: where it holds none, no build directory
/// of `wt` can go (the remover enters a mirror only through real directories).
fn mirror_stands(home: &Path, wt: &Path) -> bool {
    let root = atpkg::build_cache::cache_root(home);
    let spellings = [Some(wt.to_path_buf()), std::fs::canonicalize(wt).ok()];
    spellings.into_iter().flatten().any(|spelled| {
        atpkg::build_cache::build_dir(&root, &spelled)
            .parent()
            .is_some_and(|mirror| std::fs::symlink_metadata(mirror).is_ok_and(|m| m.is_dir()))
    })
}

/// How long the shared build directory of `wt`, a worktree of `checkout` that stays, must be
/// unwritten before it may go, where the step judges it for that at all: `[disk]` lets a build
/// directory go ([`Facts::build_dirs`]), `wt` is outside an agent's shape and in no removal,
/// its build directory was not judged and kept within the last day ([`Facts::build_rested`]),
/// and the cache holds a mirror of it ([`mirror_stands`]).
fn build_dir_window(facts: &Facts<'_>, checkout: &Path, wt: &Path) -> Option<Duration> {
    let window = facts.build_dirs?;
    (!retired::agent_shaped(checkout, wt)
        && !retired::in_retirement(checkout, wt)
        && !facts.build_rested.iter().any(|rested| rested == wt)
        && mirror_stands(facts.home, wt))
    .then_some(window)
}

/// Whether a step that kept `wt`, a worktree of `checkout`, kept its build directory by a
/// judgment it could make again at once: `wt` stands and the step judges it for its build
/// directory ([`build_dir_window`]). Such a worktree rests ([`Done::build_kept`]).
fn build_dir_judged(facts: &Facts<'_>, checkout: &Path, wt: &Path) -> bool {
    std::fs::symlink_metadata(wt).is_ok() && build_dir_window(facts, checkout, wt).is_some()
}

/// The build-start line that names `wt` (a build of a workspace at or under it, under its path
/// as given or as resolved, as [`quiet_for`] matches one) and is dated within `window` of `now`,
/// a line dated after `now` included. A build that compiled nothing writes no file in its build
/// directory, so the files' own times do not say it was used; where the clock reads too early
/// to date the window, every line that names it counts.
fn built_within<'a>(
    facts: &Facts<'a>,
    wt: &Path,
    now: SystemTime,
    window: Duration,
) -> Option<&'a Start> {
    let real = std::fs::canonicalize(wt).ok();
    let since_ms = now
        .checked_sub(window)
        .and_then(|since| since.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|since| since.as_millis());
    facts.starts.iter().find(|s| {
        (s.workspace.starts_with(wt) || real.as_ref().is_some_and(|r| s.workspace.starts_with(r)))
            && since_ms.is_none_or(|since| u128::from(s.at_ms) > since)
    })
}

/// The step's judgment of every candidate it reached.
#[derive(Debug, Default)]
pub struct Plan {
    /// Each candidate reached, `(checkout, worktree, verdict)`, in the order judged.
    pub judged: Vec<(PathBuf, PathBuf, Result<Verdict, String>)>,
    /// Why judging stopped before every candidate was reached.
    pub unfinished: Option<String>,
    /// The candidate reached last, when judging stopped short.
    pub reached: Option<PathBuf>,
    /// No candidate was judged: an open session's transcript cannot be named, and each stays
    /// for that alone.
    pub unjudged: bool,
}

/// Judge each `(checkout, worktree)` of `found` at `now`, oldest registration first, starting
/// after `after` and wrapping, until half the time left to `wall` is spent: cheapest first (its
/// kind, the session and nesting clauses, the judge, the build folder), then every
/// still-admitted candidate against every open session's transcripts, read by three quarters of
/// it, so a long backlog still leaves the reads and some removals their time. While any open
/// session's transcript cannot be named, every candidate stays.
#[must_use]
pub fn plan(
    git: &dyn Git,
    facts: &Facts<'_>,
    found: &[(PathBuf, PathBuf)],
    after: Option<&Path>,
    now: SystemTime,
    wall: Instant,
) -> Plan {
    if let Some((pid, why)) = facts.open.iter().find_map(|s| match &s.transcript {
        Err(why) => Some((s.pid, why)),
        Ok(_) => None,
    }) {
        let why = format!(
            "open session pid {pid} has no readable transcript ({why}), so whether it names a \
             worktree is unknown"
        );
        return Plan {
            judged: found
                .iter()
                .map(|(c, w)| (c.clone(), w.clone(), Err(why.clone())))
                .collect(),
            unfinished: None,
            reached: None,
            unjudged: true,
        };
    }
    let begun = Instant::now();
    let budget = wall.saturating_duration_since(begun);
    let judge_until = begun + budget / 2;
    let read_until = begun + budget * 3 / 4;
    let mut order: Vec<(SystemTime, PathBuf, PathBuf)> = found
        .iter()
        .map(|(c, w)| (registered_at(w), w.clone(), c.clone()))
        .collect();
    order.sort();
    if let Some(at) = after.and_then(|after| order.iter().position(|(_, w, _)| w == after)) {
        order.rotate_left(at + 1);
    }
    let mut out = Plan::default();
    for (_, wt, checkout) in order {
        if Instant::now() >= judge_until {
            out.unfinished =
                Some("the step's time ran out; the rest are judged at its next run".to_owned());
            break;
        }
        let verdict = judge(git, facts, &checkout, &wt, now);
        out.reached = Some(wt.clone());
        out.judged.push((checkout, wt, verdict));
    }
    if out.unfinished.is_none() {
        out.reached = None;
    }
    if let Some(why) = transcripts_clear(facts, &mut out.judged, read_until) {
        out.unfinished.get_or_insert(why);
    }
    out
}

/// Which rules may take a worktree, by the build-start lines that name it ([`quiet_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Quiet {
    /// A session the window holds live built in it: neither rule.
    Live(String),
    /// A line names it and the window's sessions are not known here: the merged rule alone.
    Merged(String),
    /// Either; the pinned rule once nothing in it was written for this long.
    Pinned(Duration),
}

/// Which rules may take `wt`, by every build-start line that names it (a build of a workspace at
/// or under it), and how long the pinned rule holds it idle: an hour where each of those lines
/// names a session the window holds as exited; seven days where no line names it or one names
/// a session the window does not hold (another window's, or one from before its start, which
/// may be live); the merged rule alone where the window's sessions are not known and a line
/// names it; neither while a session the window holds live built in it.
fn quiet_for(facts: &Facts<'_>, wt: &Path) -> Quiet {
    let real = std::fs::canonicalize(wt).ok();
    let naming: Vec<&Start> = facts
        .starts
        .iter()
        .filter(|s| {
            s.workspace.starts_with(wt) || real.as_ref().is_some_and(|r| s.workspace.starts_with(r))
        })
        .collect();
    let Some(roster) = facts.roster else {
        return match naming.first() {
            Some(start) => Quiet::Merged(format!(
                "session {} built in it and the sessions this host holds are not known here, so \
                 only the merged rule takes it",
                start.session
            )),
            None => Quiet::Pinned(retired::UNWRITTEN),
        };
    };
    let held = |start: &Start, live: bool| {
        roster
            .iter()
            .any(|seen| seen.live == live && seen.sid == start.session)
    };
    if let Some(start) = naming.iter().find(|&&start| held(start, true)) {
        return Quiet::Live(format!(
            "a live session's worktree: session {} built in it",
            start.session
        ));
    }
    if !naming.is_empty() && naming.iter().all(|&start| held(start, false)) {
        Quiet::Pinned(RECORDS_QUIET)
    } else {
        Quiet::Pinned(retired::UNWRITTEN)
    }
}

/// One candidate's verdict before the transcripts are read: the merged rule's, else, where the
/// step can record what pins owe and the tree stands, the pinned rule's. A worktree outside an
/// agent's shape, which neither rule takes, is held to the merged rule's other clauses where
/// `[disk]` lets build directories go ([`Facts::build_dirs`]), the cache holds a mirror of it
/// ([`mirror_stands`]) and no build started in it within the window ([`built_within`]): admitted,
/// it stays and gives back its shared build directories ([`Verdict::BuildDir`]). One whose
/// build directory was judged and kept within the last day is not judged for it
/// ([`build_dir_window`]).
fn judge(
    git: &dyn Git,
    facts: &Facts<'_>,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
) -> Result<Verdict, String> {
    if let Some(end) = wt
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| NOT_JUDGED.iter().find(|end| n.ends_with(**end)))
    {
        return Err(format!(
            "a `{end}` tree: the retired gate's snapshot or the release's cut tree, which the \
             step never judges"
        ));
    }
    let stands = std::fs::symlink_metadata(wt).is_ok();
    if stands {
        // A removal that stopped part way may have taken its `.git` and not written it back:
        // its receipt, not its `.git`, says what it is, and the judge holds it to that.
        if !retired::in_retirement(checkout, wt) {
            match kind(wt) {
                Kind::Main => return Err("a main checkout".to_owned()),
                Kind::Linked { checkout: of }
                    if std::fs::canonicalize(checkout).ok().as_ref() == Some(&of) => {}
                Kind::Linked { .. } | Kind::Other => {
                    return Err(format!("not a linked worktree of {}", checkout.display()));
                }
            }
        }
        unused_here(facts, wt)?;
    } else if let Some(aside) = retired::tree_standing(wt)? {
        // A removal that stopped moved its tree aside: what keeps the tree keeps it there.
        unused_here(facts, &aside)?;
    }
    let quiet = if stands {
        quiet_for(facts, wt)
    } else {
        Quiet::Pinned(retired::UNWRITTEN)
    };
    if let Quiet::Live(why) = quiet {
        return Err(why);
    }
    let verdict = match retired::judge_worktree(git, facts.home, checkout, wt, now) {
        Ok(admitted) => Verdict::Merged(admitted),
        Err(why)
            if stands
                && !retired::in_retirement(checkout, wt)
                && !retired::agent_shaped(checkout, wt) =>
        {
            let Some(window) = build_dir_window(facts, checkout, wt) else {
                return Err(why);
            };
            if let Some(start) = built_within(facts, wt, now, window) {
                return Err(format!(
                    "session {} started a build in it within {}, so its build directory stays; \
                     {why}",
                    start.session,
                    span(window)
                ));
            }
            let admitted = retired::judge_worktree_anywhere(git, facts.home, checkout, wt, now)
                .map_err(|merged| {
                    format!(
                        "outside an agent's shape, so the worktree stays whatever it holds, and \
                         its shared build directory stays too: {merged}"
                    )
                })?;
            Verdict::BuildDir(admitted)
        }
        Err(why) if !stands || facts.owed.is_none() || retired::in_retirement(checkout, wt) => {
            return Err(why);
        }
        Err(why) => {
            let quiet = match quiet {
                Quiet::Pinned(quiet) => quiet,
                Quiet::Live(only) | Quiet::Merged(only) => return Err(format!("{why}; {only}")),
            };
            match retired::judge_pinned_worktree(git, facts.home, checkout, wt, now, quiet) {
                Ok(admitted) => Verdict::Pinned { admitted, quiet },
                Err(pinned) if pinned == why => return Err(why),
                Err(pinned) => return Err(format!("{why}; by the pinned rule, {pinned}")),
            }
        }
    };
    if stands {
        let window = match &verdict {
            Verdict::Merged(_) => retired::UNWRITTEN,
            Verdict::BuildDir(_) => facts.build_dirs.unwrap_or(retired::UNWRITTEN),
            Verdict::Pinned { quiet, .. } => *quiet,
        };
        build_folder_quiet(facts, wt, now, git.until(), window)?;
    }
    Ok(verdict)
}

/// What a transcript names `wt` by: its folder's name, which every spelling of its path holds
/// (absolute, resolved, `~/…`, `$HOME/…`, `../…`, under `/private` or not), as found and
/// resolved, each also as JSON escapes it. A name some unrelated text also holds keeps the
/// worktree, which is the safe side.
fn spellings(wt: &Path) -> Vec<String> {
    let mut out: Vec<String> = [Some(wt.to_path_buf()), std::fs::canonicalize(wt).ok()]
        .into_iter()
        .flatten()
        .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
        .collect();
    let escaped: Vec<String> = out
        .iter()
        .map(|s| s.replace('\\', "\\\\").replace('"', "\\\""))
        .collect();
    out.extend(escaped);
    out.retain(|s| !s.is_empty());
    out.sort();
    out.dedup();
    out
}

/// The transcripts of the session that writes `transcript` (`<session>.jsonl`): it, and every
/// `*.jsonl` under its subagents' folder to [`SUBAGENT_DEPTH`] levels down (a subagent's own,
/// `<session>/subagents/*.jsonl`, and a workflow's agents',
/// `<session>/subagents/workflows/<run>/*.jsonl`). Their working directory is the session's
/// own, so only what they wrote names a worktree they use. A folder that cannot be listed, a
/// folder deeper than [`SUBAGENT_DEPTH`], and more than [`MAX_SUBAGENTS`] transcripts are an
/// `Err`: what they hold cannot be read through.
fn session_transcripts(transcript: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = vec![transcript.to_path_buf()];
    let subagents = transcript.with_extension("").join("subagents");
    let mut folders = vec![(subagents, 1_usize)];
    while let Some((folder, depth)) = folders.pop() {
        let unlisted = |e: std::io::Error| format!("{} could not be listed: {e}", folder.display());
        let listing = match std::fs::read_dir(&folder) {
            Ok(listing) => listing,
            Err(e) if depth == 1 && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(unlisted(e)),
        };
        for entry in listing {
            let entry = entry.map_err(unlisted)?;
            let path = entry.path();
            let kind = entry.file_type().map_err(unlisted)?;
            if kind.is_dir() {
                if depth >= SUBAGENT_DEPTH {
                    return Err(format!(
                        "{} is deeper than {SUBAGENT_DEPTH} levels of subagent folders",
                        path.display()
                    ));
                }
                folders.push((path, depth + 1));
            } else if path.extension().is_some_and(|x| x == "jsonl") {
                if files.len() > MAX_SUBAGENTS {
                    return Err(format!(
                        "{} holds more than {MAX_SUBAGENTS} transcripts",
                        folder.display()
                    ));
                }
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// Hold every still-admitted candidate of `judged` against every open session's transcripts
/// (its own and its subagents'): one a transcript names, or every one when a transcript cannot
/// be read through, stays. `Some` when `wall` passed during the reads.
fn transcripts_clear(
    facts: &Facts<'_>,
    judged: &mut [(PathBuf, PathBuf, Result<Verdict, String>)],
    wall: Instant,
) -> Option<String> {
    let admitted: Vec<usize> = judged
        .iter()
        .enumerate()
        .filter(|(_, (_, _, verdict))| verdict.is_ok())
        .map(|(i, _)| i)
        .collect();
    if admitted.is_empty() {
        return None;
    }
    let needles: Vec<(usize, String)> = admitted
        .iter()
        .flat_map(|&i| spellings(&judged[i].1).into_iter().map(move |s| (i, s)))
        .collect();
    let mut named: Vec<Option<String>> = vec![None; judged.len()];
    for session in facts.open {
        let Ok(transcript) = &session.transcript else {
            continue;
        };
        let hits = match scan_session(transcript, &needles, wall) {
            Ok(hits) => hits,
            Err(why) => {
                let late = Instant::now() >= wall;
                for &i in &admitted {
                    judged[i].2 = Err(format!(
                        "the open session pid {}'s transcripts at {} could not be read through \
                         ({why}), so whether they name it is unknown",
                        session.pid,
                        transcript.display()
                    ));
                }
                return late.then(|| {
                    "the step's time ran out reading transcripts; the rest are judged at its \
                     next run"
                        .to_owned()
                });
            }
        };
        for (i, path) in hits {
            named[i].get_or_insert_with(|| {
                format!(
                    "the open session pid {}'s transcript {} names it",
                    session.pid,
                    path.display()
                )
            });
        }
    }
    for (i, why) in named.into_iter().enumerate() {
        if let Some(why) = why {
            judged[i].2 = Err(why);
        }
    }
    None
}

/// Which of `needles` the session that writes `transcript` names ([`session_transcripts`]),
/// each with the transcript that names it.
fn scan_session(
    transcript: &Path,
    needles: &[(usize, String)],
    wall: Instant,
) -> Result<Vec<(usize, PathBuf)>, String> {
    let mut hits = Vec::new();
    for path in session_transcripts(transcript)? {
        for i in scan(&path, needles, wall)? {
            hits.push((i, path.clone()));
        }
    }
    Ok(hits)
}

/// Which of `needles` the file at `path` holds, read in [`SCAN_CHUNK`] pieces that overlap by
/// the longest needle less one byte, so no needle is missed across a piece's edge.
fn scan(path: &Path, needles: &[(usize, String)], wall: Instant) -> Result<Vec<usize>, String> {
    use std::io::Read as _;
    let longest = needles.iter().map(|(_, n)| n.len()).max().unwrap_or(0);
    if longest == 0 {
        return Ok(Vec::new());
    }
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return Err("it is not a regular file".to_owned());
    }
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut found = vec![false; needles.len()];
    let mut held: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; SCAN_CHUNK];
    loop {
        if Instant::now() >= wall {
            return Err("the step's time ran out".to_owned());
        }
        let n = match file.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        if n == 0 {
            break;
        }
        held.extend_from_slice(&chunk[..n]);
        {
            let text = String::from_utf8_lossy(&held);
            for (seen, (_, needle)) in found.iter_mut().zip(needles) {
                if !*seen && text.contains(needle.as_str()) {
                    *seen = true;
                }
            }
        }
        let keep = longest - 1;
        if held.len() > keep {
            held.drain(..held.len() - keep);
        }
    }
    let mut hits: Vec<usize> = needles
        .iter()
        .zip(&found)
        .filter(|(_, seen)| **seen)
        .map(|((i, _), _)| *i)
        .collect();
    hits.sort_unstable();
    hits.dedup();
    Ok(hits)
}

/// What a build cache's remover asks before it takes one build directory, with why it goes:
/// `false` keeps it.
pub type Record<'a> = &'a mut dyn FnMut(&Path, Unused) -> bool;

/// The remover of a kept worktree's shared build directories ([`Hooks::remove_build`]): the
/// worktree, the step's leave (how long each must be unwritten, whether it may go on, when its
/// reads end), and what it asks before each; what became of each it asked for.
pub type RemoveBuild<'a> =
    &'a mut dyn FnMut(&Path, &Leave<'_>, Record<'_>) -> Vec<(PathBuf, Unused)>;

/// What the step calls to act: whether it still holds the pass lock, the removers of one
/// worktree of a checkout, the ledger's append of one row (`false`: not on record), and what its
/// pins ask of the machine.
pub struct Hooks<'a> {
    /// Whether the pass lock is still this host's.
    pub still_held: &'a dyn Fn() -> bool,
    /// Remove the worktree (second) of the checkout (first), judged again just before.
    pub remove: &'a mut dyn FnMut(&Path, &Path) -> Result<(), String>,
    /// Remove the worktree (second) of the checkout (first) once its pins (third) hold what
    /// it holds, judged again just before by the pinned rule.
    pub remove_pinned: &'a mut dyn FnMut(&Path, &Path, &Pinned) -> Result<(), String>,
    /// Append one row to the ledger; `false` when it is not on record.
    pub journal: &'a mut dyn FnMut(&str) -> bool,
    /// Whether no process uses a worktree, asked before anything of it is pinned
    /// (`retired::worktree_unused`).
    pub unused: &'a dyn Fn(&Path) -> Result<(), String>,
    /// The free bytes of the volume holding a path, `None` where they cannot be read
    /// ([`super::free_bytes`]).
    pub free: &'a dyn Fn(&Path) -> Option<u64>,
    /// `aterm agh <args>`, for an origin on GitHub ([`pins::agh_until`]).
    pub agh: pins::Agh<'a>,
    /// What the step last journalled of each worktree it kept once admitted, and of the owed
    /// file ([`Schedule::said`]): the same reason goes on record once.
    pub said: &'a mut BTreeMap<PathBuf, String>,
    /// Take the shared build directories of a worktree the step keeps, each unwritten for the
    /// window, within the step's leave, its record asked before each (`false` keeps it), each
    /// returned with what became of it (`atpkg::build_cache::reclaim_merged_worktree`).
    pub remove_build: RemoveBuild<'a>,
    /// The build-start record's lines read again now ([`atpkg::build_start::read`]): what the
    /// second judgment of a worktree that gives back a build directory holds it to, since
    /// [`Facts::starts`] is as old as the look that began the step.
    pub starts: &'a dyn Fn() -> Vec<Start>,
    /// Whether the home volume, read now, lets the custody writes run (a snapshot, a push):
    /// `Err` with why they wait, where it reads under its emergency line or could not be read
    /// (`aterm_capacity::floor::custody_writes_wait`).
    pub custody: &'a dyn Fn() -> Result<(), String>,
}

/// What [`Schedule::said`] holds of a worktree whose pins wait for room: its `deferred` row is
/// on record, and is not written again while they wait.
const DEFERRED_SAID: &str = "deferred until the volume reads above its emergency line";

/// The [`Schedule::said`] key of the owed pushes' wait for room: beside the owed file (whose own
/// key holds why it could not be read or written).
fn pushes_deferred_key(owed: &Path) -> PathBuf {
    owed.with_extension("deferred")
}

/// One step: [`plan`] the candidates, then remove each admitted one until `wall`, journalling
/// its intent before (a removal whose intent is not on record does not happen) and its outcome
/// after, and deleting its merged branch once it went ([`delete_merged_branch`]). A candidate
/// the pinned rule admitted is pinned first ([`pins::pin`], once no process uses it and while
/// the step's snapshots have read less than [`pins::STEP_BYTES`]) and what its pins owe an
/// origin recorded ([`pins::owe`]) before its intent; its branch stays, and a pin that fails
/// keeps it, journalled once for each reason. What its pins owe is dropped again where its
/// removal never began, and marked its worktree's gone once it went. Every owed push that is
/// due is then tried ([`pins::push_owed`]) and each that went, changed why it is owed, was
/// parked or dropped journalled. A candidate outside an agent's shape the judgment admitted
/// stays, and gives back its shared build directories ([`give_back`]). While the home volume
/// reads under its emergency line ([`Hooks::custody`], asked before each pin and before the
/// pushes), a candidate the pinned rule admitted stays unpinned with one `deferred` row, and the
/// owed pushes wait with one `deferred` push row ([`Done::waits`]), each once for each wait: a
/// step that reads the volume above the line ends every wait on record. Returns what it did and
/// the candidate it reached last when it stopped short.
#[must_use]
pub fn step(
    git: &dyn Git,
    facts: &Facts<'_>,
    found: &[(PathBuf, PathBuf)],
    after: Option<&Path>,
    now: SystemTime,
    wall: Instant,
    mut hooks: Hooks<'_>,
) -> (Done, Option<PathBuf>) {
    let at = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0);
    // A volume read above the line ends every wait on record, whether or not this step reaches
    // its worktree (one resumed since is no candidate): a later wait is a new one, and its
    // `deferred` row goes on record again.
    if (hooks.custody)().is_ok() {
        hooks.said.retain(|_, said| said != DEFERRED_SAID);
    }
    let planned = plan(git, facts, found, after, now, wall);
    let mut done = Done {
        unfinished: planned.unfinished,
        ..Done::default()
    };
    // The candidate the removals reached last, and whether they stopped before the end.
    let mut handled: Option<PathBuf> = None;
    let mut stopped = false;
    let mut budget = pins::STEP_BYTES;
    for (checkout, wt, verdict) in planned.judged {
        let verdict = match verdict {
            Ok(verdict) => verdict,
            Err(why) => {
                handled = Some(wt.clone());
                if !planned.unjudged && build_dir_judged(facts, &checkout, &wt) {
                    done.build_kept.push(wt.clone());
                }
                done.kept.push((wt, why));
                continue;
            }
        };
        if Instant::now() >= wall {
            done.unfinished.get_or_insert_with(|| {
                "the step's time ran out; the rest go at its next run".to_owned()
            });
            stopped = true;
            break;
        }
        if !(hooks.still_held)() {
            done.unfinished = Some("the pass lock was lost".to_owned());
            stopped = true;
            break;
        }
        handled = Some(wt.clone());
        if matches!(verdict, Verdict::BuildDir(_)) {
            let times = (at, now, wall);
            give_back(git, facts, &mut hooks, times, &checkout, &wt, &mut done);
            continue;
        }
        let bytes = super::dir_bytes(&wt).0;
        let admitted = verdict.admitted();
        let pinned = match &verdict {
            Verdict::Merged(_) => None,
            // Its worktree stays (`give_back`, above): it never reaches a remover.
            Verdict::BuildDir(_) => continue,
            Verdict::Pinned { quiet, .. } => {
                if let Err(why) = (hooks.custody)() {
                    if hooks.said.get(&wt).map(String::as_str) != Some(DEFERRED_SAID) {
                        (hooks.journal)(&super::worktree_deferred_row(
                            at,
                            &row(&wt, &checkout, admitted, &why, None, 0),
                        ));
                        hooks.said.insert(wt.clone(), DEFERRED_SAID.to_owned());
                    }
                    done.deferred.push((wt, why));
                    continue;
                }
                let asked = Ask {
                    git,
                    facts,
                    unused: hooks.unused,
                    free: hooks.free,
                    now,
                };
                match pin_one(&asked, &checkout, &wt, admitted, *quiet, budget) {
                    Ok((pinned, dropped)) => {
                        budget = budget.saturating_sub(pinned.bytes);
                        for owed in &dropped {
                            (hooks.journal)(&super::worktree_push_row(
                                at,
                                "dropped",
                                Some(owed),
                                "the owed file had no room for it; its refs stand in its checkout",
                            ));
                        }
                        Some(pinned)
                    }
                    Err(why) => {
                        if hooks.said.get(&wt) != Some(&why) {
                            (hooks.journal)(&super::worktree_row(
                                at,
                                SweepAction::Kept,
                                &row(&wt, &checkout, admitted, &why, None, 0),
                            ));
                            hooks.said.insert(wt.clone(), why.clone());
                        }
                        done.kept.push((wt, why));
                        continue;
                    }
                }
            }
        };
        let why = match &pinned {
            None => admitted.why.clone(),
            Some(pinned) => format!("{}; pinned on {}", admitted.why, pinned.words()),
        };
        let intent = super::worktree_row(
            at,
            SweepAction::Removing,
            &row(&wt, &checkout, admitted, &why, None, bytes),
        );
        if !(hooks.journal)(&intent) {
            let refusal = Refusal::Unjournalled(wt.clone());
            (hooks.journal)(&super::denial_row(at, "", &refusal));
            done.refused.push(refusal);
            disown(facts, &wt, pinned.as_ref(), &mut done);
            continue;
        }
        let removed = match &pinned {
            None => (hooks.remove)(&checkout, &wt),
            Some(pinned) => (hooks.remove_pinned)(&checkout, &wt, &pinned.pinned),
        };
        match removed {
            Ok(()) => {
                hooks.said.remove(&wt);
                if let (Some(owed), Some(_)) = (facts.owed, &pinned)
                    && let Err(why) = pins::went(owed, &wt)
                {
                    done.unrecorded = Some(why);
                }
                let branch_deleted = match (&admitted.branch, &pinned) {
                    (Some(_), Some(_)) => Err("it went by the pinned rule, which leaves its \
                                               branch where it is"
                        .to_owned()),
                    (Some(branch), None) => {
                        delete_merged_branch(git, &checkout, branch, &admitted.head)
                    }
                    (None, _) => Err("it was on no branch the removal knew of (detached, or \
                                      its removal resumed from a receipt, which records none)"
                        .to_owned()),
                };
                (hooks.journal)(&super::worktree_row(
                    at,
                    SweepAction::Removed,
                    &row(&wt, &checkout, admitted, &why, Some(&branch_deleted), bytes),
                ));
                done.removed.push(Removed {
                    path: wt.clone(),
                    checkout: checkout.clone(),
                    branch: admitted.branch.clone(),
                    branch_deleted,
                    bytes,
                    pins: pinned.as_ref().map(pins::Pins::words),
                });
            }
            Err(why) => {
                // A removal no receipt names any more took nothing: what its pins owe is the
                // worktree's still, standing whole. One a receipt names stopped part way, and
                // its pushes stay owed.
                if !retired::in_retirement(&checkout, &wt) {
                    disown(facts, &wt, pinned.as_ref(), &mut done);
                }
                (hooks.journal)(&super::worktree_row(
                    at,
                    SweepAction::Kept,
                    &row(&wt, &checkout, admitted, &why, None, 0),
                ));
                done.kept.push((wt.clone(), why));
            }
        }
    }
    if let Some(owed) = facts.owed {
        let waits = (hooks.custody)().err();
        let key = pushes_deferred_key(owed);
        if waits.is_none() {
            hooks.said.remove(&key);
        }
        let round = if Instant::now() < wall && waits.is_none() {
            pins::push_owed(
                git,
                hooks.agh,
                owed,
                pins::epoch_secs(now),
                wall + GIT_GRACE,
            )
        } else {
            match pins::load(owed) {
                Ok(owed) => pins::Round {
                    owed,
                    ..pins::Round::default()
                },
                Err(why) => pins::Round {
                    unrecorded: Some(why),
                    ..pins::Round::default()
                },
            }
        };
        let rows = [
            ("pushed", &round.pushed),
            ("owed", &round.changed),
            ("parked", &round.parked),
            ("dropped", &round.dropped),
        ];
        for (action, list) in rows {
            for entry in list {
                let text = if action == "dropped" {
                    "the owed file had no room for it; its refs stand in its checkout"
                } else {
                    entry.why.as_str()
                };
                (hooks.journal)(&super::worktree_push_row(at, action, Some(entry), text));
            }
        }
        match &round.unrecorded {
            Some(why) if hooks.said.get(owed) != Some(why) => {
                (hooks.journal)(&super::worktree_push_row(at, "unrecorded", None, why));
                hooks.said.insert(owed.to_path_buf(), why.clone());
            }
            Some(_) => {}
            None => {
                hooks.said.remove(owed);
            }
        }
        if let Some(why) = waits
            && !round.owed.is_empty()
        {
            if !hooks.said.contains_key(&key) {
                let text = format!("{} owed push(es) wait: {why}", round.owed.len());
                (hooks.journal)(&super::worktree_push_row(
                    at,
                    super::WORKTREE_DEFERRED,
                    None,
                    &text,
                ));
                hooks.said.insert(key, why.clone());
            }
            done.pushes_deferred = Some(why);
        }
        done.pushed = round.pushed.len();
        done.owed = round.owed;
        done.parked = round.parked;
        if round.unrecorded.is_some() {
            done.unrecorded = round.unrecorded;
        }
    }
    // Removals that stopped short name where the next run starts: after the last candidate they
    // handled (where this run started when they handled none), so an admitted one left for lack
    // of time comes first next time; else judging that stopped short names the last it reached.
    let reached = if stopped {
        handled.or_else(|| after.map(Path::to_path_buf))
    } else if done.unfinished.is_some() {
        planned.reached
    } else {
        None
    };
    (done, reached)
}

/// Give back the shared build directories of `wt`, a worktree of `checkout` outside an agent's
/// shape that the judgment admitted ([`Verdict::BuildDir`]); the worktree itself is not touched
/// and is listed as kept, with what became of its build directories ([`take_build_dirs`]).
/// Where a build directory of it still stands afterwards, it rests a day before it is judged
/// for it again ([`Done::build_kept`]).
fn give_back(
    git: &dyn Git,
    facts: &Facts<'_>,
    hooks: &mut Hooks<'_>,
    times: (i64, SystemTime, Instant),
    checkout: &Path,
    wt: &Path,
    done: &mut Done,
) {
    let why = take_build_dirs(git, facts, hooks, times, checkout, wt, done);
    if mirror_stands(facts.home, wt) {
        done.build_kept.push(wt.to_path_buf());
    }
    done.kept.push((wt.to_path_buf(), why));
}

/// [`give_back`]'s removal, and why `wt` is listed as kept. Whether no process uses the
/// worktree is asked first ([`Hooks::unused`]), and it is judged again ([`judge`]) against the
/// build-start record as it reads now ([`Hooks::starts`]): the step's first judgment can be as
/// old as its wall by now, the lines it was handed older, and an edit or a build made since
/// keeps the directory for the person's next build. Then the cache's remover takes each build
/// directory of it ([`Hooks::remove_build`]) while the pass lock is held and the wall's grace
/// has not passed, its read of a directory's files ending there too. Before each, whether a
/// process uses the worktree is asked again (the judgment and the remover's own reads took
/// time) and a `build-cache-sweep` intent row goes on record, sized by a walk that ends with
/// the grace; one whose intent is not on record does not go, and is refused as unjournalled.
/// Each intent's outcome row follows.
fn take_build_dirs(
    git: &dyn Git,
    facts: &Facts<'_>,
    hooks: &mut Hooks<'_>,
    (at, now, wall): (i64, SystemTime, Instant),
    checkout: &Path,
    wt: &Path,
    done: &mut Done,
) -> String {
    if let Err(why) = (hooks.unused)(wt) {
        return why;
    }
    let starts = (hooks.starts)();
    let facts = Facts {
        starts: &starts,
        ..*facts
    };
    let admitted = match judge(git, &facts, checkout, wt, now) {
        Ok(Verdict::BuildDir(admitted)) => admitted,
        Ok(Verdict::Merged(_) | Verdict::Pinned { .. }) => {
            return "it no longer reads as a worktree outside an agent's shape; it is judged \
                    again at the step's next run"
                .to_owned();
        }
        Err(why) => return why,
    };
    let Some(window) = facts.build_dirs else {
        return format!("{}; `[disk]` lets no build directory go", admitted.why);
    };
    let (still, unused) = (hooks.still_held, hooks.unused);
    let until = wall + GIT_GRACE;
    let within = || still() && Instant::now() < until;
    let leave = Leave {
        quiet: window,
        within: &within,
        until: Some(until),
    };
    let journal = &mut *hooks.journal;
    let mut sized: Vec<(PathBuf, Unused, u64)> = Vec::new();
    let mut unrecorded = false;
    let mut used: Option<String> = None;
    let went = (hooks.remove_build)(wt, &leave, &mut |path, why| {
        if !within() {
            return false;
        }
        if let Err(process) = unused(wt) {
            used = Some(process);
            return false;
        }
        let size = std::fs::Metadata::len;
        let bytes = super::walk_bytes_until(path, |_| true, size, Some(until)).0;
        let intent = super::sweep_row(at, path, SweepAction::Removing, bytes, why);
        let on = journal(&intent);
        if on {
            sized.push((path.to_path_buf(), why, bytes));
        } else {
            unrecorded = true;
        }
        on
    });
    let mut any = false;
    for (path, why) in went {
        let bytes = crate::harness::cli::disk_sweep_sized(&sized, &path, why);
        let outcome = super::sweep_row(at, &path, SweepAction::outcome(why), bytes, why);
        journal(&outcome);
        if !why.nothing_went() {
            any = true;
            done.given_back.push(GivenBack {
                worktree: wt.to_path_buf(),
                path,
                why,
                bytes,
            });
        }
    }
    if unrecorded {
        let refusal = Refusal::Unjournalled(wt.to_path_buf());
        journal(&super::denial_row(at, "", &refusal));
        done.refused.push(refusal);
    }
    let stays = "outside an agent's shape, so the worktree stays";
    let merged = &admitted.why;
    if any {
        format!("{merged}; {stays} and only its shared build directory goes")
    } else if let Some(process) = used {
        format!("{merged}; {stays}, and so does its shared build directory: {process}")
    } else if unrecorded {
        format!(
            "{merged}; {stays}, and so does its shared build directory: the ledger took no row \
             of it"
        )
    } else {
        format!(
            "{merged}; {stays}, and the cache's remover kept its shared build directory (a build \
             holds its locks, a process uses it, its custody keeps it, something wrote it since, \
             or the step's time or its pass lock ran out)"
        )
    }
}

/// Drop what `pinned`, the pins of `wt`, owe from the owed file: its removal never began.
fn disown(facts: &Facts<'_>, wt: &Path, pinned: Option<&pins::Pins>, done: &mut Done) {
    if let (Some(owed), Some(pinned)) = (facts.owed, pinned)
        && let Err(why) = pins::disown(owed, wt, &pinned.pushes)
    {
        done.unrecorded = Some(why);
    }
}

/// What pinning a worktree asks beside it: the step's git and facts, whether no process uses
/// the worktree, its volume's free bytes, and the step's now.
struct Ask<'a> {
    git: &'a dyn Git,
    facts: &'a Facts<'a>,
    unused: &'a dyn Fn(&Path) -> Result<(), String>,
    free: &'a dyn Fn(&Path) -> Option<u64>,
    now: SystemTime,
}

/// Pin `wt`, a worktree of `checkout` the pinned rule admitted (`admitted`, held to `quiet`),
/// once no process uses it, its snapshot reading at most `budget`, and record what its pins owe
/// before anything of it goes: the pins, and the owed pushes the owed file had no room for.
/// `Err` keeps it.
fn pin_one(
    ask: &Ask<'_>,
    checkout: &Path,
    wt: &Path,
    admitted: &Admitted,
    quiet: Duration,
    budget: u64,
) -> Result<(pins::Pins, Vec<pins::Owed>), String> {
    let owed = ask
        .facts
        .owed
        .ok_or("the step has nowhere to record what its pins owe")?;
    (ask.unused)(wt)?;
    let free = (ask.free)(checkout);
    let pinned = pins::pin(ask.git, checkout, wt, admitted, quiet, free, budget)?;
    let dropped =
        pins::owe(owed, wt, &pinned.pushes, pins::epoch_secs(ask.now)).map_err(|why| {
            format!("what its pins owe its origin could not be recorded ({why}), so it stays")
        })?;
    Ok((pinned, dropped))
}

/// One ledger row's facts of the worktree `wt` of `checkout` the judge admitted.
fn row<'a>(
    wt: &'a Path,
    checkout: &'a Path,
    admitted: &'a Admitted,
    text: &'a str,
    branch_deleted: Option<&'a Result<(), String>>,
    bytes: u64,
) -> WorktreeRow<'a> {
    WorktreeRow {
        path: wt,
        checkout,
        branch: admitted.branch.as_deref(),
        head: &admitted.head,
        bytes,
        text,
        branch_deleted,
    }
}

/// Delete the local branch `branch` (the full `refs/heads/…`) of `checkout`, at `head`, once its
/// worktree went. It stays when it is `main` or origin's default branch, when any worktree has
/// it checked out, or is rebasing or bisecting it, when it no longer names `head`, or when
/// `origin/main` does not contain `head`; the deletion itself is compare-and-delete on `head`
/// (`git update-ref --no-deref -d`), so a branch moved meanwhile stays.
///
/// # Errors
/// Why the branch stays.
pub fn delete_merged_branch(
    git: &dyn Git,
    checkout: &Path,
    branch: &str,
    head: &str,
) -> Result<(), String> {
    let Some(name) = branch.strip_prefix("refs/heads/") else {
        return Err(format!("{branch} is not a local branch"));
    };
    if name == "main" {
        return Err("it is main".to_owned());
    }
    let default = git.run(
        checkout,
        &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"],
    );
    match default.code {
        Some(0) => {
            if default.out.trim().strip_prefix("refs/remotes/origin/") == Some(name) {
                return Err("it is origin's default branch".to_owned());
            }
        }
        Some(_) => {}
        None => {
            return Err(format!(
                "git could not say which branch is origin's default: {}",
                default.err
            ));
        }
    }
    let listing = git.run(checkout, &["worktree", "list", "--porcelain"]);
    if listing.code != Some(0) {
        return Err(format!("git worktree list failed: {}", listing.err));
    }
    if listing
        .out
        .lines()
        .any(|line| line.strip_prefix("branch ") == Some(branch))
    {
        return Err("a worktree has it checked out".to_owned());
    }
    let common = checkout.join(".git");
    let mut admins = vec![common.clone()];
    match std::fs::read_dir(common.join("worktrees")) {
        Ok(entries) => {
            for entry in entries.take(MAX_ENTRIES) {
                let entry = entry
                    .map_err(|e| format!("git's worktree entries could not be listed: {e}"))?;
                admins.push(entry.path());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("git's worktree entries could not be listed: {e}")),
    }
    for admin in &admins {
        for held in [
            admin.join("rebase-merge").join("head-name"),
            admin.join("rebase-apply").join("head-name"),
            admin.join("BISECT_START"),
        ] {
            match std::fs::symlink_metadata(&held) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{} could not be read: {e}", held.display())),
                Ok(_) => {}
            }
            let Some(text) = super::discover::small_text(&held) else {
                return Err(format!("{} could not be read", held.display()));
            };
            let named = text.trim();
            if named == branch || named == name {
                return Err(format!(
                    "git is rebasing or bisecting it ({})",
                    held.display()
                ));
            }
        }
    }
    let now_at = git.run(checkout, &["rev-parse", "--verify", "--quiet", branch]);
    if now_at.code != Some(0) || now_at.out.trim() != head {
        return Err(format!(
            "it no longer names {}",
            head.get(..10).unwrap_or(head)
        ));
    }
    let merged = git.run(
        checkout,
        &[
            "merge-base",
            "--is-ancestor",
            head,
            "refs/remotes/origin/main",
        ],
    );
    if merged.code != Some(0) {
        return Err(format!(
            "origin/main does not contain {}",
            head.get(..10).unwrap_or(head)
        ));
    }
    let deleted = git.run(checkout, &["update-ref", "--no-deref", "-d", branch, head]);
    if deleted.code != Some(0) {
        return Err(format!("git did not delete it: {}", deleted.err));
    }
    Ok(())
}

#[cfg(all(test, unix, debug_assertions))]
#[path = "disk_worktree_tests.rs"]
mod tests;
