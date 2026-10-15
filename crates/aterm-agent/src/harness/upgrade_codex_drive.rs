// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Codex branch of the upgrade step, its I/O half
//! ([`super::super::upgrade_codex`] is the pure half), under the step's lock
//! and ledger. The window's host takes it in the session's worker at its
//! loop's idle point ([`super::step`], scoped to the tab); a hand-run `aterm
//! harness upgrade` takes it for every Codex at once ([`super::sweep`]).
//!
//! 1. **Daemons first.** For `~/.codex` and every `$CODEX_HOME` a live Codex TUI
//!    uses, the daemon `app-server-daemon/daemon.pid` names (proven by argv
//!    `app-server … --managed-daemon` and an executable under that home's
//!    `packages/app-server-daemon/releases/`) is read from files and the
//!    kernel: its version, whether the vendor updater is armed
//!    (`auto-update-version`), and each thread it holds (its
//!    `thread-writer-locks/<id>.lock` in `lsof`) with its rollout. Only when
//!    [`cx::daemon_step`] says so is `<managed codex> app-server daemon update
//!    --from-cli --yes` run, with the running daemon's own environment, never
//!    the window's. Bounded, and never a kill. The window's step asks every
//!    Codex tab the daemon serves (hold, person, the user's word) before
//!    moving it.
//! 2. **Then the clients.** Each Codex TUI leading a tab this pass serves, on a
//!    build older than the managed one, is moved at an idle point
//!    ([`cx::requested_step`]) by a typed `/exit`, fenced on the judged screen
//!    and guarded on the composer row (a daemon-mode client at once, an
//!    embedded one after the notice and its READY), and relaunched as `<twin>
//!    resume <flags> <thread>`. The thread comes from the TUI's exit hint (the
//!    shell integration's block, else the rows above the new prompt) or the
//!    lock the kernel proved it held, typed through the one relaunch line
//!    ([`super::super::relaunch`]'s `type_relaunch_line_with`). Under the
//!    window (`Opts::hand_back`) the new TUI is `adopted` and carried on at the
//!    loop's next idle point. No signal is ever sent to a Codex process.
//! 3. **Then restarts left in flight** whose TUI is gone.
//!
//! A Codex upgrade's state is filed per tab (`<state>/upgrade/codex-<tab>.json`):
//! a daemon client's thread is unknown until it exits, and at most one
//! foreground Codex lives in a tab. Claude loops skip those files.

use aterm_uds::spawnfd::Command;
use std::collections::BTreeSet;
use std::io::BufRead as _;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_phase::codex::CodexGoal;

use super::super::goal_hold::{self, Hold, How, Owner, Stage};
use super::super::relaunch::{ExitCause, first_idle};
use super::super::upgrade_codex::{
    self as cx, DaemonFacts, DaemonStep, ExitHint, GoalStep, Mode, TurnState,
};
use super::upgrade::{Agent, Dialect, Facts, Phase, Step, Version};
use super::{
    Behind, Client, Due, Hand, HostClaim, Job, Kernel, Left, Live, LiveTab, NoPlan, Opts, Plan,
    PromptLook, RelaunchLineError, Report, SAME_PROGRAM, STALE_S, St, TAIL_BYTES, TURN_WAIT, Typed,
    UNREAD_PROCESS, UNREADABLE_TABLE, alive, attended_once, background_procs, base_name,
    cell_is_dim, claim_still_live, compact, connect, cwd_of, drain_expired, end_asked_at, exe_of,
    first_word, foreground_shell, gave_up_words, held, held_back, held_by_status, held_of, ids,
    is_generation, kernel_start, key_takes_gen, ledger, live_background, load, not_a_job, now_s,
    owned_by_aterm, owner_word, process_in_tab, quiet_s, roster, said, save, screen, squash,
    state_dir, status_line, table, tail_to_end, turn_takes_gen, turn_verdict,
    type_relaunch_line_with, under_hand, unique_tab_for_group, unplanned, upgrade, wait_until,
};
use crate::supervise::approvals;
use crate::supervise::policy::turn_end::{CODEX_EXIT, GOAL_PAUSE, GOAL_RESUME};
use crate::supervise::policy::{key_args, row_guard};
use crate::supervise::screen::HumanInput;

// ---------------------------------------------------------------- the kernel

/// The process facts the Codex lane rests on, each read when asked: the Claude
/// lane's [`Kernel`] plus Codex's needs. A trait so a driver test can script a
/// TUI that exits on `/exit` and a relaunch that appears on its line.
pub(super) trait CodexKernel: Kernel {
    /// Whether `pid` lives.
    fn alive(&self, pid: u32) -> bool;
    /// `pid`'s argv and initial environment (`KERN_PROCARGS2`).
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs>;
    /// The `TZ` `pid` was started with ([`wall_zone`]): `Some(None)` for none
    /// (machine zone), `None` when its environment is unreadable.
    fn tz(&self, pid: u32) -> Option<Option<String>> {
        self.args(pid).map(|a| a.env_var("TZ").map(str::to_owned))
    }
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
    /// That work named for the notice and the give-up's ledger row
    /// ([`upgrade::running_clause`], [`super::held_of`]). Default: nothing.
    fn held(&self, pid: u32) -> Vec<upgrade::Held> {
        let _ = pid;
        Vec::new()
    }
    /// The background terminals under `pid` ([`cx::terminals_under`]):
    /// descendants leading a session of their own, by name.
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
        let out = aterm_uds::spawnfd::command("lsof")
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
        let out = aterm_uds::spawnfd::command("lsof")
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

    // Fail closed: an unreadable process table reads as a terminal still
    // running, so the embedded exit's last look waits.
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
        let out = aterm_uds::spawnfd::command("lsof")
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

/// The kernel's session id of `pid` (`getsid`), `None` when unreadable. macOS
/// answers it for processes in any visible session.
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

/// The managed Codex: atpkg's twin (`<prefix>/agents/codex`, the shim a
/// relaunch line runs) and the store build it execs, whose package names the
/// version.
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
/// `$HOME/.codex`, else `<home>/.codex`; canonical when it exists, matching
/// `lsof`'s canonical paths.
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

/// The candidates: with the host's roster, each tab whose foreground group
/// leader is named `codex` (the twin `exec`s the store build, so the leader is
/// the TUI), claimed by that group and never where two tabs name the same
/// group; without one (a hand-run sweep), each `codex` whose environment names
/// its tab (`ATERM_PARENT_SESSION_ID`) and that leads its terminal's
/// foreground group. At most one per tab, so a suspended Codex never
/// overwrites the notice the foreground one answers. `procs` is `(pid, ppid,
/// name)`.
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

/// The TUIs among the candidates, with argv, build, version and home read; a
/// non-TUI (`exec`, the daemon) is left out, and an unreadable one is a report.
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
        // Only Codex under the sweep's own home: one started with another
        // `$HOME` is not ours to move, and a test's scratch-home sweep never
        // reaches a real Codex.
        let own = canonical(&opts.home);
        if args.env_var("HOME").map(|h| canonical(Path::new(h))) != Some(own) {
            if f.claim.is_some() {
                reports.push(report("skip:another-home"));
            }
            continue;
        }
        if !cx::is_tui_argv(&args.argv) {
            // The daemon and its updater carry their launcher's environment;
            // `exec` is no session. Only a foreground one in a tab is said.
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

/// One pass of the Codex branch: daemons, clients, restarts left in flight.
/// Scoped to `opts.only_sid`, it visits that tab's TUI alone, but the daemon is
/// asked about every Codex tab it serves before it moves.
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
    // Bookkeeping under the lock, as the Claude lane's ([`super::prune_states`]):
    // the records of tabs that are gone, settled a week ago.
    let known_live: Vec<String> = live_tabs
        .into_iter()
        .flatten()
        .map(|t| t.sid.clone())
        .chain(tuis.iter().map(|t| t.tab.clone()))
        .collect();
    let graph = registry_vouches_for_state(
        aterm_uds::control_socket_dir_is_private(),
        aterm_types::dirs::runs_under_private_state_root(),
    )
    .then(aterm_uds::control_socket_dir)
    .flatten()
    .map(|dir| dir.join("graph"));
    prune(opts, now_s(), registry_life(graph.as_deref(), &known_live));
    let target = Target::read();
    sweep_with(opts, live_tabs, tuis, target.as_ref(), &k, &mut reports);
    reports
}

/// Whether tab `opts.only_sid` has a Codex move to take, asked at a worker's
/// start, every activation notice and every look ([`super::due`]): a Codex TUI
/// leads the tab on a build older than the managed Codex or on a daemon behind
/// it, or on a current daemon whose vendor updater is still armed
/// ([`daemon_due`], the pin alone, looked at most every [`PIN_LOOK_S`] and with
/// no row, tab mark or turn end of its own: `wait:pin:<why>`). Read-only and
/// cheap.
///
/// `None` when no Codex leads the tab ([`tab_tui`]); the Claude Code half reads
/// it then. A Codex not read whole is [`super::UNREAD_PROCESS`]; one this
/// upgrade does not move, or with no managed target, is `No`. Every `Some` is
/// conclusive: Claude's session records are not scanned.
pub(super) fn due(opts: &Opts, tabs: &[LiveTab]) -> Option<Due> {
    due_with(opts, tabs, &Live, Target::read, ids, env_tab)
}

/// [`due`]'s testable seam: the foreground identified as the tab's Codex
/// ([`tab_tui`]) before the managed target is read, with the sweep's owner and
/// TTY checks. A non-Codex or shared foreground stays `None`.
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
            // A goal the move paused keeps the tab due until it is resumed
            // (`cx::goal_owed`).
            let owed = goal_path(opts, &tui.tab)
                .and_then(|p| goal_hold::read(&p))
                .is_some_and(|h| cx::goal_owed(Some(&h)));
            let moves = owed
                || target().is_some_and(|target| {
                    tui.version < target.version || daemon_due(opts, &tui.home, &target, k, now_s())
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
    /// A Codex this upgrade does not move: another home's, no TUI (`exec`, the
    /// daemon), an unnamed build, or another tab's by environment.
    NotOurs,
    /// A Codex whose process could not be read whole — its ids, its argv or
    /// its executable, or the tab's foreground moved between two reads.
    Unread,
}

/// The Codex leading tab `opts.only_sid`, read as [`due`] reads it with injected
/// process reads (`k`, `read_ids`, `read_env_tab`): the leader's name first,
/// then ids and package. `None` when the leader is not named `codex` or its
/// argv (a sysctl) is unreadable (another user's, or gone), and when the group
/// leads another tab too; the Claude Code half then reads the tab.
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

/// [`super::note_behind`]'s Codex half: the Codex TUI leading the tab
/// ([`tab_tui`]) on an older build gets its state minted as its first visit
/// would ([`minted`]), behind since `since`, where no state of this TUI stands
/// or the standing one finished with another build ([`super::behind_anew`]). A
/// restart in flight is left alone. `None` when no Codex leads the tab; an
/// unreadable one is [`Behind::Unread`].
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

/// The tab's state as `tui`'s: a state is about one TUI process (pid and kernel
/// start); another TUI in the tab starts afresh. Returns the TUI's kernel
/// start, squashed, too.
fn own_prior(tui: &Tui, prior: Option<St>) -> (Option<St>, String) {
    let start = kernel_start(tui.pid)
        .map(|s| squash(&s))
        .unwrap_or_default();
    let prior = prior.filter(|st| st.notice_pid == tui.pid && st.notice_start == start);
    (prior, start)
}

/// The state a visit of `tui` mints for `target` at `now` over its own `prior`
/// ([`own_prior`], started `start`): [`St::for_target`], keeping the lane's
/// record of text it left in this TUI's composer, plus the TUI's identity, tab
/// and home.
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
    let who = super::Proc {
        start: &start,
        started_s: 0,
    };
    let mut st = St::for_target(prior, &tui.version, &candidate, None, who, now);
    st.left_typed = left;
    st.agent = Agent::Codex;
    st.notice_pid = tui.pid;
    st.notice_start = start;
    st.tab.clone_from(&tui.tab);
    st.codex_home = tui.home.to_string_lossy().into_owned();
    st
}

/// `home`'s daemon read from files and the kernel (no `lsof`): its build
/// (`None`: unreadable) and whether it is pinned (vendor updater gone). `None`
/// when no daemon lives there.
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

/// Whether `home`'s daemon ([`daemon_build`]) makes its tab due at `now`: an
/// older build than the managed Codex, or the managed build still unpinned with
/// no pin look within [`PIN_LOOK_S`] ([`pin_looked_at`]). `false` when absent
/// or unreadable.
fn daemon_due(opts: &Opts, home: &Path, target: &Target, k: &dyn CodexKernel, now: u64) -> bool {
    daemon_build(home, k).is_some_and(|(version, pinned)| {
        match daemon_owes(version, pinned, target) {
            Owes::Move => true,
            Owes::Pin => pin_look_due(pin_looked_at(opts, home), now),
            Owes::Nothing => false,
        }
    })
}

/// What a daemon on `running` (`pinned`: the vendor's updater gone) owes the
/// managed Codex `target` ([`daemon_due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owes {
    /// An older build: the daemon's move, which its clients wait for
    /// (`daemon-first`).
    Move,
    /// The managed build, unpinned: the pin alone. It still restarts the
    /// daemon, so it waits for every thread idle; no client waits on it.
    Pin,
    /// Pinned on the managed build, ahead of it, or unreadable.
    Nothing,
}

/// [`Owes`] of a daemon on `running`, `pinned` or not, against `target`.
pub(super) fn daemon_owes(running: Option<Version>, pinned: bool, target: &Target) -> Owes {
    match running {
        Some(v) if v < target.version => Owes::Move,
        Some(v) if v == target.version && !pinned => Owes::Pin,
        _ => Owes::Nothing,
    }
}

/// How often a tab is looked at for its daemon's pin alone. The pin restarts
/// the daemon, so it waits for every thread idle, which a goal-mode thread may
/// never be; the armed vendor updater can restart it mid-turn, so the pin is
/// kept but asked about at most once an hour per daemon.
pub(super) const PIN_LOOK_S: u64 = 3_600;

/// Whether a daemon owing the pin alone is due a look at `now`, given its last
/// look `last` ([`pin_looked_at`]): none yet, or [`PIN_LOOK_S`] ago.
#[must_use]
pub(super) fn pin_look_due(last: Option<u64>, now: u64) -> bool {
    last.is_none_or(|at| now.saturating_sub(at) >= PIN_LOOK_S)
}

/// Where the last look for `home`'s daemon's pin alone is stamped
/// (`<state>/codex-daemons/<label>-pin.json`, `{"at": <unix s>}`).
fn pin_stamp(opts: &Opts, home: &Path) -> PathBuf {
    state_dir(opts)
        .join("codex-daemons")
        .join(format!("{}-pin.json", daemon_label(opts, home)))
}

/// When a step last looked at `home`'s daemon for its pin alone and did not
/// pin it; `None` for no such look.
fn pin_looked_at(opts: &Opts, home: &Path) -> Option<u64> {
    std::fs::read_to_string(pin_stamp(opts, home))
        .ok()
        .and_then(|t| aterm_json::from_str::<aterm_json::Value>(&t).ok())
        .and_then(|v| v.get("at").and_then(aterm_json::Value::as_u64))
}

/// Stamp (`Some(now)`) or clear (`None`) the pin's last look
/// ([`pin_looked_at`]). A dry run (the upgrade watch) writes nothing.
fn note_pin_look(opts: &Opts, home: &Path, at: Option<u64>) {
    if opts.dry_run {
        return;
    }
    let path = pin_stamp(opts, home);
    match at {
        Some(at) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, format!("{{\"at\":{at}}}\n"));
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// [`sweep`] over TUIs already found (every visible Codex TUI, for the daemon
/// to ask; the pass visits those `opts.only_sid` names) and a target already
/// read.
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
    // Daemons first: the default home and every home a visited TUI uses, each
    // asked about every TUI it serves. Never at a background-work break
    // (`opts.background`), where only the notice is typed.
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
    // At a break the daemon is only read — its client's mode rests on it
    // living — and never stepped.
    if opts.background {
        for tui in &visited {
            if let Some((running, _)) = daemon_build(&tui.home, k) {
                daemons.push((
                    tui.home.clone(),
                    DaemonView {
                        running,
                        wait: String::new(),
                        pid: 0,
                        threads: Vec::new(),
                    },
                ));
            }
        }
    }
    // Then the clients.
    for tui in visited {
        let daemon = daemons
            .iter()
            .find(|(h, _)| *h == tui.home)
            .map(|(_, v)| v.clone());
        reports.push(visit(opts, tui, target, daemon, k));
    }
    // Then the restarts left in flight whose TUI is gone (idle points only).
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
    /// Each thread it holds, its turn state and spawn tree ([`cx::Loaded`]),
    /// and seconds since its rollout was written (no rollout yet: idle,
    /// lineage unread).
    threads: Vec<(cx::Loaded, u64)>,
    /// The background terminals running under it ([`cx::terminals_under`]).
    terminals: usize,
    /// The environment it was started with.
    env: Vec<String>,
}

