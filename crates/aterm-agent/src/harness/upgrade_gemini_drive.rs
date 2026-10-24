// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE GEMINI CLI LANE.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::relaunch::{
    ExitCause, NO_SHELL, NO_SHELL_DETAIL, WAIT_HAND_LIFTS, exit_clock, first_idle, hand_waits_out,
};
use super::super::upgrade_gemini::{self as ug, ExitKind, NoWitness, Precheck};
use super::upgrade::{Agent, Phase};
use super::{
    Client, Hand, Kernel, Live, LiveTab, NoPlan, Opts, Plan, PromptLook, RelaunchLineError, Report,
    STALE_S, St, TURN_WAIT, TabLife, Typed, alive, connect, cwd_of, exe_of, exit_status, held, ids,
    is_generation, ledger, line_status, load, now_s, pause, registry_graph, registry_life, roster,
    running_block, said, save, screen, shell_dialect_of, squash, state_dir, table, turn_takes_gen,
    turn_verdict, type_relaunch_line_with, under_hand, upgrade, wait_now, wait_until,
};
use crate::harness::laps;
use crate::supervise::run::composer_guard_at;

/// The process facts the Gemini CLI lane rests on.
pub(super) trait GeminiKernel: Kernel {
    fn alive(&self, pid: u32) -> bool;
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs>;
    fn exe(&self, pid: u32) -> Option<PathBuf>;
    fn cwd(&self, pid: u32) -> Option<String>;
    fn start_ms(&self, pid: u32) -> Option<u64>;
    fn child_of(&self, pid: u32) -> Option<u32>;
    fn reads_terminal(&self, child: u32) -> bool;
    fn shell_has_terminal(&self, shell: u32) -> bool;
    fn gemini_children(&self, shell: u32) -> Vec<(u32, Vec<String>)>;
    fn project_geminis(&self, root: &str) -> Option<Vec<(u32, Vec<String>, u64)>>;
    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool;
}

fn live_start_ms(pid: u32) -> Option<u64> {
    #[cfg(target_vendor = "apple")]
    {
        let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
        Some(birth.seconds.saturating_mul(1000) + birth.microseconds / 1000)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        super::started_ms(pid).map(|ms| ms.saturating_sub(ug::START_READ_SLACK_MS))
    }
}

impl GeminiKernel for Live {
    fn alive(&self, pid: u32) -> bool {
        alive(pid)
    }

    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        atpkg::caller_shell::process_args(pid)
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        exe_of(pid)
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        cwd_of(pid)
    }

    fn start_ms(&self, pid: u32) -> Option<u64> {
        live_start_ms(pid)
    }

    fn child_of(&self, pid: u32) -> Option<u32> {
        table()
            .into_iter()
            .filter(|(_, ppid, name)| *ppid == pid && name == "node")
            .map(|(child, _, _)| child)
            .find(|child| {
                atpkg::caller_shell::process_args(*child)
                    .is_some_and(|a| a.argv.iter().skip(1).any(|w| ug::is_gemini_script(w)))
            })
    }

    fn reads_terminal(&self, child: u32) -> bool {
        child != 0 && alive(child) && ids(child).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    }

    fn shell_has_terminal(&self, shell: u32) -> bool {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    }

    fn gemini_children(&self, shell: u32) -> Vec<(u32, Vec<String>)> {
        table()
            .into_iter()
            .filter(|(_, ppid, name)| *ppid == shell && name == "node")
            .filter_map(|(pid, _, _)| {
                let argv = atpkg::caller_shell::process_args(pid)?.argv;
                ug::is_gemini_process(exe_of(pid).as_deref(), &argv).then_some((pid, argv))
            })
            .collect()
    }

    fn project_geminis(&self, root: &str) -> Option<Vec<(u32, Vec<String>, u64)>> {
        let procs = table();
        if procs.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for (pid, _, _) in procs.into_iter().filter(|(_, _, name)| name == "node") {
            let Some(args) = atpkg::caller_shell::process_args(pid) else {
                // Gone between the table and its read: it runs nothing now.
                if alive(pid) {
                    return None;
                }
                continue;
            };
            let image = args.argv.first().map(PathBuf::from);
            if !ug::is_gemini_process(image.as_deref(), &args.argv) {
                continue;
            }
            match cwd_of(pid) {
                Some(cwd) if cwd == root => {
                    out.push((pid, args.argv, live_start_ms(pid).unwrap_or(0)));
                }
                Some(_) => {}
                None if alive(pid) => return None,
                None => {}
            }
        }
        Some(out)
    }

    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool {
        super::process_in_tab(c, pid, tab, None, None)
    }
}

