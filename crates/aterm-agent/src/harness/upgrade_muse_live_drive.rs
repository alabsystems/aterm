// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE MUSE CODE LIVE UPGRADE, ITS I/O HALF: the quiet flow's one step ([`upgrade_visit`]) at the idle point the window's host parked its loop at ([`super::super::super::upgrade_muse_live`] is the pure half, and says what was measured).

use std::path::{Path, PathBuf};

use super::super::super::upgrade_muse_live::{self as live, Look, Open};
use super::super::build_start;
use super::super::upgrade::{Agent, Phase};
use super::super::{
    Client, Due, Hand, LiveTab, Opts, Report, St, Terminated, connect, held, load,
    managed_agent_twin, now_s, restore, said, save, save_checked, screen, store_version, terminate,
};
use super::{
    MuseKernel, carry_in_flight_with, composer_empty, key, ledger, mz, plan, relaunch_held,
};

/// The install word a Muse Code build's start note is filed under (`build_start::proved_named`): its own, so a Claude Code build of the same spelling is never another's.
pub(super) const SOURCE: &str = "muse-managed";

/// The wait word of a step at the agent's background break: nothing ends a
/// Muse Code there (the restart is the next idle point's).
const AT_BREAK: &str = "wait:break";

/// The foreground leader of tab `tab` by the host's roster (`tabs`), where it is a Muse Code: its pid.
pub(super) fn leader(tabs: &[LiveTab], tab: &str, k: &dyn MuseKernel) -> Option<u32> {
    let pid = u32::try_from(tabs.iter().find(|t| t.sid == tab)?.fgpgid?).ok()?;
    let named = |p: &Path| p.file_name().and_then(|n| n.to_str()) == Some(mz::PROGRAM);
    let muse = k.exe(pid).is_some_and(|exe| named(&exe))
        || k.args(pid)
            .and_then(|a| a.argv.first().cloned())
            .is_some_and(|argv0| named(Path::new(&argv0)));
    muse.then_some(pid)
}

/// A move owed: the version the running build's record names, the twin's build's, and the twin a relaunch types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Move {
    /// The running build's version (its store record).
    pub(super) running: String,
    /// The twin's build's version (its store record): newer by [`live::moves_onto`].
    pub(super) target: String,
    /// The managed twin (`<prefix>/agents/muse`) the relaunch types.
    pub(super) twin: PathBuf,
}

/// A store build's version as its record names it, only where the record
/// and the build's directory agree ([`live::build_version`]).
fn build_record(exe: &Path) -> Option<String> {
    let version = store_version(exe)?;
    let dir = exe.parent()?.parent()?.file_name()?.to_str()?;
    live::build_version(&version, Some(dir)).map(|_| version)
}

/// THE MOVE `exe` (the running Muse Code's mapped executable) IS OWED onto the build the managed twin `twin` execs: `Ok(Some)` where the twin's build is strictly newer by atpkg's revision-aware order of their store records; `Ok(None)` for a process run from no store build (another install: nothing managed moves it), no twin, or a build not newer; `Err(word)` where a record cannot be read (`no-version`).
pub(super) fn behind(exe: &Path, twin: Option<PathBuf>) -> Result<Option<Move>, &'static str> {
    if !mz::store_build(exe) {
        return Ok(None);
    }
    let Some(twin) = twin else {
        return Ok(None);
    };
    let Some(build) = std::fs::read_to_string(&twin)
        .ok()
        .and_then(|text| super::super::catalog::twin_binary(&text))
    else {
        return Ok(None);
    };
    let running = build_record(exe).ok_or(super::super::UNREAD_VERSION)?;
    let target = build_record(&build).ok_or(super::super::UNREAD_VERSION)?;
    Ok(live::moves_onto(&running, &target).then_some(Move {
        running,
        target,
        twin,
    }))
}

/// [`super::super::due`]'s answer for a tab a Muse Code leads (`None`: not this lane's tab): `Yes` while its build is behind the twin's, `No` when current (or no TUI, or no managed build), `Unread` where its process or a build record cannot be read.
pub(in super::super) fn upgrade_due(opts: &Opts, tabs: &[LiveTab]) -> Option<Due> {
    upgrade_due_with(
        opts,
        tabs,
        managed_agent_twin(mz::PROGRAM),
        &super::super::Live,
    )
}

