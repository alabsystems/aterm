// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The relaunch primitive: a Claude Code that is no longer running is started
//! again in its own tab on its own conversation: one line typed at the tab's
//! shell once it holds the terminal again, `--resume <id>` at its end
//! ([`super::upgrade::relaunch_line`]), then, once the new process holds the
//! conversation, one turn telling it to carry on. Unless configured
//! otherwise the harness is fully automatic, so nothing here waits on a
//! person (owner, 2026-09-24).
//!
//! Callers:
//!
//! * The live upgrade ([`super::upgrade_drive`]) ends the agent (SIGTERM after
//!   it answered READY) and relaunches it on the newer build ([`relaunch`],
//!   [`await_new`], [`carry_on`]).
//! * The restart in place ([`restart_here`]): at a point nothing typed can
//!   answer, such as Claude Code's critical-memory banner ([`Restart::Memory`]),
//!   the host ends the agent without announcement and relaunches the same
//!   program.
//! * Relaunch on exit ([`after_exit`]): an agent that crashed out of its tab
//!   is relaunched on its conversation. What the line needs is read while it
//!   runs ([`snapshot`]). A crash leaves `sessions/<pid>.json`; a graceful
//!   exit removes it and was someone's decision, never relaunched. Which one
//!   it was is read as the exit is seen ([`exit_record`], within
//!   [`EXIT_SETTLE`]) and kept ([`ExitRecord`]), since a later Claude Code
//!   removes dead processes' records. The host decides whether ([`on_exit`]:
//!   `[harness] relaunch`, no keystroke within `human_grace_s`, no holder)
//!   and when ([`Relaunches`]: a growing back-off; [`Outcome::Cannot`] is the
//!   keyed attention).
//!
//!   A Codex is relaunched on exit too ([`CodexRun`]): its snapshot names its
//!   thread (an embedded TUI's writer lock, or its launch `resume <id>`), and
//!   the crash witness is the shell's command-block exit status (`0` or a
//!   SIGHUP/SIGINT/SIGTERM: someone's; SIGKILL or failure: a crash). The
//!   thread lock is left behind by every signal, so it is no witness. A
//!   daemon-mode client that named no thread resumes on the one thread its
//!   daemon began in its directory after it started (UUIDv7 ids), never a
//!   guess between several.
//!
//!   An exit the harness's own restart made, whose step returned before the
//!   relaunch line was typed, is read first ([`restarted`],
//!   [`OnExit::Restarted`]): never a person's, a holder's or `[harness]
//!   relaunch = false`. It is carried from where it stopped
//!   ([`carry_restart`]) at least every [`CARRY_EVERY`], so the record cannot
//!   expire between tries; typing still waits on a person and a hold, and a
//!   failure is said as `refused:<why>`.
//!
//! In the window the new process belongs to its supervisor as soon as it holds
//! the conversation (`adopted`, [`Opts::hand_back`]); the continuation is
//! typed at that loop's first idle point ([`owed`], [`resume`]).
//!
//! Every fact is re-read at the act and every upgrade guard is kept: the
//! shell's own foreground job on an aterm tab's terminal, the tab's unique PTY
//! claim re-read before typing, no live process holding the conversation,
//! adoption only of a child of the shell the line was typed at. A relaunch in
//! flight is filed under `<state>/upgrade/<conversation>.json` like an
//! upgrade, so whichever caller comes next finishes it.
//!
//! THE LINE CARRIES THE LOGIN ITS AGENT RUNS UNDER
//! (`docs/DESIGN-claude-accounts-2026-10-01.md` §6): Claude Code chooses its
//! login by `CLAUDE_SECURESTORAGE_CONFIG_DIR`, and the tab's shell keeps the
//! environment it was started with — so a session relaunched under another
//! of the owner's accounts would fall back to the shell's login at its next
//! relaunch unless every relaunch types the variable again. Each planner
//! does: the restart in place, the relaunch on exit and after aterm itself
//! ended, and the upgrade's own all re-apply exactly what the process's own
//! exec environment held ([`Snapshot::login`]: unset, set and empty, or a
//! directory — three lines), and an account move ([`restart_under`]) types
//! its target's instead. It is REQUIRED environment ([`line_for`]): a line
//! that cannot hold it is refused, never typed without it. A machine with no
//! roster has no such variable, and every line is what it was.
//!
//! The one line also HEALS THE SHELL it is typed at (2026-09-26): a tab whose
//! shell lost its integration key before the re-key channel existed reads
//! `status integration=degraded`, and nothing but a line typed while the
//! SHELL holds the terminal can reach it. The relaunch line is that line, for
//! every caller above: the window is asked for a one-use key file (`rekey
//! shell=<pid>`) and the shell takes it in front of the relaunch
//! ([`type_relaunch_line_with`], [`upgrade::with_rekey`]). The same line
//! reaches a shell whose integration predates LOADERS (2026-09-26: `status
//! integration_rev=frozen`), and a loader whose prompt hook never runs
//! (2026-10-01: a fish from before the prompt event under its own prompt),
//! which no body pointer ever will: the window names its own loader in the
//! file for a shell that does not sign the window's revision, and the line
//! sources it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::Value;

use super::accounts;
use super::upgrade::{self, Dialect, SessionFile, Version};
use super::upgrade_drive::{
    Client, Hand, Job, Kernel, Live, Opts, Report, SAME_PROGRAM, Screen, SessionRosterCache, St,
    TAIL_BYTES, Targets, Terminated, Typed, alive, composer_clear, composer_empty, connect,
    continuation_composer_empty, conversation_live, cwd_of, exe_of, first_word, foreground_shell,
    held, host_roster, ids, is_our_ancestor, kernel_start, launch_kept, ledger, line_model_of,
    live_background, load, model_and_mark, native_root, note_stuck, now_s, owned_by_aterm,
    process_in_tab, record_asked, require_unique_owner, restart_models, restore, roster, said,
    save, screen, session_file_of, session_files, since, squash, state_dir, supervisor_typed,
    sweep_lock, table, tail_to_end, take_back_ask, tasked, terminate, transcript,
    transcript_exists, turn, turn_fenced, type_line, typing_fence, under_hand,
    unique_tab_for_group,
};
use crate::supervise::limit::one_line;
pub use crate::supervise::policy::turn_end::Restart;
use crate::supervise::screen::HumanInput;

// ---------------------------------------------------------------- the plan

/// A relaunch planned from the live process: the shell that gets its prompt
/// back, its dialect (for a re-key in the same line), and the line typed.
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

/// What EVERY Claude Code relaunch exports (the survey switch, the same one aterm writes into
/// the user's settings and exports from the managed launcher): a same-program relaunch runs
/// the process image, which the launcher's own exports never reach, and a session started with
/// `--setting-sources` that leaves the user's settings out never reads the file.
pub(super) const CLAUDE_LAUNCH_ENV: [(&str, &str); 1] = [(SURVEY_SWITCH_VAR, "1")];

/// The environment variable Claude Code turns every rating survey off by.
pub(super) const SURVEY_SWITCH_VAR: &str = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY";

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
    /// (Claude's default). `None` keeps the launch's flags — all but a
    /// `--model` the conversation's record marks FAILED, which [`plan`]
    /// drops ([`super::upgrade_models::line_model`]).
    pub(super) model: Option<&'a str>,
    /// The permission mode to resume in ([`resume_mode`],
    /// [`with_permission_mode`]). `None` keeps the launch's flags.
    pub(super) mode: Option<&'a str>,
    /// THE LOGIN THE LINE MUST CARRY ([`login_env`]): the value of
    /// [`accounts::LOGIN_ENV`] for the relaunched process — the one the
    /// process it replaces had in its own exec environment
    /// ([`Snapshot::login`]: set and empty is the default login NAMED, which
    /// is not the same line as none), or an account move's target. `None`
    /// adds nothing: the process had no such variable, and the new one takes
    /// its shell's, as every relaunch did before accounts existed.
    pub(super) login: Option<&'a str>,
}

/// The REQUIRED environment of a relaunch line for `login`
/// ([`Launch::login`]): the one pair `(LOGIN_ENV, value)`, or none.
/// Required means the line is refused rather than typed without it
/// ([`line_for`]): a relaunch that dropped it would resume the conversation
/// under whatever login the tab's shell has — silently the wrong account.
pub(super) fn login_env(login: Option<&str>) -> Vec<(&'static str, &str)> {
    login
        .map(|value| (accounts::LOGIN_ENV, value))
        .into_iter()
        .collect()
}

/// Plan the relaunch from the live process: `shell` (the parent the job read
/// proved holds the agent, [`foreground_shell`]; never re-derived), its
/// dialect, the heal it needs, the resume directory, and the line
/// ([`line_for`]). The upgrade plans twice: before its announcement, so an
/// uncarryable launch is refused early, and again before its signal.
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
    // A line that asks no model of its own never carries a `--model` the
    // record marks FAILED (2026-10-01, [`line_model_of`]).
    let model = line_model_of(opts, launch.session, launch.argv, launch.model);
    let line = line_for(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        launch.exe,
        launch.argv,
        launch.resume.then_some(launch.session),
        model,
        launch.mode,
        &login_env(launch.login),
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
///
/// THE LINE'S ENVIRONMENT IS TWO KINDS (the accounts design's feasibility
/// review, finding 5: one list, dropped whole on any error, would have
/// relaunched a session on the wrong account the day a line ran long).
/// `required` — the login the relaunched process must run under
/// ([`login_env`]) — is on the line or the line is REFUSED (`refused:line`).
/// The survey switch ([`CLAUDE_LAUNCH_ENV`]) is a nicety, dropped first: the
/// line is tried with both, then with the required part alone. With nothing
/// required that is the line and its fallback as they always were.
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
    required: &[(&str, &str)],
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
    // The survey switch is a nicety and the line's bound a hard one: a launch
    // whose line fits only WITHOUT the export (the switch adds 38 bytes to
    // a bound of 1000) is relaunched without it, never refused for it. What
    // is REQUIRED stays on both tries, first on the line.
    let both: Vec<(&str, &str)> = required.iter().copied().chain(CLAUDE_LAUNCH_ENV).collect();
    upgrade::relaunch_line_env(dialect, heal, cd, &both, exe, &flags)
        .or_else(|_| upgrade::relaunch_line_env(dialect, heal, cd, required, exe, &flags))
        .map_err(|e| NoPlan::Refused {
            why: format!("line:{e}"),
            what: "line".to_string(),
            detail: e,
        })
}

/// `flags` with every `--model` spelling replaced by `model` (before the
/// trailing `--resume` pair), or dropped when it is empty. A command-line model
/// is session-only, unlike `/model <id>`, which also changes the default.
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

/// The permission mode `session` last ran in: the transcript's newest
/// `{"type":"permission-mode"}` row. `None` when absent or unknown.
fn last_permission_mode(home: &Path, session: &str) -> Option<String> {
    let path = transcript(home, session)?;
    let (tail, _) = tail_to_end(&path, TAIL_BYTES);
    permission_mode_of(&tail)
}

/// [`last_permission_mode`]'s scan, pure: the newest `permission-mode` row
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
/// (`shown`, [`screen_mode`]), else the transcript's newest `permission-mode`
/// row ([`last_permission_mode`]). Claude writes that row only when it
/// re-appends session metadata, so a mode chosen at idle reaches the screen
/// long before the file.
pub(super) fn resume_mode(opts: &Opts, session: &str, shown: Option<String>) -> Option<String> {
    shown.or_else(|| last_permission_mode(&opts.home, session))
}

/// The pill `tab` shows now ([`screen_mode`]), for a restart with no look of
/// its own.
fn tab_mode(opts: &Opts, tab: &str) -> Option<String> {
    let mut c = connect(opts, tab).ok()?;
    screen_mode(&screen(&mut c, tab)?.rows)
}

/// The `--permission-mode` value of the pill under Claude's composer on `rows`
/// ([`super::lights::read_screen`]); `None` without a composer or known pill.
pub(super) fn screen_mode(rows: &[String]) -> Option<String> {
    super::lights::read_screen(rows)
        .and_then(|shown| shown.mode)
        .map(|mode| mode_flag(mode).to_string())
}

