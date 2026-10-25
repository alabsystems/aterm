// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Agent relaunch primitive: resumes an exited agent in its own tab on its existing conversation.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::Value;

use super::accounts;
use super::upgrade::{self, Dialect, SessionFile, Version};
use super::upgrade_drive::{
    Client, Hand, Job, Kernel, Live, Opts, Records, Report, SAME_PROGRAM, Screen,
    SessionRosterCache, St, TAIL_BYTES, Targets, Terminated, Typed, WhereWritten, alive,
    composer_clear, composer_empty, connect, continuation_composer_empty, conversation_live,
    conversation_typed_by, credits_billed_for, cwd_of, earlier_tabs, exe_of,
    finished_where_written, first_word, foreground_shell, held, home_carried, host_roster, ids,
    is_our_ancestor, kept_move, kernel_start, launch_kept, ledger, left_to_person, line_model_of,
    live_background, load, model_and_mark, native_root, note_stuck, now_s, owned_by_aterm,
    process_in_tab, record_asked, restart_models, restore, roster, said, save, screen, since,
    squash, state_dir, sweep_lock, table, tail_to_end, take_back_ask, terminate, turn, turn_fenced,
    type_line, typing_fence, under_hand, unique_tab_for_group,
};
use crate::supervise::approvals;
use crate::supervise::limit::one_line;
pub use crate::supervise::policy::turn_end::Restart;
use crate::supervise::policy::turn_end::TurnEndReading;
use crate::supervise::screen::HumanInput;

/// A relaunch planned from the live process.
pub(super) struct Plan {
    pub(super) shell: u32,
    pub(super) dialect: Dialect,
    pub(super) line: String,
}

/// Why a session has no relaunch plan.
pub(super) enum NoPlan {
    /// A fact this pass could not read (one word).
    Wait(&'static str),
    /// A launch that can never be carried onto this target.
    Refused {
        why: String,
        what: String,
        detail: String,
    },
}

/// What every Claude Code relaunch exports: the survey switch, which aterm also sets in the
/// user's settings and the managed launcher.
pub(super) const CLAUDE_LAUNCH_ENV: [(&str, &str); 1] = [(SURVEY_SWITCH_VAR, "1")];

/// The environment variable Claude Code turns every rating survey off by.
pub(super) const SURVEY_SWITCH_VAR: &str = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY";

/// What a relaunch line is made from.
#[derive(Clone, Copy)]
pub(super) struct Launch<'a> {
    /// The conversation `--resume` names.
    pub(super) session: &'a str,
    /// Whether the line resumes it.
    pub(super) resume: bool,
    /// The directory the conversation was started in, when known.
    pub(super) cwd: &'a str,
    /// The agent the line relaunches (its kernel cwd is asked when `cwd` is empty and it still runs).
    pub(super) agent: u32,
    /// Its argv, `argv[0]` first.
    pub(super) argv: &'a [String],
    /// What the line executes.
    pub(super) exe: &'a Path,
    /// The model the relaunch asks for ([`with_model`]).
    pub(super) model: Option<&'a str>,
    /// The permission mode to resume in ([`resume_mode`], [`with_permission_mode`]).
    pub(super) mode: Option<&'a str>,
    /// The Claude home the line must carry ([`required_env`]).
    pub(super) home: Option<&'a str>,
    /// The `HOME` the line must carry ([`required_env`]).
    pub(super) homedir: Option<&'a str>,
    /// The login the line must carry ([`login_env`]).
    pub(super) login: Option<&'a str>,
    /// Whether the process it replaces authenticated by its stored login ([`Snapshot::stored_login`]).
    pub(super) stored_login: bool,
    /// Whether the line moves the model: without usage credits, a move needs a named model and a
    /// captured Standard billing context equal to the target's.
    pub(super) billing: ModelBilling<'a>,
}

/// Whether a planned line requests a new model, rather than retaining the session's existing choice.
#[derive(Clone, Copy)]
pub(super) enum ModelBilling<'a> {
    Keep,
    Move(Option<&'a super::profile::Context>),
}

fn billing_allows_launch(
    usage_credits: bool,
    billing: ModelBilling<'_>,
    model: Option<&str>,
    target: Option<&super::profile::Context>,
) -> bool {
    match billing {
        ModelBilling::Keep => true,
        ModelBilling::Move(_) if usage_credits => true,
        ModelBilling::Move(from) => {
            model.is_some_and(|model| !model.is_empty())
                && from.zip(target).is_some_and(|(from, target)| {
                    from.selection == super::profile::Selection::Standard && from == target
                })
        }
    }
}

/// The required environment of a relaunch line for `login` ([`Launch::login`]).
pub(super) fn login_env(login: Option<&str>) -> Vec<(&'static str, &str)> {
    login
        .map(|value| (accounts::LOGIN_ENV, value))
        .into_iter()
        .collect()
}

/// Everything a relaunch line must carry ([`Launch::home`], [`Launch::login`]).
pub(super) fn required_env<'a>(
    home: Option<&'a str>,
    homedir: Option<&'a str>,
    login: Option<&'a str>,
) -> Vec<(&'static str, &'a str)> {
    home.map(|value| (accounts::HOME_ENV, value))
        .into_iter()
        .chain(homedir.map(|value| (accounts::HOMEDIR_ENV, value)))
        .chain(login_env(login))
        .collect()
}

/// The [`Snapshot::shell`] of an agent no shell holds.
pub const NO_SHELL: u32 = 0;

/// The ledger's words for a plan [`NO_SHELL`] refuses.
pub(super) const NO_SHELL_DETAIL: &str =
    "the agent is its tab's own program, no shell's job: nothing would read a relaunch line";

/// Plan the relaunch from the live process.
pub(super) fn plan(
    opts: &Opts,
    launch: &Launch<'_>,
    shell: u32,
    t: &[super::upgrade_drive::ProcRow],
) -> Result<Plan, NoPlan> {
    if shell == NO_SHELL {
        return Err(NoPlan::Refused {
            why: "not-a-shell-job".to_string(),
            what: "not-a-shell-job".to_string(),
            detail: NO_SHELL_DETAIL.to_string(),
        });
    }
    let Some(dialect) = shell_dialect(shell, t) else {
        return Err(NoPlan::Wait("shell-dialect"));
    };
    let heal = atpkg::hooks::hook_file(&opts.home, dialect.hook_ext()).map(|(p, _)| p);
    let want = if launch.cwd.is_empty() {
        cwd_of(launch.agent).unwrap_or_default()
    } else {
        launch.cwd.to_string()
    };
    let cd =
        (!want.is_empty() && cwd_of(shell).as_deref() != Some(want.as_str())).then(|| want.clone());
    let model = line_model_of(opts, launch.session, launch.argv, launch.model);
    let required = required_env(launch.home, launch.homedir, launch.login);
    let exact = match launch.billing {
        ModelBilling::Move(Some(from)) if !opts.usage_credits => {
            from.for_launch(&opts.home, &want, &required)
        }
        _ => None,
    };
    if !billing_allows_launch(
        opts.usage_credits,
        launch.billing,
        model,
        exact.as_ref().map(|env| &env.context),
    ) {
        return Err(NoPlan::Refused {
            why: "model-billing-context".to_string(),
            what: "model-billing-context".to_string(),
            detail: "the target model's profile context is unknown or differs from the captured billing context".to_string(),
        });
    }
    let tap = opts
        .usage_tap
        .as_ref()
        .and_then(|armed| tap_of(opts, launch, armed, &want));
    let exact_values: Vec<(&str, &str)> = exact.as_ref().map_or_else(Vec::new, |env| {
        env.values
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect()
    });
    let line = line_tapped_clear(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        launch.exe,
        launch.argv,
        launch.resume.then_some(launch.session),
        model,
        launch.mode,
        if exact.is_some() {
            &exact_values
        } else {
            &required
        },
        exact.as_ref().map(|env| env.unset.as_slice()),
        tap.as_deref(),
    )?;
    Ok(Plan {
        shell,
        dialect,
        line,
    })
}

fn tap_of(
    opts: &Opts,
    launch: &Launch<'_>,
    armed: &super::tap::Armed,
    cwd: &str,
) -> Option<String> {
    let lone = armed.lone_for(launch.login);
    let own = launch.stored_login
        && launch.homedir.is_none()
        && super::tap::own_login(launch.login, launch.home, lone);
    if !own || super::tap::settings_value(&armed.exe).is_none() {
        return None;
    }
    let settings = super::upgrade_drive::tap_settings(opts, launch.argv, cwd);
    let user = super::upgrade_drive::tap_user_settings(&opts.home);
    super::tap::gate(&super::tap::GateFacts {
        exe: Some(&armed.exe),
        stored_login: launch.stored_login,
        login: launch.login,
        lone_login: lone,
        home: launch.home,
        argv: launch.argv,
        settings: &settings,
        user_settings: Some(&user),
        profile_store: super::upgrade_drive::tap_profile_store(opts),
    })
}

/// The dialect of the shell `shell`, as its executable names it.
pub(super) fn shell_dialect(shell: u32, t: &[super::upgrade_drive::ProcRow]) -> Option<Dialect> {
    atpkg::caller_shell::process_args(shell)
        .map(|a| a.exec_path)
        .or_else(|| {
            t.iter()
                .find(|(p, _, _)| *p == shell)
                .map(|(_, _, n)| n.to_string())
        })
        .as_deref()
        .and_then(Dialect::from_exe_name)
}

/// The pure half of [`plan`].
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
    line_tapped(
        dialect, heal, cd, exe, argv, session, model, mode, required, None,
    )
}

/// [`line_for`] with the usage tap's `--settings` value `tap`, where its gate gave one ([`super::tap::gate`]; [`with_tap`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn line_tapped(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    argv: &[String],
    session: Option<&str>,
    model: Option<&str>,
    mode: Option<&str>,
    required: &[(&str, &str)],
    tap: Option<&str>,
) -> Result<String, NoPlan> {
    line_tapped_clear(
        dialect, heal, cd, exe, argv, session, model, mode, required, None, tap,
    )
}

#[allow(clippy::too_many_arguments)]
fn line_tapped_clear(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    argv: &[String],
    session: Option<&str>,
    model: Option<&str>,
    mode: Option<&str>,
    required: &[(&str, &str)],
    unset: Option<&[&str]>,
    tap: Option<&str>,
) -> Result<String, NoPlan> {
    let mut flags =
        upgrade::rewrite_argv(argv, session.unwrap_or("-")).map_err(|e| NoPlan::Refused {
            why: format!("argv:{e}"),
            what: e.to_string(),
            detail: "the launch flags cannot be carried into a resume".to_string(),
        })?;
    if session.is_none() {
        flags.truncate(flags.len().saturating_sub(2));
    }
    let flags = with_permission_mode(with_model(flags, model), mode);
    let both: Vec<(&str, &str)> = required.iter().copied().chain(CLAUDE_LAUNCH_ENV).collect();
    let tapped = tap.map(|value| with_tap(flags.clone(), value));
    let line = |env: &[(&str, &str)], flags: &[String]| match unset {
        Some(unset) => upgrade::relaunch_line_env_clear(dialect, heal, cd, env, unset, exe, flags),
        None => upgrade::relaunch_line_env(dialect, heal, cd, env, exe, flags),
    };
    tapped
        .as_deref()
        .map_or_else(|| Err(String::new()), |t| line(&both, t))
        .or_else(|_| {
            tapped
                .as_deref()
                .map_or_else(|| Err(String::new()), |t| line(required, t))
        })
        .or_else(|_| line(&both, &flags))
        .or_else(|_| line(required, &flags))
        .map_err(|e| NoPlan::Refused {
            why: format!("line:{e}"),
            what: "line".to_string(),
            detail: e,
        })
}

/// `flags` with the usage tap's pair, `--settings <value>`, before the trailing `--resume` pair (at the end of a fresh start's flags).
pub(super) fn with_tap(mut flags: Vec<String>, value: &str) -> Vec<String> {
    let at = flags
        .iter()
        .rposition(|f| f == "--resume")
        .unwrap_or(flags.len());
    flags.splice(at..at, ["--settings".to_string(), value.to_string()]);
    flags
}

/// The Claude Code a manager runs, as a worker it starts must run it (`spawn agent=claude`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerClaude {
    /// The program the manager's process runs.
    pub exe: PathBuf,
    /// [`accounts::HOME_ENV`] the worker's line carries.
    pub home: Option<String>,
    /// The `HOME` the worker's line carries.
    pub homedir: Option<String>,
    /// [`accounts::LOGIN_ENV`] likewise (set and empty is the default login named).
    pub login: Option<String>,
}

/// [`CallerClaude`] of process `pid`, a session's foreground job, under the user's `home`.
#[must_use]
pub fn caller_claude(pid: u32, home: &Path) -> Option<CallerClaude> {
    let args = atpkg::caller_shell::process_args(pid)?;
    let (exe, _) = launched(
        &args.argv,
        exe_of(pid).as_deref(),
        &native_root(home),
        false,
    )?;
    let vars = super::upgrade_drive::HomeVars::of(&args);
    let records = Records::of_process(
        home,
        vars,
        pid,
        || aterm_sysprobe::process_cwd(pid),
        args.env_var("PWD"),
    );
    let (claude_home, homedir) = worker_homes(home, vars, records.dir());
    Some(CallerClaude {
        exe,
        home: claude_home,
        homedir,
        login: args.env_var(accounts::LOGIN_ENV).map(str::to_owned),
    })
}

/// The `(home, homedir)` a worker's line carries of its manager's Claude home ([`CallerClaude`]),
/// from the manager's exec environment `vars` and its records' directory `dir` ([`Records::dir`]).
pub(super) fn worker_homes(
    user_home: &Path,
    vars: super::upgrade_drive::HomeVars<'_>,
    dir: Option<&Path>,
) -> (Option<String>, Option<String>) {
    match vars.config {
        Some(config) if Path::new(config).is_absolute() => (Some(config.to_owned()), None),
        Some(config) => {
            let placed = dir
                .filter(|dir| dir.is_absolute())
                .and_then(Path::to_str)
                .unwrap_or(config);
            (Some(placed.to_owned()), None)
        }
        None => (
            None,
            dir.filter(|dir| !super::upgrade_drive::same_dir(dir, &user_home.join(".claude")))
                .and_then(super::upgrade_drive::homedir_holding),
        ),
    }
}

/// The line a Claude Code worker's shell runs at its first prompt (`spawn agent=claude`): Claude
/// Code's row of [`super::worker::line`].
pub fn task_launch_line(
    dialect: Dialect,
    exe: &Path,
    model: Option<&str>,
    home: Option<&str>,
    homedir: Option<&str>,
    login: Option<&str>,
) -> Result<String, String> {
    let Some(claude) = super::worker::worker_cli(aterm_phase::Program::Claude) else {
        return Err("no worker row for Claude Code".to_string());
    };
    let required = required_env(home, homedir, login);
    super::worker::line_of(
        claude,
        dialect,
        exe,
        &[],
        &required,
        &[],
        &super::worker::Fresh {
            model,
            ..super::worker::Fresh::default()
        },
    )
}

/// `flags` with every `--model` spelling replaced by `model` (before the trailing `--resume` pair), or dropped when it is empty.
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

const PERMISSION_MODES: [&str; 7] = [
    "default",
    "manual",
    "acceptEdits",
    "plan",
    "auto",
    "dontAsk",
    "bypassPermissions",
];

fn last_permission_mode(records: &Records, session: &str) -> Option<String> {
    let path = records.transcript(session)?;
    let (tail, _) = tail_to_end(&path, TAIL_BYTES);
    permission_mode_of(&tail)
}

/// [`last_permission_mode`]'s scan, pure.
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

/// The permission mode to resume `session` in.
pub(super) fn resume_mode(
    records: &Records,
    session: &str,
    shown: Option<String>,
) -> Option<String> {
    shown.or_else(|| last_permission_mode(records, session))
}

fn tab_mode(opts: &Opts, tab: &str) -> Option<String> {
    let mut c = connect(opts, tab).ok()?;
    screen_mode(&screen(&mut c, tab)?.rows)
}

/// The `--permission-mode` value of the pill under Claude's composer on `rows` ([`super::lights::read_screen`]).
pub(super) fn screen_mode(rows: &[String]) -> Option<String> {
    super::lights::read_screen(rows)
        .and_then(|shown| shown.mode)
        .map(|mode| mode_flag(mode).to_string())
}

const fn mode_flag(mode: super::lights::Mode) -> &'static str {
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

/// `flags` (the launch's, from [`upgrade::rewrite_argv`], resume pair last) resumed in permission `mode` ([`resume_mode`]).
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

/// A session with no relaunch plan.
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

/// How long a relaunch in flight may still act on its tab, from the agent's end (exiting) or the relaunch line (relaunched).
pub const STALE_S: u64 = 300;

/// The one clock a restart's line goes stale by.
#[must_use]
pub(super) fn exit_clock(at_s: u64, exited_at: u64, hand_seen_s: u64) -> u64 {
    at_s.max(exited_at).max(hand_seen_s)
}

/// A stale exit's ledger line for `who` (`the agent`, `Codex`), alike in both lanes.
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

