// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE RELAUNCH PRIMITIVE: a Claude Code that is no longer running is started
//! again in its own tab, on its own conversation — one line typed at the
//! tab's shell once that shell holds the terminal again, `--resume <id>` at
//! its end ([`super::upgrade::relaunch_line`]), then, once the new process
//! holds the conversation, one turn telling it to carry on.
//!
//! Three callers, one primitive (owner, 2026-09-24: *"UNLESS aterm is
//! configured otherwise, it is in FULLY AUTOMATIC mode and there cannot be
//! any interruptions because there is nobody to address the interruption"*):
//!
//! * THE LIVE UPGRADE ([`super::upgrade_drive`]) ends the agent itself —
//!   SIGTERM, after the agent answered READY — and relaunches it on the
//!   newer build ([`relaunch`], [`await_new`], [`carry_on`]).
//! * THE RESTART IN PLACE ([`restart_here`]): the session's supervisor met a
//!   point nothing typed can answer — Claude Code's critical-memory banner
//!   ([`Restart::Memory`]) — and its host ends the agent at that idle point,
//!   with the upgrade's last looks and no announcement (a process past
//!   saving reads nothing), and relaunches it on the SAME program.
//! * RELAUNCH ON EXIT ([`after_exit`]): an agent that CRASHED out of its
//!   tab — a crash, an OOM kill, a SIGKILL — is relaunched on its
//!   conversation. The process is gone by then, so what the
//!   line needs is read WHILE IT RUNS ([`snapshot`]): its pid and kernel
//!   start, its argv, its shell, its tab. The conversation is Claude's own
//!   record, which the exit left behind: a crash cannot remove
//!   `sessions/<pid>.json`, and an exit that did — a graceful one — was
//!   someone's decision (a person's, an orchestrator's over the socket, a
//!   `kill` from elsewhere) and is never relaunched against them. Which of
//!   the two it was is read AS THE EXIT IS SEEN ([`exit_record`], within
//!   [`EXIT_SETTLE`]) and kept ([`ExitRecord`]): any Claude Code that starts
//!   later removes dead processes' records, so the record as it stands
//!   after the back-off says nothing about the exit (D2, 2026-09-26). The host
//!   decides WHETHER ([`on_exit`]: `[harness] relaunch`, and no person's
//!   keystroke within `human_grace_s`, no holder's hand) and WHEN
//!   ([`Relaunches`]: a growing back-off, never a silent give-up —
//!   [`Outcome::Cannot`] is the keyed attention).
//!
//! Either way, in the window the new process is its supervisor's the moment
//! it holds the conversation (`adopted`, [`Opts::hand_back`]): the loop
//! answers whatever it opened with, and the continuation is typed at that
//! loop's first idle point, by its host's step there ([`owed`], [`resume`])
//! — never by a step waiting for idle with no loop running, which a startup
//! dialog outlasts.
//!
//! Every fact is re-read at the act and every guard the upgrade proved is
//! kept: the shell's own foreground job on a terminal an aterm tab owns, the
//! tab's unique PTY claim read again right before the line is typed, no live
//! process holding the conversation, the relaunch adopted only when the
//! shell the line was typed at started it. A relaunch in flight is filed
//! under `<state>/upgrade/<conversation>.json` exactly as an upgrade's is, so
//! whichever caller comes next finishes it.
//!
//! The one line also HEALS THE SHELL it is typed at (2026-09-26): a tab whose
//! shell lost its integration key before the re-key channel existed reads
//! `status integration=degraded`, and nothing but a line typed while the
//! SHELL holds the terminal can reach it. The relaunch line is that line, for
//! every caller above: the window is asked for a one-use key file (`rekey
//! shell=<pid>`) and the shell takes it in front of the relaunch
//! ([`type_relaunch_line_with`], [`upgrade::with_rekey`]). The same line
//! reaches a shell whose integration predates LOADERS (2026-09-26: `status
//! integration_rev=frozen`), which no body pointer ever will: the window names
//! its own loader in the file, and the line sources it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::Value;

use super::upgrade::{self, Dialect, SessionFile, Version};
use super::upgrade_drive::{
    Client, Job, Kernel, Live, Opts, Report, Screen, SessionRosterCache, St, TAIL_BYTES, Targets,
    Terminated, Typed, alive, background, composer_empty, connect, continuation_composer_empty,
    conversation_live, cwd_of, exe_of, first_word, foreground_shell, held, host_roster, ids,
    is_our_ancestor, kernel_start, ledger, load, model_and_mark, native_root, now_s,
    owned_by_aterm, process_in_tab, record_asked, require_unique_owner, restore, riding_model,
    roster, said, save, screen, session_file_of, session_files, since, squash, state_dir,
    sweep_lock, table, tail_to_end, terminate, transcript, transcript_exists, turn, turn_fenced,
    type_line, typing_fence, unique_tab_for_group,
};
use crate::supervise::limit::one_line;
pub use crate::supervise::policy::turn_end::Restart;
use crate::supervise::screen::HumanInput;

// ---------------------------------------------------------------- the plan

/// A relaunch planned from the live process: the shell that gets its prompt
/// back, its dialect (what a re-key taken in the same line is written in:
/// [`type_relaunch_line_with`]), and the one line typed at it.
pub(super) struct Plan {
    pub(super) shell: u32,
    pub(super) dialect: Dialect,
    pub(super) line: String,
}

/// Why a session has no relaunch plan.
pub(super) enum NoPlan {
    /// A fact this pass could not read (one word): nothing is typed, and a
    /// later pass asks again.
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

/// What a relaunch line is made from.
pub(super) struct Launch<'a> {
    /// The conversation `--resume` names.
    pub(super) session: &'a str,
    /// Whether the line resumes it: `false` for a conversation with no
    /// message yet, which cannot be resumed — the agent is started afresh.
    pub(super) resume: bool,
    /// The directory the conversation was started in, when known.
    pub(super) cwd: &'a str,
    /// The agent the line relaunches (its kernel cwd is asked when `cwd` is
    /// empty and it still runs).
    pub(super) agent: u32,
    /// Its argv, `argv[0]` first.
    pub(super) argv: &'a [String],
    /// What the line executes: an absolute path.
    pub(super) exe: &'a Path,
    /// The model the relaunch asks for — the priority list's
    /// ([`super::upgrade_models`]), or a bucket's fallback and its way back
    /// ([`restart_here`]): the launch's own `--model`, every spelling, is
    /// replaced by it; an EMPTY one drops it (Claude's default). `None`
    /// keeps the launch's flags.
    pub(super) model: Option<&'a str>,
    /// The permission mode to resume in ([`resume_mode`]): the relaunch
    /// names it ([`with_permission_mode`]) rather than carrying the launch's.
    /// `None` keeps the launch's flags.
    pub(super) mode: Option<&'a str>,
}

/// THE RELAUNCH, PLANNED from the live process: `shell` — the parent the job
/// read just proved holds (or held) the agent as its foreground job
/// ([`foreground_shell`]; never re-derived here, so the plan and the proof
/// name one process) — its dialect, the heal it needs, the directory the
/// conversation resumes in, and from those the one line ([`line_for`]).
/// The upgrade plans TWICE: before its announcement, so a launch that can
/// never be carried is refused before the agent is asked to wind down, and
/// again before its one signal, which is the last word.
pub(super) fn plan(
    opts: &Opts,
    launch: &Launch<'_>,
    shell: u32,
    t: &[(u32, u32, String)],
) -> Result<Plan, NoPlan> {
    let Some(dialect) = shell_dialect(shell, t) else {
        return Err(NoPlan::Wait("shell-dialect"));
    };
    let heal = atpkg::hooks::hook_file(&opts.home, dialect.hook_ext()).map(|(p, _)| p);
    // `--resume` finds a conversation under the directory it was started in.
    let want = if launch.cwd.is_empty() {
        cwd_of(launch.agent).unwrap_or_default()
    } else {
        launch.cwd.to_string()
    };
    let cd = (!want.is_empty() && cwd_of(shell).as_deref() != Some(want.as_str())).then_some(want);
    let line = line_for(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        launch.exe,
        launch.argv,
        launch.resume.then_some(launch.session),
        launch.model,
        launch.mode,
    )?;
    Ok(Plan {
        shell,
        dialect,
        line,
    })
}

/// The dialect of the shell `shell`, as its executable names it: the kernel's
/// argv, or the name in the process table `t` where that cannot be read.
pub(super) fn shell_dialect(shell: u32, t: &[(u32, u32, String)]) -> Option<Dialect> {
    atpkg::caller_shell::process_args(shell)
        .map(|a| a.exec_path)
        .or_else(|| {
            t.iter()
                .find(|(p, _, _)| *p == shell)
                .map(|(_, _, n)| n.clone())
        })
        .as_deref()
        .and_then(Dialect::from_exe_name)
}

/// The pure half of [`plan`]: the launch flags carried into a resume of
/// `session` ([`upgrade::rewrite_argv`]) — or into a fresh start when there
/// is none, the same flags without `--resume` — asking for `model` from the
/// priority list when one is given ([`with_model`]) and in the permission
/// `mode` when one is known ([`with_permission_mode`]), and the line that
/// runs them ([`upgrade::relaunch_line`]), or the refusal either one makes.
#[allow(clippy::too_many_arguments)]
pub(super) fn line_for(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    argv: &[String],
    session: Option<&str>,
    model: Option<&str>,
    mode: Option<&str>,
) -> Result<String, NoPlan> {
    let mut flags =
        upgrade::rewrite_argv(argv, session.unwrap_or("-")).map_err(|e| NoPlan::Refused {
            why: format!("argv:{e}"),
            what: e.to_string(),
            detail: "the launch flags cannot be carried into a resume".to_string(),
        })?;
    if session.is_none() {
        // `rewrite_argv` ends with `--resume <id>`, the one pair it adds.
        flags.truncate(flags.len().saturating_sub(2));
    }
    let flags = with_permission_mode(with_model(flags, model), mode);
    upgrade::relaunch_line(dialect, heal, cd, exe, &flags).map_err(|e| NoPlan::Refused {
        why: format!("line:{e}"),
        what: "line".to_string(),
        detail: e,
    })
}

/// `flags` with every `--model` spelling replaced by `model`, when one is
/// given — before the `--resume` pair [`upgrade::rewrite_argv`] ends with —
/// or dropped, when it is empty. A command-line model is session-only, where
/// Claude's own `/model <id>` also rewrites the person's default for every
/// new session (2.1.282).
pub(super) fn with_model(flags: Vec<String>, model: Option<&str>) -> Vec<String> {
    let Some(model) = model else {
        return flags;
    };
    let mut out = Vec::with_capacity(flags.len() + 2);
    let mut it = flags.into_iter().peekable();
    let mut tail = Vec::new();
    while let Some(f) = it.next() {
        if f == "--model" {
            let _ = it.next();
        } else if f.starts_with("--model=") {
        } else if f == "--resume" {
            tail.push(f);
            tail.extend(it.by_ref());
        } else {
            out.push(f);
        }
    }
    if !model.is_empty() {
        out.push("--model".to_string());
        out.push(model.to_string());
    }
    out.extend(tail);
    out
}

/// The permission modes a `permission-mode` transcript row may name (2.1.283:
/// `default` became `manual`); any other value is not carried.
const PERMISSION_MODES: [&str; 7] = [
    "default",
    "manual",
    "acceptEdits",
    "plan",
    "auto",
    "dontAsk",
    "bypassPermissions",
];

/// The permission mode `session` last ran in: its transcript's newest
/// `{"type":"permission-mode"}` row — Claude writes one as the mode changes
/// (mid-turn too) and with each prompt, so a crash's conversation has it as
/// well as a live one's. `None` when the tail names none, or a mode this
/// module does not know.
pub(super) fn last_permission_mode(home: &Path, session: &str) -> Option<String> {
    let path = transcript(home, session)?;
    let (tail, _) = tail_to_end(&path, TAIL_BYTES);
    permission_mode_of(&tail)
}

/// [`last_permission_mode`]'s scan, pure: the NEWEST `permission-mode` row
/// decides, known mode or not.
pub(super) fn permission_mode_of(tail: &str) -> Option<String> {
    tail.lines()
        .rev()
        .filter(|l| l.contains("\"permission-mode\""))
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .find(|v| v.get("type").and_then(Value::as_str) == Some("permission-mode"))?
        .get("permissionMode")
        .and_then(Value::as_str)
        .filter(|m| PERMISSION_MODES.contains(m))
        .map(str::to_owned)
}

