// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE I/O HALF OF [`super::upgrade`]: Claude Code sessions advanced, one step
//! at a time, through the cooperative live upgrade — and Codex sessions by the
//! same step's CODEX BRANCH (`upgrade_codex_drive.rs`, over
//! [`super::upgrade_codex`]): the shared daemon first, by the vendor's verb,
//! then the tab's TUI by a typed `/exit` and `codex resume`, never a signal.
//! A tab whose foreground job is a Codex is that branch's alone.
//!
//! Two callers, the same steps. THE WINDOW'S HOST drives it per session: its
//! worker asks [`due`] when the session is attached and whenever atpkg's
//! activation notice says a newer build was installed
//! ([`super::upgrade_wake`] — a push; there is no timer), asks its
//! supervisor's loop for the session's next idle point, and takes ONE
//! [`step`] there, in the loop — announce, or, once the agent answered
//! READY, end it and relaunch it
//! ([`super::relaunch`]), handing the new process straight back to its
//! supervisor ([`Opts::hand_back`]), whose next idle point types the
//! continuation ([`super::relaunch::resume`]). A PERSON or a script runs
//! [`sweep`] (`aterm harness upgrade`): one step for every session at once,
//! the continuation typed in the same step.
//!
//! Every fact comes from a source that cannot be stale for the process it
//! describes: Claude's own session file (checked against the KERNEL start time,
//! so a recycled pid never reads as the session), `KERN_PROCARGS2` for argv
//! and any readable tab id (`ATERM_PARENT_SESSION_ID`), the host instance's
//! owner-only PTY foreground-group roster for a tab id when macOS omits the
//! environment, the process table for what runs under the agent, whose terminal
//! it is on and whether it is its shell's job, and the tab's own screen and
//! hold over the control socket — a person's recent keystroke (`human_ms=`)
//! included. Whatever cannot be read is a WAIT, never a guess; nothing here
//! types or signals unless [`super::upgrade::next_step`] says go, and every
//! step taken is one ledger line.
//!
//! State is one small JSON file per conversation under
//! `<harness state>/upgrade/`, so the next step — the host's at the next idle
//! point, or a hand-run sweep's — resumes exactly where the last one stopped,
//! including across the agent's own exit while that restart is fresh and its
//! shell lives ([`super::relaunch::expired`]). The same files are THE OWNER'S
//! VIEW and carry THE OWNER'S WORD (`upgrade_status`: [`rows`], [`View`],
//! [`ask`]): how long each session has been behind and what its last step
//! waited on, and `aterm harness upgrade <sid> --now|--defer|--skip`.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_json::{Map, Value};
use aterm_types::control_verbs::ScreenGen;

use super::relaunch::{
    Launch, NoPlan, Plan, PromptMark, Relaunch, RelaunchLineError, STALE_S, await_new, carry_on,
    confirm, expired, last_permission_mode, plan, relaunch, relaunch_of, type_relaunch_line_with,
    unplanned, wait_until,
};
use super::upgrade::{self, Candidate, Facts, Phase, Request, SessionFile, Source, Step, Version};
use super::upgrade_catalog::{self as catalog, Baked};
use super::upgrade_models::{self as models, ModelRecord, ModelVerdict, Priority};
use crate::RelayClient;
use crate::supervise::screen::HumanInput;

#[path = "upgrade_status.rs"]
mod upgrade_status;

#[path = "upgrade_codex_drive.rs"]
mod codex;
pub(super) use upgrade_status::runs_under;
pub use upgrade_status::{
    ATTENTION_OWNER, Ask, Remedy, Row, STALLED_AFTER_S, View, ask, ask_for, rows, status_rows,
    word_marker,
};

/// What one sweep is told.
#[derive(Clone, Debug)]
pub struct Opts {
    /// The user's home (Claude's `~/.claude`, the native install, the hooks).
    pub home: PathBuf,
    /// The harness state directory; this module writes under `upgrade/` in it.
    pub state: PathBuf,
    /// A control socket to use instead of the one each tab's id resolves to.
    pub sock: Option<String>,
    /// Only this tab (`s-<hex>`), when set.
    pub only_sid: Option<String>,
    /// Plan and print; type, signal and write nothing.
    pub dry_run: bool,
    /// `[harness] human_grace_s`: a tab a PERSON typed into this recently
    /// (its `status` `human_ms=`) is held — nothing is typed into it and its
    /// agent is not signalled.
    pub human_grace_s: u32,
    /// A SUPERVISOR LOOP TAKES THE RELAUNCHED AGENT (the window's host): a
    /// relaunch ends as soon as the new process holds the conversation
    /// (`adopted`), and the continuation is typed at the loop's next idle
    /// point ([`super::relaunch::resume`]) — after the loop has answered
    /// whatever the new process opened with (a trust dialog, a resume
    /// choice, a new build's model consent), which a carry-on waiting for
    /// idle with no loop running never gets past. `false` (the hand-run verb,
    /// which has no loop) types it in the same step.
    pub hand_back: bool,
    /// THE STEP IS TAKEN AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK, not at
    /// an idle point (the window's host,
    /// [`crate::supervise::IdleHost::at_background`]): only the FIRST notice
    /// may be typed here
    /// ([`upgrade::Facts::background_point`]) — nothing is ended, no restart
    /// left in flight is carried on, and no Codex daemon is moved.
    pub background: bool,
}

/// What a sweep did with one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The agent's pid.
    pub pid: u32,
    /// The aterm tab (`s-<hex>`), or `-`.
    pub tab: String,
    /// The conversation.
    pub session: String,
    /// The running version.
    pub from: String,
    /// The target (`<version>(<source>)`), or `-`.
    pub to: String,
    /// The step taken or the reason for waiting.
    pub step: String,
}

impl Report {
    /// Whether this report is something the sweep DID or put on the record
    /// (typed, signalled, relaunched, gave up, refused, held back), as opposed
    /// to a wait, a skip or `current`.
    #[must_use]
    pub fn is_act(&self) -> bool {
        !(self.step == "current"
            || self.step.starts_with("wait:")
            || self.step.starts_with("skip:")
            || self.step.starts_with("busy:")
            || self.step.starts_with("would-"))
    }

    /// One line: `upgrade pid=… tab=… session=… from=… to=… step=…`.
    #[must_use]
    pub fn line(&self) -> String {
        self.line_as("upgrade")
    }

    /// The same line under another name — `relaunch` for a relaunch on
    /// exit, `carry-on` for a relaunched agent's continuation — so a log
    /// never names a relaunch an upgrade (the live E2E of 2026-09-25 read
    /// `relaunch: upgrade pid=…` for a SIGKILL's relaunch).
    #[must_use]
    pub fn line_as(&self, kind: &str) -> String {
        format!(
            "{kind} pid={} tab={} session={} from={} to={} step={}",
            self.pid, self.tab, self.session, self.from, self.to, self.step
        )
    }
}

pub(super) fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------- processes

/// `ps` in the C locale and UTC, the rendering Claude's `procStart` uses.
fn ps(args: &[&str]) -> Option<String> {
    let out = Command::new("ps")
        .args(args)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(super) fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The kernel's start time of `pid`, rendered as Claude renders `procStart`.
pub(super) fn kernel_start(pid: u32) -> Option<String> {
    let s = squash(&ps(&["-o", "lstart=", "-p", &pid.to_string()])?);
    (!s.is_empty()).then_some(s)
}

pub(super) fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(p) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 checks existence and permission; nothing is delivered.
        let rc = unsafe { libc::kill(p, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    // Windows: the workspace's one `OpenProcess` probe, the same contract
    // (a pid that exists but is not ours reads alive, like `EPERM`).
    #[cfg(not(unix))]
    {
        aterm_uds::process::pid_alive(pid)
    }
}

/// `(ppid, pgid, tpgid)` of `pid`.
pub(super) fn ids(pid: u32) -> Option<(u32, i64, i64)> {
    let out = ps(&["-o", "ppid=,pgid=,tpgid=", "-p", &pid.to_string()])?;
    let mut it = out.split_whitespace();
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

/// A cheap kernel group lookup before any process-table sweep. The focused
/// `ids` read later checks the controlling terminal's foreground group too.
fn process_group(pid: u32) -> Option<i64> {
    #[cfg(unix)]
    {
        let pid = libc::pid_t::try_from(pid).ok()?;
        // SAFETY: getpgid reads one process's kernel identity without changing it.
        let group = unsafe { libc::getpgid(pid) };
        (group > 0).then_some(i64::from(group))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// The whole table: `(pid, ppid, command basename)`.
pub(super) fn table() -> Vec<(u32, u32, String)> {
    let Some(out) = ps(&["-A", "-o", "pid=,ppid=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some((pid, ppid, base_name(&it.collect::<Vec<_>>().join(" "))))
        })
        .collect()
}

/// A `comm` column's basename, a login shell's leading `-` dropped.
fn base_name(comm: &str) -> String {
    comm.rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim_start_matches('-')
        .to_string()
}

/// The whole table with each process's controlling terminal:
/// `(pid, ppid, tty, command basename)`, the tty `??` (macOS) or `?` (Linux)
/// for none.
fn tty_table() -> Vec<(u32, u32, String, String)> {
    let Some(out) = ps(&["-A", "-o", "pid=,ppid=,tty=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let tty = it.next()?.to_string();
            Some((pid, ppid, tty, base_name(&it.collect::<Vec<_>>().join(" "))))
        })
        .collect()
}

/// THE PROCESS THAT OWNS `pid`'s TERMINAL: walking up from `pid`, the first
/// ancestor NOT on `pid`'s tty — the program that opened that pty and holds its
/// master. `(pid, basename)`, or `None` when `pid` has no terminal or the walk
/// breaks. Every process between `pid` and it (a shell, a wrapper) is on the
/// same terminal, so this is the one fact that tells an aterm tab from a pane:
/// a multiplexer (tmux, screen, zellij, dtach) opens a pty of its own for every
/// pane, and the pane's shell is that pty's child, not the tab's.
fn terminal_owner(pid: u32, t: &[(u32, u32, String, String)]) -> Option<(u32, &str)> {
    let row = |p: u32| t.iter().find(|(q, _, _, _)| *q == p);
    let tty = row(pid)?.2.as_str();
    if !tty.chars().any(|c| c.is_ascii_digit()) {
        return None; // `??`, `?`, `-`: no controlling terminal at all.
    }
    let mut p = pid;
    for _ in 0..64 {
        let (_, pp, _, _) = row(p)?;
        let (q, _, qtty, name) = row(*pp)?;
        if qtty != tty {
            return Some((*q, name.as_str()));
        }
        p = *q;
    }
    None
}

/// Whether the terminal `owner` ([`terminal_owner`]) opened is an ATERM TAB'S:
/// the owner is an aterm process (`aterm`, or an argv0 alias `aterm-<tool>`),
/// or the kernel's pid 1 — the aterm that opened the pty handed its tabs to a
/// successor (a live update) and exited, so the shell was reparented (measured
/// 2026-09-23: every tab shell on the owner's machine has ppid 1, the agent
/// under it on the same tty). A multiplexer holding a pane's pty cannot have
/// exited while the pane lives, so pid 1 never stands for one. `script`, `ssh`,
/// `sudo`'s own pty and every multiplexer read as NOT the tab's: what is typed
/// into the tab reaches whatever that program shows, not the agent.
pub(super) fn owned_by_aterm(owner: (u32, &str)) -> bool {
    owner.0 == 1 || owner.1 == "aterm" || owner.1.starts_with("aterm-")
}

/// The owner's name as one report word: `[A-Za-z0-9._-]`, at most 32 bytes.
fn owner_word(owner: Option<(u32, &str)>) -> String {
    let word: String = owner
        .map(|(_, name)| name)
        .unwrap_or("none")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect();
    if word.is_empty() {
        "?".to_string()
    } else {
        word
    }
}

/// Where the agent stands among its terminal's jobs, from its own `(pgid,
/// tpgid)` and its parent's `pgid`. A job-control shell starts every job in a
/// process group of its own (`pgid == pid` for the agent it ran) and hands it
/// the terminal (`tpgid`); a script, `sh -c`, `make`, an IDE task or a shell
/// with job control off runs its child IN ITS OWN group instead. That
/// difference is the whole of what the restart rests on: it waits for the
/// parent's `pgid == tpgid` as "the shell took the terminal back", and for a
/// parent sharing the agent's group that already holds while the agent runs —
/// the relaunch line would be typed into whatever the launcher runs next, or,
/// the launcher gone with the agent, never (measured 2026-09-23 on a pty: a
/// bash launcher's `pgid == tpgid` with its child running, both in one group;
/// an interactive zsh's job in a group of its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Job {
    /// A job of a job-control shell, and the terminal's foreground now.
    Foreground,
    /// A job, but not the terminal's foreground right now (suspended, or put in
    /// the background): what is typed would reach the shell, not the agent.
    Background,
    /// Not a job of a job-control shell: nothing will take the terminal back
    /// when it ends. Refused for good, before anything is typed.
    NoJobControl,
}

/// [`Job`] from the numbers `ps` reports.
fn job(agent: u32, agent_pgid: i64, tpgid: i64, parent_pgid: i64) -> Job {
    if agent_pgid != i64::from(agent) || parent_pgid == agent_pgid {
        Job::NoJobControl
    } else if tpgid != agent_pgid {
        Job::Background
    } else {
        Job::Foreground
    }
}

/// [`job`] read from the kernel now, with the parent it was read against.
fn job_of(agent: u32) -> Option<(Job, u32)> {
    let (parent, pgid, tpgid) = ids(agent)?;
    let (_, parent_pgid, _) = ids(parent)?;
    Some((job(agent, pgid, tpgid, parent_pgid), parent))
}

/// What one [`Job`] read lets through: `Ok(shell)` when the agent is its
/// shell's foreground job — `shell` the parent the read was taken against —
/// else the one word the step waits with. Every look before an announce or a
/// SIGTERM asks this, so a ctrl-z between two looks is caught by the second.
pub(super) fn foreground_shell(job: Option<(Job, u32)>) -> Result<u32, &'static str> {
    match job {
        Some((Job::Foreground, shell)) => Ok(shell),
        Some((Job::Background, _)) => Err("not-foreground"),
        Some((Job::NoJobControl, _)) => Err("not-a-shell-job"),
        None => Err("ids"),
    }
}

/// THE PROCESS FACTS every act rests on, each read from the kernel at the
/// moment it is asked. The job is asked more than once ON PURPOSE: the agent can
/// be suspended or backgrounded between the first look and the act, so the
/// announce, the SIGTERM and the last look before it each read it again. A
/// trait so a test can script what the Nth read answers — no real process can
/// be made to change on cue between two reads — and prove that each re-read is
/// what stands between such a change and the act.
pub(super) trait Kernel {
    /// Where the agent stands among its terminal's jobs, and its parent
    /// ([`job_of`]).
    fn job(&self, agent: u32) -> Option<(Job, u32)>;
    /// `pid`'s parent, `None` when it cannot be read NOW: the process is gone,
    /// or `ps` could not be run. Never a verdict about the process.
    fn parent(&self, pid: u32) -> Option<u32>;
    /// The program that opened `pid`'s terminal ([`terminal_owner`]).
    fn terminal(&self, pid: u32) -> Option<(u32, String)>;
}

/// The kernel itself: `ps` and `kill(pid, 0)`.
pub(super) struct Live;

impl Kernel for Live {
    fn job(&self, agent: u32) -> Option<(Job, u32)> {
        job_of(agent)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        if !alive(pid) {
            return None;
        }
        ids(pid).map(|(ppid, _, _)| ppid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        terminal_owner(pid, &tty_table()).map(|(owner, name)| (owner, name.to_string()))
    }
}

/// Shells and the keep-awake helper: WORK running under the agent (a Bash tool, a
/// background shell, a build, a `caffeinate` a command started). MCP servers and
/// other helpers are not: a resume restarts them. Nor is the agent's own
/// keep-awake ([`OWN_KEEP_AWAKE`]).
const BACKGROUND: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "tcsh",
    "caffeinate",
];

/// THE AGENT'S OWN KEEP-AWAKE IS NOT WORK: Claude Code starts `caffeinate -i -t
/// 300` as its OWN child at every turn ("Started caffeinate to prevent sleep" /
/// "Stopped sleep inhibitor, allowing sleep", its 2.1.283 binary) and stops it
/// some 30 s after the turn ends. It runs nothing and holds nothing a restart
/// could lose — the relaunched agent starts its own — and `-t` ends it by itself
/// if the agent does not. Counted as work it held every restart of the
/// 2026-09-26 end-to-end run 82-85 s past the READY answer, and 300 s more
/// whenever it outlasted the second look. Only as the agent's DIRECT child: a
/// `caffeinate` a Bash tool started runs under the tool's shell, and one under
/// any other helper is not the agent's own — both stay work.
const OWN_KEEP_AWAKE: &str = "caffeinate";

/// The background work under `pid`, by basename: every [`BACKGROUND`] program
/// at any depth below it, except the agent's own keep-awake
/// ([`OWN_KEEP_AWAKE`], `pid`'s direct child), whose descendants are walked
/// all the same. The one reading of "something runs under the agent" every
/// restart asks — the live upgrade's gate and its last look, the model-only
/// restart's, and a Codex embedded session's.
pub(super) fn background(pid: u32, t: &[(u32, u32, String)]) -> Vec<String> {
    let mut frontier = vec![pid];
    let mut found = Vec::new();
    let mut seen = 0;
    while let Some(p) = frontier.pop() {
        seen += 1;
        if seen > 4096 {
            break;
        }
        for (c, pp, name) in t {
            if *pp == p {
                frontier.push(*c);
                let own_keep_awake = p == pid && name == OWN_KEEP_AWAKE;
                if BACKGROUND.contains(&name.as_str()) && !own_keep_awake {
                    found.push(name.clone());
                }
            }
        }
    }
    found
}

/// Whether `pid` is this process or one of its ancestors: ending it would end
/// the sweep that is driving it.
pub(super) fn is_our_ancestor(pid: u32, t: &[(u32, u32, String)]) -> bool {
    let mut p = std::process::id();
    for _ in 0..64 {
        if p == pid {
            return true;
        }
        match t.iter().find(|(c, _, _)| *c == p) {
            Some((_, pp, _)) if *pp != p && *pp != 0 => p = *pp,
            _ => return false,
        }
    }
    false
}

/// The kernel's working directory of `pid`.
pub(super) fn cwd_of(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        return std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix('n').map(str::to_owned))
    }
}

/// The executable the kernel mapped for `pid` (its text), canonical.
pub(super) fn exe_of(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        return std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-d", "txt", "-Fn"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix('n').map(PathBuf::from))
    }
}

/// How long `<exe> --version` may take before it is killed and read as no
/// version. A healthy Claude build answers in well under a second; the bound
/// is for one that never does — a quarantined download held at a first-run
/// dialog, a wrapper waiting on a lock — which would otherwise hold
/// `sweep.lock`, and with it every Claude upgrade on the machine, for as long
/// as it hangs. Insurance, not a measured Claude fault: the hangs measured on
/// the owner's machine (2026-09-24, over two minutes) were hand-run probes of
/// a quarantined Homebrew codex cask, which this sweep never execs.
const VERSION_WAIT: Duration = Duration::from_secs(5);

/// What one `<exe> --version` probe found ([`version_within`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Probed {
    /// It answered, naming this version.
    Version(Version),
    /// It ran to its end within the bound and named no version (it failed,
    /// could not be run, or printed something else): a property of the FILE,
    /// remembered while the file is unchanged ([`cached_version`]).
    NoVersion,
    /// It did not finish within the bound — the child still ran, or something
    /// it started still held its output — and its process group was killed:
    /// as much a property of the MOMENT (a loaded machine, a first-run
    /// dialog) as of the file, so it is asked again on a back-off
    /// ([`VERSION_RETRY_FIRST`]), never written off for good.
    TimedOut,
}

/// What `<exe> --version` says ([`Probed`]): its leading version, no version
/// (a child that fails or prints none), or no answer within [`VERSION_WAIT`].
fn version_of(exe: &Path) -> Probed {
    version_within(exe, VERSION_WAIT)
}