/// Why a relaunch in flight whose agent is gone must stop instead of acting on the tab, `(word, ledger detail)`, or `None` while it may still act.
pub(super) fn expired(st: &St, now: u64) -> Option<(&'static str, String)> {
    use upgrade::Phase;
    match st.phase {
        Phase::Exiting { at_s }
            if now.saturating_sub(exit_clock(at_s, st.exited_at, st.hand_seen_s)) > STALE_S =>
        {
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

/// What a new process holding the conversation of a relaunch in flight is to that relaunch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Relaunch {
    /// The relaunch this record typed.
    Ours,
    /// Proven not the relaunch.
    NotOurs,
    /// Its parent could not be read now (it ended as it was asked, or `ps` could not run).
    Unread,
}

/// [`Relaunch`] from the new process's parent as read now (`None`: unread) and the shell the relaunch line was typed at (`0`: none was recorded).
pub(super) const fn relaunch_by_parent(parent: Option<u32>, shell: u32) -> Relaunch {
    match parent {
        _ if shell == 0 => Relaunch::NotOurs,
        None => Relaunch::Unread,
        Some(p) if p == shell => Relaunch::Ours,
        Some(_) => Relaunch::NotOurs,
    }
}

/// Whether `s` is the relaunch this record typed.
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

/// Poll `cond` for at most `limit`, for conditions with no event to wait on (exit has [`super::upgrade_wake::wait_exit`], the verdict [`first_idle`]).
pub(super) fn wait_until(limit: Duration, cond: impl FnMut() -> bool) -> bool {
    wait_every(limit, poll_gap(limit), cond)
}

fn poll_gap(limit: Duration) -> Duration {
    (limit / 12).clamp(Duration::from_millis(5), Duration::from_millis(250))
}

fn wait_every(limit: Duration, gap: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = wait_now();
    while wait_now().saturating_duration_since(start) < limit {
        if cond() {
            return true;
        }
        pause(gap);
    }
    cond()
}

/// The harness's wait clock.
pub(super) fn wait_now() -> Instant {
    #[cfg(test)]
    if let Some((at, _)) = VIRTUAL_WAITS.with(std::cell::Cell::get) {
        return at;
    }
    Instant::now()
}

/// The harness's pause between two looks, or before a line it types.
pub(super) fn pause(d: Duration) {
    #[cfg(test)]
    if let Some((at, held)) = VIRTUAL_WAITS.with(std::cell::Cell::get) {
        VIRTUAL_WAITS.with(|v| v.set(Some((at + d, held))));
        return;
    }
    std::thread::sleep(d);
}

#[cfg(test)]
thread_local! {
    static VIRTUAL_WAITS: std::cell::Cell<Option<(Instant, usize)>> =
        const { std::cell::Cell::new(None) };
}

/// A hold on this thread's wait clock ([`virtual_waits`]).
#[cfg(all(test, unix))]
#[must_use]
pub(crate) struct VirtualWaits(());

/// The harness's waits on this thread run on their own steps while the hold lives ([`wait_now`], [`pause`]).
#[cfg(all(test, unix))]
pub(crate) fn virtual_waits() -> VirtualWaits {
    VIRTUAL_WAITS.with(|v| {
        v.set(Some(
            v.get()
                .map_or((Instant::now(), 1), |(at, held)| (at, held + 1)),
        ));
    });
    VirtualWaits(())
}

#[cfg(all(test, unix))]
impl Drop for VirtualWaits {
    fn drop(&mut self) {
        VIRTUAL_WAITS.with(|v| {
            v.set(
                v.get()
                    .and_then(|(at, held)| (held > 1).then_some((at, held - 1))),
            );
        });
    }
}

/// The agent ended (signalled or by itself).
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
    if !super::upgrade_wake::wait_exit(old, opts.timings.exited) {
        return said(r, "wait:exiting");
    }
    st.seen_gone(now_s());
    hand.keep(c);
    let shell = st.shell;
    let prompt_back = wait_until(opts.timings.prompt_back, || {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    });
    hand.keep(c);
    if !prompt_back {
        return said(r, "wait:shell-prompt");
    }
    pause(opts.timings.prompt_drawn);
    let tab = st.tab.clone();
    let line = st.line.clone();
    let dialect = shell_dialect(shell, &table());
    let asked = record_asked(opts, session, &st.model_list);
    let block = line_block(c, &tab);
    let typed = type_relaunch_line(
        opts,
        &st.agent_records(&opts.home),
        c,
        shell,
        &tab,
        session,
        &line,
        dialect,
        PromptLook::of(st),
        |c, pid, tab| process_in_tab(c, pid, tab, None, None),
    );
    if typed.is_err() {
        take_back_ask(opts, session, asked);
    }
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
            st.stop("relaunch-refused", now_s());
            let r = said(r, format!("failed:relaunch:{}", first_word(&e)));
            ledger(opts, &r, &st.line);
            return r;
        }
    }
    typed_recorded(opts, st, session, block, |st| {
        ledger(opts, &said(r.clone(), "relaunched"), &st.line);
    });
    await_held(opts, r, st, c, hand, session, k)
}

fn typed_recorded(
    opts: &Opts,
    st: &mut St,
    session: &str,
    block: Option<u64>,
    after: impl FnOnce(&St),
) {
    st.relaunched(now_s());
    st.line_block = block;
    st.relaunched_pid = 0;
    save(opts, session, st);
    after(st);
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RelaunchLineError {
    Wait(&'static str),
    /// A hand the line waits on stands on the tab ([`hand_on_tab`]: a hold, another driver's lease, a turn).
    Hand,
    Turn(String),
}

/// Type the relaunch line.
#[allow(clippy::too_many_arguments)]
pub(super) fn type_relaunch_line(
    opts: &Opts,
    records: &Records,
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
        || {
            records
                .session_files()
                .is_some_and(|files| !conversation_live(&files, session))
        },
        tab_probe,
    )
}

/// What one relaunch attempt judges the returned prompt by.
pub(super) struct PromptLook<'a> {
    pub(super) mark: &'a mut Option<PromptMark>,
    pub(super) ended_at_s: u64,
    /// Where a hand held the line ([`hand_held_since`]).
    pub(super) held_since_s: u64,
}

impl<'a> PromptLook<'a> {
    /// The look record `st` keeps.
    pub(super) fn of(st: &'a mut St) -> Self {
        let ended_at_s = end_asked_at(st);
        let held_since_s = hand_held_since(st);
        Self {
            mark: &mut st.prompt,
            ended_at_s,
            held_since_s,
        }
    }
}

/// The instant a person's keys must predate where a hand held the line ([`PromptLook::held_since_s`]).
pub(super) fn hand_held_since(st: &St) -> u64 {
    match st.phase {
        upgrade::Phase::Exiting { at_s } if st.hand_seen_s != 0 => at_s.max(1),
        _ => 0,
    }
}

/// When the agent's end was asked, to date a person's keys against ([`PromptLook::ended_at_s`]).
pub(super) fn end_asked_at(st: &St) -> u64 {
    match st.phase {
        upgrade::Phase::Exiting { at_s } if ends_its_agent(&st.cause) => at_s,
        _ => 0,
    }
}

/// The one relaunch line, whichever agent it resumes.
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
    let PromptLook {
        mark,
        ended_at_s,
        held_since_s,
    } = look;
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    if !owner_clear() {
        return Err(RelaunchLineError::Wait("conversation-owner-ambiguous"));
    }
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    match super::upgrade_drive::status_line(c, tab) {
        Some(line) if line.starts_with("OK") && hand_on_tab(&line) => {
            return Err(RelaunchLineError::Hand);
        }
        line if line
            .as_deref()
            .is_none_or(|l| super::upgrade_drive::held_by_status(l, opts.human_grace_s)) =>
        {
            return Err(RelaunchLineError::Wait("held"));
        }
        _ => {}
    }
    if mark.is_none()
        && let Some(scr) = screen(c, tab)
    {
        *mark = first_mark(c, tab, &scr, ended_at_s, held_since_s)?;
    }
    let healed = dialect.and_then(|d| healed_line(c, tab, shell, d, line));
    let typed = type_at_prompt(
        c,
        shell,
        tab,
        healed.as_deref().unwrap_or(line),
        mark,
        &mut tab_probe,
        opts.timings.fence_every,
    );
    if typed.is_err() && healed.is_some() {
        withdraw_rekey(c, tab);
    }
    typed
}

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

fn issue_rekey(c: &mut Client, tab: &str, shell: u32) -> Option<PathBuf> {
    let reply = c
        .request_line(&format!("@{tab} rekey shell={shell}"))
        .ok()?;
    rekey_path_of(&reply)
}

fn rekey_path_of(reply: &str) -> Option<PathBuf> {
    let rest = reply
        .trim_end_matches(['\r', '\n'])
        .strip_prefix("OK rekey ")?;
    let (_, path) = rest.split_once("path=")?;
    Path::new(path).is_absolute().then(|| PathBuf::from(path))
}

fn withdraw_rekey(c: &mut Client, tab: &str) {
    let _ = c.request_line(&format!("@{tab} rekey withdraw"));
}

fn refused_line(e: String) -> RelaunchLineError {
    if e.starts_with("ERR busy") {
        RelaunchLineError::Wait("held")
    } else if e.starts_with("ERR halted") {
        RelaunchLineError::Hand
    } else {
        RelaunchLineError::Turn(e)
    }
}

fn type_at_prompt(
    c: &mut Client,
    shell: u32,
    tab: &str,
    line: &str,
    mark: &mut Option<PromptMark>,
    tab_probe: &mut impl FnMut(&mut Client, u32, &str) -> bool,
    fence_every: Duration,
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
                Err(Typed::Changed) => pause(fence_every),
                Err(Typed::Yielded) => return Err(RelaunchLineError::Wait("yield")),
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

fn first_mark(
    c: &mut Client,
    tab: &str,
    scr: &Screen,
    ended_at_s: u64,
    held_since_s: u64,
) -> Result<Option<PromptMark>, RelaunchLineError> {
    let Some(mark) = PromptMark::of(scr) else {
        return Ok(None);
    };
    let row = scr
        .cursor
        .and_then(|(r, _)| scr.rows.get(r))
        .map_or("", String::as_str);
    if first_drawn(row, mark.col).is_some_and(|at| at < mark.col.saturating_add(2)) {
        return Err(RelaunchLineError::Wait("typing"));
    }
    if held_since_s != 0
        && scr
            .human
            .quiet_since(held_since_s.saturating_mul(1000), now_ms())
            != Some(true)
    {
        return Err(RelaunchLineError::Wait("keyed-while-held"));
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
        None if blank_before(row, mark.col) => Ok(Some(mark)),
        None if unkeyed => Err(RelaunchLineError::Wait("unmarked")),
        None => Err(RelaunchLineError::Wait("typing")),
    }
}

fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}

struct InputStart {
    row: String,
    col: usize,
}

impl InputStart {
    fn names(&self, cursor_row: &str) -> bool {
        left_of(&self.row, self.col) == left_of(cursor_row, self.col)
    }
}

fn left_of(row: &str, col: usize) -> &str {
    let end = aterm_grapheme::grid_graphemes(row)
        .find(|g| col < g.end_col)
        .map_or(row.len(), |g| g.byte_offset);
    &row[..end]
}

fn first_drawn(row: &str, col: usize) -> Option<usize> {
    aterm_grapheme::grid_graphemes(row)
        .find(|g| g.end_col > col && !g.text.trim().is_empty())
        .map(|g| g.start_col)
}

fn blank_before(row: &str, col: usize) -> bool {
    col > 0
        && aterm_grapheme::grid_graphemes(row)
            .filter(|g| g.start_col < g.end_col && g.start_col < col)
            .last()
            .is_none_or(|g| g.end_col < col || g.text.trim().is_empty())
}

fn input_start(c: &mut Client, tab: &str) -> Option<InputStart> {
    let block = newest_block(c, tab)?;
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

/// Where a shell's prompt left the cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PromptMark {
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) left: String,
    /// The column where right-prompt text began when marked, `None` for a blank rest of row.
    pub(super) right: Option<usize>,
}

/// What a later read of the prompt shows against its [`PromptMark`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtPrompt {
    /// The cursor where the mark left it (text redrawn in place — a ticking clock — keeps it there), and nothing between it and the right prompt.
    Same,
    /// A person typed after the mark.
    Typed,
    /// Anything else: an async redraw of the prompt, a new prompt below.
    Moved,
}

impl PromptMark {
    pub(super) fn of(scr: &Screen) -> Option<PromptMark> {
        let (row, col) = scr.cursor?;
        let text = scr.rows.get(row)?;
        Some(PromptMark {
            row: scr.first.saturating_add(row),
            col,
            left: left_of(text, col).to_string(),
            right: first_drawn(text, col),
        })
    }