/// The permission mode to resume `session` in: the pill on the tab's screen
/// where it reads one — the live mode, exactly the person's choice — else the
/// transcript's newest `permission-mode` row ([`last_permission_mode`]).
/// Claude writes that row only when it re-appends its session metadata (about
/// every 32 KiB of transcript, a compaction, a resume, its exit), not as the
/// mode changes, so a mode chosen at idle — a shift+tab, an aterm light — is
/// on the screen long before it is in the file. An exited agent's last frame
/// usually still shows its pill; a graceful exit re-stamps the file.
///
/// The live upgrade reads the transcript alone ([`last_permission_mode`]):
/// its exchanges with the host are fenced and counted, and its announcement
/// is a prompt the agent answers before it is ended.
pub(super) fn resume_mode(opts: &Opts, tab: &str, session: &str) -> Option<String> {
    connect(opts, tab)
        .ok()
        .and_then(|mut c| screen(&mut c, tab))
        .and_then(|scr| super::lights::read_screen(&scr.rows))
        .and_then(|shown| shown.mode)
        .map(|mode| mode_flag(mode).to_string())
        .or_else(|| last_permission_mode(&opts.home, session))
}

/// The `--permission-mode` value of a mode Claude's pill shows. Manual mode
/// is `default`, the spelling every build accepts (2.1.283 takes `manual`
/// too).
fn mode_flag(mode: super::lights::Mode) -> &'static str {
    use super::lights::Mode;
    match mode {
        Mode::Bypass => "bypassPermissions",
        Mode::Auto => "auto",
        Mode::AcceptEdits => "acceptEdits",
        Mode::DontAsk => "dontAsk",
        Mode::Plan => "plan",
        Mode::Manual => "default",
    }
}

/// `flags` — the launch's, as [`upgrade::rewrite_argv`] carried them (the
/// resume pair last) — resumed in permission `mode` ([`resume_mode`]). Claude
/// does not bring a resumed conversation back in the mode it ran in (plan and
/// bypass never; any other only when no mode flag is given, and then a
/// `permissions.defaultMode` setting or the auto-mode default can decide
/// instead), so a relaunch carrying the LAUNCH's flags brought a session
/// started with `--dangerously-skip-permissions` back in bypass after its
/// person had left it (measured on this repo's own conversation, 2026-09-26:
/// auto before an upgrade relaunch, bypass after).
///
/// A relaunch never moves a session INTO bypass: with the mode bypass, or
/// unknown, the flags are returned as launched. Otherwise every bypass and
/// mode flag goes; `--allow-dangerously-skip-permissions` keeps bypass in the
/// shift+tab cycle when the launch had put it there; and the mode is ALWAYS
/// named, the default included, so no setting and no build's default picks
/// another one. Rewritten on the parsed flags, like [`with_model`]: a launch
/// prompt, a `--` or a many-valued flag cannot swallow what is added.
pub(super) fn with_permission_mode(flags: Vec<String>, mode: Option<&str>) -> Vec<String> {
    let Some(mode) = mode.filter(|m| *m != "bypassPermissions") else {
        return flags;
    };
    let mode = if mode == "manual" { "default" } else { mode };
    let mut out = Vec::with_capacity(flags.len() + 3);
    let mut tail = Vec::new();
    let mut bypass_in_cycle = false;
    let mut it = flags.into_iter();
    while let Some(f) = it.next() {
        match f.as_str() {
            "--dangerously-skip-permissions" | "--allow-dangerously-skip-permissions" => {
                bypass_in_cycle = true;
            }
            "--permission-mode" => {
                bypass_in_cycle |= it.next().is_some_and(|m| m == "bypassPermissions");
            }
            a if a.starts_with("--permission-mode=") => {
                bypass_in_cycle |= a == "--permission-mode=bypassPermissions";
            }
            "--resume" => {
                tail.push(f);
                tail.extend(it.by_ref());
            }
            _ => out.push(f),
        }
    }
    if bypass_in_cycle {
        out.push("--allow-dangerously-skip-permissions".to_string());
    }
    out.push("--permission-mode".to_string());
    out.push(mode.to_string());
    out.extend(tail);
    out
}

/// A session with no relaunch plan. A wait types and records nothing. A
/// refusal FAILS the relaunch to this target (not retried for it) with one
/// ledger line — `would-refuse:` in a dry run, which records nothing.
pub(super) fn unplanned(opts: &Opts, r: Report, st: &mut St, no: NoPlan) -> Report {
    match no {
        NoPlan::Wait(why) => said(r, format!("wait:{why}")),
        NoPlan::Refused { what, .. } if opts.dry_run => said(r, format!("would-refuse:{what}")),
        NoPlan::Refused { why, what, detail } => {
            st.stop(&why);
            let r = said(r, format!("refused:{what}"));
            ledger(opts, &r, &detail);
            r
        }
    }
}

// ---------------------------------------------------------------- the act

/// How long a relaunch in flight may still act on its tab, from the agent's
/// end (exiting) or the relaunch line (relaunched). The relaunch line is
/// typed at a prompt, and minutes after the agent ended the person may be
/// typing at that prompt, or it may be gone with its tab: an exit that old
/// is never relaunched from this record, a relaunch that old never waited
/// for again.
pub(super) const STALE_S: u64 = 300;

/// Why a relaunch in flight whose agent is gone must stop instead of acting
/// on the tab, `(word, ledger detail)`, or `None` while it may still act.
pub(super) fn expired(st: &St, now: u64) -> Option<(&'static str, &'static str)> {
    use upgrade::Phase;
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

/// What a new process holding the conversation of a relaunch in flight is to
/// that relaunch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Relaunch {
    /// The relaunch this record typed: carried on with.
    Ours,
    /// PROVEN not the relaunch: another conversation, the process that
    /// ended, a parent READ as another than the recorded shell, or no shell
    /// recorded at all.
    NotOurs,
    /// Its parent could not be read now (it ended as it was asked, or `ps`
    /// could not run): no verdict either way.
    Unread,
}

/// [`Relaunch`] from the new process's parent as read now (`None`: unread)
/// and the shell the relaunch line was typed at (`0`: none was recorded).
pub(super) fn relaunch_by_parent(parent: Option<u32>, shell: u32) -> Relaunch {
    match parent {
        _ if shell == 0 => Relaunch::NotOurs,
        None => Relaunch::Unread,
        Some(p) if p == shell => Relaunch::Ours,
        Some(_) => Relaunch::NotOurs,
    }
}

/// Whether `s` is the relaunch this record typed: a process other than the
/// one that ended, holding the SAME conversation, and a child of the shell
/// the line was typed at (the line runs the build as that shell's child —
/// directly, or `exec`ed by a `( cd … )` subshell of it). Neither half alone
/// will do: a person's fresh `claude` at that prompt after a relaunch that
/// failed has the shell but not the conversation (measured 2026-09-23:
/// adopted, told it was upgraded and resumed, its conversation filed `done`),
/// and the conversation resumed by hand in another tab has the conversation
/// but not the shell. A parent that cannot be read is [`Relaunch::Unread`],
/// never `NotOurs`: the in-flight branch of the upgrade's visit records a
/// failure from `NotOurs` for good.
pub(super) fn relaunch_of(
    k: &dyn Kernel,
    s: &SessionFile,
    session: &str,
    old: u32,
    shell: u32,
) -> Relaunch {
    if s.pid == old || s.session_id != session {
        return Relaunch::NotOurs;
    }
    relaunch_by_parent(k.parent(s.pid), shell)
}

/// Poll `cond` every 250 ms for at most `limit`: what it asks — a shell's
/// terminal group, Claude's own session records, an agent's status — has no
/// event to wait on (the agent's exit does: [`super::upgrade_wake::wait_exit`]).
pub(super) fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    cond()
}