/// [`upgrade_due`] over the twin `twin` and the kernel `k`.
pub(super) fn upgrade_due_with(
    opts: &Opts,
    tabs: &[LiveTab],
    twin: Option<PathBuf>,
    k: &dyn MuseKernel,
) -> Option<Due> {
    let tab = opts.only_sid.as_deref()?;
    let pid = leader(tabs, tab, k)?;
    let Some(args) = k.args(pid) else {
        return Some(Due::Unread(super::super::UNREAD_PROCESS));
    };
    if !mz::is_tui_argv(&args.argv) {
        return Some(Due::No);
    }
    let Some(exe) = k.exe(pid) else {
        return Some(Due::Unread(super::super::UNREAD_PROCESS));
    };
    Some(match behind(&exe, twin) {
        Ok(Some(_)) => Due::Yes,
        Ok(None) => Due::No,
        Err(word) => Due::Unread(word),
    })
}

/// Whether a Muse Code leads tab `opts.only_sid` ([`super::super::note_behind`]'s lane answer): its lane mints no ladder state (the quiet flow has none), so nothing is noted for it.
pub(in super::super) fn leads(opts: &Opts, tabs: &[LiveTab]) -> bool {
    opts.only_sid
        .as_deref()
        .is_some_and(|tab| leader(tabs, tab, &super::super::Live).is_some())
}

/// The Muse Code lane's share of one sweep ([`super::super::lanes`]): the quiet flow's step for tab `opts.only_sid`, where a Muse Code leads it by the host's own roster.
pub(in super::super) fn upgrade_pass(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let owned;
    let tabs = match live_tabs {
        Some(t) => t,
        None => {
            owned = super::super::instance_tabs(opts);
            &owned
        }
    };
    let twin = managed_agent_twin(mz::PROGRAM);
    if opts.only_sid.is_some() {
        return upgrade_visit(opts, tabs, twin, &super::super::Live)
            .into_iter()
            .collect();
    }
    tabs.iter()
        .filter(|t| {
            leader(tabs, &t.sid, &super::super::Live).is_some()
                || load(opts, &key(&t.sid))
                    .is_some_and(|st| st.agent == Agent::Muse && st.in_flight())
        })
        .filter_map(|t| {
            let mut single = opts.clone();
            single.only_sid = Some(t.sid.clone());
            upgrade_visit(&single, tabs, twin.clone(), &super::super::Live)
        })
        .collect()
}

/// How much of a session's log the look reads from its end: the measured log
/// of a session resumed twenty times was 1.2 MB, and this process's start
/// must be in the window (else [`Open::Unproved`]).
const LOG_BYTES: u64 = 32 << 20;

/// A file's modification time, microseconds since the epoch.
fn modified_us(meta: &std::fs::Metadata) -> Option<u64> {
    let at = meta.modified().ok()?;
    u64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_micros()).ok()
}

/// A second's slack on a file's clock against the log's own.
const CLOCK_SLACK_US: u64 = 1_000_000;

/// G-M1 AND G-M2 OVER THE SESSION'S LOGS (the session's `.session.lock` is `lock`; the process `pid`, started at `started_ms` by the kernel): its own `session.jsonl`, this process's part of it ([`live::quiet_log`]), then every `subagent/<id>/session.jsonl` beside it from that part's start on ([`live::open_work`]).
pub(super) fn log_look(lock: &Path, pid: u32, started_ms: u64) -> Result<(), Open> {
    if started_ms == 0 {
        return Err(Open::Unproved);
    }
    let dir = lock.parent().ok_or(Open::Unreadable)?;
    let log = super::session_log_tail(lock, LOG_BYTES).ok_or(Open::Unreadable)?;
    let since_us = started_ms.saturating_sub(1000).saturating_mul(1000);
    let from_us = live::quiet_log(&log, pid, since_us)?;
    let subagents = match std::fs::read_dir(dir.join("subagent")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(Open::Unreadable),
    };
    for entry in subagents {
        let path = entry.map_err(|_| Open::Unreadable)?.path();
        let older = |meta: &std::fs::Metadata| {
            modified_us(meta).is_some_and(|at| at.saturating_add(CLOCK_SLACK_US) < from_us)
        };
        let sub_log = path.join(mz::LOG);
        match std::fs::metadata(&sub_log) {
            Ok(meta) if older(&meta) => continue,
            Ok(_) => {}
            Err(_) => match std::fs::metadata(&path) {
                Ok(meta) if older(&meta) => continue,
                _ => return Err(Open::Unreadable),
            },
        }
        let text = std::fs::read_to_string(&sub_log).map_err(|_| Open::Unreadable)?;
        live::open_work(&text, from_us)?;
    }
    Ok(())
}

/// G-M1'S SCREEN HALF: whether Muse Code's own reader reads `rows` as the quiet restart's idle point — Idle AUTHORITATIVELY and no wall.
#[must_use]
pub(super) fn screen_idle(rows: &[String]) -> bool {
    aterm_phase::muse::phase(rows) == (aterm_phase::Phase::Idle, true)
        && aterm_phase::muse::wall(rows).is_none()
}

