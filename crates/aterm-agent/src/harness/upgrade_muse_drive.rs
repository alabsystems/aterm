// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Muse Code crash-relaunch and [`live`] upgrade I/O driver ([`super::super::upgrade_muse`],
//! [`super::super::upgrade_muse_live`]).
//!
//! Drives [`snapshot`] ([`open_files`], [`witness`], [`follow`]), [`look_at_exit`]/[`after_exit`],
//! [`relaunch`] ([`relaunched_by_us`], [`adopt`], [`line_word`], [`line_ended`],
//! [`after_exit_onto`]), [`carry_on`], and per-tab records ([`upgrade::Agent::record_key`],
//! [`prune`], [`holders`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::relaunch::{
    ExitCause, NO_SHELL, NO_SHELL_DETAIL, WAIT_HAND_LIFTS, exit_clock, first_idle, hand_waits_out,
};
use super::super::upgrade_muse::{self as mz, ExitKind};
use super::upgrade::{Agent, Dialect, Phase};
use super::{
    Client, Hand, Kernel, Live, LiveTab, NoPlan, Opts, Plan, PromptLook, RelaunchLineError, Report,
    STALE_S, St, TURN_WAIT, TabLife, Typed, alive, connect, cwd_of, descendants_in, exe_of,
    exit_status, file_holders, held, ids, is_generation, ledger, line_status, load,
    managed_agent_twin, now_s, open_files, pause, registry_graph, registry_life, roster,
    running_block, said, save, screen, shell_dialect_of, squash, started_ms, state_dir,
    store_version, table, turn_takes_gen, turn_verdict, twin_version, type_relaunch_line_with,
    under_hand, upgrade, wait_now, wait_until,
};

/// The process facts the Muse Code lane rests on, each read when asked.
pub(super) trait MuseKernel: Kernel {
    /// Whether `pid` lives.
    fn alive(&self, pid: u32) -> bool;
    /// `pid`'s argv and initial environment (`KERN_PROCARGS2`).
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs>;
    /// The executable the kernel mapped for `pid`.
    fn exe(&self, pid: u32) -> Option<PathBuf>;
    /// Every file `pid` holds open through a descriptor, `None` when unreadable.
    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>>;
    /// Every process holding `file` open, `None` when unreadable.
    fn holders_of(&self, file: &Path) -> Option<Vec<u32>>;
    /// Whether `shell`'s group holds its terminal again (`pgid == tpgid`).
    fn shell_has_terminal(&self, shell: u32) -> bool;
    /// The live `muse` processes whose parent is `shell`, with their argv.
    fn muse_children(&self, shell: u32) -> Vec<(u32, Vec<String>)>;
    /// `pid`'s working directory.
    fn cwd(&self, pid: u32) -> Option<String>;
    /// Whether `pid` is still the foreground group of `tab` by the host's own
    /// roster: asked right before every typed act.
    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool;
    /// Every descendant of `pid` (`(pid, basename)`), `None` where the
    /// process table cannot be read: the live upgrade's process rule (G-M3,
    /// an idle Muse Code has no child at all).
    fn descendants(&self, pid: u32) -> Option<Vec<(u32, String)>> {
        descendants_in(&table(), pid)
    }
    /// When `pid` started, unix milliseconds (the kernel's): where a
    /// session's log begins to be this process's.
    fn started_ms(&self, pid: u32) -> Option<u64> {
        started_ms(pid)
    }
}

impl MuseKernel for Live {
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
        open_files(pid)
    }

    fn holders_of(&self, file: &Path) -> Option<Vec<u32>> {
        file_holders(file)
    }

    fn shell_has_terminal(&self, shell: u32) -> bool {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    }

    fn muse_children(&self, shell: u32) -> Vec<(u32, Vec<String>)> {
        table()
            .into_iter()
            .filter(|(_, ppid, name)| *ppid == shell && name == mz::PROGRAM)
            .filter_map(|(pid, _, _)| Some((pid, atpkg::caller_shell::process_args(pid)?.argv)))
            .collect()
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        cwd_of(pid)
    }

    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool {
        super::process_in_tab(c, pid, tab, None, None)
    }
}

/// The Muse Code lane's per-tab record prefix ([`Agent::record_prefix`]),
/// `muse-`: placed at compile time, which fails where Muse Code keys none.
const PREFIX: &str = match Agent::Muse.record_prefix() {
    Some(prefix) => prefix,
    None => panic!("the Muse Code lane keys its records by tab"),
};

/// The state key of the Muse Code relaunch in `tab` (`muse-<tab>`, [`Agent::record_key`]).
pub(super) fn key(tab: &str) -> String {
    format!("{PREFIX}{tab}")
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// THE MANAGED MUSE CODE A RELAUNCH TYPES: atpkg's twin
/// (`<prefix>/agents/muse`, whose `exec '<store build>' "$@"` names a
/// `muse`), never the store build the crashed process ran (critic X3: typed,
/// it pins that build and skips whatever the twin exports).
fn relaunch_program(program: &Path, twin: Option<PathBuf>) -> Result<PathBuf, &'static str> {
    if !mz::store_build(program) {
        return Ok(program.to_path_buf());
    }
    twin.ok_or("no-managed-muse")
}