    pub(super) fn at(&self, scr: &Screen) -> AtPrompt {
        let Some(now) = PromptMark::of(scr) else {
            return AtPrompt::Moved;
        };
        if now.row == self.row && now.col == self.col {
            let text = now
                .row
                .checked_sub(scr.first)
                .and_then(|r| scr.rows.get(r))
                .map_or("", String::as_str);
            let end = self.right.unwrap_or(usize::MAX);
            let typed = first_drawn(text, self.col).is_some_and(|at| at < end);
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

    /// The mark as the state file keeps it.
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

    /// [`Self::word`] read back.
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

/// How many fenced tries the relaunch line gets ([`type_relaunch_line`]), [`super::Timings::fence_every`] apart, before it is pasted unfenced.
pub(super) const RELAUNCH_FENCE_TRIES: usize = 5;

/// After the relaunch line.
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

fn await_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    session: &str,
    k: &dyn Kernel,
) -> Report {
    let records = st.agent_records(&opts.home);
    let (old, shell) = (st.pid, st.shell);
    let fresh = starts_afresh(&st.cause);
    let mut new: Option<SessionFile> = None;
    let mut roster = SessionRosterCache::default();
    let mut back = ShellBack::after(st.line_block, &opts.timings);
    let mut ended = false;
    let look_back = ends_at_once(&st.cause);
    let tab = st.tab.clone();
    let limit = held_wait(&opts.timings, &st.cause);
    wait_every(limit, poll_gap(limit).min(opts.timings.shell_look), || {
        hand.keep(c);
        new = roster.files(&records, wait_now()).and_then(|files| {
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
        let now = wait_now();
        if look_back && new.is_none() && back.due(now) {
            let over = back.block.and_then(|block| line_over(c, &tab, block));
            ended = back.saw(k.shell_holds_tab(shell), over, now);
        }
        new.is_some() || ended
    });
    if let Some(sf) = &new {
        first_idle(c, hand, &st.tab, opts.timings.first_idle, || {
            records
                .session_file(sf.pid)
                .is_some_and(|sf| sf.status == "idle")
        });
    }
    match new {
        Some(sf) if fresh => {
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
            if st.relaunched_pid == sf.pid {
                return r;
            }
            st.relaunched_pid = sf.pid;
            let tab = st.tab.clone();
            let promise = Adopted::of(
                st,
                || {
                    let ours = conversation_typed_by(opts, &tab, session, st);
                    let written = WhereWritten::new(opts, &records, session, &ours);
                    nothing_owed(opts, &written, &tab, st).map(|(_, why)| why)
                },
                || opts.land || tab_under_land(c, &tab),
            );
            ledger(opts, &r, &promise.detail());
            save(opts, session, st);
            r
        }
        Some(sf) => carry_on(opts, r, st, c, session, &sf, Some(hand)),
        None if ended && st.roll_back(now_s()) => {
            let failed = said(r.clone(), "failed:no-resume");
            ledger(
                opts,
                &failed,
                &format!(
                    "the relaunched agent ended without registering the conversation: its shell \
                     has the tab back; resumed on {}, the build it ran",
                    one_line(&st.to)
                ),
            );
            save(opts, session, st);
            let r = Report {
                to: format!("{}({})", st.to, st.source),
                ..r
            };
            relaunch_held(opts, r, st, c, hand, session, k)
        }
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

/// What an `adopted` row promises of the relaunch's last step, which the tab's loop takes at its
/// first idle point ([`resume`]), ranked as [`carry_on_once`] ranks it there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Adopted {
    /// A quiet restart ([`resumes_quietly`]), resumed with nothing typed (`done:resumed`).
    Quiet,
    /// The conversation's own records owe nothing, for the why [`nothing_owed`] names.
    NothingOwed(&'static str),
    /// Under Autopilot Land ([`Opts::land`], or the tab's own) no continuation is typed.
    Land,
    /// Any other move owes the continuation, which the tab's supervisor types at the agent's
    /// first idle point.
    Owed,
}

impl Adopted {
    fn of(
        st: &St,
        none_owed: impl FnOnce() -> Option<&'static str>,
        land: impl FnOnce() -> bool,
    ) -> Self {
        if resumes_quietly(st) {
            Self::Quiet
        } else if let Some(why) = none_owed() {
            Self::NothingOwed(why)
        } else if land() {
            Self::Land
        } else {
            Self::Owed
        }
    }

    /// The `adopted` row's words.
    pub(super) fn detail(self) -> String {
        match self {
            Self::Quiet => {
                "the relaunched agent holds the conversation; resumed while idle, nothing typed"
                    .to_string()
            }
            Self::NothingOwed(why) => format!(
                "the relaunched agent holds the conversation; no continuation is owed: {why}"
            ),
            Self::Land => "the relaunched agent holds the conversation; under Land no \
                           continuation is typed: its loop decides its point"
                .to_string(),
            Self::Owed => "the relaunched agent holds the conversation; its supervisor types \
                           the continuation at its first idle point"
                .to_string(),
        }
    }
}

/// Whether tab `tab` is under Land as the relaunch adopts its agent.
pub(super) fn tab_under_land(c: &mut Client, tab: &str) -> bool {
    super::upgrade_drive::status_line(c, tab).is_some_and(|line| land_by_status(&line))
}

fn land_by_status(line: &str) -> bool {
    autopilot_by_status(line) == Some("land")
}

fn autopilot_by_status(line: &str) -> Option<&str> {
    if !crate::is_ok_reply(line) {
        return None;
    }
    line.split_whitespace()
        .find_map(|w| w.strip_prefix("autopilot="))
        .filter(|a| *a != "-")
}

fn autopilot_on(c: &mut Client, tab: &str) -> bool {
    super::upgrade_drive::status_line(c, tab)
        .is_some_and(|line| matches!(autopilot_by_status(&line), Some("cruise" | "land")))
}

/// Whether a restart of `cause` is carried on with nothing typed.
#[must_use]
pub fn carried_quietly(cause: &str, quiet: bool) -> bool {
    cause.starts_with(CAUSE_ACCOUNT) || (quiet && !relaunch_cause(cause))
}

fn resumes_quietly(st: &St) -> bool {
    carried_quietly(&st.cause, st.quiet)
}

fn looks_back(st: &St) -> bool {
    resumes_quietly(st) || !st.look_word.is_empty()
}

fn look_after_nothing(st: &mut St, word: &str, version: &str, pid: u32) {
    st.resumed_on = version.to_string();
    st.resumed_pid = pid;
    st.confirm_by = st.done_at.saturating_add(RESTORE_LOOK.as_secs());
    st.look_word = word.to_string();
}

fn held_wait(t: &super::Timings, cause: &str) -> Duration {
    if cause == CAUSE_HOST {
        t.restored_held
    } else {
        t.held
    }
}

/// Whether [`await_held`] watches for a relaunched agent that ended as it started ([`ShellBack`]).
pub(super) fn ends_at_once(cause: &str) -> bool {
    cause == CAUSE_HOST || ends_its_agent(cause)
}

/// Tracks whether the relaunched agent ended as it started.
#[derive(Debug)]
pub(super) struct ShellBack {
    since: Option<Instant>,
    looked: Option<Instant>,
    block: Option<u64>,
    left: bool,
    look: Duration,
    ended_at_once: Duration,
}

impl ShellBack {
    /// The tracker for a line typed into the command block `block` ([`St::line_block`]; `None`: none read), on the clock `t`.
    pub(super) const fn after(block: Option<u64>, t: &super::Timings) -> Self {
        Self {
            since: None,
            looked: None,
            block,
            left: false,
            look: t.shell_look,
            ended_at_once: t.ended_at_once,
        }
    }

    /// Whether a look is due at `now` ([`Self::look`] since the last).
    pub(super) fn due(&mut self, now: Instant) -> bool {
        let due = self
            .looked
            .is_none_or(|at| now.saturating_duration_since(at) >= self.look);
        if due {
            self.looked = Some(now);
        }
        due
    }

    /// One look at `now`, given whether the shell holds its terminal ([`Kernel::shell_holds_tab`])
    /// and whether the line's block is over ([`line_over`]; `None`: unread). True once the shell
    /// has held the tab for `ended_at_once` with the line run: its block over or, with no block
    /// known, the shell seen to give the tab up before.
    pub(super) fn saw(&mut self, holds: Option<bool>, over: Option<bool>, now: Instant) -> bool {
        let ran = match holds {
            None => return false,
            Some(false) => {
                self.left = true;
                self.since = None;
                return false;
            }
            Some(true) if self.block.is_some() => match over {
                None => return false,
                Some(over) => over,
            },
            Some(true) => self.left,
        };
        if !ran {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) >= self.ended_at_once
    }
}

/// The command block a relaunch line is typed into ([`St::line_block`]).
pub(super) fn line_block(c: &mut Client, tab: &str) -> Option<u64> {
    let newest = newest_block(c, tab)?;
    let state = newest.get("state").and_then(Value::as_str)?;
    matches!(state, "prompt" | "entering")
        .then(|| newest.get("id").and_then(Value::as_u64))
        .flatten()
}

/// The command line the tab's shell runs now, as its integration recorded it (typed).
pub(super) fn running_cmdline(c: &mut Client, tab: &str) -> Option<String> {
    let newest = newest_block(c, tab)?;
    if newest.get("state").and_then(Value::as_str) != Some("executing") {
        return None;
    }
    newest
        .get("cmdline")
        .and_then(Value::as_str)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
}

fn line_over(c: &mut Client, tab: &str, block: u64) -> Option<bool> {
    let newest = newest_block(c, tab)?;
    let id = newest.get("id").and_then(Value::as_u64)?;
    match id.cmp(&block) {
        std::cmp::Ordering::Greater => Some(true),
        std::cmp::Ordering::Equal => {
            Some(newest.get("state").and_then(Value::as_str) == Some("complete"))
        }
        std::cmp::Ordering::Less => None,
    }
}

fn newest_block(c: &mut Client, tab: &str) -> Option<Value> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 1 --json")).ok()?;
    if !crate::is_ok_reply(&head) {
        return None;
    }
    let json = if body.trim().is_empty() {
        head.strip_prefix("OK ")?
    } else {
        &body
    };
    let v: Value = aterm_json::from_str(json.trim()).ok()?;
    v.get("blocks")?.as_array()?.last().cloned()
}

/// Wait at most `within` for the relaunched agent's first idle, renewing the harness's `hand`
/// first; an opening box is the supervisor's to answer once the step returns.
pub(super) fn first_idle(
    c: &mut Client,
    hand: &mut Hand,
    tab: &str,
    within: Duration,
    record_idle: impl FnMut() -> bool,
) {
    hand.keep(c);
    let awaited = c.request_line(&format!(
        "@{tab} await agent idle timeout={}",
        within.as_millis()
    ));
    if !awaited.is_ok_and(|reply| crate::is_ok_reply(&reply)) {
        wait_until(within, record_idle);
    }
}

const SINCE_BYTES: u64 = 4 << 20;

/// How long after the continuation the resumed session's first answer may still confirm the model.
pub(super) const MODEL_WAIT: Duration = Duration::from_secs(120);

/// The new process holds the conversation.
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

/// [`carry_on`] with `tab_probe`, the seam for the race between the first ownership read and the
/// typed continuation.
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
    let records = st.agent_records(&opts.home);
    let mut conversation_changed = false;
    let mut tail_available = true;
    let settled = wait_until(opts.timings.carry_settle, || {
        // This wait (up to 120 s) outlasts the hand's 60 s lease: renew it throughout.
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        let Some(sf) = records.session_file(new.pid) else {
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
    let Some(current) = continuation_session(&records, new.pid, key) else {
        return said(r, "wait:conversation-changed");
    };
    if resumes_quietly(st) {
        return done_quietly(opts, r, st, key, &current.version, new.pid);
    }
    let from = Version::parse(&st.from);
    if settled
        && (records.unique_owner(&current).is_err()
            || kernel_start(new.pid).as_deref() != Some(squash(&new.proc_start).as_str())
            || squash(&current.proc_start) != squash(&new.proc_start)
            || !tab_probe(c, new.pid, tab))
    {
        return said(r, "wait:tab-ownership-changed");
    }
    let Some(latest) = continuation_session(&records, new.pid, key) else {
        return said(r, "wait:conversation-changed");
    };
    if latest.status != "idle"
        || squash(&latest.proc_start) != squash(&new.proc_start)
        || kernel_start(new.pid).as_deref() != Some(squash(&latest.proc_start).as_str())
    {
        return said(r, "wait:changed");
    }
    if let Err(why) = records.unique_owner(&latest) {
        return said(r, format!("wait:{why}"));
    }
    let ours = conversation_typed_by(opts, tab, key, st);
    let written = WhereWritten::new(opts, &records, key, &ours);
    if settled && let Some((word, why)) = nothing_owed(opts, &written, tab, st) {
        return done_with_nothing(opts, r, st, key, word, why, &latest.version, new.pid);
    }
    if opts.land || tab_under_land(c, tab) {
        st.phase = upgrade::Phase::Done;
        r.from.clone_from(&st.from);
        r.to = format!("{}({})", latest.version, st.source);
        let r = said(r, "done:landing");
        st.outcome = format!(
            "claude restarted on {} · nothing typed: Autopilot is Land, and the tab's loop \
             decides its point",
            one_line(&latest.version)
        );
        st.done_at = now_s();
        look_after_nothing(st, &r.step, &latest.version, new.pid);
        ledger(opts, &r, &st.outcome);
        save(opts, key, st);
        return r;
    }
    let (said_done, irreversible) = if settled {
        written.said_done().unwrap_or_default()
    } else {
        (false, false)
    };
    let to = Version::parse(&latest.version);
    let before = (!st.model_before.is_empty()).then(|| st.model_before.clone());
    let text = if relaunch_cause(&st.cause) {
        Some(resumed_prompt(&latest.version, &st.cause))
    } else {
        from.as_ref().zip(to.as_ref()).map(|(f, t)| {
            upgrade::continue_prompt_with_model(f, t, before.as_deref(), recorded(&st.model_list))
        })
    };
    let text = text.map(|text| {
        if said_done {
            upgrade::checking(&text, irreversible)
        } else {
            text
        }
    });
    let typed = settled
        && match text {
            Some(text) => {
                if !tab_probe(c, new.pid, tab) {
                    return said(r, "wait:tab-ownership-changed");
                }
                if super::upgrade_drive::held(c, tab, opts.human_grace_s) {
                    return said(r, "wait:held");
                }
                let Some(typed) = under_hand(c, tab, hand, |c| {
                    type_continuation(c, tab, &text, opts.timings.fence_every)
                }) else {
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
        look_after_nothing(st, &r.step, &latest.version, new.pid);
        ledger(opts, &r, &st.outcome);
        save(opts, key, st);
        return r;
    }
    st.resumed_on.clone_from(&latest.version);
    st.resumed_pid = new.pid;
    st.confirm_by = now_s().saturating_add(confirm_within.as_secs());
    save(opts, key, st);
    let r = said(
        r,
        if said_done {
            "continued:check"
        } else {
            "continued"
        },
    );
    ledger(
        opts,
        &r,
        if said_done {
            "its last words claim the work is done: the continuation's facts and the Cruise \
             prompt are typed; a later step reads the model its answer names"
        } else {
            "the continuation is typed; a later step reads the model its answer names"
        },
    );
    r
}

fn done_quietly(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    key: &str,
    version: &str,
    pid: u32,
) -> Report {
    st.phase = upgrade::Phase::Done;
    r.from.clone_from(&st.from);
    r.to = format!("{version}({})", st.source);
    let r = said(r, "done:resumed");
    st.outcome = quiet_outcome(st, version);
    st.done_at = now_s();
    st.resumed_on = version.to_string();
    st.resumed_pid = pid;
    st.confirm_by = st.done_at.saturating_add(RESTORE_LOOK.as_secs());
    ledger(opts, &r, &st.outcome);
    save(opts, key, st);
    r
}

/// Why a conversation nobody but the harness has asked anything is owed no carry-on
/// (`done:taskless`).
pub(super) const NOBODY_ASKED: &str = "nobody has asked this conversation anything";

/// Why a relaunch whose conversation's record says its task landed is carried on with nothing ([`nothing_owed`]).
pub const RECORD_LANDED: &str = "its record says it defended its task as landed and nobody has asked \
                                 anything since (the tab's loop checks the repository)";

fn nothing_owed(
    opts: &Opts,
    written: &WhereWritten<'_>,
    tab: &str,
    st: &St,
) -> Option<(&'static str, &'static str)> {
    let (records, key, ours) = (written.records(), written.session(), written.ours());
    if records.tasked(key, ours) == Some(false) {
        return Some(("done:taskless", NOBODY_ASKED));
    }
    if written.finished() == Some(true) {
        return Some(("done:finished", RECORD_LANDED));
    }
    if left_to_person(opts, records, tab, key, st) {
        return Some(("done:left", "its last turn was a person's own exchange"));
    }
    if loop_typed_since(opts, tab, relaunched_line_at(st)) {
        return Some(("done:carried", "the tab's loop resumed it first"));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn done_with_nothing(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    key: &str,
    word: &str,
    why: &str,
    version: &str,
    pid: u32,
) -> Report {
    st.phase = upgrade::Phase::Done;
    r.from.clone_from(&st.from);
    r.to = format!("{version}({})", st.source);
    let r = said(r, word);
    st.outcome = format!(
        "claude restarted on {} · nothing typed: {why}",
        one_line(version)
    );
    st.done_at = now_s();
    look_after_nothing(st, &r.step, version, pid);
    ledger(opts, &r, &st.outcome);
    save(opts, key, st);
    r
}

fn relaunched_line_at(st: &St) -> u64 {
    match st.phase {
        upgrade::Phase::Relaunched { at_s } => at_s.max(st.relaunched_at),
        _ => st.relaunched_at,
    }
}

fn loop_typed_since(opts: &Opts, tab: &str, since: u64) -> bool {
    let Some(root) = opts.aterm_state.as_deref() else {
        return false;
    };
    let Ok(since) = i64::try_from(since) else {
        return false;
    };
    since != 0
        && approvals::typed_texts(&approvals::ledger_under(root, Some(tab)), Some(tab))
            .iter()
            .any(|t| {
                t.at_ms.div_euclid(1000) > since
                    && !t.text.starts_with("relaunch ")
                    && t.text != super::usage::USAGE_COMMAND
            })
}

fn type_continuation(c: &mut Client, tab: &str, text: &str, fence_every: Duration) -> bool {
    for _ in 0..CONTINUE_FENCE_TRIES {
        let generation = match typing_fence(c, tab) {
            Ok(generation) => generation,
            Err("changed") => {
                pause(fence_every);
                continue;
            }
            Err(_) => return false,
        };
        match turn_fenced(c, tab, text, generation.as_deref()) {
            Ok(()) => return true,
            Err(Typed::Changed | Typed::Yielded) => pause(fence_every),
            Err(Typed::Cut(_) | Typed::Refused(_)) => return false,
        }
    }
    composer_clear(c, tab) && turn(c, tab, text).is_ok()
}

/// How many fenced tries the continuation gets ([`type_continuation`]).
pub(super) const CONTINUE_FENCE_TRIES: usize = 3;

/// The model after, for a restart whose continuation was typed.
pub(super) fn confirm(opts: &Opts, r: &Report, st: &mut St, key: &str, now: u64) -> Option<Report> {
    // A quiet restart's look, or that of a carry-on that typed nothing ([`restore_look`]):
    // a sweep reads it, and types its line only into a tab whose own Autopilot is on, under
    // the tab's own guards.
    if looks_back(st) {
        return restore_look(opts, r, st, key, now, None, |c, pid, tab| {
            process_in_tab(c, pid, tab, None, None)
        });
    }
    let after = resumed_model(&st.agent_records(&opts.home), key, st.mark, &st.resumed_on);
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

/// How long a quiet restart's look stays open ([`restore_look`]).
pub(super) const RESTORE_LOOK: Duration = Duration::from_secs(15);

const RESTORE_TYPE_WITHIN_S: u64 = STALE_S;

fn told_nothing(opts: &Opts, records: &Records, key: &str, st: &St) -> Option<&'static str> {
    let ours = conversation_typed_by(opts, &st.tab, key, st);
    if finished_where_written(opts, records, key, &ours) == Some(true) {
        Some("its worker finished its task")
    } else if records.tasked(key, &ours) == Some(false) {
        Some("nobody has asked it anything")
    } else {
        None
    }
}

const RESTORE_LOST: &str =
    "Claude Code reported that something this session ran did not come back after the restart";

/// The one look after a quiet restart or a carry-on that typed nothing, open [`RESTORE_LOOK`]: if
/// the resumed agent warned that something did not come back, a tasked agent is told to restore
/// what it relies on.
pub(super) fn restore_look(
    opts: &Opts,
    r: &Report,
    st: &mut St,
    key: &str,
    now: u64,
    tab: Option<&str>,
    mut tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Option<Report> {
    let records = st.agent_records(&opts.home);
    if !look_warned(&records, key, st) {
        if now < st.confirm_by {
            return None;
        }
        st.confirm_by = 0;
        let word = match std::mem::take(&mut st.look_word) {
            w if w.is_empty() => "done:resumed".to_string(),
            w => w,
        };
        save(opts, key, st);
        return Some(said(r.clone(), word));
    }
    let quiet = told_nothing(opts, &records, key, st);
    let late = now >= st.confirm_by.saturating_add(RESTORE_TYPE_WITHIN_S);
    let Some(tab) = tab else {
        if late {
            return Some(restore_lost(
                opts,
                r,
                st,
                key,
                quiet.unwrap_or("no idle point came to say so at"),
            ));
        }
        let tab = st.tab.clone();
        if quiet.is_some() || !super::tap::is_tab_id(&tab) {
            return None;
        }
        let mut c = connect(opts, &tab).ok()?;
        if !autopilot_on(&mut c, &tab) {
            return None;
        }
        return restore_line(opts, r, st, key, &records, &mut c, &tab, &mut tab_probe).ok();
    };
    if let Some(why) = quiet {
        return Some(restore_lost(opts, r, st, key, why));
    }
    let wait = |st: &mut St, word: &str| {
        if late {
            restore_lost(opts, r, st, key, "no idle point came to say so at")
        } else {
            said(r.clone(), word)
        }
    };
    let Ok(mut c) = connect(opts, tab) else {
        return Some(wait(st, "wait:no-socket"));
    };
    Some(
        match restore_line(opts, r, st, key, &records, &mut c, tab, &mut tab_probe) {
            Ok(r) => r,
            Err(word) => wait(st, word),
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the look's record and its tab's connection, as restore_look holds them"
)]
fn restore_line(
    opts: &Opts,
    r: &Report,
    st: &mut St,
    key: &str,
    records: &Records,
    c: &mut Client,
    tab: &str,
    tab_probe: &mut impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<Report, &'static str> {
    let idle = continuation_session(records, st.resumed_pid, key).is_some_and(|sf| {
        sf.status == "idle"
            && kernel_start(sf.pid).as_deref() == Some(squash(&sf.proc_start).as_str())
    });
    if !idle {
        return Err("wait:changed");
    }
    if !tab_probe(c, st.resumed_pid, tab) {
        return Err("wait:tab-ownership-changed");
    }
    if held(c, tab, opts.human_grace_s) {
        return Err("wait:held");
    }
    let text = upgrade::restore_prompt(Version::parse(&st.resumed_on).as_ref(), st.is_upgrade());
    match under_hand(c, tab, None, |c| {
        type_continuation(c, tab, &text, opts.timings.fence_every)
    }) {
        None => Err("wait:held"),
        Some(false) => Ok(restore_lost(
            opts,
            r,
            st,
            key,
            "its line could not be typed",
        )),
        Some(true) => {
            st.confirm_by = 0;
            st.look_word.clear();
            st.outcome = format!(
                "{} · {RESTORE_LOST}; told it to restore what it relies on",
                st.outcome
            );
            let r = said(r.clone(), "continued:restore");
            ledger(opts, &r, &st.outcome);
            save(opts, key, st);
            Ok(r)
        }
    }
}

fn look_warned(records: &Records, key: &str, st: &St) -> bool {
    let until = st.done_at.saturating_add(RESTORE_LOOK.as_secs());
    records.transcript(key).is_some_and(|path| {
        let rows = if st.mark == 0 {
            tail_to_end(&path, TAIL_BYTES).0
        } else {
            since(&path, st.mark, SINCE_BYTES)
        };
        upgrade::resumed_build_warned(&rows, &st.resumed_on, st.relaunched_at, until)
    })
}

fn restore_lost(opts: &Opts, r: &Report, st: &mut St, key: &str, why: &str) -> Report {
    st.confirm_by = 0;
    st.look_word.clear();
    st.outcome = format!("{} · {RESTORE_LOST}; nothing typed: {why}", st.outcome);
    let r = said(r.clone(), "done:lost");
    ledger(opts, &r, &st.outcome);
    save(opts, key, st);
    r
}

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

fn resumed_model(records: &Records, session: &str, mark: u64, version: &str) -> Option<String> {
    if mark == 0 {
        return None;
    }
    let path = records.transcript(session)?;
    upgrade::transcript_first_model(&since(&path, mark, SINCE_BYTES), version)
}

/// The record of process `pid` in `records` while it holds conversation `key`.
pub(super) fn continuation_session(records: &Records, pid: u32, key: &str) -> Option<SessionFile> {
    records.session_file(pid).filter(|sf| sf.session_id == key)
}

/// The [`St::cause`] of a relaunch [`after_exit`] files.
pub(super) const CAUSE_EXIT: &str = "exit";

/// The [`St::cause`] of a relaunch that starts the agent afresh.
pub(super) const CAUSE_FRESH: &str = "exit-fresh";

/// The [`St::cause`] of the upgrade's restart of a conversation nobody has asked anything ([`super::upgrade::Step::Fresh`]).
pub(super) const CAUSE_UPGRADE_FRESH: &str = "upgrade-fresh";

/// Whether a record of `cause` starts its agent afresh.
pub(super) fn starts_afresh(cause: &str) -> bool {
    cause == CAUSE_FRESH || cause == CAUSE_UPGRADE_FRESH
}

/// The [`St::cause`] of a restart the host made for Claude Code's critical-memory banner ([`restart_here`], [`Restart::Memory`]).
pub(super) const CAUSE_MEMORY: &str = "memory";

/// The [`St::cause`] of a relaunch after the stall's remedy ended an agent that had stopped reading its input ([`after_exit`] with [`super::restart_stamp::Cause::Stall`]).
pub(super) const CAUSE_STALL: &str = "stall";

/// The [`St::cause`] of a relaunch after the explicit restart ended an agent that was not stalled (`aterm ctl restart`; [`after_exit`] with [`super::restart_stamp::Cause::Asked`]).
pub(super) const CAUSE_ASKED: &str = "asked";

/// The [`St::cause`] of a relaunch after aterm itself ended while the agent ran and the next launch reopened its tab ([`after_host_ended`]).
pub(super) const CAUSE_HOST: &str = "host";

/// The [`St::cause`] of a restart onto a bucket's fallback model, `model:` then the model ([`Restart::Model`]).
pub(super) const CAUSE_MODEL: &str = "model:";

/// The [`St::cause`] of a restart back from a bucket's fallback at its reset, `model-back:` then the model, empty for the launch's own ([`Restart::ModelBack`]).
pub(super) const CAUSE_MODEL_BACK: &str = "model-back:";

/// The [`St::cause`] of a restart under another of the owner's accounts, `account:` then the label ([`Restart::Account`]) and nothing else of the account.
pub(super) const CAUSE_ACCOUNT: &str = "account:";

/// What an account move answers where a restart of another kind was in flight for the conversation ([`restart_as`], [`carried_word`]).
pub const IN_FLIGHT_OTHER: &str = "wait:restart-in-flight";

/// The word [`restart_as`] answers after carrying the conversation's in-flight restart (cause `cause`, the carry's word `step`) instead of making the restart `why`.
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

/// What a restart in place ([`restart_from`]) asks for and records, by `why`.
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

/// [`model_restart`]'s answer.
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

/// Whether a restart moves the session onto a model billed to usage credits.
#[must_use]
pub(super) fn moves_onto_credits(
    why: &Restart,
    model: Option<&str>,
    billed: Option<&[String]>,
) -> bool {
    matches!(why, Restart::Model { .. } | Restart::ModelBack { .. })
        && model.is_some_and(|m| {
            billed.is_none_or(|billed| super::upgrade_models::billed_to_credits(m, billed))
        })
}

/// The fallback origin on `st` ([`St::fallback_from`]), or for an older fallback record, the launch model it kept.
pub(super) fn fallback_origin(st: &St) -> Option<String> {
    st.fallback_from.clone().or_else(|| {
        st.cause
            .starts_with(CAUSE_MODEL)
            .then(|| st.launch_model.clone())
    })
}

/// The fallback origin a restart for another reason records.
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

/// Whether a record of `cause` is a relaunch's (its continuation says why, [`resumed_prompt`]; an account move's is none, [`carried_quietly`]) rather than the upgrade's.
pub(super) fn relaunch_cause(cause: &str) -> bool {
    cause == CAUSE_EXIT
        || cause == CAUSE_MEMORY
        || cause == CAUSE_STALL
        || cause == CAUSE_ASKED
        || cause == CAUSE_HOST
        || cause.starts_with(CAUSE_MODEL)
        || cause.starts_with(CAUSE_MODEL_BACK)
        || cause.starts_with(CAUSE_ACCOUNT)
}

/// Whether a record of `cause` was filed by a restart that ended its agent itself.
pub(super) fn ends_its_agent(cause: &str) -> bool {
    cause.is_empty()
        || cause == CAUSE_UPGRADE_FRESH
        || cause == CAUSE_MEMORY
        || cause.starts_with(CAUSE_MODEL)
        || cause.starts_with(CAUSE_MODEL_BACK)
        || cause.starts_with(CAUSE_ACCOUNT)
}

/// The continuation typed to a relaunched agent once it holds its conversation.
#[must_use]
pub fn resumed_prompt(version: &str, cause: &str) -> String {
    resumed_prompt_as("Claude Code", version, cause)
}

/// [`resumed_prompt`] for the agent `agent` names.
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
    } else if cause == CAUSE_ASKED {
        format!(
            "Restarted: someone asked aterm to restart {agent} (`aterm ctl restart`), so this \
             session was ended and restarted"
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
        "{} {why} on {agent} {} and resumed. {} {}",
        upgrade::HARNESS_MARK,
        one_line(version),
        upgrade::RELAUNCH_RESTORES,
        upgrade::CARRY_ON
    )
}

/// The restart in place.
#[must_use]
pub fn restart_here(opts: &Opts, why: &Restart) -> Report {
    restart_from(opts, why, snapshot(opts))
}

/// [`restart_here`] for a host that keeps the owner's roster of Claude Code accounts.
#[must_use]
pub fn restart_under(opts: &Opts, why: &Restart, roster: &accounts::Roster) -> Report {
    restart_as(opts, why, roster, snapshot(opts), &Live)
}

/// Whether the process `snap` was read from runs under the account the roster labels `label`.
#[must_use]
pub fn runs_under(snap: &Snapshot, roster: &accounts::Roster, label: &str) -> bool {
    roster
        .row_for_login(snap.login.as_deref())
        .is_some_and(|row| row.label == label)
}

/// The login a relaunch with no process to read (a cold restore) carries.
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

/// How a session of a pooled CLI other than Claude Code is relaunched ([`pool_line`]).
#[derive(Debug, Clone, Copy)]
pub struct PoolRelaunch<'a> {
    /// The shell that reads the line.
    pub dialect: Dialect,
    /// The folder the line starts in, where it names one.
    pub cd: Option<&'a str>,
    /// The program's executable.
    pub exe: &'a Path,
    /// The launch flags it carries over. A Codex line keeps only those
    /// [`super::codex_alab::kept_flags`] keeps ([`pool_line`]).
    pub flags: &'a [String],
    /// The session it resumes, where one is known.
    pub resume: Option<&'a str>,
    /// The CLI's own home the program runs in ([`crate::ai::facts::home_of`]: Grok Build's
    /// `GROK_HOME`, else `~/.grok`), where a row of its own names its leader socket
    /// ([`pool_args`]); `None` where it was not read, which a Grok Build line onto a row of its
    /// own refuses.
    pub home: Option<&'a Path>,
}

/// The variables of [`crate::ai::connector::Connector::credential_env`] a pooled line leaves in
/// place: none is a login of the CLI's own carried in place of a row's, and removing one would
/// take it from what reads it. None was measured with a login (no login was read); each rests on
/// the CLI's own text or what the variable is.
const KEPT_CREDENTIALS: &[(accounts::Cli, &str)] = &[
    // codex-old 0.164.0's text names `OPENAI_API_KEY` as what its API-key sign-in screen offers
    // ("Detected OPENAI_API_KEY environment variable. Paste a different key if you prefer to use
    // another account.") and what `codex login --with-api-key` reads on stdin (`printenv
    // OPENAI_API_KEY | codex login --with-api-key`, its `login --help`); none of it names the
    // variable as read ahead of `auth.json`, where a piped key is stored (its `OPENAI_API_KEY`
    // field) in the home a row's `CODEX_HOME` names. It is no Codex variable: tools the agent
    // runs read it (Codex's bundled image skill's CLI fallback "Requires `OPENAI_API_KEY`"), and
    // aterm passes it to the shells it opens, so removing it would take it from those tools.
    (accounts::Cli::Codex, "OPENAI_API_KEY"),
    // grok 1.0.70's own "Auth Precedence": a per-model `api_key` or `env_key`, then the session
    // token in `auth.json`, then `XAI_API_KEY`, the "fallback when no session token is active".
    // A row's login is read ahead of it (only a model whose own `env_key` names it, the user's
    // config, reads it first), and it is xAI's API key ("API key from console.x.ai", grok's own
    // table), not a variable of Grok Build's.
    (accounts::Cli::Grok, "XAI_API_KEY"),
    // Claude Code's provider switches pick Bedrock or Vertex and carry no login; removing one
    // would move a session off the provider its person chose.
    (accounts::Cli::Claude, "CLAUDE_CODE_USE_BEDROCK"),
    (accounts::Cli::Claude, "CLAUDE_CODE_USE_VERTEX"),
];

/// The credential variables a pooled launch or relaunch of `cli` removes, so the login a row's
/// seam names is the one used: its [`crate::ai::connector::Connector::credential_env`] but for
/// [`KEPT_CREDENTIALS`]. Grok Build reads a `GROK_AUTH` login ahead of `GROK_AUTH_PATH`'s file
/// (grok 1.0.70: `GROK_AUTH set but failed to parse as JSON, falling back to file`), and
/// codex-old 0.164.0 names `CODEX_API_KEY` and `CODEX_ACCESS_TOKEN`, both its own, as
/// credentials in place of a login (``run `codex login` or set CODEX_API_KEY``; `Use Agent
/// Identity auth from CODEX_ACCESS_TOKEN`). Kimi Code 2.3.1 runs on an environment model in
/// place of its home's login and provider while `KIMI_MODEL_NAME` and `KIMI_MODEL_API_KEY` are
/// set, and sends a `KIMI_CODE_CUSTOM_HEADERS` `Authorization` line in place of the provider's
/// bearer (both measured, `docs/measured/kimi-credentials-2026-10-24.md`); the name is the
/// switch, since a name with no key fails the run and a key with no name is never read, so both
/// go.
#[must_use]
pub fn ahead_env(cli: accounts::Cli) -> Vec<&'static str> {
    cli.connector()
        .credential_env
        .iter()
        .copied()
        .filter(|name| !KEPT_CREDENTIALS.contains(&(cli, *name)))
        .collect()
}

/// Grok Build's flag naming the leader socket its client attaches to (`grok --help`).
const LEADER_SOCKET_FLAG: &str = "--leader-socket";

/// The variable Grok Build reads its leader socket from beside [`LEADER_SOCKET_FLAG`], which its
/// client hands a leader it starts (grok 1.0.70's strings).
const LEADER_SOCKET_ENV: &str = "GROK_LEADER_SOCKET";

/// What Grok Build unified's shared daemon adds to the leader's socket path for its own
/// (`PATH.daemon`, `grok leader list --help`).
const LEADER_DAEMON_SUFFIX: &str = ".daemon";

/// The longest socket path `sockaddr_un` holds on this platform, its NUL aside.
const MAX_SUN_PATH: usize = if cfg!(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
)) {
    103
} else {
    107
};

/// The leader socket Grok Build's sessions on the row `label`, signed in at the auth file `login`,
/// share in the grok home `home`: `leader-<label>-<8 hex>.sock`, the hex the top half of the
/// file's FNV-1a/64, so a renewal that binds the row to a fresh file names a fresh leader. That
/// is the shape `grok leader list --help` says `grok leader list/kill` still find
/// (`~/.grok/leader-*.sock`). Where that path and its daemon's (`PATH.daemon`) do not fit
/// `sockaddr_un`, the name is the FNV-1a/64 of the label, a NUL and the file in hex instead
/// (`leader-<16 hex>.sock`).
///
/// # Errors
///
/// `home` is relative, or even the hashed name does not fit.
pub fn grok_leader_socket(home: &Path, label: &str, login: &str) -> Result<PathBuf, String> {
    if !home.has_root() {
        return Err(format!(
            "the grok home {} is relative, so no leader socket is named in it",
            home.display()
        ));
    }
    let fits = |path: &Path| {
        path.to_str()
            .is_some_and(|p| p.len() + LEADER_DAEMON_SUFFIX.len() <= MAX_SUN_PATH)
    };
    let tag = aterm_hash::fnv1a_64(login.as_bytes()) >> 32;
    let named = home.join(format!("leader-{label}-{tag:08x}.sock"));
    if fits(&named) {
        return Ok(named);
    }
    let both = aterm_hash::fnv1a_64_extend(aterm_hash::fnv1a_64(label.as_bytes()), b"\0");
    let both = aterm_hash::fnv1a_64_extend(both, login.as_bytes());
    let hashed = home.join(format!("leader-{both:016x}.sock"));
    if fits(&hashed) {
        return Ok(hashed);
    }
    Err(format!(
        "{} and its daemon's socket do not fit sockaddr_un ({MAX_SUN_PATH} bytes)",
        hashed.display()
    ))
}

/// Whether the carried leader socket `path` is a pool row's ([`grok_leader_socket`]): named
/// `leader-*.sock`, in the grok home `home` where that was read.
fn names_row_socket(path: &str, home: Option<&Path>) -> bool {
    let path = Path::new(path);
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("leader-") && name.ends_with(".sock"))
        && home.is_none_or(|home| path.parent() == Some(home))
}

/// What a launch of a pooled CLI's program under one row carries beside the row's login
/// variable ([`pool_args`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolArgs {
    /// The variables it removes from the program's environment: the CLI's credential variables
    /// carried in place of a stored login ([`ahead_env`]), and for Grok Build on a row of its
    /// own `GROK_LEADER_SOCKET`, so the row's own socket flag is the one read. The default row
    /// keeps `GROK_LEADER_SOCKET`, a person's own choice whose value the line does not read.
    pub unset: Vec<&'static str>,
    /// Its flags: the carried ones, but for a Grok Build `--leader-socket` that may name the
    /// leader of the login the session ran on. A row of its own drops every carried one and adds
    /// its own ([`grok_leader_socket`]); the default row drops one that names a row's socket and
    /// keeps a person's own.
    pub flags: Vec<String>,
}

/// [`PoolArgs`] for a launch of `cli` under `env` with the carried `flags`, its leader socket
/// named in the CLI's home `home` ([`PoolRelaunch::home`]). Grok Build's leader (off by default;
/// on with `[cli] use_leader = true` or `--leader`) is shared by every client that attaches to
/// its socket. A client on another row attached to the old socket could stay on the leader's
/// login (unmeasured: no leader was started with a login), so each row of its own gets a socket
/// of its own, and the machine's default row adds none (its leader is `grok`'s default one).
///
/// # Errors
///
/// A Grok Build row of its own whose leader socket cannot be named: no `home`, or
/// [`grok_leader_socket`]'s refusal.
pub fn pool_args(
    cli: accounts::Cli,
    env: &super::accounts_pool::LoginEnv,
    home: Option<&Path>,
    flags: &[String],
) -> Result<PoolArgs, String> {
    let mut unset = ahead_env(cli);
    if cli != accounts::Cli::Grok {
        return Ok(PoolArgs {
            unset,
            flags: flags.to_vec(),
        });
    }
    let drops = |path: &str| env.value.is_some() || names_row_socket(path, home);
    let mut kept = Vec::with_capacity(flags.len() + 2);
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        if flag == LEADER_SOCKET_FLAG {
            if let Some(path) = it.next()
                && !drops(path)
            {
                kept.push(flag.clone());
                kept.push(path.clone());
            }
        } else if let Some(path) = flag
            .strip_prefix(LEADER_SOCKET_FLAG)
            .and_then(|rest| rest.strip_prefix('='))
        {
            if !drops(path) {
                kept.push(flag.clone());
            }
        } else {
            kept.push(flag.clone());
        }
    }
    if let Some(login) = env.value.as_deref() {
        let home = home.ok_or_else(|| {
            format!(
                "Grok Build's home was not read, so account {}'s leader socket cannot be named",
                env.label
            )
        })?;
        let socket = grok_leader_socket(home, &env.label, login)?;
        kept.push(LEADER_SOCKET_FLAG.to_string());
        kept.push(socket.to_string_lossy().into_owned());
        unset.push(LEADER_SOCKET_ENV);
    }
    Ok(PoolArgs { unset, flags: kept })
}

/// The argv that resumes session `id` of `cli`'s program, as each CLI's `--help` spells it.
#[must_use]
pub fn resume_argv(cli: accounts::Cli, id: &str) -> [String; 2] {
    let word = match cli {
        accounts::Cli::Codex => "resume",
        accounts::Cli::Claude | accounts::Cli::Grok => "--resume",
        accounts::Cli::Kimi => "--session",
    };
    [word.to_string(), id.to_string()]
}

/// The line that relaunches `cli`'s program as `how` says, under the login `env` names
/// ([`super::accounts_pool::login_env`]): that CLI's variable exported with the row's value, or
/// unset for the machine's default login, beside [`pool_args`]' removals and flags. A Codex
/// line is `resume`, the flags, then its thread: the order codex-alab's own relaunch types
/// ([`super::codex_alab::resume_words`]), whose new TUI the codex-alab driver recognises by the
/// thread being its last word. codex-old 0.164.0's `codex resume --help` usage begins
/// `codex resume [OPTIONS] [SESSION_ID]`. Its flags are those [`super::codex_alab::kept_flags`]
/// keeps, which drops `-i`/`--image` (a value list ahead of the thread would read it as a file)
/// and every word that is not a flag. Every other CLI's resume pair follows the flags. Nothing
/// types a pooled Codex line yet: [`super::accounts_pool::session_move`] is its one caller, and
/// the host does not take that advice.
///
/// # Errors
///
/// [`pool_args`]' refusal, a carried Codex word [`super::codex_alab::kept_flags`] refuses (an
/// unknown flag, or a flag or subcommand not resumable in place), or why the dialect cannot
/// spell the line.
pub fn pool_line(
    how: &PoolRelaunch<'_>,
    cli: accounts::Cli,
    env: &super::accounts_pool::LoginEnv,
) -> Result<String, String> {
    let PoolArgs { mut unset, flags } = pool_args(cli, env, how.home, how.flags)?;
    let args: Vec<String> = match cli {
        accounts::Cli::Codex => {
            let argv: Vec<String> = std::iter::once(String::new()).chain(flags).collect();
            let kept = super::codex_alab::kept_flags(&argv, |_, _| true)
                .map_err(|why| format!("Codex's carried flags cannot resume a thread: {why}"))?;
            super::codex_alab::resume_words(kept, how.resume)
        }
        accounts::Cli::Claude | accounts::Cli::Grok | accounts::Cli::Kimi => {
            let resume = how.resume.map(|id| resume_argv(cli, id));
            flags
                .into_iter()
                .chain(resume.into_iter().flatten())
                .collect()
        }
    };
    let set: Vec<(&str, &str)> = match env.value.as_deref() {
        Some(value) => vec![(env.var, value)],
        None => {
            unset.insert(0, env.var);
            Vec::new()
        }
    };
    if unset.is_empty() {
        upgrade::relaunch_line_env(how.dialect, None, how.cd, &set, how.exe, &args)
    } else {
        upgrade::relaunch_line_env_clear(how.dialect, None, how.cd, &set, &unset, how.exe, &args)
    }
}

/// [`restart_here`] from `snap`, the tab's agent as [`snapshot`] read it (or why it could not).
pub(super) fn restart_from(
    opts: &Opts,
    why: &Restart,
    snap: Result<Snapshot, &'static str>,
) -> Report {
    restart_with(opts, why, snap, &Live)
}

/// [`restart_from`] with process facts from `k` ([`Kernel`]), the seam a test scripts the agent's job and terminal through.
pub(super) fn restart_with(
    opts: &Opts,
    why: &Restart,
    snap: Result<Snapshot, &'static str>,
    k: &dyn Kernel,
) -> Report {
    restart_as(opts, why, &accounts::Roster::default(), snap, k)
}

/// The login an account move's relaunch line carries.
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
    if snap.shell == NO_SHELL {
        return said(r, "refused:not-a-shell-job");
    }
    let records = Records::of_snapshot(&opts.home, &snap);
    let Some(sf) = records
        .session_file(snap.pid)
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
    let (riding, hand) = restart_models(
        opts,
        &records,
        Some(&sf),
        &snap.start,
        &session,
        &snap.argv,
        matches!(why, Restart::Memory | Restart::Account { .. }),
        snap.profile.as_ref(),
    );
    let model_list = riding.clone().unwrap_or_default();
    let launched = launch_kept(opts, &session, &snap.argv).unwrap_or_default();
    let riding_move = riding.is_some();
    let ModelRestart {
        model,
        cause,
        launch_model,
        fallback_from,
    } = model_restart(why, prior.as_ref(), launched, riding, hand);
    if moves_onto_credits(
        why,
        model.as_deref(),
        credits_billed_for(opts, snap.profile.as_ref()).as_deref(),
    ) {
        return said(r, "refused:model-on-credits");
    }
    let mode = resume_mode(&records, &session, tab_mode(opts, &tab));
    let login = target.or_else(|| snap.login.clone());
    let launch = Launch {
        session: &session,
        resume: records.transcript_exists(&session) != Some(false),
        cwd: &sf.cwd,
        agent: snap.pid,
        argv: &snap.argv,
        exe: &snap.program,
        model: model.as_deref(),
        mode: mode.as_deref(),
        home: snap.home.as_deref(),
        homedir: None,
        login: login.as_deref(),
        stored_login: snap.stored_login,
        billing: if riding_move || matches!(why, Restart::Model { .. } | Restart::ModelBack { .. })
        {
            ModelBilling::Move(snap.profile.as_ref())
        } else {
            ModelBilling::Keep
        },
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
    st.keep_home(&records, &opts.home);
    if let Some(prior) = prior.as_ref() {
        prior.kept_by_restart(&tab, &mut st);
    }
    let planned =
        home_carried(&opts.home, snap.home.as_deref(), &sf.cwd, &records).and_then(|homedir| {
            let launch = Launch {
                homedir: homedir.as_deref(),
                ..launch
            };
            plan(opts, &launch, snap.shell, &t)
        });
    let Plan { shell, line, .. } = match planned {
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
    let Some(mut hand) = Hand::take(&mut c, &tab) else {
        return said(r, "wait:held");
    };
    let aborted: Option<Report> = 'signal: {
        let again = records.session_file(snap.pid);
        let still = again.is_some_and(|a| {
            a.session_id == session
                && a.status == "idle"
                && kernel_start(snap.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        }) && foreground_shell(k.job(snap.pid)) == Ok(shell)
            && k.terminal(snap.pid)
                .as_ref()
                .is_some_and(|(p, name)| owned_by_aterm((*p, name.as_str())))
            && screen(&mut c, &tab).is_some_and(|scr| at_rest_for(why, &mut c, &tab, &scr))
            && live_background(snap.pid).is_empty()
            && !held(&mut c, &tab, opts.human_grace_s);
        if !still {
            break 'signal Some(said(r.clone(), "wait:changed"));
        }
        if let Err(why) = records.unique_owner(&sf) {
            break 'signal Some(said(r.clone(), format!("wait:{why}")));
        }
        let Ok(pid) = i32::try_from(snap.pid) else {
            break 'signal Some(said(r.clone(), "wait:pid"));
        };
        let recent = records
            .transcript(&session)
            .map(|p| tail_to_end(&p, TAIL_BYTES));
        (st.model_before, st.mark) = model_and_mark(recent.as_ref());
        st.pid = snap.pid;
        st.shell = shell;
        st.tab.clone_from(&tab);
        st.line = line;
        st.phase = upgrade::Phase::Exiting { at_s: now_s() };
        super::upgrade_drive::note_moved_writes(opts, &snap, &session, &sf.cwd);
        save(opts, &session, &st);
        match terminate(&mut c, &tab, pid, opts.human_grace_s) {
            Terminated::Sent => None,
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

/// Whether the tab's screen lets [`restart_as`] signal its agent: the composer empty, or, for an
/// account move, a sign-in menu drawn in the composer's place
/// ([`TurnEndReading::sign_in_menu_drawn`], read as Claude Code's, the one agent whose accounts
/// move), where nothing typed stands to lose and the login the move leaves is the menu's own.
fn at_rest_for(why: &Restart, c: &mut Client, tab: &str, scr: &Screen) -> bool {
    composer_empty(c, tab, scr)
        || (matches!(why, Restart::Account { .. })
            && TurnEndReading::sign_in_menu_drawn(
                Some(aterm_phase::Program::Claude.name()),
                &scr.rows,
                scr.cursor.map(|(_, col)| col),
            ))
}

/// What a relaunch needs, read while the agent runs (argv, parent and start time go with the process).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The tab (`s-<hex>`).
    pub tab: String,
    /// The agent: the tab's foreground job.
    pub pid: u32,
    /// Its kernel start, as Claude renders `procStart`.
    pub start: String,
    /// The job-control shell it is the foreground job of.
    pub shell: u32,
    /// What it runs ([`launched`]).
    pub program: PathBuf,
    /// Its argv with `program` as `argv[0]`.
    pub argv: Vec<String>,
    /// Its conversation, when Claude had registered it.
    pub session: Option<String>,
    /// The directory it was started in.
    pub cwd: String,
    /// Its version, when Claude had registered it (a Codex's: its package's).
    pub version: Option<String>,
    /// The agent it is, with what only that agent's relaunch reads ([`AgentRun`]).
    pub run: AgentRun,
    /// The dialect of `shell` from its executable ([`shell_dialect`]; a Codex's as its lane's plan reads it).
    pub dialect: Option<Dialect>,
    /// The login the process runs under, as its own exec environment names it.
    pub login: Option<String>,
    /// The Claude home it runs under.
    pub home: Option<String>,
    /// Where its records are.
    pub claude_dir: Option<PathBuf>,
    /// Whether it authenticates by its stored login, by its own exec environment ([`accounts::ProcessLogin::of_exec_env`]'s `stored_login`).
    pub stored_login: bool,
    /// The exact profile paths and filename selector captured from this process's own exec environment and resolved records.
    pub profile: Option<super::profile::Context>,
}

impl Snapshot {
    /// The agent it is of, by its run ([`Snapshot::run`]).
    #[must_use]
    pub const fn agent(&self) -> upgrade::Agent {
        self.run.agent()
    }

    /// A Codex's run, `None` for any other agent's.
    #[must_use]
    pub const fn codex(&self) -> Option<&CodexRun> {
        match &self.run {
            AgentRun::Codex(run) => Some(run),
            AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => None,
        }
    }

    /// [`Snapshot::codex`], to change.
    pub const fn codex_mut(&mut self) -> Option<&mut CodexRun> {
        match &mut self.run {
            AgentRun::Codex(run) => Some(run),
            AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => None,
        }
    }

    /// A Muse Code's run, `None` for any other agent's.
    #[must_use]
    pub const fn muse(&self) -> Option<&MuseRun> {
        match &self.run {
            AgentRun::Muse(run) => Some(run),
            AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => None,
        }
    }

    /// An agy's run, `None` for any other agent's.
    #[must_use]
    pub const fn agy(&self) -> Option<&AgyRun> {
        match &self.run {
            AgentRun::Agy(run) => Some(run),
            AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Gemini(_) => None,
        }
    }

    /// A Gemini CLI's run, `None` for any other agent's.
    #[must_use]
    pub const fn gemini(&self) -> Option<&GeminiRun> {
        match &self.run {
            AgentRun::Gemini(run) => Some(run),
            AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) => None,
        }
    }
}

/// The agent a [`Snapshot`] is of, with the run only that agent's relaunch reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRun {
    /// Claude Code: its run is the snapshot's own fields (`login`, `home`, `claude_dir`, `stored_login`).
    Claude,
    /// A Codex TUI ([`CodexRun`]).
    Codex(CodexRun),
    /// A Muse Code TUI ([`MuseRun`]).
    Muse(MuseRun),
    /// An agy TUI ([`AgyRun`]).
    Agy(AgyRun),
    /// A Gemini CLI TUI ([`GeminiRun`]).
    Gemini(GeminiRun),
}

impl AgentRun {
    /// The agent whose run it is.
    #[must_use]
    pub const fn agent(&self) -> upgrade::Agent {
        match self {
            AgentRun::Claude => upgrade::Agent::Claude,
            AgentRun::Codex(_) => upgrade::Agent::Codex,
            AgentRun::Muse(_) => upgrade::Agent::Muse,
            AgentRun::Agy(_) => upgrade::Agent::Agy,
            AgentRun::Gemini(_) => upgrade::Agent::Gemini,
        }
    }
}

/// What a Muse Code TUI's relaunch on exit needs beside the [`Snapshot`]. The snapshot's `session`
/// is the one whose `.session.lock` its process holds open with its pid in the body (`None`: none
/// proven, and no relaunch then resumes anything).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuseRun {
    /// The data home its sessions are under (canonical).
    pub data: PathBuf,
    /// The homes its relaunch line carries, as its own exec environment set them, verbatim (`XDG_DATA_HOME`, `XDG_CONFIG_HOME`; [`super::upgrade_muse::line_env`]).
    pub env: Vec<(String, String)>,
    /// The session's workspace as its own log last named it ([`super::upgrade_muse::log_workspace`]).
    pub workspace: Option<String>,
    /// The session's `.session.lock` the witness read.
    pub lock: Option<PathBuf>,
    /// When it started, unix milliseconds (to the second; `0`: unread).
    pub started_ms: u64,
    /// The shell-integration command block it runs in (its `id`, read while it runs), or `None` where none could be read.
    pub block: Option<u64>,
}

/// What an agy TUI's relaunch on exit needs beside the [`Snapshot`]. The snapshot's `session` is
/// the conversation whose presence lock (`<config>/presence/<uuid>.lock`) its process holds open
/// (`None`: none proven, as for a fresh agy that took no turn, and no relaunch then resumes
/// anything), and its `cwd` is the workspace, agy's launch directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgyRun {
    /// The config directory its conversations are under (canonical).
    pub config: PathBuf,
    /// The home its relaunch line carries (`HOME=<dir>`, where its own exec environment set another than the user's; [`super::upgrade_agy::relaunch_env`]).
    pub env: Vec<(String, String)>,
    /// The conversation's presence lock the witness read.
    pub lock: Option<PathBuf>,
    /// The crash file it holds open (`<config>/crashes/crash_<pid>_<uuid>.log`, read with its open files).
    pub crash: Option<PathBuf>,
    /// When it started, unix milliseconds (to the second; `0`: unread).
    pub started_ms: u64,
    /// The shell-integration command block it runs in (its `id`, read while it runs), or `None` where none could be read.
    pub block: Option<u64>,
}

/// What a Gemini CLI TUI's relaunch on exit needs beside the [`Snapshot`]. The snapshot's `pid` is
/// the parent node (the tab's foreground job, `node <gemini script>`), its `session` the one
/// session header of its project begun since it started (the start-time rule; `None`: none, and
/// no relaunch then resumes anything), and its `cwd` the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeminiRun {
    /// The gemini directory its sessions are under (canonical).
    pub home: PathBuf,
    /// Its project's chats directory (`<home>/tmp/<slug>/chats`, the slug whose `.project_root` names its project).
    pub chats: Option<PathBuf>,
    /// The homes its relaunch line carries (`GEMINI_CLI_HOME` where set, `HOME` where another than the user's; [`super::upgrade_gemini::relaunch_env`]).
    pub env: Vec<(String, String)>,
    /// When the parent started, unix milliseconds, never later than it did (`0`: unread).
    pub started_ms: u64,
    /// The child node it started (`0`: none found).
    pub child: u32,
    /// Why the start-time rule named no session, where it named none ([`super::upgrade_gemini::NoWitness`]).
    pub unwitnessed: Option<super::upgrade_gemini::NoWitness>,
    /// The shell-integration command block it runs in (its `id`, read while it runs), or `None` where none could be read.
    pub block: Option<u64>,
}

/// What a codex-alab's relaunch needs beside the [`Snapshot`] (whose `session` is its thread, when nameable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexRun {
    /// The `$CODEX_HOME` it runs with (canonical).
    pub home: PathBuf,
    /// The `CODEX_HOME` its own exec environment set, verbatim.
    pub home_env: Option<String>,
    /// The crash rule's validated line.
    pub self_claimed: Option<super::self_claimed::Resume>,
}

/// Whether the host's relaunch would bring back what the explicit restart of `snap`'s agent ends (`ExitCause::Asked`, `ExitCause::Stall`), as far as a live read can tell.
#[must_use]
pub fn carries_restart(user_home: &Path, snap: &Snapshot) -> bool {
    match &snap.run {
        AgentRun::Claude => resumes_on_exit(user_home, snap),
        AgentRun::Codex(_) => snap.shell != NO_SHELL && self_claimed_line(snap).is_some(),
        AgentRun::Muse(_) => false,
        AgentRun::Agy(_) => {
            snap.shell != NO_SHELL && super::upgrade_drive::agy_carries_restart(user_home, snap)
        }
        AgentRun::Gemini(_) => false,
    }
}

/// Whether an exit of `snap`'s agent now would be relaunched on its own conversation ([`after_exit`]), as far as a live read can tell.
#[must_use]
pub fn resumes_on_exit(user_home: &Path, snap: &Snapshot) -> bool {
    exit_line(user_home, snap).is_some()
}

/// The resume line `snap` alone implies for a relaunch of its agent on its own conversation ([`after_exit`], [`after_host_ended`]).
#[must_use]
pub fn exit_line(user_home: &Path, snap: &Snapshot) -> Option<String> {
    if snap.shell == NO_SHELL {
        return None;
    }
    match &snap.run {
        AgentRun::Claude => {}
        AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => {
            return None;
        }
    }
    let dialect = snap.dialect?;
    let session = snap
        .session
        .as_deref()
        .filter(|s| upgrade::is_session_id(s))?;
    if upgrade::one_shot(&snap.argv) || snap.cwd.chars().any(char::is_control) {
        return None;
    }
    home_carried(
        user_home,
        snap.home.as_deref(),
        &snap.cwd,
        &Records::of_snapshot(user_home, snap),
    )
    .ok()?;
    line_for(
        dialect,
        None,
        None,
        &snap.program,
        &snap.argv,
        Some(session),
        None,
        None,
        &required_env(snap.home.as_deref(), None, snap.login.as_deref()),
    )
    .ok()
}

/// The line a person is told to run to resume the agent `snap` describes by hand, for a user whose home is `user_home`.
#[must_use]
pub fn told_command(user_home: &Path, state: Option<&Path>, snap: &Snapshot) -> Option<String> {
    match &snap.run {
        AgentRun::Claude => {}
        AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => {
            return None;
        }
    }
    let records = Records::of_snapshot(user_home, snap);
    let homedir = home_carried(user_home, snap.home.as_deref(), &snap.cwd, &records).ok()?;
    let line =
        super::upgrade_drive::resume_command_kept(state, &snap.argv, snap.session.as_deref()?)?;
    let env: Vec<(&str, &str)> = snap
        .home
        .as_deref()
        .map(|home| (accounts::HOME_ENV, home))
        .into_iter()
        .chain(
            homedir
                .as_deref()
                .map(|homedir| (accounts::HOMEDIR_ENV, homedir)),
        )
        .collect();
    super::resume::with_env(&env, &line)
}

/// The program a Claude Code process runs and its args.
#[must_use]
pub fn launched(
    argv: &[String],
    image: Option<&Path>,
    native: &Path,
    registered: bool,
) -> Option<(PathBuf, Vec<String>)> {
    let is_claude = |p: &Path| match p
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(aterm_phase::program_of)
    {
        Some(aterm_phase::Program::Claude) => true,
        Some(
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic,
        )
        | None => false,
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

/// Read the [`Snapshot`] of the Claude Code (or codex-alab, Muse Code, agy or Gemini CLI) in tab `opts.only_sid` on `opts.sock`.
pub fn snapshot(opts: &Opts) -> Result<Snapshot, &'static str> {
    snapshot_with(opts, &Live)
}

/// Read the foreground agent with its host-proven live primary copy, when present.
/// The ordinary job, start-time, tab and terminal checks still precede its admission.
///
/// # Errors
/// Returns the same refusal codes as [`snapshot`].
pub fn snapshot_with_claim(
    opts: &Opts,
    claim: Option<&super::self_claimed::Copy>,
) -> Result<Snapshot, &'static str> {
    snapshot_with_claimed(opts, &Live, claim)
}

/// The lane that reads a tab's foreground agent ([`snapshot_with`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SnapshotLane {
    /// Claude Code's records.
    Claude,
    /// Codex-alab's own path.
    CodexAlab,
    /// The Muse Code lane's.
    Muse,
    /// The agy lane's.
    Agy,
    /// The Gemini CLI lane's.
    Gemini,
    /// No lane relaunches it.
    None,
}

/// The lane a foreground process is read by: a Gemini CLI by its `node` image and script, any
/// other by the basename of its mapped image (`image`; `None`: unreadable), a Codex's lane only
/// for an ALab build.
pub(super) fn snapshot_lane(image: Option<&Path>, argv: &[String]) -> SnapshotLane {
    if super::upgrade_gemini::is_gemini_process(image, argv) {
        return SnapshotLane::Gemini;
    }
    match image
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .and_then(aterm_phase::program_of)
    {
        Some(aterm_phase::Program::Codex) if image.is_some_and(super::codex_alab::alab_bytes) => {
            SnapshotLane::CodexAlab
        }
        Some(aterm_phase::Program::Muse) => SnapshotLane::Muse,
        Some(aterm_phase::Program::Agy) => SnapshotLane::Agy,
        Some(
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Gemini,
        ) => SnapshotLane::None,
        Some(aterm_phase::Program::Claude | aterm_phase::Program::Generic) | None => {
            SnapshotLane::Claude
        }
    }
}

/// The snapshot of a crashed codex-alab no read reached while it ran.
pub fn self_claimed_snapshot(
    opts: &Opts,
    copy: &super::self_claimed::Copy,
    resume: &super::self_claimed::Resume,
) -> Result<Snapshot, &'static str> {
    self_claimed_snapshot_with(opts, &Live, copy, resume)
}

pub(super) fn self_claimed_snapshot_with(
    opts: &Opts,
    k: &dyn Kernel,
    copy: &super::self_claimed::Copy,
    resume: &super::self_claimed::Resume,
) -> Result<Snapshot, &'static str> {
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
    let shell = u32::try_from(group).map_err(|_| "no-foreground")?;
    if shell == copy.pid || k.shell_holds_tab(shell) != Some(true) {
        return Err("no-shell");
    }
    if !k
        .terminal(shell)
        .as_ref()
        .is_some_and(|(p, name)| owned_by_aterm((*p, name.as_str())))
    {
        return Err("terminal");
    }
    let args = atpkg::caller_shell::process_args(shell).ok_or("argv-unreadable")?;
    if args
        .env_var("ATERM_PARENT_SESSION_ID")
        .is_some_and(|env_tab| env_tab != tab)
    {
        return Err("tab-identity-conflict");
    }
    Ok(snapshot_of_copy(
        tab,
        copy,
        resume,
        shell,
        shell_dialect(shell, &[]),
    ))
}