/// The agent ended (it was signalled, or it exited by itself): once it is
/// gone and the shell has its prompt back, type the relaunch line.
///
/// Gone is the kernel's word, its `NOTE_EXIT` pushed
/// ([`super::upgrade_wake::wait_exit`]). Claude's graceful
/// shutdown removes its `sessions/<pid>.json` before the process ends, so a
/// signalled agent is gone after its record is; a CRASHED one never removes
/// it, and waiting for the record too (the rule until 2026-09-24, when only a
/// SIGTERM the upgrade sent came here) would wait out every crash.
pub(super) fn relaunch(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    session: &str,
    k: &dyn Kernel,
) -> Report {
    let old = st.pid;
    if !super::upgrade_wake::wait_exit(old, Duration::from_secs(30)) {
        // Never a harder signal: the agent finishes exiting on its own, and a
        // later pass finds the phase and the process as they are.
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
    let line = st.line.clone();
    // Read again rather than recorded: a relaunch a later step resumes finds
    // the same shell, and one whose name cannot be read now types no re-key.
    let dialect = shell_dialect(shell, &table());
    match type_relaunch_line(
        opts,
        c,
        shell,
        &tab,
        session,
        &line,
        dialect,
        &mut st.prompt,
        |c, pid, tab| process_in_tab(c, pid, tab, None, None),
    ) {
        Ok(()) => st.prompt = None,
        Err(RelaunchLineError::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(RelaunchLineError::Turn(e)) => {
            st.stop("relaunch-refused");
            let r = said(r, format!("failed:relaunch:{}", first_word(&e)));
            ledger(opts, &r, &st.line);
            return r;
        }
    }
    st.phase = upgrade::Phase::Relaunched { at_s: now_s() };
    ledger(opts, &said(r.clone(), "relaunched"), &st.line);
    save(opts, session, st);
    await_new(opts, r, st, c, session, k)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RelaunchLineError {
    Wait(&'static str),
    Turn(String),
}

/// The complete session scan can outlast a foreground-shell claim. Probe the
/// PTY again immediately before typing, and expose that second read to a
/// deterministic race test through the same seam used by production. The
/// session's `status` is read as the line is typed — after every wait before
/// it, which can add up to most of a minute: a person typing at the returned
/// prompt within the grace, a halt or anyone's hand holds the tab
/// ([`super::upgrade_drive`]'s `held`, the upgrade's own last look before its
/// signal), and nothing is typed.
///
/// THE SHELL IS PROVEN TO HOLD THE TAB IMMEDIATELY BEFORE EVERY TYPED TRY
/// (review of 2026-09-25): a fenced turn parks up to 30 s on a person's
/// typing, and a person who started `vim`, `ssh`, a REPL or another `claude`
/// at the returned prompt in that time had the relaunch line and its Enter
/// typed into THAT program. So `tab_probe` is asked again right before each
/// try, fenced or not, and a prompt that MOVED after the mark is never marked
/// again within the attempt — it waits (`prompt-moved`), and only a later
/// attempt, through [`relaunch`]'s own check that the shell has its prompt
/// back, marks afresh.
///
/// WHERE THE PROMPT LEFT THE CURSOR (review of 2026-09-25): the fence proves
/// only that the screen did not move between a read and the paste, never
/// that the prompt's input line is EMPTY — a person who typed `rm -rf build`
/// at the returned prompt and paused had the line appended to theirs, and
/// Enter pressed on both. The prompt is MARKED as first read ([`PromptMark`],
/// [`first_mark`]: never on a read that already shows typing after the
/// prompt), and the mark is kept across attempts (`St::prompt`): a read that
/// finds a person's typing after it — to its right included, a caret moved
/// home over a half-typed command — WAITS (`typing`), never typed at, and so
/// does every later attempt until the prompt reads as marked again (their
/// text deleted). A prompt that moved for another reason (an async redraw, a
/// new prompt, a program started at it) waits too (`prompt-moved`), and a
/// later attempt marks it afresh. A prompt whose text ticks in place (a
/// clock) keeps its cursor.
///
/// Bash is typed to ([`type_line`]): its 3.2 (macOS's `/bin/bash`) has no
/// bracketed paste, and a crashed agent leaves the terminal's mode on;
/// keystrokes take no fence, so the mark is read once more right before
/// them. zsh and fish read a bracketed paste (they set the mode at each
/// prompt themselves) and are pasted to, so no key binding or abbreviation
/// of theirs acts on the line — FENCED on a fresh read of the prompt
/// ([`turn_fenced`]): a keystroke echoed between that read and the paste
/// types nothing, and the prompt is read again. A person typing through the
/// whole yield is a wait, never the refusal that would fail a relaunch whose
/// agent is already gone. A prompt that never holds still for the whole
/// budget (a ticking right prompt, an async `vcs_info` redraw) is pasted
/// unfenced once a fresh read still finds the prompt as marked and the shell
/// still holds the tab: a relaunch that never came would leave the tab at a
/// bare prompt.
///
/// THE HEAL RIDES THE LINE (2026-09-26): with the shell's `dialect` known, a
/// tab whose shell integration the window reports `degraded` is re-keyed IN
/// this line ([`type_relaunch_line_with`]) — for every relaunch this primitive
/// types, the upgrade's and a relaunch on exit's alike.
#[allow(clippy::too_many_arguments)]
pub(super) fn type_relaunch_line(
    opts: &Opts,
    c: &mut Client,
    shell: u32,
    tab: &str,
    session: &str,
    line: &str,
    dialect: Option<Dialect>,
    mark: &mut Option<PromptMark>,
    tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<(), RelaunchLineError> {
    type_relaunch_line_with(
        opts,
        c,
        shell,
        tab,
        line,
        dialect,
        mark,
        || session_files(&opts.home).is_some_and(|files| !conversation_live(&files, session)),
        tab_probe,
    )
}

/// THE ONE RELAUNCH LINE, whichever agent it resumes: [`type_relaunch_line`]
/// with the check that no process holds the conversation any more given as
/// `owner_clear` — Claude Code's session files for a Claude restart, the
/// thread's writer lock for a Codex one (`upgrade_drive`'s `codex` branch).
/// Every other guard — the tab's shell proven before each try, a person's
/// hold, the prompt's mark, the fence — is this one function's, for both.
///
/// THE HEAL RIDES THE LINE (2026-09-26): with the shell's `dialect` known, and
/// once every check that can make this attempt WAIT before the first try has
/// passed, the window is asked for a typed re-key of the tab
/// ([`issue_rekey`]) — granted only for a tab whose integration is degraded or
/// predates loaders (the window then also names its own loader: the line
/// upgrades the shell's integration in place, 2026-09-26), and only while
/// `shell` leads its foreground group — and the line typed is
/// [`upgrade::with_rekey`]'s. A shell spawned before the re-key channel has no
/// hook that would ever read a key file, and its foreground is usually an
/// agent, where typed text is a prompt; this line is the one typed while the
/// SHELL holds the terminal, so every relaunch — the upgrade's, a restart in
/// place, a relaunch on exit, the Codex branch's — heals it at no extra
/// typing. Nothing about it can stop the relaunch: a refusal, an older
/// window, a healed line too long for the tty, and the plain line is typed as
/// before. An attempt that then types nothing takes the key back at once
/// ([`withdraw_rekey`]), so the one-use file never waits for a line that is
/// not coming; one the window is never told of is taken back by the window
/// itself.
#[allow(clippy::too_many_arguments)]
pub(super) fn type_relaunch_line_with(
    opts: &Opts,
    c: &mut Client,
    shell: u32,
    tab: &str,
    line: &str,
    dialect: Option<Dialect>,
    mark: &mut Option<PromptMark>,
    owner_clear: impl FnOnce() -> bool,
    mut tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<(), RelaunchLineError> {
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    if !owner_clear() {
        return Err(RelaunchLineError::Wait("conversation-owner-ambiguous"));
    }
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    if held(c, tab, opts.human_grace_s) {
        return Err(RelaunchLineError::Wait("held"));
    }
    if mark.is_none()
        && let Some(scr) = screen(c, tab)
    {
        *mark = first_mark(c, tab, &scr)?;
    }
    let healed = dialect.and_then(|d| healed_line(c, tab, shell, d, line));
    let typed = type_at_prompt(
        c,
        shell,
        tab,
        healed.as_deref().unwrap_or(line),
        mark,
        &mut tab_probe,
    );
    if typed.is_err() && healed.is_some() {
        withdraw_rekey(c, tab);
    }
    typed
}

/// The relaunch line with the tab's shell taking a fresh key in front of it
/// ([`upgrade::with_rekey`]), or `None` — the window refused (a healthy tab
/// with a loader, an unintegrated one, a re-key of the shell's own already
/// waiting, a shell no
/// longer in the foreground), knows no such verb (an older aterm), or the
/// healed line would not fit, in which case the key it issued is taken back.
fn healed_line(
    c: &mut Client,
    tab: &str,
    shell: u32,
    dialect: Dialect,
    line: &str,
) -> Option<String> {
    let path = issue_rekey(c, tab, shell)?;
    match upgrade::with_rekey(dialect, line, &path) {
        Ok(healed) => Some(healed),
        Err(_) => {
            withdraw_rekey(c, tab);
            None
        }
    }
}

/// Ask the window for a TYPED RE-KEY of `tab`, whose foreground shell the
/// caller proved `shell` to be (`@<tab> rekey shell=<pid>`, owner-only): the
/// window mints a key, writes it to a one-use file with the re-key channel's
/// discipline, authorizes it as one the shell has not taken, and answers the
/// file's PATH — never the key. `None` on any other answer.
fn issue_rekey(c: &mut Client, tab: &str, shell: u32) -> Option<PathBuf> {
    let reply = c
        .request_line(&format!("@{tab} rekey shell={shell}"))
        .ok()?;
    rekey_path_of(&reply)
}

/// The path an `OK rekey … path=<path>` reply names — the rest of the line, so
/// a path with a space in it ("Application Support") comes back whole.
fn rekey_path_of(reply: &str) -> Option<PathBuf> {
    let rest = reply
        .trim_end_matches(['\r', '\n'])
        .strip_prefix("OK rekey ")?;
    let (_, path) = rest.split_once("path=")?;
    Path::new(path).is_absolute().then(|| PathBuf::from(path))
}

/// Take back the typed re-key of `tab` whose line was not typed (`@<tab>
/// rekey withdraw`): the file goes and the authorization it replaced stands
/// again. Best effort; the window's own expiry is the backstop.
fn withdraw_rekey(c: &mut Client, tab: &str) {
    let _ = c.request_line(&format!("@{tab} rekey withdraw"));
}

/// The typing half of [`type_relaunch_line_with`]: `line` at the marked
/// prompt — typed to bash, pasted fenced to zsh and fish.
fn type_at_prompt(
    c: &mut Client,
    shell: u32,
    tab: &str,
    line: &str,
    mark: &mut Option<PromptMark>,
    tab_probe: &mut impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<(), RelaunchLineError> {
    let bash = atpkg::caller_shell::process_args(shell)
        .and_then(|a| Dialect::from_exe_name(&a.exec_path))
        == Some(Dialect::Bash);
    if !bash {
        for _ in 0..RELAUNCH_FENCE_TRIES {
            let scr = screen(c, tab);
            at_mark(mark, scr.as_ref())?;
            if !tab_probe(c, shell, tab) {
                return Err(RelaunchLineError::Wait("tab-ownership-changed"));
            }
            let Some(generation) = scr.and_then(|s| s.generation) else {
                break;
            };
            match turn_fenced(c, tab, line, Some(&generation)) {
                Ok(()) => return Ok(()),
                Err(Typed::Changed) => std::thread::sleep(RELAUNCH_FENCE_EVERY),
                Err(Typed::Yielded) => return Err(RelaunchLineError::Wait("yield")),
                Err(Typed::Refused(e)) => return Err(RelaunchLineError::Turn(e)),
            }
        }
    }
    at_mark(mark, screen(c, tab).as_ref())?;
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    if bash {
        return type_line(c, tab, line).map_err(RelaunchLineError::Turn);
    }
    match turn_fenced(c, tab, line, None) {
        Ok(()) => Ok(()),
        Err(Typed::Yielded | Typed::Changed) => Err(RelaunchLineError::Wait("yield")),
        Err(Typed::Refused(e)) => Err(RelaunchLineError::Turn(e)),
    }
}

/// The FIRST mark of a returned prompt ([`PromptMark::of`] over `scr`), made
/// only where nothing is typed after the prompt yet (review of 2026-09-25:
/// typeahead that reached the prompt before its first read — typed while the
/// agent exited — BECAME the mark, and the line was appended to it). Where
/// the shell's integration marked the input's start (`133;B`, [`input_start`])
/// the cursor must sit exactly there, on the prompt that start was marked on;
/// where it did not, the cursor must follow a blank (every common prompt ends
/// in one, and typing does not) — a prompt with no trailing blank and no
/// integration is then never typed at (the restart stops at its stale bound,
/// the tab at its prompt: the safe way to be wrong). Either way the cell at
/// the cursor and the one after it must be blank: a caret moved home over a
/// typed command sits ON it. `Ok(None)`: a read with no cursor, typed at as
/// before; a wait (`typing`) where the read shows typing.
fn first_mark(
    c: &mut Client,
    tab: &str,
    scr: &Screen,
) -> Result<Option<PromptMark>, RelaunchLineError> {
    let Some(mark) = PromptMark::of(scr) else {
        return Ok(None);
    };
    let row = scr
        .cursor
        .and_then(|(r, _)| scr.rows.get(r))
        .map_or("", String::as_str);
    let at_cursor: String = row.chars().skip(mark.col).take(2).collect();
    let prompt_ok = match input_start(c, tab) {
        Some((prompt_row, col)) => {
            mark.col == col
                && prompt_row.chars().take(col).collect::<String>()
                    == row.chars().take(col).collect::<String>()
        }
        None => mark.left.chars().count() < mark.col || mark.left.ends_with(char::is_whitespace),
    };
    if !at_cursor.trim().is_empty() || !prompt_ok {
        return Err(RelaunchLineError::Wait("typing"));
    }
    Ok(Some(mark))
}

/// Where the shell's integration marked the current prompt's input to start
/// (`133;B`): that prompt's row as the `line` verb reads it, and the column —
/// from the newest command block while it is `entering` (`blocks 1 --json`'s
/// `"cmd"`/`"cmdcol"`). `None` where no such mark reads: no integration, a
/// block in any other state, or a host that sends no `cmdcol`.
fn input_start(c: &mut Client, tab: &str) -> Option<(String, usize)> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 1 --json")).ok()?;
    if !head.starts_with("OK") {
        return None;
    }
    let json = if body.trim().is_empty() {
        head.strip_prefix("OK ")?
    } else {
        &body
    };
    let v: Value = aterm_json::from_str(json.trim()).ok()?;
    let block = v.get("blocks")?.as_array()?.last()?.clone();
    if block.get("state").and_then(Value::as_str) != Some("entering") {
        return None;
    }
    let row = block.get("cmd")?.as_u64()?;
    let col = usize::try_from(block.get("cmdcol")?.as_u64()?).ok()?;
    let reply = c.request_line(&format!("@{tab} line {row}")).ok()?;
    let text = reply.strip_prefix("OK")?;
    Some((text.strip_prefix(' ').unwrap_or(text).to_string(), col))
}

/// Judge one read of the prompt against `mark` ([`PromptMark::at`]): `Ok`
/// where the line may be typed on it — the prompt as marked, or no mark and a
/// read with nothing to mark by (no screen, no cursor: a host that sends none
/// types as it always did) — and a wait where a person typed after it
/// (`typing`), where it MOVED (`prompt-moved`: the mark is dropped, and only
/// a later attempt marks it afresh — never this one, whose next read could be
/// a program a person started at the prompt), or where a marked prompt can no
/// longer be read.
fn at_mark(mark: &mut Option<PromptMark>, scr: Option<&Screen>) -> Result<(), RelaunchLineError> {
    let Some(scr) = scr else {
        return if mark.is_some() {
            Err(RelaunchLineError::Wait("screen-unreadable"))
        } else {
            Ok(())
        };
    };
    match mark.as_ref().map(|m| m.at(scr)) {
        Some(AtPrompt::Same) => Ok(()),
        Some(AtPrompt::Typed) => Err(RelaunchLineError::Wait("typing")),
        Some(AtPrompt::Moved) => {
            *mark = None;
            Err(RelaunchLineError::Wait("prompt-moved"))
        }
        None if PromptMark::of(scr).is_none() => Ok(()),
        None => Err(RelaunchLineError::Wait("prompt-moved")),
    }
}

/// WHERE A SHELL'S PROMPT LEFT THE CURSOR, as a read of it showed: the screen
/// row ([`Screen::first`] added back, so two reads compare), the column, the
/// row's text left of the cursor — the prompt — and where text to its RIGHT
/// began (a right prompt), if any. Kept in the relaunch's state across
/// attempts (`St::prompt`), so a later step judges the prompt against the
/// SAME mark.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PromptMark {
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) left: String,
    /// The column the first non-blank text right of the cursor began at when
    /// the mark was taken (a right prompt, `RPROMPT`), `None` for a blank
    /// rest of the row: the cells from the cursor up to it must stay blank
    /// ([`Self::at`]).
    pub(super) right: Option<usize>,
}

/// What a later read of the prompt shows against its [`PromptMark`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtPrompt {
    /// The cursor where the mark left it (text redrawn in place — a ticking
    /// clock — keeps it there), and nothing between it and the right prompt.
    Same,
    /// A person typed after the mark: on its row, further right, the mark's
    /// text still in front — or wrapped onto the next row (a screen that
    /// scrolled keeps the cursor's row but puts the marked prompt, with the
    /// typing after it, on the row above) — or the cursor back on the mark
    /// with text to its right (a caret moved home, ctrl-a, over a half-typed
    /// command: a pure cursor move changes no screen generation, so the fence
    /// would have let the line in front of it, and Enter under both).
    Typed,
    /// Anything else: an async redraw of the prompt, a new prompt below.
    Moved,
}

impl PromptMark {
    pub(super) fn of(scr: &Screen) -> Option<PromptMark> {
        let (row, col) = scr.cursor?;
        let text = scr.rows.get(row)?;
        let left = text.chars().take(col).collect();
        let right = text
            .chars()
            .enumerate()
            .skip(col)
            .find(|(_, ch)| !ch.is_whitespace())
            .map(|(i, _)| i);
        Some(PromptMark {
            row: scr.first.saturating_add(row),
            col,
            left,
            right,
        })
    }

    pub(super) fn at(&self, scr: &Screen) -> AtPrompt {
        let Some(now) = PromptMark::of(scr) else {
            return AtPrompt::Moved;
        };
        if now.row == self.row && now.col == self.col {
            // The gap the mark saw blank, from the cursor up to its right
            // prompt: anything in it now is typing. A right prompt that grows
            // leftward into it reads as typing too — nothing is typed, the
            // safe way to be wrong.
            let text = now
                .row
                .checked_sub(scr.first)
                .and_then(|r| scr.rows.get(r))
                .map_or("", String::as_str);
            let end = self.right.unwrap_or(usize::MAX);
            let typed = text
                .chars()
                .enumerate()
                .skip(self.col)
                .take_while(|(i, _)| *i < end)
                .any(|(_, ch)| !ch.is_whitespace());
            return if typed {
                AtPrompt::Typed
            } else {
                AtPrompt::Same
            };
        }
        if now.row == self.row && now.col > self.col && now.left.starts_with(&self.left) {
            return AtPrompt::Typed;
        }
        let above = now
            .row
            .checked_sub(scr.first)
            .and_then(|r| r.checked_sub(1))
            .and_then(|r| scr.rows.get(r));
        if above.is_some_and(|row| {
            row.starts_with(&self.left) && row.chars().count() > self.left.chars().count()
        }) {
            return AtPrompt::Typed;
        }
        AtPrompt::Moved
    }

    /// The mark as the state file keeps it: `v2,<row>,<col>,<right|->,<text>`.
    pub(super) fn word(&self) -> String {
        format!(
            "v2,{},{},{},{}",
            self.row,
            self.col,
            self.right
                .map_or_else(|| "-".to_string(), |r| r.to_string()),
            self.left
        )
    }

    /// [`Self::word`] read back; anything else — an older build's mark,
    /// which knew nothing right of the cursor — is no mark, and the next
    /// attempt marks the prompt afresh ([`first_mark`]).
    pub(super) fn parse(word: &str) -> Option<PromptMark> {
        let mut parts = word.strip_prefix("v2,")?.splitn(4, ',');
        Some(PromptMark {
            row: parts.next()?.parse().ok()?,
            col: parts.next()?.parse().ok()?,
            right: match parts.next()? {
                "-" => None,
                r => Some(r.parse().ok()?),
            },
            left: parts.next()?.to_string(),
        })
    }
}

/// How many fenced tries the relaunch line gets ([`type_relaunch_line`]), and
/// how far apart, before it is pasted unfenced.
pub(super) const RELAUNCH_FENCE_TRIES: usize = 5;
const RELAUNCH_FENCE_EVERY: Duration = Duration::from_millis(400);

/// After the relaunch line: find the new process holding the conversation —
/// the relaunch itself ([`relaunch_of`]), nothing else. A parent that cannot
/// be read yet is not found yet: the wait goes on. Found, it is carried on
/// with ([`carry_on`]) — or, where a supervisor loop takes it
/// ([`Opts::hand_back`]), ADOPTED: the step ends there, the record stays in
/// flight, and the loop's next idle point types the continuation
/// ([`resume`]).
pub(super) fn await_new(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    session: &str,
    k: &dyn Kernel,
) -> Report {
    let home = opts.home.clone();
    let (old, shell) = (st.pid, st.shell);
    // A fresh start holds a conversation of its own: the shell's child is it.
    let fresh = st.cause == CAUSE_FRESH;
    let mut new: Option<SessionFile> = None;
    let mut roster = SessionRosterCache::default();
    wait_until(Duration::from_secs(90), || {
        new = roster.files(&home, Instant::now()).and_then(|files| {
            files
                .iter()
                .find(|s| {
                    if fresh {
                        s.pid != old && relaunch_by_parent(k.parent(s.pid), shell) == Relaunch::Ours
                    } else {
                        relaunch_of(k, s, session, old, shell) == Relaunch::Ours
                    }
                })
                .cloned()
        });
        new.is_some()
    });
    match new {
        Some(sf) if fresh => {
            // Nothing was said before it ended: nothing to carry on with.
            st.phase = upgrade::Phase::Done;
            let mut r = said(r, "done:fresh");
            r.pid = sf.pid;
            ledger(opts, &r, &sf.session_id);
            save(opts, session, st);
            r
        }
        Some(sf) if opts.hand_back => {
            let mut r = said(r, "adopted");
            r.pid = sf.pid;
            ledger(
                opts,
                &r,
                "the relaunched agent holds the conversation; its supervisor types the \
                 continuation at its first idle point",
            );
            save(opts, session, st);
            r
        }
        Some(sf) => carry_on(opts, r, st, c, session, &sf),
        None => {
            if let upgrade::Phase::Relaunched { at_s } = st.phase
                && now_s().saturating_sub(at_s) > STALE_S
            {
                st.stop("no-resume");
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

/// How much of a transcript past the restart's mark is read for the resumed
/// session's first turn. Measured 2026-09-24 (the owner's session, 2.1.281 to
/// 2.1.282): a queued task notification, three system rows and the
/// continuation came before it — a few KiB.
const SINCE_BYTES: u64 = 4 << 20;

/// How long after the continuation the resumed session's first answer may
/// still be what the model is confirmed from. NEVER WAITED FOR: the step that
/// types the continuation reads once and a later one
/// ([`super::upgrade_drive::confirmations`], run first in every sweep and
/// every host step) reads again, until this has passed. A sweep runs its
/// visits one after another and its orphan pass after all of them, so a visit
/// that blocked on the answer would age every restart left exiting toward
/// [`STALE_S`] — and the answer can take all of this (a long first thought,
/// retries, a usage limit that writes only `<synthetic>` rows). Measured
/// 2026-09-24: 10 s (a thinking block, the first row a turn writes); the
/// `turn` that types the continuation settles on the agent's screen, so the
/// first read usually has it.
pub(super) const MODEL_WAIT: Duration = Duration::from_secs(120);

/// The new process holds the conversation: once it settles, tell it to carry
/// on — with the upgrade's words, or with the relaunch's ([`resumed_prompt`])
/// when the record is one [`after_exit`] made — and read, once, which model
/// it came back on ([`confirm`]). `key` is the conversation the record is
/// filed under.
pub(super) fn carry_on(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    key: &str,
    new: &SessionFile,
) -> Report {
    carry_on_with_tab_probe(opts, r, st, c, key, new, MODEL_WAIT, |c, pid, tab| {
        process_in_tab(c, pid, tab, None, None)
    })
}

/// The tab probe is a seam for the race between the first ownership read and
/// the final typed continuation. Production uses the kernel-backed PTY claim
/// for BOTH reads; a test changes its answer after the first one.
/// `confirm_within` is how long after the continuation the resumed session's
/// first answer may still confirm the model ([`confirm`]): [`MODEL_WAIT`] in
/// production, short where a test proves the bound. It is never waited for.
#[allow(clippy::too_many_arguments)]
pub(super) fn carry_on_with_tab_probe(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    key: &str,
    new: &SessionFile,
    confirm_within: Duration,
    mut tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Report {
    r.pid = new.pid;
    let tab = st.tab.clone();
    let tab = tab.as_str();
    let home = opts.home.clone();
    let mut conversation_changed = false;
    let mut tail_available = true;
    let settled = wait_until(Duration::from_secs(120), || {
        let Some(sf) = session_file_of(&home, new.pid) else {
            return false;
        };
        if sf.session_id != key {
            conversation_changed = true;
            return true;
        }
        sf.status == "idle" && continuation_composer_empty(c, tab, &mut tail_available)
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
    let before = (!st.model_before.is_empty()).then(|| st.model_before.clone());
    let text = if relaunch_cause(&st.cause) {
        Some(resumed_prompt(&latest.version, &st.cause))
    } else {
        from.as_ref().zip(to.as_ref()).map(|(f, t)| {
            upgrade::continue_prompt_with_model(f, t, before.as_deref(), recorded(&st.model_list))
        })
    };
    let typed = settled
        && match text {
            Some(text) => {
                if !tab_probe(c, new.pid, tab) {
                    return said(r, "wait:tab-ownership-changed");
                }
                // A person at the keyboard wins, here too: the continuation
                // comes at the relaunched agent's first idle point, just when
                // a person who saw the crash is likeliest at the keys (the
                // hazards review of 2026-09-25: it was typed with no look).
                // Held, it stays owed: the next idle point types it.
                if super::upgrade_drive::held(c, tab, opts.human_grace_s) {
                    return said(r, "wait:held");
                }
                type_continuation(c, tab, &text)
            }
            None => false,
        };
    st.phase = upgrade::Phase::Done;
    r.from.clone_from(&st.from);
    r.to = format!("{}({})", latest.version, st.source);
    if !typed {
        let r = said(r, "done:no-continue");
        st.outcome = outcome_line(st, &latest.version, None);
        st.done_at = now_s();
        ledger(opts, &r, &st.outcome);
        save(opts, key, st);
        return r;
    }
    // TYPED, and on disk before anything else is read: a step that dies from
    // here on leaves a restart that is DONE, never one in flight that the next
    // step would carry on — telling the agent twice. What is still owed is
    // only the model after, read now and, while the answer may still come, by
    // a later step.
    st.resumed_on.clone_from(&latest.version);
    st.resumed_pid = new.pid;
    st.confirm_by = now_s().saturating_add(confirm_within.as_secs());
    save(opts, key, st);
    confirm(opts, &r, st, key, now_s()).unwrap_or_else(|| {
        let r = said(r, "continued");
        ledger(
            opts,
            &r,
            "the continuation is typed; the resumed session had not answered yet, and a later \
             step reads its model",
        );
        r
    })
}

/// The continuation, typed once, fenced on a fresh read whose composer is
/// still empty ([`typing_fence`], [`turn_fenced`]): a screen that moved
/// between that read and the paste — or a person typing through the yield —
/// is read again, at most [`CONTINUE_FENCE_TRIES`] times; a draft, a box or a
/// refusal types nothing and is not retried. A screen that never holds still
/// for the fence (a resumed Claude still redrawing its history, a ticking
/// status line) is typed UNFENCED after one more look at its composer, as
/// every continuation was before the fence: the restart is DONE either way,
/// and giving up here left a resumed session with no continuation at all
/// (the 2026-09-24 review) — the one outcome the fence must not buy.
fn type_continuation(c: &mut Client, tab: &str, text: &str) -> bool {
    for _ in 0..CONTINUE_FENCE_TRIES {
        let Ok(generation) = typing_fence(c, tab) else {
            return false;
        };
        match turn_fenced(c, tab, text, generation.as_deref()) {
            Ok(()) => return true,
            Err(Typed::Changed | Typed::Yielded) => std::thread::sleep(RELAUNCH_FENCE_EVERY),
            Err(Typed::Refused(_)) => return false,
        }
    }
    typing_fence(c, tab).is_ok() && turn(c, tab, text).is_ok()
}

/// How many fenced tries the continuation gets ([`type_continuation`]).
pub(super) const CONTINUE_FENCE_TRIES: usize = 3;

/// THE MODEL AFTER, for a restart whose continuation was typed: the resumed
/// session's first answer ([`resumed_model`]) is its `done` row, and so is
/// having none once `confirm_by` has passed (`done:model-unconfirmed`) — at
/// once when no mark was taken. Either ends the confirmation, saved, and is
/// `r` with that step. `None` while the answer may still come.
///
/// A model other than the one before is the expected outcome for a session
/// launched without --model (it takes the current default), so it changes
/// nothing but the words. Only a model that could not be confirmed changes
/// the step.
pub(super) fn confirm(opts: &Opts, r: &Report, st: &mut St, key: &str, now: u64) -> Option<Report> {
    let after = resumed_model(&opts.home, key, st.mark, &st.resumed_on);
    if after.is_none() && st.mark != 0 && now < st.confirm_by {
        return None;
    }
    let r = said(
        r.clone(),
        if after.is_some() {
            "done"
        } else {
            "done:model-unconfirmed"
        },
    );
    st.outcome = outcome_line(st, &st.resumed_on, after.as_deref());
    st.done_at = now;
    ledger(opts, &r, &st.outcome);
    st.confirm_by = 0;
    save(opts, key, st);
    Some(r)
}

/// The owner's outcome line ([`upgrade::restart_outcome`], or
/// [`upgrade::restart_outcome_listed`] for a relaunch that asked for a model
/// from the priority list) from what the restart recorded, the model after as
/// `after`.
fn outcome_line(st: &St, to: &str, after: Option<&str>) -> String {
    if !st.model_list.is_empty() {
        return upgrade::restart_outcome_listed(
            to,
            recorded(&st.model_before),
            after,
            &st.model_list,
        );
    }
    upgrade::restart_outcome(
        to,
        recorded(&st.model_before),
        after,
        recorded(&st.launch_model),
    )
}

/// A recorded field, `None` when it is empty (nothing was recorded).
fn recorded(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// THE RESUMED MODEL: the model the first assistant turn the relaunched build
/// `version` wrote past the restart's mark names
/// ([`upgrade::transcript_first_model`]) — the new process's own answer, since
/// the resumed conversation writes no assistant turn until it is asked. One
/// read, never a wait. `None`: no mark was taken, the transcript is not found,
/// or the session has not answered yet.
fn resumed_model(home: &Path, session: &str, mark: u64, version: &str) -> Option<String> {
    if mark == 0 {
        return None;
    }
    let path = transcript(home, session)?;
    upgrade::transcript_first_model(&since(&path, mark, SINCE_BYTES), version)
}

pub(super) fn continuation_session(home: &Path, pid: u32, key: &str) -> Option<SessionFile> {
    session_file_of(home, pid).filter(|sf| sf.session_id == key)
}

// ---------------------------------------------------------------- relaunch on exit

/// The [`St::cause`] of a relaunch [`after_exit`] files: its continuation
/// says the agent was relaunched, not upgraded.
pub(super) const CAUSE_EXIT: &str = "exit";

/// The [`St::cause`] of a relaunch that starts the agent afresh: its
/// conversation had no message, so there is none to resume
/// ([`super::upgrade_drive::transcript_exists`]) and nothing to carry on.
pub(super) const CAUSE_FRESH: &str = "exit-fresh";

/// The [`St::cause`] of a restart the host made for Claude Code's
/// critical-memory banner ([`restart_here`], [`Restart::Memory`]).
pub(super) const CAUSE_MEMORY: &str = "memory";

/// The [`St::cause`] of a relaunch after the stall's remedy ended an agent
/// that had stopped reading its input ([`after_exit`] with `stalled`, U1).
pub(super) const CAUSE_STALL: &str = "stall";

/// The [`St::cause`] of a restart onto a bucket's fallback model, `model:`
/// then the model ([`Restart::Model`]).
pub(super) const CAUSE_MODEL: &str = "model:";

/// The [`St::cause`] of a restart back from a bucket's fallback at its
/// reset, `model-back:` then the model, empty for the launch's own
/// ([`Restart::ModelBack`]).
pub(super) const CAUSE_MODEL_BACK: &str = "model-back:";

/// Whether a record of `cause` is a relaunch's — its continuation says why
/// it was relaunched ([`resumed_prompt`]) — rather than the upgrade's.
fn relaunch_cause(cause: &str) -> bool {
    cause == CAUSE_EXIT
        || cause == CAUSE_MEMORY
        || cause == CAUSE_STALL
        || cause.starts_with(CAUSE_MODEL)
        || cause.starts_with(CAUSE_MODEL_BACK)
}

/// THE CONTINUATION a relaunched agent is typed once it holds its
/// conversation again — why it was relaunched (`cause`: an exit nobody asked
/// for, [`CAUSE_EXIT`]; the host's restart for the memory banner,
/// [`CAUSE_MEMORY`]), then carry on, and decide for itself what it was
/// waiting on the user for ([`upgrade::CARRY_ON`]).
#[must_use]
pub fn resumed_prompt(version: &str, cause: &str) -> String {
    let session_only = "for this session only: your default model is unchanged";
    let why = if cause == CAUSE_MEMORY {
        "Restarted: Claude Code reported its memory critical, so this session was ended and \
         restarted"
            .to_string()
    } else if cause == CAUSE_STALL {
        "Relaunched: Claude Code stopped reading its input and was ended, so this session was \
         restarted"
            .to_string()
    } else if let Some(model) = cause.strip_prefix(CAUSE_MODEL) {
        format!(
            "Relaunched on {}: the model this session ran reached its usage limit, so it was \
             restarted with that model ({session_only})",
            one_line(model)
        )
    } else if let Some(model) = cause.strip_prefix(CAUSE_MODEL_BACK) {
        let model = if model.is_empty() {
            "the model it was launched with".to_string()
        } else {
            one_line(model)
        };
        format!(
            "Relaunched on {model}: the usage limit that moved this session to another model has \
             reset, so it was restarted back on it ({session_only})"
        )
    } else {
        "Relaunched: Claude Code exited without anyone asking, and this session was restarted"
            .to_string()
    };
    format!(
        "[aterm harness] {why} on Claude Code {} and resumed. {}",
        one_line(version),
        upgrade::CARRY_ON
    )
}

/// THE RESTART IN PLACE (D3 of the reconciliation of 2026-09-25): the Claude
/// Code in tab `opts.only_sid`, at its supervisor's idle point, ended and
/// relaunched on the SAME program on its own conversation — the host's
/// answer to a point nothing typed can answer ([`Restart`]). The upgrade's
/// restart with no announcement: nothing is typed into the agent first (a
/// process past saving reads nothing), and every last look the upgrade takes
/// before its one signal is taken here too — the same process, idle by
/// Claude's own record, its shell's foreground job on a terminal an aterm
/// tab owns and this tab alone claims, the composer empty, no background
/// work under it, nobody's hand, never this process's own ancestry. The
/// relaunch is [`relaunch`] itself, handed back to the loop that asked
/// (`adopted`), whose next idle point types the continuation with the
/// restart's reason ([`resumed_prompt`]). A restart left between the signal
/// and the new process is carried on first ([`carry_in_flight`]).
///
/// One step, one word: `adopted` (`relaunched` where no loop takes it),
/// `would-restart[:model=<m>]` in a dry run, `wait:<why>` (not now — asked again at a
/// later point), `busy:<why>` (another actor on the upgrade state),
/// `refused:<what>` or `failed:<why>` (never: the loop escalates).
#[must_use]
pub fn restart_here(opts: &Opts, why: &Restart) -> Report {
    restart_from(opts, why, snapshot(opts))
}

/// [`restart_here`] from `snap`, the tab's agent as [`snapshot`] read it (or
/// why it could not): the seam a test reads its own agent through.
pub(super) fn restart_from(
    opts: &Opts,
    why: &Restart,
    snap: Result<Snapshot, &'static str>,
) -> Report {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let mut r = Report {
        pid: 0,
        tab: tab.clone(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: String::new(),
    };
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    let snap = match snap {
        Ok(snap) if snap.tab == tab => snap,
        Ok(_) => return said(r, "wait:tab-identity-conflict"),
        Err(why) => return said(r, format!("wait:{why}")),
    };
    r.pid = snap.pid;
    // Claude's own record of THIS process names the conversation, the build
    // and whether it is idle.
    let Some(sf) = session_file_of(&opts.home, snap.pid)
        .filter(|sf| squash(&sf.proc_start) == snap.start)
        .filter(|sf| upgrade::is_session_id(&sf.session_id))
    else {
        return said(r, "wait:no-session-record");
    };
    let session = sf.session_id.clone();
    r.session.clone_from(&session);
    r.from.clone_from(&sf.version);
    r.to = format!("{}(same)", sf.version);
    let prior = load(opts, &session);
    if let Some(st) = prior.clone().filter(St::in_flight) {
        if st.tab != tab {
            return said(r, "wait:conversation-in-other-tab");
        }
        return carry_in_flight(opts, r, st, &session, &tab);
    }
    if upgrade::one_shot(&snap.argv) {
        return said(r, "refused:one-shot");
    }
    let t = table();
    if is_our_ancestor(snap.pid, &t) {
        return said(r, "refused:self");
    }
    // The model the line asks for: none for the memory banner (the launch's
    // own flags); the bucket's fallback; at its reset the bucket's model, or
    // — the notice named none — the model the launch named before the
    // fallback's relaunch replaced it (its record kept it), else none.
    let launched = upgrade::launch_model(&snap.argv).unwrap_or_default();
    // The memory banner's restart carries the model the priority list moves
    // the conversation to, when one is due: every restart is its moment.
    let riding = match why {
        Restart::Memory => riding_model(opts, &sf, &snap.argv),
        _ => None,
    };
    let (model, cause, launch_model) = match why {
        Restart::Memory => (riding.clone(), CAUSE_MEMORY.to_string(), launched),
        Restart::Model { to } => (Some(to.clone()), format!("{CAUSE_MODEL}{to}"), launched),
        Restart::ModelBack { to } => {
            let before = prior
                .as_ref()
                .filter(|st| st.cause.starts_with(CAUSE_MODEL))
                .map(|st| st.launch_model.clone())
                .unwrap_or_default();
            let back = to.clone().unwrap_or(before);
            (
                Some(back.clone()),
                format!("{CAUSE_MODEL_BACK}{back}"),
                launched,
            )
        }
    };
    let mode = resume_mode(opts, &tab, &session);
    let launch = Launch {
        session: &session,
        resume: transcript_exists(&opts.home, &session) != Some(false),
        cwd: &sf.cwd,
        agent: snap.pid,
        argv: &snap.argv,
        exe: &snap.program,
        model: model.as_deref(),
        mode: mode.as_deref(),
    };
    let mut st = St {
        from: sf.version.clone(),
        to: sf.version.clone(),
        source: "same".to_string(),
        cause,
        salt: now_s(),
        launch_model,
        model_list: riding.clone().unwrap_or_default(),
        ..St::default()
    };
    let Plan { shell, line, .. } = match plan(opts, &launch, snap.shell, &t) {
        Ok(p) => p,
        Err(no) => return unplanned(opts, r, &mut st, no),
    };
    if opts.dry_run {
        return said(
            r,
            match &model {
                Some(m) => format!("would-restart:model={m}"),
                None => "would-restart".to_string(),
            },
        );
    }
    let Ok(mut c) = connect(opts, &tab) else {
        return said(r, "wait:no-socket");
    };
    // The last look before the one irreversible act: the same process, idle
    // by its own record, its shell's foreground job on the tab's terminal,
    // the composer empty, nothing running under it, nobody's hand.
    let again = session_file_of(&opts.home, snap.pid);
    let still = again.is_some_and(|a| {
        a.session_id == session
            && a.status == "idle"
            && kernel_start(snap.pid).as_deref() == Some(squash(&a.proc_start).as_str())
    }) && foreground_shell(Live.job(snap.pid)) == Ok(shell)
        && Live
            .terminal(snap.pid)
            .as_ref()
            .is_some_and(|(p, name)| owned_by_aterm((*p, name.as_str())))
        && screen(&mut c, &tab).is_some_and(|scr| composer_empty(&mut c, &tab, &scr))
        && background(snap.pid, &t).is_empty()
        && !held(&mut c, &tab, opts.human_grace_s);
    if !still {
        return said(r, "wait:changed");
    }
    if let Err(why) = require_unique_owner(&opts.home, &sf) {
        return said(r, format!("wait:{why}"));
    }
    let Ok(pid) = i32::try_from(snap.pid) else {
        return said(r, "wait:pid");
    };
    // The model the agent ran and where its transcript ended: the resumed
    // session's answer past it says which model it came back on.
    let recent = transcript(&opts.home, &session).map(|p| tail_to_end(&p, TAIL_BYTES));
    (st.model_before, st.mark) = model_and_mark(recent.as_ref());
    st.pid = snap.pid;
    st.shell = shell;
    st.tab.clone_from(&tab);
    st.line = line;
    st.phase = upgrade::Phase::Exiting { at_s: now_s() };
    save(opts, &session, &st);
    if let Some(m) = &riding {
        record_asked(opts, &session, m);
    }
    match terminate(&mut c, &tab, pid, opts.human_grace_s) {
        Terminated::Sent => {}
        Terminated::Refused(no) => {
            // Nothing was sent: no restart is in flight, and the record is
            // what it was (an upgrade's, or none).
            restore(opts, &session, prior.as_ref());
            return said(r, format!("wait:signal-{no}"));
        }
        Terminated::Failed => {
            restore(opts, &session, prior.as_ref());
            return said(r, "failed:signal-refused");
        }
    }
    ledger(
        opts,
        &said(r.clone(), format!("restart:{}", why.word())),
        "SIGTERM",
    );
    let r = relaunch(opts, r, &mut st, &mut c, &session, &Live);
    save(opts, &session, &st);
    r
}

/// What a relaunch needs, read while the agent runs: after it ends, nothing
/// of it can be read again (its argv, its parent and its start time go with
/// the process).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The tab (`s-<hex>`).
    pub tab: String,
    /// The agent: the tab's foreground job.
    pub pid: u32,
    /// Its kernel start, as Claude renders `procStart`: the pid-reuse guard.
    pub start: String,
    /// The job-control shell it is the foreground job of.
    pub shell: u32,
    /// What it runs ([`launched`]): an absolute path.
    pub program: PathBuf,
    /// Its argv with `program` as `argv[0]`.
    pub argv: Vec<String>,
    /// Its conversation, when Claude had registered it.
    pub session: Option<String>,
    /// The directory it was started in.
    pub cwd: String,
    /// Its version, when Claude had registered it.
    pub version: Option<String>,
}

/// The program a Claude Code process runs and the args it was given:
/// `(program, argv with program first)`.
///
/// A SCRIPT run by its `#!` interpreter shows the interpreter as the kernel's
/// image and the script as `argv[1]` — aterm's managed twin (`#!/bin/sh`,
/// until it `exec`s the store build), an npm install (`#!/usr/bin/env
/// node`) — so an absolute `argv[1]` named `claude` is the program. Otherwise
/// the kernel's image is, when it is Claude's: named `claude`, under the
/// native installer's versions directory, or `registered` (Claude wrote its
/// own `sessions/<pid>.json` for this very process). Anything else — a
/// wrapper that runs Claude as a child (`caffeinate claude`) — is no program
/// this module relaunches.
#[must_use]
pub fn launched(
    argv: &[String],
    image: Option<&Path>,
    native: &Path,
    registered: bool,
) -> Option<(PathBuf, Vec<String>)> {
    let is_claude = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .and_then(aterm_phase::program_of)
            == Some(aterm_phase::Program::Claude)
    };
    if let Some(script) = argv.get(1).map(Path::new)
        && script.is_absolute()
        && is_claude(script)
    {
        let mut out = vec![script.to_string_lossy().into_owned()];
        out.extend(argv[2..].iter().cloned());
        return Some((script.to_path_buf(), out));
    }
    let image = image.filter(|p| p.is_absolute())?;
    if !(is_claude(image) || image.starts_with(native) || registered) {
        return None;
    }
    let mut out = vec![image.to_string_lossy().into_owned()];
    out.extend(argv.iter().skip(1).cloned());
    Some((image.to_path_buf(), out))
}

/// Read the [`Snapshot`] of the Claude Code in tab `opts.only_sid` of the
/// instance on `opts.sock`, or the one word that says why not: the tab's
/// foreground job must be a job-control shell's foreground job on a terminal
/// an aterm tab owns, claimed by this tab alone, and a Claude program.
///
/// # Errors
/// The word naming the first fact that could not be read or did not hold.
pub fn snapshot(opts: &Opts) -> Result<Snapshot, &'static str> {
    let tab = opts.only_sid.as_deref().ok_or("no-tab")?;
    let mut c = connect(opts, tab).map_err(|_| "no-socket")?;
    let tabs = host_roster(&mut c).ok_or("roster")?;
    let group = tabs
        .iter()
        .find(|t| t.sid == tab)
        .and_then(|t| t.fgpgid)
        .ok_or("no-foreground")?;
    if unique_tab_for_group(&tabs, group) != Some(tab) {
        return Err("tab-ambiguous");
    }
    // A job-control shell starts each job in a group of its own, led by the
    // job's first process: the agent, or the twin that execs it in place.
    let pid = u32::try_from(group).map_err(|_| "no-foreground")?;
    let shell = match Live.job(pid) {
        Some((Job::NoJobControl, _)) => return Err("not-a-shell-job"),
        job => foreground_shell(job)?,
    };
    let owner = Live.terminal(pid);
    if !owner
        .as_ref()
        .is_some_and(|(p, name)| owned_by_aterm((*p, name.as_str())))
    {
        return Err("terminal");
    }
    let args = atpkg::caller_shell::process_args(pid).ok_or("argv-unreadable")?;
    if args
        .env_var("ATERM_PARENT_SESSION_ID")
        .is_some_and(|env_tab| env_tab != tab)
    {
        return Err("tab-identity-conflict");
    }
    let start = kernel_start(pid).ok_or("start")?;
    let file = session_file_of(&opts.home, pid).filter(|sf| squash(&sf.proc_start) == start);
    let image = exe_of(pid);
    let (program, argv) = launched(
        &args.argv,
        image.as_deref(),
        &native_root(&opts.home),
        file.is_some(),
    )
    .ok_or("not-claude")?;
    let cwd = file
        .as_ref()
        .map(|sf| sf.cwd.clone())
        .filter(|c| !c.is_empty())
        .or_else(|| cwd_of(pid))
        .unwrap_or_default();
    Ok(Snapshot {
        tab: tab.to_string(),
        pid,
        start,
        shell,
        program,
        argv,
        session: file.as_ref().map(|sf| sf.session_id.clone()),
        cwd,
        version: file.map(|sf| sf.version),
    })
}

/// Where the tab's foreground job stands against a [`Snapshot`]: which
/// process group the tab's program is published for now (`fg`: its
/// `program_pgid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Foreground {
    /// The snapshot's agent still holds the tab: follow its record.
    Agent,
    /// The shell holds the tab again: the agent left and its exit is still
    /// to be handled — the snapshot is what that needs, kept as it is.
    Shell,
    /// Another job holds the tab (a person ran another agent, a wrapper, a
    /// script): the snapshot describes nothing there any more.
    Other,
}

/// [`Foreground`] of `snap` when the tab's program is published for process
/// group `fg`.
#[must_use]
pub fn foreground(snap: &Snapshot, fg: i64) -> Foreground {
    if fg == i64::from(snap.pid) {
        Foreground::Agent
    } else if fg == i64::from(snap.shell) {
        Foreground::Shell
    } else {
        Foreground::Other
    }
}

/// FOLLOW the snapshot's agent while it runs: its conversation, build and
/// directory as Claude's own record of that very process (the same pid and
/// kernel start) names them now — an in-app `/clear` or `/resume` moves the
/// conversation under the same process, and a relaunch must resume the one
/// it holds at its exit, not the one it held at the snapshot. One small
/// file read, no socket and no `ps`. Whether anything moved.
pub fn follow(home: &Path, snap: &mut Snapshot) -> bool {
    let Some(sf) =
        session_file_of(home, snap.pid).filter(|sf| squash(&sf.proc_start) == snap.start)
    else {
        return false;
    };
    let before = snap.clone();
    if upgrade::is_session_id(&sf.session_id) {
        snap.session = Some(sf.session_id);
    }
    if !sf.version.is_empty() {
        snap.version = Some(sf.version);
    }
    if !sf.cwd.is_empty() {
        snap.cwd = sf.cwd;
    }
    *snap != before
}

/// What a relaunch on exit runs, `(exe, version, source)`: with `upgrade` —
/// the host's `[harness] upgrade` switch, live — the newest build the
/// session may move to ([`upgrade::choose_target`] over `candidates`, so a
/// crash is also an upgrade), else the program it ran (`same`). Configuration
/// only takes power away: with the switch off, a crash never moves the
/// session onto another build.
pub(super) fn relaunch_target(
    upgrade: bool,
    running: Option<&Version>,
    candidates: &[upgrade::Candidate],
    program: &Path,
    version: Option<&str>,
) -> (PathBuf, String, String) {
    let target = running
        .filter(|_| upgrade)
        .and_then(|v| upgrade::choose_target(v, candidates));
    match target {
        Some(t) => (t.exe, t.version.to_string(), t.source.as_str().to_string()),
        None => (
            program.to_path_buf(),
            version.unwrap_or("-").to_string(),
            "same".to_string(),
        ),
    }
}

// ------------------------------------------------ what the exit left behind

/// WHAT THE EXIT LEFT of Claude's own record of the agent
/// (`sessions/<pid>.json`, of the same pid and kernel start): read ONCE, as
/// the exit is seen ([`exit_record`]), and kept for every attempt at the
/// relaunch ([`after_exit`]). A crash, an OOM kill or a SIGKILL leaves the
/// record behind; a graceful exit removes it. But ANY Claude Code that starts
/// later removes dead processes' records too (measured 2026-09-26 on Claude
/// Code 2.1.283: the record of a SIGKILLed agent gone 0.79 s after its kill,
/// the moment a Claude in another tab started), so a record read after the
/// back-off (1 s, 10 s, 60 s, ten minutes) said "graceful" of a crash, and
/// the crash was silently never relaunched (D2 of that day's live test).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitRecord {
    /// Nothing was read at the exit: the same process still ran through the
    /// look ([`EXIT_GONE`]: stopped, or not yet reaped), nothing could be
    /// read, or the look was cut short. The attempt reads the record itself.
    Unread,
    /// The record SURVIVED the exit — a crash — as it was read then: it
    /// names the conversation as the agent last held it.
    Survived(SessionFile),
    /// The exit REMOVED it: a graceful exit, someone's decision (or a launch
    /// that never registered a conversation).
    Removed,
}

impl ExitRecord {
    /// One word for the host's log.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Survived(_) => "survived",
            Self::Removed => "removed",
        }
    }
}

/// ONE LOOK at an agent that left its tab ([`look_at_exit`]): whether the
/// same process (its pid and kernel start) still runs, and Claude's own
/// record of it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitLook {
    /// The snapshot's process still runs.
    pub running: bool,
    /// Its record, when one of its own start is on disk.
    pub record: Option<SessionFile>,
}

/// How long after the look at an exit starts the exit itself may still be
/// removing its own record: a graceful `/exit` removes it in its last
/// moments, around when its exit is seen (measured 2026-09-26, Claude Code
/// 2.1.283: gone 0.02 s after the `/exit`, before its process had ended),
/// so a record read at the exact instant could call a clean exit a crash.
/// The only window left in which another Claude Code's start can turn a
/// crash into a graceful exit: a quarter second, where the back-off it
/// replaces was a second to ten minutes.
pub const EXIT_SETTLE: Duration = Duration::from_millis(250);

/// How often [`exit_record`] looks.
pub const EXIT_LOOK: Duration = Duration::from_millis(25);

/// How long a process that still runs after its exit was seen (a job
/// stopped, a zombie its shell has not reaped) is looked at before the read
/// is left to the attempt ([`ExitRecord::Unread`]).
pub const EXIT_GONE: Duration = Duration::from_secs(2);

/// [`ExitLook`] of `snap`'s agent, live: `running` read FIRST — a graceful
/// exit removes its record before its process ends, so a record read after
/// the process is seen gone is one its exit left behind.
#[must_use]
pub fn look_at_exit(home: &Path, snap: &Snapshot) -> ExitLook {
    let running = alive(snap.pid) && kernel_start(snap.pid).as_deref() == Some(snap.start.as_str());
    let record = session_file_of(home, snap.pid).filter(|sf| squash(&sf.proc_start) == snap.start);
    ExitLook { running, record }
}

/// READ WHAT THE EXIT LEFT, once, as it is seen — independent of the
/// relaunch's back-off. `look` is one [`ExitLook`] (`None`: nothing can be
/// read), `wait` waits one [`EXIT_LOOK`] and says whether it waited it all
/// (`false`: cut short — the agent is back, or the worker is stopped).
///
/// A look with the process gone and no record decides at once: a removal is
/// final, and nothing brings a dead process's record back. One with the
/// record still there decides only once [`EXIT_SETTLE`] has passed since the
/// first look, so an exit still removing its own record is never read as a
/// crash; one with the process still running waits up to [`EXIT_GONE`].
pub fn exit_record(
    mut look: impl FnMut() -> Option<ExitLook>,
    mut wait: impl FnMut(Duration) -> bool,
) -> ExitRecord {
    let mut waited = Duration::ZERO;
    loop {
        let Some(now) = look() else {
            return ExitRecord::Unread;
        };
        if now.running {
            if waited >= EXIT_GONE {
                return ExitRecord::Unread;
            }
        } else {
            match now.record {
                None => return ExitRecord::Removed,
                Some(sf) if waited >= EXIT_SETTLE => return ExitRecord::Survived(sf),
                Some(_) => {}
            }
        }
        if !wait(EXIT_LOOK) {
            return ExitRecord::Unread;
        }
        waited += EXIT_LOOK;
    }
}

/// RELAUNCH ON EXIT: the agent `snap` recorded has left its tab, and the
/// host decided nobody owns the exit ([`on_exit`]). Relaunch it on its
/// conversation in the same tab — on the newest build it may move to while
/// `upgrade` (the host's `[harness] upgrade`, live) allows it, else on the
/// program it ran ([`relaunch_target`]) — then tell it to carry on. One step,
/// like the upgrade's: the step's word says what happened ([`outcome`] reads
/// it). `left`: what the exit left of Claude's own record, read as the exit
/// was seen ([`exit_record`]) — a crash or a graceful exit is decided on
/// THAT, never on the record as it stands after the back-off, which any
/// other Claude Code's start may have removed since. `stalled`: the agent
/// had stopped reading its input — its supervisor held for the stall the
/// server published — so the exit was the stall's remedy (`signal term`,
/// whose handler may end it gracefully, or `signal kill`), never a person's
/// or an orchestrator's decision: relaunched even with its record removed,
/// the continuation saying why (U1).
#[must_use]
pub fn after_exit(
    opts: &Opts,
    snap: &Snapshot,
    left: &ExitRecord,
    upgrade: bool,
    stalled: bool,
) -> Report {
    let mut r = Report {
        pid: snap.pid,
        tab: snap.tab.clone(),
        session: snap.session.clone().unwrap_or_else(|| "-".to_string()),
        from: snap.version.clone().unwrap_or_else(|| "-".to_string()),
        to: "-".to_string(),
        step: String::new(),
    };
    // ONE ACTOR on the upgrade state at a time, the upgrade and a hand-run
    // `aterm harness upgrade` included.
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    // The same process still runs (suspended, or put in the background, by
    // someone): it did not exit, and there is nothing to relaunch.
    if alive(snap.pid) && kernel_start(snap.pid).as_deref() == Some(snap.start.as_str()) {
        return said(r, "wait:not-exited");
    }
    // A one-shot run ended as it was launched to: nothing to bring back.
    if upgrade::one_shot(&snap.argv) {
        return said(r, "ended:one-shot");
    }
    // Claude's own record when the exit left it — a crash cannot remove it,
    // and it names the conversation as the agent last held it — else the
    // snapshot's. As it was read AT THE EXIT: by now, after the back-off,
    // any Claude Code that started since may have removed a crash's record
    // (D2). Only an exit nothing could read then is read now.
    let survivor = match left {
        ExitRecord::Survived(sf) => Some(sf.clone()),
        ExitRecord::Removed => None,
        ExitRecord::Unread => {
            session_file_of(&opts.home, snap.pid).filter(|sf| squash(&sf.proc_start) == snap.start)
        }
    };
    let Some(session) = survivor
        .as_ref()
        .map(|sf| sf.session_id.clone())
        .or_else(|| snap.session.clone())
        .filter(|s| upgrade::is_session_id(s))
    else {
        // No record names a conversation: the launch never registered one (a
        // subcommand, `claude mcp list`), or it removed its record as it
        // ended by itself. Either way its end was its own.
        return said(r, "ended:no-conversation");
    };
    r.session.clone_from(&session);
    let version = survivor
        .as_ref()
        .map(|sf| sf.version.clone())
        .or_else(|| snap.version.clone());
    if let Some(v) = &version {
        r.from.clone_from(v);
    }
    let cwd = survivor
        .as_ref()
        .map(|sf| sf.cwd.clone())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| snap.cwd.clone());
    // A relaunch already in flight for this conversation is carried on, not
    // typed twice. One too old to act on is closed on the record instead, and
    // this step makes a fresh one: the host decided just now that nobody is
    // at the tab, which is what the age limit stood in for.
    if let Some(mut st) = load(opts, &session).filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != snap.tab {
            return said(r, "wait:conversation-in-other-tab");
        }
        // A record of ANOTHER process than the one that just left: that
        // relaunch landed (its agent is the one that left, adopted and gone
        // again before its continuation) or was overtaken — closed, and this
        // exit relaunched afresh.
        let overtaken = (st.pid != snap.pid).then_some((
            "exited-before-continuing",
            "the relaunched agent exited before its continuation was typed",
        ));
        if let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s())) {
            if !opts.dry_run {
                st.phase = upgrade::Phase::Failed(why.to_string());
                ledger(opts, &said(r.clone(), format!("failed:{why}")), detail);
                save(opts, &session, &st);
            }
        } else {
            return carry_in_flight(opts, r, st, &session, &snap.tab);
        }
    }
    // A GRACEFUL exit was someone's decision: Claude removes its own record
    // as it ends — a person's `/exit` or ctrl-d, an orchestrator's `/exit`
    // over the control socket, a `kill` from elsewhere — and a crash, an OOM
    // kill or a SIGKILL cannot. Only an exit that left the record behind —
    // read as the exit was seen, above — is relaunched (the hazards review
    // of 2026-09-25: a worker its manager ended over the socket read as
    // "nobody asked" — no window keystroke, no named hand — and was
    // relaunched on its conversation a second later).
    // The upgrade's own SIGTERM is the record in flight, carried on above;
    // the stall's remedy is no one's decision about the session (U1).
    if survivor.is_none() && !stalled {
        return said(r, "ended:graceful-exit");
    }
    if !alive(snap.shell) {
        return said(r, "refused:shell-gone");
    }
    let running = version.as_deref().and_then(Version::parse);
    let candidates = if upgrade {
        let native = native_root(&opts.home);
        Targets::read(&opts.home).for_session(snap.program.starts_with(&native))
    } else {
        Vec::new()
    };
    let (exe, to, source) = relaunch_target(
        upgrade,
        running.as_ref(),
        &candidates,
        &snap.program,
        version.as_deref(),
    );
    r.to = format!("{to}({source})");
    // A conversation with no message has no transcript and cannot be
    // resumed: the agent is started afresh, with the same flags. One whose
    // transcripts cannot be read is resumed (Claude says if it cannot).
    let resume = transcript_exists(&opts.home, &session) != Some(false);
    // The model the priority list moves the conversation to rides the
    // relaunch when one is due — read off the crash's own record (a graceful
    // exit left none to read it by).
    let riding = survivor
        .as_ref()
        .filter(|_| resume)
        .and_then(|sf| riding_model(opts, sf, &snap.argv));
    let mode = resume_mode(opts, &snap.tab, &session);
    let launch = Launch {
        session: &session,
        resume,
        cwd: &cwd,
        agent: snap.pid,
        argv: &snap.argv,
        exe: &exe,
        model: riding.as_deref(),
        mode: mode.as_deref(),
    };
    let mut st = St {
        from: version.unwrap_or_default(),
        to,
        source,
        cause: match (resume, stalled) {
            (false, _) => CAUSE_FRESH,
            (true, true) => CAUSE_STALL,
            (true, false) => CAUSE_EXIT,
        }
        .to_string(),
        salt: now_s(),
        launch_model: upgrade::launch_model(&snap.argv).unwrap_or_default(),
        model_list: riding.clone().unwrap_or_default(),
        ..St::default()
    };
    if resume {
        // The model the agent last ran and where its transcript ended: the
        // resumed session's answer past it says which model it came back on.
        let recent = transcript(&opts.home, &session).map(|p| tail_to_end(&p, TAIL_BYTES));
        (st.model_before, st.mark) = model_and_mark(recent.as_ref());
    }
    let Plan { shell, line, .. } = match plan(opts, &launch, snap.shell, &table()) {
        Ok(p) => p,
        Err(no) => return unplanned(opts, r, &mut st, no),
    };
    if opts.dry_run {
        return said(
            r,
            if resume {
                "would-relaunch"
            } else {
                "would-relaunch:fresh"
            },
        );
    }
    let Ok(mut c) = connect(opts, &snap.tab) else {
        return said(r, "wait:no-socket");
    };
    if !roster(&mut c).contains(&snap.tab) {
        return said(r, "wait:tab-not-live");
    }
    st.pid = snap.pid;
    st.shell = shell;
    st.tab.clone_from(&snap.tab);
    st.line = line;
    st.phase = upgrade::Phase::Exiting { at_s: now_s() };
    save(opts, &session, &st);
    if let Some(m) = &riding {
        record_asked(opts, &session, m);
    }
    ledger(opts, &said(r.clone(), "exited"), &st.line);
    let r = relaunch(opts, r, &mut st, &mut c, &session, &Live);
    save(opts, &session, &st);
    r
}