/// THE SESSION `pid` HOLDS, by the kernel and by Muse Code's own lock: among
/// the files `pid` holds open (`files`, `lsof`'s canonical paths), the
/// `.session.lock`s at session depth under `root`
/// ([`mz::session_of_lock`]: never a subagent's), each read for the pid its
/// body names ([`mz::lock_pid`]); exactly one naming `pid` is its session,
/// with that lock's path.
fn witness(root: &Path, files: &[PathBuf], pid: u32) -> Option<(String, PathBuf)> {
    let held: BTreeSet<&PathBuf> = files.iter().collect();
    let mut found = held.into_iter().filter_map(|lock| {
        let session = mz::session_of_lock(root, lock)?;
        let body = std::fs::read_to_string(lock).ok()?;
        (mz::lock_pid(&body) == Some(pid)).then(|| (session, lock.clone()))
    });
    let one = found.next()?;
    found.next().is_none().then_some(one)
}

/// THE SESSIONS WHOSE LOCK NAMES `pid`, under `root`, written no earlier
/// than `since_ms` (the process's start, a second's slack): every lock at
/// session depth read, no `lsof` asked — the follow's read, where the lock
/// the witness found names its pid no more (`/new`, an in-TUI `/resume`),
/// and the exit's, where a crash left `pid=` in the lock of the session its
/// process held last.
fn locks_naming(root: &Path, pid: u32, since_ms: u64) -> Vec<(String, PathBuf)> {
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };
    let written_ms = |lock: &Path| -> Option<u64> {
        let at = std::fs::metadata(lock).ok()?.modified().ok()?;
        u64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
    };
    dirs(root)
        .iter()
        .flat_map(|y| dirs(y))
        .flat_map(|m| dirs(&m))
        .flat_map(|d| dirs(&d))
        .map(|s| s.join(mz::LOCK))
        .filter_map(|lock| {
            let session = mz::session_of_lock(root, &lock)?;
            let body = std::fs::read_to_string(&lock).ok()?;
            (mz::lock_pid(&body) == Some(pid)
                && written_ms(&lock).is_some_and(|at| at.saturating_add(1000) >= since_ms))
            .then_some((session, lock))
        })
        .collect()
}

/// The lock of `session` under `root`, wherever its date shard is
/// (`<root>/YYYY/MM/DD/<session>/.session.lock`): a session resumed on a
/// later day keeps the directory it began in.
fn find_lock(root: &Path, session: &str) -> Option<PathBuf> {
    if !mz::is_session_uuid(session) {
        return None;
    }
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };
    dirs(root)
        .iter()
        .flat_map(|y| dirs(y))
        .flat_map(|m| dirs(&m))
        .map(|d| d.join(session).join(mz::LOCK))
        .find(|lock| mz::session_of_lock(root, lock).is_some() && lock.is_file())
}

/// The session's own log beside its lock: at most `cap` bytes from its end
/// (a log longer than that is read from there, its first partial line
/// dropped).
fn session_log_tail(lock: &Path, cap: u64) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let log = lock.parent()?.join(mz::LOG);
    let mut f = std::fs::File::open(log).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(cap);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    f.take(cap).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(if from > 0 {
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text
    })
}

/// How much of a session log's end the exit's fallback reads (its last
/// run's start must be in it): the measured log of a session resumed twenty
/// times was 1.2 MB.
const LOG_BYTES: u64 = 32 << 20;

/// How much of it a snapshot reads for the session's workspace, at every
/// idle point: past that, the process's own directory stands in.
const WORKSPACE_BYTES: u64 = 4 << 20;

/// [`session_log_tail`] as the exit's fallback reads it.
fn session_log(lock: &Path) -> Option<String> {
    session_log_tail(lock, LOG_BYTES)
}

/// The session's workspace as its log's newest start names it, read off
/// [`WORKSPACE_BYTES`] of its end ([`mz::log_workspace`]).
fn workspace_of(lock: &Path) -> Option<String> {
    session_log_tail(lock, WORKSPACE_BYTES).and_then(|log| mz::log_workspace(&log))
}

/// The data home `args` names ([`mz::data_home`]), its sessions root, and
/// the homes its relaunch carries ([`mz::relaunch_env`]: its `HOME` too
/// where that is another than `user_home`, the user's own).
fn homes_of(
    args: &atpkg::caller_shell::ProcArgs,
    user_home: &Path,
) -> Option<(PathBuf, Vec<(String, String)>)> {
    let data = mz::data_home(
        args.env_var(mz::DATA_ENV),
        args.env_var(mz::HOME_ENV),
        user_home,
    )?;
    Some((
        canonical(&data),
        mz::relaunch_env(|name| args.env_var(name), user_home),
    ))
}

/// THE WORD OF THE RELAUNCH LINE'S OWN COMMAND (its block `block`,
/// [`St::line_block`], read right before the line was typed), once it ended
/// ([`mz::line_exit_kind`]): Muse Code refused the resume (`1`, `2`), or the
/// shell could not run the line (`cd` failed, no program: refused too), or
/// the relaunched Muse ended by its own or someone's hand, or crashed.
fn line_word(c: &mut Client, tab: &str, block: Option<u64>) -> Option<ExitKind> {
    line_status(c, tab, block).map(mz::line_exit_kind)
}

/// WHETHER `st` IS THE RECORD OF AN EARLIER ATTEMPT AT THIS VERY EXIT THAT
/// TYPED ITS LINE: the same tab's record (its key), naming `snap`'s process,
/// its line typed ([`St::relaunched_at`]) after that process started (a
/// record of an earlier process that had the same pid number was typed
/// before it).
fn typed_for_exit(st: &St, snap: &super::super::relaunch::Snapshot) -> bool {
    let started_ms = snap.muse().map_or(0, |run| run.started_ms);
    st.agent == Agent::Muse
        && st.pid == snap.pid
        && st.relaunched_at != 0
        && (started_ms == 0
            || st.relaunched_at.saturating_mul(1000).saturating_add(1000) >= started_ms)
}