/// The snapshot [`self_claimed_snapshot`] makes (pure).
#[must_use]
pub fn snapshot_of_copy(
    tab: &str,
    copy: &super::self_claimed::Copy,
    resume: &super::self_claimed::Resume,
    shell: u32,
    dialect: Option<Dialect>,
) -> Snapshot {
    Snapshot {
        tab: tab.to_string(),
        pid: copy.pid,
        start: copy.start.clone(),
        shell,
        program: PathBuf::from(&resume.argv[0]),
        argv: resume.argv.clone(),
        session: Some(resume.session.clone()),
        cwd: resume.cwd.clone().unwrap_or_default(),
        version: None,
        run: AgentRun::Codex(CodexRun {
            home: copy.home.clone(),
            home_env: resume.codex_home.clone(),
            self_claimed: Some(resume.clone()),
        }),
        dialect,
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    }
}

/// [`snapshot`] with the agent's job and terminal read from `k` ([`Kernel`]).
pub(super) fn snapshot_with(opts: &Opts, k: &dyn Kernel) -> Result<Snapshot, &'static str> {
    snapshot_with_claimed(opts, k, None)
}

fn snapshot_with_claimed(
    opts: &Opts,
    k: &dyn Kernel,
    claim: Option<&super::self_claimed::Copy>,
) -> Result<Snapshot, &'static str> {
    let tab = opts.only_sid.as_deref().ok_or("no-tab")?;
    let mut c = connect(opts, tab).map_err(|_| "no-socket")?;
    super::laps::lap("connect");
    let tabs = host_roster(&mut c).ok_or("roster")?;
    super::laps::lap("roster");
    let group = tabs
        .iter()
        .find(|t| t.sid == tab)
        .and_then(|t| t.fgpgid)
        .ok_or("no-foreground")?;
    if unique_tab_for_group(&tabs, group) != Some(tab) {
        return Err("tab-ambiguous");
    }
    let pid = u32::try_from(group).map_err(|_| "no-foreground")?;
    let shell = match k.job(pid) {
        Some((Job::NoJobControl, _)) => return Err("not-a-shell-job"),
        Some((Job::TabProgram { .. }, _)) => NO_SHELL,
        job => foreground_shell(job)?,
    };
    if shell != NO_SHELL
        && !k
            .terminal(pid)
            .as_ref()
            .is_some_and(|(p, name)| owned_by_aterm((*p, name.as_str())))
    {
        return Err("terminal");
    }
    super::laps::lap("job");
    let args = atpkg::caller_shell::process_args(pid).ok_or("argv-unreadable")?;
    super::laps::lap("args");
    if args
        .env_var("ATERM_PARENT_SESSION_ID")
        .is_some_and(|env_tab| env_tab != tab)
    {
        return Err("tab-identity-conflict");
    }
    let start = kernel_start(pid).ok_or("start")?;
    let image = exe_of(pid);
    super::laps::lap("process");
    match snapshot_lane(image.as_deref(), &args.argv) {
        SnapshotLane::CodexAlab => {
            return super::upgrade_drive::codex_alab_snapshot_with_claim(
                opts, tab, pid, start, shell, &args, claim,
            );
        }
        SnapshotLane::Muse => {
            return super::upgrade_drive::muse_snapshot(opts, tab, pid, start, shell, &args);
        }
        SnapshotLane::Agy => {
            return super::upgrade_drive::agy_snapshot(opts, tab, pid, start, shell, &args);
        }
        SnapshotLane::Gemini => {
            return super::upgrade_drive::gemini_snapshot(opts, tab, pid, start, shell, &args);
        }
        SnapshotLane::None => return Err(NO_LANE),
        SnapshotLane::Claude => {}
    }
    let records = Records::of_process(
        &opts.home,
        super::upgrade_drive::HomeVars::of(&args),
        pid,
        || aterm_sysprobe::process_cwd(pid),
        args.env_var("PWD"),
    );
    let file = records
        .session_file(pid)
        .filter(|sf| squash(&sf.proc_start) == start);
    super::laps::lap("claude.records");
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
        run: AgentRun::Claude,
        dialect: (shell != NO_SHELL)
            .then(|| shell_dialect(shell, &[]))
            .flatten(),
        login: args.env_var(accounts::LOGIN_ENV).map(str::to_owned),
        home: args.env_var(accounts::HOME_ENV).map(str::to_owned),
        claude_dir: records.dir().map(Path::to_path_buf),
        stored_login: accounts::ProcessLogin::of_exec_env(&args.env)
            .is_some_and(|who| who.stored_login),
        profile: records.billing_context(&opts.home, &args),
    })
}

