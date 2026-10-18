// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FINDING THE BUILD DIRECTORIES — every cargo build directory the disk watch
//! can reach from where the agents work, and who OWNS each.
//!
//! # Why this exists
//!
//! Measured on a 460 GB volume that kept falling to 3-6 GB free: the hogs
//! were build directories a watch of only `target*` directly under each
//! supervised session's working directory never sees; the space was
//! elsewhere — a checkout's `crates/target ->
//! target.noindex` (49 GB), x.py's bootstrap trees under `build/<host>/`
//! (77 GB), an agent worktree's build directory beside it rather than in it
//! (19 GB), and the scratch builds of Claude Code sessions that had ended (45 GB
//! under `/private/tmp/claude-<uid>`). `~/.cache/aterm/cells` is a layout
//! nothing writes, and the watch never looks there.
//!
//! # Where it looks ([`Roots`])
//!
//! Every path is injected; nothing here reads the environment.
//!
//! * each supervised agent's `CARGO_TARGET_DIR`, where its environment could
//!   be read ([`Origin::AgentTargetDir`]);
//! * each supervised session's working directory and the git worktree root
//!   above it, and each direct child of `$HOME` and every git worktree of
//!   one (`.git/worktrees/*/gitdir`, read — no `git` is run): a bounded walk
//!   two levels down ([`CHECKOUT_DEPTH`]) for directories carrying a build
//!   tool's marker ([`Origin::Checkout`]) — which finds `crates/target`
//!   behind a checkout and `wt/m1-target` beside a worktree;
//! * an x.py checkout's `build/<host>/` (a checkout holding `x.py` and
//!   `build/`): its cargo build directories only — `stage<N>-<kind>` for N
//!   of 1 or more and `bootstrap-tools`, each with a signed `CACHEDIR.TAG` —
//!   and NEVER `stage<N>` (a sysroot), anything of stage0, `llvm`, or a
//!   `-sysroot`/`-bin` tree ([`bootstrap_cargo_dir`], [`Origin::Bootstrap`]);
//! * Claude Code's per-user temp root, each session's `scratchpad/` walked
//!   [`SCRATCH_DEPTH`] levels down ([`Origin::ClaudeScratch`]) — only when
//!   the root is this user's private directory (no link, this uid's, writable
//!   by nobody else: on a shared `/tmp` another user could plant links).
//!
//! The roots with no owner that can end (the first three) are walked before
//! the temp root ([`discover`]): a build directory one of them
//! names is theirs, and a walk cut short before they are done reaches no
//! owner's at all.
//!
//! The walk never follows a symlink — except a `target*` link, which is
//! RESOLVED to the directory it names (`target -> target.noindex`), never
//! into another mount — never enters a hidden directory, `~/Library` or (on
//! macOS) a privacy-guarded folder of `$HOME` ([`HOME_SKIP`]), never enters
//! or even `stat`s a mount point ([`Roots::mounts`]: a stalled network
//! mount answers by blocking), never reports or walks a directory off the
//! measured device ([`Roots::device`]), never enters an x.py checkout's
//! `build/` but by the bootstrap rule, and stops at a build directory it
//! found (its profiles are the profile walk's, `super::target`). Two names
//! for one directory (a link and its target, a cwd that is also `$HOME`'s
//! child) are one, by device and inode, its origins MERGED
//! ([`Origin::merge`]). It is bounded in entries, wall clock and directories
//! found ([`Budget`]); a walk cut short keeps what it found and says so.
//!
//! # Who owns it ([`Owned`])
//!
//! Most build directories belong to a checkout and nothing here can say the
//! checkout is done with them. One kind has an owner that can be GONE: a
//! Claude Code session's scratch build. The temp root is every Claude
//!   Code's of this uid, `<root>/<project>/<session>`, where `<project>` is
//!   the directory the Claude Code was launched in with every character but
//!   an ASCII letter or digit written `-` (past 200 characters, the first
//!   200, a `-` and a hash) — measured with Claude Code 2.1.287, whose
//!   working directory as the kernel holds it is that directory. WHO MAY
//!   HOLD A SESSION is read from the KERNEL, which no settings file can move
//!   ([`claude_census`]): every process of this uid is read, and each Claude
//!   Code among them — by what it was exec'd as — with its working
//!   directory and start. Each keeps its registry of running processes
//!   (`sessions/<pid>.json`, the session it holds now, rewritten when that
//!   changes) in ITS Claude Code directory, `CLAUDE_CONFIG_DIR`, else
//!   `$HOME/.claude` — but a settings `env` block moves that directory AFTER
//!   its exec (measured, 2.1.287), so no guess of it is trusted: a Claude
//!   Code that a registry read here PINS to its session (its pid, at its
//!   kernel start) holds that session, and one no registry pins may hold any
//!   session of the project it works in, and keeps them all
//!   ([`RunningClaude::may_hold`]). The registries and transcripts read are
//!   `~/.claude`'s, the identities' and the directory each running process
//!   was started with ([`Roots::claude_dirs`]) — for REACH: each pin lets
//!   the rest of a project's sessions be judged, and a Claude Code the
//!   kernel does not name one (a renamed copy) is still named running by its
//!   registry there. A process that may be a Claude Code but cannot be read
//!   — read again first, as one caught mid-spawn is read once its exec
//!   completes ([`settled`]) — makes no session gone
//!   ([`Roots::claude_unread`]). A session is gone
//!   when no running Claude Code may hold it, a known directory holds its
//!   transcript, no known registry names a running process for it, and
//!   neither the transcript nor any file under the session's temp directory
//!   was written for the quiet window. A session NO known directory holds a
//!   transcript of is never called gone, and neither is one whose registry
//!   cannot be read whole; the removal reads the running processes again
//!   ([`still_gone`]).
//!
//! An owner owns only what lies INSIDE its folder: a `target*` link in a
//! scratchpad that points out of the session's directory (a scratch
//! worktree's `target -> ~/repo/target.noindex`) makes what it names
//! [`Origin::Linked`], nobody's that can end — and a directory a checkout,
//! an agent or the caller ALSO names is theirs.
//!
//! What a gone owner's build directory may lose is the report's decision
//! (`super::report`). The removal first asks that NO RUNNING PROCESS has its
//! working directory, executable or an open file in it or the owner's
//! folder ([`unused`] — cargo runs tests without its locks, so only this
//! says nothing runs from it), then checks again under cargo's locks, right
//! before it deletes ([`recheck`]): the directory still inside the owner's
//! folder, the owner still gone.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::target::{CACHE_DIRS, CARGO_LOCKS, CLOCK_DIRS, human_age};
use super::{MAX_WALK_ENTRIES, RUSTC_INFO, cachedir_tag_signed, mtime_of};

/// Levels below a checkout root the marker walk enters: two, so a
/// workspace one level down (`trust/crates/target`) and a build directory
/// beside a worktree (`trust-wt/m1-target`, `trust-wt/m1/target`) are found.
pub const CHECKOUT_DEPTH: usize = 2;

/// Levels below a session's `scratchpad/` the walk enters.
pub const SCRATCH_DEPTH: usize = 4;

/// The most build directories one discovery reports.
pub const MAX_FOUND: usize = 128;

/// The most children of one directory the walk reads before it moves on.
const MAX_CHILDREN: usize = 4096;

/// The most worktrees read from one repository.
const MAX_WORKTREES: usize = 64;

/// The most entries a session's temp directory may hold for it to be dated
/// ([`newest_write`]); a larger one is never called quiet.
pub const MAX_SESSION_ENTRIES: usize = 100_000;

/// The levels a working directory's git worktree root is looked for above it.
const MAX_ANCESTORS: usize = 32;

/// Where the watch never looks, and when a checkout is gone: the one rule,
/// shared with atpkg's build cache sweep.
pub use aterm_toolchain::checkout;

/// Directories never walked (as `$HOME`'s children, where a working
/// directory that is `$HOME` meets them too) — the system's own trees and,
/// on macOS, the privacy-guarded folders ([`checkout::HOME_SKIP`]). A
/// checkout there is still searched from an agent working in it.
pub use checkout::HOME_SKIP;

/// What the kernel says of a running process's executable ([`Process::exe`]).
pub use aterm_uds::holders::Exe;

/// Discovery's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Directory entries it may read, all roots together.
    pub entries: usize,
    /// Wall clock it may spend.
    pub wall: Duration,
    /// Build directories it may report.
    pub found: usize,
}

impl Budget {
    /// The shipped bounds.
    pub const SHIPPED: Budget = Budget {
        entries: MAX_WALK_ENTRIES,
        wall: Duration::from_secs(20),
        found: MAX_FOUND,
    };
}