/// [`version_of`] with its bound given, and the WHOLE probe inside it: the
/// child's exit AND the read of its output. The child runs in a process group
/// of its own; its output is read on a thread, so a chatty child cannot fill
/// the pipe and stall. The read ends only when the LAST holder of the pipe's
/// write end closes it, and that need not be the child: a grandchild it left
/// running (`sleep 12 & echo 2.1.282; exit 0` — measured by the 2026-09-24
/// review at 12.22 s against a 300 ms bound, because the read was joined with
/// no deadline) keeps it open after the child has exited. So the read is
/// waited for against the SAME deadline, and at the deadline the group is
/// killed either way and the probe answers [`Probed::TimedOut`] without
/// waiting for the reader — which ends as soon as the group's last writer
/// dies, or, for a writer that left the group (`setsid`), whenever that one
/// exits: the sweep does not wait on it.
fn version_within(exe: &Path, limit: Duration) -> Probed {
    let deadline = Instant::now() + limit;
    let mut cmd = Command::new(exe);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let Ok(mut child) = cmd.spawn() else {
        return Probed::NoVersion;
    };
    // Unix kills the probe's whole group at the deadline; Windows has no group kill
    // here, so only the child itself is killed (a grandchild holding the pipe is
    // abandoned with the reader thread, which the timeout already does not wait on).
    #[cfg(unix)]
    let group = libc::pid_t::try_from(child.id()).ok();
    let kill_group = || {
        #[cfg(unix)]
        if let Some(group) = group {
            // SAFETY: a signal to the process group this spawn made
            // (`process_group(0)`: its pgid is the child's own pid). After the
            // child is reaped it is sent only while something still holds the
            // pipe — a member of that group, whose id is not reused while it
            // lives (a holder that left the group with `setsid` is the one it
            // does not reach).
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        kill_group();
        let _ = child.kill();
        let _ = child.wait();
        return Probed::NoVersion;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let _reader = std::thread::Builder::new()
        .name("agent-version-probe".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    let succeeded = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return Probed::TimedOut;
            }
        }
    };
    let Ok(out) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
        // The child is gone and something of its group still holds the pipe.
        kill_group();
        return Probed::TimedOut;
    };
    if !succeeded {
        return Probed::NoVersion;
    }
    let text = String::from_utf8_lossy(&out);
    text.split_whitespace()
        .next()
        .and_then(Version::parse)
        .map_or(Probed::NoVersion, Probed::Version)
}

// ---------------------------------------------------------------- targets

/// Where Claude's native installer keeps its builds.
pub(super) fn native_root(home: &Path) -> PathBuf {
    home.join(".local/share/claude/versions")
}

/// The builds a session could move to, read ONCE per sweep.
pub(super) struct Targets {
    /// aterm's managed twin (`<prefix>/agents/claude`).
    managed: Option<Candidate>,
    /// The native install's current build (what `~/.local/bin/claude` names).
    native: Option<Candidate>,
}

impl Targets {
    pub(super) fn read(home: &Path) -> Targets {
        let managed = atpkg::store::resolve_configured().and_then(|layout| {
            let twin = layout.agents_dir().join("claude");
            let version = cached_version(&twin)?;
            Some(Candidate {
                exe: twin,
                version,
                source: Source::Managed,
            })
        });
        let native = std::fs::canonicalize(home.join(".local/bin/claude"))
            .ok()
            .filter(|target| target.starts_with(native_root(home)))
            .and_then(|target| {
                // The native installer names each build's file by its version.
                let version = target
                    .file_name()
                    .and_then(|n| Version::parse(&n.to_string_lossy()))
                    .or_else(|| cached_version(&target))?;
                Some(Candidate {
                    exe: target,
                    version,
                    source: Source::Native,
                })
            });
        Targets { managed, native }
    }

    /// What THIS session could move to: the managed twin always, the native
    /// build only for a session that runs native (a managed session is never
    /// moved onto a vendor-updated copy).
    pub(super) fn for_session(&self, running_native: bool) -> Vec<Candidate> {
        self.managed
            .iter()
            .chain(self.native.iter().filter(|_| running_native))
            .cloned()
            .collect()
    }
}

/// How long after a probe that TIMED OUT ([`Probed::TimedOut`]) the same file
/// is probed again, doubled after each timeout in a row up to
/// [`VERSION_RETRY_MAX`]. A timeout is not a verdict on the build — one slow
/// probe on a loaded machine used to mean that build was never probed again
/// while the process lived, so never upgraded to (the 2026-09-24 review) —
/// but neither is it re-run every minute while it holds `sweep.lock` for
/// [`VERSION_WAIT`] each time.
const VERSION_RETRY_FIRST: Duration = Duration::from_secs(2 * 60);
const VERSION_RETRY_MAX: Duration = Duration::from_secs(60 * 60);

/// What [`cached_version`] remembers of one file as it was (size, mtime).
#[derive(Clone, Debug)]
enum Seen {
    /// The probe finished: its answer, until the file changes.
    Answered(Option<Version>),
    /// It timed out `streak` times in a row; not asked again before `retry`.
    TimedOut { streak: u32, retry: Instant },
}

/// [`version_of`], asked again only when the file's size or mtime moved: the
/// twin is re-rendered whenever the build it execs changes, and the host asks
/// at every worker's start and every activation notice. An answer — a
/// version, or a clean failure to name one — is remembered while the file is
/// unchanged; a TIMEOUT is asked again on a back-off ([`VERSION_RETRY_FIRST`]).
fn cached_version(exe: &Path) -> Option<Version> {
    cached_version_with(exe, Instant::now(), version_of)
}

/// [`cached_version`] at `now`, over the `probe` it caches.
fn cached_version_with(
    exe: &Path,
    now: Instant,
    probe: impl FnOnce(&Path) -> Probed,
) -> Option<Version> {
    type Cache = std::collections::HashMap<PathBuf, (u64, SystemTime, Seen)>;
    static CACHE: std::sync::Mutex<Option<Cache>> = std::sync::Mutex::new(None);
    let meta = std::fs::metadata(exe)
        .ok()
        .filter(std::fs::Metadata::is_file)?;
    let stamp = (meta.len(), meta.modified().ok()?);
    let before = CACHE.lock().ok().and_then(|cache| {
        cache
            .as_ref()
            .and_then(|c| c.get(exe))
            .filter(|(len, at, _)| (*len, *at) == stamp)
            .map(|(_, _, seen)| seen.clone())
    });
    let streak = match before {
        Some(Seen::Answered(v)) => return v,
        Some(Seen::TimedOut { retry, .. }) if now < retry => return None,
        Some(Seen::TimedOut { streak, .. }) => streak,
        None => 0,
    };
    let (seen, v) = match probe(exe) {
        Probed::Version(v) => (Seen::Answered(Some(v.clone())), Some(v)),
        Probed::NoVersion => (Seen::Answered(None), None),
        Probed::TimedOut => {
            let wait = VERSION_RETRY_FIRST
                .saturating_mul(1 << streak.min(16))
                .min(VERSION_RETRY_MAX);
            let retry = now.checked_add(wait).unwrap_or(now);
            (
                Seen::TimedOut {
                    streak: streak.saturating_add(1),
                    retry,
                },
                None,
            )
        }
    };
    if let Ok(mut cache) = CACHE.lock() {
        cache
            .get_or_insert_with(Cache::new)
            .insert(exe.to_path_buf(), (stamp.0, stamp.1, seen));
    }
    v
}

// ---------------------------------------------------------------- the tab

pub(super) type Client = RelayClient<aterm_uds::CtlStream>;

pub(super) fn connect(opts: &Opts, sid: &str) -> Result<Client, String> {
    let sock = match &opts.sock {
        Some(s) => s.clone(),
        None => aterm_ctl::resolve_sock_for(Some(sid)).map_err(|e| format!("{e}"))?,
    };
    let token = aterm_ctl::read_token_beside(&sock).map_err(|e| format!("{e}"))?;
    RelayClient::connect_local(&sock, &token).map_err(|e| format!("{sock}: {e}"))
}

/// The instance's live tab ids.
pub(super) fn roster(c: &mut Client) -> Vec<String> {
    let Ok((head, body)) = c.request_counted("sessions") else {
        return Vec::new();
    };
    if !head.starts_with("OK") {
        return Vec::new();
    }
    body.lines()
        .filter_map(|l| {
            l.split_whitespace()
                .find(|t| t.starts_with("s-"))
                .map(str::to_owned)
        })
        .collect()
}

/// `who` reads the owner-only registry without the `sessions` window-placement
/// hop. Each PTY master's foreground process group is a kernel-backed tab
/// claim, available even when macOS returns argv without another process's env.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LiveTab {
    pub(super) sid: String,
    pub(super) fgpgid: Option<i64>,
}

pub(super) fn host_roster(c: &mut Client) -> Option<Vec<LiveTab>> {
    let (head, body) = c.request_counted("who").ok()?;
    parse_host_roster(&head, &body)
}

fn parse_host_roster(head: &str, body: &str) -> Option<Vec<LiveTab>> {
    if !head.starts_with("OK") {
        return None;
    }
    let mut tabs = Vec::new();
    for row in body.lines() {
        let mut words = row.split_whitespace();
        words.next()?.parse::<u64>().ok()?;
        let sid = words.next()?.to_string();
        if !sid.starts_with("s-") || tabs.iter().any(|t: &LiveTab| t.sid == sid) {
            return None;
        }
        let fgpgid = row
            .split_whitespace()
            .find_map(|word| word.strip_prefix("fgpgid="))
            .and_then(|word| word.parse::<i64>().ok())
            .filter(|&group| group > 0);
        tabs.push(LiveTab { sid, fgpgid });
    }
    Some(tabs)
}

fn tab_is_live(tabs: &[LiveTab], sid: &str) -> bool {
    tabs.iter().any(|tab| tab.sid == sid)
}

/// A duplicated foreground group is ambiguity, never permission to choose
/// whichever tab happened to be listed first.
pub(super) fn unique_tab_for_group(tabs: &[LiveTab], group: i64) -> Option<&str> {
    let mut found = None;
    for tab in tabs.iter().filter(|tab| tab.fgpgid == Some(group)) {
        if found.is_some() {
            return None;
        }
        found = Some(tab.sid.as_str());
    }
    found
}

/// Recheck the unique PTY claim before a delayed typed or signalled step.
pub(super) fn process_in_tab(
    c: &mut Client,
    pid: u32,
    tab: &str,
    expected_group: Option<i64>,
    expected_parent: Option<u32>,
) -> bool {
    let Some(tabs) = host_roster(c) else {
        return false;
    };
    let Some(group) = process_group(pid) else {
        return false;
    };
    expected_group.is_none_or(|expected| expected == group)
        && unique_tab_for_group(&tabs, group) == Some(tab)
        && ids(pid).is_some_and(|(parent, pgid, tpgid)| {
            expected_parent.is_none_or(|expected| expected == parent)
                && pgid == group
                && tpgid == group
        })
}

/// The tab's visible rows, the cursor within them, and its content sequence.
pub(super) struct Screen {
    pub(super) rows: Vec<String>,
    pub(super) cursor: Option<(usize, usize)>,
    pub(super) seq: u64,
    /// The screen row `rows[0]` is: the coordinates the `cell` verb takes.
    pub(super) first: usize,
    /// The screen GENERATION (`gen`, `<epoch>.<seq>`) the read was taken at:
    /// what a fenced `turn if-gen=` names ([`turn_fenced`]). `None` from a
    /// host that sends none.
    pub(super) generation: Option<String>,
    /// When a PERSON last gave the session input, as the server stamped it
    /// at the read (`"human_ms"`): the one presence fact
    /// ([`upgrade::attended_by`], [`upgrade::Facts::attended`]).
    pub(super) human: HumanInput,
}

pub(super) fn screen(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(c, &format!("@{sid} text --json"))
}

/// Claude's composer is pinned near the bottom. The continuation wait needs
/// only that slice while a draft is present; a positive result is confirmed
/// against the whole screen before it can authorize a turn.
const CONTINUATION_TAIL_ROWS: usize = 20;

fn screen_tail(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(
        c,
        &format!("@{sid} text --json tail={CONTINUATION_TAIL_ROWS}"),
    )
}

fn screen_query(c: &mut Client, request: &str) -> Option<Screen> {
    let (head, body) = c.request_counted(request).ok()?;
    if !head.starts_with("OK") {
        return None;
    }
    parse_screen(if body.trim().is_empty() {
        head.strip_prefix("OK ")?
    } else {
        &body
    })
}

/// One `text --json` object: its rows, the cursor made relative to them (the
/// cursor row is absolute; `first` is the row the reply starts at), and `seq`.
fn parse_screen(json: &str) -> Option<Screen> {
    let v: Value = aterm_json::from_str(json.trim()).ok()?;
    let rows: Vec<String> = v
        .get("rows")?
        .as_array()?
        .iter()
        .map(|r| r.as_str().unwrap_or("").to_string())
        .collect();
    let first = v.get("first").and_then(Value::as_u64).unwrap_or(0);
    let cursor = v.get("cursor").and_then(|cur| {
        let row = cur.get("row")?.as_u64()?.checked_sub(first)?;
        let col = cur.get("col")?.as_u64()?;
        Some((usize::try_from(row).ok()?, usize::try_from(col).ok()?))
    });
    let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let generation = v
        .get("gen")
        .and_then(Value::as_str)
        .filter(|g| is_generation(g))
        .map(str::to_owned);
    // Absent (a host older than the stamp) is unknown; `null` is never; a
    // number is its age; anything else is unknown, never a guess — the
    // supervisor's own reading of the same field.
    let human = match v.get("human_ms") {
        None => HumanInput::Unknown,
        Some(Value::Null) => HumanInput::Never,
        Some(h) => h.as_u64().map_or(HumanInput::Unknown, HumanInput::Ago),
    };
    Some(Screen {
        rows,
        cursor,
        seq,
        first: usize::try_from(first).ok()?,
        generation,
        human,
    })
}

/// Whether `g` is a screen generation the server takes back as the `if-gen=`
/// fence — asked of the ONE reader the server parses it with
/// ([`ScreenGen::parse`], shared through `aterm_types::control_verbs`), never
/// a local copy of its shape: a local "two runs of digits" check passed an
/// epoch past `u64::MAX` that the server answers with `ERR usage`. Anything
/// else is never put on a fenced request line.
fn is_generation(g: &str) -> bool {
    ScreenGen::parse(g).is_some()
}

/// [`upgrade::composer_is_empty`] over `scr`, with the one fact its rows cannot
/// carry: whether the text at the cursor's row, column 2 is drawn DIM (Claude's
/// placeholder suggestion) or not (a typed draft whose caret was moved home).
/// Read with the `cell` verb only when the cursor IS at column 2, the one case
/// the answer can change. A reply that is not an `OK` naming `dim` — an `ERR`,
/// a lost connection, an unexpected shape — reads NOT dim: a suggestion then
/// waits as a draft would, and nothing is ever typed over a draft.
pub(super) fn composer_empty(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    let dim = match scr.cursor {
        Some((row, 2)) => scr.first.checked_add(row).is_some_and(|at| {
            c.request_line(&format!("@{sid} cell {at} 2"))
                .is_ok_and(|line| cell_is_dim(&line))
        }),
        _ => false,
    };
    upgrade::composer_is_empty(&scr.rows, scr.cursor, dim)
}

/// The idle continuation can wait for two minutes with a person's draft in
/// the composer. Read only the bottom rows on those repeated negative polls.
/// A missing cursor/caret makes that slice inconclusive; an unreadable tail
/// may be an older host, so fall back to the full-screen path for this wait.
/// A positive tail is never enough to type a continuation: the full screen
/// must independently show an empty composer.
pub(super) fn continuation_composer_empty(
    c: &mut Client,
    sid: &str,
    tail_available: &mut bool,
) -> bool {
    if *tail_available {
        match screen_tail(c, sid) {
            Some(scr)
                if scr.cursor.is_some_and(|(row, _)| row < scr.rows.len())
                    && aterm_phase::phase::composer_index(&scr.rows).is_some() =>
            {
                if !composer_empty(c, sid, &scr) {
                    return false;
                }
            }
            Some(_) => {}
            None => *tail_available = false,
        }
    }
    screen(c, sid).is_some_and(|scr| composer_empty(c, sid, &scr))
}

use crate::supervise::screen::cell_is_dim;

/// Whether someone other than this sweep holds the tab: a halt, a hand on it
/// (a turn, a lease, a driver), or a person who typed into it within
/// `grace_s` seconds — `0` asks the hold and the hands alone (the upgrade's
/// gates read the person as [`upgrade::Facts::attended`], which the owner's
/// `--now` waives).
pub(super) fn held(c: &mut Client, sid: &str, grace_s: u32) -> bool {
    c.request_line(&format!("@{sid} status"))
        .map_or(true, |line| held_by_status(&line, grace_s))
}

/// [`held`] over one `status` reply; anything but an `OK` reads as held. A
/// `human_ms=` under the grace is a person at the keyboard, and a person wins
/// (the owner's rule); `-` or no field at all (an older server) is nobody.
fn held_by_status(line: &str, grace_s: u32) -> bool {
    !line.starts_with("OK")
        || line
            .split_whitespace()
            .any(|t| t == "hold=1" || (t.starts_with("hand=") && t != "hand=-"))
        || super::relaunch::person_present(line, grace_s)
}

/// Type one line at a shell's prompt as KEYSTROKES — the bytes written raw
/// (`send`), then Enter — never as a paste. `Ok` only when both answer `OK`.
///
/// A paste is framed for bracketed paste whenever the TERMINAL's mode says
/// so, and an agent that crashed never gave back the mode it set: measured
/// 2026-09-25, a SIGKILLed Claude Code 2.1.282 left `bracketed_paste=true`
/// (with its kitty keyboard, `modifyOtherKeys` and mouse modes) on the tab,
/// and the relaunch line pasted at macOS's `/bin/bash` 3.2 — whose readline
/// has no bracketed paste — ended in the stray `01~` of the closing frame,
/// glued to `--resume <id>`: Claude opened its resume picker on a
/// conversation that does not exist. Raw bytes are read as typed whatever
/// the terminal's modes; Enter stays a carriage return under the kitty
/// `disambiguate` flag the agent leaves.
pub(super) fn type_line(c: &mut Client, sid: &str, line: &str) -> Result<(), String> {
    for request in [
        format!("@{sid} send -- {line}"),
        format!("@{sid} key enter"),
    ] {
        let reply = c.request_line(&request).map_err(|e| format!("{e}"))?;
        if !reply.starts_with("OK") {
            return Err(reply);
        }
    }
    Ok(())
}

/// Why a typed turn did not go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Typed {
    /// The screen moved between the read that judged it and the paste — a
    /// keystroke's echo, a box, output — and the host typed NOTHING
    /// (`turn if-gen=`'s `skipped reason=changed`). A wait: the next look
    /// judges the screen as it is.
    Changed,
    /// A person kept typing through the whole park on the typing momentum
    /// (`yield=`), and the host typed NOTHING (`ERR yield timeout`). A wait,
    /// like [`Typed::Changed`] — never a refusal: a refusal of the relaunch
    /// line fails an upgrade whose agent has already been ended.
    Yielded,
    /// Anything else the host answered (or the connection failed with).
    Refused(String),
}

impl Typed {
    /// The report word: `changed`, `yield`, or the refusal's first words.
    pub(super) fn word(&self) -> String {
        match self {
            Typed::Changed => "changed".to_string(),
            Typed::Yielded => "yield".to_string(),
            Typed::Refused(e) => first_word(e),
        }
    }
}

/// Type one turn into the tab. `Ok` only on an `OK` reply.
pub(super) fn turn(c: &mut Client, sid: &str, text: &str) -> Result<(), String> {
    turn_fenced(c, sid, text, None).map_err(|e| match e {
        Typed::Refused(e) => e,
        other => other.word(),
    })
}

/// Whether the host's `turn` takes the `if-gen=` fence: its `help turn`
/// names it. A host older than the fence would TYPE a leading `if-gen=…` as
/// part of the text (its option parser stops at the first word it does not
/// know), so the fence is never sent unprobed.
fn turn_takes_gen(c: &mut Client) -> bool {
    c.request_counted("help turn")
        .is_ok_and(|(head, body)| head.starts_with("OK") && (head + &body).contains("if-gen="))
}

/// THE CHECKED TYPE: one turn into the tab, parked first on a person's typing
/// momentum (`yield=0.2`, the loop's own guarded turn's floor — the person
/// never waits) and, with a `generation`, pasted only while the screen is
/// still the one it names — the read that judged it — checked by the host
/// under its terminal lock immediately before the paste (`turn if-gen=`). The
/// screen read, the process and owner checks and the paste were three steps
/// with a window between them; a person's first keystroke in that window was
/// pasted in front of, or merged with, what the harness typed. `None`, or a
/// host without the fence, types unfenced.
pub(super) fn turn_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
) -> Result<(), Typed> {
    let fence = generation
        .filter(|g| is_generation(g))
        .filter(|_| turn_takes_gen(c))
        .map(|g| format!("if-gen={g} "))
        .unwrap_or_default();
    let (head, _) = c
        .request_counted(&format!(
            "@{sid} turn {fence}yield=0.2 idle=1500 timeout=30000 {text}"
        ))
        .map_err(|e| Typed::Refused(format!("{e}")))?;
    turn_verdict(&head)
}

