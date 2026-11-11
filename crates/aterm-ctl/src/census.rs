// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Census of this user's aterm processes read from the kernel process table.
#![cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "the census reads a Unix process table; elsewhere only its types and tests use the rest"
    )
)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use aterm_types::control_socket::{SocketDirective, socket_directive};
use aterm_uds::holders::Exe;
use aterm_uds::process::{Lineage, ProcessBirth};

use crate::Parent;

/// One instance no published socket reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreached {
    /// The process id.
    pub pid: u32,
    /// How long it has run, in words (`11d 22h`, `3h 05m`, `42s`).
    pub age: String,
    /// Its launch words, joined by spaces.
    pub command: String,
    /// Started with its control socket switched off (`--no-control-sock`, `--control-sock 0|off`).
    pub socketless: bool,
}

/// One aterm process of this user that is no window or headless instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// The process id.
    pub pid: u32,
    /// `aterm <verb>[ <sub>]` when the verb table names its verb, an alias's own name (`atpkg`),
    /// else its executable's file name.
    pub name: String,
    /// How long it has run, in words.
    pub age: String,
    /// Its launcher exited and launchd adopted it (`Parent::Adopted`).
    pub adopted: bool,
    /// Its executable sits under a build's `target*` directory.
    pub build: bool,
    /// The kernel says its executable's file was unlinked since it started.
    pub deleted: bool,
    /// A link broker launchd adopted that is not the one process found serving the fabric's
    /// socket: no supervisor keeps it ([`supervise`]).
    pub unsupervised: bool,
    /// The token `aterm ctl end` takes for it, where a witness proves it the leftover of a run
    /// ([`supervise`]).
    pub token: Option<String>,
}

/// The census's name of the fabric's link broker: an aterm image running `link broker`.
pub const BROKER: &str = "aterm link broker";

/// Marks each link broker of `census` that launchd adopted and that is not `serving` (the one
/// process the `fabric:` row's probe found holding the fabric's listener; `None` when the fabric
/// is off, since a probe that found no one holder says nothing and is never asked here)
/// unsupervised, with the token `offered` gives its pid: the `end=` token the leftover view
/// offers where a witness proves it, `None` otherwise.
pub fn supervise(
    census: &mut Census,
    serving: Option<u32>,
    offered: &dyn Fn(u32) -> Option<String>,
) {
    for program in census
        .programs
        .iter_mut()
        .filter(|p| p.adopted && p.name == BROKER && Some(p.pid) != serving)
    {
        program.unsupervised = true;
        program.token = offered(program.pid);
    }
}

/// Instance count, unreached instances, and other aterm programs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Census {
    /// The rendezvous directory that was read, for the words.
    pub dir: String,
    /// Window and headless instances of this user, reached or not.
    pub instances: usize,
    /// The ones no published socket names, oldest first.
    pub unreached: Vec<Unreached>,
    /// The other aterm processes of this user but this one, oldest first.
    pub programs: Vec<Program>,
}

/// What the front door does with `argv[1]` (`aterm <word>`): a verb, or a flag it answers before
/// it routes a window (`--version`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    /// Opens a window in this process when no running instance takes the request.
    Window,
    /// It runs as the program `aterm <verb>`, or answers and exits.
    Program,
}

/// The front door's routes, handed in by aterm-cli, whose tables they are.
#[derive(Clone, Copy)]
pub struct FrontDoor<'a> {
    /// What it does when started under `argv[0]`'s file name with `argv[1]`, before it reads
    /// `argv[1]` as a verb: a window alias (`aterm-gui`) is `Some(Window)`, an alias of another
    /// program (`atpkg`, `aterm-ctl`, `aterm-askpass`) `Some(Program)`, and its own names
    /// (`aterm`, `aterm-cli`, any other) `None`.
    pub alias: &'a dyn Fn(&str, &str) -> Option<Verb>,
    /// What it does with `argv[1]` under its own names (`aterm <word>`).
    pub verb: &'a dyn Fn(&str) -> Option<Verb>,
}

