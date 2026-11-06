// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cutter binary provenance and freshness verification at process start.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use crate::ledger::{GitCli, GitRunner};

/// Why this binary is not a commit's own cutter, and the one way to run one that is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotOwn {
    /// What the judgment found.
    pub why: String,
    /// The remedy: what to do, ending in what to run again.
    pub fix: String,
}

impl NotOwn {
    fn new(why: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            fix: fix.into(),
        }
    }
}

/// What this running cutter is: the commit it is the build of, or why it is none.
pub type Stamp = Result<String, NotOwn>;

/// The checkout paths whose state decides the cutter binary.
pub const SOURCE_INPUTS: [&str; 6] = [
    "crates",
    "vendor",
    ".cargo",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
];

/// The files at a checkout's root a build reads that its dep-info does not list: the
/// workspace's manifest and lockfile, the toolchain pin and cargo's config.
const ROOT_BUILD_INPUTS: [&str; 5] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    ".cargo/config",
];

/// The most symbolic links [`from_root`] follows for one path (Linux's `MAXSYMLINKS`).
const MAX_LINKS: usize = 40;

const OWN_CRATE: &str = "crates/aterm-release";
const OWN_MAIN: &str = "crates/aterm-release/src/main.rs";

const RELINK: &str = "run it as `targo --unverified ship <verb>` from a clean checkout of the \
                      commit it acts for, a `targo run` that checks it against that tree (and \
                      relinks it when it is stale) first";

/// What a cutter's build inputs are dated against: it is a commit's build only if none of them
/// changed after this moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Since {
    /// The start of this process's parent when that is the `targo` that ran it (`targo run`,
    /// `targo --unverified ship`), the program cargo names in `$CARGO` for what it runs, and
    /// the executable was placed after that start and before this process's own ([`since`]).
    /// The kernel records a process's start at its fork, which an `exec` keeps, so this is when
    /// the parent's process was forked: no later than that `targo` began. A `targo` that starts
    /// the binary as its child reads the lockfile, manifests and config, checks each unit's
    /// sources against that unit's last compile (compiling it again, and relinking, when one is
    /// newer) and places the binary before it starts it; on macOS, the one platform where this
    /// moment is read, it places it again on every run (its status change time moves on a run
    /// that compiles nothing). An input unchanged since that moment is one the binary was built
    /// from, by cargo's own freshness rule, whenever the units that read it compiled.
    Launch(SystemTime),
    /// What the cutter that handed this one on (to a step it runs, or to the engine for its
    /// transforms) was judged from, read only from a live ancestor that handed it
    /// ([`handed_since`]).
    Handed(SystemTime),
    /// The executable's link (its modification time), for a cutter neither launched nor handed
    /// (on macOS one run directly, by a runner, or by a nested `targo` that exec'd into it; on
    /// any other platform every one, as no process's start is read there, so none is handed a
    /// moment either): an input edited during the build that linked it, after a unit read it,
    /// is older than this and goes unseen.
    Link(SystemTime),
}

impl Since {
    /// The moment itself.
    #[must_use]
    pub fn at(self) -> SystemTime {
        match self {
            Self::Launch(at) | Self::Handed(at) | Self::Link(at) => at,
        }
    }

    /// The moment, in words that follow "changed after".
    fn words(self) -> &'static str {
        match self {
            Self::Launch(_) => "the process of the `targo` that started this cutter was forked",
            Self::Handed(_) => "the moment the cutter that handed this one on was judged from",
            Self::Link(_) => "this cutter was linked",
        }
    }
}

/// What [`judge_at_start`] found: the stamp, and the moment it dated the inputs from, when it
/// got that far.
struct Judged {
    stamp: Stamp,
    since: Option<Since>,
}

static RUNNING: OnceLock<Judged> = OnceLock::new();

/// Judge this process. `main` calls it first, before any step moves a checkout.
pub fn judge_at_start() {
    RUNNING.get_or_init(|| match std::env::current_exe() {
        Ok(exe) => {
            let var = |name: &str| std::env::var_os(name);
            let handed = handed_since(
                var(crate::release_steps::RELEASE_CUTTER_ENV).as_deref(),
                var(crate::release_steps::RELEASE_CUTTER_SINCE_ENV).as_deref(),
                &exe,
                &ancestor,
            );
            judge_process(&exe, launcher_start(), started(std::process::id()), handed)
        }
        Err(e) => Judged {
            stamp: Err(NotOwn::new(
                format!("it cannot name its own executable ({e})"),
                RELINK,
            )),
            since: None,
        },
    });
}

/// This process's verdict, from [`judge_at_start`].
/// # Errors
/// This process is no commit's build, or was never judged.
pub fn running() -> Result<&'static str, &'static NotOwn> {
    RUNNING
        .get_or_init(|| Judged {
            stamp: Err(NotOwn::new("it was not judged when it started", RELINK)),
            since: None,
        })
        .stamp
        .as_ref()
        .map(String::as_str)
}

/// What this process's start judgment dated its inputs from, for a cutter it hands on
/// ([`hand_value`]); `None` when it was never judged or never got that far.
#[must_use]
pub fn reference() -> Option<SystemTime> {
    RUNNING.get()?.since.map(Since::at)
}

/// What the kernel says of one of this process's ancestors ([`ancestor`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ancestor {
    /// Its start.
    pub start: SystemTime,
    /// The inode of the executable it runs, where read.
    pub image: Option<u64>,
}

/// Process `pid` when it is one of this process's [`ancestors`], its start read (macOS); else
/// `None`.
fn ancestor(pid: u32) -> Option<Ancestor> {
    if !ancestors().contains(&pid) {
        return None;
    }
    Some(Ancestor {
        start: started(pid)?,
        image: aterm_sysprobe::kproc::image_inode(pid),
    })
}

/// What the cutter that handed this one on was judged from: the moment `since`
/// (`ATERM_RELEASE_CUTTER_SINCE`, as [`since_value`] writes it) carries, read only when
/// `named` (`ATERM_RELEASE_CUTTER`) is this executable `exe`, both resolved, so a link to it
/// counts, and the process `since` names as the one that handed it is a live ancestor of this
/// one (`ancestor`, [`ancestor`] for this process) that started when `since` says and runs this
/// very file (the inode of its executable is `exe`'s); else `None`. So a variable a process
/// inherited from elsewhere, or one whose handing process is gone, is no moment. The boundary:
/// the moment is trusted from that ancestor, so a process below it that writes the variable
/// itself, naming that ancestor, is inside it. A value earlier than the moment that cutter was
/// judged from only refuses more.
pub(crate) fn handed_since(
    named: Option<&OsStr>,
    since: Option<&OsStr>,
    exe: &Path,
    ancestor: &dyn Fn(u32) -> Option<Ancestor>,
) -> Option<SystemTime> {
    let resolved = |path: &Path| fs::canonicalize(path).ok();
    if resolved(Path::new(named?))? != resolved(exe)? {
        return None;
    }
    let moment = |word: &str| {
        let nanos: u64 = word.parse().ok()?;
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(nanos))
    };
    let mut words = since?.to_str()?.split(' ');
    let (Some(at), Some(by), Some(started), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return None;
    };
    let handing = ancestor(by.parse().ok()?)?;
    (handing.start == moment(started)? && handing.image.is_some() && handing.image == inode(exe))
        .then_some(moment(at)?)
}

/// The inode of the file at `exe`, where read.
fn inode(exe: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        fs::metadata(exe).ok().map(|meta| meta.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = exe;
        None
    }
}

/// The moment `at` handed on by the process `pid` that started at `start`, as [`handed_since`]
/// reads it: the three as whole nanoseconds since the Unix epoch, the pid between, split by
/// spaces; `None` before the Unix epoch or past what 64 bits of nanoseconds hold.
#[must_use]
pub fn since_value(at: SystemTime, pid: u32, start: SystemTime) -> Option<String> {
    let nanos = |t: SystemTime| {
        let nanos = t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
        u64::try_from(nanos).ok()
    };
    Some(format!("{} {pid} {}", nanos(at)?, nanos(start)?))
}

/// `at` handed on by this process ([`since_value`]); `None` where its start is unread (all but
/// macOS), so no moment is handed there.
#[must_use]
pub fn hand_value(at: SystemTime) -> Option<String> {
    let pid = std::process::id();
    since_value(at, pid, started(pid)?)
}

/// Process `pid`'s start, as the kernel records it (macOS); `None` where it is unread.
fn started(pid: u32) -> Option<SystemTime> {
    let record = aterm_sysprobe::kproc::record(pid)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(
        record.start_s,
        record.start_us.saturating_mul(1_000),
    ))
}

/// The most ancestors [`ancestors`] reads.
const MAX_ANCESTORS: usize = 64;

/// This process's ancestors' pids, its parent's first, as the kernel records them (macOS): none
/// where a record is unread.
pub(crate) fn ancestors() -> Vec<u32> {
    let mut found = Vec::new();
    let mut pid = std::process::id();
    while found.len() < MAX_ANCESTORS {
        let Some(parent) = aterm_sysprobe::kproc::record(pid).map(|r| r.ppid) else {
            break;
        };
        if parent == 0 || found.contains(&parent) {
            break;
        }
        found.push(parent);
        pid = parent;
    }
    found
}

/// The start of the `targo` that ran this process, read from the kernel and this process's
/// `$CARGO` ([`launcher`]); `None` where any of it is unread (as it is but on macOS).
fn launcher_start() -> Option<SystemTime> {
    let parent = aterm_sysprobe::kproc::record(std::process::id())?.ppid;
    launcher(
        aterm_sysprobe::process_path(parent).as_deref(),
        std::env::var_os("CARGO").as_deref(),
        started(parent),
    )
}