/// Where discovery looks. Every path is injected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// The working directories of the supervised sessions (the verb's own).
    pub cwds: Vec<PathBuf>,
    /// `$HOME`: its direct children and their worktrees are walked, and its
    /// guarded folders are never looked into ([`HOME_SKIP`]).
    pub home: Option<PathBuf>,
    /// The Claude Code directories whose registries and transcripts say
    /// whether a session ended ([`claude_dirs`]): `~/.claude`, each running
    /// Claude Code's own ([`Running::dirs`]) and the identities'. Not walked;
    /// carried to the survey's [`Owners`].
    pub claude_dirs: Vec<PathBuf>,
    /// The running Claude Codes ([`Running::claudes`]): one no registry read
    /// here pins to its session keeps every session of its project.
    pub claudes: Vec<RunningClaude>,
    /// Why a process that may be a Claude Code could not be read
    /// ([`claude_census`]'s refusal): then no session is called ended.
    pub claude_unread: Option<String>,
    /// The `CARGO_TARGET_DIR` each supervised agent was started with.
    pub agent_target_dirs: Vec<PathBuf>,
    /// Claude Code's per-user temp root ([`claude_tmp_root`]).
    pub claude_tmp: Option<PathBuf>,
    /// The device of the volume whose free space is measured: a root, a
    /// directory or a build directory on any other is never entered or
    /// reported (removing it frees nothing there). `None`: any device.
    pub device: Option<u64>,
    /// The system's mount points ([`mount_points`]): never entered, never
    /// even `stat`ed — a stalled network mount answers a `stat` of its root
    /// by blocking, and a blocked look holds the disk watch until restart.
    pub mounts: Vec<PathBuf>,
}

/// Claude Code's per-user temp root for `uid`: `/private/tmp/claude-<uid>`
/// where `/private/tmp` exists (macOS), else `/tmp/claude-<uid>`.
#[must_use]
pub fn claude_tmp_root(uid: u32) -> PathBuf {
    let base = if Path::new("/private/tmp").is_dir() {
        "/private/tmp"
    } else {
        "/tmp"
    };
    PathBuf::from(format!("{base}/claude-{uid}"))
}

/// The Claude Code directories a survey reads sessions' ends from
/// ([`Roots::claude_dirs`]): `home`'s own (`~/.claude`, as
/// [`footer::claude_dir_of`] spells it) and every one in `known` — a
/// supervised agent's, an identity's, the caller's, each resolved by its
/// reader from its `CLAUDE_CONFIG_DIR` — each once.
///
/// [`footer::claude_dir_of`]: super::super::footer::claude_dir_of
#[must_use]
pub fn claude_dirs(home: Option<&Path>, known: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let own = super::super::footer::claude_dir_of(None, home.and_then(Path::to_str), || None);
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in own.into_iter().chain(known) {
        if dir.is_absolute() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

/// What the running processes say of the Claude Codes ([`claude_census`]):
/// the ones running, or why one may run unseen — then no session is called
/// ended.
pub type ClaudeCensus = Result<Running, String>;

/// The running Claude Codes of this uid ([`claude_census`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Running {
    /// Each one, as the kernel names it.
    pub claudes: Vec<RunningClaude>,
    /// The Claude Code directory each process of this uid was started with
    /// (`CLAUDE_CONFIG_DIR`, else `$HOME/.claude`, in its environment at
    /// exec), each once — every process's, not only a Claude Code's: one
    /// the kernel does not name one (a copy of it renamed) keeps its
    /// registry there too. Read for REACH, never trusted for safety: there a
    /// registry pins a Claude Code to its session and names the sessions
    /// running, and transcripts date one, but a settings file moves the
    /// directory after the exec (measured with Claude Code 2.1.287), and a
    /// Claude Code no registry read here pins keeps its whole project
    /// ([`RunningClaude::may_hold`]).
    pub dirs: Vec<PathBuf>,
}

/// One running Claude Code ([`Running::claudes`]): what the KERNEL says of
/// it, which no settings file can move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningClaude {
    /// Its pid.
    pub pid: u32,
    /// When the kernel started it, as Claude Code writes `procStart` in its
    /// registry record (`ps -o lstart=` in UTC and the C locale, its spaces
    /// squashed): a record naming its pid pins it only at this start (a pid
    /// reused since the record was written pins nothing). `None`: not read,
    /// never pinned.
    pub started: Option<String>,
    /// Its working directory. `None`: not read — it may hold a session of
    /// any project.
    pub cwd: Option<PathBuf>,
}

/// Claude Code's cap on a project folder's name: past it the name is its
/// first [`PROJECT_NAME_CAP`] characters, a `-` and a hash of the path
/// (measured, Claude Code 2.1.287).
const PROJECT_NAME_CAP: usize = 200;

/// A directory's name as Claude Code writes it for its project folder: every
/// character but an ASCII letter or digit as `-`, ASCII only (see
/// [`RunningClaude::may_hold`]).
fn project_name(dir: &str) -> String {
    dir.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                char::from(b)
            } else {
                '-'
            }
        })
        .collect()
}

impl RunningClaude {
    /// May it hold a session of the project folder `project` (its temp
    /// directory's `<project>`)? Claude Code names that folder after the
    /// directory it was LAUNCHED in, resolved ([`project_name`]; measured
    /// with Claude Code 2.1.287 against its working directory as the kernel
    /// holds it, which is that directory), and it changes into its own
    /// worktree mid-session (`<repo>/.claude/worktrees/<name>`), while the
    /// session keeps its folder: so a working directory names its own
    /// project and, inside such a worktree, every project at or under the
    /// repository. A working directory not read, or not ASCII (the name is
    /// written from its NFC form, one `-` per UTF-16 unit, which the
    /// kernel's spelling need not share), may hold any.
    #[must_use]
    pub fn may_hold(&self, project: &std::ffi::OsStr) -> bool {
        let (Some(cwd), Some(project)) = (
            self.cwd
                .as_deref()
                .and_then(Path::to_str)
                .filter(|c| c.is_ascii()),
            project.to_str(),
        ) else {
            return true;
        };
        // `name` as Claude Code writes it: whole up to the cap, past it its
        // head, a `-` and a hash. (`name` is ASCII: a byte is a character.)
        let cap = PROJECT_NAME_CAP;
        let named = |name: &str| {
            if name.len() <= cap {
                project == name
            } else {
                project.starts_with(&name[..cap]) && project.as_bytes().get(cap) == Some(&b'-')
            }
        };
        // A directory's own project, or one of a directory under it.
        let at_or_under = |name: &str| {
            if name.len() < cap {
                project == name || project.starts_with(&format!("{name}-"))
            } else {
                project.starts_with(&name[..cap])
            }
        };
        named(&project_name(cwd))
            || cwd
                .split_once("/.claude/worktrees/")
                .is_some_and(|(repo, _)| at_or_under(&project_name(repo)))
    }

    /// Where it runs, for a line.
    fn place(&self) -> String {
        self.cwd.as_deref().map_or_else(
            || "a folder that could not be read".to_owned(),
            |c| c.display().to_string(),
        )
    }
}

/// One running process of this uid, as [`claude_census`] reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Process {
    /// Its pid.
    pub pid: u32,
    /// What the kernel says of its executable, read AFTER its exec record,
    /// so one that exited meanwhile reads [`Exe::Exited`]. `None`: not read.
    pub exe: Option<Exe>,
    /// Its exec record (`KERN_PROCARGS2` on macOS, `/proc` on Linux), `None`
    /// when it could not be read.
    pub exec: Option<Exec>,
    /// Its working directory, read only for a Claude Code or a process whose
    /// own directory is not read ([`RunningClaude::cwd`]).
    pub cwd: Option<PathBuf>,
    /// Its kernel start, read only for a Claude Code
    /// ([`RunningClaude::started`]).
    pub started: Option<String>,
}

/// What a process was exec'd as ([`Process::exec`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exec {
    /// The path handed to `exec`.
    pub path: String,
    /// `argv[0]` and `argv[1]` (an interpreter's script: `node
    /// …/claude-code/cli.js`, atpkg's shim `sh …/agents/claude` before its
    /// own `exec`).
    pub argv: Vec<String>,
    /// `CLAUDE_CONFIG_DIR` in its environment at exec.
    pub config: Option<String>,
    /// `HOME` in its environment at exec.
    pub home: Option<String>,
}

/// The names Claude Code's own binary answers to as a TOOL, not a Claude
/// Code (it dispatches on `argv[0]`): its shell's `grep` is `ARGV0=ugrep
/// <claude>`, its `find` `ARGV0=bfs <claude>`, its `rg` and its Linux
/// sandbox's `apply-seccomp` likewise (Claude Code 2.1.287's shell
/// snapshot). Measured 2026-10-02: 7 such `ugrep`s ran orphaned for a day
/// and more, in the working directories of the sessions that started them.
/// A name not here reads as a Claude Code: one more kept project, never a
/// session lost.
const CLAUDE_TOOLS: &[&str] = &["ugrep", "bfs", "rg", "apply-seccomp"];

