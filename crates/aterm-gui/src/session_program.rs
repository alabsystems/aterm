// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Program identity: `status program=` and the `sessions` row's `program=`.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::session_timeline::SessionTimeline;

const PROGRAM_MAX: usize = 32;

/// The program name a file's path names: its file name (a vendor `versions/<VERSION>` file names
/// its tool, not its version), its leading `-`s dropped (a login shell's `-zsh`), kept to
/// `[A-Za-z0-9._+-]` and at most [`PROGRAM_MAX`] bytes. For a word that is a file's own path (a
/// front's target, a shim script), and for an `argv[0]` [`program_from_argv0`] reads as one word.
pub(crate) fn path_program(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    let base = parts.next().unwrap_or("");
    let base = match (parts.next(), parts.next()) {
        (Some("versions"), Some(tool)) if base.starts_with(|c: char| c.is_ascii_digit()) => tool,
        _ => base,
    };
    let name: String = base
        .trim_start_matches('-')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        .take(PROGRAM_MAX)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Whether `c` ends a process title's first word: a blank of any kind (an argument word follows
/// it) or a control character (a terminal sequence's bytes follow it).
fn is_title_break(c: char) -> bool {
    c.is_whitespace() || c.is_control()
}

/// What the kernel and the file system say of a process's image, for reading the `argv[0]` it
/// may have rewritten ([`program_from_argv0`]); asked only of an `argv[0]` that is a process
/// title.
pub(crate) trait Image {
    /// Whether `path` names, by device and inode, the file the process's image was mapped from.
    fn is_file(&self, path: &str) -> bool;
    /// Whether `path`, an absolute path, names a program file (a regular file with an execute
    /// bit, links followed), whichever process maps it.
    fn is_program(&self, path: &str) -> bool;
    /// The names the kernel keeps for the exec, which no process sets afterwards: the mapped
    /// file's name and macOS's `p_name` ([`crate::command_ran::ExecNames::iter`]).
    fn names(&self) -> Vec<String>;
}

/// The [`Image`] of process `pid`, read once and only when asked: the kernel's names and the
/// mapped file's identity ([`crate::command_ran::read_names`]: macOS `proc_pidpath`'s file and
/// `p_name`, Linux `/proc/<pid>/exe` itself; nothing off unix), with the paths a title names
/// stat'ed ([`LeaderImage::of`]); or the kernel's names alone, nothing stat'ed
/// ([`LeaderImage::names_only`]).
pub(crate) struct LeaderImage {
    pid: i32,
    /// Whether a path is stat'ed: the mapped file's and the paths a title names.
    stat: bool,
    #[cfg(unix)]
    read: std::cell::OnceCell<crate::command_ran::ExecNames>,
}

impl LeaderImage {
    /// The image of `pid`, unread, whose files are stat'ed when asked: for a reader on its own
    /// thread (the status sweep, the leader read, a control peer's name).
    pub(crate) const fn of(pid: i32) -> Self {
        Self {
            pid,
            stat: true,
            #[cfg(unix)]
            read: std::cell::OnceCell::new(),
        }
    }

    /// The image of `pid` as the thread that draws may read it: the names the kernel keeps for
    /// the exec ([`crate::command_ran::kernel_names`]: a process-table read, no file system),
    /// and no path stat'ed, so a title that names an automount or a network path never stalls a
    /// frame; a path that only a stat could vouch for is not vouched for.
    pub(crate) const fn names_only(pid: i32) -> Self {
        Self {
            pid,
            stat: false,
            #[cfg(unix)]
            read: std::cell::OnceCell::new(),
        }
    }

    #[cfg(unix)]
    fn read(&self) -> &crate::command_ran::ExecNames {
        self.read.get_or_init(|| {
            if self.stat {
                crate::command_ran::read_names(self.pid, &mut |_| false)
            } else {
                crate::command_ran::kernel_names(self.pid)
            }
        })
    }

    /// Whether `path` may be stat'ed: this image stats, and it is an absolute path short enough
    /// to name a file.
    #[cfg(unix)]
    fn statted(&self, path: &str) -> bool {
        self.stat && path.starts_with('/') && path.len() <= crate::command_ran::PATH_MAX
    }
}

impl Image for LeaderImage {
    #[cfg(unix)]
    fn is_file(&self, path: &str) -> bool {
        self.statted(path)
            && self.read().file.is_some_and(|image| {
                crate::command_ran::file_id(std::path::Path::new(path)) == Some(image)
            })
    }

    #[cfg(not(unix))]
    fn is_file(&self, _path: &str) -> bool {
        let _ = (self.pid, self.stat);
        false
    }

    #[cfg(unix)]
    fn is_program(&self, path: &str) -> bool {
        use crate::command_ran::Files as _;
        self.statted(path)
            && crate::command_ran::RealFiles
                .program(std::path::Path::new(path))
                .is_some()
    }

    #[cfg(not(unix))]
    fn is_program(&self, _path: &str) -> bool {
        false
    }

    #[cfg(unix)]
    fn names(&self) -> Vec<String> {
        self.read().iter().map(str::to_owned).collect()
    }

    #[cfg(not(unix))]
    fn names(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What Linux's `/proc/<pid>/exe` appends to the path of a file unlinked since the exec (a
/// binary a package upgrade replaced by rename): a mark, never part of the file's name.
const DELETED_MARK: &str = " (deleted)";

/// The program name `argv[0]` names (H6: a name steers, the kernel's read of the image confirms).
/// An `argv[0]` of one word (`claude`, `-zsh`, `/bin/sleep`, `exec -a NAME`'s) names it as its
/// path does ([`path_program`]). One with a blank or a control character in it is a process
/// title a process may have rewritten (setproctitle's `postgres: alice orders …`, npm's
/// `npm run <script>`, perl's `$0`), whose words after the first are arguments: it is read whole
/// only where the kernel vouches for the whole, its path naming by device and inode the file the
/// image was mapped from (an app path with spaces) or its name being one the kernel keeps for the
/// exec (Linux's ` (deleted)` mark dropped); otherwise only its first word names the program, a
/// trailing `:` dropped. A first word that is a path is a path cut at a blank, so it may end in a
/// directory's word (`/opt/My Tools/tool` cut to `/opt/My`): it names the program only where it
/// names a program file or its name is one the kernel keeps. Otherwise, and for a title with no
/// first word, the kernel's own name for the exec. So no word after a title's first ever reaches
/// `program=`, and no directory's word does.
pub(crate) fn program_from_argv0(argv0: &str, image: &dyn Image) -> Option<String> {
    let Some(cut) = argv0.find(is_title_break) else {
        return path_program(argv0);
    };
    let kept = || {
        image
            .names()
            .into_iter()
            .filter_map(|n| path_program(n.strip_suffix(DELETED_MARK).unwrap_or(&n)))
    };
    let vouched =
        |path: &str, name: &String| kept().any(|kept| kept == *name) || image.is_program(path);
    let whole = path_program(argv0);
    if whole
        .as_ref()
        .is_some_and(|whole| image.is_file(argv0) || kept().any(|kept| kept == *whole))
    {
        return whole;
    }
    let first = argv0[..cut].trim_end_matches(':');
    let named = path_program(first).filter(|name| !first.contains('/') || vouched(first, name));
    named.or_else(|| kept().next())
}

use atpkg::image::SCRIPT_SHELLS;

/// Program for `argv0 [argv1]` ([`program_from_argv0`] of the process whose image is `image`,
/// [`SCRIPT_SHELLS`], [`atpkg_shim`]).
pub(crate) fn program_from_argv(
    argv0: &str,
    argv1: Option<&str>,
    is_shim: &dyn Fn(&str) -> bool,
    image: &dyn Image,
) -> Option<String> {
    let name = program_from_argv0(argv0, image)?;
    if SCRIPT_SHELLS.contains(&name.as_str())
        && let Some(script) = argv1.filter(|a| !a.starts_with('-'))
        && let Some(agent) = path_program(script)
        && atpkg::stub::is_agent_program(&agent)
        && is_shim(script)
    {
        return Some(agent);
    }
    Some(name)
}

fn script_in(script: &str, dirs: &[std::path::PathBuf]) -> bool {
    let Some(parent) = std::path::Path::new(script).parent() else {
        return false;
    };
    let resolved = std::fs::canonicalize(parent).ok();
    dirs.iter().any(|dir| {
        dir == parent
            || resolved
                .as_ref()
                .is_some_and(|r| std::fs::canonicalize(dir).is_ok_and(|d| d == *r))
    })
}

/// Which of atpkg's shims a script is ([`shim_in`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shim {
    /// The managed twin in `agents/`, or a `bin/` shim: proof of a managed copy (`copy=managed`).
    Managed,
    /// The session-scoped `reroute/` stub: names its agent, proves nothing (`copy=-`), as its header says ("never proof of an install").
    Reroute,
}

/// Which shim `script` is in `layout` ([`Shim::Managed`], [`Shim::Reroute`], [`script_in`]).
pub(crate) fn shim_in(script: &str, layout: &atpkg::store::Layout) -> Option<Shim> {
    if script_in(script, &[layout.agents_dir(), layout.bin_dir()]) {
        Some(Shim::Managed)
    } else if script_in(script, &[layout.reroute_dir()]) {
        Some(Shim::Reroute)
    } else {
        None
    }
}

/// [`shim_in`] the configured managed prefix.
pub(crate) fn shim_of(script: &str) -> Option<Shim> {
    atpkg::store::resolve_configured().and_then(|layout| shim_in(script, &layout))
}

/// Whether `script` is atpkg's own shim of either kind, for naming ([`program_from_argv`]).
pub(crate) fn atpkg_shim(script: &str) -> bool {
    shim_of(script).is_some()
}

/// `argv[0]` and `argv[1]` of `pid` ([`aterm_uds::process::read_process_argv`]: none for another
/// user's process, one gone, or a read the kernel cut short).
pub(crate) fn argv_head_of(pid: i32) -> Option<(String, Option<String>)> {
    if pid <= 0 {
        return None;
    }
    argv_head(&aterm_uds::process::read_process_argv(pid)?)
}

/// `argv[0]` where it is not empty, and `argv[1]` where it is there and not empty, each read as
/// lossy UTF-8.
fn argv_head(words: &[std::ffi::OsString]) -> Option<(String, Option<String>)> {
    let text = |w: &std::ffi::OsString| w.to_string_lossy().into_owned();
    let argv0 = words.first().filter(|w| !w.is_empty()).map(text)?;
    let argv1 = words.get(1).filter(|w| !w.is_empty()).map(text);
    Some((argv0, argv1))
}

/// The published program of `pid` ([`program_of_words`] its arguments, its image read with
/// its files stat'ed: [`LeaderImage::of`]), as a reader on its own thread reads it: `reveal`'s
/// agent check on the control thread ([`crate::control_reveal::Live`]), as a control peer's name
/// is read there, and the live tests' reading of the law [`leader_facts`] ships;
/// [`program_of_names_only`] is the one the thread that draws reads.
pub(crate) fn program_of(pid: i32) -> Option<String> {
    program_of_image(pid, &LeaderImage::of(pid))
}

/// [`program_of`] as the thread that draws may read it: the image's names from the process
/// table alone, no path a title names stat'ed ([`LeaderImage::names_only`]).
pub(crate) fn program_of_names_only(pid: i32) -> Option<String> {
    program_of_image(pid, &LeaderImage::names_only(pid))
}

/// [`program_of_words`] of `pid`'s arguments against `image`.
fn program_of_image(pid: i32, image: &dyn Image) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    program_of_words(&aterm_uds::process::read_process_argv(pid)?, image)
}

/// The published program of a process whose arguments are `words` and whose image is `image`: a
/// build's front by the build it becomes ([`front_program`]), else [`program_from_argv`] its head
/// ([`argv_head`]).
fn program_of_words(words: &[std::ffi::OsString], image: &dyn Image) -> Option<String> {
    if aterm_uds::build_front::front(words).is_some() {
        return front_program(words);
    }
    let (argv0, argv1) = argv_head(words)?;
    program_from_argv(&argv0, argv1.as_deref(), &atpkg_shim, image)
}

/// THE PROGRAM A BUILD'S FRONT BECOMES, where `argv` names one ([`aterm_uds::build_front::front`]):
/// its target's name as [`path_program`] reads it, which is how the tab names the build once
/// the front has exec'd it. The one naming rule of a front, for `status` and the survey (the
/// leader's facts) and for `sys jobs` (`vitals_jobs`' rows). `None` for any other argv.
pub(crate) fn front_program<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Option<String> {
    let front = aterm_uds::build_front::front(argv)?;
    path_program(&front.target.as_ref().to_string_lossy())
}

/// A BUILD'S FRONT WAITING FOR MEMORY, as `status` says it (`wait=memory wait_gib= wait_ahead=`):
/// what its ledger says of its claim, where the ledger was read and holds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BuildWait {
    /// The memory its waiting claim asks, bytes.
    pub(crate) mem: Option<u64>,
    /// The memory claims ahead of it.
    pub(crate) ahead: Option<usize>,
}