/// The executable image a process runs, as the kernel names it ([`image_of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Image {
    /// The path it was started from.
    pub(crate) path: PathBuf,
    /// The kernel marks that file unlinked since (Linux's ` (deleted)`).
    pub(crate) deleted: bool,
}

impl Image {
    /// The image whose path the kernel's reader gave as `path`.
    fn named(path: PathBuf) -> Self {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(path) = unmarked(&path) {
            return Self {
                path,
                deleted: true,
            };
        }
        Self {
            path,
            deleted: false,
        }
    }
}

/// `pid`'s image ([`aterm_uds::holders::exe_of`]).
pub(crate) fn image_of(pid: u32) -> Option<Image> {
    match aterm_uds::holders::exe_of(pid) {
        Exe::At(path) => Some(Image::named(path)),
        Exe::Unnamed | Exe::Exited => None,
    }
}

/// `link` without the ` (deleted)` Linux's `/proc/<pid>/exe` appends once its file is unlinked.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn unmarked(link: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = link.as_os_str().as_bytes().strip_suffix(b" (deleted)")?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

/// One process of this user, as the census's table lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) pid: u32,
    pub(crate) ppid: Option<u32>,
    pub(crate) birth: Option<ProcessBirth>,
    pub(crate) age_s: Option<u64>,
}

/// The kernel reads the census makes.
pub(crate) trait Kernel {
    /// This user's processes, or `None` when the process table cannot be read.
    fn listed(&self) -> Option<Vec<Listed>>;
    /// `pid`'s executable image ([`image_of`]).
    fn exe(&self, pid: u32) -> Option<Image>;
    /// Whether `pid`'s one thread has run ([`aterm_uds::process::thread_ran`]).
    fn ran(&self, pid: u32) -> Option<bool>;
    /// `pid`'s argument words ([`aterm_uds::process::read_process_argv`]).
    fn argv(&self, pid: u32) -> Option<Vec<OsString>>;
    /// `pid`'s lineage ([`aterm_uds::process::read_lineage`]), asked only when its parent is 1.
    fn lineage(&self, pid: u32) -> Option<Lineage>;
    /// Whether `pid`'s standard input is a terminal ([`aterm_uds::holders::fd_is_terminal`] of
    /// descriptor 0), the front door's own test, asked only when an aterm's route turns on it.
    fn stdin_terminal(&self, pid: u32) -> Option<bool>;
    /// launchd's pid version ([`aterm_uds::process::launchd_idversion`]).
    fn launchd_version(&self) -> Option<i32>;
}

/// The shipped [`Kernel`].
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LiveKernel;

#[cfg(unix)]
impl Kernel for LiveKernel {
    #[cfg(target_os = "macos")]
    fn listed(&self) -> Option<Vec<Listed>> {
        let records = aterm_sysprobe::kproc::records()?;
        let uid = records.iter().find(|r| r.pid == std::process::id())?.uid;
        let now = unix_now();
        Some(
            records
                .into_iter()
                .filter(|r| r.uid == uid && !r.zombie && r.pid > 0)
                .map(|r| Listed {
                    pid: r.pid,
                    ppid: Some(r.ppid),
                    birth: Some(ProcessBirth {
                        seconds: r.start_s,
                        microseconds: u64::from(r.start_us),
                    }),
                    age_s: now.and_then(|now| now.checked_sub(r.start_s)),
                })
                .collect(),
        )
    }

    #[cfg(not(target_os = "macos"))]
    fn listed(&self) -> Option<Vec<Listed>> {
        let clocks = crate::graph_hygiene::Clocks::default();
        let pids = aterm_uds::holders::uid_pids().ok()?;
        let read = |pid: u32| {
            let p = i32::try_from(pid).ok();
            Listed {
                pid,
                ppid: p.and_then(aterm_uds::process::read_process_parent),
                birth: p
                    .and_then(|p| crate::graph_hygiene::read_birth(p, &clocks))
                    .map(|b| b.at),
                age_s: None,
            }
        };
        Some(read_then_aged(pids, read, unix_now))
    }