/// What a daemon-mode client's visit is told of its home's daemon: its version
/// once this sweep's daemon step is taken and, while behind, what the daemon
/// waits on, carried into the client's own wait (`daemon-first:<why>`) so the
/// real blocker is named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DaemonView {
    /// The version the daemon runs, `None` when unreadable.
    pub(super) running: Option<Version>,
    /// The daemon step's word while it waits or failed (`busy-thread`,
    /// `background-terminal`, `update-failed`, …); empty otherwise.
    pub(super) wait: String,
    /// The daemon's pid, to ask which Codex are attached
    /// ([`CodexKernel::daemon_clients`]); `0` when only its build was read.
    pub(super) pid: u32,
    /// Every thread the daemon holds, with its turn state by rollout
    /// ([`rollout_turn_at`]; [`TurnState::Unknown`] counts as running) and spawn
    /// tree ([`cx::Lineage`]). A client's `/exit` waits on a turn of its own
    /// conversation, or one not placeable elsewhere ([`cx::daemon_turn`],
    /// [`upgrade::Facts::daemon_turn`]). Empty when only the build was read,
    /// and after an update.
    pub(super) threads: Vec<cx::Loaded>,
}

/// The daemon of `home`, when one lives: `None` for no daemon, a dead or
/// recycled `daemon.pid`, or files the kernel will not show.
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
                None => (
                    cx::Loaded {
                        thread,
                        turn: TurnState::Idle,
                        lineage: cx::Lineage::Unread,
                    },
                    u64::MAX,
                ),
                Some(path) => {
                    let lineage = first_line(&path, META_BYTES)
                        .map_or(cx::Lineage::Unread, |line| cx::session_lineage(&line));
                    let loaded = cx::Loaded {
                        thread,
                        turn: rollout_turn_at(&path),
                        lineage,
                    };
                    (loaded, age_s(&path, now))
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

/// How much of a rollout's first line is read for its `session_meta` spawn tree
/// ([`cx::session_lineage`]); the line carries base instructions (about 20 KB).
/// Longer reads as [`cx::Lineage::Unread`].
const META_BYTES: u64 = 1 << 20;

/// The first line of `path`, at most `cap` bytes of it (lossily decoded, as
/// [`tail_to_end`] reads a tail); `None` where the file cannot be read.
fn first_line(path: &Path, cap: u64) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = Vec::new();
    std::io::BufReader::new(std::io::Read::take(file, cap))
        .read_until(b'\n', &mut line)
        .ok()?;
    Some(String::from_utf8_lossy(&line).into_owned())
}

/// How far back from a rollout's end its last turn event is looked for
/// ([`rollout_turn_at`]), [`TAIL_BYTES`] at a time. A turn event further back
/// is [`TurnState::Unknown`] (counted running). A line longer than a tail is
/// carried once (`TurnBack`) rather than re-copied.
const TURN_BACK_BYTES: u64 = 8 << 20;

/// A thread's turn state from its rollout at `path`: the last turn event in the
/// last [`TURN_BACK_BYTES`] ([`cx::rollout_turn_back`]), read back from the end
/// [`TAIL_BYTES`] at a time. [`TurnState::Unknown`] (counted running) when none
/// is found or the file is unreadable.
fn rollout_turn_at(path: &Path) -> TurnState {
    let Ok(mut file) = std::fs::File::open(path) else {
        return TurnState::Unknown;
    };
    let len = file.metadata().map_or(0, |m| m.len());
    cx::rollout_turn_back(&mut file, len, TAIL_BYTES, TURN_BACK_BYTES)
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
/// exists; a thread without one has no user message, and `codex resume`
/// refuses it.
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
/// `codex-daemon@<name>` for another (`<…>/<name>/.codex`).
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

/// How long `app-server daemon update` may run: it copies a ~310 MB package and
/// restarts the daemon.
const DAEMON_UPDATE_WAIT: Duration = Duration::from_secs(180);

/// How long after a failed update the same target is not tried again.
const DAEMON_RETRY_S: u64 = 30 * 60;

/// The user's word on the Codex in `tui`'s tab, from the state its visits keep
/// (`codex-<tab>`), only while that state is about this TUI process ([`visit`]
/// re-mints otherwise). An expired deferral is no word.
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

/// The daemon step's word as a client carries it ([`DaemonView::wait`]): the
/// wait's word, `update-failed` for a failed update, nothing when moved or
/// current.
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

/// The daemon step for `home` ([`cx::daemon_step`]) and what a daemon client's
/// step is told of it ([`DaemonView`]). `None`: no daemon in `home`.
fn daemon_pass(
    opts: &Opts,
    home: &Path,
    target: &Target,
    tuis: &[Tui],
    k: &dyn CodexKernel,
) -> Option<(Report, DaemonView)> {
    let (r, running, pid, threads) = daemon_step_for(opts, home, target, tuis, k)?;
    let view = DaemonView {
        running,
        wait: daemon_wait_word(&r.step),
        pid,
        threads,
    };
    Some((r, view))
}

/// A daemon's threads, each one's turn state and spawn tree
/// ([`DaemonView::threads`]).
type Turns = Vec<cx::Loaded>;

/// [`daemon_pass`]'s step, the daemon's version after it, its pid and its
/// threads' turn states (all idle after an update).
fn daemon_step_for(
    opts: &Opts,
    home: &Path,
    target: &Target,
    tuis: &[Tui],
    k: &dyn CodexKernel,
) -> Option<(Report, Option<Version>, u32, Turns)> {
    let now = now_s();
    let d = read_daemon(home, k, now)?;
    let turns: Turns = d.threads.iter().map(|(l, _)| l.clone()).collect();
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
    let busy = d.threads.iter().filter(|(l, _)| l.running()).count();
    let settling = d
        .threads
        .iter()
        .filter(|(l, age)| !l.running() && *age < upgrade::quiet_s())
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
        // Only now is each Codex tab this daemon serves asked: the user's word
        // (a `--skip` of this target or `--defer` holds the daemon; `--now`
        // means that tab is not "attended"), a hold, a person.
        let target_word = target.version.to_string();
        let served: Vec<&Tui> = tuis.iter().filter(|t| t.home == home).collect();
        for tui in &served {
            let (word, st) = owner_word_on(opts, tui, now);
            facts.owner_held |= word.holds(&st.phase, &target_word, now);
            let Ok(mut c) = connect(opts, &tui.tab) else {
                facts.held = true;
                continue;
            };
            // Holds and hands; a person is `attended` within that tab's grace
            // (KEYS_GAP_S under a `--now`).
            facts.held |= held(&mut c, &tui.tab, 0);
            let scr = screen(&mut c, &tui.tab);
            let rung = if word == upgrade::Request::Now {
                upgrade::Rung::Land
            } else {
                st.rung_at(now)
            };
            facts.attended |= scr.as_ref().is_none_or(|s| {
                upgrade::attended_by(s.human, upgrade::person_grace(rung, opts.human_grace_s))
            });
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
    // The pin alone (a current daemon with its updater armed): its waits and
    // failures are said as the pin's (`wait:pin:<why>`, `failed:pin:<why>`),
    // owning no turn end, moving no client, raising no row; the look is
    // stamped so the tab is due again only after PIN_LOOK_S ([`daemon_due`]).
    let pin_only = daemon_owes(d.version.clone(), d.pinned, target) == Owes::Pin;
    let word = |w: String| if pin_only { pin_word(&w) } else { w };
    if pin_only && !matches!(step, DaemonStep::Update | DaemonStep::Current) {
        note_pin_look(opts, home, Some(now));
    }
    let r = match step {
        DaemonStep::Current => said(r, "current"),
        DaemonStep::Wait(why) => said(r, word(format!("wait:{why}"))),
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
                if pin_only {
                    note_pin_look(opts, home, Some(now));
                }
                let r = said(r, word("wait:daemon-env".to_string()));
                return Some((r, d.version, d.pid, turns));
            }
            match update_daemon(&target.exe, &d.env, DAEMON_UPDATE_WAIT) {
                Ok(v) => {
                    let after = read_daemon(home, k, now_s());
                    let pinned = after.as_ref().is_some_and(|a| a.pinned);
                    let running = after.as_ref().and_then(|a| a.version.clone());
                    if running.as_ref() == Some(&target.version) && pinned {
                        r.pid = after.as_ref().map_or(r.pid, |a| a.pid);
                        let _ = std::fs::remove_file(&failures);
                        note_pin_look(opts, home, None);
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
                        let (pid, threads) = after.map_or((d.pid, Vec::new()), |a| {
                            let threads = a.threads.into_iter().map(|(l, _)| l).collect();
                            (a.pid, threads)
                        });
                        return Some((r, running, pid, threads));
                    }
                    note_failure(&failures, &target.version, now);
                    if pin_only {
                        note_pin_look(opts, home, Some(now));
                    }
                    let r = said(r, word("failed:daemon-update:unverified".to_string()));
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
                    if pin_only {
                        note_pin_look(opts, home, Some(now));
                    }
                    let r = said(r, word(format!("failed:daemon-update:{why}")));
                    ledger(opts, &r, &format!("{}: nothing was killed", home.display()));
                    r
                }
            }
        }
    };
    Some((r, d.version, d.pid, turns))
}

/// A daemon step's word said as the pin's ([`Owes::Pin`]): `wait:<why>` becomes
/// `wait:pin:<why>` and `failed:daemon-update:<why>` becomes `failed:pin:<why>`.
fn pin_word(step: &str) -> String {
    if let Some(why) = step.strip_prefix("wait:") {
        return format!("wait:pin:{why}");
    }
    if let Some(why) = step.strip_prefix("failed:daemon-update:") {
        return format!("failed:pin:{why}");
    }
    step.to_string()
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
///
/// On Windows through `aterm_uds::spawn::set_flags`, the one creation-flags setter:
/// `CREATE_NEW_PROCESS_GROUP` alone, from a process with no console (the window's
/// harness host), gives the verb a VISIBLE console window (measured 2026-09-30) —
/// and the daemon it (re)starts as its own descendant, which lives for days, would
/// inherit it. Quiet, the verb gets a windowless console there and keeps sharing a
/// terminal's console, Ctrl+C off as the new group always had it.
fn own_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    aterm_uds::spawn::set_flags(
        cmd,
        aterm_uds::spawn::Console::Quiet,
        aterm_uds::spawn::CREATE_NEW_PROCESS_GROUP,
    );
}

/// Run `<exe> app-server daemon update --from-cli --yes` with exactly `env`
/// (the running daemon's), stdin closed, in its own process group. Output is
/// read on a thread so a daemon inheriting the pipe cannot hold the sweep. Past
/// `limit` the sweep stops waiting and says so; the verb is left to finish
/// (reaped by a thread), never killed, since a kill mid-copy leaves the package
/// half laid.
///
/// NOTHING BUT STDIO IS INHERITED. The verb (re)starts the daemon as its own
/// descendant, a process that lives for days. A plain spawn, on a std without
/// SP-1, hands on every descriptor without close-on-exec; one survives the
/// exec into the verb and on into the daemon. In the GUI that is Metal's
/// shader-cache files, open read-write without close-on-exec (measured
/// 2026-09-27), and anything caught between another thread's `pipe` and its
/// `fcntl`. So the verb is started by `aterm_uds::spawnfd::command`, which
/// hands it its stdio and nothing else — on Darwin one `posix_spawn` with
/// `POSIX_SPAWN_CLOEXEC_DEFAULT`, elsewhere the strip aterm-pty's shell child
/// runs (`InheritStrip`), bounded by the kernel's per-process maximum (the
/// product fd-hygiene sweep of 2026-09-27; the bound, 2026-09-30).
fn update_daemon(exe: &Path, env: &[String], limit: Duration) -> Result<Version, String> {
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.args(["app-server", "daemon", "update", "--from-cli", "--yes"])
        .env_clear()
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
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

/// A placeholder thread id of the real one's length, for planning a daemon
/// client's line before its exit names the thread.
const PLAN_THREAD: &str = "00000000-0000-0000-0000-000000000000";

/// How long a typed `/exit` may take to end the TUI (an embedded session shuts
/// its runtime down first).
const EXIT_WAIT: Duration = Duration::from_secs(30);

/// How old a recorded `/exit` must be before a still-live TUI is read as one
/// the `/exit` did not take: past the visit that typed it plus a second
/// [`EXIT_WAIT`]. From then on no visit waits for it under the sweep lock.
const EXIT_TAKEN_S: u64 = 2 * EXIT_WAIT.as_secs();

/// The `/exit` did not take: the TUI outlived it by more than a sweep (an Enter
/// Codex read as a newline). Nothing waits on it again: a `/exit` left in the
/// composer is recorded and left standing ([`left_standing`]), never taken back
/// or sent by the supervisor (a person's Enter would make it their exit, with
/// no relaunch); past [`STALE_S`] the move stops, `stale-exit`, said once.
fn exit_not_taken(opts: &Opts, r: Report, st: &mut St, c: &mut Client, at_s: u64) -> Report {
    let left = match left_standing(c, &st.tab, CODEX_EXIT) {
        Left::Gone => "no `/exit` is left in its composer",
        Left::Kept(why) => {
            st.left_typed = CODEX_EXIT.to_string();
            if matches!(why, "unsent" | "mixed") {
                "a `/exit` stands unsent in its composer; clear it there"
            } else {
                "its composer could not be read; a `/exit` may stand there unsent"
            }
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
        &format!("Codex survived its `/exit` and was never relaunched; {left}"),
    )
}

/// Where the kernel proves this TUI's thread lives ([`Mode`]): the thread whose
/// writer lock it holds (embedded), or none while its daemon lives (daemon
/// mode). `Err` names the wait.
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

/// [`super::composer_empty`] for Codex ([`cx::composer_is_empty`]): the caret
/// row's column-2 cell is asked for `dim` only when the cursor is there; a
/// reply without `dim` reads as a draft.
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
/// generation — what the typed turn is fenced on. The host's `help turn` is
/// asked BEFORE the read (once per connection: [`turn_takes_gen`] keeps the
/// answer), so the fenced `turn` asks nothing between the read and the paste.
fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    let _ = turn_takes_gen(c);
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

/// The guard a typed text's Enter is pressed under (`turn submit=guarded:<re>`):
/// the cursor's row must still be the composer's, ending in the text's last
/// word (the caret row, or a continuation row indented two). Codex reads an
/// Enter inside a typed burst as a newline, so the submit is guarded.
fn guard_for(text: &str) -> String {
    let word = text.split_whitespace().last().unwrap_or(text);
    let tail = row_guard(word);
    let tail = tail.trim_start_matches('^');
    // Either of the input line's marks (`›`, and 0.158.0's `»`): the row is
    // the composer's whichever the build draws.
    let marks: String = aterm_phase::codex::CARETS.iter().collect();
    if text.split_whitespace().count() == 1 {
        return format!("^[{marks}]\\s{tail}");
    }
    format!("^[{marks}\\s]\\s.*{tail}")
}

/// What [`type_text`] answers when the text was typed and not submitted: the
/// guarded Enter missed (`reason=guard`), or the Enter was written and the
/// composer still holds the text (`submitted=0 pressed=1`, read as a newline).
const LEFT_TYPED: &str = "left-typed";

/// Type `text` into the composer as one turn, fenced on `generation` (when the
/// host fences) and submitted under [`guard_for`]'s guard.
/// `Err(Typed::Refused(LEFT_TYPED))` when typed but not sent: the caller
/// records it ([`St::left_typed`]) and leaves it standing ([`left_standing`]).
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
            if screen(c, tab).is_some_and(|scr| composer_holds(&scr, text)) {
                Err(Typed::Refused(LEFT_TYPED.into()))
            } else {
                Ok(())
            }
        }
        Ok(()) => Ok(()),
        // Typed, but the Enter was not pressed: the cursor's row was no longer
        // the composer's (`reason=guard`).
        Err(Typed::Cut(why)) if why == "guard" => Err(Typed::Refused(LEFT_TYPED.into())),
        Err(e) => Err(e),
    }
}

