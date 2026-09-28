// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PROGRAM IDENTITY: `status program=` and the `sessions` row's `program=`,
//! the argv[0] basename of the PTY's foreground process-group leader.
//!
//! WHY argv[0]. `detail=` comes from shell integration alone, so an adopted
//! shell whose marks were lost, or a shell with no integration, reads `-` for
//! the life of whatever it runs. The process table always knows. But not by
//! its executable path: Claude Code's self-updater runs the binary from
//! `~/.local/share/claude/versions/2.1.280`, so `proc_pidpath` and `p_comm`
//! both say `2.1.280` (measured 2026-09-23 with `lsof`), while argv[0] — what
//! `ps -o comm` prints — says `claude`. So this reads argv[0]
//! (`KERN_PROCARGS2` on macOS, `/proc/<pid>/cmdline` on Linux).
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
//! `agents/` or `bin/` ([`atpkg_shim`]) — is that agent ([`program_from_argv`]).
//! Only that file: a user's own wrapper script named `claude` elsewhere keeps
//! the interpreter's name while the wrapper runs. If it later execs Claude,
//! the bounded same-group confirmation can name the new image.
//!
//! WHEN. The status sweep already holds each due session's `tcgetpgrp`
//! answer; it asks for a resolution when the foreground group CHANGES
//! (`SessionTimeline::note_foreground_group`), on bounded unknown-name and
//! transient-shell deadlines, and after later screen movement when an `exec`
//! keeps the same group. The sysctl runs on one background thread
//! ([`ProgramResolver`]), never on the event loop. Its queue retains one
//! latest job per session, and a changed answer wakes the status observer
//! even after the screen stops moving.
//!
//! THE SAME READ MEASURES THE SHELL'S PATH AND THE AGENT'S COPY (gap audit
//! 2026-09-24). `path=frozen` was an adoption mark that nothing lowered: a tab
//! whose shell the live upgrade had healed at 2026-09-23 21:28 (its relaunch
//! line sources the atpkg hook; the resulting `claude`, pid 4205, had
//! `PATH=<prefix>/agents:…` at exec) read `path=frozen` a day later, and the
//! band told the owner to type `. ~/.aterm/shell.d/00-atpkg.zsh` into that
//! tab — whose foreground was Claude, so the line would have been a prompt.
//! And the real harm there was invisible: 4205 was the FOREIGN native build
//! (`~/.local/share/claude/versions/2.1.281`), which no column could say. The
//! `KERN_PROCARGS2` buffer this module already reads carries the leader's
//! exec path and its environment at exec, so one read answers both
//! ([`leader_facts`]): `copy=` from the exec path ([`copy_of`]), and the PATH
//! verdict ([`path_verdict`]) — taken ONLY from a leader whose parent is the
//! session's own shell, because that environment is the one the shell exported.
//! The shell cannot be read directly: macOS hides a platform binary's
//! environment (measured 2026-09-24: `KERN_PROCARGS2` of `/bin/zsh` is 25
//! bytes, argv only; an ad-hoc-signed copy of `/bin/sleep` exposed its env
//! where `/bin/sleep` hid it), so the evidence comes from its children.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::session_timeline::SessionTimeline;

/// The longest program token published, in bytes.
const PROGRAM_MAX: usize = 32;

/// The program name argv[0] names: its basename, a login shell's leading `-`
/// dropped (`-zsh` is `zsh`), and a vendor VERSION DIRECTORY read as the tool
/// it holds (`…/claude/versions/2.1.280` is `claude`). Reduced to
/// `[A-Za-z0-9._+-]` and capped at [`PROGRAM_MAX`] bytes; `None` when nothing
/// is left.
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

/// The interpreters an agent's shim script runs under (`#!/bin/sh`), whose
/// first argument then names the script ([`program_from_argv`]).
const SCRIPT_SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh"];

