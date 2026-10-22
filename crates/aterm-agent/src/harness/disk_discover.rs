// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Finding the build directories: every cargo build directory the disk watch can reach from
//! where the agents work, and who owns each. Every path is injected ([`Roots`]); nothing here
//! reads the environment.
//!
//! # Where it looks
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
//! macOS) a privacy-guarded folder of `$HOME` ([`HOME_SKIP`]; on Windows
//! `AppData` and every `OneDrive*`, and anywhere a folder whose attributes
//! say a look would fetch it from the cloud, or a reparse point that is no
//! link: [`fetches_or_reparses`], read with cloud placeholders exposed
//! ([`expose_placeholders`]) since a process is shown one inside a sync root
//! as a plain folder; and never lists or reads a marker in any directory
//! that lies in a cloud sync root, nor records one — a root, a working
//! directory inside OneDrive's Documents, an agent's `CARGO_TARGET_DIR`
//! there, where a `target*` link leads, a sync root a listing meets:
//! [`in_sync_root`], each one said once in the notes), never enters
//! or even `stat`s a mount point ([`Roots::mounts`]: a stalled network
//! mount answers by blocking), never reports or walks a directory off the
//! measured device ([`Roots::device`]; on Windows off its volume's GUID
//! either, [`Roots::volume`]), never enters an x.py checkout's
//! `build/` but by the bootstrap rule, and stops at a build directory it
//! found (its profiles are the profile walk's, `super::target`). Two names
//! for one directory (a link and its target, a cwd that is also `$HOME`'s
//! child; on Windows a verbatim, a plain and a differently cased spelling)
//! are one, by device and inode (the one identity export,
//! [`aterm_sysprobe::volumes::file_id`]), its origins MERGED
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
//!   `~/.claude`'s, managed accounts' and the directory each running process
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
//! An owner owns only what lies inside its folder: a `target*` link out of it makes what it
//! names [`Origin::Linked`], and a directory a checkout, an agent or the caller also names is
//! theirs.
//!
//! Whether a gone owner's build directory goes is the report's decision (`super::report`).
//! The removal first asks that no running process uses it or the owner's folder ([`unused`]),
//! then checks again under cargo's locks ([`recheck`]).

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
/// on macOS, the privacy-guarded folders; on Windows `AppData` and every
/// `OneDrive*` ([`checkout::HOME_SKIP`], matched by [`checkout::home_skip`]
/// blind to case as the file system is). A checkout there is still searched
/// from an agent working in it.
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
    /// The Claude Code directories whose registries and transcripts say whether a session
    /// ended ([`claude_dirs`]). Not walked; carried to the survey's [`Owners`].
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
    /// The device of the volume whose free space is measured ([`device_of`]:
    /// `st_dev`, on Windows the volume serial — one width,
    /// [`aterm_sysprobe::volumes::FileId::dev`]): a root, a directory or a
    /// build directory on any other is never entered or reported (removing it
    /// frees nothing there). `None`: any device (the hand-run verb's reach;
    /// the window's watch removes nothing then).
    pub device: Option<u64>,
    /// On Windows, that measured volume's own GUID
    /// ([`super::target::VolumeGuid::of_path`], as its free-space reading
    /// keys it): while [`Self::device`] fences, a root, a directory or a build
    /// directory must be on the volume of this GUID too — a cloned volume
    /// shares its source's serial, never its GUID — and with none read
    /// nothing is on the measured volume. Unread elsewhere.
    pub volume: Option<super::target::VolumeGuid>,
    /// The system's mount points ([`mount_points`]): never entered, never
    /// even `stat`ed — a stalled network mount answers a `stat` of its root
    /// by blocking, and a blocked look holds the disk watch until restart.
    pub mounts: Vec<PathBuf>,
    /// Leave `$HOME`'s children and their worktrees unwalked: a pass handed
    /// no session but an agent's demand reaches that agent's own checkout
    /// ([`Self::cwds`]) and aterm's build cache, not every checkout of `$HOME`.
    pub skip_home_children: bool,
}

/// Claude Code's per-user temp root for `uid`: `/private/tmp/claude-<uid>`
/// where `/private/tmp` exists (macOS), else `/tmp/claude-<uid>`. On Windows
/// `claude` under the user's own temp directory, which holds no uid
/// ([`windows_claude_tmp`]).
#[must_use]
pub fn claude_tmp_root(uid: u32) -> PathBuf {
    if cfg!(windows) {
        return windows_claude_tmp(|name| std::env::var_os(name))
            .unwrap_or_else(|| std::env::temp_dir().join("claude"));
    }
    let base = if Path::new("/private/tmp").is_dir() {
        "/private/tmp"
    } else {
        "/tmp"
    };
    PathBuf::from(format!("{base}/claude-{uid}"))
}

/// Claude Code's temp root on Windows, from the environment `var` reads:
/// `claude` under Node's `os.tmpdir()`, `%TEMP%`, else `%TMP%` (measured:
/// `%TEMP%\claude\<project>\<session>\scratchpad`). `None` where neither
/// names an absolute directory.
#[must_use]
pub fn windows_claude_tmp(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    ["TEMP", "TMP"]
        .into_iter()
        .filter_map(|name| var(name).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .next()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("claude"))
}

/// The Claude Code directories a survey reads sessions' ends from
/// ([`Roots::claude_dirs`]): `home`'s own (`~/.claude`, as
/// [`footer::claude_dir_of`] spells it) and every one in `known` — a
/// supervised agent's, an account's, the caller's, each resolved by its
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
    /// (`CLAUDE_CONFIG_DIR`, else `$HOME/.claude`, at exec), each once; every process's,
    /// since a renamed Claude Code keeps its registry there too. Read for reach, never
    /// trusted for safety: a settings `env` block can move it after exec
    /// ([`RunningClaude::may_hold`]).
    pub dirs: Vec<PathBuf>,
}

/// One running Claude Code ([`Running::claudes`]): what the kernel says of it, which no
/// settings file can move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningClaude {
    /// Its pid.
    pub pid: u32,
    /// When the kernel started it, as Claude Code writes `procStart` in its registry
    /// (`ps -o lstart=` in UTC and the C locale, spaces squashed): a record naming its pid
    /// pins it only at this start, since a pid may be reused. `None`: not read, never pinned.
    pub started: Option<String>,
    /// Its working directory. `None`: not read — it may hold a session of
    /// any project.
    pub cwd: Option<PathBuf>,
    /// The profile of the home it runs in — `.claude.json` beside or inside
    /// its Claude Code directory, by the one resolver of a home
    /// (`super::super::profile::path_of`, over its exec's `CLAUDE_CONFIG_DIR`
    /// and `HOME`): the account it is on, for the account stats' count of
    /// the sessions on a home. `None` for a process the kernel does not name
    /// a Claude Code (kept only because its directory was not read) and for a
    /// home that could not be resolved.
    pub profile: Option<PathBuf>,
    /// Its exec environment gives it no credential, provider, profile or
    /// route of its own ([`Exec::stored_login`]): the account stats' word on
    /// whether an API key, Bedrock or Vertex spends from its home.
    pub stored_login: bool,
}

/// Claude Code's cap on a project folder's name: past it the name is its first
/// [`PROJECT_NAME_CAP`] characters, a `-` and a hash of the path (Claude Code 2.1.287).
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

/// A working directory as Claude Code spells the one it names its project
/// after: on Windows a verbatim prefix dropped (`\\?\C:\x` is `C:\x`,
/// `\\?\UNC\srv\share\x` is `\\srv\share\x`) and a trailing separator past
/// a drive's root (the PEB keeps `C:\x\`; Node's `process.cwd()` says
/// `C:\x`); elsewhere as it is.
fn as_claude_spells(cwd: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if !cfg!(windows) {
        return Cow::Borrowed(cwd);
    }
    let cwd: Cow<'_, str> = match cwd.strip_prefix(r"\\?\") {
        None => Cow::Borrowed(cwd),
        Some(rest) => match rest.get(..4) {
            Some(unc) if unc.eq_ignore_ascii_case(r"UNC\") => {
                Cow::Owned(format!(r"\\{}", &rest[4..]))
            }
            _ => Cow::Borrowed(rest),
        },
    };
    let trimmed = cwd.trim_end_matches(['\\', '/']);
    // `C:\` keeps its separator: a root is never trimmed to `C:`.
    if trimmed.len() <= 2 || trimmed.len() == cwd.len() {
        cwd
    } else {
        Cow::Owned(trimmed.to_owned())
    }
}