/// Where the tab's foreground job stands against a [`Snapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Foreground {
    /// The snapshot's agent still holds the tab.
    Agent,
    /// The shell holds the tab again.
    Shell,
    /// Another job holds the tab (a person ran another agent, a wrapper, a script).
    Other,
}

/// [`Foreground`] of `snap` when the tab's program is published for process group `fg`.
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

/// Follow the snapshot's agent while it runs.
pub fn follow(home: &Path, snap: &mut Snapshot) -> bool {
    match &snap.run {
        AgentRun::Claude => {}
        AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => {
            return false;
        }
    }
    Records::of_snapshot(home, snap)
        .session_file(snap.pid)
        .is_some_and(|sf| follow_record(snap, &sf))
}

/// Follow `snap` through one record of its own process, as [`follow`] does through the one on disk.
pub fn follow_record(snap: &mut Snapshot, sf: &SessionFile) -> bool {
    if !matches!(snap.run, AgentRun::Claude)
        || sf.pid != snap.pid
        || squash(&sf.proc_start) != snap.start
    {
        return false;
    }
    let before = snap.clone();
    if upgrade::is_session_id(&sf.session_id) {
        snap.session = Some(sf.session_id.clone());
    }
    if !sf.version.is_empty() {
        snap.version = Some(sf.version.clone());
    }
    if !sf.cwd.is_empty() {
        snap.cwd.clone_from(&sf.cwd);
    }
    *snap != before
}