/// What one line typed UNDER THE HARNESS'S HAND answered ([`type_under`]).
#[derive(Debug)]
enum Under {
    /// The tab is another driver's (`Hand::take` refused): nothing typed.
    Held,
    /// The fence's read refused the tab as it stood (a box, a draft, a
    /// screen that will not hold still): nothing typed.
    Fence(&'static str),
    /// [`type_text`]'s own answer.
    Typed(Typed),
}

/// [`type_text`] as every line this lane types is typed: UNDER THE
/// HARNESS'S HAND ([`under_hand`]: `hand` where the caller holds one, else
/// one for this line alone), so the window's presence layer reads it as
/// aterm's own and raises no row (ruling 313), and fenced on a read taken
/// under it. The wire is the lease acquire, the fence's `help turn` and
/// screen reads, the `turn`, the lease release — nothing of aterm's between
/// the last read and the `turn`.
fn type_under(c: &mut Client, tab: &str, text: &str, hand: Option<&mut Hand>) -> Result<(), Under> {
    under_hand(c, tab, hand, |c| {
        let generation = typing_fence(c, tab).map_err(Under::Fence)?;
        type_text(c, tab, text, generation.as_deref()).map_err(Under::Typed)
    })
    .unwrap_or(Err(Under::Held))
}

/// Whether Codex's composer on `scr` holds exactly `text` and nothing else
/// — a trailing empty line (an Enter taken as a newline) aside.
fn composer_holds(scr: &super::Screen, text: &str) -> bool {
    aterm_phase::codex::composer_draft(&scr.rows)
        .is_some_and(|(_, lines)| compact(&lines.concat()) == compact(text))
}

/// What stands of text the Codex lane left typed, read only (as the Claude
/// lane's `upgrade_drive::left_standing`). aterm never erases a person's words,
/// and screen fences cannot rule out a person's keys in flight, so the lane
/// never takes its own text back: it stands, recorded ([`St::left_typed`]) so
/// no look reads it as a person's draft, and the supervisor never sends it
/// (`supervise::policy::turn_end::Composer::Own`). The user is told and a
/// person clears it.
///
/// One read: the composer holding exactly `text` (whitespace dropped across
/// wrapping, [`composer_holds`]) is `unsent`; its head among more is `mixed`;
/// anything else, or nothing, is `Gone`. A box or no composer keeps the record.
fn left_standing(c: &mut Client, tab: &str, text: &str) -> Left {
    let Some(scr) = screen(c, tab) else {
        return Left::Kept("screen-unreadable");
    };
    if cx::box_on_screen(&scr.rows) {
        return Left::Kept("box");
    }
    if aterm_phase::codex::composer(&scr.rows).is_none() {
        return Left::Kept("no-composer");
    }
    if composer_empty(c, tab, &scr) {
        return Left::Gone;
    }
    if composer_holds(&scr, text) {
        return Left::Kept("unsent");
    }
    let head: String = compact(text).chars().take(24).collect();
    let mixed = aterm_phase::codex::composer_draft(&scr.rows)
        .is_some_and(|(_, lines)| compact(&lines.concat()).contains(&head));
    if mixed {
        Left::Kept("mixed")
    } else {
        Left::Gone
    }
}

/// The lane's own text was typed and not submitted: record it and say it once;
/// it stands ([`left_standing`]). `what` names it (`notice`, `exit`,
/// `goal-pause`, `goal-resume`).
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
    let shown = if what == "notice" {
        "the upgrade notice".to_string()
    } else {
        format!("`{text}`")
    };
    ledger(
        opts,
        &r,
        &format!("{shown} typed, not sent: it stays in the composer until a person clears it"),
    );
    let _ = settle_left_typed(st, c, tab);
    r
}

/// One read of what stands of [`St::left_typed`] ([`left_standing`], which
/// presses nothing): the record goes once the text is gone.
fn settle_left_typed(st: &mut St, c: &mut Client, tab: &str) -> Left {
    let left = left_standing(c, tab, &st.left_typed);
    if left == Left::Gone {
        st.left_typed.clear();
    }
    left
}

/// One visit to a Codex TUI: the Claude lane's visit, step for step, with
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
    // The tab's goal record ([`goal_hold`]): what the goal pause still owes.
    let gpath = goal_path(opts, &tui.tab).filter(|_| !opts.dry_run && !opts.background);
    let hold = gpath.as_deref().and_then(goal_hold::read);
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
                adopt(opts, r, &mut st, &mut c, tui.pid, k, None)
            } else {
                carry_on(opts, r, &mut st, &mut c, tui.pid, k, None)
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
        // Current: a state owed nothing more goes, so the status view never shows
        // a pending or failed move for a TUI already on its build.
        if !opts.dry_run
            && prior.as_ref().is_some_and(|st| {
                matches!(st.phase, Phase::Pending | Phase::Announced { .. })
                    || (matches!(st.phase, Phase::Failed(_)) && st.exited_at != 0)
            })
        {
            super::remove(opts, &session);
        }
        // A goal the move paused, still owed its resume: this Codex is not the
        // one it was paused in, or that one became current by other means.
        if let Some(path) = gpath.as_deref().filter(|_| cx::goal_owed(hold.as_ref())) {
            // Moved: the Codex here is aterm's relaunch. Another is the
            // person's own restart; the paused one, current, is abandoned.
            let other = hold.as_ref().is_some_and(|h| h.pid != tui.pid);
            let ours = prior
                .as_ref()
                .is_some_and(|st| st.relaunched_pid == tui.pid || st.resumed_pid == tui.pid);
            let moved = other && ours;
            let abandoned = match (other, ours) {
                (true, true) => None,
                (true, false) => Some(cx::BY_HAND),
                (false, _) => Some("abandoned:current"),
            };
            let mut st = prior.unwrap_or_default();
            return goal_look(
                opts,
                r,
                &mut st,
                (tui, k, daemon.as_ref()),
                path,
                hold.as_ref(),
                moved,
                abandoned,
            );
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
    // A stopped round rests, then a new one starts; no stop is for good. Until
    // `RETRY_S` has passed it waits `failed:<why>` (a given-up Codex round
    // hears no late READY). Past it the round is re-armed ([`super::rearm`])
    // unless the user's word holds it: `wait:skipped` or `wait:deferred`, as
    // the Claude lane words them.
    if let Phase::Failed(why) = &st.phase {
        if !upgrade::retry_due(&st.phase, st.failed_for(now), false) {
            // A goal the move paused is resumed: the round stopped, and the
            // move will not come before its rest ends.
            if let Some(path) = gpath.as_deref().filter(|_| cx::goal_owed(hold.as_ref())) {
                let r = goal_look(
                    opts,
                    r,
                    &mut st,
                    (tui, k, daemon.as_ref()),
                    path,
                    hold.as_ref(),
                    false,
                    Some("abandoned:failed"),
                );
                save(opts, &session, &st);
                return r;
            }
            return said(r, format!("wait:failed:{why}"));
        }
        let step = upgrade::rearm_held(&st.request_for(&tui.tab), Step::Rearm, &st.to, now);
        if let Step::Wait(held) = step {
            return said(r, format!("wait:{held}"));
        }
        let r = super::rearm(opts, r, &mut st, now);
        save(opts, &session, &st);
        return r;
    }
    // The process proofs: the TUI is its shell's foreground job on a terminal
    // an aterm tab owns.
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
    // An unreadable screen: recorded and timed as a blocker from the first look
    // that met it (`blocked:screen-unreadable` after BLOCKED_AFTER_S).
    let Some(scr) = screen(&mut c, &tui.tab) else {
        st.note_wait("screen-unreadable", now);
        st.note_blocked(Some("screen-unreadable"), now);
        save(opts, &session, &st);
        return said(r, "wait:screen-unreadable");
    };
    if st.blocked == "screen-unreadable" {
        st.note_blocked(None, now);
    }
    // The lane's own text left in the composer first, read and never pressed
    // ([`settle_left_typed`]): while it stands the look waits
    // (`wait:left-typed:<why>`). A dry run reads it like a real look and
    // saves nothing.
    if !st.left_typed.is_empty()
        && let Left::Kept(why) = settle_left_typed(&mut st, &mut c, &tui.tab)
    {
        let r = said(r, format!("wait:left-typed:{why}"));
        st.note_step(&r.step, "", now);
        save(opts, &session, &st);
        return r;
    }
    let mode = match mode_of(tui, daemon.is_some(), k) {
        Ok(mode) => mode,
        Err(why) => {
            st.note_wait(why, now);
            save(opts, &session, &st);
            return said(r, format!("wait:{why}"));
        }
    };
    // One `status` read: holds, hands, and the agent's verdict quiet is
    // measured from (`super::quiet_s`).
    let status = status_line(&mut c, &tui.tab);
    let quiet_s = quiet_s(status.as_deref(), &mut st, scr.seq, now);
    // The turn: an embedded conversation's rollout says it; a daemon-mode
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
    let (turn, busy_screen) = screen_turn(&scr.rows, rollout.as_deref().map(rollout_turn_at));
    // The ladder: rung, person, and the repaint-proof settle (the reader's idle
    // verdict, look after look, so a redrawn goal counter or clock does not
    // restart it; [`super::ladder_look`], shared with Claude Code's look and
    // the Tier-1 bind).
    let ladder = super::ladder_look(&mut st, "codex", &scr, tui.pid, opts.human_grace_s, now);
    // The reset the wall names ([`St::limit_until`]), in the zone this Codex
    // printed it in, kept as first placed while the words stand
    // ([`keep_wall`]); `0` when no wall, no named reset, or unreadable zone.
    keep_wall(
        &mut st,
        wall_reset_text(&scr.rows).as_deref(),
        k,
        tui.pid,
        &tui.home,
        now,
    );
    // And whether that wall is money, no usage window ([`St::limit_spend`]).
    st.limit_spend = wall_spends(&scr.rows);
    let mut facts = Facts {
        status: turn.to_string(),
        status_age_s: rollout.as_ref().map_or(quiet_s, |p| age_s(p, now)),
        composer_empty: composer_empty(&mut c, &tui.tab, &scr),
        approval_box: cx::box_on_screen(&scr.rows),
        busy_footer: busy_screen,
        background: k.background(tui.pid),
        // The hold and the hands alone: a person near the tab is `attended`,
        // which the ladder narrows to a keystroke from KeysOnly on.
        held: status.as_deref().is_none_or(|line| held_by_status(line, 0)),
        quiet_s,
        hold_s: 0,
        owner_now: false,
        attended: ladder.attended,
        background_point: opts.background,
        // A thread with no rollout yet (written with its first message) is no
        // conversation: never announced to, but ended with `/exit` and started
        // plain (`cx::next_step`, `Mode::Embedded { conversation: false }`); a
        // daemon client is never announced to.
        taskless: false,
        // A Codex at its usage wall is never typed at, and its clocks are held:
        // Codex records what is typed at the wall as a failed turn, so repeated
        // notices would pile up in history. The reducer waits `limited` before
        // announcing, re-asking, giving up or exiting ([`upgrade::clock_held`]).
        // The wall's block stays on screen until a turn goes through. The
        // quiet upgrade (default, `/exit` only) treats a wall that names its
        // reset as lifted an hour after it; the ask flow and a wall naming no
        // reset keep the screen's word. Pinned by the
        // `a_codex_at_its_usage_wall_*` tests.
        limited: upgrade::limited(Agent::Codex, &scr.rows),
        // A Codex whose login is gone answers nothing typed either: the same
        // wait, by the screen's word alone.
        login: upgrade::login_wall(Agent::Codex, &scr.rows),
        undelivered: false,
        // Claude Code's transcript alone says a queue of its notices is
        // full; a Codex rollout's is not read for it.
        queued: false,
        // Stamped below, once the READY is read ([`St::time_ready`]).
        ready_s: 0,
        // Status is already the screen's reading and the rollout's turn;
        // Claude Code's session-file status has no Codex counterpart.
        idle_looks: 0,
        // A stopped round never reaches here (re-armed or waiting, above).
        failed_s: st.failed_for(now),
        rung: ladder.rung,
        typing: ladder.typing,
        still_s: ladder.still_s,
        daemon_turn: codex_daemon_turn(
            &mut st,
            tui.pid,
            &mode,
            &scr.rows,
            daemon.as_ref(),
            &ladder,
            |pid| k.daemon_clients(pid),
            now,
        ),
        // Claude Code's lane alone asks at the end of the agent's own work
        // (`St::time_work`): the Codex reducer asks nothing of it.
        work_ended: false,
        // Only the Claude lane reads READY against the screen and transcript;
        // the Codex lane reads its rollout by its own rule.
        ready_unrecorded: false,
        ready_superseded: false,
        // `[harness] upgrade_ask = false` (default): an embedded conversation
        // is told nothing before its `/exit` ([`upgrade::Facts::quiet`]).
        quiet: !opts.ask,
        // The owner's waiting record has stood a look before a move that
        // types nothing first ([`upgrade::Facts::unnoticed`]).
        unnoticed: now.saturating_sub(st.behind_since()) < upgrade::quiet_s(),
    };
    // Quiet upgrade: a wall left on screen an hour past the reset it names is
    // no limit, and the tab is an idle point. A reset to come, a wall naming
    // none, and the ask flow keep the screen's word.
    if facts.quiet
        && st.limit_until != 0
        && now >= st.limit_until.saturating_add(super::LIMIT_GRACE_S)
    {
        facts.limited = false;
    }
    // An embedded session's background terminals are its own work (they die
    // with the `/exit`), with no shell between Codex and them: the kernel's
    // session leaders under the TUI plus Codex's status line. They count as
    // the agent's background, so the reducer's rule bounds the wait: waited
    // for, re-asked (naming them) after a READY stood REASK_S, given up past
    // MAX_ASKS. A daemon client's terminals run in the daemon and outlive
    // its `/exit`; the daemon step waits on them.
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
    // The READY answer's clock, held at the wall: work outliving the answer is
    // re-asked about, by name, after a whole re-ask window.
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
    // Waiting on terminals alone at an idle point keeps their name (and the
    // `/ps` remedy). A terminal never lets the exit through: it would end with
    // the TUI.
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
    // The goal pause (`cx::goal_step`): at the Land rung, a goal whose own turn
    // is all that holds the move is paused with `/goal pause` and resumed once
    // the move is made or abandoned.
    let mut goal_held = false;
    let pursued = aterm_phase::codex::goal_state(&scr.rows) == Some(CodexGoal::Pursuing);
    let goal_holds = pursued
        && match mode {
            Mode::Daemon => facts.daemon_turn == upgrade::DaemonTurn::Goal,
            Mode::Embedded { .. } => facts.status != "idle",
        };
    if let Some(path) = gpath.as_deref() {
        let hurried = st.request_for(&tui.tab) == upgrade::Request::Now;
        let waived = upgrade::gate_announce(&Facts {
            owner_now: hurried,
            status: "idle".to_string(),
            daemon_turn: upgrade::DaemonTurn::None,
            ..facts.clone()
        });
        // The daemon's own wait counts too: a client whose daemon is behind
        // moves only after it, and a daemon held by something the pause does
        // not end would leave the goal paused for nothing. Its busy threads
        // and settle are the goal's own turn and quiet.
        let waived = match daemon.as_ref().filter(|_| daemon_behind) {
            Some(d) if !matches!(d.wait.as_str(), "" | "busy-thread" | "settling") => {
                upgrade::Gate::Wait("daemon-first")
            }
            _ => waived,
        };
        let paused_elsewhere = hold
            .as_ref()
            .is_some_and(|h| h.owes() && h.owner == Owner::Upgrade && h.pid != tui.pid);
        let abandoned = match step {
            Step::Wait("skipped") => Some("abandoned:skipped"),
            Step::Wait("deferred") => Some("abandoned:deferred"),
            // The Codex paused is gone and this one, behind, is no relaunch
            // of aterm's: the person restarted it.
            _ if paused_elsewhere => Some(cx::BY_HAND),
            _ => None,
        };
        let weigh_esc = hold.as_ref().is_some_and(|h| {
            h.owner == Owner::Upgrade
                && h.stage == Stage::Pausing
                && h.how == How::Typed
                && now.saturating_sub(h.tried_at) >= cx::PAUSE_TAKE_S
        });
        let rollout = weigh_esc
            .then(|| goal_rollout(tui, &mode, daemon.as_ref(), k))
            .flatten();
        let mut look = cx::GoalLook {
            hold: hold.as_ref(),
            behind: true,
            goal: aterm_phase::codex::goal_state(&scr.rows),
            footer: aterm_phase::codex::footer_status(&scr.rows).is_some(),
            land: hurried || facts.rung == upgrade::Rung::Land,
            goal_holds,
            waived,
            switch: switch_open(opts, &tui.tab),
            moved: false,
            abandoned,
            head: rollout
                .as_deref()
                .map(|p| rollout_head_at(p) == TurnState::Head),
            free: facts.composer_empty && !facts.approval_box && !facts.busy_footer,
            esc_free: facts.composer_empty && !facts.approval_box,
            typing: facts.typing,
            person_since: person_since(scr.human, hold.as_ref(), now),
            sandboxed: false,
            now,
        };
        // A resume without the move is weighed against the thread's sandbox
        // first — read only then (its rollouts, `codex_usage::look`).
        if resumes_unmoved(&look) {
            look.sandboxed = thread_fell(tui, goal_thread(tui, &mode, daemon.as_ref(), k), k);
        }
        match cx::goal_step(&look) {
            GoalStep::Pass => {}
            // Quiet (`[harness] upgrade_ask = false`, default): no new pause
            // for the move; the goal's turn holds it like any turn. A pause an
            // earlier round made is still resumed.
            GoalStep::Pause if !opts.ask => {}
            GoalStep::Hold => goal_held = true,
            GoalStep::Pause => {
                if let Some(why) = pause_refused(opts, st.blocked.as_str(), tui, &mode, target, k) {
                    let r = said(r, format!("wait:goal:{why}"));
                    st.note_step(&r.step, &facts.status, now);
                    save(opts, &session, &st);
                    return r;
                }
                if tui
                    .claim
                    .as_ref()
                    .is_some_and(|claim| !claim_still_live(&mut c, tui.pid, claim))
                {
                    return said(r, "wait:tab-ownership-changed");
                }
                let r = goal_do(
                    opts,
                    r,
                    &mut st,
                    &mut c,
                    (&tui.tab, tui.pid, &target.version),
                    path,
                    hold.as_ref(),
                    GoalStep::Pause,
                    None,
                    None,
                );
                st.note_step(&r.step, &facts.status, now);
                save(opts, &session, &st);
                return r;
            }
            other => {
                let r = goal_do(
                    opts,
                    r,
                    &mut st,
                    &mut c,
                    (&tui.tab, tui.pid, &target.version),
                    path,
                    hold.as_ref(),
                    other,
                    rollout.as_deref(),
                    None,
                );
                st.note_step(&r.step, &facts.status, now);
                save(opts, &session, &st);
                return r;
            }
        }
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
            ledger(
                opts,
                &r,
                &gave_up_words(Agent::Codex, &k.held(tui.pid), facts.quiet),
            );
            r
        }
        Step::Announce => announce(opts, r, &mut st, &mut c, tui, target, &mode, proven, k, now),
        // Never planned here (`Facts::taskless` is false on this lane): a
        // thread with nothing to resume takes the plain restart above.
        Step::Fresh => said(r, "wait:fresh"),
        Step::Terminate if opts.dry_run => said(r, "would-exit"),
        Step::Terminate => match proven {
            Some(shell) => {
                // A QUIET move of a conversation nothing was asked of
                // ([`upgrade::Facts::quiet`]): nothing is typed after its
                // relaunch either ([`St::quiet`]). Not one THIS TUI was once
                // told to wind down (round eight, #23): a notice reached it
                // ([`St::noticed_at`], stamped by every notice and kept for
                // the TUI's record) and its round went back to pending since
                // — the owner's hold run out, a give-up re-armed by its rest
                // or by `--now` (a stop an older build recorded, with no
                // stamp: [`St::last_stop`]). This lane types no release line,
                // so the continuation is the only word that tells it to go on.
                let told_to_wind_down = st.noticed_at != 0 || st.last_stop == upgrade::GAVE_UP;
                st.quiet = facts.quiet
                    && mode.cooperative()
                    && st.phase == Phase::Pending
                    && !told_to_wind_down;
                exit(
                    opts,
                    r,
                    &mut st,
                    &mut c,
                    tui,
                    target,
                    (&mode, daemon.as_ref()),
                    shell,
                    k,
                )
            }
            None => said(r, "wait:ids"),
        },
    };
    // A goal-held wait worded as the goal's: before the pause (`goal`) and
    // after it (`goal-held`).
    let result = goal_wait_word(result, goal_held, goal_holds);
    // No `St::work_seen` here: nothing on this lane reads it. Its reducer
    // re-arms no resting round at the end of the TUI's work, so the owner's
    // view reads a quiet Codex give-up as one `--now` moves, whatever work
    // was seen (`upgrade_status::Row::rests_on_own_work`, round eight, the
    // review of #4's review).
    st.note_step(&result.step, &facts.status, now);
    save(opts, &session, &st);
    result
}