/// A Muse Code TUI's run, read while it runs, for relaunch on exit.
pub(super) fn snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
    k: &dyn MuseKernel,
) -> Result<super::super::relaunch::Snapshot, &'static str> {
    if !mz::is_tui_argv(&args.argv) {
        return Err("not-a-tui");
    }
    let exe = k.exe(pid).ok_or("exe-unreadable")?;
    let (data, env) = homes_of(args, &opts.home).ok_or("home-unreadable")?;
    let files = k.open_files(pid).ok_or("files-unreadable")?;
    let held = witness(&mz::sessions_root(&data), &files, pid);
    let workspace = held.as_ref().and_then(|(_, lock)| workspace_of(lock));
    let cwd = k.cwd(pid).unwrap_or_default();
    let started_ms = k.started_ms(pid).unwrap_or(0);
    Ok(super::super::relaunch::Snapshot {
        tab: tab.to_string(),
        pid,
        start,
        shell,
        version: store_version(&exe),
        program: exe,
        argv: args.argv.clone(),
        session: held.as_ref().map(|(s, _)| s.clone()),
        cwd,
        run: super::super::relaunch::AgentRun::Muse(super::super::relaunch::MuseRun {
            data,
            env,
            workspace,
            lock: held.map(|(_, lock)| lock),
            started_ms,
            block: (shell != NO_SHELL)
                .then(|| connect(opts, tab).ok())
                .flatten()
                .and_then(|mut c| running_block(&mut c, tab)),
        }),
        dialect: (shell != NO_SHELL)
            .then(|| shell_dialect(shell, k))
            .flatten(),
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    })
}

/// Follow a Muse Code's snapshot `snap` between the loop's idle points, cheaply.
pub(super) fn follow(snap: &mut super::super::relaunch::Snapshot) -> bool {
    let pid = snap.pid;
    let super::super::relaunch::AgentRun::Muse(run) = &mut snap.run else {
        return false;
    };
    let unmoved = run.lock.as_ref().is_some_and(|lock| {
        std::fs::read_to_string(lock).is_ok_and(|body| mz::lock_pid(&body) == Some(pid))
    });
    if unmoved || run.started_ms == 0 {
        return false;
    }
    let held = match locks_naming(&mz::sessions_root(&run.data), pid, run.started_ms).as_slice() {
        [one] => Some(one.clone()),
        _ => return false,
    };
    let session = held.as_ref().map(|(s, _)| s.clone());
    if session == snap.session {
        return false;
    }
    run.workspace = held.as_ref().and_then(|(_, lock)| workspace_of(lock));
    run.lock = held.map(|(_, lock)| lock);
    snap.session = session;
    true
}

/// THE WORD THE EXIT OF `snap`'S MUSE CODE LEFT: its shell's exit status of
/// the block it ran in ([`super::super::relaunch::MuseRun::block`]), read by
/// Muse Code's table ([`mz::exit_kind`]); `None` while no word is drawn.
fn shell_word(opts: &Opts, snap: &super::super::relaunch::Snapshot) -> Option<ExitKind> {
    let block = snap.muse()?.block;
    connect(opts, &snap.tab)
        .ok()
        .and_then(|mut c| exit_status(&mut c, &snap.tab, block))
        .map(mz::exit_kind)
}

/// THE WORD OF THE SESSION'S OWN LOG for an exit whose shell said nothing
/// ([`mz::log_verdict`]): only for a snapshot whose process the witness saw
/// holding that session — a start Muse refused wrote nothing there.
fn log_word(snap: &super::super::relaunch::Snapshot) -> Option<ExitKind> {
    let run = snap.muse()?;
    let session = snap.session.as_deref()?;
    let lock = run
        .lock
        .clone()
        .or_else(|| find_lock(&mz::sessions_root(&run.data), session))?;
    mz::log_verdict(&session_log(&lock)?)
}

/// One look at a Muse Code that left its tab ([`super::super::relaunch::exit_record`] repeats it).
pub(super) fn look_at_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    k: &dyn MuseKernel,
) -> super::super::relaunch::ExitLook {
    let running =
        k.alive(snap.pid) && super::kernel_start(snap.pid).as_deref() == Some(snap.start.as_str());
    let word = if running {
        None
    } else {
        shell_word(opts, snap)
    };
    super::super::relaunch::ExitLook {
        running,
        record: None,
        agent: Agent::Muse,
        crashed: word.map(|w| w == ExitKind::Crashed),
        refused: word == Some(ExitKind::Refused),
        restarted_itself: false,
    }
}

/// The dialect of `shell` from its executable, as the relaunch line is
/// written for it ([`shell_dialect_of`]); `None` for a shell no line is
/// written for (`sh`, tcsh, nushell…), which [`plan`] waits on for ever.
fn shell_dialect(shell: u32, k: &dyn MuseKernel) -> Option<Dialect> {
    shell_dialect_of(shell, k.args(shell))
}

/// Relaunch on exit, Muse Code lane.
pub(super) fn after_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    _upgrade: bool,
    cause: ExitCause,
    k: &dyn MuseKernel,
) -> Report {
    after_exit_onto(opts, snap, left, cause, managed_agent_twin(mz::PROGRAM), k)
}

