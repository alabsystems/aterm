// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE GEMINI CLI LANE, ITS I/O HALF ([`super::super::upgrade_gemini`] is the
//! pure half): a Gemini CLI that crashed out of its tab is relaunched there on
//! its own session, in its own project, and one aterm's own end took is
//! brought back in its reopened tab. Relaunch on exit only: no live upgrade
//! moves a Gemini CLI (`lanes().upgrade` is false for it: a Homebrew or npm
//! install, no atpkg row), so no sweep visits it, and nothing is ever typed
//! into a Gemini CLI that runs but its relaunch's continuation.
//!
//! 1. **The snapshot** ([`snapshot`]): the tab's foreground Gemini CLI parent
//!    (`node <gemini script>`, a TUI), its Gemini home (`GEMINI_CLI_HOME` or
//!    `HOME`), its project (its working directory) and that project's chats
//!    directory ([`project_dir`]), its start to the millisecond, its child,
//!    the shell-integration block it runs in, and the session the start-time
//!    rule names ([`witness_of`]: the one session header of its project begun
//!    since it started; none on anything else). The rule is read again at the
//!    exit, so a `/clear` or an in-TUI `/resume` since the last idle point
//!    leaves no witness.
//! 2. **The exit** ([`look_at_exit`], [`after_exit`]): the shell's exit
//!    status of the block it ran in, by Gemini CLI's own table
//!    (`upgrade_gemini::exit_kind`: `0`/`130` clean, `1`/`137` a crash, `41`,
//!    `42`, `52`… a refused start); with no word, nothing is relaunched. A
//!    worker's `42` that is ITS OWN RESTART meeting the `--session-id` its
//!    first start wrote ([`own_restart`]) is resumed on that session as a
//!    crash is, quietly: nothing typed after it — even with no turn taken,
//!    for a launch with no first prompt ([`resumes_turnless`], measured: a
//!    folder-trust restart).
//! 3. **The relaunch** ([`relaunch`]): once the parent is gone and its shell
//!    holds the terminal again — a parent SIGKILLed leaves its child orphaned
//!    in the old group, reading no terminal its shell holds (its reads fail),
//!    and it ends at the first byte typed (measured) — once no other live
//!    Gemini CLI of the project may run the session, and once the read-only
//!    pre-check (`upgrade_gemini::precheck`) says what `--resume` would load
//!    holds every turn, `cd <project>` and `gemini` (the shell's own)
//!    `--resume <uuid>` after the launch's carried flags, behind the homes the
//!    process ran under, typed at the tab's shell by the relaunch primitive's
//!    own typing
//!    (`type_relaunch_line_with`). The new process is found as the shell's
//!    child resuming that session ([`relaunched_by_us`]) and adopted once the
//!    start-time rule names that session for it ([`adopt`]). A line whose own
//!    command ends with no Gemini CLI found leading the tab is decided by its
//!    block's word at once ([`line_word`], [`line_ended`]), and so is every
//!    later attempt at the same exit ([`after_exit_onto`]): a resume the CLI
//!    refused (`42`), or a line the shell could not run, is never typed again.
//! 4. **The continuation** ([`carry_on`]): typed at the loop's first idle
//!    point, never as `-i` on the line (which sends it at once: a task done
//!    under Land, which every lane adopts with nothing typed, would get it);
//!    once the start-time rule names the session for the new process and its
//!    composer reads empty and idle, by a guarded `turn` under its caret.
//! 5. **What it never types**: a session the rule cannot name, one that took
//!    no turn (but its own bare restart's), or one whose resume would load a
//!    copy missing turns — the
//!    person is told (`refused:…`, the host's `Cannot`) with Gemini CLI's own
//!    line where it is safe to run ([`told`]), and what aterm could not tell
//!    where it is not.
//!
//! Records are filed per tab (`<state>/upgrade/gemini-<tab>.json`,
//! [`upgrade::Agent::record_key`]), pruned as codex-alab's path prunes its own
//! ([`prune`]), and their holders read for the owner's view ([`holders`]).

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
use crate::supervise::run::composer_guard_at;

// ---------------------------------------------------------------- the kernel