    fn exe(&self, pid: u32) -> Option<Image> {
        image_of(pid)
    }

    fn ran(&self, pid: u32) -> Option<bool> {
        aterm_uds::process::thread_ran(i32::try_from(pid).ok()?)
    }

    fn argv(&self, pid: u32) -> Option<Vec<OsString>> {
        aterm_uds::process::read_process_argv(i32::try_from(pid).ok()?)
    }

    fn lineage(&self, pid: u32) -> Option<Lineage> {
        aterm_uds::process::read_lineage(i32::try_from(pid).ok()?)
    }

    fn launchd_version(&self) -> Option<i32> {
        aterm_uds::process::launchd_idversion()
    }

    fn stdin_terminal(&self, pid: u32) -> Option<bool> {
        aterm_uds::holders::fd_is_terminal(pid, 0)
    }
}

/// The rows `read` makes of `pids`, aged against `now`, which is read after every row.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn read_then_aged(
    pids: Vec<u32>,
    read: impl FnMut(u32) -> Listed,
    now: impl FnOnce() -> Option<u64>,
) -> Vec<Listed> {
    let rows: Vec<Listed> = pids.into_iter().map(read).collect();
    let now = now();
    rows.into_iter()
        .map(|row| Listed {
            age_s: now
                .zip(row.birth)
                .and_then(|(now, b)| now.checked_sub(b.seconds)),
            ..row
        })
        .collect()
}

/// Now, unix seconds.
#[cfg(unix)]
fn unix_now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Take the census of this machine, now.
///
/// # Errors
/// Returns an error if the rendezvous directory or process table cannot be read.
#[cfg(unix)]
pub fn window_census(door: FrontDoor<'_>) -> Result<Census, String> {
    let (outcome, targets) = crate::inspect_fleet();
    let targets = targets.into_iter().map(crate::Target::listed).collect();
    let (dir, published) = published_in(outcome, targets)?;
    census_of(&LiveKernel, std::process::id(), &published, dir, door)
}

/// Off Unix there is no process table read here; the census says so rather than guessing.
///
/// # Errors
/// Always: the census runs on macOS and Linux only.
#[cfg(not(unix))]
pub fn window_census(_door: FrontDoor<'_>) -> Result<Census, String> {
    Err("macOS and Linux only".to_string())
}

/// What the rendezvous walk says is published ([`crate::inspect_fleet`]).
///
/// # Errors
/// Returns an error if the rendezvous directory cannot be resolved or read.
#[cfg(any(unix, test))]
fn published_in(
    outcome: crate::DirOutcome,
    targets: Vec<(u32, String)>,
) -> Result<(String, BTreeSet<u32>), String> {
    let pids = |targets: Vec<(u32, String)>| {
        targets
            .into_iter()
            .map(|(pid, _)| pid)
            .filter(|pid| *pid != 0)
            .collect::<BTreeSet<u32>>()
    };
    match outcome {
        crate::DirOutcome::Found { path, .. } | crate::DirOutcome::Empty(path) => {
            Ok((path.display().to_string(), pids(targets)))
        }
        crate::DirOutcome::Missing { path, .. } => Ok((
            format!("{} (which does not exist)", path.display()),
            BTreeSet::new(),
        )),
        other => Err(crate::discovery_report(other, &[]).0),
    }
}