/// [`after_exit`] onto the twin `twin` ([`managed_agent_twin`]).
#[allow(clippy::too_many_lines)]
pub(super) fn after_exit_onto(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    cause: ExitCause,
    twin: Option<PathBuf>,
    k: &dyn MuseKernel,
) -> Report {
    use super::super::relaunch::{CAUSE_ASKED, CAUSE_EXIT, CAUSE_HOST, CAUSE_STALL, ExitRecord};
    let tab = snap.tab.clone();
    let session_key = key(&tab);
    let from = snap.version.clone().unwrap_or_else(|| "-".to_string());
    let mut r = Report {
        pid: snap.pid,
        tab: tab.clone(),
        session: session_key.clone(),
        from: from.clone(),
        to: "-".to_string(),
        step: String::new(),
    };
    let Some(run) = snap.muse() else {
        return said(r, "refused:not-muse");
    };
    let _held = match super::sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    if k.alive(snap.pid) && super::kernel_start(snap.pid).as_deref() == Some(snap.start.as_str()) {
        return said(r, "wait:not-exited");
    }
    prune_known(opts, vec![snap.tab.clone()]);
    let mut relaunched_before = false;
    let prior = load(opts, &session_key);
    let mut closed = None;
    if let Some(mut st) = prior.clone().filter(St::in_flight) {
        let overtaken = (st.pid != snap.pid || st.relaunched_gone(|pid| k.alive(pid))).then(|| {
            match st.phase {
                Phase::Exiting { .. } => (
                    "resumed-elsewhere",
                    "the session was resumed in this tab outside aterm's relaunch, and that \
                     process has exited"
                        .to_owned(),
                ),
                _ => (
                    "exited-before-continuing",
                    "the relaunched Muse Code exited before its continuation was typed".to_owned(),
                ),
            }
        });
        let overtook = overtaken.is_some();
        let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s(), k)) else {
            return carry_in_flight_with(opts, r, st, &session_key, k);
        };
        relaunched_before = st.relaunched_on_exit_of(snap.pid);
        if !opts.dry_run {
            if overtook {
                st.close_overtaken(why, now_s());
            } else {
                st.fail(why, now_s());
            }
            ledger(opts, &said(r.clone(), format!("failed:{why}")), &detail);
            save(opts, &session_key, &st);
        }
        closed = Some(st);
    }
    if let Some(earlier) = closed.or(prior).filter(|st| typed_for_exit(st, snap)) {
        let line = connect(opts, &tab)
            .ok()
            .and_then(|mut c| line_word(&mut c, &tab, earlier.line_block));
        let stop = match line {
            Some(ExitKind::Crashed) => None,
            Some(ExitKind::Refused) => Some((
                "refused:resume-refused",
                "Muse Code refused the relaunch's resume, or the shell could not run its line: \
                 typed again it would refuse again",
            )),
            Some(ExitKind::Clean) => Some((
                "ended:relaunch-ended",
                "the relaunched Muse Code ended by its own or someone's hand before it was \
                 adopted",
            )),
            None => Some((
                "refused:relaunch-unwitnessed",
                "the end of the relaunch's own line cannot be read: nothing is typed again",
            )),
        };
        if let Some((step, detail)) = stop {
            let r = said(r, step);
            if !opts.dry_run {
                ledger(opts, &r, &format!("{detail} ({})", earlier.line.trim()));
            }
            return r;
        }
        relaunched_before = true;
    }
    let word = match left {
        ExitRecord::Crashed | ExitRecord::Restarted => Some(ExitKind::Crashed),
        ExitRecord::Removed => Some(ExitKind::Clean),
        ExitRecord::Refused => Some(ExitKind::Refused),
        ExitRecord::Unread | ExitRecord::Survived(_) if cause == ExitCause::Exit => {
            if relaunched_before {
                Some(ExitKind::Crashed)
            } else {
                shell_word(opts, snap).or_else(|| log_word(snap))
            }
        }
        ExitRecord::Unread | ExitRecord::Survived(_) => None,
    };
    if cause == ExitCause::Exit {
        match word {
            Some(ExitKind::Crashed) => {}
            Some(ExitKind::Clean) => return said(r, "ended:graceful-exit"),
            Some(ExitKind::Refused) => return said(r, "ended:refused-start"),
            None => return said(r, "ended:exit-unwitnessed"),
        }
    }
    if snap.shell == NO_SHELL {
        return said(r, "refused:not-a-shell-job");
    }
    if !k.alive(snap.shell) {
        return said(r, "refused:shell-gone");
    }
    let root = mz::sessions_root(&run.data);
    let at_exit = match locks_naming(&root, snap.pid, run.started_ms).as_slice() {
        [one] if run.started_ms != 0 => Some(one.clone()),
        _ => None,
    };
    let (session, lock) = match at_exit {
        Some((session, lock)) => (session, Some(lock)),
        None => {
            let Some(session) = snap.session.clone().filter(|s| mz::is_session_uuid(s)) else {
                return said(r, "ended:no-conversation");
            };
            let lock = run
                .lock
                .clone()
                .filter(|l| l.is_file())
                .or_else(|| find_lock(&root, &session));
            (session, lock)
        }
    };
    let Some(lock) = lock else {
        return said(r, "ended:no-session");
    };
    let program = match relaunch_program(&snap.program, twin) {
        Ok(p) => p,
        Err(why) => return said(r, format!("refused:{why}")),
    };
    let to = if program == snap.program {
        from.clone()
    } else {
        twin_version(&program).unwrap_or_else(|| "-".to_string())
    };
    let source = if program == snap.program {
        super::SAME_PROGRAM
    } else {
        "managed"
    };
    r.to = format!("{to}({source})");
    let workspace = run
        .workspace
        .clone()
        .or_else(|| workspace_of(&lock))
        .unwrap_or_else(|| snap.cwd.clone());
    if opts.dry_run {
        return said(r, "would-relaunch");
    }
    let Ok(mut c) = connect(opts, &tab) else {
        return said(r, "wait:no-socket");
    };
    if !roster(&mut c).contains(&tab) {
        return said(r, "wait:tab-not-live");
    }
    let now = now_s();
    let mut st = St {
        agent: Agent::Muse,
        thread: session,
        muse_data: run.data.to_string_lossy().into_owned(),
        muse_env: mz::env_words(&run.env),
        argv: snap.argv.clone(),
        cwd: workspace,
        twin: program.to_string_lossy().into_owned(),
        pid: snap.pid,
        shell: snap.shell,
        tab: tab.clone(),
        from,
        to,
        source: source.to_string(),
        cause: match cause {
            ExitCause::Stall => CAUSE_STALL,
            ExitCause::Asked => CAUSE_ASKED,
            ExitCause::HostEnded => CAUSE_HOST,
            ExitCause::Exit => CAUSE_EXIT,
        }
        .to_string(),
        salt: now,
        exited_at: now,
        phase: Phase::Exiting { at_s: now },
        ..St::default()
    };
    save(opts, &session_key, &st);
    super::super::relaunch::consume_stamp(opts, snap, cause);
    ledger(
        opts,
        &said(r.clone(), "exited"),
        &format!(
            "the Muse Code left its tab ({}); relaunched on its session {}",
            if cause == ExitCause::HostEnded {
                "aterm ended while it ran"
            } else {
                word.map_or(left.word(), ExitKind::word)
            },
            st.thread
        ),
    );
    let r = relaunch(opts, r, &mut st, &mut c, k);
    save(opts, &session_key, &st);
    r
}