impl BuildWait {
    /// What a front's ledger says of it ([`atpkg::build_admit::front_claim`]): waiting with its
    /// figures; admitted (it execs its build, or runs it where no holder armed), no wait; unread
    /// or holding no claim of its pid (its ledger is not the one this reader reads), still a
    /// front, so waiting, its figures unknown.
    pub(crate) fn of(claim: Option<atpkg::build_admit::FrontClaim>) -> Option<Self> {
        match claim {
            Some(atpkg::build_admit::FrontClaim::Waiting { mem, ahead }) => Some(Self {
                mem: Some(mem),
                ahead: Some(ahead.len()),
            }),
            Some(atpkg::build_admit::FrontClaim::Held) => None,
            None => Some(Self::default()),
        }
    }

    /// `status`'s fields: ` wait=memory wait_gib=<GiB to a tenth|-> wait_ahead=<n|->`, or
    /// ` wait=- wait_gib=- wait_ahead=-` for a program that waits for none.
    pub(crate) fn fields(wait: Option<&Self>) -> String {
        let Some(wait) = wait else {
            return " wait=- wait_gib=- wait_ahead=-".to_string();
        };
        let gib = wait.mem.map_or_else(
            || "-".to_string(),
            |bytes| {
                let g = u128::from(1_u64 << 30);
                let tenths = (u128::from(bytes) * 10 + g / 2) / g;
                format!("{}.{}", tenths / 10, tenths % 10)
            },
        );
        let ahead = wait
            .ahead
            .map_or_else(|| "-".to_string(), |n| n.to_string());
        format!(" wait=memory wait_gib={gib} wait_ahead={ahead}")
    }
}

/// [`BuildWait::of`] the front `pid`, read in the ledger the front claims in
/// ([`atpkg::build_admit::ledger_of_front`], `env` its environment).
fn build_wait_of(pid: i32, env: &dyn Fn(&str) -> Option<String>) -> Option<BuildWait> {
    let claim = u32::try_from(pid)
        .ok()
        .zip(atpkg::build_admit::ledger_of_front(env))
        .and_then(|(pid, ledger)| atpkg::build_admit::front_claim(&ledger, pid).ok().flatten());
    BuildWait::of(claim)
}

/// What the shell's exported PATH does with the agent programs ([`path_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathVerdict {
    /// aterm's managed `agents/` comes before every other directory holding an agent program.
    Live,
    /// No `agents/` ahead of a foreign copy, but the session's `reroute/` stubs are, with `ATERM_CHILD` set: typed `claude`/`codex` still reach the managed twin, unless the shell hashed the foreign path before the stubs existed (zsh keeps it until `rehash`).
    RerouteOnly,
    /// A foreign copy wins, or neither managed directory is on PATH.
    Frozen,
}

impl PathVerdict {
    /// The `path=` word.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Live | Self::RerouteOnly => "live",
            Self::Frozen => "frozen",
        }
    }
}

/// The PATH verdict ([`PathVerdict::Live`], [`PathVerdict::RerouteOnly`], [`PathVerdict::Frozen`]).
pub(crate) fn path_verdict(
    path_env: &str,
    aterm_child: bool,
    is_agents: &dyn Fn(&str) -> bool,
    is_reroute: &dyn Fn(&str) -> bool,
    holds_agent: &dyn Fn(&str) -> bool,
) -> PathVerdict {
    let mut rerouted = false;
    for dir in path_env.split(':').filter(|d| !d.is_empty()) {
        if is_agents(dir) {
            return PathVerdict::Live;
        }
        if is_reroute(dir) {
            rerouted |= aterm_child;
            continue;
        }
        if holds_agent(dir) {
            break;
        }
    }
    if rerouted {
        PathVerdict::RerouteOnly
    } else {
        PathVerdict::Frozen
    }
}

/// `copy=` for an agent the leader runs (`managed` under the managed prefix's `store/`).
pub(crate) fn copy_of(
    program: Option<&str>,
    exec_path: &str,
    store: Option<&std::path::Path>,
    shim: bool,
) -> Option<&'static str> {
    if !atpkg::stub::is_agent_program(program?) {
        return None;
    }
    let store = store?;
    Some(
        if shim || std::path::Path::new(exec_path).starts_with(store) {
            "managed"
        } else {
            "foreign"
        },
    )
}

/// What one read of the foreground leader tells.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeaderFacts {
    /// [`program_from_argv`].
    pub(crate) program: Option<String>,
    /// [`copy_of`] the program, or the agent script a runtime runs (`node /usr/local/bin/claude`).
    pub(crate) copy: Option<&'static str>,
    /// [`path_verdict`] of the leader's PATH at exec.
    pub(crate) path: Option<PathVerdict>,
    /// Whether started with `--no-harness`, `--no-alab-harness`, or `ATERM_NO_HARNESS=1`.
    pub(crate) no_harness: bool,
    /// A build's front, named [`Self::program`] by the build it becomes ([`front_program`]),
    /// whose claim waits for memory ([`BuildWait`]); `None` for any other leader.
    pub(crate) build_wait: Option<BuildWait>,
}

/// [`LeaderFacts`] from one parsed `KERN_PROCARGS2` read of the process whose image is `image`.
pub(crate) fn facts_from(
    args: &atpkg::caller_shell::ProcArgs,
    ppid: Option<i32>,
    shell: i32,
    managed: Option<&atpkg::store::Layout>,
    shim_of: &dyn Fn(&str) -> Option<Shim>,
    holds_agent: &dyn Fn(&str) -> bool,
    image: &dyn Image,
) -> LeaderFacts {
    let argv0 = args.argv.first().map_or("", String::as_str);
    let argv1 = args.argv.get(1).map(String::as_str);
    let front = front_program(&args.argv);
    let is_front = front.is_some();
    let named = program_from_argv(argv0, argv1, &|script| shim_of(script).is_some(), image);
    let shim = !is_front
        && named
            .as_deref()
            .is_some_and(|p| program_from_argv0(argv0, image).as_deref() != Some(p));
    let program = front.or(named);
    let reroute = shim && argv1.and_then(shim_of) == Some(Shim::Reroute);
    let store = managed.map(|layout| layout.prefix.join("store"));
    let script_agent = program
        .as_deref()
        .filter(|p| aterm_phase::may_host_agent(p))
        .and(argv1)
        .filter(|script| !script.starts_with('-'))
        .and_then(path_program)
        .filter(|script| atpkg::stub::is_agent_program(script));
    let copy = if reroute {
        None
    } else {
        copy_of(
            script_agent.as_deref().or(program.as_deref()),
            &args.exec_path,
            store.as_deref(),
            shim,
        )
    };
    let path = (shell > 0 && ppid == Some(shell))
        .then(|| args.env_var("PATH"))
        .flatten()
        .zip(managed)
        .map(|(path_env, layout)| {
            let agents = layout.agents_dir();
            let reroute = layout.reroute_dir();
            let same = |dir: &str, want: &std::path::Path| {
                let dir = std::path::Path::new(dir.trim_end_matches('/'));
                dir == want
                    || std::fs::canonicalize(dir)
                        .ok()
                        .zip(std::fs::canonicalize(want).ok())
                        .is_some_and(|(a, b)| a == b)
            };
            path_verdict(
                path_env,
                args.env_var("ATERM_CHILD").is_some(),
                &|dir| same(dir, &agents),
                &|dir| same(dir, &reroute),
                holds_agent,
            )
        });
    let no_harness = args
        .env_var(atpkg::yolo::NO_HARNESS_ENV)
        .is_some_and(|v| !v.is_empty() && v != "0")
        || args
            .argv
            .iter()
            .skip(1)
            .take_while(|a| a.as_str() != "--")
            .any(|a| matches!(a.as_str(), "--no-harness" | "--no-alab-harness"));
    LeaderFacts {
        program,
        copy,
        path,
        no_harness,
        build_wait: is_front.then(BuildWait::default),
    }
}

fn holds_agent(dir: &str, laid: &[&str]) -> bool {
    let dir = std::path::Path::new(dir);
    laid.iter().any(|name| dir.join(name).is_file())
}