// ---------------------------------------------------------------- the goal pause

/// The tab's goal record ([`goal_hold::path`]) under [`Opts::aterm_state`];
/// `None` when unknown, and then no goal is ever paused.
fn goal_path(opts: &Opts, tab: &str) -> Option<PathBuf> {
    opts.aterm_state
        .as_deref()
        .map(|root| goal_hold::path(root, tab))
}

/// Whether a save-then-wait switch is open on `tab`: its loop's ledger holds an
/// unclosed wind-down ([`approvals::open_wind_down`]). Unknown (no state root)
/// reads as open.
fn switch_open(opts: &Opts, tab: &str) -> bool {
    opts.aterm_state.as_deref().is_none_or(|root| {
        approvals::open_wind_down(&approvals::ledger_under(root, Some(tab)), Some(tab)).is_some()
    })
}

/// A goal thread's turn, with its head told apart, from its rollout at `path`
/// ([`cx::rollout_head_back`]).
fn rollout_head_at(path: &Path) -> TurnState {
    let Ok(mut file) = std::fs::File::open(path) else {
        return TurnState::Unknown;
    };
    let len = file.metadata().map_or(0, |m| m.len());
    cx::rollout_head_back(&mut file, len, TAIL_BYTES, TURN_BACK_BYTES)
}

/// The rollout of the thread the goal runs in, for its head
/// ([`rollout_head_at`]): an embedded conversation's thread, or the root the
/// kernel names as a daemon client's own ([`cx::own_thread`]). `None` when
/// unproven, and then no Esc is pressed.
fn goal_rollout(
    tui: &Tui,
    mode: &Mode,
    daemon: Option<&DaemonView>,
    k: &dyn CodexKernel,
) -> Option<PathBuf> {
    find_rollout(&tui.home, &goal_thread(tui, mode, daemon, k)?)
}

/// The thread the goal runs in: an embedded conversation's, or the root the
/// kernel names as a daemon client's own ([`cx::own_thread`]); `None` when
/// unproven.
fn goal_thread(
    tui: &Tui,
    mode: &Mode,
    daemon: Option<&DaemonView>,
    k: &dyn CodexKernel,
) -> Option<String> {
    match mode {
        Mode::Embedded { thread, .. } => Some(thread.clone()),
        Mode::Daemon => {
            let d = daemon.filter(|d| d.pid != 0)?;
            let clients = k.daemon_clients(d.pid)?;
            cx::own_thread(tui.pid, &d.threads, Some(&clients)).map(str::to_string)
        }
    }
}

/// Whether look `g` would resume a goal without its move, the one resume a
/// sandbox withholds ([`cx::SANDBOXED`]); only then is the sandbox read.
fn resumes_unmoved(g: &cx::GoalLook) -> bool {
    match cx::goal_step(g) {
        GoalStep::Resume("moved") => false,
        // A resume made again is the move's where the first one was.
        GoalStep::Resume("again") => g.hold.is_some_and(|h| h.why != "moved"),
        GoalStep::Resume(_) => true,
        _ => false,
    }
}

/// Whether the goal's thread fell into a sandbox its launch bypassed, per
/// Codex's records under the TUI's home (`codex_usage::look`: the rollout's
/// last `turn_context`, or for an unnamed daemon thread every recent rollout
/// in the folder).
fn thread_fell(tui: &Tui, thread: Option<String>, k: &dyn CodexKernel) -> bool {
    use crate::supervise::codex_usage;
    let cwd = k.cwd(tui.pid);
    let now = std::time::SystemTime::now();
    codex_usage::look(
        &tui.home,
        codex_usage::Whose {
            cwd: cwd.as_deref(),
            argv: &tui.argv,
            thread: thread.as_deref(),
            active_at: now,
        },
        now,
    )
    .sandbox_fell
    .is_some()
}