/// The start of the `targo` that ran a process whose parent runs `parent_exe` and started at
/// `parent_start`, and whose `$CARGO` is `cargo`: `parent_start` when the parent is the program
/// `cargo` names (cargo sets it for what it runs; another program that inherited it is no parent
/// of that name), both resolved; `None` where any of that is unread or the parent is another
/// program.
fn launcher(
    parent_exe: Option<&Path>,
    cargo: Option<&OsStr>,
    parent_start: Option<SystemTime>,
) -> Option<SystemTime> {
    let cargo = fs::canonicalize(cargo?).ok()?;
    if fs::canonicalize(parent_exe?).ok()? != cargo {
        return None;
    }
    parent_start
}

/// When the file `meta` describes was last placed: its status change time, which a new copy
/// (cargo's on macOS) or a new link (cargo's elsewhere) sets; `None` where it is unread.
#[cfg(unix)]
fn placed(meta: &fs::Metadata) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt as _;
    let secs = u64::try_from(meta.ctime()).ok()?;
    let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}

/// See the unix twin: no status change time is read here.
#[cfg(not(unix))]
fn placed(_meta: &fs::Metadata) -> Option<SystemTime> {
    None
}

/// The kernel records a process's start in whole microseconds, dropping the rest: the process
/// started before its recorded start and this much more.
const START_GRAIN: Duration = Duration::from_micros(1);

/// The [`Since`] of a cutter linked at `linked` whose executable was last placed at `placed`, in
/// a process that started at `own`, the `targo` that began at `launcher` ran ([`launcher`]), and
/// was handed `handed`: its launch when the executable was placed after that `targo` began and
/// before this process did (cargo places a binary before it starts it, so a binary placed later
/// is not the one that `targo` started, nor one placed before it began one it held to its
/// inputs), else what it was handed, else its link.
fn since(
    linked: SystemTime,
    placed: Option<SystemTime>,
    launcher: Option<SystemTime>,
    own: Option<SystemTime>,
    handed: Option<SystemTime>,
) -> Since {
    match (launcher, placed, own) {
        (Some(launcher), Some(placed), Some(own))
            if launcher <= placed
                && own.checked_add(START_GRAIN).is_some_and(|end| placed < end) =>
        {
            Since::Launch(launcher)
        }
        _ => handed.map_or(Since::Link(linked), Since::Handed),
    }
}

/// [`judge`] of the process running `exe` that started at `own`, run by the `targo` that began
/// at `launcher` (`None` when none ran it, or a start is unread) and handed `handed`, from the
/// moment [`since`] picks for it.
fn judge_process(
    exe: &Path,
    launcher: Option<SystemTime>,
    own: Option<SystemTime>,
    handed: Option<SystemTime>,
) -> Judged {
    let meta = match fs::metadata(exe) {
        Ok(meta) => meta,
        Err(e) => {
            return Judged {
                stamp: Err(NotOwn::new(
                    format!("cannot read {} ({e})", exe.display()),
                    RELINK,
                )),
                since: None,
            };
        }
    };
    let linked = match meta.modified() {
        Ok(linked) => linked,
        Err(e) => {
            return Judged {
                stamp: Err(NotOwn::new(
                    format!("{} has no link time ({e})", exe.display()),
                    RELINK,
                )),
                since: None,
            };
        }
    };
    let since = since(linked, placed(&meta), launcher, own, handed);
    Judged {
        stamp: judge(exe, since),
        since: Some(since),
    }
}

/// The remedy for an input changed after `since`, the moment a cutter was judged from. One
/// judged from the start of the `targo` that ran it ([`Since::Launch`]) is judged again by the
/// next such run from that run's start, so the run alone is the remedy. Any other (its link, or
/// a moment it was handed) says nothing of how the next run is judged: one judged from its link
/// again, as a run is where a start is unread or the binary was not placed between its
/// `targo`'s start and its own, sees a link cargo moves only for a change it tracks, so the
/// build is cleaned first, and the run relinks it.
fn stale_fix(exe: &Path, root: &Path, since: Since) -> String {
    match since {
        Since::Launch(_) => RELINK.to_string(),
        Since::Handed(_) | Since::Link(_) => format!(
            "`targo clean{} -p aterm-release` in {} (cargo relinks it only for a change it \
             tracks, and a run not judged from its `targo`'s start is judged from its link), \
             then {RELINK}",
            profile_flag(exe),
            root.display()
        ),
    }
}

/// Whether the cutter at `exe` is its checkout's `HEAD` build: its inputs judged from `since`,
/// and a stale one's remedy as [`stale_fix`] gives it for `since`.
fn judge(exe: &Path, since: Since) -> Stamp {
    let dep_info = exe.with_extension("d");
    let text = crate::durable::read_text(&dep_info).map_err(|e| {
        NotOwn::new(
            format!(
                "cannot read {} ({e}), the dep-info cargo writes beside a binary it links",
                dep_info.display()
            ),
            RELINK,
        )
    })?;
    let (output, sources) = dep_info_sources(&text, exe).ok_or_else(|| {
        NotOwn::new(
            format!("{} does not describe {}", dep_info.display(), exe.display()),
            RELINK,
        )
    })?;
    let root = checkout_of(&sources).ok_or_else(|| {
        NotOwn::new(
            format!("{} names no {OWN_MAIN}", dep_info.display()),
            RELINK,
        )
    })?;
    let resolved_exe = fs::canonicalize(exe).map_err(|e| {
        NotOwn::new(
            format!("cannot resolve cutter {} ({e})", exe.display()),
            RELINK,
        )
    })?;
    // Dep-info and the running image may spell the same output through different
    // root or target-directory aliases. Only these proved output paths define exclusions.
    let targets: Vec<&Path> = [exe, output.as_path(), resolved_exe.as_path()]
        .into_iter()
        .filter_map(|exe| exe.parent().and_then(Path::parent))
        .collect();
    let edited = checkout_sources(sources, &root, &targets);
    let resolved = fs::canonicalize(&root)
        .map_err(|e| NotOwn::new(format!("cannot resolve {} ({e})", root.display()), RELINK))?;
    let outside = |input: &Path| {
        NotOwn::new(
            format!(
                "it reads {}, which resolves outside {} or leaves it on the way (or passes more \
                 than {MAX_LINKS} links), where no commit holds it",
                input.display(),
                root.display()
            ),
            format!(
                "keep what it reads, and each link on the way, inside {} and committed, or \
                 stop the cutter's build reading it, then {RELINK}",
                root.display()
            ),
        )
    };
    let mut inputs = CompiledInputs::default();
    let mut absent = Vec::new();
    for source in &edited {
        inputs
            .read(source, &resolved)
            .map_err(|error| match error {
                InputError::Outside(path) => outside(&path),
                InputError::Read(path, error) => NotOwn::new(
                    format!(
                        "cannot enumerate compiled input {} ({error})",
                        path.display()
                    ),
                    RELINK,
                ),
                InputError::Cycle(path) => NotOwn::new(
                    format!(
                        "compiled input directory {} contains a symbolic-link cycle",
                        path.display()
                    ),
                    format!("remove the directory cycle, then {RELINK}"),
                ),
            })?;
    }
    let CompiledInputs {
        mut walked,
        mut files,
        directories,
        ..
    } = inputs;
    for input in build_inputs(&walked, &resolved) {
        let mut read = from_root(&input, &resolved).ok_or_else(|| outside(&input))?;
        if read
            .last()
            .is_some_and(|(end, _)| fs::symlink_metadata(end).is_err())
        {
            absent.extend(read.pop().map(|(end, _)| end));
        }
        for (path, name) in read {
            walked.push(path);
            files.insert(name);
        }
    }
    let changed = changed_after(&walked, since)
        .or_else(|| absent.iter().find_map(|input| removed_after(input, since)));
    if let Some(why) = changed {
        return Err(NotOwn::new(why, stale_fix(exe, &root, since)));
    }
    let unlisted = || {
        NotOwn::new(
            format!("git could not list the state of {}", root.display()),
            format!(
                "run `git status` in {} and mend what it reports, then run it again",
                root.display()
            ),
        )
    };
    let tracked = crate::clean::tracked_files(&root).ok_or_else(unlisted)?;
    let trees = tracked_directories(&tracked);
    let untracked = files
        .iter()
        .find(|path| !tracked.contains(*path))
        .or_else(|| directories.iter().find(|path| !trees.contains(*path)));
    if let Some(untracked) = untracked {
        let kind = if directories.contains(untracked) {
            "directory"
        } else {
            "file"
        };
        return Err(NotOwn::new(
            format!(
                "it reads {untracked}, which {} does not track (an untracked or ignored {kind}, \
                 which no commit holds)",
                root.display()
            ),
            format!(
                "commit {untracked} in {} or stop the cutter's build reading it, then {RELINK}",
                root.display()
            ),
        ));
    }
    let dirty: Vec<String> = crate::clean::dirty_against_head(&root)
        .ok_or_else(unlisted)?
        .into_keys()
        .filter(|path| {
            is_source_input(path)
                || files.contains(path)
                || directories.iter().any(|dir| {
                    path.strip_prefix(dir)
                        .is_some_and(|rest| rest.starts_with('/'))
                })
        })
        .collect();
    if !dirty.is_empty() {
        return Err(NotOwn::new(
            format!(
                "its checkout {} has uncommitted cutter sources ({})",
                root.display(),
                dirty[..dirty.len().min(3)].join(", ")
            ),
            format!(
                "commit or remove what `git status` lists under {}'s cutter sources ({}), then \
                 run it again",
                root.display(),
                SOURCE_INPUTS.join(", ")
            ),
        ));
    }
    head(&GitCli::new(&root)).ok_or_else(|| {
        NotOwn::new(
            format!("{} has no HEAD commit", root.display()),
            format!(
                "check out a commit in {}, then run it again",
                root.display()
            ),
        )
    })
}

fn profile_flag(exe: &Path) -> String {
    match exe
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
    {
        Some("debug") | None => String::new(),
        Some("release") => " --release".to_string(),
        Some(profile) => format!(" --profile {profile}"),
    }
}