/// `pid`'s parent, or `None` (gone, another user's, a platform without the call).
pub(crate) fn parent_of(pid: i32) -> Option<i32> {
    if pid <= 0 {
        return None;
    }
    platform_parent(pid)
}

#[cfg(target_os = "macos")]
fn platform_parent(pid: i32) -> Option<i32> {
    let info = crate::quit_safety::pid_bsdinfo(pid)?;
    i32::try_from(info.pbi_ppid).ok()
}

#[cfg(target_os = "linux")]
fn platform_parent(pid: i32) -> Option<i32> {
    let stat =
        aterm_uds::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
            .ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_parent(_pid: i32) -> Option<i32> {
    None
}

/// [`LeaderFacts`] of `pgid`'s leader in the session whose shell is `shell`.
pub(crate) fn leader_facts(pgid: i32, shell: i32) -> LeaderFacts {
    let Some(args) = u32::try_from(pgid)
        .ok()
        .filter(|p| *p > 0)
        .and_then(atpkg::caller_shell::process_args)
    else {
        // One read of its arguments names it and says whether it is a front, so both come from
        // the one program it ran then.
        let words = (pgid > 0)
            .then(|| aterm_uds::process::read_process_argv(pgid))
            .flatten();
        let front = words
            .as_deref()
            .is_some_and(|words| aterm_uds::build_front::front(words).is_some());
        return LeaderFacts {
            program: words
                .as_deref()
                .and_then(|words| program_of_words(words, &LeaderImage::of(pgid))),
            build_wait: front.then(|| build_wait_of(pgid, &|_| None)).flatten(),
            ..LeaderFacts::default()
        };
    };
    let managed = atpkg::store::resolve_configured();
    let laid = managed
        .as_ref()
        .map(atpkg::reroute::agents_routes)
        .unwrap_or_default();
    let mut facts = facts_from(
        &args,
        parent_of(pgid),
        shell,
        managed.as_ref(),
        &|script| managed.as_ref().and_then(|layout| shim_in(script, layout)),
        &|dir| holds_agent(dir, &laid),
        &LeaderImage::of(pgid),
    );
    if facts.build_wait.is_some() {
        facts.build_wait = build_wait_of(pgid, &|key| args.env_var(key).map(str::to_owned));
    }
    facts
}

/// One resolution request: the session, its timeline, the group to name and
/// the session's shell (the only parent whose child's PATH is the shell's).
#[derive(Clone)]
struct Job {
    session: u64,
    timeline: Arc<Mutex<SessionTimeline>>,
    pgid: i32,
    shell: i32,
    /// What the footer read this job starts wakes when it moves the
    /// session's log (`claude_footer::LogWake`).
    wake: crate::claude_footer::LogWake,
}

/// The leader read a job runs ([`LeaderFacts`], [`leader_facts`]).
type Lookup = Arc<dyn Fn(i32, i32) -> LeaderFacts + Send + Sync>;
type Completion = Arc<dyn Fn(u64) + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobPhase {
    Queued,
    InFlight,
}

struct PendingJob {
    /// A retired or disabled session keeps its token until the worker drains
    /// it, so a rapid re-enable cannot put a second token behind a slow read.
    latest: Option<Job>,
    revision: u64,
    /// Changes on cancellation or a new group, but not an ordinary retry. A
    /// slow same-group lookup may still publish while a retry is waiting, so
    /// repeated retries cannot starve program identity.
    incarnation: u64,
    phase: JobPhase,
}

#[derive(Default)]
struct PendingJobs {
    by_session: HashMap<u64, PendingJob>,
    revision: u64,
}

impl PendingJobs {
    fn current(&self, job: &Job, incarnation: u64) -> bool {
        self.by_session.get(&job.session).is_some_and(|slot| {
            slot.incarnation == incarnation
                && slot.latest.as_ref().is_some_and(|latest| {
                    latest.pgid == job.pgid && Arc::ptr_eq(&latest.timeline, &job.timeline)
                })
        })
    }

    fn offer(&mut self, job: Job) -> bool {
        self.revision = self.revision.wrapping_add(1);
        if let Some(pending) = self.by_session.get_mut(&job.session) {
            if pending.latest.as_ref().is_some_and(|old| {
                old.pgid != job.pgid || !Arc::ptr_eq(&old.timeline, &job.timeline)
            }) {
                pending.incarnation = self.revision;
            }
            pending.latest = Some(job);
            pending.revision = self.revision;
            return false;
        }
        self.by_session.insert(
            job.session,
            PendingJob {
                latest: Some(job),
                revision: self.revision,
                incarnation: self.revision,
                phase: JobPhase::Queued,
            },
        );
        true
    }

    fn restart_ids(&mut self) -> Vec<u64> {
        self.by_session
            .iter_mut()
            .map(|(session, job)| {
                job.phase = JobPhase::Queued;
                *session
            })
            .collect()
    }

    fn take(&mut self, session: u64) -> Option<(Job, u64, u64)> {
        let slot = self.by_session.get_mut(&session)?;
        debug_assert!(matches!(slot.phase, JobPhase::Queued));
        if let Some(job) = slot.latest.clone() {
            slot.phase = JobPhase::InFlight;
            Some((job, slot.revision, slot.incarnation))
        } else {
            self.by_session.remove(&session);
            None
        }
    }

    fn finish(&mut self, session: u64, revision: u64) -> bool {
        match self.by_session.get_mut(&session) {
            Some(slot) if slot.revision != revision && slot.latest.is_some() => {
                slot.phase = JobPhase::Queued;
                true
            }
            Some(_) => {
                self.by_session.remove(&session);
                false
            }
            None => false,
        }
    }

    fn cancel(&mut self, session: u64) {
        if let Some(slot) = self.by_session.get_mut(&session) {
            self.revision = self.revision.wrapping_add(1);
            slot.latest = None;
            slot.revision = self.revision;
            slot.incarnation = self.revision;
        }
    }

    fn cancel_all(&mut self) {
        for slot in self.by_session.values_mut() {
            self.revision = self.revision.wrapping_add(1);
            slot.latest = None;
            slot.revision = self.revision;
            slot.incarnation = self.revision;
        }
    }
}

/// The background thread program resolutions run on, started on first use.
pub(crate) struct ProgramResolver {
    tx: Option<Sender<u64>>,
    worker: Option<JoinHandle<()>>,
    pending: Arc<Mutex<PendingJobs>>,
    lookup: Lookup,
    completion: Option<Completion>,
    /// The log's wake the footer reads this resolver starts carry ([`Self::set_log_wake`]).
    log_wake: crate::claude_footer::LogWake,
}

impl Default for ProgramResolver {
    fn default() -> Self {
        Self {
            tx: None,
            worker: None,
            pending: Arc::new(Mutex::new(PendingJobs::default())),
            lookup: Arc::new(leader_facts),
            completion: None,
            log_wake: crate::claude_footer::LogWake::default(),
        }
    }
}

impl std::fmt::Debug for ProgramResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramResolver")
            .field("started", &self.tx.is_some())
            .finish()
    }
}

impl ProgramResolver {
    /// A resolver whose reads are `lookup`'s.
    #[cfg(test)]
    pub(crate) fn with_lookup(
        lookup: impl Fn(i32, i32) -> LeaderFacts + Send + Sync + 'static,
    ) -> Self {
        Self {
            lookup: Arc::new(lookup),
            ..Self::default()
        }
    }

    /// What the footer reads this resolver starts wake when they move a session's log.
    pub(crate) fn set_log_wake(&mut self, wake: &crate::claude_footer::LogWake) {
        self.log_wake = wake.clone();
    }

    /// The status subsystem is off: cancel all queued work.
    pub(crate) fn clear_pending(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel_all();
    }

    /// Retiring a tab releases its latest queued job.
    pub(crate) fn retire(&mut self, session: u64) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel(session);
    }

    fn start_worker(&mut self) -> bool {
        let (tx, rx) = channel::<u64>();
        let pending = Arc::clone(&self.pending);
        let lookup = Arc::clone(&self.lookup);
        let completion = self.completion.clone();
        match crate::qos::spawn_lane_thread(
            "aterm-program-id",
            crate::qos::Role::Responsive,
            move || {
                run_worker(rx, pending, lookup, completion);
            },
        ) {
            Ok(worker) => {
                self.tx = Some(tx);
                self.worker = Some(worker);
                true
            }
            Err(e) => {
                aterm_log::warn!("program identity: could not start the resolver: {e}");
                false
            }
        }
    }

    fn recover_finished_worker(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            let _ = self.worker.take().expect("finished worker").join();
            self.tx = None;
        }
    }

    fn ensure_worker(&mut self) -> Option<bool> {
        self.recover_finished_worker();
        if self.tx.is_some() {
            return Some(false);
        }
        if !self.start_worker() {
            return None;
        }
        let ids = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .restart_ids();
        let tx = self.tx.as_ref().expect("worker just started").clone();
        for session in ids {
            if tx.send(session).is_err() {
                self.tx = None;
                self.worker.take();
                return None;
            }
        }
        Some(true)
    }

    /// Resolve `pgid`'s leader off-thread into `timeline`.
    pub(crate) fn request(
        &mut self,
        session: u64,
        timeline: &Arc<Mutex<SessionTimeline>>,
        pgid: i32,
        shell: i32,
        proxy: Option<&aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
    ) {
        if pgid <= 0 {
            return;
        }
        if self.completion.is_none()
            && let Some(proxy) = proxy.cloned()
        {
            self.completion = Some(Arc::new(move |session| {
                let _ = proxy.send_event(crate::Wake::ProgramResolved { session });
            }));
        }
        let new_slot = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .offer(Job {
                session,
                timeline: Arc::clone(timeline),
                pgid,
                shell,
                wake: self.log_wake.clone(),
            });
        let Some(started_fresh) = self.ensure_worker() else {
            return;
        };
        if !started_fresh
            && new_slot
            && self.tx.as_ref().is_some_and(|tx| tx.send(session).is_err())
        {
            self.tx = None;
            self.worker.take();
            let _ = self.ensure_worker();
        }
    }
}

