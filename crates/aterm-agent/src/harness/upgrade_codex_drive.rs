// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE CODEX BRANCH OF THE UPGRADE STEP — its I/O half
//! ([`super::super::upgrade_codex`] is its pure half), under the step's own
//! lock and ledger. The window's host takes it where it takes a Claude Code
//! session's step: in the session's own worker, at its loop's idle point
//! ([`super::step`], scoped to the tab); a hand-run `aterm harness upgrade`
//! takes it for every Codex at once ([`super::sweep`]). Either way:
//!
//! 1. **THE DAEMONS FIRST.** For `~/.codex` and every `$CODEX_HOME` a live
//!    Codex TUI runs with, the daemon `app-server-daemon/daemon.pid` names —
//!    proven the daemon by its argv (`app-server … --managed-daemon`) and its
//!    executable (under that home's `packages/app-server-daemon/releases/`) —
//!    is read from files and the kernel alone: its package's version, whether
//!    the vendor's updater is armed (`auto-update-version`), and every thread
//!    it holds (its `thread-writer-locks/<id>.lock` open in `lsof`, attached
//!    to a tab or not) with that thread's rollout. Only when
//!    [`cx::daemon_step`] says so is anything run: `<managed codex>
//!    app-server daemon update --from-cli --yes`, with the environment the
//!    running daemon was started with (read from the kernel), never the
//!    window's — the daemon it restarts runs every command of every
//!    daemon-mode session, and those must not change environment under them.
//!    Bounded, and never a kill of the daemon.
//!    The window's step reaches the daemon from ONE tab, and asks every Codex
//!    tab of the window the daemon serves (its hold, its person, the owner's
//!    word on it) before it moves.
//! 2. **THEN THE CLIENTS.** Every Codex TUI that is the foreground job of a
//!    tab this pass serves, on a build older than the managed one, is moved
//!    at an idle point ([`cx::requested_step`]) by a TYPED `/exit`, fenced on
//!    the screen it was judged by and guarded on the composer's row — a
//!    daemon-mode client at once (its daemon keeps the thread), an embedded
//!    session after the notice and its READY answer — and relaunched at the
//!    returned prompt as `<twin> resume <flags> <thread>`, the thread read
//!    from the TUI's own exit hint (the shell integration's block for the
//!    command, else the rows directly above the new prompt), or from the lock
//!    the kernel proved it held — typed through the ONE relaunch line
//!    ([`super::super::relaunch`]'s `type_relaunch_line_with`, the thread's
//!    writer lock standing in for Claude Code's session file). Under the
//!    window (`Opts::hand_back`) the new TUI is handed back to its supervisor
//!    (`adopted`) and carried on at the loop's next idle point, as a Claude
//!    Code restart is. No signal is ever sent to a Codex process.
//! 3. **THEN THE RESTARTS LEFT IN FLIGHT** whose TUI is gone.
//!
//! A Codex upgrade's state is filed per TAB (`<state>/upgrade/codex-<tab>.json`):
//! a daemon-mode client's thread is unknown until it exits, and at most one
//! foreground Codex lives in a tab. Every Claude loop passes those files by.

use std::collections::BTreeSet;
use std::io::BufRead as _;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::super::relaunch::first_idle;
use super::super::upgrade_codex::{self as cx, DaemonFacts, DaemonStep, ExitHint, Mode, TurnState};
use super::upgrade::{Agent, Dialect, Facts, Phase, Step, Version};
use super::{
    Behind, Client, Due, Hand, HostClaim, Job, Kernel, Live, LiveTab, NoPlan, Opts, Plan,
    RelaunchLineError, Report, STALE_S, St, TAIL_BYTES, TURN_WAIT, Typed, UNREAD_PROCESS,
    UNREADABLE_TABLE, alive, attended_once, background_procs, base_name, cell_is_dim,
    claim_still_live, connect, cwd_of, drain_expired, exe_of, first_word, foreground_shell,
    gave_up_words, held, held_back, held_by_status, held_of, ids, is_generation, kernel_start,
    ledger, live_background, load, not_a_job, now_s, owned_by_aterm, owner_word, process_in_tab,
    quiet_s, roster, said, save, screen, squash, state_dir, status_line, table, tail_to_end,
    turn_takes_gen, turn_verdict, type_relaunch_line_with, unique_tab_for_group, unplanned,
    upgrade, wait_until,
};
use crate::supervise::policy::{key_args, row_guard, server_fences_gen};

// ---------------------------------------------------------------- the kernel

/// THE PROCESS FACTS the Codex lane rests on, each read at the moment it is
/// asked: the Claude lane's [`Kernel`] (the job, the parent, the terminal's
/// owner) and what Codex needs besides. A trait so a driver test can script a
/// TUI that exits when `/exit` is typed and a relaunch that appears when its
/// line is — no real Codex can be made to do either on cue.
pub(super) trait CodexKernel: Kernel {
    /// Whether `pid` lives.
    fn alive(&self, pid: u32) -> bool;
    /// `pid`'s argv and initial environment (`KERN_PROCARGS2`).
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs>;
    /// The executable the kernel mapped for `pid`.
    fn exe(&self, pid: u32) -> Option<PathBuf>;
    /// Every file `pid` holds open (`lsof`), `None` when unreadable.
    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>>;
    /// Every process holding `file` open, `None` when unreadable.
    fn holders_of(&self, file: &Path) -> Option<Vec<u32>>;
    /// Whether `shell`'s group holds its terminal again (`pgid == tpgid`).
    fn shell_has_terminal(&self, shell: u32) -> bool;
    /// The live `codex` processes whose parent is `shell`, with their argv.
    fn codex_children(&self, shell: u32) -> Vec<(u32, Vec<String>)>;
    /// The background work under `pid` ([`super::background`]).
    fn background(&self, pid: u32) -> Vec<String>;
    /// That work NAMED for the notice and the give-up's ledger row
    /// ([`upgrade::running_clause`], [`super::held_of`]). The default names
    /// nothing.
    fn held(&self, pid: u32) -> Vec<upgrade::Held> {
        let _ = pid;
        Vec::new()
    }
    /// The BACKGROUND TERMINALS under `pid` ([`cx::terminals_under`]): the
    /// descendants that lead a session of their own, by name.
    fn terminals(&self, pid: u32) -> Vec<String>;
    /// The Codex processes attached to the daemon `daemon`
    /// ([`cx::daemon_clients_in`]), `None` when that cannot be read.
    fn daemon_clients(&self, daemon: u32) -> Option<Vec<u32>>;
    /// `pid`'s working directory.
    fn cwd(&self, pid: u32) -> Option<String>;
    /// Whether `pid` is still the foreground group of `tab` by the host's own
    /// roster ([`process_in_tab`]): asked right before every typed act.
    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool;
}

impl CodexKernel for Live {
    fn alive(&self, pid: u32) -> bool {
        alive(pid)
    }

    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        atpkg::caller_shell::process_args(pid)
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        exe_of(pid)
    }

    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>> {
        let out = Command::new("lsof")
            .args(["-n", "-P", "-w", "-p", &pid.to_string(), "-Fn"])
            .output()
            .ok()?;
        // lsof exits 1 for a pid it found nothing for: an unreadable answer.
        if !out.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.strip_prefix('n').map(PathBuf::from))
                .collect(),
        )
    }

    fn holders_of(&self, file: &Path) -> Option<Vec<u32>> {
        let out = Command::new("lsof")
            .args(["-n", "-P", "-w", "-t"])
            .arg(file)
            .output()
            .ok()?;
        // Exit 1 with nothing on stdout is "no process has it open".
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() && !text.trim().is_empty() {
            return None;
        }
        Some(
            text.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect(),
        )
    }

    fn shell_has_terminal(&self, shell: u32) -> bool {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    }

    fn codex_children(&self, shell: u32) -> Vec<(u32, Vec<String>)> {
        table()
            .into_iter()
            .filter(|(_, ppid, name)| *ppid == shell && name == "codex")
            .filter_map(|(pid, _, _)| Some((pid, atpkg::caller_shell::process_args(pid)?.argv)))
            .collect()
    }

    fn background(&self, pid: u32) -> Vec<String> {
        live_background(pid)
    }

    fn held(&self, pid: u32) -> Vec<upgrade::Held> {
        held_of(pid, &background_procs(pid, &table()))
    }

    // FAIL CLOSED, as `live_background`: a table `ps` could not give reads as a
    // terminal still running, so the embedded exit's last look waits.
    fn terminals(&self, pid: u32) -> Vec<String> {
        let t = table();
        if t.is_empty() {
            return vec![UNREADABLE_TABLE.to_string()];
        }
        cx::terminals_under(pid, &t, session_of)
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }

    fn daemon_clients(&self, daemon: u32) -> Option<Vec<u32>> {
        // The unix sockets of every process named `codex` (`-c` is a name
        // prefix; `-a` ANDs it with `-U`): one read, a few processes.
        let out = Command::new("lsof")
            .args(["-a", "-n", "-P", "-w", "-U", "-c", "codex", "-F", "pcdn"])
            .output()
            .ok()?;
        cx::daemon_clients_in(&String::from_utf8_lossy(&out.stdout), daemon)
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        cwd_of(pid)
    }

    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool {
        process_in_tab(c, pid, tab, None, None)
    }
}

/// The kernel's session id of `pid` (`getsid`), `None` when it cannot be
/// read. macOS answers it for a process of any session the caller may see
/// (measured 2026-09-26 on a daemon's children from another session).
fn session_of(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        let p = libc::pid_t::try_from(pid).ok()?;
        // SAFETY: getsid reads one process's session id; nothing is changed.
        let sid = unsafe { libc::getsid(p) };
        u32::try_from(sid).ok().filter(|s| *s > 0)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

// ---------------------------------------------------------------- the target

/// The managed Codex: atpkg's twin (`<prefix>/agents/codex`, the POSIX shim a
/// relaunch line runs, as every managed relaunch does) and the store build it
/// execs, whose package names the version.
#[derive(Clone, Debug)]
pub(super) struct Target {
    /// `<prefix>/agents/codex`.
    pub(super) twin: PathBuf,
    /// The store build the twin execs (`…/store/codex/<build>/bin/codex`).
    pub(super) exe: PathBuf,
    /// Its package's version.
    pub(super) version: Version,
}

impl Target {
    /// Read from the configured store. `None`: no managed Codex, or one whose
    /// package names no version.
    fn read() -> Option<Target> {
        let twin = atpkg::store::resolve_configured()?
            .agents_dir()
            .join("codex");
        Self::of_twin(&twin)
    }

    /// The target a twin at `twin` names.
    pub(super) fn of_twin(twin: &Path) -> Option<Target> {
        let exe = super::catalog::twin_binary(&std::fs::read_to_string(twin).ok()?)?;
        let version = cx::package_version(&exe)?;
        Some(Target {
            twin: twin.to_path_buf(),
            exe,
            version,
        })
    }
}

// ---------------------------------------------------------------- discovery

/// One Codex TUI this sweep found in a tab.
#[derive(Clone, Debug)]
pub(super) struct Tui {
    pub(super) pid: u32,
    pub(super) tab: String,
    /// The host's PTY-group claim (the window's sweep), `None` for a hand-run
    /// sweep, which proves the tab by the TUI's environment alone.
    pub(super) claim: Option<HostClaim>,
    pub(super) argv: Vec<String>,
    /// The version of the build it runs (its package's).
    pub(super) version: Version,
    /// The `$CODEX_HOME` it runs with (canonical).
    pub(super) home: PathBuf,
}

/// The `$CODEX_HOME` a process started with `env` uses: `CODEX_HOME`, else
/// `$HOME/.codex`, else the sweep's own `<home>/.codex` — canonical when it
/// exists, since `lsof` names what it finds under it canonically
/// (`/private/tmp/…` for `/tmp/…`).
fn home_of(env: &atpkg::caller_shell::ProcArgs, fallback: &Path) -> PathBuf {
    let home = env
        .env_var("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            env.env_var("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".codex"))
        })
        .unwrap_or_else(|| fallback.join(".codex"));
    canonical(&home)
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// A candidate before its facts are read: the pid and the tab it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Found {
    pub(super) pid: u32,
    pub(super) tab: String,
    pub(super) claim: Option<HostClaim>,
}