fn head(git: &dyn GitRunner) -> Option<String> {
    let out = git
        .git(&["rev-parse", "--verify", "-q", "HEAD^{commit}"])
        .ok()
        .filter(crate::ledger::RunOut::success)?;
    let commit = out.stdout_utf8().trim().to_string();
    aterm_codec::hex::is_lower(&commit, 40).then_some(commit)
}

fn dep_info_sources(text: &str, target: &Path) -> Option<(PathBuf, Vec<PathBuf>)> {
    let resolved_target = fs::canonicalize(target).ok()?;
    let same = |a: &Path| fs::canonicalize(a).is_ok_and(|a| a == resolved_target);
    text.lines().find_map(|line| {
        let (first, rest) = split_first_escaped(line)?;
        let out = first.strip_suffix(':')?;
        same(Path::new(out))
            .then(|| {
                let output = PathBuf::from(out);
                let output = if output.is_absolute() {
                    output
                } else {
                    std::env::current_dir().ok()?.join(output)
                };
                Some((
                    output,
                    split_escaped(rest).into_iter().map(PathBuf::from).collect(),
                ))
            })
            .flatten()
    })
}

fn split_first_escaped(line: &str) -> Option<(std::borrow::Cow<'_, str>, &str)> {
    let trimmed = line.trim_start_matches(' ');
    if trimmed.is_empty() {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    let mut has_escape = false;
    while i < bytes.len() {
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b' ') {
            has_escape = true;
            i += 2;
        } else if bytes[i] == b' ' {
            break;
        } else {
            i += 1;
        }
    }
    let raw_first = &trimmed[..i];
    let rest = &trimmed[i..];
    let first = if has_escape {
        std::borrow::Cow::Owned(raw_first.replace("\\ ", " "))
    } else {
        std::borrow::Cow::Borrowed(raw_first)
    };
    Some((first, rest))
}

fn split_escaped(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                word.push(' ');
                chars.next();
            }
            ' ' => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn checkout_of(sources: &[PathBuf]) -> Option<PathBuf> {
    sources
        .iter()
        .find(|source| source.ends_with(OWN_MAIN))
        .and_then(|main| main.ancestors().nth(4))
        .map(Path::to_path_buf)
}

fn checkout_sources(sources: Vec<PathBuf>, root: &Path, targets: &[&Path]) -> Vec<PathBuf> {
    let resolved = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // Align only the checkout root. Descendant source links must still reach
    // from_root intact, where their own tracking and containment are checked.
    let align = |path: &Path| {
        path.strip_prefix(root)
            .or_else(|_| path.strip_prefix(&resolved))
            .ok()
            .map(|rest| resolved.join(rest))
    };
    let targets: Vec<PathBuf> = targets
        .iter()
        .map(|path| align(path).unwrap_or_else(|| path.to_path_buf()))
        .filter(|path| !resolved.starts_with(path))
        .collect();
    sources
        .into_iter()
        .filter(|source| {
            let input = match align(source) {
                Some(source) => !targets.iter().any(|target| source.starts_with(target)),
                // Unknown root aliases may identify checkout inputs too. This
                // can only include an input, never exclude one as generated.
                // Keep the original spelling for link and containment checks.
                None => source
                    .ancestors()
                    .find_map(|path| fs::canonicalize(path).ok())
                    .is_some_and(|path| path.starts_with(&resolved)),
            };
            input && !source.components().any(|c| c.as_os_str() == ".git")
        })
        .collect()
}

/// The files a build of `sources` may read that its dep-info does not list, there or not:
/// [`ROOT_BUILD_INPUTS`], and the `Cargo.toml` of each folder from a package root up to `root`:
/// its package's manifest, and each place above it cargo looks for a workspace's. The package
/// roots are the cutter's own, [`OWN_CRATE`] (its package's folder, known without reading the
/// tree, so its manifest stays a candidate after a commit removes it), and each source's (the
/// nearest folder above it inside `root` that holds a `Cargo.toml`). A folder between a source and its package root is
/// none of these, so cargo reads no manifest there. Residual: a dependency package's manifest a
/// commit removed after the moment the cutter is judged from is no candidate, as its sources'
/// package root is then a folder above it.
fn build_inputs(sources: &[PathBuf], root: &Path) -> Vec<PathBuf> {
    let inside = |folder: &&Path| folder.starts_with(root);
    let own = root.join(OWN_CRATE);
    let parents: BTreeSet<&Path> = sources
        .iter()
        .filter_map(|source| source.parent())
        .filter(inside)
        .collect();
    let packages = parents.into_iter().filter_map(|parent| {
        parent
            .ancestors()
            .take_while(inside)
            .find(|folder| fs::symlink_metadata(folder.join("Cargo.toml")).is_ok())
    });
    let folders: BTreeSet<&Path> = std::iter::once(own.as_path())
        .chain(packages)
        .flat_map(|package| package.ancestors().take_while(inside))
        .collect();
    let inputs: BTreeSet<PathBuf> = ROOT_BUILD_INPUTS
        .iter()
        .map(|input| root.join(input))
        .chain(folders.into_iter().map(|folder| folder.join("Cargo.toml")))
        .collect();
    inputs.into_iter().collect()
}

#[derive(Default)]
struct CompiledInputs {
    walked: Vec<PathBuf>,
    files: BTreeSet<String>,
    directories: BTreeSet<String>,
    expanding: BTreeSet<PathBuf>,
    expanded: BTreeSet<PathBuf>,
}

enum InputError {
    Outside(PathBuf),
    Read(PathBuf, std::io::Error),
    Cycle(PathBuf),
}

impl CompiledInputs {
    /// A directory dependency includes its names and every descendant, even those ignored
    /// by Git. Keep every link in the original path and inspect the resolved directory once;
    /// only a completed expansion may be reused, so a link back into the active walk refuses.
    fn read(&mut self, input: &Path, resolved: &Path) -> Result<(), InputError> {
        let read = from_root(input, resolved).ok_or_else(|| InputError::Outside(input.into()))?;
        for (path, name) in read {
            self.walked.push(path.clone());
            if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                self.files.insert(name);
                continue;
            }
            self.directories.insert(name);
            if self.expanding.contains(&path) {
                return Err(InputError::Cycle(path));
            }
            if self.expanded.contains(&path) {
                continue;
            }
            self.expanding.insert(path.clone());
            let mut entries = fs::read_dir(&path)
                .map_err(|error| InputError::Read(path.clone(), error))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| InputError::Read(path.clone(), error))?;
            entries.sort();
            for entry in entries {
                self.read(&entry, resolved)?;
            }
            self.expanding.remove(&path);
            self.expanded.insert(path);
        }
        Ok(())
    }
}

/// Git records files and gitlinks, not directory entries. Their ancestors are the
/// directory trees a tracked input can establish; empty untracked directories establish none.
fn tracked_directories(files: &BTreeSet<String>) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for file in files {
        let mut below = file.as_str();
        while let Some((parent, _)) = below.rsplit_once('/') {
            directories.insert(parent.to_owned());
            below = parent;
        }
    }
    directories
}

/// What reading `path` reads in the checkout at `resolved`, its resolved root: each symbolic
/// link inside `resolved` that `path` passes through, in a folder or as its last part, then the
/// entry it ends at, each beside the name git gives it there (its parts from `resolved`, joined by
/// `/`). So a link and the file it leads to must each be one a commit holds. `path` is resolved
/// part by part as the kernel resolves it: a link's target from the link's folder, a `..` after
/// the links before it; it ends at its first part that is not there, as nothing below that is.
/// `None` when it ends outside `resolved`, leaves `resolved` once inside it (by a link or a `..`,
/// to a place no commit holds, though it may come back), or passes more than [`MAX_LINKS`]
/// links.
fn from_root(path: &Path, resolved: &Path) -> Option<Vec<(PathBuf, String)>> {
    let name = |at: &Path| {
        let rel = at.strip_prefix(resolved).ok()?;
        let mut out = String::with_capacity(rel.as_os_str().len());
        for part in rel.components() {
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&part.as_os_str().to_string_lossy());
        }
        (!out.is_empty()).then_some(out)
    };
    let mut read = Vec::new();
    let mut at = PathBuf::new();
    let mut left = Vec::new();
    let mut links = 0;
    let mut inside = false;
    queue(&mut at, &mut left, path);
    while let Some(part) = left.pop() {
        if at.starts_with(resolved) {
            inside = true;
        } else if inside {
            return None;
        }
        if part == ".." {
            at.pop();
            continue;
        }
        let next = at.join(&part);
        match fs::symlink_metadata(&next) {
            Ok(meta) if meta.file_type().is_symlink() => {
                links += 1;
                if links > MAX_LINKS {
                    return None;
                }
                if next.starts_with(resolved) {
                    let link_name = name(&next)?;
                    read.push((next.clone(), link_name));
                }
                queue(&mut at, &mut left, &fs::read_link(&next).ok()?);
            }
            Ok(_) => at = next,
            Err(_) => {
                at = next;
                break;
            }
        }
    }
    let file = name(&at)?;
    read.push((at, file));
    Some(read)
}

/// Put `path`'s parts on `left`, the first on top, starting `at` again from the root when `path`
/// is absolute.
fn queue(at: &mut PathBuf, left: &mut Vec<OsString>, path: &Path) {
    if path.has_root() {
        *at = PathBuf::from("/");
    }
    let start = left.len();
    left.extend(path.components().filter_map(|part| match part {
        Component::Normal(part) => Some(part.to_os_string()),
        Component::ParentDir => Some(OsString::from("..")),
        Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
    }));
    left[start..].reverse();
}