/// [`super::expired`] for a Muse Code relaunch: its exit over [`STALE_S`] ago
/// — or the last hand that held its line, whichever is later — and not
/// relaunched, its shell gone, or relaunched and never come up.
fn expired(st: &St, now: u64, k: &dyn MuseKernel) -> Option<(&'static str, String)> {
    match st.phase {
        Phase::Exiting { at_s }
            if now.saturating_sub(exit_clock(at_s, 0, st.hand_seen_s)) > STALE_S =>
        {
            Some((
                "stale-exit",
                super::super::relaunch::stale_exit_line(st, "Muse Code", now),
            ))
        }
        Phase::Exiting { .. } if st.shell == 0 || !k.alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran Muse Code is gone: there is no prompt to relaunch at".to_owned(),
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "no-resume",
            "the relaunched Muse Code never came up in the tab".to_owned(),
        )),
        _ => None,
    }
}

/// The lock of the session `st` relaunches, under the data home it records.
fn st_lock(st: &St) -> Option<PathBuf> {
    find_lock(
        &mz::sessions_root(Path::new(&st.muse_data)),
        st.thread.as_str(),
    )
}

/// Plan the relaunch from what the record keeps.
pub(super) fn plan(opts: &Opts, st: &St, k: &dyn MuseKernel) -> Result<Plan, NoPlan> {
    // No shell ([`NO_SHELL`], a tab's own Muse Code) is refused for good, as
    // the Claude lane's plan refuses it: nothing would read the line.
    if st.shell == NO_SHELL {
        return Err(NoPlan::Refused {
            why: "not-a-shell-job".to_string(),
            what: "not-a-shell-job".to_string(),
            detail: NO_SHELL_DETAIL.to_string(),
        });
    }
    let Some(dialect) = shell_dialect(st.shell, k) else {
        return Err(NoPlan::Wait("shell-dialect"));
    };
    let heal = atpkg::hooks::hook_file(&opts.home, dialect.hook_ext()).map(|(p, _)| p);
    let want = st.cwd.clone();
    let cd =
        (!want.is_empty() && k.cwd(st.shell).as_deref() != Some(want.as_str())).then_some(want);
    let flags = mz::rewrite_argv(&st.argv, Some(&st.thread)).map_err(|e| NoPlan::Refused {
        why: format!("argv:{e}"),
        what: e.to_string(),
        detail: "the launch flags cannot be carried into a resume".to_string(),
    })?;
    let env = mz::env_of_words(&st.muse_env);
    let line = upgrade::relaunch_line_env(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        &mz::line_env(&env),
        Path::new(&st.twin),
        &flags,
    )
    .map_err(|e| NoPlan::Refused {
        why: format!("line:{e}"),
        what: "line".to_string(),
        detail: e,
    })?;
    Ok(Plan {
        shell: st.shell,
        dialect,
        line,
    })
}

/// Relaunch the Muse Code `st` records at its tab's shell, the harness's
/// hand on the tab through every wait and until the new TUI's first idle.
fn relaunch(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn MuseKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = relaunch_held(opts, r, st, c, &mut hand, k);
    if !matches!(st.phase, Phase::Exiting { .. }) {
        hand.give_back(c);
    }
    r
}