/// THE CANDIDATES: with the host's roster, each tab whose foreground group's
/// LEADER is a process named `codex` (a job-control shell's job leads its own
/// group, and the twin `exec`s the store build, so the leader IS the TUI) —
/// claimed by that group, and never where two tabs name the same group; with
/// none (a hand-run sweep), each process named `codex` whose environment
/// names its tab (`ATERM_PARENT_SESSION_ID`) AND that leads its terminal's
/// foreground group. Either way at most ONE per tab: the upgrade's state is
/// filed per tab, and a Codex suspended under another (ctrl-z, then a second
/// `codex`) must never write over the notice the foreground one answers.
/// `procs` is `(pid, ppid, name)`.
pub(super) fn candidates(
    live_tabs: Option<&[LiveTab]>,
    procs: &[(u32, u32, String)],
    mut read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
    mut env_tab: impl FnMut(u32) -> Option<String>,
) -> Vec<Found> {
    let named = |pid: u32| procs.iter().any(|(p, _, n)| *p == pid && n == "codex");
    match live_tabs {
        Some(tabs) => tabs
            .iter()
            .filter_map(|tab| {
                let group = tab.fgpgid?;
                let leader = u32::try_from(group).ok()?;
                if !named(leader) || unique_tab_for_group(tabs, group) != Some(tab.sid.as_str()) {
                    return None;
                }
                let (parent, pgid, tpgid) = read_ids(leader)?;
                if pgid != group || tpgid != group {
                    return None;
                }
                if env_tab(leader).is_some_and(|env| env != tab.sid) {
                    return None;
                }
                Some(Found {
                    pid: leader,
                    tab: tab.sid.clone(),
                    claim: Some(HostClaim {
                        tab: tab.sid.clone(),
                        group,
                        parent,
                    }),
                })
            })
            .collect(),
        None => {
            let mut out: Vec<Found> = Vec::new();
            for (pid, _, _) in procs.iter().filter(|(_, _, n)| n == "codex") {
                let Some(tab) = env_tab(*pid).filter(|t| t.starts_with("s-")) else {
                    continue;
                };
                let leads = read_ids(*pid)
                    .is_some_and(|(_, pgid, tpgid)| pgid == i64::from(*pid) && tpgid == pgid);
                if !leads {
                    continue;
                }
                out.push(Found {
                    pid: *pid,
                    tab,
                    claim: None,
                });
            }
            // Two foreground leaders claiming one tab: ambiguity, never a
            // guess — neither is visited.
            let claims = out.clone();
            out.retain(|f| claims.iter().filter(|g| g.tab == f.tab).count() == 1);
            out
        }
    }
}

/// The TUIs among the candidates, each with its argv, build, version and
/// home read; a candidate that is no TUI (`exec`, the daemon, which carries
/// its launcher's environment) is left out, and one whose facts cannot be
/// read is a report.
fn tuis(opts: &Opts, found: Vec<Found>, k: &dyn CodexKernel) -> (Vec<Tui>, Vec<Report>) {
    let mut tuis = Vec::new();
    let mut reports = Vec::new();
    for f in found {
        let report = |step: &str| Report {
            pid: f.pid,
            tab: f.tab.clone(),
            session: key(&f.tab),
            from: "-".to_string(),
            to: "-".to_string(),
            step: step.to_string(),
        };
        if opts.only_sid.as_ref().is_some_and(|s| *s != f.tab) {
            continue;
        }
        let Some(args) = k.args(f.pid) else {
            reports.push(report("wait:argv-unreadable"));
            continue;
        };
        // THE SWEEP SERVES ITS OWN HOME'S CODEX: one started with another
        // `$HOME` (a sandboxed throwaway, another account's) is not this
        // owner's to move — and a test's sweep, whose home is a scratch
        // directory, never reaches a Codex the machine really runs.
        let own = canonical(&opts.home);
        if args.env_var("HOME").map(|h| canonical(Path::new(h))) != Some(own) {
            if f.claim.is_some() {
                reports.push(report("skip:another-home"));
            }
            continue;
        }
        if !cx::is_tui_argv(&args.argv) {
            // The daemon and its updater carry the environment of the TUI
            // that started them; `exec` is no session. Only a foreground
            // one in a tab is said.
            if f.claim.is_some() {
                reports.push(report("skip:not-a-tui"));
            }
            continue;
        }
        let Some(exe) = k.exe(f.pid) else {
            reports.push(report("wait:exe-unreadable"));
            continue;
        };
        let Some(version) = cx::package_version(&exe) else {
            reports.push(report("wait:version-unreadable"));
            continue;
        };
        let home = home_of(&args, &opts.home);
        tuis.push(Tui {
            pid: f.pid,
            tab: f.tab,
            claim: f.claim,
            argv: args.argv,
            version,
            home,
        });
    }
    (tuis, reports)
}

/// The state key of the Codex upgrade in `tab`.
pub(super) fn key(tab: &str) -> String {
    format!("codex-{tab}")
}

// ---------------------------------------------------------------- the sweep

/// The tab id a process's environment names (`ATERM_PARENT_SESSION_ID`).
fn env_tab(pid: u32) -> Option<String> {
    atpkg::caller_shell::process_args(pid)
        .and_then(|a| a.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned))
}

/// ONE PASS of the Codex branch (module header): the daemons, the clients,
/// the restarts left in flight. Scoped to one tab (`opts.only_sid`: the
/// window's step, taken by that tab's worker at its idle point), it visits
/// that tab's TUI alone — but the daemon it runs on is asked about EVERY
/// Codex tab it can see, since the daemon serves them all and each is asked
/// (its hold, its person, the owner's word on it) before the daemon moves.
pub(super) fn sweep(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    if live_tabs.is_some_and(<[LiveTab]>::is_empty) {
        return Vec::new();
    }
    let procs = table();
    let found = candidates(live_tabs, &procs, ids, env_tab);
    let k = Live;
    let every = Opts {
        only_sid: None,
        ..opts.clone()
    };
    let (tuis, mut reports) = tuis(&every, found, &k);
    reports.retain(|r| opts.only_sid.as_ref().is_none_or(|sid| *sid == r.tab));
    let target = Target::read();
    sweep_with(opts, live_tabs, tuis, target.as_ref(), &k, &mut reports);
    reports
}

/// Whether the tab `opts.only_sid` has a Codex move to take — the host's
/// question at a worker's start, at every activation notice and at every
/// look ([`super::due`]): a Codex TUI leads the tab on a build older than
/// the managed Codex, or on a daemon that is behind it or still runs the
/// vendor's updater. Read-only, and cheap: the tab's foreground leader's
/// name first (no process table), then its package and the daemon's files.
/// `None` when no Codex leads the tab ([`tab_tui`]) — the tab is then the
/// Claude Code half's to read; a Codex whose process could not be read
/// whole is [`super::UNREAD_PROCESS`] (the review of the silent-drop fix:
/// read as no Codex at all, it fell to the Claude Code half, which found no
/// record of it); one this upgrade does not move, or with no managed Codex
/// to move to, is `No`. Every `Some` is the Codex lane's conclusive answer:
/// Claude's session records cannot change it and are not scanned
/// ([`super::due`]'s roster read is skipped), even when no managed Codex
/// target is installed.
pub(super) fn due(opts: &Opts, tabs: &[LiveTab]) -> Option<Due> {
    due_with(opts, tabs, &Live, Target::read, ids, env_tab)
}

/// [`due`]'s testable read seam: the foreground identified as the tab's
/// Codex ([`tab_tui`]) BEFORE the managed target is read, with the same
/// owner/TTY checks as the upgrade sweep. A foreground that is no Codex, or
/// one no tab owns alone, stays `None`, never a reason to omit Claude's pass.
fn due_with(
    opts: &Opts,
    tabs: &[LiveTab],
    k: &dyn CodexKernel,
    target: impl FnOnce() -> Option<Target>,
    read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
    read_env_tab: impl FnMut(u32) -> Option<String>,
) -> Option<Due> {
    Some(match tab_tui(opts, tabs, k, read_ids, read_env_tab)? {
        TabTui::Tui(tui) => {
            let moves = target().is_some_and(|target| {
                tui.version < target.version || daemon_moves(&tui.home, &target, k)
            });
            if moves { Due::Yes } else { Due::No }
        }
        TabTui::NotOurs => Due::No,
        TabTui::Unread => Due::Unread(UNREAD_PROCESS),
    })
}

/// What a look read of the Codex that leads a tab ([`tab_tui`]).
enum TabTui {
    /// Its TUI, read whole.
    Tui(Tui),
    /// A Codex this upgrade does not move: another home's, no TUI (`exec`,
    /// the daemon), one whose build nobody can name, or another tab's by its
    /// environment.
    NotOurs,
    /// A Codex whose process could not be read whole — its ids, its argv or
    /// its executable, or the tab's foreground moved between two reads.
    Unread,
}

/// The Codex that leads the tab `opts.only_sid`, read as [`due`] reads it,
/// the process reads injected (`k`, `read_ids`, `read_env_tab`): the
/// foreground leader's name first (no process table), then its ids and its
/// package. `None` when the leader is no process named `codex`, or its argv
/// cannot say which: that read is a sysctl, not a spawn — a live process of
/// the owner's answers it, and one that does not (another user's, a leader
/// gone from its group) is none of this upgrade's; the Claude Code half
/// reads the tab then. `None` too when the group leads another tab besides
/// this one: a name alone does not prove which tab owns the process, so the
/// Claude fallback stays open.
fn tab_tui(
    opts: &Opts,
    tabs: &[LiveTab],
    k: &dyn CodexKernel,
    mut read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
    read_env_tab: impl FnMut(u32) -> Option<String>,
) -> Option<TabTui> {
    let sid = opts.only_sid.as_deref()?;
    let group = tabs.iter().find(|t| t.sid == sid).and_then(|t| t.fgpgid)?;
    let leader = u32::try_from(group).ok()?;
    let name = k
        .args(leader)
        .map(|a| base_name(&a.exec_path))
        .filter(|n| n == "codex")?;
    if unique_tab_for_group(tabs, group) != Some(sid) {
        // A duplicate foreground group proves no Codex owner: never read as
        // this tab's Codex, nor as nothing of this upgrade's.
        return None;
    }
    let procs = [(leader, 0, name)];
    // Its ids unread, or caught as the foreground moved: nothing read.
    let mut unread = false;
    let found: Vec<Found> = candidates(
        Some(tabs),
        &procs,
        |pid| {
            let read = read_ids(pid).filter(|&(_, pgid, front)| pgid == group && front == group);
            unread |= read.is_none();
            read
        },
        read_env_tab,
    )
    .into_iter()
    .filter(|f| f.tab == sid)
    .collect();
    if found.is_empty() {
        return Some(if unread {
            TabTui::Unread
        } else {
            TabTui::NotOurs
        });
    }
    let (tuis, reports) = tuis(opts, found, k);
    Some(match tuis.into_iter().next() {
        Some(tui) => TabTui::Tui(tui),
        // [`tuis`]' own waits for a process it could not read.
        None if reports.iter().any(|r| {
            matches!(
                r.step.as_str(),
                "wait:argv-unreadable" | "wait:exe-unreadable"
            )
        }) =>
        {
            TabTui::Unread
        }
        None => TabTui::NotOurs,
    })
}

/// [`super::note_behind`]'s Codex half (N3 of the live re-test of
/// 2026-09-26): the Codex TUI that leads the tab ([`tab_tui`]) on a build
/// older than the managed Codex gets its state minted as its first visit
/// would mint it ([`minted`]), behind since `since` — where no state of this
/// TUI's stands, or the one that stands is finished with another build
/// ([`super::behind_anew`]). A restart in flight in the tab is its own.
/// `None` when no Codex leads the tab (the Claude Code half's to read, as
/// [`due`]); a Codex whose process could not be read whole is
/// [`Behind::Unread`].
pub(super) fn note_behind(opts: &Opts, tabs: &[LiveTab], since: u64) -> Option<Behind> {
    let tui = match tab_tui(opts, tabs, &Live, ids, env_tab)? {
        TabTui::Tui(tui) => tui,
        TabTui::NotOurs => return Some(Behind::Nothing),
        TabTui::Unread => return Some(Behind::Unread(UNREAD_PROCESS)),
    };
    let Some(target) = Target::read().filter(|target| tui.version < target.version) else {
        return Some(Behind::Nothing);
    };
    let session = key(&tui.tab);
    let prior = load(opts, &session).filter(|st| st.agent == Agent::Codex);
    if prior.as_ref().is_some_and(St::in_flight) {
        return Some(Behind::Nothing);
    }
    let (prior, start) = own_prior(&tui, prior);
    if prior
        .as_ref()
        .is_some_and(|st| !super::behind_anew(st, &target.version))
    {
        return Some(Behind::Nothing);
    }
    let mut st = minted(&tui, &target, prior, start, now_s());
    st.pending_since = st.pending_since.min(since);
    save(opts, &session, &st);
    Some(Behind::Noted)
}

/// The tab's state as it is `tui`'s: a state is about ONE TUI process (its
/// pid and kernel start) — another in the tab, the person quit that one and
/// ran this, starts afresh, its own flags and its own answer to a notice.
/// With the TUI's kernel start, squashed.
fn own_prior(tui: &Tui, prior: Option<St>) -> (Option<St>, String) {
    let start = kernel_start(tui.pid)
        .map(|s| squash(&s))
        .unwrap_or_default();
    let prior = prior.filter(|st| st.notice_pid == tui.pid && st.notice_start == start);
    (prior, start)
}

/// The state a visit of `tui` mints for `target` at `now` over its own
/// `prior` ([`own_prior`], started `start`): [`St::for_target`], with the
/// lane's own text it left typed in THIS TUI's composer kept — its to clear,
/// onto whatever target the state is minted for now — and the TUI's
/// identity, tab and home.
fn minted(tui: &Tui, target: &Target, prior: Option<St>, start: String, now: u64) -> St {
    let candidate = upgrade::Candidate {
        exe: target.twin.clone(),
        version: target.version.clone(),
        source: upgrade::Source::Managed,
    };
    let left = prior
        .as_ref()
        .map(|p| p.left_typed.clone())
        .unwrap_or_default();
    let mut st = St::for_target(prior, &tui.version, &candidate, None, now);
    st.left_typed = left;
    st.agent = Agent::Codex;
    st.notice_pid = tui.pid;
    st.notice_start = start;
    st.tab.clone_from(&tui.tab);
    st.codex_home = tui.home.to_string_lossy().into_owned();
    st
}