/// The `--permission-mode` value of a pill's mode. Manual mode is `default`,
/// the spelling every build accepts.
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

/// `flags` (the launch's, from [`upgrade::rewrite_argv`], resume pair last)
/// resumed in permission `mode` ([`resume_mode`]). Claude does not restore a
/// resumed conversation's mode, so the launch's flags alone could bring a
/// session back in bypass after its person had left it.
///
/// A relaunch never moves a session into bypass: with the mode bypass or
/// unknown, the flags are returned as launched. Otherwise every bypass and
/// mode flag goes, `--allow-dangerously-skip-permissions` keeps bypass in the
/// shift+tab cycle if the launch had it, and the mode is always named so no
/// setting or build default picks another. Rewritten on the parsed flags, like
/// [`with_model`].
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

/// A session with no relaunch plan. A wait types and records nothing; a
/// refusal fails the relaunch for this target with one ledger line
/// (`would-refuse:` in a dry run, which records nothing).
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

/// How long a relaunch in flight may still act on its tab, from the agent's end
/// (exiting) or the relaunch line (relaunched): after that the person may be
/// typing at the prompt, or the tab gone.
///
/// The end is the one a look saw ([`St::exited_at`]), so a slow shutdown is
/// not refused the moment it exits. A look's first sighting says nothing of
/// when the exit came, so it decides on the signal ([`expired`] before the
/// stamp); the host's carry, which knows the exit is fresh, stamps it first
/// ([`carry_restart`]).
pub const STALE_S: u64 = 300;

/// A stale exit's ledger line, for `who` (`the agent`, `Codex`), alike in
/// both lanes: an end no look has seen yet is only the one asked for
/// ([`St::exited_at`]), and so is one seen only within the bound (a sweep
/// whose first sighting stamps it now), and the wait the last try recorded
/// ([`St::wait`]) says what held the line, in a person's words (tab #1,
/// 2026-10-01: five minutes of `wait:typing`, then "never relaunched" and
/// nothing more). A wait with no words gives the bare line; `--status`'s
/// wait log keeps it.
pub(super) fn stale_exit_line(st: &St, who: &str, now: u64) -> String {
    let what = if st.exited_at != 0 && now.saturating_sub(st.exited_at) > STALE_S {
        "ended"
    } else {
        "was asked to end"
    };
    let held = super::upgrade_drive::wait_words_of(st.agent, &st.wait)
        .map(|words| format!(": {words}"))
        .unwrap_or_default();
    format!(
        "{who} {what} over {} minutes ago and was not relaunched{held}",
        STALE_S / 60
    )
}