/// One `turn` reply's head, judged: a `skipped` verdict typed nothing — the
/// fence's `reason=changed` is [`Typed::Changed`] — a yield that outlived the
/// turn's timeout typed nothing either ([`Typed::Yielded`]), and any other
/// `OK` typed.
fn turn_verdict(head: &str) -> Result<(), Typed> {
    if head.starts_with("ERR yield timeout") {
        return Err(Typed::Yielded);
    }
    if !head.starts_with("OK") {
        return Err(Typed::Refused(head.to_string()));
    }
    let words: Vec<&str> = head.split_whitespace().collect();
    if words.contains(&"skipped") {
        return Err(if words.contains(&"reason=changed") {
            Typed::Changed
        } else {
            Typed::Refused(head.to_string())
        });
    }
    Ok(())
}

// ---------------------------------------------------------------- models

/// The step's MODEL HALF, read once per pass: the priority list (grown from
/// Claude Code's own recommendations), the managed build's baked catalog, and
/// the list's best model available to this user on that build.
pub(super) struct ModelCtx {
    list: Priority,
    baked: Option<Baked>,
    target: Option<String>,
    /// The person's saved default model (`~/.claude/settings.json` `model`):
    /// a choice of theirs, moved only within its family.
    default_model: Option<String>,
}

impl ModelCtx {
    /// No model half at all (a test's pass, or a list that cannot be read):
    /// nothing is ever due.
    pub(super) fn none() -> ModelCtx {
        ModelCtx {
            list: Priority::seed(0),
            baked: None,
            target: None,
            default_model: None,
        }
    }

    fn read(opts: &Opts, targets: &Targets) -> ModelCtx {
        let now = i64::try_from(now_s()).unwrap_or(0);
        let path = state_dir(opts).join("models.json");
        let Ok(mut list) = (if opts.dry_run {
            std::fs::read_to_string(&path).map_or_else(
                |_| Ok(Priority::seed(now)),
                |t| Priority::parse(&t).ok_or_else(String::new),
            )
        } else {
            models::load_or_seed(&path, now)
        }) else {
            return ModelCtx::none();
        };
        let managed = targets.managed.as_ref();
        let baked = managed
            .and_then(|m| std::fs::read_to_string(&m.exe).ok())
            .and_then(|twin| catalog::twin_binary(&twin))
            .and_then(|bin| catalog::baked_cached(&bin, &state_dir(opts).join("builds")));
        let label = managed.map(|m| m.version.to_string()).unwrap_or_default();
        let ev = models::read_evidence(&opts.home.join(".claude"), &opts.home.join(".claude.json"));
        let recs = models::recommendations(&ev, baked.as_ref(), &label);
        let changes = models::ingest(&mut list, &recs, baked.as_ref(), now);
        if !changes.is_empty() && !opts.dry_run {
            let _ = models::write_atomic(&path, &list.render());
            for ch in &changes {
                let r = Report {
                    pid: 0,
                    tab: "-".to_string(),
                    session: "-".to_string(),
                    from: "-".to_string(),
                    to: ch.id.clone(),
                    step: "models-changed".to_string(),
                };
                ledger(opts, &r, &format!("{} ({})", ch.placed, ch.source));
            }
        }
        let user = std::fs::read_to_string(opts.home.join(".claude").join("settings.json"))
            .map(|t| models::parse_user_settings(&t))
            .unwrap_or_default();
        let target =
            models::target_allowed(&list, &label, &ev, baked.as_ref(), user.allowed.as_deref());
        ModelCtx {
            list,
            baked,
            target,
            default_model: user.default,
        }
    }
}

fn model_record_path(opts: &Opts, session: &str) -> PathBuf {
    state_dir(opts)
        .join("models")
        .join(format!("{session}.json"))
}

fn load_model_record(opts: &Opts, session: &str) -> ModelRecord {
    std::fs::read_to_string(model_record_path(opts, session))
        .ok()
        .and_then(|t| ModelRecord::parse(&t))
        .unwrap_or_default()
}

fn save_model_record(opts: &Opts, session: &str, rec: &ModelRecord) {
    if opts.dry_run {
        return;
    }
    let _ = models::write_atomic(&model_record_path(opts, session), &rec.render());
}

/// One conversation's model, read once per visit.
struct ModelRead {
    verdict: ModelVerdict,
    /// The conversation's prompt cache is cold (no answer for
    /// [`models::CACHE_COLD_S`]).
    cold: bool,
    /// Seconds the CURRENT due move has been due (0 when none is) — the clock
    /// [`models::MODEL_WARM_MAX_S`] bounds.
    due_for_s: u64,
}

/// Read what the conversation runs NOW and decide. A model this harness asked
/// for that now runs is recorded as APPLIED (never asked for again).
fn model_read(opts: &Opts, sf: &SessionFile, launch: Option<&str>, mctx: &ModelCtx) -> ModelRead {
    let mut record = load_model_record(opts, &sf.session_id);
    if mctx.target.is_none() && record.set.is_empty() {
        // Nothing is due: no model can be run and none is pending. Clear the
        // due clock here too, or a move that comes back later would inherit
        // this one's start and skip the warm-cache grace.
        if !record.due_to.is_empty() || record.due_since != 0 {
            record.due_to.clear();
            record.due_since = 0;
            save_model_record(opts, &sf.session_id, &record);
        }
        return ModelRead {
            verdict: ModelVerdict::Keep("no-model-available"),
            cold: false,
            due_for_s: 0,
        };
    }
    let tail = transcript(&opts.home, &sf.session_id)
        .map(|p| tail_to_end(&p, TAIL_BYTES).0)
        .unwrap_or_default();
    let live = models::live_model_at(
        &tail,
        mctx.baked.as_ref(),
        launch,
        models::parse_lstart(&sf.proc_start),
    );
    let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
    if !record.set.is_empty()
        && live
            .as_ref()
            .is_some_and(|l| base(&l.id) == base(&record.set))
        && !record.applied.iter().any(|a| base(a) == base(&record.set))
    {
        record.applied.push(record.set.clone());
        save_model_record(opts, &sf.session_id, &record);
        let mut r = blank(sf);
        r.to = format!("model:{}", record.set);
        ledger(opts, &said(r, "model-verified"), "the conversation runs it");
    }
    // Asked for and still not what runs once the relaunch has had its time:
    // FAILED for this conversation, never asked for again.
    if !record.set.is_empty()
        && live
            .as_ref()
            .is_none_or(|l| base(&l.id) != base(&record.set))
        && now_s().saturating_sub(record.set_at) >= models::MODEL_SETTLE_S
    {
        let failed = std::mem::take(&mut record.set);
        record.failed.push(failed.clone());
        save_model_record(opts, &sf.session_id, &record);
        let mut r = blank(sf);
        r.to = format!("model:{failed}");
        ledger(
            opts,
            &said(r, "model-failed"),
            "asked for on a relaunch and not what runs",
        );
    }
    // A `/model` newer than the last answer is a PERSON's (the harness never
    // types one): remembered, so the answers that follow do not end its
    // protection.
    if let Some(l) = live.as_ref().filter(|l| l.by_command)
        && record.human != l.id
    {
        record.human.clone_from(&l.id);
        save_model_record(opts, &sf.session_id, &record);
    }
    let verdict = models::model_due(
        &mctx.list,
        live.as_ref(),
        mctx.target.as_deref(),
        launch,
        mctx.default_model.as_deref(),
        &record,
    );
    let cold = models::last_answer_at(&tail)
        .is_none_or(|at| now_s().saturating_sub(at) >= models::CACHE_COLD_S);
    // THE DUE CLOCK: since when THIS move has been due. It restarts when the
    // target changes (a newer model is a new wait, not the old one's
    // remainder) and is cleared when the conversation DEFINITELY needs no move
    // (it runs the target, or the move is not the harness's to make), so a move
    // that stopped being due and came back never inherits a stale clock.
    //
    // `model-unknown` is NOT such a verdict. It means this visit could not read
    // what the conversation runs — a transcript tail with no answer in it, which
    // a long tool result can cause for many visits running — and clearing on it
    // would restart the bound on every such flicker, so an active session's
    // move might never land: the never-lands shape this clock exists to end
    // (adversarial review, 2026-09-25). An unknown visit leaves the clock
    // exactly as it was.
    let now = now_s();
    let due_for_s = match &verdict {
        ModelVerdict::Due { to, .. } => {
            // A start AHEAD of now (the wall clock stepped back after it was
            // stamped: a manual date change, a boot before NTP) would read as
            // 0 s due for the whole skew and postpone the bound by exactly
            // that much. Restart it instead: the bound then lands on time.
            if base(&record.due_to) != base(to) || record.due_since == 0 || record.due_since > now {
                record.due_to.clone_from(to);
                record.due_since = now;
                save_model_record(opts, &sf.session_id, &record);
            }
            now.saturating_sub(record.due_since)
        }
        ModelVerdict::Keep("model-unknown") => 0,
        ModelVerdict::Keep(_) => {
            if !record.due_to.is_empty() || record.due_since != 0 {
                record.due_to.clear();
                record.due_since = 0;
                save_model_record(opts, &sf.session_id, &record);
            }
            0
        }
    };
    ModelRead {
        verdict,
        cold,
        due_for_s,
    }
}

/// THE MODEL THAT RIDES A RESTART MADE FOR ANOTHER REASON — a relaunch on
/// exit, the restart in place (THE MODEL LADDER wired into every restart
/// ours makes): the priority list's model when a move is DUE for this
/// conversation (`sf`, launched with `argv`), taken now because the restart
/// happens regardless ([`models::model_moves_now`]'s `build-restart`: the
/// switch re-reads a history the restart re-reads anyway). `None` when
/// nothing is due, or the move is not the harness's to make. The caller
/// records it as asked for before the one act ([`record_asked`]).
pub(super) fn riding_model(opts: &Opts, sf: &SessionFile, argv: &[String]) -> Option<String> {
    let targets = Targets::read(&opts.home);
    riding_model_in(opts, sf, argv, &ModelCtx::read(opts, &targets))
}

/// [`riding_model`] against `mctx`: the seam a test reads its own list
/// through.
pub(super) fn riding_model_in(
    opts: &Opts,
    sf: &SessionFile,
    argv: &[String],
    mctx: &ModelCtx,
) -> Option<String> {
    let launch = upgrade::launch_model(argv);
    let mread = model_read(opts, sf, launch.as_deref(), mctx);
    let ModelVerdict::Due { to, .. } = mread.verdict else {
        return None;
    };
    models::model_moves_now(mread.cold, true, mread.due_for_s).map(|_| to)
}

/// Remember `model` as the one the relaunch of `session` asks for, BEFORE
/// its one act, so it is never asked for twice (a model then not taken is
/// recorded as failed, [`model_read`]).
pub(super) fn record_asked(opts: &Opts, session: &str, model: &str) {
    if opts.dry_run || model.is_empty() {
        return;
    }
    let mut rec = load_model_record(opts, session);
    rec.set = model.to_string();
    rec.set_at = now_s();
    save_model_record(opts, session, &rec);
}

/// The build a session runs, as a relaunch target for a MODEL-ONLY restart:
/// the managed twin when that is the build it runs (so the relaunch goes
/// through the twin, as every managed relaunch does), the native build for a
/// native session, else the process's own executable.
fn same_build(
    targets: &Targets,
    exe: &Path,
    running: &Version,
    running_native: bool,
) -> Option<Candidate> {
    let pick = if running_native {
        targets.native.as_ref()
    } else {
        targets.managed.as_ref()
    };
    if let Some(c) = pick.filter(|c| c.version == *running) {
        return Some(c.clone());
    }
    (!exe.as_os_str().is_empty()).then(|| Candidate {
        exe: exe.to_path_buf(),
        version: running.clone(),
        source: if running_native {
            Source::Native
        } else {
            Source::Managed
        },
    })
}

/// The cheap pre-filter [`due`] and the pass ask: whether this session's
/// model is due (the launch's own `--model` is not read here — it is in
/// [`visit_with_claim`]).
fn model_wants_a_look(opts: &Opts, sf: &SessionFile, mctx: &ModelCtx) -> bool {
    mctx.target.is_some()
        && matches!(
            model_read(opts, sf, None, mctx).verdict,
            ModelVerdict::Due { .. }
        )
}

/// `aterm harness upgrade models`: the list, best first, each model's
/// availability on the managed build (and why not), the target, and what
/// Claude Code itself recommends. Reads only (`opts.dry_run` is honoured by
/// [`ModelCtx::read`]: no list is written, no ledger row).
#[must_use]
pub fn models_report(opts: &Opts) -> String {
    use std::fmt::Write as _;
    let targets = Targets::read(&opts.home);
    let ctx = ModelCtx::read(opts, &targets);
    let label = targets
        .managed
        .as_ref()
        .map_or_else(|| "?".to_string(), |m| m.version.to_string());
    let ev = models::read_evidence(&opts.home.join(".claude"), &opts.home.join(".claude.json"));
    let (_, verdicts) = models::target(&ctx.list, &label, &ev, ctx.baked.as_ref());
    let mut out = format!("Claude Code (managed): {label}\n");
    for (i, (id, a)) in verdicts.iter().enumerate() {
        let name = ctx
            .baked
            .as_ref()
            .and_then(|b| b.model(id))
            .map_or(String::new(), |m| format!(" ({})", m.display_name));
        let verdict = match a {
            models::Availability::Yes(why) => format!("available — {why}"),
            models::Availability::No(why) => format!("NOT available — {why}"),
        };
        let _ = writeln!(out, "  {}. {id}{name}: {verdict}", i + 1);
    }
    let _ = writeln!(
        out,
        "target: {}",
        ctx.target.as_deref().unwrap_or("none available")
    );
    for (id, source) in models::recommendations(&ev, ctx.baked.as_ref(), &label) {
        let _ = writeln!(out, "  recommended by Claude Code: {id} ({source})");
    }
    out
}

/// `aterm harness upgrade models set <id>,<id>,...`: replace the list, best
/// first; every entry a Claude model id, no repeats. One ledger row.
///
/// # Errors
/// An entry that is not a model id, a repeat, or a write that failed.
pub fn set_models(opts: &Opts, list: &str) -> Result<Vec<String>, String> {
    let now = i64::try_from(now_s()).unwrap_or(0);
    let path = state_dir(opts).join("models.json");
    let ids: Vec<String> = list
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut p = models::load_or_seed(&path, now).unwrap_or_else(|_| Priority::seed(now));
    let candidate = Priority {
        ids: ids.clone(),
        history: p.history.clone(),
    };
    if ids.is_empty() || Priority::parse(&candidate.render()).is_none() {
        return Err(
            "every entry must be a Claude model id (claude-<family>-<n>[-<n>]), no repeats"
                .to_string(),
        );
    }
    p.ids.clone_from(&ids);
    p.history.push(models::Change {
        id: list.to_string(),
        placed: "set".into(),
        source: "owner".into(),
        at: now,
    });
    models::write_atomic(&path, &p.render()).map_err(|e| format!("{}: {e}", path.display()))?;
    let r = Report {
        pid: 0,
        tab: "-".to_string(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: ids.join(","),
        step: "models-set".to_string(),
    };
    ledger(opts, &r, "the priority list was set by hand");
    Ok(ids)
}

/// How the one signal went.
pub(super) enum Terminated {
    Sent,
    /// Not sent, and why (a hold, or the process no longer leads the
    /// foreground group): a wait, not a failure.
    Refused(String),
    /// The kernel refused a signal we did send.
    Failed,
}

/// SIGTERM to the agent as ONE server-side decision: `signal term pid=<pid>`
/// reaches exactly that process, only while it leads the tab's foreground
/// group, and is refused under a hold — so a hold placed after the last
/// `status` read can never be read stale. An aterm older than `pid=` answers
/// `ERR unknown signal` having sent nothing; then the hold is re-read and the
/// kernel is asked directly, as before.
pub(super) fn terminate(c: &mut Client, tab: &str, pid: i32, grace_s: u32) -> Terminated {
    match c.request_line(&format!("@{tab} signal term pid={pid}")) {
        Ok(reply) if reply.starts_with("OK") => Terminated::Sent,
        Ok(reply) if reply.starts_with("ERR unknown signal") => {
            if held(c, tab, grace_s) {
                return Terminated::Refused("held".to_string());
            }
            #[cfg(unix)]
            {
                // SAFETY: a plain signal to one verified pid of our own uid.
                if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
                    Terminated::Sent
                } else {
                    Terminated::Failed
                }
            }
            // Windows has no SIGTERM for a console process, and
            // `TerminateProcess` is the harder stop this drive never sends: a
            // wait, never a kill. No Windows step gets here anyway — every
            // job read is `ps -o …`, which Windows does not ship.
            #[cfg(not(unix))]
            {
                let _ = pid;
                Terminated::Refused("unsupported".to_string())
            }
        }
        Ok(reply) if reply.starts_with("ERR halted") => Terminated::Refused("held".to_string()),
        Ok(_) | Err(_) => Terminated::Refused("refused".to_string()),
    }
}

// ---------------------------------------------------------------- state

/// What a restart's record took when it was written ([`St::signalled`]) and
/// puts back if its signal is never sent ([`St::unsent`]).
#[derive(Debug)]
pub(super) struct Unsent {
    phase: Phase,
    release: String,
    marker: String,
    markers: Vec<String>,
}