/// `home`'s daemon read from its files and the kernel alone (no `lsof`):
/// the build it runs (`None`: unreadable) and whether it is PINNED (the
/// vendor's updater gone). `None` when no daemon lives there.
fn daemon_build(home: &Path, k: &dyn CodexKernel) -> Option<(Option<Version>, bool)> {
    let pid = std::fs::read_to_string(home.join("app-server-daemon/daemon.pid"))
        .ok()
        .and_then(|text| cx::parse_daemon_pid(&text))
        .filter(|pid| k.alive(*pid))?;
    let exe = k.exe(pid).map(|e| canonical(&e))?;
    if !exe.starts_with(canonical(&home.join("packages/app-server-daemon/releases"))) {
        return None;
    }
    let pinned = !home
        .join("packages/app-server-daemon/auto-update-version")
        .exists();
    Some((cx::package_version(&exe), pinned))
}

/// Whether `home`'s daemon ([`daemon_build`]) is one the daemon step would
/// move once nothing runs in it: on an older build than the managed Codex,
/// or on it with the vendor's updater armed. `false` when no daemon lives
/// there or it cannot be read.
fn daemon_moves(home: &Path, target: &Target, k: &dyn CodexKernel) -> bool {
    daemon_build(home, k).is_some_and(|(version, pinned)| {
        version.is_some_and(|v| v < target.version || (v == target.version && !pinned))
    })
}

/// [`sweep`] over TUIs already found — every Codex TUI the pass can see, the
/// daemon's to ask; the pass visits those `opts.only_sid` names — and a
/// target already read.
pub(super) fn sweep_with(
    opts: &Opts,
    live_tabs: Option<&[LiveTab]>,
    tuis: Vec<Tui>,
    target: Option<&Target>,
    k: &dyn CodexKernel,
    reports: &mut Vec<Report>,
) {
    let visited: Vec<&Tui> = tuis
        .iter()
        .filter(|t| opts.only_sid.as_ref().is_none_or(|sid| *sid == t.tab))
        .collect();
    let Some(target) = target else {
        // No managed Codex: nothing to move anything onto — said per TUI,
        // never silence.
        reports.extend(visited.iter().map(|t| Report {
            pid: t.pid,
            tab: t.tab.clone(),
            session: key(&t.tab),
            from: t.version.to_string(),
            to: "-".to_string(),
            step: "skip:no-managed-codex".to_string(),
        }));
        return;
    };
    // THE DAEMONS FIRST: the default home (a hand-run pass), and every home
    // a visited TUI runs with — each asked about every TUI it serves. Never
    // at a break of the agent's own background work (`opts.background`),
    // where the notice alone is typed and nothing is moved.
    let mut homes: BTreeSet<PathBuf> = BTreeSet::new();
    if opts.only_sid.is_none() {
        homes.insert(canonical(&opts.home.join(".codex")));
    }
    homes.extend(visited.iter().map(|t| t.home.clone()));
    if opts.background {
        homes.clear();
    }
    let mut daemons: Vec<(PathBuf, DaemonView)> = Vec::new();
    for home in &homes {
        if let Some((report, view)) = daemon_pass(opts, home, target, &tuis, k) {
            reports.push(report);
            daemons.push((home.clone(), view));
        }
    }
    // At a break the daemon is only READ — its client's mode rests on it
    // living — and never stepped.
    if opts.background {
        for tui in &visited {
            if let Some((running, _)) = daemon_build(&tui.home, k) {
                daemons.push((
                    tui.home.clone(),
                    DaemonView {
                        running,
                        wait: String::new(),
                    },
                ));
            }
        }
    }
    // THEN THE CLIENTS.
    for tui in visited {
        let daemon = daemons
            .iter()
            .find(|(h, _)| *h == tui.home)
            .map(|(_, v)| v.clone());
        reports.push(visit(opts, tui, target, daemon, k));
    }
    // THEN THE RESTARTS LEFT IN FLIGHT whose TUI is gone (an idle point's).
    if !opts.background {
        reports.extend(orphans(opts, live_tabs, &tuis, k));
    }
}

// ---------------------------------------------------------------- the daemon

/// What one read of a home's daemon found.
#[derive(Clone, Debug)]
struct DaemonRead {
    pid: u32,
    version: Option<Version>,
    pinned: bool,
    /// Each thread it holds: its turn state and seconds since its rollout
    /// was written (a thread with no rollout yet — no message — is idle).
    threads: Vec<(String, TurnState, u64)>,
    /// The background terminals running under it ([`cx::terminals_under`]).
    terminals: usize,
    /// The environment it was started with.
    env: Vec<String>,
}

/// What a daemon-mode client's visit is told of its home's daemon: the
/// version it runs once this sweep's daemon step is taken, and — while that
/// is behind the target — what the daemon waits on, carried into the
/// client's own wait (`daemon-first:<why>`) so the owner's view names the
/// real blocker (review of 2026-09-26: every client read `daemon-first` and
/// nothing said a detached thread, say, held the daemon for hours).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DaemonView {
    /// The version the daemon runs, `None` when unreadable.
    pub(super) running: Option<Version>,
    /// The daemon step's word while it waits or failed (`busy-thread`,
    /// `background-terminal`, `update-failed`, …); empty otherwise.
    pub(super) wait: String,
}

/// The daemon of `home`, when one lives (module header): `None` for a home
/// with no daemon, a `daemon.pid` naming a dead or recycled pid, or one whose
/// files the kernel will not show.
fn read_daemon(home: &Path, k: &dyn CodexKernel, now: u64) -> Option<DaemonRead> {
    let pid = cx::parse_daemon_pid(
        &std::fs::read_to_string(home.join("app-server-daemon/daemon.pid")).ok()?,
    )?;
    if !k.alive(pid) {
        return None;
    }
    let args = k.args(pid)?;
    if !args.argv.iter().any(|a| a == "app-server")
        || !args.argv.iter().any(|a| a == "--managed-daemon")
    {
        return None;
    }
    let releases = home.join("packages/app-server-daemon/releases");
    let exe = k.exe(pid).map(|e| canonical(&e))?;
    if !exe.starts_with(canonical(&releases)) {
        return None;
    }
    let files = k.open_files(pid)?;
    let locks = canonical(&home.join("thread-writer-locks"));
    let threads = files
        .iter()
        .filter(|f| f.parent() == Some(locks.as_path()))
        .filter_map(|f| cx::lock_thread(&f.file_name()?.to_string_lossy()).map(str::to_owned))
        .map(|thread| {
            let rollout = files
                .iter()
                .find(|f| {
                    f.file_name()
                        .is_some_and(|n| cx::is_rollout_of(&n.to_string_lossy(), &thread))
                })
                .cloned()
                .or_else(|| find_rollout(home, &thread));
            match rollout {
                None => (thread, TurnState::Idle, u64::MAX),
                Some(path) => {
                    let (tail, _) = tail_to_end(&path, TAIL_BYTES);
                    (thread, cx::rollout_turn(&tail), age_s(&path, now))
                }
            }
        })
        .collect();
    Some(DaemonRead {
        pid,
        version: cx::package_version(&exe),
        pinned: !home
            .join("packages/app-server-daemon/auto-update-version")
            .exists(),
        threads,
        terminals: k.terminals(pid).len(),
        env: args.env,
    })
}

/// Seconds since `path` was written.
fn age_s(path: &Path, now: u64) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| now.saturating_sub(d.as_secs()))
}

/// The rollout of `thread` under `home`'s `sessions/<y>/<m>/<d>/`, when one
/// exists: a thread with none has no user message yet, and `codex resume`
/// refuses it (`no rollout found for thread id`, the research).
pub(super) fn find_rollout(home: &Path, thread: &str) -> Option<PathBuf> {
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    for year in dirs(&home.join("sessions")) {
        for month in dirs(&year) {
            for day in dirs(&month) {
                let Ok(entries) = std::fs::read_dir(&day) else {
                    continue;
                };
                for e in entries.flatten() {
                    if cx::is_rollout_of(&e.file_name().to_string_lossy(), thread) {
                        return Some(e.path());
                    }
                }
            }
        }
    }
    None
}

/// The report word for `home`'s daemon: `codex-daemon` for `~/.codex`,
/// `codex-daemon@<dir>` for another (an identity's `<…>/<name>/.codex`
/// reads `codex-daemon@<name>`).
fn daemon_label(opts: &Opts, home: &Path) -> String {
    if canonical(&opts.home.join(".codex")) == home {
        return "codex-daemon".to_string();
    }
    let name: String = home
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect();
    format!("codex-daemon@{name}")
}

/// How long `app-server daemon update` may run: it copies a ~310 MB package
/// and restarts the daemon (measured: two seconds with every thread idle).
const DAEMON_UPDATE_WAIT: Duration = Duration::from_secs(180);

/// How long after a failed update the same target is not tried again.
const DAEMON_RETRY_S: u64 = 30 * 60;

/// The owner's word on the Codex in `tui`'s tab, from the state its visits
/// keep (`codex-<tab>`) — only while that state is about THIS TUI process
/// (its pid and kernel start, the rule [`visit`] re-mints a state by): a
/// word given to a Codex the person has since quit is gone with it. A
/// deferral that ran out is no word.
fn owner_word_on(opts: &Opts, tui: &Tui, now: u64) -> (upgrade::Request, St) {
    let start = kernel_start(tui.pid)
        .map(|s| squash(&s))
        .unwrap_or_default();
    match load(opts, &key(&tui.tab))
        .filter(|st| st.agent == Agent::Codex)
        .filter(|st| st.notice_pid == tui.pid && st.notice_start == start)
    {
        Some(st) => {
            let word = match st.request_for(&tui.tab) {
                upgrade::Request::DeferUntil(t) if now >= t => upgrade::Request::None,
                word => word,
            };
            (word, st)
        }
        None => (upgrade::Request::None, St::default()),
    }
}

/// The daemon step's report word as a daemon-mode client carries it
/// ([`DaemonView::wait`]): the wait's own word, `update-failed` for a failed
/// update (the next sweeps back off with that word), nothing while it moved
/// or is current.
fn daemon_wait_word(step: &str) -> String {
    if let Some(why) = step.strip_prefix("wait:") {
        return why.to_string();
    }
    if step.starts_with("failed:") {
        return "update-failed".to_string();
    }
    if step.starts_with("would-") {
        return "would-update".to_string();
    }
    String::new()
}

/// THE DAEMON STEP for `home` ([`cx::daemon_step`]), and what a daemon-mode
/// client's own step is told of it ([`DaemonView`]): the version the daemon
/// runs once the step is taken, and what it waits on. `None`: no daemon
/// lives in `home`.
fn daemon_pass(
    opts: &Opts,
    home: &Path,
    target: &Target,
    tuis: &[Tui],
    k: &dyn CodexKernel,
) -> Option<(Report, DaemonView)> {
    let (r, running) = daemon_step_for(opts, home, target, tuis, k)?;
    let view = DaemonView {
        running,
        wait: daemon_wait_word(&r.step),
    };
    Some((r, view))
}