/// Why a relaunch in flight whose agent is gone must stop instead of acting
/// on the tab, `(word, ledger detail)`, or `None` while it may still act.
/// An exit is stale [`STALE_S`] after its agent was seen gone, or after the
/// signal while no look has seen it gone yet.
///
/// A STALE EXIT NAMES WHAT HELD ITS LINE ([`stale_exit_line`]).
pub(super) fn expired(st: &St, now: u64) -> Option<(&'static str, String)> {
    use upgrade::Phase;
    match st.phase {
        Phase::Exiting { at_s } if now.saturating_sub(at_s.max(st.exited_at)) > STALE_S => {
            Some(("stale-exit", stale_exit_line(st, "the agent", now)))
        }
        Phase::Exiting { .. } if st.shell == 0 || !alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran the agent is gone: there is no prompt to relaunch at".to_string(),
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "no-resume",
            "the relaunched agent never registered the conversation".to_string(),
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
    /// Proven not the relaunch: another conversation, the ended process, a
    /// parent read as another than the recorded shell, or no shell recorded.
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

/// Whether `s` is the relaunch this record typed: another process than the one
/// that ended, holding the same conversation, and a child of the shell the line
/// was typed at (directly, or `exec`ed by a `( cd … )` subshell). Both halves
/// are needed: a person's fresh `claude` has the shell but not the
/// conversation; a hand resume elsewhere has the conversation but not the
/// shell. An unreadable parent is [`Relaunch::Unread`], never `NotOurs`, which
/// records a permanent failure.
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

/// Poll `cond` every 250 ms for at most `limit`, for conditions with no event
/// to wait on (exit has [`super::upgrade_wake::wait_exit`], the verdict
/// [`first_idle`]).
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

/// The agent ended (signalled or by itself): once it is gone and the shell has
/// its prompt back, type the relaunch line.
///
/// The harness's hand ([`super::upgrade_drive::Hand`]) is on the tab
/// throughout: taken or renewed here, renewed through every wait
/// ([`Hand::keep`]), kept while this connection types, and given back once
/// the relaunched agent is idle ([`await_new`]) or the relaunch fails. A
/// relaunch left `Exiting` keeps it until the next attempt renews it or it
/// lapses.
///
/// Gone is the kernel's `NOTE_EXIT` ([`super::upgrade_wake::wait_exit`]). A
/// graceful shutdown removes `sessions/<pid>.json` first; a crash never does,
/// so the record is not waited for.
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
    // Gone: from here the restart is the tab's record, stamped when the agent
    // is seen gone, never at the signal.
    st.seen_gone(now_s());
    hand.keep(c);
    let shell = st.shell;
    let prompt_back = wait_until(prompt_back_wait(), || {
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
    // When the agent's end was asked — known only for a restart that ended
    // it ([`end_asked_at`]): a person's keys before it went to the agent,
    // never to the prompt this line is typed at.
    let ended_at_s = end_asked_at(st);
    // A MODEL IS ASKED BY THE LINE THAT ASKS IT (2026-10-02): the model this
    // line carries is recorded as asked right before it is typed, and taken
    // back when it is not, whichever restart ended the agent (the upgrade's,
    // the restart in place, a relaunch on exit). Written before the signal,
    // a restart whose signal was refused or whose line never came kept it,
    // and it settled FAILED ten minutes later though no relaunch asked it.
    let asked = record_asked(opts, session, &st.model_list);
    let typed = type_relaunch_line(
        opts,
        c,
        shell,
        &tab,
        session,
        &line,
        dialect,
        PromptLook {
            mark: &mut st.prompt,
            ended_at_s,
        },
        |c, pid, tab| process_in_tab(c, pid, tab, None, None),
    );
    if typed.is_err() {
        take_back_ask(opts, session, asked);
    }
    match typed {
        Ok(()) => st.prompt = None,
        Err(RelaunchLineError::Wait(why)) => {
            // Kept on the record by every try — the host's carries too, which
            // no sweep's visit notes — so the move's row says what holds the
            // line, and the stale bound that ends it names it ([`expired`]).
            st.note_wait(why, now_s());
            return said(r, format!("wait:{why}"));
        }
        Err(RelaunchLineError::Turn(e)) => {
            st.stop("relaunch-refused", now_s());
            let r = said(r, format!("failed:relaunch:{}", first_word(&e)));
            ledger(opts, &r, &st.line);
            return r;
        }
    }
    st.phase = upgrade::Phase::Relaunched { at_s: now_s() };
    // A new line: no process of it is adopted yet ([`St::relaunched_pid`]).
    st.relaunched_pid = 0;
    ledger(opts, &said(r.clone(), "relaunched"), &st.line);
    save(opts, session, st);
    await_held(opts, r, st, c, hand, session, k)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RelaunchLineError {
    Wait(&'static str),
    Turn(String),
}

/// Type the relaunch line. The PTY is probed again right before typing, through
/// the seam the race test uses. The session's `status` is read as the line is
/// typed: a person typing within the grace, a halt, or anyone's hand holds the
/// tab ([`super::upgrade_drive`]'s `held`) and nothing is typed.
///
/// The shell is proven to hold the tab right before every try (`tab_probe`),
/// since a person may have started another program at the prompt. A prompt
/// that moved after the mark is not re-marked within the attempt: it waits
/// (`prompt-moved`), and a later attempt marks afresh.
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
/// clock) keeps its cursor. Where the shell's own mark of its prompt names
/// another row than the cursor's, the read's `human_ms` stamp decides: no
/// person's key since the agent's end was asked (`look.ended_at_s`) means
/// the shell's line holds nothing of a person's ([`first_mark`], tab #1 of
/// 2026-10-01), and where it cannot say so the wait names what holds it
/// (`mark-elsewhere`), never `typing`; a prompt the shell does not mark at
/// all that the shape rule cannot clear waits `unmarked` where the stamp
/// shows nobody typed.
///
/// Bash is typed to ([`type_line`]): bash 3.2 has no bracketed paste, and
/// keystrokes take no fence, so the mark is read once more right before them.
/// zsh and fish are pasted to (so no binding or abbreviation acts), fenced on a
/// fresh read ([`turn_fenced`]). Typing through the whole yield is a wait,
/// never a refusal. A prompt that never holds still is pasted unfenced once a
/// fresh read still finds it as marked and the shell holding the tab.
///
/// With the shell's `dialect` known, a `degraded` tab is re-keyed in this line
/// ([`type_relaunch_line_with`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn type_relaunch_line(
    opts: &Opts,
    c: &mut Client,
    shell: u32,
    tab: &str,
    session: &str,
    line: &str,
    dialect: Option<Dialect>,
    look: PromptLook<'_>,
    tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<(), RelaunchLineError> {
    type_relaunch_line_with(
        opts,
        c,
        shell,
        tab,
        line,
        dialect,
        look,
        || session_files(&opts.home).is_some_and(|files| !conversation_live(&files, session)),
        tab_probe,
    )
}

/// What one relaunch attempt judges the returned prompt by: its MARK, kept
/// across attempts (`St::prompt`, [`PromptMark`]), and when the agent's end
/// was asked (`ended_at_s`, Unix seconds, [`end_asked_at`]: the restart's
/// signal, or the `/exit` it typed; `0` unknown — a relaunch on exit, whose
/// agent left before anything was asked). A person's keys before that
/// instant went to the agent, never to this prompt, so a read whose
/// `human_ms` stamp dates the session's last person input before it proves
/// that nothing at the prompt is a person's typing ([`first_mark`]).
pub(super) struct PromptLook<'a> {
    pub(super) mark: &'a mut Option<PromptMark>,
    pub(super) ended_at_s: u64,
}

/// WHEN THE AGENT'S END WAS ASKED, as a relaunch line may date a person's
/// keys against it ([`PromptLook::ended_at_s`]): the record's
/// [`upgrade::Phase::Exiting`] instant, but only where its restart ENDED ITS
/// AGENT ITSELF ([`ends_its_agent`]: the upgrade's SIGTERM, a Codex `/exit`
/// it typed, a restart in place) — stamped before the signal, after a last
/// look that found no person's hand on the tab, so every person's key before
/// it went to the agent. A relaunch on exit ([`CAUSE_EXIT`], [`CAUSE_FRESH`],
/// [`CAUSE_STALL`], [`CAUSE_HOST`]) stamps its record when it is MADE, after
/// the agent left and its back-off ran (up to ten minutes): a person's keys
/// in between went to the prompt this line is typed at, and dated against
/// that stamp they would read as the agent's — `0`, unknown.
pub(super) fn end_asked_at(st: &St) -> u64 {
    match st.phase {
        upgrade::Phase::Exiting { at_s } if ends_its_agent(&st.cause) => at_s,
        _ => 0,
    }
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
/// whose shell does not sign the window's own revision (the window then also
/// names its own loader: the line upgrades the shell's integration in place,
/// 2026-09-26), and only while
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
    look: PromptLook<'_>,
    owner_clear: impl FnOnce() -> bool,
    mut tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<(), RelaunchLineError> {
    let PromptLook { mark, ended_at_s } = look;
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
        *mark = first_mark(c, tab, &scr, ended_at_s)?;
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
/// that runs its integration, an unintegrated one, a re-key of the shell's own already
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

/// Ask the window for a typed re-key of `tab`, whose foreground shell the
/// caller proved to be `shell` (`@<tab> rekey shell=<pid>`, owner-only). The
/// window writes a one-use key file, authorizes it, and answers its path, never
/// the key. `None` on any other answer.
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

/// A refused relaunch turn: a busy tab (another driver's turn or lease) is a
/// wait, never a refusal failing a relaunch whose agent is gone.
fn refused_line(e: String) -> RelaunchLineError {
    if e.starts_with("ERR busy") {
        RelaunchLineError::Wait("held")
    } else {
        RelaunchLineError::Turn(e)
    }
}

/// The typing half of [`type_relaunch_line_with`]: `line` at the marked prompt
/// (typed to bash, pasted fenced to zsh and fish) on the connection holding the
/// harness's hand ([`super::upgrade_drive::Hand`]), whose hold refuses every
/// other connection's writes.
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
                // Part of the line typed and no Enter: the prompt holds it
                // now, and the next attempt reads it as typing after the mark.
                Err(Typed::Cut(_)) => return Err(RelaunchLineError::Wait("cut")),
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
        Err(Typed::Cut(_)) => Err(RelaunchLineError::Wait("cut")),
        Err(Typed::Refused(e)) => Err(refused_line(e)),
    }
}

/// The FIRST mark of a returned prompt ([`PromptMark::of`] over `scr`), made
/// only where nothing is typed after the prompt yet (review of 2026-09-25:
/// typeahead that reached the prompt before its first read — typed while the
/// agent exited — BECAME the mark, and the line was appended to it). The cell
/// at the cursor and the one after it must be blank, whatever else: a caret
/// moved home over a typed command sits ON it (`typing`). Then, in order:
///
/// 1. THE SHELL'S OWN MARK, where its integration marked the input's start
///    (`133;B`, [`input_start`]) on the row the cursor is on — the row the
///    mark names reads, left of the mark's column, as the cursor's row does:
///    the cursor must sit exactly at that column (`typing` otherwise: text
///    after the prompt, or a caret moved into it).
/// 2. A MARK THAT NAMES ANOTHER ROW, with NO PERSON'S INPUT since the
///    agent's end was asked (`ended_at_s`, by the read's own `human_ms`
///    stamp, [`HumanInput::quiet_since`]): the shell's integration says it
///    is at its prompt, entering a command, and its line holds nothing a
///    person typed — the prompt is clean, typed at (tab #1, 2026-10-01: an
///    engine whose absolute rows had slid read the prompt's mark back as the
///    line above it — `claude --resume …` — and every attempt for five
///    minutes waited `typing` on a prompt nobody had touched, the agent left
///    down). With a person's input since, or no stamp that can say — typing
///    that wrapped past the prompt's row looks just so — it waits
///    `mark-elsewhere`, never `typing`: what holds the line is that the
///    shell's mark and the screen disagree and nothing else clears it.
/// 3. NO MARK — no integration, or a newest block that is not `entering` (a
///    command the shell still runs holds the terminal: `claude; read`'s
///    builtin) — the cursor must follow a blank (every common prompt ends in
///    one, and typing does not). The stamp never stands in for that shape:
///    it proves no person typed, not that the shell is at a prompt (typed
///    into `while :; do claude; read -k1; done`, the line's first key ends
///    the `read` and the rest goes to the agent the loop starts again). The
///    wait is `typing` where a person keyed since or the stamp cannot say,
///    `unmarked` where it shows nobody did.
///
/// `Ok(None)`: a read with no cursor, typed at as before.
fn first_mark(
    c: &mut Client,
    tab: &str,
    scr: &Screen,
    ended_at_s: u64,
) -> Result<Option<PromptMark>, RelaunchLineError> {
    let Some(mark) = PromptMark::of(scr) else {
        return Ok(None);
    };
    let row = scr
        .cursor
        .and_then(|(r, _)| scr.rows.get(r))
        .map_or("", String::as_str);
    let at_cursor: String = row.chars().skip(mark.col).take(2).collect();
    if !at_cursor.trim().is_empty() {
        return Err(RelaunchLineError::Wait("typing"));
    }
    let unkeyed = ended_at_s != 0
        && scr
            .human
            .quiet_since(ended_at_s.saturating_mul(1000), now_ms())
            == Some(true);
    match input_start(c, tab) {
        Some(start) if start.names(row) => {
            if mark.col == start.col {
                Ok(Some(mark))
            } else {
                Err(RelaunchLineError::Wait("typing"))
            }
        }
        Some(_) if unkeyed => Ok(Some(mark)),
        Some(_) => Err(RelaunchLineError::Wait("mark-elsewhere")),
        None if mark.left.chars().count() < mark.col
            || mark.left.ends_with(char::is_whitespace) =>
        {
            Ok(Some(mark))
        }
        None if unkeyed => Err(RelaunchLineError::Wait("unmarked")),
        None => Err(RelaunchLineError::Wait("typing")),
    }
}

/// Now in Unix milliseconds, the clock [`HumanInput::quiet_since`] reads a
/// stamp's age against.
fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}

/// Where the shell's integration marked the current prompt's input to start
/// (`133;B`), from the newest command block while it is `entering` (`blocks 1
/// --json`'s `"cmd"`/`"cmdcol"`).
struct InputStart {
    /// The text of the row the mark names, as the `line` verb reads it.
    row: String,
    /// The column the input starts at.
    col: usize,
}

impl InputStart {
    /// Whether the mark names `cursor_row` — the cursor's row as the screen
    /// read it: the two read the same left of the mark's column.
    fn names(&self, cursor_row: &str) -> bool {
        self.row
            .chars()
            .take(self.col)
            .eq(cursor_row.chars().take(self.col))
    }
}

/// [`InputStart`] as the tab answers it. `None` where no such mark reads: no
/// integration, a block in any other state, or a host that sends no `cmdcol`.
fn input_start(c: &mut Client, tab: &str) -> Option<InputStart> {
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
    Some(InputStart {
        row: text.strip_prefix(' ').unwrap_or(text).to_string(),
        col,
    })
}

/// Judge one prompt read against `mark` ([`PromptMark::at`]): `Ok` where the
/// line may be typed (prompt as marked, or no mark and nothing to mark by);
/// a wait on typing (`typing`), on movement (`prompt-moved`, dropping the mark
/// for a later attempt), or when a marked prompt can no longer be read.
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

/// Where a shell's prompt left the cursor: the screen row ([`Screen::first`]
/// added back, so reads compare), the column, the row's text left of the cursor,
/// and where right-prompt text began, if any. Kept across attempts
/// (`St::prompt`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PromptMark {
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) left: String,
    /// The column where right-prompt text began when marked, `None` for a blank
    /// rest of row; cells from the cursor up to it must stay blank ([`Self::at`]).
    pub(super) right: Option<usize>,
}

/// What a later read of the prompt shows against its [`PromptMark`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtPrompt {
    /// The cursor where the mark left it (text redrawn in place — a ticking
    /// clock — keeps it there), and nothing between it and the right prompt.
    Same,
    /// A person typed after the mark: further right on its row, wrapped onto
    /// the next row (or the row above after a scroll), or with the cursor back
    /// on the mark and text to its right (caret moved home, which changes no
    /// screen generation and so slips past the fence).
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
            // Anything now in the gap the mark saw blank is typing; a right
            // prompt growing into it reads as typing too (the safe error).
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

    /// [`Self::word`] read back; anything else is no mark, and the next
    /// attempt marks afresh ([`first_mark`]).
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

/// After the relaunch line: find the new process holding the conversation
/// ([`relaunch_of`]); an unreadable parent means not found yet. Found, carry on
/// ([`carry_on`]), or where a supervisor loop takes it ([`Opts::hand_back`]),
/// adopt it: the record stays in flight and the loop's next idle point types
/// the continuation ([`resume`]). A later step finding the relaunch in flight
/// comes here too, holding the tab as the relaunch did ([`await_held`]).
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

/// [`await_new`] with the harness's `hand` kept on the tab through the wait,
/// until the new agent is idle ([`first_idle`]), and through the continuation
/// when this step types it ([`carry_on`]). Its caller gives it back.
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
    // The hand stays on the tab until the new process is idle, and through the
    // continuation when this step types it.
    if let Some(sf) = &new {
        first_idle(c, hand, &st.tab, || {
            session_file_of(&home, sf.pid).is_some_and(|sf| sf.status == "idle")
        });
    }
    match new {
        Some(sf) if fresh => {
            // Nothing was said before it ended, so there is nothing to carry
            // on with; the finished move shows as `upgrade=done/…`.
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
        // ADOPTED ONCE, SAID ONCE (the live test of 2026-09-30 wrote two
        // `adopted` rows in one second: the upgrade's visit adopted the
        // relaunched agent, then the same pass's orphan pass, judging from
        // session files read before the relaunch, carried it again —
        // `upgrade_drive::registered_since` now reads them afresh). An
        // adopted restart stays in flight until its loop's idle point carries
        // it on ([`resume`]), and a step that finds it meanwhile — a visit, a
        // restart in place's or a relaunch's look at the record in flight —
        // finds the same process (the host's carry of the old agent's exit
        // is answered before it waits, [`carry_own`]). The adopted
        // process is on the record ([`St::relaunched_pid`], cleared as a
        // relaunch line is typed): that step says `adopted` to its caller and
        // writes nothing; a new move's process writes its own row.
        Some(sf) if opts.hand_back => {
            let mut r = said(r, "adopted");
            r.pid = sf.pid;
            if st.relaunched_pid == sf.pid {
                return r;
            }
            st.relaunched_pid = sf.pid;
            ledger(opts, &r, adopted_detail(st));
            save(opts, session, st);
            r
        }
        Some(sf) => carry_on(opts, r, st, c, session, &sf, Some(hand)),
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

/// What the `adopted` row says, from the record: a QUIET restart
/// ([`resumes_quietly`]) is resumed as it was with nothing typed, as the same
/// move's `done:resumed` says ([`quiet_outcome`]) — the live test of
/// 2026-09-30 read "its supervisor types the continuation" a second before
/// that row said nothing was typed. Any other move owes the continuation.
fn adopted_detail(st: &St) -> &'static str {
    if resumes_quietly(st) {
        "the relaunched agent holds the conversation; resumed while idle, nothing typed"
    } else {
        "the relaunched agent holds the conversation; its supervisor types the continuation at \
         its first idle point"
    }
}

/// Whether the restart `st` records is carried on with NOTHING typed
/// ([`St::quiet`]): an upgrade's quiet move, never a relaunch's
/// ([`relaunch_cause`], whose continuation says why it came back).
fn resumes_quietly(st: &St) -> bool {
    st.quiet && !relaunch_cause(&st.cause)
}

/// How long one [`await_held`] step waits for the relaunched process to
/// register its conversation; a later step waits again, until [`STALE_S`].
const HELD_WAIT: Duration = Duration::from_secs(90);
/// How long a cold restore's relaunch waits for a foreground agent to register,
/// so the one sweep lock is released for other tabs; a later retry checks the
/// same record. Long enough for [`ShellBack`] to see [`ENDED_AT_ONCE`].
const RESTORED_HELD_WAIT: Duration = Duration::from_secs(7);

#[cfg(test)]
thread_local! {
    /// A test's shorter [`HELD_WAIT`], for this thread only ([`held_wait`]):
    /// a step that must NOT end early is waited out in seconds.
    pub(super) static HELD_WAIT_IN_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// How long [`relaunch_held`] waits for the shell the agent left to take its
/// terminal back before it says `wait:shell-prompt`.
const PROMPT_BACK: Duration = Duration::from_secs(15);

/// A test's shorter relaunch waits, for this thread only
/// ([`WAITS_IN_TEST`]): a stand-in shell that never takes its terminal back, or
/// one that holds it throughout, is waited out in a fraction of a second instead
/// of the production seconds. The accessors ([`prompt_back_wait`],
/// [`ended_at_once`], [`shell_look`]) read it; the production values are the
/// constants beside them.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) struct WaitsInTest {
    pub(super) prompt_back: Duration,
    pub(super) ended_at_once: Duration,
    pub(super) shell_look: Duration,
}

#[cfg(test)]
thread_local! {
    pub(super) static WAITS_IN_TEST: std::cell::Cell<Option<WaitsInTest>> =
        const { std::cell::Cell::new(None) };
}

/// [`PROMPT_BACK`], or a test's shorter one ([`WAITS_IN_TEST`]).
fn prompt_back_wait() -> Duration {
    #[cfg(test)]
    if let Some(w) = WAITS_IN_TEST.with(std::cell::Cell::get) {
        return w.prompt_back;
    }
    PROMPT_BACK
}

/// [`ENDED_AT_ONCE`], or a test's shorter one ([`WAITS_IN_TEST`]).
fn ended_at_once() -> Duration {
    #[cfg(test)]
    if let Some(w) = WAITS_IN_TEST.with(std::cell::Cell::get) {
        return w.ended_at_once;
    }
    ENDED_AT_ONCE
}

/// [`SHELL_LOOK`], or a test's shorter one ([`WAITS_IN_TEST`]).
fn shell_look() -> Duration {
    #[cfg(test)]
    if let Some(w) = WAITS_IN_TEST.with(std::cell::Cell::get) {
        return w.shell_look;
    }
    SHELL_LOOK
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
/// [`STALE_S`]. Decided 2026-09-30 under the owner's standing direction
/// ("never interrupt an agent that is working"): as built — the others keep
/// the full wait, since an early stop there would first need a no-retype
/// rule for a `failed:` word in the live loop.
pub(super) fn ends_at_once(cause: &str) -> bool {
    cause == CAUSE_HOST
}

/// How long the shell a relaunch line was typed at may hold its terminal with
/// nothing registered before the relaunched agent is read as ended
/// ([`ShellBack`]). A shell hands the terminal to what it starts at once.
pub(super) const ENDED_AT_ONCE: Duration = Duration::from_secs(5);

/// How often [`await_held`] asks whether the shell has the tab back: a `ps`
/// each time, so not at the roster's pace.
const SHELL_LOOK: Duration = Duration::from_secs(1);

/// Tracks whether the relaunched agent ended as it started: the shell the line
/// was typed at has held its terminal for [`ENDED_AT_ONCE`] in a row. Anything
/// else holding it restarts the count; an unreadable look leaves it unchanged.
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
            .is_none_or(|at| now.saturating_duration_since(at) >= shell_look());
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
                now.saturating_duration_since(since) >= ended_at_once()
            }
            Some(false) => {
                self.since = None;
                false
            }
            None => false,
        }
    }
}