/// The published program of a process whose argv begins `argv0 [argv1]`:
/// [`program_from_argv0`], except that a shell interpreter
/// ([`SCRIPT_SHELLS`]) running a SCRIPT (`argv1` is not an option) whose
/// name is an agent's (`claude`, `codex` — `aterm_phase::program_of`) AND
/// that `is_shim` says is atpkg's own shim ([`atpkg_shim`] in production) is
/// that agent: atpkg's shim before its `exec` (module header, "THE SHIM
/// WINDOW"). Any other script keeps the interpreter's name (`sh build.sh` is
/// `sh`, and so is `sh ~/bin/claude`, a user's own wrapper), so nothing else
/// changes name. `is_shim` is asked only for an agent-named script.
pub(crate) fn program_from_argv(
    argv0: &str,
    argv1: Option<&str>,
    is_shim: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let name = program_from_argv0(argv0)?;
    if SCRIPT_SHELLS.contains(&name.as_str())
        && let Some(script) = argv1.filter(|a| !a.starts_with('-'))
        && let Some(agent) = program_from_argv0(script)
        && aterm_phase::program_of(&agent).is_some()
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

/// Whether `script` is atpkg's own shim: a file directly in the configured
/// managed prefix's `agents/` (the twin) or `bin/`
/// (`atpkg::store::resolve_configured`, the prefix every reader outside
/// atpkg's CLI resolves). Reads `aterm.toml` and stats the prefix, so it runs
/// only for an agent-named script, on the resolver thread.
pub(crate) fn atpkg_shim(script: &str) -> bool {
    atpkg::store::resolve_configured()
        .is_some_and(|layout| script_in(script, &[layout.agents_dir(), layout.bin_dir()]))
}

/// argv[0] and argv[1] of `pid`, as the process itself was exec'd with them,
/// or `None` (gone, another user's, or a platform without the call).
pub(crate) fn argv_head_of(pid: i32) -> Option<(String, Option<String>)> {
    if pid <= 0 {
        return None;
    }
    platform_argv_head(pid)
}

/// The published program of `pid`: [`program_from_argv`] of
/// [`argv_head_of`], atpkg's shims told by [`atpkg_shim`].
pub(crate) fn program_of(pid: i32) -> Option<String> {
    let (argv0, argv1) = argv_head_of(pid)?;
    program_from_argv(&argv0, argv1.as_deref(), &atpkg_shim)
}

#[cfg(target_os = "macos")]
fn platform_argv_head(pid: i32) -> Option<(String, Option<String>)> {
    // KERN_PROCARGS2 answers `int argc`, the exec path (NUL-terminated, then
    // NUL padding), then argv[0..argc], each NUL-terminated.
    // `KERN_PROCARGS2` (`<sys/sysctl.h>`), which the workspace's libc does
    // not export.
    const KERN_PROCARGS2: libc::c_int = 49;
    let mut mib = [libc::CTL_KERN, KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query (null buffer) on a fixed three-int MIB.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size < std::mem::size_of::<libc::c_int>() {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` holds `size` writable bytes and `size` says so; the kernel
    // writes at most that many and updates `size` to what it wrote.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    argv_head_from_procargs2(&buf)
}

#[cfg(target_os = "linux")]
fn platform_argv_head(pid: i32) -> Option<(String, Option<String>)> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
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

/// argv[0] and (when `argc > 1`) argv[1] out of a `KERN_PROCARGS2` buffer.
/// Pure, so the layout is testable without a process.
#[cfg(any(target_os = "macos", test))]
fn argv_head_from_procargs2(buf: &[u8]) -> Option<(String, Option<String>)> {
    let argv0 = argv0_from_procargs2(buf)?;
    let int = std::mem::size_of::<i32>();
    let argc = i32::from_ne_bytes(buf.get(..int)?.try_into().ok()?);
    let argv1 = (argc > 1)
        .then(|| {
            let rest = &buf[int..];
            let path_end = rest.iter().position(|b| *b == 0)?;
            let rest = &rest[path_end..];
            let start = rest.iter().position(|b| *b != 0)?;
            let rest = &rest[start..];
            let end0 = rest.iter().position(|b| *b == 0)?;
            let rest = rest.get(end0 + 1..)?;
            let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
            let arg = &rest[..end];
            (!arg.is_empty()).then(|| String::from_utf8_lossy(arg).into_owned())
        })
        .flatten();
    Some((argv0, argv1))
}

/// argv[0] out of a `KERN_PROCARGS2` buffer.
#[cfg(any(target_os = "macos", test))]
fn argv0_from_procargs2(buf: &[u8]) -> Option<String> {
    let int = std::mem::size_of::<i32>();
    let argc = i32::from_ne_bytes(buf.get(..int)?.try_into().ok()?);
    if argc < 1 {
        return None;
    }
    let rest = &buf[int..];
    // Skip the exec path, then the NUL padding after it.
    let path_end = rest.iter().position(|b| *b == 0)?;
    let rest = &rest[path_end..];
    let start = rest.iter().position(|b| *b != 0)?;
    let rest = &rest[start..];
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    let argv0 = &rest[..end];
    (!argv0.is_empty()).then(|| String::from_utf8_lossy(argv0).into_owned())
}

// ---------------------------------------------------------------- the leader's facts

/// What the shell's exported PATH does with `claude` and `codex`, read from a
/// child's environment at exec ([`path_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathVerdict {
    /// aterm's managed `agents/` comes before every other directory holding a
    /// `claude` or `codex`.
    Live,
    /// No `agents/` ahead of a foreign copy, but the session's `reroute/`
    /// stubs are, with `ATERM_CHILD` set: typed `claude`/`codex` still reach
    /// the managed twin — unless the shell hashed the foreign path before the
    /// stubs existed (a zsh `hash` keeps the old path until `rehash`).
    RerouteOnly,
    /// A foreign copy wins, or neither managed directory is on PATH.
    Frozen,
}

impl PathVerdict {
    /// The `path=` word: a reroute-only PATH reaches the managed twin, so it
    /// is `live`; `path_evidence=` says it was measured.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Live | Self::RerouteOnly => "live",
            Self::Frozen => "frozen",
        }
    }
}

/// THE PATH VERDICT, pure: walk `path_env` in order; aterm's `agents/`
/// (`is_agents`) reached before any directory that holds an agent
/// (`holds_agent`) is [`PathVerdict::Live`]; the session's `reroute/`
/// (`is_reroute`) reached first — and `aterm_child`, the variable its stubs
/// route on — is [`PathVerdict::RerouteOnly`]; anything else is
/// [`PathVerdict::Frozen`], including a PATH with neither managed directory
/// (then `claude` is a foreign copy or nothing).
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

/// `copy=` for an agent the leader runs: `managed` when its executable is in
/// the managed prefix's `store/` (the twin `exec`s the store binary by that
/// path — measured 2026-09-23: `…/store/claude/2026092201/bin/claude`) or it
/// is atpkg's own shim before its `exec` (`shim`); `foreign` for anything
/// else (the native installer's `~/.local/share/claude/versions/<v>`, a brew
/// cask). `None` for a program that is not an agent, or with no managed
/// prefix to compare against.
pub(crate) fn copy_of(
    program: Option<&str>,
    exec_path: &str,
    store: Option<&std::path::Path>,
    shim: bool,
) -> Option<&'static str> {
    aterm_phase::program_of(program?)?;
    let store = store?;
    Some(
        if shim || std::path::Path::new(exec_path).starts_with(store) {
            "managed"
        } else {
            "foreign"
        },
    )
}

/// Everything one read of the foreground leader tells: its published name,
/// the agent copy it runs, and — only when it is a direct child of the
/// session's shell whose environment was readable — the shell's PATH verdict.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeaderFacts {
    /// [`program_from_argv`].
    pub(crate) program: Option<String>,
    /// [`copy_of`].
    pub(crate) copy: Option<&'static str>,
    /// [`path_verdict`] of the leader's PATH at exec.
    pub(crate) path: Option<PathVerdict>,
}