/// What a relaunch on exit runs, `(exe, version, source)`.
pub(super) fn relaunch_target(
    upgrade: bool,
    running: Option<&Version>,
    candidates: &[upgrade::Candidate],
    held: impl Fn(&upgrade::Candidate) -> bool,
    program: &Path,
    version: Option<&str>,
) -> (PathBuf, String, String) {
    let target = running
        .filter(|_| upgrade)
        .and_then(|v| upgrade::choose_target(v, candidates))
        .filter(|t| !held(t));
    match target {
        Some(t) => (t.exe, t.version.to_string(), t.source.as_str().to_string()),
        None => (
            program.to_path_buf(),
            version.unwrap_or("-").to_string(),
            SAME_PROGRAM.to_string(),
        ),
    }
}

/// What the exit left of Claude's record of the agent (`sessions/<pid>.json`, same pid and kernel start).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitRecord {
    /// Nothing was read at the exit.
    Unread,
    /// The record survived the exit.
    Survived(Box<SessionFile>),
    /// The exit removed it: graceful, someone's decision (or no conversation was ever registered).
    Removed,
    /// The agent crashed, by its shell's exit status.
    Crashed,
    /// The agent refused its own start, by its shell's exit status.
    Refused,
    /// A Gemini CLI's own restart, refused for its own session ID, read at the exit from its
    /// shell's word ([`ExitLook::restarted_itself`]).
    Restarted,
}

impl ExitRecord {
    /// The exit was a crash, read at the exit.
    #[must_use]
    pub const fn crashed(&self) -> bool {
        matches!(self, Self::Survived(_) | Self::Crashed | Self::Restarted)
    }

    /// One word for the host's log.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Survived(_) => "survived",
            Self::Removed => "removed",
            Self::Crashed => "crashed",
            Self::Refused => "refused",
            Self::Restarted => "restarted",
        }
    }
}

/// The exit is its launch's own end, by what it left as it was seen ([`exit_record`]).
#[must_use]
pub fn launch_ended(snap: &Snapshot, left: &ExitRecord) -> Option<&'static str> {
    let one_shot = match &snap.run {
        AgentRun::Claude | AgentRun::Codex(_) => upgrade::one_shot(&snap.argv),
        AgentRun::Muse(_) => !super::upgrade_muse::is_tui_argv(&snap.argv),
        AgentRun::Agy(_) => !super::upgrade_agy::is_tui_argv(&snap.argv),
        AgentRun::Gemini(_) => !super::upgrade_gemini::is_tui_argv(&snap.argv),
    };
    if one_shot {
        Some("one-shot")
    } else if *left == ExitRecord::Removed {
        Some("graceful-exit")
    } else if *left == ExitRecord::Refused {
        Some("refused-start")
    } else {
        None
    }
}

/// One look at an agent that left its tab ([`look_at_exit`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitLook {
    /// The snapshot's process still runs.
    pub running: bool,
    /// Its record, when one of its own start is on disk (Claude Code's).
    pub record: Option<SessionFile>,
    /// Whose look it is ([`Snapshot::agent`]).
    pub agent: upgrade::Agent,
    /// A Muse Code's or an agy's exit per its shell, by its own table ([`super::upgrade_muse::exit_kind`]; an agy's with its crash file, [`super::upgrade_agy::exit_kind`]).
    pub crashed: Option<bool>,
    /// A Muse Code's or an agy's shell says it refused its start ([`ExitRecord::Refused`]), with `crashed` `Some(false)`.
    pub refused: bool,
    /// A Gemini CLI's shell says it restarted itself ([`ExitRecord::Restarted`]), with `crashed`
    /// `Some(true)` (`upgrade_gemini::ExitKind::Restarted`).
    pub restarted_itself: bool,
}

/// How long after the first look an exit may still be removing its own record.
pub const EXIT_SETTLE: Duration = Duration::from_millis(250);

/// How often [`exit_record`] looks.
pub const EXIT_LOOK: Duration = Duration::from_millis(25);

/// How long a process still running after its exit was seen (stopped, or unreaped) is looked at before the read is left to the attempt ([`ExitRecord::Unread`]).
pub const EXIT_GONE: Duration = Duration::from_secs(2);

/// [`ExitLook`] of `snap`'s agent, live.
#[must_use]
pub fn look_at_exit(home: &Path, snap: &Snapshot) -> ExitLook {
    let running = alive(snap.pid) && kernel_start(snap.pid).as_deref() == Some(snap.start.as_str());
    let record = Records::of_snapshot(home, snap)
        .session_file(snap.pid)
        .filter(|sf| squash(&sf.proc_start) == snap.start);
    ExitLook {
        running,
        record,
        agent: upgrade::Agent::Claude,
        crashed: None,
        refused: false,
        restarted_itself: false,
    }
}

/// Follow a Muse Code's snapshot between the loop's idle points.
pub fn follow_muse(snap: &mut Snapshot) -> bool {
    let muse = match &snap.run {
        AgentRun::Muse(_) => true,
        AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => false,
    };
    muse && super::upgrade_drive::muse_follow(snap)
}

/// [`ExitLook`] of a Muse Code `snap` ([`MuseRun`]), live.
#[must_use]
pub fn look_at_muse_exit(opts: &Opts, snap: &Snapshot) -> ExitLook {
    super::upgrade_drive::muse_look_at_exit(opts, snap)
}

/// [`ExitLook`] of an agy `snap` ([`AgyRun`]), live.
#[must_use]
pub fn look_at_agy_exit(opts: &Opts, snap: &Snapshot) -> ExitLook {
    super::upgrade_drive::agy_look_at_exit(opts, snap)
}

/// [`ExitLook`] of a Gemini CLI `snap` ([`GeminiRun`]), live.
#[must_use]
pub fn look_at_gemini_exit(opts: &Opts, snap: &Snapshot) -> ExitLook {
    super::upgrade_drive::gemini_look_at_exit(opts, snap)
}

/// Whether the exit is [`ExitRecord::Restarted`], read as the exit is seen, before deciding whose
/// exit it is.
pub fn restarted_itself(
    mut look: impl FnMut() -> Option<ExitLook>,
    mut wait: impl FnMut(Duration) -> bool,
) -> bool {
    let mut waited = Duration::ZERO;
    loop {
        match look() {
            None => return false,
            Some(now) if !now.running && now.crashed.is_some() => {
                return now.crashed == Some(true) && now.restarted_itself;
            }
            Some(_) => {}
        }
        if waited >= EXIT_GONE || !wait(EXIT_LOOK) {
            return false;
        }
        waited += EXIT_LOOK;
    }
}

/// Read what the exit left, once, as it is seen, independent of the back-off.
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
            match now.agent {
                upgrade::Agent::Muse | upgrade::Agent::Agy | upgrade::Agent::Gemini => {
                    match now.crashed {
                        Some(true) if now.restarted_itself => return ExitRecord::Restarted,
                        Some(true) => return ExitRecord::Crashed,
                        Some(false) if now.refused => return ExitRecord::Refused,
                        Some(false) => return ExitRecord::Removed,
                        None if waited >= EXIT_GONE => return ExitRecord::Unread,
                        None => {}
                    }
                }
                upgrade::Agent::Claude => match now.record {
                    None => return ExitRecord::Removed,
                    Some(sf) if waited >= EXIT_SETTLE => {
                        return ExitRecord::Survived(Box::new(sf));
                    }
                    Some(_) => {}
                },
                upgrade::Agent::Codex => {
                    return ExitRecord::Unread;
                }
            }
        }
        if !wait(EXIT_LOOK) {
            return ExitRecord::Unread;
        }
        waited += EXIT_LOOK;
    }
}

/// Relaunch on exit: `snap`'s agent left its tab and [`on_exit`] found nobody owns the exit.
#[must_use]
pub fn after_exit(
    opts: &Opts,
    snap: &Snapshot,
    left: &ExitRecord,
    upgrade: bool,
    asked: Option<super::restart_stamp::Cause>,
) -> Report {
    let cause = match asked {
        Some(super::restart_stamp::Cause::Stall) => ExitCause::Stall,
        Some(super::restart_stamp::Cause::Asked) => ExitCause::Asked,
        None => ExitCause::Exit,
    };
    after_exit_as(opts, snap, left, upgrade, cause, None)
}

/// Consume the explicit restart's stamp once the relaunch of the exit it named has filed its record (under the caller's sweep lock).
pub(super) fn consume_stamp(opts: &Opts, snap: &Snapshot, cause: ExitCause) {
    if opts.dry_run || !matches!(cause, ExitCause::Stall | ExitCause::Asked) {
        return;
    }
    let _ =
        super::restart_stamp::take_if(&opts.state, &snap.tab, |s| s.names(snap.pid, &snap.start));
}

/// Why an agent that is no longer running is relaunched ([`after_exit_as`]; a codex-alab's, its copy's relaunch, [`super::upgrade_drive::self_claimed_relaunch`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExitCause {
    /// Its own exit: relaunched only when it left Claude's record (a crash), never after a graceful one.
    Exit,
    /// The stall's remedy ended it.
    Stall,
    /// The explicit restart ended it (`aterm ctl restart`, its stamp naming the process).
    Asked,
    /// Aterm itself ended while it ran.
    HostEnded,
}

/// The agent of a tab a crash of aterm took.
#[must_use]
pub fn after_host_ended(opts: &Opts, snap: &Snapshot, upgrade: bool) -> Report {
    after_host_ended_from(opts, snap, upgrade, None)
}

/// [`after_host_ended`] of an agent whose layout names the tab it ran in, `was`.
#[must_use]
pub fn after_host_ended_from(
    opts: &Opts,
    snap: &Snapshot,
    upgrade: bool,
    was: Option<&str>,
) -> Report {
    after_exit_as(
        opts,
        snap,
        &ExitRecord::Unread,
        upgrade,
        ExitCause::HostEnded,
        was,
    )
}

/// The line the crash rule validated for `snap`'s codex-alab ([`CodexRun::self_claimed`]), when its snapshot carries one.
#[must_use]
pub const fn self_claimed_line(snap: &Snapshot) -> Option<&super::self_claimed::Resume> {
    match &snap.run {
        AgentRun::Codex(run) => run.self_claimed.as_ref(),
        AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => None,
    }
}

/// Whether the current validated copy still names this exact published root and line.
#[must_use]
pub fn same_self_claim(snap: &Snapshot, copy: &super::self_claimed::Copy) -> bool {
    copy.pid == snap.pid
        && copy.start == snap.start
        && copy.resume.as_ref().is_ok_and(|resume| {
            snap.session.as_deref() == Some(resume.session.as_str())
                && self_claimed_line(snap) == Some(resume)
        })
}

