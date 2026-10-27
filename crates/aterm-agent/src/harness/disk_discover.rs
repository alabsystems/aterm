// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cargo build-directory discovery and process/session ownership checks for automatic disk reclamation.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::target::{CACHE_DIRS, CARGO_LOCKS, CLOCK_DIRS, human_age};
use super::{MAX_WALK_ENTRIES, RUSTC_INFO, cachedir_tag_signed, mtime_of};

pub const CHECKOUT_DEPTH: usize = 2;

pub const SCRATCH_DEPTH: usize = 4;

pub const MAX_FOUND: usize = 128;

const MAX_CHILDREN: usize = 4096;

const MAX_WORKTREES: usize = 64;

pub const MAX_SESSION_ENTRIES: usize = 100_000;

const MAX_ANCESTORS: usize = 32;

pub use aterm_toolchain::checkout;

pub use checkout::HOME_SKIP;

pub use aterm_uds::holders::Exe;

/// File-count and directory-depth bounds for build-directory discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub entries: usize,
    pub wall: Duration,
    pub found: usize,
}

impl Budget {
    pub const SHIPPED: Budget = Budget {
        entries: MAX_WALK_ENTRIES,
        wall: Duration::from_secs(20),
        found: MAX_FOUND,
    };
}

/// Injected filesystem roots and running-process snapshot searched by [`discover`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    pub cwds: Vec<PathBuf>,
    pub home: Option<PathBuf>,
    pub claude_dirs: Vec<PathBuf>,
    pub claudes: Vec<RunningClaude>,
    pub claude_unread: Option<String>,
    pub agent_target_dirs: Vec<PathBuf>,
    pub claude_tmp: Option<PathBuf>,
    pub device: Option<u64>,
    pub volume: Option<super::target::VolumeGuid>,
    pub mounts: Vec<PathBuf>,
    pub skip_home_children: bool,
}

/// Returns Claude Code's per-user temporary root (`/private/tmp/claude-<uid>` or `/tmp/claude-<uid>`).
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

/// Resolves Claude Code's temporary root on Windows from environment variables.
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

/// Returns the deduplicated list of `~/.claude` config directories to scan for session transcripts.
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

/// Result of scanning the process table for running Claude Code instances of this UID.
pub type ClaudeCensus = Result<Running, String>;

/// Running Claude Code instances and any unreadable PIDs for this UID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Running {
    pub claudes: Vec<RunningClaude>,
    pub dirs: Vec<PathBuf>,
}

/// Kernel-observed PID, working directory, start timestamp, and config directory of a running Claude Code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningClaude {
    pub pid: u32,
    pub started: Option<String>,
    pub cwd: Option<PathBuf>,
    pub profile: Option<PathBuf>,
    pub stored_login: bool,
}

const PROJECT_NAME_CAP: usize = 200;

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
    if trimmed.len() <= 2 || trimmed.len() == cwd.len() {
        cwd
    } else {
        Cow::Owned(trimmed.to_owned())
    }
}

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
    /// Returns whether this running Claude Code instance may own a session under `project`.
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
        let same = |a: &str, b: &str| {
            if cfg!(windows) {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        };
        let begins = |s: &str, head: &str| s.get(..head.len()).is_some_and(|h| same(h, head));
        let cap = PROJECT_NAME_CAP;
        let named = |name: &str| {
            if name.len() <= cap {
                same(project, name)
            } else {
                begins(project, &name[..cap]) && project.as_bytes().get(cap) == Some(&b'-')
            }
        };
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

    fn place(&self) -> String {
        self.cwd.as_deref().map_or_else(
            || "a folder that could not be read".to_owned(),
            |c| c.display().to_string(),
        )
    }
}

/// Kernel process snapshot inspected by [`claude_census`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub exe: Option<Exe>,
    pub exec: Option<Exec>,
    pub cwd: Option<PathBuf>,
    pub started: Option<String>,
}

/// Executable path and `argv[0..=1]` of a running process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exec {
    pub path: String,
    pub argv: Vec<String>,
    pub config: Option<String>,
    pub home: Option<String>,
    pub stored_login: bool,
}