/// A relaunch in flight — this module's or the upgrade's, left between the
/// agent's end and the new process — carried on from where it stopped.
fn carry_in_flight(opts: &Opts, r: Report, mut st: St, session: &str, tab: &str) -> Report {
    if opts.dry_run {
        return said(r, format!("would-resume:{}", st.phase.word()));
    }
    let Ok(mut c) = connect(opts, tab) else {
        return said(r, "wait:no-socket");
    };
    let r = match st.phase {
        upgrade::Phase::Exiting { .. } => relaunch(opts, r, &mut st, &mut c, session, &Live),
        _ => await_new(opts, r, &mut st, &mut c, session, &Live),
    };
    save(opts, session, &st);
    r
}

/// THE CONTINUATION OWED to tab `opts.only_sid`: a relaunch its supervisor
/// ADOPTED (`adopted`, [`Opts::hand_back`]) and has not carried on yet, or a
/// typed continuation whose resumed model is still to be read
/// ([`confirm`]). The host parks the tab's loop for it at its next idle
/// point, whatever `[harness] upgrade` says — a relaunch on exit owes it as
/// much as an upgrade does. Read-only: the state files alone.
#[must_use]
pub fn owed(opts: &Opts) -> bool {
    owed_record(opts).is_some()
}

/// The conversation and record [`owed`] finds for tab `opts.only_sid`.
fn owed_record(opts: &Opts) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let dir = std::fs::read_dir(state_dir(opts)).ok()?;
    dir.flatten().find_map(|e| {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            return None;
        }
        let session = path.file_stem()?.to_string_lossy().into_owned();
        let st = load(opts, &session)?;
        let due = matches!(st.phase, upgrade::Phase::Relaunched { .. }) || st.confirming();
        (st.tab == tab && due).then_some((session, st))
    })
}