fn after_exit_as(
    opts: &Opts,
    snap: &Snapshot,
    left: &ExitRecord,
    upgrade: bool,
    cause: ExitCause,
    was: Option<&str>,
) -> Report {
    match &snap.run {
        AgentRun::Codex(_) => {
            if cause != ExitCause::Exit
                && let Some(resume) = self_claimed_line(snap)
            {
                return super::upgrade_drive::self_claimed_relaunch(opts, snap, resume, cause);
            }
            let r = Report {
                pid: snap.pid,
                tab: snap.tab.clone(),
                session: snap.session.clone().unwrap_or_else(|| "-".to_string()),
                from: snap.version.clone().unwrap_or_else(|| "-".to_string()),
                to: "-".to_string(),
                step: String::new(),
            };
            return said(r, "ended:no-lane");
        }
        AgentRun::Muse(_) => {
            return super::upgrade_drive::muse_after_exit(opts, snap, left, upgrade, cause);
        }
        AgentRun::Agy(_) => {
            return super::upgrade_drive::agy_after_exit(opts, snap, left, upgrade, cause);
        }
        AgentRun::Gemini(_) => {
            return super::upgrade_drive::gemini_after_exit(opts, snap, left, upgrade, cause);
        }
        AgentRun::Claude => {}
    }
    let mut r = Report {
        pid: snap.pid,
        tab: snap.tab.clone(),
        session: snap.session.clone().unwrap_or_else(|| "-".to_string()),
        from: snap.version.clone().unwrap_or_else(|| "-".to_string()),
        to: "-".to_string(),
        step: String::new(),
    };
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    if alive(snap.pid) && kernel_start(snap.pid).as_deref() == Some(snap.start.as_str()) {
        return said(r, "wait:not-exited");
    }
    if upgrade::one_shot(&snap.argv) {
        return said(r, "ended:one-shot");
    }
    let records = Records::of_snapshot(&opts.home, snap);
    let survivor = match left {
        ExitRecord::Survived(sf) => Some(SessionFile::clone(sf)),
        ExitRecord::Removed => None,
        ExitRecord::Unread | ExitRecord::Crashed | ExitRecord::Refused | ExitRecord::Restarted => {
            records
                .session_file(snap.pid)
                .filter(|sf| squash(&sf.proc_start) == snap.start)
        }
    };
    let Some(session) = survivor
        .as_ref()
        .map(|sf| sf.session_id.clone())
        .or_else(|| snap.session.clone())
        .filter(|s| upgrade::is_session_id(s))
    else {
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
    if cause != ExitCause::HostEnded
        && let Some(st) = load(opts, &session)
            .filter(|st| ended_by_restart(st, upgrade::Agent::Claude, &snap.tab, snap.pid, now_s()))
    {
        r.to = format!("{}({})", st.to, st.source);
        return carry_own(opts, r, st, &session, &snap.tab);
    }
    let mut relaunched_before = false;
    if let Some(mut st) = load(opts, &session).filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != snap.tab && cause != ExitCause::HostEnded {
            return said(r, "wait:conversation-in-other-tab");
        }
        let overtaken = if cause == ExitCause::HostEnded && st.tab != snap.tab {
            Some((
                "host-ended",
                "aterm ended while this relaunch was in flight; its tab was reopened".to_string(),
            ))
        } else {
            (st.pid != snap.pid || st.relaunched_gone(alive)).then(|| match st.phase {
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
        if !overtook && hand_waits_out(opts, &mut st, &session, alive) {
            return said(r, WAIT_HAND_LIFTS);
        }
        if let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s())) {
            relaunched_before = st.relaunched_on_exit_of(snap.pid);
            if !opts.dry_run {
                if overtook {
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
    let read_before = relaunched_before && matches!(left, ExitRecord::Unread);
    if survivor.is_none() && cause == ExitCause::Exit && !read_before {
        return said(r, "ended:graceful-exit");
    }
    if !opts.dry_run {
        super::upgrade_drive::note_moved_writes(opts, snap, &session, &cwd);
    }
    if snap.shell == NO_SHELL {
        return said(r, "refused:not-a-shell-job");
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
    let prior = load(opts, &session);
    let now = now_s();
    let held = |t: &upgrade::Candidate| {
        prior
            .as_ref()
            .is_some_and(|st| st.holds_off(&snap.tab, t, now))
            || super::upgrade_drive::build_proved(opts, &r, t, now).is_err()
    };
    let (exe, to, source) = relaunch_target(
        upgrade,
        running.as_ref(),
        &candidates,
        held,
        &snap.program,
        version.as_deref(),
    );
    r.to = format!("{to}({source})");
    let resume = records.transcript_exists(&session) != Some(false);
    let (riding, hand) = if resume {
        restart_models(
            opts,
            &records,
            survivor.as_ref(),
            &snap.start,
            &session,
            &snap.argv,
            true,
            snap.profile.as_ref(),
        )
    } else {
        (None, None)
    };
    let mode = resume_mode(&records, &session, tab_mode(opts, &snap.tab));
    let launch = Launch {
        session: &session,
        resume,
        cwd: &cwd,
        agent: snap.pid,
        argv: &snap.argv,
        exe: &exe,
        model: riding.as_deref().or(hand.as_deref()),
        mode: mode.as_deref(),
        home: snap.home.as_deref(),
        homedir: None,
        login: snap.login.as_deref(),
        stored_login: snap.stored_login,
        billing: if riding.is_some() {
            ModelBilling::Move(snap.profile.as_ref())
        } else {
            ModelBilling::Keep
        },
    };
    let mut st = St {
        from: version.unwrap_or_default(),
        to,
        source,
        cause: match (resume, cause) {
            (false, _) => CAUSE_FRESH,
            (true, ExitCause::Stall) => CAUSE_STALL,
            (true, ExitCause::Asked) => CAUSE_ASKED,
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
    st.keep_home(&records, &opts.home);
    if let Some(prior) = prior.as_ref() {
        if st.source == SAME_PROGRAM {
            prior.kept_by_restart(&snap.tab, &mut st);
        } else {
            prior.retried_by(&mut st);
        }
    }
    let was = was.map(str::to_owned);
    st.tabs = earlier_tabs(
        &snap.tab,
        prior
            .iter()
            .flat_map(|p| p.tabs.iter().chain(std::iter::once(&p.tab)))
            .chain(was.as_ref()),
    );
    let left_now = now_s();
    st.tabs_until = prior
        .iter()
        .flat_map(|p| p.tabs_until.iter().cloned())
        .chain(
            was.iter()
                .chain(prior.iter().map(|p| &p.tab))
                .filter(|t| !t.is_empty() && **t != snap.tab)
                .map(|t| (t.clone(), left_now)),
        )
        .filter(|(t, _)| st.tabs.contains(t))
        .collect();
    if resume {
        let recent = records
            .transcript(&session)
            .map(|p| tail_to_end(&p, TAIL_BYTES));
        (st.model_before, st.mark) = model_and_mark(recent.as_ref());
    }
    let planned =
        home_carried(&opts.home, snap.home.as_deref(), &cwd, &records).and_then(|homedir| {
            let launch = Launch {
                homedir: homedir.as_deref(),
                ..launch
            };
            plan(opts, &launch, snap.shell, &table())
        });
    let Plan { shell, line, .. } = match planned {
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
    consume_stamp(opts, snap, cause);
    ledger(opts, &said(r.clone(), "exited"), &st.line);
    let r = relaunch(opts, r, &mut st, &mut c, &session, &Live);
    save(opts, &session, &st);
    r
}

/// The fallback origin an exit's relaunch of `session` records ([`fallback_carried`] of the one its last record keeps).
pub(super) fn exit_fallback(
    opts: &Opts,
    session: &str,
    riding: Option<&str>,
    hand: Option<&str>,
) -> Option<String> {
    let origin = load(opts, session).as_ref().and_then(fallback_origin);
    fallback_carried(origin, riding, hand)
}

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

/// How long after its agent was seen gone ([`St::exited_at`]) a failed or landed restart's record
/// still owns that exit.
pub(super) const STOPPED_OWN_S: u64 = 2 * STALE_S;

fn restart_of(st: &St, agent: upgrade::Agent, tab: &str, pid: u32) -> bool {
    st.agent == agent && st.tab == tab && st.pid == pid && ends_its_agent(&st.cause)
}

/// Whether `st` records a harness restart that ended agent `pid` (of `agent`'s lane) that left tab `tab` at `now` ([`restart_of`]) and is still that exit's.
pub(super) fn ended_by_restart(
    st: &St,
    agent: upgrade::Agent,
    tab: &str,
    pid: u32,
    now: u64,
) -> bool {
    restart_of(st, agent, tab, pid)
        && match agent {
            upgrade::Agent::Claude | upgrade::Agent::Muse | upgrade::Agent::Agy => {
                st.in_flight()
                    || (matches!(st.phase, upgrade::Phase::Failed(_))
                        && st.exited_at != 0
                        && now.saturating_sub(exit_clock(0, st.exited_at, st.hand_seen_s))
                            <= STOPPED_OWN_S)
            }
            upgrade::Agent::Codex => false,
            upgrade::Agent::Gemini => false,
        }
}

fn restart_record(opts: &Opts, agent: upgrade::Agent, pid: Option<u32>) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let pid = pid?;
    let now = now_s();
    record_where(opts, |st| ended_by_restart(st, agent, tab, pid, now))
}

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

fn landed(opts: &Opts, pid: u32) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let now = now_s();
    let (session, st) = record_where(opts, |st| {
        restart_of(st, upgrade::Agent::Claude, tab, pid)
            && st.phase == upgrade::Phase::Done
            && st.exited_at != 0
            && now.saturating_sub(exit_clock(0, st.exited_at, st.hand_seen_s)) <= STOPPED_OWN_S
    })?;
    let others: Vec<SessionFile> = st
        .agent_records(&opts.home)
        .session_files()?
        .into_iter()
        .filter(|sf| sf.pid != pid)
        .collect();
    conversation_live(&others, &session).then_some((session, st))
}

fn quiet_landed(opts: &Opts, agent: upgrade::Agent, pid: u32) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let now = now_s();
    record_where(opts, |st| {
        restart_of(st, agent, tab, pid)
            && st.phase == upgrade::Phase::Done
            && st.exited_at != 0
            && now.saturating_sub(exit_clock(0, st.exited_at, st.hand_seen_s)) <= STOPPED_OWN_S
            && st.resumed_pid != 0
            && alive(st.resumed_pid)
    })
}

fn adopted_holds(opts: &Opts, st: &St, session: &str) -> bool {
    matches!(st.phase, upgrade::Phase::Relaunched { .. })
        && st.relaunched_pid != 0
        && continuation_session(&st.agent_records(&opts.home), st.relaunched_pid, session)
            .is_some_and(|sf| {
                sf.pid == st.relaunched_pid && conversation_live(std::slice::from_ref(&sf), session)
            })
}

/// Whether the harness's own restart (the upgrade's, or a restart in place) ended the agent that
/// left tab `opts.only_sid`, with its record still owning that exit.
#[must_use]
pub fn restarted(opts: &Opts, agent: upgrade::Agent, pid: Option<u32>) -> bool {
    restart_record(opts, agent, pid).is_some()
}

/// The label of the account move whose restart signalled the Claude Code `pid` of tab `opts.only_sid`.
#[must_use]
pub fn account_restart(opts: &Opts, pid: u32) -> Option<String> {
    restart_record(opts, upgrade::Agent::Claude, Some(pid))
        .and_then(|(_, st)| st.cause.strip_prefix(CAUSE_ACCOUNT).map(str::to_owned))
}

/// The restart [`restarted`] found, carried on for its agent's exit.
#[must_use]
pub fn carry_restart(
    opts: &Opts,
    agent: upgrade::Agent,
    pid: Option<u32>,
    through_hand: bool,
) -> Report {
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
    let Some((session, mut st)) = restart_record(opts, agent, pid) else {
        let found = pid.and_then(|pid| match agent {
            upgrade::Agent::Codex => None,
            upgrade::Agent::Claude => landed(opts, pid),
            upgrade::Agent::Muse | upgrade::Agent::Agy => quiet_landed(opts, agent, pid),
            upgrade::Agent::Gemini => None,
        });
        let Some((session, st)) = found else {
            return said(r, "refused:no-restart-in-flight");
        };
        let r = Report {
            session,
            from: st.from.clone(),
            to: format!("{}({})", st.to, st.source),
            ..r
        };
        return said(r, "landed");
    };
    let stamped = if through_hand {
        st.carried_through_hand(now_s())
    } else {
        st.seen_gone(now_s())
    };
    if carried_here(st.agent) && stamped && !opts.dry_run {
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

fn carry_own(opts: &Opts, r: Report, st: St, session: &str, tab: &str) -> Report {
    told_by_its_row(opts, carry_own_step(opts, r, st, session, tab), session)
}

fn told_by_its_row(opts: &Opts, r: Report, session: &str) -> Report {
    let Some(why) = r.step.strip_prefix("refused:") else {
        return r;
    };
    let raised = load(opts, session).is_some_and(|st| {
        st.is_upgrade() && st.exited_at != 0 && matches!(st.phase, upgrade::Phase::Failed(_))
    });
    if raised {
        let told = format!("told:{why}");
        said(r, told)
    } else {
        r
    }
}

fn carry_own_step(opts: &Opts, r: Report, mut st: St, session: &str, tab: &str) -> Report {
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
    if carried_here(st.agent) && adopted_holds(opts, &st, session) {
        let mut r = said(r, "adopted");
        r.pid = st.relaunched_pid;
        return r;
    }
    if carried_here(st.agent) && st.relaunched_gone(alive) {
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
    if hand_waits_out(opts, &mut st, session, alive) {
        return said(r, WAIT_HAND_LIFTS);
    }
    if carried_here(st.agent)
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
        upgrade::Agent::Muse => super::upgrade_drive::muse_carry_in_flight(opts, r, st, session),
        upgrade::Agent::Agy => super::upgrade_drive::agy_carry_in_flight(opts, r, st, session),
        upgrade::Agent::Gemini => {
            super::upgrade_drive::gemini_carry_in_flight(opts, r, st, session)
        }
    };
    match r.step.strip_prefix("failed:").map(str::to_string) {
        Some(why) => refused(r, &why),
        None => r,
    }
}

/// A restart's line under a hand is waited out, by every carrier of its record.
pub(super) fn hand_waits_out(
    opts: &Opts,
    st: &mut St,
    session: &str,
    alive: impl Fn(u32) -> bool,
) -> bool {
    if opts.dry_run
        || !matches!(st.phase, upgrade::Phase::Exiting { .. })
        || st.shell == 0
        || !alive(st.shell)
        || !hand_stands(opts, &st.tab)
    {
        return false;
    }
    let now = now_s();
    let changed = st.saw_hand(now) | (st.wait != "held");
    st.note_wait("held", now);
    if changed {
        save(opts, session, st);
    }
    true
}

fn hand_stands(opts: &Opts, tab: &str) -> bool {
    connect(opts, tab)
        .ok()
        .and_then(|mut c| super::upgrade_drive::status_line(&mut c, tab))
        .is_some_and(|s| s.starts_with("OK") && hand_on_tab(&s))
}

/// The limit row the agent `snap` names stands at, by its transcript's tail (`tail`, [`super::upgrade_drive::snapshot_transcript_tail`]).
#[must_use]
pub fn standing_limit(opts: &Opts, snap: &Snapshot, tail: &str) -> Option<upgrade::StandingLimit> {
    let limit = upgrade::transcript_standing_limit(tail)?;
    let moved = snap
        .session
        .as_deref()
        .is_some_and(|session| moved_off(opts, session, limit.at, snap));
    (!moved).then_some(limit)
}

/// Whether conversation `session` was moved off the limit row written at `at` (unix seconds).
#[must_use]
pub fn moved_off(opts: &Opts, session: &str, at: Option<u64>, live: &Snapshot) -> bool {
    let kept = |st: &St| {
        st.moved_at != 0
            && at.is_none_or(|at| at <= st.moved_at)
            && runs_moved(
                live,
                st.moved_login.as_deref(),
                st.moved_left_model.as_deref(),
            )
    };
    let record = load(opts, session).is_some_and(|st| {
        let (login, left) = own_move_target(&st);
        let own = moves(&st.cause)
            && at.is_none_or(|at| st.relaunched_at == 0 || at <= st.relaunched_at)
            && runs_moved(live, login.as_deref(), left.as_deref());
        own || kept(&st)
    });
    record || kept_move(opts, session).is_some_and(|st| kept(&st))
}

fn runs_moved(live: &Snapshot, login: Option<&str>, left: Option<&str>) -> bool {
    let runs = live.login.as_deref().or(live.home.as_deref()).unwrap_or("");
    login.is_none_or(|l| runs == l) && left.is_none_or(|m| launch_model_of(&live.argv) != m)
}

fn launch_model_of(argv: &[String]) -> String {
    upgrade::launch_model(argv).unwrap_or_default()
}

fn own_move_target(st: &St) -> (Option<String>, Option<String>) {
    if st.cause.starts_with(CAUSE_ACCOUNT) {
        (st.login.clone(), None)
    } else if st.cause.starts_with(CAUSE_MODEL) {
        (None, fallback_origin(st))
    } else {
        (None, None)
    }
}

/// The target of the last move record `st` knows of ([`last_move_at`]).
pub(super) fn last_move_target(st: &St) -> (Option<String>, Option<String>) {
    if moves(&st.cause) && st.relaunched_at != 0 && st.relaunched_at >= st.moved_at {
        own_move_target(st)
    } else {
        (st.moved_login.clone(), st.moved_left_model.clone())
    }
}

fn moves(cause: &str) -> bool {
    cause.starts_with(CAUSE_MODEL)
        || cause.starts_with(CAUSE_MODEL_BACK)
        || cause.starts_with(CAUSE_ACCOUNT)
}

/// The last move record `st` knows of ([`super::upgrade_drive::St::moved_at`]), unix seconds, `0` for none.
pub(super) fn last_move_at(st: &St) -> u64 {
    if moves(&st.cause) && st.relaunched_at != 0 {
        st.relaunched_at.max(st.moved_at)
    } else {
        st.moved_at
    }
}

/// Whether the agent a relaunch of conversation `session` in tab `tab` adopted is gone.
#[must_use]
pub fn adopted_gone(opts: &Opts, tab: &str, session: &str) -> Option<bool> {
    let st = load(opts, session).filter(|st| st.in_flight() && st.tab == tab)?;
    (st.relaunched_pid != 0).then(|| st.relaunched_gone(alive))
}

/// Whether a relaunch of conversation `session` is in flight in tab `tab` ([`St::in_flight`]).
#[must_use]
pub fn in_flight_here(opts: &Opts, tab: &str, session: &str) -> bool {
    load(opts, session).is_some_and(|st| st.in_flight() && st.tab == tab)
}

const fn carried_here(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

/// The word of a step asked of a process no lane relaunches (a vendor `codex`, an image no lane reads; [`super::lanes::lanes`]).
pub const NO_LANE: &str = "no-lane";

/// The continuation owed to tab `opts.only_sid`.
#[must_use]
pub fn owed(opts: &Opts) -> bool {
    owed_of(opts, None)
}

/// [`owed`], of `only`'s record alone where it names an agent.
#[must_use]
pub fn owed_of(opts: &Opts, only: Option<upgrade::Agent>) -> bool {
    owed_record(opts, only).is_some()
}

fn owed_record(opts: &Opts, only: Option<upgrade::Agent>) -> Option<(String, St)> {
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
        let its = only.is_none_or(|agent| st.agent == agent);
        (st.tab == tab && due && its).then_some((session, st))
    })
}

/// Carry on the relaunched agent at its loop's idle point, where the loop parked for an [`owed`] continuation.
#[must_use]
pub fn resume(opts: &Opts) -> Report {
    resume_as(opts, false, None)
}

/// [`resume`] at a point the loop read as a task landed under land (`done_under_land`, `crate::supervise::IdlePoint::done_under_land`).
#[must_use]
pub fn resume_as(opts: &Opts, done_under_land: bool, only: Option<upgrade::Agent>) -> Report {
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
    let Some((session, mut st)) = owed_record(opts, only) else {
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
        if looks_back(&st) {
            return restore_look(
                opts,
                &r,
                &mut st,
                &session,
                now_s(),
                Some(&tab),
                |c, pid, tab| process_in_tab(c, pid, tab, None, None),
            )
            .unwrap_or_else(|| said(r, "wait:restore"));
        }
        return confirm(opts, &r, &mut st, &session, now_s())
            .unwrap_or_else(|| said(r, "wait:model"));
    }
    let carry = Opts {
        hand_back: done_under_land,
        background: false,
        ..opts.clone()
    };
    let r = match st.agent {
        upgrade::Agent::Codex => {
            super::upgrade_drive::codex_carry_in_flight(&carry, r, st, &session)
        }
        upgrade::Agent::Claude => carry_in_flight(&carry, r, st, &session, &tab),
        upgrade::Agent::Muse => super::upgrade_drive::muse_carry_in_flight(&carry, r, st, &session),
        upgrade::Agent::Agy => super::upgrade_drive::agy_carry_in_flight(&carry, r, st, &session),
        upgrade::Agent::Gemini => {
            super::upgrade_drive::gemini_carry_in_flight(&carry, r, st, &session)
        }
    };
    if done_under_land && r.step == "adopted" {
        return carried_done(opts, r, &session);
    }
    r
}

/// The word of a relaunch carried on with nothing because its task landed under Land ([`resume_as`]).
pub const CARRIED_DONE: &str = "done:said-done";

fn carried_done(opts: &Opts, r: Report, session: &str) -> Report {
    let Some(mut st) =
        load(opts, session).filter(|st| matches!(st.phase, upgrade::Phase::Relaunched { .. }))
    else {
        return r;
    };
    st.phase = upgrade::Phase::Done;
    st.done_at = now_s();
    let why = "nothing typed: under Land, its task landed";
    st.outcome = match st.agent {
        upgrade::Agent::Claude => {
            format!("claude restarted on {} · {why}", one_line(&st.to))
        }
        upgrade::Agent::Codex => format!(
            "codex on {} · the same conversation resumed; {why}",
            one_line(&st.to)
        ),
        upgrade::Agent::Muse => format!(
            "muse on {} · the same conversation resumed; {why}",
            one_line(&st.to)
        ),
        upgrade::Agent::Agy => format!(
            "agy on {} · the same conversation resumed; {why}",
            one_line(&st.to)
        ),
        upgrade::Agent::Gemini => format!(
            "gemini on {} · the same session resumed; {why}",
            one_line(&st.to)
        ),
    };
    let r = said(r, CARRIED_DONE);
    ledger(opts, &r, &st.outcome);
    save(opts, session, &st);
    r
}

/// The carry-on owed to tab `opts.only_sid` settled with nothing typed, off the host's idle point.
#[must_use]
pub fn settle(opts: &Opts) -> Report {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let r = Report {
        pid: 0,
        tab: tab.clone(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: String::new(),
    };
    if opts.dry_run {
        return said(r, "would-settle");
    }
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    let Some((session, mut st)) = owed_record(opts, None) else {
        return said(r, "current");
    };
    let mut r = Report {
        session: session.clone(),
        from: st.from.clone(),
        to: format!("{}({})", st.to, st.source),
        ..r
    };
    if !carried_here(st.agent) {
        return said(r, "wait:owed");
    }
    if st.confirming() {
        r.pid = st.resumed_pid;
        r.to = format!("{}({})", st.resumed_on, st.source);
        if !looks_back(&st) {
            return said(r, "wait:model");
        }
        return settle_look(opts, &r, &mut st, &session, now_s())
            .unwrap_or_else(|| said(r, "wait:restore"));
    }
    if !matches!(st.phase, upgrade::Phase::Relaunched { .. }) {
        return said(r, "wait:owed");
    }
    if st.relaunched_pid == 0 {
        return said(r, "wait:resume");
    }
    let records = st.agent_records(&opts.home);
    let Some(sf) = continuation_session(&records, st.relaunched_pid, &session)
        .filter(|sf| kernel_start(sf.pid).as_deref() == Some(squash(&sf.proc_start).as_str()))
    else {
        return said(r, "wait:changed");
    };
    r.pid = sf.pid;
    let r = if resumes_quietly(&st) {
        done_quietly(opts, r, &mut st, &session, &sf.version, sf.pid)
    } else {
        let ours = conversation_typed_by(opts, &tab, &session, &st);
        let written = WhereWritten::new(opts, &records, &session, &ours);
        let Some((word, why)) = nothing_owed(opts, &written, &tab, &st) else {
            return said(r, "wait:owed");
        };
        done_with_nothing(opts, r, &mut st, &session, word, why, &sf.version, sf.pid)
    };
    settle_look(opts, &r, &mut st, &session, now_s())
        .filter(|look| look.step.starts_with("done"))
        .unwrap_or(r)
}

/// Whether a [`settle`] that said `step` left something a later one can settle, typing nothing.
#[must_use]
pub fn settles_again(step: &str) -> bool {
    matches!(step, "busy:another-sweep" | "wait:resume" | "wait:restore")
        || (step.starts_with("done:") && step != "done:lost")
}

fn settle_look(opts: &Opts, r: &Report, st: &mut St, key: &str, now: u64) -> Option<Report> {
    let records = st.agent_records(&opts.home);
    if look_warned(&records, key, st) {
        return Some(match told_nothing(opts, &records, key, st) {
            Some(why) => restore_lost(opts, r, st, key, why),
            None => said(r.clone(), "wait:owed"),
        });
    }
    restore_look(opts, r, st, key, now, None, |_, _, _| false)
}

/// The host's word for an exit the explicit restart's stamp names whose process still runs.
pub const WAIT_ACT_ENDS: &str = "wait:not-exited:act-ends";

/// The word for an exit the harness made, carried through a hand that waits.
pub const WAIT_HAND_LIFTS: &str = "wait:held:act-carried";

/// What one relaunch step came to, for the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The new process holds the conversation.
    Relaunched,
    /// Nothing to relaunch or say, and why.
    Left(String),
    /// Not now, and why (a busy tab, a shell not back yet, a relaunch that did not register).
    NotYet(String),
    /// Another aterm actor holds the upgrade state.
    Busy,
    /// Never, and why: an interactive conversation that cannot come back (an uncarryable launch, a gone shell).
    Cannot(String),
}

/// [`Outcome`] from one [`after_exit`] step's word.
#[must_use]
pub fn outcome(step: &str) -> Outcome {
    // The host hands every relaunched agent to its loop ([`Opts::hand_back`]), so no other word
    // lands, and a carry-on that typed nothing (`done:no-continue`) is never one.
    if matches!(step, "adopted" | "done:fresh" | "landed" | "done:resumed") {
        Outcome::Relaunched
    } else if step == "wait:not-exited" {
        Outcome::Left("not-exited".to_string())
    } else if let Some(why) = step.strip_prefix("ended:") {
        Outcome::Left(why.to_string())
    } else if let Some(why) = step
        .strip_prefix("refused:")
        .or_else(|| step.strip_prefix("told:"))
    {
        Outcome::Cannot(why.to_string())
    } else if matches!(step, "busy:another-sweep" | WAIT_ACT_ENDS | WAIT_HAND_LIFTS) {
        Outcome::Busy
    } else if let Some(why) = step.strip_prefix("busy:") {
        // State directory cannot hold the lock.
        Outcome::Cannot(why.to_string())
    } else {
        Outcome::NotYet(step.to_string())
    }
}

/// What the host does when a supervised agent has left its tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnExit {
    /// A person typed into the session within `human_grace_s` of the exit.
    PersonAsked,
    /// The session is held: halted (`hold=1`), a drive lease, or a turn a named driver typed ([`held_by_someone`]).
    Held,
    /// Nobody owns the exit but `[harness] relaunch = false`.
    Limited,
    /// Nobody asked: relaunch it.
    Relaunch,
    /// The explicit restart ended it (its stamp names the process: a person's `restart`, the stall's remedy).
    Asked,
    /// The harness's own restart ended the agent and left its relaunch in flight ([`restarted`]).
    Restarted,
}

/// Decide whose exit this was.
#[must_use]
pub fn on_exit(
    allowed: bool,
    status: Option<&str>,
    grace_s: u32,
    asked: bool,
    restarted: bool,
) -> OnExit {
    if restarted {
        OnExit::Restarted
    } else if !asked && status.is_some_and(|s| person_present(s, grace_s)) {
        OnExit::PersonAsked
    } else if !asked && status.is_some_and(held_by_someone) {
        OnExit::Held
    } else if !allowed {
        OnExit::Limited
    } else if asked {
        OnExit::Asked
    } else {
        OnExit::Relaunch
    }
}

/// Whether one `status` reply says the session is held by someone the exit belongs to.
#[must_use]
pub fn held_by_someone(status: &str) -> bool {
    status.split_whitespace().any(|t| t == "hold=1") || driver_hand(status)
}

/// The hands the relaunch line waits on, from one `status` reply.
#[must_use]
pub fn hand_on_tab(status: &str) -> bool {
    status.split_whitespace().any(|t| {
        t == "hold=1"
            || t.strip_prefix("hand=")
                .is_some_and(|hand| hand != "-" && !super::upgrade_drive::our_hand(hand))
    })
}

/// Whether one `status` reply names another driver's hand.
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

/// Whether a person typed into the session within `grace_s` seconds, from one `status` reply ([`HumanInput::of_status`], [`HumanInput::within`]).
#[must_use]
pub fn person_present(status: &str, grace_s: u32) -> bool {
    HumanInput::of_status(status).within(grace_s) == Some(true)
}

/// The person's grace still to run by one `status` reply (`human_ms=`).
#[must_use]
pub fn grace_left(status: &str, grace_s: u32) -> Duration {
    HumanInput::of_status(status)
        .ago()
        .map_or(Duration::ZERO, |age| {
            Duration::from_secs(u64::from(grace_s)).saturating_sub(age)
        })
}

/// The pauses before consecutive relaunches of a crash.
pub const BACKOFF: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(10),
    Duration::from_secs(60),
    Duration::from_secs(600),
];

/// The look again after an [`Outcome::Busy`] step ([`WAIT_ACT_ENDS`], [`WAIT_HAND_LIFTS`], or another actor on the upgrade state).
pub const BUSY: Duration = Duration::from_secs(10);

/// An agent that ran this long since its last relaunch of a crash starts the back-off over.
pub const HEALTHY: Duration = Duration::from_secs(600);

/// The longest pause before the next attempt at a harness restart ([`OnExit::Restarted`]).
pub const CARRY_EVERY: Duration = Duration::from_secs(60);

/// Consecutive relaunch steps that did not relaunch ([`Outcome::NotYet`]) before the session's attention says so.
pub const BADGE_AFTER: u32 = 3;

/// What the session's attention says after a [`Relaunches`] step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Say {
    /// Nothing changes.
    Nothing,
    /// The attention this raised is cleared.
    Clear,
    /// The relaunch keeps failing ([`BADGE_AFTER`] in a row).
    Failing,
    /// The relaunch can never be made.
    Cannot,
    /// The relaunch is limited ([`OnExit::Limited`]).
    Limited,
}

/// One session's relaunch state as the host keeps it across workers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Relaunches {
    streak: u32,
    crash: bool,
    tries: u32,
    asks: Option<String>,
    failing: bool,
    failing_said: bool,
    misses: u32,
    last: Option<Instant>,
    pending: bool,
    left: bool,
    badged: bool,
    restarted: bool,
}

impl Relaunches {
    /// The agent left its tab at `now`, and [`on_exit`] said `decision`.
    pub fn exited(&mut self, decision: OnExit, now: Instant) -> Result<Duration, Say> {
        let crash = !matches!(decision, OnExit::Asked | OnExit::Restarted);
        self.exited_as(decision, crash, now)
    }

    /// [`Self::exited`] of an exit that is a `crash` or not.
    pub fn exited_as(
        &mut self,
        decision: OnExit,
        crash: bool,
        now: Instant,
    ) -> Result<Duration, Say> {
        self.forgive(now);
        self.crash = crash;
        self.tries = 0;
        self.failing = false;
        self.failing_said = false;
        match self.decided(decision) {
            None => Ok(self.pause()),
            Some(say) => Err(say),
        }
    }

    /// [`on_exit`] asked again during the pause (a person or holder may be back, or configuration changed).
    pub fn decide(&mut self, decision: OnExit) -> Option<Say> {
        if decision == OnExit::Relaunch && self.pending && !self.crash && !self.restarted {
            self.crash = true;
        }
        self.decided(decision)
    }

    fn decided(&mut self, decision: OnExit) -> Option<Say> {
        self.restarted = decision == OnExit::Restarted;
        match decision {
            OnExit::Relaunch | OnExit::Asked | OnExit::Restarted => {
                self.pending = true;
                self.left = false;
                None
            }
            OnExit::PersonAsked | OnExit::Held => {
                self.pending = false;
                self.left = true;
                Some(self.tried_no_more())
            }
            OnExit::Limited => {
                self.pending = false;
                self.replaced();
                self.badged = true;
                Some(Say::Limited)
            }
        }
    }

    /// A crash streak whose agent has run for [`HEALTHY`] by `now`, since its healthy run
    /// began, starts the back-off over.
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
        let step = if self.crash {
            self.streak.saturating_add(self.tries)
        } else {
            self.tries
        };
        crate::supervise::ladder::Ladder(&BACKOFF).step(usize::try_from(step).unwrap_or(usize::MAX))
    }

    /// One attempt came to `outcome` at `now`.
    pub fn attempted(&mut self, outcome: &Outcome, now: Instant) -> Say {
        match outcome {
            Outcome::Relaunched => {
                // A landing of an exit someone asked for is no crash's: it neither climbs the
                // crash back-off nor restarts the healthy run that forgives it, but it starts
                // one a streak of misses alone never had.
                if self.crash {
                    self.streak = self.streak.saturating_add(1);
                    self.last = Some(now);
                } else {
                    self.start_healthy_clock(now);
                }
                self.tries = 0;
                self.misses = 0;
                self.pending = false;
                self.restarted = false;
                self.failing = false;
                self.failing_said = false;
                self.asks = None;
                if std::mem::take(&mut self.badged) {
                    Say::Clear
                } else {
                    Say::Nothing
                }
            }
            Outcome::NotYet(_) => {
                if self.crash {
                    self.streak = self.streak.saturating_add(1);
                } else {
                    self.tries = self.tries.saturating_add(1);
                }
                self.misses = self.misses.saturating_add(1);
                if self.misses >= BADGE_AFTER && !self.badged {
                    self.badged = true;
                    self.failing = true;
                    Say::Failing
                } else if self.misses == BADGE_AFTER && !self.failing && self.asks.is_some() {
                    self.failing_said = true;
                    Say::Failing
                } else {
                    Say::Nothing
                }
            }
            Outcome::Left(_) if self.restarted => {
                self.pending = false;
                self.replaced();
                self.badged = true;
                Say::Cannot
            }
            Outcome::Left(_) => {
                self.pending = false;
                self.left = true;
                self.tried_no_more()
            }
            Outcome::Busy => Say::Nothing,
            Outcome::Cannot(_) => {
                self.pending = false;
                self.replaced();
                self.badged = true;
                Say::Cannot
            }
        }
    }

    /// An agent runs in the tab again at `now`, started by someone else.
    pub fn running(&mut self, now: Instant) -> Say {
        self.start_healthy_clock(now);
        self.pending = false;
        self.left = false;
        self.restarted = false;
        self.tries = 0;
        if self.failing || self.failing_said {
            return self.tried_no_more();
        }
        if self.asks.is_some() {
            return Say::Nothing;
        }
        if std::mem::take(&mut self.badged) {
            Say::Clear
        } else {
            Say::Nothing
        }
    }

    /// Records that the session's standing word asks a person to resume `conversation`.
    pub fn ask(&mut self, conversation: &str) {
        if self.badged {
            self.asks = Some(conversation.to_string());
        }
    }

    /// The agent in the tab, read, holds `conversation`.
    pub fn conversation_runs(&mut self, conversation: &str) -> Say {
        if self.asks.as_deref() != Some(conversation) {
            return Say::Nothing;
        }
        self.asks = None;
        self.failing = false;
        self.failing_said = false;
        if std::mem::take(&mut self.badged) {
            Say::Clear
        } else {
            Say::Nothing
        }
    }

    /// The conversation the standing word asks for, when one does.
    #[must_use]
    pub fn asks(&self) -> Option<&str> {
        self.asks.as_deref()
    }

    /// Whether the standing word is this relaunch's that keeps failing.
    #[must_use]
    pub const fn failing(&self) -> bool {
        self.failing
    }

    /// Whether this relaunch's failing was said beside a standing word that asks for a
    /// conversation.
    #[must_use]
    pub const fn failing_said(&self) -> bool {
        self.failing_said
    }

    fn tried_no_more(&mut self) -> Say {
        let said_beside = std::mem::take(&mut self.failing_said);
        if std::mem::take(&mut self.failing) {
            self.asks = None;
            self.badged = false;
            return Say::Clear;
        }
        if said_beside {
            Say::Clear
        } else {
            Say::Nothing
        }
    }

    fn replaced(&mut self) {
        self.failing_said = false;
        if std::mem::take(&mut self.failing) {
            self.asks = None;
        }
    }

    /// Whether the exit being handled is a crash ([`Self::exited_as`]).
    #[must_use]
    pub const fn crash(&self) -> bool {
        self.crash
    }

    const fn start_healthy_clock(&mut self, now: Instant) {
        if self.last.is_none() && self.streak > 0 {
            self.last = Some(now);
        }
    }

    /// Whether a relaunch is due.
    #[must_use]
    pub const fn pending(&self) -> bool {
        self.pending
    }

    /// `(left, badged, streak, misses)`.
    #[must_use]
    pub const fn counts(&self) -> (bool, bool, u32, u32) {
        (self.left, self.badged, self.streak, self.misses)
    }

    /// Whether the exit being handled is a restart of the harness's own ([`OnExit::Restarted`]).
    #[must_use]
    pub const fn restarted(&self) -> bool {
        self.restarted
    }
}

#[cfg(test)]
#[path = "relaunch_tests.rs"]
mod tests;