/// The process facts the Gemini CLI lane rests on, each read when asked: the
/// Claude lane's [`Kernel`] plus Gemini CLI's needs. A trait so a driver test
/// can script a Gemini CLI that exits and a relaunch that appears on its line.
pub(super) trait GeminiKernel: Kernel {
    /// Whether `pid` lives.
    fn alive(&self, pid: u32) -> bool;
    /// `pid`'s argv and initial environment (`KERN_PROCARGS2`).
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs>;
    /// The executable the kernel mapped for `pid`.
    fn exe(&self, pid: u32) -> Option<PathBuf>;
    /// `pid`'s working directory, as the kernel names it (the path a node
    /// process's `process.cwd()` returns: its project).
    fn cwd(&self, pid: u32) -> Option<String>;
    /// When `pid` started, unix milliseconds, never later than it did (the
    /// kernel's microsecond birth where it keeps one; else a second's read,
    /// less its slack). `None` where it cannot be read.
    fn start_ms(&self, pid: u32) -> Option<u64>;
    /// The Gemini CLI child `pid` started (`node --max-old-space-size=… <the
    /// script>`), where one lives.
    fn child_of(&self, pid: u32) -> Option<u32>;
    /// Whether the orphaned child `child` of a killed parent still reads the
    /// terminal: alive and in the terminal's foreground group.
    fn reads_terminal(&self, child: u32) -> bool;
    /// Whether `shell`'s group holds its terminal again (`pgid == tpgid`).
    fn shell_has_terminal(&self, shell: u32) -> bool;
    /// The live Gemini CLI parents whose parent is `shell`, with their argv.
    fn gemini_children(&self, shell: u32) -> Vec<(u32, Vec<String>)>;
    /// The live Gemini CLI parents ([`ug::is_gemini_process`]) whose working
    /// directory is `root`, each with its argv and its start
    /// ([`Self::start_ms`]; `0` unread). `None` where the process table, or a
    /// live one's argv or directory, cannot be read.
    fn project_geminis(&self, root: &str) -> Option<Vec<(u32, Vec<String>, u64)>>;
    /// Whether `pid` is still the foreground group of `tab` by the host's own
    /// roster: asked right before every typed act.
    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool;
}

