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
    /// `aterm <verb>[ <sub>]` when the verb table names its verb, else its file name.
    pub name: String,
    /// How long it has run, in words.
    pub age: String,
    /// Its launcher exited and launchd adopted it (`Parent::Adopted`).
    pub adopted: bool,
    /// Its executable sits under a build's `target*` directory.
    pub build: bool,
    /// The kernel says its executable's file was unlinked since it started.
    pub deleted: bool,
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

/// What the front door does with a word in argv[1] (`aterm <word>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    /// Opens a window in this process when no running instance takes the request.
    Window,
    /// It runs as the program `aterm <verb>`.
    Program,
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
    /// Whether it has a controlling terminal; `None` when that was not read.
    pub(crate) terminal: Option<bool>,
}

/// The kernel reads the census makes.
pub(crate) trait Kernel {
    /// This user's processes, or `None` when the process table cannot be read.
    fn listed(&self) -> Option<Vec<Listed>>;
    /// `pid`'s executable image ([`image_of`]).
    fn exe(&self, pid: u32) -> Option<Image>;
    /// Whether `pid` is held at macOS's program check ([`aterm_uds::process::at_program_check`]).
    fn held(&self, pid: u32) -> Option<bool>;
    /// `pid`'s argument words ([`aterm_uds::process::read_process_argv`]).
    fn argv(&self, pid: u32) -> Option<Vec<OsString>>;
    /// `pid`'s lineage ([`aterm_uds::process::read_lineage`]), asked only when its parent is 1.
    fn lineage(&self, pid: u32) -> Option<Lineage>;
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
                    terminal: Some(r.tty.is_some()),
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
                terminal: std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .ok()
                    .and_then(|stat| stat_terminal(&stat)),
            }
        };
        Some(read_then_aged(pids, read, unix_now))
    }

    fn exe(&self, pid: u32) -> Option<Image> {
        image_of(pid)
    }

    fn held(&self, pid: u32) -> Option<bool> {
        aterm_uds::process::at_program_check(i32::try_from(pid).ok()?)
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

/// Whether the process a `/proc/<pid>/stat` line describes has a controlling terminal.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn stat_terminal(stat: &str) -> Option<bool> {
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let tty: i64 = rest.split_ascii_whitespace().nth(4)?.parse().ok()?;
    Some(tty != 0)
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
pub fn window_census(verbs: &dyn Fn(&str) -> Option<Verb>) -> Result<Census, String> {
    let (outcome, targets) = crate::inspect_fleet();
    let targets = targets.into_iter().map(crate::Target::listed).collect();
    let (dir, published) = published_in(outcome, targets)?;
    census_of(&LiveKernel, std::process::id(), &published, dir, verbs)
}

/// Off Unix there is no process table read here; the census says so rather than guessing.
///
/// # Errors
/// Always: the census runs on macOS and Linux only.
#[cfg(not(unix))]
pub fn window_census(_verbs: &dyn Fn(&str) -> Option<Verb>) -> Result<Census, String> {
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
    verbs: &dyn Fn(&str) -> Option<Verb>,
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
        let Some(image) = k.exe(row.pid) else {
            continue;
        };
        let exe = image.path.as_path();
        if !is_aterm(exe) {
            continue;
        }
        let words: Option<Vec<String>> = (k.held(row.pid) != Some(true))
            .then(|| k.argv(row.pid))
            .flatten()
            .map(|argv| {
                argv.iter()
                    .map(|w| w.to_string_lossy().into_owned())
                    .collect()
            });
        let age = row.age_s.map_or_else(|| "?".to_string(), age_words);
        let window = words
            .as_deref()
            .filter(|words| alias_verb(exe, words, verbs).is_none())
            .and_then(|words| window_args(exe, words, row.terminal, verbs));
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
                name: program_name(exe, words.as_deref().unwrap_or_default(), verbs),
                age,
                adopted: parent == Parent::Adopted,
                build: in_build_dir(exe),
                deleted: image.deleted,
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

/// The file name of `path`, when it is UTF-8.
fn file_name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

/// Whether `exe` is an aterm executable.
pub(crate) fn is_aterm(exe: &Path) -> bool {
    file_name(exe).is_some_and(|name| name == "aterm" || name.starts_with("aterm-"))
}

/// Whether `exe` sits under a build's target directory.
fn in_build_dir(exe: &Path) -> bool {
    exe.ancestors()
        .skip(1)
        .filter_map(file_name)
        .any(|name| name.starts_with("target"))
}

/// The verb an `aterm-<verb>` name dispatches to when `verbs` names it.
fn alias_verb<'a>(
    exe: &'a Path,
    argv: &'a [String],
    verbs: &dyn Fn(&str) -> Option<Verb>,
) -> Option<&'a str> {
    let argv0 = argv.first().and_then(|a| file_name(Path::new(a)));
    [argv0, file_name(exe)]
        .into_iter()
        .flatten()
        .filter_map(|name| name.strip_prefix("aterm-"))
        .find(|verb| verbs(verb).is_some())
}