impl Exec {
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

const CLAUDE_TOOLS: &[&str] = &["ugrep", "bfs", "rg", "apply-seccomp"];

const WINDOWS_PROGRAMS: &[&str] = &["exe", "cmd", "bat", "ps1"];

fn claude_code(exe: Option<&Exe>, exec: &Exec, scratch: Option<&Scratch>) -> bool {
    let named = |s: &std::ffi::OsStr| {
        s.to_str().is_some_and(|s| {
            s.eq_ignore_ascii_case("claude") || s.eq_ignore_ascii_case("claude-code")
        })
    };
    let program = |s: &std::ffi::OsStr| {
        let p = Path::new(s);
        cfg!(windows)
            && p.extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|e| WINDOWS_PROGRAMS.iter().any(|x| x.eq_ignore_ascii_case(e)))
            && p.file_stem().is_some_and(named)
    };
    let in_scratch = |path: &Path| scratch.is_some_and(|s| s.holds(path));
    let claude = |n: &String| {
        let path = scratch.map_or(std::borrow::Cow::Borrowed(Path::new(n)), |s| {
            s.long(Path::new(n))
        });
        if in_scratch(&path) {
            return false;
        }
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

const SYSTEM_EXECUTABLES: &[&str] = &[
    "/System",
    "/bin",
    "/sbin",
    "/usr/bin",
    "/usr/sbin",
    "/usr/libexec",
    "/usr/lib",
];

fn told(exec: Option<&Exec>, exe: &Exe) -> bool {
    exec.is_some()
        || match exe {
            Exe::Exited => true,
            Exe::At(exe) if cfg!(windows) => platform_service(Some(exe)),
            Exe::At(exe) => SYSTEM_EXECUTABLES.iter().any(|root| exe.starts_with(root)),
            Exe::Unnamed => false,
        }
}

const EXEC_REREADS: u32 = 8;

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

/// Identifies running Claude Code processes from `processes` for `own_pid`.
pub fn claude_census(
    processes: impl IntoIterator<Item = Process>,
    home: Option<&Path>,
) -> ClaudeCensus {
    census_of(processes, home, Scratch::read().as_ref())
}

/// Canonicalized and raw Windows temporary root paths used by [`claude_code`].
pub(crate) struct Scratch {
    root: PathBuf,
    table: std::cell::OnceCell<aterm_sysprobe::volumes::Table>,
    expand: fn(&Path) -> Option<PathBuf>,
}

impl Scratch {
    fn read() -> Option<Self> {
        claude_scratch_root().map(|root| Self {
            root,
            table: std::cell::OnceCell::new(),
            expand: aterm_sysprobe::volumes::long_name,
        })
    }

    fn long<'p>(&self, path: &'p Path) -> std::borrow::Cow<'p, Path> {
        if aterm_sysprobe::paths::short_spelled(path).is_none() {
            return std::borrow::Cow::Borrowed(path);
        }
        let _quiet = aterm_sysprobe::quiet_errors();
        self.table
            .get_or_init(aterm_sysprobe::volumes::table)
            .long_spelling_by(path, self.expand)
    }

    fn holds(&self, path: &Path) -> bool {
        aterm_sysprobe::paths::windows_under(path, &self.root) == Some(true)
    }
}

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

#[derive(Debug, PartialEq, Eq)]
enum OwnDir {
    Unset,
    Read(PathBuf),
    Unread,
}

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

/// Scans all processes of the current UID to build a [`ClaudeCensus`].
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

/// Builds a [`ClaudeCensus`] from an explicit PID list and process-reader closure.
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
        if cwd.is_none() && aterm_uds::holders::exe_of(pid) == Exe::Exited {
            return None;
        }
    }
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

fn process_cwd(pid: u32) -> Option<PathBuf> {
    aterm_sysprobe::process_cwd(pid)
}

/// Returns the real UID of the current process (`0` on Windows).
#[must_use]
pub fn own_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: `getuid(2)` cannot fail and touches no memory.
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Captures the home volume's device ID and mount points for nested-checkout placement.
#[must_use]
pub fn placing_now(home: &Path) -> Option<checkout::Placing> {
    placing_of(home, &aterm_sysprobe::volumes::table())
}

/// Builds a [`checkout::Placing`] from the home directory and a pre-read mount table.
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

/// Returns the filesystem device ID (`st_dev`) of `path`, following symlinks.
#[must_use]
pub fn device_of(path: &Path) -> Option<u64> {
    identity(path).map(|(dev, _)| dev)
}