fn run_worker(
    rx: Receiver<u64>,
    pending: Arc<Mutex<PendingJobs>>,
    lookup: Lookup,
    completion: Option<Completion>,
) {
    let mut local = VecDeque::new();
    loop {
        if local.is_empty() {
            let Ok(session) = rx.recv() else { break };
            local.push_back(session);
        }
        local.extend(rx.try_iter());
        let Some(session) = local.pop_front() else {
            continue;
        };
        let Some((job, revision, incarnation)) = pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take(session)
        else {
            continue;
        };
        let facts = lookup(job.pgid, job.shell);
        let (changed, became_claude, left_claude) =
            publish_if_current(&pending, &job, incarnation, facts);
        if changed && let Some(wake) = &completion {
            wake(job.session);
        }
        if left_claude {
            crate::claude_footer::stop(job.session, job.pgid);
        }
        if became_claude {
            let pending = pending.lock().unwrap_or_else(|p| p.into_inner());
            if pending.current(&job, incarnation) {
                crate::claude_footer::request_footer(
                    job.session,
                    &job.timeline,
                    job.pgid,
                    &job.wake,
                );
            }
        }
        let changed_while_running = pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .finish(session, revision);
        if changed_while_running {
            local.extend(rx.try_iter());
            local.push_back(session);
        }
    }
}

fn publish_if_current(
    pending: &Mutex<PendingJobs>,
    job: &Job,
    incarnation: u64,
    facts: LeaderFacts,
) -> (bool, bool, bool) {
    let pending = pending.lock().unwrap_or_else(|p| p.into_inner());
    if !pending.current(job, incarnation) {
        return (false, false, false);
    }
    let mut timeline = job.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let was_claude = crate::claude_footer::drawn_for(timeline.agent().program.as_deref());
    let program_changed = timeline.set_program(job.pgid, facts.program);
    timeline.set_leader(job.pgid, facts.copy, facts.path);
    let no_harness_changed = timeline.set_leader_no_harness(job.pgid, facts.no_harness);
    let wait_changed = timeline.set_build_wait(job.pgid, facts.build_wait);
    let changed = program_changed || no_harness_changed || wait_changed;
    let became_claude =
        program_changed && crate::claude_footer::drawn_for(timeline.agent().program.as_deref());
    (changed, became_claude, program_changed && was_claude)
}

#[cfg(test)]
mod tests {
    use super::*;

    use aterm_spec::wait::MUST_HAPPEN;

    fn names(f: impl Fn(i32) -> Option<String> + Send + Sync + 'static) -> Lookup {
        Arc::new(move |pgid, _shell| LeaderFacts {
            program: f(pgid),
            ..LeaderFacts::default()
        })
    }

