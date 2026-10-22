// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Program identity: `status program=` and the `sessions` row's `program=`, the argv[0]
//! basename of the PTY's foreground process-group leader.
//!
//! The process table names the program even when shell integration (`detail=`) is absent or
//! lost. It reads argv[0] (`KERN_PROCARGS2` on macOS, `/proc/<pid>/cmdline` on Linux), not the
//! executable path: Claude Code's self-updater runs `~/.local/share/claude/versions/<v>`, so
//! `proc_pidpath` and `p_comm` say the version while argv[0] says `claude`.
//!
//! THE SHIM WINDOW. `claude` and `codex` typed in an aterm shell run atpkg's
//! twin, a `#!/bin/sh` script (`…/pkg/agents/claude`) that `exec`s the store
//! binary. Measured 2026-09-23 on a private headless aterm with a scratch
//! HOME: once exec'd, the store claude (2.1.280) and codex both keep the
//! store path as argv[0] (`…/store/claude/2026092201/bin/claude`), so they
//! read `claude` / `codex`. But until the `exec` the group's leader is
//! `/bin/sh …/agents/claude` — argv[0] `/bin/sh` — and the exec keeps the
//! pid and the group. Before bounded name confirmation, a resolution in that
//! window read `sh` (measured with a twin that had not exec'd yet) and could
//! stand through a motionless trust dialog as `program=sh agent=-`. A shell
//! interpreter whose first argument is atpkg's OWN shim
//! for an agent — a script named after it, directly in the managed prefix's
//! `agents/` or `bin/`, or the session-scoped `reroute/` stub that runs first
//! inside an aterm session ([`atpkg_shim`]) — is that agent
//! ([`program_from_argv`]). A reroute stub names the agent only: it is never
//! proof of an install (its own header says so), so its `copy=` is `-` until
//! the image it `exec`s is read (card engine §3.5).
//! Only that file: a user's own wrapper script named `claude` elsewhere keeps
//! the interpreter's name while the wrapper runs. If it later execs Claude,
//! the bounded same-group confirmation can name the new image.
//!
//! When: the status sweep asks for a resolution when the foreground group changes
//! (`SessionTimeline::note_foreground_group`), on bounded unknown-name and transient-shell
//! deadlines, and after screen movement when an `exec` keeps the group. The sysctl runs on one
//! background thread ([`ProgramResolver`]), never the event loop; its queue keeps one latest
//! job per session, and a changed answer wakes the status observer.
//!
//! The same read gives the agent's copy and the shell's PATH ([`leader_facts`]): `copy=` from
//! the exec path ([`copy_of`]), and the PATH verdict ([`path_verdict`]) from the environment at
//! exec, taken only from a leader whose parent is the session's shell. The shell itself cannot
//! be read: macOS hides a platform binary's environment (`KERN_PROCARGS2` of `/bin/zsh` is argv
//! only), so the evidence comes from its children.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::session_timeline::SessionTimeline;

/// The longest program token published, in bytes.
const PROGRAM_MAX: usize = 32;

/// The program name argv[0] names: its basename, a login shell's leading `-` dropped, and a
/// vendor version directory read as its tool (`…/claude/versions/2.1.280` is `claude`).
/// Reduced to `[A-Za-z0-9._+-]` and capped at [`PROGRAM_MAX`] bytes; `None` when nothing is
/// left.
pub(crate) fn program_from_argv0(argv0: &str) -> Option<String> {
    let trimmed = argv0.trim_end_matches('/');
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

// The interpreters an agent's shim script runs under (`#!/bin/sh`), whose
// first argument then names the script ([`program_from_argv`]): atpkg's own
// list, which its image classifier reads too (`atpkg::image`).
use atpkg::image::SCRIPT_SHELLS;

/// The published program of a process whose argv begins `argv0 [argv1]`: [`program_from_argv0`],
/// except that a shell interpreter ([`SCRIPT_SHELLS`]) running a script (`argv1` not an option)
/// named after an agent program and that `is_shim` accepts ([`atpkg_shim`] in production) is
/// that agent, as the store binary it is about to exec would be named. Any other script keeps
/// the interpreter's name (`sh build.sh`, a user's own `sh ~/bin/claude`). `is_shim` is asked
/// only for an agent-named script.
pub(crate) fn program_from_argv(
    argv0: &str,
    argv1: Option<&str>,
    is_shim: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let name = program_from_argv0(argv0)?;
    if SCRIPT_SHELLS.contains(&name.as_str())
        && let Some(script) = argv1.filter(|a| !a.starts_with('-'))
        && let Some(agent) = program_from_argv0(script)
        && atpkg::stub::is_agent_program(&agent)
        && is_shim(script)
    {
        return Some(agent);
    }
    Some(name)
}

/// Whether `script`'s directory is one of `dirs`, as spelled or resolved
/// (a prefix reached through a symlink, a PATH entry spelled another way).
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
    /// The managed twin in `agents/`, or a `bin/` shim: proof of a managed
    /// copy (`copy=managed`).
    Managed,
    /// The session-scoped `reroute/` stub: names its agent, proves nothing
    /// (`copy=-`), as its header says ("never proof of an install").
    Reroute,
}