/// [`LeaderFacts`] from one parsed `KERN_PROCARGS2` read (`args`), the
/// leader's parent (`ppid`) and the session's shell (`shell`): the PATH
/// verdict only when `ppid == shell` and the environment carried a PATH — a
/// grandchild's PATH may be anything its parent set, and a platform binary's
/// environment reads empty. `managed` is the prefix's layout, when there is one.
pub(crate) fn facts_from(
    args: &atpkg::caller_shell::ProcArgs,
    ppid: Option<i32>,
    shell: i32,
    managed: Option<&atpkg::store::Layout>,
    is_shim: &dyn Fn(&str) -> bool,
    holds_agent: &dyn Fn(&str) -> bool,
) -> LeaderFacts {
    let argv0 = args.argv.first().map_or("", String::as_str);
    let argv1 = args.argv.get(1).map(String::as_str);
    let program = program_from_argv(argv0, argv1, is_shim);
    let shim = program
        .as_deref()
        .is_some_and(|p| program_from_argv0(argv0).as_deref() != Some(p));
    let store = managed.map(|layout| layout.prefix.join("store"));
    let copy = copy_of(program.as_deref(), &args.exec_path, store.as_deref(), shim);
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

/// Whether `dir` holds a `claude` or a `codex` — what makes a directory ahead
/// of `agents/` on PATH decide which copy a typed name runs.
fn holds_agent(dir: &str) -> bool {
    let dir = std::path::Path::new(dir);
    ["claude", "codex"]
        .iter()
        .any(|name| dir.join(name).is_file())
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
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    if u32::try_from(pid).ok()? != info.pbi_pid {
        return None;
    }
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
    facts_from(
        &args,
        parent_of(pgid),
        shell,
        managed.as_ref(),
        &atpkg_shim,
        &holds_agent,
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
}

/// The leader read a job runs: `(pgid, shell)` to [`LeaderFacts`] — the
/// program's name, the agent copy it runs and the shell's PATH verdict, from
/// one process-table read ([`leader_facts`] in production).
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

/// The background thread program resolutions run on, started on first use.
/// A request whose group has left the foreground by the time it is answered
/// is dropped by [`SessionTimeline::set_program`]. There is at most one queued
/// token and one latest request per session, even if a lookup stalls while
/// status sweeps request retries. A newer foreground group replaces the slot
/// and runs after the in-flight lookup, without waiting for a retry deadline.
pub(crate) struct ProgramResolver {
    tx: Option<Sender<u64>>,
    worker: Option<JoinHandle<()>>,
    pending: Arc<Mutex<PendingJobs>>,
    lookup: Lookup,
    completion: Option<Completion>,
}

impl Default for ProgramResolver {
    fn default() -> Self {
        Self {
            tx: None,
            worker: None,
            pending: Arc::new(Mutex::new(PendingJobs::default())),
            lookup: Arc::new(leader_facts),
            completion: None,
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
    /// The status subsystem is off: queued work no longer has a consumer.
    /// An in-flight syscall can finish, but cancellation before publication
    /// prevents its result, footer read, and completion wake. A publication
    /// already committed before this call may still post its wake afterward;
    /// the cleared status observer ignores that retired session.
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
        match std::thread::Builder::new()
            .name("aterm-program-id".into())
            .spawn(move || {
                // A process lookup never holds the pending-map lock. The
                // short publication section takes pending, then the leaf
                // timeline lock, so cancellation cannot split its validity
                // check from the write.
                crate::qos::set_self(crate::qos::Role::Responsive);
                run_worker(rx, pending, lookup, completion);
            }) {
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

    /// A panic can discard both the receiver's queued ids and its in-flight
    /// job. The map retains every latest request, so the next request starts
    /// a worker and requeues each session once. `is_finished` avoids waiting
    /// for the worker on the UI thread.
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

    /// Resolve `pgid`'s leader off-thread into `timeline`. If the thread cannot
    /// be started the program stays unknown (`program=-`), never guessed. A
    /// replacement for an in-flight session is retained as its latest job,
    /// rather than sending a duplicate into an unbounded channel. `shell` is
    /// the session's shell pid: a leader that is its direct child also
    /// measures the shell's PATH ([`leader_facts`]).
    pub(crate) fn request(
        &mut self,
        session: u64,
        timeline: &Arc<Mutex<SessionTimeline>>,
        pgid: i32,
        shell: i32,
        proxy: Option<&winit::event_loop::EventLoopProxy<crate::Wake>>,
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

/// Process one id at a time, draining distinct queued sessions before a hot
/// session's updated in-flight lookup is requeued. The worker owns no sender:
/// dropping `ProgramResolver` closes the channel and lets it exit after its
/// bounded map drains.
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
        // The timeline and queue locks are released before a callback can
        // post more work to the GUI. The footer send below briefly re-takes
        // the queue lock to order it against retirement.
        if changed && let Some(wake) = &completion {
            wake(job.session);
        }
        if left_claude {
            crate::claude_footer::stop(job.session, job.pgid);
        }
        // The status sweep already refreshes a still-named Claude footer at
        // its own bounded cadence. Only the transition INTO Claude needs this
        // worker's immediate request.
        if became_claude {
            // Serialize this send with `retire`/`clear_pending`: if they win,
            // this request is stale and suppressed; if this wins, their stop
            // follows it on the footer channel. A request sent after a
            // retiring stop would otherwise keep a dead tab's watch alive.
            let pending = pending.lock().unwrap_or_else(|p| p.into_inner());
            if pending.current(&job, incarnation) {
                crate::claude_footer::request_footer(job.session, &job.timeline, job.pgid);
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
    // Lock order is pending -> timeline. The sole UI callers of `request`,
    // `retire`, and `clear_pending` release any timeline guard first, and
    // `set_program` only rings the harness's leaf bell. This short nested
    // section makes cancellation and a same-PGID re-enable linearize before
    // or after the name write, never between its check and write.
    let pending = pending.lock().unwrap_or_else(|p| p.into_inner());
    if !pending.current(job, incarnation) {
        return (false, false, false);
    }
    let mut timeline = job.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let was_claude = timeline.agent().program.as_deref() == Some("claude");
    let changed = timeline.set_program(job.pgid, facts.program);
    // The copy and the PATH verdict ride the same answer, under the same
    // currency check: a departed group's read never reaches the timeline.
    timeline.set_leader(job.pgid, facts.copy, facts.path);
    let became_claude = changed && timeline.agent().program.as_deref() == Some("claude");
    (changed, became_claude, changed && was_claude)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`Lookup`] that names the program only — no copy, no PATH verdict —
    /// for the queue tests, which are about WHICH answer publishes.
    fn names(f: impl Fn(i32) -> Option<String> + Send + Sync + 'static) -> Lookup {
        Arc::new(move |pgid, _shell| LeaderFacts {
            program: f(pgid),
            ..LeaderFacts::default()
        })
    }

    /// [`argv_head_of`] a child `spawn` just returned, once its exec has
    /// published the NEW image's argv — polled to a bounded deadline, never
    /// a fixed sleep.
    ///
    /// WHY: `spawn` returning is NOT "the new image's argv is readable". On
    /// Linux the parent is released from inside the child's `execve`: glibc's
    /// `posix_spawn` is a `CLONE_VM | CLONE_VFORK` clone, and `exec_mmap`
    /// completes the vfork (`exec_mm_release`) BEFORE it installs the new mm,
    /// while `mm->arg_start/arg_end` — what `/proc/<pid>/cmdline` reads — are
    /// set only later by the ELF loader (`create_elf_tables`). So right after
    /// `spawn` the child's cmdline reads first the PARENT's argv (the old,
    /// shared mm: this test binary's path), then EMPTY (`argv_head_of` =
    /// `None`), and only then the program's. Measured 2026-09-24 (aarch64,
    /// kernel 6.17): 159/200 reads straight after a Python
    /// `Popen(["sleep", "30"])` were empty; on origin/main `ae440363d` — no
    /// scheduling change at all — 29 of 30 runs of the two tests below had
    /// one or both fail on that `None`; and a first cut of this poll that
    /// stopped at the first `Some` read the test binary's own path in 3 runs
    /// of 100.
    ///
    /// "Not our own argv" is NOT enough either: a read inside that window
    /// can also be TORN — the kernel copies the cmdline in pieces while the
    /// exec rewrites it, so under load a read spliced the first bytes of
    /// this binary's argv0 onto the script path
    /// (`target/debug/deps/aterm_gui-b170agents/claude`), or returned our
    /// own argv0 cut short with no argv1; both differ from our argv head and
    /// both failed a test (2 of 96 concurrent runs). So the poll waits for
    /// the EXACT head the caller's exec produces (`expected`), and hands back
    /// the last read at the deadline so a genuine failure reports what was
    /// seen.
    #[cfg(unix)]
    fn settled_argv_head(
        pid: i32,
        expected: impl Fn(&str, Option<&str>) -> bool,
    ) -> Option<(String, Option<String>)> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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

    /// A test child that can neither outlive nor stall its test: stdio is
    /// null (a leftover process holding an inherited stdout open stalls any
    /// runner that pipes the test output), and it leads its own process
    /// group so [`kill_group`] reaches its descendants too (the shim's
    /// `sleep`).
    #[cfg(unix)]
    fn quiet_child(cmd: &mut std::process::Command) -> &mut std::process::Command {
        use std::os::unix::process::CommandExt;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
    }

    /// Kill a [`quiet_child`]'s whole group, then reap the child.
    #[cfg(unix)]
    fn kill_group(child: &mut std::process::Child) {
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: plain signal delivery to the group this test created
            // (`process_group(0)` makes the child's pid its pgid, and the
            // child is not yet reaped, so the pgid cannot have been reused).
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Spawn a script this test JUST wrote, retrying `ETXTBSY` to a bounded
    /// deadline. Another test thread that forks while `std::fs::write` still
    /// holds the script open for writing hands its child a copy of that fd,
    /// held until the child's own exec closes it — and an exec of a file open
    /// for writing anywhere is `ETXTBSY` (seen here 1 run in 100 — the
    /// well-known write-then-exec race of any multithreaded test harness).
    /// The window is only that other child's fork-to-exec, so a retry clears
    /// it.
    #[cfg(unix)]
    fn spawn_fresh_script(script: &std::path::Path) -> std::process::Child {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match quiet_child(&mut std::process::Command::new(script)).spawn() {
                Err(e)
                    if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                spawned => return spawned.expect("spawn the shim"),
            }
        }
    }

    /// Tier-1 for `ProgramResolverQueue`: the shipping slot transitions agree
    /// with the derived model, including a superseding foreground group and
    /// a worker restart. The `Buggy` model queues each duplicate and fails its
    /// one-token invariant in the spec gate.
    #[test]
    fn pending_job_transitions_keep_the_latest_group_and_one_token() {
        let model = aterm_spec::derive::program_resolver_queue_model();
        let mut state = model.init_state();
        let mut jobs = PendingJobs::default();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let job = |pgid| Job {
            session: 7,
            timeline: Arc::clone(&timeline),
            pgid,
            shell: 0,
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

    /// Cancelling an in-flight lookup preserves ownership of its channel
    /// token. Re-enabling the same session replaces its answer, and the old
    /// lookup cannot publish even when the new request uses the same PGID.
    #[test]
    fn cancelled_inflight_job_reenables_without_a_second_token() {
        let model = aterm_spec::derive::program_resolver_queue_model();
        let mut state = model.init_state();
        let mut jobs = PendingJobs::default();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let job = || Job {
            session: 7,
            timeline: Arc::clone(&timeline),
            pgid: 10,
            shell: 0,
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

    /// A blocked old-group lookup cannot create an unbounded queue of retries
    /// or delay the replacement behind them. Another session gets its turn
    /// before the hot session's newest request, and only applied answers post
    /// a completion wake.
    #[test]
    fn slow_lookup_coalesces_retries_and_serves_a_new_group_promptly() {
        use std::sync::mpsc::RecvTimeoutError;
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
            .recv_timeout(Duration::from_secs(2))
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

        assert_eq!(wake_rx.recv_timeout(Duration::from_secs(2)), Ok(2));
        assert_eq!(wake_rx.recv_timeout(Duration::from_secs(2)), Ok(1));
        assert_eq!(
            first.lock().unwrap().agent().program.as_deref(),
            Some("bash")
        );
        assert_eq!(
            second.lock().unwrap().agent().program.as_deref(),
            Some("zsh")
        );
        assert_eq!(*calls.lock().unwrap(), vec![10, 30, 20]);
        assert_eq!(
            wake_rx.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout)
        );
        let drained_by = std::time::Instant::now() + Duration::from_secs(2);
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
        use std::time::Duration;

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
            .recv_timeout(Duration::from_secs(2))
            .expect("first lookup started");
        for _ in 0..100 {
            resolver.request(1, &timeline, 10, 0, None);
        }
        assert_eq!(resolver.pending.lock().unwrap().by_session.len(), 1);
        release_tx.send(()).unwrap();

        assert_eq!(
            wake_rx.recv_timeout(Duration::from_secs(2)),
            Ok(Some("sh".into()))
        );
        assert_eq!(
            wake_rx.recv_timeout(Duration::from_secs(2)),
            Ok(Some("bash".into()))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn disabling_status_discards_a_late_inflight_answer() {
        use std::time::Duration;

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
            .recv_timeout(Duration::from_secs(2))
            .expect("old lookup started");
        resolver.clear_pending();

        // A new request is a completion barrier: it can run only after the
        // old lookup has returned and been discarded.
        let current = Arc::new(Mutex::new(SessionTimeline::default()));
        current.lock().unwrap().note_foreground_group(20);
        resolver.request(2, &current, 20, 0, None);
        release_tx.send(()).unwrap();
        assert_eq!(wake_rx.recv_timeout(Duration::from_secs(2)), Ok(2));
        assert_eq!(old.lock().unwrap().agent().program.as_deref(), None);
        assert_eq!(
            current.lock().unwrap().agent().program.as_deref(),
            Some("bash")
        );
    }

    #[test]
    fn reenabling_the_same_group_drops_the_pre_clear_answer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

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
            .recv_timeout(Duration::from_secs(2))
            .expect("old request entered lookup");
        resolver.clear_pending();
        resolver.request(1, &timeline, 10, 0, None);
        assert_eq!(resolver.pending.lock().unwrap().by_session.len(), 1);
        release_tx.send(()).unwrap();

        assert_eq!(wake_rx.recv_timeout(Duration::from_secs(2)), Ok(1));
        assert_eq!(
            timeline.lock().unwrap().agent().program.as_deref(),
            Some("new")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(wake_rx.recv_timeout(Duration::from_millis(50)).is_err());
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
            .recv_timeout(Duration::from_secs(2))
            .expect("worker started");
        let deadline = Instant::now() + Duration::from_secs(2);
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
        assert_eq!(wake_rx.recv_timeout(Duration::from_secs(2)), Ok(1));
        assert_eq!(
            timeline.lock().unwrap().agent().program.as_deref(),
            Some("bash")
        );
        let drained_by = Instant::now() + Duration::from_secs(2);
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

    #[test]
    fn procargs2_layout_yields_argv0_not_the_exec_path() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/Users//x/.local/share/claude/versions/2.1.280\0\0\0\0");
        buf.extend_from_slice(b"claude\0--resume\0PATH=/bin\0");
        assert_eq!(argv0_from_procargs2(&buf).as_deref(), Some("claude"));
        assert_eq!(argv0_from_procargs2(&0i32.to_ne_bytes()), None, "argc 0");
        assert_eq!(argv0_from_procargs2(b"\x01"), None, "short buffer");
    }

    /// A real child: its argv[0] is read from the process table, and a
    /// renamed argv[0] (`exec -a claude`) is what is published — the shape an
    /// agent launched through a shim or a version directory takes.
    #[cfg(unix)]
    #[test]
    fn a_live_process_is_named_by_its_argv0() {
        let mut child = quiet_child(std::process::Command::new("sleep").arg("30"))
            .spawn()
            .expect("spawn sleep");
        let pid = i32::try_from(child.id()).expect("pid");
        // Wait for the exec to land ([`settled_argv_head`]), then name the
        // process through the real `program_of`: a settled image's argv no
        // longer moves.
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
        if !std::path::Path::new("/bin/bash").exists() {
            return;
        }
        let mut child = quiet_child(
            std::process::Command::new("/bin/bash").args(["-c", "exec -a claude sleep 30"]),
        )
        .spawn()
        .expect("spawn sh");
        let pid = i32::try_from(child.id()).expect("pid");
        // `exec -a` replaces the shell in place; wait for the new image —
        // the same exec window, so the same bounded, exact-match poll.
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

    /// The shim window, pure: an interpreter running atpkg's shim NAMED after
    /// an agent is the agent; the measured store path names its tool.
    /// NEGATIVE CONTROLS: any other script, an option, a login shell, a
    /// non-shell with an agent-named argument, and an agent-named script that
    /// is NOT atpkg's (a user's own `~/bin/claude` wrapper) keep their own
    /// names.
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

    #[test]
    fn procargs2_layout_yields_argv1_after_argv0() {
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

    /// A real shim in its window: `/bin/sh <dir>/agents/claude` that has not
    /// exec'd yet reads `claude` when `<dir>/agents` is the managed one.
    /// NEGATIVE CONTROLS: the same script under another name reads `sh`, and
    /// so does the agent-named one when its directory is not managed.
    #[cfg(unix)]
    #[test]
    fn a_live_shim_before_its_exec_is_named_by_its_script() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aterm-shim-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("agents")).expect("scratch dir");
        let mut named = Vec::new();
        for name in ["agents/claude", "build.sh"] {
            let script = dir.join(name);
            std::fs::write(&script, "#!/bin/sh\nsleep 30\nexit 0\n").expect("write the shim");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            let mut child = spawn_fresh_script(&script);
            let pid = i32::try_from(child.id()).expect("pid");
            // The kernel runs `#!` scripts as `/bin/sh <script>` from the
            // first instruction — once its exec has laid out argv, which on
            // Linux is AFTER `spawn` returns ([`settled_argv_head`]).
            let path = script.to_string_lossy().into_owned();
            let head = settled_argv_head(pid, |a0, a1| {
                a0.rsplit('/').next() == Some("sh") && a1 == Some(path.as_str())
            });
            kill_group(&mut child);
            let (argv0, argv1) = head.expect("the shim's argv");
            assert_eq!(
                argv1.as_deref(),
                Some(path.as_str()),
                "settled on the script"
            );
            let managed = [dir.join("agents")];
            let unmanaged = [dir.join("elsewhere")];
            named.push((
                program_from_argv(&argv0, argv1.as_deref(), &|s| script_in(s, &managed)),
                program_from_argv(&argv0, argv1.as_deref(), &|s| script_in(s, &unmanaged)),
            ));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(named[0].0.as_deref(), Some("claude"));
        assert_eq!(named[0].1.as_deref(), Some("sh"), "not the managed agents/");
        assert_eq!(named[1].0.as_deref(), Some("sh"));
    }

    /// A `KERN_PROCARGS2` buffer as the kernel lays one out: argc, the exec
    /// path, NUL padding, argv, then the environment.
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
    /// `pid 4205` as measured 2026-09-24: the FOREIGN native claude, started by
    /// its tab's shell (1916) with the managed `agents/` first on PATH — the
    /// relaunch line had sourced the hook.
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

    /// THE PATH VERDICT from synthetic `KERN_PROCARGS2` buffers: agents/ first
    /// is live, the reroute stubs first (with `ATERM_CHILD`) are reroute-only,
    /// a foreign copy first is frozen — and the PATH of a leader that is NOT
    /// the shell's direct child is never read as the shell's (NEGATIVE
    /// CONTROL: the same live buffer under another parent measures nothing).
    #[cfg(target_os = "macos")]
    #[test]
    fn the_shells_path_is_measured_only_from_its_direct_child() {
        let agents = format!("{PREFIX}/agents");
        let reroute = format!("{PREFIX}/reroute");
        let no_shim = |_: &str| false;
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

    /// `copy=`: the store's executable is managed, so is atpkg's own shim
    /// before its `exec`; anything else an agent runs is foreign; a program
    /// that is no agent has no copy.
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

    /// A live child of THIS process (the resolver's own read, not a built
    /// buffer): a non-platform binary with a PATH set is measured when this
    /// process is named its shell, and not when another pid is.
    #[cfg(unix)]
    #[test]
    fn a_live_child_is_measured_against_its_real_parent() {
        let me = std::env::current_exe().expect("exe");
        let mut child = std::process::Command::new(&me)
            .arg("--exact")
            .arg("session_status::program::tests::park_for_a_read")
            .env("ATERM_PROGRAM_TEST_PARK", "1")
            .env("PATH", "/nonexistent/agents:/usr/bin")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the test binary");
        let pid = i32::try_from(child.id()).expect("pid");
        let parent = i32::try_from(std::process::id()).expect("pid");
        let mut read = None;
        for _ in 0..200 {
            read = u32::try_from(pid)
                .ok()
                .and_then(atpkg::caller_shell::process_args)
                .filter(|a| a.env_var("ATERM_PROGRAM_TEST_PARK").is_some());
            if read.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let ppid = parent_of(pid);
        let _ = child.kill();
        let _ = child.wait();
        let args = read.expect("a non-platform child's environment is readable");
        assert_eq!(ppid, Some(parent));
        let fake = atpkg::store::Layout {
            prefix: std::path::PathBuf::from("/nonexistent"),
        };
        let facts =
            |shell: i32| facts_from(&args, ppid, shell, Some(&fake), &|_| false, &|_| false);
        assert_eq!(facts(parent).path, Some(PathVerdict::Live));
        assert_eq!(facts(parent + 1).path, None, "not this shell's child");
    }

    /// The stand-in child's body: parked briefly when asked, so its
    /// environment can be read.
    #[test]
    fn park_for_a_read() {
        if std::env::var_os("ATERM_PROGRAM_TEST_PARK").is_some() {
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
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