/// Is a process running `exe` (the kernel's, resolved), exec'd as `exec`, a
/// Claude Code? Its executable, exec path, `argv[0]` or `argv[1]` has a
/// path component `claude` or `claude-code` (ASCII case aside) — the atpkg
/// store's `…/store/claude/<build>/bin/claude`, the native installer's
/// `~/.local/share/claude/versions/<version>` (whose own name is the
/// version, reached by a link of any name), a cask's
/// `…/claude-code/<version>/claude`, npm's `node
/// …/@anthropic-ai/claude-code/cli.js`; never `~/.claude/…` nor the temp
/// root's `claude-<uid>` — and its `argv[0]` is not one of the binary's own
/// tools ([`CLAUDE_TOOLS`]).
fn claude_code(exe: Option<&Exe>, exec: &Exec) -> bool {
    let claude = |n: &String| {
        Path::new(n).components().any(|c| match c {
            std::path::Component::Normal(s) => s.to_str().is_some_and(|s| {
                s.eq_ignore_ascii_case("claude") || s.eq_ignore_ascii_case("claude-code")
            }),
            _ => false,
        })
    };
    let tool = exec
        .argv
        .first()
        .and_then(|argv0| Path::new(argv0).file_name());
    let exe = match exe {
        Some(Exe::At(p)) => p.to_str().map(str::to_owned),
        _ => None,
    };
    !tool.is_some_and(|t| CLAUDE_TOOLS.iter().any(|n| t == *n))
        && exe
            .iter()
            .chain(std::iter::once(&exec.path))
            .chain(&exec.argv)
            .any(claude)
}

/// Where the system's own executables live (macOS: its sealed volume):
/// none is a Claude Code. Measured 2026-10-02: macOS read 410 of this uid's
/// 483 processes with an empty environment, every one under these roots, and
/// every process elsewhere whole.
const SYSTEM_EXECUTABLES: &[&str] = &[
    "/System",
    "/bin",
    "/sbin",
    "/usr/bin",
    "/usr/sbin",
    "/usr/libexec",
    "/usr/lib",
];

/// Can the census tell what a process is, its exec record read as `exec`
/// and then its executable as `exe`: its record read, it exited, or it runs
/// one of the system's own executables ([`SYSTEM_EXECUTABLES`])? One it
/// cannot tell may be a Claude Code ([`claude_census`]).
fn told(exec: Option<&Exec>, exe: &Exe) -> bool {
    exec.is_some()
        || match exe {
            Exe::Exited => true,
            Exe::At(exe) => SYSTEM_EXECUTABLES.iter().any(|root| exe.starts_with(root)),
            Exe::Unnamed => false,
        }
}

/// How many more times the census reads a process it cannot tell
/// ([`settled`]) before it refuses on it, pausing 1 ms before the first and
/// twice as long before each next: 255 ms in all. A budget, not a verdict:
/// most settle within a millisecond, but one has been measured taking 61 ms
/// under load (2026-10-02), and only one that never settles pays it whole.
const EXEC_REREADS: u32 = 8;

/// A process's exec record and then its executable, as `read` gives them,
/// read again while the census cannot tell it ([`told`]), at most
/// [`EXEC_REREADS`] times, `pause` taken between. A process caught in the
/// middle of its spawn has no exec record yet, and the kernel still names
/// its PARENT's executable for it (a build tool's, a test binary's, aterm's
/// own); its record is there once its exec completes, and that record is
/// what it is. Measured 2026-10-02 at load average 60: every one of 69
/// caught settled within 1 ms, while one read refused 22 of 300 censuses —
/// 130 of 300 with a spawn loop beside it. One the census still cannot tell
/// after the last read is refused, as before: what it may hold is kept.
fn settled(
    mut read: impl FnMut() -> (Option<Exec>, Exe),
    mut pause: impl FnMut(Duration),
) -> (Option<Exec>, Exe) {
    let mut got = read();
    for n in 0..EXEC_REREADS {
        if told(got.0.as_ref(), &got.1) {
            break;
        }
        pause(Duration::from_millis(1 << n));
        got = read();
    }
    got
}

/// The running Claude Codes among `processes` ([`claude_code`]), each with
/// its working directory and start — KERNEL facts, which decide who may hold
/// a session ([`Owners`]) — and, for reach, the Claude Code directory each
/// process was started with ([`Running::dirs`]): one that lies where the
/// watch never looks ([`checkout::never_looked_at`] in `home`: a
/// privacy-guarded folder, a volume) is not read — but aterm's own state
/// directory, where its identities and accounts keep theirs
/// ([`in_aterm_state`]), is — and none is ever a reason to refuse: a process
/// whose directory is not read keeps its project, as an unpinned Claude Code
/// does ([`OwnDir::Unread`]). Refused,
/// so that no session is called ended ([`Roots::claude_unread`]), only when
/// a process may be a Claude Code that cannot be told ([`told`]): its exec
/// record could not be read — even read again while its spawn may be in
/// flight ([`settled`]) — and it is not one of the system's own
/// executables. One the kernel says EXITED runs nothing — but one whose
/// executable cannot be named (unlinked since its exec, or the read refused)
/// still runs, and is refused like any other not read.
///
/// # Errors
///
/// Why a process that may be a Claude Code could not be read.
pub fn claude_census(
    processes: impl IntoIterator<Item = Process>,
    home: Option<&Path>,
) -> ClaudeCensus {
    let mut out = Running::default();
    for Process {
        pid,
        exe,
        exec,
        cwd,
        started,
    } in processes
    {
        let Some(exec) = exec else {
            if exe.as_ref().is_some_and(|exe| told(None, exe)) {
                continue;
            }
            let exe = match exe {
                Some(Exe::At(exe)) => exe.display().to_string(),
                _ => "its executable could not be named".to_owned(),
            };
            return Err(format!(
                "process {pid} ({exe}) could not be read, and may be a Claude Code"
            ));
        };
        if matches!(exe, Some(Exe::Exited)) {
            continue;
        }
        let dir = own_dir(&exec, cwd.as_deref(), home);
        // One whose own directory is not read here may be a Claude Code the
        // kernel does not name one (a copy renamed) keeping its registry
        // there: it keeps its project, as one no registry pins does.
        if claude_code(exe.as_ref(), &exec) || dir == OwnDir::Unread {
            out.claudes.push(RunningClaude { pid, started, cwd });
        }
        if let OwnDir::Read(dir) = dir
            && !out.dirs.contains(&dir)
        {
            out.dirs.push(dir);
        }
    }
    Ok(out)
}

/// The Claude Code directory a process was started with, as the census reads
/// it ([`claude_census`]).
#[derive(Debug, PartialEq, Eq)]
enum OwnDir {
    /// Neither `CLAUDE_CONFIG_DIR` nor `HOME` names one.
    Unset,
    /// Read: its registry and transcripts are judged.
    Read(PathBuf),
    /// Named but not read: not an absolute path, or where the watch never
    /// looks ([`checkout::never_looked_at`] in `home`) outside aterm's own
    /// state directory ([`in_aterm_state`]).
    Unread,
}

/// [`OwnDir`] of a process exec'd as `exec`, running in `cwd` (where read):
/// a relative `CLAUDE_CONFIG_DIR` — set and EMPTY included — names the
/// directory under it, as Claude Code resolves it
/// ([`super::super::footer::claude_dir_of`], the one resolver).
fn own_dir(exec: &Exec, cwd: Option<&Path>, home: Option<&Path>) -> OwnDir {
    let set = |v: &Option<String>| v.as_deref().is_some_and(|v| !v.is_empty());
    match super::super::footer::claude_dir_of(exec.config.as_deref(), exec.home.as_deref(), || {
        cwd.map(Path::to_path_buf)
    }) {
        None if exec.config.is_some() || set(&exec.home) => OwnDir::Unread,
        None => OwnDir::Unset,
        Some(dir)
            if checkout::never_looked_at(&dir, home).is_some() && !in_aterm_state(&dir, home) =>
        {
            OwnDir::Unread
        }
        Some(dir) => OwnDir::Read(dir),
    }
}

/// Is `dir` inside aterm's own state directory under `home` (macOS:
/// `~/Library/Application Support/aterm`) — an identity's Claude Code
/// directory (`identities/<name>/.claude`, as `spawn identity=` starts one)
/// or an account's login (`harness/claude-harness/accounts/logins/<label>`)?
/// The window reads those as it reads any other: no privacy consent guards
/// aterm's own folder. Lexical; a path that climbs out by `..` is not.
fn in_aterm_state(dir: &Path, home: Option<&Path>) -> bool {
    let platform = aterm_types::dirs::StatePlatform {
        home: home.map(Path::to_path_buf),
        ..aterm_types::dirs::StatePlatform::default()
    };
    aterm_types::dirs::resolve_state_dir(None, platform).is_some_and(|state| {
        dir.starts_with(&state)
            && !dir
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
    })
}