/// THE RELAUNCHED AGENT CARRIED ON, at its loop's idle point: the step the
/// host takes where the tab's loop parked for an [`owed`] continuation — the
/// new process found again ([`await_new`]) and told to carry on
/// ([`carry_on`], at once: the loop parked at idle, having answered whatever
/// the new process opened with), or a typed continuation's model read
/// ([`confirm`], `wait:model` while its answer may still come). Under the
/// one lock every actor on the upgrade state takes.
#[must_use]
pub fn resume(opts: &Opts) -> Report {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let r = Report {
        pid: 0,
        tab: tab.clone(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: String::new(),
    };
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    let Some((session, mut st)) = owed_record(opts) else {
        return said(r, "current");
    };
    let mut r = Report {
        session: session.clone(),
        from: st.from.clone(),
        to: format!("{}({})", st.to, st.source),
        ..r
    };
    if st.confirming() {
        r.pid = st.resumed_pid;
        r.to = format!("{}({})", st.resumed_on, st.source);
        return confirm(opts, &r, &mut st, &session, now_s())
            .unwrap_or_else(|| said(r, "wait:model"));
    }
    let carry = Opts {
        hand_back: false,
        background: false,
        ..opts.clone()
    };
    // A Codex restart's continuation is the Codex branch's to type: the same
    // step, over the TUI its relaunch line brought back.
    if st.agent == upgrade::Agent::Codex {
        return super::upgrade_drive::codex_carry_in_flight(&carry, r, st, &session);
    }
    carry_in_flight(&carry, r, st, &session, &tab)
}

/// What one relaunch step came to, for the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The new process holds the conversation.
    Relaunched,
    /// Nothing to relaunch and nothing to say, and why: the exit was the end
    /// the launch was made for — a one-shot run (`-p`, `--version`), a
    /// subcommand or any launch that never registered a conversation, a
    /// launch nothing could be read of while it ran (a wrapper, a script) —
    /// or there was no exit at all (suspended or backgrounded by someone).
    /// Journaled; the tab is left as it is.
    Left(String),
    /// Not now, and why (a busy tab, a shell not back yet, a relaunch that
    /// did not register): the host tries again after its back-off.
    NotYet(String),
    /// Another actor of aterm's own holds the upgrade state (an upgrade
    /// step, a hand-run `aterm harness upgrade`, another tab's relaunch):
    /// nothing is wrong, so it is no miss — the host looks again after
    /// [`BUSY`], and neither the back-off nor the attention moves.
    Busy,
    /// Never, and why: an interactive conversation that should come back and
    /// cannot — a launch that cannot be carried, a shell that is gone — the
    /// IRREDUCIBLE case, the one a person is asked about (keyed attention).
    Cannot(String),
}