/// Why one of `paths` may differ from what this cutter was built with: one modified after
/// `since`, or one not there. A link is dated by its own time.
fn changed_after(paths: &[PathBuf], since: Since) -> Option<String> {
    paths.iter().find_map(|path| {
        match fs::symlink_metadata(path).and_then(|meta| meta.modified()) {
            Ok(modified) if modified <= since.at() => None,
            Ok(_) => Some(format!(
                "{} changed after {}",
                path.display(),
                since.words()
            )),
            Err(_) => Some(format!(
                "{} is gone since {}",
                path.display(),
                since.words()
            )),
        }
    })
}

/// Why `input`, where a build input resolved part by part ([`from_root`]) ends at nothing, may
/// have been there at `since`: the nearest folder above it that is there was modified after
/// `since`, so an entry was added to it or removed from it since, which may have been `input`
/// or a folder that held it.
fn removed_after(input: &Path, since: Since) -> Option<String> {
    let Some((folder, meta)) = input
        .ancestors()
        .skip(1)
        .find_map(|folder| Some((folder, fs::symlink_metadata(folder).ok()?)))
    else {
        return Some(format!("no folder above {} is there", input.display()));
    };
    match meta.modified() {
        Ok(modified) if modified <= since.at() => None,
        Ok(_) => Some(format!(
            "{} changed after {}, and {} is not there: it may have been removed since",
            folder.display(),
            since.words(),
            input.display()
        )),
        Err(e) => Some(format!("cannot read {} ({e})", folder.display())),
    }
}