const PREFIX: &str = match Agent::Gemini.record_prefix() {
    Some(prefix) => prefix,
    None => panic!("the Gemini CLI lane keys its records by tab"),
};

/// The state key of the Gemini CLI relaunch in `tab` (`gemini-<tab>`, [`Agent::record_key`]).
pub(super) fn key(tab: &str) -> String {
    format!("{PREFIX}{tab}")
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// THE PROJECT DIRECTORY OF `root` under the Gemini directory `gemini` (`<gemini>/tmp/<slug>`).
pub(super) fn project_dir(gemini: &Path, root: &str) -> Option<PathBuf> {
    if root.is_empty() {
        return None;
    }
    let tmp = gemini.join(ug::TMP_DIR);
    let owned = |dir: &Path| {
        std::fs::read_to_string(dir.join(ug::PROJECT_ROOT_FILE))
            .is_ok_and(|marker| ug::owns(&marker, root))
    };
    let mapped = std::fs::read_to_string(gemini.join("projects.json"))
        .ok()
        .and_then(|text| aterm_json::from_str::<aterm_json::Value>(&text).ok())
        .and_then(|v| {
            v.get("projects")?
                .get(root)?
                .as_str()
                .filter(|slug| {
                    !slug.is_empty() && !slug.contains('/') && *slug != "." && *slug != ".."
                })
                .map(str::to_owned)
        })
        .map(|slug| tmp.join(slug))
        .filter(|dir| owned(dir));
    if mapped.is_some() {
        return mapped;
    }
    let found: Vec<PathBuf> = std::fs::read_dir(&tmp)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|dir| owned(dir))
        .collect();
    match found.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn mtime_ms(meta: &std::fs::Metadata) -> Option<u64> {
    let at = meta.modified().ok()?;
    u64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
}

type Read = (String, String, Option<u64>);

fn files_since(chats: &Path, since_ms: u64) -> Option<Vec<Read>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(chats).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !ug::is_session_file(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mtime = mtime_ms(&meta);
        if mtime.is_some_and(|at| at.saturating_add(1000) < since_ms) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            return None;
        };
        out.push((name, text, mtime));
    }
    Some(out)
}

fn session_files(chats: &Path, session: &str) -> Option<Vec<Read>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(chats).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !ug::names_session(&name, session) {
            continue;
        }
        let text = if name.ends_with(".jsonl") {
            std::fs::read_to_string(entry.path()).ok()?
        } else {
            String::new()
        };
        out.push((name, text, None));
    }
    Some(out)
}

fn as_files(read: &[Read]) -> Vec<ug::ChatsFile<'_>> {
    read.iter()
        .map(|(name, text, mtime)| ug::ChatsFile {
            name,
            text,
            mtime_ms: *mtime,
        })
        .collect()
}

/// THE PRE-CHECK OF A RESUME OF `session` IN `chats`.
pub(super) fn precheck_at(chats: Option<&Path>, session: &str) -> Precheck {
    let Some(read) = chats.and_then(|c| session_files(c, session)) else {
        return Precheck::Unreadable("chats".to_string());
    };
    ug::precheck(&as_files(&read), session)
}

fn witness_of(
    chats: Option<&Path>,
    root: &str,
    start_ms: u64,
    argv: &[String],
    pid: u32,
    k: &dyn GeminiKernel,
) -> Result<String, NoWitness> {
    let (Some(chats), false, true) = (chats, root.is_empty(), start_ms > 0) else {
        return Err(NoWitness::NoChats);
    };
    let others = k.project_geminis(root);
    laps::lap("gemini.procs");
    let together = match &others {
        Some(others) => others.iter().any(|(other, _, start)| {
            *other != pid && (*start == 0 || start.abs_diff(start_ms) <= ug::START_WINDOW_MS)
        }),
        None => true,
    };
    let read = files_since(chats, start_ms).ok_or(NoWitness::NoChats)?;
    laps::lap("gemini.files");
    let resumed = |session: &str| {
        session_files(chats, session)
            .is_some_and(|files| ug::resumes_account_for_switches(&as_files(&files), session))
    };
    let (files, hash, named) = (
        as_files(&read),
        ug::project_hash(root),
        ug::argv_session(argv),
    );
    let rule = ug::witness(
        &files,
        start_ms,
        &hash,
        named.as_deref(),
        together,
        &resumed,
    );
    laps::lap("gemini.rule");
    rule
}

