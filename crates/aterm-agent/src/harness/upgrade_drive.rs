// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE I/O HALF OF [`super::upgrade`]: one sweep over every running Claude Code
//! session this user has, each advanced one step of the cooperative live upgrade.
//!
//! Every fact comes from a source that cannot be stale for the process it
//! describes: Claude's own session file (checked against the KERNEL start time,
//! so a recycled pid never reads as the session), `KERN_PROCARGS2` for argv
//! and any readable tab id (`ATERM_PARENT_SESSION_ID`), the host instance's
//! owner-only PTY foreground-group roster for a tab id when macOS omits the
//! environment, the process table for what runs under the agent, whose terminal
//! it is on and whether it is its shell's job, and the tab's own screen and
//! hold over the control socket. Whatever
//! cannot be read is a WAIT, never a guess; nothing here types or signals
//! unless [`super::upgrade::next_step`] says go, and every step taken is one
//! ledger line.
//!
//! State is one small JSON file per conversation under
//! `<harness state>/upgrade/`, so a sweep that runs every minute — the window's,
//! or `aterm harness upgrade --every 60` — resumes exactly where the last one
//! stopped, including across the agent's own exit while that restart is fresh
//! and its shell lives (`expired`).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_json::{Map, Value};

use super::upgrade::{self, Candidate, Dialect, Facts, Phase, SessionFile, Source, Step, Version};
use crate::RelayClient;

#[path = "upgrade_wake.rs"]
mod upgrade_wake;

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
        format!(
            "upgrade pid={} tab={} session={} from={} to={} step={}",
            self.pid, self.tab, self.session, self.from, self.to, self.step
        )
    }
}