/// [`relaunch`] with the harness's `hand` held.
fn relaunch_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn MuseKernel,
) -> Report {
    let old = st.pid;
    if !wait_until(opts.timings.exited, || !k.alive(old)) {
        return said(r, "wait:exiting");
    }
    hand.keep(c);
    if st.exited_at == 0 {
        st.exited_at = now_s();
    }
    let shell = st.shell;
    if !wait_until(opts.timings.prompt_back, || k.shell_has_terminal(shell)) {
        return said(r, "wait:shell-prompt");
    }
    hand.keep(c);
    pause(opts.timings.prompt_drawn);
    let tab = st.tab.clone();
    let Some(lock) = st_lock(st) else {
        return fail(
            opts,
            r,
            st,
            "no-session",
            "the session's directory is gone: Muse Code has nothing to resume",
        );
    };
    let lock_free =
        |k: &dyn MuseKernel| -> bool { k.holders_of(&lock).is_some_and(|h| h.is_empty()) };
    if !lock_free(k) {
        return said(r, "wait:session-held");
    }
    let (line, dialect) = match plan(opts, st, k) {
        Ok(p) => (p.line, p.dialect),
        Err(NoPlan::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(NoPlan::Refused { why, detail, .. }) => return fail(opts, r, st, &why, &detail),
    };
    st.line.clone_from(&line);
    save(opts, &key(&tab), st);
    let block = super::super::relaunch::line_block(c, &tab);
    match type_relaunch_line_with(
        opts,
        c,
        shell,
        &tab,
        &line,
        Some(dialect),
        PromptLook::of(st),
        || lock_free(k),
        |c, pid, tab| k.in_tab(c, pid, tab),
    ) {
        Ok(()) => st.prompt = None,
        Err(RelaunchLineError::Wait(why)) => {
            st.note_wait(why, now_s());
            return said(r, format!("wait:{why}"));
        }
        Err(RelaunchLineError::Hand) => {
            let now = now_s();
            st.saw_hand(now);
            st.note_wait("held", now);
            return said(r, "wait:held");
        }
        Err(RelaunchLineError::Turn(e)) => {
            st.fail("relaunch-refused", now_s());
            let r = said(r, format!("failed:relaunch:{}", super::first_word(&e)));
            ledger(opts, &r, &line);
            return r;
        }
    }
    st.relaunched(now_s());
    st.line_block = block;
    st.relaunched_pid = 0;
    st.relaunched_start.clear();
    ledger(
        opts,
        &said(r.clone(), "relaunched"),
        &format!("{} (resume)", st.line),
    );
    save(opts, &key(&tab), st);
    await_held(opts, r, st, c, hand, k)
}

/// Record a Muse Code relaunch that stops here, with one ledger line.
fn fail(opts: &Opts, r: Report, st: &mut St, why: &str, detail: &str) -> Report {
    st.fail(why, now_s());
    let r = said(r, format!("failed:{why}"));
    ledger(opts, &r, detail);
    r
}

/// THE RELAUNCH LINE RAN AND ENDED with no relaunched Muse Code found
/// leading the tab ([`line_word`]'s `kind`): it never came up, or was gone
/// before it was adopted.
fn line_ended(opts: &Opts, r: Report, st: &mut St, kind: ExitKind) -> Report {
    let now = now_s();
    let (step, detail) = match kind {
        ExitKind::Refused => {
            st.fail("resume-refused", now);
            (
                "refused:resume-refused",
                "Muse Code refused the relaunch's resume, or the shell could not run its line",
            )
        }
        ExitKind::Clean => {
            st.fail("relaunch-ended", now);
            (
                "ended:relaunch-ended",
                "the relaunched Muse Code ended by its own or someone's hand before it was adopted",
            )
        }
        ExitKind::Crashed => {
            st.close_overtaken("exited-before-continuing", now);
            (
                "failed:exited-before-continuing",
                "the relaunched Muse Code crashed before its continuation was typed",
            )
        }
    };
    let r = said(r, step);
    ledger(opts, &r, &format!("{detail} ({})", st.line.trim()));
    r
}

/// Whether `pid` (argv `argv`) is the relaunch this lane typed: a child of
/// the line's shell, not the exited TUI, a Muse Code TUI resuming the
/// recorded session.
fn relaunched_by_us(pid: u32, argv: &[String], st: &St, k: &dyn MuseKernel) -> bool {
    pid != st.pid
        && matches!(st.phase, Phase::Relaunched { .. } | Phase::Exiting { .. })
        && k.parent(pid) == Some(st.shell)
        && mz::is_tui_argv(argv)
        && mz::resumed_ref(argv).as_deref() == Some(st.thread.as_str())
}

/// After the relaunch line: find the new TUI and carry on with it, or hand it
/// back to its loop ([`adopt`]).
fn await_new(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn MuseKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = await_held(opts, r, st, c, &mut hand, k);
    hand.give_back(c);
    r
}

/// [`await_new`] with the harness's `hand` held; its caller gives it back.
fn await_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn MuseKernel,
) -> Report {
    let mut new = None;
    let mut ended = None;
    let mut looked: Option<std::time::Instant> = None;
    let tab = st.tab.clone();
    wait_until(opts.timings.muse_found, || {
        hand.keep(c);
        new = k
            .muse_children(st.shell)
            .into_iter()
            .find(|(pid, argv)| relaunched_by_us(*pid, argv, st, k))
            .map(|(pid, _)| pid);
        if new.is_none()
            && looked.is_none_or(|at| {
                wait_now().saturating_duration_since(at) >= opts.timings.shell_look
            })
        {
            looked = Some(wait_now());
            ended = line_word(c, &tab, st.line_block);
        }
        new.is_some() || ended.is_some()
    });
    if new.is_some_and(|pid| k.alive(pid)) {
        first_idle(c, hand, &st.tab, opts.timings.first_idle, || true);
    }
    match new {
        Some(pid) if opts.hand_back && st.resumed_pid == pid => {
            let mut r = said(r, "adopted");
            r.pid = pid;
            r
        }
        Some(pid) if opts.hand_back => adopt(opts, r, st, c, pid, k),
        Some(pid) => carry_on(opts, r, st, c, pid, k, Some(hand)),
        None => {
            // The line's own command ended: its Muse never came up, or was
            // gone before it was found.
            if let Some(kind) = ended.or_else(|| line_word(c, &st.tab, st.line_block)) {
                return line_ended(opts, r, st, kind);
            }
            if let Phase::Relaunched { at_s } = st.phase
                && now_s().saturating_sub(at_s) > STALE_S
            {
                return fail(
                    opts,
                    r,
                    st,
                    "no-resume",
                    "the relaunched Muse Code never came up in the tab",
                );
            }
            said(r, "wait:resume")
        }
    }
}

