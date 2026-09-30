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
//!   A CODEX is relaunched on exit too (*decided 2026-09-27 under the
//!   owner's standing direction: a crashed Codex is relaunched on its thread,
//!   parity with Claude Code; built* — `upgrade_drive`'s Codex lane,
//!   [`CodexRun`]): its snapshot
//!   names the thread it holds (an embedded TUI's writer lock, or the
//!   `resume <id>` it was launched with), and the crash-versus-graceful
//!   witness is its shell's word, read as the exit is seen — the command
//!   block's exit status (`0`, or someone's SIGHUP/SIGINT/SIGTERM: theirs;
//!   SIGKILL and every failure: a crash; measured on 0.157.1, 137 for a
//!   SIGKILL and 0 for `/exit`) — nothing else: an embedded TUI's thread
//!   lock is left behind by SIGTERM, SIGINT and SIGHUP exactly as by SIGKILL
//!   (measured 2026-09-27), so it is no word, and an exit with none is left.
//!   A daemon-mode client that named no thread is resumed on the ONE thread
//!   its daemon holds that was begun in its directory after it started (the
//!   ids are UUIDv7), never a guess — not between two, nor with another
//!   client of the daemon in the same directory. The relaunch is the Codex
//!   lane's own `codex resume` line, and the carry-on its own step.
//!
//!   AN EXIT THE HARNESS'S OWN RESTART MADE is no one else's (S0 and S3 of
//!   the in-flight review, 2026-09-27): a restart — the upgrade's, Claude
//!   Code's or Codex's, or the restart in place's — whose step returned
//!   with its agent ended and the relaunch line not typed yet (a person
//!   typing at the returned prompt, a hold, a shell slow to take the
//!   terminal back) leaves its tab with no step watching it. The host reads
//!   that FIRST ([`restarted`]): the exit is the restart's, never a
//!   person's, a holder's or the owner's `[harness] relaunch = false`
//!   ([`OnExit::Restarted`]), and never the graceful exit its own SIGTERM
//!   made of it. It is carried from where it stopped ([`carry_restart`]),
//!   tried at least every [`CARRY_EVERY`] — the back-off's ten-minute step
//!   outlasted [`STALE_S`], and the record expired between two tries — and
//!   what is typed at the prompt still waits on a person and a hold, the
//!   relaunch line's own look. It lands, or is said: `refused:<why>`.
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
    Client, Hand, Job, Kernel, Live, Opts, Report, Screen, SessionRosterCache, St, TAIL_BYTES,
    Targets, Terminated, Typed, alive, composer_clear, composer_empty, connect,
    continuation_composer_empty, conversation_live, cwd_of, exe_of, first_word, foreground_shell,
    held, host_roster, ids, is_our_ancestor, kernel_start, ledger, live_background, load,
    model_and_mark, native_root, note_stuck, now_s, owned_by_aterm, process_in_tab, record_asked,
    require_unique_owner, restart_models, restore, roster, said, save, screen, session_file_of,
    session_files, since, squash, state_dir, supervisor_typed, sweep_lock, table, tail_to_end,
    tasked, terminate, transcript, transcript_exists, turn, turn_fenced, type_line, typing_fence,
    unique_tab_for_group,
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
    /// The model the relaunch asks for ([`with_model`]): the launch's own
    /// `--model`, every spelling, is replaced by it; an EMPTY one drops it
    /// (Claude's default). `None` keeps the launch's flags.
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
/// is none, the same flags without `--resume` — asking for `model` in place
/// of the launch's own when one is given ([`with_model`]) and in the
/// permission `mode` when one is known ([`with_permission_mode`]), and the
/// line that runs them ([`upgrade::relaunch_line`]), or the refusal either one
/// makes.
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
/// `{"type":"permission-mode"}` row, which a crash's conversation has as well
/// as a live one's. `None` when the tail names none, or a mode this module
/// does not know.
fn last_permission_mode(home: &Path, session: &str) -> Option<String> {
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

/// The permission mode to resume `session` in: the pill the tab shows
/// (`shown`, [`screen_mode`]) — the live mode, exactly the person's choice —
/// else the transcript's newest `permission-mode` row
/// ([`last_permission_mode`]). Claude writes that row only when it re-appends
/// its session metadata (about every 32 KiB of transcript, a compaction, a
/// resume, its exit), not as the mode changes, so a mode chosen at idle — a
/// shift+tab, an aterm light — is on the screen long before it is in the
/// file. An exited agent's last frame usually still shows its pill.
pub(super) fn resume_mode(opts: &Opts, session: &str, shown: Option<String>) -> Option<String> {
    shown.or_else(|| last_permission_mode(&opts.home, session))
}

/// The pill `tab` shows now ([`screen_mode`]), for a restart with no look of
/// its own.
fn tab_mode(opts: &Opts, tab: &str) -> Option<String> {
    let mut c = connect(opts, tab).ok()?;
    screen_mode(&screen(&mut c, tab)?.rows)
}

/// The `--permission-mode` value of the pill Claude draws under its composer
/// on `rows` ([`super::lights::read_screen`]), or `None` where no composer or
/// no pill this build knows is drawn.
pub(super) fn screen_mode(rows: &[String]) -> Option<String> {
    super::lights::read_screen(rows)
        .and_then(|shown| shown.mode)
        .map(|mode| mode_flag(mode).to_string())
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
            st.stop(&why, now_s());
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
///
/// THE END IS THE ONE A LOOK SAW ([`St::exited_at`]), and only while none
/// has the signal that asked for it (the review of 2026-09-27): a Claude
/// Code whose shutdown outlived the bound — a hung hook, an MCP teardown —
/// was refused `stale-exit` the moment it exited, its prompt just back, and
/// the agent the restart had ended stayed down. A look's own FIRST sighting
/// is no evidence of when the exit came (a sweep run by hand, a host that
/// was down): it decides on the signal ([`expired`] before the stamp), and
/// the one look that knows the exit is fresh — the host's carry, on the exit
/// it just saw — stamps it first ([`carry_restart`]).
pub const STALE_S: u64 = 300;

/// Why a relaunch in flight whose agent is gone must stop instead of acting
/// on the tab, `(word, ledger detail)`, or `None` while it may still act.
/// An exit is stale [`STALE_S`] after its agent was seen gone, or after the
/// signal while no look has seen it gone yet.
pub(super) fn expired(st: &St, now: u64) -> Option<(&'static str, &'static str)> {
    use upgrade::Phase;
    match st.phase {
        Phase::Exiting { at_s } if now.saturating_sub(at_s.max(st.exited_at)) > STALE_S => Some((
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
/// terminal group, Claude's own session records — has no event to wait on
/// (the agent's exit does: [`super::upgrade_wake::wait_exit`]; and the
/// agent's verdict: `await agent`, [`first_idle`]).
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
/// THE HARNESS'S HAND IS ON THE TAB throughout
/// ([`super::upgrade_drive::Hand`], ND1 of the live re-test of
/// 2026-09-26): taken (or renewed) here — a restart took it before its last
/// look — renewed through every wait ([`Hand::keep`]), kept while this
/// connection types the line (the hold lets its own writes and its own
/// `turn` through, and nobody else's), and given back once the relaunched
/// agent is up and idle ([`await_new`]) or the relaunch fails. A relaunch
/// left waiting with the agent ended and the line not typed (`Exiting`)
/// keeps it on the bare shell until the next attempt renews it, or it
/// lapses.
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
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = relaunch_held(opts, r, st, c, &mut hand, session, k);
    if !matches!(st.phase, upgrade::Phase::Exiting { .. }) {
        hand.give_back(c);
    }
    r
}

/// [`relaunch`] with the harness's `hand` on the tab.
fn relaunch_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    session: &str,
    k: &dyn Kernel,
) -> Report {
    let old = st.pid;
    if !super::upgrade_wake::wait_exit(old, Duration::from_secs(30)) {
        // Never a harder signal: the agent finishes exiting on its own, and a
        // later pass finds the phase and the process as they are.
        return said(r, "wait:exiting");
    }
    // Gone: whatever stops this restart from here has no process left to vet
    // it by, and is the tab's record — stamped where the agent is SEEN gone,
    // never at the signal (S1 of the in-flight review, 2026-09-27; the Codex
    // lane's relaunch stamps it so).
    st.seen_gone(now_s());
    hand.keep(c);
    let shell = st.shell;
    let prompt_back = wait_until(Duration::from_secs(15), || {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    });
    hand.keep(c);
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
            st.stop("relaunch-refused", now_s());
            let r = said(r, format!("failed:relaunch:{}", first_word(&e)));
            ledger(opts, &r, &st.line);
            return r;
        }
    }
    st.phase = upgrade::Phase::Relaunched { at_s: now_s() };
    ledger(opts, &said(r.clone(), "relaunched"), &st.line);
    save(opts, session, st);
    await_held(opts, r, st, c, hand, session, k)
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
/// (review of 2026-09-25): a fenced turn parks up to 10 s on a person's
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

/// A refused relaunch turn: a busy tab (another driver's turn or lease —
/// where the harness's hand could not be taken, or lapsed) is a wait, never
/// the refusal that fails a relaunch whose agent is already gone.
fn refused_line(e: String) -> RelaunchLineError {
    if e.starts_with("ERR busy") {
        RelaunchLineError::Wait("held")
    } else {
        RelaunchLineError::Turn(e)
    }
}

/// The typing half of [`type_relaunch_line_with`]: `line` at the marked
/// prompt — typed to bash, pasted fenced to zsh and fish — on the connection
/// that holds the harness's hand, if it does
/// ([`super::upgrade_drive::Hand`]): the hold refuses every other
/// connection's writes and lets this one's through, its `turn` included, so
/// nothing is let go for the line and no other driver's keys can land
/// before or after it.
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
                Err(Typed::Refused(e)) => return Err(refused_line(e)),
            }
        }
    }
    at_mark(mark, screen(c, tab).as_ref())?;
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    if bash {
        return type_line(c, tab, line).map_err(refused_line);
    }
    match turn_fenced(c, tab, line, None) {
        Ok(()) => Ok(()),
        Err(Typed::Yielded | Typed::Changed) => Err(RelaunchLineError::Wait("yield")),
        Err(Typed::Refused(e)) => Err(refused_line(e)),
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
/// ([`resume`]). A later step that finds the relaunch in flight comes here
/// too: it holds the tab as the relaunch did ([`await_held`]), and gives it
/// back as it returns.
pub(super) fn await_new(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    session: &str,
    k: &dyn Kernel,
) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = await_held(opts, r, st, c, &mut hand, session, k);
    hand.give_back(c);
    r
}