/// The goal record `path` put back as it was before an act that did not go
/// ahead: `prior`, or none.
fn put_back(path: &Path, prior: Option<&Hold>) {
    match prior {
        Some(h) => {
            let _ = goal_hold::write(path, h);
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// How long the screen is read for a goal change the upgrade's key made
/// (`Goal paused`, `Pursuing goal`) before the next look takes it up.
const GOAL_SHOWS_WITHIN: Duration = Duration::from_secs(3);

/// Whether `tab`'s footer comes to show `want` within [`GOAL_SHOWS_WITHIN`].
fn goal_shows(c: &mut Client, tab: &str, want: CodexGoal) -> bool {
    wait_until(GOAL_SHOWS_WITHIN, || {
        screen(c, tab).is_some_and(|s| aterm_phase::codex::goal_state(&s.rows) == Some(want))
    })
}

/// The goal pause's acts and records at one look ([`cx::goal_step`]) in `tui`'s
/// tab: the goal record at `path` is written before every key (so a restarted
/// host knows what it owes) and put back where the key did not go. Each act's
/// report is its step word; the record's edges are ledgered.
#[allow(clippy::too_many_arguments)]
fn goal_do(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    (tab, pid, to): (&str, u32, &Version),
    path: &Path,
    prior: Option<&Hold>,
    step: GoalStep,
    rollout: Option<&Path>,
    hand: Option<&mut Hand>,
) -> Report {
    let now = now_s();
    let tab = tab.to_string();
    match step {
        GoalStep::Pass | GoalStep::Hold => r,
        // Left paused on purpose, its thread fallen into a sandbox: said once,
        // with the fix (relaunch with the launch's own flags).
        GoalStep::Wait(why) if why == cx::SANDBOXED => {
            let r = said(r, format!("wait:{why}"));
            if let Some(mut h) = prior.cloned()
                && h.say("sandboxed")
            {
                let _ = goal_hold::write(path, &h);
                ledger(
                    opts,
                    &r,
                    "the Codex goal aterm paused for the move stays paused: the move did not \
                     come, and its thread fell into a sandbox its launch bypassed, where it can \
                     neither commit nor push; quit Codex, resume the thread with its launch flags \
                     (codex resume --dangerously-bypass-approvals-and-sandbox <thread>) and type \
                     /goal resume",
                );
            }
            r
        }
        GoalStep::Wait(why) => said(r, format!("wait:{why}")),
        GoalStep::Pause => {
            let hold = Hold::pausing(Owner::Upgrade, How::Typed, pid, &to.to_string(), now);
            // Claim first: an open switch's hold refuses it.
            match goal_hold::claim(path, &hold) {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return said(r, "wait:goal:switch"),
                Err(_) => return said(r, "wait:goal:record"),
            }
            match type_under(c, &tab, GOAL_PAUSE, hand) {
                Ok(()) => {
                    let mut hold = hold;
                    if goal_shows(c, &tab, CodexGoal::Paused) {
                        hold.stage = Stage::Paused;
                        hold.took_at = now_s();
                    }
                    let _ = goal_hold::write(path, &hold);
                    let r = said(r, "goal-paused");
                    ledger(
                        opts,
                        &r,
                        &format!(
                            "the Codex goal paused for the move ({GOAL_PAUSE}: the turn running \
                             finishes, and no turn follows it); aterm resumes it once Codex is \
                             on {to}"
                        ),
                    );
                    r
                }
                Err(Under::Typed(Typed::Refused(why))) if why == LEFT_TYPED => {
                    put_back(path, prior);
                    left_typed(opts, r, st, c, &tab, GOAL_PAUSE, "goal-pause")
                }
                Err(Under::Held) => {
                    put_back(path, prior);
                    said(r, "wait:held")
                }
                Err(Under::Fence(why)) => {
                    put_back(path, prior);
                    said(r, format!("wait:{why}"))
                }
                Err(Under::Typed(e)) => {
                    put_back(path, prior);
                    said(r, format!("wait:goal-pause-refused:{}", e.word()))
                }
            }
        }
        GoalStep::Esc => {
            let Some(held_now) = prior.cloned() else {
                return said(r, "wait:goal-pausing");
            };
            goal_esc(opts, r, c, &tab, path, held_now, rollout)
        }
        GoalStep::Took => {
            let Some(mut h) = prior.cloned() else {
                return r;
            };
            h.stage = Stage::Paused;
            h.took_at = now;
            let _ = goal_hold::write(path, &h);
            let r = said(r, "wait:goal-held");
            ledger(
                opts,
                &said(r.clone(), "goal-paused-seen"),
                "the Codex goal shows paused",
            );
            r
        }
        GoalStep::Resume(why) => {
            let Some(h) = prior.cloned() else {
                return r;
            };
            goal_resume_typed(opts, r, st, c, &tab, path, h, why, hand)
        }
        GoalStep::Resumed => {
            let Some(mut h) = prior.cloned() else {
                return r;
            };
            h.stage = Stage::Resumed;
            let _ = goal_hold::write(path, &h);
            let r = said(r, "goal-resumed");
            ledger(opts, &r, "the Codex goal shows pursued again");
            r
        }
        GoalStep::Release(why) => {
            let Some(mut h) = prior.cloned() else {
                return r;
            };
            h.stage = Stage::Released;
            h.why = why.to_string();
            let _ = goal_hold::write(path, &h);
            let r = said(r, format!("goal-released:{why}"));
            ledger(
                opts,
                &r,
                if why == "by-hand" {
                    "a person's hand is on the Codex goal aterm paused (their keystroke since, or \
                     the goal pursued again): it is theirs, and aterm will not resume it"
                } else {
                    "the Codex goal aterm paused is no longer paused nor pursued: nothing to \
                     resume"
                },
            );
            r
        }
        GoalStep::GiveUp => {
            let Some(mut h) = prior.cloned() else {
                return r;
            };
            h.stage = Stage::Released;
            h.why = "pause-refused".to_string();
            h.say("pause-refused");
            let _ = goal_hold::write(path, &h);
            let r = said(r, "goal-pause-refused");
            ledger(
                opts,
                &r,
                &format!(
                    "the Codex goal never showed paused ({GOAL_PAUSE}, then Esc at a turn's head): \
                     aterm leaves it running, and the move waits for it as for any turn"
                ),
            );
            r
        }
    }
}

/// The Esc at a goal turn's head ([`GoalStep::Esc`]), read again just before
/// the key: rollout still at its head ([`TurnState::Head`]), footer still
/// `Pursuing goal`, no box, empty composer, no keystroke within
/// [`upgrade::KEYS_GAP_S`]. Then one Esc, fenced and guarded on the footer's
/// goal row, the record naming it first; otherwise nothing
/// (`wait:goal-pausing`).
fn goal_esc(
    opts: &Opts,
    r: Report,
    c: &mut Client,
    tab: &str,
    path: &Path,
    mut hold: Hold,
    rollout: Option<&Path>,
) -> Report {
    let Some(rollout) = rollout else {
        return said(r, "wait:goal-pausing");
    };
    if rollout_head_at(rollout) != TurnState::Head {
        return said(r, "wait:goal-pausing");
    }
    let Some(scr) = screen(c, tab) else {
        return said(r, "wait:screen-unreadable");
    };
    let pursuing = aterm_phase::anchors::anchor("codex.goal.pursuing");
    let footer = scr.rows.iter().rposition(|row| row.contains(pursuing));
    let Some(row) = footer.filter(|_| {
        aterm_phase::codex::goal_state(&scr.rows) == Some(CodexGoal::Pursuing)
            && !cx::box_on_screen(&scr.rows)
    }) else {
        return said(r, "wait:goal-pausing");
    };
    if !composer_empty(c, tab, &scr) || held(c, tab, upgrade::KEYS_GAP_S) {
        return said(r, "wait:goal-pausing");
    }
    let fence = scr
        .generation
        .as_deref()
        .filter(|g| is_generation(g))
        .filter(|_| key_takes_gen(c));
    let Some(generation) = fence else {
        return said(r, "wait:goal-pausing:no-fence");
    };
    let before = hold.clone();
    hold.how = How::Esc;
    hold.tried_at = now_s();
    if goal_hold::write(path, &hold).is_err() {
        return said(r, "wait:goal:record");
    }
    let args = key_args(&row_guard(&scr.rows[row]), "esc", Some(generation));
    // The Esc goes under aterm's own hand (another driver's tab: not pressed).
    let key = under_hand(c, tab, None, |c| {
        c.request_line(&format!("@{tab} key {args}"))
    })
    .and_then(Result::ok);
    match key {
        Some(l) if l.starts_with("OK") && !l.split_whitespace().any(|w| w == "skipped") => {
            if goal_shows(c, tab, CodexGoal::Paused) {
                hold.stage = Stage::Paused;
                hold.took_at = now_s();
                let _ = goal_hold::write(path, &hold);
            }
            let r = said(r, "goal-esc");
            ledger(
                opts,
                &r,
                "the Codex goal's turn stopped at its head with Esc (nothing of it had run), \
                 which pauses the goal: its typed pause never showed",
            );
            r
        }
        // Not pressed (the screen moved past the judged read, or the guard
        // missed): the record as it was, the one Esc still to come.
        _ => {
            let _ = goal_hold::write(path, &before);
            said(r, "wait:goal-pausing")
        }
    }
}

/// Type the resume ([`GoalStep::Resume`]): the record says `Resuming` first (a
/// restarted host verifies it on the footer before resuming again), then
/// `/goal resume` at an empty composer, fenced and guarded; put back where it
/// did not go. `why` (`moved`, `abandoned:<why>`, `bound`, `again`) is said
/// once where the move did not come.
#[allow(clippy::too_many_arguments)]
fn goal_resume_typed(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    path: &Path,
    hold: Hold,
    why: &'static str,
    hand: Option<&mut Hand>,
) -> Report {
    let before = hold.clone();
    let mut h = hold;
    h.stage = Stage::Resuming;
    h.resume_at = now_s();
    h.resumes = h.resumes.saturating_add(1);
    if why != "again" {
        h.why = why.to_string();
    }
    // The record first (a host that restarts after the key finds what it
    // owes), the line under the hand after it; put back where it did not go.
    if goal_hold::write(path, &h).is_err() {
        return said(r, "wait:goal:record");
    }
    match type_under(c, tab, GOAL_RESUME, hand) {
        Ok(()) => {
            if goal_shows(c, tab, CodexGoal::Pursuing) {
                h.stage = Stage::Resumed;
            }
            let missed = h.why != "moved";
            if missed {
                h.say("abandoned");
            }
            let _ = goal_hold::write(path, &h);
            let r = said(r, format!("goal-resumed:{}", word_of(&h.why)));
            ledger(
                opts,
                &r,
                &if missed {
                    format!(
                        "the Codex goal resumed ({GOAL_RESUME}) without the move to {}: the move \
                         did not come ({}); the upgrade tries again after a rest",
                        h.to, h.why
                    )
                } else {
                    format!(
                        "the Codex goal resumed ({GOAL_RESUME}) on {}: it carries on where it was",
                        h.to
                    )
                },
            );
            r
        }
        Err(Under::Typed(Typed::Refused(w))) if w == LEFT_TYPED => {
            put_back(path, Some(&before));
            left_typed(opts, r, st, c, tab, GOAL_RESUME, "goal-resume")
        }
        Err(Under::Held) => {
            put_back(path, Some(&before));
            said(r, "wait:held")
        }
        Err(Under::Fence(w)) => {
            put_back(path, Some(&before));
            said(r, format!("wait:{w}"))
        }
        Err(Under::Typed(e)) => {
            put_back(path, Some(&before));
            said(r, format!("wait:goal-resume-refused:{}", e.word()))
        }
    }
}

/// A reason as one word of a step (`abandoned:failed` → `abandoned-failed`).
fn word_of(why: &str) -> String {
    why.replace(':', "-")
}

/// The relaunched Codex's first moments, for a goal the move paused: where the
/// goal record owes a resume, the new TUI `new` is put on the record
/// ([`Hold::relaunched`], the one Codex whose box the approval policy may
/// answer for this hold), then read for up to [`RESUME_BOX_WAIT`] until it
/// shows its `Resume paused goal?` box ([`aterm_phase::codex::resume_box`]) or
/// a footer naming the goal; never merely a composer, which is drawn briefly
/// before the box. Then, with no keystroke within [`upgrade::KEYS_GAP_S`]:
///
/// * the box with `Resume goal` focused: [`answer_box`];
/// * the box with focus elsewhere, or a person at the keys: theirs, nothing
///   pressed;
/// * footer `Goal paused (/goal resume)`: [`goal_resume_typed`];
/// * footer `Pursuing goal`: already resumed; released, nothing typed.
///
/// `Some(word)` for what was done, `None` where the record owes nothing, or
/// neither showed.
fn resume_on_relaunch(
    opts: &Opts,
    st: &mut St,
    c: &mut Client,
    new: u32,
    hand: Option<&mut Hand>,
) -> Option<String> {
    let path = goal_path(opts, &st.tab)?;
    let mut hold = goal_hold::read(&path).filter(|h| cx::goal_owed(Some(h)))?;
    if hold.relaunched != new {
        hold.relaunched = new;
        let _ = goal_hold::write(&path, &hold);
    }
    let tab = st.tab.clone();
    let mut read = None;
    wait_until(RESUME_BOX_WAIT, || {
        read = screen(c, &tab).filter(|s| {
            aterm_phase::codex::resume_box(&s.rows).is_some()
                || aterm_phase::codex::goal_state(&s.rows).is_some()
        });
        read.is_some()
    });
    let scr = read?;
    let r = Report {
        pid: new,
        tab: tab.clone(),
        session: key(&tab),
        from: st.from.clone(),
        to: format!("{}({})", st.to, st.source),
        step: String::new(),
    };
    if let Some(word) = answer_box(opts, &st.to, c, &path, hold.clone(), &scr, r.clone()) {
        return Some(word);
    }
    if held(c, &tab, upgrade::KEYS_GAP_S) {
        return Some("goal-resume:held".to_string());
    }
    let owed = Some(hold);
    let step = cx::goal_step(&cx::GoalLook {
        hold: owed.as_ref(),
        behind: false,
        goal: aterm_phase::codex::goal_state(&scr.rows),
        footer: aterm_phase::codex::footer_status(&scr.rows).is_some(),
        land: true,
        goal_holds: false,
        waived: upgrade::Gate::Go,
        switch: switch_open(opts, &tab),
        moved: true,
        abandoned: None,
        head: None,
        free: composer_empty(c, &tab, &scr) && !cx::busy_on_screen(&scr.rows),
        esc_free: false,
        typing: false,
        person_since: false,
        // The move is made: a resume here is the move's, never withheld.
        sandboxed: false,
        now: now_s(),
    });
    let to = Version::parse(&st.to)?;
    let r = goal_do(
        opts,
        r,
        st,
        c,
        (&tab, new, &to),
        &path,
        owed.as_ref(),
        step,
        None,
        hand,
    );
    (!r.step.is_empty()).then_some(r.step)
}

/// The paused goal's box on the Codex the upgrade relaunched, read in `scr`
/// (`None` when absent). If a person has it (a keystroke within
/// [`upgrade::KEYS_GAP_S`], or focus moved off the resume) that is recorded
/// ([`cx::BOX_THEIRS`]) and nothing is pressed; else the record says
/// `Resuming`, then one Enter fenced and guarded on the focused row, with the
/// goal's pursuit verified. `hold` is the record at `path`; `to` the build.
/// Returns `goal-resumed:moved` when pressed, `goal-resume:<why>` when not.
fn answer_box(
    opts: &Opts,
    to: &str,
    c: &mut Client,
    path: &Path,
    hold: Hold,
    scr: &super::Screen,
    r: Report,
) -> Option<String> {
    let (row, resume) = aterm_phase::codex::resume_box(&scr.rows)?;
    let tab = r.tab.clone();
    let theirs = |word: &str| {
        let mut h = hold.clone();
        h.tried_at = now_s();
        h.say(cx::BOX_THEIRS);
        let _ = goal_hold::write(path, &h);
        Some(word.to_string())
    };
    if held(c, &tab, upgrade::KEYS_GAP_S) {
        return theirs("goal-resume:held");
    }
    if !resume {
        return theirs("goal-resume:box");
    }
    let Some(generation) = scr
        .generation
        .as_deref()
        .filter(|g| is_generation(g))
        .filter(|_| key_takes_gen(c))
    else {
        return Some("goal-resume:no-fence".to_string());
    };
    let mut h = hold;
    h.stage = Stage::Resuming;
    h.resume_at = now_s();
    h.resumes = h.resumes.saturating_add(1);
    h.why = "moved".to_string();
    if goal_hold::write(path, &h).is_err() {
        return Some("goal-resume:record".to_string());
    }
    let args = key_args(&row_guard(&scr.rows[row]), "enter", Some(generation));
    // The Enter goes under aterm's own hand (another driver's tab: not pressed).
    let pressed = under_hand(&mut *c, &tab, None, |c| {
        c.request_line(&format!("@{tab} key {args}"))
    })
    .and_then(Result::ok)
    .is_some_and(|l| l.starts_with("OK") && !l.split_whitespace().any(|w| w == "skipped"));
    if !pressed {
        h.stage = Stage::Paused;
        h.resumes = h.resumes.saturating_sub(1);
        let _ = goal_hold::write(path, &h);
        return Some("goal-resume:missed".to_string());
    }
    if goal_shows(c, &tab, CodexGoal::Pursuing) {
        h.stage = Stage::Resumed;
        let _ = goal_hold::write(path, &h);
    }
    let r = said(r, "goal-resumed:moved");
    ledger(
        opts,
        &r,
        &format!(
            "the relaunched Codex asked whether to resume its paused goal, and aterm, which \
             paused it for the move, resumed it: it carries on on {to}"
        ),
    );
    Some(r.step)
}

/// How long the relaunched Codex is read for its paused-goal box or composer
/// ([`resume_on_relaunch`]).
const RESUME_BOX_WAIT: Duration = Duration::from_secs(20);

/// Whether a person's hand has been on the tab since the hold's last edge
/// ([`Hold::last_at`]), by stamp `human` at `now`; an unknown stamp counts as a
/// hand. `false` with no hold.
fn person_since(human: HumanInput, hold: Option<&Hold>, now: u64) -> bool {
    hold.is_some_and(|h| {
        let since = u32::try_from(now.saturating_sub(h.last_at())).unwrap_or(u32::MAX);
        human.within(since).unwrap_or(true)
    })
}

/// Why a pause would buy nothing though the goal alone holds the move: a
/// blocker refused the move ([`St::blocked`]), the TUI is not the foreground
/// job, or its relaunch line cannot be planned. `None`: pause.
fn pause_refused(
    opts: &Opts,
    blocked: &str,
    tui: &Tui,
    mode: &Mode,
    target: &Target,
    k: &dyn CodexKernel,
) -> Option<&'static str> {
    if !blocked.is_empty() {
        return Some("blocked");
    }
    let Ok(shell) = foreground_shell(k.job(tui.pid)) else {
        return Some("ids");
    };
    let thread = match mode {
        Mode::Daemon => Some(PLAN_THREAD.to_string()),
        Mode::Embedded { thread, .. } => Some(thread.clone()),
    };
    plan(opts, tui, shell, thread.as_deref(), target, k)
        .err()
        .map(|_| "unplanned")
}

/// The step's wait worded as the goal's: `goal-held` for every wait while the
/// paused goal is held, `goal` where a pursued goal's own turn holds the move.
///
/// `goal-held` owns the session's turn ends
/// (`upgrade_drive::owns_turn_ends`), and must at every wait of a held goal:
/// otherwise the supervisor could type a continuation into the paused Codex,
/// keeping its thread busy so the daemon never moves.
fn goal_wait_word(r: Report, held: bool, holds: bool) -> Report {
    // Every wait but the goal lane's own words (`wait:goal-…`, `wait:goal:…`),
    // the daemon's `wait:goal` among them.
    let ordinary =
        r.step.starts_with("wait:") && (r.step == "wait:goal" || !r.step.starts_with("wait:goal"));
    if held && ordinary {
        said(r, "wait:goal-held")
    } else if holds && matches!(r.step.as_str(), "wait:not-idle" | "wait:busy") {
        said(r, "wait:goal")
    } else {
        r
    }
}

/// A goal the move paused, looked at where no move is taken (the Codex current,
/// `moved`, or the round stopped, `abandoned`) at the tab's idle point: one
/// screen read, then [`cx::goal_step`] carried out ([`goal_do`]). `st` is the
/// tab's upgrade record, for text the lane may leave typed.
#[allow(clippy::too_many_arguments)]
fn goal_look(
    opts: &Opts,
    r: Report,
    st: &mut St,
    (tui, k, daemon): (&Tui, &dyn CodexKernel, Option<&DaemonView>),
    path: &Path,
    hold: Option<&Hold>,
    moved: bool,
    abandoned: Option<&'static str>,
) -> Report {
    let Ok(mut c) = connect(opts, &tui.tab) else {
        return said(r, "wait:no-socket");
    };
    if !roster(&mut c).contains(&tui.tab) {
        return said(r, "wait:tab-not-live");
    }
    let Some(scr) = screen(&mut c, &tui.tab) else {
        return said(r, "wait:screen-unreadable");
    };
    let now = now_s();
    let switch = switch_open(opts, &tui.tab);
    // The relaunch's box still standing at a later look: answered as there,
    // under the same guards (aterm's relaunch, box not a person's, no switch
    // open).
    if let Some(h) = hold.filter(|h| moved && !switch && h.stage == Stage::Paused) {
        let mut h = h.clone();
        if h.relaunched != tui.pid {
            h.relaunched = tui.pid;
            let _ = goal_hold::write(path, &h);
        }
        if !h.said(cx::BOX_THEIRS)
            && let Some(word) = answer_box(opts, &h.to, &mut c, path, h.clone(), &scr, r.clone())
        {
            // Not pressed (a person at the keys, its focus moved, no fence):
            // the resume is waited on, as for any box standing there.
            return if word.starts_with("goal-resumed") {
                said(r, word)
            } else {
                said(r, "wait:goal-resume")
            };
        }
    }
    let free = !cx::box_on_screen(&scr.rows)
        && !cx::busy_on_screen(&scr.rows)
        && composer_empty(&mut c, &tui.tab, &scr);
    let mut look = cx::GoalLook {
        hold,
        behind: false,
        goal: aterm_phase::codex::goal_state(&scr.rows),
        footer: aterm_phase::codex::footer_status(&scr.rows).is_some(),
        land: true,
        goal_holds: false,
        waived: upgrade::Gate::Go,
        switch,
        moved,
        abandoned,
        head: None,
        free,
        esc_free: false,
        typing: scr.human.within(upgrade::KEYS_GAP_S).unwrap_or(true),
        person_since: person_since(scr.human, hold, now),
        sandboxed: false,
        now,
    };
    if resumes_unmoved(&look) {
        let thread = mode_of(tui, daemon.is_some(), k)
            .ok()
            .and_then(|mode| goal_thread(tui, &mode, daemon, k));
        look.sandboxed = thread_fell(tui, thread, k);
    }
    let step = cx::goal_step(&look);
    // The lane's own words left unsent (a missed guarded Enter of `/goal
    // resume`, or a failed round's `/exit` or `/goal pause`) stand, never
    // taken back. Any such line on screen, or the text the record says it
    // left, is said for what it is: a person's hold, owning no turn end, for
    // the supervisor to escalate.
    let own_left = [CODEX_EXIT, GOAL_PAUSE, GOAL_RESUME]
        .iter()
        .any(|line| composer_holds(&scr, line))
        || (!st.left_typed.is_empty() && composer_holds(&scr, &st.left_typed));
    if matches!(step, GoalStep::Wait("goal-resume" | "goal-resuming")) && own_left {
        return said(r, "wait:left-typed:unsent");
    }
    let r = goal_do(
        opts,
        r,
        st,
        &mut c,
        (&tui.tab, tui.pid, &tui.version),
        path,
        hold,
        step,
        None,
        None,
    );
    if r.step.is_empty() {
        said(r, "current")
    } else {
        r
    }
}

/// The turn a Codex TUI's screen shows: `busy` while its status row runs (`esc
/// to interrupt`, [`cx::busy_on_screen`]) or, for an embedded conversation,
/// while its rollout's last turn runs (`rollout`; `None` for a daemon client,
/// [`codex_daemon_turn`]), else `idle`; and whether that row is up. Shared
/// with the ladder's Tier-1 bind ([`Facts::status`], [`Facts::busy_footer`]).
pub(super) fn screen_turn(rows: &[String], rollout: Option<TurnState>) -> (&'static str, bool) {
    let busy_screen = cx::busy_on_screen(rows);
    let idle = !busy_screen && rollout.is_none_or(|t| t == TurnState::Idle);
    (if idle { "idle" } else { "busy" }, busy_screen)
}

/// The words a Codex usage wall names its reset by: the `try again at <time>`
/// of the limiting wall the last turn ended on ([`aterm_phase::codex::wall`]).
/// Walls that are no limit (context window, overload, server error) name no
/// reset here. `None`: no such wall, or `try again later`.
pub(super) fn wall_reset_text(rows: &[String]) -> Option<String> {
    aterm_phase::codex::wall(rows)
        .filter(|w| w.kind.reads_limited())?
        .reset
}

/// Whether the wall the last turn ended on is money (a spend cap, credit limit,
/// no credits: [`aterm_phase::WallKind::Spend`]): no window, no `try again at`,
/// only the account's person lifts it ([`St::limit_spend`]).
pub(super) fn wall_spends(rows: &[String]) -> bool {
    aterm_phase::codex::wall(rows).is_some_and(|w| w.kind == aterm_phase::WallKind::Spend)
}

/// The reset `text` names ([`wall_reset_text`]) in unix seconds, past or future,
/// placed at `now` by the supervisor's grammar
/// ([`crate::supervise::limit::parse_reset`],
/// [`crate::supervise::limit::reset_in_zone`]) in the zone the Codex printed it
/// in (`offset_at(t)`, [`wall_zone`], [`zone_offset_at`]). A bare clock time
/// is the print's own day (codex-rs names the date otherwise), so never
/// tomorrow (`ahead_max` 0); [`keep_wall_reset`] keeps the first placement.
/// `None`: unreadable words or zone offset, never guessed.
pub(super) fn wall_reset_at(
    text: &str,
    now: u64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<u64> {
    use crate::supervise::limit;
    let spec = limit::parse_reset(text)?;
    let at = limit::reset_in_zone(&spec, i64::try_from(now).ok()?, 0, offset_at)?;
    u64::try_from(at).ok().filter(|&at| at > 0)
}

/// The zone a Codex prints its wall's reset in: codex-rs uses chrono's `Local`
/// (the process's `TZ`, else the machine's zone) and names no zone, so it is
/// read from the Codex `pid`'s start environment ([`CodexKernel::tz`];
/// `Some(None)`: the machine's zone). A daemon in `home` may print a client's
/// wall, so its zone must agree. `None`: unreadable or disagreeing.
fn wall_zone(k: &dyn CodexKernel, pid: u32, home: &Path) -> Option<Option<String>> {
    let tz = k.tz(pid)?;
    let daemon = std::fs::read_to_string(home.join("app-server-daemon/daemon.pid"))
        .ok()
        .and_then(|text| cx::parse_daemon_pid(&text))
        .filter(|&daemon| daemon != pid && k.alive(daemon));
    match daemon {
        Some(daemon) => (k.tz(daemon)? == tz).then_some(tz),
        None => Some(tz),
    }
}

/// `zone`'s UTC offset at instant `t`: a `TZ` naming a zone the machine knows
/// (leading `:` dropped, [`crate::supervise::limit::offset_at`]; any other
/// reads `None`), or with none named the machine's own zone
/// ([`crate::supervise::limit::machine_offset_at`]). Never this process's `TZ`,
/// never a silent UTC.
fn zone_offset_at(zone: Option<&str>, t: i64) -> Option<i64> {
    use crate::supervise::limit;
    match zone {
        Some(tz) => limit::offset_at(Some(tz.strip_prefix(':').unwrap_or(tz)), t),
        None => limit::machine_offset_at(t),
    }
}

/// The wall's reset kept on the record (`St::limit_text`, `St::limit_until`):
/// `text` is what the on-screen wall names now ([`wall_reset_text`]; `None`
/// clears to `0`). The same words are the same reset and are not placed again
/// (a bare time re-placed each look would drift a day at every midnight), as
/// `supervise::run`'s `refresh_reset` keeps a notice's. New words are placed by
/// `place` ([`wall_reset_at`]); unplaceable words keep nothing.
pub(super) fn keep_wall_reset(
    st: &mut St,
    text: Option<&str>,
    place: impl FnOnce(&str) -> Option<u64>,
) {
    let kept = match text {
        Some(text) if text == st.limit_text && st.limit_until != 0 => return,
        Some(text) => place(text).map(|at| (text, at)),
        None => None,
    };
    let (text, at) = kept.unwrap_or_default();
    text.clone_into(&mut st.limit_text);
    st.limit_until = at;
}

/// [`keep_wall_reset`] for one look at Codex `pid` (home `home`), new words
/// placed at `now` in its printing zone ([`place_wall`]); used by visit and
/// watch alike.
pub(super) fn keep_wall(
    st: &mut St,
    text: Option<&str>,
    k: &dyn CodexKernel,
    pid: u32,
    home: &Path,
    now: u64,
) {
    keep_wall_reset(st, text, |text| place_wall(k, pid, home, text, now));
}

/// The reset a wall's words `text` name, placed at `now` in the zone Codex
/// `pid` (home `home`) printed them in ([`wall_zone`], [`wall_reset_at`]).
/// Shared by [`keep_wall`] and the supervise loop (`super::codex_wall_reset`),
/// so one wall has one reading. `None`: unreadable words or zone.
pub(super) fn place_wall(
    k: &dyn CodexKernel,
    pid: u32,
    home: &Path,
    text: &str,
    now: u64,
) -> Option<u64> {
    let zone = wall_zone(k, pid, home)?;
    wall_reset_at(text, now, &|t| zone_offset_at(zone.as_deref(), t))
}

/// [`wall_reset_text`] of `tab`'s screen and whether its wall is money
/// ([`wall_spends`]), read now for the watch (`upgrade_watch`); one `text` read,
/// nothing typed. `None`: no screen read; the record keeps what it had.
pub(super) fn screen_wall_text(opts: &Opts, tab: &str) -> Option<(Option<String>, bool)> {
    let mut c = connect(opts, tab).ok()?;
    let scr = screen(&mut c, tab)?;
    Some((wall_reset_text(&scr.rows), wall_spends(&scr.rows)))
}

/// The daemon's turns as a client's `/exit` must wait for them
/// ([`upgrade::Facts::daemon_turn`], [`cx::daemon_turn`]), for Codex `pid` in
/// `mode` at one look (screen `rows`, ladder reading `ladder`), its home's
/// daemon as read (`daemon`), and the daemon's attached clients (`clients`,
/// asked only when a thread runs), at `now`. In `st` it keeps the threads found
/// running across this screen's looks ([`St::still_busy`]), so a root running
/// through [`upgrade::QUIET_S`] with another Codex attached is placed in
/// another session ([`cx::still_through`]); nothing while the footer shows a
/// goal being pursued ([`cx::goal_on_screen`]), since that goal's next turn is
/// indistinguishable. [`upgrade::DaemonTurn::None`] for an embedded session and
/// where no thread runs. Shared with the ladder's Tier-1 bind.
#[allow(clippy::too_many_arguments)]
pub(super) fn codex_daemon_turn(
    st: &mut St,
    pid: u32,
    mode: &Mode,
    rows: &[String],
    daemon: Option<&DaemonView>,
    ladder: &super::LadderLook,
    clients: impl FnOnce(u32) -> Option<Vec<u32>>,
    now: u64,
) -> upgrade::DaemonTurn {
    let running: Vec<String> = daemon
        .map(|d| {
            d.threads
                .iter()
                .filter(|l| l.running())
                .map(|l| l.thread.clone())
                .collect()
        })
        .unwrap_or_default();
    // The still run's record of running threads (`<thread>@<first look>`),
    // kept across looks of one run, restarted with a new one, and empty while
    // the screen is not idle or shows a pursued goal.
    let goal = cx::goal_on_screen(rows);
    let in_run = ladder.idle_read && ladder.still_s > 0;
    let (record, elsewhere) = if ladder.idle_read && !goal {
        cx::still_through(&still_record(st), &running, in_run, now)
    } else {
        (Vec::new(), Vec::new())
    };
    st.still_busy = record
        .iter()
        .map(|(thread, since)| format!("{thread}@{since}"))
        .collect::<Vec<_>>()
        .join(" ");
    let Some(d) = daemon.filter(|_| *mode == Mode::Daemon && !running.is_empty()) else {
        return upgrade::DaemonTurn::None;
    };
    let attached = (d.pid != 0).then(|| clients(d.pid)).flatten();
    cx::daemon_turn(pid, &d.threads, attached.as_deref(), &elsewhere, goal)
}

/// The still run's record as the last look left it ([`St::still_busy`]:
/// `<thread>@<first look that found it running>`, space-separated).
fn still_record(st: &St) -> Vec<(String, u64)> {
    st.still_busy
        .split_whitespace()
        .filter_map(|entry| {
            let (thread, since) = entry.split_once('@')?;
            Some((thread.to_string(), since.parse().ok()?))
        })
        .collect()
}

/// The conversation the kernel names for a daemon client ([`cx::own_thread`]),
/// from the daemon as the `/exit`'s last look read it (`d`): every held thread
/// hangs from one root (the conversation and its subagents) and this TUI is its
/// only client; the root is what `codex resume` takes. Used where shell
/// integration marks do not reach aterm ([`marks_reach`]). `None` where the
/// kernel does not prove it, and the `/exit` then waits
/// (`no-shell-integration`).
fn kernel_thread(tui: &Tui, d: &DaemonRead, k: &dyn CodexKernel) -> Option<String> {
    let threads: Turns = d.threads.iter().map(|(l, _)| l.clone()).collect();
    let clients = k.daemon_clients(d.pid)?;
    cx::own_thread(tui.pid, &threads, Some(&clients)).map(str::to_owned)
}

/// A turn begun since the sweep read the daemon: a goal's thread starts its next
/// turn within milliseconds of the last, so a client's `/exit` re-reads the
/// daemon at its last look (`d`) and asks [`cx::daemon_turn`] again, with this
/// look's still-run placements ([`still_record`]). `true`: a turn the `/exit`
/// must wait for is running; retried at the next idle point.
fn turn_since_the_sweep(tui: &Tui, st: &St, d: &DaemonRead, k: &dyn CodexKernel, now: u64) -> bool {
    let threads: Turns = d.threads.iter().map(|(l, _)| l.clone()).collect();
    if !threads.iter().any(cx::Loaded::running) {
        return false;
    }
    let placed: Vec<String> = still_record(st)
        .into_iter()
        .filter(|(_, since)| now.saturating_sub(*since) >= upgrade::QUIET_S)
        .map(|(thread, _)| thread)
        .collect();
    let clients = k.daemon_clients(d.pid);
    cx::daemon_turn(tui.pid, &threads, clients.as_deref(), &placed, false)
        != upgrade::DaemonTurn::None
}

/// The notice to an embedded conversation, only for a relaunch that can be
/// carried out: [`plan`] is asked first.
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
    // What runs under Codex, named BEFORE the closing sentence
    // ([`upgrade::with_running`]): the guarded Enter waits on the text's last
    // word, which must be the sentence's short `later.` — a background
    // command's long path there wraps inside itself, and the Enter never
    // went (the review of 2026-09-30). Held to the longest paste Codex draws
    // as text ([`cx::PASTE_SHOWN_MAX`]): past it, the placeholder Codex draws
    // instead is left standing as the person's draft.
    let text = upgrade::with_running_within(
        &cx::prepare_prompt(&tui.version, &target.version, &marker),
        &k.held(tui.pid),
        cx::PASTE_SHOWN_MAX,
    );
    if tui
        .claim
        .as_ref()
        .is_some_and(|claim| !claim_still_live(c, tui.pid, claim))
    {
        return said(r, "wait:tab-ownership-changed");
    }
    match type_under(c, &tui.tab, &text, None) {
        Ok(()) => {
            st.spend_ask(asks, &marker);
            st.marker = marker;
            st.phase = Phase::Announced { at_s: now, asks };
            st.noticed_at = now;
            st.last_stop.clear();
            st.thread.clone_from(thread);
            st.mode = "embedded".to_string();
            let r = said(r, format!("announced:{asks}"));
            ledger(opts, &r, &st.marker);
            r
        }
        // Typed and not submitted: said and recorded, never taken back (the
        // safety ruling of 2026-09-30) — and never read by the next look as
        // the person's draft (review of 2026-09-26: ~600 characters of
        // notice sat in the composer, the miss said nowhere, and every later
        // visit waited `draft`).
        Err(Under::Typed(Typed::Refused(why))) if why == LEFT_TYPED => {
            left_typed(opts, r, st, c, &tui.tab, &text, "notice")
        }
        Err(Under::Held) => said(r, "wait:held"),
        Err(Under::Fence(why)) => said(r, format!("wait:{why}")),
        Err(Under::Typed(e)) => said(r, format!("wait:announce-refused:{}", e.word())),
    }
}

/// Plan the relaunch: the shell's dialect, the heal it needs, the resume
/// directory (the TUI's, when the shell's differs), and the line (`<twin>
/// resume <kept flags> <thread>`, or the kept flags alone with `thread` `None`).
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
    let Some(dialect) = shell_dialect(shell, k, table) else {
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

/// The dialect of the shell `shell`, as its executable names it: the kernel's
/// argv, else its name in the process table `read_table` gives (read only
/// then). `None`, unread or a shell no relaunch line is written for (`sh`,
/// tcsh, nushell…), is what [`plan_for`] waits on for ever
/// (`wait:shell-dialect`). The run's [`snapshot`] reads it the same way, from
/// the argv alone (as `relaunch::snapshot` reads a Claude Code's), so the
/// explicit restart is carried only where this plan would write its line
/// ([`super::super::relaunch::carries_restart`]).
fn shell_dialect(
    shell: u32,
    k: &dyn CodexKernel,
    read_table: impl FnOnce() -> Vec<(u32, u32, String)>,
) -> Option<Dialect> {
    k.args(shell)
        .map(|a| a.exec_path)
        .or_else(|| {
            read_table()
                .into_iter()
                .find(|(p, _, _)| *p == shell)
                .map(|(_, _, n)| n)
        })
        .as_deref()
        .map(base_name)
        .as_deref()
        .and_then(Dialect::from_exe_name)
}

/// The exit: plan the relaunch, take the last look, write the state, type
/// `/exit` (fenced on the read that found the composer empty, guarded on its
/// row), then carry on the relaunch. Never a signal. `(mode, daemon)`: the
/// client's mode and its daemon as the pass read it.
#[allow(clippy::too_many_arguments)]
fn exit(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tui: &Tui,
    target: &Target,
    (mode, daemon): (&Mode, Option<&DaemonView>),
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
    // The last look before the irreversible keystroke: nobody's hand, and no
    // person within this rung's grace (`[harness] human_grace_s`, then
    // KEYS_GAP_S from KeysOnly on and under `--now`).
    let now = now_s();
    let rung = if st.request_for(&tui.tab) == upgrade::Request::Now {
        upgrade::Rung::Land
    } else {
        st.rung_at(now)
    };
    let grace = upgrade::person_grace(rung, opts.human_grace_s);
    // The harness's hand on the tab ([`Hand`]), taken before the last look, so
    // from it through `/exit`, bare shell and relaunch line to the new TUI's
    // first idle no other connection's write lands. Given back where `/exit`
    // is not typed; the relaunch renews and returns it.
    let Some(mut hand) = Hand::take(c, &tui.tab) else {
        return said(r, "wait:held");
    };
    match exit_typed(
        opts,
        r,
        st,
        c,
        tui,
        target,
        (mode, daemon),
        shell,
        grace,
        line,
        k,
    ) {
        ControlFlow::Continue(r) => relaunch(opts, r, st, c, k),
        ControlFlow::Break(r) => {
            hand.give_back(c);
            r
        }
    }
}

/// [`exit`] under the harness's hand from its last look: `Continue` once `/exit`
/// is typed, `Break` with the report of what stopped it.
#[allow(clippy::too_many_arguments)]
fn exit_typed(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tui: &Tui,
    target: &Target,
    (mode, daemon): (&Mode, Option<&DaemonView>),
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
    // A daemon client's daemon, read once more: a turn begun since the sweep
    // holds the `/exit` ([`turn_since_the_sweep`]), as does a daemon no longer
    // readable where the pass read its threads (`pid` 0 was a build-only
    // read). The thread is also named from this read below.
    let fresh = if *mode == Mode::Daemon {
        let now = now_s();
        let d = read_daemon(&tui.home, k, now);
        let changed = match &d {
            Some(d) => turn_since_the_sweep(tui, st, d, k, now),
            None => daemon.is_some_and(|view| view.pid != 0),
        };
        if changed {
            return ControlFlow::Break(said(r, "wait:changed"));
        }
        d
    } else {
        None
    };
    if mode.cooperative() && !k.background(tui.pid).is_empty() {
        return ControlFlow::Break(said(r, "wait:background"));
    }
    // An embedded session's background terminals end with it.
    if matches!(mode, Mode::Embedded { .. }) && !k.terminals(tui.pid).is_empty() {
        return ControlFlow::Break(said(r, "wait:background-terminal"));
    }
    // A daemon client's thread comes from what it prints as it leaves, told
    // from the next prompt by integration marks, or where they do not reach
    // aterm, from the kernel ([`kernel_thread`]): the root is recorded before
    // `/exit` and resumed (a hint naming another thread stops the relaunch).
    // Without either `/exit` is not typed, and the wait is a blocker timed
    // from its first gate ([`St::note_blocked`]).
    let mut named = None;
    if *mode == Mode::Daemon && !marks_reach(c, &tui.tab) {
        match fresh.as_ref().and_then(|d| kernel_thread(tui, d, k)) {
            Some(thread) => named = Some(thread),
            None => {
                st.note_blocked(Some("no-shell-integration"), now_s());
                return ControlFlow::Break(said(r, "wait:no-shell-integration"));
            }
        }
    }
    if st.blocked == "no-shell-integration" {
        st.note_blocked(None, now_s());
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
        Mode::Daemon => named.clone().unwrap_or_default(),
    };
    // A daemon client's line is planned again once its exit names the thread;
    // until then none is recorded, since the placeholder plan only vets flags
    // and length, and an orphan pass types `st.line` verbatim.
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
    // A new exit: no TUI of it is up or adopted yet, and its own is not
    // seen gone (the counterpart of `St::signalled`'s reset).
    st.resumed_pid = 0;
    st.relaunched_pid = 0;
    st.exited_at = 0;
    save(opts, &key(&tui.tab), st);
    match type_text(c, &tui.tab, CODEX_EXIT, generation.as_deref()) {
        Ok(()) => {}
        // Typed and not submitted: `/exit` stands in the composer (a person's
        // Enter would end Codex with no relaunch); said and recorded, never
        // taken back, never sent by the supervisor.
        Err(Typed::Refused(why)) if why == LEFT_TYPED => {
            st.phase = before;
            let r = left_typed(opts, r, st, c, &tui.tab, CODEX_EXIT, "exit");
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
        &match &named {
            Some(thread) => format!(
                "/exit ({mode_word}) — no signal; no shell integration marks, its thread {thread} \
                 named by the kernel (its daemon's one conversation, this TUI its one client)"
            ),
            None => format!("/exit ({mode_word}) — no signal"),
        },
    );
    ControlFlow::Continue(r)
}

/// The exit hint the TUI just printed, from its own rows: the shell
/// integration's finished block for its command (read whole with `blocktext`,
/// scrollback included), else the rows directly above the new prompt's first
/// row ([`cx::hint_above_prompt`]; the entering block says how tall the prompt
/// is). Never the whole screen, which may hold an older hint. A daemon update
/// can wipe the tab's blocks, so the finished block is often gone while the
/// entering one is not.
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
    /// The finished block ending where the entering prompt begins: the command
    /// that just ran, not an older survivor.
    done: Option<u64>,
    /// The rows the entering prompt draws above its command row (`cmd -
    /// prompt`: a `%~` / `%#` prompt draws one).
    prompt_rows: usize,
}

/// The shell integration's marks at the newest prompt (`blocks 2 --json`):
/// `None` unless the newest block is an entering prompt.
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

/// Whether shell integration marks reach aterm in `tab` (`status
/// integration=on`, or the host tracks a block there), so a daemon client's
/// exit rows can be told from the next prompt ([`exit_hint`]). Without them
/// `/exit` is not typed.
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

/// [`relaunch`] with the harness's `hand` on the tab ([`Hand`]) through every
/// wait, the typed line, and until the new TUI's first idle ([`await_new`]),
/// as `relaunch::relaunch` does for Claude Code.
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
        // The kernel proved the thread; a hint naming another is a
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
        // A daemon client's thread is its hint's. A hint recorded by an earlier
        // attempt stands; a different hint since is another Codex's exit.
        "daemon" => match (hint, st.thread.as_str()) {
            (Some(h), "") => Some(h.thread),
            (Some(h), recorded) if h.thread == recorded => Some(h.thread),
            (Some(_), _) => {
                return fail(
                    opts,
                    r,
                    st,
                    "hint-mismatch",
                    "the exit at this prompt named another thread than the one recorded (an \
                     earlier attempt's hint, or the kernel's)",
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
    // An embedded thread's lock is released with the TUI; resuming while it is
    // held is refused by Codex (`already has an active writer`).
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
    // Recorded before the line is typed: a sweep that dies after typing it
    // leaves the next one the thread to recognise its relaunch by.
    st.thread = thread.clone().unwrap_or_default();
    st.line.clone_from(&line);
    save(opts, &key(&tab), st);
    // When its `/exit` was typed — known only for a restart that typed it
    // (`relaunch::end_asked_at`): a person's keys before it went to Codex.
    let ended_at_s = end_asked_at(st);
    match type_relaunch_line_with(
        opts,
        c,
        shell,
        &tab,
        &line,
        Some(dialect),
        PromptLook {
            mark: &mut st.prompt,
            ended_at_s,
        },
        || lock_free(k),
        |c, pid, tab| k.in_tab(c, pid, tab),
    ) {
        Ok(()) => st.prompt = None,
        Err(RelaunchLineError::Wait(why)) => {
            // Kept on the record, as Claude Code's relaunch keeps it: the stale
            // bound that ends the move names what held its line ([`expired`]).
            st.note_wait(why, now_s());
            return said(r, format!("wait:{why}"));
        }
        Err(RelaunchLineError::Turn(e)) => {
            st.fail("relaunch-refused", now_s());
            let r = said(r, format!("failed:relaunch:{}", first_word(&e)));
            ledger(opts, &r, &line);
            return r;
        }
    }
    st.relaunched(now_s());
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

/// Whether `pid` (argv `argv`) is the relaunch this upgrade typed: a child of
/// the line's shell, not the exited TUI, whose argv resumes the recorded thread
/// (or nothing, for a plain relaunch).
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

/// After the relaunch line: find the new TUI ([`relaunched_by_us`]) and carry on
/// with it, or where a supervisor loop takes it ([`Opts::hand_back`]) hand it
/// back ([`adopt`]), as [`super::super::relaunch::await_new`] does, the hand on
/// the tab until its first idle. A later step finding the relaunch in flight
/// comes here too.
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
        Some(pid) if opts.hand_back => adopt(opts, r, st, c, pid, k, Some(hand)),
        Some(pid) => carry_on(opts, r, st, c, pid, k, Some(hand)),
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

/// THE RELAUNCHED TUI `new` FOUND ([`relaunched_by_us`]) — by the wait after
/// the relaunch line, or by a visit that finds it in the tab — and handed to
/// [`adopt`] or [`carry_on`]: on the record ([`St::relaunched_pid`]) whatever
/// becomes of its adoption or its carry-on, so that once it is gone a Codex
/// leaving the tab is that TUI, its exit its own (`relaunch::restarted`).
///
/// A RELAUNCH THAT KEPT THE TUI ON ITS OWN PROGRAM ([`SAME_PROGRAM`]: a
/// crash's, while the owner's hold or the switch kept it off the managed
/// build) hands its record to `new` as well — its identity, pid and kernel
/// start, the one a visit keeps a record by ([`own_prior`],
/// [`owner_word_on`]; round eight, #24). That record carries what the
/// relaunch kept of the upgrade's ([`St::kept_by_restart`]: the owner's hold,
/// how long the TUI has been behind), and the relaunched TUI is still behind:
/// with the record still naming the TUI that crashed, its first visit read
/// it as another TUI's and minted a round with no word and its move clock at
/// zero — under the quiet flow, a move onto the very build the owner had
/// skipped, one launch late. A relaunch onto the managed build leaves the
/// record as it was: the TUI it brings up is on that build, and a newer one
/// begins a new wait.
fn found(st: &mut St, new: u32) {
    st.relaunched_pid = new;
    if st.source == SAME_PROGRAM {
        st.notice_pid = new;
        st.notice_start = kernel_start(new).map(|s| squash(&s)).unwrap_or_default();
    }
}

/// How long a relaunched TUI may take to lead its tab's foreground group
/// before it is handed back ([`adopt`]).
const ADOPT_WAIT: Duration = Duration::from_secs(30);

/// Hand the relaunched Codex back to its supervisor (`adopted`): the new TUI
/// leads the tab by the host's own roster (so the gap is not read as the agent
/// leaving) and the record stays in flight. The loop answers whatever it opened
/// with, and at its next idle point the relaunch primitive's continuation
/// ([`super::super::relaunch::resume`]) types the carry-on ([`carry_on`]). The
/// adopted TUI is recorded ([`St::resumed_pid`]) so a later exit is its own,
/// never the restart's (`relaunch::restarted`); one whose adoption timed out
/// stays on [`St::relaunched_pid`].
fn adopt(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn CodexKernel,
    hand: Option<&mut Hand>,
) -> Report {
    found(st, new);
    let tab = st.tab.clone();
    if !wait_until(ADOPT_WAIT, || k.in_tab(c, new, &tab)) {
        return said(r, "wait:resume");
    }
    st.resumed_pid = new;
    r.pid = new;
    // A goal the move paused: the relaunched Codex opens on its box, which
    // the upgrade answers with the resume before the loop reads the tab.
    let _ = resume_on_relaunch(opts, st, c, new, hand);
    let r = said(r, "adopted");
    ledger(
        opts,
        &r,
        "the relaunched Codex leads the tab; its supervisor types the continuation at its \
         first idle point",
    );
    r
}

/// A Codex restart in flight (old TUI exiting, or relaunched and not carried on)
/// carried on from where it stopped: the relaunch primitive's continuation step
/// for a Codex record ([`super::super::relaunch::resume`]), and every other
/// caller's (`opts.hand_back` off: carried on in the same step).
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
        let r = fail(opts, r, &mut st, why, &detail);
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

/// The relaunched TUI `new` is up. A daemon (or plain) client is done once its
/// composer is drawn; an embedded conversation once the new process holds the
/// thread's lock and the continuation is typed at an idle empty composer with
/// no hand on the tab (a person keeps it owed until the next idle point).
///
/// Deliberately not gated on the usage wall ([`upgrade::limited`]): a resumed
/// Codex replays its last block, so one returning onto a wall shows it and this
/// line fails there once per restart (the record is done after it). The window
/// types the carry-on only at an idle point, which a wall is not
/// ([`aterm_phase::Phase::Limited`]). The replayed wall never lifts by itself,
/// so a gate would wait for a person or stop the record past [`STALE_S`].
fn carry_on(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn CodexKernel,
    mut hand: Option<&mut Hand>,
) -> Report {
    found(st, new);
    r.pid = new;
    let tab = st.tab.clone();
    // A goal the move paused: resumed first (its box answered, or `/goal
    // resume` typed) — and then IT is the carry-on: the goal's own next turn
    // goes on where it was, so no continuation is typed over it.
    let _ = resume_on_relaunch(opts, st, c, new, hand.as_deref_mut());
    let goal_carries = goal_path(opts, &tab)
        .and_then(|p| goal_hold::read(&p))
        .is_some_and(|h| {
            h.owner == Owner::Upgrade && h.to == st.to && (h.owes() || h.why == "moved")
        });
    // The hand the caller holds is renewed through every wait below (60 s,
    // 30 s, 120 s against a 60 s lease).
    let up = wait_until(Duration::from_secs(60), || {
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        screen(c, &tab).is_some_and(|s| aterm_phase::codex::composer(&s.rows).is_some())
    });
    let to = Version::parse(&st.to);
    let from = Version::parse(&st.from);
    let mut typed = false;
    if st.mode == "embedded" && up && !goal_carries && !st.quiet {
        let lock = PathBuf::from(&st.codex_home)
            .join("thread-writer-locks")
            .join(format!("{}.lock", st.thread));
        let holds = wait_until(Duration::from_secs(30), || {
            if let Some(h) = hand.as_deref_mut() {
                h.keep(c);
            }
            k.holders_of(&lock).is_some_and(|h| h.contains(&new))
        });
        let settled = holds
            && wait_until(Duration::from_secs(120), || {
                if let Some(h) = hand.as_deref_mut() {
                    h.keep(c);
                }
                typing_fence(c, &tab).is_ok()
            });
        // A person at the keyboard wins here too (the Claude restart's
        // carry-on asks the same): held, it stays owed.
        if settled && held(c, &tab, opts.human_grace_s) {
            return said(r, "wait:held");
        }
        if settled
            && k.in_tab(c, new, &tab)
            && let (Some(f), Some(t)) = (&from, &to)
        {
            // A relaunch on exit says why it was relaunched; the upgrade's
            // restart says what it moved onto.
            let text = if super::super::relaunch::relaunch_cause(&st.cause) {
                super::super::relaunch::resumed_prompt_as("Codex", &st.to, &st.cause)
            } else {
                cx::continue_prompt(f, t)
            };
            // UNDER THE HARNESS'S HAND (the caller's, else one for these
            // tries): the tries are one hold, and a tab another driver
            // holds is a wait, the carry-on still owed.
            let tries = under_hand(c, &tab, hand, |c| {
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
                        Err(Typed::Cut(_) | Typed::Refused(_)) => break,
                    }
                }
            });
            if tries.is_none() {
                return said(r, "wait:held");
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
    } else if goal_carries {
        "done:goal"
    } else if st.quiet {
        // Quiet: resumed while idle, nothing typed.
        "done:resumed"
    } else if st.mode == "embedded" {
        "done:no-continue"
    } else {
        "done"
    };
    let r = said(r, step);
    ledger(opts, &r, &st.outcome);
    r
}

// ------------------------------------------------------ relaunch on exit

/// A Codex TUI's run, read while it runs, for relaunch on exit
/// ([`super::super::relaunch::snapshot`] vetted job, tab and terminal): store
/// build and version, `$CODEX_HOME`, whether it holds its thread's lock, the
/// thread (the lock's, else its launch `resume <id>`,
/// [`cx::resumed_thread`], else the one its daemon began for it,
/// [`daemon_thread`]), its directory and start time.
pub(super) fn snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
    k: &dyn CodexKernel,
) -> Result<super::super::relaunch::Snapshot, &'static str> {
    if !cx::is_tui_argv(&args.argv) {
        return Err("not-a-tui");
    }
    let exe = k.exe(pid).ok_or("exe-unreadable")?;
    let home = home_of(args, &opts.home);
    let files = k.open_files(pid).ok_or("files-unreadable")?;
    let locks = home.join("thread-writer-locks");
    let held: Vec<String> = files
        .iter()
        .filter(|f| f.parent() == Some(locks.as_path()))
        .filter_map(|f| cx::lock_thread(&f.file_name()?.to_string_lossy()).map(str::to_owned))
        .collect();
    let cwd = k.cwd(pid).unwrap_or_default();
    let started_ms = super::started_ms(pid).unwrap_or(0);
    // A daemon client holds nothing: its resumed thread, else the one its
    // daemon began for it, read while it runs (an updated daemon drops dead
    // clients' threads). An ambiguous read says so; no earlier name is kept.
    let mut ambiguous = false;
    let (embedded, thread) = match held.as_slice() {
        [thread] => (true, Some(thread.clone())),
        [] => (
            false,
            cx::resumed_thread(&args.argv).or_else(|| {
                match daemon_thread(&home, &cwd, started_ms, pid, k) {
                    Ok(thread) => thread,
                    Err(why) => {
                        ambiguous = why == "thread-ambiguous";
                        None
                    }
                }
            }),
        ),
        _ => return Err("threads-ambiguous"),
    };
    Ok(super::super::relaunch::Snapshot {
        tab: tab.to_string(),
        pid,
        start,
        shell,
        version: cx::package_version(&exe).map(|v| v.to_string()),
        program: exe,
        argv: args.argv.clone(),
        session: thread,
        cwd,
        codex: Some(super::super::relaunch::CodexRun {
            home,
            embedded,
            started_ms,
            ambiguous,
            block: connect(opts, tab)
                .ok()
                .and_then(|mut c| running_block(&mut c, tab)),
        }),
        // As the lane's plan reads it ([`shell_dialect`]), from the shell's
        // kernel argv alone: a shell it writes no line for waits for ever at
        // exit, so the explicit restart is not carried there
        // ([`super::super::relaunch::carries_restart`]). The plan reads it
        // again at exit; the frozen remedy still promises no Codex its
        // conversation ([`super::super::relaunch::resumes_on_exit`]).
        dialect: shell_dialect(shell, k, Vec::new),
        // Claude Code's variables: a Codex has no such login or home.
        login: None,
        home: None,
        claude_dir: None,
    })
}

/// One look at a Codex that left its tab ([`super::super::relaunch::exit_record`]
/// repeats it): whether the process still runs and, once gone, its shell's exit
/// status for the command that ran IT ([`exit_status`] of the block its
/// snapshot names, [`cx::crashed_status`]). Nothing else is a witness: an
/// embedded TUI's thread lock is left behind by every signal as by SIGKILL.
/// With no status (no integration), the exit is left.
pub(super) fn look_at_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    k: &dyn CodexKernel,
) -> super::super::relaunch::ExitLook {
    let running =
        k.alive(snap.pid) && super::kernel_start(snap.pid).as_deref() == Some(snap.start.as_str());
    let block = snap.codex.as_ref().and_then(|run| run.block);
    let crashed = if running {
        None
    } else {
        connect(opts, &snap.tab)
            .ok()
            .and_then(|mut c| exit_status(&mut c, &snap.tab, block))
            .map(cx::crashed_status)
    };
    super::super::relaunch::ExitLook {
        running,
        record: None,
        codex: true,
        crashed,
    }
}

/// The exit status of the command that just ran in `tab`, per shell
/// integration: the finished block ending where the entering prompt begins
/// (`blocks 2 --json`) — block `block` where the TUI's snapshot named the one
/// it ran in ([`super::super::relaunch::CodexRun::block`]), so another
/// command run since is never read as its exit. `None` while there is none.
fn exit_status(c: &mut Client, tab: &str, block: Option<u64>) -> Option<i64> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 2 --json")).ok()?;
    parse_exit_status(&head, &body, block)
}

/// The block a TUI runs in, read while it runs: the tab's last block,
/// executing ([`super::super::relaunch::CodexRun::block`]).
fn running_block(c: &mut Client, tab: &str) -> Option<u64> {
    use aterm_json::Value;
    let (head, body) = c.request_counted(&format!("@{tab} blocks 1 --json")).ok()?;
    let blocks = blocks_of(&head, &body)?;
    let last = blocks.last()?;
    (last.get("state").and_then(Value::as_str) == Some("executing"))
        .then(|| last.get("id").and_then(Value::as_u64))
        .flatten()
}

/// The blocks of one `blocks --json` reply.
fn blocks_of(head: &str, body: &str) -> Option<Vec<aterm_json::Value>> {
    if !head.starts_with("OK") {
        return None;
    }
    let json = if body.trim().is_empty() {
        head.strip_prefix("OK ")?.to_string()
    } else {
        body.to_string()
    };
    let v: aterm_json::Value = aterm_json::from_str(json.trim()).ok()?;
    Some(v.get("blocks")?.as_array()?.clone())
}

/// [`exit_status`] over one `blocks --json` reply.
fn parse_exit_status(head: &str, body: &str, block: Option<u64>) -> Option<i64> {
    use aterm_json::Value;
    let blocks = blocks_of(head, body)?;
    let (last, rest) = blocks.split_last()?;
    if last.get("state").and_then(Value::as_str) != Some("entering") {
        return None;
    }
    let prompt = last.get("prompt").and_then(Value::as_u64)?;
    let done = rest.last()?;
    (done.get("state").and_then(Value::as_str) == Some("complete")
        && done.get("end").and_then(Value::as_u64) == Some(prompt)
        && block.is_none_or(|block| done.get("id").and_then(Value::as_u64) == Some(block)))
    .then(|| done.get("exit").and_then(Value::as_i64))
    .flatten()
}

/// A daemon client's thread when it named none: the one thread its daemon holds
/// that was begun after client `me` started (UUIDv7 id,
/// [`cx::thread_created_ms`], with a second's slack) in its directory
/// ([`cx::rollout_cwd`]). `Ok(None)` when none (nothing to resume); `Err` when
/// the daemon or its clients are unreadable, or `thread-ambiguous` when several
/// fit or another live client runs in the same directory, never a guess. The
/// host keeps no name over a later ambiguous read
/// ([`super::super::relaunch::CodexRun::ambiguous`]).
fn daemon_thread(
    home: &Path,
    cwd: &str,
    started_ms: u64,
    me: u32,
    k: &dyn CodexKernel,
) -> Result<Option<String>, &'static str> {
    let daemon = read_daemon(home, k, now_s()).ok_or("no-daemon")?;
    let here = canonical(Path::new(cwd));
    let clients = k.daemon_clients(daemon.pid).ok_or("clients-unreadable")?;
    // A client whose directory cannot be read may be in this one.
    if clients
        .iter()
        .any(|&c| c != me && k.cwd(c).is_none_or(|d| canonical(Path::new(&d)) == here))
    {
        return Err("thread-ambiguous");
    }
    let mut found: Vec<String> = daemon
        .threads
        .iter()
        .map(|(loaded, _)| &loaded.thread)
        .filter(|t| cx::thread_created_ms(t).is_some_and(|ms| ms + 1000 >= started_ms))
        .filter(|t| {
            find_rollout(home, t)
                .and_then(|r| first_line(&r, META_BYTES))
                .and_then(|l| cx::rollout_cwd(&l))
                .is_some_and(|c| canonical(Path::new(&c)) == here)
        })
        .cloned()
        .collect();
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => Err("thread-ambiguous"),
    }
}