/// What a program is called.
fn program_name(exe: &Path, argv: &[String], verbs: &dyn Fn(&str) -> Option<Verb>) -> String {
    let (verb, rest) = match alias_verb(exe, argv, verbs) {
        Some(verb) => (Some(verb), argv.get(1..)),
        None => match argv.get(1) {
            Some(word) if verbs(word).is_some() => (Some(word.as_str()), argv.get(2..)),
            _ => (None, None),
        },
    };
    let Some(verb) = verb else {
        return exe_word(exe);
    };
    match rest.and_then(<[String]>::first).filter(|w| is_sub_word(w)) {
        Some(sub) => format!("aterm {verb} {sub}"),
        None => format!("aterm {verb}"),
    }
}

/// A sub-verb word: `^[a-z][a-z0-9-]{0,23}$`.
fn is_sub_word(word: &str) -> bool {
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
fn window_args<'a>(
    exe: &Path,
    argv: &'a [String],
    terminal: Option<bool>,
    verbs: &dyn Fn(&str) -> Option<Verb>,
) -> Option<Vec<&'a str>> {
    let argv0 = argv.first().and_then(|a| file_name(Path::new(a)));
    let alias = [argv0, file_name(exe)]
        .into_iter()
        .flatten()
        .any(|name| matches!(name, "aterm-gui" | "aterm-windowed"));
    let args: Vec<&str> = argv
        .iter()
        .skip(1)
        .map(String::as_str)
        .take_while(|a| !matches!(*a, "-e" | "--command" | "--"))
        .collect();
    let window = alias
        || match args.first() {
            Some(word) if !word.starts_with('-') => verbs(word) == Some(Verb::Window),
            _ => {
                let session = argv0 == Some("aterm-cli") || args.contains(&"--session");
                !session
                    && (args
                        .iter()
                        .any(|a| matches!(*a, "--window" | "--headless" | "--diagnose"))
                        || terminal == Some(false))
            }
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
fn age_words(secs: u64) -> String {
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
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const LAUNCHD: i32 = 4_100;

    struct Row {
        listed: Listed,
        exe: Option<Image>,
        argv: Option<Vec<String>>,
        lineage: Option<Lineage>,
        held: bool,
    }

    /// A process table with the reads the census makes of it, recording which pids it was asked
    /// the arguments of.
    #[derive(Default)]
    struct Fake {
        rows: Vec<Row>,
        launchd: Option<i32>,
        unreadable: bool,
        argv_asked: RefCell<BTreeSet<u32>>,
    }

    impl Fake {
        fn with(rows: Vec<Row>) -> Self {
            Self {
                rows,
                launchd: Some(LAUNCHD),
                ..Self::default()
            }
        }

        fn row(&self, pid: u32) -> Option<&Row> {
            self.rows.iter().find(|r| r.listed.pid == pid)
        }
    }

    impl Kernel for Fake {
        fn listed(&self) -> Option<Vec<Listed>> {
            (!self.unreadable).then(|| self.rows.iter().map(|r| r.listed).collect())
        }
        fn exe(&self, pid: u32) -> Option<Image> {
            self.row(pid)?.exe.clone()
        }
        fn held(&self, pid: u32) -> Option<bool> {
            self.row(pid).map(|r| r.held)
        }
        fn argv(&self, pid: u32) -> Option<Vec<OsString>> {
            self.argv_asked.borrow_mut().insert(pid);
            let words = self.row(pid)?.argv.as_ref()?;
            Some(words.iter().map(OsString::from).collect())
        }
        fn lineage(&self, pid: u32) -> Option<Lineage> {
            self.row(pid)?.lineage
        }
        fn launchd_version(&self) -> Option<i32> {
            self.launchd
        }
    }

    const NOW: u64 = 1_790_000_000;

    fn proc(pid: u32, ppid: u32, age_s: u64, exe: &str, argv: &[&str]) -> Row {
        Row {
            listed: Listed {
                pid,
                ppid: Some(ppid),
                birth: Some(ProcessBirth {
                    seconds: NOW - age_s,
                    microseconds: u64::from(pid),
                }),
                age_s: Some(age_s),
                terminal: Some(true),
            },
            exe: Some(Image {
                path: PathBuf::from(exe),
                deleted: false,
            }),
            argv: Some(argv.iter().map(ToString::to_string).collect()),
            lineage: None,
            held: false,
        }
    }

    fn detached(mut row: Row) -> Row {
        row.listed.terminal = Some(false);
        row
    }

    fn under_launchd(mut row: Row, launcher_version: Option<i32>) -> Row {
        row.listed.ppid = Some(1);
        row.lineage = launcher_version.map(|launcher_version| Lineage {
            identity: aterm_uds::process::Identity {
                pid: i32::try_from(row.listed.pid).expect("pid"),
                birth: row.listed.birth.expect("a birth").into(),
                idversion: Some(90_000),
            },
            uniqueid: u64::from(row.listed.pid),
            parent: 1,
            pgid: row.listed.pid,
            launcher_version: Some(launcher_version),
            tty: None,
        });
        row
    }

    const ATERM: &str = "/opt/aterm/bin/aterm";
    const BUNDLE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

    fn verbs(word: &str) -> Option<Verb> {
        match word {
            "link" | "ctl" | "doctor" | "pkg" => Some(Verb::Program),
            "new-window" | "new-tab" => Some(Verb::Window),
            _ => None,
        }
    }

    fn table() -> Vec<Row> {
        vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            proc(7641, 1, 128_929, BUNDLE, &[BUNDLE, "--window"]),
            proc(
                67548,
                50,
                1_031_523,
                "/w/aterm-before",
                &["./aterm-before", "--headless"],
            ),
            proc(
                19235,
                50,
                341_206,
                "/w/target/debug/deps/aterm_pty-8a8f4252995814f9",
                &[
                    "/w/target/debug/deps/aterm_pty-8a8f4252995814f9",
                    "--headless",
                ],
            ),
            proc(300, 50, 300, ATERM, &["aterm"]),
            proc(
                301,
                50,
                2,
                ATERM,
                &["aterm", "ctl", "--sock", "/x", "windows"],
            ),
            proc(302, 50, 9, ATERM, &["aterm", "--session", "--headless"]),
        ]
    }

    fn census(k: &Fake, published: &[u32]) -> Census {
        let published = published.iter().copied().collect();
        census_of(k, 100, &published, "/run/aterm".into(), &verbs).expect("census")
    }

    #[test]
    fn it_names_the_unreached_instance_and_ignores_the_reached_one() {
        let k = Fake::with(table());
        let c = census(&k, &[7641]);
        assert_eq!(c.instances, 2, "{c:?}");
        assert_eq!(
            c.unreached,
            vec![Unreached {
                pid: 67548,
                age: "11d 22h".to_string(),
                command: "./aterm-before --headless".to_string(),
                socketless: false,
            }]
        );
        let c = census(&k, &[7641, 67548]);
        assert_eq!((c.instances, c.unreached.len()), (2, 0));
        let programs: Vec<(u32, &str)> = c
            .programs
            .iter()
            .map(|p| (p.pid, p.name.as_str()))
            .collect();
        assert_eq!(
            programs,
            [(300, "aterm"), (302, "aterm"), (301, "aterm ctl")],
            "the sessions and the client are programs, oldest first"
        );
    }

    #[test]
    fn an_aterm_program_is_known_by_its_executable_not_its_name() {
        let mut unnamed = proc(503, 50, 40, ATERM, &["aterm", "--headless"]);
        unnamed.exe = None;
        let k = Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            proc(500, 50, 40, "/bin/bash", &["aterm", "--headless"]),
            proc(501, 50, 40, ATERM, &["x", "--headless"]),
            proc(
                502,
                50,
                40,
                "/w/target/debug/deps/aterm_pty-8a8f4252995814f9",
                &["aterm", "--headless"],
            ),
            unnamed,
        ]);
        let c = census(&k, &[]);
        assert_eq!(c.instances, 1, "{c:?}");
        let unreached: Vec<u32> = c.unreached.iter().map(|u| u.pid).collect();
        assert_eq!(unreached, [501]);
        assert!(c.programs.is_empty(), "{c:?}");
        assert_eq!(
            *k.argv_asked.borrow(),
            BTreeSet::from([501]),
            "only a proven aterm's arguments are read"
        );
        assert!(!is_aterm(Path::new("/bin/zsh")));
        assert!(!is_aterm(Path::new("/x/atermx")));
        assert!(is_aterm(Path::new("/repo/target/debug/aterm-gui")));
    }

    #[test]
    fn a_held_program_s_arguments_are_never_read() {
        let mut held = proc(900, 50, 30, ATERM, &["aterm", "--headless"]);
        held.held = true;
        let k = Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            held,
            proc(901, 50, 30, ATERM, &["aterm", "--headless"]),
        ]);
        let c = census(&k, &[]);
        assert!(
            !k.argv_asked.borrow().contains(&900),
            "a held program's arguments were read: {c:?}"
        );
        assert_eq!(c.instances, 1, "{c:?}");
        let names: Vec<(u32, &str)> = c
            .programs
            .iter()
            .map(|p| (p.pid, p.name.as_str()))
            .collect();
        assert_eq!(names, [(900, "aterm")], "{c:?}");
    }

    #[test]
    fn verbs_are_counted_by_their_words() {
        let k = Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            proc(
                600,
                50,
                10,
                ATERM,
                &[ATERM, "link", "broker", "/r/b.sock", "/r/b.log"],
            ),
            proc(
                601,
                50,
                10,
                "/w/target/debug/aterm-link",
                &["/w/target/debug/aterm-link", "broker", "/r/b.sock"],
            ),
            proc(
                602,
                50,
                10,
                ATERM,
                &["aterm", "link", "serve", "--fleet", "f"],
            ),
            proc(
                603,
                50,
                10,
                ATERM,
                &["aterm", "ctl", "--sock", "x", "await"],
            ),
            proc(604, 50, 10, ATERM, &["aterm", "frobnicate"]),
            proc(605, 50, 10, ATERM, &["aterm-ctl", "status"]),
            proc(606, 50, 10, ATERM, &["aterm", "link", "Broker;rm"]),
            proc(
                607,
                50,
                10,
                "/opt/bin/aterm-dev build",
                &["/opt/bin/aterm-dev build"],
            ),
            proc(
                608,
                50,
                10,
                "/w/target/debug/aterm-lab",
                &["aterm-lab", "instance"],
            ),
        ]);
        let c = census(&k, &[]);
        let names: BTreeMap<u32, &str> = c
            .programs
            .iter()
            .map(|p| (p.pid, p.name.as_str()))
            .collect();
        let said: Vec<&str> = names.values().copied().collect();
        assert!(!said.contains(&"aterm frobnicate"), "{names:?}");
        assert!(!said.contains(&"aterm doctor"), "{names:?}");
        assert_eq!(
            names,
            BTreeMap::from([
                (600, "aterm link broker"),
                (601, "aterm link broker"),
                (602, "aterm link serve"),
                (603, "aterm ctl"),
                (604, "aterm"),
                (605, "aterm ctl status"),
                (606, "aterm link"),
                (607, "aterm?"),
                (608, "aterm-lab"),
            ])
        );
        assert_eq!(c.instances, 0, "{c:?}");
    }

    #[test]
    fn an_adopted_program_is_said_and_a_launchd_started_one_is_not() {
        let broker = |pid| {
            proc(
                pid,
                4_000,
                20,
                ATERM,
                &[ATERM, "link", "broker", "/r/b.sock"],
            )
        };
        let k = Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            under_launchd(broker(700), Some(31)),
            under_launchd(broker(701), Some(LAUNCHD)),
            under_launchd(broker(702), None),
            broker(703),
            under_launchd(
                proc(704, 4_000, 80, BUNDLE, &[BUNDLE, "pkg", "update"]),
                Some(77),
            ),
        ]);
        let c = census(&k, &[]);
        let by_pid: BTreeMap<u32, &Program> = c.programs.iter().map(|p| (p.pid, p)).collect();
        assert!(by_pid[&700].adopted, "adopted: {c:?}");
        assert!(!by_pid[&701].adopted, "launchd started it: {c:?}");
        assert!(!by_pid[&702].adopted, "unjudged: {c:?}");
        assert!(!by_pid[&703].adopted, "its launcher lives: {c:?}");
        assert!(
            by_pid[&704].adopted && by_pid[&704].name == "aterm pkg update",
            "{c:?}"
        );
        let mut unread = Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            under_launchd(broker(700), Some(31)),
        ]);
        unread.launchd = None;
        assert!(
            !census(&unread, &[]).programs[0].adopted,
            "launchd's version unread proves nothing"
        );
    }

    #[test]
    fn a_build_and_a_deleted_binary_are_said() {
        let broker = |pid, exe: &str| proc(pid, 50, 20, exe, &[exe, "link", "broker"]);
        let mut gone = broker(801, "/w/gone/target/debug/aterm");
        gone.exe = Some(Image {
            path: PathBuf::from("/w/gone/target/debug/aterm"),
            deleted: true,
        });
        let k = Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            broker(800, "/w/wt/target/debug/aterm"),
            gone,
            broker(802, BUNDLE),
            broker(803, "/w/wt/target.noindex/release/aterm-link"),
        ]);
        let c = census(&k, &[]);
        let by_pid: BTreeMap<u32, &Program> = c.programs.iter().map(|p| (p.pid, p)).collect();
        assert!(by_pid[&800].build && !by_pid[&800].deleted, "{c:?}");
        assert!(by_pid[&801].build && by_pid[&801].deleted, "{c:?}");
        assert!(!by_pid[&802].build && !by_pid[&802].deleted, "{c:?}");
        assert!(by_pid[&803].build, "{c:?}");
        assert!(
            !in_build_dir(Path::new("/target")),
            "the file itself is no directory"
        );
    }

    /// Linux's `exe` link of an unlinked file ends ` (deleted)`: the census takes the mark off,
    /// so the process is still known by its file name and is said to run a deleted binary.
    #[cfg(unix)]
    #[test]
    fn linux_s_deleted_mark_is_taken_off_the_executable() {
        assert_eq!(
            unmarked(Path::new("/w/wt/target/debug/aterm (deleted)")),
            Some(PathBuf::from("/w/wt/target/debug/aterm"))
        );
        assert_eq!(unmarked(Path::new("/w/wt/target/debug/aterm")), None);
        let image = unmarked(Path::new("/w/aterm-link (deleted)")).expect("marked");
        assert!(is_aterm(&image), "{}", image.display());
        assert_eq!(exe_word(&image), "aterm-link");
    }

    #[test]
    fn a_census_without_its_own_row_is_not_taken() {
        let k = Fake::with(table());
        census_of(&k, 99_999, &BTreeSet::new(), String::new(), &verbs).expect_err("no own row");
        let mut unreadable = Fake::with(table());
        unreadable.unreadable = true;
        census_of(&unreadable, 100, &BTreeSet::new(), String::new(), &verbs).expect_err("no table");
    }

    #[test]
    fn window_mode_is_read_from_the_launch_flags() {
        let routed = |exe: &str, argv: &[&str], terminal: Option<bool>| {
            let words: Vec<String> = argv.iter().map(ToString::to_string).collect();
            window_args(Path::new(exe), &words, terminal, &verbs).is_some()
        };
        let is_window = |exe: &str, argv: &[&str]| routed(exe, argv, Some(true));
        let started_alone = |exe: &str, argv: &[&str]| routed(exe, argv, Some(false));
        assert!(is_window(
            ATERM,
            &["aterm", "--headless", "--columns", "80"]
        ));
        assert!(is_window(ATERM, &["aterm", "--diagnose"]));
        assert!(is_window("/w/aterm-gui", &["aterm-gui"]));
        assert!(is_window(ATERM, &["aterm-gui"]), "the alias name");
        assert!(
            is_window(ATERM, &["aterm-windowed"]),
            "the windowed image's name"
        );
        assert!(is_window(ATERM, &["aterm", "new-window"]), "a window verb");
        assert!(is_window(ATERM, &["aterm-gui", "new-tab", "-d", "/w"]));
        assert!(started_alone(BUNDLE, &[BUNDLE]));
        assert!(
            started_alone(BUNDLE, &[BUNDLE, "--no-control-sock"]),
            "a window's flags keep it a window"
        );
        assert!(
            !is_window(BUNDLE, &["aterm"]),
            "a bare `aterm` typed on a terminal is a session, whatever its executable"
        );
        assert!(
            !routed(ATERM, &["aterm"], None),
            "an unread terminal proves nothing"
        );
        assert!(!started_alone(ATERM, &["aterm", "--session"]));
        assert!(
            !started_alone(ATERM, &["aterm-cli"]),
            "the forced session's name"
        );
        let spaced = "/Users//a/My Apps/aterm.app/Contents/MacOS/aterm";
        assert!(
            started_alone(spaced, &[spaced]),
            "a bundle path with a space"
        );
        assert!(!is_window(
            ATERM,
            &["aterm", "-e", "sh", "-c", "aterm --headless"]
        ));
        assert!(!is_window(ATERM, &["aterm", "--", "--window"]));
        assert!(!started_alone(
            ATERM,
            &["aterm", "pkg", "doctor", "--window"]
        ));
        assert!(!is_window(
            "/w/aterm-link",
            &["aterm-link", "serve", "--fleet", "f"]
        ));
        let support = "/Users//a/Library/Application Support/aterm/bin/aterm";
        assert!(is_window(support, &[support, "--headless"]));
        assert!(is_window(ATERM, &["/usr/local/bin/aterm", "--headless"]));

        let socketless = |argv: &[&str]| {
            let words: Vec<String> = argv.iter().map(ToString::to_string).collect();
            socket_off(
                &window_args(Path::new(ATERM), &words, Some(true), &verbs).expect("a window"),
            )
        };
        assert!(socketless(&["aterm", "--headless", "--no-control-sock"]));
        assert!(socketless(&[
            "aterm",
            "--headless",
            "--control-sock",
            "off"
        ]));
        assert!(socketless(&["aterm", "--headless", "--control-sock", "0"]));
        assert!(!socketless(&[
            "aterm",
            "--headless",
            "--control-sock",
            "/tmp/a.sock"
        ]));
        assert!(!socketless(&["aterm", "--headless"]));
    }

    #[test]
    fn a_window_is_counted_by_the_front_door_s_route() {
        let k = Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            proc(1001, 50, 50, ATERM, &["aterm", "new-window", "-d", "/w"]),
            detached(proc(1002, 1, 40, BUNDLE, &[BUNDLE, "--no-control-sock"])),
            proc(1003, 50, 30, BUNDLE, &["aterm"]),
        ]);
        let c = census(&k, &[1001]);
        assert_eq!(c.instances, 2, "{c:?}");
        let unreached: Vec<(u32, bool)> =
            c.unreached.iter().map(|u| (u.pid, u.socketless)).collect();
        assert_eq!(unreached, [(1002, true)], "{c:?}");
        let programs: Vec<(u32, &str)> = c
            .programs
            .iter()
            .map(|p| (p.pid, p.name.as_str()))
            .collect();
        assert_eq!(programs, [(1003, "aterm")], "{c:?}");
    }

    #[test]
    fn an_unreached_instance_s_words_are_its_escaped_launch_flags() {
        let k = Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            proc(
                1100,
                50,
                20,
                ATERM,
                &[
                    "aterm",
                    "--headless",
                    "--working-directory",
                    "/w/\u{1b}]52;c;eA==\u{7}\r\n",
                    "-e",
                    "sh",
                    "-c",
                    "LOGIN=secret run",
                ],
            ),
        ]);
        let c = census(&k, &[]);
        let said = &c.unreached[0].command;
        assert_eq!(
            said, "aterm --headless --working-directory /w/\\u{1b}]52;c;eA==\\u{7}\\r\\n -e …",
            "{c:?}"
        );
        assert!(!said.chars().any(char::is_control), "{said:?}");
        assert!(!said.contains("secret"), "{said:?}");
    }

    #[test]
    fn a_missing_directory_publishes_nothing_and_an_unreadable_one_is_not_measured() {
        let targets = vec![
            (7641, "/r/aterm-7641.sock".to_string()),
            (0, "/x".to_string()),
        ];
        let found = crate::DirOutcome::Found {
            path: PathBuf::from("/r"),
            n: 2,
        };
        let (dir, published) = published_in(found, targets).expect("found");
        assert_eq!((dir.as_str(), published), ("/r", BTreeSet::from([7641])));

        let missing = crate::DirOutcome::Missing {
            path: PathBuf::from("/scratch/run/aterm"),
            why: "resolved from $XDG_RUNTIME_DIR".to_string(),
        };
        let (dir, published) = published_in(missing, Vec::new()).expect("missing is an answer");
        assert!(published.is_empty());
        let c = census_of(&Fake::with(table()), 100, &published, dir, &verbs).expect("census");
        let pids: Vec<u32> = c.unreached.iter().map(|u| u.pid).collect();
        assert_eq!(
            pids,
            [67548, 7641],
            "every instance is unreached from there"
        );

        let unreadable = crate::DirOutcome::Unreadable {
            path: PathBuf::from("/r"),
            err: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        published_in(unreadable, Vec::new()).expect_err("not measured");
        published_in(crate::DirOutcome::Unresolvable, Vec::new()).expect_err("not measured");
    }

    #[test]
    fn a_published_foreign_instance_counts_as_reached() {
        let targets = vec![
            (7641, "/r/aterm-7641.sock".to_string()),
            (67548, "/var/folders/xy/T/at.Q1/a.sock".to_string()),
        ];
        let found = crate::DirOutcome::Found {
            path: PathBuf::from("/r"),
            n: 2,
        };
        let (dir, published) = published_in(found, targets).expect("found");
        assert!(published.contains(&67548), "the graph entry publishes it");
        let c = census_of(&Fake::with(table()), 100, &published, dir, &verbs).expect("census");
        assert_eq!((c.instances, c.unreached.len()), (2, 0));
    }

    #[test]
    fn ages_are_words_and_the_oldest_comes_first() {
        assert_eq!(age_words(11 * 86_400 + 22 * 3_600 + 5), "11d 22h");
        assert_eq!(age_words(3 * 3_600 + 5 * 60), "3h 05m");
        assert_eq!(age_words(4 * 60 + 12), "4m 12s");
        assert_eq!(age_words(42), "42s");

        let mut t = table();
        t.push(proc(
            68000,
            50,
            5,
            ATERM,
            &["aterm", "--headless", "--no-control-sock"],
        ));
        let c = census(&Fake::with(t), &[7641]);
        let pids: Vec<u32> = c.unreached.iter().map(|u| u.pid).collect();
        assert_eq!(pids, [67548, 68000]);
        assert!(c.unreached[1].socketless);
        assert_eq!(c.unreached[1].age, "5s");
    }

    #[test]
    fn a_process_born_as_the_table_is_read_has_an_age() {
        let clock = std::cell::Cell::new(NOW);
        let read = |pid: u32| {
            clock.set(clock.get() + 1);
            Listed {
                pid,
                ppid: Some(1),
                birth: Some(ProcessBirth {
                    seconds: clock.get(),
                    microseconds: 0,
                }),
                age_s: None,
                terminal: None,
            }
        };
        let rows = read_then_aged(vec![7, 8], read, || Some(clock.get()));
        let ages: Vec<Option<u64>> = rows.iter().map(|r| r.age_s).collect();
        assert_eq!(ages, [Some(1), Some(0)], "{rows:?}");
    }

    #[test]
    fn a_stat_line_s_terminal_is_its_tty_number() {
        assert_eq!(
            stat_terminal("4711 (zsh) S 4700 4711 4711 34817 4711 4194560 0"),
            Some(true)
        );
        assert_eq!(
            stat_terminal("4712 (a) b) S 1 4712 4712 0 -1 4194560 0"),
            Some(false)
        );
        assert_eq!(stat_terminal("4713 (cut"), None);
    }

    /// Live: the shipped table lists this process with its real parent, and the kernel names
    /// its executable, a test binary that is not aterm.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_live_table_lists_this_process() {
        let me = std::process::id();
        let listed = LiveKernel.listed().expect("the table reads");
        let row = listed
            .iter()
            .find(|r| r.pid == me)
            .expect("this process is listed");
        assert_eq!(row.ppid, Some(std::os::unix::process::parent_id()));
        assert!(row.birth.is_some() && row.age_s.is_some(), "{row:?}");
        assert!(row.terminal.is_some(), "{row:?}");
        let image = LiveKernel
            .exe(me)
            .expect("this process's executable is named");
        assert!(!image.deleted, "{image:?}");
        assert!(!is_aterm(&image.path), "{}", image.path.display());
        assert_eq!(LiveKernel.held(me), Some(false), "this process runs");
    }
}