/// [`Outcome`] from one [`after_exit`] step's word.
#[must_use]
pub fn outcome(step: &str) -> Outcome {
    // The new process holds the conversation: `adopted` (its loop types the
    // continuation at its first idle point, [`resume`]) or `done:fresh` (a
    // fresh start, nothing to carry on). The host hands every relaunched
    // agent to its loop ([`Opts::hand_back`]), so no other word lands — and
    // a carry-on that typed nothing (`done:no-continue`) is never one.
    if matches!(step, "adopted" | "done:fresh") {
        Outcome::Relaunched
    } else if step == "wait:not-exited" {
        Outcome::Left("not-exited".to_string())
    } else if let Some(why) = step.strip_prefix("ended:") {
        Outcome::Left(why.to_string())
    } else if let Some(why) = step.strip_prefix("refused:") {
        // `refused:shell-gone` included: the one word a gone shell gives.
        Outcome::Cannot(why.to_string())
    } else if step == "busy:another-sweep" {
        Outcome::Busy
    } else if let Some(why) = step.strip_prefix("busy:") {
        // The state directory cannot hold the lock at all: no later try
        // gets past that.
        Outcome::Cannot(why.to_string())
    } else {
        Outcome::NotYet(step.to_string())
    }
}

/// What the host does when a supervised agent has left its tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnExit {
    /// A person typed into the session within `human_grace_s` of the exit:
    /// the exit is theirs (`/exit`, ctrl-c twice, ctrl-d), and the tab is
    /// left to them.
    PersonAsked,
    /// The session is HELD: halted (`hold=1`) or in someone's hand — a drive
    /// lease, or a turn a named driver typed ([`held_by_someone`]). The exit belongs to
    /// that holder (an orchestrator that ended its worker), never relaunched
    /// against it.
    Held,
    /// Nobody owns the exit, but it is not relaunched: `[harness] relaunch =
    /// false` (configuration took the power away), or an agent the relaunch
    /// is not built for yet (Codex: a capability missing, not a limit). Said
    /// once on the session's attention, as what it is.
    Limited,
    /// Nobody asked: relaunch it.
    Relaunch,
}