fn bundle_version(script: Option<&String>) -> Option<String> {
    let resolved = std::fs::canonicalize(script?).ok()?;
    let install = resolved.parent()?.parent()?;
    ug::package_version(&std::fs::read_to_string(install.join("package.json")).ok()?)
}

fn session_free(chats: Option<&Path>, root: &str, session: &str, k: &dyn GeminiKernel) -> bool {
    let Some(others) = k.project_geminis(root) else {
        return false;
    };
    others.iter().all(|(pid, argv, start)| {
        ug::argv_session(argv).as_deref() != Some(session)
            && witness_of(chats, root, *start, argv, *pid, k).as_deref() != Ok(session)
    })
}

/// THE LINE A PERSON IS TOLD for `snap`'s Gemini CLI.
pub(super) fn told(snap: &super::super::relaunch::Snapshot) -> Option<String> {
    let run = snap.gemini()?;
    let session = snap
        .session
        .clone()
        .or_else(|| ug::argv_session(&snap.argv))?;
    told_at(&session, run.chats.as_deref(), &run.env, &snap.cwd)
}

/// [`told`] of `session` in the project `root` whose chats directory is `chats`.
pub(super) fn told_at(
    session: &str,
    chats: Option<&Path>,
    env: &[(String, String)],
    root: &str,
) -> Option<String> {
    matches!(precheck_at(chats, session), Precheck::Safe { .. })
        .then(|| ug::told_line(session, env, root))
        .flatten()
}

fn line_word(c: &mut Client, st: &St, k: &dyn GeminiKernel) -> Option<ExitKind> {
    let status = line_status(c, &st.tab, st.line_block)?;
    if relaunched_lives(st, k) {
        return Some(ExitKind::Stopped);
    }
    Some(ug::line_exit_kind(status))
}

fn relaunched_lives(st: &St, k: &dyn GeminiKernel) -> bool {
    st.relaunched_pid != 0
        && k.alive(st.relaunched_pid)
        && (st.relaunched_start.is_empty()
            || k.process_start(st.relaunched_pid)
                .is_some_and(|s| squash(&s) == st.relaunched_start))
}

fn typed_for_exit(st: &St, snap: &super::super::relaunch::Snapshot) -> bool {
    let started_ms = snap.gemini().map_or(0, |run| run.started_ms);
    st.agent == Agent::Gemini
        && st.pid == snap.pid
        && st.relaunched_at != 0
        && (started_ms == 0
            || st.relaunched_at.saturating_mul(1000).saturating_add(1000) >= started_ms)
}

/// A Gemini CLI TUI's run.
pub(super) fn snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
    k: &dyn GeminiKernel,
) -> Result<super::super::relaunch::Snapshot, &'static str> {
    let exe = k.exe(pid).ok_or("exe-unreadable")?;
    if !ug::is_gemini_process(Some(&exe), &args.argv) {
        return Err("not-gemini");
    }
    if !ug::is_tui_argv(&args.argv) {
        return Err("not-a-tui");
    }
    let home = ug::gemini_dir(
        args.env_var(ug::CLI_HOME_ENV),
        args.env_var(ug::HOME_ENV),
        &opts.home,
    )
    .map(|h| canonical(&h))
    .ok_or("home-unreadable")?;
    let env = ug::relaunch_env(|name| args.env_var(name), &opts.home);
    let cwd = k.cwd(pid).unwrap_or_default();
    let started_ms = k.start_ms(pid).unwrap_or(0);
    laps::lap("gemini.process");
    let chats = project_dir(&home, &cwd).map(|p| ug::chats_dir(&p));
    laps::lap("gemini.project");
    let child = k.child_of(pid).unwrap_or(0);
    laps::lap("gemini.child");
    let witnessed = witness_of(chats.as_deref(), &cwd, started_ms, &args.argv, pid, k);
    let version = bundle_version(args.argv.get(1));
    laps::lap("gemini.version");
    let block = (shell != NO_SHELL)
        .then(|| connect(opts, tab).ok())
        .flatten()
        .and_then(|mut c| running_block(&mut c, tab));
    laps::lap("gemini.block");
    let dialect = (shell != NO_SHELL)
        .then(|| shell_dialect(shell, k))
        .flatten();
    laps::lap("gemini.dialect");
    Ok(super::super::relaunch::Snapshot {
        tab: tab.to_string(),
        pid,
        start,
        shell,
        version,
        program: exe,
        argv: args.argv.clone(),
        session: witnessed.as_ref().ok().cloned(),
        cwd,
        run: super::super::relaunch::AgentRun::Gemini(super::super::relaunch::GeminiRun {
            home,
            chats,
            env,
            started_ms,
            child,
            unwitnessed: witnessed.err(),
            block,
        }),
        dialect,
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    })
}