    /// [`argv_head_of`] a just-spawned child once it reads exactly the head `expected` accepts,
    /// polled to a bounded deadline; at the deadline, the last read.
    #[cfg(unix)]
    fn settled_argv_head(
        pid: i32,
        expected: impl Fn(&str, Option<&str>) -> bool,
    ) -> Option<(String, Option<String>)> {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            let head = argv_head_of(pid);
            let settled = head
                .as_ref()
                .is_some_and(|(a0, a1)| expected(a0, a1.as_deref()));
            if settled || std::time::Instant::now() >= deadline {
                return head;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// A test child that can neither outlive nor stall its test: null stdio (a leftover holding
    /// an inherited stdout stalls a runner that pipes test output), and its own process group
    /// so [`kill_group`] reaches its descendants.
    #[cfg(unix)]
    fn quiet_child(cmd: &mut aterm_uds::spawnfd::Command) -> &mut aterm_uds::spawnfd::Command {
        cmd.stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .process_group(0)
    }

    /// Kill a [`quiet_child`]'s whole group, then reap the child.
    #[cfg(unix)]
    fn kill_group(child: &mut aterm_uds::spawnfd::Child) {
        if let Ok(pid) = i32::try_from(child.id()) {
            // The group this test created: `process_group(0)` made the child's pid its pgid,
            // and the unreaped child keeps that pid from reuse.
            let _ = aterm_uds::process::kill_group_until_empty(pid);
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn pending_job_transitions_keep_the_latest_group_and_one_token() {
        let model = aterm_models_harness::program_resolver_queue_model();
        let mut state = model.init_state();
        let mut jobs = PendingJobs::default();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let job = |pgid| Job {
            session: 7,
            timeline: Arc::clone(&timeline),
            pgid,
            shell: 0,
            wake: crate::claude_footer::LogWake::default(),
        };

        assert!(jobs.offer(job(10)));
        assert!(model.fire("AskFirst", &mut state));
        assert_eq!(state["queued"], 1);
        assert_eq!(jobs.by_session.len(), 1);
        assert!(matches!(jobs.by_session[&7].phase, JobPhase::Queued));

        let (old, old_revision, _) = jobs.take(7).expect("old group queued");
        assert_eq!(old.pgid, 10);
        assert!(model.fire("Take", &mut state));
        assert!(!jobs.offer(job(20)), "replace the in-flight slot");
        assert!(model.fire("AskNewGroup", &mut state));
        assert!(!jobs.offer(job(20)), "collapse a duplicate retry");
        assert!(model.fire("AskAgain", &mut state));
        assert_eq!(jobs.by_session.len(), 1);
        assert!(matches!(jobs.by_session[&7].phase, JobPhase::InFlight));
        assert_eq!(state["queued"], 0);

        assert!(
            jobs.finish(7, old_revision),
            "the latest group is still owed"
        );
        assert!(model.fire("Finish", &mut state));
        assert_eq!(jobs.by_session[&7].latest.as_ref().unwrap().pgid, 20);
        assert!(matches!(jobs.by_session[&7].phase, JobPhase::Queued));
        assert_eq!(state["queued"], 1);

        let (latest, _, _) = jobs.take(7).expect("latest group queued");
        assert_eq!(latest.pgid, 20);
        assert!(model.fire("Take", &mut state));
        assert!(model.fire("Crash", &mut state));
        assert_eq!(jobs.restart_ids(), vec![7]);
        assert!(model.fire("Restart", &mut state));
        assert!(matches!(jobs.by_session[&7].phase, JobPhase::Queued));
        assert_eq!(state["queued"], 1);

        let (_, latest_revision, _) = jobs.take(7).expect("restart requeued latest");
        assert!(model.fire("Take", &mut state));
        assert!(!jobs.finish(7, latest_revision));
        assert!(model.fire("Finish", &mut state));
        assert!(jobs.by_session.is_empty());
        assert_eq!(state["completed"], 2);
        assert_eq!(state["queued"], 0);
    }

    #[test]
    fn cancelled_inflight_job_reenables_without_a_second_token() {
        let model = aterm_models_harness::program_resolver_queue_model();
        let mut state = model.init_state();
        let mut jobs = PendingJobs::default();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let job = || Job {
            session: 7,
            timeline: Arc::clone(&timeline),
            pgid: 10,
            shell: 0,
            wake: crate::claude_footer::LogWake::default(),
        };

        assert!(jobs.offer(job()));
        assert!(model.fire("AskFirst", &mut state));
        let (_, old_revision, old_incarnation) = jobs.take(7).unwrap();
        assert!(model.fire("Take", &mut state));
        jobs.cancel(7);
        assert!(model.fire("Cancel", &mut state));
        assert_ne!(jobs.by_session[&7].incarnation, old_incarnation);
        assert!(!jobs.offer(job()), "the old read still owns the token");
        assert!(model.fire("Reenable", &mut state));
        assert_ne!(jobs.by_session[&7].revision, old_revision);
        assert_eq!(state["queued"], 0, "no duplicate token while in flight");

        assert!(jobs.finish(7, old_revision), "new request gets one replay");
        assert!(model.fire("Finish", &mut state));
        assert_eq!(state["queued"], 1);
        assert!(matches!(jobs.by_session[&7].phase, JobPhase::Queued));
        let (_, current_revision, _) = jobs.take(7).unwrap();
        assert!(model.fire("Take", &mut state));
        assert!(!jobs.finish(7, current_revision));
        assert!(model.fire("Finish", &mut state));
        assert!(jobs.by_session.is_empty());
        assert_eq!(state["queued"], 0);

        let mut queued_state = model.init_state();
        assert!(jobs.offer(job()));
        assert!(model.fire("AskFirst", &mut queued_state));
        jobs.cancel(7);
        assert!(model.fire("Cancel", &mut queued_state));
        assert!(!jobs.offer(job()));
        assert!(model.fire("Reenable", &mut queued_state));
        assert_eq!(jobs.by_session.len(), 1);
        assert!(matches!(jobs.by_session[&7].phase, JobPhase::Queued));
        assert_eq!(queued_state["queued"], 1);
        let (_, revision, _) = jobs.take(7).unwrap();
        assert!(model.fire("Take", &mut queued_state));
        assert!(!jobs.finish(7, revision));
        assert!(model.fire("Finish", &mut queued_state));

        let mut cancelled_state = model.init_state();
        assert!(jobs.offer(job()));
        assert!(model.fire("AskFirst", &mut cancelled_state));
        jobs.cancel(7);
        assert!(model.fire("Cancel", &mut cancelled_state));
        assert!(jobs.take(7).is_none());
        assert!(model.fire("DropCancelled", &mut cancelled_state));
        assert!(jobs.by_session.is_empty());
    }

    #[test]
    fn a_claude_code_argv0_starts_and_stops_the_footer_watch_like_claude() {
        let named = |p: &str| LeaderFacts {
            program: Some(p.into()),
            ..LeaderFacts::default()
        };
        for (name, draws) in [("claude", true), ("claude-code", true), ("codex", false)] {
            let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
            timeline.lock().unwrap().note_foreground_group(42);
            let pending = Mutex::new(PendingJobs::default());
            let job = Job {
                session: 7,
                timeline: Arc::clone(&timeline),
                pgid: 42,
                shell: 0,
                wake: crate::claude_footer::LogWake::default(),
            };
            pending.lock().unwrap().offer(job.clone());
            let (_, _, incarnation) = pending.lock().unwrap().take(7).unwrap();
            assert_eq!(
                publish_if_current(&pending, &job, incarnation, named(name)),
                (true, draws, false),
                "{name}: entering"
            );
            assert_eq!(
                publish_if_current(&pending, &job, incarnation, named("zsh")),
                (true, false, draws),
                "{name}: leaving"
            );
        }
    }

    #[test]
    fn slow_lookup_coalesces_retries_and_serves_a_new_group_promptly() {
        use std::time::Duration;

        let (started_tx, started_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let lookup: Lookup = {
            let calls = Arc::clone(&calls);
            names(move |pgid| {
                calls.lock().unwrap().push(pgid);
                if pgid == 10 {
                    started_tx.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                }
                Some(
                    match pgid {
                        10 => "sleep",
                        20 => "bash",
                        30 => "zsh",
                        _ => panic!("unexpected group {pgid}"),
                    }
                    .into(),
                )
            })
        };
        let (wake_tx, wake_rx) = channel::<u64>();
        let mut resolver = ProgramResolver {
            lookup,
            completion: Some(Arc::new(move |session| {
                let _ = wake_tx.send(session);
            })),
            ..ProgramResolver::default()
        };
        let first = Arc::new(Mutex::new(SessionTimeline::default()));
        let second = Arc::new(Mutex::new(SessionTimeline::default()));
        first.lock().unwrap().note_foreground_group(10);
        second.lock().unwrap().note_foreground_group(30);
        resolver.request(1, &first, 10, 0, None);
        started_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("worker entered old lookup");

        first.lock().unwrap().note_foreground_group(20);
        for _ in 0..100 {
            resolver.request(1, &first, 20, 0, None);
        }
        resolver.request(2, &second, 30, 0, None);
        let retired = Arc::new(Mutex::new(SessionTimeline::default()));
        retired.lock().unwrap().note_foreground_group(40);
        resolver.request(3, &retired, 40, 0, None);
        resolver.retire(3);
        assert_eq!(resolver.pending.lock().unwrap().by_session.len(), 3);
        release_tx.send(()).unwrap();

        assert_eq!(wake_rx.recv_timeout(MUST_HAPPEN), Ok(2));
        assert_eq!(wake_rx.recv_timeout(MUST_HAPPEN), Ok(1));
        assert_eq!(
            first.lock().unwrap().agent().program.as_deref(),
            Some("bash")
        );
        assert_eq!(
            second.lock().unwrap().agent().program.as_deref(),
            Some("zsh")
        );
        assert_eq!(*calls.lock().unwrap(), vec![10, 30, 20]);
        let drained_by = std::time::Instant::now() + MUST_HAPPEN;
        while !resolver.pending.lock().unwrap().by_session.is_empty() {
            assert!(
                std::time::Instant::now() < drained_by,
                "worker did not retire jobs"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn slow_same_group_lookup_can_publish_while_retries_coalesce() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (entered_tx, entered_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let calls = Arc::new(AtomicUsize::new(0));
        let lookup: Lookup = {
            let calls = Arc::clone(&calls);
            names(move |_| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    entered_tx.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                    Some("sh".into())
                } else {
                    Some("bash".into())
                }
            })
        };
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(10);
        let (wake_tx, wake_rx) = channel::<Option<String>>();
        let callback_timeline = Arc::clone(&timeline);
        let mut resolver = ProgramResolver {
            lookup,
            completion: Some(Arc::new(move |_| {
                let name = callback_timeline.lock().unwrap().agent().program.clone();
                let _ = wake_tx.send(name);
            })),
            ..ProgramResolver::default()
        };
        resolver.request(1, &timeline, 10, 0, None);
        entered_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("first lookup started");
        for _ in 0..100 {
            resolver.request(1, &timeline, 10, 0, None);
        }
        assert_eq!(resolver.pending.lock().unwrap().by_session.len(), 1);
        release_tx.send(()).unwrap();

        assert_eq!(wake_rx.recv_timeout(MUST_HAPPEN), Ok(Some("sh".into())));
        assert_eq!(wake_rx.recv_timeout(MUST_HAPPEN), Ok(Some("bash".into())));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn disabling_status_discards_a_late_inflight_answer() {
        let (entered_tx, entered_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let lookup: Lookup = names(move |pgid| {
            if pgid == 10 {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            Some("bash".into())
        });
        let (wake_tx, wake_rx) = channel::<u64>();
        let mut resolver = ProgramResolver {
            lookup,
            completion: Some(Arc::new(move |session| {
                let _ = wake_tx.send(session);
            })),
            ..ProgramResolver::default()
        };
        let old = Arc::new(Mutex::new(SessionTimeline::default()));
        old.lock().unwrap().note_foreground_group(10);
        resolver.request(1, &old, 10, 0, None);
        entered_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("old lookup started");
        resolver.clear_pending();

        let current = Arc::new(Mutex::new(SessionTimeline::default()));
        current.lock().unwrap().note_foreground_group(20);
        resolver.request(2, &current, 20, 0, None);
        release_tx.send(()).unwrap();
        assert_eq!(wake_rx.recv_timeout(MUST_HAPPEN), Ok(2));
        assert_eq!(old.lock().unwrap().agent().program.as_deref(), None);
        assert_eq!(
            current.lock().unwrap().agent().program.as_deref(),
            Some("bash")
        );
    }

    #[test]
    fn reenabling_the_same_group_drops_the_pre_clear_answer() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (entered_tx, entered_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let calls = Arc::new(AtomicUsize::new(0));
        let lookup: Lookup = {
            let calls = Arc::clone(&calls);
            names(move |_| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    entered_tx.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                    Some("old".into())
                } else {
                    Some("new".into())
                }
            })
        };
        let (wake_tx, wake_rx) = channel::<u64>();
        let mut resolver = ProgramResolver {
            lookup,
            completion: Some(Arc::new(move |session| {
                let _ = wake_tx.send(session);
            })),
            ..ProgramResolver::default()
        };
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(10);
        resolver.request(1, &timeline, 10, 0, None);
        entered_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("old request entered lookup");
        resolver.clear_pending();
        resolver.request(1, &timeline, 10, 0, None);
        assert_eq!(resolver.pending.lock().unwrap().by_session.len(), 1);
        release_tx.send(()).unwrap();

        assert_eq!(wake_rx.recv_timeout(MUST_HAPPEN), Ok(1));
        assert_eq!(
            timeline.lock().unwrap().agent().program.as_deref(),
            Some("new")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn worker_panic_requeues_the_latest_group_on_the_next_request() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        let (entered_tx, entered_rx) = channel::<()>();
        let fail_once = Arc::new(AtomicBool::new(true));
        let lookup: Lookup = {
            let fail_once = Arc::clone(&fail_once);
            names(move |_pgid| {
                if fail_once.swap(false, Ordering::SeqCst) {
                    entered_tx.send(()).unwrap();
                    panic!("injected process lookup panic");
                }
                Some("bash".into())
            })
        };
        let (wake_tx, wake_rx) = channel::<u64>();
        let mut resolver = ProgramResolver {
            lookup,
            completion: Some(Arc::new(move |session| {
                let _ = wake_tx.send(session);
            })),
            ..ProgramResolver::default()
        };
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(10);
        resolver.request(1, &timeline, 10, 0, None);
        entered_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("worker started");
        let deadline = Instant::now() + MUST_HAPPEN;
        while !resolver
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            assert!(Instant::now() < deadline, "worker did not exit after panic");
            aterm_uds::deadline::poll_yield();
        }
        timeline.lock().unwrap().note_foreground_group(20);
        resolver.request(1, &timeline, 20, 0, None);
        assert_eq!(wake_rx.recv_timeout(MUST_HAPPEN), Ok(1));
        assert_eq!(
            timeline.lock().unwrap().agent().program.as_deref(),
            Some("bash")
        );
        let drained_by = Instant::now() + MUST_HAPPEN;
        while !resolver.pending.lock().unwrap().by_session.is_empty() {
            assert!(
                Instant::now() < drained_by,
                "replacement job did not retire"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A test's [`Image`]: the mapped file is the one `file` spells, the kernel keeps `names`
    /// for the exec, and `programs` (and `file`) are the program files there are.
    struct Mapped {
        file: Option<&'static str>,
        names: &'static [&'static str],
        programs: &'static [&'static str],
    }

    impl Image for Mapped {
        fn is_file(&self, path: &str) -> bool {
            self.file == Some(path)
        }

        fn is_program(&self, path: &str) -> bool {
            self.file == Some(path) || self.programs.contains(&path)
        }

        fn names(&self) -> Vec<String> {
            self.names.iter().map(|n| (*n).to_owned()).collect()
        }
    }

    /// An image the kernel says nothing of.
    const NO_IMAGE: Mapped = Mapped {
        file: None,
        names: &[],
        programs: &[],
    };

    #[test]
    fn argv0_reduces_to_the_program_name() {
        let p = |argv0: &str| program_from_argv0(argv0, &NO_IMAGE);
        assert_eq!(p("claude").as_deref(), Some("claude"));
        assert_eq!(p("-zsh").as_deref(), Some("zsh"));
        assert_eq!(p("/bin/sleep").as_deref(), Some("sleep"));
        assert_eq!(
            p("/Users//x/.local/share/claude/versions/2.1.280").as_deref(),
            Some("claude"),
            "a vendor version directory names its tool, not its version"
        );
        assert_eq!(
            p("/opt/versions/tool").as_deref(),
            Some("tool"),
            "only a VERSION-shaped basename under versions/ is rewritten"
        );
        assert_eq!(
            p("evil\u{1b}]0;x\u{7}").as_deref(),
            Some("evil"),
            "a control character ends the name: no byte of a sequence after it joins it"
        );
        assert_eq!(p(""), None);
        assert_eq!(p("///"), None);
        assert_eq!(p(&"a".repeat(100)).map(|s| s.len()), Some(32));
    }

    /// PRIV4-1, A REWRITTEN TITLE NAMES ITS PROGRAM BY ITS FIRST WORD: an `argv[0]` a process
    /// rewrote (setproctitle's `postgres: …` and `nginx: …`, npm's `npm run <script>`, perl's
    /// `$0`) never carries a word after its first into the name, whatever blank or control
    /// character divides them; a title with no first word reads the kernel's name for the exec.
    /// A path with a blank is read whole only where the kernel vouches for it: its file is the
    /// mapped image's by device and inode, or its name is one the kernel keeps for the exec (a
    /// store `claude` under `Application Support` whose file an update replaced; a Linux binary
    /// a package upgrade replaced, whose `/proc/<pid>/exe` name ends ` (deleted)`). A first word
    /// that is a path cut at a blank names the program only where it is a program file or a
    /// name the kernel keeps; else the kernel's name stands, never a directory's word
    /// (`/opt/QZ… Tools/qztool` cut to `/opt/QZ…`). One word (`exec -a NAME`) still names the
    /// program as H6 ruled. NEGATIVE CONTROLS (scratch): reading every `argv[0]` as one path (the
    /// filter alone, as before) fails the first row with `qztQZP7W1QZP7W2`; taking a cut path's
    /// file name unvouched (the rule before the review) fails the `QZP7W16`-`QZP7W19` rows with
    /// their directory words; keeping Linux's ` (deleted)` mark fails the `QZP7W16` row with
    /// `qztooldeleted`.
    #[test]
    fn a_rewritten_title_names_its_program_by_its_first_word_only() {
        let perl = Mapped {
            file: Some("/usr/bin/perl"),
            names: &["perl"],
            programs: &["/opt/x/qzt"],
        };
        let app = "/Applications/Q Z.app/Contents/MacOS/qzapp";
        let replaced = Mapped {
            file: None,
            names: &["qztool (deleted)"],
            programs: &[],
        };
        let rows: [(&str, &Mapped, Option<&str>); 19] = [
            ("qzt QZP7W1 QZP7W2", &perl, Some("qzt")),
            (
                "postgres: jsmith orders 10.0.0.5(5432) QZP7W3",
                &perl,
                Some("postgres"),
            ),
            (
                "nginx: master process /usr/local/bin/nginx -c /u/jsmith/QZP7W4/site.conf",
                &perl,
                Some("nginx"),
            ),
            ("npm run QZP7W5", &perl, Some("npm")),
            ("/opt/x/qzt -c /u/jsmith/QZP7W6.conf", &perl, Some("qzt")),
            ("qzt\u{a0}QZP7W7", &perl, Some("qzt")),
            ("qzt\tQZP7W8", &perl, Some("qzt")),
            ("qzt\u{1b}]0;QZP7W9\u{7}", &perl, Some("qzt")),
            (" QZP7W10", &perl, Some("perl")),
            (": QZP7W11", &perl, Some("perl")),
            (": QZP7W12", &NO_IMAGE, None),
            (
                app,
                &Mapped {
                    file: Some(app),
                    names: &["qzreal"],
                    programs: &[],
                },
                Some("qzapp"),
            ),
            (
                app,
                &Mapped {
                    file: None,
                    names: &["qzapp"],
                    programs: &[],
                },
                Some("qzapp"),
            ),
            (app, &perl, Some("perl")),
            ("qzname", &perl, Some("qzname")),
            ("/opt/QZP7W16 Tools/qztool", &replaced, Some("qztool")),
            ("/opt/QZP7W17 Tools/qztool", &NO_IMAGE, None),
            ("/opt/QZP7W18 Tools/qztool", &perl, Some("perl")),
            ("./QZP7W19 d/qzt x", &perl, Some("perl")),
        ];
        let wrong: Vec<String> = rows
            .iter()
            .filter_map(|(argv0, image, want)| {
                let got = program_from_argv0(argv0, *image);
                (got.as_deref() != *want).then(|| format!("{argv0:?}: {got:?}, want {want:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert!(
            rows.iter()
                .filter_map(|(argv0, image, _)| program_from_argv0(argv0, *image))
                .all(|name| !name.contains("QZP7W")),
            "no word after a title's first is named"
        );
    }

    /// A real child is named by its argv[0] from the process table, a renamed one
    /// (`exec -a claude`) included.
    #[cfg(unix)]
    #[test]
    fn a_live_process_is_named_by_its_argv0() {
        let mut child = quiet_child(aterm_uds::spawnfd::command("sleep").arg("30"))
            .spawn()
            .expect("spawn sleep");
        let pid = i32::try_from(child.id()).expect("pid");
        let head = settled_argv_head(pid, |a0, a1| a0 == "sleep" && a1 == Some("30"));
        let named = program_of(pid);
        kill_group(&mut child);
        assert_eq!(
            head.map(|(a0, _)| a0).as_deref(),
            Some("sleep"),
            "sleep's argv never settled"
        );
        assert_eq!(named.as_deref(), Some("sleep"));
        assert_eq!(program_of(-1), None);

        let Some(bash) = aterm_spec::host::test_shell("bash") else {
            aterm_spec::host::could_not_run(format!(
                "the `exec -a` half did not run: {}",
                aterm_spec::host::tool("bash").refusal()
            ));
        };
        let home = aterm_tempfile::tempdir().expect("scratch home");
        let mut child = quiet_child(
            aterm_spec::host::shell_command(&bash, home.path())
                .args(["-c", "exec -a claude sleep 30"]),
        )
        .spawn()
        .expect("spawn bash");
        let pid = i32::try_from(child.id()).expect("pid");
        let head = settled_argv_head(pid, |a0, a1| a0 == "claude" && a1 == Some("30"));
        let named = program_of(pid);
        kill_group(&mut child);
        assert_eq!(
            head.map(|(a0, _)| a0).as_deref(),
            Some("claude"),
            "the renamed image never settled"
        );
        assert_eq!(named.as_deref(), Some("claude"));
    }

    /// PRIV4-1 LIVE: a real perl that set its `$0` to a title (setproctitle's shape) is named by
    /// the title's first word, never the words after it, by [`program_of`], by
    /// [`leader_facts`] (the leader read `status program=` and the log's `prog=` come from) and
    /// by [`program_of_names_only`] (the thread that draws); a real process run through a path
    /// with a blank, whose file is its image by device and inode, is named whole, and read with
    /// no stat it is named by the kernel's name for its image (`sleep`: macOS's `p_name` is the
    /// link's target's name too, so nothing vouches for the whole), never by the directory's
    /// word. NEGATIVE CONTROLS (scratch): reading every `argv[0]` as one path fails the perl
    /// assertions with `qztQZP7W13QZP7W14`; cutting every title at its first blank fails the
    /// `read whole` assertion with `q`; taking a cut path's file name unvouched fails the last
    /// with `q`.
    #[cfg(unix)]
    #[test]
    fn a_live_title_is_named_by_its_first_word_and_its_image_path_whole() {
        let Some(perl) = ["/usr/bin/perl", "/bin/perl"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
        else {
            aterm_spec::host::could_not_run("no perl here for the `$0` half".to_owned());
        };
        let mut child = quiet_child(
            aterm_uds::spawnfd::command(perl).args(["-e", "$0 = 'qzt QZP7W13 QZP7W14'; sleep 30"]),
        )
        .spawn()
        .expect("spawn perl");
        let pid = i32::try_from(child.id()).expect("pid");
        let head = settled_argv_head(pid, |a0, _| a0.starts_with("qzt "));
        let named = program_of(pid);
        let leader = leader_facts(pid, 0).program;
        let drawn = program_of_names_only(pid);
        kill_group(&mut child);
        assert!(
            head.is_some_and(|(a0, _)| a0.starts_with("qzt ")),
            "perl's title never settled"
        );
        assert_eq!(named.as_deref(), Some("qzt"), "program_of");
        assert_eq!(leader.as_deref(), Some("qzt"), "leader_facts");
        assert_eq!(drawn.as_deref(), Some("qzt"), "program_of_names_only");

        let scratch = aterm_tempfile::tempdir().expect("a scratch dir");
        let dir = scratch.path().join("q z");
        std::fs::create_dir(&dir).expect("mkdir");
        let link = dir.join("qzsleep");
        std::os::unix::fs::symlink("/bin/sleep", &link).expect("link");
        let spelled = link.to_string_lossy().into_owned();
        let mut child = quiet_child(aterm_uds::spawnfd::command(&link).arg("30"))
            .spawn()
            .expect("spawn sleep by a path with a blank");
        let pid = i32::try_from(child.id()).expect("pid");
        let head = settled_argv_head(pid, |a0, a1| a0 == spelled && a1 == Some("30"));
        let named = program_of(pid);
        let drawn = program_of_names_only(pid);
        kill_group(&mut child);
        assert_eq!(
            head.map(|(a0, _)| a0),
            Some(spelled),
            "the path never settled"
        );
        assert_eq!(named.as_deref(), Some("qzsleep"), "read whole");
        if cfg!(target_vendor = "apple") {
            assert_eq!(
                drawn.as_deref(),
                Some("sleep"),
                "read with no stat: the kernel's name for the image, never the directory's word"
            );
        } else {
            assert!(
                drawn.as_deref().is_some_and(|d| d != "q"),
                "read with no stat: the image's name, never the directory's word: {drawn:?}"
            );
        }
    }

    /// The thread that draws names a process from the process table alone
    /// ([`LeaderImage::names_only`], [`program_of_names_only`]): it vouches for no path by a stat,
    /// not even the very file the process maps, while [`LeaderImage::of`] (a reader's own
    /// thread) does, and both read the names the kernel keeps. NEGATIVE CONTROL (scratch): a
    /// names-only image that stats fails the second assertion.
    #[cfg(unix)]
    #[test]
    fn the_drawing_thread_names_a_process_with_no_stat() {
        let pid = i32::try_from(std::process::id()).expect("pid");
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .expect("this test's executable");
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .expect("its file name");
        let exe = exe.to_string_lossy().into_owned();
        let statted = LeaderImage::of(pid);
        let unstatted = LeaderImage::names_only(pid);
        assert!(
            statted.is_file(&exe) && statted.is_program(&exe),
            "a reader's own thread stats the image's path"
        );
        assert!(
            !unstatted.is_file(&exe) && !unstatted.is_program(&exe),
            "the thread that draws stats nothing"
        );
        assert!(
            statted.names().contains(&name) && unstatted.names().contains(&name),
            "both read the kernel's names: {:?} {:?}",
            statted.names(),
            unstatted.names()
        );
    }

    #[test]
    fn a_shim_script_named_after_an_agent_is_that_agent() {
        let atpkg = |script: &str| script.contains("/pkg/agents/");
        let p = |a0: &str, a1: Option<&str>| program_from_argv(a0, a1, &atpkg, &NO_IMAGE);
        let twin = "/Users//x/Library/Application Support/aterm/pkg/agents/claude";
        assert_eq!(p("/bin/sh", Some(twin)).as_deref(), Some("claude"));
        assert_eq!(
            p("/bin/bash", Some("/opt/pkg/agents/codex")).as_deref(),
            Some("codex")
        );
        for agent in atpkg::stub::AGENT_PROGRAMS {
            let shim = format!("/opt/pkg/agents/{agent}");
            assert_eq!(p("/bin/sh", Some(&shim)).as_deref(), Some(*agent));
        }
        assert_eq!(
            program_from_argv(
                "/Users//x/Library/Application Support/aterm/pkg/store/claude/2026092201/bin/claude",
                None,
                &atpkg,
                &Mapped {
                    file: None,
                    names: &["claude"],
                    programs: &[],
                },
            )
            .as_deref(),
            Some("claude"),
            "the exec'd store claude, argv[0] as measured: a path with a blank, read whole \
             where the kernel keeps its name"
        );
        assert_eq!(p("/bin/sh", Some("build.sh")).as_deref(), Some("sh"));
        assert_eq!(p("/bin/sh", Some("-c")).as_deref(), Some("sh"));
        assert_eq!(p("-zsh", None).as_deref(), Some("zsh"));
        assert_eq!(p("sleep", Some("claude")).as_deref(), Some("sleep"));
        assert_eq!(p("", Some(twin)), None);
        assert_eq!(
            p("/bin/sh", Some("/Users//x/bin/claude")).as_deref(),
            Some("sh"),
            "a user's own wrapper named claude is not the agent"
        );
    }

    /// [`script_in`]: the script's own directory, as spelled or resolved —
    /// never a deeper one or a sibling.
    #[cfg(unix)]
    #[test]
    fn a_shim_is_a_file_directly_in_a_managed_dir() {
        let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
        let dir = scratch.path().to_path_buf();
        let agents = dir.join("pkg/agents");
        std::fs::create_dir_all(&agents).expect("scratch dir");
        std::os::unix::fs::symlink(dir.join("pkg"), dir.join("link")).expect("symlink");
        let dirs = [agents.clone()];
        let at = |p: std::path::PathBuf| script_in(&p.to_string_lossy(), &dirs);
        assert!(at(agents.join("claude")));
        assert!(
            at(dir.join("link/agents/claude")),
            "the prefix through a link"
        );
        assert!(!at(agents.join("sub/claude")), "deeper");
        assert!(!at(dir.join("pkg/claude")), "the prefix itself");
        assert!(!at(dir.join("elsewhere/agents/claude")), "another agents/");
    }

    #[test]
    fn the_head_is_argv0_and_a_nonempty_argv1() {
        let head = |words: &[&str]| {
            argv_head(
                &words
                    .iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>(),
            )
        };
        let owned = |a0: &str, a1: Option<&str>| Some((a0.to_string(), a1.map(str::to_string)));
        assert_eq!(
            head(&["sh", "/x/codex", "--resume"]),
            owned("sh", Some("/x/codex"))
        );
        assert_eq!(head(&["claude"]), owned("claude", None));
        assert_eq!(
            head(&["sh", ""]),
            owned("sh", None),
            "an empty argv[1] is none"
        );
        assert_eq!(head(&[]), None, "no argv");
        assert_eq!(head(&["", "/x/codex"]), None, "an empty argv[0]");
    }

    /// A `KERN_PROCARGS2` buffer as the kernel lays one out: argc, the exec
    /// path, NUL padding, argv, then the environment.
    #[cfg(target_os = "macos")]
    fn procargs2(exec: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
        let mut buf = i32::try_from(argv.len()).unwrap().to_ne_bytes().to_vec();
        buf.extend_from_slice(exec.as_bytes());
        buf.extend_from_slice(b"\0\0\0\0");
        for word in argv.iter().chain(env) {
            buf.extend_from_slice(word.as_bytes());
            buf.push(0);
        }
        buf
    }

    const PREFIX: &str = "/Users//o/Library/Application Support/aterm/pkg";

    #[cfg(target_os = "macos")]
    fn layout() -> atpkg::store::Layout {
        atpkg::store::Layout {
            prefix: std::path::PathBuf::from(PREFIX),
        }
    }

    #[cfg(target_os = "macos")]
    fn measured_4205(path: &str) -> atpkg::caller_shell::ProcArgs {
        let exec = "/Users//o/.local/share/claude/versions/2.1.281";
        atpkg::caller_shell::parse_procargs2(&procargs2(
            exec,
            &[exec, "--resume", "03396a15"],
            &[&format!("PATH={path}"), "ATERM_CHILD=1", "HOME=/Users//o"],
        ))
        .expect("the buffer parses")
    }

    #[cfg(target_os = "macos")]
    fn holds(dir: &str) -> bool {
        dir == "/Users//o/.local/bin" || dir == "/opt/homebrew/bin"
    }

    /// The PATH verdict from synthetic `KERN_PROCARGS2` buffers: `agents/` first is live, the
    /// reroute stubs first (with `ATERM_CHILD`) reroute-only, a foreign copy first frozen; a
    /// leader that is not the shell's direct child measures nothing.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_shells_path_is_measured_only_from_its_direct_child() {
        let agents = format!("{PREFIX}/agents");
        let reroute = format!("{PREFIX}/reroute");
        let no_shim = |_: &str| None;
        let facts = |path: &str, ppid: Option<i32>| {
            facts_from(
                &measured_4205(path),
                ppid,
                1916,
                Some(&layout()),
                &no_shim,
                &holds,
                &NO_IMAGE,
            )
        };
        let live = facts(
            &format!("{agents}:{reroute}:/Users//o/.local/bin:/usr/bin"),
            Some(1916),
        );
        assert_eq!(live.program.as_deref(), Some("claude"));
        assert_eq!(live.path, Some(PathVerdict::Live));
        assert_eq!(live.copy, Some("foreign"), "the native build is foreign");
        let rerouted = facts(
            &format!("{reroute}:/Users//o/.local/bin:/usr/bin"),
            Some(1916),
        );
        assert_eq!(rerouted.path, Some(PathVerdict::RerouteOnly));
        assert_eq!(PathVerdict::RerouteOnly.word(), "live");
        let frozen = facts(
            &format!("/Users//o/.local/bin:{agents}:/usr/bin"),
            Some(1916),
        );
        assert_eq!(
            frozen.path,
            Some(PathVerdict::Frozen),
            "a foreign copy first"
        );
        let bare = facts("/usr/bin:/bin", Some(1916));
        assert_eq!(
            bare.path,
            Some(PathVerdict::Frozen),
            "no managed directory at all"
        );
        let grandchild = facts(&format!("{agents}:/usr/bin"), Some(4000));
        assert_eq!(
            grandchild.path, None,
            "another parent's child measures nothing"
        );
        assert_eq!(grandchild.program.as_deref(), Some("claude"));
        assert_eq!(
            facts(&agents, None).path,
            None,
            "an unread parent measures nothing"
        );
        let outside = path_verdict(
            &format!("{reroute}:/Users//o/.local/bin"),
            false,
            &|d| d == agents,
            &|d| d == reroute,
            &holds,
        );
        assert_eq!(outside, PathVerdict::Frozen);
        let hidden = atpkg::caller_shell::parse_procargs2(&procargs2("/bin/zsh", &["-zsh"], &[]))
            .expect("argv only");
        let shell_read = facts_from(
            &hidden,
            Some(1),
            1916,
            Some(&layout()),
            &no_shim,
            &holds,
            &NO_IMAGE,
        );
        assert_eq!(shell_read.path, None);
        assert_eq!(shell_read.copy, None, "zsh is no agent");
    }

    #[test]
    fn an_agents_copy_is_managed_only_from_the_store() {
        let store = std::path::PathBuf::from(format!("{PREFIX}/store"));
        let store = Some(store.as_path());
        let managed = format!("{PREFIX}/store/claude/1000002000001000282/bin/claude");
        assert_eq!(
            copy_of(Some("claude"), &managed, store, false),
            Some("managed")
        );
        assert_eq!(
            copy_of(
                Some("claude"),
                "/Users//o/.local/share/claude/versions/2.1.281",
                store,
                false
            ),
            Some("foreign")
        );
        assert_eq!(
            copy_of(
                Some("codex"),
                "/opt/homebrew/Caskroom/codex/0.149.0/codex",
                store,
                false
            ),
            Some("foreign")
        );
        assert_eq!(
            copy_of(Some("claude"), "/bin/sh", store, true),
            Some("managed")
        );
        assert_eq!(copy_of(Some("zsh"), "/bin/zsh", store, false), None);
        assert_eq!(
            copy_of(Some("claude"), &managed, None, false),
            None,
            "no prefix"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_reroute_stub_is_named_its_agent_with_no_copy() {
        let layout = atpkg::store::Layout {
            prefix: std::path::PathBuf::from(PREFIX),
        };
        let stub = layout.reroute_dir().join("claude");
        let twin = layout.agents_dir().join("claude");
        let facts = |script: &std::path::Path, shim_of: &dyn Fn(&str) -> Option<Shim>| {
            let args = atpkg::caller_shell::ProcArgs {
                exec_path: "/bin/sh".to_string(),
                argv: vec![
                    "/bin/sh".to_string(),
                    script.to_string_lossy().into_owned(),
                    "--resume".to_string(),
                ],
                env: Vec::new(),
            };
            facts_from(
                &args,
                None,
                0,
                Some(&layout),
                shim_of,
                &|_| false,
                &NO_IMAGE,
            )
        };
        let now = |script: &str| shim_in(script, &layout);
        let rerouted = facts(&stub, &now);
        assert_eq!(rerouted.program.as_deref(), Some("claude"));
        assert_eq!(rerouted.copy, None, "the stub proves no install");
        let managed = facts(&twin, &now);
        assert_eq!(managed.program.as_deref(), Some("claude"));
        assert_eq!(managed.copy, Some("managed"));
        assert_eq!(
            facts(std::path::Path::new("/Users//o/bin/claude"), &now)
                .program
                .as_deref(),
            Some("sh"),
            "a user's own wrapper"
        );
        let old = |script: &str| {
            script_in(script, &[layout.agents_dir(), layout.bin_dir()]).then_some(Shim::Managed)
        };
        assert_eq!(
            facts(&stub, &old).program.as_deref(),
            Some("sh"),
            "NEGATIVE CONTROL: the old rule names the stub sh"
        );
        assert_eq!(
            shim_in(&stub.to_string_lossy(), &layout),
            Some(Shim::Reroute)
        );
        assert_eq!(
            shim_in(&twin.to_string_lossy(), &layout),
            Some(Shim::Managed)
        );
        assert_eq!(
            shim_in(&layout.bin_dir().join("claude").to_string_lossy(), &layout),
            Some(Shim::Managed)
        );
    }

    #[test]
    fn a_runtime_running_an_agents_script_runs_a_foreign_copy() {
        let layout = atpkg::store::Layout {
            prefix: std::path::PathBuf::from(PREFIX),
        };
        let facts = |exec: &str, argv: &[&str]| {
            let args = atpkg::caller_shell::ProcArgs {
                exec_path: exec.to_string(),
                argv: argv.iter().map(|a| (*a).to_string()).collect(),
                env: Vec::new(),
            };
            facts_from(
                &args,
                None,
                0,
                Some(&layout),
                &|_| None,
                &|_| false,
                &NO_IMAGE,
            )
        };
        let npm = facts(
            "/usr/local/bin/node",
            &["node", "/usr/local/bin/claude", "--resume", "03396a15"],
        );
        assert_eq!(npm.program.as_deref(), Some("node"), "the process's name");
        assert_eq!(npm.copy, Some("foreign"), "an npm Claude is foreign");
        let server = facts("/usr/local/bin/node", &["node", "server.js"]);
        assert_eq!(server.program.as_deref(), Some("node"));
        assert_eq!(server.copy, None, "a runtime's own script is no agent");
        let flag = facts("/usr/local/bin/node", &["node", "--inspect"]);
        assert_eq!(flag.copy, None, "an option names no script");
        let wrapper = facts("/bin/sh", &["sh", "/home/u/bin/claude"]);
        assert_eq!(wrapper.program.as_deref(), Some("sh"));
        assert_eq!(wrapper.copy, None, "a user's wrapper, not a runtime");
    }

    #[test]
    fn only_a_laid_agent_can_be_shadowed() {
        let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
        std::fs::write(scratch.path().join("muse"), b"").expect("a foreign muse");
        let dir = scratch.path().to_str().expect("utf-8");
        assert!(!holds_agent(dir, &["claude"]), "muse has no twin here");
        assert!(holds_agent(dir, &["claude", "muse"]));
    }

    const PARK: &str = "ATERM_PROGRAM_TEST_PARK";

    /// A live child of this process, read by the resolver itself: its PATH is measured when
    /// this process is named its shell, and not otherwise.
    #[cfg(unix)]
    #[test]
    fn a_live_child_is_measured_against_its_real_parent() {
        let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
        let release = scratch.path().join("read");
        let mut stand_in = crate::test_alone::StandIn::helper(module_path!(), "park_for_a_read");
        stand_in
            .command()
            .env(PARK, &release)
            .env("PATH", "/nonexistent/agents:/usr/bin");
        let mut child = [stand_in.spawn()];
        let pid = i32::try_from(child[0].id()).expect("pid");
        let parent = i32::try_from(std::process::id()).expect("pid");
        let mut read = None;
        crate::test_alone::until_or_ended(
            "a non-platform child's environment is readable",
            &mut child,
            || {
                read = u32::try_from(pid)
                    .ok()
                    .and_then(atpkg::caller_shell::process_args)
                    .filter(|a| a.env_var(PARK).is_some());
                read.is_some()
            },
        );
        let ppid = parent_of(pid);
        std::fs::write(&release, b"read").expect("release the stand-in");
        let [child] = child;
        child.passed();
        let args = read.expect("read above");
        assert_eq!(ppid, Some(parent));
        let fake = atpkg::store::Layout {
            prefix: std::path::PathBuf::from("/nonexistent"),
        };
        let facts = |shell: i32| {
            facts_from(
                &args,
                ppid,
                shell,
                Some(&fake),
                &|_| None,
                &|_| false,
                &NO_IMAGE,
            )
        };
        assert_eq!(facts(parent).path, Some(PathVerdict::Live));
        assert_eq!(facts(parent + 1).path, None, "not this shell's child");
    }

    #[test]
    fn park_for_a_read() {
        let Some(release) = std::env::var_os(PARK) else {
            return;
        };
        let release = std::path::PathBuf::from(release);
        let released = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(10),
            || release.exists().then_some(()),
        );
        assert!(released.is_some(), "the parent never finished its read");
    }

    #[test]
    fn a_late_answer_for_a_departed_group_is_dropped() {
        let tl = Arc::new(Mutex::new(SessionTimeline::default()));
        assert!(tl.lock().unwrap().note_foreground_group(10));
        tl.lock().unwrap().set_program(10, Some("sleep".into()));
        assert_eq!(tl.lock().unwrap().agent().program.as_deref(), Some("sleep"));
        assert!(!tl.lock().unwrap().note_foreground_group(10), "same group");
        assert!(tl.lock().unwrap().note_foreground_group(11));
        assert_eq!(
            tl.lock().unwrap().agent().program,
            None,
            "the old name goes"
        );
        tl.lock().unwrap().set_program(10, Some("sleep".into()));
        assert_eq!(
            tl.lock().unwrap().agent().program,
            None,
            "late answer dropped"
        );
    }

    #[test]
    fn no_harness_flag_or_env_is_detected_on_leader() {
        let make = |argv: &[&str], env: &[&str]| {
            let args = atpkg::caller_shell::ProcArgs {
                exec_path: "/opt/agy".to_string(),
                argv: argv.iter().map(|s| (*s).to_string()).collect(),
                env: env.iter().map(|s| (*s).to_string()).collect(),
            };
            facts_from(&args, None, 0, None, &|_| None, &|_| false, &NO_IMAGE).no_harness
        };
        assert!(make(&["agy", "--no-harness"], &[]));
        assert!(make(&["agy", "--no-alab-harness"], &[]));
        assert!(make(&["agy"], &["ATERM_NO_HARNESS=1"]));
        assert!(!make(&["agy", "--", "--no-harness"], &[]));
        assert!(!make(&["agy"], &["ATERM_NO_HARNESS=0"]));
        assert!(!make(&["agy"], &[]));

        let mut tl = crate::session_timeline::SessionTimeline::default();
        assert!(tl.note_foreground_group(4100));
        assert!(tl.set_leader_no_harness(4100, true));
        assert!(tl.leader_no_harness());
        assert!(tl.unapplied_leader_no_harness());
        assert!(tl.take_leader_no_harness());
        assert!(!tl.unapplied_leader_no_harness());
        assert!(!tl.take_leader_no_harness());

        assert!(tl.note_foreground_group(4200));
        assert!(!tl.leader_no_harness());
        assert!(tl.set_leader_no_harness(4200, true));
        assert!(tl.unapplied_leader_no_harness());
        assert!(tl.take_leader_no_harness());
    }

    /// The argv a `targo` shim's hop execs its front with (`atpkg::build_cache_registered`'s
    /// rendered hop, as its tests record it): the helper, the verb, `--shim targo <placed>`, the
    /// store's `targo` and the typed words.
    const HOP: [&str; 9] = [
        "/Applications/aterm.app/Contents/MacOS/atpkg",
        "__build_admit",
        "--shim",
        "targo",
        "config",
        "/Users//o/.local/share/aterm/store/trust/bin/targo",
        "--unverified",
        "build",
        "-p",
    ];

    fn read_as(exec: &str, argv: &[&str]) -> LeaderFacts {
        let args = atpkg::caller_shell::ProcArgs {
            exec_path: exec.to_string(),
            argv: argv.iter().map(|s| (*s).to_string()).collect(),
            env: Vec::new(),
        };
        facts_from(
            &args,
            Some(1916),
            1916,
            None,
            &|_| None,
            &|_| false,
            &NO_IMAGE,
        )
    }

    /// A BUILD'S FRONT IS NAMED BY THE BUILD IT BECOMES, WAITING: the leader whose argv is a
    /// shim's hop reads `program=targo` (its target's name, as the tab names the build once the
    /// front execs it) and waits for memory, its figures unread until its ledger is; the
    /// fallback reader names it the same way. NEGATIVE CONTROL: the same image running another
    /// verb reads `atpkg` with no wait, a build typed plainly reads `targo` with none, and a
    /// search for the verb's word reads as the search.
    #[test]
    fn a_front_is_named_by_the_build_it_becomes_and_waits() {
        let exec = HOP[0];
        let front = read_as(exec, &HOP);
        assert_eq!(front.program.as_deref(), Some("targo"));
        assert_eq!(front.build_wait, Some(BuildWait::default()));
        assert_eq!(front.copy, None);
        let words: Vec<String> = HOP.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(front_program(&words).as_deref(), Some("targo"));
        let registration = read_as(
            exec,
            &[
                exec,
                "__build_cache_register",
                "targo",
                "--unverified",
                "build",
            ],
        );
        assert_eq!(registration.program.as_deref(), Some("atpkg"));
        assert_eq!(registration.build_wait, None);
        let capabilities = read_as(exec, &[exec, "__build_admit", "--capabilities"]);
        assert_eq!(capabilities.program.as_deref(), Some("atpkg"));
        assert_eq!(capabilities.build_wait, None);
        let typed = read_as(HOP[5], &[HOP[5], "--unverified", "build"]);
        assert_eq!(typed.program.as_deref(), Some("targo"));
        assert_eq!(typed.build_wait, None);
        let search = read_as("/usr/bin/rg", &["rg", "__build_admit", "crates/atpkg"]);
        assert_eq!(search.program.as_deref(), Some("rg"));
        assert_eq!(search.build_wait, None);
    }

    /// What a front's ledger says becomes its wait: waiting with its claim and the claims ahead;
    /// admitted, none; unread, waiting with no figures. `status` prints the claim to a tenth of a
    /// GiB and `-` for what is unknown. NEGATIVE CONTROL: a leader with no wait prints `-` for
    /// all three.
    #[test]
    fn a_fronts_wait_is_what_its_ledger_says() {
        use atpkg::build_admit::FrontClaim;
        let gib = 1_u64 << 30;
        let waiting = BuildWait::of(Some(FrontClaim::Waiting {
            mem: 12 * gib,
            ahead: vec![71, 72],
        }));
        assert_eq!(
            waiting,
            Some(BuildWait {
                mem: Some(12 * gib),
                ahead: Some(2)
            })
        );
        assert_eq!(BuildWait::of(Some(FrontClaim::Held)), None);
        assert_eq!(BuildWait::of(None), Some(BuildWait::default()));
        assert_eq!(
            BuildWait::fields(waiting.as_ref()),
            " wait=memory wait_gib=12.0 wait_ahead=2"
        );
        let odd = BuildWait {
            mem: Some(gib * 147 / 10),
            ahead: Some(0),
        };
        assert_eq!(
            BuildWait::fields(Some(&odd)),
            " wait=memory wait_gib=14.7 wait_ahead=0"
        );
        assert_eq!(
            BuildWait::fields(Some(&BuildWait::default())),
            " wait=memory wait_gib=- wait_ahead=-"
        );
        assert_eq!(BuildWait::fields(None), " wait=- wait_gib=- wait_ahead=-");
    }

    /// The timeline keeps a front's wait for its group alone: a reading for another group is
    /// dropped, a move of the foreground takes the wait away, and the same group read again as
    /// admitted clears it. NEGATIVE CONTROL: an unchanged reading reports no change.
    #[test]
    fn the_timeline_keeps_a_fronts_wait_for_its_group_alone() {
        let wait = BuildWait {
            mem: Some(1 << 30),
            ahead: Some(1),
        };
        let mut tl = crate::session_timeline::SessionTimeline::default();
        assert!(tl.note_foreground_group(5100));
        assert!(!tl.set_build_wait(5200, Some(wait)), "another group's");
        assert_eq!(tl.build_wait(), None);
        assert!(tl.set_build_wait(5100, Some(wait)));
        assert!(!tl.set_build_wait(5100, Some(wait)), "no change");
        assert_eq!(tl.build_wait(), Some(&wait));
        assert!(
            tl.set_build_wait(5100, None),
            "admitted: it execs its build"
        );
        assert_eq!(tl.build_wait(), None);
        assert!(tl.set_build_wait(5100, Some(wait)));
        assert!(tl.note_foreground_group(5300));
        assert_eq!(tl.build_wait(), None, "the front left the foreground");
    }
}