/// THE LOOK FROM WHAT WAS READ: the person's hold, the screen's rows (`None`: unread, which reads neither idle nor empty), its composer's emptiness (asked only of a screen that was read), the session's logs and the process's descendants.
#[must_use]
pub(super) fn look_of(
    held: bool,
    rows: Option<&[String]>,
    composer_empty: impl FnOnce() -> bool,
    log: Result<(), Open>,
    children: Option<usize>,
) -> Look {
    Look {
        held,
        idle: rows.is_some_and(screen_idle),
        composer_empty: rows.is_some() && composer_empty(),
        log,
        children,
    }
}

/// THE LOOK AT MUSE CODE `pid` IN `tab` the quiet flow's gate takes ([`live::gate`]): the hold and the person (`held`, under the grace), the screen by Muse's own reader ([`screen_idle`]) and its composer, the session's logs ([`log_look`]) and its descendants ([`look_of`]).
pub(super) fn look_at(
    opts: &Opts,
    c: &mut Client,
    tab: &str,
    pid: u32,
    lock: &Path,
    started_ms: u64,
    k: &dyn MuseKernel,
) -> Look {
    let held = held(c, tab, opts.human_grace_s);
    let scr = screen(c, tab);
    look_of(
        held,
        scr.as_ref().map(|s| s.rows.as_slice()),
        || scr.as_ref().is_some_and(|s| composer_empty(c, tab, s)),
        log_look(lock, pid, started_ms),
        k.descendants(pid).map(|d| d.len()),
    )
}

/// THE CLAIM (G-M4): the record the quiet restart writes BEFORE its SIGTERM, from the snapshot `snap` of the Muse Code it ends and the move `mv` — the process, its shell and tab, its session and the homes its line carries, its launch's argv and its session's workspace, the twin, the versions, and the UPGRADE'S cause (empty: [`super::super::super::relaunch::ends_its_agent`]), in flight ([`Phase::Exiting`]) and quiet: what makes its exit the upgrade's own (`relaunch::ended_by_restart`) and what the relaunch plans its line from.
pub(super) fn claim_record(
    snap: &super::super::super::relaunch::Snapshot,
    mv: &Move,
    now: u64,
) -> Option<St> {
    let run = snap.muse()?;
    let thread = snap.session.clone().filter(|s| mz::is_session_uuid(s))?;
    Some(St {
        agent: Agent::Muse,
        thread,
        muse_data: run.data.to_string_lossy().into_owned(),
        muse_env: mz::env_words(&run.env),
        argv: snap.argv.clone(),
        cwd: run.workspace.clone().unwrap_or_else(|| snap.cwd.clone()),
        twin: mv.twin.to_string_lossy().into_owned(),
        pid: snap.pid,
        shell: snap.shell,
        tab: snap.tab.clone(),
        from: mv.running.clone(),
        to: mv.target.clone(),
        source: "managed".to_string(),
        cause: String::new(),
        quiet: true,
        salt: now,
        phase: Phase::Exiting { at_s: now },
        ..St::default()
    })
}

/// Whether `lock`'s body still names `pid` (the session's `.session.lock`).
fn lock_names(lock: &Path, pid: u32) -> bool {
    std::fs::read_to_string(lock).is_ok_and(|body| mz::lock_pid(&body) == Some(pid))
}