/// Relaunch on exit, Codex lane ([`super::super::relaunch::after_exit`] hands a
/// Codex snapshot here): TUI `snap` left its tab and nobody owns the exit. Its
/// shell's word (`left`, or — none read as the exit was seen — the word of the
/// block it ran in, read now) decides: its own exit or someone's signal is
/// theirs (`ended:graceful-exit`), no word is left (`ended:exit-unwitnessed`), unless
/// the explicit restart ended it ([`ExitCause::Stall`], [`ExitCause::Asked`]:
/// its stamp, consumed as the relaunch is filed) or aterm itself ended
/// ([`ExitCause::HostEnded`], `snap` naming the new tab and shell). A crash is
/// relaunched on its thread (held, or its daemon's, [`daemon_thread`]; plain if
/// none has a rollout) via [`relaunch`], on the managed build while `upgrade`
/// allows, else the build it ran, and handed back (`adopted`) for its
/// supervisor to carry on ([`carry_on`]).
pub(super) fn after_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    upgrade: bool,
    cause: ExitCause,
    k: &dyn CodexKernel,
) -> Report {
    use super::super::relaunch::{
        CAUSE_ASKED, CAUSE_EXIT, CAUSE_FRESH, CAUSE_HOST, CAUSE_STALL, ExitRecord,
    };
    let tab = snap.tab.clone();
    let session = key(&tab);
    let from = snap.version.clone().unwrap_or_else(|| "-".to_string());
    let mut r = Report {
        pid: snap.pid,
        tab: tab.clone(),
        session: session.clone(),
        from: from.clone(),
        to: "-".to_string(),
        step: String::new(),
    };
    let Some(run) = snap.codex.as_ref() else {
        return said(r, "refused:not-codex");
    };
    let _held = match super::sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    if k.alive(snap.pid) && super::kernel_start(snap.pid).as_deref() == Some(snap.start.as_str()) {
        return said(r, "wait:not-exited");
    }
    // A restart already in flight for this tab is carried on, never typed
    // twice; one of another process is closed, and so is one whose relaunch
    // brought up a TUI gone already ([`St::relaunched_gone`], the Claude
    // lane's rule too). A record with no relaunch typed yet was overtaken by
    // a process a person started by hand, never aterm's. One of THIS exit,
    // closed, says what an earlier attempt of it read
    // ([`St::relaunched_on_exit_of`]).
    let mut relaunched_before = false;
    if let Some(mut st) = load(opts, &session).filter(St::in_flight) {
        let overtaken = (st.pid != snap.pid || st.relaunched_gone(|pid| k.alive(pid))).then(|| {
            match st.phase {
                Phase::Exiting { .. } => (
                    "resumed-elsewhere",
                    "the conversation was resumed in this tab outside aterm's relaunch, and that \
                     process has exited"
                        .to_owned(),
                ),
                _ => (
                    "exited-before-continuing",
                    "the relaunched Codex exited before its continuation was typed".to_owned(),
                ),
            }
        });
        let overtook = overtaken.is_some();
        let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s(), k)) else {
            return carry_in_flight_with(opts, r, st, &session, k);
        };
        relaunched_before = st.relaunched_on_exit_of(snap.pid);
        if !opts.dry_run {
            if overtook {
                st.close_overtaken(why, now_s());
            } else {
                st.fail(why, now_s());
            }
            ledger(opts, &said(r.clone(), format!("failed:{why}")), &detail);
            save(opts, &session, &st);
        }
    }
    // AN EXIT NOTHING COULD READ AS IT WAS SEEN is read now, as the Claude
    // lane reads its record (`relaunch::after_exit_as`; round three of the
    // joint audit of 2026-10-01): the shell's word comes through this
    // process's parser, which a seamless update's park stops for its whole
    // hold, and a prompt can come back later than the look waits
    // ([`super::super::relaunch::EXIT_GONE`]) — left unread, a crash was
    // never relaunched, and nothing said so. Only the word of the block the
    // TUI ran in ([`look_at_exit`], [`super::super::relaunch::CodexRun::block`]):
    // a command run at the prompt since is never read as its exit. Once an
    // earlier attempt read that word and relaunched, it stands: the
    // relaunched TUI's own block has taken its place as the newest, and read
    // again it said none — the crash dropped, `exit-unwitnessed`, when the
    // TUI that attempt brought up was gone again before anything read it. A
    // RESIDUAL, stated: the window owes a handoff only a crash read at the
    // exit ([`super::super::relaunch::ExitRecord::crashed`]), so a park
    // whose Commit goes ahead before this attempt carries this one nowhere.
    let reread;
    let left = if matches!(left, ExitRecord::Unread) && cause == ExitCause::Exit {
        reread = if relaunched_before {
            ExitRecord::Crashed
        } else if run.block.is_some() {
            match look_at_exit(opts, snap, k).crashed {
                Some(true) => ExitRecord::Crashed,
                Some(false) => ExitRecord::Removed,
                None => ExitRecord::Unread,
            }
        } else {
            ExitRecord::Unread
        };
        &reread
    } else {
        left
    };
    if !left.crashed() && cause == ExitCause::Exit {
        return said(
            r,
            if matches!(left, ExitRecord::Unread) {
                "ended:exit-unwitnessed"
            } else {
                "ended:graceful-exit"
            },
        );
    }
    if !k.alive(snap.shell) {
        return said(r, "refused:shell-gone");
    }
    let thread = match (&snap.session, run.embedded) {
        (Some(t), _) => Some(t.clone()),
        (None, true) => None,
        (None, false) => match daemon_thread(&run.home, &snap.cwd, run.started_ms, snap.pid, k) {
            Ok(t) => t,
            Err(why) => return said(r, format!("refused:{why}")),
        },
    };
    // A thread with no rollout has no message yet: the TUI comes back plain.
    let conversation = thread.filter(|t| find_rollout(&run.home, t).is_some());
    let running = snap.version.as_deref().and_then(Version::parse);
    // The tab's upgrade record: the owner's hold on the move keeps a crash
    // off the build it names, as the Claude lane's relaunch does
    // (`relaunch::relaunch_target`, round eight, #24) — only while that
    // record is about THE TUI THAT CRASHED (its pid and kernel start, the
    // rule every read of a Codex record keeps: [`own_prior`],
    // [`owner_word_on`]). A word given to a Codex the person has since quit
    // is gone with it; read here, a TUI started after it that crashed before
    // its first visit was relaunched on another TUI's word and clock, and
    // [`found`] stamped them onto the TUI it brought up (#24's review).
    let start = squash(&snap.start);
    let prior =
        load(opts, &session).filter(|st| st.notice_pid == snap.pid && st.notice_start == start);
    let hold = prior.as_ref().and_then(|st| st.owners_hold(&tab));
    let managed = relaunch_onto(
        Target::read(),
        upgrade,
        running.as_ref(),
        hold.as_ref(),
        now_s(),
    );
    let (exe, to, source) = match managed {
        Some(t) => (t.twin, t.version.to_string(), "managed".to_string()),
        None => (snap.program.clone(), from.clone(), SAME_PROGRAM.to_string()),
    };
    r.to = format!("{to}({source})");
    if opts.dry_run {
        return said(
            r,
            if conversation.is_some() {
                "would-relaunch"
            } else {
                "would-relaunch:fresh"
            },
        );
    }
    let Ok(mut c) = connect(opts, &tab) else {
        return said(r, "wait:no-socket");
    };
    if !roster(&mut c).contains(&tab) {
        return said(r, "wait:tab-not-live");
    }
    let now = now_s();
    let mut st = St {
        agent: Agent::Codex,
        mode: match (&conversation, run.embedded) {
            (None, _) => "fresh",
            (Some(_), true) => "embedded",
            (Some(_), false) => "daemon",
        }
        .to_string(),
        thread: conversation.clone().unwrap_or_default(),
        codex_home: run.home.to_string_lossy().into_owned(),
        argv: snap.argv.clone(),
        cwd: snap.cwd.clone(),
        twin: exe.to_string_lossy().into_owned(),
        pid: snap.pid,
        shell: snap.shell,
        tab: tab.clone(),
        from,
        to,
        source,
        cause: match (&conversation, cause) {
            (None, _) => CAUSE_FRESH,
            (Some(_), ExitCause::Stall) => CAUSE_STALL,
            (Some(_), ExitCause::Asked) => CAUSE_ASKED,
            (Some(_), ExitCause::HostEnded) => CAUSE_HOST,
            (Some(_), ExitCause::Exit) => CAUSE_EXIT,
        }
        .to_string(),
        salt: now,
        exited_at: now,
        phase: Phase::Exiting { at_s: now },
        ..St::default()
    };
    // Kept on its own program, the owner's hold and how long it has been
    // behind still stand ([`St::kept_by_restart`]).
    if st.source == SAME_PROGRAM
        && let Some(prior) = prior.as_ref()
    {
        prior.kept_by_restart(&tab, &mut st);
    }
    save(opts, &session, &st);
    super::super::relaunch::consume_stamp(opts, snap, cause);
    ledger(
        opts,
        &said(r.clone(), "exited"),
        &format!(
            "the Codex {} left its tab ({}); relaunched {}",
            st.mode,
            if cause == ExitCause::HostEnded {
                "aterm ended while it ran"
            } else {
                left.word()
            },
            if st.thread.is_empty() {
                "plain"
            } else {
                "on its thread"
            }
        ),
    );
    let r = relaunch(opts, r, &mut st, &mut c, k);
    save(opts, &session, &st);
    r
}