/// THE DECISION: whose exit this was. `allowed` is `[harness] relaunch` for
/// an agent the relaunch is written for; `status` the session's `status`
/// reply (`None`: unreadable — nobody, as an absent field is); `grace_s` is
/// `[harness] human_grace_s`; `stalled`: the agent had stopped reading its
/// input when it exited (its supervisor held for the stall), so a person's
/// keystroke just before was no `/exit` — nothing read it — and the exit is
/// the stall's remedy's (U1). A person or a holder owns the exit before the
/// owner's limit is asked about, so their own exit is never said to them.
#[must_use]
pub fn on_exit(allowed: bool, status: Option<&str>, grace_s: u32, stalled: bool) -> OnExit {
    if !stalled && status.is_some_and(|s| person_present(s, grace_s)) {
        OnExit::PersonAsked
    } else if status.is_some_and(held_by_someone) {
        OnExit::Held
    } else if !allowed {
        OnExit::Limited
    } else {
        OnExit::Relaunch
    }
}

/// Whether one `status` reply says the session is HELD by someone the exit
/// belongs to: `hold=1`, a drive lease (`hand=lease:…`), or a turn a named
/// driver typed (`hand=turn:<id>:<driver>`). A turn with no driver named
/// (`hand=turn:<id>`) is an owner-class caller's — this host's own loop
/// types its continuations so — and a session's `driving:` hand is about
/// the sessions IT drives: neither holds this one. (The act's last look
/// before it types, [`super::upgrade_drive`]'s `held`, is stricter: any
/// hand at all, and a person.)
#[must_use]
pub fn held_by_someone(status: &str) -> bool {
    status.split_whitespace().any(|t| t == "hold=1") || driver_hand(status)
}

