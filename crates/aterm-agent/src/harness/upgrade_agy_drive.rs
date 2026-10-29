// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE AGY LANE, ITS I/O HALF ([`super::super::upgrade_agy`] is the pure half): an agy that crashed out of its tab is relaunched there on its own conversation, and one aterm's own end took is brought back in its reopened tab.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::relaunch::{
    ExitCause, NO_SHELL, NO_SHELL_DETAIL, WAIT_HAND_LIFTS, exit_clock, first_idle, hand_waits_out,
};
use super::super::upgrade_agy::{self as ag, ExitKind};
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

/// The process facts the agy lane rests on, each read when asked: the Claude lane's [`Kernel`] plus agy's needs.
pub(super) trait AgyKernel: Kernel {
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
    /// The live agy processes whose argv names `conversation` ([`ag::resumed_conversation`], any spelling), `None` when one's argv cannot be read: a second agy opened on a held conversation holds no lock, even once the holder is gone (measured).
    fn agy_resuming(&self, conversation: &str) -> Option<Vec<u32>>;
    /// The live agy processes that have `conversation` open without its lock under `config` ([`ag::conversation_db`], [`ag::holds_no_lock`]).
    fn lockless_openers(&self, config: &Path, conversation: &str) -> Option<Vec<u32>>;
    /// Whether `shell`'s group holds its terminal again (`pgid == tpgid`).
    fn shell_has_terminal(&self, shell: u32) -> bool;
    /// The live `agy` processes whose parent is `shell`, with their argv.
    fn agy_children(&self, shell: u32) -> Vec<(u32, Vec<String>)>;
    /// `pid`'s working directory.
    fn cwd(&self, pid: u32) -> Option<String>;
    /// Whether `pid` is still the foreground group of `tab` by the host's own roster: asked right before every typed act.
    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool;
    /// Every descendant of `pid` (`(pid, basename)`), `None` where the process table cannot be read.
    fn descendants(&self, pid: u32) -> Option<Vec<(u32, String)>> {
        descendants_in(&table(), pid)
    }
    /// When `pid` started, unix milliseconds (the kernel's): where its conversation's transcript begins to be this process's.
    fn started_ms(&self, pid: u32) -> Option<u64> {
        started_ms(pid)
    }
    /// The answer of the read-only query of `conversation`'s summary rows in the database under `config` ([`live::read_summaries`]).
    fn summaries(&self, config: &Path, conversation: &str) -> Result<String, ()> {
        live::read_summaries(config, conversation)
    }
    /// What agy's quota rotation makes of a launch on `model` (its `--model`; `None`: agy's
    /// default) as `exe` under the relaunch's `env` and config `flags`
    /// ([`crate::harness::agy_quota_store::rotate_worker`]); `None` in a kernel that reads no quota.
    fn quota_pick(
        &self,
        _model: Option<&str>,
        _exe: &Path,
        _env: &[(String, String)],
        _flags: &[String],
    ) -> Option<crate::harness::agy_quota::Pick> {
        None
    }
}

impl AgyKernel for Live {
    fn alive(&self, pid: u32) -> bool {
        alive(pid)
    }

    fn quota_pick(
        &self,
        model: Option<&str>,
        exe: &Path,
        env: &[(String, String)],
        flags: &[String],
    ) -> Option<crate::harness::agy_quota::Pick> {
        crate::harness::agy_quota_store::rotate_worker(
            model,
            exe,
            env,
            flags,
            crate::supervise::limit::unix_now(),
        )
        .map(|l| l.pick)
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

    fn agy_resuming(&self, conversation: &str) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        for (pid, _, _) in table()
            .into_iter()
            .filter(|(_, _, name)| name == ag::PROGRAM)
        {
            match atpkg::caller_shell::process_args(pid) {
                Some(args) => {
                    if ag::resumed_conversation(&args.argv).as_deref() == Some(conversation) {
                        out.push(pid);
                    }
                }
                None if alive(pid) => return None,
                None => {}
            }
        }
        Some(out)
    }