/// `pid`'s start as [`GeminiKernel::start_ms`] reads it on this host.
fn live_start_ms(pid: u32) -> Option<u64> {
    #[cfg(target_vendor = "apple")]
    {
        let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
        Some(birth.seconds.saturating_mul(1000) + birth.microseconds / 1000)
    }
    // A start read to the second (`ps`'s elapsed time) may name a moment up
    // to a second AFTER the real one: two seconds less never does.
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
                // Alive and unreadable: nobody can say.
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

// ---------------------------------------------------------------- the records

/// The Gemini CLI lane's per-tab record prefix ([`Agent::record_prefix`]),
/// `gemini-`: placed at compile time, which fails where Gemini CLI keys none.
const PREFIX: &str = match Agent::Gemini.record_prefix() {
    Some(prefix) => prefix,
    None => panic!("the Gemini CLI lane keys its records by tab"),
};

/// The state key of the Gemini CLI relaunch in `tab` (`gemini-<tab>`,
/// [`Agent::record_key`]).
pub(super) fn key(tab: &str) -> String {
    format!("{PREFIX}{tab}")
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

// ---------------------------------------------------------------- the files

/// THE PROJECT DIRECTORY OF `root` under the Gemini directory `gemini`
/// (`<gemini>/tmp/<slug>`): the slug `projects.json` maps the project to,
/// where that directory's `.project_root` names it, else the one directory
/// under `tmp/` whose `.project_root` does (the CLI's own lookup, bundle:
/// `ProjectRegistry.getShortId`). `None` where none, or two, do.
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

/// When `meta` says its file was last written, unix milliseconds.
fn mtime_ms(meta: &std::fs::Metadata) -> Option<u64> {
    let at = meta.modified().ok()?;
    u64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// One session file read: its name, its words, its mtime.
type Read = (String, String, Option<u64>);

/// THE SESSION FILES OF `chats` WRITTEN SINCE `since_ms` (a second's slack
/// before it): every `session-*.jsonl` whose mtime is no earlier, the only
/// ones that can hold a header, a turn or a switch of a process started then.
/// `None` where the directory cannot be read.
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

/// THE FILES OF `session` IN `chats` (`session-*-<id8>.jsonl`, a legacy
/// `.json` named with no words), for the pre-check and the witness's count of
/// that session's resumes. `None` where the directory, or one of them, cannot
/// be read.
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

/// THE PRE-CHECK OF A RESUME OF `session` IN `chats`, read now
/// ([`ug::precheck`]); unreadable reads as [`Precheck::Unreadable`].
pub(super) fn precheck_at(chats: Option<&Path>, session: &str) -> Precheck {
    let Some(read) = chats.and_then(|c| session_files(c, session)) else {
        return Precheck::Unreadable("chats".to_string());
    };
    ug::precheck(&as_files(&read), session)
}

/// THE SESSION THE PROCESS `pid` (argv `argv`, started at `start_ms`) OF THE
/// PROJECT `root` RUNS, by the start-time rule ([`ug::witness`]) over the
/// project's chats directory `chats`, read now: with another Gemini CLI of
/// the project started within [`ug::START_WINDOW_MS`] of it (or none that can
/// be read) as started together.
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
    let together = match k.project_geminis(root) {
        Some(others) => others.iter().any(|(other, _, start)| {
            *other != pid && (*start == 0 || start.abs_diff(start_ms) <= ug::START_WINDOW_MS)
        }),
        None => true,
    };
    let read = files_since(chats, start_ms).ok_or(NoWitness::NoChats)?;
    // A switch onto another session no stamped write follows, written since
    // it started: read over every file of that session, it is a `--resume`
    // launch's only where its headers outnumber its switches.
    let resumed = |session: &str| {
        session_files(chats, session)
            .is_some_and(|files| ug::resumes_account_for_switches(&as_files(&files), session))
    };
    ug::witness(
        &as_files(&read),
        start_ms,
        &ug::project_hash(root),
        ug::argv_session(argv).as_deref(),
        together,
        &resumed,
    )
}

/// The version of the Gemini CLI install the script `script` (a parent's
/// `argv[1]`) is (`<install>/package.json` beside its `bundle/`, the link
/// resolved); `None` where none is read. Never `--version`, which runs the
/// CLI under its own home.
fn bundle_version(script: Option<&String>) -> Option<String> {
    let resolved = std::fs::canonicalize(script?).ok()?;
    let install = resolved.parent()?.parent()?;
    ug::package_version(&std::fs::read_to_string(install.join("package.json")).ok()?)
}

/// Whether no other live Gemini CLI of the project runs `session`, as far as
/// a read tells: none names it in its argv (a person resumed it in another
/// tab), and the start-time rule names it for none. Two live processes on one
/// session are unmeasured, so nothing is typed while one does
/// (`wait:session-held`); a table that cannot be read holds it too. One the
/// rule names no session for is not read as running it: beside this one's
/// own files, every other Gemini CLI of the project reads as unwitnessed
/// (this one's header is begun since it started), so reading it as a holder
/// would hold every session of a project two Gemini CLIs share for ever. The
/// stated limit: an in-TUI `/resume` onto this session in another tab, made
/// between its crash and its relaunch, is not seen.
fn session_free(chats: Option<&Path>, root: &str, session: &str, k: &dyn GeminiKernel) -> bool {
    let Some(others) = k.project_geminis(root) else {
        return false;
    };
    others.iter().all(|(pid, argv, start)| {
        ug::argv_session(argv).as_deref() != Some(session)
            && witness_of(chats, root, *start, argv, *pid, k).as_deref() != Ok(session)
    })
}

// ---------------------------------------------------------------- the told line

/// THE LINE A PERSON IS TOLD for `snap`'s Gemini CLI, where running it is
/// safe: Gemini CLI's own `gemini --resume <uuid>` in its project, behind its
/// homes ([`ug::told_line`]), for the session its witness named — else the
/// one its argv names — and only where the pre-check, read now, says that
/// resume loads every turn ([`Precheck::Safe`]). `None` otherwise: what aterm
/// could not tell is said instead, never a line that would bring the session
/// back without its turns.
pub(super) fn told(snap: &super::super::relaunch::Snapshot) -> Option<String> {
    let run = snap.gemini()?;
    let session = snap
        .session
        .clone()
        .or_else(|| ug::argv_session(&snap.argv))?;
    told_at(&session, run.chats.as_deref(), &run.env, &snap.cwd)
}

/// [`told`] of `session` in the project `root` whose chats directory is
/// `chats`, behind the homes `env`: the line where the pre-check, read now,
/// says the resume loads every turn.
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

// ---------------------------------------------------------------- the line's word

/// THE WORD OF THE RELAUNCH LINE'S OWN COMMAND (its block `block`,
/// [`St::line_block`], read right before the line was typed), once it ended
/// ([`ug::line_exit_kind`]): the CLI refused the resume (`42`), or the shell
/// could not run the line (refused too), or the relaunched Gemini CLI ended
/// by its own or someone's hand, or crashed. `None` while that command runs,
/// where no block was read before it was typed, or where the tab no longer
/// shows it. A line whose Gemini CLI still lives was stopped
/// ([`ExitKind::Stopped`]): the line `exec`s it, so nothing else ends the
/// block with the process alive.
fn line_word(c: &mut Client, st: &St, k: &dyn GeminiKernel) -> Option<ExitKind> {
    let status = line_status(c, &st.tab, st.line_block)?;
    if relaunched_lives(st, k) {
        return Some(ExitKind::Stopped);
    }
    Some(ug::line_exit_kind(status))
}

/// Whether the Gemini CLI the relaunch line started ([`St::relaunched_pid`])
/// still lives: alive, and the very process found, never another that took
/// its number.
fn relaunched_lives(st: &St, k: &dyn GeminiKernel) -> bool {
    st.relaunched_pid != 0
        && k.alive(st.relaunched_pid)
        && (st.relaunched_start.is_empty()
            || k.process_start(st.relaunched_pid)
                .is_some_and(|s| squash(&s) == st.relaunched_start))
}

/// WHETHER `st` IS THE RECORD OF AN EARLIER ATTEMPT AT THIS VERY EXIT THAT
/// TYPED ITS LINE: the same tab's record, naming `snap`'s process, its line
/// typed after that process started.
fn typed_for_exit(st: &St, snap: &super::super::relaunch::Snapshot) -> bool {
    let started_ms = snap.gemini().map_or(0, |run| run.started_ms);
    st.agent == Agent::Gemini
        && st.pid == snap.pid
        && st.relaunched_at != 0
        && (started_ms == 0
            || st.relaunched_at.saturating_mul(1000).saturating_add(1000) >= started_ms)
}

// ------------------------------------------------------- relaunch on exit

/// A Gemini CLI TUI's run, read while it runs, for relaunch on exit
/// ([`super::super::relaunch::snapshot`] vetted job, tab and terminal): the
/// parent `pid` (`node <script>`), its program and argv, its Gemini home, its
/// project (its working directory) and that project's chats directory, its
/// start and its child, the homes its line carries, the block it runs in, and
/// the session the start-time rule names ([`witness_of`]: `None` where none
/// is, with why).
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
    let chats = project_dir(&home, &cwd).map(|p| ug::chats_dir(&p));
    let witnessed = witness_of(chats.as_deref(), &cwd, started_ms, &args.argv, pid, k);
    Ok(super::super::relaunch::Snapshot {
        tab: tab.to_string(),
        pid,
        start,
        shell,
        version: bundle_version(args.argv.get(1)),
        program: exe,
        argv: args.argv.clone(),
        session: witnessed.as_ref().ok().cloned(),
        cwd,
        run: super::super::relaunch::AgentRun::Gemini(super::super::relaunch::GeminiRun {
            home,
            chats,
            env,
            started_ms,
            child: k.child_of(pid).unwrap_or(0),
            unwitnessed: witnessed.err(),
            // A tab's own Gemini CLI ([`NO_SHELL`]) runs in no shell's block,
            // and no shell writes it a line: read for its readers, relaunched
            // by nothing ([`after_exit_onto`], [`plan`]).
            block: (shell != NO_SHELL)
                .then(|| connect(opts, tab).ok())
                .flatten()
                .and_then(|mut c| running_block(&mut c, tab)),
        }),
        dialect: (shell != NO_SHELL)
            .then(|| shell_dialect(shell, k))
            .flatten(),
        // Claude Code's variables: a Gemini CLI has no such login or home,
        // and no usage tap.
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    })
}

/// THE WORD THE EXIT OF `snap`'S GEMINI CLI LEFT: its shell's exit status of
/// the block it ran in ([`super::super::relaunch::GeminiRun::block`]), read by
/// its table ([`ug::exit_kind`]) — but for its own restart met by its own
/// session id ([`own_restart`]: [`ExitKind::Restarted`]); `None` while no
/// status is drawn.
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

/// WHETHER `status` IS `snap`'S OWN RESTART REFUSED FOR ITS OWN SESSION ID:
/// a launch under `--session-id <uuid>` that exited `42` ([`ug::restart_id`])
/// where the start-time rule, read now, names a session for the process
/// ([`witness_of`]) — which is that very id: the rule names one only where
/// the session begun since the start is the one the argv names
/// ([`ug::argv_session`], which with no `--resume`/`-r` is the
/// `--session-id` `restart_id` read), so no other comparison is owed. Its
/// first start wrote it, and the `42` was a later run of the same argv:
/// Gemini CLI's own restart (`/settings`, IDE trust, admin settings), which a
/// worker's line meets rather than send its task twice (`harness::worker`).
/// A launch whose id was written before it started, one whose session begun
/// since is another than its id, or one the rule names nothing for, was
/// refused at its start.
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

/// One look at a Gemini CLI that left its tab
/// ([`super::super::relaunch::exit_record`] repeats it): whether the parent
/// still runs (stopped, it is no exit yet) and, once gone, its shell's word
/// by its table: a crash, its own or someone's end, or a start it refused. A
/// stopped or unwitnessed end decides nothing here (read again at the
/// attempt). No file is a witness of the end: none is held or left.
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
        // Its own restart refused for its own id is owed a relaunch as a
        // crash is ([`ExitRecord::crashed`]: the host owes it through the
        // back-off, a handoff carries it); the attempt tells the two apart
        // ([`after_exit_onto`]).
        crashed: match word {
            Some(ExitKind::Crashed | ExitKind::Restarted) => Some(true),
            Some(ExitKind::Clean | ExitKind::Refused) => Some(false),
            Some(ExitKind::Stopped | ExitKind::Unwitnessed) | None => None,
        },
        refused: word == Some(ExitKind::Refused),
        restarted_itself: word == Some(ExitKind::Restarted),
    }
}