/// [`daemon_pass`]'s step and the version the daemon runs after it.
fn daemon_step_for(
    opts: &Opts,
    home: &Path,
    target: &Target,
    tuis: &[Tui],
    k: &dyn CodexKernel,
) -> Option<(Report, Option<Version>)> {
    let now = now_s();
    let d = read_daemon(home, k, now)?;
    let label = daemon_label(opts, home);
    let mut r = Report {
        pid: d.pid,
        tab: "-".to_string(),
        session: label.clone(),
        from: d
            .version
            .as_ref()
            .map_or_else(|| "?".to_string(), Version::to_string),
        to: format!("{}(managed)", target.version),
        step: String::new(),
    };
    let busy = d
        .threads
        .iter()
        .filter(|(_, s, _)| *s != TurnState::Idle)
        .count();
    let settling = d
        .threads
        .iter()
        .filter(|(_, s, age)| *s == TurnState::Idle && *age < upgrade::QUIET_S)
        .count();
    let mut facts = DaemonFacts {
        running: d.version.clone(),
        managed: target.version.clone(),
        pinned: d.pinned,
        busy_threads: busy,
        terminals: d.terminals,
        settling_threads: settling,
        attended: false,
        held: false,
        owner_held: false,
        unseen_clients: 0,
    };
    let mut step = cx::daemon_step(&facts);
    let mut clients_unreadable = false;
    if step == DaemonStep::Update {
        // Only now are the tabs asked, each Codex tab this daemon serves: the
        // owner's word on it (a `--skip` of this target or a `--defer` holds
        // the daemon that runs its conversation; a `--now` is the person at
        // the tab asking, so that tab is not "attended"), a hold on it, a
        // person at it.
        let target_word = target.version.to_string();
        let served: Vec<&Tui> = tuis.iter().filter(|t| t.home == home).collect();
        for tui in &served {
            let (word, st) = owner_word_on(opts, tui, now);
            facts.owner_held |= word.holds(&st.phase, &target_word, now);
            let Ok(mut c) = connect(opts, &tui.tab) else {
                facts.held = true;
                continue;
            };
            // The hold and the hands alone: a person is `attended`.
            facts.held |= held(&mut c, &tui.tab, 0);
            let scr = screen(&mut c, &tui.tab);
            if word != upgrade::Request::Now {
                facts.attended |= scr
                    .as_ref()
                    .is_none_or(|s| upgrade::attended_by(s.human, opts.human_grace_s));
            }
            // The tab's own status line backs the kernel's count: a
            // background terminal of its thread runs in this daemon.
            if scr.is_some_and(|s| cx::terminals_on_screen(&s.rows)) {
                facts.terminals += 1;
            }
        }
        // And every Codex attached to it must be one of those: a client this
        // sweep does not list was asked none of that.
        match k.daemon_clients(d.pid) {
            Some(clients) => {
                facts.unseen_clients = clients
                    .iter()
                    .filter(|p| !served.iter().any(|t| t.pid == **p))
                    .count();
            }
            None => {
                facts.unseen_clients = 1;
                clients_unreadable = true;
            }
        }
        step = cx::daemon_step(&facts);
        if clients_unreadable && step == DaemonStep::Wait("unseen-client") {
            step = DaemonStep::Wait("clients-unreadable");
        }
    }
    let failures = state_dir(opts)
        .join("codex-daemons")
        .join(format!("{label}.json"));
    if step == DaemonStep::Update && recently_failed(&failures, &target.version, now) {
        step = DaemonStep::Wait("update-failed");
    }
    let r = match step {
        DaemonStep::Current => said(r, "current"),
        DaemonStep::Wait(why) => said(r, format!("wait:{why}")),
        DaemonStep::Update if opts.dry_run => said(r, "would-update-daemon"),
        DaemonStep::Update => {
            // The home the daemon's own environment names must be this one:
            // the verb acts on the daemon of the home it resolves.
            let env_home = home_of(
                &atpkg::caller_shell::ProcArgs {
                    exec_path: String::new(),
                    argv: Vec::new(),
                    env: d.env.clone(),
                },
                &opts.home,
            );
            if env_home != home {
                return Some((said(r, "wait:daemon-env"), d.version));
            }
            match update_daemon(&target.exe, &d.env, DAEMON_UPDATE_WAIT) {
                Ok(v) => {
                    let after = read_daemon(home, k, now_s());
                    let pinned = after.as_ref().is_some_and(|a| a.pinned);
                    let running = after.as_ref().and_then(|a| a.version.clone());
                    if running.as_ref() == Some(&target.version) && pinned {
                        r.pid = after.as_ref().map_or(r.pid, |a| a.pid);
                        let _ = std::fs::remove_file(&failures);
                        let r = said(r, "daemon-updated");
                        ledger(
                            opts,
                            &r,
                            &format!(
                                "{}: the daemon runs {v}, pinned (the vendor's updater is gone); \
                                 attached TUIs reconnect",
                                home.display()
                            ),
                        );
                        return Some((r, running));
                    }
                    note_failure(&failures, &target.version, now);
                    let r = said(r, "failed:daemon-update:unverified");
                    ledger(
                        opts,
                        &r,
                        &format!(
                            "{}: the update answered {v}, but the daemon now reads {} {}",
                            home.display(),
                            running.map_or_else(|| "?".to_string(), |v| v.to_string()),
                            if pinned { "pinned" } else { "unpinned" }
                        ),
                    );
                    r
                }
                Err(why) => {
                    note_failure(&failures, &target.version, now);
                    let r = said(r, format!("failed:daemon-update:{why}"));
                    ledger(opts, &r, &format!("{}: nothing was killed", home.display()));
                    r
                }
            }
        }
    };
    Some((r, d.version))
}

/// Whether an update to `to` failed within [`DAEMON_RETRY_S`] of `now`.
fn recently_failed(path: &Path, to: &Version, now: u64) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| aterm_json::from_str::<aterm_json::Value>(&t).ok())
        .is_some_and(|v| {
            v.get("to").and_then(aterm_json::Value::as_str) == Some(to.to_string().as_str())
                && v.get("at")
                    .and_then(aterm_json::Value::as_u64)
                    .is_some_and(|at| now.saturating_sub(at) < DAEMON_RETRY_S)
        })
}

fn note_failure(path: &Path, to: &Version, now: u64) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("{{\"to\":\"{to}\",\"at\":{now}}}\n"));
}

/// Start `cmd` in a process group of its own, so a signal to the sweep's group never
/// reaches the vendor's verb mid-copy (Windows: a new console process group).
fn own_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(cmd, 0x0000_0200); // CREATE_NEW_PROCESS_GROUP
}

/// RUN THE VENDOR'S VERB: `<exe> app-server daemon update --from-cli --yes`,
/// with exactly the environment `env` (the running daemon's), stdin closed,
/// in a process group of its own. Its answer is read line by line on a
/// thread, so a daemon that inherits the pipe cannot hold the sweep past the
/// verb's own exit. Past `limit` the sweep stops WAITING and says so — the
/// verb is left to finish on its own (a thread reaps it), never killed: a
/// kill mid-copy is the one way to leave the daemon's package half laid.
fn update_daemon(exe: &Path, env: &[String], limit: Duration) -> Result<Version, String> {
    let mut cmd = Command::new(exe);
    cmd.args(["app-server", "daemon", "update", "--from-cli", "--yes"])
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    own_process_group(&mut cmd);
    for kv in env {
        if let Some((k, v)) = kv.split_once('=')
            && !k.is_empty()
        {
            cmd.env(k, v);
        }
    }
    let mut child = cmd.spawn().map_err(|_| "spawn".to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(out) = child.stdout.take() {
        let _ = std::thread::Builder::new()
            .name("codex-daemon-update".into())
            .spawn(move || {
                for line in std::io::BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
    }
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = std::thread::Builder::new()
                    .name("codex-daemon-update-reap".into())
                    .spawn(move || {
                        let _ = child.wait();
                    });
                return Err("timeout".to_string());
            }
            Err(_) => return Err("wait".to_string()),
        }
    };
    let grace = Instant::now() + Duration::from_secs(2);
    let mut text = String::new();
    while let Ok(line) = rx.recv_timeout(grace.saturating_duration_since(Instant::now())) {
        text.push_str(&line);
        text.push('\n');
    }
    if !status.success() {
        return Err(format!("exit-{}", status.code().unwrap_or(-1)));
    }
    cx::parse_update_answer(&text)
}

// ---------------------------------------------------------------- the client

/// A placeholder thread id of the real one's length, for planning a
/// daemon-mode client's line before its exit names the thread: the flags and
/// the line's length are refused before anything is typed.
const PLAN_THREAD: &str = "00000000-0000-0000-0000-000000000000";

/// How long a typed `/exit` may take to end the TUI (measured: 1.7 s for a
/// daemon-mode client, 6.6 s for an embedded session, which shuts its
/// runtime down first).
const EXIT_WAIT: Duration = Duration::from_secs(30);

/// How old a recorded `/exit` is before a TUI still alive under it is read as
/// one the `/exit` did not take: past the visit that typed it and waited
/// [`EXIT_WAIT`], by a second wait's worth. From then on no visit waits
/// [`EXIT_WAIT`] again under the sweep lock for it (review of 2026-09-26:
/// every sweep held the machine-wide lock 30 s for it, forever).
const EXIT_TAKEN_S: u64 = 2 * EXIT_WAIT.as_secs();

/// THE `/exit` DID NOT TAKE: the TUI it was typed into outlived it by more
/// than a sweep (an Enter written and not verified, `submitted=0 pressed=1`,
/// that Codex read as a newline). Nothing waits on it again: a `/exit` left
/// in its composer is cleared while it alone is there (a person's Enter
/// would take it as their own exit, with no relaunch), and past
/// [`STALE_S`] the move stops, `stale-exit`, said once.
fn exit_not_taken(opts: &Opts, r: Report, st: &mut St, c: &mut Client, at_s: u64) -> Report {
    let cleared = match clear_left_typed(c, &st.tab, "/exit") {
        Left::Cleared => {
            ledger(
                opts,
                &said(r.clone(), "left-typed-cleared"),
                "the TUI outlived its `/exit`; the `/exit` left in its composer is cleared",
            );
            "the `/exit` left in its composer is cleared"
        }
        Left::Gone => "no `/exit` is left in its composer",
        Left::Kept(_) => {
            st.left_typed = "/exit".to_string();
            "a `/exit` may still sit in its composer: nothing is typed over it"
        }
    };
    if now_s().saturating_sub(at_s) <= STALE_S {
        return said(r, "wait:exit-not-taken");
    }
    fail(
        opts,
        r,
        st,
        "stale-exit",
        &format!("the TUI survived its `/exit` and was never relaunched; {cleared}"),
    )
}

/// Where the kernel proves this TUI's thread lives ([`Mode`]): the thread
/// whose writer lock it holds (embedded), or none held while its home's
/// daemon lives (daemon mode). `Err` names the wait.
fn mode_of(tui: &Tui, daemon_lives: bool, k: &dyn CodexKernel) -> Result<Mode, &'static str> {
    let files = k.open_files(tui.pid).ok_or("files-unreadable")?;
    let locks = tui.home.join("thread-writer-locks");
    let held: Vec<String> = files
        .iter()
        .filter(|f| f.parent() == Some(locks.as_path()))
        .filter_map(|f| cx::lock_thread(&f.file_name()?.to_string_lossy()).map(str::to_owned))
        .collect();
    match held.as_slice() {
        [thread] => Ok(Mode::Embedded {
            thread: thread.clone(),
            conversation: find_rollout(&tui.home, thread).is_some(),
        }),
        [] if daemon_lives => Ok(Mode::Daemon),
        [] => Err("no-daemon"),
        _ => Err("threads-ambiguous"),
    }
}

/// The report of a visit to `tui`.
fn blank(tui: &Tui) -> Report {
    Report {
        pid: tui.pid,
        tab: tui.tab.clone(),
        session: key(&tui.tab),
        from: tui.version.to_string(),
        to: "-".to_string(),
        step: String::new(),
    }
}

/// [`super::composer_empty`] for Codex's composer ([`cx::composer_is_empty`]):
/// the cell at the caret row's column 2 is asked for `dim` only when the
/// cursor is there — a reply that does not name `dim` reads as a draft.
fn composer_empty(c: &mut Client, tab: &str, scr: &super::Screen) -> bool {
    let dim = match scr.cursor {
        Some((row, 2)) => scr.first.checked_add(row).is_some_and(|at| {
            c.request_line(&format!("@{tab} cell {at} 2"))
                .is_ok_and(|line| cell_is_dim(&line))
        }),
        _ => false,
    };
    cx::composer_is_empty(&scr.rows, scr.cursor, dim)
}

/// The last look before typing into Codex's composer: a FRESH read with no
/// box up, no turn running and the composer empty, and that read's
/// generation — what the typed turn is fenced on.
fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    if cx::box_on_screen(&scr.rows) {
        return Err("box");
    }
    if cx::busy_on_screen(&scr.rows) {
        return Err("busy");
    }
    if !composer_empty(c, tab, &scr) {
        return Err("draft");
    }
    Ok(scr.generation)
}

/// The guard a typed text's Enter is pressed under (`turn
/// submit=guarded:<re>`): the row holding the cursor must still be the
/// composer's, ending in the text's last word — Codex's caret row, or a
/// continuation row indented two. Codex takes an Enter that lands inside a
/// burst of typed text as a NEWLINE (its paste guard, measured by the
/// supervisor's lane), so the submit is a guarded one, as measured working
/// for `/exit` here four times of four.
fn guard_for(text: &str) -> String {
    let word = text.split_whitespace().last().unwrap_or(text);
    let tail = row_guard(word);
    let tail = tail.trim_start_matches('^');
    if text.split_whitespace().count() == 1 {
        return format!("^{}\\s{tail}", cx::CARET);
    }
    format!("^[{}\\s]\\s.*{tail}", cx::CARET)
}

/// What [`type_text`] answers when the text was TYPED and not submitted: the
/// guarded Enter missed (`OK 0 turn skipped reason=guard`, the text left in
/// the composer), or the Enter was written and the composer still holds the
/// text (`submitted=0 pressed=1`: Codex took it as a newline, say).
const LEFT_TYPED: &str = "left-typed";