/// Returns the system's active mount points without stat'ing individual filesystems.
#[must_use]
pub fn mount_points() -> Vec<PathBuf> {
    aterm_sysprobe::volumes::table().points()
}

/// Provenance of a discovered Cargo build directory, determining its reclamation policy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Origin {
    #[default]
    Named,
    Checkout {
        root: PathBuf,
    },
    AgentTargetDir,
    BuildCache {
        workspace: PathBuf,
        mirror: PathBuf,
    },
    Bootstrap {
        repo: PathBuf,
        slim: bool,
    },
    ClaudeScratch {
        session: String,
        session_dir: PathBuf,
    },
    Linked {
        from: PathBuf,
    },
}

impl Origin {
    /// Returns whether this origin belongs to a transient session or workspace that can become gone.
    #[must_use]
    pub const fn ownable(&self) -> bool {
        matches!(
            self,
            Origin::ClaudeScratch { .. } | Origin::BuildCache { .. }
        )
    }

    fn owner_dir(&self) -> Option<&Path> {
        match self {
            Origin::ClaudeScratch { session_dir, .. } => Some(session_dir),
            Origin::BuildCache { mirror, .. } => Some(mirror),
            _ => None,
        }
    }

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

/// Discovered Cargo build directory paired with its [`Origin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub origin: Origin,
}

/// Discovered build directories and any roots that could not be fully traversed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovered {
    pub found: Vec<Found>,
    pub notes: Vec<String>,
    pub cut: Option<String>,
}

/// Discovers all Cargo build directories reachable from `roots` within `budget`.
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
        w.build_cache(&atpkg::build_cache::moved_from(home));
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

fn marked(dir: &Path) -> bool {
    read_into(dir);
    cachedir_tag_signed(&dir.join("CACHEDIR.TAG"))
        || std::fs::symlink_metadata(dir.join(RUSTC_INFO)).is_ok_and(|m| m.is_file())
}

/// Returns whether `name` is a `rustc` `x.py` bootstrap Cargo build directory (`stage0-rustc`, etc.).
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

fn is_xpy(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join("x.py")).is_ok_and(|m| m.is_file())
}

fn inside_xpy_build(dir: &Path) -> bool {
    dir.ancestors()
        .take(MAX_ANCESTORS)
        .any(|a| a.file_name().is_some_and(|n| n == "build") && a.parent().is_some_and(is_xpy))
}

/// Resolves the main repository root for a linked `git worktree` directory.
#[must_use]
pub fn worktree_root_of(dir: &Path) -> Option<PathBuf> {
    worktree_root(dir)
}

fn worktree_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .take(MAX_ANCESTORS)
        .find(|d| std::fs::symlink_metadata(d.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

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

fn small_bytes(p: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(p).ok()?;
    if !meta.is_file() || meta.len() > 4096 {
        return None;
    }
    std::fs::read(p).ok()
}

fn small_text(p: &Path) -> Option<String> {
    String::from_utf8(small_bytes(p)?).ok()
}

fn dev_nofollow(p: &Path) -> Option<u64> {
    aterm_sysprobe::volumes::file_id(p, false).map(|id| id.dev)
}

fn identity(p: &Path) -> Option<(u64, u128)> {
    aterm_sysprobe::volumes::file_id(p, true).map(|id| (id.dev, id.ino))
}

fn strictly_inside(path: &Path, dir: &Path) -> bool {
    if cfg!(windows) {
        use aterm_sysprobe::paths::{is_under, windows_key};
        is_under(path, dir) == Some(true) && windows_key(path) != windows_key(dir)
    } else {
        path.starts_with(dir) && path != dir
    }
}

fn home_skipped(name: &std::ffi::OsStr) -> bool {
    if cfg!(windows) {
        checkout::home_skip(name).is_some()
    } else {
        HOME_SKIP.iter().any(|s| name == *s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(windows),
    expect(dead_code, reason = "only Windows passes over an entry it lists")
)]
enum PassedOver {
    Reparse,
    Fetches,
}

fn fetches_or_reparses(e: &std::fs::DirEntry, ty: std::fs::FileType) -> Option<PassedOver> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const REPARSE: u32 = 0x0000_0400;
        const FETCHES: u32 = 0x0000_1000 | 0x0004_0000 | 0x0040_0000;
        let Ok(attributes) = e.metadata().map(|m| m.file_attributes()) else {
            return Some(PassedOver::Fetches);
        };
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

/// Guard that suppresses cloud placeholder hydration on Windows while held.
#[cfg(windows)]
pub(crate) type Exposed = aterm_dirfd::ExposedPlaceholders;
/// No-op placeholder guard on non-Windows platforms.
#[cfg(not(windows))]
pub(crate) struct Exposed;

/// Enables raw reparse-point reads for cloud placeholders on the current thread.
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
    static READS: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, windows))]