    fn lockless_openers(&self, config: &Path, conversation: &str) -> Option<Vec<u32>> {
        let db = ag::conversation_db(config, conversation);
        let agys: BTreeSet<u32> = table()
            .into_iter()
            .filter(|(_, _, name)| name == ag::PROGRAM)
            .map(|(pid, _, _)| pid)
            .collect();
        let mut out = Vec::new();
        for pid in agys {
            match open_files(pid) {
                Some(files) => {
                    if files.iter().any(|f| f == &db) && ag::holds_no_lock(config, &files) {
                        out.push(pid);
                    }
                }
                None if alive(pid) => return None,
                None => {}
            }
        }
        Some(out)
    }

    fn shell_has_terminal(&self, shell: u32) -> bool {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    }

    fn agy_children(&self, shell: u32) -> Vec<(u32, Vec<String>)> {
        table()
            .into_iter()
            .filter(|(_, ppid, name)| *ppid == shell && name == ag::PROGRAM)
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

/// The agy lane's per-tab record prefix ([`Agent::record_prefix`]), `agy-`.
const PREFIX: &str = match Agent::Agy.record_prefix() {
    Some(prefix) => prefix,
    None => panic!("the agy lane keys its records by tab"),
};

/// The state key of the agy relaunch in `tab` (`agy-<tab>`, [`Agent::record_key`]).
pub(super) fn key(tab: &str) -> String {
    format!("{PREFIX}{tab}")
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The managed agy a relaunch types: atpkg's twin (`<prefix>/agents/agy`), never the store build the crashed process ran.
fn relaunch_program(program: &Path, twin: Option<PathBuf>) -> Result<PathBuf, &'static str> {
    if !ag::store_build(program) {
        return Ok(program.to_path_buf());
    }
    twin.ok_or("no-managed-agy")
}

/// Whether an exit of `snap`'s agy now would be relaunched on its conversation onto `twin` ([`managed_agent_twin`]).
pub(super) fn carries_restart_onto(
    snap: &super::super::relaunch::Snapshot,
    home: &Path,
    twin: Option<PathBuf>,
) -> bool {
    let Some(run) = snap.agy() else {
        return false;
    };
    let (Some(dialect), Some(conversation)) = (
        snap.dialect,
        snap.session
            .as_deref()
            .filter(|c| ag::is_conversation_uuid(c)),
    ) else {
        return false;
    };
    let (Ok(flags), Ok(program)) = (
        ag::rewrite_argv(&snap.argv, Some(conversation)),
        relaunch_program(&snap.program, twin),
    ) else {
        return false;
    };
    let heal = atpkg::hooks::hook_file(home, dialect.hook_ext()).map(|(p, _)| p);
    let cd = match dialect {
        Dialect::Zsh | Dialect::Bash => (!snap.cwd.is_empty()).then_some(snap.cwd.as_str()),
        Dialect::Fish => None,
    };
    upgrade::relaunch_line_env(
        dialect,
        heal.as_deref(),
        cd,
        &ag::line_env(&run.env),
        &program,
        &flags,
    )
    .is_ok()
}

/// What the files a live agy holds open say ([`witness`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Held {
    conversation: Option<(String, PathBuf)>,
    crash: Option<PathBuf>,
}

/// The conversation `pid` holds, by the kernel and by agy's own lock under `config` ([`ag::lock_conversation`]).
fn witness(config: &Path, files: &[PathBuf], pid: u32) -> Held {
    fn one<T: Clone>(found: &[T]) -> Option<T> {
        match found {
            [one] => Some(one.clone()),
            _ => None,
        }
    }
    let held: BTreeSet<&PathBuf> = files.iter().collect();
    let conversations: Vec<(String, PathBuf)> = held
        .iter()
        .filter_map(|lock| {
            ag::lock_conversation(config, lock).map(|conversation| (conversation, (*lock).clone()))
        })
        .collect();
    let crashes: Vec<PathBuf> = held
        .iter()
        .filter(|file| ag::crash_file(config, file, pid))
        .map(|file| (*file).clone())
        .collect();
    Held {
        conversation: one(&conversations),
        crash: one(&crashes),
    }
}

/// Whether the dead process's crash file is left: `crash`, else a crash file of `pid` under
/// `config` written no earlier than `started_ms` (a second's slack), and none when
/// `started_ms` is `0` (unread).
fn crash_left(config: &Path, pid: u32, crash: Option<&Path>, started_ms: u64) -> bool {
    if let Some(file) = crash {
        return file.is_file();
    }
    if started_ms == 0 {
        return false;
    }
    let written_ms = |file: &Path| -> Option<u64> {
        let at = std::fs::metadata(file).ok()?.modified().ok()?;
        u64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
    };
    std::fs::read_dir(config.join(ag::CRASHES))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .any(|file| {
            ag::crash_file(config, &file, pid)
                && written_ms(&file).is_some_and(|at| at.saturating_add(1000) >= started_ms)
        })
}

/// The config `args` name ([`ag::config_dir`], canonical), and the home its relaunch carries ([`ag::relaunch_env`]).
fn homes_of(
    args: &atpkg::caller_shell::ProcArgs,
    user_home: &Path,
) -> Option<(PathBuf, Vec<(String, String)>)> {
    let config = ag::config_dir(&args.argv, args.env_var(ag::HOME_ENV), user_home)?;
    Some((
        canonical(&config),
        ag::relaunch_env(|name| args.env_var(name), user_home),
    ))
}

/// The word of the relaunch line's own command ([`St::line_block`]) once it ended ([`ag::line_exit_kind`]).
fn line_word(c: &mut Client, st: &St, k: &dyn AgyKernel) -> Option<ExitKind> {
    let status = line_status(c, &st.tab, st.line_block)?;
    if relaunched_lives(st, k) {
        return Some(ExitKind::Stopped);
    }
    let left = st.relaunched_pid != 0
        && crash_left(
            Path::new(&st.agy_config),
            st.relaunched_pid,
            None,
            st.relaunched_at.saturating_mul(1000),
        );
    Some(ag::line_exit_kind(status, left))
}

/// Whether the agy the relaunch line started ([`St::relaunched_pid`]) still lives.
fn relaunched_lives(st: &St, k: &dyn AgyKernel) -> bool {
    st.relaunched_pid != 0
        && k.alive(st.relaunched_pid)
        && (st.relaunched_start.is_empty()
            || k.process_start(st.relaunched_pid)
                .is_some_and(|s| squash(&s) == st.relaunched_start))
}

/// Whether `st` is the record of an earlier attempt at this very exit that typed its line.
fn typed_for_exit(st: &St, snap: &super::super::relaunch::Snapshot) -> bool {
    let started_ms = snap.agy().map_or(0, |run| run.started_ms);
    st.agent == Agent::Agy
        && st.pid == snap.pid
        && st.relaunched_at != 0
        && (started_ms == 0
            || st.relaunched_at.saturating_mul(1000).saturating_add(1000) >= started_ms)
}

/// An agy TUI's run, read while it runs, for relaunch on exit ([`super::super::relaunch::snapshot`]).
pub(super) fn snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
    k: &dyn AgyKernel,
) -> Result<super::super::relaunch::Snapshot, &'static str> {
    if !ag::is_tui_argv(&args.argv) {
        return Err("not-a-tui");
    }
    let exe = k.exe(pid).ok_or("exe-unreadable")?;
    let (config, env) = homes_of(args, &opts.home).ok_or("config-unreadable")?;
    let files = k.open_files(pid).ok_or("files-unreadable")?;
    let held = witness(&config, &files, pid);
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
        session: held.conversation.as_ref().map(|(c, _)| c.clone()),
        cwd,
        run: super::super::relaunch::AgentRun::Agy(super::super::relaunch::AgyRun {
            config,
            env,
            lock: held.conversation.map(|(_, lock)| lock),
            crash: held.crash,
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

/// The word the exit of `snap`'s agy left ([`ag::exit_kind`]), or `None` while no status is drawn.
fn shell_word(opts: &Opts, snap: &super::super::relaunch::Snapshot) -> Option<ExitKind> {
    let run = snap.agy()?;
    let status = connect(opts, &snap.tab)
        .ok()
        .and_then(|mut c| exit_status(&mut c, &snap.tab, run.block))?;
    let left = crash_left(&run.config, snap.pid, run.crash.as_deref(), run.started_ms);
    Some(ag::exit_kind(status, left))
}

/// One look at an agy that left its tab ([`super::super::relaunch::exit_record`]).
pub(super) fn look_at_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    k: &dyn AgyKernel,
) -> super::super::relaunch::ExitLook {
    let running =
        k.alive(snap.pid) && k.process_start(snap.pid).as_deref() == Some(snap.start.as_str());
    let word = if running {
        None
    } else {
        shell_word(opts, snap)
    };
    super::super::relaunch::ExitLook {
        running,
        record: None,
        agent: Agent::Agy,
        crashed: match word {
            Some(ExitKind::Crashed) => Some(true),
            Some(ExitKind::Clean | ExitKind::Refused) => Some(false),
            Some(ExitKind::Stopped | ExitKind::Unwitnessed) | None => None,
        },
        refused: word == Some(ExitKind::Refused),
    }
}

/// The dialect of `shell` from its executable ([`shell_dialect_of`]).
fn shell_dialect(shell: u32, k: &dyn AgyKernel) -> Option<Dialect> {
    shell_dialect_of(shell, k.args(shell))
}

/// Relaunch on exit, agy lane ([`super::super::relaunch::after_exit`] hands an agy snapshot here): its TUI `snap` left its tab and nobody owns the exit.
pub(super) fn after_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    _upgrade: bool,
    cause: ExitCause,
    k: &dyn AgyKernel,
) -> Report {
    after_exit_onto(opts, snap, left, cause, managed_agent_twin(ag::PROGRAM), k)
}