/// What the sweep remembers about one conversation's upgrade. Everything a
/// relaunch needs is written here BEFORE the agent is signalled, so a sweep that
/// dies between the exit and the relaunch leaves the next one enough to finish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct St {
    pub(super) phase: Phase,
    pub(super) from: String,
    pub(super) to: String,
    pub(super) source: String,
    /// The READY marker of the latest notice ([`upgrade::ready_marker`]).
    pub(super) marker: String,
    /// EVERY READY marker this round of the upgrade typed and still honours,
    /// the latest included ([`answered`]): notices queued behind a usage limit
    /// are delivered AT ONCE (measured 2026-09-26: four, at 06:00), and the
    /// agent answers whichever it answers — it chose the last, but any is the
    /// answer to a notice of this upgrade. Forgotten with `marker`
    /// ([`St::forget_markers`]): a void, a release, the owner's word.
    pub(super) markers: Vec<String>,
    /// THE RELEASE OWED ([`upgrade::release_prompt`]): why the upgrade
    /// abandoned a notice the agent was given without restarting it (`gave-up`,
    /// `void`, `skipped`, `deferred`, a stop's reason), or empty — nothing is
    /// owed. Set once per abandonment ([`St::owe_release`]); cleared when the
    /// line is typed or dropped ([`release_void`]), when a new notice
    /// supersedes it, or when the restart ends the agent (its carry-on says
    /// continue).
    pub(super) release: String,
    /// EVERY READY MARKER THIS ROUND TYPED, honoured or not — [`St::markers`]
    /// forgets them at a void, a stop, a release, while a release owed for
    /// the round still needs them. Never an answer to anything: a READY to
    /// one is the agent's LAST ANSWER TO THE UPGRADE, after which a direction
    /// counts ([`release_void`], [`upgrade::directed_since_ready`]; the
    /// second review of 2026-09-26). Opened by the round's first notice
    /// ([`St::announced`]), and carried with a release owed onto a new
    /// target's state ([`St::for_target`]).
    pub(super) asked: Vec<String>,
    pub(super) salt: u64,
    pub(super) last_seq: u64,
    pub(super) seq_since_s: u64,
    /// The agent that was signalled.
    pub(super) pid: u32,
    /// The process whose tab received the READY notice. `pid` becomes the
    /// process signalled at restart, so notice ownership needs its own fence.
    pub(super) notice_pid: u32,
    pub(super) notice_start: String,
    /// The shell that gets its prompt back.
    pub(super) shell: u32,
    /// The tab the shell lives in.
    pub(super) tab: String,
    /// The line typed at that prompt.
    pub(super) line: String,
    /// Where that prompt left the cursor when a relaunch attempt first read it
    /// ([`PromptMark`]): kept across attempts, so a person's typing at the
    /// prompt is judged against the prompt as it came back, never against
    /// their own half-typed command.
    pub(super) prompt: Option<PromptMark>,
    /// The last reason this upgrade was held back that the ledger was told
    /// ([`held_back`]), so it is said once, not every sweep.
    pub(super) noted: String,
    /// Why the agent is being relaunched: empty for this module's upgrade,
    /// [`super::relaunch::CAUSE_EXIT`] for a relaunch on exit — which of the
    /// two continuations the new process is typed.
    pub(super) cause: String,
    /// The model the agent's last turn named when it answered READY
    /// ([`upgrade::transcript_model`] over the tail that proved the answer), or
    /// empty when no turn named one. Said in the continuation and in the
    /// outcome, never acted on: the relaunch carries the launch's own flags.
    pub(super) model_before: String,
    /// Where that tail read ended in the transcript, in bytes: the resumed
    /// session's first turn is the first one written past it. `0`: no mark was
    /// taken (a restart an older build began), and the resumed model is then
    /// unconfirmed rather than read from the start of the conversation.
    pub(super) mark: u64,
    /// The `--model` the relaunch line keeps from the launch
    /// ([`upgrade::launch_model`]), or empty when the launch named none: what
    /// decides the model a resumed session comes back on, said in the outcome.
    pub(super) launch_model: String,
    /// The model the relaunch line ASKS FOR from the priority list
    /// ([`super::upgrade_models`]), or empty when the relaunch keeps the
    /// launch's own `--model` (or names none): what the outcome says decided
    /// the model after.
    pub(super) model_list: String,
    /// Set once the continuation is typed while the resumed session's first
    /// answer is still to be read ([`super::relaunch::confirm`]): the second (since the epoch)
    /// past which a sweep that still finds none records the model as
    /// unconfirmed. `0`: nothing is pending. The phase is DONE by then, never
    /// in flight again, so nothing ever types the continuation twice.
    pub(super) confirm_by: u64,
    /// The build the relaunched process runs — its session file's `version`,
    /// the one its transcript rows carry — and its pid, for the `done` row.
    pub(super) resumed_on: String,
    pub(super) resumed_pid: u32,
    /// The look that first saw the person's hold standing now
    /// ([`upgrade::person_hold`]), every look since seeing it too, none more
    /// than [`upgrade::HOLD_GAP_S`] apart; `0`: none.
    pub(super) hold_since_s: u64,
    /// The last look that saw that hold ([`upgrade::hold_since`]); `0`: none.
    pub(super) hold_seen_s: u64,
    /// When the READY answer the upgrade acts on was first heard, held at
    /// the session's limit ([`upgrade::ready_since`]); `0`: none now. What
    /// bounds how long the agent's own background work may hold a READY
    /// restart ([`upgrade::Facts::ready_s`]).
    pub(super) ready_since: u64,
    // THE OWNER'S VIEW (gap audit 2026-09-24: two sessions sat 1 and 2
    // releases behind for 8h22m with `step=wait:not-idle` every minute, and
    // nothing kept how long or why). Read by [`upgrade_status`]; none of it
    // gates an act except `request`, the owner's own word.
    /// When the session was first found behind the build it is being moved
    /// to, carried onto a newer target while nothing has begun ([`St::for_target`]):
    /// the age is the session's, not the build's. `0` in a state an older
    /// build wrote, read as `salt` ([`St::behind_since`]).
    pub(super) pending_since: u64,
    /// The last wait a step recorded ([`upgrade::wait_word`]); empty once it acts.
    pub(super) wait: String,
    /// When that wait began: the first step that recorded the same word.
    pub(super) wait_since: u64,
    /// The owner's word ([`Request`]), written by `aterm harness upgrade <sid>
    /// --now|--defer|--skip` under the sweep lock ([`upgrade_status::ask`]).
    pub(super) request: Request,
    /// The tab the owner NAMED with that word: the word is on the tab, so the
    /// same conversation resumed in another tab carries none of it
    /// ([`St::request_for`]). Empty: no word, or one written before the field.
    pub(super) request_tab: String,
    /// When (unix seconds) the owner's word was written; `0`: none, or one
    /// written before the field. What bounds how long a `--now` keeps an
    /// overdue upgrade from reading stalled ([`upgrade_status::Row::stall`]).
    pub(super) request_at: u64,
    /// The owner's outcome line of a finished restart
    /// ([`super::relaunch::confirm`]) and when it was written — what the window
    /// records once, as the restart's record.
    pub(super) outcome: String,
    pub(super) done_at: u64,
    /// Which agent this upgrade moves ([`upgrade::Agent`]): a Codex state is
    /// filed as `codex-<tab>` and driven by the Codex lane ([`codex`]) alone —
    /// every Claude loop here passes it by.
    pub(super) agent: upgrade::Agent,
    /// Codex: how the TUI ran — `daemon` (a client of the shared daemon),
    /// `embedded` (it held its thread), `fresh` (no conversation yet).
    pub(super) mode: String,
    /// Codex: the thread the TUI held (embedded, by its writer lock) or the
    /// one its exit named (daemon mode); empty until known.
    pub(super) thread: String,
    /// Codex: the `$CODEX_HOME` the TUI ran with.
    pub(super) codex_home: String,
    /// Codex: the TUI's argv, its working directory and the managed twin, as
    /// they were at the exit — what the relaunch is planned from once the
    /// exit names the thread, by which time the TUI is gone.
    pub(super) argv: Vec<String>,
    pub(super) cwd: String,
    pub(super) twin: String,
    /// Codex: text the lane TYPED into the composer and did not submit — a
    /// guarded Enter that missed leaves it there (`turn … reason=guard`).
    /// Kept so every later visit says `left-typed`, never reads it as a
    /// person's draft, and clears exactly that text while it alone is in the
    /// composer (review of 2026-09-26). Empty: nothing of the lane's there.
    pub(super) left_typed: String,
    /// Codex: when the lane last left text typed (unix seconds; `0`: never).
    /// The act that missed is not tried again for [`upgrade::REASK_S`]: a
    /// guard that misses for a reason that stands would otherwise type into
    /// the person's composer, and clear it, every sweep. The owner's `--now`
    /// lifts it.
    pub(super) left_at: u64,
    /// Codex: when the TUI the `/exit` was typed into was seen GONE (unix
    /// seconds); `0` while it lives or no exit was typed. A move that fails
    /// after it has no process left to vet it by — its record is the tab's
    /// (`upgrade_status::Row::exited_at`).
    pub(super) exited_at: u64,
}

impl St {
    fn fresh(from: &Version, to: &Candidate, now: u64) -> St {
        St {
            from: from.to_string(),
            to: to.version.to_string(),
            source: to.source.as_str().to_string(),
            salt: now,
            seq_since_s: now,
            pending_since: now,
            ..St::default()
        }
    }