fn record_reads(on: bool) -> Vec<PathBuf> {
    READS
        .with(|r| std::mem::replace(&mut *r.borrow_mut(), on.then(Vec::new)))
        .unwrap_or_default()
}

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
    seen: BTreeMap<(u64, u128), usize>,
    found: Vec<Found>,
    device: Option<u64>,
    home: Option<PathBuf>,
    #[cfg(windows)]
    volume: Option<super::target::VolumeGuid>,
    #[cfg(windows)]
    table: Option<aterm_sysprobe::volumes::Table>,
    mounts: BTreeSet<PathBuf>,
    foreign: Vec<PathBuf>,
    notes: Vec<String>,
    _exposed: Exposed,
}

impl Walk {
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
            #[cfg(windows)]
            table: roots.device.map(|_| aterm_sysprobe::volumes::table()),
            mounts: roots.mounts.iter().cloned().collect(),
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

    fn on_device(&self, p: &Path) -> bool {
        self.device
            .is_none_or(|d| identity(p).is_some_and(|(dev, _)| dev == d) && self.on_volume(p, true))
    }

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

    fn same_device(&self, p: &Path) -> bool {
        self.device
            .is_none_or(|d| dev_nofollow(p) == Some(d) && self.on_volume(p, false))
    }

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

    fn add(&mut self, dir: &Path, origin: &Origin) {
        self.add_via(dir, origin, dir);
    }

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

    fn fenced(&self, p: &Path) -> bool {
        self.home.as_deref().is_some_and(|home| {
            atpkg::protected::under_app_data(home, p)
                || atpkg::protected::under_file_provider_domain(home, p)
        })
    }

    fn checkout(&mut self, root: &Path) {
        let Ok(root) = std::fs::canonicalize(root) else {
            return;
        };
        let root = root.as_path();
        if self.fenced(root) || !self.on_device(root) || inside_xpy_build(root) {
            return;
        }
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

/// Session or removed-workspace owner re-verified by [`still_gone`] immediately before deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    ClaudeSession {
        session: String,
        session_dir: PathBuf,
        claude_dirs: Vec<PathBuf>,
        home: Option<PathBuf>,
    },
    BuildCache {
        workspace: PathBuf,
        mirror: PathBuf,
        home: Option<PathBuf>,
    },
}

impl Owner {
    /// Returns the root directory inside which the owner's build directory must reside.
    #[must_use]
    pub fn dir(&self) -> &Path {
        match self {
            Owner::ClaudeSession { session_dir, .. } => session_dir,
            Owner::BuildCache { mirror, .. } => mirror,
        }
    }

    /// Returns the required quiet duration in seconds before a gone owner's build directory can be removed.
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

    /// Formats a human-readable description of the gone owner for the cleanup journal.
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

/// Ownership status of a discovered build directory (`Live`, `Gone`, or `Unknown`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Owned {
    #[default]
    NotOwned,
    Live(String),
    Gone {
        evidence: String,
        owner: Owner,
    },
}

/// Cached session and worktree ownership resolver for a single discovery pass.
pub struct Owners {
    home: Option<PathBuf>,
    claude_dirs: Vec<PathBuf>,
    claudes: Vec<RunningClaude>,
    unread: Option<String>,
    now: i64,
    quiet_s: i64,
    registries: BTreeMap<PathBuf, Registry>,
    sessions: BTreeMap<PathBuf, Owned>,
    placing: Option<checkout::Placing>,
}