/// Whether one `status` reply names ANOTHER DRIVER'S hand on the session: a
/// drive lease (`hand=lease:<holder>`) or a turn a named driver typed
/// (`hand=turn:<id>:<holder>`) — never an owner-class turn, which names
/// nobody (the supervisor's own continuations are typed so).
#[must_use]
pub fn driver_hand(status: &str) -> bool {
    status.split_whitespace().any(|t| {
        t.strip_prefix("hand=").is_some_and(|hand| {
            hand.starts_with("lease:")
                || hand
                    .strip_prefix("turn:")
                    .is_some_and(|turn| turn.contains(':'))
        })
    })
}

/// Whether a PERSON typed into the session within `grace_s` seconds, from
/// one `status` reply's stamp ([`HumanInput::of_status`], tested by
/// [`HumanInput::within`]): a person at the keyboard wins, and the
/// supervisor's hands stay off the session until the grace has passed. No
/// keystroke ever, or a server that does not say, is nobody.
#[must_use]
pub fn person_present(status: &str, grace_s: u32) -> bool {
    HumanInput::of_status(status).within(grace_s) == Some(true)
}

/// The pauses before a session's consecutive relaunches: a crash that repeats
/// at once is not relaunched at once, and one that keeps repeating is
/// relaunched every ten minutes for as long as it does — never given up on.
pub const BACKOFF: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(10),
    Duration::from_secs(60),
    Duration::from_secs(600),
];

/// The look again after a step found another actor on the upgrade state
/// ([`Outcome::Busy`]). A retry only while that actor holds it, never counted.
pub const BUSY: Duration = Duration::from_secs(10);

/// An agent that ran this long since its last relaunch starts the back-off
/// over: it crashed once, not in a loop.
pub const HEALTHY: Duration = Duration::from_secs(600);

/// Consecutive relaunch steps that did not relaunch ([`Outcome::NotYet`])
/// before the session's attention says so. The host keeps trying after it.
pub const BADGE_AFTER: u32 = 3;

/// What the session's attention says after a [`Relaunches`] step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Say {
    /// Nothing changes.
    Nothing,
    /// The attention this raised is cleared: an agent runs in the tab again.
    Clear,
    /// The relaunch keeps failing ([`BADGE_AFTER`] in a row); it is still
    /// tried, and the attention says so.
    Failing,
    /// The relaunch can never be made: the one case a person is asked about.
    Cannot,
    /// The relaunch is LIMITED ([`OnExit::Limited`]): the agent is not
    /// relaunched, and the attention says why.
    Limited,
}

/// ONE SESSION'S RELAUNCH STATE, as the host keeps it across its workers: the
/// back-off ([`BACKOFF`], started over by a [`HEALTHY`] run), whether a
/// relaunch is due, whether the tab was left (to a person or a holder, or
/// because the exit was the launch's own end), and whether the session's
/// attention says the relaunch is limited, cannot be made or keeps failing. A
/// bounded machine: `HarnessRelaunchOnExit` in `aterm-spec` is its model,
/// and the Tier-1 test drives this type through every one of its actions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Relaunches {
    /// Relaunch attempts since the agent last ran [`HEALTHY`]ly.
    streak: u32,
    /// Consecutive attempts that did not relaunch.
    misses: u32,
    /// When the last relaunch landed.
    last: Option<Instant>,
    /// A relaunch is due.
    pending: bool,
    /// The tab is left: the exit was a person's or a holder's, one of them
    /// came back during the back-off, or the exit was the launch's own end.
    left: bool,
    /// The session's attention carries this relaunch's word.
    badged: bool,
}

impl Relaunches {
    /// The agent left its tab at `now`, and [`on_exit`] said `decision`: the
    /// pause before the relaunch, or — nothing due — what the attention says.
    ///
    /// # Errors
    /// No relaunch is due: the [`Say`] for the session's attention.
    pub fn exited(&mut self, decision: OnExit, now: Instant) -> Result<Duration, Say> {
        self.forgive(now);
        match self.decide(decision) {
            None => Ok(self.pause()),
            Some(say) => Err(say),
        }
    }

    /// [`on_exit`] asked again while a relaunch waits out its pause (a person
    /// or a holder may have come back, the owner may have limited it):
    /// `None` while it is still due, else what the attention says.
    pub fn decide(&mut self, decision: OnExit) -> Option<Say> {
        match decision {
            OnExit::Relaunch => {
                self.pending = true;
                self.left = false;
                None
            }
            OnExit::PersonAsked | OnExit::Held => {
                self.pending = false;
                self.left = true;
                Some(Say::Nothing)
            }
            OnExit::Limited => {
                self.pending = false;
                self.badged = true;
                Some(Say::Limited)
            }
        }
    }

    /// A relaunched agent that has run [`HEALTHY`]ly by `now` starts the
    /// back-off over: it crashed once, not in a loop.
    pub fn forgive(&mut self, now: Instant) {
        if self
            .last
            .is_some_and(|at| now.saturating_duration_since(at) >= HEALTHY)
        {
            self.streak = 0;
        }
    }

    /// The pause before the next attempt.
    #[must_use]
    pub fn pause(&self) -> Duration {
        crate::supervise::ladder::Ladder(&BACKOFF)
            .step(usize::try_from(self.streak).unwrap_or(usize::MAX))
    }

    /// One attempt came to `outcome` at `now`: what the attention says.
    pub fn attempted(&mut self, outcome: &Outcome, now: Instant) -> Say {
        match outcome {
            Outcome::Relaunched => {
                self.streak = self.streak.saturating_add(1);
                self.misses = 0;
                self.last = Some(now);
                self.pending = false;
                if std::mem::take(&mut self.badged) {
                    Say::Clear
                } else {
                    Say::Nothing
                }
            }
            Outcome::NotYet(_) => {
                self.streak = self.streak.saturating_add(1);
                self.misses = self.misses.saturating_add(1);
                if self.misses >= BADGE_AFTER && !self.badged {
                    self.badged = true;
                    Say::Failing
                } else {
                    Say::Nothing
                }
            }
            Outcome::Left(_) => {
                self.pending = false;
                self.left = true;
                Say::Nothing
            }
            Outcome::Busy => Say::Nothing,
            Outcome::Cannot(_) => {
                self.pending = false;
                self.badged = true;
                Say::Cannot
            }
        }
    }

    /// An agent runs in the tab again, started by someone else: nothing is
    /// due, and a word this raised is cleared.
    pub fn running(&mut self) -> Say {
        self.pending = false;
        self.left = false;
        if std::mem::take(&mut self.badged) {
            Say::Clear
        } else {
            Say::Nothing
        }
    }

    /// Whether a relaunch is due.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// `(left, badged, streak, misses)`: the model's `asked`, `badged`, and
    /// what its `step` and `misses` are read from.
    #[must_use]
    pub fn counts(&self) -> (bool, bool, u32, u32) {
        (self.left, self.badged, self.streak, self.misses)
    }
}

#[cfg(test)]
#[path = "relaunch_tests.rs"]
mod tests;