/// How long the harness's hand waits for a relaunched agent to come up idle
/// before giving it back ([`first_idle`]); an opening box is the supervisor's
/// to answer once the step returns.
const FIRST_IDLE: Duration = Duration::from_secs(15);

/// Wait for the relaunched agent's first idle with the harness's `hand` on the
/// tab (renewed first; at most [`FIRST_IDLE`]): one pushed `await agent idle`,
/// or on a server without the verb, `record_idle` polled.
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

/// How much transcript past the restart's mark is read for the resumed
/// session's first turn; a few system rows and the continuation precede it.
const SINCE_BYTES: u64 = 4 << 20;

/// How long after the continuation the resumed session's first answer may still
/// confirm the model. Never waited for: the typing step reads nothing, and
/// later steps ([`super::upgrade_drive::confirmations`], first in every sweep
/// and host step) read until this passes. Blocking a visit on it would age
/// other exiting restarts toward [`STALE_S`], and the answer can take this long
/// (long first thought, retries, a usage limit).
pub(super) const MODEL_WAIT: Duration = Duration::from_secs(120);

/// The new process holds the conversation: once it settles, tell it to carry on
/// (the upgrade's words, or the relaunch's for an [`after_exit`] record,
/// [`resumed_prompt`]); `continued`, with the model left to a later step
/// ([`confirm`]). `key` is the conversation the record is filed under.
pub(super) fn carry_on(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    key: &str,
    new: &SessionFile,
    hand: Option<&mut Hand>,
) -> Report {
    carry_on_with_tab_probe(
        opts,
        r,
        st,
        c,
        key,
        new,
        MODEL_WAIT,
        |c, pid, tab| process_in_tab(c, pid, tab, None, None),
        hand,
    )
}