    /// The state for moving onto `to` (and, when the priority list asks for
    /// one, onto `model`): the prior one when it names that version and —
    /// once announced — that model, else a fresh one that keeps what still
    /// holds while nothing has begun — how long the session has been behind,
    /// and the owner's word. A skip is of ONE version, so it does not follow
    /// onto a newer one. An announcement that named another model is
    /// announced again: the READY it asked for was consent to a different
    /// restart.
    fn for_target(
        prior: Option<St>,
        from: &Version,
        to: &Candidate,
        model: Option<&str>,
        now: u64,
    ) -> St {
        match prior {
            // Reused when still Pending, when it carries the same model, or
            // when it FAILED: every `Failed` reason (not-a-shell-job, a plan
            // refusal, unanswered, signal/relaunch refused, no-resume,
            // resumed-elsewhere) is about the process or its launch line and is
            // final for the build, whatever model would have ridden it — a
            // model comparison alone re-minted it every sweep, and the same
            // refusal was said and ledgered every 60 s (main's review,
            // 2026-09-25).
            Some(mut st)
                if st.to == to.version.to_string()
                    && (matches!(st.phase, Phase::Pending | Phase::Failed(_))
                        || st.model_list == model.unwrap_or_default()) =>
            {
                // Managed and native builds can share a version, and which
                // one this session moves to follows its launch: until the
                // restart begins, the label says the build the sweep would
                // start NOW (a state minted `native` read `2.1.282(native)`
                // in the ledger while the target was the managed twin —
                // measured 2026-09-25 on the owner's session).
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
                    st.source = to.source.as_str().to_string();
                }
                st
            }
            // A notice for another build or model is abandoned here: the agent
            // it reached is owed its release, carried onto the new state until
            // a new notice supersedes it.
            Some(mut old) if matches!(old.phase, Phase::Pending | Phase::Announced { .. }) => {
                old.owe_release("retargeted");
                let pending_since = old.behind_since();
                let (request, request_tab, request_at) = match old.request {
                    Request::Skip(_) => (Request::None, String::new(), 0),
                    kept => (kept, old.request_tab, old.request_at),
                };
                St {
                    pending_since,
                    request,
                    request_tab,
                    request_at,
                    tab: old.tab,
                    release: old.release,
                    asked: old.asked,
                    notice_pid: old.notice_pid,
                    notice_start: old.notice_start,
                    ..St::fresh(from, to, now)
                }
            }
            // A stopped upgrade's release still owed follows onto the new one,
            // with the notice's fence: it is typed only to the process that
            // notice reached ([`release_void`]).
            Some(old) => St {
                release: old.release,
                asked: old.asked,
                tab: old.tab,
                notice_pid: old.notice_pid,
                notice_start: old.notice_start,
                ..St::fresh(from, to, now)
            },
            None => St::fresh(from, to, now),
        }
    }

    /// The owner's word as it applies to this conversation in `tab`: none
    /// when the owner named another tab. `--now` waives a wait for the tab
    /// the owner looked at; resumed elsewhere, the conversation is asked
    /// afresh (review of 2026-09-25: the word was keyed by conversation alone
    /// and followed it into any tab).
    fn request_for(&self, tab: &str) -> Request {
        if self.request_tab.is_empty() || self.request_tab == tab {
            self.request.clone()
        } else {
            Request::None
        }
    }

    /// A NEW ROUND of this upgrade: the owner's word arms it afresh
    /// ([`upgrade_status::ask`]). The salt is minted again, past every marker
    /// the old one could have made (`salt + asks`, asks at most
    /// [`upgrade::MAX_ASKS`]), so no READY given before the word can answer a
    /// notice typed after it (review of 2026-09-25: a re-armed notice reused
    /// its marker). How long the session has been behind is kept: a state an
    /// older build wrote read that off the salt.
    fn new_round(&mut self, now: u64) {
        if self.pending_since == 0 {
            self.pending_since = self.salt;
        }
        self.salt = now.max(
            self.salt
                .saturating_add(u64::from(upgrade::MAX_ASKS))
                .saturating_add(1),
        );
    }

    /// Since when the session has been behind: [`Self::pending_since`], or
    /// the second the state was minted for a state an older build wrote.
    fn behind_since(&self) -> u64 {
        if self.pending_since == 0 {
            self.salt
        } else {
            self.pending_since
        }
    }

    /// Record the wait `word` a sweep found, keeping when it began while the
    /// same word repeats.
    fn note_wait(&mut self, word: &str, now: u64) {
        if self.wait != word {
            self.wait = word.to_string();
            self.wait_since = now;
        }
    }

    /// What a visit's report says about waiting: a `wait:` step is recorded
    /// ([`upgrade::wait_word`], with Claude's `status` beside `not-idle`), an
    /// act clears it. A `skip:`/`busy:`/`would-` report changes nothing. A
    /// `held-back:<why>` that reaches here is the FIRST sweep of a wait said
    /// once ([`attended_once`]: `held-back:attended`, then `wait:attended`),
    /// so it is recorded as that wait — never as an act that cleared one.
    fn note_step(&mut self, step: &str, status: &str, now: u64) {
        if let Some(why) = step
            .strip_prefix("wait:")
            .or_else(|| step.strip_prefix("held-back:"))
        {
            self.note_wait(&upgrade::wait_word(why, status), now);
        } else if !step.starts_with("skip:") && !step.starts_with("would-") {
            self.wait.clear();
            self.wait_since = 0;
        }
    }

    fn to_json(&self) -> String {
        let (name, at, asks, why) = match &self.phase {
            Phase::Pending => ("pending", 0, 0, ""),
            Phase::Announced { at_s, asks } => ("announced", *at_s, *asks, ""),
            Phase::Exiting { at_s } => ("exiting", *at_s, 0, ""),
            Phase::Relaunched { at_s } => ("relaunched", *at_s, 0, ""),
            Phase::Done => ("done", 0, 0, ""),
            Phase::Failed(w) => ("failed", 0, 0, w.as_str()),
        };
        let mut o = Map::new();
        for (k, v) in [
            ("phase", name),
            ("why", why),
            ("from", &self.from),
            ("to", &self.to),
            ("source", &self.source),
            ("marker", &self.marker),
            ("notice_start", &self.notice_start),
            ("tab", &self.tab),
            ("line", &self.line),
            (
                "prompt",
                &self
                    .prompt
                    .as_ref()
                    .map(PromptMark::word)
                    .unwrap_or_default(),
            ),
            ("noted", &self.noted),
            ("cause", &self.cause),
            ("model_before", &self.model_before),
            ("launch_model", &self.launch_model),
            ("model_list", &self.model_list),
            ("resumed_on", &self.resumed_on),
            ("wait", &self.wait),
            ("request", &self.request.word()),
            ("request_tab", &self.request_tab),
            ("outcome", &self.outcome),
            ("agent", self.agent.word()),
            ("mode", &self.mode),
            ("thread", &self.thread),
            ("codex_home", &self.codex_home),
            ("cwd", &self.cwd),
            ("twin", &self.twin),
            ("left_typed", &self.left_typed),
            ("release", &self.release),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        for (k, v) in [
            ("at", at),
            ("asks", u64::from(asks)),
            ("salt", self.salt),
            ("last_seq", self.last_seq),
            ("seq_since", self.seq_since_s),
            ("pid", u64::from(self.pid)),
            ("notice_pid", u64::from(self.notice_pid)),
            ("shell", u64::from(self.shell)),
            ("mark", self.mark),
            ("confirm_by", self.confirm_by),
            ("resumed_pid", u64::from(self.resumed_pid)),
            ("hold_since", self.hold_since_s),
            ("hold_seen", self.hold_seen_s),
            ("ready_since", self.ready_since),
            ("pending_since", self.pending_since),
            ("wait_since", self.wait_since),
            ("done_at", self.done_at),
            ("request_at", self.request_at),
            ("exited_at", self.exited_at),
            ("left_at", self.left_at),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        for (k, list) in [
            ("argv", &self.argv),
            ("markers", &self.markers),
            ("asked", &self.asked),
        ] {
            if !list.is_empty() {
                o.insert(
                    k.into(),
                    Value::Array(list.iter().map(|a| Value::from(a.as_str())).collect()),
                );
            }
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    fn from_json(text: &str) -> Option<St> {
        let v: Value = aterm_json::from_str(text).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let small = |k: &str| u32::try_from(n(k)).unwrap_or(0);
        let list = |k: &str| -> Vec<String> {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let phase = match s("phase").as_str() {
            "pending" => Phase::Pending,
            "announced" => Phase::Announced {
                at_s: n("at"),
                asks: u32::try_from(n("asks")).unwrap_or(u32::MAX),
            },
            "exiting" => Phase::Exiting { at_s: n("at") },
            "relaunched" => Phase::Relaunched { at_s: n("at") },
            "done" => Phase::Done,
            "failed" => Phase::Failed(s("why")),
            _ => return None,
        };
        Some(St {
            phase,
            from: s("from"),
            to: s("to"),
            source: s("source"),
            marker: s("marker"),
            markers: list("markers"),
            release: s("release"),
            asked: list("asked"),
            salt: n("salt"),
            last_seq: n("last_seq"),
            seq_since_s: n("seq_since"),
            pid: small("pid"),
            notice_pid: small("notice_pid"),
            notice_start: s("notice_start"),
            shell: small("shell"),
            tab: s("tab"),
            line: s("line"),
            prompt: PromptMark::parse(&s("prompt")),
            noted: s("noted"),
            cause: s("cause"),
            model_before: s("model_before"),
            mark: n("mark"),
            launch_model: s("launch_model"),
            model_list: s("model_list"),
            confirm_by: n("confirm_by"),
            resumed_on: s("resumed_on"),
            resumed_pid: small("resumed_pid"),
            hold_since_s: n("hold_since"),
            hold_seen_s: n("hold_seen"),
            ready_since: n("ready_since"),
            pending_since: n("pending_since"),
            wait: s("wait"),
            wait_since: n("wait_since"),
            request: Request::parse(&s("request")).unwrap_or_default(),
            request_tab: s("request_tab"),
            request_at: n("request_at"),
            outcome: s("outcome"),
            done_at: n("done_at"),
            agent: upgrade::Agent::parse(&s("agent")),
            mode: s("mode"),
            thread: s("thread"),
            codex_home: s("codex_home"),
            argv: list("argv"),
            cwd: s("cwd"),
            twin: s("twin"),
            left_typed: s("left_typed"),
            left_at: n("left_at"),
            exited_at: n("exited_at"),
        })
    }

    pub(super) fn in_flight(&self) -> bool {
        matches!(self.phase, Phase::Exiting { .. } | Phase::Relaunched { .. })
    }

    /// Whether a typed continuation's model is still to be read
    /// ([`super::relaunch::confirm`]).
    pub(super) fn confirming(&self) -> bool {
        self.phase == Phase::Done && self.confirm_by != 0
    }

    fn notice_belongs_to(&self, sf: &SessionFile, tab: &str) -> bool {
        self.tab == tab
            && self.notice_pid == sf.pid
            && !self.notice_start.is_empty()
            && self.notice_start == squash(&sf.proc_start)
    }

    // THE UPGRADE'S OWN TRANSITIONS: what every step that types, gives up,
    // voids or stops does to the record — one place each, so the release a
    // notice owes the agent is never forgotten on one path of many (the
    // derived model `harness_upgrade_never_strands_model` is bound to these).

    /// Whether a notice of THIS round of a Claude upgrade reached an agent
    /// that still lives to hold for it: the upgrade was announced, or gave up
    /// asking. A restart past its signal (exiting, relaunched) has no such
    /// agent — it was ended, and its conversation is the carry-on's, or the
    /// person's who resumed it by hand (review of 2026-09-26: a release owed
    /// by `resumed-elsewhere` was typed into the process a person had
    /// started in another tab, often on the new build, where "nothing will
    /// restart this session" is false). A relaunch's record ([`St::cause`])
    /// typed no notice, and a Codex record is its own lane's.
    fn notified(&self) -> bool {
        self.agent == upgrade::Agent::Claude
            && self.cause.is_empty()
            && match &self.phase {
                Phase::Announced { .. } => true,
                Phase::Failed(why) => why == upgrade::GAVE_UP,
                Phase::Pending | Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => {
                    false
                }
            }
    }

    /// Whether this phase ACTS ON a READY answer to the round's markers: an
    /// announced upgrade, and one that gave up asking but still hears a late
    /// answer ([`upgrade::next_step`]). No other phase does anything with one
    /// — and so no other phase may let one hold the release
    /// ([`upgrade::gate_release`], [`heard`]).
    fn hears_ready(&self) -> bool {
        match &self.phase {
            Phase::Announced { .. } => true,
            Phase::Failed(why) => why == upgrade::GAVE_UP,
            _ => false,
        }
    }

    /// The upgrade abandons the notice it gave the agent, for `why`: ONE
    /// release line is owed ([`St::release`]) — once per abandonment, the
    /// first reason kept — if a notice of this round reached it.
    pub(super) fn owe_release(&mut self, why: &str) {
        if self.release.is_empty() && self.notified() {
            self.release = why.to_string();
        }
    }

    /// The upgrade STOPS for `why` (a refusal, a failed signal or relaunch, a
    /// conversation resumed elsewhere): [`Phase::Failed`], the release the
    /// agent is owed if it was asked, and the round's markers FORGOTTEN — no
    /// stopped phase acts on a READY, and the release line says nothing will
    /// restart the session. Kept, they were the stranding the review of
    /// 2026-09-26 measured: a signal the kernel refused after READY, and a
    /// plan refused at the restart, left the READY standing as the agent's
    /// last word, which held the release (`wait:release:ready`) on every
    /// later visit — the agent that had answered neither restarted nor
    /// released.
    pub(super) fn stop(&mut self, why: &str) {
        self.owe_release(why);
        self.forget_markers();
        self.phase = Phase::Failed(why.to_string());
    }

    /// THE RESTART'S RECORD, written before its SIGTERM ([`restart`]): the
    /// agent `pid`, the `shell` its prompt comes back to in `tab`, the
    /// relaunch `line`, and [`Phase::Exiting`] at `now` — and what the
    /// restart consumes: the READY it acts on (the round's markers, no longer
    /// honoured once the agent is signalled) and the release a notice
    /// abandoned before this one (the carry-on says continue). Answers what
    /// was taken, for [`St::unsent`] when the signal is never sent.
    pub(super) fn signalled(
        &mut self,
        pid: u32,
        shell: u32,
        tab: &str,
        line: String,
        now: u64,
    ) -> Unsent {
        let back = Unsent {
            phase: std::mem::replace(&mut self.phase, Phase::Exiting { at_s: now }),
            release: std::mem::take(&mut self.release),
            marker: std::mem::take(&mut self.marker),
            markers: std::mem::take(&mut self.markers),
        };
        self.pid = pid;
        self.shell = shell;
        tab.clone_into(&mut self.tab);
        self.line = line;
        back
    }

    /// The signal of a restart [`St::signalled`] recorded was NEVER SENT (the
    /// session changed before it, the server refused it): the phase, the
    /// READY and the release it took are put back, as they were.
    pub(super) fn unsent(&mut self, back: Unsent) {
        self.phase = back.phase;
        self.release = back.release;
        self.marker = back.marker;
        self.markers = back.markers;
    }

    /// The kernel REFUSED the signal of a restart [`St::signalled`] recorded:
    /// the agent answered READY and lives on, stopped — the upgrade stops
    /// (`signal-refused`, [`St::stop`]) and the agent is owed its release.
    pub(super) fn signal_failed(&mut self, back: Unsent) {
        self.unsent(back);
        self.stop("signal-refused");
    }

    /// [`Step::GiveUp`]: the upgrade stops ASKING — [`upgrade::GAVE_UP`],
    /// the agent released, and its markers still honoured until the release
    /// line is typed (a READY answer that comes first restarts it). The
    /// owner's `--now` is spent with the round it asked to move (the second
    /// review of 2026-09-26): kept, it waived the settling window and the
    /// attended-tab guard for a late READY hours later, and SIGTERMed an
    /// agent a person had just typed to; a late READY restarts under the
    /// ordinary waits, and the owner's next `--now` re-arms the upgrade.
    fn give_up(&mut self) {
        self.owe_release("gave-up");
        self.phase = Phase::Failed(upgrade::GAVE_UP.to_string());
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
    }

    /// [`Step::Void`] at `now`: the READY answer a person, or the agent's own
    /// background work, held past the drain is void — the markers forgotten,
    /// so no answer given to them can end the agent later — and the agent
    /// released, with an announced upgrade's re-ask clock restarted, so the
    /// next notice comes [`upgrade::REASK_S`] after the release, not on its
    /// heels. A gave-up upgrade stays given up, its release still owed. A
    /// lane that types no release (Codex) keeps its clock: its re-ask is what
    /// moves the agent on.
    fn void(&mut self, now: u64) {
        self.owe_release("void");
        self.forget_markers();
        if let Phase::Announced { asks, .. } = self.phase
            && !self.release.is_empty()
        {
            self.phase = Phase::Announced { at_s: now, asks };
        }
    }

    /// The `asks`-th notice, READY marker `marker`, typed at `now`: it asks
    /// afresh, so it supersedes a release still owed. The round's earlier
    /// markers stay honoured beside it.
    fn announced(&mut self, marker: String, now: u64, asks: u32) {
        // A first notice opens its round: nothing earlier is its answer.
        if self.phase == Phase::Pending {
            self.markers.clear();
            self.asked.clear();
        }
        if !self.markers.contains(&marker) {
            self.markers.push(marker.clone());
        }
        if !self.asked.contains(&marker) {
            self.asked.push(marker.clone());
        }
        self.marker = marker;
        self.phase = Phase::Announced { at_s: now, asks };
        self.release.clear();
        self.ready_since = 0;
    }

    /// The release line was typed: nothing is owed, and the agent was told
    /// nothing will restart it — so no READY to this round's notices may.
    fn released(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    /// The release owed was DROPPED ([`release_void`]): nothing is owed, and
    /// the round is over as if the line had been typed — its markers
    /// forgotten.
    fn dropped(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    /// The upgrade's clocks HELD through a limit that stood until `until`
    /// ([`hold_clock`]): an announced upgrade's re-ask clock
    /// ([`upgrade::clock_held_until`]) and the READY clock
    /// ([`St::ready_since`]) start no sooner than then. Whether either moved.
    fn hold_clock(&mut self, until: u64) -> bool {
        let phase = upgrade::clock_held_until(&self.phase, until);
        let ready_since = if self.ready_since == 0 {
            0
        } else {
            self.ready_since.max(until)
        };
        let moved = phase != self.phase || ready_since != self.ready_since;
        self.phase = phase;
        self.ready_since = ready_since;
        moved
    }

    /// Forget every READY marker this round issued ([`answered`] then finds
    /// no answer, whatever the transcript holds).
    pub(super) fn forget_markers(&mut self) {
        self.marker.clear();
        self.markers.clear();
        self.ready_since = 0;
    }
}

pub(super) fn state_dir(opts: &Opts) -> PathBuf {
    opts.state.join("upgrade")
}

fn state_path(opts: &Opts, session: &str) -> PathBuf {
    state_dir(opts).join(format!("{session}.json"))
}

/// A SAME-BUILD restart (`from == to`: the build current, the priority
/// list's model behind) that has not begun, for a session whose model the
/// list no longer moves (a person's `/model`, a list set since): nothing is
/// owed, so its state goes. Kept, it would read PENDING to the owner's view
/// (`upgrade_status`) for as long as the conversation lives. A build upgrade
/// is never forgotten here; nor is an announced one, which the agent was
/// already asked about.
fn forget_unwanted_model_restart(
    opts: &Opts,
    session: &str,
    prior: Option<&St>,
    running: &Version,
) {
    let running = running.to_string();
    if opts.dry_run
        || !prior.is_some_and(|st| {
            st.phase == Phase::Pending
                && st.from == running
                && st.to == running
                && st.release.is_empty()
        })
    {
        return;
    }
    let _ = std::fs::remove_file(state_path(opts, session));
}

/// How long a settled upgrade's state outlives the last live Claude that held
/// its conversation ([`prune_states`]).
const STATE_KEEP_S: u64 = 7 * 24 * 60 * 60;

/// Remove the state of conversations nothing will visit again: no live
/// session file names the conversation, the file has not been written for
/// [`STATE_KEEP_S`], and the upgrade is SETTLED — done, failed, or pending
/// with no word of the owner's on it. A restart in flight is the orphan
/// pass's to finish, an announcement may still be answered, and an owner's
/// `--now`/`--defer`/`--skip` is theirs to see in `--status`: all three are
/// kept. A conversation resumed after the prune is asked afresh — which is
/// what a week-old state would ask anyway. Without it `<state>/upgrade/`
/// grew one file per conversation, for ever (gap audit 2026-09-24). Called
/// under the sweep lock, only with a session-file list that was READ: an
/// unreadable one proves nothing dead.
fn prune_states(opts: &Opts, files: &[SessionFile], now: u64) {
    if opts.dry_run {
        return;
    }
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return;
    };
    let live: std::collections::BTreeSet<&str> =
        files.iter().map(|f| f.session_id.as_str()).collect();
    for entry in dir.flatten() {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if live.contains(id) {
            continue;
        }
        let written = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok());
        if written.is_none_or(|at| now.saturating_sub(at.as_secs()) < STATE_KEEP_S) {
            continue;
        }
        let Some(st) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| St::from_json(&text))
        else {
            continue;
        };
        let settled = match st.phase {
            Phase::Done | Phase::Failed(_) => true,
            Phase::Pending => st.request == Request::None,
            _ => false,
        };
        if st.agent == upgrade::Agent::Claude && settled {
            let _ = std::fs::remove_file(&path);
        }
    }
}

pub(super) fn load(opts: &Opts, session: &str) -> Option<St> {
    St::from_json(&std::fs::read_to_string(state_path(opts, session)).ok()?)
}

pub(super) fn save(opts: &Opts, session: &str, st: &St) {
    if opts.dry_run {
        return;
    }
    let path = state_path(opts, session);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, st.to_json()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Put back what `session`'s record was before an act that did not go
/// ahead: `prior`, or no record at all.
pub(super) fn restore(opts: &Opts, session: &str, prior: Option<&St>) {
    if opts.dry_run {
        return;
    }
    match prior {
        Some(st) => save(opts, session, st),
        None => {
            let _ = std::fs::remove_file(state_path(opts, session));
        }
    }
}

/// One ledger line per step taken: `<state>/upgrade/ledger.jsonl`.
pub(super) fn ledger(opts: &Opts, r: &Report, detail: &str) {
    if opts.dry_run {
        return;
    }
    let mut o = Map::new();
    o.insert("t".into(), Value::from(now_s()));
    o.insert("pid".into(), Value::from(r.pid));
    for (k, v) in [
        ("tab", &r.tab),
        ("session", &r.session),
        ("from", &r.from),
        ("to", &r.to),
        ("step", &r.step),
    ] {
        o.insert(k.into(), Value::from(v.as_str()));
    }
    o.insert("detail".into(), Value::from(detail));
    let dir = state_dir(opts);
    let _ = std::fs::create_dir_all(&dir);
    let path = ledger_path(opts);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    }
    // Kept to its newest rows once it passes the bound, as the disk journal
    // is ([`super::cli::bound_ledger`]): every write is under the sweep lock,
    // so no other sweeper appends between the cut's read and its rename.
    let _ = super::cli::bound_ledger(&path, LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS);
}

/// `<state>/upgrade/ledger.jsonl` — what `aterm harness ledger upgrade` reads.
#[must_use]
pub fn ledger_path(opts: &Opts) -> PathBuf {
    state_dir(opts).join("ledger.jsonl")
}

/// The ledger's size past which it is cut back to its newest
/// [`LEDGER_KEEP_ROWS`] rows: the disk journal's bound
/// ([`super::cli::DISK_LEDGER_MAX_BYTES`]). Unbounded until 2026-09-24.
const LEDGER_MAX_BYTES: u64 = super::cli::DISK_LEDGER_MAX_BYTES;

/// How many rows survive a cut ([`LEDGER_MAX_BYTES`]).
const LEDGER_KEEP_ROWS: usize = super::cli::DISK_LEDGER_KEEP_ROWS;

// ---------------------------------------------------------------- claude files

#[cfg(test)]
thread_local! {
    /// Counts complete directory scans, not candidates, in one test thread.
    static SESSION_FILE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A partial roster cannot prove a conversation has only one live owner. A
/// sibling JSON may be mid-write, unreadable or malformed; any such entry
/// invalidates the whole scan until the next sweep.
pub(super) fn session_files(home: &Path) -> Option<Vec<SessionFile>> {
    #[cfg(test)]
    SESSION_FILE_SCANS.with(|count| count.set(count.get() + 1));
    let dir = std::fs::read_dir(home.join(".claude/sessions")).ok()?;
    let mut files = Vec::new();
    for entry in dir {
        let path = entry.ok()?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let file_pid = path.file_stem()?.to_str()?.parse::<u32>().ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        let file = upgrade::parse_session_file(&text).ok()?;
        if file.pid != file_pid {
            return None;
        }
        files.push(file);
    }
    Some(files)
}

/// The new Claude process creates a new `<pid>.json` in this directory. During
/// its launch, checking every session file every 250 ms reparses the whole
/// history even when the directory has not changed. Keep a complete successful
/// snapshot until the directory moves, and re-read it at least once a second:
/// an in-place write to an existing JSON file does not change the directory's
/// mtime. An unreadable or incomplete snapshot is retried on the next poll.
pub(super) const SESSION_ROSTER_RESCAN: Duration = Duration::from_secs(1);

/// [`session_files`] behind [`SESSION_ROSTER_RESCAN`]'s cache: what the
/// relaunch's wait for the new process reads (`relaunch::await_new`).
#[derive(Default)]
pub(super) struct SessionRosterCache {
    stamp: Option<SystemTime>,
    scanned_at: Option<Instant>,
    files: Option<Vec<SessionFile>>,
}

impl SessionRosterCache {
    pub(super) fn files(&mut self, home: &Path, now: Instant) -> Option<&[SessionFile]> {
        let stamp = std::fs::metadata(home.join(".claude/sessions"))
            .and_then(|m| m.modified())
            .ok();
        self.files_at(stamp, now, || session_files(home))
    }

    fn files_at(
        &mut self,
        stamp: Option<SystemTime>,
        now: Instant,
        scan: impl FnOnce() -> Option<Vec<SessionFile>>,
    ) -> Option<&[SessionFile]> {
        if self.files.is_none()
            || stamp.is_none()
            || stamp != self.stamp
            || self
                .scanned_at
                .is_none_or(|at| now.saturating_duration_since(at) >= SESSION_ROSTER_RESCAN)
        {
            // Record the stamp BEFORE scanning. A file created during the
            // scan then forces another read on the next poll.
            self.stamp = stamp;
            self.scanned_at = Some(now);
            self.files = scan();
        }
        self.files.as_deref()
    }
}

/// State is keyed by conversation, so a READY marker can be acted on only
/// when exactly one kernel-verified live process holds that conversation.
/// An unreadable kernel start is ambiguous and fails closed.
fn unique_live_owner(files: &[SessionFile], wanted: &SessionFile) -> bool {
    let mut owner = None;
    for sf in files.iter().filter(|sf| sf.session_id == wanted.session_id) {
        if !alive(sf.pid) {
            continue;
        }
        let Some(start) = kernel_start(sf.pid) else {
            return false;
        };
        if start != squash(&sf.proc_start) {
            continue;
        }
        if owner.replace(sf.pid).is_some() {
            return false;
        }
    }
    owner == Some(wanted.pid)
}

/// Preliminary owner check against the sweep's already-complete roster. It may
/// skip an irrelevant or ambiguous candidate, but it never authorizes an act:
/// those still re-read the whole directory at their own decision point.
fn preliminary_unique_owner(
    files: Option<&[SessionFile]>,
    wanted: &SessionFile,
) -> Result<(), &'static str> {
    let files = files.ok_or("session-files-unreadable")?;
    unique_live_owner(files, wanted)
        .then_some(())
        .ok_or("conversation-owner-ambiguous")
}

/// Fresh, complete directory read for every decision that can type, signal or
/// consume a notice. The initial sweep roster is only a candidate index.
pub(super) fn require_unique_owner(home: &Path, wanted: &SessionFile) -> Result<(), &'static str> {
    let files = session_files(home);
    preliminary_unique_owner(files.as_deref(), wanted)
}

pub(super) fn conversation_live(files: &[SessionFile], session: &str) -> bool {
    files
        .iter()
        .filter(|sf| sf.session_id == session)
        .any(|sf| {
            alive(sf.pid)
                && kernel_start(sf.pid).is_none_or(|start| start == squash(&sf.proc_start))
        })
}

/// The host prefilters with one cheap group lookup. Only a process whose
/// group uniquely matches a local PTY foreground group incurs focused `ps`
/// and argv reads. The later visit still applies the newer job-control and
/// terminal-owner guards; a group match alone never authorizes an act.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HostClaim {
    tab: String,
    group: i64,
    parent: u32,
}

fn host_candidates<'a>(
    files: &'a [SessionFile],
    live_tabs: &[LiveTab],
    mut needs_inspection: impl FnMut(&SessionFile, &str) -> bool,
    mut read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    mut read_group: impl FnMut(u32) -> Option<i64>,
    mut read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> Vec<(&'a SessionFile, atpkg::caller_shell::ProcArgs, HostClaim)> {
    if !live_tabs.iter().any(|tab| tab.fgpgid.is_some()) {
        return Vec::new();
    }
    files
        .iter()
        .filter(|sf| {
            sf.kind == "interactive"
                && sf.entrypoint == "cli"
                && upgrade::is_session_id(&sf.session_id)
        })
        .filter_map(|sf| {
            let group = read_group(sf.pid)?;
            let tab = unique_tab_for_group(live_tabs, group)?;
            if !needs_inspection(sf, tab) {
                return None;
            }
            let (parent, checked_group, tty_front) = read_ids(sf.pid)?;
            if checked_group != group || tty_front != group {
                return None;
            }
            let args = read_args(sf.pid)?;
            if args
                .env_var("ATERM_PARENT_SESSION_ID")
                .is_some_and(|env_tab| env_tab != tab)
            {
                return None;
            }
            Some((
                sf,
                args,
                HostClaim {
                    tab: tab.to_string(),
                    group,
                    parent,
                },
            ))
        })
        .collect()
}

/// The host need not inspect a process or build a whole process table when
/// every known target is already at or below the version Claude reported.
/// A native target remains a possibility until the process is examined, and
/// an in-flight restart must keep its recovery path even if its new process
/// already reports the target version. Unknown versions take the full path.
fn needs_upgrade_inspection(sf: &SessionFile, targets: &Targets, prior: Option<&St>) -> bool {
    // A restart in flight, and a release owed to the agent whatever build it
    // runs now ([`release_visit`]).
    if prior.is_some_and(|st| st.in_flight() || !st.release.is_empty()) {
        return true;
    }
    let Some(running) = Version::parse(&sf.version) else {
        return true;
    };
    targets
        .managed
        .iter()
        .chain(targets.native.iter())
        .any(|target| target.version > running)
}

fn claim_still_live(c: &mut Client, pid: u32, claim: &HostClaim) -> bool {
    process_in_tab(c, pid, &claim.tab, Some(claim.group), Some(claim.parent))
}

pub(super) fn session_file_of(home: &Path, pid: u32) -> Option<SessionFile> {
    let t = std::fs::read_to_string(home.join(format!(".claude/sessions/{pid}.json"))).ok()?;
    upgrade::parse_session_file(&t).ok()
}

/// Whether any transcript of `session` exists (`None`: the projects
/// directory cannot be read, so nobody can say). Claude files a
/// conversation's transcript only once it has a message: a conversation
/// without one cannot be resumed (measured 2026-09-24, Claude Code 2.1.281:
/// `--resume` of an agent killed before its first turn answers "No
/// conversation found with session ID" and exits).
pub(super) fn transcript_exists(home: &Path, session: &str) -> Option<bool> {
    let name = format!("{session}.jsonl");
    let dirs = std::fs::read_dir(home.join(".claude/projects")).ok()?;
    Some(dirs.flatten().any(|e| e.path().join(&name).is_file()))
}

/// How much of a transcript's end is read for the READY answer and the model
/// the agent ran.
pub(super) const TAIL_BYTES: u64 = 262_144;

/// The one transcript holding `session`.
pub(super) fn transcript(home: &Path, session: &str) -> Option<PathBuf> {
    let mut found = None;
    for e in std::fs::read_dir(home.join(".claude/projects"))
        .ok()?
        .flatten()
    {
        let p = e.path().join(format!("{session}.jsonl"));
        if p.is_file() {
            if found.is_some() {
                return None;
            }
            found = Some(p);
        }
    }
    found
}

/// The last `bytes` of `path`, decoded LOSSILY. The cut lands wherever `bytes`
/// says, and a cut inside a multi-byte character made a strict decode
/// (`read_to_string`) refuse the WHOLE window and leave it empty: a READY answer
/// on the last line read as no answer, for as long as the idle transcript kept
/// that length — re-asked after [`upgrade::REASK_S`], given up on after
/// [`upgrade::MAX_ASKS`] (measured 2026-09-23: 0.1-0.2% of the line-end lengths
/// of real transcripts, and most of a CJK-heavy one). Lossy spoils only the
/// already-cut first line (a U+FFFD that [`upgrade::transcript_has_ready`] skips
/// as not JSON), and a last line caught half-written while Claude appends.
///
/// Returned with the byte offset where the bytes read END — the mark a later
/// read starts from ([`since`]), taken from what was read rather than from a
/// length asked separately, so an append landing between the two is never
/// skipped by both.
pub(super) fn tail_to_end(path: &Path, bytes: u64) -> (String, u64) {
    let Ok(mut f) = std::fs::File::open(path) else {
        return (String::new(), 0);
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return (String::new(), 0);
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let end = start.saturating_add(buf.len() as u64);
    (lossy(buf), end)
}

/// At most `cap` bytes of `path` from byte `from` on, decoded lossily like
/// [`tail_to_end`]. Empty when the file is shorter than `from`: a transcript
/// that shrank was rewritten, and nothing in it can be told to come after the
/// mark.
pub(super) fn since(path: &Path, from: u64, cap: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    if f.metadata().map_or(0, |m| m.len()) < from || f.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(cap).read_to_end(&mut buf);
    lossy(buf)
}

fn lossy(buf: Vec<u8>) -> String {
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

// ---------------------------------------------------------------- the sweep

/// ONE SWEEP: every live Claude Code session advanced by at most one step (the
/// restart counts as one: exit, relaunch and continue, each bounded), then every
/// restart a previous sweep left in flight carried on.
///
/// ONE ACTOR AT A TIME, machine-wide: a hand-run `aterm harness upgrade`, the
/// window's host ([`step`]) and a relaunch on exit
/// ([`super::relaunch::after_exit`]) share `<state>/upgrade/sweep.lock`, and
/// one that finds it held reports one `busy` line and does nothing — two
/// would each read `pending` and type the notice twice.
#[must_use]
pub fn sweep(opts: &Opts) -> Vec<Report> {
    sweep_with_roster(opts, None)
}

/// THE WINDOW'S STEP for one tab (`opts.only_sid`, on the instance
/// `opts.sock` names): the Claude Code that is the tab's foreground job — or
/// the restart of one an earlier step left in flight — advanced ONE step, at
/// an idle point its supervisor parked at (the host's worker, which calls this
/// when [`due`] says so). Every proof the sweep makes is made here: the
/// instance's own PTY roster names the tab, uniquely, and every act re-reads
/// it. `current` when there is nothing to do.
#[must_use]
pub fn step(opts: &Opts) -> Report {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let tabs = connect(opts, &tab)
        .ok()
        .and_then(|mut c| host_roster(&mut c));
    let current = || Report {
        pid: 0,
        tab: tab.clone(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: "current".to_string(),
    };
    let Some(tabs) = tabs.filter(|tabs| tab_is_live(tabs, &tab)) else {
        return Report {
            step: "wait:tab-not-live".to_string(),
            ..current()
        };
    };
    let reports = sweep_with_roster(opts, Some(&tabs));
    pick(&reports, &tab).cloned().unwrap_or_else(current)
}

/// The one word [`step`] answers with, from every report its pass made: THE
/// TAB'S OWN first — its act, else its wait — then any act, then any wait.
/// A Codex tab's pass also reports its daemon (`tab=-`), and the daemon's
/// act must never stand for the client's wait (a `daemon-updated` read as
/// the tab's last word would leave its client unmoved until the next
/// activation notice); a client that is current leaves the daemon's word.
fn pick<'a>(reports: &'a [Report], tab: &str) -> Option<&'a Report> {
    let own = |r: &&Report| r.tab == tab;
    reports
        .iter()
        .filter(own)
        .find(|r| r.is_act())
        .or_else(|| reports.iter().filter(own).find(|r| r.step != "current"))
        .or_else(|| reports.iter().find(|r| r.is_act()))
        .or_else(|| reports.iter().find(|r| r.step != "current"))
}

/// Whether tab `opts.only_sid` has an upgrade to take: its foreground Codex
/// runs an older managed build, or its foreground Claude Code runs an older
/// build or a model behind the priority list's best available one
/// ([`super::upgrade_models`], THE MODEL LADDER), or a restart of this tab
/// is in flight. Read-only, and cheap enough for the host to ask at a
/// worker's start and at every activation notice: one roster read, the
/// session files when needed, the installed builds' cached versions, the model list.
/// Anything that cannot be read is `false` — nothing is parked for a guess.
#[must_use]
pub fn due(opts: &Opts) -> bool {
    let Some(tab) = opts.only_sid.as_deref() else {
        return false;
    };
    if !in_flight(opts).is_empty() || confirming(opts) {
        return true;
    }
    let Some(tabs) = connect(opts, tab)
        .ok()
        .and_then(|mut c| host_roster(&mut c))
    else {
        return false;
    };
    // The Codex branch: a Codex TUI leads the tab, behind the managed Codex
    // or served by a daemon that is ([`codex::due`]).
    due_after_codex(opts, &tabs, codex::due(opts, &tabs))
}

/// The Codex lane owns a tab whose foreground leader is Codex, even when its
/// managed build is already current. Only an unknown or other foreground can
/// have a Claude session file that makes an upgrade due. Keep this seam apart
/// so the read-heavy Claude roster can be proved absent on both Codex answers.
fn due_after_codex(opts: &Opts, tabs: &[LiveTab], codex_due: Option<bool>) -> bool {
    if let Some(due) = codex_due {
        return due;
    }
    let Some(tab) = opts.only_sid.as_deref() else {
        return false;
    };
    let Some(files) = session_files(&opts.home) else {
        return false;
    };
    let targets = std::cell::OnceCell::new();
    let mctx = std::cell::OnceCell::new();
    !host_candidates(
        &files,
        tabs,
        |sf, t| {
            let tg = targets.get_or_init(|| Targets::read(&opts.home));
            t == tab
                && (needs_upgrade_inspection(sf, tg, load(opts, &sf.session_id).as_ref())
                    || model_wants_a_look(opts, sf, mctx.get_or_init(|| ModelCtx::read(opts, tg))))
        },
        atpkg::caller_shell::process_args,
        process_group,
        ids,
    )
    .is_empty()
}

/// What the window's host does after one [`step`] on a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// Park again at the session's next idle point: the step acted and the
    /// next one waits on the agent (it was announced to — its answer comes at
    /// the end of a turn).
    NextIdle,
    /// Look again after this pause: the step waited (the session busy, held,
    /// a person at it, not settled yet, its READY not in, another actor on
    /// the lock), or held the session back. Each consecutive wait pauses
    /// longer ([`LATER`]).
    Later(std::time::Duration),
    /// Nothing more for this build: the upgrade is done, the session is
    /// current, it was refused, failed or given up — on the record — or the
    /// owner skipped it.
    Finished,
    /// The step cannot run at all — its state directory cannot hold the lock
    /// — and says so ONCE; nothing more until the next activation notice.
    Broken,
}

/// The pauses before consecutive looks at a session whose step waited: the
/// first long enough for a session that just went idle to settle
/// ([`upgrade::QUIET_S`]), the last a ten-minute look for as long as it
/// waits.
pub const LATER: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(upgrade::QUIET_S),
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(300),
    std::time::Duration::from_secs(600),
];