/// [`after_exit`] onto the twin `twin` ([`managed_agent_twin`]): the seam a test names its own through.
#[allow(clippy::too_many_lines)]
pub(super) fn after_exit_onto(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    cause: ExitCause,
    twin: Option<PathBuf>,
    k: &dyn AgyKernel,
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
    let Some(run) = snap.agy() else {
        return said(r, "refused:not-agy");
    };
    let _held = match super::sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    if k.alive(snap.pid) && k.process_start(snap.pid).as_deref() == Some(snap.start.as_str()) {
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
                    "the conversation was resumed in this tab outside aterm's relaunch, and that \
                     process has exited"
                        .to_owned(),
                ),
                _ => (
                    "exited-before-continuing",
                    "the relaunched agy exited before its continuation was typed".to_owned(),
                ),
            }
        });
        let overtook = overtaken.is_some();
        if !overtook && hand_waits_out(opts, &mut st, &session_key, |pid| k.alive(pid)) {
            if st.exited_at == 0 {
                st.exited_at = now_s();
                save(opts, &session_key, &st);
            }
            return said(r, WAIT_HAND_LIFTS);
        }
        let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s(), k)) else {
            return carry_in_flight_with(opts, r, st, &session_key, k);
        };
        relaunched_before = st.relaunched_on_exit_of(snap.pid);
        if !opts.dry_run {
            if overtook {
                st.close_overtaken(why, now_s());
            } else {
                if st.exited_at == 0 {
                    st.exited_at = now_s();
                }
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
            .and_then(|mut c| line_word(&mut c, &earlier, k));
        let stop = match line {
            Some(ExitKind::Crashed) => None,
            Some(ExitKind::Refused) => Some((
                "refused:resume-refused",
                "agy refused the relaunch, or the shell could not run its line: typed again it \
                 would refuse again",
            )),
            Some(ExitKind::Clean | ExitKind::Stopped) => Some((
                "ended:relaunch-ended",
                "the relaunched agy ended, or was stopped, by its own or someone's hand before it \
                 was adopted",
            )),
            Some(ExitKind::Unwitnessed) | None => Some((
                "refused:relaunch-unwitnessed",
                "the end of the relaunch's own line cannot be read, or no crash file tells it: \
                 nothing is typed again",
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
        ExitRecord::Crashed => Some(ExitKind::Crashed),
        ExitRecord::Removed => Some(ExitKind::Clean),
        ExitRecord::Refused => Some(ExitKind::Refused),
        ExitRecord::Unread | ExitRecord::Survived(_) if cause == ExitCause::Exit => {
            if relaunched_before {
                Some(ExitKind::Crashed)
            } else {
                shell_word(opts, snap)
            }
        }
        ExitRecord::Unread | ExitRecord::Survived(_) => None,
    };
    if cause == ExitCause::Exit {
        match word {
            Some(ExitKind::Crashed) => {}
            Some(ExitKind::Clean) => return said(r, "ended:graceful-exit"),
            Some(ExitKind::Refused) => return said(r, "ended:refused-start"),
            Some(ExitKind::Stopped) => return said(r, "ended:stopped"),
            Some(ExitKind::Unwitnessed) | None => return said(r, "ended:exit-unwitnessed"),
        }
    }
    if snap.shell == NO_SHELL {
        return said(r, "refused:not-a-shell-job");
    }
    if !k.alive(snap.shell) {
        return said(r, "refused:shell-gone");
    }
    let Some(conversation) = snap.session.clone().filter(|c| ag::is_conversation_uuid(c)) else {
        return said(r, "ended:no-conversation");
    };
    let lock = run
        .lock
        .clone()
        .unwrap_or_else(|| ag::presence_lock(&run.config, &conversation));
    if !lock.is_file() {
        return said(r, "ended:conversation-gone");
    }
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
        agent: Agent::Agy,
        thread: conversation,
        agy_config: run.config.to_string_lossy().into_owned(),
        agy_env: ag::env_words(&run.env),
        argv: snap.argv.clone(),
        cwd: snap.cwd.clone(),
        twin: program.to_string_lossy().into_owned(),
        pid: snap.pid,
        shell: snap.shell,
        proc_start: snap.start.clone(),
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
            "the agy left its tab ({}); relaunched on its conversation {}",
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

/// [`super::expired`] for an agy relaunch.
fn expired(st: &St, now: u64, k: &dyn AgyKernel) -> Option<(&'static str, String)> {
    match st.phase {
        Phase::Exiting { at_s }
            if now.saturating_sub(exit_clock(at_s, st.exited_at, st.hand_seen_s)) > STALE_S =>
        {
            Some((
                "stale-exit",
                super::super::relaunch::stale_exit_line(st, "agy", now),
            ))
        }
        Phase::Exiting { .. } if st.shell == 0 || !k.alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran agy is gone: there is no prompt to relaunch at".to_owned(),
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "no-resume",
            "the relaunched agy never came up in the tab".to_owned(),
        )),
        _ => None,
    }
}

/// The presence lock of the conversation `st` relaunches, under the config it
/// records.
fn st_lock(st: &St) -> PathBuf {
    ag::presence_lock(Path::new(&st.agy_config), &st.thread)
}

/// Whether the conversation `st` relaunches is free.
fn conversation_free(st: &St, k: &dyn AgyKernel) -> bool {
    free_at(Path::new(&st.agy_config), &st_lock(st), &st.thread, k)
}

/// [`conversation_free`] of the conversation `conversation` under the config
/// `config`, whose presence lock is `lock`.
fn free_at(config: &Path, lock: &Path, conversation: &str, k: &dyn AgyKernel) -> bool {
    k.holders_of(lock).is_some_and(|h| h.is_empty())
        && k.agy_resuming(conversation).is_some_and(|p| p.is_empty())
        && k.lockless_openers(config, conversation)
            .is_some_and(|p| p.is_empty())
}

/// Plan the relaunch from what the record keeps.
pub(super) fn plan(opts: &Opts, st: &St, k: &dyn AgyKernel) -> Result<Plan, NoPlan> {
    // No shell ([`NO_SHELL`], a tab's own agy) is refused for good, as the
    // Claude and Muse Code lanes' plans refuse it: nothing would read the
    // line.
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
    let cd = (!want.is_empty()
        && !super::super::relaunch::same_dir(k.cwd(st.shell).as_deref(), &want))
    .then_some(want);
    let flags = ag::rewrite_argv(&st.argv, Some(&st.thread)).map_err(|e| NoPlan::Refused {
        why: format!("argv:{e}"),
        what: e.to_string(),
        detail: "the launch flags cannot be carried into a relaunch".to_string(),
    })?;
    let env = ag::env_of_words(&st.agy_env);
    let line = upgrade::relaunch_line_env(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        &ag::line_env(&env),
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
        tap_dropped: None,
        line_dir: None,
    })
}

/// Relaunch the agy `st` records at its tab's shell, the harness's hand on
/// the tab through every wait and until the new TUI's first idle.
fn relaunch(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn AgyKernel) -> Report {
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
    k: &dyn AgyKernel,
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
    if !st_lock(st).is_file() {
        return fail(
            opts,
            r,
            st,
            "conversation-gone",
            "the conversation's presence lock is gone from its config: agy would start afresh",
        );
    }
    if !conversation_free(st, k) {
        return said(r, "wait:conversation-held");
    }
    let (line, dialect) = match plan(opts, st, k) {
        Ok(p) => (p.line, p.dialect),
        Err(NoPlan::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(NoPlan::Refused { why, detail, .. }) => return fail(opts, r, st, &why, &detail),
    };
    st.line.clone_from(&line);
    save(opts, &key(&tab), st);
    let block = super::super::relaunch::line_block(c, &tab);
    let (config, lock, conversation) = (
        PathBuf::from(&st.agy_config),
        st_lock(st),
        st.thread.clone(),
    );
    let free = |k: &dyn AgyKernel| free_at(&config, &lock, &conversation, k);
    let typed = type_relaunch_line_with(
        opts,
        c,
        shell,
        &tab,
        &line,
        Some(dialect),
        PromptLook::of(st),
        || free(k),
        |c, pid, tab| k.in_tab(c, pid, tab),
    );
    match typed {
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

/// Record an agy relaunch that stops here, with one ledger line.
fn fail(opts: &Opts, r: Report, st: &mut St, why: &str, detail: &str) -> Report {
    st.fail(why, now_s());
    let r = said(r, format!("failed:{why}"));
    ledger(opts, &r, detail);
    r
}

/// The relaunch line ran and ended with no relaunched agy found leading the tab ([`line_word`]).
fn line_ended(opts: &Opts, r: Report, st: &mut St, kind: ExitKind) -> Report {
    let now = now_s();
    let (step, detail) = match kind {
        ExitKind::Refused => {
            st.fail("resume-refused", now);
            (
                "refused:resume-refused",
                "agy refused the relaunch, or the shell could not run its line",
            )
        }
        ExitKind::Clean | ExitKind::Stopped => {
            st.fail("relaunch-ended", now);
            (
                "ended:relaunch-ended",
                "the relaunched agy ended, or was stopped, by its own or someone's hand before it \
                 was adopted",
            )
        }
        ExitKind::Crashed => {
            st.close_overtaken("exited-before-continuing", now);
            (
                "failed:exited-before-continuing",
                "the relaunched agy crashed before its continuation was typed",
            )
        }
        ExitKind::Unwitnessed => {
            st.fail("relaunch-unwitnessed", now);
            (
                "refused:relaunch-unwitnessed",
                "the relaunched agy ended and no crash file tells how: nothing is typed again",
            )
        }
    };
    let r = said(r, step);
    ledger(opts, &r, &format!("{detail} ({})", st.line.trim()));
    r
}

/// Whether `pid` (argv `argv`) is the relaunch this lane typed: a child of
/// the line's shell, not the exited TUI, an agy TUI resuming the recorded
/// conversation.
fn relaunched_by_us(pid: u32, argv: &[String], st: &St, k: &dyn AgyKernel) -> bool {
    pid != st.pid
        && matches!(st.phase, Phase::Relaunched { .. } | Phase::Exiting { .. })
        && k.parent(pid) == Some(st.shell)
        && ag::is_tui_argv(argv)
        && ag::resumed_conversation(argv).as_deref() == Some(st.thread.as_str())
}

/// After the relaunch line: find the new TUI and carry on with it, or hand it
/// back to its loop ([`adopt`]).
fn await_new(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn AgyKernel) -> Report {
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
    k: &dyn AgyKernel,
) -> Report {
    let mut new = None;
    let mut ended = None;
    let mut looked: Option<std::time::Instant> = None;
    wait_until(opts.timings.agy_found, || {
        hand.keep(c);
        new = k
            .agy_children(st.shell)
            .into_iter()
            .find(|(pid, argv)| relaunched_by_us(*pid, argv, st, k))
            .map(|(pid, _)| pid);
        if let Some(pid) = new {
            found(st, pid, k);
        }
        if new.is_none()
            && looked.is_none_or(|at| {
                wait_now().saturating_duration_since(at) >= opts.timings.shell_look
            })
        {
            looked = Some(wait_now());
            ended = line_word(c, st, k);
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
            // The line's own command ended: its agy never came up, or was
            // gone before it was found.
            if let Some(kind) = ended.or_else(|| line_word(c, st, k)) {
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
                    "the relaunched agy never came up in the tab",
                );
            }
            said(r, "wait:resume")
        }
    }
}

/// The relaunched TUI `new`, on the record whatever becomes of it: once it is
/// gone, an agy leaving the tab is that TUI, its exit its own.
fn found<K: Kernel + ?Sized>(st: &mut St, new: u32, k: &K) {
    if st.relaunched_pid == 0 {
        st.relaunched_start = k.process_start(new).map(|s| squash(&s)).unwrap_or_default();
    } else if st.relaunched_pid != new {
        st.relaunched_start.clear();
    }
    st.relaunched_pid = new;
}

/// The relaunched agy started a fresh conversation and does not hold the conversation's presence lock.
fn started_fresh(opts: &Opts, r: Report, st: &mut St, new: u32) -> Report {
    st.fail("started-fresh", now_s());
    let mut r = said(r, "refused:started-fresh");
    r.pid = new;
    ledger(
        opts,
        &r,
        &format!(
            "the relaunched agy holds no lock on conversation {}: it started a fresh one \
             (`conversation not found` under its config), or another agy has that conversation \
             open; nothing is typed into it",
            st.thread
        ),
    );
    r
}

/// Whether the relaunched agy `new` takes the conversation's presence lock
/// within [`super::super::Timings::agy_lock`]; the hand kept meanwhile.
fn takes_lock(
    opts: &Opts,
    st: &St,
    c: &mut Client,
    new: u32,
    k: &dyn AgyKernel,
    mut hand: Option<&mut Hand>,
) -> bool {
    let lock = st_lock(st);
    wait_until(opts.timings.agy_lock, || {
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        k.holders_of(&lock).is_some_and(|h| h.contains(&new))
    })
}

/// Hand the relaunched agy back to its supervisor (`adopted`).
fn adopt(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn AgyKernel,
) -> Report {
    found(st, new, k);
    let tab = st.tab.clone();
    let mut gone = false;
    let led = wait_until(opts.timings.agy_adopt, || {
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
            word = line_word(c, st, k);
            word.is_some()
        });
        if let Some(kind) = word {
            return line_ended(opts, r, st, kind);
        }
        return said(r, "wait:resume");
    }
    if !takes_lock(opts, st, c, new, k, None) {
        if !k.alive(new) {
            let mut word = None;
            wait_until(opts.timings.ended_at_once, || {
                word = line_word(c, st, k);
                word.is_some()
            });
            if let Some(kind) = word {
                return line_ended(opts, r, st, kind);
            }
            return said(r, "wait:resume");
        }
        return started_fresh(opts, r, st, new);
    }
    st.resumed_pid = new;
    r.pid = new;
    let r = said(r, "adopted");
    ledger(
        opts,
        &r,
        if st.quiet {
            "the relaunched agy leads the tab and holds its conversation; at its first idle \
             point its supervisor confirms it still does, and types nothing"
        } else {
            "the relaunched agy leads the tab and holds its conversation; its supervisor types \
             the continuation at its first idle point"
        },
    );
    r
}

/// An agy relaunch in flight carried on from where it stopped: the relaunch primitive's continuation step for an agy record ([`super::super::relaunch::resume`]), and every other caller's.
pub(in crate::harness) fn carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    carry_in_flight_with(opts, r, st, session, &Live)
}

/// [`carry_in_flight`] over the kernel `k`.
fn carry_in_flight_with(
    opts: &Opts,
    r: Report,
    mut st: St,
    session: &str,
    k: &dyn AgyKernel,
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
    let see_gone = |st: &mut St| -> bool {
        if st.exited_at != 0 || st.pid == 0 || k.alive(st.pid) {
            return false;
        }
        st.exited_at = now_s();
        true
    };
    if hand_waits_out(opts, &mut st, session, |pid| k.alive(pid)) {
        if see_gone(&mut st) {
            save(opts, session, &st);
        }
        return said(r, WAIT_HAND_LIFTS);
    }
    let stale = expired(&st, now_s(), k);
    if see_gone(&mut st) {
        save(opts, session, &st);
    }
    if let Some((why, detail)) = stale {
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

/// Whether agy's composer on `scr` is empty ([`aterm_phase::agy::composer_draft`]).
fn composer_empty(scr: &super::Screen) -> bool {
    aterm_phase::agy::composer_draft(&scr.rows)
        .is_some_and(|(_, lines)| lines.iter().all(|l| l.trim().is_empty()))
}

/// The last look before typing into agy's composer, returning the screen's generation.
fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    let _ = turn_takes_gen(c);
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    match aterm_phase::agy::phase(&scr.rows) {
        (aterm_phase::Phase::Idle | aterm_phase::Phase::Question, true) => {}
        (aterm_phase::Phase::Prompt, _) => return Err("box"),
        _ => return Err("busy"),
    }
    if !composer_empty(&scr) {
        return Err("draft");
    }
    Ok(scr.generation)
}

/// Type `text` into agy's composer as one turn, fenced on `generation`.
fn type_text(c: &mut Client, tab: &str, text: &str, generation: Option<&str>) -> Result<(), Typed> {
    let fence = generation
        .filter(|g| is_generation(g))
        .filter(|_| turn_takes_gen(c))
        .map(|g| format!("if-gen={g} yield=0.2 "))
        .unwrap_or_default();
    let guard = super::composer_guard('>', text);
    let (head, _) = c
        .request_counted(&format!(
            "@{tab} turn {fence}submit=guarded:{guard} idle=600 {TURN_WAIT} {text}"
        ))
        .map_err(|e| Typed::Refused(format!("{e}")))?;
    turn_verdict(&head)
}

/// Carry on with the relaunched TUI `new` once its composer is drawn and it holds its conversation's lock.
#[allow(clippy::too_many_arguments)]
fn carry_on(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn AgyKernel,
    mut hand: Option<&mut Hand>,
) -> Report {
    found(st, new, k);
    r.pid = new;
    let tab = st.tab.clone();
    let up = wait_until(opts.timings.agy_up, || {
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        screen(c, &tab).is_some_and(|s| aterm_phase::agy::composer(&s.rows).is_some())
    });
    let mut typed = false;
    let mut holds = false;
    if up {
        holds = takes_lock(opts, st, c, new, k, hand.as_deref_mut());
        if !holds && k.alive(new) {
            return started_fresh(opts, r, st, new);
        }
        let settled = !st.quiet
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
            let text =
                super::super::relaunch::resumed_prompt_as(Agent::Agy.product(), &st.to, &st.cause);
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
    let rotated = st
        .cause
        .strip_prefix(super::super::relaunch::CAUSE_MODEL)
        .filter(|m| !m.is_empty());
    st.outcome = if let Some(model) = rotated {
        format!(
            "agy moved onto {model} by its quota (on {}) · the same conversation resumed, \
             nothing typed",
            st.to
        )
    } else if st.quiet {
        format!(
            "agy upgraded to {} (from {}) · the same conversation resumed, nothing typed",
            st.to, st.from
        )
    } else {
        format!("agy on {} · the same conversation resumed", st.to)
    };
    let step = if !up {
        "done:unconfirmed"
    } else if typed {
        "continued"
    } else if st.quiet && holds {
        "done:resumed"
    } else if st.quiet {
        "done:unconfirmed"
    } else {
        "done:no-continue"
    };
    let r = said(r, step);
    ledger(opts, &r, &st.outcome);
    r
}

/// Remove agy records nothing will visit again once their tab is proven gone (`life`, [`TabLife::Gone`]).
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
            .and_then(|stem| Agent::Agy.record_tab(stem))
        else {
            continue;
        };
        if !super::sat_a_week(&entry, now) {
            continue;
        }
        let Some(st) = load(opts, &key(tab)) else {
            continue;
        };
        let ours = st.agent == Agent::Agy && (st.tab.is_empty() || st.tab == tab);
        if ours && !st.in_flight() && super::settled(&st) && life(tab) == TabLife::Gone {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The prune of one sweep: every tab the host's roster names is live, and the rendezvous registry speaks for the rest where it may.
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

/// The live agy holders of the `agy-<tab>` records in `sessions`, for the status view ([`super::upgrade_status::Holder`]): the TUI leading the tab and its build.
pub(super) fn holders(
    sessions: &BTreeSet<String>,
    live_tabs: Option<&[LiveTab]>,
) -> Vec<super::upgrade_status::Holder> {
    if !sessions.iter().any(|s| Agent::Agy.record_tab(s).is_some()) {
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
        .filter(|(_, _, name)| name == ag::PROGRAM)
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

#[path = "upgrade_agy_live_drive.rs"]
pub(super) mod live;

#[cfg(all(test, unix))]
#[path = "upgrade_agy_drive_tests.rs"]
mod tests;