/// The dialect of `shell` from its executable, as the relaunch line is
/// written for it ([`shell_dialect_of`]).
fn shell_dialect(shell: u32, k: &dyn GeminiKernel) -> Option<upgrade::Dialect> {
    shell_dialect_of(shell, k.args(shell))
}

/// Relaunch on exit, Gemini CLI lane ([`super::super::relaunch::after_exit`]
/// hands a Gemini CLI snapshot here): its TUI `snap` left its tab and nobody
/// owns the exit. Its shell's word decides: its own end or someone's is
/// theirs (`ended:graceful-exit`), a start it refused is never retried
/// (`ended:refused-start`), a stopped one is no exit yet, no word relaunches
/// nothing (`ended:exit-unwitnessed`) — unless aterm itself ended
/// ([`ExitCause::HostEnded`]). A crash is relaunched via [`relaunch`] on the
/// session the start-time rule names, read again now; a session it cannot
/// name (`refused:no-witness`), one that took no turn (`refused:no-turn`), or
/// one whose resume would load a copy missing turns
/// (`refused:history-unsafe`) is told, never typed. `_upgrade` moves nothing:
/// Gemini CLI has no upgrade lane.
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
        // Stopped, alive: no exit yet, never a crash.
        return said(r, "wait:not-exited");
    }
    prune_known(opts, vec![snap.tab.clone()]);
    let mut relaunched_before = false;
    // The session an earlier attempt at this very exit witnessed and typed
    // its line for: its relaunch's own resume wrote a header since this
    // process started, so the rule read again would name none.
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
    // AN EXIT AN EARLIER ATTEMPT ALREADY RELAUNCHED is typed again only on the
    // word of THAT relaunch's own line: its Gemini CLI crashing types it
    // again; a resume the CLI refused (`42`) or a line the shell could not
    // run is never typed again (`refused:resume-refused`); one that ended by
    // its own or someone's hand is theirs (`ended:relaunch-ended`); one whose
    // end cannot be read relaunches nothing more
    // (`refused:relaunch-unwitnessed`).
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
        // ITS OWN RESTART, READ AT ITS EXIT ([`look_at_exit`]'s word, kept
        // as the exit was seen): resumed quietly whatever the shell draws by
        // now — the word is gone once another command's block is the newest,
        // and nothing else says which of the two it was (held to the model's
        // `ARestartTypesNothingAfter` by its tier-1 replay).
        ExitRecord::Restarted if !relaunched_before && cause == ExitCause::Exit => {
            Some(ExitKind::Restarted)
        }
        // A crash read at its exit: the block's word, read again where it is
        // still drawn, tells an own restart a record of an older look
        // conflated; where the word is gone the crash stands, continued
        // after its line. Never a restart on a guess: any `--session-id`
        // launch's `42` would be its own restart ([`own_restart`]), every
        // worker's included, and a crash read so went with no continuation
        // (the model's
        // `ACrashRelaunchedIsContinued`).
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
    // ITS OWN RESTART, refused for its own session id: resumed on that
    // session by id as a crash is, and QUIET — nothing typed after it (the
    // person who restarted it is at the tab; nothing of a turn was cut).
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
    // A tab's own Gemini CLI ([`NO_SHELL`]): no shell is left to type its
    // relaunch into, and none would come back in that tab.
    if snap.shell == NO_SHELL {
        return said(r, "refused:not-a-shell-job");
    }
    if !k.alive(snap.shell) {
        return said(r, "refused:shell-gone");
    }
    // THE SESSION, BY THE START-TIME RULE READ AGAIN NOW (the process is
    // gone, so nothing of its own moves the files): a `/clear` or an in-TUI
    // `/resume` since the snapshot leaves no witness, and no other process's
    // header since its start may be read as its own. Where an earlier
    // attempt at this exit typed its line (its own process crashed), the
    // session that attempt witnessed stands: that resume's header is begun
    // since this process started.
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
    // THE PRE-CHECK: what `--resume` would load must hold every turn — and
    // a session that took none is resumed only for its own bare restart
    // ([`resumes_turnless`]).
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
        // The project: its sessions are its working directory's.
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