impl Owners {
    /// Creates an [`Owners`] resolver at `now` with `quiet_s` and the running [`ClaudeCensus`].
    #[must_use]
    pub const fn new(
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

    /// Attaches home-volume [`checkout::Placing`] facts for judging nested workspaces.
    #[must_use]
    pub fn placed(mut self, placing: Option<checkout::Placing>) -> Owners {
        self.placing = placing;
        self
    }

    /// Resolves the [`Owned`] status of a build directory with `origin`.
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

fn unread(why: &str) -> Owned {
    Owned::Live(format!("no session is called ended: {why}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    pid: u32,
    session: String,
    started: String,
}

type Registry = Option<Vec<Record>>;

/// The session records under `claude_dir` whose writer still runs: its pid is this user's live
/// process ([`upgrade_drive::writer_may_live`](super::super::upgrade_drive::writer_may_live):
/// no zombie, and no other user's process that took the number) and started when the record
/// says (an unread start may), as [`conversation_live`](super::super::upgrade_drive::conversation_live)
/// judges a conversation. Claude Code keeps a file whose pid answers anything but `ESRCH`.
fn live_records(claude_dir: &Path) -> Registry {
    use super::super::upgrade_drive::{kernel_start, squash, writer_may_live};
    let files = super::super::upgrade_drive::session_files_in(claude_dir)?;
    Some(
        files
            .into_iter()
            .filter(|f| {
                writer_may_live(f.pid)
                    && kernel_start(f.pid).is_none_or(|start| start == squash(&f.proc_start))
            })
            .map(|f| Record {
                pid: f.pid,
                started: super::super::upgrade_drive::squash(&f.proc_start),
                session: f.session_id,
            })
            .collect(),
    )
}

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
    if records().any(|r| r.session == *session) {
        return Owned::Live(format!("Claude Code session {} is running", short(session)));
    }
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

/// Function signature for re-running the Claude Code process census prior to deletion.
pub type Census<'a> = &'a dyn Fn(Option<&Path>) -> ClaudeCensus;

/// Re-verifies right before deletion that `owner` remains gone at `now`.
pub fn still_gone(owner: &Owner, now: i64, quiet_s: i64, census: Census<'_>) -> Result<(), String> {
    let verdict = match owner {
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

/// Verifies that the canonical build directory `path` lies strictly inside `owner`'s directory.
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

/// Returns whether an unreadable process with executable `exe` is a benign OS platform service.
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

/// Returns whether `exe` is a Windows system or Defender service binary.
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

/// Function signature for scanning processes that hold open paths under target directories.
pub type Processes<'a> =
    &'a dyn Fn(&[PathBuf], &[u32]) -> std::io::Result<aterm_uds::holders::PathScan>;

/// Verifies that no running process uses `path` or its `owner` directory.
pub fn unused(owner: &Owner, path: &Path, processes: Processes<'_>) -> Result<(), String> {
    let mut dirs = vec![path.to_path_buf()];
    if let Ok(dir) = std::fs::canonicalize(owner.dir()) {
        dirs.push(dir);
    }
    unused_under(&dirs, processes)
}

/// Verifies that no running process has an open cwd, executable, or descriptor under `dirs`.
pub fn unused_under(dirs: &[PathBuf], processes: Processes<'_>) -> Result<(), String> {
    scan_says_unused(&processes(dirs, &[std::process::id()]), |_| true)
}

/// Evaluates a [`aterm_uds::holders::PathScan`] against `counts` to check whether `dirs` are
/// unused.
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

/// Scans all processes of this UID for open references under `dirs`. A debug build in an
/// aterm-lab jail that names its mark reads the jail's processes alone
/// ([`aterm_uds::holders::lab_jail_pids`]): the owner's, outside it, cannot be read from inside.
pub fn scan_processes(
    dirs: &[PathBuf],
    skip: &[u32],
) -> std::io::Result<aterm_uds::holders::PathScan> {
    #[cfg(all(debug_assertions, target_vendor = "apple"))]
    let mut scan = match aterm_uds::holders::lab_jail_pids() {
        Some(pids) => aterm_uds::holders::scan_path_users_of(&pids?, dirs, skip)?,
        None => aterm_uds::holders::scan_path_users(dirs, skip)?,
    };
    #[cfg(not(all(debug_assertions, target_vendor = "apple")))]
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

/// Re-checks under Cargo locks that `owner` is still gone, unused, and quiet before whole-directory removal.
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

fn short(session: &str) -> &str {
    session.get(..8).unwrap_or(session)
}

/// Returns the newest file modification timestamp (excluding Cargo lock files) under `dir`.
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

#[path = "disk_discover_windows_tests.rs"]
#[cfg(all(test, windows))]
mod windows_tests;