/// Computes the census over a kernel's table, this process's pid, and the published pids.
///
/// # Errors
/// The table cannot be read, or holds no row for `own_pid`, so it is not this user's.
pub(crate) fn census_of(
    k: &impl Kernel,
    own_pid: u32,
    published: &BTreeSet<u32>,
    dir: String,
    door: FrontDoor<'_>,
) -> Result<Census, String> {
    let listed = k
        .listed()
        .ok_or_else(|| "the process table could not be read".to_string())?;
    if !listed.iter().any(|row| row.pid == own_pid) {
        return Err("this process is missing from the process table".to_string());
    }
    let mut launchd = None;
    let mut instances = 0;
    let mut unreached: Vec<(u64, Unreached)> = Vec::new();
    let mut programs: Vec<(u64, Program)> = Vec::new();
    for row in listed.iter().filter(|row| row.pid != own_pid) {
        let Some((image, words)) = aterm_words(k, row.pid) else {
            continue;
        };
        let exe = image.path.as_path();
        let age = row.age_s.map_or_else(|| "?".to_string(), age_words);
        // A tether's watcher (`aterm_uds::tether`) is launchd's by design, for as long as the
        // helper it watches runs, and so is a build's claim holder (`aterm_uds::build_front`),
        // for as long as the build it holds memory for, or what that build left running, runs:
        // no window under any name, and not an orphan to report.
        let by_design = words
            .as_deref()
            .and_then(|words| words.get(1))
            .and_then(|verb| aterm_uds::left::by_design(verb));
        if let Some(name) = by_design {
            programs.push((
                row.age_s.unwrap_or(0),
                Program {
                    pid: row.pid,
                    name: name.to_string(),
                    age,
                    adopted: false,
                    build: in_build_dir(exe),
                    deleted: image.deleted,
                    unsupervised: false,
                    token: None,
                },
            ));
            continue;
        }
        let window = words
            .as_deref()
            .and_then(|words| window_args(exe, words, || k.stdin_terminal(row.pid), door));
        if let Some(args) = window {
            instances += 1;
            if !published.contains(&row.pid) {
                unreached.push((
                    row.age_s.unwrap_or(0),
                    Unreached {
                        pid: row.pid,
                        age,
                        command: launch_words(words.as_deref().unwrap_or_default(), args.len()),
                        socketless: socket_off(&args),
                    },
                ));
            }
            continue;
        }
        let (lineage, launchd) = if row.ppid == Some(1) {
            (
                k.lineage(row.pid),
                *launchd.get_or_insert_with(|| k.launchd_version()),
            )
        } else {
            (None, None)
        };
        let parent = Parent::of(row.ppid, lineage.as_ref(), row.birth, launchd);
        programs.push((
            row.age_s.unwrap_or(0),
            Program {
                pid: row.pid,
                name: program_name(exe, words.as_deref().unwrap_or_default(), door),
                age,
                adopted: parent == Parent::Adopted,
                build: in_build_dir(exe),
                deleted: image.deleted,
                unsupervised: false,
                token: None,
            },
        ));
    }
    unreached.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.pid.cmp(&b.1.pid)));
    programs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.pid.cmp(&b.1.pid)));
    Ok(Census {
        dir,
        instances,
        unreached: unreached.into_iter().map(|(_, u)| u).collect(),
        programs: programs.into_iter().map(|(_, p)| p).collect(),
    })
}

/// `pid`'s image and argument words when its executable is an aterm one ([`is_aterm`]): the
/// words (decoded lossily) are read only then, and only once its one thread has run
/// ([`Kernel::ran`]: a read of a program's arguments at its first instruction can hang, M29).
/// They are kept only while the kernel names the same image after the read, so an exec
/// between the two reads leaves no other program's words; such a process is no aterm. No read
/// is atomic with an exec, so one into a held program after the `ran` read can still hang the
/// argument read, which nothing here bounds. `None` for any other process, or one whose
/// executable the kernel no longer names; the words are `None` when its thread has not run, or
/// is unread, or they are unread.
fn aterm_words(k: &impl Kernel, pid: u32) -> Option<(Image, Option<Vec<String>>)> {
    let image = k.exe(pid).filter(|image| is_aterm(&image.path))?;
    let Some(argv) = (k.ran(pid) == Some(true)).then(|| k.argv(pid)).flatten() else {
        return Some((image, None));
    };
    if k.exe(pid)?.path != image.path {
        return None;
    }
    let words = argv
        .iter()
        .map(|w| w.to_string_lossy().into_owned())
        .collect();
    Some((image, Some(words)))
}