/// WHETHER A SESSION THAT TOOK NO TURN IS RESUMED ALL THE SAME
/// ([`Precheck::NoTurns`]): only on its own restart (`restarted`, the
/// record's `quiet`) of a launch that carried no first prompt
/// ([`ug::carries_first_prompt`]). MEASURED on 0.46.0 (lab, 2026-10-21):
/// Gemini CLI writes its session's header at its UI's mount, before any
/// dialog is answered, so its folder-trust dialog's `Trust folder` restarts
/// it onto a session already written, and its `--session-id` is refused
/// (`42`) with no turn in it; `gemini --resume <id>` of that session comes
/// up at its composer on that very session, the next turn recorded into its
/// file — what the restart itself would have come back as. A crash's session
/// that took no turn is left (nothing of it to bring back), and so is a
/// restart's whose first prompt was never sent: the resume would drop it.
/// No restart can be refused before the header is written: with no session
/// file under its id the restarted run starts it afresh and the parent never
/// exits (the bundle's `resolveSessionId`), so no other witness is owed.
fn resumes_turnless(restarted: bool, argv: &[String]) -> bool {
    restarted && !ug::carries_first_prompt(argv)
}

/// A pre-check that is not [`Precheck::Safe`] in a person's words.
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

/// A relaunch refused before any record is made: one ledger line, the step's
/// word (`refused:…`, the host's `Cannot`, which tells the person).
fn refuse(opts: &Opts, r: Report, step: &str, detail: &str) -> Report {
    let r = said(r, step);
    if !opts.dry_run {
        ledger(opts, &r, detail);
    }
    r
}