fn shell_word(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    k: &dyn GeminiKernel,
) -> Option<ExitKind> {
    let run = snap.gemini()?;
    let status = connect(opts, &snap.tab)
        .ok()
        .and_then(|mut c| exit_status(&mut c, &snap.tab, run.block))?;
    Some(if own_restart(snap, status, k) {
        ExitKind::Restarted
    } else {
        ug::exit_kind(status)
    })
}

fn own_restart(snap: &super::super::relaunch::Snapshot, status: i64, k: &dyn GeminiKernel) -> bool {
    let (Some(run), Some(_)) = (snap.gemini(), ug::restart_id(status, &snap.argv)) else {
        return false;
    };
    witness_of(
        run.chats.as_deref(),
        &snap.cwd,
        run.started_ms,
        &snap.argv,
        snap.pid,
        k,
    )
    .is_ok()
}

/// One look at a Gemini CLI that left its tab ([`super::super::relaunch::exit_record`] repeats it).
pub(super) fn look_at_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    k: &dyn GeminiKernel,
) -> super::super::relaunch::ExitLook {
    let running =
        k.alive(snap.pid) && k.process_start(snap.pid).as_deref() == Some(snap.start.as_str());
    let word = if running {
        None
    } else {
        shell_word(opts, snap, k)
    };
    super::super::relaunch::ExitLook {
        running,
        record: None,
        agent: Agent::Gemini,
        crashed: match word {
            Some(ExitKind::Crashed | ExitKind::Restarted) => Some(true),
            Some(ExitKind::Clean | ExitKind::Refused) => Some(false),
            Some(ExitKind::Stopped | ExitKind::Unwitnessed) | None => None,
        },
        refused: word == Some(ExitKind::Refused),
        restarted_itself: word == Some(ExitKind::Restarted),
    }
}

fn shell_dialect(shell: u32, k: &dyn GeminiKernel) -> Option<upgrade::Dialect> {
    shell_dialect_of(shell, k.args(shell))
}

/// Relaunch on exit.
pub(super) fn after_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    _upgrade: bool,
    cause: ExitCause,
    k: &dyn GeminiKernel,
) -> Report {
    after_exit_onto(opts, snap, left, cause, k)
}