/// Which shim `script` is in `layout`: a file directly in its `agents/` or
/// `bin/` ([`Shim::Managed`]) or its `reroute/` ([`Shim::Reroute`]), its
/// directory as spelled or resolved ([`script_in`]).
pub(crate) fn shim_in(script: &str, layout: &atpkg::store::Layout) -> Option<Shim> {
    if script_in(script, &[layout.agents_dir(), layout.bin_dir()]) {
        Some(Shim::Managed)
    } else if script_in(script, &[layout.reroute_dir()]) {
        Some(Shim::Reroute)
    } else {
        None
    }
}

/// [`shim_in`] the configured managed prefix
/// (`atpkg::store::resolve_configured`, the prefix every reader outside
/// atpkg's CLI resolves). Reads `aterm.toml` and stats the prefix, so it runs
/// only for an agent-named script, on the resolver thread.
pub(crate) fn shim_of(script: &str) -> Option<Shim> {
    atpkg::store::resolve_configured().and_then(|layout| shim_in(script, &layout))
}

/// Whether `script` is atpkg's own shim of either kind, for naming
/// ([`program_from_argv`]): the reroute stub is named its agent too, at its
/// exec, so the sweep names a launch before the store image's `exec`.
pub(crate) fn atpkg_shim(script: &str) -> bool {
    shim_of(script).is_some()
}

/// argv[0] and argv[1] of `pid`, as the process itself was exec'd with them,
/// or `None` (gone, another user's, or a platform without the call).
pub(crate) fn argv_head_of(pid: i32) -> Option<(String, Option<String>)> {
    if pid <= 0 {
        return None;
    }
    platform_argv_head(pid)
}

/// The published program of `pid`: [`program_from_argv`] of [`argv_head_of`].
pub(crate) fn program_of(pid: i32) -> Option<String> {
    let (argv0, argv1) = argv_head_of(pid)?;
    program_from_argv(&argv0, argv1.as_deref(), &atpkg_shim)
}

#[cfg(target_os = "macos")]
fn platform_argv_head(pid: i32) -> Option<(String, Option<String>)> {
    // aterm-uds's reader, whose buffer holds the whole answer: one sized by a
    // size query first was given only the argc of a shell that exec'd between
    // the two calls.
    argv_head_from_procargs2(&aterm_uds::procargs::read(u32::try_from(pid).ok()?)?)
}