/// THE QUIET FLOW'S STEP for tab `opts.only_sid` over the host's roster `tabs` (the module's steps 1–6), the twin `twin` and the kernel `k`.
#[allow(clippy::too_many_lines)]
pub(super) fn upgrade_visit(
    opts: &Opts,
    tabs: &[LiveTab],
    twin: Option<PathBuf>,
    k: &dyn MuseKernel,
) -> Option<Report> {
    let tab = opts.only_sid.clone()?;
    let session_key = key(&tab);
    let report = |pid: u32| Report {
        pid,
        tab: tab.clone(),
        session: session_key.clone(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: String::new(),
    };
    if let Some(st) =
        load(opts, &session_key).filter(|st| st.agent == Agent::Muse && st.in_flight())
    {
        let r = report(leader(tabs, &tab, k).unwrap_or(st.pid));
        if opts.background {
            return Some(said(r, AT_BREAK));
        }
        return Some(carry_in_flight_with(opts, r, st, &session_key, k));
    }
    let pid = leader(tabs, &tab, k)?;
    let r = report(pid);
    let Some(args) = k.args(pid) else {
        return Some(said(r, format!("wait:{}", super::super::UNREAD_PROCESS)));
    };
    if !mz::is_tui_argv(&args.argv) {
        return Some(said(r, "current"));
    }
    let Some(exe) = k.exe(pid) else {
        return Some(said(r, format!("wait:{}", super::super::UNREAD_PROCESS)));
    };
    let mv = match behind(&exe, twin) {
        Ok(Some(mv)) => mv,
        Ok(None) => return Some(said(r, "current")),
        Err(word) => return Some(said(r, format!("wait:{word}"))),
    };
    let r = Report {
        from: mv.running.clone(),
        to: format!("{}(managed)", mv.target),
        ..r
    };
    if opts.background {
        return Some(said(r, AT_BREAK));
    }
    let shell = match super::super::foreground_shell(k.job(pid)) {
        Ok(shell) => shell,
        Err("not-a-shell-job") => return Some(said(r, "skip:not-a-shell-job")),
        Err(why) => return Some(said(r, format!("wait:{why}"))),
    };
    let Some(start) = k.process_start(pid).map(|s| super::super::squash(&s)) else {
        return Some(said(r, format!("wait:{}", super::super::UNREAD_PROCESS)));
    };
    let snap = match super::snapshot(opts, &tab, pid, start.clone(), shell, &args, k) {
        Ok(snap) => snap,
        Err(why) => return Some(said(r, format!("wait:{why}"))),
    };
    let Some(run) = snap.muse() else {
        return Some(said(r, "wait:not-muse"));
    };
    let (Some(lock), Some(_)) = (run.lock.clone(), snap.session.as_deref()) else {
        return Some(said(r, "wait:no-session"));
    };
    let started_ms = run.started_ms;
    let Ok(mut c) = connect(opts, &tab) else {
        return Some(said(r, "wait:no-socket"));
    };
    if let Err(why) = live::gate(&look_at(opts, &mut c, &tab, pid, &lock, started_ms, k)) {
        return Some(said(r, format!("wait:{why}")));
    }
    let now = now_s();
    let Some(mut st) = claim_record(&snap, &mv, now) else {
        return Some(said(r, "wait:no-session"));
    };
    match plan(opts, &st, k) {
        Ok(_) => {}
        Err(super::NoPlan::Wait(why)) => return Some(said(r, format!("wait:{why}"))),
        Err(super::NoPlan::Refused { why, .. }) => return Some(said(r, format!("skip:{why}"))),
    }
    if let Err(why) = build_start::proved_named(opts, &r, &mv.twin, &mv.target, SOURCE, now) {
        return Some(said(r, format!("wait:{why}")));
    }
    if opts.dry_run {
        return Some(said(r, "would-restart"));
    }
    let Some(mut hand) = Hand::take(&mut c, &tab) else {
        return Some(said(r, "wait:held"));
    };
    let prior = load(opts, &session_key);
    if save_checked(opts, &session_key, &st).is_err() {
        hand.give_back(&mut c);
        return Some(said(r, "wait:claim-unwritten"));
    }
    let still = k.alive(pid)
        && k.process_start(pid)
            .map(|s| super::super::squash(&s))
            .as_deref()
            == Some(start.as_str())
        && lock_names(&lock, pid)
        && super::super::foreground_shell(k.job(pid)) == Ok(shell)
        && k.in_tab(&mut c, pid, &tab)
        && live::gate(&look_at(opts, &mut c, &tab, pid, &lock, started_ms, k)).is_ok();
    if !still {
        restore(opts, &session_key, prior.as_ref());
        hand.give_back(&mut c);
        return Some(said(r, "wait:changed-before-signal"));
    }
    let Ok(signalled) = i32::try_from(pid) else {
        restore(opts, &session_key, prior.as_ref());
        hand.give_back(&mut c);
        return Some(said(r, "wait:pid"));
    };
    match terminate(&mut c, &tab, signalled, opts.human_grace_s) {
        Terminated::Sent => {}
        Terminated::Refused(why) => {
            restore(opts, &session_key, prior.as_ref());
            hand.give_back(&mut c);
            return Some(said(r, format!("wait:signal-{why}")));
        }
        Terminated::Failed => {
            st.fail("signal-refused", now_s());
            save(opts, &session_key, &st);
            hand.give_back(&mut c);
            let r = said(r, "failed:signal-refused");
            ledger(
                opts,
                &r,
                "the kernel refused the SIGTERM: Muse Code runs on, on the build it ran",
            );
            return Some(r);
        }
    }
    ledger(
        opts,
        &said(r.clone(), "terminated"),
        &format!(
            "SIGTERM (idle: nothing asked or typed; its session {} resumed on {} as it was)",
            st.thread, mv.target
        ),
    );
    let r = relaunch_held(opts, r, &mut st, &mut c, &mut hand, k);
    if !matches!(st.phase, Phase::Exiting { .. }) {
        hand.give_back(&mut c);
    }
    save(opts, &session_key, &st);
    Some(r)
}
