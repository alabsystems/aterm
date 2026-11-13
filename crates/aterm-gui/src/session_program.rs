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
#[path = "session_program_tests.rs"]
mod tests;