/// The repository a Claude Code worktree working directory lies in
/// (`<repo>/.claude/worktrees/<name>`; on Windows either separator, any
/// case), if it lies in one.
fn worktree_repo(cwd: &str) -> Option<&str> {
    if !cfg!(windows) {
        return cwd.split_once("/.claude/worktrees/").map(|(repo, _)| repo);
    }
    let marker = r"\.claude\worktrees\";
    let at = cwd.as_bytes().windows(marker.len()).position(|w| {
        w.iter()
            .zip(marker.as_bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(b) || (*a == b'/' && *b == b'\\'))
    })?;
    cwd.get(..at)
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
    ///
    /// On Windows the kernel's working directory (the PEB's) keeps a
    /// trailing separator Claude Code's own (`process.cwd()`) drops, a
    /// worktree is `<repo>\.claude\worktrees\<name>`, and the file system is
    /// blind to case: there names compare blind to ASCII case, so a spelling
    /// in another case holds the project rather than missing it.
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
        let spelled = as_claude_spells(cwd);
        let cwd: &str = &spelled;
        // Two names alike: exactly, or on Windows blind to ASCII case.
        let same = |a: &str, b: &str| {
            if cfg!(windows) {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        };
        let begins = |s: &str, head: &str| s.get(..head.len()).is_some_and(|h| same(h, head));
        // `name` as Claude Code writes it: whole up to the cap, past it its
        // head, a `-` and a hash. (`name` is ASCII: a byte is a character.)
        let cap = PROJECT_NAME_CAP;
        let named = |name: &str| {
            if name.len() <= cap {
                same(project, name)
            } else {
                begins(project, &name[..cap]) && project.as_bytes().get(cap) == Some(&b'-')
            }
        };
        // A directory's own project, or one of a directory under it.
        let at_or_under = |name: &str| {
            if name.len() < cap {
                same(project, name) || begins(project, &format!("{name}-"))
            } else {
                begins(project, &name[..cap])
            }
        };
        named(&project_name(cwd))
            || worktree_repo(cwd).is_some_and(|repo| at_or_under(&project_name(repo)))
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
    /// What the kernel says of its executable, read after its exec record, so one that
    /// exited meanwhile reads [`Exe::Exited`]. `None`: not read.
    pub exe: Option<Exe>,
    /// Its exec record (`KERN_PROCARGS2` on macOS, `/proc` on Linux, its
    /// process parameters on Windows: [`atpkg::caller_shell::process_args`]),
    /// `None` when it could not be read.
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
    /// The variable its `os.homedir()` reads, in its environment at exec:
    /// `HOME`, on Windows `USERPROFILE` ([`super::super::accounts::HOMEDIR_ENV`]).
    pub home: Option<String>,
    /// Its environment at exec reads as its stored login: nothing in it gives
    /// it a credential, provider, profile or route of its own — the
    /// environment's half of the host's verdict
    /// ([`super::super::accounts::ProcessLogin::of_exec_env`]). `false` for
    /// an environment that reads empty, which was not read.
    pub stored_login: bool,
}

impl Exec {
    /// What the census keeps of the exec record `args` ([`Exec`]): its path,
    /// `argv[0]` and `argv[1]`, its two variables and the verdict of its
    /// environment on its login — the rest dropped here. `None` for a record naming neither a path nor an argument (the
    /// kernel's for a process that is no program's).
    fn of(args: atpkg::caller_shell::ProcArgs) -> Option<Self> {
        use super::super::accounts::{HOME_ENV, HOMEDIR_ENV};
        if args.exec_path.is_empty() && args.argv.is_empty() {
            return None;
        }
        Some(Self {
            config: args.env_var(HOME_ENV).map(str::to_owned),
            home: args.env_var(HOMEDIR_ENV).map(str::to_owned),
            stored_login: super::super::accounts::ProcessLogin::of_exec_env(&args.env)
                .is_some_and(|login| login.stored_login),
            path: args.exec_path,
            argv: args.argv.into_iter().take(2).collect(),
        })
    }
}

/// The names Claude Code's binary answers to as a tool, not a Claude Code (it dispatches on
/// `argv[0]`): its shell's `grep` is `ARGV0=ugrep <claude>`, its `find` `ARGV0=bfs <claude>`,
/// likewise `rg` and the Linux sandbox's `apply-seccomp`; orphaned ones can run for days in a
/// session's directory. A name not here reads as a Claude Code: one more kept project, never
/// a session lost.
const CLAUDE_TOOLS: &[&str] = &["ugrep", "bfs", "rg", "apply-seccomp"];

/// The extensions a Windows program file carries, which [`claude_code`]
/// reads a component without (`claude.exe` is `claude`), ASCII case aside.
const WINDOWS_PROGRAMS: &[&str] = &["exe", "cmd", "bat", "ps1"];

/// Is a process running `exe` (the kernel's, resolved), exec'd as `exec`, a
/// Claude Code? Its executable, exec path, `argv[0]` or `argv[1]` has a
/// path component `claude` or `claude-code` (ASCII case aside) — the atpkg
/// store's `…/store/claude/<build>/bin/claude`, the native installer's
/// `~/.local/share/claude/versions/<version>` (whose own name is the
/// version, reached by a link of any name), a cask's
/// `…/claude-code/<version>/claude`, npm's `node
/// …/@anthropic-ai/claude-code/cli.js`; never `~/.claude/…` nor the temp
/// root's `claude-<uid>` — and its `argv[0]` is not one of the binary's own
/// tools ([`CLAUDE_TOOLS`]). On Windows a component is also read without an
/// executable's extension ([`WINDOWS_PROGRAMS`]): the native installer runs
/// `~\.local\bin\claude.exe` (a copy, its versions' folder named nowhere on
/// the way), npm's shim is `claude.cmd`.
///
/// `scratch` is Claude Code's temp root on Windows ([`Scratch`]:
/// `%TEMP%\claude`, spelled long), `None` elsewhere. There a path that lies
/// in that root (the root itself included) names the folder the sessions'
/// scratchpads live in, never the program — as macOS's `claude-<uid>` names
/// no Claude Code — so a program run from a scratchpad
/// (`%TEMP%\claude\<project>\<session>\scratchpad\…\aterm.exe`, a test
/// binary, a build's tool) is no Claude Code: none of its `claude`
/// components counts, the root's own, one under it, and one `%TEMP%` itself
/// lies in (`…\claude\tmp\claude\…`: the temp path, not a program). Each
/// path is spelled long first ([`Scratch::long`]: a scratchpad's paths, and
/// every `argv[0]` started from one, keep `%TEMP%`'s 8.3 short names where
/// it has them), then placed by the one path rule
/// ([`aterm_sysprobe::paths::windows_under`]: verbatim or plain, any case,
/// `/` read as Win32 reads it). A path it cannot place against the root (an
/// 8.3 spelling nothing spells long, a relative `argv[0]`) still counts —
/// one more kept project, never a session lost.
fn claude_code(exe: Option<&Exe>, exec: &Exec, scratch: Option<&Scratch>) -> bool {
    let named = |s: &std::ffi::OsStr| {
        s.to_str().is_some_and(|s| {
            s.eq_ignore_ascii_case("claude") || s.eq_ignore_ascii_case("claude-code")
        })
    };
    // `claude.exe` on Windows: the name without a program's extension.
    let program = |s: &std::ffi::OsStr| {
        let p = Path::new(s);
        cfg!(windows)
            && p.extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|e| WINDOWS_PROGRAMS.iter().any(|x| x.eq_ignore_ascii_case(e)))
            && p.file_stem().is_some_and(named)
    };
    // Does this path (spelled long) lie in the temp root, or is it the root?
    let in_scratch = |path: &Path| scratch.is_some_and(|s| s.holds(path));
    let claude = |n: &String| {
        let path = scratch.map_or(std::borrow::Cow::Borrowed(Path::new(n)), |s| {
            s.long(Path::new(n))
        });
        // In the root: every `claude` on the way is the temp path's.
        if in_scratch(&path) {
            return false;
        }
        // Else by component: one through which the path is placed in the
        // root (the rest of it unplaceable) is the temp path's alike.
        let mut through = PathBuf::new();
        path.components().any(|c| {
            through.push(c);
            match c {
                std::path::Component::Normal(s) => {
                    (named(s) || program(s)) && !in_scratch(&through)
                }
                _ => false,
            }
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

/// Where the system's own executables live (macOS: its sealed volume): none is a Claude
/// Code. On macOS most of a uid's processes run from here and many cannot be read.
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
/// one of the system's own executables ([`SYSTEM_EXECUTABLES`]; on Windows
/// exactly one of the measured services that refuse every same-user reader,
/// [`platform_service`] — `ctfmon.exe` and `taskhostw.exe` in the system's
/// own `System32`, Defender's `DefenderSessionHelper.exe` in a version
/// folder of its platform, never a folder alone)? One it cannot tell may be
/// a Claude Code ([`claude_census`]).
fn told(exec: Option<&Exec>, exe: &Exe) -> bool {
    exec.is_some()
        || match exe {
            Exe::Exited => true,
            Exe::At(exe) if cfg!(windows) => platform_service(Some(exe)),
            Exe::At(exe) => SYSTEM_EXECUTABLES.iter().any(|root| exe.starts_with(root)),
            Exe::Unnamed => false,
        }
}

/// How many more times the census reads a process it cannot tell ([`settled`]) before it
/// refuses, pausing 1 ms before the first and twice as long before each next: 255 ms in all.
/// Most settle within a millisecond; 61 ms has been measured under load.
const EXEC_REREADS: u32 = 8;

/// A process's exec record and executable as `read` gives them, read again while the census
/// cannot tell it ([`told`]), at most [`EXEC_REREADS`] times with `pause` between. A process
/// caught mid-spawn has no exec record yet and the kernel still names its parent's
/// executable; the record appears once its exec completes. One still untold after the last
/// read is refused.
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
/// directory, where its managed accounts keep theirs
/// ([`in_aterm_state`]), is — and none is ever a reason to refuse: a process
/// whose directory is not read keeps its project, as an unpinned Claude Code
/// does ([`OwnDir::Unread`]). Refused,
/// so that no session is called ended ([`Roots::claude_unread`]), only when
/// a process may be a Claude Code that cannot be told ([`told`]): its exec
/// record could not be read — even read again while its spawn may be in
/// flight ([`settled`]) — and it is not one of the system's own
/// executables. One the kernel says EXITED runs nothing — but one whose
/// executable cannot be named (unlinked since its exec, or the read refused)
/// still runs, and is refused like any other not read. On Windows a program
/// run from Claude Code's temp root is no Claude Code by its path there
/// ([`claude_code`], against [`Scratch::read`]).
///
/// # Errors
///
/// A process that may be a Claude Code could not be told ([`told`]): no exec record even
/// after [`settled`], and not a system executable. Then no session is called ended
/// ([`Roots::claude_unread`]).
pub fn claude_census(
    processes: impl IntoIterator<Item = Process>,
    home: Option<&Path>,
) -> ClaudeCensus {
    census_of(processes, home, Scratch::read().as_ref())
}

/// Claude Code's temp root as the census places a program's path against
/// it ([`claude_code`]), on Windows: the root spelled long
/// ([`claude_scratch_root`]), and the rule that spells each path long
/// before it is placed. The kernel names a process's image long, but a
/// session's scratchpad paths keep `%TEMP%` as the environment spells it
/// (Node's `os.tmpdir()` answers it as set), and so does the `argv[0]` of
/// every program started from one: on a machine whose `%TEMP%` carries 8.3
/// short names (`C:\Users\ANDREW~1\…`, `C:/Users//ANDREW~1/…` from Git Bash)
/// those key nowhere until they are spelled long.
pub(crate) struct Scratch {
    /// The root, spelled long where it could be.
    root: PathBuf,
    /// The mount table a short-spelled path is placed on before the file
    /// system is asked to spell it long (a local, non-automounted volume
    /// only): read the first time one is met, once per census.
    table: std::cell::OnceCell<aterm_sysprobe::volumes::Table>,
    /// The long spelling of the part of a path through its last short name
    /// ([`aterm_sysprobe::volumes::long_name`]; a test's own stand-in).
    expand: fn(&Path) -> Option<PathBuf>,
}

impl Scratch {
    /// The census's own: Claude Code's temp root on Windows
    /// ([`claude_scratch_root`]), each path spelled long by the file
    /// system. `None` elsewhere.
    fn read() -> Option<Self> {
        claude_scratch_root().map(|root| Self {
            root,
            table: std::cell::OnceCell::new(),
            expand: aterm_sysprobe::volumes::long_name,
        })
    }

    /// `path` spelled long where it carries 8.3 short names and lies on a
    /// local, non-automounted volume
    /// ([`aterm_sysprobe::volumes::Table::long_spelling_by`]: lexical, no
    /// link followed, with the system's error boxes off); as it is
    /// otherwise, and where the file system cannot spell it (a scratchpad
    /// removed since), which places it nowhere.
    fn long<'p>(&self, path: &'p Path) -> std::borrow::Cow<'p, Path> {
        if aterm_sysprobe::paths::short_spelled(path).is_none() {
            return std::borrow::Cow::Borrowed(path);
        }
        let _quiet = aterm_sysprobe::quiet_errors();
        self.table
            .get_or_init(aterm_sysprobe::volumes::table)
            .long_spelling_by(path, self.expand)
    }

    /// Does `path`, spelled long ([`Self::long`]), lie in the root, or is it
    /// the root? By the one path rule; one it cannot place does not.
    fn holds(&self, path: &Path) -> bool {
        aterm_sysprobe::paths::windows_under(path, &self.root) == Some(true)
    }
}

/// Claude Code's temp root as [`claude_code`] places a path against it, on
/// Windows ([`Scratch`]): [`claude_tmp_root`] (`%TEMP%\claude`, the root the
/// search walks for the sessions' scratch builds), spelled long where
/// `%TEMP%` carries 8.3 short names (a long or spaced user name gets
/// `C:\Users\ANDREW~1\…`): [`aterm_sysprobe::volumes::Table::long_spelling`],
/// asked of a local, non-automounted volume only, with the system's error
/// boxes off. `None` elsewhere, where the root is `claude-<uid>`, which no
/// component rule reads as a Claude Code's name.
fn claude_scratch_root() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let root = claude_tmp_root(own_uid());
    if aterm_sysprobe::paths::short_spelled(&root).is_none() {
        return Some(root);
    }
    let _quiet = aterm_sysprobe::quiet_errors();
    Some(
        aterm_sysprobe::volumes::table()
            .long_spelling(&root)
            .into_owned(),
    )
}

/// [`claude_census`] with Claude Code's temp root `scratch` (Windows,
/// [`Scratch`]) already read.
fn census_of(
    processes: impl IntoIterator<Item = Process>,
    home: Option<&Path>,
    scratch: Option<&Scratch>,
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
        let claude = claude_code(exe.as_ref(), &exec, scratch);
        if claude || dir == OwnDir::Unread {
            let profile = home.filter(|_| claude).and_then(|user| {
                super::super::profile::path_of(
                    user,
                    exec.config.as_deref(),
                    exec.home.as_deref(),
                    || cwd.clone(),
                )
            });
            out.claudes.push(RunningClaude {
                pid,
                started,
                cwd,
                profile,
                stored_login: exec.stored_login,
            });
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
    /// Neither `CLAUDE_CONFIG_DIR` nor `HOME` (`USERPROFILE` on Windows)
    /// names one.
    Unset,
    /// Read: its registry and transcripts are judged.
    Read(PathBuf),
    /// Named but not read: not an absolute path, or where the watch never
    /// looks ([`checkout::never_looked_at`] in `home`) outside aterm's own
    /// state directory ([`in_aterm_state`]).
    Unread,
}

/// [`OwnDir`] of a process exec'd as `exec`, running in `cwd` (where read). A relative
/// `CLAUDE_CONFIG_DIR`, empty included, names a directory under `cwd`, as Claude Code
/// resolves it ([`super::super::footer::claude_dir_of`]).
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

/// Whether `dir` lies in aterm's own state directory under `home` (macOS:
/// `~/Library/Application Support/aterm`), where managed accounts keep their Claude
/// Code directories. No privacy consent guards it, so the census reads them. Lexical; a path
/// that climbs out by `..` is not.
fn in_aterm_state(dir: &Path, home: Option<&Path>) -> bool {
    let platform = aterm_types::dirs::StatePlatform {
        home: home.map(Path::to_path_buf),
        ..aterm_types::dirs::StatePlatform::default()
    };
    aterm_types::dirs::resolve_state_dir(None, platform).is_some_and(|state| {
        aterm_sysprobe::paths::is_under(dir, &state) == Some(true)
            && !dir
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
    })
}

/// [`claude_census`] of every process of this uid running now, each read by
/// [`read_process`], this process's own passed over ([`census_of_running`]).
/// Read below the floor only.
///
/// # Errors
///
/// As [`claude_census`]; or the processes could not be listed.
pub fn running_claude_census(home: Option<&Path>) -> ClaudeCensus {
    let pids = aterm_uds::holders::uid_pids()
        .map_err(|e| format!("the running processes could not be listed ({e})"))?;
    let scratch = Scratch::read();
    census_of_running(
        pids,
        |pid| read_process(pid, home, scratch.as_ref()),
        home,
        scratch.as_ref(),
    )
}

/// [`claude_census`] of the running processes `pids` names, each read by
/// `read`, Claude Code's temp root `scratch` already read — THIS PROCESS
/// PASSED OVER, unread, on every OS. It is aterm (the verb, or the window
/// whose look asks), never a Claude Code, whatever its path says: on
/// Windows one run from a Claude Code scratchpad has a `claude` component
/// in its image (`%TEMP%\claude\…`), and anywhere one started with a
/// `CLAUDE_CONFIG_DIR` the watch never reads would read as a Claude Code
/// that keeps its project. The Claude Code directory it was started with is
/// the caller's, which the survey reads anyway ([`claude_dirs`]).
pub(crate) fn census_of_running(
    pids: impl IntoIterator<Item = u32>,
    read: impl FnMut(u32) -> Option<Process>,
    home: Option<&Path>,
    scratch: Option<&Scratch>,
) -> ClaudeCensus {
    let me = std::process::id();
    census_of(
        pids.into_iter().filter(|&pid| pid != me).filter_map(read),
        home,
        scratch,
    )
}

/// The running process `pid` as [`claude_census`] reads it: its exec record
/// read ([`atpkg::caller_shell::process_args`]: `KERN_PROCARGS2` on macOS,
/// `/proc` on Linux, its process parameters on Windows; again while a spawn
/// is in flight, [`settled`]) and dropped at once but for what it was exec'd
/// as and its two variables ([`Exec::of`]) — never kept, never logged; the
/// working directory of a Claude Code, or of one whose own directory is not
/// read, and a Claude Code's start, from the kernel ([`process_cwd`]) — a
/// Claude Code by [`claude_code`]'s rule, Claude Code's temp root `scratch`
/// (Windows) as the census reads it. `None`: it exited meanwhile, and holds
/// nothing.
fn read_process(pid: u32, home: Option<&Path>, scratch: Option<&Scratch>) -> Option<Process> {
    let read = || {
        let exec = atpkg::caller_shell::process_args(pid).and_then(Exec::of);
        (exec, aterm_uds::holders::exe_of(pid))
    };
    let (exec, exe) = settled(read, std::thread::sleep);
    let (mut cwd, mut started) = (None, None);
    let claude = exec
        .as_ref()
        .is_some_and(|e| claude_code(Some(&exe), e, scratch));
    if claude
        || exec
            .as_ref()
            .is_some_and(|e| own_dir(e, None, home) == OwnDir::Unread)
    {
        cwd = process_cwd(pid);
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
}

/// The working directory of process `pid` as the kernel holds it
/// ([`aterm_sysprobe::process_cwd`]; on Windows its process parameters',
/// spelled without the trailing separator the PEB keeps, which
/// [`RunningClaude::may_hold`] reads past either way). `None` where it
/// cannot be read: that process may then hold any project.
fn process_cwd(pid: u32) -> Option<PathBuf> {
    aterm_sysprobe::process_cwd(pid)
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

/// What places a workspace nested in a removed checkout under `home`
/// ([`checkout::Placing`]): `home`'s device and every mount point of the
/// mount table read now — `None` when the table was cut or `home` cannot be
/// read, and then no such workspace is called gone.
#[must_use]
pub fn placing_now(home: &Path) -> Option<checkout::Placing> {
    placing_of(home, &aterm_sysprobe::volumes::table())
}

/// [`placing_now`] from the mount table `table` already read.
#[must_use]
pub fn placing_of(
    home: &Path,
    table: &aterm_sysprobe::volumes::Table,
) -> Option<checkout::Placing> {
    if !table.complete {
        return None;
    }
    Some(checkout::Placing {
        home_dev: dev_nofollow(home)?,
        mounts: table.points(),
    })
}

/// The device of the directory at `path`, a link followed (as `statvfs`
/// follows it): what [`Roots::device`] is set from.
#[must_use]
pub fn device_of(path: &Path) -> Option<u64> {
    identity(path).map(|(dev, _)| dev)
}

/// The system's mount points, read from the mount table without asking any
/// file system ([`aterm_sysprobe::volumes::table`]: `getfsstat(MNT_NOWAIT)` on
/// macOS, `/proc/self/mountinfo` on Linux): what [`Roots::mounts`] is set
/// from. A mount point written under `/System/Volumes/Data` (macOS's data
/// volume) is listed in its firmlinked form too
/// ([`aterm_sysprobe::volumes::Table::points`]). Empty where there is no
/// reader.
#[must_use]
pub fn mount_points() -> Vec<PathBuf> {
    aterm_sysprobe::volumes::table().points()
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
    /// Under a Claude Code session's `scratchpad/`, its canonical path inside the session's
    /// directory ([`Walk::add`]).
    ClaudeScratch {
        /// The session id (the directory's name).
        session: String,
        /// `<claude tmp>/<project>/<session>`, canonical: its transcript is
        /// `<claude dir>/projects/<project>/<session>.jsonl`, in whichever
        /// Claude Code directory holds it ([`Owners`]).
        session_dir: PathBuf,
    },
    /// Reached from an owner's folder by a `target*` link pointing out of it, or from two
    /// owners' at once: it belongs to whatever else names it, never to an owner that can end
    /// (a scratch worktree's `target -> ~/repo/target.noindex` is the checkout's).
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

    /// One build directory reached twice: the origin it keeps. A directory any checkout,
    /// agent or caller names is theirs, whichever walk met it first; two different owners
    /// make it [`Origin::Linked`].
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
    /// Its path, canonical.
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
    /// The walk stopped short of every build directory it would reach (its
    /// budget, a folder it could not read): why. A pass that ran on it did
    /// not run out of what it may take.
    pub cut: Option<String>,
}

/// Find every build directory reachable from `roots` within `budget`.
///
/// The roots whose build directories have NO owner that can end are walked
/// FIRST — the agents' `CARGO_TARGET_DIR`, their working directories,
/// `$HOME`'s checkouts — and Claude Code's temp root after: a build directory any of those names is theirs ([`Origin::merge`]),
/// and a walk cut short before they are done reaches no owner's at all (each
/// later walk reads nothing once the budget is spent), so an owner's build
/// directory is never one a checkout the walk missed might use.
///
/// The whole walk runs with this thread seeing cloud placeholders as the
/// reparse points they are (the walk holds [`expose_placeholders`]'s guard,
/// [`Walk::new`]): in a process's default mode one inside a sync root reads
/// as a plain folder, and every attribute skip here would pass it. A thread
/// that cannot be switched searches nothing. And no root is read into — no
/// listing, no marker — before it is known to lie in no sync root
/// ([`Walk::outside_sync_root`]): a look into a file or folder not yet
/// fetched fetches it.
#[must_use]
pub fn discover(roots: &Roots, budget: Budget) -> Discovered {
    let mut w = match Walk::new(roots, budget) {
        Ok(w) => w,
        Err(why) => {
            let why = format!("nothing was searched: {why}");
            return Discovered {
                found: Vec::new(),
                notes: vec![format!("cargo-targets: {why}")],
                cut: Some(why),
            };
        }
    };
    for dir in &roots.agent_target_dirs {
        if dir.is_absolute()
            && let Ok(real) = std::fs::canonicalize(dir)
            && !w.fenced(&real)
            && w.outside_sync_root(&real)
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
        if w.fenced(&cwd) {
            continue;
        }
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
        .filter(|_| !roots.skip_home_children)
        .and_then(|h| std::fs::canonicalize(h).ok())
        && w.on_device(&home)
        && w.outside_sync_root(&home)
    {
        for (name, child, ty) in w.children(&home) {
            let n = name.to_string_lossy();
            if !ty.is_dir() || n.starts_with('.') || home_skipped(&name) {
                continue;
            }
            if !w.same_device(&child) {
                continue;
            }
            w.checkout(&child);
            for wt in worktrees(&child, &[&home, roots.home.as_deref().unwrap_or(&home)]) {
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
    let cut = w.cut.clone();
    if let Some(why) = w.cut {
        notes.push(format!(
            "cargo-targets: the search for build directories stopped short ({why}); the {} found are surveyed",
            w.found.len()
        ));
    }
    Discovered {
        found: w.found,
        notes,
        cut,
    }
}

/// Does `dir` carry a build tool's marker: a signed `CACHEDIR.TAG`, or
/// cargo's `.rustc_info.json`? It reads the tag's bytes, so the walk asks
/// only of a directory it listed, or of one it first found in no sync root
/// ([`Walk::outside_sync_root`]).
fn marked(dir: &Path) -> bool {
    read_into(dir);
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

/// [`worktree_root`], for tier 3's checkout ([`super::idle::checkout_of`]).
#[must_use]
pub fn worktree_root_of(dir: &Path) -> Option<PathBuf> {
    worktree_root(dir)
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
/// `.git/worktrees/*/gitdir` (each names `<worktree>/.git`). A gitdir can
/// name any path, so one in a place never looked into
/// ([`checkout::never_looked_at`] in any spelling of `$HOME` in `homes`), or
/// one whose spelling walks up through `..`, is dropped before anything
/// there is looked at, not even `stat`ed: on macOS a look into a
/// privacy-guarded folder can put a consent dialog on the owner's screen.
fn worktrees(repo: &Path, homes: &[&Path]) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(repo.join(".git").join("worktrees")) else {
        return Vec::new();
    };
    let placed = |p: &Path| {
        p.is_absolute()
            && !p
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            && homes
                .iter()
                .all(|home| checkout::never_looked_at(p, Some(home)).is_none())
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .take(MAX_WORKTREES)
        .filter_map(|e| small_text(&e.path().join("gitdir")))
        .map(|t| PathBuf::from(t.trim()))
        .filter(|p| placed(p))
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

/// The device of `p`, a link NOT followed, through the one identity export
/// ([`aterm_sysprobe::volumes::file_id`]: `st_dev` on unix, on Windows the
/// 64-bit volume serial — the one width every device fence compares),
/// `None` where it cannot be read (a FAT volume keeps no file ids) — never a
/// constant, which would pass every device fence and let [`placing_of`]
/// accept a complete table with a home device of 0.
fn dev_nofollow(p: &Path) -> Option<u64> {
    aterm_sysprobe::volumes::file_id(p, false).map(|id| id.dev)
}

/// `(device, inode)` of the directory at `p`, a link followed: the one
/// identity export's ([`aterm_sysprobe::volumes::FileId`]: on unix `st_dev`
/// and `st_ino`, on Windows the volume serial and the 128-bit file id).
/// `None` where it cannot be read: such a directory is never recorded.
fn identity(p: &Path) -> Option<(u64, u128)> {
    aterm_sysprobe::volumes::file_id(p, true).map(|id| (id.dev, id.ino))
}

/// Is `path` strictly inside `dir` (under it, not it)? On Windows by the one
/// path rule ([`aterm_sysprobe::paths::is_under`]: any case, a verbatim or a
/// plain spelling), a path it cannot place NOT inside — an owner's verdict
/// never reaches a directory it cannot place in the owner's folder; by
/// component elsewhere.
fn strictly_inside(path: &Path, dir: &Path) -> bool {
    if cfg!(windows) {
        use aterm_sysprobe::paths::{is_under, windows_key};
        is_under(path, dir) == Some(true) && windows_key(path) != windows_key(dir)
    } else {
        path.starts_with(dir) && path != dir
    }
}

/// Is `name` (a directory's child, as listed) one of `$HOME`'s folders the
/// walk never enters ([`HOME_SKIP`])? On Windows blind to case as NTFS is,
/// `OneDrive*` matching every `OneDrive - <organization>`
/// ([`checkout::home_skip`]); exactly as listed elsewhere.
fn home_skipped(name: &std::ffi::OsStr) -> bool {
    if cfg!(windows) {
        checkout::home_skip(name).is_some()
    } else {
        HOME_SKIP.iter().any(|s| name == *s)
    }
}

/// Why the walk's listing passes over an entry ([`fetches_or_reparses`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(windows),
    expect(dead_code, reason = "only Windows passes over an entry it lists")
)]
enum PassedOver {
    /// A reparse point that is no link, and carries no bit that says a look
    /// fetches it: a cloud sync root or a placeholder of one (OneDrive's, or
    /// another sync client's), or another kind. One that is a directory is asked whether
    /// it lies in a sync root ([`in_sync_root`]: opened as itself, which
    /// fetches nothing) and, if so, said ([`Walk::children`]).
    Reparse,
    /// A look into it could make a cloud client fetch it (recalled on open
    /// or on data access, offline), whether or not it is also a reparse
    /// point — so it is never opened, not even to ask why — or its
    /// attributes could not be read.
    Fetches,
}

/// Is the directory entry `e` one a look into could make a cloud client
/// fetch, or that is a reparse point other than a link — never entered or
/// marked — and which? Windows reads its attributes from the directory's
/// own listing (the find data: what kind of entry, never how fresh it is):
/// recalled on open or on data access (a OneDrive or other cloud
/// placeholder), or offline, whatever else it carries — a reparse point
/// among them too, so nothing opens it even to ask whether it is a sync
/// root; else a reparse point std does not read as a link (a link is the
/// walk's [`std::fs::FileType::is_symlink`] arm). `None` — entered as any
/// other — always, elsewhere.
fn fetches_or_reparses(e: &std::fs::DirEntry, ty: std::fs::FileType) -> Option<PassedOver> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        // FILE_ATTRIBUTE_REPARSE_POINT, _OFFLINE, _RECALL_ON_OPEN,
        // _RECALL_ON_DATA_ACCESS.
        const REPARSE: u32 = 0x0000_0400;
        const FETCHES: u32 = 0x0000_1000 | 0x0004_0000 | 0x0040_0000;
        // Attributes not read: passed over, as one that fetches is.
        let Ok(attributes) = e.metadata().map(|m| m.file_attributes()) else {
            return Some(PassedOver::Fetches);
        };
        // A fetch bit first: one that carries it is never opened.
        if attributes & FETCHES != 0 {
            Some(PassedOver::Fetches)
        } else if attributes & REPARSE != 0 && !ty.is_symlink() {
            Some(PassedOver::Reparse)
        } else {
            None
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (e, ty);
        None
    }
}

/// What [`expose_placeholders`] holds: on Windows the thread's mode to put
/// back ([`aterm_dirfd::ExposedPlaceholders`]); nothing elsewhere.
#[cfg(windows)]
pub(crate) type Exposed = aterm_dirfd::ExposedPlaceholders;
/// What [`expose_placeholders`] holds: on Windows the thread's mode to put
/// back ([`aterm_dirfd::ExposedPlaceholders`]); nothing elsewhere.
#[cfg(not(windows))]
pub(crate) struct Exposed;

/// While the returned guard lives, this thread's reads through `std` see a
/// cloud placeholder as the reparse point it is — on Windows, where a
/// process is shown a placeholder inside a sync root (OneDrive's Documents
/// and Desktop) as a plain file or folder by default, so an attribute skip
/// ([`fetches_or_reparses`]) would pass it. `aterm-dirfd`'s own calls switch
/// it themselves; this is for the disk code's `std` reads. Guards nest, and
/// the thread's own mode is back when the last drops. Nothing elsewhere.
///
/// # Errors
/// The thread could not be switched (the system refuses only an invalid
/// mode or a thread with no TEB): the caller judges nothing a placeholder
/// might be.
pub(crate) fn expose_placeholders() -> Result<Exposed, String> {
    #[cfg(windows)]
    {
        aterm_dirfd::expose_placeholders().map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        Ok(Exposed)
    }
}

/// Does the canonical directory `p` lie in a cloud sync root — the root
/// itself, or a placeholder folder under one (OneDrive's Documents or
/// Desktop, a working directory inside them)? Then nothing in it is a
/// candidate: each file a delete there removes is removed from the cloud
/// and from every synced device, and a look into a folder not yet fetched
/// fetches it. Read as the reclaim reads it (`aterm_dirfd::pin`, every
/// component opened as itself with placeholders exposed): a component that
/// is a reparse point other than a link refuses the pin — any such, a
/// virtualization root's too, which the reclaim would refuse alike. Any
/// other answer (a share, a component gone) leaves the walk's own rules to
/// judge. Never, elsewhere. Asked through [`Walk::outside_sync_root`].
fn in_sync_root(p: &Path) -> bool {
    #[cfg(windows)]
    {
        aterm_dirfd::pin(p).err() == Some(aterm_dirfd::Errno::FOREIGN_REPARSE)
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        false
    }
}

#[cfg(all(test, windows))]
thread_local! {
    /// [`record_reads`].
    static READS: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

/// A test's witness of what the walk reads INTO: on this thread, from
/// `record_reads(true)` until `record_reads(false)` (which answers them, in
/// order), every directory the walk lists ([`Walk::children`]) or reads a
/// build tool's marker in ([`marked`]) — what would make a cloud client
/// fetch a folder or a file not yet fetched.
#[cfg(all(test, windows))]
fn record_reads(on: bool) -> Vec<PathBuf> {
    READS
        .with(|r| std::mem::replace(&mut *r.borrow_mut(), on.then(Vec::new)))
        .unwrap_or_default()
}

/// Note `dir`, about to be read into, for [`record_reads`] (a test's, on
/// Windows; nothing otherwise).
fn read_into(dir: &Path) {
    #[cfg(all(test, windows))]
    READS.with(|r| {
        if let Some(reads) = r.borrow_mut().as_mut() {
            reads.push(dir.to_path_buf());
        }
    });
    #[cfg(not(all(test, windows)))]
    let _ = dir;
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
    /// Each build directory found, by `(device, inode)` ([`identity`]): its
    /// index in `found`.
    seen: BTreeMap<(u64, u128), usize>,
    found: Vec<Found>,
    /// [`Roots::device`].
    device: Option<u64>,
    /// Canonical [`Roots::home`], for the App Data and file-provider fence
    /// ([`Self::fenced`]).
    home: Option<PathBuf>,
    /// [`Roots::volume`] (read on Windows only).
    #[cfg(windows)]
    volume: Option<super::target::VolumeGuid>,
    /// The mount table each directory's volume GUID is read from
    /// ([`Self::on_volume`]), read once while [`Self::device`] fences.
    #[cfg(windows)]
    table: Option<aterm_sysprobe::volumes::Table>,
    /// [`Roots::mounts`].
    mounts: BTreeSet<PathBuf>,
    /// The mount points a `target*` link is never followed into
    /// ([`Self::leads_into_mount`]).
    foreign: Vec<PathBuf>,
    /// What a root could not be searched for, said once each.
    notes: Vec<String>,
    /// This thread sees cloud placeholders as reparse points while the walk
    /// lives ([`expose_placeholders`]): every listing ([`Self::children`])
    /// and attribute skip ([`fetches_or_reparses`]) reads them so. Last, so
    /// it drops after everything else the walk holds.
    _exposed: Exposed,
}

impl Walk {
    /// A walk of `roots` within `budget`, its thread switched to seeing cloud
    /// placeholders as the reparse points they are until it drops.
    ///
    /// # Errors
    /// The thread could not be switched ([`expose_placeholders`]): nothing
    /// may be searched, since every attribute skip would pass a placeholder.
    fn new(roots: &Roots, budget: Budget) -> Result<Walk, String> {
        let exposed = expose_placeholders()?;
        Ok(Walk {
            started: Instant::now(),
            budget,
            entries: 0,
            cut: None,
            seen: BTreeMap::new(),
            found: Vec::new(),
            device: roots.device,
            home: roots
                .home
                .as_deref()
                .map(|h| std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf())),
            #[cfg(windows)]
            volume: roots.volume,
            // Read once, on Windows only while a device fences: where each
            // directory's volume GUID is looked up ([`Walk::on_volume`]).
            #[cfg(windows)]
            table: roots.device.map(|_| aterm_sysprobe::volumes::table()),
            mounts: roots.mounts.iter().cloned().collect(),
            // The mounts a `target*` link must not lead into: every one but
            // the root and those `$HOME` lives on (on Windows placed by the
            // one path rule: one it cannot place `$HOME` on stays foreign).
            foreign: roots
                .mounts
                .iter()
                .filter(|m| m.as_path() != Path::new("/"))
                .filter(|m| {
                    !roots
                        .home
                        .as_deref()
                        .is_some_and(|h| aterm_sysprobe::paths::is_under(h, m) == Some(true))
                })
                .cloned()
                .collect(),
            notes: Vec::new(),
            _exposed: exposed,
        })
    }

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

    /// Is the directory at `p` (a link followed) on the measured device — on
    /// Windows by its volume's GUID too ([`Self::on_volume`])?
    fn on_device(&self, p: &Path) -> bool {
        self.device
            .is_none_or(|d| identity(p).is_some_and(|(dev, _)| dev == d) && self.on_volume(p, true))
    }

    /// While [`Self::device`] fences, is the directory at `p` on the volume
    /// of [`Self::volume`]'s GUID, by its mount in the table read for this
    /// walk — `p` resolved first when `follow` (a link followed, a `subst`
    /// letter placed), else read as spelled (a child as a canonical
    /// directory lists it, never a link)? A cloned volume shares its
    /// source's serial, so the serial alone passes it. With no GUID or no
    /// table read, a path that does not resolve or a mount with no GUID,
    /// nothing is. Always, elsewhere (the device is the whole fence).
    fn on_volume(&self, p: &Path, follow: bool) -> bool {
        #[cfg(windows)]
        {
            use super::target::VolumeGuid;
            let resolved;
            let p = if follow {
                let Ok(real) = std::fs::canonicalize(p) else {
                    return false;
                };
                resolved = real;
                resolved.as_path()
            } else {
                p
            };
            let here = self
                .table
                .as_ref()
                .and_then(|t| aterm_sysprobe::volumes::mount_of(p, t))
                .and_then(aterm_sysprobe::volumes::Mount::key)
                .and_then(|k| VolumeGuid::of_key(&k));
            self.volume.is_some() && here == self.volume
        }
        #[cfg(not(windows))]
        {
            let _ = (p, follow);
            true
        }
    }

    /// Does the link at `link` name a path under a mount point other than
    /// the root and `$HOME`'s ([`Roots::mounts`])? Read as written, before
    /// anything resolves it: resolving would `stat` that mount's root, and a
    /// stalled one answers by blocking. An unreadable link is treated as one.
    ///
    /// On Windows the link's text (a junction's `\\?\C:\…`, a symbolic
    /// link's as written) and the mount points (`D:\`, `C:\mnt\data`, the
    /// network's `\\`) are compared by the one path key, lexically as on
    /// unix: a text the key cannot place (relative past `..`, an 8.3 name, a
    /// volume GUID path) or a mount point it cannot key is treated as one.
    fn leads_into_mount(&self, link: &Path) -> bool {
        let Ok(to) = std::fs::read_link(link) else {
            return true;
        };
        let abs = link.parent().map_or_else(|| to.clone(), |p| p.join(&to));
        if cfg!(windows) {
            use aterm_sysprobe::paths::{key_under, windows_key};
            let Some(key) = windows_key(&abs) else {
                return true;
            };
            return self.foreign.iter().any(|m| {
                if m.as_os_str() == aterm_sysprobe::volumes::NETWORK {
                    key.starts_with(r"\\")
                } else {
                    windows_key(m).is_none_or(|m| key_under(&key, &m))
                }
            });
        }
        self.foreign.iter().any(|m| abs.starts_with(m))
    }

    /// Is `p` (a child as a directory lists it) one of [`Roots::mounts`]? On
    /// Windows by the one path key (the table lists `C:\mnt\data`, a walk
    /// meets `\\?\C:\mnt\data` in any case), a child it cannot key counted
    /// as one: never entered.
    fn is_mount(&self, p: &Path) -> bool {
        if cfg!(windows) {
            let Some(key) = aterm_sysprobe::paths::windows_key(p) else {
                return true;
            };
            return self
                .mounts
                .iter()
                .any(|m| aterm_sysprobe::paths::windows_key(m).is_some_and(|m| m == key));
        }
        self.mounts.contains(p)
    }

    /// Is the entry at `p` (a link NOT followed) on the measured device — on
    /// Windows by its volume's GUID too ([`Self::on_volume`])?
    fn same_device(&self, p: &Path) -> bool {
        self.device
            .is_none_or(|d| dev_nofollow(p) == Some(d) && self.on_volume(p, false))
    }

    /// The children of `dir`, sorted, each with its type as the directory
    /// lists it (a link is a link, never followed) — a mount point left out
    /// before anything asks its file system ([`Self::is_mount`]), and so is
    /// (on Windows) an entry a look could make a cloud client fetch, or a
    /// reparse point that is no link ([`fetches_or_reparses`]).
    ///
    /// A folder left out as a reparse point that is no link is asked whether
    /// it lies in a cloud sync root ([`in_sync_root`]: opened as itself from
    /// its volume's root, which fetches nothing) and, when it does, said once
    /// ([`Self::sync_root`]) — the sync root a `$HOME` search meets (a
    /// sync client's folder, a `Synced` folder, one a checkout holds), never a folder
    /// under it, which the walk never lists. Not one whose name the walk
    /// passes over anyway: a hidden folder, or one of `$HOME`'s it never
    /// enters ([`HOME_SKIP`]: the owner's own `OneDrive*` is skipped by name,
    /// silently, as `AppData` is).
    fn children(&mut self, dir: &Path) -> Vec<(OsString, PathBuf, std::fs::FileType)> {
        if self.spent() {
            return Vec::new();
        }
        read_into(dir);
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut reparse_folders = Vec::new();
        let mut out: Vec<(OsString, PathBuf, std::fs::FileType)> = rd
            .take(MAX_CHILDREN)
            .flatten()
            .filter(|e| !self.is_mount(&e.path()))
            .filter_map(|e| {
                let ty = e.file_type().ok()?;
                match fetches_or_reparses(&e, ty) {
                    None => Some((e.file_name(), e.path(), ty)),
                    Some(PassedOver::Reparse)
                        if ty.is_dir()
                            && !e.file_name().to_string_lossy().starts_with('.')
                            && !home_skipped(&e.file_name()) =>
                    {
                        reparse_folders.push(e.path());
                        None
                    }
                    Some(_) => None,
                }
            })
            .collect();
        for folder in reparse_folders {
            if in_sync_root(&folder) {
                self.sync_root(&folder);
            }
        }
        self.entries = self.entries.saturating_add(out.len());
        super::beat::tick(out.len());
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Record the build directory at `dir` once by device and inode, on the measured device
    /// only. An owner's origin holds only inside the owner's folder; outside it (reached by a
    /// link) it is [`Origin::Linked`]. Met again, the origins merge ([`Origin::merge`]).
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
        if self
            .device
            .is_some_and(|d| d != id.0 || !self.on_volume(&path, false))
        {
            return;
        }
        if !self.outside_sync_root(&path) {
            return;
        }
        let origin = match origin.owner_dir() {
            Some(owner) if !strictly_inside(&path, owner) => Origin::Linked {
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

    /// Whether `p` lies at or under a root macOS guards with
    /// `kTCCServiceSystemPolicyAppData` (`~/Library/Containers`,
    /// `~/Library/Group Containers`) or a file-provider domain
    /// (`~/Library/CloudStorage`, `~/Library/Mobile Documents`), so no
    /// checkout root, `CARGO_TARGET_DIR`, worktree, `target*` symlink or
    /// `marked_walk` child ever opens or lists inside another app's container.
    fn fenced(&self, p: &Path) -> bool {
        self.home.as_deref().is_some_and(|home| {
            atpkg::protected::under_app_data(home, p)
                || atpkg::protected::under_file_provider_domain(home, p)
        })
    }

    /// A checkout root: x.py's bootstrap trees, then the marker walk —
    /// never a root on another device, never one inside another app's data or
    /// a file-provider domain, and never one inside an x.py tree's `build/`
    /// (the bootstrap rule's alone).
    fn checkout(&mut self, root: &Path) {
        let Ok(root) = std::fs::canonicalize(root) else {
            return;
        };
        let root = root.as_path();
        if self.fenced(root) || !self.on_device(root) || inside_xpy_build(root) {
            return;
        }
        // Never walked inside a sync root: nothing there is a candidate, and
        // a look into a folder not yet fetched fetches it.
        if !self.outside_sync_root(root) {
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
            if n.starts_with('.')
                || (xpy && n == "build")
                || home_skipped(&name)
                || self.fenced(&path)
            {
                continue;
            }
            if ty.is_symlink() {
                // The ONE link followed: a checkout's `target -> target.noindex`
                // — its marker never read where it leads into a sync root or
                // another app's container.
                if n.starts_with("target")
                    && !self.leads_into_mount(&path)
                    && let Ok(real) = std::fs::canonicalize(&path)
                    && !self.fenced(&real)
                    && std::fs::metadata(&real).is_ok_and(|m| m.is_dir())
                    && self.outside_sync_root(&real)
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

    /// Say once that `dir`, in a cloud sync root ([`in_sync_root`]), is
    /// neither searched nor offered — in the words every refusal of such a
    /// place uses ([`super::target::IN_SYNC_ROOT`]).
    fn sync_root(&mut self, dir: &Path) {
        let note = format!(
            "cargo-targets: {} {}",
            dir.display(),
            super::target::IN_SYNC_ROOT
        );
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    /// May the walk read into the directory at `dir` — list it, read a
    /// marker in it, record it? Not when it lies in a cloud sync root
    /// ([`in_sync_root`], said once: [`Self::sync_root`]). Asked of every
    /// directory the walk did not reach by a listing of one already asked:
    /// each root (an agent's `CARGO_TARGET_DIR`, a checkout, `$HOME`, the
    /// build cache, Claude Code's temp root), where a `target*` link leads,
    /// and each build directory recorded. A listing's entries need no asking:
    /// a placeholder or a sync root among them reads as a reparse point that
    /// is no link while the walk lives ([`Self::_exposed`]), and the listing
    /// leaves it out ([`fetches_or_reparses`]). On Windows `dir` is resolved
    /// first, as every read of it would resolve it (a link on the way into a
    /// sync root is seen through); one that does not resolve is not read
    /// into either. Always, elsewhere.
    fn outside_sync_root(&mut self, dir: &Path) -> bool {
        let resolved;
        let asked = if cfg!(windows) {
            let Ok(real) = std::fs::canonicalize(dir) else {
                return false;
            };
            resolved = real;
            resolved.as_path()
        } else {
            dir
        };
        if in_sync_root(asked) {
            self.sync_root(dir);
            return false;
        }
        true
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
        if !self.outside_sync_root(&tmp) {
            return;
        }
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
    /// marked `.cargo-build` under the mirror of its workspace's path, the
    /// workspace read back as `/` joined to the mirror's path below the
    /// cache — the inverse of the writer's join on macOS, the one platform
    /// anything writes the cache on. On Windows nothing does (a workspace's
    /// `C:` could not be a component below it), and a folder found there
    /// anyway names `\<rel>`, no absolute path: its owner is never seen gone
    /// (`checkout::gone_placed` refuses it), so it is kept.
    fn build_cache(&mut self, cache: &Path) {
        if !std::fs::symlink_metadata(cache).is_ok_and(|m| m.is_dir()) {
            return;
        }
        let Ok(real_cache) = std::fs::canonicalize(cache) else {
            return;
        };
        let cache = real_cache.as_path();
        if self.fenced(cache) || !self.on_device(cache) || !self.outside_sync_root(cache) {
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
        /// The Claude Code directories it was judged by ([`Roots::claude_dirs`]).
        claude_dirs: Vec<PathBuf>,
        /// `$HOME`, for [`still_gone`]'s census: no Claude Code directory in its guarded
        /// folders is read.
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
    /// `quiet_s` ([`super::Config::quiet_s`], at least a day) and whether the
    /// volume is `short` (below the floor): a session's that window; a
    /// REMOVED WORKSPACE's build cache, below the floor, only
    /// [`super::PRESSURE_MIN_IDLE_S`] — its workspace is SEEN gone, nobody
    /// builds there again, and a day of waiting is a day of a full disk — and
    /// while the volume is only low, atpkg's own above-pressure window
    /// (`atpkg::build_cache::RECLAIM_GONE`, a day). A build that recreated the
    /// worktree since writes a manifest, which the recheck under the locks
    /// reads ([`still_gone`]), and a process inside it keeps it ([`unused`]).
    #[must_use]
    pub fn quiet_s(&self, quiet_s: i64, short: bool) -> i64 {
        match self {
            Owner::BuildCache { .. } if short => super::PRESSURE_MIN_IDLE_S,
            Owner::BuildCache { .. } => {
                i64::try_from(atpkg::build_cache::RECLAIM_GONE.as_secs()).unwrap_or(i64::MAX)
            }
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
    /// What places a workspace nested in a removed checkout
    /// ([`checkout::gone_placed`], [`Self::placed`]); `None`: such a workspace
    /// is away, not removed, and keeps its cache.
    placing: Option<checkout::Placing>,
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
            placing: None,
        }
    }

    /// These owners, a removed workspace judged with `placing` (the home
    /// volume's device and a complete mount table, [`placing_now`]): one nested
    /// in a removed checkout can be seen gone ([`checkout::gone_placed`]).
    #[must_use]
    pub fn placed(mut self, placing: Option<checkout::Placing>) -> Owners {
        self.placing = placing;
        self
    }

    /// Who owns a build directory found as `origin`.
    pub fn of(&mut self, origin: &Origin) -> Owned {
        match origin {
            Origin::Named
            | Origin::Checkout { .. }
            | Origin::AgentTargetDir
            | Origin::Bootstrap { .. }
            | Origin::Linked { .. } => Owned::NotOwned,
            Origin::BuildCache { workspace, mirror } => build_cache_owner(
                workspace,
                mirror,
                self.home.as_deref(),
                self.placing.as_ref(),
            ),
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

/// A session kept because a process that may be a Claude Code could not be read
/// ([`claude_census`]).
fn unread(why: &str) -> Owned {
    Owned::Live(format!("no session is called ended: {why}"))
}

/// One registry record naming a running process ([`live_records`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    /// The process.
    pid: u32,
    /// The session it holds now (Claude Code rewrites the record when the session changes,
    /// as on `/clear`).
    session: String,
    /// Its `procStart`, spaces squashed ([`RunningClaude::started`]).
    started: String,
}

/// One registry's records of running processes ([`live_records`]); `None`:
/// not read whole.
type Registry = Option<Vec<Record>>;

/// The records in `claude_dir`'s registry (`sessions/<pid>.json`) whose pid is alive, or
/// `None` when it cannot be read whole: then no session it could name is gone and no process
/// it could name is pinned.
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
/// no longer one ([`checkout::gone_placed`], the rule
/// `atpkg::build_cache::reclaim_watched` reads too), with `placing` a
/// workspace nested in a removed checkout among them.
fn build_cache_owner(
    workspace: &Path,
    mirror: &Path,
    home: Option<&Path>,
    placing: Option<&checkout::Placing>,
) -> Owned {
    match self::checkout::gone_placed(workspace, home, placing) {
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

/// A Claude Code session's owner verdict, each directory's registry read through `registry`
/// (`None`: not read whole). A running process any known registry names for it keeps it, as
/// does a running Claude Code that may hold a session of its project
/// ([`RunningClaude::may_hold`]) unless a registry pins it (its pid at its kernel start).
/// Only a directory holding the session's transcript can say it ended: its registry read
/// whole, and the transcript, its directory (subagents') and the session's temp directory all
/// unwritten for the quiet window. With no such directory it is never called ended.
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
    // Each known directory's registry and the session's transcript there, if any.
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
    // A running process any known registry names for it keeps it, even where that
    // directory holds no transcript of it.
    if records().any(|r| r.session == *session) {
        return Owned::Live(format!("Claude Code session {} is running", short(session)));
    }
    // A running Claude Code no registry pins to its session (its registry moved where none
    // is read, or it keeps none: `claude -p`) may hold any session of its project.
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

/// The Claude Code census a removal reads again ([`still_gone`]), given `$HOME`: the
/// product's is [`running_claude_census`], a test's the processes it names.
pub type Census<'a> = &'a dyn Fn(Option<&Path>) -> ClaudeCensus;

/// Check, right before a removal, that `owner` is still gone at `now`. A session is judged
/// again by the Claude Codes running now (`census`) and their directories added to the
/// survey's, since one started since may have resumed it.
///
/// # Errors
///
/// Why it no longer is.
pub fn still_gone(owner: &Owner, now: i64, quiet_s: i64, census: Census<'_>) -> Result<(), String> {
    let verdict = match owner {
        // Placed again from a mount table read now: a volume mounted since
        // the survey keeps the workspace "away".
        Owner::BuildCache {
            workspace,
            mirror,
            home,
        } => build_cache_owner(
            workspace,
            mirror,
            home.as_deref(),
            home.as_deref().and_then(placing_now).as_ref(),
        ),
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

/// Whether the build directory at `path` (canonical) lies inside `owner`'s folder: an owner's
/// verdict never reaches a directory outside it.
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
    if strictly_inside(path, &dir) {
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
///
/// Windows (measured 2026-10-19: of 89 same-user processes, these three and
/// no other refused a reader, always): ONLY an image that is exactly
/// `%SystemRoot%\System32\ctfmon.exe` (the text-input host),
/// `%SystemRoot%\System32\taskhostw.exe` (the task host), or
/// `DefenderSessionHelper.exe` directly in a version folder of
/// `%ProgramData%\Microsoft\Windows Defender\Platform\` — never a directory
/// alone, which would pass any program the system's tree holds
/// ([`windows_service`]); the folders are the system's own readings
/// (`GetSystemDirectoryW`, `SHGetKnownFolderPath`), never the environment's.
/// An unreadable process whose image is not named stays held.
pub(crate) fn platform_service(exe: Option<&Path>) -> bool {
    #[cfg(windows)]
    {
        use std::sync::OnceLock;
        static SYSTEM: OnceLock<Option<PathBuf>> = OnceLock::new();
        static PROGRAM_DATA: OnceLock<Option<PathBuf>> = OnceLock::new();
        exe.is_some_and(|p| {
            windows_service(
                p,
                SYSTEM.get_or_init(super::win::system_directory).as_deref(),
                PROGRAM_DATA
                    .get_or_init(super::win::program_data)
                    .as_deref(),
            )
        })
    }
    #[cfg(not(windows))]
    {
        exe.is_some_and(|p| {
            ["/System/", "/usr/libexec/", "/usr/sbin/", "/sbin/"]
                .iter()
                .any(|root| p.starts_with(root))
        })
    }
}

/// [`platform_service`]'s Windows rule over `exe`, an unreadable process's
/// image, given the system directory `system` (`…\System32`) and the
/// program data folder `program_data`: `exe` is exactly `ctfmon.exe` or
/// `taskhostw.exe` directly in `system`, or `DefenderSessionHelper.exe`
/// directly in a version folder (digits, `.` and `-`) directly in
/// `program_data\Microsoft\Windows Defender\Platform`. Compared by the one
/// path key ([`aterm_sysprobe::paths::windows_key`]: any case, a verbatim
/// or a plain spelling); a path either side cannot key, or a folder not
/// read, is no service.
#[must_use]
pub fn windows_service(exe: &Path, system: Option<&Path>, program_data: Option<&Path>) -> bool {
    use aterm_sysprobe::paths::windows_key;
    let Some(image) = windows_key(exe) else {
        return false;
    };
    let keyed = |dir: &Path, name: &str| windows_key(&dir.join(name));
    if let Some(system) = system
        && ["ctfmon.exe", "taskhostw.exe"]
            .iter()
            .any(|name| keyed(system, name).is_some_and(|k| k == image))
    {
        return true;
    }
    let Some(platform) = program_data.and_then(|d| {
        windows_key(
            &d.join("Microsoft")
                .join("Windows Defender")
                .join("Platform"),
        )
    }) else {
        return false;
    };
    let Some(version) = image
        .strip_prefix(platform.as_str())
        .and_then(|rest| rest.strip_prefix('\\'))
        .and_then(|rest| rest.strip_suffix(r"\DEFENDERSESSIONHELPER.EXE"))
    else {
        return false;
    };
    !version.is_empty()
        && version
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// The process scan [`unused`] judges by: given canonical directories and pids to leave out,
/// which processes use a path under one and which could not be read. The product's is
/// [`aterm_uds::holders::scan_path_users`]; a test names its own processes
/// ([`aterm_uds::holders::scan_path_users_of`]) so its verdict never turns on the machine.
pub type Processes<'a> =
    &'a dyn Fn(&[PathBuf], &[u32]) -> std::io::Result<aterm_uds::holders::PathScan>;

/// Whether any running process uses the build directory at `path` or its owner's folder (its
/// working directory, executable or an open file there). Cargo runs tests without its locks
/// and a binary run from `deps/` holds none, so only this says nothing runs from a gone
/// owner's build directory. This process is left out (its reclaim holds the lock files). An
/// unreadable process that is not a [`platform_service`], or a failed listing, keeps the
/// directory.
///
/// # Errors
///
/// Who uses it, or why that could not be ruled out.
pub fn unused(owner: &Owner, path: &Path, processes: Processes<'_>) -> Result<(), String> {
    let mut dirs = vec![path.to_path_buf()];
    if let Ok(dir) = std::fs::canonicalize(owner.dir()) {
        dirs.push(dir);
    }
    unused_under(&dirs, processes)
}

/// Is any running process USING a path under one of `dirs` (each canonical)
/// — its working directory, its executable or an open file there, as
/// `processes` reads them ([`unused`]'s rule, for any set of directories: a
/// checkout and its build directory for tier 3)?
///
/// # Errors
///
/// Who uses one, or why that could not be ruled out.
pub fn unused_under(dirs: &[PathBuf], processes: Processes<'_>) -> Result<(), String> {
    scan_says_unused(&processes(dirs, &[std::process::id()]), |_| true)
}

/// [`unused_under`]'s verdict on a `scan` already read, counting only the
/// uses `counts` keeps (one scan over several directories, judged for each):
/// a counted use, a process that could not be read (unless a platform
/// service) or a scan that failed says why it is not unused.
///
/// # Errors
///
/// Who uses it, or why that could not be ruled out.
pub fn scan_says_unused(
    scan: &std::io::Result<aterm_uds::holders::PathScan>,
    counts: impl Fn(&aterm_uds::holders::PathUse) -> bool,
) -> Result<(), String> {
    let scan = scan
        .as_ref()
        .map_err(|e| format!("the running processes could not be listed: {e}"))?;
    if let Some(u) = scan.users.iter().find(|u| counts(u)) {
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

/// THE PRODUCT'S [`Processes`]: every process of this uid
/// ([`aterm_uds::holders::scan_path_users`]) — and, where the scan passes
/// over this process (`skip` names it: its descriptors are the reclaim's own
/// handles and lock files), this process's own EXECUTABLE and WORKING
/// DIRECTORY, which no skip covers: an aterm run from a checkout's
/// `target/release` is that build directory's user, and every tab's `aterm`
/// and the re-exec after an update resolve to it.
///
/// # Errors
///
/// As [`aterm_uds::holders::scan_path_users`].
pub fn scan_processes(
    dirs: &[PathBuf],
    skip: &[u32],
) -> std::io::Result<aterm_uds::holders::PathScan> {
    let mut scan = aterm_uds::holders::scan_path_users(dirs, skip)?;
    let me = std::process::id();
    if skip.contains(&me) {
        let own = [
            ("exe", std::env::current_exe()),
            ("cwd", std::env::current_dir()),
        ];
        if let Some((how, path)) = own_use(&own, dirs)? {
            scan.users
                .push(aterm_uds::holders::PathUse { pid: me, how, path });
        }
    }
    Ok(scan)
}

/// The first of `own` (this process's executable and working directory,
/// each resolved) at or under one of `dirs`. A failed reading or resolution
/// leaves this process unknown and refuses the scan, never positive absence.
fn own_use(
    own: &[(&'static str, std::io::Result<PathBuf>)],
    dirs: &[PathBuf],
) -> std::io::Result<Option<(&'static str, PathBuf)>> {
    let table = cfg!(windows).then(aterm_sysprobe::volumes::table);
    for (how, path) in own {
        let path = path.as_ref().map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("own {how} could not be read: {error}"),
            )
        })?;
        let path = std::fs::canonicalize(path).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "own {how} {} could not be resolved: {error}",
                    path.display()
                ),
            )
        })?;
        // One it cannot place against a directory (on Windows the one path
        // rule over the mount table read now answers `None`) is a use.
        if dirs.iter().any(|d| {
            table.as_ref().map_or_else(
                || aterm_sysprobe::paths::is_under(&path, d),
                |t| t.is_under(&path, d),
            ) != Some(false)
        }) {
            return Ok(Some((*how, path)));
        }
    }
    Ok(None)
}

/// What a removal of a gone owner's build directory checks under cargo's locks, right before
/// it deletes: the directory is inside the owner's folder ([`owns`]) and the owner is still
/// gone ([`still_gone`]). [`unused`] is asked earlier, before the reclaim opens the directory:
/// a child forked while the reclaim holds its handles carries copies until it execs, and
/// would read as a user of the directory.
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

/// The newest write under `dir`: the mtime of every regular file but cargo's lock files, and
/// of each profile's [`CLOCK_DIRS`] (the reclaim's clock); never a directory's own (an unlink
/// writes it), never through a link or onto another device, never inside a profile's caches.
/// `Ok(None)`: nothing to date.
///
/// # Errors
///
/// A directory could not be read, or the tree holds more than `max` entries: a tree that
/// cannot be dated is never called quiet.
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
            super::beat::tick(1);
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

// Windows: spellings, junctions, attributes, the measured platform services.
#[path = "disk_discover_windows_tests.rs"]
#[cfg(all(test, windows))]
mod windows_tests;