#[cfg(target_os = "linux")]
fn platform_argv_head(pid: i32) -> Option<(String, Option<String>)> {
    // aterm-uds's read, which leaks nothing under Rosetta and drops the word its
    // interpreter put first (`aterm_uds::process::read_cmdline`).
    let bytes = aterm_uds::process::read_cmdline(u32::try_from(pid).ok()?)?;
    let mut args = bytes.split(|b| *b == 0);
    let first = args.next().filter(|a| !a.is_empty())?;
    let second = args.next().filter(|a| !a.is_empty());
    let text = |a: &[u8]| String::from_utf8_lossy(a).into_owned();
    Some((text(first), second.map(text)))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_argv_head(_pid: i32) -> Option<(String, Option<String>)> {
    None
}

/// argv[0] and (when `argc > 1`) argv[1] out of a `KERN_PROCARGS2` buffer
/// ([`aterm_uds::procargs::parse`]), each where it is not empty. Pure, so the
/// policy is testable without a process.
#[cfg(any(target_os = "macos", test))]
fn argv_head_from_procargs2(buf: &[u8]) -> Option<(String, Option<String>)> {
    let args = aterm_uds::procargs::parse(buf)?;
    let text = |a: &&[u8]| String::from_utf8_lossy(a).into_owned();
    let argv0 = args.argv.first().filter(|a| !a.is_empty()).map(text)?;
    let argv1 = args.argv.get(1).filter(|a| !a.is_empty()).map(text);
    Some((argv0, argv1))
}

// ---------------------------------------------------------------- the leader's facts

/// What the shell's exported PATH does with the agent programs, read from a child's
/// environment at exec ([`path_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathVerdict {
    /// aterm's managed `agents/` comes before every other directory holding an
    /// agent program.
    Live,
    /// No `agents/` ahead of a foreign copy, but the session's `reroute/` stubs are, with
    /// `ATERM_CHILD` set: typed `claude`/`codex` still reach the managed twin, unless the shell
    /// hashed the foreign path before the stubs existed (zsh keeps it until `rehash`).
    RerouteOnly,
    /// A foreign copy wins, or neither managed directory is on PATH.
    Frozen,
}

impl PathVerdict {
    /// The `path=` word: a reroute-only PATH reaches the managed twin, so it is `live`.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Live | Self::RerouteOnly => "live",
            Self::Frozen => "frozen",
        }
    }
}

/// The PATH verdict: walk `path_env` in order. aterm's `agents/` (`is_agents`) before any
/// directory that holds an agent (`holds_agent`) is [`PathVerdict::Live`]; the session's
/// `reroute/` (`is_reroute`) first, with `aterm_child` set (its stubs route on it), is
/// [`PathVerdict::RerouteOnly`]; anything else, including neither managed directory on PATH,
/// is [`PathVerdict::Frozen`].
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

/// `copy=` for an agent the leader runs: `managed` when its executable is under the managed
/// prefix's `store/` (the twin execs the store binary by that path) or it is atpkg's shim
/// before its exec (`shim`); `foreign` otherwise (the native installer's
/// `~/.local/share/claude/versions/<v>`, a brew cask). `None` for a program that is not an
/// agent program, or with no managed prefix.
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

/// What one read of the foreground leader tells: its name, the agent copy it runs and, for a
/// direct child of the session's shell with a readable environment, the PATH verdict.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeaderFacts {
    /// [`program_from_argv`].
    pub(crate) program: Option<String>,
    /// [`copy_of`] the program, or the agent script a runtime runs
    /// (`node /usr/local/bin/claude`).
    pub(crate) copy: Option<&'static str>,
    /// [`path_verdict`] of the leader's PATH at exec.
    pub(crate) path: Option<PathVerdict>,
}

/// [`LeaderFacts`] from one parsed `KERN_PROCARGS2` read (`args`), the leader's parent (`ppid`)
/// and the session's shell (`shell`). The PATH verdict needs `ppid == shell` and a PATH in the
/// environment: a grandchild's PATH may be anything, and a platform binary's environment reads
/// empty. `managed` is the prefix's layout, when there is one.
pub(crate) fn facts_from(
    args: &atpkg::caller_shell::ProcArgs,
    ppid: Option<i32>,
    shell: i32,
    managed: Option<&atpkg::store::Layout>,
    shim_of: &dyn Fn(&str) -> Option<Shim>,
    holds_agent: &dyn Fn(&str) -> bool,
) -> LeaderFacts {
    let argv0 = args.argv.first().map_or("", String::as_str);
    let argv1 = args.argv.get(1).map(String::as_str);
    let program = program_from_argv(argv0, argv1, &|script| shim_of(script).is_some());
    let shim = program
        .as_deref()
        .is_some_and(|p| program_from_argv0(argv0).as_deref() != Some(p));
    // A reroute stub names its agent and proves no copy: `copy=-` until the
    // image it `exec`s is read.
    let reroute = shim && argv1.and_then(shim_of) == Some(Shim::Reroute);
    let store = managed.map(|layout| layout.prefix.join("store"));
    // A script runtime running an agent's script (an npm install of Claude
    // Code: `node /usr/local/bin/claude`) keeps the runtime's name, as any
    // script does, but the copy it runs is that agent's, and no store build
    // is a script, so it reads `foreign`. The script's name decides the copy
    // only, never the program: `node ~/bin/claude` stays `program=node`.
    let script_agent = program
        .as_deref()
        .filter(|p| aterm_phase::may_host_agent(p))
        .and(argv1)
        .filter(|script| !script.starts_with('-'))
        .and_then(program_from_argv0)
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
    LeaderFacts {
        program,
        copy,
        path,
    }
}