/// Type `text` into the composer as ONE turn, fenced on `generation` (when
/// the host takes the fence) and submitted under [`guard_for`]'s guard.
/// `Err(Typed::Refused(LEFT_TYPED))` when the text was typed and did not go:
/// the caller records it ([`St::left_typed`]) and clears it
/// ([`clear_left_typed`]) — never left for the next look to read as a
/// person's draft.
fn type_text(c: &mut Client, tab: &str, text: &str, generation: Option<&str>) -> Result<(), Typed> {
    let fence = generation
        .filter(|g| is_generation(g))
        .filter(|_| turn_takes_gen(c))
        .map(|g| format!("if-gen={g} yield=0.2 "))
        .unwrap_or_default();
    let (head, _) = c
        .request_counted(&format!(
            "@{tab} turn {fence}submit=guarded:{} {TURN_WAIT} {text}",
            guard_for(text)
        ))
        .map_err(|e| Typed::Refused(format!("{e}")))?;
    match turn_verdict(&head) {
        // The Enter was written and the submit not seen: the composer says
        // whether the text went.
        Ok(()) if head.split_whitespace().any(|w| w == "submitted=0") => {
            if screen(c, tab).is_some_and(|scr| composer_holds(&scr, text).is_some()) {
                Err(Typed::Refused(LEFT_TYPED.into()))
            } else {
                Ok(())
            }
        }
        Ok(()) => Ok(()),
        // The text is typed and its Enter was NOT pressed: the row under the
        // cursor was no longer the composer's.
        Err(Typed::Refused(h)) if h.contains("reason=guard") => {
            Err(Typed::Refused(LEFT_TYPED.into()))
        }
        Err(e) => Err(e),
    }
}

/// `s` with every whitespace character dropped: a composer's text compared
/// across its wrapping (a wrap may fall inside a word — measured, the notice's
/// `ATERM-UPGRADE-` / `READY-…`) and its rows' indentation.
fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The caret row of Codex's composer on `scr` when it holds exactly `text`
/// and nothing else — a trailing empty line (an Enter taken as a newline)
/// aside.
fn composer_holds(scr: &super::Screen, text: &str) -> Option<usize> {
    let (caret, lines) = cx::composer_draft(&scr.rows)?;
    (compact(&lines.concat()) == compact(text)).then_some(caret)
}

/// What became of text the lane left typed ([`clear_left_typed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Left {
    /// It was alone in the composer and is cleared.
    Cleared,
    /// It is no longer there: the person cleared it, or submitted it.
    Gone,
    /// Still there, and why it was not cleared: the composer holds more than
    /// it (a person's typing — theirs now), no composer is drawn (a box), or
    /// the host cannot fence the key on the read that judged it.
    Kept(&'static str),
}

/// Whether the host's `key` takes the `if-gen=` fence (its `help key` names
/// it, [`server_fences_gen`]).
fn key_takes_gen(c: &mut Client) -> bool {
    c.request_counted("help key")
        .is_ok_and(|(head, body)| head.starts_with("OK") && server_fences_gen(&body))
}