/// The relaunched TUI `new`, on the record whatever becomes of it: once it is
/// gone, a Muse Code leaving the tab is that TUI, its exit its own.
fn found<K: Kernel + ?Sized>(st: &mut St, new: u32, k: &K) {
    if st.relaunched_pid == 0 {
        st.relaunched_start = k.process_start(new).map(|s| squash(&s)).unwrap_or_default();
    } else if st.relaunched_pid != new {
        st.relaunched_start.clear();
    }
    st.relaunched_pid = new;
}

/// Hand the relaunched Muse Code back to its supervisor (`adopted`): the new
/// TUI leads the tab by the host's own roster and the record stays in
/// flight; at its loop's next idle point the relaunch primitive's
/// continuation ([`super::super::relaunch::resume`]) types the carry-on
/// ([`carry_on`]).
fn adopt(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn MuseKernel,
) -> Report {
    found(st, new, k);
    let tab = st.tab.clone();
    let mut gone = false;
    let led = wait_until(opts.timings.muse_adopt, || {
        let led = k.in_tab(c, new, &tab);
        gone = !led && !k.alive(new);
        led || gone
    }) && !gone;
    if !led {
        let mut word = None;
        let bound = if gone {
            opts.timings.ended_at_once
        } else {
            Duration::ZERO
        };
        wait_until(bound, || {
            word = line_word(c, &tab, st.line_block);
            word.is_some()
        });
        if let Some(kind) = word {
            return line_ended(opts, r, st, kind);
        }
        return said(r, "wait:resume");
    }
    st.resumed_pid = new;
    r.pid = new;
    let r = said(r, "adopted");
    ledger(
        opts,
        &r,
        if st.quiet {
            "the relaunched Muse Code leads the tab; at its first idle point its supervisor \
             confirms the session's lock names it, and types nothing"
        } else {
            "the relaunched Muse Code leads the tab; its supervisor types the continuation at its \
             first idle point"
        },
    );
    r
}

/// A Muse Code relaunch in flight carried on from where it stopped.
pub(in crate::harness) fn carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    carry_in_flight_with(opts, r, st, session, &Live)
}