/// Whether `dir` holds one of `laid`, the agent programs whose managed twin is laid
/// (`atpkg::reroute::agents_routes`): only those can be shadowed by a directory ahead of
/// `agents/`.
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
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state ppid …` — comm may hold spaces and parens; the last
    // `)` ends it.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_parent(_pid: i32) -> Option<i32> {
    None
}

/// [`LeaderFacts`] of `pgid`'s leader in the session whose shell is `shell`:
/// one `KERN_PROCARGS2` read (argv, exec path and environment) and one parent
/// read. A buffer the full parser does not recognise still names the program
/// through the argv-only reader, with no copy and no PATH verdict.
pub(crate) fn leader_facts(pgid: i32, shell: i32) -> LeaderFacts {
    let Some(args) = u32::try_from(pgid)
        .ok()
        .filter(|p| *p > 0)
        .and_then(atpkg::caller_shell::process_args)
    else {
        return LeaderFacts {
            program: program_of(pgid),
            ..LeaderFacts::default()
        };
    };
    let managed = atpkg::store::resolve_configured();
    let laid = managed
        .as_ref()
        .map(atpkg::reroute::agents_routes)
        .unwrap_or_default();
    facts_from(
        &args,
        parent_of(pgid),
        shell,
        managed.as_ref(),
        &|script| managed.as_ref().and_then(|layout| shim_in(script, layout)),
        &|dir| holds_agent(dir, &laid),
    )
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
    /// session's log (`claude_footer::LogWake`, session log Phase 3B).
    wake: crate::claude_footer::LogWake,
}

/// The leader read a job runs: `(pgid, shell)` to [`LeaderFacts`] ([`leader_facts`] in
/// production).
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

    /// Replace a session's latest request. Only a new session needs a channel
    /// token: a queued worker reads the replacement, and an in-flight worker
    /// requeues once after its current lookup finishes.
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

    /// A crashed worker lost its local queue and channel. Every retained slot
    /// gets exactly one token on the replacement channel, including the lookup
    /// that was in flight when the old worker exited.
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

    /// A changed in-flight slot needs one more pass over its latest job. The
    /// caller puts its existing token at the tail after other queued sessions.
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

    /// Leave a tombstone for its one queued token or running read. Dropping
    /// the slot here would allow an immediate re-enable to enqueue a second
    /// token, and could accept the old read as the new request's answer.
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

/// The background thread program resolutions run on, started on first use. It holds at most
/// one queued token and one latest request per session, however long a lookup stalls; a newer
/// foreground group replaces the slot and runs after the in-flight lookup.
/// [`SessionTimeline::set_program`] drops an answer for a group no longer in front.
pub(crate) struct ProgramResolver {
    tx: Option<Sender<u64>>,
    worker: Option<JoinHandle<()>>,
    pending: Arc<Mutex<PendingJobs>>,
    lookup: Lookup,
    completion: Option<Completion>,
    /// The log's wake the footer reads this resolver starts carry
    /// ([`Self::set_log_wake`]).
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
    /// A resolver whose reads are `lookup`'s: the status sweep's binds put a
    /// program in front with no process (`session_status_edge_inbox_tests`).
    #[cfg(test)]
    pub(crate) fn with_lookup(
        lookup: impl Fn(i32, i32) -> LeaderFacts + Send + Sync + 'static,
    ) -> Self {
        Self {
            lookup: Arc::new(lookup),
            ..Self::default()
        }
    }

    /// What the footer reads this resolver starts wake when they move a
    /// session's log (the instance's subscribers and the clause switch).
    pub(crate) fn set_log_wake(&mut self, wake: &crate::claude_footer::LogWake) {
        self.log_wake = wake.clone();
    }

    /// The status subsystem is off: cancel all queued work. An in-flight read may finish but
    /// cannot publish; a publication committed before this call may still post its wake, which
    /// the cleared status observer ignores.
    pub(crate) fn clear_pending(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel_all();
    }

    /// Retiring a tab releases its latest queued job even if the worker is
    /// blocked in a different session's process-table read. Its one channel
    /// token becomes a harmless tombstone when the worker reaches it.
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
                // Lookups run without the pending lock (lock order: `publish_if_current`).
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

    /// A panicked worker loses its queued ids and in-flight job; the map keeps every latest
    /// request, so the next request starts a worker that requeues each session once.
    /// `is_finished` keeps the UI thread from waiting on the worker.
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

    /// `Some(true)` means a new channel was populated from the retained map;
    /// the caller must not send a second token for its freshly offered job.
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

    /// Resolve `pgid`'s leader off-thread into `timeline`; if the thread cannot start, the
    /// program stays unknown (`program=-`). A request for a session already pending replaces its
    /// latest job rather than sending another token. `shell` is the session's shell pid: a
    /// leader that is its direct child also measures the shell's PATH ([`leader_facts`]).
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
        // `ensure_worker` requeued every retained slot on a fresh channel.
        if !started_fresh
            && new_slot
            && self.tx.as_ref().is_some_and(|tx| tx.send(session).is_err())
        {
            // The worker exited between `is_finished` and `send`. Its map
            // remains intact; a fresh worker requeues it immediately.
            self.tx = None;
            self.worker.take();
            let _ = self.ensure_worker();
        }
    }
}

/// Process one id at a time, draining distinct queued sessions before requeueing a hot
/// session's updated in-flight lookup. The worker owns no sender: dropping `ProgramResolver`
/// closes the channel and the worker exits once its map drains.
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
        // No lock is held across the wake callback; the footer send below re-takes the queue
        // lock to order it against retirement.
        if changed && let Some(wake) = &completion {
            wake(job.session);
        }
        if left_claude {
            crate::claude_footer::stop(job.session, job.pgid);
        }
        // The status sweep refreshes a still-named Claude footer itself; only the transition
        // into Claude needs an immediate request.
        if became_claude {
            // Ordered against `retire`/`clear_pending` by the queue lock: if they win, this
            // request is stale and suppressed; if this wins, their stop follows it on the
            // footer channel, so a dead tab's watch cannot outlive its stop.
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
    // Lock order is pending -> timeline. The UI callers of `request`, `retire` and
    // `clear_pending` hold no timeline guard, and `set_program` only rings the harness's leaf
    // bell. Holding both makes cancellation or a same-pgid re-enable land before or after the
    // name write, never between its check and the write.
    let pending = pending.lock().unwrap_or_else(|p| p.into_inner());
    if !pending.current(job, incarnation) {
        return (false, false, false);
    }
    let mut timeline = job.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let was_claude = crate::claude_footer::drawn_for(timeline.agent().program.as_deref());
    let changed = timeline.set_program(job.pgid, facts.program);
    // The copy and the PATH verdict ride the same answer, under the same
    // currency check: a departed group's read never reaches the timeline.
    timeline.set_leader(job.pgid, facts.copy, facts.path);
    let became_claude =
        changed && crate::claude_footer::drawn_for(timeline.agent().program.as_deref());
    (changed, became_claude, changed && was_claude)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The bound for waits on something that must happen; a check that something does not
    // happen keeps its own short window.
    use aterm_spec::wait::MUST_HAPPEN;

    /// A [`Lookup`] that names the program only, for the queue tests.
    fn names(f: impl Fn(i32) -> Option<String> + Send + Sync + 'static) -> Lookup {
        Arc::new(move |pgid, _shell| LeaderFacts {
            program: f(pgid),
            ..LeaderFacts::default()
        })
    }

    /// [`argv_head_of`] a just-spawned child once it reads exactly the head `expected` accepts,
    /// polled to a bounded deadline; at the deadline, the last read.
    ///
    /// `spawn` returning does not make the new image's argv readable. On Linux `posix_spawn`'s
    /// vfork releases the parent inside `execve` before the ELF loader sets the argv bounds, so
    /// `/proc/<pid>/cmdline` reads the parent's argv, then nothing, then the program's; a read
    /// can also be torn mid-exec. Only an exact match is settled.
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
            // SAFETY: signal delivery to the group this test created: `process_group(0)` made
            // the child's pid its pgid, and the unreaped child keeps that pid from reuse.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Conformance with the `ProgramResolverQueue` model: the slot transitions agree with it
    /// through a superseding group and a worker restart. The model's `Buggy` variant (each
    /// duplicate queued) fails its one-token invariant.
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

    /// Cancelling an in-flight lookup keeps its channel token; a re-enable replaces the answer,
    /// and the old lookup cannot publish even when the new request has the same PGID.
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

        // If cancellation occurs while queued, a re-enable also reuses the
        // token that has not yet been taken by the worker.
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

        // A cancelled queued token is consumed without performing a lookup.
        let mut cancelled_state = model.init_state();
        assert!(jobs.offer(job()));
        assert!(model.fire("AskFirst", &mut cancelled_state));
        jobs.cancel(7);
        assert!(model.fire("Cancel", &mut cancelled_state));
        assert!(jobs.take(7).is_none());
        assert!(model.fire("DropCancelled", &mut cancelled_state));
        assert!(jobs.by_session.is_empty());
    }

    /// The resolver's two footer transitions, for each name the host
    /// supervises as Claude Code: a `claude-code` argv0 entering the front
    /// asks for the footer's facts and leaving it stops the watch, as
    /// `claude` does. NEGATIVE CONTROL: a Codex leader starts no watch.
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

    /// A blocked old-group lookup cannot create an unbounded queue of retries
    /// or delay the replacement behind them. Another session gets its turn
    /// before the hot session's newest request.
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

        // A new request is a completion barrier: it can run only after the
        // old lookup has returned and been discarded.
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
            std::thread::yield_now();
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

    #[test]
    fn argv0_reduces_to_the_program_name() {
        assert_eq!(program_from_argv0("claude").as_deref(), Some("claude"));
        assert_eq!(program_from_argv0("-zsh").as_deref(), Some("zsh"));
        assert_eq!(program_from_argv0("/bin/sleep").as_deref(), Some("sleep"));
        assert_eq!(
            program_from_argv0("/Users//x/.local/share/claude/versions/2.1.280").as_deref(),
            Some("claude"),
            "a vendor version directory names its tool, not its version"
        );
        assert_eq!(
            program_from_argv0("/opt/versions/tool").as_deref(),
            Some("tool"),
            "only a VERSION-shaped basename under versions/ is rewritten"
        );
        assert_eq!(
            program_from_argv0("evil\u{1b}]0;x\u{7}").as_deref(),
            Some("evil0x")
        );
        assert_eq!(program_from_argv0(""), None);
        assert_eq!(program_from_argv0("///"), None);
        assert_eq!(
            program_from_argv0(&"a".repeat(100)).map(|s| s.len()),
            Some(32)
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
        // Wait for the exec to land, then name the process through the real `program_of`.
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

        // `exec -a` is a bash builtin (not POSIX sh's).
        let Some(bash) = aterm_spec::host::test_shell("bash") else {
            aterm_spec::host::could_not_run(format!(
                "the `exec -a` half did not run: {}",
                aterm_spec::host::tool("bash").refusal()
            ));
        };
        // A real bash, so a hermetic one (§8.6 of the init design): a scratch home, and no
        // inherited `$BASH_ENV`, which a non-interactive bash would source.
        let home = aterm_tempfile::tempdir().expect("scratch home");
        let mut child = quiet_child(
            aterm_spec::host::shell_command(&bash, home.path())
                .args(["-c", "exec -a claude sleep 30"]),
        )
        .spawn()
        .expect("spawn bash");
        let pid = i32::try_from(child.id()).expect("pid");
        // `exec -a` replaces the shell in place: the same exec window, the same poll.
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

    /// An interpreter running atpkg's shim named after an agent is that agent, and the store
    /// path names its tool. Any other script, an option, a login shell, a non-shell with an
    /// agent-named argument, and a user's own `~/bin/claude` keep their own names.
    #[test]
    fn a_shim_script_named_after_an_agent_is_that_agent() {
        let atpkg = |script: &str| script.contains("/pkg/agents/");
        let p = |a0: &str, a1: Option<&str>| program_from_argv(a0, a1, &atpkg);
        let twin = "/Users//x/Library/Application Support/aterm/pkg/agents/claude";
        assert_eq!(p("/bin/sh", Some(twin)).as_deref(), Some("claude"));
        assert_eq!(
            p("/bin/bash", Some("/opt/pkg/agents/codex")).as_deref(),
            Some("codex")
        );
        // Every agent program, not only claude and codex.
        for agent in atpkg::stub::AGENT_PROGRAMS {
            let shim = format!("/opt/pkg/agents/{agent}");
            assert_eq!(p("/bin/sh", Some(&shim)).as_deref(), Some(*agent));
        }
        assert_eq!(
            p(
                "/Users//x/Library/Application Support/aterm/pkg/store/claude/2026092201/bin/claude",
                None
            )
            .as_deref(),
            Some("claude"),
            "the exec'd store claude, argv[0] as measured"
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
        let dir = std::env::temp_dir().join(format!("aterm-shimdir-{}", std::process::id()));
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The head is argv[0], never the exec path, and argv[1] where argc says
    /// there is one; a process with no argv[0] has no head. The layout's edge
    /// cases are `aterm_uds::procargs`'s (`procargs_tests.rs`).
    #[test]
    fn procargs2_layout_yields_argv1_after_argv0() {
        let mut two = 2i32.to_ne_bytes().to_vec();
        two.extend_from_slice(b"/Users//x/.local/share/claude/versions/2.1.280\0\0\0\0");
        two.extend_from_slice(b"claude\0\0PATH=/bin\0");
        assert_eq!(
            argv_head_from_procargs2(&two),
            Some(("claude".to_string(), None)),
            "argv[0], not the exec path; an empty argv[1] is none"
        );
        assert_eq!(
            argv_head_from_procargs2(&0i32.to_ne_bytes()),
            None,
            "argc 0"
        );
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/sh\0\0\0");
        buf.extend_from_slice(b"/bin/sh\0/pkg/agents/claude\0--resume\0PATH=/bin\0");
        assert_eq!(
            argv_head_from_procargs2(&buf),
            Some((
                "/bin/sh".to_string(),
                Some("/pkg/agents/claude".to_string())
            ))
        );
        let mut one = 1i32.to_ne_bytes().to_vec();
        one.extend_from_slice(b"/bin/zsh\0\0-zsh\0HOME=/x\0");
        assert_eq!(
            argv_head_from_procargs2(&one),
            Some(("-zsh".to_string(), None)),
            "argc 1: the environment is not argv[1]"
        );
    }

    /// A `KERN_PROCARGS2` buffer as the kernel lays one out: argc, the exec
    /// path, NUL padding, argv, then the environment.
    #[cfg(target_os = "macos")] // fed only to atpkg::caller_shell::parse_procargs2, macOS-only
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

    #[cfg(target_os = "macos")] // atpkg::caller_shell::parse_procargs2 is macOS-only
    fn layout() -> atpkg::store::Layout {
        atpkg::store::Layout {
            prefix: std::path::PathBuf::from(PREFIX),
        }
    }

    #[cfg(target_os = "macos")] // atpkg::caller_shell::parse_procargs2 is macOS-only
    /// The `KERN_PROCARGS2` read of a foreign native claude exec'd with `PATH=<path>`; the
    /// tests name its shell pid 1916.
    fn measured_4205(path: &str) -> atpkg::caller_shell::ProcArgs {
        let exec = "/Users//o/.local/share/claude/versions/2.1.281";
        atpkg::caller_shell::parse_procargs2(&procargs2(
            exec,
            &[exec, "--resume", "03396a15"],
            &[&format!("PATH={path}"), "ATERM_CHILD=1", "HOME=/Users//o"],
        ))
        .expect("the buffer parses")
    }

    #[cfg(target_os = "macos")] // atpkg::caller_shell::parse_procargs2 is macOS-only
    /// Which directory holds a `claude`: `~/.local/bin` (the native installer's
    /// link) and brew's, never the managed ones (they are asked first).
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
        // The reroute stubs route only under `ATERM_CHILD`.
        let outside = path_verdict(
            &format!("{reroute}:/Users//o/.local/bin"),
            false,
            &|d| d == agents,
            &|d| d == reroute,
            &holds,
        );
        assert_eq!(outside, PathVerdict::Frozen);
        // A platform binary's hidden environment carries no PATH: no reading.
        let hidden = atpkg::caller_shell::parse_procargs2(&procargs2("/bin/zsh", &["-zsh"], &[]))
            .expect("argv only");
        let shell_read = facts_from(&hidden, Some(1), 1916, Some(&layout()), &no_shim, &holds);
        assert_eq!(shell_read.path, None);
        assert_eq!(shell_read.copy, None, "zsh is no agent");
    }

    /// `copy=`: the store's executable and atpkg's own shim are managed, anything else an agent
    /// runs is foreign, and a program that is no agent has no copy.
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

    /// REROUTE NAMING (card engine §3.5): the session-scoped reroute stub, a
    /// `#!/bin/sh` script directly in `reroute/`, is named its agent at its
    /// exec, with no copy: it is never proof of an install, and the store
    /// image it `exec`s sets the copy when it is read. The managed twin in
    /// `agents/` stays `copy=managed`, and a user's own wrapper stays `sh`.
    /// NEGATIVE CONTROL: the old rule (only `agents/` and `bin/` are shims)
    /// names the stub `sh`.
    #[test]
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
            facts_from(&args, None, 0, Some(&layout), shim_of, &|_| false)
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

    /// AN NPM CLAUDE IS A FOREIGN COPY: `node /usr/local/bin/claude` keeps
    /// the runtime's name (`program=node`), and its copy was asked of that
    /// name, which is no agent, so `copy=-` hid exactly the build `copy=`
    /// exists to show. The copy now asks the agent script a runtime runs.
    /// NEGATIVE CONTROLS: a runtime's own script has no copy, and a shell
    /// running a user's wrapper named `claude` (not atpkg's shim) keeps the
    /// shell's name and no copy, as before.
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
            facts_from(&args, None, 0, Some(&layout), &|_| None, &|_| false)
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

    /// A PATH directory shadows only an agent whose managed twin is laid.
    #[test]
    fn only_a_laid_agent_can_be_shadowed() {
        let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
        std::fs::write(scratch.path().join("muse"), b"").expect("a foreign muse");
        let dir = scratch.path().to_str().expect("utf-8");
        assert!(!holds_agent(dir, &["claude"]), "muse has no twin here");
        assert!(holds_agent(dir, &["claude", "muse"]));
    }

    /// The env var naming the file that releases [`park_for_a_read`]; set only on that stand-in.
    const PARK: &str = "ATERM_PROGRAM_TEST_PARK";

    /// A live child of this process, read by the resolver itself: its PATH is measured when
    /// this process is named its shell, and not otherwise. The stand-in must report it ran
    /// [`park_for_a_read`], so a renamed helper fails here.
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
        let facts = |shell: i32| facts_from(&args, ppid, shell, Some(&fake), &|_| None, &|_| false);
        assert_eq!(facts(parent).path, Some(PathVerdict::Live));
        assert_eq!(facts(parent + 1).path, None, "not this shell's child");
    }

    /// The stand-in [`a_live_child_is_measured_against_its_real_parent`] runs: inert unless
    /// [`PARK`] is set, then parked until that file exists, for at most a hang detector's minute.
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
}