/// CLEAR WHAT THE LANE LEFT TYPED: while Codex's composer holds exactly
/// `text` and nothing else, press `ctrl+u` — fenced on the read that judged
/// it (`key if-gen=`) and guarded on the composer's caret row (`if=`) — and
/// read again, up to four times: Codex 0.157 clears its composer's line with
/// `ctrl+u`, the notice's wrapped rows at once, and a `/exit` an Enter left
/// on a second line in two (the first joins the lines; measured 2026-09-26).
/// Nothing is pressed unless the key can be fenced: a guard alone could match
/// an OLDER notice in the transcript while a person's draft sits in the
/// composer. A composer holding anything else is the person's, and kept.
fn clear_left_typed(c: &mut Client, tab: &str, text: &str) -> Left {
    let mut pressed = false;
    for _ in 0..4 {
        let Some(scr) = screen(c, tab) else {
            return Left::Kept("screen-unreadable");
        };
        if cx::box_on_screen(&scr.rows) {
            return Left::Kept("box");
        }
        if cx::composer_row(&scr.rows).is_none() {
            return Left::Kept("no-composer");
        }
        if composer_empty(c, tab, &scr) {
            return if pressed { Left::Cleared } else { Left::Gone };
        }
        let Some(caret) = composer_holds(&scr, text) else {
            let head: String = compact(text).chars().take(24).collect();
            let there = cx::composer_draft(&scr.rows)
                .is_some_and(|(_, lines)| compact(&lines.concat()).contains(&head));
            return if there || pressed {
                Left::Kept("mixed")
            } else {
                Left::Gone
            };
        };
        let Some(generation) = scr.generation.as_deref().filter(|g| is_generation(g)) else {
            return Left::Kept("no-fence");
        };
        if !key_takes_gen(c) {
            return Left::Kept("no-fence");
        }
        let args = key_args(&row_guard(&scr.rows[caret]), "ctrl+u", Some(generation));
        match c.request_line(&format!("@{tab} key {args}")) {
            // The screen moved since the read: read it again.
            Ok(l) if l.starts_with("OK") && l.split_whitespace().any(|w| w == "skipped") => {}
            Ok(l) if l.starts_with("OK") => pressed = true,
            _ => return Left::Kept("key-refused"),
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Left::Kept("stuck")
}

/// The lane's own text is typed and was not submitted: RECORD it, SAY it
/// once, and CLEAR it now while it alone is in the composer
/// ([`clear_left_typed`]). `what` names it (`notice`, `exit`).
fn left_typed(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    text: &str,
    what: &str,
) -> Report {
    st.left_typed = text.to_string();
    st.left_at = now_s();
    let r = said(r, format!("left-typed:{what}"));
    ledger(
        opts,
        &r,
        &format!(
            "the {what} was typed and not submitted (the composer's row moved, or the Enter \
             did not take): it is cleared while it alone is in the composer, and nothing is \
             typed over it"
        ),
    );
    settle_left_typed(opts, &r, st, c, tab);
    r
}

/// One attempt at clearing [`St::left_typed`]: the record goes once the text
/// is cleared (one ledger row, `left-typed-cleared`) or gone.
fn settle_left_typed(opts: &Opts, r: &Report, st: &mut St, c: &mut Client, tab: &str) -> Left {
    let text = st.left_typed.clone();
    let left = clear_left_typed(c, tab, &text);
    match left {
        Left::Cleared => {
            st.left_typed.clear();
            ledger(
                opts,
                &said(r.clone(), "left-typed-cleared"),
                "the lane's own text is cleared from the composer (ctrl+u, while it alone was \
                 there)",
            );
        }
        Left::Gone => st.left_typed.clear(),
        Left::Kept(_) => {}
    }
    left
}

/// ONE VISIT to a Codex TUI: the Claude lane's visit, step for step, with
/// Codex's readers, Codex's exit and no signal.
#[allow(clippy::too_many_lines)]
pub(super) fn visit(
    opts: &Opts,
    tui: &Tui,
    target: &Target,
    daemon: Option<DaemonView>,
    k: &dyn CodexKernel,
) -> Report {
    let mut r = blank(tui);
    let session = key(&tui.tab);
    let prior = load(opts, &session).filter(|st| st.agent == Agent::Codex);
    // A restart in flight in this tab: the old TUI still exiting, or a new one
    // that may be its relaunch.
    if let Some(mut st) = prior.clone().filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        // A restart in flight is an idle point's to carry on.
        if opts.background {
            return said(r, "wait:background");
        }
        if opts.dry_run {
            return said(r, format!("would-resume:{}", st.phase.word()));
        }
        let Ok(mut c) = connect(opts, &tui.tab) else {
            return said(r, "wait:no-socket");
        };
        if !roster(&mut c).contains(&tui.tab) {
            return said(r, "wait:tab-not-live");
        }
        let r = if tui.pid == st.pid {
            // The TUI the exit was typed into still lives.
            match st.phase {
                Phase::Exiting { at_s } if now_s().saturating_sub(at_s) > EXIT_TAKEN_S => {
                    exit_not_taken(opts, r, &mut st, &mut c, at_s)
                }
                Phase::Exiting { .. } => relaunch(opts, r, &mut st, &mut c, k),
                _ => said(r, "wait:exiting"),
            }
        } else if relaunched_by_us(tui.pid, &tui.argv, &st, k) {
            if opts.hand_back {
                adopt(opts, r, &mut st, &mut c, tui.pid, k)
            } else {
                carry_on(opts, r, &mut st, &mut c, tui.pid, k)
            }
        } else {
            st.fail("resumed-elsewhere", now_s());
            let r = said(r, "failed:resumed-elsewhere");
            ledger(
                opts,
                &r,
                "a Codex this upgrade did not relaunch runs in the tab",
            );
            r
        };
        save(opts, &session, &st);
        return r;
    }
    if tui.version >= target.version {
        // Current: a state owed nothing more goes, so the owner's view never
        // shows a pending move for a TUI already on its build — nor a move
        // that failed after its exit, which a Codex on the build in the tab
        // now ends (the person resumed it by hand).
        if !opts.dry_run
            && prior.as_ref().is_some_and(|st| {
                matches!(st.phase, Phase::Pending | Phase::Announced { .. })
                    || (matches!(st.phase, Phase::Failed(_)) && st.exited_at != 0)
            })
        {
            let _ = std::fs::remove_file(super::state_path(opts, &session));
        }
        return said(r, "current");
    }
    let now = now_s();
    r.to = format!("{}(managed)", target.version);
    let (prior, start) = own_prior(tui, prior);
    let mut st = minted(tui, target, prior, start, now);
    if matches!(st.request, upgrade::Request::DeferUntil(t) if now >= t) {
        st.request = upgrade::Request::None;
        st.request_tab.clear();
        st.request_at = 0;
    }
    // A STOPPED ROUND RESTS, THEN A NEW ONE STARTS: no stop is for good (the
    // owner, 2026-09-27). Until `RETRY_S` has passed since it stopped it
    // waits `failed:<why>` — looked at again, never read by the reducer; a
    // Codex round that gave up hears no late READY (the lane's own rule).
    // Past it — unless the owner's skip or a deferral holds it — the round is
    // re-armed here, and the next look types its first notice under every
    // gate ([`super::rearm`]).
    if let Phase::Failed(why) = &st.phase {
        let due = if upgrade::retry_due(&st.phase, st.failed_for(now), false) {
            Step::Rearm
        } else {
            Step::Wait("failed")
        };
        if upgrade::rearm_held(&st.request_for(&tui.tab), due, &st.to, now) != Step::Rearm {
            return said(r, format!("wait:failed:{why}"));
        }
        let r = super::rearm(opts, r, &mut st, now);
        save(opts, &session, &st);
        return r;
    }
    // THE PROCESS PROOFS: the TUI is its shell's foreground job, on a
    // terminal an aterm tab owns.
    let job = k.job(tui.pid);
    if matches!(job, Some((Job::NoJobControl, _))) {
        let r = not_a_job(opts, r, &mut st);
        save(opts, &session, &st);
        return r;
    }
    if let Err(why) = foreground_shell(job) {
        st.note_wait(why, now);
        save(opts, &session, &st);
        return said(r, format!("wait:{why}"));
    }
    let owner = k.terminal(tui.pid);
    let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
    if !owner.is_some_and(owned_by_aterm) {
        let why = format!("terminal:{}", owner_word(owner));
        st.note_wait(&why, now);
        let r = held_back(opts, r, &mut st, &session, &why);
        save(opts, &session, &st);
        return r;
    }
    let Ok(mut c) = connect(opts, &tui.tab) else {
        return said(r, "wait:no-socket");
    };
    if !roster(&mut c).contains(&tui.tab) {
        return said(r, "wait:tab-not-live");
    }
    if tui
        .claim
        .as_ref()
        .is_some_and(|claim| !claim_still_live(&mut c, tui.pid, claim))
    {
        return said(r, "wait:tab-ownership-changed");
    }
    let Some(scr) = screen(&mut c, &tui.tab) else {
        return said(r, "wait:screen-unreadable");
    };
    // THE LANE'S OWN TEXT LEFT IN THE COMPOSER FIRST: cleared while it alone
    // is there; kept, it is said (`left-typed`) — never read as a person's
    // draft, never typed over.
    if !st.left_typed.is_empty() {
        let r = match settle_left_typed(opts, &r, &mut st, &mut c, &tui.tab) {
            Left::Gone => None,
            Left::Cleared => Some(said(r.clone(), "left-typed-cleared")),
            Left::Kept(why) => Some(said(r.clone(), format!("wait:left-typed:{why}"))),
        };
        if let Some(r) = r {
            st.note_step(&r.step, "", now);
            save(opts, &session, &st);
            return r;
        }
    }
    let mode = match mode_of(tui, daemon.is_some(), k) {
        Ok(mode) => mode,
        Err(why) => {
            st.note_wait(why, now);
            save(opts, &session, &st);
            return said(r, format!("wait:{why}"));
        }
    };
    // One `status` read: the hold and the hands, and the agent's verdict the
    // quiet is measured from (`super::quiet_s`).
    let status = status_line(&mut c, &tui.tab);
    let quiet_s = quiet_s(status.as_deref(), &mut st, scr.seq, now);
    // THE TURN: an embedded conversation's rollout says it; a daemon-mode
    // client's thread is not known before it exits, so its screen does.
    let rollout = match &mode {
        Mode::Embedded {
            thread,
            conversation: true,
        } => find_rollout(&tui.home, thread),
        _ => None,
    };
    let tail = rollout
        .as_ref()
        .map(|p| tail_to_end(p, TAIL_BYTES).0)
        .unwrap_or_default();
    let busy_screen = cx::busy_on_screen(&scr.rows);
    let idle = !busy_screen && (rollout.is_none() || cx::rollout_turn(&tail) == TurnState::Idle);
    let mut facts = Facts {
        status: if idle { "idle" } else { "busy" }.to_string(),
        status_age_s: rollout.as_ref().map_or(quiet_s, |p| age_s(p, now)),
        composer_empty: composer_empty(&mut c, &tui.tab, &scr),
        approval_box: cx::box_on_screen(&scr.rows),
        busy_footer: busy_screen,
        background: k.background(tui.pid),
        // The hold and the hands alone: a person's keystroke is `attended`,
        // which the owner's `--now` waives.
        held: status.as_deref().is_none_or(|line| held_by_status(line, 0)),
        quiet_s,
        hold_s: 0,
        owner_now: false,
        attended: upgrade::attended_by(scr.human, opts.human_grace_s),
        background_point: opts.background,
        // A thread with no rollout yet — Codex writes it with the thread's
        // first message — is no conversation: it is never announced to, but
        // ended with `/exit` and started plain (`cx::next_step`, `Mode::
        // Embedded { conversation: false }`), and a daemon-mode client is
        // never announced to at all. Whose turns a rollout holds is not read:
        // a conversation with one is announced to.
        taskless: false,
        // A Codex at its usage wall reads nothing typed: never asked there,
        // and its re-ask clock is held — the Claude lane's rule.
        limited: upgrade::limited(Agent::Codex, &scr.rows),
        // A Codex whose login is gone answers nothing typed either: its
        // login notice is the same wait (the screen's word alone — what a
        // Codex writes to its rollout at the wall is not measured).
        login: upgrade::login_wall(Agent::Codex, &scr.rows),
        undelivered: false,
        // Stamped below, once the READY is read ([`St::time_ready`]).
        ready_s: 0,
        // A stopped round never reaches here (re-armed or waiting, above).
        failed_s: st.failed_for(now),
    };
    // AN EMBEDDED SESSION'S BACKGROUND TERMINALS ARE ITS OWN WORK (measured: a
    // `sleep` a finished turn left running died with the `/exit`), and no shell
    // stands between Codex and them for `background` to see: the kernel's
    // session leaders under the TUI, and Codex's own status line. They count as
    // the agent's background exactly as a Claude shell does, so the reducer's
    // one rule bounds the wait on them: waited for, asked about again (naming
    // them) once a READY has stood a whole REASK_S, and given up on past
    // MAX_ASKS — never an endless `wait:background-terminal` nothing re-asks.
    // A daemon-mode client's run in the daemon and outlive its `/exit`
    // (measured), which the daemon's own step waits on.
    let terminals: Vec<String> = if matches!(mode, Mode::Embedded { .. }) {
        let mut t = k.terminals(tui.pid);
        if t.is_empty() && cx::terminals_on_screen(&scr.rows) {
            t.push("background terminal".to_string());
        }
        t
    } else {
        Vec::new()
    };
    facts.background.extend(terminals.iter().cloned());
    st.phase = upgrade::clock_held(&st.phase, &facts, now);
    // A person's box or draft is timed across sweeps exactly as the Claude
    // lane times it: past the drain it voids an embedded session's READY.
    (st.hold_since_s, st.hold_seen_s) =
        upgrade::hold_since((st.hold_since_s, st.hold_seen_s), &facts, now);
    if st.hold_since_s != 0 {
        facts.hold_s = now.saturating_sub(st.hold_since_s);
    }
    let ready = !st.marker.is_empty() && cx::rollout_has_ready(&tail, &st.marker);
    // The READY answer's own clock, held at the wall, as the Claude lane
    // keeps it: work under the agent that outlives the answer is asked about
    // again, naming it, once the answer has had a whole re-ask window.
    st.time_ready(ready, &mut facts, now);
    let daemon_behind = daemon
        .as_ref()
        .and_then(|d| d.running.clone())
        .is_none_or(|running| running < target.version);
    let mut step = cx::requested_step(
        &st.request_for(&tui.tab),
        &mode,
        &st.phase,
        &facts,
        ready,
        daemon_behind,
        now,
        &st.to,
    );
    // Waited on for its terminals alone at an idle point, the wait keeps their
    // name — and the status view its `/ps` remedy. (At a break, `background` is
    // the break's own wait and keeps its name.) A terminal never lets the exit
    // through, whatever the plan says: it would end with the TUI.
    if !terminals.is_empty()
        && (step == Step::Terminate
            || (!opts.background
                && step == Step::Wait("background")
                && facts.background.len() == terminals.len()))
    {
        step = Step::Wait("background-terminal");
    }
    // At a break of the agent's own background work nothing is ended,
    // whatever the plan says.
    if opts.background {
        step = upgrade::break_step(step);
    }
    // An act whose text was left typed is not tried again at once: whatever
    // moved the composer's row may still stand (`St::left_at`).
    if matches!(step, Step::Announce | Step::Terminate)
        && st.left_at != 0
        && now.saturating_sub(st.left_at) < upgrade::REASK_S
    {
        step = Step::Wait("left-typed-backoff");
    }
    let mut proven = None;
    if matches!(step, Step::Announce | Step::Terminate) {
        match foreground_shell(k.job(tui.pid)) {
            Ok(shell) => proven = Some(shell),
            Err(why) => step = Step::Wait(why),
        }
    }
    let result = match step {
        Step::Wait("attended") => attended_once(opts, r, &mut st),
        // What the daemon waits on, carried into this client's own wait.
        Step::Wait("daemon-first") => {
            let why = daemon
                .as_ref()
                .map(|d| d.wait.as_str())
                .filter(|w| !w.is_empty())
                .unwrap_or("behind");
            said(r, format!("wait:daemon-first:{why}"))
        }
        Step::Wait(why) => said(r, format!("wait:{why}")),
        Step::Rearm => super::rearm(opts, r, &mut st, now),
        Step::Void(why) => drain_expired(opts, r, &mut st, why, &facts, now),
        Step::GiveUp => {
            st.fail(upgrade::GAVE_UP, now);
            let r = said(r, "gave-up");
            ledger(opts, &r, &gave_up_words(&k.held(tui.pid)));
            r
        }
        Step::Announce => announce(opts, r, &mut st, &mut c, tui, target, &mode, proven, k, now),
        // Never planned here (`Facts::taskless` is false on this lane): a
        // thread with nothing to resume takes the plain restart above.
        Step::Fresh => said(r, "wait:fresh"),
        Step::Terminate if opts.dry_run => said(r, "would-exit"),
        Step::Terminate => match proven {
            Some(shell) => exit(opts, r, &mut st, &mut c, tui, target, &mode, shell, k),
            None => said(r, "wait:ids"),
        },
    };
    st.note_step(&result.step, &facts.status, now);
    save(opts, &session, &st);
    result
}

/// THE NOTICE to an embedded conversation, for a relaunch that can be
/// carried out: the plan is asked first ([`plan`]), so a launch the exit
/// would then refuse never asks the agent to wind down for nothing.
#[allow(clippy::too_many_arguments)]
fn announce(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tui: &Tui,
    target: &Target,
    mode: &Mode,
    proven: Option<u32>,
    k: &dyn CodexKernel,
    now: u64,
) -> Report {
    let Mode::Embedded { thread, .. } = mode else {
        return said(r, "wait:mode");
    };
    let Some(shell) = proven else {
        return said(r, "wait:ids");
    };
    if let Err(no) = plan(opts, tui, shell, Some(thread), target, k) {
        return unplanned(opts, r, st, no);
    }
    if opts.dry_run {
        return said(r, "would-announce");
    }
    let asks = match st.phase {
        Phase::Announced { asks, .. } => asks + 1,
        _ => 1,
    };
    let marker = upgrade::ready_marker(
        thread,
        &target.version,
        st.salt.wrapping_add(u64::from(asks)),
    );
    let text = cx::prepare_prompt(&tui.version, &target.version, &marker)
        + &upgrade::running_clause(&k.held(tui.pid));
    if tui
        .claim
        .as_ref()
        .is_some_and(|claim| !claim_still_live(c, tui.pid, claim))
    {
        return said(r, "wait:tab-ownership-changed");
    }
    let generation = match typing_fence(c, &tui.tab) {
        Ok(g) => g,
        Err(why) => return said(r, format!("wait:{why}")),
    };
    match type_text(c, &tui.tab, &text, generation.as_deref()) {
        Ok(()) => {
            st.marker = marker;
            st.phase = Phase::Announced { at_s: now, asks };
            st.last_stop.clear();
            st.thread.clone_from(thread);
            st.mode = "embedded".to_string();
            let r = said(r, format!("announced:{asks}"));
            ledger(opts, &r, &st.marker);
            r
        }
        // Typed and not submitted: said, recorded and cleared — never left
        // for the next look to read as the person's draft (review of
        // 2026-09-26: ~600 characters of notice sat in the composer, the
        // miss said nowhere, and every later visit waited `draft`).
        Err(Typed::Refused(why)) if why == LEFT_TYPED => {
            left_typed(opts, r, st, c, &tui.tab, &text, "notice")
        }
        Err(e) => said(r, format!("wait:announce-refused:{}", e.word())),
    }
}

/// THE RELAUNCH, PLANNED: the shell's dialect, the heal it needs, the
/// directory to resume in (the TUI's own, when the shell's differs), and the
/// line — `<twin> resume <kept flags> <thread>`, or the kept flags alone for
/// a session with no conversation (`thread` `None`).
fn plan(
    opts: &Opts,
    tui_or_st: &Tui,
    shell: u32,
    thread: Option<&str>,
    target: &Target,
    k: &dyn CodexKernel,
) -> Result<Plan, NoPlan> {
    plan_for(
        opts,
        &tui_or_st.argv,
        k.cwd(tui_or_st.pid).unwrap_or_default(),
        shell,
        thread,
        &target.twin,
        k,
    )
}

/// [`plan`] from what an in-flight state recorded.
fn plan_for(
    opts: &Opts,
    argv: &[String],
    want: String,
    shell: u32,
    thread: Option<&str>,
    twin: &Path,
    k: &dyn CodexKernel,
) -> Result<Plan, NoPlan> {
    let shell_name = k.args(shell).map(|a| a.exec_path).or_else(|| {
        table()
            .into_iter()
            .find(|(p, _, _)| *p == shell)
            .map(|(_, _, n)| n)
    });
    let Some(dialect) = shell_name
        .as_deref()
        .map(base_name)
        .as_deref()
        .and_then(Dialect::from_exe_name)
    else {
        return Err(NoPlan::Wait("shell-dialect"));
    };
    let heal = atpkg::hooks::hook_file(&opts.home, dialect.hook_ext()).map(|(p, _)| p);
    let cd = (!want.is_empty() && k.cwd(shell).as_deref() != Some(want.as_str())).then_some(want);
    let flags = cx::rewrite_argv(argv, thread).map_err(|e| NoPlan::Refused {
        why: format!("argv:{e}"),
        what: e.to_string(),
        detail: "the launch flags cannot be carried into a resume".to_string(),
    })?;
    let line = upgrade::relaunch_line(dialect, heal.as_deref(), cd.as_deref(), twin, &flags)
        .map_err(|e| NoPlan::Refused {
            why: format!("line:{e}"),
            what: "line".to_string(),
            detail: e,
        })?;
    Ok(Plan {
        shell,
        dialect,
        line,
    })
}

/// THE EXIT: the relaunch planned, the last look, the state written, then
/// `/exit` typed — fenced on the read that judged the composer empty,
/// guarded on the composer's row — and the relaunch carried on. Never a
/// signal (module header).
#[allow(clippy::too_many_arguments)]
fn exit(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tui: &Tui,
    target: &Target,
    mode: &Mode,
    shell: u32,
    k: &dyn CodexKernel,
) -> Report {
    let thread = match mode {
        Mode::Daemon => Some(PLAN_THREAD.to_string()),
        Mode::Embedded {
            thread,
            conversation: true,
        } => Some(thread.clone()),
        Mode::Embedded { .. } => None,
    };
    let Plan { line, .. } = match plan(opts, tui, shell, thread.as_deref(), target, k) {
        Ok(p) => p,
        Err(no) => return unplanned(opts, r, st, no),
    };
    // The last look before the one irreversible keystroke: nobody's hand (a
    // person's within the grace: `0` under the owner's `--now`).
    let grace = if st.request_for(&tui.tab) == upgrade::Request::Now {
        0
    } else {
        opts.human_grace_s
    };
    // THE HARNESS'S HAND ON THE TAB ([`Hand`], ND1), as for Claude Code's
    // restart: taken before the last look, so from it — through the `/exit`,
    // the bare shell and the relaunch line — to the relaunched TUI's first
    // idle no other connection's write lands. Given back where the `/exit`
    // is not typed; the relaunch renews it and gives it back.
    let Some(mut hand) = Hand::take(c, &tui.tab) else {
        return said(r, "wait:held");
    };
    match exit_typed(opts, r, st, c, tui, target, mode, shell, grace, line, k) {
        ControlFlow::Continue(r) => relaunch(opts, r, st, c, k),
        ControlFlow::Break(r) => {
            hand.give_back(c);
            r
        }
    }
}

/// [`exit`] under the harness's hand, from its last look: `Continue` once
/// the `/exit` is typed (the relaunch's to go on with), `Break` with the
/// report of a look, or a keystroke, that stopped it — a report either way,
/// neither an error.
#[allow(clippy::too_many_arguments)]
fn exit_typed(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tui: &Tui,
    target: &Target,
    mode: &Mode,
    shell: u32,
    grace: u32,
    line: String,
    k: &dyn CodexKernel,
) -> ControlFlow<Report, Report> {
    let mode_word = match mode {
        Mode::Daemon => "daemon",
        Mode::Embedded {
            conversation: true, ..
        } => "embedded",
        Mode::Embedded { .. } => "fresh",
    };
    let owner = k.terminal(tui.pid);
    let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
    if foreground_shell(k.job(tui.pid)) != Ok(shell)
        || !owner.is_some_and(owned_by_aterm)
        || tui
            .claim
            .as_ref()
            .is_some_and(|claim| !claim_still_live(c, tui.pid, claim))
        || held(c, &tui.tab, grace)
    {
        return ControlFlow::Break(said(r, "wait:changed"));
    }
    if mode.cooperative() && !k.background(tui.pid).is_empty() {
        return ControlFlow::Break(said(r, "wait:background"));
    }
    // An embedded session's background terminals end with it (measured).
    if matches!(mode, Mode::Embedded { .. }) && !k.terminals(tui.pid).is_empty() {
        return ControlFlow::Break(said(r, "wait:background-terminal"));
    }
    // A daemon-mode client's thread is known only from what it prints as it
    // leaves, and that is told from the prompt after it only by the shell
    // integration's marks: without them the `/exit` is not typed.
    if *mode == Mode::Daemon && !marks_reach(c, &tui.tab) {
        return ControlFlow::Break(said(r, "wait:no-shell-integration"));
    }
    let generation = match typing_fence(c, &tui.tab) {
        Ok(g) => g,
        Err(why) => return ControlFlow::Break(said(r, format!("wait:{why}"))),
    };
    let before = st.phase.clone();
    st.pid = tui.pid;
    st.shell = shell;
    st.tab.clone_from(&tui.tab);
    st.mode = mode_word.to_string();
    st.thread = match mode {
        Mode::Embedded { thread, .. } => thread.clone(),
        Mode::Daemon => String::new(),
    };
    // The line a daemon-mode client's relaunch types is planned again once
    // its exit names the thread; until then NONE is recorded — the plan
    // above names a placeholder thread only to vet the flags and the line's
    // length, and a state carrying it would hand an older build's orphan
    // pass (which types `st.line` verbatim) a resume of a thread that does
    // not exist (review of 2026-09-26).
    st.line = if *mode == Mode::Daemon {
        String::new()
    } else {
        line
    };
    // What the relaunch is planned from again once the exit names the
    // thread: recorded, never re-read from a process that is gone by then.
    st.argv.clone_from(&tui.argv);
    st.cwd = k.cwd(tui.pid).unwrap_or_default();
    st.twin = target.twin.to_string_lossy().into_owned();
    st.phase = Phase::Exiting { at_s: now_s() };
    save(opts, &key(&tui.tab), st);
    match type_text(c, &tui.tab, "/exit", generation.as_deref()) {
        Ok(()) => {}
        // Typed and not submitted: the TUI lives on with `/exit` in its
        // composer, which a person's Enter would take as their own exit with
        // no relaunch — said, recorded and cleared (review of 2026-09-26).
        Err(Typed::Refused(why)) if why == LEFT_TYPED => {
            st.phase = before;
            let r = left_typed(opts, r, st, c, &tui.tab, "/exit", "exit");
            save(opts, &key(&tui.tab), st);
            return ControlFlow::Break(r);
        }
        Err(e) => {
            st.phase = before;
            save(opts, &key(&tui.tab), st);
            if let Typed::Refused(why) = &e {
                ledger(
                    opts,
                    &said(r.clone(), "exit-refused"),
                    &format!("`/exit` not submitted: {why}"),
                );
            }
            return ControlFlow::Break(said(r, format!("wait:exit-{}", e.word())));
        }
    }
    ledger(
        opts,
        &said(r.clone(), "exit-typed"),
        &format!("/exit ({mode_word}) — no signal"),
    );
    ControlFlow::Continue(r)
}

/// THE EXIT HINT the TUI just printed, from its OWN rows: the shell
/// integration's finished command block for the command that ran it (read
/// whole with `blocktext`, scrollback included: an inline TUI's output runs
/// past the screen, measured 2026-09-26 at 65 rows), else the rows directly
/// above the new prompt's FIRST row ([`cx::hint_above_prompt`]) — the
/// entering block says how many rows the prompt draws above its command row.
/// Never the whole screen: an older exit's hint stays on it. An inline TUI's
/// daemon update wipes the tab's blocks (measured: `{"blocks":[]}` after the
/// reconnect), so the finished block is often gone while the entering one,
/// drawn after the exit, is not.
fn exit_hint(c: &mut Client, tab: &str) -> Option<ExitHint> {
    let scr = screen(c, tab)?;
    let marks = prompt_marks(c, tab);
    if let Some(rows) = marks
        .as_ref()
        .and_then(|m| m.done)
        .and_then(|id| block_rows(c, tab, id))
    {
        return cx::parse_exit_hint(&rows);
    }
    let above = marks.map_or(0, |m| m.prompt_rows);
    let (row, _) = scr.cursor?;
    cx::hint_above_prompt(&scr.rows, row.checked_sub(above)?)
}

/// What the shell integration marks at the tab's newest prompt ([`prompt_marks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PromptMarks {
    /// The finished command block that ENDS where the entering prompt begins
    /// — the command that just ran, and no older one whose output merely
    /// survived a wipe.
    done: Option<u64>,
    /// The rows the entering prompt draws above its command row (`cmd -
    /// prompt`: a `%~` / `%#` prompt draws one).
    prompt_rows: usize,
}