/// [`await_new`] with the harness's `hand` on the tab: kept through the wait
/// for the new process, and on it until the new agent is up and idle
/// ([`first_idle`]) — a prompt another driver sends meanwhile is refused and
/// lands, retried, in an agent that reads it — and, where this step types
/// the continuation itself, through it ([`carry_on`]: its own connection's
/// turn goes through the hold). Its caller gives it back.
fn await_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    session: &str,
    k: &dyn Kernel,
) -> Report {
    let home = opts.home.clone();
    let (old, shell) = (st.pid, st.shell);
    // A fresh start holds a conversation of its own: the shell's child is it.
    let fresh = starts_afresh(&st.cause);
    let mut new: Option<SessionFile> = None;
    let mut roster = SessionRosterCache::default();
    let mut back = ShellBack::default();
    let mut ended = false;
    let look_back = ends_at_once(&st.cause);
    wait_until(held_wait(&st.cause), || {
        hand.keep(c);
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
        let now = Instant::now();
        if look_back && new.is_none() && back.due(now) {
            ended = back.saw(k.shell_holds_tab(shell), now);
        }
        new.is_some() || ended
    });
    // The new process holds the conversation: the harness's hand stays on the
    // tab until it is up and idle — and, where this step types the
    // continuation itself, through it.
    if let Some(sf) = &new {
        first_idle(c, hand, &st.tab, || {
            session_file_of(&home, sf.pid).is_some_and(|sf| sf.status == "idle")
        });
    }
    match new {
        Some(sf) if fresh => {
            // Nothing was said before it ended: nothing to carry on with. The
            // finished move is the owner's to see (`upgrade=done/…`), as any
            // other restart's is.
            st.phase = upgrade::Phase::Done;
            st.done_at = now_s();
            st.outcome = format!(
                "claude restarted afresh on {} · nothing to resume",
                one_line(&sf.version)
            );
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
        // The relaunched agent ended as it started (Claude's `No conversation
        // found`, a crash at start): its shell has had the tab back for
        // [`ENDED_AT_ONCE`] and nothing registered the conversation — read
        // for a restored tab's relaunch only ([`ends_at_once`]). Said now
        // (day six, D30: the wait went on 90 s at a time for five minutes,
        // the harness's hand on the tab throughout), with the stale
        // relaunch's word and stop — what it printed is on the tab.
        None if ended => {
            st.stop("no-resume", now_s());
            let r = said(r, "failed:no-resume");
            ledger(
                opts,
                &r,
                "the relaunched agent ended without registering the conversation: its shell \
                 has the tab back",
            );
            r
        }
        None => {
            if let upgrade::Phase::Relaunched { at_s } = st.phase
                && now_s().saturating_sub(at_s) > STALE_S
            {
                st.stop("no-resume", now_s());
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

/// How long one [`await_held`] step waits for the relaunched process to
/// register its conversation; a later step waits again, until [`STALE_S`].
const HELD_WAIT: Duration = Duration::from_secs(90);
/// A cold restore has other tabs to relaunch. A foreground agent that never
/// registers its conversation must release the one sweep lock soon enough for
/// their first steps; the same in-flight record is checked on a later retry.
/// This still lets [`ShellBack`] observe [`ENDED_AT_ONCE`] before the step
/// ends, so a launch that returned to its shell is reported immediately.
const RESTORED_HELD_WAIT: Duration = Duration::from_secs(7);

#[cfg(test)]
thread_local! {
    /// A test's shorter [`HELD_WAIT`], for this thread only ([`held_wait`]):
    /// a step that must NOT end early is waited out in seconds.
    pub(super) static HELD_WAIT_IN_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// [`HELD_WAIT`] for a live relaunch, [`RESTORED_HELD_WAIT`] for a cold
/// restore, or a test's shorter one ([`HELD_WAIT_IN_TEST`]).
fn held_wait(cause: &str) -> Duration {
    #[cfg(test)]
    if let Some(wait) = HELD_WAIT_IN_TEST.with(std::cell::Cell::get) {
        return wait;
    }
    if ends_at_once(cause) {
        RESTORED_HELD_WAIT
    } else {
        HELD_WAIT
    }
}

/// Whether [`await_held`] reads a relaunched agent as ENDED once its shell
/// has held the tab for [`ENDED_AT_ONCE`] ([`ShellBack`]): only a restored
/// tab's relaunch, after aterm itself ended ([`CAUSE_HOST`]), whose host
/// tries it once and says a failure at once (ruling 296 of the messages
/// design, as amended in review). NOT the live relaunch on exit, nor the
/// upgrade: the live loop reads `failed:no-resume` as not yet
/// ([`outcome`]) and each retry after its back-off types the relaunch line
/// into the tab afresh, so an early verdict there retyped a refusing agent
/// at about +7 s, +23 s and +90 s where this wait retypes it only past
/// [`STALE_S`] — a change to P6's cadence that is its owner's call.
pub(super) fn ends_at_once(cause: &str) -> bool {
    cause == CAUSE_HOST
}

/// How long the shell a relaunch line was typed at may hold its terminal
/// again, with nothing registered, before the relaunched agent is read as
/// ended ([`ShellBack`]). The shell hands the terminal to what it starts as
/// it starts it, so a prompt that holds it this long started nothing that
/// still runs.
pub(super) const ENDED_AT_ONCE: Duration = Duration::from_secs(5);

/// How often [`await_held`] asks whether the shell has the tab back: a `ps`
/// each time, so not at the roster's pace.
const SHELL_LOOK: Duration = Duration::from_secs(1);

/// THE RELAUNCHED AGENT ENDED AS IT STARTED (day six, D30): read from the
/// shell the relaunch line was typed at, which has held its terminal again
/// for [`ENDED_AT_ONCE`] in a row. A look that finds anything else holding
/// it — the agent still starting, a program a person started — starts the
/// count over; a look that cannot be read is no verdict and leaves it as it
/// is.
#[derive(Debug, Default)]
pub(super) struct ShellBack {
    /// Since when every read look found the shell holding its terminal.
    since: Option<Instant>,
    /// The last look, for [`SHELL_LOOK`].
    looked: Option<Instant>,
}

impl ShellBack {
    /// Whether a look is due at `now` ([`SHELL_LOOK`] since the last).
    pub(super) fn due(&mut self, now: Instant) -> bool {
        let due = self
            .looked
            .is_none_or(|at| now.saturating_duration_since(at) >= SHELL_LOOK);
        if due {
            self.looked = Some(now);
        }
        due
    }

    /// One look at `now` ([`Kernel::shell_holds_tab`]): whether the shell
    /// has now held its terminal for [`ENDED_AT_ONCE`].
    pub(super) fn saw(&mut self, holds: Option<bool>, now: Instant) -> bool {
        match holds {
            Some(true) => {
                let since = *self.since.get_or_insert(now);
                now.saturating_duration_since(since) >= ENDED_AT_ONCE
            }
            Some(false) => {
                self.since = None;
                false
            }
            None => false,
        }
    }
}

/// How long the harness's hand waits on a relaunched agent to come up idle
/// before it is given back anyway ([`first_idle`]): an agent that opens
/// with a box (a new build's consent) is its supervisor's to answer, and the
/// supervisor is still until the step returns.
const FIRST_IDLE: Duration = Duration::from_secs(15);

/// THE RELAUNCHED AGENT'S FIRST IDLE, waited for with the harness's `hand` on
/// the tab (renewed first: at most [`FIRST_IDLE`] follows): the server's own
/// verdict on its screen, pushed — ONE `await agent idle`, the wait an
/// orchestrator's own `await agent idle` wakes from — or, from a server
/// without the verb, `record_idle` (the agent's own record saying idle),
/// polled.
pub(super) fn first_idle(
    c: &mut Client,
    hand: &mut Hand,
    tab: &str,
    record_idle: impl FnMut() -> bool,
) {
    hand.keep(c);
    let awaited = c.request_line(&format!(
        "@{tab} await agent idle timeout={}",
        FIRST_IDLE.as_millis()
    ));
    if !awaited.is_ok_and(|reply| reply.starts_with("OK")) {
        wait_until(FIRST_IDLE, record_idle);
    }
}

/// How much of a transcript past the restart's mark is read for the resumed
/// session's first turn. Measured 2026-09-24 (the owner's session, 2.1.281 to
/// 2.1.282): a queued task notification, three system rows and the
/// continuation came before it — a few KiB.
const SINCE_BYTES: u64 = 4 << 20;

/// How long after the continuation the resumed session's first answer may
/// still be what the model is confirmed from. NEVER WAITED FOR: the step that
/// types the continuation reads nothing, and every later one
/// ([`super::upgrade_drive::confirmations`], run first in every sweep and
/// every host step) reads, until this has passed. A sweep runs its
/// visits one after another and its orphan pass after all of them, so a visit
/// that blocked on the answer would age every restart left exiting toward
/// [`STALE_S`] — and the answer can take all of this (a long first thought,
/// retries, a usage limit that writes only `<synthetic>` rows). Measured
/// 2026-09-24: 10 s (a thinking block, the first row a turn writes); the
/// `turn` that types the continuation returns once its submit is taken
/// ([`super::upgrade_drive::TURN_WAIT`]), so a later step's read usually has
/// it.
pub(super) const MODEL_WAIT: Duration = Duration::from_secs(120);

/// The new process holds the conversation: once it settles, tell it to carry
/// on — with the upgrade's words, or with the relaunch's ([`resumed_prompt`])
/// when the record is one [`after_exit`] made — `continued`, the model it
/// came back on left to a later step ([`confirm`]). `key` is the conversation
/// the record is filed under.
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
///
/// A CARRY-ON THAT WAITS on a relaunch past its bound — the new agent never
/// idle, a box on its screen, the tab's owner changed — is said once on the
/// ledger (`stuck:relaunched`, [`super::upgrade_drive::note_stuck`]; L4 of
/// the upgrade's leftovers): nothing bounds that wait while the conversation
/// is held. Nothing is typed for it, and the wait stands.
#[allow(clippy::too_many_arguments)]
pub(super) fn carry_on_with_tab_probe(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    key: &str,
    new: &SessionFile,
    confirm_within: Duration,
    tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Report {
    let r = carry_on_once(opts, r, st, c, key, new, confirm_within, tab_probe);
    if r.step.starts_with("wait:") && matches!(st.phase, upgrade::Phase::Relaunched { .. }) {
        note_stuck(opts, &r, st, key, now_s());
    }
    r
}

/// [`carry_on_with_tab_probe`]'s one attempt.
#[allow(clippy::too_many_arguments)]
fn carry_on_once(
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
    // A conversation nobody but the harness has asked anything is carried on
    // with NOTHING (D1 of the live E2E of 2026-09-26): its own turns are no
    // task, and a carry-on typed into it starts one nobody asked for — the
    // E2E's B 1a5299ab was carried on and then continued, and its agent
    // could only ask what it should work on.
    if settled && tasked(&home, key, &supervisor_typed(opts, tab)) == Some(false) {
        st.phase = upgrade::Phase::Done;
        r.from.clone_from(&st.from);
        r.to = format!("{}({})", latest.version, st.source);
        let r = said(r, "done:taskless");
        st.outcome = format!(
            "claude restarted on {} · nothing typed: nobody has asked this conversation anything",
            one_line(&latest.version)
        );
        st.done_at = now_s();
        ledger(opts, &r, &st.outcome);
        save(opts, key, st);
        return r;
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
    // only the model after, which its answer names: a LATER step's to read
    // ([`confirm`]). This one returned once the submit was taken, before any
    // answer, and says only that it typed — `continued`, the word the host
    // awaits the harness's own turn by (`upgrade_drive::typed`).
    st.resumed_on.clone_from(&latest.version);
    st.resumed_pid = new.pid;
    st.confirm_by = now_s().saturating_add(confirm_within.as_secs());
    save(opts, key, st);
    let r = said(r, "continued");
    ledger(
        opts,
        &r,
        "the continuation is typed; a later step reads the model its answer names",
    );
    r
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
        let generation = match typing_fence(c, tab) {
            Ok(generation) => generation,
            // The composer moved between the fence's two reads (a `cell`
            // asked between them, design record 2026-09-28 C7a): the same
            // word, and the same retry, as the host's own `changed`.
            Err("changed") => {
                std::thread::sleep(RELAUNCH_FENCE_EVERY);
                continue;
            }
            Err(_) => return false,
        };
        match turn_fenced(c, tab, text, generation.as_deref()) {
            Ok(()) => return true,
            Err(Typed::Changed | Typed::Yielded) => std::thread::sleep(RELAUNCH_FENCE_EVERY),
            Err(Typed::Refused(_)) => return false,
        }
    }
    composer_clear(c, tab) && turn(c, tab, text).is_ok()
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
/// by the model rule) from what the restart recorded, the model after as
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
pub(super) fn recorded(s: &str) -> Option<&str> {
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

/// The [`St::cause`] of the UPGRADE's restart of a conversation nobody has
/// asked anything ([`super::upgrade::Step::Fresh`]): started afresh on the
/// newer build, nothing to preserve and nothing carried on — and, unlike a
/// relaunch's, a move of the tab's the owner's view shows
/// (`upgrade_status`'s rows).
pub(super) const CAUSE_UPGRADE_FRESH: &str = "upgrade-fresh";

/// Whether a record of `cause` starts its agent afresh: its new process holds
/// a conversation of its own, and nothing is carried on ([`await_new`]).
pub(super) fn starts_afresh(cause: &str) -> bool {
    cause == CAUSE_FRESH || cause == CAUSE_UPGRADE_FRESH
}

/// The [`St::cause`] of a restart the host made for Claude Code's
/// critical-memory banner ([`restart_here`], [`Restart::Memory`]).
pub(super) const CAUSE_MEMORY: &str = "memory";

/// The [`St::cause`] of a relaunch after the stall's remedy ended an agent
/// that had stopped reading its input ([`after_exit`] with `stalled`, U1).
pub(super) const CAUSE_STALL: &str = "stall";

/// The [`St::cause`] of a relaunch after aterm ITSELF ended while the agent
/// ran — a crash, a kill, a power loss, a restart — and the next launch
/// reopened its tab ([`after_host_ended`], 2026-09-27).
pub(super) const CAUSE_HOST: &str = "host";

/// The [`St::cause`] of a restart onto a bucket's fallback model, `model:`
/// then the model ([`Restart::Model`]).
pub(super) const CAUSE_MODEL: &str = "model:";

/// The [`St::cause`] of a restart back from a bucket's fallback at its
/// reset, `model-back:` then the model, empty for the launch's own
/// ([`Restart::ModelBack`]).
pub(super) const CAUSE_MODEL_BACK: &str = "model-back:";

/// What a restart in place ([`restart_from`]) asks for and records, by
/// `why`: the model its line asks for, its [`St::cause`], the launch's own
/// `--model` (`launched`) and the fallback origin ([`St::fallback_from`]),
/// read off the conversation's last record `prior` ([`fallback_origin`]).
/// The memory banner's restart asks for the model riding it (`riding`), else
/// the model a person chose by hand (`hand`), and carries the origin
/// ([`fallback_carried`]); a fallback asks for its model and keeps the origin
/// standing, else sets it — the hand choice, else the launch's own; the way
/// back asks for a hand choice made since, else the origin, else the model
/// the bucket's notice named, else none (`--model` dropped), and spends it.
pub(super) fn model_restart(
    why: &Restart,
    prior: Option<&St>,
    launched: String,
    riding: Option<String>,
    hand: Option<String>,
) -> ModelRestart {
    let origin = prior.and_then(fallback_origin);
    match why {
        Restart::Memory => ModelRestart {
            fallback_from: fallback_carried(origin, riding.as_deref(), hand.as_deref()),
            model: riding.or(hand),
            cause: CAUSE_MEMORY.to_string(),
            launch_model: launched,
        },
        Restart::Model { to } => ModelRestart {
            model: Some(to.clone()),
            cause: format!("{CAUSE_MODEL}{to}"),
            fallback_from: Some(origin.or(hand).unwrap_or_else(|| launched.clone())),
            launch_model: launched,
        },
        Restart::ModelBack { to } => {
            // The notice names the family, not what the person was on: the
            // exact model (a hand choice, a `[1m]` window) comes first.
            let back = hand.or(origin).or_else(|| to.clone()).unwrap_or_default();
            ModelRestart {
                model: Some(back.clone()),
                cause: format!("{CAUSE_MODEL_BACK}{back}"),
                launch_model: launched,
                fallback_from: None,
            }
        }
    }
}

/// [`model_restart`]'s answer: the fields of a restart's record the model decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelRestart {
    /// The model the line asks for ([`Launch::model`]).
    pub(super) model: Option<String>,
    /// [`St::cause`].
    pub(super) cause: String,
    /// [`St::launch_model`].
    pub(super) launch_model: String,
    /// [`St::fallback_from`].
    pub(super) fallback_from: Option<String>,
}

/// The fallback origin standing on `st` ([`St::fallback_from`]) — or, for a
/// fallback's own record an older build wrote, the launch model that record
/// kept. `None`: no fallback stands.
pub(super) fn fallback_origin(st: &St) -> Option<String> {
    st.fallback_from.clone().or_else(|| {
        st.cause
            .starts_with(CAUSE_MODEL)
            .then(|| st.launch_model.clone())
    })
}

/// The fallback origin a restart made for another reason — a memory
/// banner's, an exit's relaunch, the upgrade's — records: the one standing
/// (`origin`), carried — or, where the line asks for a model a person chose
/// by hand (`hand`, nothing `riding`), that model, which the way back at the
/// bucket's reset would otherwise undo.
pub(super) fn fallback_carried(
    origin: Option<String>,
    riding: Option<&str>,
    hand: Option<&str>,
) -> Option<String> {
    origin.map(|origin| match (riding, hand) {
        (None, Some(hand)) => hand.to_string(),
        _ => origin,
    })
}

/// Whether a record of `cause` is a relaunch's — its continuation says why
/// it was relaunched ([`resumed_prompt`]) — rather than the upgrade's. A
/// relaunch after aterm itself ended ([`CAUSE_HOST`]) is one (ruling 293 of
/// the messages design: left out, its continuation told the agent it had
/// been upgraded, from and to the same version).
pub(super) fn relaunch_cause(cause: &str) -> bool {
    cause == CAUSE_EXIT
        || cause == CAUSE_MEMORY
        || cause == CAUSE_STALL
        || cause == CAUSE_HOST
        || cause.starts_with(CAUSE_MODEL)
        || cause.starts_with(CAUSE_MODEL_BACK)
}

/// Whether a record of `cause` was filed by a restart that ENDED ITS AGENT
/// ITSELF — the upgrade's SIGTERM (cause empty, or [`CAUSE_UPGRADE_FRESH`])
/// or the restart in place's ([`restart_here`]: [`CAUSE_MEMORY`],
/// [`CAUSE_MODEL`], [`CAUSE_MODEL_BACK`]) — rather than by a relaunch on
/// exit, whose agent was gone before its record was made ([`CAUSE_EXIT`],
/// [`CAUSE_FRESH`], [`CAUSE_HOST`], and [`CAUSE_STALL`]: the stall's remedy
/// is its supervisor's signal, not the record's). An allowlist: a relaunch
/// cause added later is no restart that ended its agent until it is named
/// here.
pub(super) fn ends_its_agent(cause: &str) -> bool {
    cause.is_empty()
        || cause == CAUSE_UPGRADE_FRESH
        || cause == CAUSE_MEMORY
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
    resumed_prompt_as("Claude Code", version, cause)
}

/// [`resumed_prompt`] for the agent `agent` names (`Claude Code`, `Codex`).
#[must_use]
pub fn resumed_prompt_as(agent: &str, version: &str, cause: &str) -> String {
    let session_only = "for this session only: your default model is unchanged";
    let why = if cause == CAUSE_MEMORY {
        format!(
            "Restarted: {agent} reported its memory critical, so this session was ended and \
             restarted"
        )
    } else if cause == CAUSE_HOST {
        "Relaunched: aterm ended while this session ran (a crash, a kill or a restart), and \
         reopened its tab"
            .to_string()
    } else if cause == CAUSE_STALL {
        format!(
            "Relaunched: {agent} stopped reading its input and was ended, so this session was \
             restarted"
        )
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
        format!("Relaunched: {agent} exited without anyone asking, and this session was restarted")
    };
    format!(
        "{} {why} on {agent} {} and resumed. {}",
        upgrade::HARNESS_MARK,
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
    restart_with(opts, why, snap, &Live)
}

/// [`restart_from`] with the process facts `k` answers ([`Kernel`]): the
/// seam a test scripts the agent's job and terminal through, to drive the
/// restart past its last look to the record it saves.
pub(super) fn restart_with(
    opts: &Opts,
    why: &Restart,
    snap: Result<Snapshot, &'static str>,
    k: &dyn Kernel,
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
    // The model the line asks for and the fallback origin it records
    // ([`model_restart`]): the memory banner's restart carries the model the
    // rule moves the conversation to, when one is due; every restart reads a
    // `/model` made since the launch, which the launch's `--model` would undo.
    let (riding, hand) = restart_models(
        opts,
        Some(&sf),
        &snap.start,
        &session,
        &snap.argv,
        matches!(why, Restart::Memory),
    );
    let model_list = riding.clone().unwrap_or_default();
    let launched = upgrade::launch_model(&snap.argv).unwrap_or_default();
    let ModelRestart {
        model,
        cause,
        launch_model,
        fallback_from,
    } = model_restart(why, prior.as_ref(), launched, riding, hand);
    let mode = resume_mode(opts, &session, tab_mode(opts, &tab));
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
        model_list,
        fallback_from,
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
    // The harness's hand on the tab from the last look to the relaunched
    // agent's first idle ([`super::upgrade_drive::Hand`]), as for every
    // restart: given back on every way out before the signal, and by the
    // relaunch after it.
    let Some(mut hand) = Hand::take(&mut c, &tab) else {
        return said(r, "wait:held");
    };
    let aborted: Option<Report> = 'signal: {
        // The last look before the one irreversible act: the same process,
        // idle by its own record, its shell's foreground job on the tab's
        // terminal, the composer empty, nothing running under it, nobody's
        // hand and nobody's unread input.
        let again = session_file_of(&opts.home, snap.pid);
        let still = again.is_some_and(|a| {
            a.session_id == session
                && a.status == "idle"
                && kernel_start(snap.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        }) && foreground_shell(k.job(snap.pid)) == Ok(shell)
            && k.terminal(snap.pid)
                .as_ref()
                .is_some_and(|(p, name)| owned_by_aterm((*p, name.as_str())))
            && screen(&mut c, &tab).is_some_and(|scr| composer_empty(&mut c, &tab, &scr))
            && live_background(snap.pid).is_empty()
            && !held(&mut c, &tab, opts.human_grace_s);
        if !still {
            break 'signal Some(said(r.clone(), "wait:changed"));
        }
        if let Err(why) = require_unique_owner(&opts.home, &sf) {
            break 'signal Some(said(r.clone(), format!("wait:{why}")));
        }
        let Ok(pid) = i32::try_from(snap.pid) else {
            break 'signal Some(said(r.clone(), "wait:pid"));
        };
        // The model the agent ran and where its transcript ended: the
        // resumed session's answer past it says which model it came back on.
        let recent = transcript(&opts.home, &session).map(|p| tail_to_end(&p, TAIL_BYTES));
        (st.model_before, st.mark) = model_and_mark(recent.as_ref());
        st.pid = snap.pid;
        st.shell = shell;
        st.tab.clone_from(&tab);
        st.line = line;
        st.phase = upgrade::Phase::Exiting { at_s: now_s() };
        save(opts, &session, &st);
        record_asked(opts, &session, &st.model_list);
        match terminate(&mut c, &tab, pid, opts.human_grace_s) {
            Terminated::Sent => None,
            // Nothing was sent: no restart is in flight, and the record is
            // what it was (an upgrade's, or none).
            Terminated::Refused(no) => {
                restore(opts, &session, prior.as_ref());
                Some(said(r.clone(), format!("wait:signal-{no}")))
            }
            Terminated::Failed => {
                restore(opts, &session, prior.as_ref());
                Some(said(r.clone(), "failed:signal-refused"))
            }
        }
    };
    if let Some(r) = aborted {
        hand.give_back(&mut c);
        return r;
    }
    ledger(
        opts,
        &said(r.clone(), format!("restart:{}", why.word())),
        "SIGTERM",
    );
    let r = relaunch(opts, r, &mut st, &mut c, &session, k);
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
    /// Its version, when Claude had registered it (a Codex's: its package's).
    pub version: Option<String>,
    /// A CODEX's run ([`CodexRun`]); `None` for Claude Code.
    pub codex: Option<CodexRun>,
    /// The dialect of `shell` as its executable named it when the snapshot
    /// was read ([`shell_dialect`]): `None` where it could not be read, or
    /// for a shell the relaunch line is not written for (nushell, tcsh, …),
    /// whose relaunch [`plan`] waits on for ever. Read here, off any event
    /// loop, so [`resumes_on_exit`] stays a pure function a window can ask.
    pub dialect: Option<Dialect>,
}

/// What a Codex TUI's relaunch on exit needs of its run beside the
/// [`Snapshot`] (whose `session` is the thread it holds, when one can be
/// named while it runs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexRun {
    /// The `$CODEX_HOME` it runs with (canonical).
    pub home: PathBuf,
    /// It holds its thread's writer lock itself (`--no-daemon`, or a launch
    /// Codex keeps out of the daemon); `false`: a client of its home's
    /// daemon, which holds the thread.
    pub embedded: bool,
    /// When it started, unix milliseconds (to the second): a daemon-mode
    /// client's thread is one its daemon began after this.
    pub started_ms: u64,
    /// This read could not tell a daemon-mode client's thread apart from
    /// another's (`thread-ambiguous`): the host keeps no earlier read's name
    /// over it. Never carried across a restart (a restored run names its
    /// thread or none).
    pub ambiguous: bool,
}

/// Whether an exit of `snap`'s agent now would be RELAUNCHED ON ITS OWN
/// CONVERSATION by this host's relaunch ([`after_exit`]) — as far as
/// anything read while the agent runs can tell: a conversation it holds
/// ([`upgrade::is_session_id`]), a launch that is not a one-shot run, a
/// shell whose dialect the relaunch line is written for, and a line
/// [`line_for`] makes from its argv (so every flag is one
/// [`upgrade::rewrite_argv`] knows and the launch is resumable in place).
/// Pure: the dialect was read into the snapshot ([`Snapshot::dialect`]).
///
/// WHY (resume-hint review, 2026-09-26). A frozen Claude Code's remedy
/// says "aterm relaunches it on its conversation" INSTEAD of naming the
/// `claude --resume <id>` a person would run, so that sentence must be
/// true. It was decided from the agent kind and `[harness] relaunch`
/// alone, while the relaunch itself refuses an argv with a flag the
/// rewrite table does not know (`argv:unknown-flag`: the installed Claude
/// Code 2.1.283 lists `--permission-prompt-tool` and `--client-data-url`,
/// neither in the table), a `--worktree` launch (`argv:not-resumable`),
/// waits for ever on a shell it cannot write a line for, and has nothing
/// to relaunch without a snapshot. In each of those the person was told
/// the relaunch would come, was given no command, and nothing came. A
/// `false` here sends the remedy back to the command
/// ([`super::resume::command`]).
///
/// A CODEX's snapshot answers `false`: the Codex lane relaunches it
/// ([`super::upgrade_drive::codex_after_exit`]), but on its conversation
/// only when the thread it held still has a rollout at the exit — a thread
/// with none comes back as a fresh TUI — which nothing read while it runs
/// can promise. Its remedy names no relaunch and no command (a Codex has no
/// `claude --resume` line), so it never promises one that does not come.
#[must_use]
pub fn resumes_on_exit(snap: &Snapshot) -> bool {
    if snap.codex.is_some() {
        return false;
    }
    let Some(dialect) = snap.dialect else {
        return false;
    };
    let Some(session) = snap
        .session
        .as_deref()
        .filter(|s| upgrade::is_session_id(s))
    else {
        return false;
    };
    // The line WITHOUT its `cd`: the plan adds one only where the shell's
    // directory differs from the conversation's at the exit, which is not
    // known before it (and a fish tab whose shell left that directory is
    // refused then — the one case this cannot see). A control character in
    // the directory refuses the line either way.
    !upgrade::one_shot(&snap.argv)
        && !snap.cwd.chars().any(char::is_control)
        && line_for(
            dialect,
            None,
            None,
            &snap.program,
            &snap.argv,
            Some(session),
            None,
            None,
        )
        .is_ok()
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
    let image = exe_of(pid);
    // A Codex TUI: the Codex lane reads its run.
    if image
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .and_then(aterm_phase::program_of)
        == Some(aterm_phase::Program::Codex)
    {
        return super::upgrade_drive::codex_snapshot(opts, tab, pid, start, shell, &args);
    }
    let file = session_file_of(&opts.home, pid).filter(|sf| squash(&sf.proc_start) == start);
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
        codex: None,
        // The kernel's argv of the shell alone (no process-table fallback):
        // one read, and an unreadable one only costs the remedy its
        // relaunch wording ([`resumes_on_exit`]), never a relaunch.
        dialect: shell_dialect(shell, &[]),
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
    // A Codex keeps no record of its process to follow: its snapshot is
    // taken again at the loop's idle points.
    if snap.codex.is_some() {
        return false;
    }
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
    /// that never registered a conversation). A Codex's: its shell's word
    /// that the exit was its own orderly one or someone's signal.
    Removed,
    /// A CODEX CRASHED, by its shell's word (its command's exit status,
    /// [`CodexRun`]): no record of its own to carry, the thread is the
    /// snapshot's.
    Crashed,
}

impl ExitRecord {
    /// One word for the host's log.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Survived(_) => "survived",
            Self::Removed => "removed",
            Self::Crashed => "crashed",
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
    /// Its record, when one of its own start is on disk (Claude Code's).
    pub record: Option<SessionFile>,
    /// A CODEX's look: `record` is never read, `crashed` decides.
    pub codex: bool,
    /// A Codex's exit as its shell tells it: `Some(true)`
    /// a crash, `Some(false)` its own exit or someone's signal, `None` not
    /// told yet ([`super::upgrade_codex::crashed_status`]).
    pub crashed: Option<bool>,
}

/// How long after the look at an exit starts the exit itself may still be
/// removing its own record: a graceful `/exit` removes it in its last
/// moments, around when its exit is seen (measured 2026-09-26, Claude Code
/// 2.1.283: gone 0.02 s after the `/exit`, before its process had ended),
/// so a record read at the exact instant could call a clean exit a crash.
/// The window in which another Claude Code's start can turn a crash into a
/// graceful exit, once the exit is SEEN: a quarter second, where the
/// back-off it replaces was a second to ten minutes. How soon an exit is
/// seen is the caller's, and can widen it: the window host keeps an agent
/// its roster could not name while the agent still holds its tab, and an
/// exit during that misread that no bell rang for (neither the agent's name
/// nor the shell's could be read) is seen only at the keep's next look, up
/// to two seconds on (`aterm-gui`'s `harness_host`, `HOLDS_LOOK_MAX`).
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
    ExitLook {
        running,
        record,
        codex: false,
        crashed: None,
    }
}

/// [`ExitLook`] of a CODEX `snap` ([`CodexRun`]), live: its shell's exit
/// status for the command that ran it, read over the control socket
/// (`opts.sock`) — the one word on it.
#[must_use]
pub fn look_at_codex_exit(opts: &Opts, snap: &Snapshot) -> ExitLook {
    super::upgrade_drive::codex_look_at_exit(opts, snap)
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
/// crash; one with the process still running waits up to [`EXIT_GONE`]. A
/// CODEX look decides the moment its shell's word comes
/// ([`ExitLook::crashed`]), and waits for it up to [`EXIT_GONE`] (the shell
/// writes it as it takes the terminal back).
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
        } else if now.codex {
            match now.crashed {
                Some(true) => return ExitRecord::Crashed,
                Some(false) => return ExitRecord::Removed,
                None if waited >= EXIT_GONE => return ExitRecord::Unread,
                None => {}
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
    let cause = if stalled {
        ExitCause::Stall
    } else {
        ExitCause::Exit
    };
    after_exit_as(opts, snap, left, upgrade, cause)
}

/// Why an agent that is no longer running is relaunched ([`after_exit_as`];
/// a Codex's, the Codex lane's own step).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExitCause {
    /// Its own exit: relaunched only when it left Claude's record (a crash),
    /// never after a graceful one — someone's decision.
    Exit,
    /// The stall's remedy ended it (U1): nobody's decision about the session.
    Stall,
    /// aterm itself ended while it ran (2026-09-27): its shell and tab went
    /// with aterm — a hangup the agent may have answered by removing its own
    /// record, which is no one's decision about the session either.
    HostEnded,
}

/// THE AGENT OF A TAB A CRASH TOOK (2026-09-27, the owner: after an exit they
/// did not choose, reopen the layout and relaunch the agents aterm hosted).
/// aterm ended while `snap`'s agent ran — a crash, a kill, a power loss, a
/// restart — and the next launch reopened its tab, whose new shell is
/// `snap.shell` in tab `snap.tab`; the rest of `snap` is what the live layout
/// carried of the agent. Relaunched on its conversation as [`after_exit`]
/// would, except that the graceful-exit rule does not apply (the hangup was
/// aterm's, not a person's), and a relaunch in flight for another tab — the
/// crashed run's — is closed, not waited on: that tab is gone. A CODEX's
/// snapshot (the layout carries its run) is relaunched by the Codex lane,
/// on the thread it held.
#[must_use]
pub fn after_host_ended(opts: &Opts, snap: &Snapshot, upgrade: bool) -> Report {
    after_exit_as(
        opts,
        snap,
        &ExitRecord::Unread,
        upgrade,
        ExitCause::HostEnded,
    )
}

fn after_exit_as(
    opts: &Opts,
    snap: &Snapshot,
    left: &ExitRecord,
    upgrade: bool,
    cause: ExitCause,
) -> Report {
    // A CODEX is relaunched by the Codex lane, on the thread it held.
    if snap.codex.is_some() {
        return super::upgrade_drive::codex_after_exit(opts, snap, left, upgrade, cause);
    }
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
        // (`Crashed` is a Codex's word; a Claude Code look never says it.)
        ExitRecord::Unread | ExitRecord::Crashed => {
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
    // THE HARNESS'S OWN RESTART ENDED THIS VERY AGENT (S0 of the in-flight
    // review, 2026-09-27): its record names the process that left, in this
    // tab. The exit is that restart's — carried from where it stopped, or
    // said — and never the graceful exit its own SIGTERM made of it: until
    // then a restart the back-off let expire was closed here and fell through
    // to `ended:graceful-exit`, the agent never relaunched and nothing said.
    // Not for a tab aterm's own end took ([`ExitCause::HostEnded`]): there
    // the record too old to act on is closed below and a fresh relaunch made.
    if cause != ExitCause::HostEnded
        && let Some(st) =
            load(opts, &session).filter(|st| ended_by_restart(st, &snap.tab, snap.pid, now_s()))
    {
        r.to = format!("{}({})", st.to, st.source);
        return carry_own(opts, r, st, &session, &snap.tab);
    }
    // A relaunch already in flight for this conversation is carried on, not
    // typed twice. One too old to act on is closed on the record instead, and
    // this step makes a fresh one: the host decided just now that nobody is
    // at the tab, which is what the age limit stood in for.
    if let Some(mut st) = load(opts, &session).filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != snap.tab && cause != ExitCause::HostEnded {
            return said(r, "wait:conversation-in-other-tab");
        }
        // A record of ANOTHER process than the one that just left: that
        // relaunch landed (its agent is the one that left, adopted and gone
        // again before its continuation) or was overtaken — closed, and this
        // exit relaunched afresh.
        let overtaken = if cause == ExitCause::HostEnded && st.tab != snap.tab {
            Some((
                "host-ended",
                "aterm ended while this relaunch was in flight; its tab was reopened",
            ))
        } else {
            (st.pid != snap.pid).then_some((
                "exited-before-continuing",
                "the relaunched agent exited before its continuation was typed",
            ))
        };
        if let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s())) {
            if !opts.dry_run {
                if overtaken.is_some() {
                    // The exit that closes it is ANOTHER process's — the
                    // relaunched agent's, or a later one's — never the end
                    // of the agent this record names, which its relaunch
                    // had already seen gone: no failure after ITS exit
                    // (`upgrade_status::Row::failed_after_exit`), so the
                    // owner's view vets the stop against a holder as any
                    // stop's, never a day-long stall with `claude --resume`
                    // for a relaunch that landed (the review of 2026-09-27:
                    // a graceful `/exit` of the relaunched agent read so).
                    st.exited_at = 0;
                } else {
                    st.seen_gone(now_s());
                }
                st.stop(why, now_s());
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
    // The harness's own restart's SIGTERM is the record carried or said
    // above ([`ended_by_restart`]); the stall's remedy is no one's decision
    // about the session (U1).
    if survivor.is_none() && cause == ExitCause::Exit {
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
    // The model the rule moves the conversation to rides the relaunch when
    // one is due — read off the crash's own record (a graceful exit left none
    // to read it by) — else a `/model` made since the launch, which the
    // launch's `--model` would undo.
    let (riding, hand) = if resume {
        restart_models(
            opts,
            survivor.as_ref(),
            &snap.start,
            &session,
            &snap.argv,
            true,
        )
    } else {
        (None, None)
    };
    let mode = resume_mode(opts, &session, tab_mode(opts, &snap.tab));
    let launch = Launch {
        session: &session,
        resume,
        cwd: &cwd,
        agent: snap.pid,
        argv: &snap.argv,
        exe: &exe,
        model: riding.as_deref().or(hand.as_deref()),
        mode: mode.as_deref(),
    };
    let mut st = St {
        from: version.unwrap_or_default(),
        to,
        source,
        cause: match (resume, cause) {
            (false, _) => CAUSE_FRESH,
            (true, ExitCause::Stall) => CAUSE_STALL,
            (true, ExitCause::HostEnded) => CAUSE_HOST,
            (true, ExitCause::Exit) => CAUSE_EXIT,
        }
        .to_string(),
        salt: now_s(),
        launch_model: upgrade::launch_model(&snap.argv).unwrap_or_default(),
        model_list: riding.clone().unwrap_or_default(),
        fallback_from: exit_fallback(opts, &session, riding.as_deref(), hand.as_deref()),
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
            match (resume, launch.model) {
                (false, _) => "would-relaunch:fresh".to_string(),
                (true, Some(m)) => format!("would-relaunch:model={m}"),
                (true, None) => "would-relaunch".to_string(),
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
    record_asked(opts, &session, &st.model_list);
    ledger(opts, &said(r.clone(), "exited"), &st.line);
    let r = relaunch(opts, r, &mut st, &mut c, &session, &Live);
    save(opts, &session, &st);
    r
}

/// The fallback origin an exit's relaunch of `session` records
/// ([`fallback_carried`] of the one its last record keeps).
pub(super) fn exit_fallback(
    opts: &Opts,
    session: &str,
    riding: Option<&str>,
    hand: Option<&str>,
) -> Option<String> {
    let origin = load(opts, session).as_ref().and_then(fallback_origin);
    fallback_carried(origin, riding, hand)
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

// ------------------------------------------------ a restart left in flight

/// How long after its agent was seen gone ([`St::exited_at`]) a restart that
/// STOPPED is still the one that ended an agent leaving its tab: twice
/// [`STALE_S`], the longest a carry of it lives (its bound, one pause of
/// [`CARRY_EVERY`] and one attempt's waits) — so a pid the kernel hands a
/// later process in the same tab is never read as that agent.
const STOPPED_OWN_S: u64 = 2 * STALE_S;

/// Whether `st` is the record of a RESTART OF THE HARNESS'S OWN whose signal
/// ended the agent `pid` that left tab `tab` at `now`: a Claude Code record
/// of a cause that ends its agent ([`ends_its_agent`]) naming that very
/// process in that tab — in flight (its signal sent: one refused or never
/// sent is put back before anything waits on it), or stopped at most
/// [`STOPPED_OWN_S`] after its agent was seen gone ([`St::exited_at`]). A
/// stop before any exit — a signal the kernel refused — left the agent
/// running, and its later exit is someone's.
pub(super) fn ended_by_restart(st: &St, tab: &str, pid: u32, now: u64) -> bool {
    st.agent == upgrade::Agent::Claude
        && st.tab == tab
        && st.pid == pid
        && ends_its_agent(&st.cause)
        && (st.in_flight()
            || (matches!(st.phase, upgrade::Phase::Failed(_))
                && st.exited_at != 0
                && now.saturating_sub(st.exited_at) <= STOPPED_OWN_S))
}

/// Whether a Codex that left its tab was ended by the restart record `st`
/// ([`restarted`]): its `/exit` typed and the old TUI not yet replaced —
/// exiting, or relaunched with no new TUI adopted and none found gone (the
/// old TUI's exit can be read after the line is typed, while the new one
/// comes up). Once the relaunched TUI is ADOPTED
/// (`upgrade_codex_drive::adopt` stamps [`St::resumed_pid`]) a Codex leaving
/// is that TUI, its exit its own (the review of 2026-09-27: matched on the
/// tab alone, a person's `/exit` of the adopted TUI was carried as the
/// restart's, the harness's hand held on the tab until the record expired,
/// and the tab badged as an agent the harness had ended). So is one the
/// restart FOUND ([`St::relaunched_pid`]) and never adopted — its adoption
/// ran out of time, its carry-on found a person at the keys — once it is
/// gone: only that TUI could have left then, the old one long before it
/// came up. One found and still alive is not what left.
fn codex_exit_is_restarts(st: &St) -> bool {
    match st.phase {
        upgrade::Phase::Exiting { .. } => true,
        upgrade::Phase::Relaunched { .. } => {
            st.resumed_pid == 0 && (st.relaunched_pid == 0 || alive(st.relaunched_pid))
        }
        _ => false,
    }
}

/// The record of the restart [`restarted`] finds for tab `opts.only_sid`, with
/// the conversation it is filed under.
fn restart_record(opts: &Opts, codex: bool, pid: Option<u32>) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let now = now_s();
    let dir = std::fs::read_dir(state_dir(opts)).ok()?;
    dir.flatten().find_map(|e| {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            return None;
        }
        let session = path.file_stem()?.to_string_lossy().into_owned();
        let st = load(opts, &session)?;
        let ours = if codex {
            st.agent == upgrade::Agent::Codex && st.tab == tab && codex_exit_is_restarts(&st)
        } else {
            pid.is_some_and(|pid| ended_by_restart(&st, tab, pid, now))
        };
        ours.then_some((session, st))
    })
}

/// THE HARNESS'S OWN RESTART ENDED THE AGENT THAT LEFT tab `opts.only_sid`
/// (S0 and S3 of the in-flight review, 2026-09-27): the upgrade's, or the
/// restart in place's, whose step returned with the relaunch still to type
/// (`wait:held`, `wait:typing`, `wait:shell-prompt`, `wait:resume` …) and
/// whose agent then left the tab with no step watching it. `codex`: the
/// agent that left was a Codex — found by its tab, whatever its snapshot
/// names: the tab's one Codex record in flight, no relaunched TUI adopted yet nor found
/// gone, is its restart's (filed per tab, `codex-<tab>`,
/// [`codex_exit_is_restarts`]); else `pid` is the Claude Code that left (the
/// host's snapshot of it), and the restart is the record that ended that very
/// process ([`ended_by_restart`]). Read-only: the state files, and whether the
/// relaunched Codex TUI a record names still lives; no lock.
#[must_use]
pub fn restarted(opts: &Opts, codex: bool, pid: Option<u32>) -> bool {
    restart_record(opts, codex, pid).is_some()
}

/// THE RESTART [`restarted`] FOUND, CARRIED ON for its agent's exit: one step
/// under the one lock every actor on the upgrade state takes. `adopted` (or
/// `done:fresh`) once the relaunch holds the tab; `wait:<why>` while a
/// person types at the returned prompt, a hold or a hand is on the tab, the
/// shell has not taken the terminal back, or the relaunch has not registered
/// — nothing is typed then, and the host tries again ([`CARRY_EVERY`]);
/// `busy:<why>` for another actor; and `refused:<why>` once it can never
/// land ([`carry_own`]), or is no longer to be found
/// (`refused:no-restart-in-flight`) — the one case a person is asked about,
/// never the graceful exit the restart's own SIGTERM made of it.
#[must_use]
pub fn carry_restart(opts: &Opts, codex: bool, pid: Option<u32>) -> Report {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let r = Report {
        pid: pid.unwrap_or(0),
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
    let Some((session, mut st)) = restart_record(opts, codex, pid) else {
        return said(r, "refused:no-restart-in-flight");
    };
    // THE EXIT THE HOST JUST SAW: this carry follows it by at most one
    // [`CARRY_EVERY`], so it is when the agent was seen gone, and the
    // stale-exit bound counts from it ([`STALE_S`]) — never from a signal a
    // slow shutdown outlived (the review of 2026-09-27). Kept for the next
    // try, which counts from the same exit. A Codex restart's lane keeps its
    // own.
    if st.agent == upgrade::Agent::Claude && st.seen_gone(now_s()) && !opts.dry_run {
        save(opts, &session, &st);
    }
    let r = Report {
        session: session.clone(),
        from: st.from.clone(),
        to: format!("{}({})", st.to, st.source),
        ..r
    };
    carry_own(opts, r, st, &session, &tab)
}

/// A restart of the harness's own whose signal ended its agent
/// ([`ended_by_restart`], or a Codex restart in flight), carried from where
/// it stopped — and FINAL where it can no longer land: one already stopped
/// says so again, one too old to act on is stopped ([`St::stop`], with when
/// its agent was seen gone) and one that stops in the carry says so, each
/// `refused:<why>` (S0 of the in-flight review, 2026-09-27). One relaunched
/// and never registered is never relaunched afresh: its line was typed once,
/// and a second minutes later, at a prompt a person may be typing at, is
/// what [`STALE_S`] exists to prevent. A dry run says `would-refuse:<why>`
/// and writes nothing.
fn carry_own(opts: &Opts, r: Report, mut st: St, session: &str, tab: &str) -> Report {
    let refused = |r: Report, why: &str| {
        said(
            r,
            if opts.dry_run {
                format!("would-refuse:{why}")
            } else {
                format!("refused:{why}")
            },
        )
    };
    if let upgrade::Phase::Failed(why) = &st.phase {
        return refused(r, why);
    }
    // A Codex restart is its lane's to carry, its own age limit included:
    // counted from its typed `/exit`, never from the TUI's exit — a TUI still
    // alive a minute after its `/exit` is one the `/exit` did not take.
    if st.agent == upgrade::Agent::Claude
        && let Some((why, detail)) = expired(&st, now_s())
    {
        if !opts.dry_run {
            st.seen_gone(now_s());
            st.stop(why, now_s());
            ledger(opts, &said(r.clone(), format!("failed:{why}")), detail);
            save(opts, session, &st);
        }
        return refused(r, why);
    }
    let r = match st.agent {
        upgrade::Agent::Codex => super::upgrade_drive::codex_carry_in_flight(opts, r, st, session),
        upgrade::Agent::Claude => carry_in_flight(opts, r, st, session, tab),
    };
    match r.step.strip_prefix("failed:").map(str::to_string) {
        Some(why) => refused(r, &why),
        None => r,
    }
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
    /// false` — configuration took the power away. Said once on the
    /// session's attention, as what it is.
    Limited,
    /// Nobody asked: relaunch it.
    Relaunch,
    /// The harness's OWN RESTART ended the agent and left its relaunch in
    /// flight ([`restarted`]: the upgrade's SIGTERM, a Codex `/exit` it
    /// typed, the restart in place's): the exit is that restart's — never a
    /// person's, a holder's or the owner's limit — and it is CARRIED
    /// ([`carry_restart`]) until it lands or is said. What is typed at the
    /// prompt still waits on a person and a hold: the relaunch line's own
    /// look (`held`, the prompt's mark) makes it wait, never this decision.
    Restarted,
}

/// THE DECISION: whose exit this was. `allowed` is `[harness] relaunch`
/// (the relaunch is written for Claude Code and Codex); `status` the session's `status`
/// reply (`None`: unreadable — nobody, as an absent field is); `grace_s` is
/// `[harness] human_grace_s`; `stalled`: the agent had stopped reading its
/// input when it exited (its supervisor held for the stall), so a person's
/// keystroke just before was no `/exit` — nothing read it — and the exit is
/// the stall's remedy's (U1); `restarted`: the harness's own restart ended
/// it ([`restarted`]), which is asked FIRST — its exit is that restart's,
/// whoever is at the tab and whatever the owner's switch says
/// ([`OnExit::Restarted`]; S0 of the in-flight review, 2026-09-27: the
/// owner who pressed Upgrade now had typed within the grace, so the exit
/// read as theirs and the agent the upgrade ended was never relaunched). A
/// person or a holder owns any other exit before the owner's limit is asked
/// about, so their own exit is never said to them.
#[must_use]
pub fn on_exit(
    allowed: bool,
    status: Option<&str>,
    grace_s: u32,
    stalled: bool,
    restarted: bool,
) -> OnExit {
    if restarted {
        OnExit::Restarted
    } else if !stalled && status.is_some_and(|s| person_present(s, grace_s)) {
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
/// nobody (the supervisor's own continuations are typed so), and never this
/// process's own hand on a tab whose agent it restarts
/// ([`super::upgrade_drive::Hand`]).
#[must_use]
pub fn driver_hand(status: &str) -> bool {
    status.split_whitespace().any(|t| {
        t.strip_prefix("hand=").is_some_and(|hand| {
            (hand.starts_with("lease:") && !super::upgrade_drive::our_hand(hand))
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

/// The longest pause before the next attempt at a restart of the harness's
/// own ([`OnExit::Restarted`]): the back-off's ten-minute step outlasts
/// [`STALE_S`], so after three misses its record expired between two
/// attempts, never carried (S0 of the in-flight review, 2026-09-27). Tried
/// at least this often, it is carried while it may still act and said the
/// attempt after it cannot.
pub const CARRY_EVERY: Duration = Duration::from_secs(60);

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
    /// The exit being handled is a restart of the harness's own
    /// ([`OnExit::Restarted`]): carried, never left to a person, a holder or
    /// a limit — the model's `restarted`.
    restarted: bool,
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
        self.restarted = decision == OnExit::Restarted;
        match decision {
            OnExit::Relaunch | OnExit::Restarted => {
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
                self.restarted = false;
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
            // THE HARNESS'S OWN RESTART IS NEVER LEFT (the model's
            // `RestartCarried`, the review of 2026-09-27): its carry answers
            // no launch's own end and no agent that never exited, and were
            // it to, the tab would be left with nothing said — it is said,
            // as a relaunch that cannot land.
            Outcome::Left(_) if self.restarted => {
                self.pending = false;
                self.badged = true;
                Say::Cannot
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
        self.restarted = false;
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

    /// Whether the exit being handled is a restart of the harness's own
    /// ([`OnExit::Restarted`]): the model's `restarted`.
    #[must_use]
    pub fn restarted(&self) -> bool {
        self.restarted
    }
}

#[cfg(test)]
#[path = "relaunch_tests.rs"]
mod tests;