/// [`super::expired`] for a Gemini CLI relaunch: its exit over [`STALE_S`]
/// ago — or the last hand that held its line, whichever is later — and not
/// relaunched, its shell gone, or relaunched and never come up.
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

/// The chats directory `st` records, `None` for none.
fn st_chats(st: &St) -> Option<PathBuf> {
    (!st.gemini_chats.is_empty()).then(|| PathBuf::from(&st.gemini_chats))
}

/// Plan the relaunch from what the record keeps: the shell's dialect, the
/// heal it needs, the `cd` to the project (where the shell's differs), and
/// the line — `gemini <carried flags> --resume <uuid>` ([`ug::rewrite_argv`];
/// the shell's own `gemini`, never a Cellar path) behind the homes the
/// process ran under ([`ug::line_env`], REQUIRED: a line that cannot hold
/// them is refused).
pub(super) fn plan(opts: &Opts, st: &St, k: &dyn GeminiKernel) -> Result<Plan, NoPlan> {
    // No shell ([`NO_SHELL`], a tab's own Gemini CLI) is refused for good, as
    // the other lanes' plans refuse it: nothing would read the line.
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
    // The homes, and a worker's own trusted-folders file while it stands:
    // without it its relaunch would stop at a folder-trust dialog its launch
    // never drew, which the window harness never presses on a worker
    // (`supervise::policy::approval`). A file gone with its worker's session
    // is named to nothing.
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

/// Relaunch the Gemini CLI `st` records at its tab's shell, the harness's
/// hand on the tab through every wait and until the new TUI's first idle.
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
    // THE ORPHAN: a parent SIGKILLed leaves its child alive in the old group.
    // It reads the terminal only while that group is the foreground one — the
    // moment before the shell takes the terminal back; once the shell holds
    // it, the orphan's reads fail and it ends at the first byte typed, which
    // reaches the shell whole (measured). Nothing is typed while it reads.
    let child = st.gemini_child;
    if !wait_until(opts.timings.prompt_back, || !k.reads_terminal(child)) {
        return said(r, "wait:orphan-reads");
    }
    // The shell draws its prompt after it takes the terminal back.
    pause(opts.timings.prompt_drawn);
    let tab = st.tab.clone();
    let chats = st_chats(st);
    // Nothing is typed while another live Gemini CLI of the project may run
    // the session (two processes on one session are unmeasured).
    if !session_free(chats.as_deref(), &st.cwd, &st.thread, k) {
        return said(r, "wait:session-held");
    }
    // The pre-check again, right before the line: what `--resume` loads now.
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

/// Record a Gemini CLI relaunch that stops here, with one ledger line.
fn fail(opts: &Opts, r: Report, st: &mut St, why: &str, detail: &str) -> Report {
    st.fail(why, now_s());
    let step = match why {
        // Told, never typed: the host's `Cannot`.
        "no-turn" | "history-unsafe" => format!("refused:{why}"),
        _ => format!("failed:{why}"),
    };
    let r = said(r, step);
    ledger(opts, &r, detail);
    r
}

/// THE RELAUNCH LINE RAN AND ENDED with no relaunched Gemini CLI found leading
/// the tab: decided at once by its word ([`line_word`]) — a resume the CLI
/// refused, or a line the shell could not run, stops for good
/// (`refused:resume-refused`, never typed again: critic X4); one that ended
/// by its own or someone's hand is theirs (`ended:relaunch-ended`); one that
/// crashed is closed as gone before it was adopted, and the exit's next
/// attempt types it again on that very word ([`after_exit_onto`]); one no
/// word tells stops (`refused:relaunch-unwitnessed`).
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
        // The line names no `--session-id` ([`ug::rewrite_argv`]): its own
        // `42` is a refused resume ([`ug::line_exit_kind`]), never this.
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

/// Whether `pid` (argv `argv`) is the relaunch this lane typed: a child of
/// the line's shell, not the exited TUI, a Gemini CLI TUI resuming the
/// recorded session.
fn relaunched_by_us(pid: u32, argv: &[String], st: &St, k: &dyn GeminiKernel) -> bool {
    pid != st.pid
        && matches!(st.phase, Phase::Relaunched { .. } | Phase::Exiting { .. })
        && k.parent(pid) == Some(st.shell)
        && ug::is_tui_argv(argv)
        && ug::argv_session(argv).as_deref() == Some(st.thread.as_str())
}

/// After the relaunch line: find the new TUI and carry on with it, or hand it
/// back to its loop ([`adopt`]). A later step finding the relaunch in flight
/// comes here too.
fn await_new(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn GeminiKernel) -> Report {
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

/// The relaunched TUI `new`, on the record whatever becomes of it.
fn found<K: Kernel + ?Sized>(st: &mut St, new: u32, k: &K) {
    if st.relaunched_pid == 0 {
        st.relaunched_start = k.process_start(new).map(|s| squash(&s)).unwrap_or_default();
    } else if st.relaunched_pid != new {
        st.relaunched_start.clear();
    }
    st.relaunched_pid = new;
}

/// What the start-time rule names for the relaunched process `new`, within
/// [`super::super::Timings::gemini_witness`] (its header lands 542-1108 ms
/// after its child starts, measured): `Some(Ok(session))`, `Some(Err)` for a
/// rule that names none (another start, a `/clear`), `None` for one gone or
/// never read meanwhile.
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
        // Read with no argv to agree with: its line resumed the record's
        // session, and the header naming another is what this asks.
        let mut now = witness_of(chats.as_deref(), &st.cwd, start, &[], new, k);
        // Its header begun past the window (a loaded machine): read again
        // with its line's session to agree with, which a late header naming
        // that very session is, whoever began it.
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
        // Not written yet: looked at again; anything else decides.
        let settled = !matches!(now, Err(NoWitness::NoHeader));
        seen = Some(now);
        settled || !k.alive(new)
    });
    seen.filter(|_| k.alive(new))
}