/// The shell integration's marks at the newest prompt (`blocks 2 --json`):
/// `None` unless the newest block is an ENTERING prompt.
fn prompt_marks(c: &mut Client, tab: &str) -> Option<PromptMarks> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 2 --json")).ok()?;
    parse_prompt_marks(&head, &body)
}

/// [`prompt_marks`] over one `blocks --json` reply.
fn parse_prompt_marks(head: &str, body: &str) -> Option<PromptMarks> {
    use aterm_json::Value;
    if !head.starts_with("OK") {
        return None;
    }
    let json = if body.trim().is_empty() {
        head.strip_prefix("OK ")?.to_string()
    } else {
        body.to_string()
    };
    let v: Value = aterm_json::from_str(json.trim()).ok()?;
    let blocks = v.get("blocks")?.as_array()?;
    let (last, rest) = blocks.split_last()?;
    let n = |b: &Value, k: &str| b.get(k).and_then(Value::as_u64);
    if last.get("state").and_then(Value::as_str) != Some("entering") {
        return None;
    }
    let prompt = n(last, "prompt")?;
    let prompt_rows = usize::try_from(n(last, "cmd")?.checked_sub(prompt)?).ok()?;
    let done = rest
        .last()
        .filter(|d| d.get("state").and_then(Value::as_str) == Some("complete"))
        .filter(|d| n(d, "end") == Some(prompt))
        .and_then(|d| n(d, "id"));
    Some(PromptMarks { done, prompt_rows })
}

/// One command block's output rows (`blocktext <id>`), scrollback included.
fn block_rows(c: &mut Client, tab: &str, id: u64) -> Option<Vec<String>> {
    let (head, body) = c.request_counted(&format!("@{tab} blocktext {id}")).ok()?;
    head.starts_with("OK")
        .then(|| body.lines().map(str::to_owned).collect())
}

/// Whether the shell integration's marks reach aterm in `tab` — the
/// integration is `on` (`status integration=`), or the host tracks a block
/// there — so the rows a daemon-mode client prints as it leaves can be told
/// from the prompt after them ([`exit_hint`]). Without the marks a prompt of
/// more than one row hides the hint, and the `/exit` would end the client
/// with no relaunch (review of 2026-09-26): it is not typed.
fn marks_reach(c: &mut Client, tab: &str) -> bool {
    if c.request_line(&format!("@{tab} status"))
        .is_ok_and(|l| l.split_whitespace().any(|w| w == "integration=on"))
    {
        return true;
    }
    c.request_counted(&format!("@{tab} blocks 1 --json"))
        .is_ok_and(|(head, body)| {
            let json = if body.trim().is_empty() {
                head.strip_prefix("OK ").unwrap_or("").to_string()
            } else {
                body
            };
            head.starts_with("OK")
                && aterm_json::from_str::<aterm_json::Value>(json.trim())
                    .ok()
                    .and_then(|v| v.get("blocks")?.as_array().map(|b| !b.is_empty()))
                    .unwrap_or(false)
        })
}

/// The TUI was told to exit: once it is gone and the shell has its prompt
/// back, read the thread it held from its own words, and type the relaunch.
fn relaunch(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn CodexKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = relaunch_held(opts, r, st, c, &mut hand, k);
    if !matches!(st.phase, Phase::Exiting { .. }) {
        hand.give_back(c);
    }
    r
}