/// [`claude_census`] of every process of this uid running now: each one's
/// exec record read (`KERN_PROCARGS2` on macOS, `/proc` on Linux; again
/// while a spawn is in flight, [`settled`]) and dropped at once but for what
/// it was exec'd as and its two variables — never kept, never logged; a
/// Claude Code's working directory and start read from the kernel. Read
/// below the floor only; this process's own too.
///
/// # Errors
///
/// As [`claude_census`]; or the processes could not be listed.
pub fn running_claude_census(home: Option<&Path>) -> ClaudeCensus {
    let pids = aterm_uds::holders::uid_pids()
        .map_err(|e| format!("the running processes could not be listed ({e})"))?;
    let processes = pids.into_iter().filter_map(|pid| {
        let read = || {
            let exec = atpkg::caller_shell::process_args(pid)
                .filter(|args| !args.exec_path.is_empty() || !args.argv.is_empty())
                .map(|args| Exec {
                    config: args.env_var("CLAUDE_CONFIG_DIR").map(str::to_owned),
                    home: args.env_var("HOME").map(str::to_owned),
                    path: args.exec_path,
                    argv: args.argv.into_iter().take(2).collect(),
                });
            (exec, aterm_uds::holders::exe_of(pid))
        };
        let (exec, exe) = settled(read, std::thread::sleep);
        let (mut cwd, mut started) = (None, None);
        let claude = exec.as_ref().is_some_and(|e| claude_code(Some(&exe), e));
        if claude
            || exec
                .as_ref()
                .is_some_and(|e| own_dir(e, None, home) == OwnDir::Unread)
        {
            cwd = aterm_sysprobe::process_cwd(pid);
            // One that exited since it was listed holds nothing.
            if cwd.is_none() && aterm_uds::holders::exe_of(pid) == Exe::Exited {
                return None;
            }
        }
        // Only one the kernel names a Claude Code can be pinned: another's
        // registry is not read.
        if claude {
            started = super::super::upgrade_drive::kernel_start(pid);
        }
        Some(Process {
            pid,
            exe: Some(exe),
            exec,
            cwd,
            started,
        })
    });
    claude_census(processes, home)
}

/// This process's real uid.
#[must_use]
pub fn own_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid(2) cannot fail and touches no memory.
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// The device of the directory at `path`, a link followed (as `statvfs`
/// follows it): what [`Roots::device`] is set from.
#[must_use]
pub fn device_of(path: &Path) -> Option<u64> {
    identity(path).map(|(dev, _)| dev)
}

/// The system's mount points, read from the mount table without asking any
/// file system (`getmntinfo(MNT_NOWAIT)` on macOS, `/proc/self/mountinfo` on
/// Linux): what [`Roots::mounts`] is set from. A mount point written under
/// `/System/Volumes/Data` (macOS's data volume) is listed in its firmlinked
/// form too. Empty where there is no reader.
#[must_use]
pub fn mount_points() -> Vec<PathBuf> {
    let mut out = raw_mount_points();
    let firmlinked: Vec<PathBuf> = out
        .iter()
        .filter_map(|p| p.strip_prefix("/System/Volumes/Data").ok())
        .filter(|rel| !rel.as_os_str().is_empty())
        .map(|rel| Path::new("/").join(rel))
        .collect();
    out.extend(firmlinked);
    out.sort();
    out.dedup();
    out
}

#[cfg(target_os = "macos")]
fn raw_mount_points() -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    unsafe extern "C" {
        #[cfg_attr(target_arch = "x86_64", link_name = "getmntinfo$INODE64")]
        fn getmntinfo(mntbufp: *mut *mut libc::statfs, flags: libc::c_int) -> libc::c_int;
    }
    const MNT_NOWAIT: libc::c_int = 2;
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo(3) points `buf` at an array of `n` entries it owns
    // (valid until the next call on this thread); MNT_NOWAIT asks no file
    // system, so a stalled mount cannot block it.
    let n = unsafe { getmntinfo(&raw mut buf, MNT_NOWAIT) };
    let Ok(n) = usize::try_from(n) else {
        return Vec::new();
    };
    if buf.is_null() {
        return Vec::new();
    }
    // SAFETY: as above — `n` initialized entries at `buf`.
    let all = unsafe { std::slice::from_raw_parts(buf, n) };
    all.iter()
        .map(|fs| {
            let name = &fs.f_mntonname;
            let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
            let bytes: Vec<u8> = name[..len].iter().map(|c| c.to_ne_bytes()[0]).collect();
            PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))
        })
        .filter(|p| p.is_absolute())
        .collect()
}

#[cfg(target_os = "linux")]
fn raw_mount_points() -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| l.split(' ').nth(4))
        .map(unescape_mount)
        .filter(|p| p.is_absolute())
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn raw_mount_points() -> Vec<PathBuf> {
    Vec::new()
}

/// A mountinfo field with its octal escapes (`\040` for a space) decoded.
#[cfg(any(target_os = "linux", test))]
fn unescape_mount(field: &str) -> PathBuf {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = b
            .get(i + 1..i + 4)
            .filter(|d| b[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)))
            .map(|d| d.iter().fold(0u32, |a, c| a * 8 + u32::from(c - b'0')));
        match octal.and_then(|v| u8::try_from(v).ok()) {
            Some(v) => {
                out.push(v);
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

/// How a build directory was found — which also says what frees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Origin {
    /// Named by the caller (the verb's line).
    #[default]
    Named,
    /// Under a checkout: a working directory, a worktree, a child of `$HOME`.
    Checkout {
        /// The root it was found under.
        root: PathBuf,
    },
    /// A supervised agent's `CARGO_TARGET_DIR`.
    AgentTargetDir,
    /// One workspace's build directory in the Rust build cache
    /// (`atpkg::build_cache`, `<cache><workspace>/.cargo-build`): its
    /// intermediates, the workspace's `target/` holding its binaries. Its
    /// OWNER is the workspace: one SEEN gone ([`checkout::gone`], the rule
    /// `atpkg::build_cache::reclaim` shares) makes it [`Owned::Gone`], and
    /// below the floor it goes whole after [`super::PRESSURE_MIN_IDLE_S`]
    /// unwritten ([`Owner::quiet_s`]) — a removed worktree's 10-40 GiB cache
    /// is what filled the disk twice (2026-10-16). A live workspace's is
    /// reclaimed as a checkout's is.
    BuildCache {
        /// The workspace the mirror folder stands for: the folder holding
        /// the `.cargo-build` with the cache root taken off, as an absolute
        /// path (`atpkg::build_cache::build_dir`'s inverse).
        workspace: PathBuf,
        /// The mirror folder that holds the `.cargo-build`, canonical: the
        /// build directory lies inside it.
        mirror: PathBuf,
    },
    /// One of x.py's cargo build directories under `<repo>/build/<host>/`.
    Bootstrap {
        /// The x.py checkout.
        repo: PathBuf,
        /// Does it carry `scripts/slim_build.py`, its own reclaimer?
        slim: bool,
    },
    /// Under a Claude Code session's `scratchpad/` — its canonical path
    /// INSIDE the session's directory ([`Walk::add`]).
    ClaudeScratch {
        /// The session id (the directory's name).
        session: String,
        /// `<claude tmp>/<project>/<session>`, canonical: its transcript is
        /// `<claude dir>/projects/<project>/<session>.jsonl`, in whichever
        /// Claude Code directory holds it ([`Owners`]).
        session_dir: PathBuf,
    },
    /// Reached from an owner's folder by a `target*` link that points OUT of
    /// it, or from two owners' at once: it belongs to whatever else names it,
    /// never to an owner that can end. (A scratch worktree's `target ->
    /// ~/repo/target.noindex` must never make the checkout's build directory
    /// an ended session's.)
    Linked {
        /// The link, or the second owner's folder, it was reached through.
        from: PathBuf,
    },
}

impl Origin {
    /// Can this origin's owner be GONE (a session)?
    #[must_use]
    pub fn ownable(&self) -> bool {
        matches!(
            self,
            Origin::ClaudeScratch { .. } | Origin::BuildCache { .. }
        )
    }

    /// The folder an ownable origin's build directory must lie inside.
    fn owner_dir(&self) -> Option<&Path> {
        match self {
            Origin::ClaudeScratch { session_dir, .. } => Some(session_dir),
            Origin::BuildCache { mirror, .. } => Some(mirror),
            _ => None,
        }
    }

    /// One build directory reached twice: the origin it keeps. A directory
    /// any checkout, agent or caller names is THEIRS (never an owner's that
    /// can end), whichever walk met it first; two different owners make it
    /// [`Origin::Linked`].
    fn merge(self, other: Origin) -> Origin {
        if self == other || !self.ownable() {
            return self;
        }
        if !other.ownable() {
            return other;
        }
        let from = other.owner_dir().map(Path::to_path_buf).unwrap_or_default();
        Origin::Linked { from }
    }
}

/// One build directory found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its path, CANONICAL.
    pub path: PathBuf,
    /// How it was found: every walk that met it, merged ([`Origin::merge`]).
    pub origin: Origin,
}

/// What discovery found, and what it could not finish.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovered {
    /// The build directories, each once.
    pub found: Vec<Found>,
    /// Why the walk stopped short, when it did.
    pub notes: Vec<String>,
}