/// One aterm child of a process, as the census knows aterm: by its executable, its argument
/// words read only then and only once its thread has run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Child {
    /// The process id.
    pub pid: u32,
    /// Its argument words, `None` when they were not read (its thread had not run, or they are
    /// unread).
    pub words: Option<Vec<String>>,
}

/// This user's processes and their parents, as the census lists them, read once: no `ps`, and
/// no process's arguments.
pub struct Listing(Vec<Listed>);

impl Listing {
    /// The process table, now.
    ///
    /// # Errors
    /// The table cannot be read (and off macOS and Linux, always).
    pub fn read() -> Result<Self, String> {
        #[cfg(unix)]
        {
            LiveKernel
                .listed()
                .map(Self)
                .ok_or_else(|| "the process table could not be read".to_string())
        }
        #[cfg(not(unix))]
        {
            Err("macOS and Linux only".to_string())
        }
    }

    /// The aterm processes whose parent is `parent` ([`Child`]), in table order.
    #[must_use]
    pub fn aterm_children(&self, parent: u32) -> Vec<Child> {
        #[cfg(unix)]
        {
            children_in(&LiveKernel, &self.0, parent)
        }
        #[cfg(not(unix))]
        {
            let _ = parent;
            Vec::new()
        }
    }
}

/// The rows of `listed` whose parent is `parent` (no child of pid 0 or of launchd) that `k`
/// proves aterm ([`aterm_words`]), in table order.
pub(crate) fn children_in(k: &impl Kernel, listed: &[Listed], parent: u32) -> Vec<Child> {
    listed
        .iter()
        .filter(|row| parent > 1 && row.ppid == Some(parent))
        .filter_map(|row| {
            let (_, words) = aterm_words(k, row.pid)?;
            Some(Child {
                pid: row.pid,
                words,
            })
        })
        .collect()
}

/// The file name of `path`, when it is UTF-8.
fn file_name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

/// Whether `exe` is an aterm executable.
pub(crate) fn is_aterm(exe: &Path) -> bool {
    aterm_uds::left::is_aterm_image(exe)
}

/// Whether `exe` sits under a build's target directory.
fn in_build_dir(exe: &Path) -> bool {
    exe.ancestors()
        .skip(1)
        .filter_map(file_name)
        .any(|name| name.starts_with("target"))
}

/// `argv[0]`'s or the executable's file name, whichever the front door routes as an alias
/// ([`FrontDoor::alias`]), with that route.
fn alias_of<'a>(exe: &'a Path, argv: &'a [String], door: FrontDoor<'_>) -> Option<(&'a str, Verb)> {
    let first = argv.get(1).map_or("", String::as_str);
    [
        argv.first().and_then(|a| file_name(Path::new(a))),
        file_name(exe),
    ]
    .into_iter()
    .flatten()
    .find_map(|name| Some((name, (door.alias)(name, first)?)))
}