/// THE RELAUNCHED GEMINI CLI CAME UP ON ANOTHER SESSION than the one its line
/// resumed — its start-time rule names another (`--resume <uuid>` brings back
/// that id or refuses, so it never should): nothing more is said into it,
/// and the person is told (`refused:started-other`).
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

/// Hand the relaunched Gemini CLI back to its supervisor (`adopted`): the new
/// TUI leads the tab by the host's own roster and THE START-TIME RULE NAMES
/// ITS SESSION for it (else another: [`started_other`]; none: adopted on its
/// line's word, `adopt-unwitnessed` in the ledger); the record stays in
/// flight, and at its loop's next idle point the relaunch primitive's
/// continuation ([`super::super::relaunch::resume`]) types the carry-on
/// ([`carry_on`]) — only where the rule names the session by then.
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
            // The rule names none for it (another Gemini CLI started in the
            // project with it): its line resumed this session by id, which
            // the CLI brings back or refuses (`42`), so it is adopted on that
            // line's word, said — and its continuation is never typed
            // ([`carry_on`] reads the rule again).
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

/// A Gemini CLI relaunch in flight carried on from where it stopped: the
/// relaunch primitive's continuation step for a Gemini CLI record
/// ([`super::super::relaunch::resume`]), and every other caller's.
pub(in crate::harness) fn carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    carry_in_flight_with(opts, r, st, session, &Live)
}