/// [`after_exit`] over the kernel `k`: the seam a test drives.
#[allow(clippy::too_many_lines)]
pub(super) fn after_exit_onto(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    cause: ExitCause,
    k: &dyn GeminiKernel,
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
    let Some(run) = snap.gemini() else {
        return said(r, "refused:not-gemini");
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
    let mut typed_on: Option<String> = None;
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
                    "the relaunched Gemini CLI exited before it was adopted".to_owned(),
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
            .and_then(|mut c| line_word(&mut c, &earlier, k));
        let stop = match line {
            Some(ExitKind::Crashed | ExitKind::Restarted) => None,
            Some(ExitKind::Refused) => Some((
                "refused:resume-refused",
                "Gemini CLI refused the relaunch's resume, or the shell could not run its line: \
                 typed again it would refuse again",
            )),
            Some(ExitKind::Clean | ExitKind::Stopped) => Some((
                "ended:relaunch-ended",
                "the relaunched Gemini CLI ended, or was stopped, by its own or someone's hand \
                 before it was adopted",
            )),
            Some(ExitKind::Unwitnessed) | None => Some((
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
        typed_on = Some(earlier.thread.clone()).filter(|s| ug::is_session_uuid(s));
    }
    let word = match left {
        ExitRecord::Restarted if !relaunched_before && cause == ExitCause::Exit => {
            Some(ExitKind::Restarted)
        }
        ExitRecord::Crashed if !relaunched_before && cause == ExitCause::Exit => {
            match shell_word(opts, snap, k) {
                Some(ExitKind::Restarted) => Some(ExitKind::Restarted),
                _ => Some(ExitKind::Crashed),
            }
        }
        ExitRecord::Crashed | ExitRecord::Restarted => Some(ExitKind::Crashed),
        ExitRecord::Removed => Some(ExitKind::Clean),
        ExitRecord::Refused => Some(ExitKind::Refused),
        ExitRecord::Unread | ExitRecord::Survived(_) if cause == ExitCause::Exit => {
            if relaunched_before {
                Some(ExitKind::Crashed)
            } else {
                shell_word(opts, snap, k)
            }
        }
        ExitRecord::Unread | ExitRecord::Survived(_) => None,
    };
    let restarted = word == Some(ExitKind::Restarted);
    if cause == ExitCause::Exit {
        match word {
            Some(ExitKind::Crashed | ExitKind::Restarted) => {}
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
    let witnessed = match typed_on {
        Some(session) => Ok(session),
        None => witness_of(
            run.chats.as_deref(),
            &snap.cwd,
            run.started_ms,
            &snap.argv,
            snap.pid,
            k,
        ),
    };
    let session = match witnessed {
        Ok(session) if snap.session.as_ref().is_none_or(|s| *s == session) => session,
        Ok(_) => {
            return refuse(
                opts,
                r,
                "refused:no-witness",
                "the session its start named moved since its last idle point",
            );
        }
        Err(why) => return refuse(opts, r, "refused:no-witness", why.said()),
    };
    match precheck_at(run.chats.as_deref(), &session) {
        Precheck::Safe { .. } => {}
        Precheck::NoTurns if resumes_turnless(restarted, &snap.argv) => {}
        Precheck::NoTurns => {
            return refuse(
                opts,
                r,
                "refused:no-turn",
                &format!("session {session} took no turn: there is nothing to resume"),
            );
        }
        other => {
            return refuse(
                opts,
                r,
                "refused:history-unsafe",
                &unsafe_said(&session, &other),
            );
        }
    }
    let to = from.clone();
    r.to = format!("{to}(path)");
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
        agent: Agent::Gemini,
        thread: session,
        gemini_chats: run
            .chats
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        gemini_env: ug::env_words(&run.env),
        gemini_child: run.child,
        argv: snap.argv.clone(),
        cwd: snap.cwd.clone(),
        twin: ug::PROGRAM.to_string(),
        pid: snap.pid,
        shell: snap.shell,
        tab: tab.clone(),
        from,
        to,
        source: "path".to_string(),
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
        quiet: restarted,
        ..St::default()
    };
    save(opts, &session_key, &st);
    super::super::relaunch::consume_stamp(opts, snap, cause);
    ledger(
        opts,
        &said(r.clone(), "exited"),
        &format!(
            "the Gemini CLI left its tab ({}); relaunched on its session {}",
            if cause == ExitCause::HostEnded {
                "aterm ended while it ran"
            } else if restarted {
                "its own restart met the session id its first start wrote (exit 42)"
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

fn resumes_turnless(restarted: bool, argv: &[String]) -> bool {
    restarted && !ug::carries_first_prompt(argv)
}

fn unsafe_said(session: &str, check: &Precheck) -> String {
    match check {
        Precheck::Unsafe { pick, missing } => format!(
            "`gemini --resume {session}` would load {pick}, which lacks {missing} turn(s) another \
             file of the session holds (Gemini CLI's own resume would bring it back without them)"
        ),
        Precheck::NoFile => {
            format!(
                "no file of session {session} is one Gemini CLI would resume (it would exit 42)"
            )
        }
        Precheck::Unreadable(what) => {
            format!("what `gemini --resume {session}` would load cannot be read ({what})")
        }
        Precheck::Safe { .. } | Precheck::NoTurns => String::new(),
    }
}

fn refuse(opts: &Opts, r: Report, step: &str, detail: &str) -> Report {
    let r = said(r, step);
    if !opts.dry_run {
        ledger(opts, &r, detail);
    }
    r
}

fn expired(st: &St, now: u64, k: &dyn GeminiKernel) -> Option<(&'static str, String)> {
    match st.phase {
        Phase::Exiting { at_s }
            if now.saturating_sub(exit_clock(at_s, 0, st.hand_seen_s)) > STALE_S =>
        {
            Some((
                "stale-exit",
                super::super::relaunch::stale_exit_line(st, "Gemini CLI", now),
            ))
        }
        Phase::Exiting { .. } if st.shell == 0 || !k.alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran Gemini CLI is gone: there is no prompt to relaunch at".to_owned(),
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "no-resume",
            "the relaunched Gemini CLI never came up in the tab".to_owned(),
        )),
        _ => None,
    }
}

fn st_chats(st: &St) -> Option<PathBuf> {
    (!st.gemini_chats.is_empty()).then(|| PathBuf::from(&st.gemini_chats))
}

/// Plan the relaunch from what the record keeps.
pub(super) fn plan(opts: &Opts, st: &St, k: &dyn GeminiKernel) -> Result<Plan, NoPlan> {
    // No shell (a tab's own Gemini CLI) is refused for good: nothing would read the line.
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
    let flags = ug::rewrite_argv(&st.argv, Some(&st.thread)).map_err(|e| NoPlan::Refused {
        why: format!("argv:{e}"),
        what: e.to_string(),
        detail: "the launch flags cannot be carried into a relaunch".to_string(),
    })?;
    let mut env = ug::env_of_words(&st.gemini_env);
    if let Some(file) =
        ug::trusted_folders_of_words(&st.gemini_env).filter(|f| Path::new(f).is_file())
    {
        env.push((ug::TRUSTED_FOLDERS_ENV.to_string(), file.to_string()));
    }
    let line = upgrade::relaunch_line_env(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        &ug::line_env(&env),
        Path::new(ug::PROGRAM),
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

fn relaunch(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn GeminiKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = relaunch_held(opts, r, st, c, &mut hand, k);
    if !matches!(st.phase, Phase::Exiting { .. }) {
        hand.give_back(c);
    }
    r
}

/// [`relaunch`] with the harness's `hand` held.
#[allow(clippy::too_many_lines)]
fn relaunch_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn GeminiKernel,
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
    let child = st.gemini_child;
    if !wait_until(opts.timings.prompt_back, || !k.reads_terminal(child)) {
        return said(r, "wait:orphan-reads");
    }
    pause(opts.timings.prompt_drawn);
    let tab = st.tab.clone();
    let chats = st_chats(st);
    if !session_free(chats.as_deref(), &st.cwd, &st.thread, k) {
        return said(r, "wait:session-held");
    }
    match precheck_at(chats.as_deref(), &st.thread) {
        Precheck::Safe { .. } => {}
        Precheck::NoTurns if resumes_turnless(st.quiet, &st.argv) => {}
        Precheck::NoTurns => {
            return fail(
                opts,
                r,
                st,
                "no-turn",
                &format!(
                    "session {} took no turn: there is nothing to resume",
                    st.thread
                ),
            );
        }
        other => {
            let detail = unsafe_said(&st.thread, &other);
            return fail(opts, r, st, "history-unsafe", &detail);
        }
    }
    let (line, dialect) = match plan(opts, st, k) {
        Ok(p) => (p.line, p.dialect),
        Err(NoPlan::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(NoPlan::Refused { why, detail, .. }) => return fail(opts, r, st, &why, &detail),
    };
    st.line.clone_from(&line);
    save(opts, &key(&tab), st);
    let block = super::super::relaunch::line_block(c, &tab);
    let (root, session) = (st.cwd.clone(), st.thread.clone());
    let free = |k: &dyn GeminiKernel| {
        !k.reads_terminal(child) && session_free(chats.as_deref(), &root, &session, k)
    };
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

fn fail(opts: &Opts, r: Report, st: &mut St, why: &str, detail: &str) -> Report {
    st.fail(why, now_s());
    let step = match why {
        "no-turn" | "history-unsafe" => format!("refused:{why}"),
        _ => format!("failed:{why}"),
    };
    let r = said(r, step);
    ledger(opts, &r, detail);
    r
}

fn line_ended(opts: &Opts, r: Report, st: &mut St, kind: ExitKind) -> Report {
    let now = now_s();
    let (step, detail) = match kind {
        ExitKind::Refused => {
            st.fail("resume-refused", now);
            (
                "refused:resume-refused",
                "Gemini CLI refused the relaunch's resume, or the shell could not run its line",
            )
        }
        ExitKind::Clean | ExitKind::Stopped => {
            st.fail("relaunch-ended", now);
            (
                "ended:relaunch-ended",
                "the relaunched Gemini CLI ended, or was stopped, by its own or someone's hand \
                 before it was adopted",
            )
        }
        ExitKind::Crashed | ExitKind::Restarted => {
            st.close_overtaken("exited-before-continuing", now);
            (
                "failed:exited-before-continuing",
                "the relaunched Gemini CLI crashed before it was adopted",
            )
        }
        ExitKind::Unwitnessed => {
            st.fail("relaunch-unwitnessed", now);
            (
                "refused:relaunch-unwitnessed",
                "the relaunched Gemini CLI ended and its status tells nothing: nothing is typed \
                 again",
            )
        }
    };
    let r = said(r, step);
    ledger(opts, &r, &format!("{detail} ({})", st.line.trim()));
    r
}

fn relaunched_by_us(pid: u32, argv: &[String], st: &St, k: &dyn GeminiKernel) -> bool {
    pid != st.pid
        && matches!(st.phase, Phase::Relaunched { .. } | Phase::Exiting { .. })
        && k.parent(pid) == Some(st.shell)
        && ug::is_tui_argv(argv)
        && ug::argv_session(argv).as_deref() == Some(st.thread.as_str())
}

fn await_new(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn GeminiKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = await_held(opts, r, st, c, &mut hand, k);
    hand.give_back(c);
    r
}

fn await_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn GeminiKernel,
) -> Report {
    let mut new = None;
    let mut ended = None;
    let mut looked: Option<std::time::Instant> = None;
    wait_until(opts.timings.gemini_found, || {
        hand.keep(c);
        new = k
            .gemini_children(st.shell)
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
                    "the relaunched Gemini CLI never came up in the tab",
                );
            }
            said(r, "wait:resume")
        }
    }
}

fn found<K: Kernel + ?Sized>(st: &mut St, new: u32, k: &K) {
    if st.relaunched_pid == 0 {
        st.relaunched_start = k.process_start(new).map(|s| squash(&s)).unwrap_or_default();
    } else if st.relaunched_pid != new {
        st.relaunched_start.clear();
    }
    st.relaunched_pid = new;
}

fn new_witness(
    opts: &Opts,
    st: &St,
    c: &mut Client,
    new: u32,
    k: &dyn GeminiKernel,
    mut hand: Option<&mut Hand>,
) -> Option<Result<String, NoWitness>> {
    let chats = st_chats(st);
    let start = k.start_ms(new).unwrap_or(0);
    let mut seen = None;
    wait_until(opts.timings.gemini_witness, || {
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        let mut now = witness_of(chats.as_deref(), &st.cwd, start, &[], new, k);
        if now == Err(NoWitness::LateHeader) {
            let line = [
                String::new(),
                String::new(),
                "--resume".to_string(),
                st.thread.clone(),
            ];
            if let Ok(session) = witness_of(chats.as_deref(), &st.cwd, start, &line, new, k) {
                now = Ok(session);
            }
        }
        let settled = !matches!(now, Err(NoWitness::NoHeader));
        seen = Some(now);
        settled || !k.alive(new)
    });
    seen.filter(|_| k.alive(new))
}

fn started_other(opts: &Opts, r: Report, st: &mut St, new: u32, other: &str) -> Report {
    st.fail("started-other", now_s());
    let mut r = said(r, "refused:started-other");
    r.pid = new;
    ledger(
        opts,
        &r,
        &format!(
            "the relaunched Gemini CLI runs session {other}, not {}: aterm brought back another \
             session",
            st.thread
        ),
    );
    r
}

fn adopt(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn GeminiKernel,
) -> Report {
    found(st, new, k);
    let tab = st.tab.clone();
    let mut gone = false;
    let led = wait_until(opts.timings.gemini_adopt, || {
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
    match new_witness(opts, st, c, new, k, None) {
        Some(Ok(session)) if session == st.thread => {}
        Some(Ok(other)) => return started_other(opts, r, st, new, &other),
        Some(Err(_)) => {
            ledger(
                opts,
                &said(r.clone(), "adopt-unwitnessed"),
                "the start-time rule names no session for the relaunched Gemini CLI; its line \
                 resumed this session by id, so it is adopted, and no continuation is typed \
                 unless the rule names the session by then",
            );
        }
        None => {
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
    }
    st.resumed_pid = new;
    r.pid = new;
    let r = said(r, "adopted");
    ledger(
        opts,
        &r,
        "the relaunched Gemini CLI leads the tab on its session; its supervisor types the \
         continuation at its first idle point",
    );
    r
}

/// A Gemini CLI relaunch in flight carried on from where it stopped.
pub(in crate::harness) fn carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    carry_in_flight_with(opts, r, st, session, &Live)
}

fn carry_in_flight_with(
    opts: &Opts,
    r: Report,
    mut st: St,
    session: &str,
    k: &dyn GeminiKernel,
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

fn composer_empty(scr: &super::Screen) -> bool {
    aterm_phase::gemini::composer_draft(&scr.rows)
        .is_some_and(|(_, lines)| lines.iter().all(|l| l.trim().is_empty()))
}

fn typing_fence(c: &mut Client, tab: &str) -> Result<(Option<String>, char), &'static str> {
    let _ = turn_takes_gen(c);
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    match aterm_phase::gemini::phase(&scr.rows) {
        (aterm_phase::Phase::Idle | aterm_phase::Phase::Question, true) => {}
        (aterm_phase::Phase::Prompt, _) => return Err("box"),
        _ => return Err("busy"),
    }
    let Some(caret) = aterm_phase::gemini::caret_on(&scr.rows) else {
        return Err("shell-mode");
    };
    if !composer_empty(&scr) {
        return Err("draft");
    }
    Ok((scr.generation, caret))
}

fn type_text(
    c: &mut Client,
    tab: &str,
    text: &str,
    caret: char,
    generation: Option<&str>,
) -> Result<(), Typed> {
    let fence = generation
        .filter(|g| is_generation(g))
        .filter(|_| turn_takes_gen(c))
        .map(|g| format!("if-gen={g} yield=0.2 "))
        .unwrap_or_default();
    let drawn = aterm_phase::gemini::folded_paste(text);
    let guard = composer_guard_at(caret, CARET_COLUMN, drawn.as_deref().unwrap_or(text));
    let (head, _) = c
        .request_counted(&format!(
            "@{tab} turn {fence}submit=guarded:{guard} idle=600 {TURN_WAIT} {text}"
        ))
        .map_err(|e| Typed::Refused(format!("{e}")))?;
    turn_verdict(&head)
}

const CARET_COLUMN: usize = 1;

/// The relaunched TUI `new` is up.
#[allow(clippy::too_many_arguments)]
fn carry_on(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn GeminiKernel,
    mut hand: Option<&mut Hand>,
) -> Report {
    found(st, new, k);
    r.pid = new;
    let tab = st.tab.clone();
    let up = wait_until(opts.timings.gemini_up, || {
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        screen(c, &tab).is_some_and(|s| aterm_phase::gemini::composer_draft(&s.rows).is_some())
    });
    let mut typed = false;
    let mut witnessed = false;
    if up {
        witnessed = match new_witness(opts, st, c, new, k, hand.as_deref_mut()) {
            Some(Ok(other)) if other != st.thread => {
                return started_other(opts, r, st, new, &other);
            }
            Some(Ok(_)) => true,
            Some(Err(_)) | None => false,
        };
        let settled = !st.quiet
            && witnessed
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
            let text = super::super::relaunch::resumed_prompt_as(
                Agent::Gemini.product(),
                &st.to,
                &st.cause,
            );
            let tries = under_hand(c, &tab, hand, |c| {
                for _ in 0..3 {
                    let Ok((generation, caret)) = typing_fence(c, &tab) else {
                        break;
                    };
                    match type_text(c, &tab, &text, caret, generation.as_deref()) {
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
    st.outcome = if up && !witnessed && !st.quiet {
        format!(
            "gemini on {} · its session resumed by id; no continuation typed: no session file \
             names the session for the relaunched Gemini CLI",
            st.to
        )
    } else {
        format!("gemini on {} · the same session resumed", st.to)
    };
    let step = if !up {
        "done:unconfirmed"
    } else if typed {
        "continued"
    } else if st.quiet {
        "done:resumed"
    } else {
        "done:no-continue"
    };
    let r = said(r, step);
    ledger(opts, &r, &st.outcome);
    r
}

/// Remove Gemini CLI records nothing will visit again.
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
            .and_then(|stem| Agent::Gemini.record_tab(stem))
        else {
            continue;
        };
        if !super::sat_a_week(&entry, now) {
            continue;
        }
        let Some(st) = load(opts, &key(tab)) else {
            continue;
        };
        let ours = st.agent == Agent::Gemini && (st.tab.is_empty() || st.tab == tab);
        if ours && !st.in_flight() && super::settled(&st) && life(tab) == TabLife::Gone {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The prune of one sweep.
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

fn prune_known(opts: &Opts, known_live: Vec<String>) {
    let graph = registry_graph();
    prune(opts, now_s(), registry_life(graph.as_deref(), &known_live));
}

/// The live Gemini CLI holders of the `gemini-<tab>` records in `sessions`.
pub(super) fn holders(
    sessions: &BTreeSet<String>,
    live_tabs: Option<&[LiveTab]>,
) -> Vec<super::upgrade_status::Holder> {
    if !sessions
        .iter()
        .any(|s| Agent::Gemini.record_tab(s).is_some())
    {
        return Vec::new();
    }
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
    table()
        .iter()
        .filter(|(_, _, name)| name == "node")
        .filter_map(|(pid, _, _)| {
            let argv = atpkg::caller_shell::process_args(*pid)?.argv;
            if !ug::is_gemini_process(exe_of(*pid).as_deref(), &argv) {
                return None;
            }
            let tab = tab_of(*pid)?;
            let session = key(&tab);
            sessions
                .contains(&session)
                .then(|| super::upgrade_status::Holder {
                    session,
                    version: bundle_version(argv.get(1)).unwrap_or_default(),
                    tab: Some(tab),
                    ..Default::default()
                })
        })
        .collect()
}

#[cfg(all(test, unix))]
#[path = "upgrade_gemini_drive_tests.rs"]
mod tests;
