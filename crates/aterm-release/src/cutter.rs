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

/// Provenance of the running cutter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stamp {
    /// Cargo's dep-info and freshness judgment binds this image to the commit.
    Built(String),
    /// A content-addressed frozen-tool record binds this exact image and a clean clone to R.
    Proved(String),
}

impl Stamp {
    #[must_use]
    pub fn commit(&self) -> &str {
        match self {
            Self::Built(commit) | Self::Proved(commit) => commit,
        }
    }
}

type Verdict = Result<Stamp, NotOwn>;

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
    stamp: Verdict,
    since: Option<Since>,
}

static RUNNING: OnceLock<Judged> = OnceLock::new();

/// Judge this process. `main` calls it first, before any step moves a checkout.
pub fn judge_at_start() {
    RUNNING.get_or_init(|| match std::env::current_exe() {
        Ok(exe) => {
            match crate::release_tools::proved_running(&exe) {
                Ok(Some(commit)) => {
                    return Judged {
                        stamp: Ok(Stamp::Proved(commit)),
                        since: None,
                    };
                }
                Err(why) => {
                    return Judged {
                        stamp: Err(NotOwn::new(
                            format!("the frozen-tool proof is invalid ({why})"),
                            "from the clean release clone run `targo --unverified ship release` with the same arguments",
                        )),
                        since: None,
                    };
                }
                Ok(None) => {}
            }
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
        .map(Stamp::commit)
}

/// This process's complete startup provenance, including a frozen content proof.
pub fn running_stamp() -> Result<&'static Stamp, &'static NotOwn> {
    RUNNING
        .get_or_init(|| Judged {
            stamp: Err(NotOwn::new("it was not judged when it started", RELINK)),
            since: None,
        })
        .stamp
        .as_ref()
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
        stamp: judge(exe, since).map(Stamp::Built),
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
fn judge(exe: &Path, since: Since) -> Result<String, NotOwn> {
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
    let untracked = files
        .iter()
        .find(|path| !tracked.contains(*path))
        .or_else(|| {
            directories
                .iter()
                .find(|path| !is_tracked_directory(&tracked, path))
        });
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

pub(crate) fn declared_sources(text: &str) -> Result<Vec<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    for line in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (target, rest) = split_first_escaped(line).ok_or("unreadable compiler dep-info")?;
        if !target.ends_with(':') {
            return Err("unknown compiler dep-info rule".into());
        }
        paths.extend(split_escaped(rest).into_iter().map(PathBuf::from));
    }
    if paths.is_empty() {
        return Err("empty compiler source dep-info".into());
    }
    Ok(paths.into_iter().collect())
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
            entries.sort_unstable();
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
fn is_tracked_directory(files: &BTreeSet<String>, dir: &str) -> bool {
    let mut prefix = String::with_capacity(dir.len() + 1);
    prefix.push_str(dir);
    prefix.push('/');
    files
        .range::<str, _>((
            std::ops::Bound::Included(prefix.as_str()),
            std::ops::Bound::Unbounded,
        ))
        .next()
        .is_some_and(|first| first.starts_with(&prefix))
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
#[path = "cutter_tests.rs"]
mod tests;