fn is_source_input(path: &str) -> bool {
    SOURCE_INPUTS.iter().any(|input| {
        path.strip_prefix(input)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::testing;
    use std::time::Duration;

    struct Built {
        _dir: aterm_tempfile::TempDir,
        work: PathBuf,
        exe: PathBuf,
        head: String,
    }

    fn git(work: &Path, args: &[&str]) -> String {
        let out = testing::git()
            .arg("-C")
            .arg(work)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn set_modified(path: &Path, at: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    /// Date the folder at `path` (opened to read, as a folder cannot be opened to write).
    fn set_folder_modified(path: &Path, at: SystemTime) {
        fs::File::open(path).unwrap().set_modified(at).unwrap();
    }

    /// Date every file and folder under `path`, it included, but `.git` and the symbolic links,
    /// which nothing here dates without following them.
    #[cfg(unix)]
    fn backdate(path: &Path, at: SystemTime) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if entry.file_name() == ".git" || kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                backdate(&entry.path(), at);
            } else {
                set_modified(&entry.path(), at);
            }
        }
        set_folder_modified(path, at);
    }

    fn built() -> Built {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-release-cutter-")
            .tempdir()
            .unwrap();
        let work = dir.path().join("work with space");
        testing::init(
            &work,
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        fs::create_dir_all(work.join("crates/aterm-release/src")).unwrap();
        fs::write(work.join(OWN_MAIN), "fn main() {}\n").unwrap();
        fs::write(work.join("README.md"), "prose\n").unwrap();
        git(&work, &["add", "--all"]);
        git(&work, &["commit", "-q", "-m", "the cutter"]);
        let head = git(&work, &["rev-parse", "HEAD"]);
        let gate = dir.path().join("target/gate");
        fs::create_dir_all(&gate).unwrap();
        let exe = gate.join("aterm-release");
        fs::write(&exe, "linked").unwrap();
        let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
        fs::write(
            exe.with_extension("d"),
            format!("{}: {}\n", escaped(&exe), escaped(&work.join(OWN_MAIN))),
        )
        .unwrap();
        // Linked a day after its sources: later than any file or folder a test changes without
        // giving it a time of its own, so only those a test dates after the link are.
        let source_time = fs::metadata(work.join(OWN_MAIN))
            .unwrap()
            .modified()
            .unwrap();
        set_modified(&exe, source_time + Duration::from_secs(24 * 60 * 60));
        Built {
            _dir: dir,
            work,
            exe,
            head,
        }
    }

    /// The judgment of a cutter run directly, from its link.
    fn judged(b: &Built) -> Stamp {
        judge(&b.exe, Since::Link(linked(b)))
    }

    /// Whether `fix` is the remedy for a stale input of a cutter not judged from a `targo`'s
    /// start: clean its build in its checkout, then run it as `targo` runs it.
    fn cleans_first(b: &Built, fix: &str) -> bool {
        fix.starts_with(&format!(
            "`targo clean --profile gate -p aterm-release` in {} (",
            b.work.display()
        )) && fix.ends_with(&format!("then {RELINK}"))
    }

    #[test]
    fn a_fresh_build_of_a_clean_checkout_is_its_heads_cutter() {
        let b = built();
        assert_eq!(judged(&b), Ok(b.head.clone()));
        fs::write(b.work.join("README.md"), "edited\n").unwrap();
        assert_eq!(judged(&b), Ok(b.head.clone()));
    }

    #[test]
    fn a_source_changed_after_the_link_refuses() {
        let b = built();
        let linked = fs::metadata(&b.exe).unwrap().modified().unwrap();
        set_modified(&b.work.join(OWN_MAIN), linked + Duration::from_secs(1));
        let not = judged(&b).expect_err("stale");
        assert!(
            not.why.contains("changed after this cutter was linked"),
            "{not:?}"
        );
        assert!(cleans_first(&b, &not.fix), "{not:?}");
        fs::remove_file(b.work.join(OWN_MAIN)).unwrap();
        let not = judged(&b).expect_err("gone");
        assert!(not.why.contains("is gone"), "{not:?}");
    }

    /// A build input there that no commit holds refuses, as does one edited and not committed,
    /// though each is older than the link.
    #[test]
    fn an_uncommitted_cutter_source_refuses() {
        let b = built();
        let old = fs::metadata(&b.exe).unwrap().modified().unwrap() - Duration::from_secs(60);
        let lock = b.work.join("Cargo.lock");
        fs::write(&lock, "# not committed\n").unwrap();
        set_modified(&lock, old);
        let not = judged(&b).expect_err("untracked");
        assert!(
            not.why.contains("reads Cargo.lock, which") && not.why.contains("does not track"),
            "{not:?}"
        );
        commit(&b.work, "the lockfile");
        fs::write(&lock, "# edited, not committed\n").unwrap();
        set_modified(&lock, old);
        let not = judged(&b).expect_err("dirty");
        assert!(
            not.why.contains("uncommitted cutter sources (Cargo.lock)"),
            "{not:?}"
        );
        assert!(not.fix.contains("git status"), "{not:?}");
    }

    fn linked(b: &Built) -> SystemTime {
        fs::metadata(&b.exe).unwrap().modified().unwrap()
    }

    /// Write `text` at `rel` in the checkout, modified `at`.
    fn place(b: &Built, rel: &str, text: &str, at: SystemTime) {
        let path = b.work.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        set_modified(&path, at);
    }

    /// Commit everything in the checkout; the new `HEAD`.
    fn commit(work: &Path, message: &str) -> String {
        git(work, &["add", "--all"]);
        git(work, &["commit", "-q", "-m", message]);
        git(work, &["rev-parse", "HEAD"])
    }

    /// Make the binary's dep-info name its main and `rels`, files in its checkout.
    fn compiled(b: &Built, rels: &[&str]) {
        let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
        let words: Vec<String> = std::iter::once(OWN_MAIN)
            .chain(rels.iter().copied())
            .map(|rel| escaped(&b.work.join(rel)))
            .collect();
        fs::write(
            b.exe.with_extension("d"),
            format!("{}: {}\n", escaped(&b.exe), words.join(" ")),
        )
        .unwrap();
    }

    #[cfg(unix)]
    mod directory_inputs {
        use super::*;

        const WATCH: &str = "fixtures/steps";
        const CHILD: &str = "fixtures/steps/nested/b.jsonl";

        fn fixture() -> (Built, String) {
            let b = built();
            let old = linked(&b) - Duration::from_secs(60);
            place(&b, "fixtures/steps/a.jsonl", "first\n", old);
            place(&b, CHILD, "nested\n", old);
            place(
                &b,
                ".gitignore",
                "fixtures/steps/ignored.jsonl\nfixtures/steps/ignored-dir/\n",
                old,
            );
            let head = commit(&b.work, "tracked directory inputs");
            compiled(&b, &[WATCH]);
            backdate(&b.work, old);
            (b, head)
        }

        #[test]
        fn a_directory_dep_info_entry_accepts_its_tracked_tree() {
            let (b, head) = fixture();
            assert_eq!(judged(&b), Ok(head));
        }

        #[test]
        fn directory_inputs_reject_nested_edits_and_removals_outside_source_prefixes() {
            for remove in [false, true] {
                let (b, _) = fixture();
                let old = linked(&b) - Duration::from_secs(60);
                if remove {
                    fs::remove_file(b.work.join(CHILD)).unwrap();
                } else {
                    place(&b, CHILD, "different\n", old);
                }
                backdate(&b.work, old);
                let not = judged(&b).expect_err("changed descendant is a compiled input");
                assert!(
                    not.why.contains("uncommitted cutter sources") && not.why.contains(CHILD),
                    "{not:?}"
                );
            }
        }

        #[test]
        fn directory_input_freshness_retains_directory_entry_changes() {
            let (b, _) = fixture();
            let nested = b.work.join("fixtures/steps/nested");
            let transient = nested.join("removed.jsonl");
            fs::write(&transient, "added then removed\n").unwrap();
            fs::remove_file(transient).unwrap();
            set_folder_modified(&nested, linked(&b) + Duration::from_secs(1));
            let not = judged(&b).expect_err("a removed entry still changes the directory");
            assert!(
                not.why.contains("fixtures/steps/nested changed after"),
                "{not:?}"
            );
        }

        #[test]
        fn directory_inputs_reject_untracked_ignored_and_empty_entries() {
            for (name, directory) in [
                ("loose.jsonl", false),
                ("ignored.jsonl", false),
                ("empty", true),
                ("ignored-dir", true),
            ] {
                let (b, _) = fixture();
                let path = b.work.join(WATCH).join(name);
                if directory {
                    fs::create_dir(&path).unwrap();
                } else {
                    fs::write(&path, "not committed\n").unwrap();
                }
                backdate(&b.work, linked(&b) - Duration::from_secs(60));
                let not = judged(&b).expect_err("untracked directory input");
                assert!(
                    not.why.contains(name) && not.why.contains("does not track"),
                    "{not:?}"
                );
            }
        }

        #[test]
        fn directory_inputs_keep_internal_links_and_their_targets_in_the_proof() {
            let (b, _) = fixture();
            let old = linked(&b) - Duration::from_secs(60);
            place(&b, "fixtures/target.jsonl", "target\n", old);
            let alias = b.work.join(WATCH).join("alias.jsonl");
            std::os::unix::fs::symlink("../target.jsonl", &alias).unwrap();
            std::os::unix::fs::symlink("nested", b.work.join(WATCH).join("nested-alias")).unwrap();
            let head = commit(&b.work, "internal file and directory links");
            backdate(&b.work, old);
            assert_eq!(judged(&b), Ok(head.clone()));

            place(&b, "fixtures/target.jsonl", "target changed\n", old);
            let not =
                judged(&b).expect_err("linked target outside watched directory still checked");
            assert!(not.why.contains("fixtures/target.jsonl"), "{not:?}");
            place(&b, "fixtures/target.jsonl", "target\n", old);
            assert_eq!(judged(&b), Ok(head));

            fs::remove_file(&alias).unwrap();
            std::os::unix::fs::symlink("nested/b.jsonl", &alias).unwrap();
            let not = judged(&b).expect_err("link identity itself remains checked");
            assert!(not.why.contains("fixtures/steps/alias.jsonl"), "{not:?}");
        }

        #[test]
        fn directory_inputs_refuse_escape_and_cycles() {
            for cycle in [false, true] {
                let (b, _) = fixture();
                let old = linked(&b) - Duration::from_secs(60);
                let link = b.work.join(WATCH).join("link");
                let target = if cycle { "." } else { "../../../outside" };
                fs::write(b.work.with_file_name("outside"), "outside\n").unwrap();
                std::os::unix::fs::symlink(target, &link).unwrap();
                commit(&b.work, "a directory input link");
                backdate(&b.work, old);
                let not = judged(&b).expect_err("escaping and cyclic directory walks refuse");
                assert!(
                    not.why.contains(if cycle {
                        "symbolic-link cycle"
                    } else {
                        "resolves outside"
                    }),
                    "{not:?}"
                );
            }
        }
    }

    /// A commit that changes only a build input dep-info does not list (the lockfile, a crate's
    /// manifest) is no build of the binary linked before it, though no listed source changed and
    /// the checkout is clean; the same commit made before the link is that binary's. NEGATIVE
    /// CONTROL: compare the dep-info's sources alone, and the lockfile's commit is the binary's.
    #[test]
    fn a_build_input_committed_after_the_link_refuses() {
        for input in [
            "Cargo.lock",
            "Cargo.toml",
            "crates/aterm-release/Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
        ] {
            let b = built();
            let after = linked(&b) + Duration::from_secs(1);
            place(&b, input, "# committed after the link\n", after);
            commit(&b.work, "only a build input");
            let not = judged(&b).expect_err(input);
            assert!(
                not.why
                    .ends_with(&format!("{input} changed after this cutter was linked")),
                "{input}: {not:?}"
            );
            assert!(cleans_first(&b, &not.fix), "{not:?}");

            let b = built();
            place(
                &b,
                input,
                "# committed before\n",
                linked(&b) - Duration::from_secs(60),
            );
            let head = commit(&b.work, "a build input before the link");
            assert_eq!(judged(&b), Ok(head), "{input}");
        }
    }

    /// A build input there at the link and removed after it (by a commit checked out since, or by
    /// hand) refuses though nothing that is there changed: the folder that held it changed after
    /// the link, so it may have lost it. That holds for the cutter's own package manifest, gone
    /// from the folder no source's package root then names, and for a manifest above it
    /// (`crates/Cargo.toml`, where cargo looks for a workspace). One whose folder is unchanged
    /// since the link was not there when it was linked. NEGATIVE CONTROLS: check only the build
    /// inputs that are there, and each removal is judged the new HEAD's build; take the package
    /// roots from the sources alone, and the removed package manifest is.
    #[test]
    fn a_build_input_removed_after_the_link_refuses() {
        for input in [
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "crates/aterm-release/Cargo.toml",
            "crates/Cargo.toml",
        ] {
            let b = built();
            let at = linked(&b);
            place(
                &b,
                "crates/aterm-release/Cargo.toml",
                "# the package root's manifest\n",
                at - Duration::from_secs(60),
            );
            place(
                &b,
                input,
                "# committed before the link\n",
                at - Duration::from_secs(60),
            );
            commit(&b.work, "a build input before the link");
            git(&b.work, &["rm", "-q", input]);
            let head = commit(&b.work, "the build input removed");
            let gone = fs::canonicalize(&b.work).unwrap().join(input);
            let folder = gone
                .ancestors()
                .skip(1)
                .find(|folder| folder.exists())
                .unwrap()
                .to_path_buf();
            set_folder_modified(&folder, at + Duration::from_secs(1));
            let not = judged(&b).expect_err(input);
            let dated = format!("{} changed after this cutter was linked", folder.display());
            assert!(
                not.why.starts_with(&dated)
                    && not
                        .why
                        .ends_with("is not there: it may have been removed since"),
                "{input}: {not:?}"
            );
            assert!(cleans_first(&b, &not.fix), "{not:?}");
            set_folder_modified(&folder, at - Duration::from_secs(1));
            assert_eq!(judged(&b), Ok(head), "{input}, removed before the link");
        }
    }

    /// A compiled input read through a symbolic link names the link and the file it leads to,
    /// each held to git and to the link: a tracked link, to the file or to a folder on its way,
    /// retargeted (uncommitted) to another tracked one refuses by the link's name, though what it
    /// leads to now is tracked and clean; a link made again after the link (a commit checked out
    /// since that retargets it) refuses as changed, though every file is older than the link.
    /// Links committed and untouched, or put back, are HEAD's. NEGATIVE CONTROLS: name each input
    /// by the file it ends at alone, and the retargeted link is HEAD's build; date each link by
    /// the file it leads to, and the link made again is.
    #[cfg(unix)]
    #[test]
    fn a_compiled_input_read_through_a_link_names_the_link_and_its_file() {
        let b = built();
        let old = linked(&b) - Duration::from_secs(60);
        for rel in ["docs/a.md", "docs/b.md", "gen/one/x.rs", "gen/two/x.rs"] {
            place(&b, rel, "committed\n", old);
        }
        let link = |target: &str, rel: &str| {
            let at = b.work.join(rel);
            drop(fs::remove_file(&at));
            std::os::unix::fs::symlink(target, at).unwrap();
        };
        link("a.md", "docs/link.md");
        link("one", "gen/now");
        let head = commit(&b.work, "a link to a file and one to a folder");
        compiled(&b, &["docs/link.md", "gen/now/x.rs"]);
        assert_eq!(judged(&b), Ok(head.clone()), "committed links");

        for (target, was, rel) in [("b.md", "a.md", "docs/link.md"), ("two", "one", "gen/now")] {
            link(target, rel);
            let not = judged(&b).expect_err(rel);
            assert!(
                not.why
                    .contains(&format!("uncommitted cutter sources ({rel})")),
                "{rel}: {not:?}"
            );
            link(was, rel);
            assert_eq!(judged(&b), Ok(head.clone()), "{rel} put back");
        }

        link("b.md", "docs/link.md");
        commit(&b.work, "the link retargeted");
        let past = SystemTime::now() - Duration::from_secs(60 * 60);
        backdate(&b.work, past - Duration::from_secs(60));
        set_modified(&b.exe, past);
        compiled(&b, &["docs/link.md"]);
        let not = judged(&b).expect_err("a link made again after the link");
        assert!(
            not.why
                .ends_with("docs/link.md changed after this cutter was linked"),
            "{not:?}"
        );
    }

    /// A build input that is a symbolic link, or is read through one (`.cargo` a link to a
    /// folder), is held as a compiled input read through a link is: the file it leads to, edited
    /// and not committed, refuses by that file's name; a commit checked out after the link that
    /// retargets the link refuses by the link's name, though every file is older than the link
    /// and the checkout is clean. Committed and untouched, or put back, it is HEAD's. NEGATIVE
    /// CONTROLS: date each link by the file it leads to, and each retargeted link is the new
    /// HEAD's build; hold to git only a build input's own name, and each edited file is HEAD's.
    #[cfg(unix)]
    #[test]
    fn a_build_input_read_through_a_link_is_held_to_the_link_and_its_file() {
        for (rel, was, now, read, other) in [
            (
                "rust-toolchain.toml",
                "toolchains/a.toml",
                "toolchains/b.toml",
                "toolchains/a.toml",
                "toolchains/b.toml",
            ),
            (
                ".cargo",
                "cfgs/a",
                "cfgs/b",
                "cfgs/a/config.toml",
                "cfgs/b/notes.md",
            ),
        ] {
            let b = built();
            let old = linked(&b) - Duration::from_secs(60);
            place(&b, read, "committed\n", old);
            place(&b, other, "committed\n", old);
            std::os::unix::fs::symlink(was, b.work.join(rel)).unwrap();
            let head = commit(&b.work, "a build input read through a link");
            assert_eq!(judged(&b), Ok(head.clone()), "{rel} committed");

            place(&b, read, "edited, not committed\n", old);
            let not = judged(&b).expect_err(read);
            assert!(
                not.why
                    .contains(&format!("uncommitted cutter sources ({read})")),
                "{rel}: {not:?}"
            );
            place(&b, read, "committed\n", old);
            assert_eq!(judged(&b), Ok(head), "{read} put back");

            fs::remove_file(b.work.join(rel)).unwrap();
            std::os::unix::fs::symlink(now, b.work.join(rel)).unwrap();
            commit(&b.work, "the link retargeted");
            let past = SystemTime::now() - Duration::from_secs(60 * 60);
            backdate(&b.work, past - Duration::from_secs(60));
            set_modified(&b.exe, past);
            let not = judged(&b).expect_err(rel);
            assert!(
                not.why
                    .ends_with(&format!("{rel} changed after this cutter was linked")),
                "{rel}: {not:?}"
            );
            assert!(cleans_first(&b, &not.fix), "{not:?}");
        }
    }

    /// A compiled input read through a tracked link that leaves the checkout and comes back into
    /// it refuses, though the link and the file it ends at are tracked and clean: no commit holds
    /// the hop outside, so retargeting it would change what the cutter reads unseen. A link that
    /// stays inside is HEAD's. NEGATIVE CONTROL: refuse only an input that ends outside the
    /// checkout, and the link out and back is HEAD's build.
    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_checkout_and_back_refuses() {
        let b = built();
        place(
            &b,
            "docs/a.md",
            "committed\n",
            linked(&b) - Duration::from_secs(60),
        );
        let hop = b.work.with_file_name("hop.md");
        std::os::unix::fs::symlink(b.work.join("docs/a.md"), &hop).unwrap();
        std::os::unix::fs::symlink("a.md", b.work.join("docs/in.md")).unwrap();
        std::os::unix::fs::symlink("../../hop.md", b.work.join("docs/out.md")).unwrap();
        let head = commit(&b.work, "a link inside and one out and back");
        compiled(&b, &["docs/in.md"]);
        assert_eq!(judged(&b), Ok(head), "a link that stays inside");
        compiled(&b, &["docs/out.md"]);
        let not = judged(&b).expect_err("a link out of the checkout and back");
        assert!(
            not.why.contains("docs/out.md, which resolves outside")
                && not.why.contains("or leaves it on the way"),
            "{not:?}"
        );
    }

    /// A compiled input no commit holds refuses: one `.gitignore` hides, in the checkout or in a
    /// submodule of it, one untracked outside the cutter's source folders, and one a link takes
    /// outside the checkout; so does a tracked one changed since `HEAD` outside them, named
    /// through `..`. A tracked one in a submodule or named through `..` is the commit's.
    /// NEGATIVE CONTROL: leave the dep-info's own inputs out of the git checks, and the ignored
    /// `gen/out.rs` is `HEAD`'s build.
    #[test]
    fn a_compiled_input_no_commit_holds_refuses() {
        let b = built();
        let old = linked(&b) - Duration::from_secs(60);
        let sub = b.work.with_file_name("sub");
        testing::init(
            &sub,
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        fs::write(sub.join(".gitignore"), "gen/\n").unwrap();
        fs::write(sub.join("lib.rs"), "pub fn f() {}\n").unwrap();
        commit(&sub, "the submodule");
        let url = sub.to_str().unwrap();
        let file = "protocol.file.allow=always";
        git(
            &b.work,
            &["-c", file, "submodule", "add", "-q", url, "vendor/sub"],
        );
        fs::write(b.work.join(".gitignore"), "gen/\n").unwrap();
        place(&b, "docs/included.md", "committed\n", old);
        let head = commit(&b.work, "a submodule, an ignore rule, a document");
        set_modified(&b.work.join("vendor/sub/lib.rs"), old);
        let through = "crates/aterm-release/../../docs/included.md";
        compiled(&b, &["vendor/sub/lib.rs", "docs/included.md", through]);
        assert_eq!(
            judged(&b),
            Ok(head),
            "tracked inputs, one in a submodule and one named through `..`"
        );

        place(&b, "gen/out.rs", "generated\n", old);
        place(&b, "vendor/sub/gen/out.rs", "generated\n", old);
        place(&b, "loose.rs", "untracked\n", old);
        for (input, words) in [
            ("gen/out.rs", "reads gen/out.rs, which"),
            (
                "vendor/sub/gen/out.rs",
                "reads vendor/sub/gen/out.rs, which",
            ),
            ("loose.rs", "reads loose.rs, which"),
        ] {
            compiled(&b, &["vendor/sub/lib.rs", input]);
            let not = judged(&b).expect_err(input);
            assert!(
                not.why.contains(words) && not.why.contains("does not track"),
                "{input}: {not:?}"
            );
            assert!(
                not.fix.starts_with(&format!("commit {input} in ")),
                "{not:?}"
            );
        }

        #[cfg(unix)]
        {
            let outside = b.work.with_file_name("outside.rs");
            fs::write(&outside, "no commit holds this\n").unwrap();
            set_modified(&outside, old);
            std::os::unix::fs::symlink(&outside, b.work.join("crates/outside.rs")).unwrap();
            compiled(&b, &["crates/outside.rs"]);
            let not = judged(&b).expect_err("an input outside the checkout");
            assert!(
                not.why
                    .contains("crates/outside.rs, which resolves outside"),
                "{not:?}"
            );
            fs::remove_file(b.work.join("crates/outside.rs")).unwrap();
        }

        place(&b, "docs/included.md", "edited, not committed\n", old);
        compiled(&b, &[through]);
        let not = judged(&b).expect_err("an edited input");
        assert!(
            not.why
                .contains("uncommitted cutter sources (docs/included.md)"),
            "{not:?}"
        );
    }

    #[test]
    fn a_foreign_dep_info_refuses() {
        let b = built();
        fs::write(
            b.exe.with_extension("d"),
            "/elsewhere/aterm-release: /x.rs\n",
        )
        .unwrap();
        let not = judged(&b).expect_err("foreign");
        assert!(not.why.contains("does not describe"), "{not:?}");
        assert_eq!(not.fix, RELINK);
    }

    #[test]
    fn a_verdict_is_never_taken_after_the_start() {
        let not = running().expect_err("never judged at its start");
        assert_eq!(not.fix, RELINK);
    }

    #[test]
    fn the_clean_remedy_names_the_profile_the_binary_sits_in() {
        let flag = |dir: &str| profile_flag(&Path::new("/t").join(dir).join("aterm-release"));
        assert_eq!(flag("debug"), "");
        assert_eq!(flag("release"), " --release");
        assert_eq!(flag("gate"), " --profile gate");
    }

    #[test]
    fn dep_info_words_keep_escaped_spaces_and_name_the_checkout() {
        assert_eq!(
            split_escaped("/a\\ b/t: /a\\ b/x.rs  /c.rs"),
            ["/a b/t:", "/a b/x.rs", "/c.rs"]
        );
        let main = PathBuf::from("/r/s").join(OWN_MAIN);
        assert_eq!(
            checkout_of(&[PathBuf::from("/x.rs"), main]),
            Some(PathBuf::from("/r/s"))
        );
        let kept = checkout_sources(
            vec![
                PathBuf::from("/r/s/crates/a.rs"),
                PathBuf::from("/r/s/target/debug/build/x/out/gen.rs"),
                PathBuf::from("/cache/build/x/out/gen.rs"),
                PathBuf::from("/r/s/.git/HEAD"),
            ],
            Path::new("/r/s"),
            &[Path::new("/r/s/target")],
        );
        assert_eq!(kept, [PathBuf::from("/r/s/crates/a.rs")]);
        assert!(is_source_input("crates/a/b.rs") && is_source_input("Cargo.lock"));
        assert!(!is_source_input("cratesx/a.rs") && !is_source_input("docs/a.md"));
        assert!(
            dep_info_sources(
                "/missing-other-output: /source.rs",
                Path::new("/missing-output")
            )
            .is_none()
        );
        assert!(
            dep_info_sources("/missing-output: /source.rs", Path::new("/missing-output")).is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn checkout_root_aliases_keep_generated_outputs_out_and_real_inputs_in() {
        use std::os::unix::fs::symlink;
        let b = built();
        let alias = b._dir.path().join("checkout-alias");
        symlink(&b.work, &alias).unwrap();
        let output = b.work.join("artifact-output/gate/aterm-release");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, "linked").unwrap();
        set_modified(&output, linked(&b));
        let generated = alias.join("artifact-output/gate/build/fixture/out/generated.rs");
        fs::create_dir_all(generated.parent().unwrap()).unwrap();
        fs::write(&generated, "generated").unwrap();
        let write_deps = |extra: &Path| {
            let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
            fs::write(
                output.with_extension("d"),
                format!(
                    "{}: {} {}\n",
                    escaped(&output),
                    escaped(&alias.join(OWN_MAIN)),
                    escaped(extra)
                ),
            )
            .unwrap();
        };
        let verdict = || judge(&output, Since::Link(linked(&b)));
        write_deps(&generated);
        assert_eq!(verdict(), Ok(b.head.clone()));
        let untracked = b.work.join("crates/aterm-release/src/untracked.rs");
        fs::write(&untracked, "real input").unwrap();
        write_deps(&untracked);
        let refused = verdict().expect_err("canonical-spelled real source must still be attested");
        assert!(refused.why.contains("untracked.rs"), "{refused:?}");
        fs::remove_file(untracked).unwrap();
        let source_alias = alias.join("crates/aterm-release/src/output-alias.rs");
        symlink(&generated, &source_alias).unwrap();
        git(
            &b.work,
            &["add", "--", "crates/aterm-release/src/output-alias.rs"],
        );
        git(&b.work, &["commit", "-qm", "tracked input link"]);
        write_deps(&source_alias);
        assert!(
            verdict().is_err(),
            "a source link into generated output is still an input"
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_the_matched_binary_output_namespace_may_use_a_target_directory_alias() {
        use std::os::unix::fs::symlink;
        let b = built();
        let physical = b._dir.path().join("build-output");
        let alias = b.work.join("output-alias");
        fs::create_dir_all(physical.join("gate/build/fixture/out")).unwrap();
        symlink(&physical, &alias).unwrap();
        let output = physical.join("gate/aterm-release");
        fs::write(&output, "linked").unwrap();
        set_modified(&output, linked(&b));
        let generated = alias.join("gate/build/fixture/out/generated.rs");
        fs::write(&generated, "generated").unwrap();
        let write_deps = |extra: &Path| {
            let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
            fs::write(
                output.with_extension("d"),
                format!(
                    "{}: {} {}\n",
                    escaped(&alias.join("gate/aterm-release")),
                    escaped(&b.work.join(OWN_MAIN)),
                    escaped(extra)
                ),
            )
            .unwrap();
        };
        let verdict = || judge(&output, Since::Link(linked(&b)));
        write_deps(&generated);
        assert_eq!(verdict(), Ok(b.head.clone()));
        let source_alias = b.work.join("crates/aterm-release/src/aliased-input.rs");
        symlink(&generated, &source_alias).unwrap();
        git(
            &b.work,
            &["add", "--", "crates/aterm-release/src/aliased-input.rs"],
        );
        git(&b.work, &["commit", "-qm", "tracked input link"]);
        write_deps(&source_alias);
        assert!(
            verdict().is_err(),
            "a non-output source link must not be canonicalized away"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_third_checkout_alias_cannot_hide_untracked_or_removed_inputs() {
        use std::os::unix::fs::symlink;
        let b = built();
        let alias = b._dir.path().join("third-checkout-alias");
        symlink(&b.work, &alias).unwrap();
        let old = linked(&b) - Duration::from_secs(60);
        place(&b, "fixtures/material.txt", "compiled input", old);
        let write_deps = |extra: &Path| {
            let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
            fs::write(
                b.exe.with_extension("d"),
                format!(
                    "{}: {} {}\n",
                    escaped(&b.exe),
                    escaped(&b.work.join(OWN_MAIN)),
                    escaped(extra)
                ),
            )
            .unwrap();
        };
        let input = alias.join("fixtures/material.txt");
        write_deps(&input);
        let untracked = judged(&b).expect_err("an alias must not hide an untracked input");
        assert!(
            untracked.why.contains("fixtures/material.txt"),
            "{untracked:?}"
        );
        let head = commit(&b.work, "tracked compiled fixture");
        assert_eq!(judged(&b), Ok(head.clone()));

        let external = b._dir.path().join("external-dependency.rs");
        fs::write(&external, "outside dependency").unwrap();
        write_deps(&external);
        assert_eq!(judged(&b), Ok(head));

        write_deps(&input);
        fs::remove_file(b.work.join("fixtures/material.txt")).unwrap();
        set_folder_modified(
            &b.work.join("fixtures"),
            linked(&b) + Duration::from_secs(1),
        );
        let removed = judged(&b).expect_err("a missing aliased leaf must remain an input");
        assert!(removed.why.contains("material.txt"), "{removed:?}");
    }

    #[cfg(unix)]
    fn placed_at(b: &Built) -> SystemTime {
        placed(&fs::metadata(&b.exe).unwrap()).expect("a status change time")
    }

    /// A dependency crate's source edited while the `targo run` that ran this process was
    /// building it (after that `targo` began, so after the unit that read it may have started,
    /// and before the link) and committed refuses: a cutter whose executable was placed after
    /// the `targo` that ran it began is judged from that start, not from its link. A run that
    /// began after the edit is the new HEAD's build. A cutter no `targo` ran, or one placed
    /// before its `targo` began, is judged from what it was handed, else its link. NEGATIVE
    /// CONTROL: judge a launched cutter from its link, as before, and the edited source is the
    /// new HEAD's build.
    #[cfg(unix)]
    #[test]
    fn a_source_edited_while_the_launching_run_built_refuses() {
        let b = built();
        let began = placed_at(&b) - Duration::from_secs(60 * 60);
        let before = began - Duration::from_secs(60);
        let lib = "crates/lib/src/lib.rs";
        place(&b, "crates/lib/Cargo.toml", "[package]\n", before);
        place(&b, lib, "OLD RULES\n", before);
        let head = commit(&b.work, "a dependency crate");
        compiled(&b, &[lib]);
        backdate(&b.work, before);
        let run = |launcher, handed| {
            let own = Some(placed_at(&b));
            judge_process(&b.exe, launcher, own, handed)
        };
        assert_eq!(run(Some(began), None).stamp, Ok(head));

        place(&b, lib, "NEW RULES\n", began + Duration::from_secs(60));
        let head = commit(&b.work, "new rules, committed while the run built");
        let not = run(Some(began), None)
            .stamp
            .expect_err("edited while the run built");
        assert!(
            not.why.ends_with(&format!(
                "{lib} changed after the process of the `targo` that started this cutter was \
                 forked"
            )),
            "{not:?}"
        );
        assert_eq!(not.fix, RELINK);
        assert_eq!(run(Some(began), None).since, Some(Since::Launch(began)));
        let later = began + Duration::from_secs(120);
        assert_eq!(
            run(Some(later), None).stamp,
            Ok(head.clone()),
            "a run that began after the edit"
        );

        let placed_before = Some(placed_at(&b) + Duration::from_secs(1));
        for launcher in [None, placed_before] {
            assert_eq!(run(launcher, None).since, Some(Since::Link(linked(&b))));
            let handed = run(launcher, Some(began));
            assert_eq!(handed.since, Some(Since::Handed(began)));
            let not = handed.stamp.expect_err("handed");
            assert!(
                not.why.ends_with(&format!(
                    "{lib} changed after the moment the cutter that handed this one on was \
                     judged from"
                )),
                "{not:?}"
            );
            assert_eq!(run(launcher, Some(later)).stamp, Ok(head.clone()));
        }
    }

    /// A `targo run` that found the cutter fresh and left the link alone judges it from that
    /// `targo`'s start, so a merge since the link that changed only what cargo relinks no unit
    /// of it for (the lockfile, the `crates` folder's entries, the checkout's) leaves it the new
    /// HEAD's build. A cutter run directly is judged from its link and refuses for the lockfile,
    /// and its remedy cleans the build first, so a run judged from its link again relinks it.
    /// NEGATIVE CONTROL: judge a launched cutter from its link, as before, and the lockfile
    /// refuses on every run.
    #[cfg(unix)]
    #[test]
    fn a_launched_run_is_judged_from_its_start_so_a_change_cargo_ignores_is_heads() {
        let b = built();
        let link = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        set_modified(&b.exe, link);
        place(&b, "crates/aterm-release/Cargo.toml", "[package]\n", link);
        place(&b, "Cargo.lock", "# before the merge\n", link);
        commit(&b.work, "the cutter's manifest and lockfile");
        backdate(&b.work, link - Duration::from_secs(60));
        let merged = link + Duration::from_secs(30 * 60);
        place(&b, "Cargo.lock", "# the merge's\n", merged);
        place(&b, "crates/other/Cargo.toml", "[package]\n", merged);
        place(&b, "crates/other/src/lib.rs", "\n", merged);
        let head = commit(&b.work, "a merge that adds a crate the cutter does not use");
        for folder in [b.work.clone(), b.work.join("crates")] {
            set_folder_modified(&folder, merged);
        }
        let began = link + Duration::from_secs(60 * 60);
        assert!(
            placed_at(&b) >= began,
            "the run placed it after its targo began"
        );
        let own = Some(placed_at(&b));
        let launched = judge_process(&b.exe, Some(began), own, None);
        assert_eq!(launched.stamp, Ok(head));
        assert_eq!(launched.since, Some(Since::Launch(began)));

        let not = judged(&b).expect_err("judged from its link");
        assert!(
            not.why
                .ends_with("Cargo.lock changed after this cutter was linked"),
            "{not:?}"
        );
        assert!(cleans_first(&b, &not.fix), "{not:?}");
    }

    /// A folder between a source and its package root holds no manifest cargo reads, so one
    /// dated after the link, by an update to a file the build never compiles, is no build input;
    /// a folder above the package root still is. NEGATIVE CONTROL: take each source's manifest
    /// candidates from its own folder up, and the dated `src` folder refuses.
    #[cfg(unix)]
    #[test]
    fn a_folder_below_a_package_root_is_no_build_input() {
        let b = built();
        let at = linked(&b);
        place(
            &b,
            "crates/aterm-release/Cargo.toml",
            "[package]\n",
            at - Duration::from_secs(60),
        );
        place(
            &b,
            "crates/aterm-release/src/linux.rs",
            "// not compiled here\n",
            at + Duration::from_secs(1),
        );
        let head = commit(
            &b.work,
            "a file the build never compiles, updated after the link",
        );
        set_folder_modified(
            &b.work.join("crates/aterm-release/src"),
            at + Duration::from_secs(1),
        );
        assert_eq!(judged(&b), Ok(head));

        set_folder_modified(&b.work.join("crates"), at + Duration::from_secs(1));
        let not = judged(&b).expect_err("the folder above the package root");
        assert!(
            not.why
                .ends_with("crates/Cargo.toml is not there: it may have been removed since"),
            "{not:?}"
        );
    }

    /// The remedy for a stale input follows the moment the cutter was judged from: the run alone
    /// for one judged from its `targo`'s start, which the next run is judged from again; the
    /// build cleaned first for one judged from its link or a handed moment, as the next run may
    /// be judged from its link (a start unread, a binary not placed between its `targo`'s start
    /// and its own), which cargo moves only for a change it tracks. NEGATIVE CONTROL: choose the
    /// remedy by whether this platform reads a process's start, and a cutter judged from its
    /// link on macOS is told to run again alone, the refusal it would meet on every run.
    #[test]
    fn a_stale_input_names_a_clean_unless_judged_from_a_targos_start() {
        let b = built();
        let at = linked(&b);
        place(
            &b,
            "Cargo.lock",
            "# after the link\n",
            at + Duration::from_secs(1),
        );
        commit(&b.work, "a lockfile after the link");
        let before = at - Duration::from_secs(60);
        for since in [Since::Link(at), Since::Handed(before)] {
            let not = judge(&b.exe, since).expect_err("stale");
            assert!(cleans_first(&b, &not.fix), "{since:?}: {not:?}");
        }
        let not = judge(&b.exe, Since::Launch(before)).expect_err("stale");
        assert_eq!(not.fix, RELINK, "{not:?}");
    }

    /// The moment a cutter is judged from: the start of the `targo` that ran it when its
    /// executable was placed after that start and before this process's own (to the grain the
    /// kernel records a start in), whatever it was handed; else what it was handed; else its
    /// link. NEGATIVE CONTROL: drop the bound by this process's start, and a binary placed after
    /// the process started is judged from that `targo`'s start.
    #[test]
    fn a_cutter_is_judged_from_its_launch_else_its_hand_off_else_its_link() {
        let t = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        let us = Duration::from_micros(1);
        let (link, placed, handed) = (t(100), t(200), t(150));
        let launched =
            |began, own, handed| since(link, Some(placed), Some(began), Some(own), handed);
        assert_eq!(
            launched(t(190), t(210), Some(handed)),
            Since::Launch(t(190))
        );
        assert_eq!(launched(t(200), t(200), None), Since::Launch(t(200)));
        assert_eq!(
            launched(t(190), t(200) - us / 2, None),
            Since::Launch(t(190)),
            "placed within the grain of its recorded start"
        );
        for (placed, began, own, what) in [
            (
                Some(placed),
                Some(t(210)),
                Some(t(220)),
                "placed before its targo began",
            ),
            (
                Some(placed),
                Some(t(190)),
                Some(t(200) - us),
                "placed after this process started",
            ),
            (
                Some(placed),
                Some(t(190)),
                Some(t(199)),
                "placed after this process started",
            ),
            (None, Some(t(190)), Some(t(210)), "no placement read"),
            (Some(placed), None, Some(t(210)), "no targo ran it"),
            (Some(placed), Some(t(190)), None, "its own start unread"),
        ] {
            assert_eq!(
                since(link, placed, began, own, Some(handed)),
                Since::Handed(handed),
                "{what}"
            );
            assert_eq!(
                since(link, placed, began, own, None),
                Since::Link(link),
                "{what}"
            );
        }
    }

    /// A process's `targo` start is its parent's only when the parent is the program `$CARGO`
    /// names, spelled through a link or not: a parent that is another program (one that
    /// inherited `$CARGO` and started the cutter itself) gives no Launch moment, and the cutter
    /// is judged from its link. NEGATIVE CONTROL: take the parent's start whatever program it
    /// runs, and the other program's start is the cutter's Launch moment.
    #[test]
    fn a_parent_that_is_not_the_program_cargo_names_gives_no_launch() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let bin = dir.path().join("store/bin");
        fs::create_dir_all(&bin).unwrap();
        let targo = bin.join("targo");
        let other = dir.path().join("other");
        fs::write(&targo, "targo").unwrap();
        fs::write(&other, "another program").unwrap();
        let began = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_520_000);
        let cargo = Some(targo.as_os_str());
        assert_eq!(launcher(Some(&targo), cargo, Some(began)), Some(began));
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(&bin, &alias).unwrap();
            let named = alias.join("targo");
            assert_eq!(
                launcher(Some(&targo), Some(named.as_os_str()), Some(began)),
                Some(began),
                "$CARGO through a link"
            );
            assert_eq!(launcher(Some(&named), cargo, Some(began)), Some(began));
        }
        let gone = dir.path().join("gone");
        for (parent, named, start, what) in [
            (Some(other.as_path()), cargo, Some(began), "another program"),
            (None, cargo, Some(began), "its parent's executable unread"),
            (Some(targo.as_path()), None, Some(began), "no $CARGO"),
            (
                Some(targo.as_path()),
                Some(gone.as_os_str()),
                Some(began),
                "$CARGO names nothing",
            ),
            (
                Some(targo.as_path()),
                cargo,
                None,
                "its parent's start unread",
            ),
        ] {
            assert_eq!(launcher(parent, named, start), None, "{what}");
        }
        let (linked, placed, own) = (
            began - Duration::from_secs(60),
            began + Duration::from_secs(1),
            began + Duration::from_secs(2),
        );
        let judged = |parent: &Path| {
            since(
                linked,
                Some(placed),
                launcher(Some(parent), cargo, Some(began)),
                Some(own),
                None,
            )
        };
        assert_eq!(judged(&targo), Since::Launch(began));
        assert_eq!(
            judged(&other),
            Since::Link(linked),
            "another program's start"
        );
    }

    /// The kernel readers the hand-off and the run claim stand on: this process's ancestors
    /// start at its parent, an ancestor's start is its record's and its image the inode of the
    /// file it runs, and this process is no ancestor of itself. The test reruns itself as a
    /// child, so the ancestor it reads runs this test binary, a file nothing relinks while it
    /// runs; its own launcher's can be (`verify`'s test compile relinks the `aterm-verify` it
    /// runs from). The launcher, which runs another file, shows the image read is the named
    /// ancestor's and not the caller's.
    #[cfg(target_os = "macos")]
    #[test]
    fn this_processs_ancestors_start_at_its_parent() {
        const PROBE: &str = "ATERM_RELEASE_ANCESTOR_PROBE";
        const NAME: &str = "cutter::tests::this_processs_ancestors_start_at_its_parent";
        let Some(parent_exe) = std::env::var_os(PROBE) else {
            let exe = std::env::current_exe().expect("this test executable");
            let mut probe = aterm_uds::spawnfd::command(&exe);
            probe
                .args([NAME, "--exact", "--nocapture", "--test-threads=1"])
                .env(PROBE, &exe);
            // A one-minute hang detector, not a latency budget.
            let out = aterm_uds::deadline::output_within(&mut probe, Duration::from_secs(60))
                .expect("the probe ran");
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && said.contains("ancestor probe held"),
                "{out:?}"
            );
            return;
        };
        let parent = std::os::unix::process::parent_id();
        let lineage = ancestors();
        assert_eq!(lineage.first(), Some(&parent), "{lineage:?}");
        let found = ancestor(parent).expect("the parent is an ancestor");
        assert_eq!(Some(found.start), started(parent));
        assert!(found.image.is_some(), "the parent's image reads");
        assert_eq!(found.image, inode(Path::new(&parent_exe)));
        let grand = *lineage.get(1).expect("a grandparent");
        let above = ancestor(grand).expect("the grandparent is an ancestor");
        assert_ne!(above.image, found.image, "the launcher runs another file");
        assert_eq!(ancestor(std::process::id()), None);
        println!("ancestor probe held");
    }

    /// A handed moment is read only beside a variable that names this executable, spelled
    /// through a link or not, as [`since_value`] writes it, and only from the process it names
    /// as the one that handed it when that is a live ancestor of this one, started when the value
    /// says and running this very file. NEGATIVE CONTROLS: take the moment whatever process it
    /// names, and one no ancestor handed is read; skip the image check, and an ancestor running
    /// another executable hands its moment on.
    #[cfg(unix)]
    #[test]
    fn a_handed_moment_is_read_only_for_this_executable() {
        let b = built();
        let at = SystemTime::UNIX_EPOCH + Duration::new(1_793_520_000, 7);
        let start = SystemTime::UNIX_EPOCH + Duration::new(1_793_519_000, 5_000);
        let value = since_value(at, 4242, start).unwrap();
        assert_eq!(value, "1793520000000000007 4242 1793519000000005000");
        let image = inode(&b.exe);
        assert!(image.is_some());
        let lineage = |pid: u32| (pid == 4242).then_some(Ancestor { start, image });
        let read = |named: Option<&Path>, since: Option<&str>| {
            handed_since(
                named.map(Path::as_os_str),
                since.map(OsStr::new),
                &b.exe,
                &lineage,
            )
        };
        assert_eq!(read(Some(&b.exe), Some(&value)), Some(at));
        let lane = b.exe.parent().unwrap().with_file_name("lane");
        std::os::unix::fs::symlink(b.exe.parent().unwrap(), &lane).unwrap();
        assert_eq!(
            read(Some(&lane.join("aterm-release")), Some(&value)),
            Some(at)
        );
        let other = b.exe.with_file_name("other");
        fs::write(&other, "linked").unwrap();
        let by = |pid: u32, start| since_value(at, pid, start).unwrap();
        let (gone, reused) = (by(4343, start), by(4242, start + Duration::from_secs(1)));
        for (named, since, what) in [
            (
                Some(other.as_path()),
                Some(value.as_str()),
                "another executable",
            ),
            (None, Some(value.as_str()), "no cutter named"),
            (Some(b.exe.as_path()), None, "no moment"),
            (
                Some(b.exe.as_path()),
                Some("1793520000000000007"),
                "no process that handed it",
            ),
            (
                Some(b.exe.as_path()),
                Some("1793520000.5 4242 1793519000000005000"),
                "not nanoseconds",
            ),
            (
                Some(b.exe.as_path()),
                Some(gone.as_str()),
                "a moment no ancestor handed",
            ),
            (
                Some(b.exe.as_path()),
                Some(reused.as_str()),
                "an ancestor that started at another time",
            ),
        ] {
            assert_eq!(read(named, since), None, "{what}");
        }
        let elsewhere = |pid: u32| {
            (pid == 4242).then_some(Ancestor {
                start,
                image: inode(&other),
            })
        };
        assert_eq!(
            handed_since(
                Some(b.exe.as_os_str()),
                Some(OsStr::new(&value)),
                &b.exe,
                &elsewhere
            ),
            None,
            "an ancestor running another executable"
        );
        assert_eq!(
            since_value(SystemTime::UNIX_EPOCH - Duration::from_secs(1), 4242, start),
            None
        );
    }
}