/// How soon a session whose step waited on a PERSON's hold — a box nobody
/// answered, a draft nobody sent ([`upgrade::person_hold`]) — is looked at
/// again, however long it has waited: often enough that the hold's dwell is
/// measured ([`upgrade::hold_since`]), so a READY answer it holds past
/// [`upgrade::DRAIN_S`] is voided rather than acted on when the person lets
/// go hours later.
pub const HOLD_LOOK: std::time::Duration = std::time::Duration::from_secs(60);

/// A look that runs a little late never begins a person's hold again: the gap
/// the drain's dwell tolerates is at least two looks.
const _: () = assert!(upgrade::HOLD_GAP_S >= 2 * HOLD_LOOK.as_secs());

/// [`After`] from one [`step`]'s word, `waits` the session's waits before
/// this one in a row.
#[must_use]
pub fn after(step: &str, waits: u32) -> After {
    let later = || {
        After::Later(
            crate::supervise::ladder::Ladder(&LATER)
                .step(usize::try_from(waits).unwrap_or(usize::MAX)),
        )
    };
    match step.split_once(':').map_or(step, |(head, _)| head) {
        // `continued`: the resumed session's answer, the model it came back
        // on, is read at the end of its turn.
        // `adopted`: the relaunched agent is its loop's, and the
        // continuation is typed at the loop's next idle point.
        "announced" | "continued" | "adopted" => After::NextIdle,
        // `gave-up`, `drain-expired`: the upgrade stopped asking, or voided a
        // READY answer, and the agent is owed its release — and an upgrade that
        // gave up still honours a READY to one of its notices, which comes at
        // the end of a turn. Both are the next idle point's, never the last
        // word (the 2026-09-26 incident: after `gave-up` the host never looked
        // again, and the READY given at 06:02 was read by nobody).
        "gave-up" | "drain-expired" => After::NextIdle,
        // `released`: the line is typed; a voided upgrade asks again later, and
        // one that gave up is looked at once more, to say its last word.
        "released" => later(),
        // The owner's `--skip` holds it for this build: nothing more until a
        // newer one is installed, or the owner's next word (both wake the
        // host). A `--defer` runs out on its own, so it is looked at again.
        _ if step == "wait:skipped" => After::Finished,
        // A STOPPED upgrade with nothing owed: `wait:failed` is said only when
        // no release is (an owed one is the step's next act, and waits as
        // `wait:release:<why>`, [`release_is_next`]) and no READY can move it
        // (a gave-up upgrade still hearing one owes its release until the line
        // forgets the markers). Nothing more for this build — the upgrade's
        // LAST WORD, which the derived model's `Look` is bound to
        // (`harness_upgrade_never_strands_model`): it is said only where the
        // agent is not left holding. (The Codex lane words a stopped record
        // `wait:failed:<why>` before its reducer, and is looked at again.)
        _ if step == "wait:failed" => After::Finished,
        "busy" if matches!(step, "busy:state-unwritable" | "busy:lock-unopenable") => After::Broken,
        // A person's hold: its dwell is sampled ([`HOLD_LOOK`]).
        _ if matches!(step, "wait:box" | "wait:draft") => After::Later(HOLD_LOOK),
        // `left-typed-cleared`: the Codex branch cleared its own text a
        // guarded Enter left in the composer; the act it missed is looked at
        // again, like any wait.
        "wait" | "busy" | "held-back" | "left-typed-cleared" => later(),
        _ => After::Finished,
    }
}

/// THE UPGRADE'S CLOCKS ARE HELD THROUGH A LIMIT EPISODE its window's loop
/// saw (the review of 2026-09-26): the upgrade records of tab
/// `opts.only_sid` have their re-ask and READY clocks start no sooner than
/// `until`, the unix second the loop last knew the session at its limit
/// ([`St::hold_clock`]). The window's host calls it as the loop's episode
/// opens and closes (`IdleHost::limited`). The upgrade's own look holds the
/// clock only at a look that FINDS the limit ([`upgrade::clock_held`]), and
/// the host takes no step during an episode — so, hosted, a notice whose
/// wind-down turn hit the weekly limit found its window long run out at the
/// first idle point after the reset, and was asked again at once, or given
/// up on and released, before the agent could answer. Files only, under the
/// sweep lock; `false` while another sweep holds it (the host tries again
/// before its next step), `true` once applied or when there is nothing to
/// apply.
#[must_use]
pub fn hold_clock(opts: &Opts, until: u64) -> bool {
    let Some(tab) = opts.only_sid.as_deref() else {
        return true;
    };
    let _held = match sweep_lock(opts) {
        Ok(held) => held,
        Err("another-sweep") => return false,
        Err(_) => return true,
    };
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return true;
    };
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if let Some(mut st) = load(opts, &session).filter(|st| st.tab == tab)
            && st.hold_clock(until)
        {
            save(opts, &session, &st);
        }
    }
    true
}

/// How many looks in a row the upgrade may wait for background work under
/// an agent that answered READY before it stops owning the session's turn
/// ends ([`owns_turn_ends`]): the drain is bounded — past it the worker is
/// continued, and the upgrade asks again at a later idle point (the
/// philosophy review of 2026-09-25: a `run_in_background` server held a
/// READY agent idle for ever).
pub const OWNED_BACKGROUND_LOOKS: u32 = 3;

/// How many quick looks in a row (the upgrade's own gate settling: the
/// screen quiet [`upgrade::QUIET_S`]) the upgrade may own the session's turn
/// ends for.
pub const OWNED_SETTLE_LOOKS: u32 = 2;

/// Whether the upgrade OWNS THE SESSION'S TURN ENDS after the step that said
/// `step` (its `waits` in a row before it) — what the window's host answers
/// its supervisor's loop with (`IdleHost::owns_turn_end`), so the turn-end
/// policy types nothing over a wind-down: an announcement out (the agent is
/// asked to finish up), and while READY is given and the restart's gate
/// only waits — its quick settle for [`OWNED_SETTLE_LOOKS`] looks, a drain
/// of background work for [`OWNED_BACKGROUND_LOOKS`]. An agent that answered
/// without READY (`awaiting-ready`), a person's hold, a release still owed
/// (`wait:release:<why>`: the agent is to go on, and the supervisor may say
/// so first), and every last word (done, failed, gave up, refused) own
/// nothing. It lives beside [`after`], the host's other reading of the same
/// word, because a step's word is this module's to say: the review of
/// 2026-09-26 found a late READY about to restart the agent reported as
/// `wait:release:ready`, which owned nothing, while its restart's settle
/// (`wait:settling`) owns the turn ends.
#[must_use]
pub fn owns_turn_ends(step: &str, waits: u32) -> bool {
    match step.split_once(':').map_or(step, |(head, _)| head) {
        "announced" => true,
        "wait" => match step {
            "wait:settling" | "wait:not-idle" | "wait:busy" => waits < OWNED_SETTLE_LOOKS,
            // A Codex that answered READY waits out its background terminal
            // as Claude Code's restart waits out a shell under it.
            "wait:background" | "wait:background-terminal" => waits < OWNED_BACKGROUND_LOOKS,
            _ => false,
        },
        _ => false,
    }
}

/// A window supplies its own live PTY roster; an unscoped CLI keeps its
/// report-only wait when macOS omits another process's environment.
fn sweep_with_roster(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => {
            return vec![Report {
                pid: 0,
                tab: "-".to_string(),
                session: "-".to_string(),
                from: "-".to_string(),
                to: "-".to_string(),
                step: format!("busy:{why}"),
            }];
        }
    };
    // Files only, and whatever else this sweep finds: first — never at a
    // break of the agent's background work, where only a notice is typed.
    let mut reports = if opts.background {
        Vec::new()
    } else {
        confirmations(opts)
    };
    // THE CODEX BRANCH, under the same lock: every Codex home's daemon, then
    // every Codex TUI in a tab, then the Codex restarts left in flight
    // ([`codex::sweep`]); for the window's step, the tab's own TUI and the
    // daemon it runs on. It needs no Claude session file.
    let codex = codex::sweep(opts, live_tabs);
    // A tab whose foreground job is a Codex is the Codex branch's alone: the
    // Claude pass has nothing there, and its machine-wide waits (no
    // `~/.claude` at all on a Codex-only machine) are no wait of that tab's.
    let codex_tab = opts
        .only_sid
        .as_ref()
        .is_some_and(|sid| live_tabs.is_some() && codex.iter().any(|r| r.tab == *sid));
    if !codex_tab {
        reports.extend(claude_pass(opts, live_tabs));
    }
    reports.extend(codex);
    reports
}

/// A Codex restart in flight carried on from where it stopped
/// ([`codex::carry_in_flight`]): the relaunch primitive's continuation step
/// ([`super::relaunch::resume`]) for a Codex record.
pub(super) fn codex_carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    codex::carry_in_flight(opts, r, st, session)
}

/// The Claude lane's share of one sweep: every live Claude Code session
/// advanced one step, then its restarts left in flight carried on.
fn claude_pass(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    if live_tabs.is_some_and(<[LiveTab]>::is_empty) {
        return Vec::new();
    }
    let Some(files) = session_files(&opts.home) else {
        return vec![Report {
            pid: 0,
            tab: "-".to_string(),
            session: "-".to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: "wait:session-files-unreadable".to_string(),
        }];
    };
    // Bookkeeping under the lock — never at a break of an agent's own
    // background work, where only a notice is typed.
    if !opts.background {
        prune_states(opts, &files, now_s());
    }
    let mut reports = Vec::new();
    reports.extend(if let Some(tabs) = live_tabs {
        // Read installed versions only after a cheap PTY-group match. A window
        // with no Claude job does not execute a managed twin just to ask its
        // version; a matched but current job stops before `ps` and argv reads.
        let targets = std::cell::OnceCell::new();
        let mctx = std::cell::OnceCell::new();
        let candidates = host_candidates(
            &files,
            tabs,
            |sf, tab| {
                let tg = targets.get_or_init(|| Targets::read(&opts.home));
                opts.only_sid.as_deref().is_none_or(|only| only == tab)
                    && (needs_upgrade_inspection(sf, tg, load(opts, &sf.session_id).as_ref())
                        || model_wants_a_look(
                            opts,
                            sf,
                            mctx.get_or_init(|| ModelCtx::read(opts, tg)),
                        ))
            },
            atpkg::caller_shell::process_args,
            process_group,
            ids,
        );
        if candidates.is_empty() {
            Vec::new()
        } else {
            let t = table();
            let targets = targets.get_or_init(|| Targets::read(&opts.home));
            let mctx = mctx.get_or_init(|| ModelCtx::read(opts, targets));
            candidates
                .into_iter()
                .map(|(sf, args, claim)| {
                    visit_models(
                        opts,
                        sf,
                        Some(&files),
                        &t,
                        targets,
                        &Live,
                        Some(&args),
                        Some(&claim),
                        mctx,
                    )
                })
                .collect()
        }
    } else if files.is_empty() {
        Vec::new()
    } else {
        let t = table();
        let targets = Targets::read(&opts.home);
        let mctx = ModelCtx::read(opts, &targets);
        files
            .iter()
            .map(|sf| {
                visit_models(
                    opts,
                    sf,
                    Some(&files),
                    &t,
                    &targets,
                    &Live,
                    None,
                    None,
                    &mctx,
                )
            })
            .collect()
    });
    if !opts.background {
        reports.extend(orphans(opts, &files, live_tabs));
    }
    reports
}

/// Take `<state>/upgrade/sweep.lock` without waiting. `Ok(None)` for a dry run,
/// which acts on nothing and so needs no exclusion.
///
/// ONE TRY DECIDES, because the guard is released by `LOCK_UN` ([`SweepLock`]) —
/// the same release as atpkg's store lock and index-probe range locks
/// (`atpkg::lock::Flock`). An `flock(2)` lives on the open file description, and
/// while ANY thread of this process is mid-spawn the child holds a copy of every
/// descriptor until its exec — so a lock released only by the close still reads as
/// held for that long. `a_second_sweeper_does_nothing_while_the_first_holds_the_lock`
/// failed a landing gate that way (2026-09-24: after the first guard dropped, the
/// next sweep still read `busy:another-sweep`; alone it passed 5 of 5). A 100 ms
/// poll over the try (upstream 80a588e78) only waited out a spawn faster than 100 ms;
/// `LOCK_UN` strips the lock from the description itself, every inherited copy
/// included, so nothing is left to wait out and a lock that reads held is a sweep's.
pub(super) fn sweep_lock(opts: &Opts) -> Result<Option<SweepLock>, &'static str> {
    if opts.dry_run {
        return Ok(None);
    }
    let dir = state_dir(opts);
    std::fs::create_dir_all(&dir).map_err(|_| "state-unwritable")?;
    let lock: std::fs::File = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("sweep.lock"))
        .map_err(|_| "lock-unopenable")?;
    match lock.try_lock() {
        Ok(()) => Ok(Some(SweepLock(lock))),
        Err(_) => Err("another-sweep"),
    }
}

/// A held [`sweep_lock`], released by `LOCK_UN` when it drops rather than by the
/// close: the next sweep finds it free the moment this one ends, whatever child
/// another thread is spawning (pinned by
/// `a_dropped_sweep_lock_is_free_while_a_copy_of_its_descriptor_lives`).
pub(super) struct SweepLock(std::fs::File);

impl Drop for SweepLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether a restart of the tab `opts.only_sid` still owes the model its
/// resumed session came back on ([`St::confirming`]).
fn confirming(opts: &Opts) -> bool {
    std::fs::read_dir(state_dir(opts)).is_ok_and(|dir| {
        dir.flatten().any(|e| {
            let path = e.path();
            path.extension().is_some_and(|x| x == "json")
                && std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| St::from_json(&text))
                    .is_some_and(|st| {
                        st.confirming() && opts.only_sid.as_ref().is_none_or(|s| *s == st.tab)
                    })
        })
    })
}

/// The conversations an earlier sweep left mid-restart — the agent
/// signalled, the new process not yet relaunched or told to carry on — that a
/// sweep told `opts` would resume ([`orphans`]' filter, without its liveness
/// checks). A sweep that stands down strands them, and says which.
#[must_use]
pub fn in_flight(opts: &Opts) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = dir
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                return None;
            }
            let session = path.file_stem()?.to_string_lossy().into_owned();
            let st = load(opts, &session).filter(St::in_flight)?;
            let ours = opts.only_sid.as_ref().is_none_or(|s| *s == st.tab);
            ours.then_some(session)
        })
        .collect();
    out.sort();
    out
}

fn blank(sf: &SessionFile) -> Report {
    Report {
        pid: sf.pid,
        tab: "-".to_string(),
        session: sf.session_id.clone(),
        from: sf.version.clone(),
        to: "-".to_string(),
        step: String::new(),
    }
}

pub(super) fn first_word(s: &str) -> String {
    s.split_whitespace().take(2).collect::<Vec<_>>().join("-")
}

pub(super) fn said(mut r: Report, step: impl Into<String>) -> Report {
    r.step = step.into();
    r
}