/// Find every build directory reachable from `roots` within `budget`.
///
/// The roots whose build directories have NO owner that can end are walked
/// FIRST — the agents' `CARGO_TARGET_DIR`, their working directories,
/// `$HOME`'s checkouts — and Claude Code's temp root after: a build directory any of those names is theirs ([`Origin::merge`]),
/// and a walk cut short before they are done reaches no owner's at all (each
/// later walk reads nothing once the budget is spent), so an owner's build
/// directory is never one a checkout the walk missed might use.
#[must_use]
pub fn discover(roots: &Roots, budget: Budget) -> Discovered {
    let mut w = Walk {
        started: Instant::now(),
        budget,
        entries: 0,
        cut: None,
        seen: BTreeMap::new(),
        found: Vec::new(),
        device: roots.device,
        mounts: roots.mounts.iter().cloned().collect(),
        // The mounts a `target*` link must not lead into: every one but the
        // root and those `$HOME` lives on.
        foreign: roots
            .mounts
            .iter()
            .filter(|m| m.as_path() != Path::new("/"))
            .filter(|m| !roots.home.as_deref().is_some_and(|h| h.starts_with(m)))
            .cloned()
            .collect(),
        notes: Vec::new(),
    };
    for dir in &roots.agent_target_dirs {
        if dir.is_absolute()
            && let Ok(real) = std::fs::canonicalize(dir)
            && marked(&real)
        {
            w.add(&real, &Origin::AgentTargetDir);
        }
    }
    for cwd in &roots.cwds {
        if !cwd.is_absolute() {
            continue;
        }
        let Ok(cwd) = std::fs::canonicalize(cwd) else {
            continue;
        };
        w.checkout(&cwd);
        if let Some(root) = worktree_root(&cwd)
            && root != cwd
        {
            w.checkout(&root);
        }
    }
    if let Some(home) = roots
        .home
        .as_deref()
        .and_then(|h| std::fs::canonicalize(h).ok())
        && w.on_device(&home)
    {
        for (name, child, ty) in w.children(&home) {
            let n = name.to_string_lossy();
            if !ty.is_dir() || n.starts_with('.') || HOME_SKIP.contains(&n.as_ref()) {
                continue;
            }
            if !w.same_device(&child) {
                continue;
            }
            w.checkout(&child);
            for wt in worktrees(&child) {
                w.checkout(&wt);
            }
        }
    }
    if let Some(home) = roots.home.as_deref() {
        w.build_cache(&atpkg::build_cache::cache_root(home));
    }
    if let Some(tmp) = &roots.claude_tmp {
        w.claude_scratch(tmp);
    }
    let mut notes = w.notes;
    if let Some(why) = w.cut {
        notes.push(format!(
            "cargo-targets: the search for build directories stopped short ({why}); the {} found are surveyed",
            w.found.len()
        ));
    }
    Discovered {
        found: w.found,
        notes,
    }
}

/// Does `dir` carry a build tool's marker: a signed `CACHEDIR.TAG`, or
/// cargo's `.rustc_info.json`?
fn marked(dir: &Path) -> bool {
    cachedir_tag_signed(&dir.join("CACHEDIR.TAG"))
        || std::fs::symlink_metadata(dir.join(RUSTC_INFO)).is_ok_and(|m| m.is_file())
}

/// Whether `name`, a child of an x.py tree's `build/<host>/`, is one of the
/// bootstrap's cargo build directories: `stage<N>-<kind>` for N of 1 or more,
/// or `bootstrap-tools`. Never `stage<N>` itself (a sysroot), anything of
/// stage0, `llvm`, or a `-sysroot`/`-bin` tree.
#[must_use]
pub fn bootstrap_cargo_dir(name: &str) -> bool {
    if name == "bootstrap-tools" {
        return true;
    }
    let Some((n, kind)) = name
        .strip_prefix("stage")
        .and_then(|rest| rest.split_once('-'))
    else {
        return false;
    };
    !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
        && !n.starts_with('0')
        && !kind.is_empty()
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !kind.contains("sysroot")
        && !kind.ends_with("bin")
}

/// Is `dir` an x.py checkout (it holds an `x.py` file)?
fn is_xpy(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join("x.py")).is_ok_and(|m| m.is_file())
}

/// Is `dir` at or under an x.py checkout's `build/`? Its trees are the
/// bootstrap rule's alone ([`bootstrap_cargo_dir`]): a working directory
/// inside `build/<host>/` must not reach stage0 or a sysroot by the plain
/// walk.
fn inside_xpy_build(dir: &Path) -> bool {
    dir.ancestors()
        .take(MAX_ANCESTORS)
        .any(|a| a.file_name().is_some_and(|n| n == "build") && a.parent().is_some_and(is_xpy))
}