fn now_s() -> u64 {
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

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The kernel's start time of `pid`, rendered as Claude renders `procStart`.
fn kernel_start(pid: u32) -> Option<String> {
    let s = squash(&ps(&["-o", "lstart=", "-p", &pid.to_string()])?);
    (!s.is_empty()).then_some(s)
}

fn alive(pid: u32) -> bool {
    let Ok(p) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 checks existence and permission; nothing is delivered.
    let rc = unsafe { libc::kill(p, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// `(ppid, pgid, tpgid)` of `pid`.
fn ids(pid: u32) -> Option<(u32, i64, i64)> {
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
fn table() -> Vec<(u32, u32, String)> {
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
fn owned_by_aterm(owner: (u32, &str)) -> bool {
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
enum Job {
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
fn foreground_shell(job: Option<(Job, u32)>) -> Result<u32, &'static str> {
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
trait Kernel {
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
struct Live;

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
/// background shell, a build). MCP servers and other helpers are not: a resume
/// restarts them.
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

/// The background work under `pid`, by basename.
fn background(pid: u32, t: &[(u32, u32, String)]) -> Vec<String> {
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
                if BACKGROUND.contains(&name.as_str()) {
                    found.push(name.clone());
                }
            }
        }
    }
    found
}

/// Whether `pid` is this process or one of its ancestors: ending it would end
/// the sweep that is driving it.
fn is_our_ancestor(pid: u32, t: &[(u32, u32, String)]) -> bool {
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
fn cwd_of(pid: u32) -> Option<String> {
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
fn exe_of(pid: u32) -> Option<PathBuf> {
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

/// `<exe> --version`'s leading version, bounded by the child's own speed.
fn version_of(exe: &Path) -> Option<Version> {
    let out = Command::new(exe).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Version::parse(text.split_whitespace().next()?)
}

// ---------------------------------------------------------------- targets

/// Where Claude's native installer keeps its builds.
fn native_root(home: &Path) -> PathBuf {
    home.join(".local/share/claude/versions")
}

/// The builds a session could move to, read ONCE per sweep.
struct Targets {
    /// aterm's managed twin (`<prefix>/agents/claude`).
    managed: Option<Candidate>,
    /// The native install's current build (what `~/.local/bin/claude` names).
    native: Option<Candidate>,
}

impl Targets {
    fn read(home: &Path) -> Targets {
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
    fn for_session(&self, running_native: bool) -> Vec<Candidate> {
        self.managed
            .iter()
            .chain(self.native.iter().filter(|_| running_native))
            .cloned()
            .collect()
    }
}

/// [`version_of`], asked again only when the file's size or mtime moved: the
/// twin is re-rendered whenever the build it execs changes, and the host asks
/// every minute.
fn cached_version(exe: &Path) -> Option<Version> {
    type Seen = std::collections::HashMap<PathBuf, (u64, SystemTime, Version)>;
    static CACHE: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
    let meta = std::fs::metadata(exe)
        .ok()
        .filter(std::fs::Metadata::is_file)?;
    let stamp = (meta.len(), meta.modified().ok()?);
    if let Ok(cache) = CACHE.lock()
        && let Some((len, at, v)) = cache.as_ref().and_then(|c| c.get(exe))
        && (*len, *at) == stamp
    {
        return Some(v.clone());
    }
    let v = version_of(exe)?;
    if let Ok(mut cache) = CACHE.lock() {
        cache
            .get_or_insert_with(Seen::new)
            .insert(exe.to_path_buf(), (stamp.0, stamp.1, v.clone()));
    }
    Some(v)
}

// ---------------------------------------------------------------- the tab

type Client = RelayClient<aterm_uds::CtlStream>;

fn connect(opts: &Opts, sid: &str) -> Result<Client, String> {
    let sock = match &opts.sock {
        Some(s) => s.clone(),
        None => aterm_ctl::resolve_sock_for(Some(sid)).map_err(|e| format!("{e}"))?,
    };
    let token = aterm_ctl::read_token_beside(&sock).map_err(|e| format!("{e}"))?;
    RelayClient::connect_local(&sock, &token).map_err(|e| format!("{sock}: {e}"))
}

/// The instance's live tab ids.
fn roster(c: &mut Client) -> Vec<String> {
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
struct LiveTab {
    sid: String,
    fgpgid: Option<i64>,
}

fn host_roster(c: &mut Client) -> Option<Vec<LiveTab>> {
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
fn unique_tab_for_group(tabs: &[LiveTab], group: i64) -> Option<&str> {
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
fn process_in_tab(
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
struct Screen {
    rows: Vec<String>,
    cursor: Option<(usize, usize)>,
    seq: u64,
    /// The screen row `rows[0]` is: the coordinates the `cell` verb takes.
    first: usize,
}

fn screen(c: &mut Client, sid: &str) -> Option<Screen> {
    let (head, body) = c.request_counted(&format!("@{sid} text --json")).ok()?;
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
    Some(Screen {
        rows,
        cursor,
        seq,
        first: usize::try_from(first).ok()?,
    })
}

/// [`upgrade::composer_is_empty`] over `scr`, with the one fact its rows cannot
/// carry: whether the text at the cursor's row, column 2 is drawn DIM (Claude's
/// placeholder suggestion) or not (a typed draft whose caret was moved home).
/// Read with the `cell` verb only when the cursor IS at column 2, the one case
/// the answer can change. A reply that is not an `OK` naming `dim` — an `ERR`,
/// a lost connection, an unexpected shape — reads NOT dim: a suggestion then
/// waits as a draft would, and nothing is ever typed over a draft.
fn composer_empty(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    let dim = match scr.cursor {
        Some((row, 2)) => scr.first.checked_add(row).is_some_and(|at| {
            c.request_line(&format!("@{sid} cell {at} 2"))
                .is_ok_and(|line| cell_is_dim(&line))
        }),
        _ => false,
    };
    upgrade::composer_is_empty(&scr.rows, scr.cursor, dim)
}

/// Whether one `cell` reply — `OK <grapheme%enc> <fg> <bg> <attrs>[ link=…]`,
/// the attrs a comma list or `none` (aterm-gui `cmd_cell`) — names `dim`. The
/// fields are split on single spaces, never on runs: a blank cell's grapheme is
/// an EMPTY token, and collapsing it would read the bg colour as the attrs.
fn cell_is_dim(line: &str) -> bool {
    line.trim_end()
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').nth(3))
        .is_some_and(|attrs| attrs.split(',').any(|a| a == "dim"))
}

/// Whether someone other than this sweep holds the tab: a halt, or a hand on it
/// (a turn, a lease, a driver).
fn held(c: &mut Client, sid: &str) -> bool {
    c.request_line(&format!("@{sid} status"))
        .map_or(true, |line| held_by_status(&line))
}

/// [`held`] over one `status` reply; anything but an `OK` reads as held.
fn held_by_status(line: &str) -> bool {
    !line.starts_with("OK")
        || line
            .split_whitespace()
            .any(|t| t == "hold=1" || (t.starts_with("hand=") && t != "hand=-"))
}

/// Type one turn into the tab. `Ok` only on an `OK` reply.
fn turn(c: &mut Client, sid: &str, text: &str) -> Result<(), String> {
    let (head, _) = c
        .request_counted(&format!("@{sid} turn idle=1500 timeout=30000 {text}"))
        .map_err(|e| format!("{e}"))?;
    if head.starts_with("OK") {
        Ok(())
    } else {
        Err(head)
    }
}

// ---------------------------------------------------------------- state

/// What the sweep remembers about one conversation's upgrade. Everything a
/// relaunch needs is written here BEFORE the agent is signalled, so a sweep that
/// dies between the exit and the relaunch leaves the next one enough to finish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct St {
    phase: Phase,
    from: String,
    to: String,
    source: String,
    marker: String,
    salt: u64,
    last_seq: u64,
    seq_since_s: u64,
    /// The agent that was signalled.
    pid: u32,
    /// The process whose tab received the READY notice. `pid` becomes the
    /// process signalled at restart, so notice ownership needs its own fence.
    notice_pid: u32,
    notice_start: String,
    /// The shell that gets its prompt back.
    shell: u32,
    /// The tab the shell lives in.
    tab: String,
    /// The line typed at that prompt.
    line: String,
    /// The last reason this upgrade was held back that the ledger was told
    /// ([`held_back`]), so it is said once, not every sweep.
    noted: String,
}

impl St {
    fn fresh(from: &Version, to: &Candidate, now: u64) -> St {
        St {
            from: from.to_string(),
            to: to.version.to_string(),
            source: to.source.as_str().to_string(),
            salt: now,
            seq_since_s: now,
            ..St::default()
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
            ("noted", &self.noted),
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
        ] {
            o.insert(k.into(), Value::from(v));
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    fn from_json(text: &str) -> Option<St> {
        let v: Value = aterm_json::from_str(text).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let small = |k: &str| u32::try_from(n(k)).unwrap_or(0);
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
            salt: n("salt"),
            last_seq: n("last_seq"),
            seq_since_s: n("seq_since"),
            pid: small("pid"),
            notice_pid: small("notice_pid"),
            notice_start: s("notice_start"),
            shell: small("shell"),
            tab: s("tab"),
            line: s("line"),
            noted: s("noted"),
        })
    }

    fn in_flight(&self) -> bool {
        matches!(self.phase, Phase::Exiting { .. } | Phase::Relaunched { .. })
    }

    fn notice_belongs_to(&self, sf: &SessionFile, tab: &str) -> bool {
        self.tab == tab
            && self.notice_pid == sf.pid
            && !self.notice_start.is_empty()
            && self.notice_start == squash(&sf.proc_start)
    }
}

fn state_dir(opts: &Opts) -> PathBuf {
    opts.state.join("upgrade")
}

fn state_path(opts: &Opts, session: &str) -> PathBuf {
    state_dir(opts).join(format!("{session}.json"))
}

fn load(opts: &Opts, session: &str) -> Option<St> {
    St::from_json(&std::fs::read_to_string(state_path(opts, session)).ok()?)
}

fn save(opts: &Opts, session: &str, st: &St) {
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

/// One ledger line per step taken: `<state>/upgrade/ledger.jsonl`.
fn ledger(opts: &Opts, r: &Report, detail: &str) {
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
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("ledger.jsonl"))
    {
        let _ = writeln!(
            f,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    }
}

// ---------------------------------------------------------------- claude files

#[cfg(test)]
thread_local! {
    /// Counts complete directory scans, not candidates, in one test thread.
    static SESSION_FILE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A partial roster cannot prove a conversation has only one live owner. A
/// sibling JSON may be mid-write, unreadable or malformed; any such entry
/// invalidates the whole scan until the next sweep.
fn session_files(home: &Path) -> Option<Vec<SessionFile>> {
    #[cfg(test)]
    SESSION_FILE_SCANS.with(|count| count.set(count.get() + 1));
    let dir = std::fs::read_dir(home.join(".claude/sessions")).ok()?;
    let mut files = Vec::new();
    for entry in dir {
        let path = entry.ok()?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let text = std::fs::read_to_string(path).ok()?;
        files.push(upgrade::parse_session_file(&text).ok()?);
    }
    Some(files)
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
fn require_unique_owner(home: &Path, wanted: &SessionFile) -> Result<(), &'static str> {
    let files = session_files(home);
    preliminary_unique_owner(files.as_deref(), wanted)
}

fn conversation_live(files: &[SessionFile], session: &str) -> bool {
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
    mut needs_inspection: impl FnMut(&SessionFile) -> bool,
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
            if !needs_inspection(sf) {
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
    if prior.is_some_and(St::in_flight) {
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

fn session_file_of(home: &Path, pid: u32) -> Option<SessionFile> {
    let t = std::fs::read_to_string(home.join(format!(".claude/sessions/{pid}.json"))).ok()?;
    upgrade::parse_session_file(&t).ok()
}

/// The one transcript holding `session`.
fn transcript(home: &Path, session: &str) -> Option<PathBuf> {
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
fn tail(path: &Path, bytes: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(bytes)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

// ---------------------------------------------------------------- the sweep

/// ONE SWEEP: every live Claude Code session advanced by at most one step (the
/// restart counts as one: exit, relaunch and continue, each bounded), then every
/// restart a previous sweep left in flight carried on.
///
/// ONE SWEEPER AT A TIME, machine-wide: the window's host and a hand-run
/// `aterm harness upgrade` share `<state>/upgrade/sweep.lock`, and a sweep that
/// finds it held reports one `busy` line and does nothing — two sweepers would
/// each read `pending` and type the notice twice.
#[must_use]
pub fn sweep(opts: &Opts) -> Vec<Report> {
    sweep_with_roster(opts, None)
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
    let mut reports = if let Some(tabs) = live_tabs {
        // Read installed versions only after a cheap PTY-group match. A window
        // with no Claude job does not execute a managed twin just to ask its
        // version; a matched but current job stops before `ps` and argv reads.
        let targets = std::cell::OnceCell::new();
        let candidates = host_candidates(
            &files,
            tabs,
            |sf| {
                needs_upgrade_inspection(
                    sf,
                    targets.get_or_init(|| Targets::read(&opts.home)),
                    load(opts, &sf.session_id).as_ref(),
                )
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
            candidates
                .into_iter()
                .map(|(sf, args, claim)| {
                    visit_with_claim(
                        opts,
                        sf,
                        Some(&files),
                        &t,
                        targets,
                        &Live,
                        Some(&args),
                        Some(&claim),
                    )
                })
                .collect()
        }
    } else if files.is_empty() {
        Vec::new()
    } else {
        let t = table();
        let targets = Targets::read(&opts.home);
        files
            .iter()
            .map(|sf| visit_with_claim(opts, sf, Some(&files), &t, &targets, &Live, None, None))
            .collect()
    };
    reports.extend(orphans(opts, &files, live_tabs));
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
fn sweep_lock(opts: &Opts) -> Result<Option<SweepLock>, &'static str> {
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
struct SweepLock(std::fs::File);

impl Drop for SweepLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// How often the window's host sweeps. The notice is re-asked on a 30-minute
/// clock and a restart waits on the agent, so a minute loses nothing.
pub const HOST_EVERY: Duration = Duration::from_secs(60);

/// The control socket is bound after the worker starts. Keep five quick
/// checks after launch even when an early roster read succeeds.
const HOST_STARTUP_EVERY: Duration = Duration::from_secs(2);
const HOST_STARTUP_TRIES: u8 = 5;
const HOST_STARTUP_DONE: u8 = HOST_STARTUP_TRIES + 1;

struct HostCadence {
    tick: u8,
}

impl HostCadence {
    fn new() -> Self {
        Self { tick: 0 }
    }

    fn next_delay(&mut self) -> Duration {
        let short = self.tick < HOST_STARTUP_TRIES;
        self.tick = self.tick.saturating_add(1).min(HOST_STARTUP_DONE);
        if short {
            HOST_STARTUP_EVERY
        } else {
            HOST_EVERY
        }
    }
}

/// Whether the window's host sweeps now: aterm.toml's `[harness] enabled`
/// (the master switch) and `[harness] upgrade` (this sweep's own switch), both
/// ON unless turned off, each read by [`super::cli::harness_switch`] — TOML's
/// answer through the parser the window itself loads aterm.toml with.
///
/// The sweep's switch was `cap.upgrade.enabled` in the harness's own
/// `config.toml` until that file and its `config` verb were deleted with the
/// second harness stack (design §0.4, 2026-09-23): aterm.toml's `[harness]`
/// table is the one policy home, and the window's parser admits `upgrade`
/// there as a key the supervisor engine carries but does not act on.
///
/// Fail-closed where it matters: a value other than `true`/`false`, a
/// `false` written below another table's header, a file the TOML parser
/// refuses (the window starts escalate-only on it, whose sweep is off), or an
/// aterm.toml that exists but cannot be read, reads OFF — the host acts on the
/// owner's sessions, and a file it cannot read is not consent. No aterm.toml
/// at all is a fresh machine, and ON.
#[must_use]
pub fn host_enabled(aterm_toml: Option<&Path>) -> bool {
    super::cli::harness_switch(aterm_toml, "enabled")
        && super::cli::harness_switch(aterm_toml, "upgrade")
}

/// THE WINDOW'S HOST: sweep every [`HOST_EVERY`] for as long as the process
/// lives, asking `enabled` every tick so a switch lands within a minute and
/// without a relaunch — the window passes its OWN parsed `[harness]` table
/// (`enabled && upgrade`, updated at every config load), so one process has
/// one parser of that table; [`host_enabled`] reads the file for a caller
/// with no parsed config. Each tick drives only the tabs of the
/// instance whose pid is `own_pid` (its control socket, looked up again every
/// tick because a self-update moves it), so two windows never contend for one
/// session; a tick that finds no socket for it sweeps nothing. `note` gets
/// what [`host_notes`] picks: every act, and a sweep that cannot run, once.
pub fn host(
    home: &Path,
    state: &Path,
    enabled: impl Fn() -> bool,
    own_pid: u32,
    mut note: impl FnMut(&Report),
) {
    let mut fault = None;
    let mut cadence = HostCadence::new();
    let mut activation_wake = upgrade_wake::ActivationWake::new();
    loop {
        activation_wake.wait(cadence.next_delay());
        // `None`: this tick swept nothing (a switch off, or no socket).
        let swept = if enabled() {
            instance_sock(own_pid).map(|sock| {
                let opts = Opts {
                    home: home.to_path_buf(),
                    state: state.to_path_buf(),
                    sock: Some(sock),
                    only_sid: None,
                    dry_run: false,
                };
                let tabs = connect(&opts, "")
                    .ok()
                    .and_then(|mut c| host_roster(&mut c));
                // Even an unreadable roster checks the sweep lock, retaining
                // the host's once-per-fault state-directory diagnosis.
                sweep_with_roster(&opts, Some(tabs.as_deref().unwrap_or(&[])))
            })
        } else {
            None
        };
        for r in host_notes(swept.as_deref(), &mut fault) {
            note(r);
        }
    }
}

/// What the host tells `note` about one tick: every act — and, ONCE per
/// change, a sweep that could not run for a reason that does not clear by
/// itself (`busy:state-unwritable`, `busy:lock-unopenable`). Until 2026-09-24
/// only acts were noted and no `busy:` step is one, so a window whose state
/// directory cannot hold the lock went inert every minute and never said so.
/// `busy:another-sweep` stays quiet: a hand-run sweep holding the lock is the
/// lock working. `fault` is the fault last said; a tick that swept nothing
/// (`None`) forgets it, so a fault that outlives a switch off and on is said
/// again when the host resumes.
fn host_notes<'a>(swept: Option<&'a [Report]>, fault: &mut Option<String>) -> Vec<&'a Report> {
    let reports = swept.unwrap_or_default();
    let now = reports
        .iter()
        .map(|r| r.step.as_str())
        .find(|s| matches!(*s, "busy:state-unwritable" | "busy:lock-unopenable"));
    let new = now.is_some() && now != fault.as_deref();
    *fault = now.map(str::to_owned);
    reports
        .iter()
        .filter(|r| r.is_act() || (new && now == Some(r.step.as_str())))
        .collect()
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

/// The control socket of the LOCAL instance whose pid is `pid`.
#[must_use]
pub fn instance_sock(pid: u32) -> Option<String> {
    aterm_ctl::local_instances()
        .ok()?
        .into_iter()
        .find_map(|(p, sock)| (p == pid).then_some(sock))
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

fn said(mut r: Report, step: impl Into<String>) -> Report {
    r.step = step.into();
    r
}

#[cfg(test)]
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
                st.phase = Phase::Failed("resumed-elsewhere".to_string());
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
    let Some(target) = upgrade::choose_target(&running, &targets.for_session(running_native))
    else {
        return said(r, "current");
    };
    r.to = format!("{}({})", target.version, target.source.as_str());
    let now = now_s();
    let mut st = match prior {
        Some(st) if st.to == target.version.to_string() => st,
        _ => St::fresh(&running, &target, now),
    };
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
            return said(r, format!("wait:{why}"));
        }
        let owner = k.terminal(sf.pid);
        let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
        if !owner.is_some_and(owned_by_aterm) {
            let why = format!("terminal:{}", owner_word(owner));
            return held_back(opts, r, &mut st, &sf.session_id, &why);
        }
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
    let Some(scr) = screen(&mut c, &tab) else {
        return said(r, "wait:screen-unreadable");
    };
    if scr.seq != st.last_seq {
        st.last_seq = scr.seq;
        st.seq_since_s = now;
    }
    let ready = !st.marker.is_empty()
        && transcript(&opts.home, &sf.session_id)
            .is_some_and(|p| upgrade::transcript_has_ready(&tail(&p, 262_144), &st.marker));
    let facts = Facts {
        status: sf.status.clone(),
        status_age_s: now.saturating_sub(sf.status_updated_at_ms / 1000),
        composer_empty: composer_empty(&mut c, &tab, &scr),
        approval_box: aterm_phase::prompt::prompt_box_span(&scr.rows).is_some(),
        busy_footer: {
            let busy = aterm_phase::anchors::anchor("busy.interrupt");
            scr.rows.iter().any(|row| row.contains(busy))
        },
        background: background(sf.pid, t),
        held: held(&mut c, &tab),
        quiet_s: now.saturating_sub(st.seq_since_s),
    };
    let mut step = upgrade::next_step(&st.phase, &facts, ready, now);
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
        Step::Wait(why) => said(r, format!("wait:{why}")),
        Step::GiveUp => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            st.phase = Phase::Failed("unanswered".to_string());
            let r = said(r, "gave-up");
            ledger(opts, &r, "no READY answer after the last announcement");
            r
        }
        Step::Announce => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            let planned = proven.map_or(Err(NoPlan::Wait("ids")), |shell| {
                plan(opts, sf, shell, &args.argv, &target, t)
            });
            let salt = st.salt;
            let result = announce(opts, r, &mut st, planned, now, |asks| {
                let marker = upgrade::ready_marker(
                    &sf.session_id,
                    &target.version,
                    salt.wrapping_add(u64::from(asks)),
                );
                let text =
                    upgrade::prepare_prompt(&running, &target.version, target.source, &marker);
                require_unique_owner(&opts.home, sf).map_err(str::to_string)?;
                if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
                    return Err("tab-ownership-changed".to_string());
                }
                turn(&mut c, &tab, &text).map(|()| marker)
            });
            if result.step.starts_with("announced:") {
                st.tab.clone_from(&tab);
                st.notice_pid = sf.pid;
                st.notice_start = squash(&sf.proc_start);
            }
            result
        }
        Step::Terminate if opts.dry_run => said(r, "would-restart"),
        Step::Terminate => restart(
            opts, r, &mut st, &mut c, &tab, sf, &args.argv, &target, t, k, host_claim,
        ),
    };
    save(opts, &sf.session_id, &st);
    result
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
            st.phase = Phase::Failed(why.to_string());
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

/// How long a restart in flight may still act on its tab, from the SIGTERM
/// (exiting) or the relaunch line (relaunched). The relaunch line is typed at a
/// prompt, and minutes after the agent ended the person may be typing at that
/// prompt, or it may be gone with its tab: an exit that old is never
/// relaunched, a relaunch that old never waited for again.
const STALE_S: u64 = 300;

/// Why a restart in flight whose agent is gone must stop instead of acting on
/// the tab, `(word, ledger detail)`, or `None` while it may still act.
fn expired(st: &St, now: u64) -> Option<(&'static str, &'static str)> {
    match st.phase {
        Phase::Exiting { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "stale-exit",
            "the agent ended minutes ago and was never relaunched: the tab is not typed into now",
        )),
        Phase::Exiting { .. } if st.shell == 0 || !alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran the agent is gone: there is no prompt to relaunch at",
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "no-resume",
            "the relaunched agent never registered the conversation",
        )),
        _ => None,
    }
}

/// Record that the agent is no job of a job-control shell ([`Job::NoJobControl`]):
/// the upgrade stops for this (session, target), said once in the ledger.
fn not_a_job(opts: &Opts, r: Report, st: &mut St) -> Report {
    if opts.dry_run {
        return said(r, "would-refuse:not-a-shell-job");
    }
    st.phase = Phase::Failed("not-a-shell-job".to_string());
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

/// What a new process holding the conversation of a restart in flight is to
/// that restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Relaunch {
    /// The relaunch the upgrade typed: carried on with.
    Ours,
    /// PROVEN not the relaunch: another conversation, the process that was
    /// ended, a parent READ as another than the recorded shell, or no shell
    /// recorded at all.
    NotOurs,
    /// Its parent could not be read now (it ended as it was asked, or `ps`
    /// could not run): no verdict either way.
    Unread,
}

/// [`Relaunch`] from the new process's parent as read now (`None`: unread)
/// and the shell the relaunch line was typed at (`0`: none was recorded).
fn relaunch_by_parent(parent: Option<u32>, shell: u32) -> Relaunch {
    match parent {
        _ if shell == 0 => Relaunch::NotOurs,
        None => Relaunch::Unread,
        Some(p) if p == shell => Relaunch::Ours,
        Some(_) => Relaunch::NotOurs,
    }
}

/// Whether `s` is the relaunch this upgrade typed: a process other than the
/// one it ended, holding the SAME conversation, and a child of the shell the
/// line was typed at (the line runs the build as that shell's child — directly,
/// or `exec`ed by a `( cd … )` subshell of it). Neither half alone will do: a
/// person's fresh `claude` at that prompt after a relaunch that failed has the
/// shell but not the conversation (measured 2026-09-23: adopted, told it was
/// upgraded and resumed, its conversation filed `done`), and the conversation
/// resumed by hand in another tab has the conversation but not the shell. A
/// parent that cannot be read is [`Relaunch::Unread`], never `NotOurs`: the
/// in-flight branch of [`visit`] records a failure from `NotOurs` for good.
fn relaunch_of(k: &dyn Kernel, s: &SessionFile, session: &str, old: u32, shell: u32) -> Relaunch {
    if s.pid == old || s.session_id != session {
        return Relaunch::NotOurs;
    }
    relaunch_by_parent(k.parent(s.pid), shell)
}

fn first_word(s: &str) -> String {
    s.split_whitespace().take(2).collect::<Vec<_>>().join("-")
}

/// Poll `cond` every 250 ms for at most `limit`.
fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    cond()
}

/// A relaunch planned from the live process: the shell that gets its prompt
/// back, and the one line typed at it.
struct Plan {
    shell: u32,
    line: String,
}

/// Why a session has no relaunch plan.
enum NoPlan {
    /// A fact this sweep could not read (one word): nothing is typed, and a
    /// later sweep asks again.
    Wait(&'static str),
    /// A launch that can never be carried onto this target.
    Refused {
        /// The `Failed` reason (`argv:…`, `line:…`).
        why: String,
        /// What the step names after `refused:`.
        what: String,
        /// The ledger line's detail.
        detail: String,
    },
}

/// THE RELAUNCH, PLANNED from the live process: `shell` — the parent the job
/// read just proved holds the agent as its foreground job ([`foreground_shell`];
/// never re-derived here, so the plan and the proof name one process) — its
/// dialect, the heal it needs, the directory the conversation resumes in, and
/// from those the one line ([`line_for`]). Planned TWICE: before the
/// announcement ([`announce`]), so a launch that can never be carried is refused
/// before the agent is asked to wind down, and in [`restart`] before the one
/// signal, which is the last word.
fn plan(
    opts: &Opts,
    sf: &SessionFile,
    shell: u32,
    argv: &[String],
    target: &Candidate,
    t: &[(u32, u32, String)],
) -> Result<Plan, NoPlan> {
    let shell_name = atpkg::caller_shell::process_args(shell)
        .map(|a| a.exec_path)
        .or_else(|| {
            t.iter()
                .find(|(p, _, _)| *p == shell)
                .map(|(_, _, n)| n.clone())
        });
    let Some(dialect) = shell_name.as_deref().and_then(Dialect::from_exe_name) else {
        return Err(NoPlan::Wait("shell-dialect"));
    };
    let heal = atpkg::hooks::hook_file(&opts.home, dialect.hook_ext()).map(|(p, _)| p);
    // `--resume` finds a conversation under the directory it was started in.
    let want = if sf.cwd.is_empty() {
        cwd_of(sf.pid).unwrap_or_default()
    } else {
        sf.cwd.clone()
    };
    let cd = (!want.is_empty() && cwd_of(shell).as_deref() != Some(want.as_str())).then_some(want);
    let line = line_for(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        &target.exe,
        argv,
        &sf.session_id,
    )?;
    Ok(Plan { shell, line })
}

/// The pure half of [`plan`]: the launch flags carried into a resume
/// ([`upgrade::rewrite_argv`]) and the line that runs them
/// ([`upgrade::relaunch_line`]), or the refusal either one makes.
fn line_for(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    argv: &[String],
    session: &str,
) -> Result<String, NoPlan> {
    let flags = upgrade::rewrite_argv(argv, session).map_err(|e| NoPlan::Refused {
        why: format!("argv:{e}"),
        what: e.to_string(),
        detail: "the launch flags cannot be carried into a resume".to_string(),
    })?;
    upgrade::relaunch_line(dialect, heal, cd, exe, &flags).map_err(|e| NoPlan::Refused {
        why: format!("line:{e}"),
        what: "line".to_string(),
        detail: e,
    })
}

/// A session with no relaunch plan. A wait types and records nothing. A
/// refusal FAILS the upgrade to this target (not retried for it) with one
/// ledger line — `would-refuse:` in a dry run, which records nothing.
fn unplanned(opts: &Opts, r: Report, st: &mut St, no: NoPlan) -> Report {
    match no {
        NoPlan::Wait(why) => said(r, format!("wait:{why}")),
        NoPlan::Refused { what, .. } if opts.dry_run => said(r, format!("would-refuse:{what}")),
        NoPlan::Refused { why, what, detail } => {
            st.phase = Phase::Failed(why);
            let r = said(r, format!("refused:{what}"));
            ledger(opts, &r, &detail);
            r
        }
    }
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
            st.marker = marker;
            st.phase = Phase::Announced { at_s: now, asks };
            let r = said(r, format!("announced:{asks}"));
            ledger(opts, &r, &st.marker);
            r
        }
        Err(e) => said(r, format!("wait:announce-refused:{}", first_word(&e))),
    }
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
    let Plan { shell, line } = match plan(opts, sf, proven, argv, target, t) {
        Ok(p) => p,
        Err(no) => return unplanned(opts, r, st, no),
    };
    // The last look before the one irreversible act: the same process, still
    // idle, still its shell's foreground job, the composer still empty, nothing
    // running under it, nobody's hand.
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
        && !held(c, tab)
        && host_claim.is_none_or(|claim| claim_still_live(c, sf.pid, claim));
    if !still {
        return said(r, "wait:changed");
    }
    if let Err(why) = require_unique_owner(&opts.home, sf) {
        return said(r, format!("wait:{why}"));
    }
    let Ok(pid) = libc::pid_t::try_from(sf.pid) else {
        return said(r, "wait:pid");
    };
    let announced = st.phase.clone();
    st.pid = sf.pid;
    st.shell = shell;
    st.tab = tab.to_string();
    st.line = line;
    st.phase = Phase::Exiting { at_s: now_s() };
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
    if !owns_notice
        || require_unique_owner(&opts.home, sf).is_err()
        || foreground_shell(k.job(sf.pid)) != Ok(shell)
        || !owner.is_some_and(owned_by_aterm)
        || host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim))
    {
        st.phase = announced;
        save(opts, &sf.session_id, st);
        return said(r, "wait:changed-before-signal");
    }
    // SAFETY: a plain signal to one verified pid of our own uid.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        st.phase = Phase::Failed("signal-refused".to_string());
        save(opts, &sf.session_id, st);
        return said(r, "failed:signal-refused");
    }
    ledger(opts, &said(r.clone(), "terminated"), "SIGTERM");
    relaunch(opts, r, st, c, &sf.session_id, k)
}

/// The agent was signalled: once it is gone and the shell has its prompt
/// back, type the relaunch line.
fn relaunch(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    session: &str,
    k: &dyn Kernel,
) -> Report {
    let home = opts.home.clone();
    let old = st.pid;
    let gone = wait_until(Duration::from_secs(30), || {
        !alive(old) && !home.join(format!(".claude/sessions/{old}.json")).exists()
    });
    if !gone {
        // Never a harder signal: the agent finishes exiting on its own, and a
        // later sweep finds the phase and the process as they are.
        return said(r, "wait:exiting");
    }
    let shell = st.shell;
    let prompt_back = wait_until(Duration::from_secs(15), || {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    });
    if !prompt_back {
        return said(r, "wait:shell-prompt");
    }
    // The shell draws its prompt after it takes the terminal back.
    std::thread::sleep(Duration::from_millis(600));
    let tab = st.tab.clone();
    match type_relaunch_line(opts, c, shell, &tab, session, &st.line, |c, pid, tab| {
        process_in_tab(c, pid, tab, None, None)
    }) {
        Ok(()) => {}
        Err(RelaunchLineError::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(RelaunchLineError::Turn(e)) => {
            st.phase = Phase::Failed("relaunch-refused".to_string());
            let r = said(r, format!("failed:relaunch:{}", first_word(&e)));
            ledger(opts, &r, &st.line);
            return r;
        }
    }
    st.phase = Phase::Relaunched { at_s: now_s() };
    ledger(opts, &said(r.clone(), "relaunched"), &st.line);
    save(opts, session, st);
    await_new(opts, r, st, c, session, k)
}

#[derive(Debug, PartialEq, Eq)]
enum RelaunchLineError {
    Wait(&'static str),
    Turn(String),
}

/// The complete session scan can outlast a foreground-shell claim. Probe the
/// PTY again immediately before typing, and expose that second read to a
/// deterministic race test through the same seam used by production.
#[allow(clippy::too_many_arguments)]
fn type_relaunch_line(
    opts: &Opts,
    c: &mut Client,
    shell: u32,
    tab: &str,
    session: &str,
    line: &str,
    mut tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<(), RelaunchLineError> {
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    if session_files(&opts.home).is_none_or(|files| conversation_live(&files, session)) {
        return Err(RelaunchLineError::Wait("conversation-owner-ambiguous"));
    }
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    turn(c, tab, line).map_err(RelaunchLineError::Turn)
}

/// After the relaunch line: find the new process holding the conversation —
/// the relaunch itself ([`relaunch_of`]), nothing else. A parent that cannot
/// be read yet is not found yet: the wait goes on.
fn await_new(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    session: &str,
    k: &dyn Kernel,
) -> Report {
    let home = opts.home.clone();
    let (old, shell) = (st.pid, st.shell);
    let mut new: Option<SessionFile> = None;
    wait_until(Duration::from_secs(90), || {
        new = session_files(&home)
            .unwrap_or_default()
            .into_iter()
            .find(|s| relaunch_of(k, s, session, old, shell) == Relaunch::Ours);
        new.is_some()
    });
    match new {
        Some(sf) => carry_on(opts, r, st, c, session, &sf),
        None => {
            if let Phase::Relaunched { at_s } = st.phase
                && now_s().saturating_sub(at_s) > STALE_S
            {
                st.phase = Phase::Failed("no-resume".to_string());
                let r = said(r, "failed:no-resume");
                ledger(
                    opts,
                    &r,
                    "the relaunched agent never registered the conversation",
                );
                return r;
            }
            said(r, "wait:resume")
        }
    }
}

/// The new process holds the conversation: once it settles, tell it to carry on.
/// `key` is the conversation the upgrade's state is filed under.
fn carry_on(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    key: &str,
    new: &SessionFile,
) -> Report {
    carry_on_with_tab_probe(opts, r, st, c, key, new, |c, pid, tab| {
        process_in_tab(c, pid, tab, None, None)
    })
}

/// The tab probe is a seam for the race between the first ownership read and
/// the final typed continuation. Production uses the kernel-backed PTY claim
/// for BOTH reads; a test changes its answer after the first one.
#[allow(clippy::too_many_arguments)]
fn carry_on_with_tab_probe(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    key: &str,
    new: &SessionFile,
    mut tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Report {
    r.pid = new.pid;
    let tab = st.tab.clone();
    let tab = tab.as_str();
    let home = opts.home.clone();
    let mut conversation_changed = false;
    let settled = wait_until(Duration::from_secs(120), || {
        let Some(sf) = session_file_of(&home, new.pid) else {
            return false;
        };
        if sf.session_id != key {
            conversation_changed = true;
            return true;
        }
        sf.status == "idle" && screen(c, tab).is_some_and(|s| composer_empty(c, tab, &s))
    });
    if conversation_changed {
        return said(r, "wait:conversation-changed");
    }
    let Some(current) = continuation_session(&home, new.pid, key) else {
        return said(r, "wait:conversation-changed");
    };
    let from = Version::parse(&st.from);
    if settled
        && (require_unique_owner(&home, &current).is_err()
            || kernel_start(new.pid).as_deref() != Some(squash(&new.proc_start).as_str())
            || squash(&current.proc_start) != squash(&new.proc_start)
            || !tab_probe(c, new.pid, tab))
    {
        return said(r, "wait:tab-ownership-changed");
    }
    let Some(latest) = continuation_session(&home, new.pid, key) else {
        return said(r, "wait:conversation-changed");
    };
    if latest.status != "idle"
        || squash(&latest.proc_start) != squash(&new.proc_start)
        || kernel_start(new.pid).as_deref() != Some(squash(&latest.proc_start).as_str())
    {
        return said(r, "wait:changed");
    }
    if let Err(why) = require_unique_owner(&home, &latest) {
        return said(r, format!("wait:{why}"));
    }
    let to = Version::parse(&latest.version);
    let typed = settled
        && match (&from, &to) {
            (Some(f), Some(t)) => {
                if !tab_probe(c, new.pid, tab) {
                    return said(r, "wait:tab-ownership-changed");
                }
                turn(c, tab, &upgrade::continue_prompt(f, t)).is_ok()
            }
            _ => false,
        };
    st.phase = Phase::Done;
    r.from.clone_from(&st.from);
    r.to = format!("{}({})", latest.version, st.source);
    let r = said(r, if typed { "done" } else { "done:no-continue" });
    ledger(opts, &r, &latest.session_id);
    save(opts, key, st);
    r
}

fn continuation_session(home: &Path, pid: u32, key: &str) -> Option<SessionFile> {
    session_file_of(home, pid).filter(|sf| sf.session_id == key)
}

#[cfg(test)]
#[path = "upgrade_drive_tests.rs"]
mod tests;