/// The managed Codex a crash's relaunch moves onto ([`after_exit`]), if any:
/// `managed` while `upgrade` (`[harness] upgrade`, live) allows and it is no
/// older than the build that ran (`running`), and while the owner's hold on
/// the tab (`hold`, [`St::owners_hold`]) does not name it at `now` — a skip
/// of it, a `Not today` standing (round eight, #24; the Claude lane's
/// `relaunch::relaunch_target` keeps the same word). `None`: relaunched on
/// the program it ran.
fn relaunch_onto(
    managed: Option<Target>,
    upgrade: bool,
    running: Option<&Version>,
    hold: Option<&upgrade::Request>,
    now: u64,
) -> Option<Target> {
    managed
        .filter(|t| upgrade && running.is_none_or(|v| t.version >= *v))
        .filter(|t| !hold.is_some_and(|word| word.holds_build(&t.version.to_string(), now)))
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
            let r = fail(opts, r, &mut st, why, &detail);
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

/// [`super::expired`] for a Codex restart: typed `/exit` over [`STALE_S`]
/// ago and not relaunched, its shell gone, or relaunched and never come up.
/// Its `stale-exit` line is Claude Code's, wait words and all
/// (`relaunch::stale_exit_line`).
fn expired(st: &St, now: u64, k: &dyn CodexKernel) -> Option<(&'static str, String)> {
    match st.phase {
        Phase::Exiting { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "stale-exit",
            super::super::relaunch::stale_exit_line(st, "Codex", now),
        )),
        Phase::Exiting { .. } if st.shell == 0 || !k.alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran Codex is gone: there is no prompt to relaunch at".to_owned(),
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "no-resume",
            "the relaunched Codex never came up in the tab".to_owned(),
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------- the prune

/// What the Codex prune ([`prune`]) knows of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabLife {
    /// Proven live: this pass saw it live, or the rendezvous registry names it.
    Live,
    /// Proven gone: no entry names it in a registry this pass can vouch for.
    Gone,
    /// Neither is proven: the record is kept.
    Unknown,
}

/// The rendezvous registry's word on a tab, for [`prune`]. Every live aterm
/// session publishes `graph/<sid>` in the default control directory
/// (`aterm_uds::control_socket_dir`) and retires it on close; tab ids are never
/// reused. An entry reads live, a missing one gone, but only in a registry this
/// pass can vouch for: it must hold an entry for every tab in the non-empty
/// `known_live` (host roster, running Codex TUIs). Otherwise everything is
/// unknown. A tab in `known_live` is live regardless.
pub(super) fn registry_life<'a>(
    graph: Option<&'a Path>,
    known_live: &'a [String],
) -> impl Fn(&str) -> TabLife + 'a {
    /// `Some(true)` an entry for `sid` exists in `graph`, `Some(false)` none,
    /// `None` unreadable or `sid` not a tab id (never joined onto a path).
    fn entry(graph: &Path, sid: &str) -> Option<bool> {
        let hex = sid.strip_prefix("s-")?;
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        match std::fs::symlink_metadata(graph.join(sid)) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    }
    let vouched = graph.filter(|graph| {
        graph.is_dir()
            && !known_live.is_empty()
            && known_live.iter().all(|sid| entry(graph, sid) == Some(true))
    });
    move |tab: &str| {
        if known_live.iter().any(|sid| sid == tab) {
            return TabLife::Live;
        }
        match vouched.and_then(|graph| entry(graph, tab)) {
            Some(true) => TabLife::Live,
            Some(false) => TabLife::Gone,
            None => TabLife::Unknown,
        }
    }
}