/// `tab_probe` is the seam for the race between the first ownership read and
/// the typed continuation; production uses the kernel PTY claim for both.
/// `confirm_within` bounds how long the first answer may confirm the model
/// ([`confirm`]): [`MODEL_WAIT`] in production. Never waited for.
///
/// A carry-on waiting past its bound (agent never idle, a box, the tab's owner
/// changed) is said once as `stuck:relaunched`
/// ([`super::upgrade_drive::note_stuck`]); nothing is typed and the wait stands.
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
    hand: Option<&mut Hand>,
) -> Report {
    let r = carry_on_once(opts, r, st, c, key, new, confirm_within, tab_probe, hand);
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
    mut hand: Option<&mut Hand>,
) -> Report {
    r.pid = new.pid;
    let tab = st.tab.clone();
    let tab = tab.as_str();
    let home = opts.home.clone();
    let mut conversation_changed = false;
    let mut tail_available = true;
    let settled = wait_until(Duration::from_secs(120), || {
        // The hand the caller holds outlasts this wait (120 s against a 60 s
        // lease): renewed through it.
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
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
    // A QUIET RESTART ([`super::upgrade_drive::St::quiet`], the default since
    // 2026-09-29) is carried on with NOTHING: the agent was idle, nothing was
    // asked of it, and it is resumed as it was — the tab's loop decides the
    // point as if nothing had moved (`done:resumed`, a last word that moves
    // nothing), so an agent that had finished, or asked its person something,
    // is never told to carry on. The model it runs now is its next answer's
    // to say, and no step waits for that answer: the outcome names the model
    // it ran before, never "unconfirmed", which read as a check that failed
    // on every default upgrade ([`quiet_outcome`]).
    if resumes_quietly(st) {
        st.phase = upgrade::Phase::Done;
        r.from.clone_from(&st.from);
        r.to = format!("{}({})", current.version, st.source);
        let r = said(r, "done:resumed");
        st.outcome = quiet_outcome(st, &current.version);
        st.done_at = now_s();
        ledger(opts, &r, &st.outcome);
        save(opts, key, st);
        return r;
    }
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
    // A conversation nobody but the harness asked anything is carried on with
    // nothing: a carry-on would start a task nobody asked for.
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
                // A person at the keyboard wins: held, the continuation stays
                // owed and the next idle point types it.
                if super::upgrade_drive::held(c, tab, opts.human_grace_s) {
                    return said(r, "wait:held");
                }
                // UNDER THE HARNESS'S HAND: the caller's, else one for the
                // typing alone; a tab another driver holds is a wait, the
                // continuation still owed.
                let Some(typed) = under_hand(c, tab, hand, |c| type_continuation(c, tab, &text))
                else {
                    return said(r, "wait:held");
                };
                typed
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
    // Typed, and saved before anything else is read: a step dying from here
    // leaves the restart done, never in flight (which would tell the agent
    // twice). Only the model after is owed, a later step's read ([`confirm`]);
    // this step says `continued` (`upgrade_drive::typed`).
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

/// Type the continuation once, fenced on a fresh read with an empty composer
/// ([`typing_fence`], [`turn_fenced`]): a moved screen or typing through the
/// yield is re-read up to [`CONTINUE_FENCE_TRIES`] times; a draft, box or
/// refusal types nothing. A screen that never holds still is typed unfenced
/// after one more look at the composer, since a resumed session without its
/// continuation is the outcome to avoid.
fn type_continuation(c: &mut Client, tab: &str, text: &str) -> bool {
    for _ in 0..CONTINUE_FENCE_TRIES {
        let generation = match typing_fence(c, tab) {
            Ok(generation) => generation,
            // The composer moved between the fence's two reads: the same word
            // and retry as the host's `changed`.
            Err("changed") => {
                std::thread::sleep(RELAUNCH_FENCE_EVERY);
                continue;
            }
            Err(_) => return false,
        };
        match turn_fenced(c, tab, text, generation.as_deref()) {
            Ok(()) => return true,
            Err(Typed::Changed | Typed::Yielded) => std::thread::sleep(RELAUNCH_FENCE_EVERY),
            // Part of it typed and not sent: never typed again over it.
            Err(Typed::Cut(_) | Typed::Refused(_)) => return false,
        }
    }
    composer_clear(c, tab) && turn(c, tab, text).is_ok()
}

/// How many fenced tries the continuation gets ([`type_continuation`]).
pub(super) const CONTINUE_FENCE_TRIES: usize = 3;

/// The model after, for a restart whose continuation was typed: the resumed
/// session's first answer ([`resumed_model`]), or `done:model-unconfirmed` once
/// `confirm_by` passes (at once without a mark). Either ends the confirmation,
/// saved. `None` while the answer may still come. A changed model only changes
/// the words (a session without `--model` takes the current default).
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

/// The outcome line ([`upgrade::restart_outcome`], or
/// [`upgrade::restart_outcome_listed`] when the model rule chose a model), with
/// `after` as the model after.
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

/// The outcome line of a quiet restart ([`super::upgrade_drive::St::quiet`]):
/// the build, that it was idle and nothing was typed, the model it ran before,
/// and any model the rule asked for. No answer confirms the model after.
fn quiet_outcome(st: &St, to: &str) -> String {
    let before = recorded(&st.model_before);
    let asked = recorded(&st.model_list);
    let model = match (before, asked) {
        (Some(b), Some(m)) => format!("; it ran {b} before, and the relaunch asked for {m}"),
        (Some(b), None) => format!("; it ran {b} before the restart"),
        (None, Some(m)) => format!("; the relaunch asked for {m}"),
        (None, None) => String::new(),
    };
    format!("claude restarted on {to} · resumed while idle, nothing typed{model}")
}

/// A recorded field, `None` when it is empty (nothing was recorded).
pub(super) fn recorded(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// The model named by the first assistant turn build `version` wrote past the
/// restart's mark ([`upgrade::transcript_first_model`]). One read, never a wait.
/// `None`: no mark, no transcript, or no answer yet.
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

/// The [`St::cause`] of the upgrade's restart of a conversation nobody has asked
/// anything ([`super::upgrade::Step::Fresh`]): started afresh on the newer
/// build, nothing carried on, shown in `upgrade_status`'s rows.
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

/// The [`St::cause`] of a relaunch after aterm itself ended while the agent ran
/// and the next launch reopened its tab ([`after_host_ended`]).
pub(super) const CAUSE_HOST: &str = "host";

/// The [`St::cause`] of a restart onto a bucket's fallback model, `model:`
/// then the model ([`Restart::Model`]).
pub(super) const CAUSE_MODEL: &str = "model:";

/// The [`St::cause`] of a restart back from a bucket's fallback at its
/// reset, `model-back:` then the model, empty for the launch's own
/// ([`Restart::ModelBack`]).
pub(super) const CAUSE_MODEL_BACK: &str = "model-back:";

/// The [`St::cause`] of a restart under another of the owner's accounts,
/// `account:` then the LABEL ([`Restart::Account`]) — the label and nothing
/// else of the account: the login directory is on the line, never in a
/// cause, a continuation or a ledger's step. WHERE THE DIRECTORY IS ON
/// RECORD: the record's own line ([`St::line`], [`St::login`]: what a carry
/// retypes), and the upgrade ledger's `detail` of the rows that quote the
/// typed line (`relaunched`, `exited`, `failed:relaunch:…`) — a path under
/// the owner's own state directory, never a credential, beside the
/// `cd '<cwd>'` that ledger already held. The supervisor's journal and its
/// approval ledger know the label only.
pub(super) const CAUSE_ACCOUNT: &str = "account:";

/// WHAT AN ACCOUNT MOVE ANSWERS WHERE A RESTART OF ANOTHER KIND WAS IN
/// FLIGHT for the conversation ([`restart_as`], [`carried_word`]): that
/// restart was carried on — its own line, under the login ITS process had —
/// and the move itself was NOT TRIED. A `wait:`, so the loop asks again and
/// nothing is latched against the account or the wall: by then the carried
/// restart has landed, and the move is made on the process it brought up.
pub const IN_FLIGHT_OTHER: &str = "wait:restart-in-flight";

/// The word [`restart_as`] answers after it carried the conversation's
/// in-flight restart (cause `cause`, the carry's own word `step`) instead of
/// making the restart `why`. An account move over a restart that is not that
/// very move's (another kind, or a move to another label) answers
/// [`IN_FLIGHT_OTHER`] for every word of the carry that is no failure —
/// `adopted` included, which says the conversation is held, never under
/// which login. The carry's own failure is said as it is (it is the tab's
/// word); every other restart, and a move's own record carried, answer the
/// carry's word unchanged. PURE.
pub(super) fn carried_word(why: &Restart, cause: &str, step: String) -> String {
    match why {
        Restart::Account { to, .. }
            if cause.strip_prefix(CAUSE_ACCOUNT) != Some(to.as_str())
                && !step.starts_with("failed:") =>
        {
            IN_FLIGHT_OTHER.to_string()
        }
        _ => step,
    }
}

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
/// A move to ANOTHER ACCOUNT carries the model as the memory banner's restart
/// does (what rides, else a hand choice) — and, where a fallback stands, IS
/// the way back: the origin, when nothing rides and nobody chose, with the
/// origin spent. The fallback answered the OLD account's bucket; the new
/// account's is not at its limit, and the supervisor lets the switch go with
/// the move (`TurnEndState::restarted`), so nothing would ever return it.
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
        Restart::Account { to, .. } => ModelRestart {
            model: riding.or(hand).or(origin),
            cause: format!("{CAUSE_ACCOUNT}{to}"),
            launch_model: launched,
            fallback_from: None,
        },
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

/// The fallback origin on `st` ([`St::fallback_from`]), or for an older
/// fallback record, the launch model it kept. `None`: no fallback stands.
pub(super) fn fallback_origin(st: &St) -> Option<String> {
    st.fallback_from.clone().or_else(|| {
        st.cause
            .starts_with(CAUSE_MODEL)
            .then(|| st.launch_model.clone())
    })
}

/// The fallback origin a restart for another reason records: the standing
/// `origin`, or when the line asks for a `hand` choice (nothing `riding`),
/// that model, which the way back would otherwise undo.
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

/// Whether a record of `cause` is a relaunch's (its continuation says why,
/// [`resumed_prompt`]) rather than the upgrade's. [`CAUSE_HOST`] counts.
pub(super) fn relaunch_cause(cause: &str) -> bool {
    cause == CAUSE_EXIT
        || cause == CAUSE_MEMORY
        || cause == CAUSE_STALL
        || cause == CAUSE_HOST
        || cause.starts_with(CAUSE_MODEL)
        || cause.starts_with(CAUSE_MODEL_BACK)
        || cause.starts_with(CAUSE_ACCOUNT)
}

/// Whether a record of `cause` was filed by a restart that ENDED ITS AGENT
/// ITSELF — the upgrade's SIGTERM (cause empty, or [`CAUSE_UPGRADE_FRESH`])
/// or the restart in place's ([`restart_here`]: [`CAUSE_MEMORY`],
/// [`CAUSE_MODEL`], [`CAUSE_MODEL_BACK`], [`CAUSE_ACCOUNT`]) — rather than by a relaunch on
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
        || cause.starts_with(CAUSE_ACCOUNT)
}

/// The continuation typed to a relaunched agent once it holds its conversation:
/// why it was relaunched (`cause`), then carry on and decide for itself what it
/// was waiting on the user for ([`upgrade::CARRY_ON`]).
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
            "Restarted: {agent}'s memory was critical (its own warning, or the Mac short of \
             memory with it the largest past aterm's ceiling), so this session was ended and \
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
    } else if let Some(label) = cause.strip_prefix(CAUSE_ACCOUNT) {
        // The LABEL and nothing else, and nothing about limits: the agent
        // cannot know them, and the move may have been made ahead of one or
        // on the owner's word.
        format!(
            "Relaunched under another Claude account ({}): this session was ended and restarted",
            one_line(label)
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
/// and the new process is carried on first ([`carry_in_flight`]) — and an
/// account move asked over one of another kind is then not tried
/// ([`IN_FLIGHT_OTHER`]).
///
/// One word per step: `adopted` (`relaunched` without a loop),
/// `would-restart[:model=<m>]` in a dry run, `wait:<why>`, `busy:<why>`,
/// `refused:<what>` or `failed:<why>` (the loop escalates).
#[must_use]
pub fn restart_here(opts: &Opts, why: &Restart) -> Report {
    restart_from(opts, why, snapshot(opts))
}

/// [`restart_here`] by a host that keeps the owner's ROSTER of Claude Code
/// accounts: the one difference is [`Restart::Account`], whose label the
/// roster turns into the login the relaunch line carries
/// ([`accounts::login_env`]: the row's directory VERBATIM, or the empty
/// value for the row with none — the default login named, so the shell's own
/// variable cannot decide). The directory is the roster's and nobody else's:
/// the supervisor's policy, its journal and its ledger know the label only.
/// A label the roster no longer lists is `refused:account-not-listed` —
/// before any lock is taken or any process looked at — and so is every
/// account move asked through [`restart_here`], which has no roster. Every
/// other restart re-applies exactly the login the agent's own process had
/// ([`Snapshot::login`]), as [`restart_here`] does.
#[must_use]
pub fn restart_under(opts: &Opts, why: &Restart, roster: &accounts::Roster) -> Report {
    restart_as(opts, why, roster, snapshot(opts), &Live)
}

/// WHETHER THE PROCESS `snap` WAS READ FROM RUNS UNDER THE ACCOUNT the
/// roster labels `label`: its own exec environment names that row's login
/// ([`accounts::Roster::row_for_login`], byte for byte; unset and empty are
/// both the row with no directory). What a host checks AFTER an account
/// move's relaunch is adopted, on a snapshot of the NEW process — `adopted`
/// says the new process holds the conversation, never which login it runs
/// under, and a restart of another kind left in flight is carried first and
/// answers `adopted` for ITS line (the feasibility review, findings 4 and
/// 7). Pure.
#[must_use]
pub fn runs_under(snap: &Snapshot, roster: &accounts::Roster, label: &str) -> bool {
    roster
        .row_for_login(snap.login.as_deref())
        .is_some_and(|row| row.label == label)
}

/// THE LOGIN A RELAUNCH WITH NO PROCESS TO READ CARRIES (the accounts
/// design, §6: "a relaunch with no process to read (a cold restore) carries
/// `active`"): the login of the machine's ACTIVE account, as
/// [`account_login`] gives it — the empty value for the row with no
/// directory, the default login NAMED. Asked by a host for an agent whose
/// tab aterm's own end took ([`after_host_ended`]): the layout that brought
/// the tab back carries no environment, so nothing says which login the
/// agent ran under, and without this it came back on whatever its reopened
/// shell has. PURE: the roster, the state and the enrolled set are the
/// caller's reads.
///
/// `None` — NOTHING IS ADDED, the relaunch as it was before accounts existed
/// — unless ALL of these hold:
///
/// * `default_home`: the leaf's Claude home is the default one (the
///   roster's judgments are about no other);
/// * the roster lists TWO OR MORE accounts enrolled on this machine (with
///   one there is nothing to choose between, and no line changes);
/// * `active` names one of them, PROVEN (it has served a turn, or the owner
///   pinned it) and not known signed out: an account nobody has been served
///   by is its canary's alone to try, never a restored tab's.
#[must_use]
pub fn cold_login(
    roster: &accounts::Roster,
    state: &accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    default_home: bool,
) -> Option<String> {
    if !default_home {
        return None;
    }
    let here = |row: &accounts::Row| row.dir.is_none() || enrolled.contains(&row.label);
    if roster.rows.iter().filter(|row| here(row)).count() < 2 {
        return None;
    }
    let active = state.active.as_deref().filter(|_| state.proven)?;
    let row = roster.row(active).filter(|row| here(row))?;
    if state.account(active).auth == accounts::Auth::SignedOut {
        return None;
    }
    Some(accounts::login_env(row).1)
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

/// [`restart_from`] with process facts from `k` ([`Kernel`]), the seam a test
/// scripts the agent's job and terminal through.
pub(super) fn restart_with(
    opts: &Opts,
    why: &Restart,
    snap: Result<Snapshot, &'static str>,
    k: &dyn Kernel,
) -> Report {
    restart_as(opts, why, &accounts::Roster::default(), snap, k)
}

/// The login an account move's relaunch line carries: the value
/// [`accounts::login_env`] gives the roster's row for the label. `None`: the
/// roster lists no such label (it was edited since the advice, or the host
/// has none).
pub(super) fn account_login(roster: &accounts::Roster, label: &str) -> Option<String> {
    roster.row(label).map(|row| accounts::login_env(row).1)
}

/// [`restart_with`] under `roster` ([`restart_under`]).
pub(super) fn restart_as(
    opts: &Opts,
    why: &Restart,
    roster: &accounts::Roster,
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
    // AN ACCOUNT MOVE'S TARGET, from the roster and nowhere else — settled
    // before the lock and before the agent is looked at: a label nobody
    // lists is refused for good, whatever else is true of the tab.
    let target = match why {
        Restart::Account { to, .. } => match account_login(roster, to) {
            Some(login) => Some(login),
            None => return said(r, "refused:account-not-listed"),
        },
        _ => None,
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
    // Claude's own record of this process names the conversation, the build
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
        // Carried on ITS line. Where the restart asked for is an account
        // move and the one in flight is not that move, the move was not
        // tried, and says so ([`IN_FLIGHT_OTHER`]).
        let cause = st.cause.clone();
        let carried = carry_in_flight(opts, r, st, &session, &tab);
        let step = carried_word(why, &cause, carried.step.clone());
        return said(carried, step);
    }
    if upgrade::one_shot(&snap.argv) {
        return said(r, "refused:one-shot");
    }
    let t = table();
    if is_our_ancestor(snap.pid, &t) {
        return said(r, "refused:self");
    }
    // The model the line asks for and the fallback origin it records
    // ([`model_restart`]); a `/model` made since launch is read so the launch's
    // `--model` does not undo it.
    let (riding, hand) = restart_models(
        opts,
        Some(&sf),
        &snap.start,
        &session,
        &snap.argv,
        matches!(why, Restart::Memory | Restart::Account { .. }),
    );
    let model_list = riding.clone().unwrap_or_default();
    let launched = launch_kept(opts, &session, &snap.argv).unwrap_or_default();
    let ModelRestart {
        model,
        cause,
        launch_model,
        fallback_from,
    } = model_restart(why, prior.as_ref(), launched, riding, hand);
    let mode = resume_mode(opts, &session, tab_mode(opts, &tab));
    // The login the line carries: an account move's target, else exactly
    // what this process had (unset: nothing is added).
    let login = target.or_else(|| snap.login.clone());
    let launch = Launch {
        session: &session,
        resume: transcript_exists(&opts.home, &session) != Some(false),
        cwd: &sf.cwd,
        agent: snap.pid,
        argv: &snap.argv,
        exe: &snap.program,
        model: model.as_deref(),
        mode: mode.as_deref(),
        login: login.as_deref(),
    };
    let mut st = St {
        from: sf.version.clone(),
        to: sf.version.clone(),
        source: SAME_PROGRAM.to_string(),
        cause,
        salt: now_s(),
        launch_model,
        model_list,
        fallback_from,
        login: login.clone(),
        ..St::default()
    };
    // On its own program: the owner's hold and how long the session has been
    // behind are the upgrade's still ([`St::kept_by_restart`], round eight,
    // #24).
    if let Some(prior) = prior.as_ref() {
        prior.kept_by_restart(&tab, &mut st);
    }
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
    // The harness's hand from the last look to the relaunched agent's first
    // idle ([`super::upgrade_drive::Hand`]), given back on every way out.
    let Some(mut hand) = Hand::take(&mut c, &tab) else {
        return said(r, "wait:held");
    };
    let aborted: Option<Report> = 'signal: {
        // The last look before the irreversible act.
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

/// What a relaunch needs, read while the agent runs (argv, parent and start
/// time go with the process).
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
    /// A Codex's run ([`CodexRun`]); `None` for Claude Code.
    pub codex: Option<CodexRun>,
    /// The dialect of `shell` from its executable ([`shell_dialect`]); `None`
    /// when unreadable or unsupported (nushell, tcsh…), which [`plan`] waits on
    /// forever. Read here so [`resumes_on_exit`] stays pure.
    pub dialect: Option<Dialect>,
    /// THE LOGIN THE PROCESS RUNS UNDER, as its own exec environment names
    /// it: the RAW value of [`accounts::LOGIN_ENV`]. Three states, kept
    /// apart because they are three relaunch lines: `None`, the variable is
    /// unset (the process took its shell's login, and so does its relaunch:
    /// nothing is added); `Some("")`, set and empty (the default login,
    /// NAMED — what a move back to the default account writes, so the
    /// shell's own variable cannot decide); `Some(dir)`, that login's
    /// directory, verbatim. Every relaunch of the agent re-applies exactly
    /// this ([`Launch::login`]) — in place, on exit, after aterm itself
    /// ended — unless the restart is itself an account move. A Codex's is
    /// always `None`: the variable is Claude Code's.
    pub login: Option<String>,
}

/// What a Codex TUI's relaunch on exit needs beside the [`Snapshot`] (whose
/// `session` is its thread, when nameable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexRun {
    /// The `$CODEX_HOME` it runs with (canonical).
    pub home: PathBuf,
    /// It holds its thread's writer lock itself (`--no-daemon`, or kept out of
    /// the daemon); `false`: a daemon client, whose daemon holds the thread.
    pub embedded: bool,
    /// When it started, unix milliseconds (to the second): a daemon-mode
    /// client's thread is one its daemon began after this.
    pub started_ms: u64,
    /// This read could not tell a daemon client's thread from another's
    /// (`thread-ambiguous`); no earlier name is kept over it. Never carried
    /// across a restart.
    pub ambiguous: bool,
    /// THE SHELL-INTEGRATION COMMAND BLOCK IT RUNS IN (its `id`, read while it
    /// runs: the tab's last block, executing), or `None` where none could be
    /// read. Its shell's word on its exit is THAT block's
    /// (`upgrade_codex_drive::look_at_exit`): read later, the newest finished
    /// block may be another command's (round three of the joint audit of
    /// 2026-10-01). Never carried across a restart.
    pub block: Option<u64>,
}

/// Whether an exit of `snap`'s agent now would be relaunched on its own
/// conversation ([`after_exit`]), as far as a live read can tell: a held
/// conversation ([`upgrade::is_session_id`]), not a one-shot launch, a
/// supported shell dialect, and a line [`line_for`] can make from the argv.
/// Pure: the dialect is in the snapshot ([`Snapshot::dialect`]).
///
/// A frozen Claude Code's remedy says "aterm relaunches it" instead of naming a
/// `claude --resume <id>` command, so this must be true when it says so;
/// `false` sends the remedy back to the command
/// ([`super::upgrade_drive::resume_command_kept`]).
///
/// A Codex snapshot answers `false`: the Codex lane
/// ([`super::upgrade_drive::codex_after_exit`]) resumes the thread only if it
/// still has a rollout at exit, which cannot be promised in advance.
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
    // The line without its `cd`, which the plan adds only when the shell's
    // directory differs at exit. A control character in the directory refuses
    // the line either way.
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
            // With the login its relaunch must carry: a line that cannot
            // hold it is refused then, so nothing is promised now.
            &login_env(snap.login.as_deref()),
        )
        .is_ok()
}

/// The program a Claude Code process runs and its args: `(program, argv with
/// program first)`.
///
/// A `#!` script shows the interpreter as the kernel's image and the script as
/// `argv[1]` (aterm's managed twin, an npm install), so an absolute `argv[1]`
/// named `claude` is the program. Otherwise the image is, when it is Claude's:
/// named `claude`, under the native installer's versions directory, or
/// `registered`. A wrapper running Claude as a child is not relaunched.
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

/// Read the [`Snapshot`] of the Claude Code in tab `opts.only_sid` on
/// `opts.sock`: the tab's foreground job must be a job-control shell's job on a
/// terminal only this aterm tab claims, and a Claude program.
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
        // The shell's kernel argv only; an unreadable one only costs the
        // remedy its relaunch wording ([`resumes_on_exit`]).
        dialect: shell_dialect(shell, &[]),
        // READ off the process's own exec environment, the same read that
        // gave its argv: what the vendor's runtime was started with, which
        // is what chose its keychain item (the accounts design, §1).
        login: args.env_var(accounts::LOGIN_ENV).map(str::to_owned),
    })
}