/// [`carry_in_flight`] over the kernel `k`.
fn carry_in_flight_with(
    opts: &Opts,
    r: Report,
    mut st: St,
    session: &str,
    k: &dyn MuseKernel,
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
    if hand_waits_out(opts, &mut st, session, |pid| k.alive(pid)) {
        return said(r, WAIT_HAND_LIFTS);
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

/// Whether Muse Code's composer on `scr` is empty: its caret row holds
/// nothing, or only its prompt hint (drawn at column 2 in its dim grey,
/// [`aterm_phase::ScreenReader::placeholder_fg`]) with nothing under it.
fn composer_empty(c: &mut Client, tab: &str, scr: &super::Screen) -> bool {
    let Some((caret, lines)) = aterm_phase::muse::composer_draft(&scr.rows) else {
        return false;
    };
    if lines.iter().all(|l| l.trim().is_empty()) {
        return true;
    }
    let hint = aterm_phase::Program::Muse.reader().placeholder_fg();
    lines.iter().skip(1).all(|l| l.trim().is_empty())
        && scr.cursor == Some((caret, 2))
        && scr.first.checked_add(caret).is_some_and(|at| {
            c.request_line(&format!("@{tab} cell {at} 2"))
                .ok()
                .and_then(|line| crate::supervise::screen::cell_is_placeholder(&line, hint))
                == Some(true)
        })
}

/// The last look before typing into Muse Code's composer: a fresh read
/// reading idle by its reader (no box, no turn running, its composer drawn)
/// with the composer empty, and that read's generation, which the typed turn
/// is fenced on.
fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    let _ = turn_takes_gen(c);
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    match aterm_phase::muse::phase(&scr.rows) {
        (aterm_phase::Phase::Idle | aterm_phase::Phase::Question, true) => {}
        (aterm_phase::Phase::Prompt, _) => return Err("box"),
        _ => return Err("busy"),
    }
    if !composer_empty(c, tab, &scr) {
        return Err("draft");
    }
    Ok(scr.generation)
}

/// Type `text` into Muse Code's composer as one turn, fenced on `generation`
/// using the standard turn echo settle and guarded submit.
fn type_text(c: &mut Client, tab: &str, text: &str, generation: Option<&str>) -> Result<(), Typed> {
    let fence = generation
        .filter(|g| is_generation(g))
        .filter(|_| turn_takes_gen(c))
        .map(|g| format!("if-gen={g} yield=0.2 "))
        .unwrap_or_default();
    let guard = super::composer_guard('❯', text);
    let (head, _) = c
        .request_counted(&format!(
            "@{tab} turn {fence}submit=guarded:{guard} {TURN_WAIT} {text}"
        ))
        .map_err(|e| Typed::Refused(format!("{e}")))?;
    turn_verdict(&head)
}

/// The relaunched TUI `new` is up: once its composer is drawn, its session's
/// lock is held by it and it reads idle and empty with no person at the tab,
/// the continuation is typed (`continued`); a quiet relaunch types nothing.
#[allow(clippy::too_many_arguments)]
fn carry_on(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn MuseKernel,
    mut hand: Option<&mut Hand>,
) -> Report {
    found(st, new, k);
    r.pid = new;
    let tab = st.tab.clone();
    let up = wait_until(opts.timings.muse_up, || {
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        screen(c, &tab).is_some_and(|s| aterm_phase::muse::composer(&s.rows).is_some())
    });
    let mut typed = false;
    let lock_moved = up
        && st.quiet
        && st_lock(st).is_some_and(|lock| {
            wait_until(opts.timings.muse_lock, || {
                if let Some(h) = hand.as_deref_mut() {
                    h.keep(c);
                }
                std::fs::read_to_string(&lock).is_ok_and(|body| mz::lock_pid(&body) == Some(new))
            })
        });
    if up && !st.quiet {
        let lock = st_lock(st);
        let holds = wait_until(opts.timings.muse_lock, || {
            if let Some(h) = hand.as_deref_mut() {
                h.keep(c);
            }
            lock.as_ref()
                .and_then(|l| k.holders_of(l))
                .is_some_and(|h| h.contains(&new))
        });
        let settled = holds
            && wait_until(opts.timings.carry_settle, || {
                if let Some(h) = hand.as_deref_mut() {
                    h.keep(c);
                }
                typing_fence(c, &tab).is_ok()
            });
        if settled && held(c, &tab, opts.human_grace_s) {
            return said(r, "wait:held");
        }
        if settled && k.in_tab(c, new, &tab) {
            let text = super::super::relaunch::resumed_prompt_as("Muse Code", &st.to, &st.cause);
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
                            pause(Duration::from_millis(400));
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
    st.outcome = if st.quiet {
        format!(
            "muse upgraded to {} (from {}) · the same session resumed, nothing typed",
            st.to, st.from
        )
    } else {
        format!("muse on {} · the same conversation resumed", st.to)
    };
    let step = if !up {
        "done:unconfirmed"
    } else if typed {
        "continued"
    } else if st.quiet && lock_moved {
        "done:resumed"
    } else if st.quiet {
        // Up, and its session's lock never named it: the relaunch is not
        // proved the session's (nothing was typed either way).
        "done:unconfirmed"
    } else {
        "done:no-continue"
    };
    let r = said(r, step);
    ledger(opts, &r, &st.outcome);
    r
}

/// Remove Muse Code records nothing will visit again, as codex-alab's path removes its own.
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
            .and_then(|stem| Agent::Muse.record_tab(stem))
        else {
            continue;
        };
        if !super::sat_a_week(&entry, now) {
            continue;
        }
        let Some(st) = load(opts, &key(tab)) else {
            continue;
        };
        let ours = st.agent == Agent::Muse && (st.tab.is_empty() || st.tab == tab);
        if ours && !st.in_flight() && super::settled(&st) && life(tab) == TabLife::Gone {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The prune of one sweep, as codex-alab's sweep prunes its records (`codex_alab_drive::sweep`).
pub(super) fn sweep_prune(opts: &Opts, live_tabs: Option<&[LiveTab]>) {
    prune_known(
        opts,
        live_tabs
            .into_iter()
            .flatten()
            .map(|t| t.sid.clone())
            .collect(),
    );
}

/// [`prune`] with the tabs `known_live` proven live, the rendezvous registry
/// read for the rest only where it can vouch for this state root
/// ([`registry_graph`], [`registry_life`]).
fn prune_known(opts: &Opts, known_live: Vec<String>) {
    let graph = registry_graph();
    prune(opts, now_s(), registry_life(graph.as_deref(), &known_live));
}

/// The live Muse Code holders of the `muse-<tab>` records in `sessions`, for the status view.
pub(super) fn holders(
    sessions: &BTreeSet<String>,
    live_tabs: Option<&[LiveTab]>,
) -> Vec<super::upgrade_status::Holder> {
    if !sessions.iter().any(|s| Agent::Muse.record_tab(s).is_some()) {
        return Vec::new();
    }
    let procs = table();
    let tab_of = |pid: u32| -> Option<String> {
        match live_tabs {
            Some(tabs) => tabs
                .iter()
                .find(|t| t.fgpgid == Some(i64::from(pid)))
                .map(|t| t.sid.clone()),
            None => atpkg::caller_shell::process_args(pid)
                .and_then(|a| a.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned)),
        }
    };
    procs
        .iter()
        .filter(|(_, _, name)| name == mz::PROGRAM)
        .filter_map(|(pid, _, _)| {
            let tab = tab_of(*pid)?;
            let session = key(&tab);
            sessions
                .contains(&session)
                .then(|| super::upgrade_status::Holder {
                    session,
                    version: exe_of(*pid)
                        .and_then(|exe| store_version(&exe))
                        .unwrap_or_default(),
                    tab: Some(tab),
                    ..Default::default()
                })
        })
        .collect()
}

#[path = "upgrade_muse_live_drive.rs"]
pub(super) mod live;

#[cfg(all(test, unix))]
#[path = "upgrade_muse_drive_tests.rs"]
mod tests;