#[cfg(all(test, unix))]
fn visit(
    opts: &Opts,
    sf: &SessionFile,
    t: &[(u32, u32, String)],
    targets: &Targets,
    k: &dyn Kernel,
) -> Report {
    let files = session_files(&opts.home);
    visit_with_claim(opts, sf, files.as_deref(), t, targets, k, None, None)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn visit_with_claim(
    opts: &Opts,
    sf: &SessionFile,
    initial_files: Option<&[SessionFile]>,
    t: &[(u32, u32, String)],
    targets: &Targets,
    k: &dyn Kernel,
    prefetched_args: Option<&atpkg::caller_shell::ProcArgs>,
    host_claim: Option<&HostClaim>,
) -> Report {
    visit_models(
        opts,
        sf,
        initial_files,
        t,
        targets,
        k,
        prefetched_args,
        host_claim,
        &ModelCtx::none(),
    )
}

/// One session advanced one step, with the pass's model half
/// ([`ModelCtx`]): a session whose model is behind the priority list
/// restarts when THE MODEL LADDER ([`models::model_moves_now`]) says the
/// move is due now — onto a newer build when there is one, else onto the
/// SAME build — with the list's model on the relaunch line (`--model`,
/// session-only; never Claude's `/model`, which also saves the person's
/// default).
#[allow(clippy::too_many_arguments)]
fn visit_models(
    opts: &Opts,
    sf: &SessionFile,
    initial_files: Option<&[SessionFile]>,
    t: &[(u32, u32, String)],
    targets: &Targets,
    k: &dyn Kernel,
    prefetched_args: Option<&atpkg::caller_shell::ProcArgs>,
    host_claim: Option<&HostClaim>,
    mctx: &ModelCtx,
) -> Report {
    let mut r = blank(sf);
    // The session file names THIS process, not a recycled pid.
    if !alive(sf.pid) || kernel_start(sf.pid).as_deref() != Some(squash(&sf.proc_start).as_str()) {
        return said(r, "wait:stale-file");
    }
    if sf.kind != "interactive" || sf.entrypoint != "cli" || !upgrade::is_session_id(&sf.session_id)
    {
        return said(r, "wait:not-an-interactive-cli");
    }
    let fetched_args;
    let args = if let Some(args) = prefetched_args {
        args
    } else {
        let Some(args) = atpkg::caller_shell::process_args(sf.pid) else {
            return said(r, "wait:argv-unreadable");
        };
        fetched_args = args;
        &fetched_args
    };
    let Some(tab) = host_claim
        .map(|claim| claim.tab.clone())
        .or_else(|| args.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned))
    else {
        // macOS may yield argv without env for another process. The unscoped
        // CLI has no PTY roster claim and must wait rather than guess a tab.
        return said(r, "wait:no-aterm-tab");
    };
    if args
        .env_var("ATERM_PARENT_SESSION_ID")
        .is_some_and(|env_tab| env_tab != tab)
    {
        return said(r, "wait:tab-identity-conflict");
    }
    r.tab.clone_from(&tab);
    if opts.only_sid.as_ref().is_some_and(|s| *s != tab) {
        return said(r, "skip:not-selected");
    }
    if let Err(why) = preliminary_unique_owner(initial_files, sf) {
        return said(r, format!("wait:{why}"));
    }
    let prior = load(opts, &sf.session_id);
    if prior.as_ref().is_some_and(|st| {
        matches!(st.phase, Phase::Announced { .. }) && !st.notice_belongs_to(sf, &tab)
    }) {
        return said(r, "wait:notice-owned-by-other-process");
    }
    if let Some(mut st) = prior.clone().filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != tab {
            return said(r, "wait:conversation-in-other-tab");
        }
        if sf.pid == st.pid {
            return said(r, "wait:exiting");
        }
        // A NEW process holds the conversation. It is the relaunch only when
        // the shell the line was typed at started it; resumed by hand anywhere
        // else, it is not this upgrade's to tell that it was upgraded. The
        // failure is PERMANENT, so it is recorded only from a parent actually
        // read: one that cannot be read now is a wait, and the next sweep asks
        // again.
        match relaunch_of(k, sf, &sf.session_id, st.pid, st.shell) {
            Relaunch::Ours => {}
            Relaunch::Unread => return said(r, "wait:ids"),
            Relaunch::NotOurs if opts.dry_run => return said(r, "would-fail:resumed-elsewhere"),
            Relaunch::NotOurs => {
                st.stop("resumed-elsewhere");
                let r = said(r, "failed:resumed-elsewhere");
                ledger(
                    opts,
                    &r,
                    "a process this upgrade did not relaunch holds the conversation",
                );
                save(opts, &sf.session_id, &st);
                return r;
            }
        }
        // The relaunch landed.
        if opts.dry_run {
            return said(r, "would-continue");
        }
        let Ok(mut c) = connect(opts, &tab) else {
            return said(r, "wait:no-socket");
        };
        if !roster(&mut c).contains(&tab) {
            return said(r, "wait:tab-not-live");
        }
        if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
            return said(r, "wait:tab-ownership-changed");
        }
        return carry_on(opts, r, &mut st, &mut c, &sf.session_id.clone(), sf);
    }
    let Some(running) = Version::parse(&sf.version) else {
        return said(r, "wait:version-unreadable");
    };
    let exe = exe_of(sf.pid).unwrap_or_default();
    let running_native = exe.starts_with(native_root(&opts.home));
    // THE MODEL HALF: what the conversation runs now against the list's best
    // available model. A change PREFERS a cold cache (a switch re-reads the
    // whole history, which a warm cache would have spared) but never waits on
    // one without bound: see `models::model_moves_now`, THE MODEL LADDER.
    let launch = upgrade::launch_model(&args.argv);
    let mread = model_read(opts, sf, launch.as_deref(), mctx);
    // Whether a newer BUILD is due regardless of the model — decided before the
    // model, because a restart already happening is the moment the model rides.
    let build_target = upgrade::choose_target(&running, &targets.for_session(running_native));
    // Once announced, the model rides the upgrade's own state: the READY
    // answer warms the cache again, and a decision re-taken then would drop
    // the model half-way.
    let announced_model = prior
        .as_ref()
        .filter(|st| matches!(st.phase, Phase::Announced { .. }) && !st.model_list.is_empty())
        .map(|st| st.model_list.clone());
    let model_to = announced_model.or_else(|| match &mread.verdict {
        ModelVerdict::Due { to, .. }
            if models::model_moves_now(mread.cold, build_target.is_some(), mread.due_for_s)
                .is_some() =>
        {
            Some(to.clone())
        }
        _ => None,
    });
    // A newer build, or — the build current, the model behind — the SAME build
    // again, relaunched with `--model`: a command-line model is session-only,
    // where Claude's own `/model <id>` also rewrites the person's default for
    // every new session (2.1.282, READ: its handler persists whenever the
    // session is interactive; MEASURED: "saved as your default for new
    // sessions").
    let target = build_target.or_else(|| {
        model_to
            .as_ref()
            .and_then(|_| same_build(targets, &exe, &running, running_native))
    });
    let Some(target) = target else {
        // Nothing to move, and still a release owed: the conversation runs
        // the build its upgrade stopped short of (resumed by hand on it after
        // a relaunch that failed), and the agent in it was asked to wind down.
        if let Some(st) = prior.clone().filter(|st| !st.release.is_empty()) {
            return release_visit(opts, r, sf, st, &tab, t, k, host_claim);
        }
        return match &mread.verdict {
            ModelVerdict::Due { .. } => said(r, "wait:model-cache-warm"),
            ModelVerdict::Keep(_) => {
                forget_unwanted_model_restart(opts, &sf.session_id, prior.as_ref(), &running);
                said(r, "current")
            }
        };
    };
    r.to = format!("{}({})", target.version, target.source.as_str());
    // The restart before this one has not said its model yet: a new upgrade
    // would start from a fresh state and lose that outcome's `done` row.
    if prior.as_ref().is_some_and(St::confirming) {
        return said(r, "wait:confirming");
    }
    let now = now_s();
    let mut st = St::for_target(prior, &running, &target, model_to.as_deref(), now);
    // The tab a PENDING upgrade waits in: what the owner names (`aterm
    // harness upgrade <sid> --now`) and what the window shows it under. From
    // the announcement on, `tab` is the notice's fence and is set there.
    if st.phase == Phase::Pending {
        st.tab.clone_from(&tab);
    }
    if matches!(st.request, Request::DeferUntil(t) if now >= t) {
        st.request = Request::None;
        st.request_tab.clear();
        st.request_at = 0;
    }
    // THE PROCESS PROOFS, before any socket is dialled and while the upgrade can
    // still act. An environment tab id, when readable, rides into tmux panes
    // unchanged; the host's PTY-group match is also only a first filter. The
    // job-control and terminal-owner checks prove typing reaches THIS agent.
    if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
        let job = k.job(sf.pid);
        if matches!(job, Some((Job::NoJobControl, _))) {
            let r = not_a_job(opts, r, &mut st);
            save(opts, &sf.session_id, &st);
            return r;
        }
        // Suspended or put in the background, the SHELL holds the terminal,
        // and nothing the screen shows is the agent's.
        if let Err(why) = foreground_shell(job) {
            st.note_wait(why, now);
            save(opts, &sf.session_id, &st);
            return said(r, format!("wait:{why}"));
        }
        let owner = k.terminal(sf.pid);
        let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
        if !owner.is_some_and(owned_by_aterm) {
            let why = format!("terminal:{}", owner_word(owner));
            st.note_wait(&why, now);
            let r = held_back(opts, r, &mut st, &sf.session_id, &why);
            save(opts, &sf.session_id, &st);
            return r;
        }
    }
    let (mut c, mut facts) = match look(opts, sf, &mut st, &tab, host_claim, t, now) {
        Ok(look) => look,
        Err(why) => return said(r, format!("wait:{why}")),
    };
    // The transcript's tail, read once an announcement is out, or a release
    // is owed: the READY answer is looked for in it, and at a restart it also
    // says which model the agent ran and where the resumed session's own
    // turns will start.
    let recent = if st.marker.is_empty() && st.markers.is_empty() && st.release.is_empty() {
        None
    } else {
        transcript(&opts.home, &sf.session_id).map(|p| tail_to_end(&p, TAIL_BYTES))
    };
    let tail = recent.as_ref().map(|(text, _)| text.as_str());
    let ready = heard(&st, sf, &tab, tail);
    // How long that READY has stood unacted on, held at the limit: what
    // bounds the agent's own background work under it.
    st.ready_since = upgrade::ready_since(st.ready_since, ready, &facts, now);
    if st.ready_since != 0 {
        facts.ready_s = now.saturating_sub(st.ready_since);
    }
    let request = st.request_for(&tab);
    let mut step = upgrade::requested_step(&request, &st.phase, &facts, ready, now, &st.to);
    // At a break of the agent's own background work the notice alone is
    // typed: nothing is ended there, whatever the plan says.
    if opts.background && !matches!(step, Step::Announce | Step::Wait(_)) {
        step = Step::Wait("background");
    }
    // The last read before anything is typed or signalled: the agent holds its
    // terminal. Suspended (ctrl-z) or put in the background since the first
    // look, the SHELL holds it, while the agent's last frame can still read as
    // an idle, empty composer. The shell this read proves is the one the
    // announcement's plan is made against.
    let mut proven = None;
    if matches!(step, Step::Announce | Step::Terminate) {
        match foreground_shell(k.job(sf.pid)) {
            Ok(shell) => proven = Some(shell),
            Err(why) => step = Step::Wait(why),
        }
    }
    let result = match step {
        Step::Wait("attended") => attended_once(opts, r, &mut st),
        Step::Wait(why) => said(r, format!("wait:{why}")),
        Step::GiveUp => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            st.give_up();
            let r = said(r, "gave-up");
            ledger(
                opts,
                &r,
                "no READY answer the restart could act on after the last announcement: the \
                 upgrade stops asking, releases the agent, and still honours a READY that comes \
                 before the release",
            );
            r
        }
        Step::Void(why) => drain_expired(opts, r, &mut st, why, &facts, now),
        Step::Announce => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            // The transcript's mode, not the screen's: the upgrade's own
            // exchanges with the host are fenced and counted, and a read here
            // would be one they never asked for.
            let mode = last_permission_mode(&opts.home, &sf.session_id);
            let planned = proven.map_or(Err(NoPlan::Wait("ids")), |shell| {
                plan(
                    opts,
                    &launch_of(
                        sf,
                        &args.argv,
                        &target,
                        model_to.as_deref(),
                        mode.as_deref(),
                    ),
                    shell,
                    t,
                )
            });
            let salt = st.salt;
            let result = announce(opts, r, &mut st, planned, now, |asks| {
                let marker = upgrade::ready_marker(
                    &sf.session_id,
                    &target.version,
                    salt.wrapping_add(u64::from(asks)),
                );
                let text = upgrade::prepare_prompt_with_model(
                    &running,
                    &target.version,
                    target.source,
                    &marker,
                    model_to.as_deref(),
                );
                require_unique_owner(&opts.home, sf).map_err(str::to_string)?;
                if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
                    return Err("tab-ownership-changed".to_string());
                }
                // The last look before typing: a FRESH read whose composer is
                // still empty and with no box up, and its generation, which
                // the typed turn is fenced on (`turn if-gen=`): a screen that
                // moved since that read (nothing typed) is a wait.
                let generation = typing_fence(&mut c, &tab).map_err(str::to_string)?;
                turn_fenced(&mut c, &tab, &text, generation.as_deref())
                    .map(|()| marker)
                    .map_err(|e| e.word())
            });
            if result.step.starts_with("announced:") {
                st.model_list = model_to.clone().unwrap_or_default();
                st.tab.clone_from(&tab);
                st.notice_pid = sf.pid;
                st.notice_start = squash(&sf.proc_start);
            }
            result
        }
        Step::Terminate if opts.dry_run => said(r, "would-restart"),
        Step::Terminate => {
            // What the agent ran, from the tail that just proved its READY
            // answer, and where that read ended: the resumed session's turns
            // are the ones past it. Recorded before the signal with the rest
            // of the relaunch; a restart that waits instead takes both again.
            (st.model_before, st.mark) = model_and_mark(recent.as_ref());
            st.launch_model = upgrade::launch_model(&args.argv).unwrap_or_default();
            // Under the owner's `--now` a person at the tab does not hold
            // the signal back: the word IS that person.
            let grace = if request == Request::Now {
                0
            } else {
                opts.human_grace_s
            };
            restart(
                opts, r, &mut st, &mut c, &tab, sf, &args.argv, &target, t, k, host_claim, grace,
            )
        }
    };
    // THE RELEASE OWED (F3): an upgrade that abandoned the notice it gave the
    // agent types one line telling it to go on — at this point if it is the
    // upgrade's next act and may be typed here, else at the next one
    // ([`release`]). Never at a break of the agent's own background work:
    // that is the notice's alone. The READY it waits behind is read again
    // off the record as the step left it: a stop in this very step forgot
    // its markers, and a READY nothing acts on holds nothing.
    let result =
        if release_is_next(&step, &result.step) && !st.release.is_empty() && !opts.background {
            let ready = heard(&st, sf, &tab, tail);
            release(
                opts, result, &mut st, &mut c, &tab, sf, host_claim, k, &facts, ready, tail,
            )
        } else {
            result
        };
    st.note_step(&result.step, &facts.status, now);
    save(opts, &sf.session_id, &st);
    result
}

/// ONE LOOK AT THE SESSION'S TAB, everything the gates are asked over: the
/// tab dialled and proven live (and, for the window's step, still the PTY
/// group's claim), its screen read once, and the [`Facts`] of that read — the
/// person stamp, the hold, the composer, a box, the busy footer, THE LIMIT
/// ([`upgrade::limited`], off the same rows), the work under the agent — with
/// the screen's quiet and a person's hold timed across looks on `st`, and an
/// announced upgrade's re-ask clock held while the session is limited
/// ([`upgrade::clock_held`]). `Err` names the wait.
fn look(
    opts: &Opts,
    sf: &SessionFile,
    st: &mut St,
    tab: &str,
    host_claim: Option<&HostClaim>,
    t: &[(u32, u32, String)],
    now: u64,
) -> Result<(Client, Facts), &'static str> {
    let mut c = connect(opts, tab).map_err(|_| "no-socket")?;
    if !roster(&mut c).iter().any(|live| live == tab) {
        return Err("tab-not-live");
    }
    if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
        return Err("tab-ownership-changed");
    }
    let scr = screen(&mut c, tab).ok_or("screen-unreadable")?;
    // A person at the tab: the server's per-session person stamp on this
    // very read, within `[harness] human_grace_s` — the one presence fact,
    // the same a hand-run sweep and the window's supervisor read
    // ([`upgrade::attended_by`]).
    let attended = upgrade::attended_by(scr.human, opts.human_grace_s);
    if scr.seq != st.last_seq {
        st.last_seq = scr.seq;
        st.seq_since_s = now;
    }
    let mut facts = Facts {
        status: sf.status.clone(),
        status_age_s: now.saturating_sub(sf.status_updated_at_ms / 1000),
        composer_empty: composer_empty(&mut c, tab, &scr),
        approval_box: aterm_phase::prompt::prompt_box_span(&scr.rows).is_some(),
        busy_footer: {
            let busy = aterm_phase::anchors::anchor("busy.interrupt");
            scr.rows.iter().any(|row| row.contains(busy))
        },
        background: background(sf.pid, t),
        // The hold and the hands alone: a person's keystroke is `attended`,
        // which the owner's `--now` waives.
        held: held(&mut c, tab, 0),
        quiet_s: now.saturating_sub(st.seq_since_s),
        hold_s: 0,
        owner_now: false,
        attended,
        background_point: opts.background,
        limited: upgrade::limited(upgrade::Agent::Claude, &scr.rows),
        ready_s: 0,
    };
    (st.hold_since_s, st.hold_seen_s) =
        upgrade::hold_since((st.hold_since_s, st.hold_seen_s), &facts, now);
    if st.hold_since_s != 0 {
        facts.hold_s = now.saturating_sub(st.hold_since_s);
    }
    st.phase = upgrade::clock_held(&st.phase, &facts, now);
    Ok((c, facts))
}

/// THE RELEASE, typed if it may be now ([`upgrade::gate_release`]): ONE line
/// ([`upgrade::release_prompt`]) into the session of an upgrade that abandoned
/// the notice it gave the agent ([`St::release`]), under every fence the notice
/// is typed under — the agent its shell's foreground job on a terminal the tab
/// owns, the conversation's one live owner, the tab still the PTY group's
/// claim, a fresh read with no box and no draft whose generation the turn is
/// fenced on. Typed: nothing is owed and the round's markers are forgotten
/// ([`St::released`]), said once in the ledger (`released:<why>`). Not typed:
/// still owed, and `r` is kept when it is an act of this step the host looks
/// on after (`gave-up`, `drain-expired:…`), else the wait says what holds the
/// release (`wait:release:<why>`) — never a word the host reads as the last
/// one ([`owed_word`]). Called only where the release is the upgrade's next
/// act ([`release_is_next`]). A release that is no longer this upgrade's to
/// type ([`release_void`]: the process the notice reached no longer holds
/// the conversation here, or the agent took up direction given after its
/// last answer to the upgrade) is dropped instead, said once
/// ([`drop_release`]), and `r` kept.
#[allow(clippy::too_many_arguments)]
fn release(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    host_claim: Option<&HostClaim>,
    k: &dyn Kernel,
    f: &Facts,
    ready: bool,
    tail: Option<&str>,
) -> Report {
    let owed = owed_word;
    if let Some(why) = release_void(st, sf, tab, tail) {
        drop_release(opts, &r, st, why);
        return r;
    }
    if let upgrade::Gate::Wait(why) = upgrade::gate_release(f, ready) {
        return owed(r, why);
    }
    if let Err(why) = foreground_shell(k.job(sf.pid)) {
        return owed(r, why);
    }
    let owner = k.terminal(sf.pid);
    if !owner
        .as_ref()
        .is_some_and(|(pid, name)| owned_by_aterm((*pid, name.as_str())))
    {
        return owed(r, "terminal");
    }
    if opts.dry_run {
        let why = st.release.clone();
        return if r.is_act() {
            r
        } else {
            said(r, format!("would-release:{why}"))
        };
    }
    if let Err(why) = require_unique_owner(&opts.home, sf) {
        return owed(r, why);
    }
    if host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim)) {
        return owed(r, "tab-ownership-changed");
    }
    let generation = match typing_fence(c, tab) {
        Ok(generation) => generation,
        Err(why) => return owed(r, why),
    };
    let text = upgrade::release_prompt(st.agent);
    match turn_fenced(c, tab, &text, generation.as_deref()) {
        Ok(()) => {
            let why = st.release.clone();
            st.released();
            let r = said(r, format!("released:{why}"));
            ledger(
                opts,
                &r,
                "the upgrade abandoned the notice it gave the agent without restarting it: one \
                 line told it the upgrade is off for now, nothing will restart the session, \
                 nothing the notice asked of it still applies, and to carry on as it would have \
                 without it",
            );
            r
        }
        Err(e) => owed(r, &e.word()),
    }
}

/// The word of a step that leaves a release OWED, `why` what holds it: `r`
/// when it is an act of this step the window's host looks on after (`gave-up`
/// and `drain-expired:…` park it at the next idle point), else
/// `wait:release:<why>`, which the host looks at again ([`after`]). NEVER A
/// LAST WORD while a release is owed (review of 2026-09-26): a stop's own
/// word — `failed:signal-refused`, `refused:<what>` — reads
/// [`After::Finished`], and a release it owed and could not type at once was
/// never looked for again by the window.
fn owed_word(r: Report, why: &str) -> Report {
    if r.is_act() && after(&r.step, 0) != After::Finished {
        r
    } else {
        said(r, format!("wait:release:{why}"))
    }
}

/// A conversation OWED A RELEASE with no upgrade to take now ([`release`]):
/// its live process runs the build the upgrade stopped short of, and the agent
/// in it was asked to wind down — the process the notice reached, when the
/// target it was asked for is gone (rolled back, uninstalled). Any OTHER
/// process holding the conversation — resumed by hand after the upgrade
/// stopped, in this tab or any other, often on the new build — is not this
/// upgrade's to tell that nothing will restart it (the review of
/// 2026-09-26): the release is dropped ([`release_void`]), and so it is once
/// the agent took up direction given after its last answer to the upgrade.
/// Else one look, and the release if it may be typed.
#[allow(clippy::too_many_arguments)]
fn release_visit(
    opts: &Opts,
    r: Report,
    sf: &SessionFile,
    mut st: St,
    tab: &str,
    t: &[(u32, u32, String)],
    k: &dyn Kernel,
    host_claim: Option<&HostClaim>,
) -> Report {
    let now = now_s();
    let recent = transcript(&opts.home, &sf.session_id).map(|p| tail_to_end(&p, TAIL_BYTES));
    let tail = recent.as_ref().map(|(text, _)| text.as_str());
    if let Some(why) = release_void(&st, sf, tab, tail) {
        if opts.dry_run {
            return said(r, format!("would-drop-release:{why}"));
        }
        drop_release(opts, &r, &mut st, why);
        save(opts, &sf.session_id, &st);
        return said(r, "current");
    }
    let (mut c, facts) = match look(opts, sf, &mut st, tab, host_claim, t, now) {
        Ok(look) => look,
        Err(why) => return said(r, format!("wait:release:{why}")),
    };
    let r = release(
        opts,
        said(r, "wait:release"),
        &mut st,
        &mut c,
        tab,
        sf,
        host_claim,
        k,
        &facts,
        false,
        tail,
    );
    st.note_step(&r.step, &facts.status, now);
    save(opts, &sf.session_id, &st);
    r
}