/// Where the tab's foreground job stands against a [`Snapshot`]: the process
/// group the tab's program is published for now (`fg`, `program_pgid`).
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

/// Follow the snapshot's agent while it runs: its conversation, build and
/// directory as Claude's record of that process names them now (`/clear` and
/// `/resume` move the conversation). One small file read. Returns whether
/// anything moved.
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
/// session onto another build. NOR DOES THE OWNER'S WORD (`hold`, the hold
/// the conversation's upgrade record carries for the tab,
/// [`St::owners_hold`]; round eight, #24): a build the owner skipped, or any
/// while their `Not today` stands, is no relaunch target at `now` — a crash
/// moved the session straight onto the build the owner had refused. A build
/// newer than a skipped one is ("until a NEWER build is the target").
pub(super) fn relaunch_target(
    upgrade: bool,
    running: Option<&Version>,
    candidates: &[upgrade::Candidate],
    hold: Option<&upgrade::Request>,
    program: &Path,
    version: Option<&str>,
    now: u64,
) -> (PathBuf, String, String) {
    let target = running
        .filter(|_| upgrade)
        .and_then(|v| upgrade::choose_target(v, candidates))
        .filter(|t| !hold.is_some_and(|word| word.holds_build(&t.version.to_string(), now)));
    match target {
        Some(t) => (t.exe, t.version.to_string(), t.source.as_str().to_string()),
        None => (
            program.to_path_buf(),
            version.unwrap_or("-").to_string(),
            SAME_PROGRAM.to_string(),
        ),
    }
}

// ------------------------------------------------ what the exit left behind

/// What the exit left of Claude's record of the agent (`sessions/<pid>.json`,
/// same pid and kernel start): read once as the exit is seen ([`exit_record`])
/// and kept for every relaunch attempt ([`after_exit`]). A crash leaves the
/// record and a graceful exit removes it, but any Claude Code starting later
/// removes dead processes' records too, so a read after the back-off cannot
/// tell them apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitRecord {
    /// Nothing was read at the exit: the process still ran through the look
    /// ([`EXIT_GONE`]), nothing was readable, or the look was cut short. The
    /// attempt reads the record itself.
    Unread,
    /// The record survived the exit — a crash — as it was read then: it
    /// names the conversation as the agent last held it.
    Survived(SessionFile),
    /// The exit removed it: graceful, someone's decision (or no conversation
    /// was ever registered). For a Codex: its shell says it exited cleanly or
    /// by someone's signal.
    Removed,
    /// A Codex crashed, by its shell's exit status ([`CodexRun`]); the thread
    /// is the snapshot's.
    Crashed,
}

impl ExitRecord {
    /// THE EXIT WAS A CRASH, read at the exit: a Claude Code's record
    /// survived it, or a Codex's shell says so. The ONE rule both what the
    /// Codex lane relaunches on (`upgrade_codex_drive`'s `codex_after_exit`)
    /// and what the window's host owes through a back-off read (its
    /// `Kept::owed`, which a seamless update carries): spelled as a surviving
    /// record alone there, a crashed Codex was owed nothing, and a park in its
    /// back-off lost its relaunch (round two of the joint audit of
    /// 2026-10-01). An exit nothing read ([`Self::Unread`]) is no crash yet.
    #[must_use]
    pub fn crashed(&self) -> bool {
        matches!(self, Self::Survived(_) | Self::Crashed)
    }

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

/// One look at an agent that left its tab ([`look_at_exit`]): whether the same
/// process (pid and kernel start) still runs, and Claude's record of it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitLook {
    /// The snapshot's process still runs.
    pub running: bool,
    /// Its record, when one of its own start is on disk (Claude Code's).
    pub record: Option<SessionFile>,
    /// A Codex's look: `record` is never read, `crashed` decides.
    pub codex: bool,
    /// A Codex's exit per its shell: `Some(true)` a crash, `Some(false)` its own
    /// exit or someone's signal, `None` not told yet
    /// ([`super::upgrade_codex::crashed_status`]).
    pub crashed: Option<bool>,
}

/// How long after the first look an exit may still be removing its own record:
/// a graceful `/exit` removes it in its last moments, so a record read at that
/// instant could call a clean exit a crash. Kept short so another Claude
/// Code's start has little time to remove a crash's record. How soon the exit
/// is seen is the caller's (`aterm-gui`'s `harness_host`, `HOLDS_LOOK_MAX`).
pub const EXIT_SETTLE: Duration = Duration::from_millis(250);

/// How often [`exit_record`] looks.
pub const EXIT_LOOK: Duration = Duration::from_millis(25);

/// How long a process still running after its exit was seen (stopped, or
/// unreaped) is looked at before the read is left to the attempt
/// ([`ExitRecord::Unread`]).
pub const EXIT_GONE: Duration = Duration::from_secs(2);

/// [`ExitLook`] of `snap`'s agent, live: `running` is read first, since a
/// graceful exit removes its record before the process ends.
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

/// [`ExitLook`] of a Codex `snap` ([`CodexRun`]), live: its shell's exit status
/// for the command, read over `opts.sock`.
#[must_use]
pub fn look_at_codex_exit(opts: &Opts, snap: &Snapshot) -> ExitLook {
    super::upgrade_drive::codex_look_at_exit(opts, snap)
}

/// Read what the exit left, once, as it is seen, independent of the back-off.
/// `look` is one [`ExitLook`] (`None`: unreadable); `wait` waits one
/// [`EXIT_LOOK`] and says whether it waited it all (`false`: cut short).
///
/// Process gone and no record decides at once. A record still present decides
/// only after [`EXIT_SETTLE`] from the first look; a process still running
/// waits up to [`EXIT_GONE`]. A Codex look decides when its shell's word comes
/// ([`ExitLook::crashed`]), waiting up to [`EXIT_GONE`].
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

/// Relaunch on exit: `snap`'s agent left its tab and [`on_exit`] found nobody
/// owns the exit. Relaunch it on its conversation in the same tab, on the
/// newest allowed build while `upgrade` allows it, else the program it ran
/// ([`relaunch_target`]), then tell it to carry on. The step's word says what
/// happened ([`outcome`]). `left` is what the exit left, read as it was seen
/// ([`exit_record`]); crash versus graceful is decided on that. `stalled`: the
/// agent had stopped reading input and the exit was the stall remedy's
/// signal, so it is relaunched even with its record removed, the continuation
/// saying why.
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
    /// aterm itself ended while it ran: the hangup the agent may have answered
    /// by removing its record is nobody's decision about the session.
    HostEnded,
}