/// [`relaunch`] with the harness's `hand` on the tab ([`Hand`], ND1): kept
/// through every wait, the line typed under it, and on the tab until the
/// relaunched TUI's first idle ([`await_new`]) — as Claude Code's relaunch
/// holds it (`relaunch::relaunch`).
fn relaunch_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn CodexKernel,
) -> Report {
    let old = st.pid;
    if !wait_until(EXIT_WAIT, || !k.alive(old)) {
        // Never a signal: a later sweep finds the TUI as it is.
        return said(r, "wait:exiting");
    }
    hand.keep(c);
    // Gone: whatever stops this move from here has no process left to vet it
    // by, and is the tab's record (`upgrade_status::Row::exited_at`).
    if st.exited_at == 0 {
        st.exited_at = now_s();
    }
    let shell = st.shell;
    if !wait_until(Duration::from_secs(15), || k.shell_has_terminal(shell)) {
        return said(r, "wait:shell-prompt");
    }
    hand.keep(c);
    // The shell draws its prompt after it takes the terminal back.
    std::thread::sleep(Duration::from_millis(600));
    let tab = st.tab.clone();
    let home = PathBuf::from(&st.codex_home);
    let hint = exit_hint(c, &tab);
    let thread = match st.mode.as_str() {
        // The kernel proved the thread; a hint naming ANOTHER is a
        // contradiction, and nothing is typed on one.
        "embedded" => match hint {
            Some(h) if h.thread != st.thread => {
                return fail(
                    opts,
                    r,
                    st,
                    "hint-mismatch",
                    "the exit named another thread",
                );
            }
            _ => Some(st.thread.clone()),
        },
        // A daemon-mode client's thread is its hint's. An earlier attempt at
        // this same relaunch (a person typing at the prompt made it wait)
        // recorded the hint it read: that stands, and a DIFFERENT hint since
        // is another Codex's exit at this prompt, never ours to resume.
        "daemon" => match (hint, st.thread.as_str()) {
            (Some(h), "") => Some(h.thread),
            (Some(h), recorded) if h.thread == recorded => Some(h.thread),
            (Some(_), _) => {
                return fail(
                    opts,
                    r,
                    st,
                    "hint-mismatch",
                    "a later exit at this prompt named another thread",
                );
            }
            (None, recorded) if !recorded.is_empty() => Some(recorded.to_string()),
            (None, _) => {
                return fail(
                    opts,
                    r,
                    st,
                    "no-resume-hint",
                    "the TUI exited without naming its thread; it runs on in the daemon — \
                     `codex resume` in the tab takes it back",
                );
            }
        },
        _ => None,
    };
    // A thread with no rollout has no message yet: nothing to resume, and the
    // TUI comes back plain.
    let thread = thread.filter(|t| find_rollout(&home, t).is_some());
    if st.mode == "embedded" && thread.is_none() {
        return fail(
            opts,
            r,
            st,
            "no-rollout",
            "the conversation's rollout is gone",
        );
    }
    // An embedded thread's lock is released with the TUI; a resume while it
    // is still held would be refused by Codex (`already has an active
    // writer`).
    let lock = thread
        .as_ref()
        .map(|t| home.join("thread-writer-locks").join(format!("{t}.lock")));
    let embedded = st.mode == "embedded";
    let lock_free = |k: &dyn CodexKernel| -> bool {
        !embedded
            || lock
                .as_ref()
                .is_some_and(|l| k.holders_of(l).is_some_and(|h| h.is_empty()))
    };
    if !lock_free(k) {
        return said(r, "wait:thread-locked");
    }
    let (line, dialect) = match plan_for(
        opts,
        &st.argv,
        st.cwd.clone(),
        shell,
        thread.as_deref(),
        &PathBuf::from(&st.twin),
        k,
    ) {
        Ok(p) => (p.line, p.dialect),
        Err(NoPlan::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(NoPlan::Refused { why, detail, .. }) => return fail(opts, r, st, &why, &detail),
    };
    // Recorded BEFORE the line is typed: a sweep that dies after typing it
    // leaves the next one the thread to recognise its relaunch by.
    st.thread = thread.clone().unwrap_or_default();
    st.line.clone_from(&line);
    save(opts, &key(&tab), st);
    match type_relaunch_line_with(
        opts,
        c,
        shell,
        &tab,
        &line,
        Some(dialect),
        &mut st.prompt,
        || lock_free(k),
        |c, pid, tab| k.in_tab(c, pid, tab),
    ) {
        Ok(()) => st.prompt = None,
        Err(RelaunchLineError::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(RelaunchLineError::Turn(e)) => {
            st.fail("relaunch-refused", now_s());
            let r = said(r, format!("failed:relaunch:{}", first_word(&e)));
            ledger(opts, &r, &line);
            return r;
        }
    }
    st.phase = Phase::Relaunched { at_s: now_s() };
    ledger(
        opts,
        &said(r.clone(), "relaunched"),
        &format!(
            "{} ({})",
            st.line,
            if st.thread.is_empty() {
                "no conversation: plain"
            } else {
                "resume"
            }
        ),
    );
    save(opts, &key(&tab), st);
    await_held(opts, r, st, c, hand, k)
}

/// Record a Codex restart that stops here, with one ledger line.
fn fail(opts: &Opts, r: Report, st: &mut St, why: &str, detail: &str) -> Report {
    st.fail(why, now_s());
    let r = said(r, format!("failed:{why}"));
    ledger(opts, &r, detail);
    r
}

/// Whether `pid` (argv `argv`) is the relaunch this upgrade typed: a child
/// of the shell the line was typed at, other than the TUI that exited, whose
/// argv resumes the recorded thread — or, for a plain relaunch, resumes
/// nothing.
fn relaunched_by_us(pid: u32, argv: &[String], st: &St, k: &dyn CodexKernel) -> bool {
    pid != st.pid
        && matches!(st.phase, Phase::Relaunched { .. } | Phase::Exiting { .. })
        && k.parent(pid) == Some(st.shell)
        && cx::is_tui_argv(argv)
        && if st.thread.is_empty() {
            !argv.iter().any(|a| a == "resume")
        } else {
            argv.iter().any(|a| a == "resume") && argv.last() == Some(&st.thread)
        }
}

/// After the relaunch line: find the new TUI — the relaunch itself
/// ([`relaunched_by_us`]), nothing else — and carry on with it, or, where a
/// supervisor loop takes it ([`Opts::hand_back`]: the window's worker), hand
/// it back ([`adopt`]) — exactly as the Claude restart's relaunch does
/// ([`super::super::relaunch::await_new`]), the harness's hand on the tab
/// from here to the new TUI's first idle. A later step that finds the
/// relaunch in flight comes here too, and holds the tab as the relaunch did.
fn await_new(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn CodexKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = await_held(opts, r, st, c, &mut hand, k);
    hand.give_back(c);
    r
}

/// [`await_new`] with the harness's `hand` on the tab, kept through the wait;
/// its caller gives it back.
fn await_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn CodexKernel,
) -> Report {
    let mut new = None;
    wait_until(Duration::from_secs(90), || {
        hand.keep(c);
        new = k
            .codex_children(st.shell)
            .into_iter()
            .find(|(pid, argv)| relaunched_by_us(*pid, argv, st, k))
            .map(|(pid, _)| pid);
        new.is_some()
    });
    if new.is_some() {
        // No record of its own says idle: the server's verdict alone, and
        // from a server without it, no wait.
        first_idle(c, hand, &st.tab, || true);
    }
    match new {
        Some(pid) if opts.hand_back => adopt(opts, r, st, c, pid, k),
        Some(pid) => carry_on(opts, r, st, c, pid, k),
        None => {
            if let Phase::Relaunched { at_s } = st.phase
                && now_s().saturating_sub(at_s) > STALE_S
            {
                return fail(
                    opts,
                    r,
                    st,
                    "no-resume",
                    "the relaunched Codex never came up in the tab",
                );
            }
            said(r, "wait:resume")
        }
    }
}

/// How long a relaunched TUI may take to lead its tab's foreground group
/// before it is handed back ([`adopt`]).
const ADOPT_WAIT: Duration = Duration::from_secs(30);

/// THE RELAUNCHED CODEX HANDED BACK TO ITS SUPERVISOR (`adopted`): the new
/// TUI leads the tab — by the host's own roster, the one the window's
/// program is published from, so the host never reads the gap between the
/// relaunch and the new program as the agent leaving — and the record stays
/// in flight; the loop answers whatever the new Codex opened with (a trust
/// gate, a new build's notice), and at its next idle point the relaunch
/// primitive's continuation step ([`super::super::relaunch::resume`]) types
/// the carry-on ([`carry_on`]).
fn adopt(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn CodexKernel,
) -> Report {
    let tab = st.tab.clone();
    if !wait_until(ADOPT_WAIT, || k.in_tab(c, new, &tab)) {
        return said(r, "wait:resume");
    }
    r.pid = new;
    let r = said(r, "adopted");
    ledger(
        opts,
        &r,
        "the relaunched Codex leads the tab; its supervisor types the continuation at its \
         first idle point",
    );
    r
}

/// A Codex restart in flight — the old TUI exiting, or relaunched and not
/// carried on yet — carried on from where it stopped: the continuation step
/// of the relaunch primitive for a Codex record
/// ([`super::super::relaunch::resume`], where the tab's loop parked at idle
/// for it), and every other caller's (`opts.hand_back` off: the new TUI is
/// carried on in the same step).
pub(super) fn carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    carry_in_flight_with(opts, r, st, session, &Live)
}

/// [`carry_in_flight`] over the kernel `k`.
fn carry_in_flight_with(
    opts: &Opts,
    r: Report,
    mut st: St,
    session: &str,
    k: &dyn CodexKernel,
) -> Report {
    let r = Report {
        pid: st.pid,
        tab: st.tab.clone(),
        session: session.to_string(),
        from: st.from.clone(),
        to: format!("{}({})", st.to, st.source),
        ..r
    };
    if opts.dry_run {
        return said(r, format!("would-resume:{}", st.phase.word()));
    }
    if let Some((why, detail)) = expired(&st, now_s(), k) {
        let r = fail(opts, r, &mut st, why, detail);
        save(opts, session, &st);
        return r;
    }
    let Ok(mut c) = connect(opts, &st.tab) else {
        return said(r, "wait:no-socket");
    };
    if !roster(&mut c).contains(&st.tab) {
        return said(r, "wait:tab-not-live");
    }
    let r = match st.phase {
        Phase::Exiting { .. } => relaunch(opts, r, &mut st, &mut c, k),
        _ => await_new(opts, r, &mut st, &mut c, k),
    };
    save(opts, session, &st);
    r
}

/// The relaunched TUI `new` is up. A daemon-mode client (or a plain one) is
/// done once its composer is drawn; an embedded conversation once the new
/// process holds the thread again (the kernel's lock) and the continuation
/// is typed at an idle, empty composer — nobody's hand on the tab: a person
/// at the keys keeps it owed, typed at the next idle point.
fn carry_on(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn CodexKernel,
) -> Report {
    r.pid = new;
    let tab = st.tab.clone();
    let up = wait_until(Duration::from_secs(60), || {
        screen(c, &tab).is_some_and(|s| cx::composer_row(&s.rows).is_some())
    });
    let to = Version::parse(&st.to);
    let from = Version::parse(&st.from);
    let mut typed = false;
    if st.mode == "embedded" && up {
        let lock = PathBuf::from(&st.codex_home)
            .join("thread-writer-locks")
            .join(format!("{}.lock", st.thread));
        let holds = wait_until(Duration::from_secs(30), || {
            k.holders_of(&lock).is_some_and(|h| h.contains(&new))
        });
        let settled =
            holds && wait_until(Duration::from_secs(120), || typing_fence(c, &tab).is_ok());
        // A person at the keyboard wins here too (the Claude restart's
        // carry-on asks the same): held, it stays owed.
        if settled && held(c, &tab, opts.human_grace_s) {
            return said(r, "wait:held");
        }
        if settled
            && k.in_tab(c, new, &tab)
            && let (Some(f), Some(t)) = (&from, &to)
        {
            let text = cx::continue_prompt(f, t);
            for _ in 0..3 {
                let Ok(generation) = typing_fence(c, &tab) else {
                    break;
                };
                match type_text(c, &tab, &text, generation.as_deref()) {
                    Ok(()) => {
                        typed = true;
                        break;
                    }
                    Err(Typed::Changed | Typed::Yielded) => {
                        std::thread::sleep(Duration::from_millis(400));
                    }
                    Err(Typed::Refused(_)) => break,
                }
            }
        }
    }
    st.phase = Phase::Done;
    st.resumed_pid = new;
    st.resumed_on.clone_from(&st.to);
    st.done_at = now_s();
    // `codex on <to> · <what came back>`: the owner's outcome line, cut at
    // ` · ` by the window's record like the Claude lane's.
    st.outcome = format!(
        "codex on {} · {}{}",
        st.to,
        if st.thread.is_empty() {
            "no conversation to resume"
        } else {
            "the same conversation resumed"
        },
        if st.mode == "daemon" {
            "; its work never stopped, in the daemon"
        } else {
            ""
        }
    );
    // A carry-on typed says so (`continued`, `upgrade_drive::typed`): its
    // answer is the harness's own turn, never the worker's work.
    let step = if !up {
        "done:unconfirmed"
    } else if typed {
        "continued"
    } else if st.mode == "embedded" {
        "done:no-continue"
    } else {
        "done"
    };
    let r = said(r, step);
    ledger(opts, &r, &st.outcome);
    r
}

/// Codex restarts a previous sweep left between the exit and the relaunch,
/// or between the relaunch and the new TUI, whose tab no Codex TUI leads
/// now.
fn orphans(
    opts: &Opts,
    live_tabs: Option<&[LiveTab]>,
    tuis: &[Tui],
    k: &dyn CodexKernel,
) -> Vec<Report> {
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
        let Some(mut st) = load(opts, &session)
            .filter(|st| st.agent == Agent::Codex)
            .filter(St::in_flight)
        else {
            continue;
        };
        if tuis.iter().any(|t| t.tab == st.tab)
            || live_tabs.is_some_and(|tabs| !super::tab_is_live(tabs, &st.tab))
            || opts.only_sid.as_ref().is_some_and(|s| *s != st.tab)
        {
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
        // No Codex leads the tab, and the one the `/exit` was typed into is
        // gone: what stops the move from here is the tab's record.
        if st.exited_at == 0 && !k.alive(st.pid) {
            st.exited_at = now_s();
        }
        if let Some((why, detail)) = expired(&st, now_s(), k) {
            if opts.dry_run {
                out.push(said(r, format!("would-fail:{why}")));
                continue;
            }
            let r = fail(opts, r, &mut st, why, detail);
            save(opts, &session, &st);
            out.push(r);
            continue;
        }
        if opts.dry_run {
            out.push(said(r, format!("would-resume:{}", st.phase.word())));
            continue;
        }
        let Ok(mut c) = connect(opts, &st.tab) else {
            out.push(said(r, "wait:no-socket"));
            continue;
        };
        if !roster(&mut c).contains(&st.tab) {
            out.push(said(r, "wait:tab-not-live"));
            continue;
        }
        let r = match st.phase {
            Phase::Exiting { .. } => relaunch(opts, r, &mut st, &mut c, k),
            _ => await_new(opts, r, &mut st, &mut c, k),
        };
        save(opts, &session, &st);
        out.push(r);
    }
    out
}

/// [`super::expired`] for a Codex restart: typed `/exit` minutes ago and
/// never relaunched, its shell gone, or relaunched and never come up.
fn expired(st: &St, now: u64, k: &dyn CodexKernel) -> Option<(&'static str, &'static str)> {
    match st.phase {
        Phase::Exiting { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "stale-exit",
            "the TUI exited minutes ago and was never relaunched: the tab is not typed into now",
        )),
        Phase::Exiting { .. } if st.shell == 0 || !k.alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran the TUI is gone: there is no prompt to relaunch at",
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => {
            Some(("no-resume", "the relaunched Codex never came up in the tab"))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- the owner's view

/// The live Codex holders of the `codex-<tab>` upgrades in `sessions`, for
/// the owner's view ([`super::upgrade_status::Holder`]): the Codex TUI that
/// leads the tab now, with the build it runs. The host proves the tab by its
/// roster; a hand-run view by the TUI's environment.
pub(super) fn holders(
    home: &Path,
    sessions: &BTreeSet<String>,
    live_tabs: Option<&[LiveTab]>,
) -> Vec<super::upgrade_status::Holder> {
    if !sessions.iter().any(|s| s.starts_with("codex-")) {
        return Vec::new();
    }
    let procs = table();
    let found = candidates(live_tabs, &procs, ids, |pid| {
        atpkg::caller_shell::process_args(pid)
            .and_then(|a| a.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned))
    });
    let (tuis, _) = tuis(
        &Opts {
            home: home.to_path_buf(),
            state: PathBuf::new(),
            sock: None,
            only_sid: None,
            dry_run: true,
            human_grace_s: 0,
            hand_back: false,
            background: false,
            aterm_state: None,
        },
        found,
        &Live,
    );
    tuis.into_iter()
        .filter(|t| sessions.contains(&key(&t.tab)))
        .map(|t| super::upgrade_status::Holder {
            session: key(&t.tab),
            version: t.version.to_string(),
            tab: Some(t.tab),
        })
        .collect()
}

// The upgrade tests drive real processes through Unix APIs (process groups, modes, inodes).
#[cfg(all(test, unix))]
#[path = "upgrade_codex_drive_tests.rs"]
mod tests;