/// Why a release owed is NO LONGER THIS UPGRADE'S TO TYPE into `sf` in
/// `tab`, if it is not: `other-process` — the process the notice reached no
/// longer holds the conversation in its tab ([`St::notice_belongs_to`]); a
/// person resumed it by hand, and the line would reach an agent that was
/// never asked, perhaps on the new build where "nothing will restart this
/// session" is false — or `directed`: someone spoke to the agent after its
/// last answer to the upgrade and it took that up
/// ([`upgrade::directed_since_ready`] over the transcript's `tail`, after a
/// READY to any marker this round typed, [`St::asked`]), and the line would
/// be typed over that newer direction (the review of 2026-09-26: a person
/// sent the draft that had voided a READY, the agent worked on it, and the
/// release then told it to go back to the work from before the notice). A
/// direction the agent answered with READY, or has not answered yet, drops
/// nothing: the agent holds for the restart, and the release is all that
/// can tell it to go on (the second review of 2026-09-26: a peer's message
/// before the READY dropped the release of an agent whose READY was then
/// voided, and it sat).
fn release_void(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> Option<&'static str> {
    if !st.notice_belongs_to(sf, tab) {
        return Some("other-process");
    }
    tail.is_some_and(|t| upgrade::directed_since_ready(t, &st.asked))
        .then_some("directed")
}

/// A release owed that is no longer this upgrade's to type ([`release_void`],
/// `why`) is DROPPED: nothing is owed, the round's markers forgotten (the
/// round is over, as if the line had been typed), said once in the ledger
/// (`release-dropped:<why>`). A dry run changes nothing.
fn drop_release(opts: &Opts, r: &Report, st: &mut St, why: &str) {
    if opts.dry_run {
        return;
    }
    st.dropped();
    ledger(
        opts,
        &said(r.clone(), format!("release-dropped:{why}")),
        if why == "directed" {
            "someone directed the conversation after the agent's last answer to the upgrade, and \
             the agent took that up: the line telling it to carry on is not typed over that"
        } else {
            "the process the notice reached no longer holds the conversation in its tab: the line \
             is not typed into another"
        },
    );
}

/// What a restart records of the transcript before its signal, from the tail
/// the READY answer was read in ([`tail_to_end`]): the model the agent's last
/// turn named (empty when none did) and where that read ended (`0` for no
/// read) — the mark past which the resumed session's first turn is looked for
/// ([`resumed_model`]).
pub(super) fn model_and_mark(recent: Option<&(String, u64)>) -> (String, u64) {
    recent.map_or((String::new(), 0), |(text, end)| {
        (upgrade::transcript_model(text).unwrap_or_default(), *end)
    })
}

/// Restarts a previous sweep left between the exit and the new process: the
/// agent is gone and no live process holds its conversation yet.
fn orphans(opts: &Opts, files: &[SessionFile], live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(mut st) = load(opts, &session).filter(St::in_flight) else {
            continue;
        };
        // A Codex restart is the Codex lane's to carry on ([`codex`]).
        if st.agent != upgrade::Agent::Claude {
            continue;
        }
        if live_tabs.is_some_and(|tabs| !tab_is_live(tabs, &st.tab)) {
            continue;
        }
        if files.iter().any(|f| f.session_id == session) || alive(st.pid) {
            continue; // `visit` has it, or the agent is still exiting.
        }
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
        let r = Report {
            pid: st.pid,
            tab: st.tab.clone(),
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.to, st.source),
            step: String::new(),
        };
        if let Some((why, detail)) = expired(&st, now_s()) {
            if opts.dry_run {
                out.push(said(r, format!("would-fail:{why}")));
                continue;
            }
            st.stop(why);
            let r = said(r, format!("failed:{why}"));
            ledger(opts, &r, detail);
            save(opts, &session, &st);
            out.push(r);
            continue;
        }
        if opts.dry_run {
            out.push(said(r, format!("would-resume:{}", st.phase.word())));
            continue;
        }
        let tab = st.tab.clone();
        let Ok(mut c) = connect(opts, &tab) else {
            out.push(said(r, "wait:no-socket"));
            continue;
        };
        if !roster(&mut c).contains(&tab) {
            out.push(said(r, "wait:tab-not-live"));
            continue;
        }
        let r = match st.phase {
            Phase::Exiting { .. } => relaunch(opts, r, &mut st, &mut c, &session, &Live),
            _ => await_new(opts, r, &mut st, &mut c, &session, &Live),
        };
        save(opts, &session, &st);
        out.push(r);
    }
    out
}

/// Whether the agent has answered a READY marker of THIS round of the upgrade
/// ([`St::marker`], [`St::markers`]) in `tail`, the transcript's tail — as its
/// last word after the latest notice ([`upgrade::transcript_has_ready`]). No
/// marker — none typed yet, or ones the drain voided ([`drain_expired`]) or a
/// release forgot — is no answer, whatever the tail holds.
fn answered(st: &St, tail: Option<&str>) -> bool {
    let Some(text) = tail else {
        return false;
    };
    std::iter::once(&st.marker)
        .chain(&st.markers)
        .filter(|m| !m.is_empty())
        .any(|m| upgrade::transcript_has_ready(text, m))
}

/// THE READY THE UPGRADE WILL ACT ON — what the reducer is given
/// ([`upgrade::next_step`]) and what holds the release back
/// ([`upgrade::gate_release`]): the agent [`answered`] a marker of this
/// round, in a phase that acts on one ([`St::hears_ready`]: announced, or
/// gave up and still hearing a late answer), from the very process and tab
/// its notice reached ([`St::notice_belongs_to`]) — resumed anywhere else,
/// the answer is no consent to end that process. A READY no phase acts on is
/// no READY: the review of 2026-09-26 measured one, left by a restart that
/// stopped after the answer, holding the release for ever.
fn heard(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> bool {
    st.hears_ready() && st.notice_belongs_to(sf, tab) && answered(st, tail)
}

/// Whether, after this visit's `step` said `word`, THE RELEASE owed is the
/// upgrade's NEXT ACT ([`release`]): the upgrade stopped asking or voided
/// the READY (`gave-up`, `drain-expired`), stopped for good in this very step
/// (`failed:…`, `refused:…`), or waits on nothing it would type or signal
/// itself ([`upgrade::release_is_next`]). A wait of the notice's or the
/// restart's own gate is not: what it waits for goes first, and its word is
/// the step's to say (the window's host owns the turn ends of a restart that
/// settles by it, [`owns_turn_ends`]).
fn release_is_next(step: &Step, word: &str) -> bool {
    match step {
        Step::GiveUp | Step::Void(_) => true,
        Step::Wait(why) => upgrade::release_is_next(why),
        Step::Announce | Step::Terminate => {
            word.starts_with("failed:") || word.starts_with("refused:")
        }
    }
}

/// THE DRAIN'S BOUND ([`upgrade::DRAIN_S`]): past it a person's state — a box nobody
/// answered, a draft nobody sent (`why`, [`upgrade::person_hold`]), standing for
/// [`upgrade::HOLD_S`] — or the agent's own background work, running
/// [`upgrade::DRAIN_S`] after the answer (`why` = `background`), still holds the
/// restart gate, so the READY answer is void ([`St::void`] at `now`). The markers
/// are forgotten (the tail is never searched for them again, so the old answer
/// can never end the agent later), the agent is owed its release — it stopped for
/// a restart that is not coming — and the ledger says why, once, from `f`'s
/// clocks. An announced upgrade stays `Announced` with its asks and its clock
/// restarted, so the ordinary re-ask asks again [`upgrade::REASK_S`] later and
/// [`upgrade::MAX_ASKS`] still bounds how often; one that gave up asking stays
/// given up, and asks no more for this build. Nothing is signalled or killed. A
/// dry run only says it.
fn drain_expired(opts: &Opts, r: Report, st: &mut St, why: &str, f: &Facts, now: u64) -> Report {
    if opts.dry_run {
        return said(r, format!("would-void:{why}"));
    }
    let asks_again = matches!(st.phase, Phase::Announced { .. });
    st.void(now);
    let r = said(r, format!("drain-expired:{why}"));
    let (what, held_s) = match why {
        "box" => ("a box nobody answered", f.hold_s),
        "draft" => ("a draft nobody sent", f.hold_s),
        _ => ("work of the agent's own still running", f.ready_s),
    };
    // The release is OWED here, and typed only where the step may type it
    // ([`release`]); a lane that types none (Codex) says nothing of one.
    let owed = if st.release.is_empty() {
        ""
    } else {
        "the agent is owed its release, and "
    };
    ledger(
        opts,
        &r,
        &format!(
            "the READY answer is void: {} min after the announcement {what} had held the \
             restart for {} min; {owed}{}",
            upgrade::DRAIN_S / 60,
            held_s / 60,
            if asks_again {
                "it is asked again later"
            } else {
                "it is not asked again for this build"
            }
        ),
    );
    r
}

/// Record that the agent is no job of a job-control shell ([`Job::NoJobControl`]):
/// the upgrade stops for this (session, target), said once in the ledger. No
/// release is owed: the line is typed under the notice's fences, the agent
/// its shell's foreground job among them, and such an agent can never be
/// proven to read it — one owed would be one the window looked for, and never
/// typed, for as long as the conversation lives.
fn not_a_job(opts: &Opts, r: Report, st: &mut St) -> Report {
    if opts.dry_run {
        return said(r, "would-refuse:not-a-shell-job");
    }
    st.stop("not-a-shell-job");
    st.release.clear();
    let r = said(r, "refused:not-a-shell-job");
    ledger(
        opts,
        &r,
        "the agent is no job of an interactive shell: nothing would take the terminal back to relaunch it",
    );
    r
}

/// The agent's terminal is not the tab's ([`owned_by_aterm`]): nothing typed
/// into the tab would reach it, so the session is not moved while that holds.
/// A WAIT — the agent may be run in the tab itself next time — but said ONCE
/// per (session, target, reason), in the ledger and as an act the window's host
/// logs: without it the session kept its old build for good and nothing said
/// why. A dry run only says the wait.
fn held_back(opts: &Opts, r: Report, st: &mut St, session: &str, why: &str) -> Report {
    if opts.dry_run || st.noted == why {
        return said(r, format!("wait:{why}"));
    }
    st.noted = why.to_string();
    let r = said(r, format!("held-back:{why}"));
    ledger(
        opts,
        &r,
        "the agent's terminal is not the tab's (a multiplexer pane, `script`, ssh): nothing typed into the tab would reach it, so it is not moved",
    );
    save(opts, session, st);
    r
}

/// The tab is ATTENDED ([`upgrade::Facts::attended`]) and the owner's `--now`
/// is not in force: the upgrade waits for the person to step away. A WAIT —
/// but said ONCE per upgrade, like [`held_back`], in the ledger and as an act
/// the window's host logs (`held-back:attended`): the host notes only acts,
/// and this wait was silent for as long as it lasted (the 2026-09-24 review).
/// Every later step that finds it so says `wait:attended`. A dry run only
/// says the wait.
fn attended_once(opts: &Opts, r: Report, st: &mut St) -> Report {
    const WHY: &str = "attended";
    if opts.dry_run || st.noted == WHY {
        return said(r, format!("wait:{WHY}"));
    }
    st.noted = WHY.to_string();
    let r = said(r, format!("held-back:{WHY}"));
    ledger(
        opts,
        &r,
        "a person is at this tab (a person gave this session input within `[harness] \
         human_grace_s`, by aterm's per-session stamp `status human_ms=`): nothing is typed into \
         it on the upgrade's own judgment until they step away, or the owner's `aterm harness \
         upgrade <sid> --now`",
    );
    r
}
/// THE ANNOUNCEMENT, for a restart that can be carried out. `planned` is
/// [`plan`]'s answer, asked FIRST: the notice asks the agent to stop starting
/// work and to answer READY, so typing it for a launch the signal would then
/// refuse interrupts the agent for nothing — and the refusal would stick for
/// this target with nothing typed back. `send` types the notice for the
/// `asks`-th time and answers its READY marker.
fn announce(
    opts: &Opts,
    r: Report,
    st: &mut St,
    planned: Result<Plan, NoPlan>,
    now: u64,
    send: impl FnOnce(u32) -> Result<String, String>,
) -> Report {
    if let Err(no) = planned {
        return unplanned(opts, r, st, no);
    }
    if opts.dry_run {
        return said(r, "would-announce");
    }
    let asks = match st.phase {
        Phase::Announced { asks, .. } => asks + 1,
        _ => 1,
    };
    match send(asks) {
        Ok(marker) => {
            st.announced(marker, now, asks);
            let r = said(r, format!("announced:{asks}"));
            ledger(opts, &r, &st.marker);
            r
        }
        Err(e) => said(r, format!("wait:announce-refused:{}", first_word(&e))),
    }
}

/// What the upgrade's relaunch line is made from: the live session's
/// conversation and directory, its argv, the target build, the model the
/// priority list moves the conversation to, if any, and the permission mode
/// it last ran in.
fn launch_of<'a>(
    sf: &'a SessionFile,
    argv: &'a [String],
    target: &'a Candidate,
    model: Option<&'a str>,
    mode: Option<&'a str>,
) -> Launch<'a> {
    Launch {
        session: &sf.session_id,
        resume: true,
        cwd: &sf.cwd,
        agent: sf.pid,
        argv,
        exe: &target.exe,
        model,
        mode,
    }
}

/// The last look before typing into Claude's composer: a FRESH read whose
/// composer is still empty and with no box up, and that read's generation —
/// what the typed turn is fenced on ([`turn_fenced`]). The facts a step was
/// decided on are a pass's worth of process reads old by the time it types;
/// this read is one request old, and the fence closes the rest.
pub(super) fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    if aterm_phase::prompt::prompt_box_span(&scr.rows).is_some() {
        return Err("box");
    }
    if !composer_empty(c, tab, &scr) {
        return Err("draft");
    }
    Ok(scr.generation)
}

#[allow(clippy::too_many_arguments)]
fn restart(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    argv: &[String],
    target: &Candidate,
    t: &[(u32, u32, String)],
    k: &dyn Kernel,
    host_claim: Option<&HostClaim>,
    grace: u32,
) -> Report {
    if !st.notice_belongs_to(sf, tab) {
        return said(r, "wait:notice-owned-by-other-process");
    }
    if is_our_ancestor(sf.pid, t) {
        return said(r, "wait:self");
    }
    // Everything the relaunch needs is read BEFORE the agent is ended, the first
    // of it that the parent IS a job-control shell holding the agent as its
    // foreground job ([`Job`]): only then does the relaunch's wait for that
    // parent's `pgid == tpgid` mean "its prompt is back". The line is then
    // planned again rather than remembered from the announcement: a hook
    // installed or a directory changed since then changes the line, and this
    // plan — made against the shell the job read just proved — is the last word.
    let proven = match k.job(sf.pid) {
        Some((Job::NoJobControl, _)) => return not_a_job(opts, r, st),
        job => match foreground_shell(job) {
            Ok(shell) => shell,
            Err(why) => return said(r, format!("wait:{why}")),
        },
    };
    let model = (!st.model_list.is_empty()).then(|| st.model_list.clone());
    let mode = last_permission_mode(&opts.home, &sf.session_id);
    let launch = launch_of(sf, argv, target, model.as_deref(), mode.as_deref());
    let Plan { shell, line, .. } = match plan(opts, &launch, proven, t) {
        Ok(p) => p,
        Err(no) => return unplanned(opts, r, st, no),
    };
    // The last look before the one irreversible act: the same process, still
    // idle, still its shell's foreground job, the composer still empty, nothing
    // running under it, nobody's hand (a person's within `grace`: `0` under
    // the owner's `--now`).
    let again = session_file_of(&opts.home, sf.pid);
    let scr = screen(c, tab);
    let still = again.is_some_and(|a| {
        a.session_id == sf.session_id
            && a.status == "idle"
            && st.notice_belongs_to(&a, tab)
            && kernel_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
    }) && foreground_shell(k.job(sf.pid)) == Ok(shell)
        && scr.is_some_and(|s| composer_empty(c, tab, &s))
        && background(sf.pid, &table()).is_empty()
        && !held(c, tab, grace)
        && host_claim.is_none_or(|claim| claim_still_live(c, sf.pid, claim));
    if !still {
        return said(r, "wait:changed");
    }
    if let Err(why) = require_unique_owner(&opts.home, sf) {
        return said(r, format!("wait:{why}"));
    }
    let Ok(pid) = i32::try_from(sf.pid) else {
        return said(r, "wait:pid");
    };
    // A restart carries the agent on (its continuation says continue): the
    // READY it acts on and the release a notice abandoned before this one are
    // taken off the record with the exit written ([`St::signalled`]), and put
    // back if the signal is never sent.
    let back = st.signalled(sf.pid, shell, tab, line, now_s());
    save(opts, &sf.session_id, st);
    // A second owner, a job-control change, or a moved terminal during the
    // state write must veto the signal. Re-read the current session file and
    // kernel claims immediately before SIGTERM.
    let current = session_file_of(&opts.home, sf.pid);
    let owns_notice = current.as_ref().is_some_and(|a| {
        a.session_id == sf.session_id
            && a.status == "idle"
            && st.notice_belongs_to(a, tab)
            && kernel_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
    });
    let owner = k.terminal(sf.pid);
    let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
    // The composer is read AGAIN, last, on the same connection: a draft a
    // person began during the state write above is seen here, before the one
    // irreversible act, not in the relaunch's wait after it.
    if !owns_notice
        || require_unique_owner(&opts.home, sf).is_err()
        || foreground_shell(k.job(sf.pid)) != Ok(shell)
        || !owner.is_some_and(owned_by_aterm)
        || host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim))
        || typing_fence(c, tab).is_err()
    {
        st.unsent(back);
        save(opts, &sf.session_id, st);
        return said(r, "wait:changed-before-signal");
    }
    // The relaunch will ask for a model from the list: remembered for this
    // conversation BEFORE the one irreversible act, so it is never asked for
    // twice (a model that is then not taken is recorded as failed).
    if !st.model_list.is_empty() {
        let mut rec = load_model_record(opts, &sf.session_id);
        rec.set.clone_from(&st.model_list);
        rec.set_at = now_s();
        save_model_record(opts, &sf.session_id, &rec);
    }
    match terminate(c, tab, pid, grace) {
        // The agent is ended: its carry-on, not a release, tells it to go on.
        Terminated::Sent => {}
        Terminated::Refused(why) => {
            st.unsent(back);
            save(opts, &sf.session_id, st);
            return said(r, format!("wait:signal-{why}"));
        }
        Terminated::Failed => {
            // The agent answered READY and lives on, stopped: it is owed its
            // release, and its READY is no longer one anything acts on.
            st.signal_failed(back);
            save(opts, &sf.session_id, st);
            return said(r, "failed:signal-refused");
        }
    }
    ledger(opts, &said(r.clone(), "terminated"), "SIGTERM");
    relaunch(opts, r, st, c, &sf.session_id, k)
}

/// THE CONFIRMATIONS a carry-on left pending ([`confirm`]): each restart whose
/// continuation was typed before its resumed session had answered is read
/// again, and says its `done` row once the answer is written or its time is
/// up. Only files are read and written — no tab is asked or typed into — so
/// every sweep runs it first, whatever else it finds; a hand-run sweep for one
/// tab (`only_sid`) confirms only that tab's.
fn confirmations(opts: &Opts) -> Vec<Report> {
    if opts.dry_run {
        return Vec::new();
    }
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let now = now_s();
    let mut out = Vec::new();
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(mut st) = load(opts, &session).filter(St::confirming) else {
            continue;
        };
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
        let r = Report {
            pid: st.resumed_pid,
            tab: st.tab.clone(),
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.resumed_on, st.source),
            step: String::new(),
        };
        out.extend(confirm(opts, &r, &mut st, &session, now));
    }
    out
}

// The upgrade tests drive real processes through Unix APIs (process groups, modes, inodes).
#[cfg(all(test, unix))]
#[path = "upgrade_drive_tests.rs"]
mod tests;