/// The agent of a tab a crash of aterm took: aterm ended while `snap`'s agent
/// ran and the next launch reopened its tab (new shell `snap.shell` in
/// `snap.tab`; the rest is what the layout carried). Relaunched as
/// [`after_exit`] would, except the graceful-exit rule does not apply, and a
/// relaunch in flight for the crashed run's tab is closed, not waited on. A
/// Codex snapshot is relaunched by the Codex lane on its thread.
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
    // A Codex is relaunched by the Codex lane, on the thread it held.
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
    // One actor on the upgrade state at a time, the upgrade and a hand-run
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
    // Claude's record as read at the exit, when it survived (it names the
    // conversation the agent last held), else the snapshot's. Only an exit
    // nothing could read then is read now.
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
        // No record names a conversation: none was registered, or the agent
        // removed its record as it ended by itself.
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
    // The harness's own restart ended this agent: its record names the process
    // that left, in this tab. The exit is carried or said, never the graceful
    // exit its SIGTERM made. Not for a tab aterm's own end took
    // ([`ExitCause::HostEnded`]), where a too-old record is closed below and a
    // fresh relaunch made.
    if cause != ExitCause::HostEnded
        && let Some(st) = load(opts, &session)
            .filter(|st| ended_by_restart(st, upgrade::Agent::Claude, &snap.tab, snap.pid, now_s()))
    {
        r.to = format!("{}({})", st.to, st.source);
        return carry_own(opts, r, st, &session, &snap.tab);
    }
    // A relaunch already in flight is carried on, not typed twice. One too old
    // to act on is closed and this step makes a fresh one: the host has just
    // decided nobody is at the tab. One of THIS exit, closed, says what an
    // earlier attempt of it read ([`St::relaunched_on_exit_of`]).
    let mut relaunched_before = false;
    if let Some(mut st) = load(opts, &session).filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != snap.tab && cause != ExitCause::HostEnded {
            return said(r, "wait:conversation-in-other-tab");
        }
        // A record of another process than the one that left: that relaunch
        // landed or was overtaken; closed, and this exit relaunched afresh.
        // So is one that LANDED on a process gone already
        // ([`St::relaunched_gone`], the Codex lane's rule too).
        let overtaken = if cause == ExitCause::HostEnded && st.tab != snap.tab {
            Some((
                "host-ended",
                "aterm ended while this relaunch was in flight; its tab was reopened".to_string(),
            ))
        } else {
            (st.pid != snap.pid || st.relaunched_gone(alive)).then(|| match st.phase {
                // No relaunch line was typed yet: the process that left is
                // one a person started by hand, never aterm's.
                upgrade::Phase::Exiting { .. } => (
                    "resumed-elsewhere",
                    "the conversation was resumed in this tab outside aterm's relaunch, and that \
                     process has exited"
                        .to_string(),
                ),
                _ => (
                    "exited-before-continuing",
                    "the relaunched agent exited before its continuation was typed".to_string(),
                ),
            })
        };
        let overtook = overtaken.is_some();
        if let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s())) {
            relaunched_before = st.relaunched_on_exit_of(snap.pid);
            if !opts.dry_run {
                if overtook {
                    // Never a day-long stall with `claude --resume` for a
                    // relaunch that landed (the review of 2026-09-27: a
                    // graceful `/exit` of the relaunched agent read so).
                    st.close_overtaken(why, now_s());
                } else {
                    st.seen_gone(now_s());
                    st.stop(why, now_s());
                }
                ledger(opts, &said(r.clone(), format!("failed:{why}")), &detail);
                save(opts, &session, &st);
            }
        } else {
            return carry_in_flight(opts, r, st, &session, &snap.tab);
        }
    }
    // A graceful exit was someone's decision (a person's `/exit` or ctrl-d, an
    // orchestrator's `/exit` over the socket, a `kill`): Claude removes its
    // record as it ends, which a crash cannot. Only an exit that left the
    // record (read as it was seen) is relaunched. The harness's own restart is
    // handled above ([`ended_by_restart`]); a stall remedy is nobody's
    // decision. An exit nothing read as it was seen that an earlier attempt
    // read as a crash and relaunched stays one: the relaunched agent's start
    // swept the record that attempt read.
    let read_before = relaunched_before && matches!(left, ExitRecord::Unread);
    if survivor.is_none() && cause == ExitCause::Exit && !read_before {
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
    // The conversation's upgrade record: the owner's hold on the move, kept
    // by the relaunch's target and by its record (round eight, #24).
    let prior = load(opts, &session);
    let hold = prior.as_ref().and_then(|st| st.owners_hold(&snap.tab));
    let (exe, to, source) = relaunch_target(
        upgrade,
        running.as_ref(),
        &candidates,
        hold.as_ref(),
        &snap.program,
        version.as_deref(),
        now_s(),
    );
    r.to = format!("{to}({source})");
    // A conversation with no message cannot be resumed: start afresh with the
    // same flags. Unreadable transcripts are resumed (Claude says if it cannot).
    let resume = transcript_exists(&opts.home, &session) != Some(false);
    // The model the rule moves the conversation to rides the relaunch when due
    // (read off the crash's record), else a `/model` made since launch.
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
        // The login the agent that left ran under, as the snapshot read it
        // while it ran: its relaunch comes back on the same account.
        login: snap.login.as_deref(),
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
        launch_model: launch_kept(opts, &session, &snap.argv).unwrap_or_default(),
        model_list: riding.clone().unwrap_or_default(),
        fallback_from: exit_fallback(opts, &session, riding.as_deref(), hand.as_deref()),
        login: snap.login.clone(),
        ..St::default()
    };
    // Kept on its own program, the session is as far behind as before, and
    // the owner's hold still stands ([`St::kept_by_restart`]).
    if st.source == SAME_PROGRAM
        && let Some(prior) = prior.as_ref()
    {
        prior.kept_by_restart(&snap.tab, &mut st);
    }
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

/// How long after its agent was seen gone ([`St::exited_at`]) a stopped restart
/// still counts as the one that ended an agent leaving its tab: twice
/// [`STALE_S`], the longest a carry lives, so a reused pid is never read as it.
const STOPPED_OWN_S: u64 = 2 * STALE_S;

/// Whether `st` is the record of a restart of the harness's own that ENDED
/// `agent` process `pid` in tab `tab` — THE ONE IDENTITY BOTH LANES READ A
/// LEAVING AGENT BY (round four of the harness audit, 2026-10-03): that agent
/// and tab, a cause that ends its agent ([`ends_its_agent`]: the upgrade's
/// SIGTERM or `/exit`, a restart in place) and THAT PROCESS ([`St::pid`]: the
/// Claude Code signalled, the Codex TUI its `/exit` was typed into). Never a
/// relaunch on exit's own record ([`CAUSE_EXIT`], [`CAUSE_FRESH`],
/// [`CAUSE_STALL`], [`CAUSE_HOST`]), whose agent was gone before it was made,
/// and never another process. The Codex lane files both kinds in one per-tab
/// slot (`codex-<tab>`); read by its tab alone, the exit of a Codex a person
/// started where a relaunch on exit waited at the prompt (`wait:typing`, a
/// hold), then quit, was carried as aterm's restart — over their word, a hold
/// and `[harness] relaunch = false` — and the crashed TUI's line typed back.
fn restart_of(st: &St, agent: upgrade::Agent, tab: &str, pid: u32) -> bool {
    st.agent == agent && st.tab == tab && st.pid == pid && ends_its_agent(&st.cause)
}

/// Whether `st` records a harness restart that ended agent `pid` (of `agent`'s
/// lane) that left tab `tab` at `now` ([`restart_of`]) and is still that
/// exit's: a Claude Code's in flight, or stopped at most [`STOPPED_OWN_S`]
/// after the agent was seen gone (a stop before any exit, a refused signal,
/// left the agent running; its later exit is someone's); a Codex's while its
/// old TUI is not yet replaced ([`codex_exit_is_restarts`]).
pub(super) fn ended_by_restart(
    st: &St,
    agent: upgrade::Agent,
    tab: &str,
    pid: u32,
    now: u64,
) -> bool {
    restart_of(st, agent, tab, pid)
        && match agent {
            upgrade::Agent::Claude => {
                st.in_flight()
                    || (matches!(st.phase, upgrade::Phase::Failed(_))
                        && st.exited_at != 0
                        && now.saturating_sub(st.exited_at) <= STOPPED_OWN_S)
            }
            upgrade::Agent::Codex => codex_exit_is_restarts(st),
        }
}

/// Whether the Codex TUI a restart record `st` names, having left its tab,
/// was ended by it ([`ended_by_restart`]): its `/exit` typed and the old TUI
/// not yet replaced (exiting, or relaunched with no new TUI adopted or found
/// gone). Once the relaunched TUI is adopted ([`St::resumed_pid`]) its exit
/// is its own. A TUI the restart found ([`St::relaunched_pid`]) but never
/// adopted counts once it is gone; while alive it is not what left.
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
/// the conversation it is filed under: the one that ended process `pid`
/// ([`ended_by_restart`]). No `pid` (the host read nothing of the agent that
/// left) names no process, and finds none.
fn restart_record(opts: &Opts, codex: bool, pid: Option<u32>) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let pid = pid?;
    let agent = if codex {
        upgrade::Agent::Codex
    } else {
        upgrade::Agent::Claude
    };
    let now = now_s();
    record_where(opts, |st| ended_by_restart(st, agent, tab, pid, now))
}

/// The first record `ours` is true of, with the conversation it is filed
/// under.
fn record_where(opts: &Opts, ours: impl Fn(&St) -> bool) -> Option<(String, St)> {
    let dir = std::fs::read_dir(state_dir(opts)).ok()?;
    dir.flatten().find_map(|e| {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            return None;
        }
        let session = path.file_stem()?.to_string_lossy().into_owned();
        let st = load(opts, &session)?;
        ours(&st).then_some((session, st))
    })
}

/// THE MOVE LANDED BEFORE ITS CARRY (the review of a78c61648, 2026-09-30):
/// the record of the harness's own restart of the Claude Code `pid` that
/// left tab `opts.only_sid` — DONE, its agent seen gone at most
/// [`STOPPED_OWN_S`] ago, as a stopped one is still its exit's
/// ([`ended_by_restart`]) — whose conversation a live process other than
/// `pid` holds NOW (a fresh read of Claude's session files). The host read
/// the exit as that restart's while it was in flight, and another actor
/// finished it before the carry's next try (a hand-run `aterm harness
/// upgrade` carries its relaunch on itself, holding the lock the carry
/// waits on): the exit was the finished move's own, and nothing is owed.
/// Read-only.
fn landed(opts: &Opts, pid: u32) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let now = now_s();
    let (session, st) = record_where(opts, |st| {
        restart_of(st, upgrade::Agent::Claude, tab, pid)
            && st.phase == upgrade::Phase::Done
            && st.exited_at != 0
            && now.saturating_sub(st.exited_at) <= STOPPED_OWN_S
    })?;
    let others: Vec<SessionFile> = session_files(&opts.home)?
        .into_iter()
        .filter(|sf| sf.pid != pid)
        .collect();
    conversation_live(&others, &session).then_some((session, st))
}

/// THE CODEX RESTART LANDED AND ITS TUI IS GONE (the review of 6c7df720f,
/// 2026-10-02): the record of the restart that ended tab `opts.only_sid`'s
/// Codex TUI `pid` ([`restart_of`]), relaunched, whose TUI it found or adopted
/// is gone already ([`St::relaunched_gone`]). That TUI's exit is its own, so
/// [`restarted`] finds the record no longer ([`codex_exit_is_restarts`]); a
/// carry of the old TUI's exit made after it said
/// `refused:no-restart-in-flight`, which the host says as a restart that
/// cannot land. Read-only.
fn codex_landed_gone(opts: &Opts, pid: u32) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    record_where(opts, |st| {
        restart_of(st, upgrade::Agent::Codex, tab, pid)
            && matches!(st.phase, upgrade::Phase::Relaunched { .. })
            && st.relaunched_gone(alive)
    })
}

/// Whether the restart `st` (filed under `session`) was ADOPTED and its
/// adopted process ([`St::relaunched_pid`]) holds the conversation now.
fn adopted_holds(opts: &Opts, st: &St, session: &str) -> bool {
    matches!(st.phase, upgrade::Phase::Relaunched { .. })
        && st.relaunched_pid != 0
        && continuation_session(&opts.home, st.relaunched_pid, session).is_some_and(|sf| {
            sf.pid == st.relaunched_pid && conversation_live(std::slice::from_ref(&sf), session)
        })
}