/// The git worktree root at or above `dir`: the nearest directory holding a
/// `.git` (a repository's directory or a worktree's file).
fn worktree_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .take(MAX_ANCESTORS)
        .find(|d| std::fs::symlink_metadata(d.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

/// The worktrees of the repository at `repo`, read from
/// `.git/worktrees/*/gitdir` (each names `<worktree>/.git`).
fn worktrees(repo: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(repo.join(".git").join("worktrees")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .take(MAX_WORKTREES)
        .filter_map(|e| small_text(&e.path().join("gitdir")))
        .map(|t| PathBuf::from(t.trim()))
        .filter(|p| p.is_absolute())
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .filter(|wt| std::fs::symlink_metadata(wt).is_ok_and(|m| m.is_dir()))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A small regular file's bytes (at most 4 KiB), never through a link.
fn small_bytes(p: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(p).ok()?;
    if !meta.is_file() || meta.len() > 4096 {
        return None;
    }
    std::fs::read(p).ok()
}

/// [`small_bytes`] as text.
fn small_text(p: &Path) -> Option<String> {
    String::from_utf8(small_bytes(p)?).ok()
}

/// The device of `p`, a link NOT followed.
fn dev_nofollow(p: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(p).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        Some(0)
    }
}

/// `(device, inode)` of the directory at `p`, a link followed.
fn identity(p: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        None
    }
}

/// Is Claude Code's temp root at `tmp` this user's own private directory —
/// a real directory (no link), owned by this uid, writable by nobody else?
/// On a shared `/tmp` another local user could make `claude-<uid>` first and
/// plant links in it; `Err` says why it is not searched.
fn private_dir(tmp: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(tmp).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err("it is not a directory (a link is never followed)".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != own_uid() {
            return Err(format!("it is owned by uid {}, not this user", meta.uid()));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(format!(
                "its mode {:o} lets another user write in it",
                meta.mode() & 0o7777
            ));
        }
    }
    Ok(())
}

struct Walk {
    started: Instant,
    budget: Budget,
    entries: usize,
    cut: Option<String>,
    /// Each build directory found, by `(device, inode)`: its index in `found`.
    seen: BTreeMap<(u64, u64), usize>,
    found: Vec<Found>,
    /// [`Roots::device`].
    device: Option<u64>,
    /// [`Roots::mounts`].
    mounts: BTreeSet<PathBuf>,
    /// The mount points a `target*` link is never followed into
    /// ([`Self::leads_into_mount`]).
    foreign: Vec<PathBuf>,
    /// What a root could not be searched for, said once each.
    notes: Vec<String>,
}

impl Walk {
    /// Is the budget spent? Says why the first time.
    fn spent(&mut self) -> bool {
        if self.cut.is_some() {
            return true;
        }
        if self.started.elapsed() >= self.budget.wall {
            self.cut = Some("it ran out of time".to_owned());
        } else if self.entries > self.budget.entries {
            self.cut = Some(format!("it read {} entries", self.budget.entries));
        } else if self.found.len() >= self.budget.found {
            self.cut = Some(format!("it found {}", self.budget.found));
        }
        self.cut.is_some()
    }

    /// Is the directory at `p` (a link followed) on the measured device?
    fn on_device(&self, p: &Path) -> bool {
        self.device
            .is_none_or(|d| identity(p).is_some_and(|(dev, _)| dev == d))
    }

    /// Does the link at `link` name a path under a mount point other than
    /// the root and `$HOME`'s ([`Roots::mounts`])? Read as written, before
    /// anything resolves it: resolving would `stat` that mount's root, and a
    /// stalled one answers by blocking. An unreadable link is treated as one.
    fn leads_into_mount(&self, link: &Path) -> bool {
        let Ok(to) = std::fs::read_link(link) else {
            return true;
        };
        let abs = link.parent().map_or_else(|| to.clone(), |p| p.join(&to));
        self.foreign.iter().any(|m| abs.starts_with(m))
    }

    /// Is the entry at `p` (a link NOT followed) on the measured device?
    fn same_device(&self, p: &Path) -> bool {
        self.device.is_none_or(|d| dev_nofollow(p) == Some(d))
    }

    /// The children of `dir`, sorted, each with its type as the directory
    /// lists it (a link is a link, never followed) — a mount point left out
    /// before anything asks its file system.
    fn children(&mut self, dir: &Path) -> Vec<(OsString, PathBuf, std::fs::FileType)> {
        if self.spent() {
            return Vec::new();
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<(OsString, PathBuf, std::fs::FileType)> = rd
            .take(MAX_CHILDREN)
            .flatten()
            .filter(|e| !self.mounts.contains(&e.path()))
            .filter_map(|e| Some((e.file_name(), e.path(), e.file_type().ok()?)))
            .collect();
        self.entries = self.entries.saturating_add(out.len());
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Record the build directory at `dir` (canonical), once by device and
    /// inode, on the measured device only. An owner's origin holds only for
    /// a directory INSIDE the owner's folder; one outside it (reached by a
    /// link) is [`Origin::Linked`]. Met again, the origins merge
    /// ([`Origin::merge`]).
    fn add(&mut self, dir: &Path, origin: &Origin) {
        self.add_via(dir, origin, dir);
    }

    /// [`Self::add`], `via` the path it was reached through (a link).
    fn add_via(&mut self, dir: &Path, origin: &Origin, via: &Path) {
        if self.spent() {
            return;
        }
        let Ok(path) = std::fs::canonicalize(dir) else {
            return;
        };
        let Some(id) = identity(&path) else {
            return;
        };
        if self.device.is_some_and(|d| d != id.0) {
            return;
        }
        let origin = match origin.owner_dir() {
            Some(owner) if !(path.starts_with(owner) && path != owner) => Origin::Linked {
                from: via.to_path_buf(),
            },
            _ => origin.clone(),
        };
        match self.seen.get(&id) {
            Some(&i) => {
                if let Some(f) = self.found.get_mut(i) {
                    f.origin = std::mem::take(&mut f.origin).merge(origin);
                }
            }
            None => {
                self.seen.insert(id, self.found.len());
                self.found.push(Found { path, origin });
            }
        }
    }

    /// A checkout root: x.py's bootstrap trees, then the marker walk —
    /// never a root on another device, and never one inside an x.py tree's
    /// `build/` (the bootstrap rule's alone).
    fn checkout(&mut self, root: &Path) {
        if !self.on_device(root) || inside_xpy_build(root) {
            return;
        }
        if is_xpy(root) {
            self.bootstrap(root);
        }
        let origin = Origin::Checkout {
            root: root.to_path_buf(),
        };
        if marked(root) {
            self.add(root, &origin);
            return;
        }
        let Some(dev) = dev_nofollow(root) else {
            return;
        };
        self.marked_walk(root, CHECKOUT_DEPTH, dev, &origin);
    }

    /// Below `dir`, `depth` levels: every directory carrying a marker (not
    /// entered further), every `target*` link resolved to one. An x.py
    /// checkout's `build/` is never entered (the bootstrap rule's alone).
    fn marked_walk(&mut self, dir: &Path, depth: usize, dev: u64, origin: &Origin) {
        let xpy = is_xpy(dir);
        for (name, path, ty) in self.children(dir) {
            if self.spent() {
                return;
            }
            let n = name.to_string_lossy();
            if n.starts_with('.') || (xpy && n == "build") || HOME_SKIP.contains(&n.as_ref()) {
                continue;
            }
            if ty.is_symlink() {
                // The ONE link followed: a checkout's `target -> target.noindex`.
                if n.starts_with("target")
                    && !self.leads_into_mount(&path)
                    && let Ok(real) = std::fs::canonicalize(&path)
                    && std::fs::metadata(&real).is_ok_and(|m| m.is_dir())
                    && marked(&real)
                {
                    self.add_via(&real, origin, &path);
                }
                continue;
            }
            if !ty.is_dir() || dev_nofollow(&path) != Some(dev) {
                continue;
            }
            if marked(&path) {
                self.add(&path, origin);
            } else if depth > 1 {
                self.marked_walk(&path, depth - 1, dev, origin);
            }
        }
    }

    /// x.py's cargo build directories under `<repo>/build/<host>/`.
    fn bootstrap(&mut self, repo: &Path) {
        let build = repo.join("build");
        if !std::fs::symlink_metadata(&build).is_ok_and(|m| m.is_dir()) {
            return;
        }
        let origin = Origin::Bootstrap {
            repo: repo.to_path_buf(),
            slim: std::fs::symlink_metadata(repo.join("scripts").join("slim_build.py"))
                .is_ok_and(|m| m.is_file()),
        };
        for (_, host, ty) in self.children(&build) {
            // `build/host -> <triple>` is a link: never followed.
            if !ty.is_dir() {
                continue;
            }
            for (name, dir, ty) in self.children(&host) {
                if ty.is_dir()
                    && bootstrap_cargo_dir(&name.to_string_lossy())
                    && cachedir_tag_signed(&dir.join("CACHEDIR.TAG"))
                {
                    self.add(&dir, &origin);
                }
            }
        }
    }

    /// Each Claude Code session's `scratchpad/` under `tmp`
    /// (`<tmp>/<project>/<session>/scratchpad`) — only when `tmp` is this
    /// user's private directory ([`private_dir`]).
    fn claude_scratch(&mut self, tmp: &Path) {
        if std::fs::symlink_metadata(tmp).is_err() {
            return;
        }
        if let Err(why) = private_dir(tmp) {
            self.notes.push(format!(
                "cargo-targets: Claude Code's temp root {} is not searched: {why}",
                tmp.display()
            ));
            return;
        }
        if !self.on_device(tmp) {
            return;
        }
        let Ok(tmp) = std::fs::canonicalize(tmp) else {
            return;
        };
        for (project, pdir, ty) in self.children(&tmp) {
            if !ty.is_dir() || project.to_string_lossy().starts_with('.') {
                continue;
            }
            for (session, sdir, ty) in self.children(&pdir) {
                let session = session.to_string_lossy().into_owned();
                if !ty.is_dir() || !super::super::upgrade::is_session_id(&session) {
                    continue;
                }
                let scratch = sdir.join("scratchpad");
                let Some(dev) = dev_nofollow(&scratch) else {
                    continue;
                };
                if !std::fs::symlink_metadata(&scratch).is_ok_and(|m| m.is_dir()) {
                    continue;
                }
                let Ok(session_dir) = std::fs::canonicalize(&sdir) else {
                    continue;
                };
                let origin = Origin::ClaudeScratch {
                    session,
                    session_dir,
                };
                self.marked_walk(&scratch, SCRATCH_DEPTH, dev, &origin);
            }
        }
    }

    /// The Rust build cache under `$HOME` (`atpkg::build_cache`): every
    /// marked `.cargo-build` under the mirror of its workspace's path.
    fn build_cache(&mut self, cache: &Path) {
        if !std::fs::symlink_metadata(cache).is_ok_and(|m| m.is_dir()) || !self.on_device(cache) {
            return;
        }
        let mut stack = vec![(cache.to_path_buf(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            for (name, child, ty) in self.children(&dir) {
                if !ty.is_dir() {
                    continue;
                }
                if name == atpkg::build_cache::LEAF {
                    if marked(&child)
                        && let Ok(rel) = dir.strip_prefix(cache)
                        && let Ok(mirror) = std::fs::canonicalize(&dir)
                    {
                        let origin = Origin::BuildCache {
                            workspace: Path::new("/").join(rel),
                            mirror,
                        };
                        self.add(&child, &origin);
                    }
                } else if depth < 64 {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Owners
// ---------------------------------------------------------------------------

/// What a removal of a gone owner's build directory checks again first
/// ([`still_gone`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// A Claude Code session.
    ClaudeSession {
        /// Its id.
        session: String,
        /// `<claude tmp>/<project>/<session>`.
        session_dir: PathBuf,
        /// The Claude Code directories it was judged by ([`Roots::claude_dirs`]):
        /// the ones holding its transcript are its registry and date it.
        claude_dirs: Vec<PathBuf>,
        /// `$HOME`, whose guarded folders no Claude Code directory is read in
        /// when the removal reads the running processes again
        /// ([`still_gone`]).
        home: Option<PathBuf>,
    },
    /// The workspace a Rust build cache's build directory serves
    /// ([`Origin::BuildCache`]).
    BuildCache {
        /// The workspace.
        workspace: PathBuf,
        /// The mirror folder under the cache root the build directory lies
        /// inside.
        mirror: PathBuf,
        /// `$HOME`, whose guarded folders are never looked into
        /// ([`checkout::gone`]).
        home: Option<PathBuf>,
    },
}

impl Owner {
    /// The owner's own folder, which its build directory must lie INSIDE:
    /// the session's temp directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        match self {
            Owner::ClaudeSession { session_dir, .. } => session_dir,
            Owner::BuildCache { mirror, .. } => mirror,
        }
    }

    /// How long nothing of a gone owner's build directory may have been
    /// written before it goes whole, given the configured quiet window
    /// `quiet_s` ([`super::Config::quiet_s`], at least a day): a session's
    /// that window; a REMOVED WORKSPACE's build cache only
    /// [`super::PRESSURE_MIN_IDLE_S`] — its workspace is SEEN gone, nobody
    /// builds there again, and a day of waiting is a day of a full disk
    /// (`atpkg::build_cache::RECLAIM_GONE` is that day for the sweep above the
    /// floor). A build that recreated the worktree since writes a manifest,
    /// which the recheck under the locks reads ([`still_gone`]), and a process
    /// inside it keeps it ([`unused`]).
    #[must_use]
    pub fn quiet_s(&self, quiet_s: i64) -> i64 {
        match self {
            Owner::BuildCache { .. } => super::PRESSURE_MIN_IDLE_S,
            Owner::ClaudeSession { .. } => quiet_s,
        }
    }

    /// Who it was, for the journal: the session id and its folder.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Owner::ClaudeSession {
                session,
                session_dir,
                ..
            } => format!(
                "Claude Code session {session}, its folder {}",
                session_dir.display()
            ),
            Owner::BuildCache {
                workspace, mirror, ..
            } => format!(
                "the Rust build cache of workspace {}, its folder {}",
                workspace.display(),
                mirror.display()
            ),
        }
    }
}

/// Whether a build directory's owner is gone, as measured.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Owned {
    /// It has no owner that can be gone: a checkout's, an agent's, x.py's.
    #[default]
    NotOwned,
    /// Its owner runs, or could not be ruled out: why.
    Live(String),
    /// Its owner is gone: the evidence a person reads, and what the removal
    /// checks again.
    Gone {
        /// The evidence.
        evidence: String,
        /// The owner.
        owner: Owner,
    },
}

/// The owners of one survey's build directories, each session and each
/// registry read once.
pub struct Owners {
    home: Option<PathBuf>,
    claude_dirs: Vec<PathBuf>,
    claudes: Vec<RunningClaude>,
    unread: Option<String>,
    now: i64,
    quiet_s: i64,
    /// Each Claude Code directory's registry, as read ([`live_records`]).
    registries: BTreeMap<PathBuf, Registry>,
    sessions: BTreeMap<PathBuf, Owned>,
}

impl Owners {
    /// Owners judged at `now`, quiet for `quiet_s` seconds: a session's by
    /// the running Claude Codes `claudes` ([`Roots::claudes`]) and the Claude
    /// Code directories `claude_dirs` ([`Roots::claude_dirs`]) — none ended
    /// while `unread` says why a process that may be a Claude Code could not
    /// be read ([`Roots::claude_unread`]); `home`'s guarded folders are never
    /// read for a Claude Code directory.
    #[must_use]
    pub fn new(
        home: Option<PathBuf>,
        claude_dirs: Vec<PathBuf>,
        claudes: Vec<RunningClaude>,
        unread: Option<String>,
        now: i64,
        quiet_s: i64,
    ) -> Owners {
        Owners {
            home,
            claude_dirs,
            claudes,
            unread,
            now,
            quiet_s,
            registries: BTreeMap::new(),
            sessions: BTreeMap::new(),
        }
    }

    /// Who owns a build directory found as `origin`.
    pub fn of(&mut self, origin: &Origin) -> Owned {
        match origin {
            Origin::Named
            | Origin::Checkout { .. }
            | Origin::AgentTargetDir
            | Origin::Bootstrap { .. }
            | Origin::Linked { .. } => Owned::NotOwned,
            Origin::BuildCache { workspace, mirror } => {
                build_cache_owner(workspace, mirror, self.home.as_deref())
            }
            Origin::ClaudeScratch {
                session,
                session_dir,
            } => {
                if let Some(o) = self.sessions.get(session_dir) {
                    return o.clone();
                }
                if let Some(why) = &self.unread {
                    return unread(why);
                }
                let owner = Owner::ClaudeSession {
                    session: session.clone(),
                    session_dir: session_dir.clone(),
                    claude_dirs: self.claude_dirs.clone(),
                    home: self.home.clone(),
                };
                let registries = &mut self.registries;
                let o = session_owner(
                    &owner,
                    &self.claudes,
                    &mut |dir| {
                        registries
                            .entry(dir.to_path_buf())
                            .or_insert_with(|| live_records(dir))
                            .clone()
                    },
                    self.now,
                    self.quiet_s,
                );
                self.sessions.insert(session_dir.clone(), o.clone());
                o
            }
        }
    }
}

/// A session not called ended because a process that may be a Claude Code
/// could not be read ([`claude_census`]): what keeps every session, said as
/// short as a line that names the session's path beside it allows.
fn unread(why: &str) -> Owned {
    Owned::Live(format!("no session is called ended: {why}"))
}

/// One registry record naming a running process ([`live_records`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    /// The process.
    pid: u32,
    /// The session it holds now (Claude Code rewrites the record when the
    /// session changes: measured, `/clear`, Claude Code 2.1.287).
    session: String,
    /// Its `procStart`, spaces squashed ([`RunningClaude::started`]).
    started: String,
}

/// One registry's records of running processes ([`live_records`]); `None`:
/// not read whole.
type Registry = Option<Vec<Record>>;

/// The records of the registry of the Claude Code directory `claude_dir`
/// that name a running process (`sessions/<pid>.json`, its pid alive), or
/// `None` when it cannot be read whole — then no session it could name is
/// gone, and no process it could name is pinned.
fn live_records(claude_dir: &Path) -> Registry {
    let files = super::super::upgrade_drive::session_files_in(claude_dir)?;
    Some(
        files
            .into_iter()
            .filter(|f| super::super::upgrade_drive::alive(f.pid))
            .map(|f| Record {
                pid: f.pid,
                started: super::super::upgrade_drive::squash(&f.proc_start),
                session: f.session_id,
            })
            .collect(),
    )
}

/// A Rust build cache's owner: gone only when its workspace is SEEN to be
/// no longer one ([`checkout::gone`], the rule `atpkg::build_cache::reclaim`
/// reads too).
fn build_cache_owner(workspace: &Path, mirror: &Path, home: Option<&Path>) -> Owned {
    match self::checkout::gone(workspace, home) {
        Ok(seen) => Owned::Gone {
            evidence: format!(
                "the Rust build cache of {}, a workspace that is gone: {seen}",
                workspace.display()
            ),
            owner: Owner::BuildCache {
                workspace: workspace.to_path_buf(),
                mirror: mirror.to_path_buf(),
                home: home.map(Path::to_path_buf),
            },
        },
        Err(why) => Owned::Live(why),
    }
}

/// A Claude Code session's owner verdict, each Claude Code directory's
/// registry read through `registry` (`None`: not readable whole). A running
/// process any known registry names for it keeps it; so does a running
/// Claude Code (`claudes`) that may hold a session of its project
/// ([`RunningClaude::may_hold`]) unless a known registry pins it to its own
/// — its pid at its kernel start. Only a directory that holds the session's
/// transcript can then say it ended: its registry must be read whole, and
/// the transcript, the transcript's directory (its subagents') and the
/// session's temp directory must all be unwritten for the quiet window. No
/// known directory holding one, it is never called ended.
fn session_owner(
    owner: &Owner,
    claudes: &[RunningClaude],
    registry: &mut dyn FnMut(&Path) -> Registry,
    now: i64,
    quiet_s: i64,
) -> Owned {
    let Owner::ClaudeSession {
        session,
        session_dir,
        claude_dirs,
        ..
    } = owner
    else {
        return Owned::Live("not a session".to_owned());
    };
    let Some(project) = session_dir.parent().and_then(Path::file_name) else {
        return Owned::Live(format!(
            "Claude Code session {} has no project folder",
            short(session)
        ));
    };
    // Each known directory's registry (its records of running processes,
    // `None`: not read whole) and the session's transcript there, if any.
    let known: Vec<(&Path, Registry, Option<PathBuf>)> = claude_dirs
        .iter()
        .map(|dir| {
            let t = dir
                .join("projects")
                .join(project)
                .join(format!("{session}.jsonl"));
            let held = std::fs::symlink_metadata(&t).is_ok_and(|m| m.is_file());
            (dir.as_path(), registry(dir), held.then_some(t))
        })
        .collect();
    let records = || known.iter().filter_map(|(_, r, _)| r.as_ref()).flatten();
    // A running process ANY known directory's registry names keeps it, even
    // where that directory holds no transcript of it (one pruned, or a copy
    // of it in another directory): what `~/.claude`'s registry kept before.
    if records().any(|r| r.session == *session) {
        return Owned::Live(format!("Claude Code session {} is running", short(session)));
    }
    // A running Claude Code no known registry pins to its session — its
    // record moved where no registry is read (a settings `env` block moves
    // its directory after its exec), or it keeps none (`claude -p`) — may
    // hold any session of the project it works in: it keeps them all.
    let pinned = |c: &RunningClaude| {
        c.started
            .as_deref()
            .is_some_and(|started| records().any(|r| r.pid == c.pid && r.started == started))
    };
    if let Some(c) = claudes.iter().find(|c| c.may_hold(project) && !pinned(c)) {
        return Owned::Live(format!(
            "Claude Code process {} runs in {}, and no registry read here names its session",
            c.pid,
            c.place()
        ));
    }
    if known.iter().all(|(_, _, t)| t.is_none()) {
        return Owned::Live(format!(
            "none of the {} Claude Code directories read here holds the transcript of session {}",
            claude_dirs.len(),
            short(session)
        ));
    }
    let mut newest = None;
    let mut dated: Vec<PathBuf> = vec![session_dir.clone()];
    for (dir, records, transcript) in &known {
        let Some(transcript) = transcript else {
            continue;
        };
        if records.is_none() {
            return Owned::Live(format!(
                "Claude Code's session registry in {} could not be read whole",
                dir.display()
            ));
        }
        newest = newest.max(mtime_of(transcript));
        // The session's transcript directory (its subagents' transcripts).
        dated.push(transcript.with_extension(""));
    }
    for dir in &dated {
        if std::fs::symlink_metadata(dir).is_err() {
            continue;
        }
        match newest_write(dir, MAX_SESSION_ENTRIES) {
            Ok(m) => newest = newest.max(m),
            Err(why) => return Owned::Live(why),
        }
    }
    let Some(newest) = newest else {
        return Owned::Live(format!(
            "nothing of Claude Code session {} could be dated",
            short(session)
        ));
    };
    let age = now.saturating_sub(newest);
    if age < quiet_s {
        return Owned::Live(format!(
            "Claude Code session {} wrote {} ago",
            short(session),
            human_age(age)
        ));
    }
    Owned::Gone {
        evidence: format!(
            "Claude Code session {} ended: no running Claude Code may hold it, and nothing of it was \
             written for {}",
            short(session),
            human_age(age)
        ),
        owner: owner.clone(),
    }
}

/// The running processes' census of Claude Codes as a removal reads it
/// again ([`still_gone`]), given `$HOME`: the product's is
/// [`running_claude_census`], a test's the processes it names.
pub type Census<'a> = &'a dyn Fn(Option<&Path>) -> ClaudeCensus;

/// Check, right before a removal, that `owner` is still gone at `now` (the
/// survey's verdict, read again). A session's is judged by the Claude Codes
/// running NOW (`census`) and by the survey's Claude Code directories with
/// theirs: a Claude Code started since may have resumed it.
///
/// # Errors
///
/// Why it no longer is.
pub fn still_gone(owner: &Owner, now: i64, quiet_s: i64, census: Census<'_>) -> Result<(), String> {
    let verdict = match owner {
        Owner::BuildCache {
            workspace,
            mirror,
            home,
        } => build_cache_owner(workspace, mirror, home.as_deref()),
        Owner::ClaudeSession {
            session,
            session_dir,
            claude_dirs,
            home,
        } => match census(home.as_deref()) {
            Err(why) => unread(&why),
            Ok(running) => {
                let mut dirs = claude_dirs.clone();
                for dir in running.dirs {
                    if !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
                let now_known = Owner::ClaudeSession {
                    session: session.clone(),
                    session_dir: session_dir.clone(),
                    claude_dirs: dirs,
                    home: home.clone(),
                };
                session_owner(
                    &now_known,
                    &running.claudes,
                    &mut live_records,
                    now,
                    quiet_s,
                )
            }
        },
    };
    match verdict {
        Owned::Gone { .. } => Ok(()),
        Owned::Live(why) => Err(why),
        Owned::NotOwned => Err("it has no owner".to_owned()),
    }
}

/// Is the build directory at `path` (canonical: the removal reaches it with
/// no link in any component) INSIDE `owner`'s folder? An owner's verdict
/// never reaches a directory outside it.
///
/// # Errors
///
/// Why it is not.
pub fn owns(owner: &Owner, path: &Path) -> Result<(), String> {
    let dir = std::fs::canonicalize(owner.dir()).map_err(|e| {
        format!(
            "its owner's folder {} could not be resolved: {e}",
            owner.dir().display()
        )
    })?;
    if path.starts_with(&dir) && path != dir {
        Ok(())
    } else {
        Err(format!(
            "it is not inside its owner's folder {}",
            dir.display()
        ))
    }
}

/// Is an unreadable process one of the platform's own services — an
/// executable on the system's own tree, which builds nothing and runs no
/// test? Measured 2026-10-01: macOS refuses a same-uid reader the working
/// directory or descriptors of `lsd`, `UserEventAgent`, `routined` and
/// other agents under `/usr/libexec` and `/System`, always; counting them
/// would keep every directory forever.
fn platform_service(exe: Option<&Path>) -> bool {
    exe.is_some_and(|p| {
        ["/System/", "/usr/libexec/", "/usr/sbin/", "/sbin/"]
            .iter()
            .any(|root| p.starts_with(root))
    })
}

/// The running processes [`unused`] judges: given the directories (each
/// canonical) and the pids to leave out, which processes use a path under
/// one of them and which could not be read. The product's is every process
/// of this uid ([`aterm_uds::holders::scan_path_users`], what
/// [`super::remover`] asks); a test names its own
/// ([`aterm_uds::holders::scan_path_users_of`]) or writes the scan, so its
/// verdict never turns on what else the machine runs.
pub type Processes<'a> =
    &'a dyn Fn(&[PathBuf], &[u32]) -> std::io::Result<aterm_uds::holders::PathScan>;

/// Is any running process USING the build directory at `path` or its
/// owner's folder — its working directory, its executable or an open file
/// there, as `processes` reads them ([`Processes`])? Cargo runs tests without
/// its locks and a binary run from `deps/` holds none, so only this can say
/// that nothing runs from a gone owner's build directory. This process is
/// left out (its own reclaim holds the lock files). A process that cannot be
/// read, unless it is a platform service ([`platform_service`]), and a
/// listing that fails both keep the directory.
///
/// # Errors
///
/// Who uses it, or why that could not be ruled out.
pub fn unused(owner: &Owner, path: &Path, processes: Processes<'_>) -> Result<(), String> {
    let mut dirs = vec![path.to_path_buf()];
    if let Ok(dir) = std::fs::canonicalize(owner.dir()) {
        dirs.push(dir);
    }
    let scan = processes(&dirs, &[std::process::id()])
        .map_err(|e| format!("the running processes could not be listed: {e}"))?;
    if let Some(u) = scan.users.first() {
        return Err(format!(
            "process {} uses {} (its {})",
            u.pid,
            u.path.display(),
            match u.how {
                "cwd" => "working directory",
                "exe" => "executable",
                _ => "open file",
            }
        ));
    }
    if let Some((pid, exe)) = scan
        .unreadable
        .iter()
        .find(|(_, exe)| !platform_service(exe.as_deref()))
    {
        return Err(format!(
            "process {pid}{} could not be read, so it is not ruled out",
            exe.as_ref()
                .map(|e| format!(" ({})", e.display()))
                .unwrap_or_default()
        ));
    }
    Ok(())
}

/// What a removal of a gone owner's build directory checks under cargo's
/// locks, right before it deletes: the directory is inside the owner's
/// folder ([`owns`]) and the owner is still gone ([`still_gone`], a session's
/// by the running processes `census` reads now). That no running process
/// uses it ([`unused`]) is asked just BEFORE the reclaim opens the
/// directory: a child this process forks while the reclaim holds
/// its handles carries copies of them until it execs, and would read as a
/// user of the very directory being reclaimed.
///
/// # Errors
///
/// Why the directory no longer goes.
pub fn recheck(
    owner: &Owner,
    path: &Path,
    now: i64,
    quiet_s: i64,
    census: Census<'_>,
) -> Result<(), String> {
    owns(owner, path)?;
    still_gone(owner, now, quiet_s, census)
}

/// The first eight characters of a session id, for a line.
fn short(session: &str) -> &str {
    session.get(..8).unwrap_or(session)
}

/// The newest write under `dir`: the mtime of every regular file but
/// cargo's lock files, and of each cargo profile's [`CLOCK_DIRS`] (the clock
/// the reclaim reads) —
/// never a directory's own (an unlink, the reclaim's included, writes it),
/// never through a link, never onto another device, and never inside a
/// profile's caches. `Ok(None)`: nothing to date.
///
/// # Errors
///
/// A directory could not be read, or the tree holds more than `max`
/// entries: a tree that cannot be dated is never called quiet.
pub fn newest_write(dir: &Path, max: usize) -> Result<Option<i64>, String> {
    let dev = dev_nofollow(dir);
    let mut newest: Option<i64> = None;
    let mut seen = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd =
            std::fs::read_dir(&d).map_err(|e| format!("{} could not be read: {e}", d.display()))?;
        let entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
        let profile = entries
            .iter()
            .any(|e| CARGO_LOCKS.iter().any(|l| e.file_name() == *l));
        for e in entries {
            seen += 1;
            if seen > max {
                return Err(format!(
                    "{} holds more than {max} entries, too many to date",
                    dir.display()
                ));
            }
            let Ok(ty) = e.file_type() else {
                continue;
            };
            let name = e.file_name();
            if ty.is_dir() {
                if profile && CACHE_DIRS.iter().any(|c| name == *c) {
                    if CLOCK_DIRS.iter().any(|c| name == *c) {
                        newest = newest.max(mtime_of(&e.path()));
                    }
                    continue;
                }
                if dev_nofollow(&e.path()) == dev {
                    stack.push(e.path());
                }
            } else if ty.is_file() && !CARGO_LOCKS.iter().any(|l| name == *l) {
                // A lock file is never written (only created, by cargo or by
                // a reclaim taking its locks): it dates nothing.
                newest = newest.max(mtime_of(&e.path()));
            }
        }
    }
    Ok(newest)
}

#[path = "disk_discover_tests.rs"]
#[cfg(all(test, unix))]
mod tests;