/// [`carry_in_flight`] over the kernel `k`.
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

/// Whether Gemini CLI's composer on `scr` is empty: its caret row holds
/// nothing (the placeholder is read as nothing,
/// [`aterm_phase::gemini::composer_draft`]) and no row of its draft holds a
/// word. `false` in shell mode (`!`), where what stands is a command.
fn composer_empty(scr: &super::Screen) -> bool {
    aterm_phase::gemini::composer_draft(&scr.rows)
        .is_some_and(|(_, lines)| lines.iter().all(|l| l.trim().is_empty()))
}

/// The last look before typing into Gemini CLI's composer: a fresh read
/// reading idle by its reader (no box, no turn running, its composer drawn,
/// no menu, not shell mode) with the composer empty, that read's generation
/// (which the typed turn is fenced on) and the caret it draws (`>`, `*` under
/// YOLO), which the guard anchors to.
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

/// Type `text` into Gemini CLI's composer as one turn, fenced on
/// `generation` and submitted under the composer's guard
/// (`composer_guard_at(<caret>, 1, …)`: its caret drawn one blank in, the
/// supervisor's own guard for this composer) once the echo has held still
/// (`idle=600`: an Enter within 30 ms of a key is a newline to it, bundle).
/// A text past the composer's paste fold is guarded on the chip it is drawn
/// as ([`aterm_phase::gemini::folded_paste`]), the row the cursor ends on: a
/// guard on its last word matched no row, and the relaunch's 538-character
/// continuation stood unsent in the composer, read as a person's draft
/// (the forever lab, 2026-10-21).
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

/// The column Gemini CLI draws its caret in (` > text`: one blank in), as
/// its reader says it ([`aterm_phase::ScreenReader::caret_column`]).
const CARET_COLUMN: usize = 1;

/// The relaunched TUI `new` is up: once its composer is drawn, THE
/// START-TIME RULE NAMES ITS SESSION FOR IT — its own file names the session
/// its line resumed (another: [`started_other`], nothing typed; none, read
/// within [`super::super::Timings::gemini_witness`]: `done:no-continue`,
/// nothing typed, said) — and it reads idle and empty with no person at the
/// tab, the continuation is typed (`continued`); a quiet relaunch types
/// nothing.
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
            // No file names its session (another Gemini CLI started with it,
            // a `/clear`, its header not written in time): its line resumed
            // the session by id, but nothing is said into a process aterm
            // cannot tell is on it.
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
        // A person at the keyboard wins: held, it stays owed.
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

// ---------------------------------------------------------------- the prune

/// Remove Gemini CLI records nothing will visit again, as codex-alab's path
/// removes its own: a record goes once its tab is proven gone, it is
/// unwritten for [`super::STATE_KEEP_S`] and settled; never one in flight or
/// not matching its name, and nothing in a dry run or a background break.
/// Called under the sweep lock.
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

/// The prune of one sweep: every tab the host's roster names is live, and
/// the rendezvous registry speaks for the rest where it may.
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
/// read for the rest only where it can vouch for this state root.
fn prune_known(opts: &Opts, known_live: Vec<String>) {
    let graph = registry_graph();
    prune(opts, now_s(), registry_life(graph.as_deref(), &known_live));
}

// ---------------------------------------------------------------- the owner's view

/// The live Gemini CLI holders of the `gemini-<tab>` records in `sessions`,
/// for the status view ([`super::upgrade_status::Holder`]): the TUI leading
/// the tab and its install's version. The host proves the tab by roster; a
/// hand-run view by the TUI's environment.
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

// The Gemini CLI lane's driver tests drive scripted processes over a
// stand-in tab.
#[cfg(all(test, unix))]
#[path = "upgrade_gemini_drive_tests.rs"]
mod tests;