/// THE HARNESS'S OWN RESTART ENDED THE AGENT THAT LEFT tab `opts.only_sid`
/// (S0 and S3 of the in-flight review, 2026-09-27): the upgrade's, or the
/// restart in place's, whose step returned with the relaunch still to type
/// (`wait:held`, `wait:typing`, `wait:shell-prompt`, `wait:resume` …) and
/// whose agent then left the tab with no step watching it. `pid` is the
/// agent that left (the host's snapshot of it; `None`, nothing read, finds
/// nothing) — a Codex TUI if `codex` says so, else a Claude Code — and the
/// restart is the record that ended that very process, both lanes by one
/// rule ([`ended_by_restart`]). Read-only: the state files, and whether the
/// relaunched Codex TUI a record names still lives; no lock.
#[must_use]
pub fn restarted(opts: &Opts, codex: bool, pid: Option<u32>) -> bool {
    restart_record(opts, codex, pid).is_some()
}

/// THE ACCOUNT MOVE WHOSE RESTART SIGNALLED the Claude Code `pid` of tab
/// `opts.only_sid`, by its LABEL: the record [`restarted`] finds for that
/// process ([`ended_by_restart`]: in flight, or stopped a short while after
/// its agent was seen gone) where its cause is an account move's
/// ([`CAUSE_ACCOUNT`]). `None`: nothing of the harness's own signalled that
/// process, or what did was a restart of another kind. What a host that
/// asked for the move reads to tell a relaunch LEFT IN FLIGHT — its signal
/// sent, its line still to land under the target's login — from one that was
/// never begun, whether or not the signalled process has gone yet: the
/// step's word cannot say (`wait:held` is said before the signal and after
/// it). Read-only: the state files; no lock.
#[must_use]
pub fn account_restart(opts: &Opts, pid: u32) -> Option<String> {
    restart_record(opts, false, Some(pid))
        .and_then(|(_, st)| st.cause.strip_prefix(CAUSE_ACCOUNT).map(str::to_owned))
}

/// THE RESTART [`restarted`] FOUND, CARRIED ON for its agent's exit: one step
/// under the one lock every actor on the upgrade state takes. `adopted` (or
/// `done:fresh`) once the relaunch holds the tab; `wait:<why>` while a
/// person types at the returned prompt, a hold or a hand is on the tab, the
/// shell has not taken the terminal back, or the relaunch has not registered
/// — nothing is typed then, and the host tries again ([`CARRY_EVERY`]);
/// `busy:<why>` for another actor; `landed` for a Claude Code restart
/// another actor finished meanwhile ([`landed`]: nothing owed, nothing
/// said), and for a restart whose relaunched agent is gone already, its
/// record closed ([`carry_own`], [`codex_landed_gone`]); and
/// `refused:<why>` once it can never land ([`carry_own`]), or is
/// no longer to be found (`refused:no-restart-in-flight`) — the one case a
/// person is asked about, never the graceful exit the restart's own SIGTERM
/// made of it.
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
        // A move FINISHED since the host read the exit as its own: that exit
        // was the move's, and the owner is asked nothing about an agent
        // that is back. A Codex restart that landed on a TUI gone already
        // ([`codex_landed_gone`]) is closed as the Codex lane's relaunch on
        // exit closes it, and the owner is asked nothing about it either.
        let found = pid.and_then(|pid| {
            if codex {
                codex_landed_gone(opts, pid)
            } else {
                landed(opts, pid)
            }
        });
        let Some((session, mut st)) = found else {
            return said(r, "refused:no-restart-in-flight");
        };
        let r = Report {
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.to, st.source),
            ..r
        };
        if codex && !opts.dry_run {
            st.close_overtaken("exited-before-continuing", now_s());
            ledger(
                opts,
                &said(r.clone(), "failed:exited-before-continuing"),
                "the relaunched Codex exited before its continuation was typed",
            );
            save(opts, &session, &st);
        }
        return said(r, "landed");
    };
    // This carry follows the exit the host just saw by at most one
    // [`CARRY_EVERY`], so the stale-exit bound ([`STALE_S`]) counts from now,
    // never from a signal a slow shutdown outlived. A Codex lane keeps its own.
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
/// ([`ended_by_restart`]), carried from where it stopped — and FINAL where it
/// can no longer land: one already stopped says so again, one too old to act
/// on is stopped ([`St::stop`], with when its agent was seen gone) and one
/// that stops in the carry says so, each `refused:<why>` (S0 of the in-flight
/// review, 2026-09-27). One relaunched
/// and never registered is never relaunched afresh: its line was typed once,
/// and a second minutes later, at a prompt a person may be typing at, is
/// what [`STALE_S`] exists to prevent. A dry run says `would-refuse:<why>`
/// and writes nothing.
///
/// ONE ALREADY ADOPTED IS ANSWERED AT ONCE (the review of a78c61648,
/// 2026-09-30): the step that relaunched it found the new process, waited
/// for its first idle point and handed it to its loop; the record stays in
/// flight until that loop's idle point carries it on ([`resume`]). While its
/// adopted process holds the conversation ([`St::relaunched_pid`]) the carry
/// of the old agent's exit says `adopted` and writes nothing — before, it
/// went back through [`await_new`]: the harness's hand on the tab again and
/// the agent's first idle point waited for again, for a process already
/// found (and, past [`STALE_S`], the record stopped `no-resume` as a
/// relaunch that "never registered").
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
    if st.agent == upgrade::Agent::Claude && adopted_holds(opts, &st, session) {
        let mut r = said(r, "adopted");
        r.pid = st.relaunched_pid;
        return r;
    }
    // LANDED AND GONE (the review of 6c7df720f, 2026-10-02): the process
    // the relaunch adopted is gone already ([`St::relaunched_gone`]). No carry
    // types its line again, so carried on, the tab was held for the dead
    // process ([`await_new`], 90 s a try) until the record went stale and
    // was said `no-resume`, a relaunch that "never registered". Closed as
    // both lanes close one overtaken ([`St::close_overtaken`]: no exit
    // stamp), and answered `landed`: the
    // restart's relaunch landed, and what ended after it is the relaunched
    // agent's own exit — never the restart's, whose carry answered with an
    // end is said as one that cannot land ([`Relaunches::attempted`]).
    if st.agent == upgrade::Agent::Claude && st.relaunched_gone(alive) {
        if !opts.dry_run {
            st.close_overtaken("exited-before-continuing", now_s());
            ledger(
                opts,
                &said(r.clone(), "failed:exited-before-continuing"),
                "the relaunched agent exited before its continuation was typed",
            );
            save(opts, session, &st);
        }
        return said(r, "landed");
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
            ledger(opts, &said(r.clone(), format!("failed:{why}")), &detail);
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

/// The continuation owed to tab `opts.only_sid`: an adopted relaunch not yet
/// carried on ([`Opts::hand_back`]), or a typed continuation whose model is
/// still to be read ([`confirm`]). The host parks the tab's loop for it at its
/// next idle point regardless of `[harness] upgrade`. Read-only.
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

/// Carry on the relaunched agent at its loop's idle point, where the loop
/// parked for an [`owed`] continuation: find the new process again
/// ([`await_new`]) and tell it to carry on ([`carry_on`]), or read a typed
/// continuation's model ([`confirm`], `wait:model` while it may come). Under
/// the upgrade state lock.
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
    /// Nothing to relaunch or say, and why: the launch's own end (a one-shot
    /// run, a subcommand, no registered conversation, an unreadable wrapper
    /// launch) or no exit at all (suspended or backgrounded). Journaled; the
    /// tab is left as is.
    Left(String),
    /// Not now, and why (a busy tab, a shell not back yet, a relaunch that
    /// did not register): the host tries again after its back-off.
    NotYet(String),
    /// Another aterm actor holds the upgrade state: no miss; the host looks
    /// again after [`BUSY`] without moving the back-off or attention.
    Busy,
    /// Never, and why: an interactive conversation that cannot come back (an
    /// uncarryable launch, a gone shell); a person is asked (keyed attention).
    Cannot(String),
}

/// [`Outcome`] from one [`after_exit`] step's word.
#[must_use]
pub fn outcome(step: &str) -> Outcome {
    // The new process holds the conversation: `adopted` (its loop types the
    // continuation at its first idle point, [`resume`]) or `done:fresh` (a
    // fresh start, nothing to carry on) — or, for the carry of a restart's
    // exit, `landed` (another actor finished the move, or its relaunched
    // agent is gone already, [`carry_restart`]).
    // The host hands every relaunched agent to its loop
    // ([`Opts::hand_back`]), so no other word lands — and a carry-on that
    // typed nothing (`done:no-continue`) is never one.
    if matches!(step, "adopted" | "done:fresh" | "landed") {
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
    /// A person typed into the session within `human_grace_s` of the exit: the
    /// exit is theirs and the tab is left to them.
    PersonAsked,
    /// The session is held: halted (`hold=1`), a drive lease, or a turn a named
    /// driver typed ([`held_by_someone`]). The exit is the holder's.
    Held,
    /// Nobody owns the exit but `[harness] relaunch = false`; said once on the
    /// session's attention.
    Limited,
    /// Nobody asked: relaunch it.
    Relaunch,
    /// The harness's own restart ended the agent and left its relaunch in
    /// flight ([`restarted`]): carried ([`carry_restart`]) until it lands or is
    /// said. Typing still waits on a person and a hold via the relaunch line's
    /// own look.
    Restarted,
}

/// Decide whose exit this was. `allowed` is `[harness] relaunch`; `status` the
/// session's `status` reply (`None`: unreadable, read as nobody); `grace_s` is
/// `[harness] human_grace_s`; `stalled`: the agent had stopped reading input,
/// so a keystroke just before was no `/exit`; `restarted`: the harness's own
/// restart ended it ([`restarted`]), asked first because that exit is the
/// restart's whoever is at the tab ([`OnExit::Restarted`]). A person or holder
/// owns any other exit before the configuration limit is asked.
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

/// Whether one `status` reply says the session is held by someone the exit
/// belongs to: `hold=1`, a drive lease (`hand=lease:…`), or a named driver's
/// turn (`hand=turn:<id>:<driver>`). An unnamed turn is an owner-class caller's
/// (this host's loop), and a `driving:` hand concerns sessions it drives.
/// [`super::upgrade_drive`]'s `held` is stricter.
#[must_use]
pub fn held_by_someone(status: &str) -> bool {
    status.split_whitespace().any(|t| t == "hold=1") || driver_hand(status)
}

/// Whether one `status` reply names another driver's hand: a drive lease
/// (`hand=lease:<holder>`) or a named driver's turn (`hand=turn:<id>:<holder>`),
/// never an owner-class turn or this process's own hand
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

/// Whether a person typed into the session within `grace_s` seconds, from one
/// `status` reply ([`HumanInput::of_status`], [`HumanInput::within`]). No
/// keystroke ever, or a server that does not say, is nobody.
#[must_use]
pub fn person_present(status: &str, grace_s: u32) -> bool {
    HumanInput::of_status(status).within(grace_s) == Some(true)
}

/// The pauses before consecutive relaunches: a repeating crash is not relaunched
/// at once, and one that keeps repeating is retried every ten minutes, never
/// given up on.
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

/// The longest pause before the next attempt at a harness restart
/// ([`OnExit::Restarted`]): shorter than [`STALE_S`], so the record cannot
/// expire between attempts.
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

/// One session's relaunch state as the host keeps it across workers: the
/// back-off ([`BACKOFF`], reset by a [`HEALTHY`] run), whether a relaunch is
/// due, whether the tab was left, and what the attention says. Modelled by
/// `HarnessRelaunchOnExit` in `aterm-spec`; the Tier-1 test drives this type
/// through every action.
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
    /// The exit being handled is a harness restart ([`OnExit::Restarted`]):
    /// carried, never left (the model's `restarted`).
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

    /// [`on_exit`] asked again during the pause (a person or holder may be
    /// back, or configuration changed): `None` while still due, else the
    /// attention's word.
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
            // A harness restart is never left (the model's `RestartCarried`):
            // if its carry answered so, it is said as a relaunch that cannot
            // land.
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