/// What a program is called: a build's front waiting for its claim (`atpkg __build_admit`,
/// which becomes its build once claimed) `atpkg build-admit`; an alias by its own name, or
/// `aterm <verb>` for an `aterm-<verb>`
/// alias whose verb the front door's table names; under the front door's own names,
/// `aterm <verb>` when the table names `argv[1]`; else its executable's file name. A verb is
/// followed by the next argument when that is a word too.
fn program_name(exe: &Path, argv: &[String], door: FrontDoor<'_>) -> String {
    if aterm_uds::build_front::front(argv).is_some() {
        return "atpkg build-admit".to_string();
    }
    let (verb, rest) = match alias_of(exe, argv, door) {
        Some((name, _)) => match name
            .strip_prefix("aterm-")
            .filter(|verb| is_word(verb) && (door.verb)(verb).is_some())
        {
            Some(verb) => (verb, argv.get(1..)),
            None => return exe_word(Path::new(name)),
        },
        None => match argv.get(1) {
            Some(word) if is_word(word) && (door.verb)(word).is_some() => {
                (word.as_str(), argv.get(2..))
            }
            _ => return exe_word(exe),
        },
    };
    match rest.and_then(<[String]>::first).filter(|w| is_word(w)) {
        Some(sub) => format!("aterm {verb} {sub}"),
        None => format!("aterm {verb}"),
    }
}

/// A word the census prints as a verb or sub-verb: `^[a-z][a-z0-9-]{0,23}$`.
fn is_word(word: &str) -> bool {
    let mut chars = word.chars();
    word.len() <= 24
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The executable's file name when it is a plain word (`^[A-Za-z0-9._+-]{1,32}$`), else `aterm?`.
fn exe_word(exe: &Path) -> String {
    file_name(exe)
        .filter(|name| {
            (1..=32).contains(&name.len())
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        })
        .unwrap_or("aterm?")
        .to_string()
}

/// The launch flags of an aterm process when it is a window or headless instance, else `None`.
/// The front door routes it: an alias by its route; another image than `aterm` (a dev tool
/// such as `aterm-verify`) is its own program; else as `crates/aterm`'s `main` does, with
/// `stdin_terminal` (whether its standard input is a terminal) asked only when no first word
/// or window flag decides.
fn window_args<'a>(
    exe: &Path,
    argv: &'a [String],
    stdin_terminal: impl FnOnce() -> Option<bool>,
    door: FrontDoor<'_>,
) -> Option<Vec<&'a str>> {
    let argv0 = argv.first().and_then(|a| file_name(Path::new(a)));
    let args: Vec<&str> = argv
        .iter()
        .skip(1)
        .map(String::as_str)
        .take_while(|a| !matches!(*a, "-e" | "--command" | "--"))
        .collect();
    let window = match alias_of(exe, argv, door) {
        Some((_, routed)) => routed == Verb::Window,
        None if file_name(exe) != Some("aterm") => false,
        None => match args.first() {
            Some(word) if !word.starts_with('-') || (door.verb)(word).is_some() => {
                (door.verb)(word) == Some(Verb::Window)
            }
            _ => {
                let session = argv0 == Some("aterm-cli") || args.contains(&"--session");
                !session
                    && (args
                        .iter()
                        .any(|a| matches!(*a, "--window" | "--headless" | "--diagnose"))
                        || stdin_terminal() == Some(false))
            }
        },
    };
    window.then_some(args)
}

/// What the census says of an instance's command line.
fn launch_words(argv: &[String], flags: usize) -> String {
    let mut said: Vec<String> = argv.iter().take(1 + flags).map(|w| escaped(w)).collect();
    if let Some(payload) = argv.get(1 + flags) {
        said.push(escaped(payload));
        said.push("…".to_string());
    }
    said.join(" ")
}

/// `word` with each control character (`ESC`, `CR`, a C1 code) written as its Rust escape.
fn escaped(word: &str) -> String {
    word.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Whether `args` switched the control socket off ([`socket_directive`]).
fn socket_off(args: &[&str]) -> bool {
    let value = args
        .iter()
        .position(|a| *a == "--control-sock")
        .and_then(|i| args.get(i + 1).copied());
    let off = args.contains(&"--no-control-sock").then_some("1");
    socket_directive(value, off) == SocketDirective::Disabled
}

/// An age in the two largest units: `11d 22h`, `3h 05m`, `4m 12s`, `42s`.
pub(crate) fn age_words(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
#[path = "census_tests.rs"]
mod tests;