/// Whether this pass's registry may speak for the records under its state root
/// ([`registry_life`]): both default, or both private. A private registry over
/// the default state root names none of the user's tabs and would read them all
/// gone, deleting settled records. Mismatched, no registry is read and nothing
/// goes.
pub(super) fn registry_vouches_for_state(private_registry: bool, private_state: bool) -> bool {
    private_registry == private_state
}

/// Remove Codex records nothing will visit again. Records are per tab and a
/// visit forgets one only when a current Codex leads that tab, so closed tabs'
/// records would stay forever. A record goes once its tab is proven
/// [`TabLife::Gone`] (`life`), unwritten for [`super::STATE_KEEP_S`], and its
/// upgrade [`super::settled`]. Never a live or unproven tab's, one in flight,
/// one an announcement or the user's word keeps, or one not matching its name;
/// nothing in a dry run or a background break. Called under the sweep lock.
///
/// Tier-1 bound to `HarnessUpgradeStateCollection`
/// (`aterm_spec::derive::harness_upgrade_state_collection_model`) in this
/// module's tests.
pub(super) fn prune(opts: &Opts, now: u64, life: impl Fn(&str) -> TabLife) {
    if opts.dry_run || opts.background {
        return;
    }
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let Some(tab) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|stem| stem.strip_prefix("codex-"))
        else {
            continue;
        };
        if !super::sat_a_week(&entry, now) {
            continue;
        }
        let Some(st) = load(opts, &key(tab)) else {
            continue;
        };
        let ours = st.agent == Agent::Codex && (st.tab.is_empty() || st.tab == tab);
        if ours && !st.in_flight() && super::settled(&st) && life(tab) == TabLife::Gone {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ---------------------------------------------------------------- the owner's view

/// The live Codex holders of the `codex-<tab>` upgrades in `sessions`, for the
/// status view ([`super::upgrade_status::Holder`]): the TUI leading the tab and
/// its build. The host proves the tab by roster; a hand-run view by the TUI's
/// environment.
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
            ask: false,
            several_logins: false,
            instances: crate::harness::upgrade_drive::Instances::Machine,
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
            // Codex keeps no status a row could read as its wait.
            ..Default::default()
        })
        .collect()
}

// The upgrade tests drive real processes through Unix APIs (process groups, modes, inodes).
#[cfg(all(test, unix))]
#[path = "upgrade_codex_drive_tests.rs"]
mod tests;

// The ladder's Tier-1 bind and a replayed stall.
#[cfg(all(test, unix))]
#[path = "upgrade_ladder_tests.rs"]
mod ladder_tests;
