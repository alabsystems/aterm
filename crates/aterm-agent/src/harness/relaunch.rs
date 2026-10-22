// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The relaunch primitive: an agent that is no longer running is started again in its own tab on
//! its own conversation. One line is typed at the tab's shell once it holds the terminal again,
//! `--resume <id>` at its end ([`super::upgrade::relaunch_line`]); once the new process holds the
//! conversation, one turn tells it to carry on.
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
//!   and when ([`Relaunches`]: a crash's growing back-off, which an exit
//!   someone asked for neither climbs nor waits; [`Outcome::Cannot`] is the
//!   keyed attention).
//!
//!   A Codex is relaunched only when it is codex-alab and aterm's copy of
//!   its own session record validated its line ([`CodexRun::self_claimed`]):
//!   the copy decides how it ended, never a look here, and a vendor `codex`
//!   is nobody's to relaunch (the Codex screen lane's retirement,
//!   2026-10-16).
//!
//!   An exit the harness's own restart made, whose step returned before the
//!   relaunch line was typed, is read first ([`restarted`],
//!   [`OnExit::Restarted`]): never a person's, a holder's or `[harness]
//!   relaunch = false`. It is carried from where it stopped
//!   ([`carry_restart`]) at least every [`CARRY_EVERY`], so the record cannot
//!   expire between tries; typing still waits on a person, a hold or any hand
//!   the line waits on is waited out uncounted with the record's clock
//!   stopped ([`WAIT_HAND_LIFTS`], [`exit_clock`]), and a failure is said as
//!   `refused:<why>`.
//!
//! In the window the new process belongs to its supervisor as soon as it holds
//! the conversation (`adopted`, [`Opts::hand_back`]); the relaunch's last
//! step is taken at that loop's first idle point ([`owed`], [`resume`]): the
//! continuation, or nothing for a quiet restart and every account move
//! ([`carried_quietly`]). Where that step types nothing, it is also taken
//! where the agent rests at a point the loop's guards withheld from the
//! host — around the loop's own act there, or its task's end ([`settle`]): a usage
//! limit's episode withholds every idle point until the limit's reset plus
//! an hour, and an owed relaunch holds the tab's automatic update behind it.
//!
//! NOTHING IS TYPED INTO A CONVERSATION WHOSE OWN RECORD SAYS ITS TASK IS
//! DONE, whatever brought it back (a crash, aterm's own end, the memory
//! restart, a stall, an asked restart, a model or an account move): one
//! nobody but the harness asked anything (`done:taskless`), and one whose
//! task LANDED — a clean `Nothing left:` defense answering one of Autopilot
//! Land's acts, with no prompt of a person's or an orchestrator's since
//! (`super::upgrade::FinishedScan`; `done:finished`; never a token or a
//! done reply), read once a restart from `upgrade_drive::FINISHED_ONCE_BYTES`
//! before the record's end, and from further back where that window settles
//! nothing (a landing behind more wakes than that is still read).
//! The restart itself is still made (the memory banner's included); only the
//! carry-on is not. A conversation whose LAST WORDS CLAIM THE WORK IS DONE,
//! its task not landed by its record, is typed under Cruise the carry-on's
//! facts and the Cruise prompt in place of `keep going` (`continued:check`;
//! `super::upgrade::FinishedScan::said_done`, the live lab's LIVE-RL-1), as
//! the turn-end policy types there. A conversation whose last turn is A
//! PERSON'S OWN EXCHANGE a tab's loop left to them (its ledger's
//! `left-to-person@v1` row, the review's RR3-TE-2) gets nothing
//! (`done:left`). And a relaunched agent THE TAB'S LOOP
//! RESUMED FIRST — its first idle point a wall, where the host's step is
//! withheld, and the turn-end policy's retry typed there — is carried on with
//! nothing (`done:carried`, the live lab's LIVE-RL-2): its carry-on would be
//! a second continuation of one resume. UNDER AUTOPILOT LAND
//! ([`Opts::land`], F2) no carry-on of Cruise's is typed at all
//! (`done:landing`): the tab's loop's land policy decides the point — the
//! land instruction, a nudge, or nothing once the task landed.
//! Codex is out of this: no rollout is read for whose turns it holds (the
//! owner's ruling on the no-task rule), so a Codex relaunch carries on as
//! before.
//!
//! A QUIET RESTART (`done:resumed`, the default upgrade) types nothing, then
//! takes ONE short look ([`RESTORE_LOOK`], [`restore_look`]) at whether the
//! resumed build itself warned that something the session ran did not come
//! back (an Artifact comment monitor, measured 2026-10-15): with a task
//! still open, one host line in aterm's own words says so
//! (`continued:restore`); a finished or taskless conversation gets nothing
//! typed, the fact on its record (`done:lost`) and the tab's host raises it
//! for the owner (a line of the digest). So does ANY restart whose carry-on
//! typed nothing (`done:finished`, `done:taskless`, `done:no-continue`;
//! [`St::look_word`]) — a crash's relaunch, aterm's own end, a memory, stall
//! or model restart (XREV-LC-2); and a relaunch's carry-on that is typed
//! says itself that what the session ran in the background may not have come
//! back ([`upgrade::RELAUNCH_RESTORES`]).
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
//! AND THE CLAUDE HOME IT RUNS UNDER, the same way ([`Snapshot::home`]):
//! `CLAUDE_CONFIG_DIR` names the directory Claude Code keeps its global
//! config in (`<dir>/.claude.json`: onboarding, folder trust, the profile)
//! and, with the login variable unset, the keychain item it signs in by
//! ([`accounts::HOME_ENV`]). A process started with it on its own command
//! line or by a wrapper (the tab's shell never had it) and relaunched
//! without it is a different Claude Code: MEASURED 2026-10-09 (2.1.295, the
//! real-claude lab, which launches `CLAUDE_CONFIG_DIR=<home>/.claude claude`):
//! the explicit restart's relaunch line carried no home, the new process
//! read `<home>/.claude.json`, which did not exist, and sat in the
//! first-run theme picker, never holding its conversation, the continuation
//! never typed (`step=wait:resume`). So every planner re-applies the value
//! the process's own exec environment held, verbatim, as REQUIRED
//! environment beside the login ([`required_env`]); unset adds nothing.
//!
//! AND READS THE AGENT'S OWN RECORDS UNDER IT ([`Records`]): Claude Code
//! started with `CLAUDE_CONFIG_DIR=<dir>` files `sessions/<pid>.json` and
//! `projects/` in `<dir>`, never in `~/.claude`. Every read of a relaunch's
//! agent — the snapshot's, the host's looks ([`follow`], [`look_at_exit`]),
//! the relaunch's plan (its conversation, whether its transcript exists, its
//! model and mode) and every step after the line (finding the new process,
//! its first idle, its continuation, its model: the home its record keeps,
//! [`St::home`]) — is made there (review of `fix/cold-restore-login-home`,
//! 2026-10-09: read under the user's home alone, an agent under another home
//! was never relaunched at all, `ended:no-conversation`, or started afresh).
//! A RELATIVE home — set and EMPTY included, which Claude Code's `??` keeps
//! — names a directory under the one the process runs in: the snapshot
//! resolves it there by the one resolver the live upgrade's sweep uses
//! ([`Records::of_process`]: under the directory that holds its record, the
//! one it started in where a worktree has moved it since) and keeps where
//! it read the record ([`Snapshot::claude_dir`]); a line whose `cd` would
//! resolve it elsewhere is refused (`refused:home`), as the sweep's is.
//! Before 2026-10-14 none of these read anything for such an agent: its
//! crash read as a graceful exit, its restart in place waited
//! `wait:no-session-record` for good.
//!
//! The one line also HEALS THE SHELL it is typed at: a tab whose
//! shell lost its integration key before the re-key channel existed reads
//! `status integration=degraded`, and nothing but a line typed while the
//! SHELL holds the terminal can reach it. The relaunch line is that line, for
//! every caller above: the window is asked for a one-use key file (`rekey
//! shell=<pid>`) and the shell takes it in front of the relaunch
//! ([`type_relaunch_line_with`], [`upgrade::with_rekey`]). The same line
//! reaches a shell whose integration predates LOADERS (`status
//! integration_rev=frozen`), and a loader whose prompt hook never runs
//! (a fish from before the prompt event under its own prompt),
//! which no body pointer ever will: the window names its own loader in the
//! file for a shell that does not sign the window's revision, and the line
//! sources it.

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

/// What every Claude Code relaunch exports (the survey switch, the one aterm also writes into the
/// user's settings and exports from the managed launcher): a same-program relaunch runs the process
/// image, which the launcher's exports never reach, and a session started with `--setting-sources`
/// that leaves the user's settings out never reads the file.
pub(super) const CLAUDE_LAUNCH_ENV: [(&str, &str); 1] = [(SURVEY_SWITCH_VAR, "1")];

/// The environment variable Claude Code turns every rating survey off by.
pub(super) const SURVEY_SWITCH_VAR: &str = "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY";

/// What a relaunch line is made from.
#[derive(Clone, Copy)]
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
    /// The model the relaunch asks for ([`with_model`]): it replaces the launch's own `--model`,
    /// every spelling, and an empty one drops it (Claude's default). `None` keeps the launch's
    /// flags, all but a `--model` the conversation's record marks failed, which [`plan`] drops
    /// ([`super::upgrade_models::line_model`]).
    pub(super) model: Option<&'a str>,
    /// The permission mode to resume in ([`resume_mode`],
    /// [`with_permission_mode`]). `None` keeps the launch's flags.
    pub(super) mode: Option<&'a str>,
    /// The Claude home the line must carry ([`required_env`]): the [`accounts::HOME_ENV`] value the
    /// process it replaces had in its exec environment ([`Snapshot::home`]), verbatim. `None` adds
    /// nothing: the new process takes its shell's.
    pub(super) home: Option<&'a str>,
    /// THE `HOME` THE LINE MUST CARRY ([`required_env`]): the one an agent
    /// with no `CLAUDE_CONFIG_DIR` kept its records under, where it is not
    /// the user's ([`super::upgrade_drive::home_carried`]'s answer). `None`
    /// adds nothing: the new process takes its shell's `HOME`, which is the
    /// one its records are under.
    pub(super) homedir: Option<&'a str>,
    /// THE LOGIN THE LINE MUST CARRY ([`login_env`]): the value of
    /// [`accounts::LOGIN_ENV`] for the relaunched process — the one the
    /// process it replaces had in its own exec environment
    /// ([`Snapshot::login`]: set and empty is the default login NAMED, which
    /// is not the same line as none), or an account move's target. `None`
    /// adds nothing on an ordinary retained-model/opted-in line: the new
    /// process takes its shell's, as before. A new model with billing off
    /// instead explicitly removes a captured-absent selector with its full
    /// profile context; it cannot inherit a changed shell login.
    pub(super) login: Option<&'a str>,
    /// Whether the process it replaces authenticated by its stored login
    /// ([`Snapshot::stored_login`]): the usage tap's first question ([`super::tap::gate`]).
    /// `false` puts no tap on the line.
    pub(super) stored_login: bool,
    /// New model selection requires target-context equality before any signal.
    pub(super) billing: ModelBilling<'a>,
}

/// Whether a planned line requests a new model, rather than retaining the
/// session's existing choice. Unknown live context remains explicit.
#[derive(Clone, Copy)]
pub(super) enum ModelBilling<'a> {
    Keep,
    Move(Option<&'a super::profile::Context>),
}

/// A new model under the key off needs known matching target context and a
/// named model. An unrepresented/default target is not a free entitlement.
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

/// The required environment of a relaunch line for `login` ([`Launch::login`]): the one pair
/// `(LOGIN_ENV, value)`, or none. The line is refused rather than typed without it ([`line_for`]):
/// dropped, the conversation would resume under the tab's shell's login, silently the wrong
/// account.
pub(super) fn login_env(login: Option<&str>) -> Vec<(&'static str, &str)> {
    login
        .map(|value| (accounts::LOGIN_ENV, value))
        .into_iter()
        .collect()
}

/// Everything a relaunch line must carry ([`Launch::home`], [`Launch::login`]): the Claude home the
/// process ran under, then its login ([`login_env`]), each only where it is `Some`. The line is
/// refused rather than typed without them ([`line_for`]).
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

/// The [`Snapshot::shell`] of an agent no shell holds: an aterm tab's own
/// program (`aterm -e claude`, `exec muse` in a tab; `Job::TabProgram`), a
/// Claude Code's, a codex-alab's, a Muse Code's, an agy's or a Gemini CLI's.
/// Its snapshot is kept for every reader of the agent (its transcript, its
/// limit and its reset's zone, its exit, the crash journal), and nothing
/// relaunches it in its own tab: every plan, restart and relaunch on exit of
/// it is refused `not-a-shell-job`, and it promises none
/// ([`resumes_on_exit`], [`carries_restart`]). The crash journal and a quit's
/// layout carry it like any agent, so a cold restore brings its conversation
/// back in the reopened tab's fresh shell. Pid 0 is no process: the harness's
/// `alive` reads it gone.
pub const NO_SHELL: u32 = 0;

/// The ledger's words for a plan [`NO_SHELL`] refuses.
pub(super) const NO_SHELL_DETAIL: &str =
    "the agent is its tab's own program, no shell's job: nothing would read a relaunch line";

/// Plan the relaunch from the live process: `shell` (the parent the job read
/// proved holds the agent, [`foreground_shell`]; never re-derived), its
/// dialect, the heal it needs, the resume directory, and the line
/// ([`line_for`]). The upgrade plans twice: before its announcement, so an
/// uncarryable launch is refused early, and again before its signal. No
/// shell ([`NO_SHELL`]) is refused for good: nothing would read the line.
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
    // `--resume` finds a conversation under the directory it was started in.
    let want = if launch.cwd.is_empty() {
        cwd_of(launch.agent).unwrap_or_default()
    } else {
        launch.cwd.to_string()
    };
    let cd =
        (!want.is_empty() && cwd_of(shell).as_deref() != Some(want.as_str())).then(|| want.clone());
    // A line that asks no model of its own never carries a `--model` the record marks failed
    // ([`line_model_of`]).
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
    // The usage tap, where `[harness] usage_tap` names this aterm
    // ([`Opts::usage_tap`]) and its gate holds for the relaunched process in
    // the directory it starts in ([`tap_of`]).
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

/// The usage tap's `--settings` value for `launch`, or `None` ([`super::tap::gate`]): `armed` names
/// this aterm and the login of the machine's one account, `cwd` is the directory the relaunched
/// process starts in. The cheap refusals come first, so nothing is read for a line that cannot
/// carry it; then the settings chain Claude Code reads there
/// ([`super::upgrade_drive::tap_settings`]) and the vendor's profile store
/// ([`super::upgrade_drive::tap_profile_store`]).
///
/// A line that carries a `HOME` of its own ([`Launch::homedir`]: an agent
/// with no `CLAUDE_CONFIG_DIR` under another `HOME` than the user's) is not
/// the default home either: its settings chain and its profile store are
/// under that `HOME`, not the user's this reads, and the tap it ran would
/// resolve its state root by that `HOME` ([`super::tap::rl_dir`]), where no
/// host reads its reading. No tap.
fn tap_of(
    opts: &Opts,
    launch: &Launch<'_>,
    armed: &super::tap::Armed,
    cwd: &str,
) -> Option<String> {
    let lone = armed.lone_login.as_deref();
    let own = launch.stored_login
        && launch.homedir.is_none()
        && super::tap::own_login(launch.login, launch.home, lone);
    if !own || super::tap::settings_value(&armed.exe).is_none() {
        return None;
    }
    let settings = super::upgrade_drive::tap_settings(opts, launch.argv, cwd);
    super::tap::gate(&super::tap::GateFacts {
        exe: Some(&armed.exe),
        stored_login: launch.stored_login,
        login: launch.login,
        lone_login: lone,
        home: launch.home,
        argv: launch.argv,
        settings: &settings,
        profile_store: super::upgrade_drive::tap_profile_store(opts),
    })
}

/// The dialect of the shell `shell`, as its executable names it: the kernel's
/// argv, or the name in the process table `t` where that cannot be read.
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

/// The pure half of [`plan`]: the launch flags carried into a resume of `session`
/// ([`upgrade::rewrite_argv`]), or into a fresh start without `--resume` when there is none, asking
/// for `model` ([`with_model`]) and permission `mode` ([`with_permission_mode`]) when given, and
/// the line that runs them ([`upgrade::relaunch_line_env`]), or the refusal either makes.
///
/// The environment is two kinds, since one list dropped whole on an error would relaunch a long
/// line on the wrong account. `required` ([`required_env`]: the home and login the relaunched
/// process must run under) is on the line or the line is refused (`refused:line`). The survey
/// switch ([`CLAUDE_LAUNCH_ENV`]) is a nicety, dropped first: the line is tried with both, then
/// with the required part alone. No usage tap: [`line_tapped`] with none.
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

/// [`line_for`] with the usage tap's `--settings` value `tap`, where its gate gave one
/// ([`super::tap::gate`]; [`with_tap`]). The tap is a nicety too: the line is tried with what is
/// required, the survey switch and the tap; then without the switch; then without the tap and with
/// the switch; then with what is required alone. With no tap that is [`line_for`]'s two tries.
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

/// Every fallback retains the explicit removals along with required values.
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
        // `rewrite_argv` ends with `--resume <id>`, the one pair it adds.
        flags.truncate(flags.len().saturating_sub(2));
    }
    let flags = with_permission_mode(with_model(flags, model), mode);
    // The survey switch adds 38 bytes to a line bound of 1000 on macOS (`upgrade`'s
    // `LineDiscipline::max_line`): a launch whose line fits only without it is relaunched without
    // it, never refused. What is required stays on every try, first on the line.
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

/// `flags` with the usage tap's pair, `--settings <value>`, before the
/// trailing `--resume` pair (at the end of a fresh start's flags). The flags
/// carry no `--settings` of their own: the tap's gate refuses a launch with
/// one ([`super::tap::gate`]), and its own is shed ([`upgrade::rewrite_argv`]).
pub(super) fn with_tap(mut flags: Vec<String>, value: &str) -> Vec<String> {
    let at = flags
        .iter()
        .rposition(|f| f == "--resume")
        .unwrap_or(flags.len());
    flags.splice(at..at, ["--settings".to_string(), value.to_string()]);
    flags
}

// ------------------------------------------------------- a worker's first line

/// The Claude Code a MANAGER runs, as a worker it starts must run it (`spawn
/// agent=claude`): the program its process runs ([`launched`]) and the Claude
/// home and login its own exec environment names — the pairs a relaunch of
/// that process carries ([`required_env`]). aterm strips `CLAUDE*` from every
/// shell it spawns, so a manager under a Claude home of its own would
/// otherwise start workers under the default one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerClaude {
    /// The program the manager's process runs: aterm's managed twin or the
    /// native install's build, as [`launched`] reads it.
    pub exe: PathBuf,
    /// [`accounts::HOME_ENV`] the worker's line carries: the manager's own
    /// where it is absolute; a relative one (empty included) as the
    /// directory it names from where the manager filed its record, absolute,
    /// since the worker runs in another directory ([`worker_homes`]).
    pub home: Option<String>,
    /// The `HOME` the worker's line carries: the manager's own where it has
    /// no `CLAUDE_CONFIG_DIR` and that `HOME` is not the user's, whose
    /// `.claude` then holds its records ([`worker_homes`], the rule of
    /// [`home_carried`]); `None` for a manager under the user's own.
    pub homedir: Option<String>,
    /// [`accounts::LOGIN_ENV`] likewise (set and empty is the default login
    /// named).
    pub login: Option<String>,
}

/// [`CallerClaude`] of process `pid`, a session's foreground job, under the
/// user's `home`: `None` when its argv cannot be read or it runs no Claude
/// program.
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
    // Where the manager's Claude Code keeps its records, by the one resolver
    // its relaunch reads them with ([`Records::of_process`]).
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

/// WHAT A WORKER'S LINE CARRIES OF ITS MANAGER'S CLAUDE HOME, `(home,
/// homedir)` ([`CallerClaude::home`], [`CallerClaude::homedir`]), from the
/// manager's exec environment `vars` and the directory its records were
/// found in (`dir`, [`Records::dir`]), for a user whose home is `user_home`.
/// The worker starts in its own `cwd=`, never the manager's: so an absolute
/// `CLAUDE_CONFIG_DIR` is carried as it is, a relative one (empty included)
/// as the absolute directory it named for the manager — left raw where that
/// was not read, which [`task_launch_line`] refuses rather than resolve
/// under the worker's directory. With no `CLAUDE_CONFIG_DIR`, Claude Code
/// keeps its records in `<HOME>/.claude`, the process's own `HOME`: a
/// manager under another `HOME` than the user's has that `HOME` carried
/// ([`home_carried`]'s rule, ATERM-KI-1015-1 LC-4), as the worker's shell
/// is the server's, under the user's.
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

/// THE LINE A CLAUDE CODE WORKER'S SHELL RUNS AT ITS FIRST PROMPT (`spawn
/// agent=claude`), Claude Code's row of the one line every worker's is
/// ([`super::worker::line`]): `exe` started fresh, `--model <model>` when one
/// is named, under the Claude home, `HOME` and login it must carry
/// ([`required_env`], refused rather than dropped) and the survey switch
/// ([`CLAUDE_LAUNCH_ENV`], a nicety dropped first, as on every relaunch
/// line), its task the fixed operand [`upgrade::TASK_OPERAND`]. So the task
/// is argv — the agent's first prompt — and the shell stays under the agent,
/// as a relaunch needs it.
///
/// # Errors
/// The line cannot be written ([`upgrade::task_line_env`]), or `home` is
/// relative: it would name another directory under the worker's `cwd=` than
/// it named for the manager ([`worker_homes`]).
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
    // A `HOME` only where the manager's records are under its own, not the
    // user's: the worker's shell is the server's, under the user's `HOME`.
    let required = required_env(home, homedir, login);
    super::worker::line_of(
        claude,
        dialect,
        exe,
        &[],
        &required,
        &super::worker::Fresh {
            model,
            ..super::worker::Fresh::default()
        },
    )
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
/// `{"type":"permission-mode"}` row, in the agent's own `records`. `None`
/// when absent or unknown.
fn last_permission_mode(records: &Records, session: &str) -> Option<String> {
    let path = records.transcript(session)?;
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
/// row ([`last_permission_mode`], in the agent's own `records`). Claude
/// writes that row only when it re-appends session metadata, so a mode
/// chosen at idle reaches the screen long before the file.
pub(super) fn resume_mode(
    records: &Records,
    session: &str,
    shown: Option<String>,
) -> Option<String> {
    shown.or_else(|| last_permission_mode(records, session))
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
/// ([`carry_restart`]). A hand the line waits on stops the clock
/// ([`exit_clock`]).
pub const STALE_S: u64 = 300;

/// THE ONE CLOCK A RESTART'S LINE GOES STALE BY: the latest of its signal
/// (`at_s`), the look that saw its agent gone (`exited_at`, [`St::exited_at`])
/// and the last look that found a hand the line waits on standing on its tab
/// (`hand_seen_s`, [`St::hand_seen_s`], [`hand_on_tab`]). A hand is waited
/// out, never timed out: the line has [`STALE_S`] once it lifts (LC-7 of the
/// 2026-10-15 sweep: a hold landing just after the upgrade's SIGTERM expired
/// the restart `stale-exit` under it, and nothing relaunched the agent once
/// the hold lifted; ruling 20 of 2026-10-14 for the explicit restart's exit).
#[must_use]
pub(super) fn exit_clock(at_s: u64, exited_at: u64, hand_seen_s: u64) -> u64 {
    at_s.max(exited_at).max(hand_seen_s)
}

/// A stale exit's ledger line, for `who` (`the agent`, `Codex`), alike in
/// both lanes: an end no look has seen yet is only the one asked for
/// ([`St::exited_at`]), and so is one seen only within the bound (a sweep
/// whose first sighting stamps it now), and the wait the last try recorded
/// ([`St::wait`]) says what held the line, in a person's words. A wait with
/// no words gives the bare line; `--status`'s wait log keeps it.
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
/// signal while no look has seen it gone yet — or after the last look that
/// found a hand on its tab, whichever is latest ([`exit_clock`]).
///
/// A STALE EXIT NAMES WHAT HELD ITS LINE ([`stale_exit_line`]).
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

/// Poll `cond` for at most `limit`, for conditions with no event to wait on
/// (exit has [`super::upgrade_wake::wait_exit`], the verdict [`first_idle`]):
/// every 250 ms, or a twelfth of `limit` when that is shorter — so every
/// production bound ([`super::Timings::PRODUCTION`], 3 s and up) polls at
/// 250 ms, and a test's millisecond bound ([`super::Timings::FAST`]) still
/// asks a dozen times.
pub(super) fn wait_until(limit: Duration, cond: impl FnMut() -> bool) -> bool {
    wait_every(limit, poll_gap(limit), cond)
}

/// [`wait_until`]'s pace for `limit`.
fn poll_gap(limit: Duration) -> Duration {
    (limit / 12).clamp(Duration::from_millis(5), Duration::from_millis(250))
}

/// [`wait_until`], `cond` polled every `gap`.
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

/// THE HARNESS'S WAIT CLOCK: the monotonic clock its bounded waits ([`wait_every`]) and the
/// looks inside them read — the system's, and in a test that runs them on its own steps
/// ([`virtual_waits`]) a clock that stands but for the [`pause`]s, so a bound a stand-in never
/// meets is waited out at once, with the same looks however loaded the machine.
pub(super) fn wait_now() -> Instant {
    #[cfg(test)]
    if let Some((at, _)) = VIRTUAL_WAITS.with(std::cell::Cell::get) {
        return at;
    }
    Instant::now()
}

/// The harness's pause between two looks, or before a line it types: `thread::sleep`, and on
/// a test's wait clock ([`virtual_waits`]) a step of that clock.
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
    /// This thread's wait clock while a test holds it ([`virtual_waits`]): where it stands,
    /// and how many holds stand on it.
    static VIRTUAL_WAITS: std::cell::Cell<Option<(Instant, usize)>> =
        const { std::cell::Cell::new(None) };
}

/// A hold on this thread's wait clock ([`virtual_waits`]); the last one dropped gives the
/// thread the system's clock back.
// Its only holders are the drive tests, which run on Unix alone.
#[cfg(all(test, unix))]
#[must_use]
pub(crate) struct VirtualWaits(());

/// THE HARNESS'S WAITS ON THIS THREAD RUN ON THEIR OWN STEPS while the hold lives
/// ([`wait_now`], [`pause`]): for a test whose stand-ins answer every look at once — a
/// scripted kernel, a scripted instance — never one beside a real process, whose events take
/// real time.
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
    if !super::upgrade_wake::wait_exit(old, opts.timings.exited) {
        // Never a harder signal: the agent finishes exiting on its own, and a
        // later pass finds the phase and the process as they are.
        return said(r, "wait:exiting");
    }
    // Gone: from here the restart is the tab's record, stamped when the agent
    // is seen gone, never at the signal.
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
    // The shell draws its prompt after it takes the terminal back.
    pause(opts.timings.prompt_drawn);
    let tab = st.tab.clone();
    let line = st.line.clone();
    // Read again rather than recorded: a relaunch a later step resumes finds
    // the same shell, and one whose name cannot be read now types no re-key.
    let dialect = shell_dialect(shell, &table());
    // A MODEL IS ASKED BY THE LINE THAT ASKS IT: the model this line carries
    // is recorded as asked right before it is typed, and taken back when it
    // is not, whichever restart ended the agent. Recorded at the signal
    // instead, a restart whose signal was refused or whose line never came
    // would keep it, and it would settle FAILED though no relaunch asked it.
    let asked = record_asked(opts, session, &st.model_list);
    // The command block the line is typed into, read before it is
    // ([`line_block`]): whether the line's command has run and ended is that
    // block's to say ([`ShellBack`]).
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
        // When the agent's end was asked — known only for a restart that
        // ended it ([`end_asked_at`]): a person's keys before it went to the
        // agent, never to the prompt this line is typed at; and whether a
        // hand held the line ([`hand_held_since`]).
        PromptLook::of(st),
        |c, pid, tab| process_in_tab(c, pid, tab, None, None),
    );
    if typed.is_err() {
        take_back_ask(opts, session, asked);
    }
    match typed {
        Ok(()) => st.prompt = None,
        Err(RelaunchLineError::Wait(why)) => {
            // Kept on the record by every try, the host's carries too, so the move's row and the
            // stale line ([`expired`]) say what held the line.
            st.note_wait(why, now_s());
            return said(r, format!("wait:{why}"));
        }
        Err(RelaunchLineError::Hand) => {
            // A hand the line waits on: its clock stops while it stands
            // ([`St::saw_hand`], [`exit_clock`]).
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

/// A RELAUNCH LINE TYPED IS RECORDED BEFORE ANYTHING ELSE IS DONE
/// (ATERM-KI-1016-1 host-5): `st` relaunched now, its command block `block`
/// and no process of the new line adopted yet ([`St::relaunched_pid`]),
/// saved for `session` — and only then `after` (the ledger's line). The
/// ledger's line came first: a step that panicked between the two left the
/// record `Exiting` with its line typed, and the next step, reading a line
/// not typed yet, typed it once more wherever the shell had its prompt back
/// (the line's agent ended at once) and nothing held the conversation.
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
    /// A hand the line waits on stands on the tab ([`hand_on_tab`]: a hold,
    /// another driver's lease, a turn) — read at the line's look, or a hold
    /// that landed after it (`ERR halted`, [`refused_line`]): a `held` wait
    /// under which the restart's clock does not run ([`St::saw_hand`]).
    Hand,
    Turn(String),
}

/// Type the relaunch line. The tab's PTY claim is probed again right before every try
/// (`tab_probe`, the race test's seam), since a person may have started another program at the
/// prompt, and the session's `status` is read: a person typing within the grace, a halt, or
/// anyone's hand holds the tab (`upgrade_drive::held`) and nothing is typed.
///
/// The fence proves only that the screen did not move between a read and the paste, never that the
/// prompt's input line is empty: a person's paused `rm -rf build` would get the line appended and
/// Enter pressed on both. So the prompt is marked as first read ([`PromptMark`], [`first_mark`])
/// and the mark kept across attempts (`St::prompt`): a person's typing after it waits `typing`
/// until the prompt reads as marked again; a prompt that moved for another reason (an async
/// redraw, a new prompt, a program started at it) waits `prompt-moved` and is marked afresh. A
/// prompt whose text ticks in place (a clock) keeps its cursor.
///
/// Bash 3.2 has no bracketed paste, so it is typed to ([`type_line`]), the mark read once more
/// right before the keystrokes; zsh and fish are pasted to (so no binding or abbreviation acts),
/// fenced on a fresh read ([`turn_fenced`]). Typing through the whole yield is a wait, never a
/// refusal; a prompt that never holds still is pasted unfenced once a fresh read still finds it as
/// marked and the shell holding the tab.
///
/// With `dialect` known a degraded tab is re-keyed in this line ([`type_relaunch_line_with`]). No
/// live process may hold the conversation, by the relaunched agent's own `records`
/// ([`St::agent_records`]).
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

/// What one relaunch attempt judges the returned prompt by: its mark, kept across attempts
/// (`St::prompt`, [`PromptMark`]), and when the agent's end was asked (`ended_at_s`, Unix seconds,
/// [`end_asked_at`]; `0` unknown). A person's keys before that instant went to the agent, so a read
/// whose `human_ms` stamp dates the last person input before it proves nothing at the prompt is a
/// person's typing ([`first_mark`]).
pub(super) struct PromptLook<'a> {
    pub(super) mark: &'a mut Option<PromptMark>,
    pub(super) ended_at_s: u64,
    /// WHERE A HAND HELD THE LINE ([`hand_held_since`]): the instant (Unix
    /// seconds) its record began waiting, which every person's input must
    /// predate for a first mark to be taken ([`first_mark`]); `0`, no hand
    /// ever held it.
    pub(super) held_since_s: u64,
}

impl<'a> PromptLook<'a> {
    /// The look record `st` keeps: its mark, when its agent's end was asked
    /// ([`end_asked_at`]) and whether a hand held its line
    /// ([`hand_held_since`]).
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

/// THE INSTANT A PERSON'S KEYS MUST PREDATE WHERE A HAND HELD THE LINE
/// ([`PromptLook::held_since_s`]; the review of LC-7 of the 2026-10-15
/// sweep): a record whose line a hand held ([`St::hand_seen_s`]) waited it
/// out for as long as it stood, an hour for all it says, with no mark taken
/// (the hand is asked first). A person who used the shell meanwhile and left
/// it idle past `human_grace_s` leaves a clean prompt that is no longer the
/// one the agent's end returned. So such a record's first mark asks that no
/// person keyed the session since it began waiting: its
/// [`upgrade::Phase::Exiting`] instant — the restart's signal, or the moment
/// a relaunch on exit was made, after which a person's keys went to the
/// shell. `0` for a record no hand held, or one past its line.
pub(super) fn hand_held_since(st: &St) -> u64 {
    match st.phase {
        upgrade::Phase::Exiting { at_s } if st.hand_seen_s != 0 => at_s.max(1),
        _ => 0,
    }
}

/// When the agent's end was asked, to date a person's keys against ([`PromptLook::ended_at_s`]):
/// the record's [`upgrade::Phase::Exiting`] instant, but only where its restart ended its agent
/// itself ([`ends_its_agent`]: the upgrade's SIGTERM, a Codex `/exit` it typed, a restart in
/// place), stamped before the signal after a last look found no person's hand, so every key before
/// it went to the agent. A relaunch on exit stamps its record after the agent left and its back-off
/// ran (up to ten minutes): keys in between went to the prompt this line is typed at, so it gives
/// `0`, unknown.
pub(super) fn end_asked_at(st: &St) -> u64 {
    match st.phase {
        upgrade::Phase::Exiting { at_s } if ends_its_agent(&st.cause) => at_s,
        _ => 0,
    }
}

/// The one relaunch line, whichever agent it resumes: [`type_relaunch_line`] with `owner_clear` as
/// the check that no process holds the conversation any more (Claude Code's session files, or a
/// Codex thread's writer lock). Every other guard (the tab's shell proven before each try, a hold,
/// the prompt's mark, the fence) is this function's, for both.
///
/// The heal rides the line: once every check that can make this attempt wait has passed, the window
/// is asked for a typed re-key ([`issue_rekey`]), granted only for a tab whose integration is
/// degraded or whose shell does not sign the window's revision (the window then also names its own
/// loader), and only while `shell` leads its foreground group. Nothing about the heal can stop the
/// relaunch: a refusal, an older window or a healed line too long for the tty types the plain line,
/// and an attempt that then types nothing takes the key back ([`withdraw_rekey`]).
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
    // One read: a hand the line waits on is said as one (its clock stops),
    // a person, unread input or an unreadable tab as a counted wait.
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

/// The relaunch line with the tab's shell taking a fresh key in front of it
/// ([`upgrade::with_rekey`]), or `None`: the window refused (a healthy integrated tab, an
/// unintegrated one, a re-key of the shell's own already waiting, a shell no longer in the
/// foreground), knows no such verb (an older aterm), or the healed line would not fit, in which
/// case the key it issued is taken back.
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
/// wait, never a refusal failing a relaunch whose agent is gone. So is a hold
/// that landed between the line's look and its typing (`ERR halted`, the
/// answer every verb that reaches the pty gets under a hold, the harness's
/// own hard lease's included): a hand the line waits on
/// ([`RelaunchLineError::Hand`]), waited out (LC-7 of the 2026-10-15 sweep:
/// read as a refusal, it stopped the restart `relaunch-refused`, and nothing
/// relaunched the agent once the hold lifted).
fn refused_line(e: String) -> RelaunchLineError {
    if e.starts_with("ERR busy") {
        RelaunchLineError::Wait("held")
    } else if e.starts_with("ERR halted") {
        RelaunchLineError::Hand
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

/// The first mark of a returned prompt ([`PromptMark::of`] over `scr`), made only where nothing is
/// typed after the prompt yet: typeahead that reached the prompt before its first read would
/// otherwise become the mark, and the line be appended to it. The cell at the cursor and the one
/// after it must be blank (a caret moved home over a typed command sits on it: `typing`). Then:
///
/// 1. The shell's own mark of the input's start (`133;B`, [`input_start`]) on the cursor's row (the
///    two read the same left of the mark's column): the cursor must sit exactly at its column, else
///    `typing`.
/// 2. A mark that names another row, with no person's input since the agent's end was asked
///    (`ended_at_s`, by the read's `human_ms` stamp, [`HumanInput::quiet_since`]): the shell is at
///    its prompt and its line holds nothing a person typed, so it is typed at (an engine whose
///    absolute rows slid reads the mark back as the line above). With input since, or no stamp that
///    can say, it waits `mark-elsewhere`: typing that wrapped past the prompt's row looks just so.
/// 3. No mark (no integration, or a newest block that is not `entering`, such as `claude; read`'s
///    builtin): the cursor must follow a blank, as every common prompt ends in one and typing does
///    not. The stamp never stands in for that shape: it proves no person typed, not that the shell
///    is at a prompt (typed into `while :; do claude; read -k1; done`, the line's first key ends
///    the `read` and the rest goes to the agent). The wait is `unmarked` where the stamp shows
///    nobody typed, else `typing`.
///
/// Before all three, A HAND THAT HELD THE LINE (`held_since_s`,
/// [`hand_held_since`]): no person's input since its record began waiting,
/// by the read's own stamp, else the wait `keyed-while-held` — a key since,
/// or a stamp that cannot say. A session nobody keyed says since when its
/// host served it so ([`HumanInput::NeverFor`]; the review of LC-4, KR-2):
/// from before the record began, nobody keyed it since — a driver's tab no
/// person ever keyed is typed at once its lease lifts; from after (a
/// seamless successor's adopted shell nothing was carried for begins at its
/// Commit), or a host that does not say, it cannot. A seamless update's
/// successor carries the outgoing instance's stamp and serving age (LC-4 of
/// ATERM-KI-1016-1). Counted, so the restart's stale bound ends it and its
/// row asks the person, as it did before a hand was waited out.
///
/// `Ok(None)`: a read with no cursor, typed at as before.
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
        left_of(&self.row, self.col) == left_of(cursor_row, self.col)
    }
}

/// The text of `row` drawn left of grid column `col`. The cursor's column
/// and the shell's input start (`cmdcol`) count CELLS, while a row's text
/// holds one char per glyph drawn (a wide glyph's second cell is no char), so
/// a column is found by grid column ([`aterm_grapheme::grid_graphemes`], cut
/// where [`aterm_grapheme::column_to_char_index`] cuts, as the session survey
/// cuts a command row at its `133;B` column), never by char count: counted by
/// chars, the cursor after `🦀 % x` sat past the row's end and the person's `x`
/// read as the blank after the prompt.
fn left_of(row: &str, col: usize) -> &str {
    let end = aterm_grapheme::grid_graphemes(row)
        .find(|g| col < g.end_col)
        .map_or(row.len(), |g| g.byte_offset);
    &row[..end]
}

/// The grid column where the first glyph other than a blank is drawn in `row`
/// at or right of column `col` (one that starts left of it and covers it
/// included), in the cells [`left_of`] counts.
fn first_drawn(row: &str, col: usize) -> Option<usize> {
    aterm_grapheme::grid_graphemes(row)
        .find(|g| g.end_col > col && !g.text.trim().is_empty())
        .map(|g| g.start_col)
}

/// Whether the cell left of grid column `col` in `row` shows a blank: past
/// the row's text (a `text` row's trailing blanks are trimmed), or a blank
/// glyph. A character the grid stores in that cell and draws in no cell of
/// its own (a ZWSP, an LRM after the prompt's last space) is no glyph, so
/// `% ` with an LRM after it still ends in a blank. Column 0 has no cell
/// before it.
fn blank_before(row: &str, col: usize) -> bool {
    col > 0
        && aterm_grapheme::grid_graphemes(row)
            .filter(|g| g.start_col < g.end_col && g.start_col < col)
            .last()
            .is_none_or(|g| g.end_col < col || g.text.trim().is_empty())
}

/// [`InputStart`] as the tab answers it. `None` where no such mark reads: no
/// integration, a block in any other state, or a host that sends no `cmdcol`.
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
            // Anything now in the gap the mark saw blank is typing; a right
            // prompt growing into it reads as typing too (the safe error).
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

/// How many fenced tries the relaunch line gets ([`type_relaunch_line`]),
/// [`super::Timings::fence_every`] apart, before it is pasted unfenced.
pub(super) const RELAUNCH_FENCE_TRIES: usize = 5;

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
    // The relaunched process files its record under the Claude home its line
    // carries ([`St::agent_records`]).
    let records = st.agent_records(&opts.home);
    let (old, shell) = (st.pid, st.shell);
    // A fresh start holds a conversation of its own: the shell's child is it.
    let fresh = starts_afresh(&st.cause);
    let mut new: Option<SessionFile> = None;
    let mut roster = SessionRosterCache::default();
    let mut back = ShellBack::after(st.line_block, &opts.timings);
    let mut ended = false;
    let look_back = ends_at_once(&st.cause);
    let tab = st.tab.clone();
    // Polled at least as often as a shell look is due ([`ShellBack`]): the
    // product's look (1 s) is slower than its poll (250 ms), so this moves
    // nothing there. On the tests' clock (a 50 ms look) the looks otherwise
    // ran at the poll's 250 ms, and a relaunch that ended as it started read
    // so about 0.75 s after its line instead of 0.35 s.
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
    // The hand stays on the tab until the new process is idle, and through the
    // continuation when this step types it.
    if let Some(sf) = &new {
        first_idle(c, hand, &st.tab, opts.timings.first_idle, || {
            records
                .session_file(sf.pid)
                .is_some_and(|sf| sf.status == "idle")
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
        // Adopted once, recorded once: an adopted restart stays in flight until its loop's idle
        // point carries it on ([`resume`]), and a step that finds the same process meanwhile
        // ([`St::relaunched_pid`]) says `adopted` and writes nothing.
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
        // The relaunched agent ended as it started (Claude's `No conversation
        // found`, a crash at start): the line's command was seen to run and
        // end, its shell has had the tab back for `ended_at_once` since,
        // and nothing registered the conversation ([`ShellBack`]) — read for
        // a relaunch whose line nothing types again ([`ends_at_once`]).
        // Said now (day six, D30: the wait went on 90 s at a time for five
        // minutes, the harness's hand on the tab throughout), with the stale
        // relaunch's word and stop — what it printed is on the tab.
        //
        // A MOVE ONTO ANOTHER BUILD IS TAKEN BACK first (LC-5 of
        // ATERM-KI-1016-1, [`St::roll_back`]): its failure kept against the
        // build, the line back — the same resume on the program the agent ran
        // ([`St::back_line`]) — is typed at the prompt the shell has back,
        // saved first as every relaunch line is, and the agent is carried on
        // there. Stopped instead, the one working agent the move ended stayed
        // down until a person resumed it.
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

/// WHAT AN `adopted` ROW PROMISES of the relaunch's last step, which the
/// tab's loop takes at its first idle point ([`resume`]), ranked as
/// [`carry_on_once`] ranks it there: quiet, then nothing owed, then Land.
/// Read at the adoption, it is what that step would do then; a turn in
/// between (a person's prompt, a loop's act) is that step's to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Adopted {
    /// A QUIET restart ([`resumes_quietly`]) is resumed as it was with
    /// nothing typed, as the same move's `done:resumed` says
    /// ([`quiet_outcome`]).
    Quiet,
    /// NOTHING OWED by the conversation's own records ([`nothing_owed`],
    /// the why it names: nobody asked it anything, its worker finished, its
    /// last turn is a person's own exchange, its loop resumed it first):
    /// nothing is typed, whatever the Autopilot, as the step's
    /// `done:taskless`, `done:finished`, `done:left` or `done:carried` says
    /// (the review's RV2-REL-1: a cleared conversation's row promised the
    /// continuation, and `done:taskless` followed).
    NothingOwed(&'static str),
    /// UNDER AUTOPILOT LAND ([`Opts::land`], F2) no continuation of Cruise's
    /// is typed: the tab's loop decides its point (`done:landing`; a task
    /// done under Land, `done:said-done`).
    Land,
    /// Any other move owes the continuation, which the tab's supervisor
    /// types at the agent's first idle point.
    Owed,
}

impl Adopted {
    /// The promise of the move `st` records. `none_owed` says why the
    /// conversation's records owe nothing ([`nothing_owed`]), asked only
    /// where the move is not quiet; `land` whether the tab is under Land,
    /// asked only where something is owed.
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

/// Whether tab `tab` is under Land as the relaunch adopts its agent: its own
/// `status` says so ([`land_by_status`]). The pass that adopts it — a cold
/// restore's, an exit's, a restart's — carries no Autopilot of its own (the
/// window's host hands one to the carry-on alone, [`resume_as`]); a
/// `status` that cannot be read says no Land.
pub(super) fn tab_under_land(c: &mut Client, tab: &str) -> bool {
    super::upgrade_drive::status_line(c, tab).is_some_and(|line| land_by_status(&line))
}

/// Whether one `status` reply says the tab's EFFECTIVE Autopilot is Land
/// (its `autopilot=` field, the first; an older server prints none, and
/// nothing then reads as Land). Anything but `OK` is no Land.
fn land_by_status(line: &str) -> bool {
    autopilot_by_status(line) == Some("land")
}

/// The tab's EFFECTIVE Autopilot as one `status` reply says it (its
/// `autopilot=` field, the first: `off`, `cruise` or `land`); `None` for a
/// reply that is not `OK`, an older server's that prints none, and `-` (no
/// agent tab).
fn autopilot_by_status(line: &str) -> Option<&str> {
    if !crate::is_ok_reply(line) {
        return None;
    }
    line.split_whitespace()
        .find_map(|w| w.strip_prefix("autopilot="))
        .filter(|a| *a != "-")
}

/// Whether the harness's hands are on tab `tab` by its own `status`, read
/// now: its effective Autopilot is Cruise or Land ([`autopilot_by_status`]).
/// Off is the owner's ("the harness keeps its hands off this session",
/// docs/DESIGN-harness-autopilot-2026-10-15.md:42); `-`, none, or a
/// `status` that cannot be read says no.
fn autopilot_on(c: &mut Client, tab: &str) -> bool {
    super::upgrade_drive::status_line(c, tab)
        .is_some_and(|line| matches!(autopilot_by_status(&line), Some("cruise" | "land")))
}

/// Whether a restart of `cause` is carried on with nothing typed: every move to another account
/// ([`CAUSE_ACCOUNT`]), whose supervisor types the point's own act (after a move made at a wall,
/// that wall's resume), and an upgrade's quiet move (`quiet`, [`St::quiet`]), never another
/// relaunch's ([`relaunch_cause`], whose continuation says why it came back). Pure.
#[must_use]
pub fn carried_quietly(cause: &str, quiet: bool) -> bool {
    cause.starts_with(CAUSE_ACCOUNT) || (quiet && !relaunch_cause(cause))
}

/// [`carried_quietly`], for the restart `st` records.
fn resumes_quietly(st: &St) -> bool {
    carried_quietly(&st.cause, st.quiet)
}

/// Whether the restart `st` records keeps [`restore_look`]'s ONE LOOK at what
/// the resumed build reports did not come back: a quiet one
/// ([`resumes_quietly`]), and any whose carry-on typed nothing
/// ([`St::look_word`]: a finished or taskless conversation, a continuation
/// that could not be typed) — what a relaunch's crash, aterm's own end or a
/// restart took is no less gone there (XREV-LC-2). A carry-on that was typed
/// says [`upgrade::RELAUNCH_RESTORES`] (or the upgrade's
/// [`upgrade::RESTART_WATCHES`]) itself, and its model is confirmed instead.
fn looks_back(st: &St) -> bool {
    resumes_quietly(st) || !st.look_word.is_empty()
}

/// A carry-on that typed nothing (its last word `word`, [`St::done_at`]
/// stamped) keeps the look open ([`looks_back`]): on the build and the
/// process it read, for [`RESTORE_LOOK`].
fn look_after_nothing(st: &mut St, word: &str, version: &str, pid: u32) {
    st.resumed_on = version.to_string();
    st.resumed_pid = pid;
    st.confirm_by = st.done_at.saturating_add(RESTORE_LOOK.as_secs());
    st.look_word = word.to_string();
}

/// How long one [`await_held`] step waits for the relaunched process to
/// register its conversation: [`super::Timings::held`] for a live relaunch, and
/// for a cold restore ([`CAUSE_HOST`]) the shorter
/// [`super::Timings::restored_held`], so the one sweep lock is released for
/// other tabs (a later retry checks the same record) — long enough for
/// [`ShellBack`] to see [`super::Timings::ended_at_once`]. A later step waits
/// again, until [`STALE_S`].
fn held_wait(t: &super::Timings, cause: &str) -> Duration {
    if cause == CAUSE_HOST {
        t.restored_held
    } else {
        t.held
    }
}

/// Whether [`await_held`] reads a relaunched agent as ENDED as it started
/// ([`ShellBack`]: the line's command seen to run and end, its shell holding
/// the tab for [`super::Timings::ended_at_once`] since): every relaunch whose line nothing
/// types again, so that a stop is final. A restored tab's ([`CAUSE_HOST`]),
/// whose host tries it once and says a failure at once (ruling 296 of the
/// messages design, as amended in review); and a restart that ended its
/// agent itself ([`ends_its_agent`]: the upgrade's, the restart in place's),
/// whose line is typed once by the restart or by the carry of its exit, and
/// whose stopped record that carry answers `refused:` ([`carry_own`]).
/// Waited out instead, an upgrade's relaunch that Claude refused at once
/// (`No conversation found`, a crash at start) stood `relaunched` once the
/// step's wait ended, with nothing left to look at a tab whose loop had
/// stopped with its agent, and read for half an hour as a start-up screen
/// waiting on a person — never as the agent that is down (the real-Claude
/// sweep of 2026-10-09). NOT the live relaunch on exit: its loop reads
/// `failed:no-resume` as not yet ([`outcome`]) and each retry after its
/// back-off types the relaunch line into the tab afresh, so an early verdict
/// there retyped a refusing agent at about +7 s, +23 s and +90 s where this
/// wait retypes it only past [`STALE_S`] ("never interrupt an agent that is
/// working"); it keeps the full wait until that loop has a no-retype rule
/// for a `failed:` word.
pub(super) fn ends_at_once(cause: &str) -> bool {
    cause == CAUSE_HOST || ends_its_agent(cause)
}

/// Tracks whether the relaunched agent ended as it started: THE LINE'S
/// COMMAND WAS SEEN TO RUN, and the shell the line was typed at has held its
/// terminal for [`super::Timings::ended_at_once`] in a row since, looked at
/// every [`super::Timings::shell_look`] (a `ps` each time, so not at the
/// roster's pace). A look that finds the line not run yet, or anything else holding the terminal, restarts the count;
/// an unreadable look leaves it unchanged.
///
/// A shell holding its terminal is no proof alone: it does its own work between reading the line
/// and starting what it names (a `preexec` built on `$(…)`, a direnv on the line's `cd`), holding
/// its terminal all the while (zsh runs even a hook's external command in its own group). So the
/// line must be seen to run, by the best witness the tab has:
///
/// 1. THE SHELL'S OWN WORD, where its integration marks command blocks
///    ([`St::line_block`], read before the line was typed: [`line_block`]):
///    the block the line was typed into has finished, or a newer one began
///    ([`line_over`]). Its `133;D` comes from the shell's next prompt, after
///    the command and every hook of it, so nothing a shell does before the
///    agent starts reads as its end. With that witness nothing else counts.
/// 2. THE KERNEL'S, where no block was read: a look found the shell NOT
///    holding its terminal since the line (a job-control shell hands its
///    terminal to the job it starts, and only to that). A command that ends
///    between two looks is not seen; the full wait, then
///    the stale bound ([`STALE_S`]), says it then.
#[derive(Debug)]
pub(super) struct ShellBack {
    /// Since when every read look found the line run and the shell holding
    /// its terminal.
    since: Option<Instant>,
    /// The last look, for [`Self::look`].
    looked: Option<Instant>,
    /// The command block the line was typed into ([`St::line_block`]):
    /// whether the line ran is its word alone.
    block: Option<u64>,
    /// With no block to read: a look found the shell not holding its
    /// terminal (the line's job held it).
    left: bool,
    /// How often a look is due ([`super::Timings::shell_look`]).
    look: Duration,
    /// How long the shell must hold its terminal once the line ran
    /// ([`super::Timings::ended_at_once`]).
    ended_at_once: Duration,
}

impl ShellBack {
    /// The tracker for a line typed into the command block `block`
    /// ([`St::line_block`]; `None`: none read), on the clock `t`: the one way
    /// to build one, so no tracker runs on zero durations (which would read a
    /// relaunch as ended at its first look).
    pub(super) fn after(block: Option<u64>, t: &super::Timings) -> Self {
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

    /// One look at `now`: whether the shell holds its terminal
    /// ([`Kernel::shell_holds_tab`]) and, where the line's block is known,
    /// whether it is over ([`line_over`], `None`: unread). Whether the line
    /// has now been seen to run and the shell has held its terminal for
    /// [`Self::ended_at_once`] since.
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

/// THE COMMAND BLOCK A RELAUNCH LINE IS TYPED INTO ([`St::line_block`]): the
/// tab's newest block while it is the prompt the shell waits at (`prompt` or
/// `entering`, `blocks 1 --json`), read right before the line is typed.
/// `None` where the tab's shell marks no blocks, or its newest is not a
/// prompt (a command still runs, or its marks are behind).
pub(super) fn line_block(c: &mut Client, tab: &str) -> Option<u64> {
    let newest = newest_block(c, tab)?;
    let state = newest.get("state").and_then(Value::as_str)?;
    matches!(state, "prompt" | "entering")
        .then(|| newest.get("id").and_then(Value::as_u64))
        .flatten()
}

/// THE COMMAND LINE THE TAB'S SHELL RUNS NOW, as its integration recorded it
/// (typed): the newest block's `cmdline` where that block is `executing`.
/// `None` when nothing is read, the newest block runs nothing, or its line
/// was not recorded.
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

/// Whether the command the relaunch line ran in block `block` is OVER, by the
/// shell's own marks (`blocks 1 --json`): that block is `complete` (its
/// `133;D`), or a newer one began (the shell's next prompt — ids only rise in
/// a terminal). `Some(false)` while it is still the newest and not complete
/// (the shell's hooks, or the agent, still run); `None` when nothing can be
/// read, or the newest block is older than it (the tab's blocks were reset).
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

/// The tab's newest shell-integration command block (`blocks 1 --json`), or
/// `None`: no reply, none marked, or one that cannot be read.
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

/// Wait for the relaunched agent's first idle with the harness's `hand` on the
/// tab (renewed first; at most `within`, [`super::Timings::first_idle`], before
/// the hand is given back — an opening box is the supervisor's to answer once
/// the step returns): one pushed `await agent idle`, or on a server without
/// the verb, `record_idle` polled.
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
    let records = st.agent_records(&opts.home);
    let mut conversation_changed = false;
    let mut tail_available = true;
    let settled = wait_until(opts.timings.carry_settle, || {
        // The hand the caller holds outlasts this wait (120 s against a 60 s
        // lease): renewed through it.
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
    // A QUIET RESTART ([`super::upgrade_drive::St::quiet`], the default, and
    // every account move: [`carried_quietly`]) is carried on with NOTHING: the
    // agent was idle, nothing was asked of it, and it is resumed as it was —
    // the tab's loop decides the point as if nothing had moved (`done:resumed`,
    // a last word that moves nothing; after a move made at a wall, that wall's
    // resume), so an agent that had finished, or asked its person something,
    // is never told to carry on. The model it runs now is its next answer's
    // to say, and no step waits for that answer: the outcome names the model
    // it ran before, never "unconfirmed", which read as a check that failed
    // on every default upgrade ([`quiet_outcome`]). ONE LOOK follows, for
    // [`RESTORE_LOOK`]: whether the resumed build itself warned that
    // something did not come back ([`restore_look`]).
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
    // What the harness typed into this conversation, in this tab and in the
    // earlier tabs that held it (a cold restore's reopened tab has a new sid:
    // [`conversation_typed`], XREV-LC-1) — no one else's prompt.
    let ours = conversation_typed_by(opts, tab, key, st);
    // The conversation's record, read once for every question below
    // ([`WhereWritten`]).
    let written = WhereWritten::new(opts, &records, key, &ours);
    // Carried on with NOTHING where the conversation's own records say a
    // carry-on is owed no longer ([`nothing_owed`]: nobody asked it anything,
    // its task landed, its last turn is a person's own exchange, or the
    // tab's loop resumed it first).
    if settled && let Some((word, why)) = nothing_owed(opts, &written, tab, st) {
        return done_with_nothing(opts, r, st, key, word, why, &latest.version, new.pid);
    }
    // AUTOPILOT LAND (`Opts::land`, F2): no continuation of Cruise's is
    // typed — not `keep going`, not the Cruise prompt. The point is the
    // tab's loop's, whose land policy types the land instruction, a nudge,
    // or nothing once the task landed, as at any point under Land
    // (ddd3452dd made a limit's end Land's the same way). Typed, a landed
    // task's memory restart started the work again, the very reason a
    // landing is wrapped in no other relaunch. The one
    // look at what did not come back stays. A pass that carries no Autopilot
    // of its own (the CLI's hand-run sweep, `Opts::land` false) reads the
    // tab's own `status`, as an adopting pass does ([`tab_under_land`];
    // ATERM-KI-1016-1: it typed Cruise's carry-on into a Land tab).
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
    // THE CONVERSATION'S LAST WORDS CLAIM THE WORK IS DONE (Cruise; its task
    // not landed by its record): the carry-on keeps its facts and types the
    // Cruise prompt in place of `keep going`, as the turn-end policy would at
    // that point (the live lab's LIVE-RL-1: a crash's restore typed `keep
    // going` after `All tasks are complete and verified; nothing is left to
    // do.`). Where that report names an irreversible act, the prompt says to
    // approve nothing irreversible, as the policy's does (RR4-RL-3).
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
                // A person at the keyboard wins: held, the continuation stays
                // owed and the next idle point types it.
                if super::upgrade_drive::held(c, tab, opts.human_grace_s) {
                    return said(r, "wait:held");
                }
                // UNDER THE HARNESS'S HAND: the caller's, else one for the
                // typing alone; a tab another driver holds is a wait, the
                // continuation still owed.
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
    // Typed, and saved before anything else is read: a step dying from here
    // leaves the restart done, never in flight (which would tell the agent
    // twice). Only the model after is owed, a later step's read ([`confirm`]);
    // this step says `continued` (`upgrade_drive::typed`).
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

/// A QUIET RESTART carried on with NOTHING ([`resumes_quietly`]:
/// `done:resumed`): done on build `version`, process `pid` holding the
/// conversation, the outcome naming the model it ran before
/// ([`quiet_outcome`]), and ONE LOOK at what the resumed build reports did
/// not come back left open for [`RESTORE_LOOK`] ([`restore_look`]).
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

/// Why a conversation nobody but the harness asked anything is owed no
/// carry-on ([`nothing_owed`]'s `done:taskless`; an [`Adopted::NothingOwed`]
/// row says it too).
pub(super) const NOBODY_ASKED: &str = "nobody has asked this conversation anything";

/// Why a relaunch whose conversation's record says its task landed is
/// carried on with nothing ([`nothing_owed`]): the record's words, a clean
/// defense answering a land act — which cannot see the repository, so it
/// says no more than that.
pub const RECORD_LANDED: &str = "its record says it defended its task as landed and nobody has asked \
                                 anything since (the tab's loop checks the repository)";

/// WHETHER THE CARRY-ON OWED TO THE RESTART `st` RECORDS IS OWED NO LONGER,
/// by what the conversation's own records say — in [`carry_on_once`]'s
/// order, the word it ends on and why — or `None` where it is still owed.
/// It types nothing and reads no screen, so it is asked off the host's
/// point too ([`settle`]). `written`: the conversation's record, with what
/// the harness typed into it ([`conversation_typed_by`]), read once
/// ([`WhereWritten`]).
fn nothing_owed(
    opts: &Opts,
    written: &WhereWritten<'_>,
    tab: &str,
    st: &St,
) -> Option<(&'static str, &'static str)> {
    let (records, key, ours) = (written.records(), written.session(), written.ours());
    // A conversation nobody but the harness asked anything is carried on with
    // nothing: a carry-on would start a task nobody asked for.
    if records.tasked(key, ours) == Some(false) {
        return Some(("done:taskless", NOBODY_ASKED));
    }
    // A conversation whose own record says its task LANDED — a clean
    // defense answering a land act, and nobody has asked anything since — is
    // carried on with nothing, whatever brought it back (a crash, aterm's own
    // end, the memory restart, a stall, an asked restart, a model or account
    // move): a carry-on would reopen a landed task (FOREVER LAND: nothing is
    // typed until a person's or an orchestrator's turn). The tab's loop reads the same record at its point
    // ([`upgrade::FinishedScan`]); this one read starts
    // `upgrade_drive::FINISHED_ONCE_BYTES` back, and further back where that
    // window settles nothing ([`upgrade::FinishedScan::settled`]). The
    // record speaks of the words alone — it cannot see the repository, the
    // landing's fourth condition — so the word says what the record says,
    // and the tab's loop holds the landing to the repository at its point
    // (a dirty one is nudged there).
    if written.finished() == Some(true) {
        return Some(("done:finished", RECORD_LANDED));
    }
    // A PERSON'S OWN EXCHANGE, LEFT TO THEM by the loop of a tab that held
    // the conversation, is still its last turn (the review's RR3-TE-2): the
    // carry-on (`nobody is here to answer: decide for yourself … keep
    // going`) would type into their exchange and invite work nobody asked
    // for. Nothing is typed; the tab's loop keeps it theirs.
    if left_to_person(opts, records, tab, key, st) {
        return Some(("done:left", "its last turn was a person's own exchange"));
    }
    // THE TAB'S LOOP RESUMED THE RELAUNCHED AGENT FIRST (the live lab's
    // LIVE-RL-2): its first idle point was a wall, where the host's step is
    // withheld, and the turn-end policy typed its own act there (a retry, a
    // `/compact`, a continuation). The carry-on owed all along would be a
    // second continuation of one resume, typed inside the back-off the
    // policy set — onto a done report, the lab's L4b. Nothing is typed; the
    // point is the loop's, and the one look at what did not come back stays.
    if loop_typed_since(opts, tab, relaunched_line_at(st)) {
        return Some(("done:carried", "the tab's loop resumed it first"));
    }
    None
}

/// The restart `st` records, carried on with NOTHING TYPED — `word`, for
/// `why` ([`nothing_owed`]): done on build `version`, process `pid` holding
/// the conversation, its one look at what did not come back kept open
/// ([`look_after_nothing`]), ledgered and saved.
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

/// When the relaunch line of the restart `st` records was typed (unix
/// seconds; `0`: unknown): its phase's stamp, else [`St::relaunched_at`].
fn relaunched_line_at(st: &St) -> u64 {
    match st.phase {
        upgrade::Phase::Relaunched { at_s } => at_s.max(st.relaunched_at),
        _ => st.relaunched_at,
    }
}

/// Whether tab `tab`'s loop typed a text of its own into the agent after
/// unix second `since` — the relaunch line's — by its approval ledger
/// (`approvals::typed_texts`): a turn-end act that resumed it (a wall's
/// retry, `/compact`, a continuation, an answer). Not a restart's own row
/// (`relaunch --model …`, `relaunch under account …`: written once its
/// relaunch line is typed, it is this relaunch, never a resume of it), nor
/// the accounts probe's `/usage`, which resumes nothing. `since` unknown
/// (`0`), or no ledger root, says no.
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

/// Type the continuation once, fenced on a fresh read with an empty composer
/// ([`typing_fence`], [`turn_fenced`]): a moved screen or typing through the
/// yield is re-read up to [`CONTINUE_FENCE_TRIES`] times; a draft, box or
/// refusal types nothing. A screen that never holds still is typed unfenced
/// after one more look at the composer, since a resumed session without its
/// continuation is the outcome to avoid.
fn type_continuation(c: &mut Client, tab: &str, text: &str, fence_every: Duration) -> bool {
    for _ in 0..CONTINUE_FENCE_TRIES {
        let generation = match typing_fence(c, tab) {
            Ok(generation) => generation,
            // The composer moved between the fence's two reads: the same word
            // and retry as the host's `changed`.
            Err("changed") => {
                pause(fence_every);
                continue;
            }
            Err(_) => return false,
        };
        match turn_fenced(c, tab, text, generation.as_deref()) {
            Ok(()) => return true,
            Err(Typed::Changed | Typed::Yielded) => pause(fence_every),
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
    // A quiet restart's look, or that of a carry-on that typed nothing
    // ([`restore_look`]): a sweep reads it, and types its line only into a
    // tab whose own Autopilot is on, under the tab's own guards.
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

/// How long a quiet restart's look stays open ([`restore_look`]): the
/// resumed build writes its warning as it resumes — a second after
/// `done:resumed` in the one measured case (2026-10-15) — so a short window,
/// never [`MODEL_WAIT`]: no point of the loop's waits on it.
pub(super) const RESTORE_LOOK: Duration = Duration::from_secs(15);

/// How long past [`RESTORE_LOOK`] a warning still owed its line may wait for
/// an idle point of the tab's to be typed at ([`restore_look`]); past it the
/// fact is recorded and nothing is typed.
const RESTORE_TYPE_WITHIN_S: u64 = STALE_S;

/// Why a warning [`restore_look`] found is told the restarted conversation
/// NOTHING: its task is finished, or nobody has asked it anything — the
/// fact is recorded instead ([`restore_lost`]). `None`: a task still open,
/// told in one line at the tab's idle point.
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

/// What the record of a restart that lost something says of it, after the
/// outcome it had.
const RESTORE_LOST: &str =
    "Claude Code reported that something this session ran did not come back after the restart";

/// A QUIET RESTART'S ONE LOOK (`[harness] upgrade_ask = false`, `done:resumed`;
/// [`St::confirming`] on a quiet record, open [`RESTORE_LOOK`]): the rows the
/// resumed build wrote past the restart's mark, read for its own warning
/// that something did not come back ([`upgrade::resumed_build_warned`]:
/// written by that build, at or after the relaunch and within the look).
/// "Resumed as it was" is not true of what Claude Code runs in its own
/// process — an Artifact comment monitor, a session's cron — which no gate of
/// the restart sees (MEASURED 2026-10-15: the monitor did not resume, and the
/// owner's comments went unanswered).
///
/// ANY RESTART WHOSE CARRY-ON TYPED NOTHING keeps the same look
/// ([`looks_back`], [`St::look_word`]: a relaunch on a crash, aterm's own
/// end, a memory, stall or model restart into a finished or taskless
/// conversation, or one whose continuation could not be typed — XREV-LC-2):
/// what the ended process ran is no less gone there. A carry-on that WAS
/// typed says so itself ([`upgrade::RELAUNCH_RESTORES`]).
///
/// With none by the look's end, the restart keeps its last word
/// (`done:resumed`, or the carry-on's), nothing typed. With one, a
/// conversation whose task is FINISHED, or that nobody has asked anything,
/// gets nothing typed — the fact goes on the restart's record and ledger
/// (`done:lost`), and the tab's host raises it for the owner — and one with a
/// task still open gets ONE host line in aterm's own words
/// ([`upgrade::restore_prompt`], `continued:restore`), typed under every
/// guard the carry-on's is: the process still holds the conversation, idle,
/// in its tab, no person or other hand on it. Its answer is the harness's own
/// turn. A sweep (`tab` `None`) decides nothing a tab's look still can: the
/// tab's look at its next idle point does, until [`RESTORE_TYPE_WITHIN_S`]
/// past the look, after which the fact is recorded instead. A sweep types
/// the open task's line itself only into a tab whose own `status` reads its
/// effective Autopilot Cruise or Land, read then ([`autopilot_on`]; the
/// review of the cold restore's restore line, KR-3), under every guard the
/// tab's look types under and the sweep's lock: that tab is the harness's to
/// type into, and a CLI-only sweep (no window host, so no tab look) said it
/// only once the bound passed. Off, `-` or an unreadable status: nothing
/// typed, as before. `None`: the look is still open. `tab_probe` is the seam
/// for the tab's claim on the process, as [`carry_on_with_tab_probe`]'s.
/// Whether the task landed is read once, from
/// `upgrade_drive::FINISHED_ONCE_BYTES` before the transcript's end and from
/// further back where that window settles nothing: a landing with more of
/// Claude Code's wakes after it than that is still read.
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
        // The carry-on's own last word again where it typed nothing
        // ([`St::look_word`]), else the quiet restart's.
        let word = match std::mem::take(&mut st.look_word) {
            w if w.is_empty() => "done:resumed".to_string(),
            w => w,
        };
        save(opts, key, st);
        return Some(said(r.clone(), word));
    }
    let quiet = told_nothing(opts, &records, key, st);
    let late = now >= st.confirm_by.saturating_add(RESTORE_TYPE_WITHIN_S);
    // A SWEEP (no tab) records nothing the tab's host can still say at its
    // idle point — the owner's fact is the host's to raise (XREV-LC-2) —
    // until the look's bound has passed with no idle point; it types an
    // open task's line only into a tab whose own Autopilot is on.
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

/// [`restore_look`]'s ONE LINE, typed into tab `tab` over `c` under every
/// guard a carry-on's is: the process still holds the conversation, idle, in
/// its tab (`tab_probe`), no person or other hand on it. `Ok` once decided —
/// typed (`continued:restore`), or a line that could not be typed (the fact
/// recorded, `done:lost`) — and `Err` the wait's word where a guard stands.
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

/// Whether the resumed build of the restart `st` records has written its own
/// warning that something did not come back ([`restore_look`]): past the
/// restart's mark, at or after the relaunch and within the look's window
/// ([`RESTORE_LOOK`] from [`St::done_at`]).
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

/// [`restore_look`]'s record of a warning it typed nothing for, and why:
/// the look ends (`done:lost`), its fact on the restart's record and ledger.
fn restore_lost(opts: &Opts, r: &Report, st: &mut St, key: &str, why: &str) -> Report {
    st.confirm_by = 0;
    st.look_word.clear();
    st.outcome = format!("{} · {RESTORE_LOST}; nothing typed: {why}", st.outcome);
    let r = said(r.clone(), "done:lost");
    ledger(opts, &r, &st.outcome);
    save(opts, key, st);
    r
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
/// restart's mark ([`upgrade::transcript_first_model`]), in the agent's own
/// `records`. One read, never a wait. `None`: no mark, no transcript, or no
/// answer yet.
fn resumed_model(records: &Records, session: &str, mark: u64, version: &str) -> Option<String> {
    if mark == 0 {
        return None;
    }
    let path = records.transcript(session)?;
    upgrade::transcript_first_model(&since(&path, mark, SINCE_BYTES), version)
}

/// The record of process `pid` in `records` while it holds conversation
/// `key`.
pub(super) fn continuation_session(records: &Records, pid: u32, key: &str) -> Option<SessionFile> {
    records.session_file(pid).filter(|sf| sf.session_id == key)
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

/// The [`St::cause`] of a relaunch after the stall's remedy ended an agent that had stopped reading
/// its input ([`after_exit`] with [`super::restart_stamp::Cause::Stall`]).
pub(super) const CAUSE_STALL: &str = "stall";

/// The [`St::cause`] of a relaunch after the explicit restart ended an agent that was not stalled
/// (`aterm ctl restart`; [`after_exit`] with [`super::restart_stamp::Cause::Asked`]). Like
/// [`CAUSE_STALL`], filed after the exit (the act's own record is its stamp), so not a restart that
/// ended its agent ([`ends_its_agent`]).
pub(super) const CAUSE_ASKED: &str = "asked";

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

/// The [`St::cause`] of a restart under another of the owner's accounts, `account:` then the label
/// ([`Restart::Account`]) and nothing else of the account. The login directory is on the record's
/// line ([`St::line`], [`St::login`]) and in the upgrade ledger's `detail` of rows that quote the
/// typed line (`relaunched`, `exited`, `failed:relaunch:…`): a path under the owner's state
/// directory, never a credential. The supervisor's journal and approval ledger know the label only.
pub(super) const CAUSE_ACCOUNT: &str = "account:";

/// What an account move answers where a restart of another kind was in flight for the conversation
/// ([`restart_as`], [`carried_word`]): that restart was carried on (its own line, under its own
/// process's login) and the move was not tried. A `wait:`, so the loop asks again and nothing is
/// latched against the account; by then the move is made on the process the carry brought up.
pub const IN_FLIGHT_OTHER: &str = "wait:restart-in-flight";

/// The word [`restart_as`] answers after carrying the conversation's in-flight restart (cause
/// `cause`, the carry's word `step`) instead of making the restart `why`. An account move over a
/// restart that is not that very move answers [`IN_FLIGHT_OTHER`] for every word but a failure
/// (`adopted` included, which never says under which login); everything else answers `step`
/// unchanged. Pure.
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

/// What a restart in place ([`restart_from`]) asks for and records, by `why`: the model its line
/// asks for, its [`St::cause`], the launch's own `--model` (`launched`) and the fallback origin
/// ([`St::fallback_from`]), read off the conversation's last record `prior` ([`fallback_origin`]).
///
/// * Memory: the model riding it (`riding`), else a hand choice (`hand`); the origin is carried
///   ([`fallback_carried`]).
/// * A fallback: its model; the origin stands, else is set to the hand choice, else the launch's.
/// * The way back: a hand choice made since, else the origin, else the model the bucket's notice
///   named, else none (`--model` dropped); the origin is spent.
/// * Another account: as memory, and where a fallback stands it is the way back (the origin, then
///   spent). The fallback answered the old account's bucket, and the supervisor drops the switch
///   with the move (`TurnEndState::restarted`), so nothing else would return it.
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

/// WHETHER A RESTART MOVES THE SESSION ONTO A MODEL BILLED TO USAGE CREDITS:
/// a model switch — the fallback ([`Restart::Model`]) or the return
/// ([`Restart::ModelBack`]) — whose line asks for `model` ([`model_restart`])
/// of a family in `billed` ([`super::upgrade_models::credits_billed`], read
/// only under `[harness] usage_credits = false`). A restart for any other
/// reason keeps the model the session rides, and moves it onto nothing.
/// Unknown billing (`None`) cannot authorize either kind of model move.
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
/// [`resumed_prompt`]; an account move's is none, [`carried_quietly`]) rather
/// than the upgrade's. [`CAUSE_HOST`] counts.
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

/// Whether a record of `cause` was filed by a restart that ended its agent itself: the upgrade's
/// SIGTERM (cause empty, or [`CAUSE_UPGRADE_FRESH`]) or a restart in place ([`CAUSE_MEMORY`],
/// [`CAUSE_MODEL`], [`CAUSE_MODEL_BACK`], [`CAUSE_ACCOUNT`]), not a relaunch on exit, whose agent
/// was gone before its record was made. An allowlist: a new cause is none until it is named here.
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

/// [`resumed_prompt`] for the agent `agent` names: Claude Code's, and the
/// Muse Code lane's relaunch (`Muse Code`), whose continuation its lane
/// types. No other lane types a continuation into another agent.
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

/// The restart in place: the Claude Code in tab `opts.only_sid`, at its supervisor's idle point,
/// ended and relaunched on the same program on its own conversation, the host's answer to a point
/// nothing typed can answer ([`Restart`]). The upgrade's restart without its announcement (a
/// process past saving reads nothing), with every last look the upgrade takes before its signal:
/// the same process, idle by Claude's record, its shell's foreground job on a terminal only this
/// aterm tab claims, the composer empty, no background work under it, nobody's hand, never this
/// process's own ancestry. The relaunch is [`relaunch`] handed back to the asking loop (`adopted`),
/// whose next idle point types the continuation ([`resumed_prompt`]; nothing for an account move,
/// [`carried_quietly`]). A restart left in flight is carried first ([`carry_in_flight`]), and an
/// account move asked over one of another kind is then not tried ([`IN_FLIGHT_OTHER`]).
///
/// One word per step: `adopted` (`relaunched` without a loop), `would-restart[:model=<m>]` in a dry
/// run, `wait:<why>`, `busy:<why>`, `refused:<what>` or `failed:<why>` (the loop escalates).
#[must_use]
pub fn restart_here(opts: &Opts, why: &Restart) -> Report {
    restart_from(opts, why, snapshot(opts))
}

/// [`restart_here`] for a host that keeps the owner's roster of Claude Code accounts. The one
/// difference is [`Restart::Account`], whose label the roster turns into the login the line carries
/// ([`accounts::login_env`]: the row's directory verbatim, or the empty value for the row with
/// none). Only the roster knows the directory; the supervisor's policy, journal and ledger know the
/// label. A label the roster no longer lists is `refused:account-not-listed` before any lock or
/// look, as is every account move asked through [`restart_here`], which has no roster.
#[must_use]
pub fn restart_under(opts: &Opts, why: &Restart, roster: &accounts::Roster) -> Report {
    restart_as(opts, why, roster, snapshot(opts), &Live)
}

/// Whether the process `snap` was read from runs under the account the roster labels `label`: its
/// exec environment names that row's login ([`accounts::Roster::row_for_login`], byte for byte;
/// unset and empty both mean the row with no directory). A host checks it on a snapshot of the new
/// process after an account move's relaunch is adopted: `adopted` never says which login it runs
/// under, and a restart of another kind left in flight answers `adopted` for its own line. Pure.
#[must_use]
pub fn runs_under(snap: &Snapshot, roster: &accounts::Roster, label: &str) -> bool {
    roster
        .row_for_login(snap.login.as_deref())
        .is_some_and(|row| row.label == label)
}

/// The login a relaunch with no process to read (a cold restore) carries, per the accounts design
/// §6: the machine's active account's, as [`account_login`] gives it (the empty value, the default
/// login named, for the row with no directory). A host asks it for an agent aterm's own end took
/// ([`after_host_ended`]) whose leaf names neither a login nor a Claude home (`aterm-gui`'s
/// `RestoredAgent::unread`); without it the agent would come back on whatever its reopened shell
/// has. Pure: the roster, state and enrolled set are the caller's reads.
///
/// `None` (nothing added) unless all of these hold:
///
/// * `default_home`: the leaf's Claude home is the default one (the roster judges no other);
/// * the roster lists two or more accounts enrolled on this machine (with one there is nothing to
///   choose between);
/// * `active` names one of them, proven (it has served a turn, or the owner pinned it) and not
///   known signed out: an account nobody has been served by is its canary's to try, never a
///   restored tab's.
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
    // An account move's target, from the roster alone, settled before the lock and before the
    // agent is looked at: a label nobody lists is refused for good.
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
    // The tab's own program ([`NO_SHELL`]): ended, it would come back in no
    // shell, so it is never ended to be restarted. Defence in depth: [`plan`]
    // refuses it too, with the same word and before any signal; this answers
    // before a record is read or a lock contended.
    if snap.shell == NO_SHELL {
        return said(r, "refused:not-a-shell-job");
    }
    // Claude's own record of this process names the conversation, the build
    // and whether it is idle — kept under the Claude home it runs under.
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
        // Carried on its own line. Where the restart asked for is an account move and the one in
        // flight is not that move, the move was not tried, and says so ([`IN_FLIGHT_OTHER`]).
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
    // A MODEL THE ACCOUNT BILLS TO USAGE CREDITS, under `[harness]
    // usage_credits = false`: no fallback and no return is made onto it
    // ([`moves_onto_credits`]); the policy waits, as at a fallback the host
    // cannot make.
    if moves_onto_credits(
        why,
        model.as_deref(),
        credits_billed_for(opts, snap.profile.as_ref()).as_deref(),
    ) {
        return said(r, "refused:model-on-credits");
    }
    let mode = resume_mode(&records, &session, tab_mode(opts, &tab));
    // The login the line carries: an account move's target, else exactly
    // what this process had (unset: nothing is added).
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
        // The home this process ran under, as it had it: a move changes
        // the login, never the home.
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
    // The relaunched process files its record under the same home ([`St::keep_home`]).
    st.keep_home(&records, &opts.home);
    // On its own program: the owner's hold and how long the session has been behind are the
    // upgrade's still ([`St::kept_by_restart`]).
    if let Some(prior) = prior.as_ref() {
        prior.kept_by_restart(&tab, &mut st);
    }
    let tap_opts;
    let plan_opts = match (&opts.usage_tap, launch.login) {
        (Some(armed), Some(dir))
            if !dir.is_empty()
                && armed.lone_login.is_none()
                && (roster.row_for_login(Some(dir)).is_some()
                    || roster.row_left(Some(dir)).is_some()) =>
        {
            tap_opts = Opts {
                usage_tap: Some(super::tap::Armed {
                    exe: armed.exe.clone(),
                    lone_login: Some(dir.to_owned()),
                }),
                ..opts.clone()
            };
            &tap_opts
        }
        _ => opts,
    };
    let planned =
        home_carried(&opts.home, snap.home.as_deref(), &sf.cwd, &records).and_then(|homedir| {
            let launch = Launch {
                homedir: homedir.as_deref(),
                ..launch
            };
            plan(plan_opts, &launch, snap.shell, &t)
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
    // The harness's hand from the last look to the relaunched agent's first
    // idle ([`super::upgrade_drive::Hand`]), given back on every way out.
    let Some(mut hand) = Hand::take(&mut c, &tab) else {
        return said(r, "wait:held");
    };
    let aborted: Option<Report> = 'signal: {
        // The last look before the irreversible act.
        let again = records.session_file(snap.pid);
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
        if let Err(why) = records.unique_owner(&sf) {
            break 'signal Some(said(r.clone(), format!("wait:{why}")));
        }
        let Ok(pid) = i32::try_from(snap.pid) else {
            break 'signal Some(said(r.clone(), "wait:pid"));
        };
        // The model the agent ran and where its transcript ended: the
        // resumed session's answer past it says which model it came back on.
        let recent = records
            .transcript(&session)
            .map(|p| tail_to_end(&p, TAIL_BYTES));
        (st.model_before, st.mark) = model_and_mark(recent.as_ref());
        st.pid = snap.pid;
        st.shell = shell;
        st.tab.clone_from(&tab);
        st.line = line;
        st.phase = upgrade::Phase::Exiting { at_s: now_s() };
        // Where it writes its conversation now, while its process still says
        // (a worktree's home under a relative one): its carry-on reads its
        // finish there once it is gone
        // ([`super::upgrade_drive::note_moved_writes`]).
        super::upgrade_drive::note_moved_writes(opts, &snap, &session, &sf.cwd);
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
    /// The job-control shell it is the foreground job of; [`NO_SHELL`] for
    /// an aterm tab's own program, which no shell holds and nothing
    /// relaunches in its tab.
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
    /// THE AGENT IT IS, with what only that agent's relaunch reads
    /// ([`AgentRun`]): Claude Code's, or a Codex's run ([`CodexRun`]).
    pub run: AgentRun,
    /// The dialect of `shell` from its executable ([`shell_dialect`]; a
    /// Codex's as its lane's plan reads it); `None` when unreadable or
    /// unsupported (nushell, tcsh…), which [`plan`] waits on forever. Read
    /// here so [`resumes_on_exit`] and [`carries_restart`] stay pure.
    pub dialect: Option<Dialect>,
    /// THE LOGIN THE PROCESS RUNS UNDER, as its own exec environment names
    /// it: the RAW value of [`accounts::LOGIN_ENV`]. Three states, kept
    /// apart because they are three relaunch lines: `None`, the variable is
    /// unset (an ordinary retained-model/opted-in relaunch adds nothing; a
    /// new model with billing off explicitly removes it); `Some("")`, set and empty (the default login,
    /// NAMED — what a move back to the default account writes, so the
    /// shell's own variable cannot decide); `Some(dir)`, that login's
    /// directory, verbatim. Every relaunch of the agent re-applies exactly
    /// this ([`Launch::login`]) — in place, on exit, after aterm itself
    /// ended — unless the restart is itself an account move. A Codex's is
    /// always `None`: the variable is Claude Code's.
    pub login: Option<String>,
    /// The Claude home it runs under: the raw value of [`accounts::HOME_ENV`] (`CLAUDE_CONFIG_DIR`)
    /// in its own exec environment, `None` where unset. Every relaunch re-applies exactly this
    /// ([`Launch::home`]): dropped, the new process reads another `.claude.json` and, with no login
    /// variable, signs in by another keychain item. Its records are read under it ([`Records`]). A
    /// Codex's is always `None`.
    pub home: Option<String>,
    /// Where its records are: the Claude Code directory `home` names, as the snapshot resolved it
    /// ([`Records::of_process`]: a relative `CLAUDE_CONFIG_DIR`, empty included, against the
    /// directory that holds its record). Every read of the agent's records is made here
    /// ([`Records::of_snapshot`]), and a relaunch line whose `cd` would resolve `home` elsewhere is
    /// refused ([`super::upgrade_drive::home_carried`], `refused:home`). `None`: not read (a
    /// relative home whose working directory could not be read, a Codex, or a snapshot a reopened
    /// layout gave back, read then from its `home` and `cwd`).
    pub claude_dir: Option<PathBuf>,
    /// Whether it authenticates by its stored login, by its own exec environment
    /// ([`accounts::ProcessLogin::of_exec_env`]'s `stored_login`). The usage tap's gate asks it of
    /// the process a line replaces ([`Launch::stored_login`], [`super::tap::gate`]). `false` where
    /// the environment was not read, as for a snapshot a reopened layout gave back: a cold restore
    /// carries no tap.
    pub stored_login: bool,
    /// The exact profile paths and filename selector captured from this
    /// process's own exec environment and resolved records. None is unknown;
    /// it cannot establish billing permission under the key being off.
    /// Live-only: durable and handoff layouts preserve their existing wire
    /// format and restore this as None.
    pub profile: Option<super::profile::Context>,
}

impl Snapshot {
    /// The agent it is of, by its run ([`Snapshot::run`]). No other program
    /// is ever snapshotted ([`snapshot`] refuses one, `no-lane` or
    /// `not-claude`), and a restored layout gives back only an agent with a
    /// lane (`aterm-gui`'s `AgentRestore::snapshot`).
    #[must_use]
    pub fn agent(&self) -> upgrade::Agent {
        self.run.agent()
    }

    /// A Codex's run, `None` for any other agent's: the Codex lane's own read
    /// of a snapshot it took ([`AgentRun::Codex`]). A decision for every agent
    /// matches [`Snapshot::run`] instead.
    #[must_use]
    pub fn codex(&self) -> Option<&CodexRun> {
        match &self.run {
            AgentRun::Codex(run) => Some(run),
            AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => None,
        }
    }

    /// [`Snapshot::codex`], to change.
    pub fn codex_mut(&mut self) -> Option<&mut CodexRun> {
        match &mut self.run {
            AgentRun::Codex(run) => Some(run),
            AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => None,
        }
    }

    /// A Muse Code's run, `None` for any other agent's: the Muse Code lane's
    /// own read of a snapshot it took ([`AgentRun::Muse`]).
    #[must_use]
    pub fn muse(&self) -> Option<&MuseRun> {
        match &self.run {
            AgentRun::Muse(run) => Some(run),
            AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => None,
        }
    }

    /// An agy's run, `None` for any other agent's: the agy lane's own read of
    /// a snapshot it took ([`AgentRun::Agy`]).
    #[must_use]
    pub fn agy(&self) -> Option<&AgyRun> {
        match &self.run {
            AgentRun::Agy(run) => Some(run),
            AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Gemini(_) => None,
        }
    }

    /// A Gemini CLI's run, `None` for any other agent's: the Gemini CLI lane's
    /// own read of a snapshot it took ([`AgentRun::Gemini`]).
    #[must_use]
    pub fn gemini(&self) -> Option<&GeminiRun> {
        match &self.run {
            AgentRun::Gemini(run) => Some(run),
            AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) => None,
        }
    }
}

/// THE AGENT A [`Snapshot`] IS OF, with the run only that agent's relaunch
/// reads: one variant per agent a lane relaunches, so the agent is told by
/// one exhaustive shape, never by which optional run is set. A new lane adds
/// its variant here (and its arm in [`AgentRun::agent`]); a snapshot of
/// another agent is built nowhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRun {
    /// Claude Code: its run is the snapshot's own fields (`login`, `home`,
    /// `claude_dir`, `stored_login`).
    Claude,
    /// A Codex TUI ([`CodexRun`]).
    Codex(CodexRun),
    /// A Muse Code TUI ([`MuseRun`]): relaunched on exit and restored, and
    /// moved onto a newer build live by the quiet flow alone
    /// ([`super::lanes::lanes`], [`super::upgrade_muse_live`]).
    Muse(MuseRun),
    /// An agy TUI ([`AgyRun`]): relaunched on exit and restored, and moved
    /// onto a newer build live by the quiet flow alone
    /// ([`super::lanes::lanes`], [`super::upgrade_agy_live`]).
    Agy(AgyRun),
    /// A Gemini CLI TUI ([`GeminiRun`]): relaunched on exit and restored,
    /// never upgraded live ([`super::lanes::lanes`]).
    Gemini(GeminiRun),
}

impl AgentRun {
    /// The agent whose run it is.
    #[must_use]
    pub fn agent(&self) -> upgrade::Agent {
        match self {
            AgentRun::Claude => upgrade::Agent::Claude,
            AgentRun::Codex(_) => upgrade::Agent::Codex,
            AgentRun::Muse(_) => upgrade::Agent::Muse,
            AgentRun::Agy(_) => upgrade::Agent::Agy,
            AgentRun::Gemini(_) => upgrade::Agent::Gemini,
        }
    }
}

/// What a Muse Code TUI's relaunch on exit needs beside the [`Snapshot`],
/// whose `session` is the session its witness proved it holds (the
/// `.session.lock` at session depth its process holds open and whose body
/// names its pid, `upgrade_drive`'s `muse::witness`; `None`: none proven, and
/// no relaunch then resumes anything) — measured on 1.4.10-R6427.1
/// ([`super::upgrade_muse`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuseRun {
    /// THE DATA HOME ITS SESSIONS ARE UNDER (canonical): its own
    /// `XDG_DATA_HOME`, else its `HOME`'s `.local/share`
    /// ([`super::upgrade_muse::data_home`]). Its sessions are
    /// `<data>/muse/sessions/YYYY/MM/DD/<uuid>/`.
    pub data: PathBuf,
    /// THE HOMES ITS RELAUNCH LINE CARRIES, as its own exec environment set
    /// them, verbatim (`XDG_DATA_HOME`, `XDG_CONFIG_HOME`;
    /// [`super::upgrade_muse::line_env`]): a Muse started with another data
    /// home keeps its session there, and a line without it resumes nothing.
    pub env: Vec<(String, String)>,
    /// THE SESSION'S WORKSPACE as its own log last named it
    /// ([`super::upgrade_muse::log_workspace`]): the relaunch line `cd`s there
    /// first (from another directory, a resume asks to trust that directory
    /// and moves the session's workspace to it). `None`: not read, and the
    /// relaunch reads it then, else uses [`Snapshot::cwd`].
    pub workspace: Option<String>,
    /// The session's `.session.lock` the witness read; `None` for none, or a
    /// snapshot a restored layout gave back (found again by its session).
    pub lock: Option<PathBuf>,
    /// When it started, unix milliseconds (to the second; `0`: unread): a
    /// lock naming its pid that was written before it started is an earlier
    /// process's, never its (the follow's and the exit's reads of the lock,
    /// which ask no `lsof`).
    pub started_ms: u64,
    /// THE SHELL-INTEGRATION COMMAND BLOCK IT RUNS IN (its `id`, read while
    /// it runs), or `None` where none could be read: its exit's word is THAT
    /// block's (`upgrade_muse::exit_kind` of its `exit`). Never carried across
    /// a restore.
    pub block: Option<u64>,
}

/// What an agy TUI's relaunch on exit needs beside the [`Snapshot`], whose
/// `session` is the conversation its witness proved it holds (the presence
/// lock `<config>/presence/<uuid>.lock` its process holds open,
/// `upgrade_drive`'s `agy::witness`; `None`: none proven — a fresh agy that
/// took no turn has none — and no relaunch then resumes anything) and whose
/// `cwd` is its workspace (agy's is its launch directory, measured) —
/// measured on 1.4.0 ([`super::upgrade_agy`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgyRun {
    /// THE CONFIG DIRECTORY ITS CONVERSATIONS ARE UNDER (canonical):
    /// `$HOME/.gemini/antigravity-cli`, as its own `HOME` and the hidden
    /// `--gemini_dir`/`--app_data_dir` of its argv place it
    /// ([`super::upgrade_agy::config_dir`]). Its presence locks are
    /// `<config>/presence/`, its crash files `<config>/crashes/`.
    pub config: PathBuf,
    /// THE HOME ITS RELAUNCH LINE CARRIES (`HOME=<dir>`, where its own exec
    /// environment set another than the user's;
    /// [`super::upgrade_agy::relaunch_env`]): an agy under another `HOME`
    /// keeps its conversations there, and a line without it starts fresh.
    pub env: Vec<(String, String)>,
    /// The conversation's presence lock the witness read; `None` for none, or
    /// a snapshot a restored layout gave back (named again by its
    /// conversation).
    pub lock: Option<PathBuf>,
    /// THE CRASH FILE IT HOLDS OPEN (`<config>/crashes/crash_<pid>_<uuid>.log`,
    /// read with its open files): its exit is a crash only where this very
    /// file is left ([`super::upgrade_agy::exit_kind`]). `None`: not read —
    /// its exit's look then reads any crash file of its pid written since it
    /// started.
    pub crash: Option<PathBuf>,
    /// When it started, unix milliseconds (to the second; `0`: unread): a
    /// crash file of its pid written before it started is an earlier
    /// process's, never its.
    pub started_ms: u64,
    /// THE SHELL-INTEGRATION COMMAND BLOCK IT RUNS IN (its `id`, read while
    /// it runs), or `None` where none could be read: its exit's word is THAT
    /// block's. Never carried across a restore.
    pub block: Option<u64>,
}

/// What a Gemini CLI TUI's relaunch on exit needs beside the [`Snapshot`],
/// whose `pid` is the PARENT node (the tab's foreground job, `node <gemini
/// script>`), whose `session` is the session the start-time rule named (the
/// one session header of its project begun since it started,
/// `upgrade_drive`'s `gemini::witness_of`; `None`: none named, and no
/// relaunch then resumes anything) and whose `cwd` is its project (the
/// directory its sessions are kept under) — measured on 0.46.0
/// ([`super::upgrade_gemini`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeminiRun {
    /// THE GEMINI DIRECTORY ITS SESSIONS ARE UNDER (canonical):
    /// `<GEMINI_CLI_HOME or HOME>/.gemini`, as its own exec environment
    /// places it ([`super::upgrade_gemini::gemini_dir`]).
    pub home: PathBuf,
    /// ITS PROJECT'S CHATS DIRECTORY (`<home>/tmp/<slug>/chats`, the slug
    /// whose `.project_root` names its project); `None`: none found, and no
    /// session is named.
    pub chats: Option<PathBuf>,
    /// THE HOMES ITS RELAUNCH LINE CARRIES (`GEMINI_CLI_HOME` where set,
    /// `HOME` where another than the user's;
    /// [`super::upgrade_gemini::relaunch_env`]): a Gemini CLI under another
    /// home keeps its sessions there, and a resume without it exits 42.
    pub env: Vec<(String, String)>,
    /// WHEN THE PARENT STARTED, unix milliseconds, never later than it did
    /// (`0`: unread): the start-time rule's one input from the process.
    /// Carried across a restore, so the rule is read again there.
    pub started_ms: u64,
    /// The child node it started (`0`: none found): orphaned when the parent
    /// is SIGKILLed, it reads the terminal until its shell takes it back.
    pub child: u32,
    /// WHY THE START-TIME RULE NAMED NO SESSION, where it named none
    /// ([`super::upgrade_gemini::NoWitness`]); `None` where it named one, or
    /// a snapshot a restored layout gave back.
    pub unwitnessed: Option<super::upgrade_gemini::NoWitness>,
    /// THE SHELL-INTEGRATION COMMAND BLOCK IT RUNS IN (its `id`, read while
    /// it runs), or `None` where none could be read: its exit's word is THAT
    /// block's. Never carried across a restore.
    pub block: Option<u64>,
}

/// What a codex-alab's relaunch needs beside the [`Snapshot`] (whose
/// `session` is its thread, when nameable). Only an ALab build is
/// snapshotted as a Codex ([`snapshot`]; a vendor `codex` is `no-lane`, the
/// Codex screen lane's retirement, 2026-10-16), and it comes back only on
/// the line aterm's copy of its own record validated ([`Self::self_claimed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexRun {
    /// The `$CODEX_HOME` it runs with (canonical).
    pub home: PathBuf,
    /// THE `CODEX_HOME` ITS OWN EXEC ENVIRONMENT SET, verbatim; `None`: it
    /// had no such variable. `home` cannot tell a variable set to
    /// `~/.codex` from the default. Its relaunch line carries it as
    /// REQUIRED environment ([`super::codex_alab::line_env`]): a TUI
    /// started `CODEX_HOME=<dir> codex` keeps its thread under `<dir>`, and
    /// relaunched at a shell without the variable it would resume under
    /// `~/.codex`, where that thread does not exist.
    pub home_env: Option<String>,
    /// THE CRASH RULE'S VALIDATED LINE (the codex-alab self-claim contract,
    /// §4): where a proven self-claim supervised this codex-alab and aterm's
    /// own copy of its session record validated its resume line against the
    /// live process ([`super::self_claimed::validate`]), that line — carried
    /// by a relaunch OWED on it and by a restore layout's leaf, so a relaunch
    /// after aterm's own end, or a successor's after a seamless update's
    /// Commit, brings it back on its thread exactly as the crash rule would
    /// ([`after_host_ended`] relaunches it through
    /// [`super::upgrade_drive::self_claimed_relaunch`]). `None` for every
    /// other Codex, which nothing relaunches. The live read never sets it:
    /// the window host attaches it from its copy.
    pub self_claimed: Option<super::self_claimed::Resume>,
}

/// Whether the host's relaunch would bring back what the EXPLICIT RESTART of
/// `snap`'s agent ends (`ExitCause::Asked`, `ExitCause::Stall`), as far
/// as a live read can tell — the act refuses where not (aterm's
/// `restart_act`, `not-relaunchable`), and no word names it. No process is
/// read; a Claude Code's reads as [`resumes_on_exit`] does, and an agy's
/// store build reads the managed twin in atpkg's store (`aterm.toml`'s
/// prefix and the twin's script).
///
/// A Claude Code's where [`resumes_on_exit`]. A Codex's only where its
/// snapshot carries the line aterm's copy of codex-alab's own record
/// validated ([`self_claimed_line`]): the one line a Codex is ever
/// relaunched on (the Codex screen lane's retirement, 2026-10-16). With no
/// such line the act would end what nothing brings back. An agy's where its
/// lane's relaunch would bring the conversation back
/// ([`super::upgrade_drive::agy_carries_restart`]), the program its line types
/// included: a store build with no managed twin is refused by its relaunch
/// (`refused:no-managed-agy`), so not ended for it. A Muse Code's never yet.
/// Never a tab's own program's ([`NO_SHELL`]): nothing relaunches it in its
/// tab.
///
/// ONE PREDICATE WITH THE RELAUNCH (LC-3 of ATERM-KI-1015-1): a Claude
/// Code's is [`exit_line`]'s, which asks everything the relaunch refuses on
/// before it types — the home its line carries included
/// ([`super::upgrade_drive::home_carried`], for a user whose home is
/// `user_home`). Asked without it, the act answered `OK` and `SIGTERM`ed an
/// agent under another `HOME` whose relaunch then said `refused:home`,
/// leaving it at its shell.
#[must_use]
pub fn carries_restart(user_home: &Path, snap: &Snapshot) -> bool {
    match &snap.run {
        AgentRun::Claude => resumes_on_exit(user_home, snap),
        AgentRun::Codex(_) => snap.shell != NO_SHELL && self_claimed_line(snap).is_some(),
        // A Muse Code's never yet: the act ends its agent with SIGTERM, and
        // what a SIGTERM does to a Muse Code in the middle of a turn — the
        // stall's very case — is unmeasured (its exit table was measured on
        // an idle one). So the act answers `restart unavailable` for it, and
        // the stall's remedy says its warning with Muse's own resume line in
        // its place, never ending it (review of the Muse Code lane,
        // 2026-10-17). Its relaunch on exit and its restore are its lane's.
        AgentRun::Muse(_) => false,
        // An agy's where its relaunch would bring its conversation back: the
        // act's SIGTERM is one of agy's graceful ways out, MEASURED in the
        // middle of a turn too (exit 0 in about half a second, its lock
        // released, its crash file removed, the conversation keeping the
        // partial turn, drawn `Interrupted` on its resume), so the stall's
        // very case ends it cleanly and the relaunch resumes it. As far as a
        // live read tells, on the lane's own predicate: a witnessed
        // conversation, a shell a line is written for, a launch whose flags
        // its line carries, a program it can type (a store build only through
        // a managed twin) and a line that holds its `HOME` within the tty's
        // bound, with its heal and, at zsh and bash, its `cd`
        // (`upgrade_agy_drive::carries_restart_onto`).
        // Never a tab's own agy ([`NO_SHELL`]), as a Codex's: no shell is
        // left to type its relaunch into.
        AgentRun::Agy(_) => {
            snap.shell != NO_SHELL && super::upgrade_drive::agy_carries_restart(user_home, snap)
        }
        // A Gemini CLI's never: the act signals the tab's foreground job, its
        // PARENT node, which ignores SIGTERM, SIGHUP and SIGINT (measured:
        // both processes still running 12 s on), so the act would escalate
        // to a SIGKILL that orphans its child. Its relaunch on exit and its
        // restore are its lane's.
        AgentRun::Gemini(_) => false,
    }
}

/// Whether an exit of `snap`'s agent now would be relaunched on its own
/// conversation ([`after_exit`]), as far as a live read can tell: a held
/// conversation ([`upgrade::is_session_id`]), not a one-shot launch, a shell
/// holding the agent ([`NO_SHELL`]: an aterm tab's own program is relaunched
/// in its tab by nothing) of a supported dialect, a line [`line_for`] can
/// make from the argv, and one that carries the Claude home its records are
/// in, for a user whose home is `user_home` ([`exit_line`]). Pure but for
/// that home's rule: the dialect is in the snapshot.
///
/// A frozen Claude Code's remedy says "aterm relaunches it" instead of naming a
/// `claude --resume <id>` command, so this must be true when it says so;
/// `false` sends the remedy back to the command
/// ([`super::upgrade_drive::resume_command_kept`]).
///
/// A Codex snapshot answers `false`: no exit of its own is relaunched by
/// this rule ([`after_exit_as`]); a codex-alab's crash is its copy's to read
/// ([`super::upgrade_drive::self_claimed_after_exit`]).
#[must_use]
pub fn resumes_on_exit(user_home: &Path, snap: &Snapshot) -> bool {
    exit_line(user_home, snap).is_some()
}

/// THE RESUME LINE `snap` ALONE IMPLIES for a relaunch of its agent on its
/// own conversation ([`after_exit`], [`after_host_ended`]) — `None` where
/// [`resumes_on_exit`] promises nothing: no held conversation, a one-shot
/// launch, no shell ([`NO_SHELL`]) or no shell dialect, a Codex, a line
/// [`line_for`] refuses, or a line that would start it under another Claude
/// home than the one its records are in (the plan's `refused:home`, below).
/// It carries everything the relaunch must: the Claude home and the login the
/// snapshot names ([`required_env`]), so a snapshot a restored layout gave
/// back (`aterm-gui`'s `AgentRestore::snapshot`) shows here the environment
/// and the flags its relaunch carries. It is NOT the line the relaunch
/// types: that is decided at exit, by [`plan`] and its caller — on the
/// program [`relaunch_target`] chooses (a newer build where `[harness]
/// upgrade` allows it), resumed only where the conversation has a
/// transcript among its agent's records (else started afresh, no
/// `--resume`), asking for a model or a permission mode the conversation's
/// record names, and with the `cd` and the integration heal the shell it
/// finds needs.
///
/// AND ONLY WHERE THE LINE STARTS IT UNDER THE CLAUDE HOME ITS RECORDS ARE
/// IN ([`super::upgrade_drive::home_carried`], for a user whose home is
/// `user_home`): the relaunch refuses (`refused:home`) a relative home its
/// line would move, and records no `HOME` names, so nothing is promised for
/// either (LC-3 of ATERM-KI-1015-1). An agent under another `HOME` with no
/// `CLAUDE_CONFIG_DIR` is promised: its line carries that `HOME` (LC-4).
#[must_use]
pub fn exit_line(user_home: &Path, snap: &Snapshot) -> Option<String> {
    if snap.shell == NO_SHELL {
        return None;
    }
    match &snap.run {
        AgentRun::Claude => {}
        // Their lanes resume only what is still there at exit, which cannot
        // be promised in advance.
        AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => {
            return None;
        }
    }
    let dialect = snap.dialect?;
    let session = snap
        .session
        .as_deref()
        .filter(|s| upgrade::is_session_id(s))?;
    // A control character in the directory refuses the line either way.
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
        // With the home and the login its relaunch must carry: a line that
        // cannot hold them is refused then, so nothing is promised now. (A
        // `HOME` it carries for an agent under another one is read off its
        // records' directory at exit; its verdict is `home_carried`'s.)
        &required_env(snap.home.as_deref(), None, snap.login.as_deref()),
    )
    .ok()
}

/// THE LINE A PERSON IS TOLD TO RUN to resume the agent `snap` describes by
/// hand, for a user whose home is `user_home` — the tab's mark of an agent
/// aterm cannot bring back, and the loop's memory, frozen-mail and turn-end
/// escalations (`aterm-gui`'s `resume_told`) — through the one rule the
/// footer's remedy goes through too
/// ([`super::upgrade_drive::resume_command_kept`], over the harness's state
/// root `state`: a `--model` the conversation's record marks failed is not
/// printed), CARRYING THE CLAUDE HOME its agent ran under as its relaunch
/// line carries it ([`super::upgrade_drive::home_carried`],
/// [`super::resume::with_env`]; ATERM-KI-1015-1 LC-4: told bare, in a tab
/// whose shell has another home, `claude --resume <id>` found no transcript
/// or met the first-run screen). Never the login
/// ([`super::resume::SHELL_LOGIN_CLAUSE`]). `None` for a Codex (no such
/// line), a launch the rewrite refuses as not resumable in place, and a home
/// no line can repeat (a relative one the line would move): no command
/// beats a wrong one.
#[must_use]
pub fn told_command(user_home: &Path, state: Option<&Path>, snap: &Snapshot) -> Option<String> {
    match &snap.run {
        AgentRun::Claude => {}
        // Each tells its own line (`aterm-gui`'s `resume_told`).
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
    let is_claude = |p: &Path| match p
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(aterm_phase::program_of)
    {
        Some(aterm_phase::Program::Claude) => true,
        Some(
            aterm_phase::Program::Codex
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

/// Read the [`Snapshot`] of the Claude Code (or codex-alab, Muse Code, agy or
/// Gemini CLI) in tab `opts.only_sid` on `opts.sock`: the tab's foreground job
/// must be a job-control shell's job on a terminal only this aterm tab claims
/// — or that tab's own program (`aterm -e claude`, `exec muse`, `exec agy`,
/// `exec gemini`), read with
/// no shell ([`NO_SHELL`]): its readers have it, and nothing relaunches it in
/// its tab.
///
/// # Errors
/// The word naming the first fact that could not be read or did not hold.
pub fn snapshot(opts: &Opts) -> Result<Snapshot, &'static str> {
    snapshot_with(opts, &Live)
}

/// The lane that reads a tab's foreground agent ([`snapshot_with`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SnapshotLane {
    /// Claude Code's records: a `claude`, or a process no reader names (a
    /// Claude Code under `node`, a wrapper).
    Claude,
    /// codex-alab's own path: an ALab build ([`super::codex_alab::alab_bytes`]).
    CodexAlab,
    /// The Muse Code lane's.
    Muse,
    /// The agy lane's.
    Agy,
    /// The Gemini CLI lane's: a `node` image whose `argv[1]` is Gemini CLI's
    /// script ([`super::upgrade_gemini::is_gemini_process`]).
    Gemini,
    /// No lane relaunches it — a vendor `codex` (the Codex screen lane's
    /// retirement, 2026-10-16), an image itself named `gemini` (no such build
    /// was measured): refused ([`NO_LANE`]), never read as Claude Code, nor
    /// as another lane's agent.
    None,
}

/// THE LANE A FOREGROUND PROCESS IS READ BY, from the basename of the image
/// the kernel mapped for it (`image`; `None`: unreadable), and for a Codex
/// whether that image is an ALab build — or, for a script runtime's image,
/// from its `argv` (`argv[1]`: Gemini CLI runs as `node <its script>`, a
/// parent and a child, and `program=` never names it).
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
        Some(aterm_phase::Program::Codex | aterm_phase::Program::Gemini) => SnapshotLane::None,
        Some(aterm_phase::Program::Claude | aterm_phase::Program::Generic) | None => {
            SnapshotLane::Claude
        }
    }
}

/// THE SNAPSHOT OF A CRASHED CODEX-ALAB NO READ REACHED WHILE IT RAN (AD-5 and
/// the second crash of AD-3, the codex-alab end-to-end run of 2026-10-16): a
/// process that claimed and died before the host's snapshot read caught it
/// (the tab's roster still naming the group it replaced) leaves its exit no
/// shell to relaunch at, and the crash rule said "the tab's own program".
/// Its own facts are aterm's COPY of its record, read while its identity was
/// live (`copy`, the line `resume` it validated); the shell is the one that
/// holds the tab NOW, at the exit: the tab's foreground group, led by a
/// job-control shell that holds its terminal ([`Kernel::shell_holds_tab`]),
/// a terminal aterm opened, and no other tab's identity in its environment.
/// A tab whose agent was its spawn command has no such shell and gets none.
///
/// # Errors
/// The word that refused it (`no-tab`, `no-socket`, `roster`,
/// `no-foreground`, `tab-ambiguous`, `no-shell`, `terminal`,
/// `argv-unreadable`, `tab-identity-conflict`).
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

/// The snapshot [`self_claimed_snapshot`] makes (pure): the dead agent's
/// identity and home from its copy, its program and line from the validated
/// `resume`, at `shell`.
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

/// [`snapshot`] with the agent's job and terminal read from `k`
/// ([`Kernel`]): the seam a test reads a stand-in agent through (its
/// process, its exec environment, its working directory and its records
/// read live).
pub(super) fn snapshot_with(opts: &Opts, k: &dyn Kernel) -> Result<Snapshot, &'static str> {
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
    let shell = match k.job(pid) {
        Some((Job::NoJobControl, _)) => return Err("not-a-shell-job"),
        // The tab's own program, on the terminal the aterm that is its
        // parent opened (the job read proved both): read, with no shell to
        // relaunch it into.
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
    let args = atpkg::caller_shell::process_args(pid).ok_or("argv-unreadable")?;
    if args
        .env_var("ATERM_PARENT_SESSION_ID")
        .is_some_and(|env_tab| env_tab != tab)
    {
        return Err("tab-identity-conflict");
    }
    let start = kernel_start(pid).ok_or("start")?;
    let image = exe_of(pid);
    // A codex-alab: its own path reads its run; a Muse Code, an agy and a
    // Gemini CLI (a `node` whose `argv[1]` is its script): their lanes'. A
    // vendor `codex` (the Codex screen lane's retirement, 2026-10-16) is
    // refused here, never read as Claude Code. A tab's own codex-alab, Muse
    // Code, agy or Gemini CLI ([`NO_SHELL`]) is read too, for its readers: no
    // shell's block names its exit, and its lane relaunches nothing from it.
    match snapshot_lane(image.as_deref(), &args.argv) {
        SnapshotLane::CodexAlab => {
            return super::upgrade_drive::codex_alab_snapshot(opts, tab, pid, start, shell, &args);
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
    // Claude's record of this process, under the Claude home its exec environment names; a
    // relative one (empty included) resolves from the directory that holds its record
    // ([`Records::of_process`]).
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
        // The shell's kernel argv only; an unreadable one only costs the
        // remedy its relaunch wording ([`resumes_on_exit`]). None without
        // a shell.
        dialect: (shell != NO_SHELL)
            .then(|| shell_dialect(shell, &[]))
            .flatten(),
        // READ off the process's own exec environment, the same read that
        // gave its argv: what the vendor's runtime was started with, which
        // is what chose its keychain item (the accounts design, §1).
        login: args.env_var(accounts::LOGIN_ENV).map(str::to_owned),
        // The same read: the home it was started under, and where it keeps
        // its records under it.
        home: args.env_var(accounts::HOME_ENV).map(str::to_owned),
        claude_dir: records.dir().map(Path::to_path_buf),
        stored_login: accounts::ProcessLogin::of_exec_env(&args.env)
            .is_some_and(|who| who.stored_login),
        profile: records.billing_context(&opts.home, &args),
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
/// `/resume` move the conversation). One small file read, under the Claude
/// home the agent runs under ([`Records::of_snapshot`]; `home` is the
/// user's). Returns whether anything moved.
pub fn follow(home: &Path, snap: &mut Snapshot) -> bool {
    // A Codex keeps no record of its process to follow: its snapshot is
    // taken again at the loop's idle points, and so is an agy's (its lock
    // names no pid: only `lsof` reads whose it is, never on this cheap
    // follow) and a Gemini CLI's (no file names its pid: the start-time rule
    // reads its project's session files, read again at its exit). A Muse
    // Code's session is followed by its lock ([`follow_muse`]).
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

/// FOLLOW `snap` THROUGH ONE RECORD OF ITS OWN PROCESS, as [`follow`] does
/// through the one on disk: the conversation, build and directory `sf` names
/// — a record read elsewhere, such as the one a crash left
/// ([`ExitRecord::Survived`], read at the exit). A Claude Code's snapshot
/// only, and only a record of that very process (its pid and kernel start):
/// another process's names nothing of this one. Returns whether anything
/// moved.
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

/// What a relaunch on exit runs, `(exe, version, source)`: with `upgrade` —
/// the host's `[harness] upgrade` switch, live — the newest build the
/// session may move to ([`upgrade::choose_target`] over `candidates`, so a
/// crash is also an upgrade), else the program it ran (`same`). Configuration
/// only takes power away: with the switch off, a crash never moves the
/// session onto another build. NOR DOES A BUILD THE CONVERSATION'S UPGRADE
/// RECORD HOLDS IT OFF (`held`, [`St::holds_off`]): the owner's word (round
/// eight, #24) — a build the owner skipped, or any while their `Not today`
/// stands, is no relaunch target, where a crash moved the session straight
/// onto the build the owner had refused; a build newer than a skipped one
/// is ("until a NEWER build is the target") — and a build that failed to
/// start for the conversation, while its stop's rest stands. The caller's
/// `held` also holds off a build not PROVED to start (LC-5 of
/// ATERM-KI-1015-1: `upgrade_build_start::proved`, the upgrade's probe).
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
    /// names the conversation as the agent last held it. Boxed: the record
    /// (where it was read included, `SessionFile::claude_dir`) dwarfs the
    /// other variants.
    Survived(Box<SessionFile>),
    /// The exit removed it: graceful, someone's decision (or no conversation
    /// was ever registered).
    Removed,
    /// The agent CRASHED, by its shell's exit status: a Muse Code's, by its
    /// own table ([`super::upgrade_muse::exit_kind`]), or an agy's, by its
    /// own table and the crash file its dead pid left
    /// ([`super::upgrade_agy::exit_kind`]); the conversation is the
    /// snapshot's. No other agent's end is read this way.
    Crashed,
    /// The agent REFUSED ITS OWN START, by its shell's exit status: a Muse
    /// Code's `1` (its session not found, an unknown session name) or `2`
    /// (its arguments; [`super::upgrade_muse::ExitKind::Refused`]), or an
    /// agy's `2` with no crash file left (its arguments;
    /// [`super::upgrade_agy::ExitKind::Refused`]). Never a crash and never
    /// someone's end: a relaunch would meet the same refusal, so none is made
    /// (critic X4 of the lanes research, 2026-10-17).
    Refused,
}

impl ExitRecord {
    /// THE EXIT WAS A CRASH, read at the exit: a Claude Code's record
    /// survived it, or a Muse Code's or an agy's shell told one
    /// ([`Self::Crashed`]). The
    /// ONE rule both what [`after_exit`] relaunches on and what the window's
    /// host owes through a back-off read (its `Kept::owed`, which a seamless
    /// update carries). An exit nothing read ([`Self::Unread`]) is no crash
    /// yet. A Codex's end is never read here: only aterm's copy of
    /// codex-alab's own record says how it ended (the Codex screen lane's
    /// retirement, 2026-10-16).
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
            Self::Refused => "refused",
        }
    }
}

/// THE EXIT IS ITS LAUNCH'S OWN END, by what it left as it was seen
/// ([`exit_record`]): an exit no relaunch on exit brings back, whatever
/// `[harness] relaunch` says — a one-shot run (`one-shot`), or a graceful
/// exit (`graceful-exit`: Claude removed its record as it ended), someone's
/// decision. `None` for an exit a relaunch on exit would bring back, or would
/// read again at its attempt ([`ExitRecord::Unread`]). [`after_exit`] ends an
/// exit nobody asked for by the same words; what the host's limit asks
/// before it says what the limit took away.
#[must_use]
pub fn launch_ended(snap: &Snapshot, left: &ExitRecord) -> Option<&'static str> {
    // Each agent's one-shot launches by its own table: a Muse Code's `exec`,
    // tools, `--help` and `--version` ([`super::upgrade_muse::is_tui_argv`]),
    // an agy's `-p`, `--output-format`, `--version` and subcommands
    // ([`super::upgrade_agy::is_tui_argv`]), a Gemini CLI's `-p`, `-o`,
    // `--acp`, `--list-sessions`, `-v` and subcommands
    // ([`super::upgrade_gemini::is_tui_argv`]), never Claude Code's flags read
    // over its argv.
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

/// One look at an agent that left its tab ([`look_at_exit`]): whether the same
/// process (pid and kernel start) still runs, and Claude's record of it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitLook {
    /// The snapshot's process still runs.
    pub running: bool,
    /// Its record, when one of its own start is on disk (Claude Code's).
    pub record: Option<SessionFile>,
    /// Whose look it is ([`Snapshot::agent`]): a Muse Code's and an agy's
    /// never read `record`, their shell's word (`crashed`, `refused`) decides;
    /// a Claude Code's is its `record`.
    pub agent: upgrade::Agent,
    /// A Muse Code's or an agy's exit per its shell, by its own table
    /// ([`super::upgrade_muse::exit_kind`]; an agy's with its crash file,
    /// [`super::upgrade_agy::exit_kind`]): `Some(true)` a crash, `Some(false)`
    /// its own exit, someone's signal or a refused start, `None` not told yet
    /// — or told, but deciding nothing at the exit (an agy stopped, or ended
    /// with no witness: read again at the attempt). `None` for every other
    /// agent's look.
    pub crashed: Option<bool>,
    /// A Muse Code's or an agy's shell says it REFUSED ITS START
    /// ([`ExitRecord::Refused`]), with `crashed` `Some(false)`. `false` for
    /// every other look and agent.
    pub refused: bool,
    /// A GEMINI CLI'S OWN RESTART REFUSED FOR ITS OWN SESSION ID, with
    /// `crashed` `Some(true)` (`upgrade_gemini::ExitKind::Restarted`): its
    /// `/settings`, IDE trust or admin settings restart ran a worker's argv
    /// again and met the `--session-id` its first start wrote (`42`). A
    /// person's keystroke before it asked for that restart, never the exit
    /// ([`restarted_itself`]). `false` for every other look and agent.
    pub restarted_itself: bool,
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
/// graceful exit removes its record before the process ends. Its record is
/// read under the Claude home it ran under ([`Records::of_snapshot`];
/// `home` is the user's).
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

/// FOLLOW A MUSE CODE'S SNAPSHOT between the loop's idle points: the session
/// it holds, which `/new` and an in-TUI `/resume` move (one small read of the
/// lock it named while that still names its pid; else the witness again,
/// `upgrade_drive`'s `muse::follow`). Whether it moved; `false`, and nothing
/// read, for any other agent's snapshot.
pub fn follow_muse(snap: &mut Snapshot) -> bool {
    let muse = match &snap.run {
        AgentRun::Muse(_) => true,
        AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => false,
    };
    muse && super::upgrade_drive::muse_follow(snap)
}

/// [`ExitLook`] of a Muse Code `snap` ([`MuseRun`]), live: its shell's exit
/// status for the command it ran in, read over `opts.sock` by Muse Code's own
/// table ([`super::upgrade_muse::exit_kind`]).
#[must_use]
pub fn look_at_muse_exit(opts: &Opts, snap: &Snapshot) -> ExitLook {
    super::upgrade_drive::muse_look_at_exit(opts, snap)
}

/// [`ExitLook`] of an agy `snap` ([`AgyRun`]), live: its shell's exit status
/// for the command it ran in, read over `opts.sock`, and whether its dead
/// pid's crash file is left — read NOW, as the exit is seen, since the next
/// agy start deletes it — by agy's own table
/// ([`super::upgrade_agy::exit_kind`]).
#[must_use]
pub fn look_at_agy_exit(opts: &Opts, snap: &Snapshot) -> ExitLook {
    super::upgrade_drive::agy_look_at_exit(opts, snap)
}

/// [`ExitLook`] of a Gemini CLI `snap` ([`GeminiRun`]), live: its shell's
/// exit status for the command its parent ran in, read over `opts.sock` by
/// Gemini CLI's own table ([`super::upgrade_gemini::exit_kind`]: a start it
/// refused, `41`/`42`/`52`…, is no crash).
#[must_use]
pub fn look_at_gemini_exit(opts: &Opts, snap: &Snapshot) -> ExitLook {
    super::upgrade_drive::gemini_look_at_exit(opts, snap)
}

/// WHETHER THE EXIT IS A GEMINI CLI'S OWN RESTART REFUSED FOR ITS OWN
/// SESSION ID ([`ExitLook::restarted_itself`]), read as the exit is seen,
/// before whose exit it is decided: looked at every [`EXIT_LOOK`] until the
/// shell's word is drawn (`crashed` told), at most [`EXIT_GONE`]. `look` and
/// `wait` are [`exit_record`]'s. `false` for an unreadable look, a process
/// still running, a word not told within the wait, and any other end.
///
/// A person's `/settings` or IDE trust restart comes seconds after their own
/// keys, inside `[harness] human_grace_s`: read after the person's grace
/// ([`on_exit`]'s [`OnExit::PersonAsked`]), the worker the restart was for
/// was left at its shell, `Session ID … already exists` (the review of
/// b9/spawn-residuals, F2).
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
/// `look` is one [`ExitLook`] (`None`: unreadable); `wait` waits one
/// [`EXIT_LOOK`] and says whether it waited it all (`false`: cut short).
///
/// Process gone and no record decides at once. A record still present decides
/// only after [`EXIT_SETTLE`] from the first look; a process still running
/// waits up to [`EXIT_GONE`].
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
                // Muse Code, agy and Gemini CLI: the shell's word by each
                // one's own table (an agy's with its crash file), a refused
                // start apart; no verdict within the wait is read again at the
                // attempt (the block, then a Muse Code's session log, an agy's
                // crash file).
                upgrade::Agent::Muse | upgrade::Agent::Agy | upgrade::Agent::Gemini => {
                    match now.crashed {
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
                // A Codex's end is never looked at here: aterm's copy of
                // codex-alab's own record decides it (the Codex screen lane's
                // retirement, 2026-10-16). Were one handed in, nothing reads
                // its exit, never as Claude Code's record.
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
/// Relaunch it on its conversation in the same tab, on the newest allowed build while `upgrade`
/// allows it, else the program it ran ([`relaunch_target`]), then tell it to carry on. The step's
/// word says what happened ([`outcome`]). `left` is what the exit left, read as it was seen
/// ([`exit_record`]): crash versus graceful is decided on that. `asked`: the explicit restart's
/// stamp names the process that left ([`super::restart_stamp::lookup`]: the stall's remedy or a
/// person's ask), so it is relaunched even with its record removed, the continuation saying why,
/// and the stamp is consumed as the relaunch's record is filed.
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

/// Consume the explicit restart's stamp once the relaunch of the exit it named has filed its record
/// (under the caller's sweep lock). Only a stamp naming `snap`'s process is taken
/// ([`super::restart_stamp::take_if`]).
pub(super) fn consume_stamp(opts: &Opts, snap: &Snapshot, cause: ExitCause) {
    if opts.dry_run || !matches!(cause, ExitCause::Stall | ExitCause::Asked) {
        return;
    }
    let _ =
        super::restart_stamp::take_if(&opts.state, &snap.tab, |s| s.names(snap.pid, &snap.start));
}

/// Why an agent that is no longer running is relaunched ([`after_exit_as`];
/// a codex-alab's, its copy's relaunch,
/// [`super::upgrade_drive::self_claimed_relaunch`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExitCause {
    /// Its own exit: relaunched only when it left Claude's record (a crash),
    /// never after a graceful one — someone's decision.
    Exit,
    /// The stall's remedy ended it: nobody's decision about the session.
    Stall,
    /// The explicit restart ended it (`aterm ctl restart`, its stamp naming
    /// the process): a decision to bring it back, not to end the session.
    Asked,
    /// aterm itself ended while it ran: the hangup the agent may have answered
    /// by removing its record is nobody's decision about the session.
    HostEnded,
}

/// The agent of a tab a crash of aterm took: aterm ended while `snap`'s agent
/// ran and the next launch reopened its tab (new shell `snap.shell` in
/// `snap.tab`; the rest is what the layout carried). Relaunched as
/// [`after_exit`] would, except the graceful-exit rule does not apply, and a
/// relaunch in flight for the crashed run's tab is closed, not waited on. A
/// Codex snapshot is relaunched only on the line its copy validated
/// ([`self_claimed_line`]), else left.
#[must_use]
pub fn after_host_ended(opts: &Opts, snap: &Snapshot, upgrade: bool) -> Report {
    after_host_ended_from(opts, snap, upgrade, None)
}

/// [`after_host_ended`] of an agent whose layout names the tab it RAN IN,
/// `was` — another sid than its reopened tab's (`snap.tab`): a cold restore
/// mints new sids. The conversation's record keeps it among its earlier
/// tabs ([`St::tabs`]), so what that tab's loop typed into the conversation
/// is read as the harness's own (`upgrade_drive::conversation_typed`): a
/// task its worker finished stays finished across the restore (XREV-LC-1).
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

/// The line the crash rule validated for `snap`'s codex-alab
/// ([`CodexRun::self_claimed`]), when its snapshot carries one: the relaunch
/// it is owed is made on it alone.
#[must_use]
pub fn self_claimed_line(snap: &Snapshot) -> Option<&super::self_claimed::Resume> {
    match &snap.run {
        AgentRun::Codex(run) => run.self_claimed.as_ref(),
        AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) | AgentRun::Gemini(_) => None,
    }
}

fn after_exit_as(
    opts: &Opts,
    snap: &Snapshot,
    left: &ExitRecord,
    upgrade: bool,
    cause: ExitCause,
    was: Option<&str>,
) -> Report {
    // A codex-alab the crash rule validated a line for is relaunched on that
    // line, as the crash rule relaunches it (its witness, its writer lock,
    // its shell at the prompt); any other Codex is nobody's (the Codex
    // screen lane's retirement, 2026-10-16). NEVER ON ITS OWN EXIT
    // (`ExitCause::Exit`): the line says nothing of how the agent ended — the
    // copy decided that, and only the copy's worker
    // ([`super::upgrade_drive::self_claimed_after_exit`]) or a relaunch it
    // OWED (aterm's end, a carried act's cause) reaches the crash rule's
    // relaunch. A line read here on an exit of its own rode a snapshot past
    // the copy that validated it (a seamless update's handed leaf, the agent
    // still running at the Commit): its end, before the successor's
    // re-proof, is nobody's, left (fail closed; the review of 2026-10-16,
    // finding 1).
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
        // A Muse Code by its own lane, on the session its witness proved.
        AgentRun::Muse(_) => {
            return super::upgrade_drive::muse_after_exit(opts, snap, left, upgrade, cause);
        }
        // An agy by its own lane, on the conversation its witness proved.
        AgentRun::Agy(_) => {
            return super::upgrade_drive::agy_after_exit(opts, snap, left, upgrade, cause);
        }
        // A Gemini CLI by its own lane, on the session the start-time rule
        // names at the attempt, pre-checked.
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
    // Claude's own records of the agent — its process's and its
    // conversation's — are under the Claude home it ran under.
    let records = Records::of_snapshot(&opts.home, snap);
    // Claude's record as read at the exit, when it survived (it names the
    // conversation the agent last held), else the snapshot's. Only an exit
    // nothing could read then is read now.
    let survivor = match left {
        ExitRecord::Survived(sf) => Some(SessionFile::clone(sf)),
        ExitRecord::Removed => None,
        // (`Crashed` and `Refused` are a Muse Code's and an agy's words; a
        // Claude Code look never says them.)
        ExitRecord::Unread | ExitRecord::Crashed | ExitRecord::Refused => records
            .session_file(snap.pid)
            .filter(|sf| squash(&sf.proc_start) == snap.start),
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
    // A relaunch already in flight is carried on, not typed twice. One too old to act on is closed
    // and this step makes a fresh one: the host has just decided nobody is at the tab. One of this
    // exit, closed, keeps what an earlier attempt of it read ([`St::relaunched_on_exit_of`]).
    let mut relaunched_before = false;
    if let Some(mut st) = load(opts, &session).filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != snap.tab && cause != ExitCause::HostEnded {
            return said(r, "wait:conversation-in-other-tab");
        }
        // A record of another process than the one that left: that relaunch landed or was
        // overtaken; closed, and this exit relaunched afresh. So is one that landed on a process
        // gone already ([`St::relaunched_gone`], the Codex lane's rule too).
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
        // A hand on the tab is waited out before the clock is read
        // ([`hand_waits_out`]): uncounted, its clock stopped.
        if !overtook && hand_waits_out(opts, &mut st, &session, alive) {
            return said(r, WAIT_HAND_LIFTS);
        }
        if let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s())) {
            relaunched_before = st.relaunched_on_exit_of(snap.pid);
            if !opts.dry_run {
                if overtook {
                    // Never a day-long stall with `claude --resume` for a
                    // relaunch that landed (a graceful `/exit` of the
                    // relaunched agent reads so).
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
    // A graceful exit was someone's decision (a person's `/exit` or ctrl-d, an orchestrator's
    // `/exit`, a `kill`): Claude removes its record as it ends, which a crash cannot. Only an exit
    // that left the record is relaunched; the harness's own restart is handled above, and a stall
    // remedy is nobody's decision. An unread exit an earlier attempt read as a crash stays one: the
    // relaunched agent's start swept the record that attempt read.
    let read_before = relaunched_before && matches!(left, ExitRecord::Unread);
    if survivor.is_none() && cause == ExitCause::Exit && !read_before {
        return said(r, "ended:graceful-exit");
    }
    // WHERE THE GONE AGENT WENT ON WRITING ITS CONVERSATION (a worktree's
    // home under a relative one), noted before anything reads its finish
    // ([`super::upgrade_drive::note_moved_writes`], ATERM-KI-1016-1): the
    // host's own note may never have been written, its finished read
    // overtaken by the crash.
    if !opts.dry_run {
        super::upgrade_drive::note_moved_writes(opts, snap, &session, &cwd);
    }
    // The tab's own program ([`NO_SHELL`]): no shell is left to type its
    // relaunch into. Defence in depth: [`plan`] refuses it too, with the same
    // word; this answers before the targets are read.
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
    // The conversation's upgrade record: what holds it off a build (the owner's word, or a build
    // that failed to start for it while its rest stands).
    let prior = load(opts, &session);
    let now = now_s();
    // And a build not PROVED to start — whether or not one failed, for any
    // conversation ([`super::upgrade_drive::build_proved`], LC-5 of
    // ATERM-KI-1015-1): the agent comes back on the program it ran, never
    // staked on a build nothing has started.
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
    // A conversation with no message cannot be resumed: start afresh with the
    // same flags. Unreadable transcripts are resumed (Claude says if it cannot).
    let resume = records.transcript_exists(&session) != Some(false);
    // The model the rule moves the conversation to rides the relaunch when due
    // (read off the crash's record), else a `/model` made since launch.
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
        // The home and login the agent ran under, as the snapshot read them: its relaunch comes
        // back in the same Claude home, on the same account.
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
    // The relaunched process files its record under the same home ([`St::keep_home`]).
    st.keep_home(&records, &opts.home);
    // Kept on its own program, the session is as far behind as before, and the owner's hold and a
    // failed start's stop still stand ([`St::kept_by_restart`]); moved onto a build that failed to
    // start for the conversation, its rest run out, a stop there again repeats
    // ([`St::retried_by`]).
    if let Some(prior) = prior.as_ref() {
        if st.source == SAME_PROGRAM {
            prior.kept_by_restart(&snap.tab, &mut st);
        } else {
            prior.retried_by(&mut st);
        }
    }
    // The earlier tabs that held the conversation: its record's, and the one
    // a cold restore's layout names ([`after_host_ended_from`]). In memory,
    // not only through `save`: this relaunch carries on with this record.
    let was = was.map(str::to_owned);
    st.tabs = earlier_tabs(
        &snap.tab,
        prior
            .iter()
            .flat_map(|p| p.tabs.iter().chain(std::iter::once(&p.tab)))
            .chain(was.as_ref()),
    );
    // …and when each lost it: the record's stamps, and now for the tabs
    // this relaunch leaves — the cold restore's `was` and the record's own
    // tab, where another — so what their loops type later is another
    // conversation's ([`St::tabs_until`], RR-LC-4).
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
        // The model the agent last ran and where its transcript ended: the
        // resumed session's answer past it says which model it came back on.
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

/// How long after its agent was seen gone ([`St::exited_at`]) — or after the
/// last hand that held its line, whichever is later ([`exit_clock`]) — a
/// stopped restart still counts as the one that ended an agent leaving its
/// tab: twice [`STALE_S`], the longest a carry lives once nothing holds it,
/// so a reused pid is never read as it.
pub(super) const STOPPED_OWN_S: u64 = 2 * STALE_S;

/// Whether `st` records a restart of the harness's own that ended `agent` process `pid` in tab
/// `tab`, the one identity both lanes read a leaving agent by: that agent and tab, a cause that
/// ends its agent ([`ends_its_agent`]), and that very process ([`St::pid`]). Never a relaunch on
/// exit's own record: the Codex lane files both kinds in one per-tab slot (`codex-<tab>`), and
/// matched by tab alone a Codex a person started at the waiting prompt would be carried as aterm's
/// restart, over their word, a hold and `[harness] relaunch = false`.
fn restart_of(st: &St, agent: upgrade::Agent, tab: &str, pid: u32) -> bool {
    st.agent == agent && st.tab == tab && st.pid == pid && ends_its_agent(&st.cause)
}

/// Whether `st` records a harness restart that ended agent `pid` (of `agent`'s
/// lane) that left tab `tab` at `now` ([`restart_of`]) and is still that
/// exit's: a Claude Code's in flight, or stopped at most [`STOPPED_OWN_S`]
/// after the agent was seen gone (a stop before any exit, a refused signal,
/// left the agent running; its later exit is someone's). Never a Codex's
/// (the Codex screen lane's retirement, 2026-10-16): the window's host reads
/// a Codex's end by aterm's copy of codex-alab's own record alone, and no
/// restart of the harness's own ends one.
pub(super) fn ended_by_restart(
    st: &St,
    agent: upgrade::Agent,
    tab: &str,
    pid: u32,
    now: u64,
) -> bool {
    restart_of(st, agent, tab, pid)
        && match agent {
            // Claude Code's upgrade and restart in place, and Muse Code's and
            // agy's live upgrades (their quiet flows, `upgrade_muse_live`,
            // `upgrade_agy_live`): the record its step wrote BEFORE the
            // SIGTERM (cause the upgrade's, [`ends_its_agent`]) claims the
            // exit the signal makes — for Muse Code a `143` and a clean
            // `session.end`, for agy a `0` with its crash file removed, each
            // of which its relaunch on exit would read as someone's end and
            // never relaunch. In flight, or stopped a short while after its
            // agent was seen gone.
            upgrade::Agent::Claude | upgrade::Agent::Muse | upgrade::Agent::Agy => {
                st.in_flight()
                    || (matches!(st.phase, upgrade::Phase::Failed(_))
                        && st.exited_at != 0
                        && now.saturating_sub(exit_clock(0, st.exited_at, st.hand_seen_s))
                            <= STOPPED_OWN_S)
            }
            upgrade::Agent::Codex => false,
            // Gemini CLI: relaunch on exit only — no upgrade or restart in
            // place of the harness's ever ends it (its lane), so no record of
            // its lane is a restart's ([`ends_its_agent`] is never true of a
            // relaunch's cause). The explicit restart's act an agy carries is
            // read by its stamp, never here: its records' causes are no
            // restart's that ended its agent ([`CAUSE_STALL`],
            // [`CAUSE_ASKED`]).
            upgrade::Agent::Gemini => false,
        }
}

/// The record of the restart [`restarted`] finds for tab `opts.only_sid`, with
/// the conversation it is filed under: the one that ended process `pid`
/// ([`ended_by_restart`]). No `pid` (the host read nothing of the agent that
/// left) names no process, and finds none.
fn restart_record(opts: &Opts, agent: upgrade::Agent, pid: Option<u32>) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let pid = pid?;
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

/// The record of the harness's own restart of the Claude Code `pid` that left tab `opts.only_sid`:
/// done, its agent seen gone at most [`STOPPED_OWN_S`] ago, and its conversation held now by a live
/// process other than `pid` (a fresh read of Claude's session files). Another actor (a hand-run
/// `aterm harness upgrade`, holding the lock the carry waits on) finished the move before the
/// carry's next try: the exit was the finished move's own, and nothing is owed. Read-only.
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

/// A QUIET UPGRADE'S RESTART THAT FINISHED — Muse Code's or agy's, `agent` —
/// since the host read the exit of process `pid` in tab `opts.only_sid` as its
/// own ([`landed`]'s half of the lanes past Claude Code's): its tab's record
/// names that process, ended it (the upgrade's cause), saw it gone at most
/// [`STOPPED_OWN_S`] ago and is done, and the process its relaunch resumed
/// ([`St::resumed_pid`]) still runs. Neither agent keeps a session file a live
/// holder is read from, so the resumed process itself is the witness.
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

/// Whether the restart `st` (filed under `session`) was adopted and its adopted process
/// ([`St::relaunched_pid`]) holds the conversation now.
fn adopted_holds(opts: &Opts, st: &St, session: &str) -> bool {
    matches!(st.phase, upgrade::Phase::Relaunched { .. })
        && st.relaunched_pid != 0
        && continuation_session(&st.agent_records(&opts.home), st.relaunched_pid, session)
            .is_some_and(|sf| {
                sf.pid == st.relaunched_pid && conversation_live(std::slice::from_ref(&sf), session)
            })
}

/// Whether the harness's own restart (the upgrade's, or a restart in place) ended the agent that
/// left tab `opts.only_sid` after its step returned with the relaunch still to type (`wait:held`,
/// `wait:typing`, `wait:shell-prompt`, `wait:resume` …). `pid` is the agent that left (`None`:
/// nothing read, nothing found), a Codex TUI or a Claude Code as `agent` says; both lanes match it
/// by one rule ([`ended_by_restart`]). Read-only: the state files, and whether a relaunched Codex
/// TUI a record names still lives; no lock.
#[must_use]
pub fn restarted(opts: &Opts, agent: upgrade::Agent, pid: Option<u32>) -> bool {
    restart_record(opts, agent, pid).is_some()
}

/// The label of the account move whose restart signalled the Claude Code `pid` of tab
/// `opts.only_sid`: the record [`restarted`] finds for that process, where its cause is
/// [`CAUSE_ACCOUNT`]. `None`: nothing of the harness's own signalled it, or a restart of another
/// kind did. A host that asked for the move reads it to tell a relaunch left in flight (signal
/// sent, line still to land under the target's login) from one never begun, which the step's word
/// cannot say (`wait:held` comes before the signal and after it). Read-only; no lock.
#[must_use]
pub fn account_restart(opts: &Opts, pid: u32) -> Option<String> {
    restart_record(opts, upgrade::Agent::Claude, Some(pid))
        .and_then(|(_, st)| st.cause.strip_prefix(CAUSE_ACCOUNT).map(str::to_owned))
}

/// THE RESTART [`restarted`] FOUND, CARRIED ON for its agent's exit: one step
/// under the one lock every actor on the upgrade state takes. `adopted` (or
/// `done:fresh`) once the relaunch holds the tab; [`WAIT_HAND_LIFTS`] while
/// a hold or any hand the line waits on stands on the tab (nothing
/// attempted, no miss, its clock stopped: [`hand_waits_out`]); `wait:<why>`
/// while a person types at the returned prompt, the shell has not taken the
/// terminal back, or the relaunch has not registered — nothing is typed
/// then, and the host tries again ([`CARRY_EVERY`]);
/// `busy:<why>` for another actor; `landed` for a Claude Code restart
/// another actor finished meanwhile ([`landed`]: nothing owed, nothing
/// said), and for a restart whose relaunched agent is gone already, its
/// record closed ([`carry_own`]); and
/// `refused:<why>` once it can never land ([`carry_own`]), or is
/// no longer to be found (`refused:no-restart-in-flight`) — the one case a
/// person is asked about, never the graceful exit the restart's own SIGTERM
/// made of it — or `told:<why>` where the move's own row asks them
/// ([`told_by_its_row`]).
///
/// `through_hand`: the host CARRIED this exit through a hand that held its
/// tab — a hold, a lease, a turn: [`hand_on_tab`] — attempting nothing while
/// it stood ([`WAIT_HAND_LIFTS`], ruling 20's mechanism, LC-7 of
/// ATERM-KI-1015-1), and this is the first carry since it lifted. The
/// relaunch's stale bound ([`STALE_S`], [`expired`]) is there so a line is
/// never typed at a prompt a person may have taken meanwhile; a tab held is
/// a prompt nobody types at, so the bound counts from now, as from a fresh
/// exit ([`St::carried_through_hand`]). Counted from the signal, a hold that
/// landed just after the upgrade's `SIGTERM` let the record go stale under
/// it (`stale-exit`, five minutes), and the agent was never relaunched once
/// the hold lifted.
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
        // A move FINISHED since the host read the exit as its own: that exit
        // was the move's, and the owner is asked nothing about an agent
        // that is back. No Codex restart is ever found ([`ended_by_restart`]).
        let found = pid.and_then(|pid| match agent {
            upgrade::Agent::Codex => None,
            upgrade::Agent::Claude => landed(opts, pid),
            // Muse Code and agy: an upgrade's restart that finished meanwhile
            // is closed `done` in its tab's own record, which names the new
            // process; never one of Claude Code's read for it. Gemini CLI: no
            // restart of the harness's ends it ([`ended_by_restart`]), so
            // none landed.
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
    // This carry follows the exit the host just saw by at most one
    // [`CARRY_EVERY`], so the stale-exit bound ([`STALE_S`]) counts from now,
    // never from a signal a slow shutdown outlived — or from a hand that held
    // the tab until now (`through_hand`). A Codex lane keeps its own.
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

/// A restart of the harness's own whose signal ended its agent ([`ended_by_restart`]), carried from
/// where it stopped, and final where it can no longer land: one already stopped says so again, one
/// too old to act on is stopped ([`St::stop`]) and one that stops in the carry says so, each
/// `refused:<why>` (`would-refuse:<why>` in a dry run, which writes nothing). One relaunched and
/// never registered is never relaunched afresh: a second line minutes later, at a prompt a person
/// may be typing at, is what [`STALE_S`] exists to prevent.
///
/// One already adopted answers `adopted` at once while its adopted process
/// ([`St::relaunched_pid`]) holds the conversation, and writes nothing: going back through
/// [`await_new`] for a process already found would, past [`STALE_S`], stop the record
/// `no-resume`. A refusal of a record its upgrade's row lists is `told:<why>`
/// ([`told_by_its_row`]).
fn carry_own(opts: &Opts, r: Report, st: St, session: &str, tab: &str) -> Report {
    told_by_its_row(opts, carry_own_step(opts, r, st, session, tab), session)
}

/// The move's own row asks the owner, not the host: a carry refused (`refused:<why>`) because an
/// upgrade's record ([`St::is_upgrade`], which the owner's view lists) stopped after its agent was
/// seen gone answers `told:<why>`. That row raises `agent-down` with the one remedy
/// (`upgrade_status::Row::agent_down`), so the host says nothing of its own over it ([`outcome`]:
/// a `Cannot` whose word is the row's); a second report would also tell `claude --resume` of a
/// fresh restart's conversation, which has no transcript. A restart in place's record (a model,
/// memory or account move) is listed in no row: its refusal stays the host's to say.
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

/// [`carry_own`]'s step, before [`told_by_its_row`] reads its refusal.
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
    // Landed and gone: the process the relaunch adopted is gone already
    // ([`St::relaunched_gone`]). No carry types its line again, so carried on, the tab would be
    // held for a dead process until the record went stale. Closed as an overtaken one
    // ([`St::close_overtaken`]: no exit stamp) and answered `landed`: what ended is the relaunched
    // agent's own exit, not the restart's.
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
    // A HAND ON THE TAB IS WAITED OUT, UNCOUNTED (LC-7 of the 2026-10-15
    // sweep; what ruling 20 of 2026-10-14 gave the explicit restart's exit):
    // a restart whose line is still to type, read while a hold or any hand
    // the line waits on stands on its tab, attempts nothing — no lease, no
    // model ask, no line — and answers [`WAIT_HAND_LIFTS`]: the host's
    // `Busy`, looked at again after [`BUSY`], never a miss, unsaid, no ring.
    // Its clock stops with it ([`exit_clock`]): once the hand lifts the line
    // has [`STALE_S`]. Asked before the expiry, in both lanes: a hand
    // standing now is what holds the line, however long no look came.
    // Before, every carry under a hold ran into the line's `wait:held`, a
    // counted miss that said `Failing` at the third, and five minutes on
    // the record expired `stale-exit` under the hold.
    if hand_waits_out(opts, &mut st, session, alive) {
        return said(r, WAIT_HAND_LIFTS);
    }
    // A Codex restart is its lane's to carry, its own age limit included:
    // counted from its typed `/exit`, never from the TUI's exit — a TUI still
    // alive a minute after its `/exit` is one the `/exit` did not take.
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
        // Muse Code's and agy's quiet upgrades: a restart's record is carried
        // by its lane's own rules (the stale bound counted from its exit, its
        // line, the adoption by its session's or conversation's lock, nothing
        // typed after). Gemini CLI: no restart of the harness's ends it, so a
        // restart's carry never finds its records ([`ended_by_restart`]); one
        // handed in is its lane's to carry, by its lane's rules.
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

/// A RESTART'S LINE UNDER A HAND IS WAITED OUT, by every carrier of its
/// record — the host's carry ([`carry_restart`]), the orphan pass, the
/// relaunch on exit's look at a relaunch in flight ([`after_exit`]) and the
/// Codex lane's: whether `st`, a restart whose line is still to type
/// ([`upgrade::Phase::Exiting`]), its shell alive (`alive`; a gone shell is
/// said, never waited out), is held by a hand the line waits on standing on
/// its tab now ([`hand_on_tab`], one `status` read). If so its clock stops
/// ([`St::saw_hand`], [`exit_clock`]), its wait reads `held`, the record is
/// saved under `session` where that changed it, and the caller answers
/// [`WAIT_HAND_LIFTS`]: nothing attempted. An unreadable tab is no hand: the
/// line's own look holds on it, a counted wait. A dry run looks at nothing.
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

/// Whether a hand the relaunch line waits on stands on `tab` now: one
/// `status` read ([`hand_on_tab`]). An unreadable tab is not one.
fn hand_stands(opts: &Opts, tab: &str) -> bool {
    connect(opts, tab)
        .ok()
        .and_then(|mut c| super::upgrade_drive::status_line(&mut c, tab))
        .is_some_and(|s| s.starts_with("OK") && hand_on_tab(&s))
}

/// THE LIMIT ROW THE AGENT `snap` NAMES STANDS AT, by its transcript's tail
/// (`tail`, [`super::upgrade_drive::snapshot_transcript_tail`]) — unless the
/// harness MOVED the conversation off it ([`moved_off`]): what the
/// supervisor's loop reads as the wall where the screen draws none
/// (`supervise::IdleHost::standing_limit`). Read-only: the state file.
#[must_use]
pub fn standing_limit(opts: &Opts, snap: &Snapshot, tail: &str) -> Option<upgrade::StandingLimit> {
    let limit = upgrade::transcript_standing_limit(tail)?;
    let moved = snap
        .session
        .as_deref()
        .is_some_and(|session| moved_off(opts, session, limit.at, snap));
    (!moved).then_some(limit)
}

/// Whether conversation `session` was MOVED OFF the limit row written at `at`
/// (unix seconds): a move — onto a bucket's fallback model ([`CAUSE_MODEL`]),
/// back from it ([`CAUSE_MODEL_BACK`]), or under another account
/// ([`CAUSE_ACCOUNT`]) — whose relaunch line was typed after the row was
/// written: the record's own restart ([`super::upgrade_drive::St::relaunched_at`];
/// either time unknown counts as after), or the last move a record written
/// over it since keeps ([`super::upgrade_drive::St::moved_at`]: a crash's
/// relaunch, a cold restore's, a memory restart, an upgrade's round). No row
/// of the model since: the row is the limit the move left — the bucket's, the
/// old account's — and the relaunched agent's carry-on is not held behind it
/// (owner decision 3, the accounts design §6). The record is the
/// conversation's, so a cold restore's new tab finds it; and a move the
/// restore's own relaunch wrote over is still the move (LIM-1's review).
///
/// ONLY FOR AN AGENT UNDER THE MOVE (the residual review's R4): `live`, the
/// agent that runs now, runs the move's target ([`last_move_target`],
/// [`runs_moved`]) — the login an account move required, a model other than
/// the one a fallback left. One back under the login or model the move LEFT
/// (a cold restore from a leaf captured before the move, a person's `claude
/// -r` under the old login in any tab: the record is the conversation's)
/// stands at that limit, and the wall's own rule acts on it — a move again,
/// or a wait. A live agent with no login variable runs its home's login:
/// the default one under the default home (the review's RR3-R4-1), the one
/// its home's directory names under another ([`runs_moved`]). A record
/// naming no target (an older build's, a move back) is the move, as before.
/// Read-only.
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
    // …or the move kept beside a record removed since
    // ([`super::upgrade_drive::kept_move`], ATERM-KI-1016-1).
    record || kept_move(opts, session).is_some_and(|st| kept(&st))
}

/// Whether agent `live` runs under a move's target: the login `login` an
/// account move required (its own login read the same), and a model other
/// than `left`, the one a fallback left (its argv's `--model` or
/// `--model=`; none named reads `""`). `None` checks nothing. ITS LOGIN IS
/// THE DIRECTORY ITS KEYCHAIN ITEM IS NAMED BY (READ in the vendor's
/// bundle, the accounts design §1): [`accounts::LOGIN_ENV`] where set
/// (empty: the default item, what `accounts::login_env` names a move to the
/// default account), else [`accounts::HOME_ENV`] verbatim, else — or that
/// empty — the default. So an unset variable under the default home is the
/// default login (the review's RR3-R4-1), and unset under another home is
/// the login that home's own string names (ATERM-KI-1016-1): a move to that
/// login is met by it, never by a move to the default one.
fn runs_moved(live: &Snapshot, login: Option<&str>, left: Option<&str>) -> bool {
    let runs = live.login.as_deref().or(live.home.as_deref()).unwrap_or("");
    login.is_none_or(|l| runs == l) && left.is_none_or(|m| launch_model_of(&live.argv) != m)
}

/// The model the relaunch's canonical launch parser keeps. The existing
/// empty fallback remains where no model is named or the argv is refused:
/// this does not read the running agent's configured default model.
fn launch_model_of(argv: &[String]) -> String {
    upgrade::launch_model(argv).unwrap_or_default()
}

/// The target of record `st`'s OWN move ([`moves`]): the login an account
/// move's line required ([`St::login`]), the model a bucket's fallback left
/// ([`fallback_origin`]); `(None, None)` for a move back, or no move.
fn own_move_target(st: &St) -> (Option<String>, Option<String>) {
    if st.cause.starts_with(CAUSE_ACCOUNT) {
        (st.login.clone(), None)
    } else if st.cause.starts_with(CAUSE_MODEL) {
        (None, fallback_origin(st))
    } else {
        (None, None)
    }
}

/// THE TARGET OF THE LAST MOVE record `st` knows of ([`last_move_at`]):
/// its own move's where that is the last ([`own_move_target`]), else the
/// one it kept ([`St::moved_login`], [`St::moved_left_model`]). PURE.
pub(super) fn last_move_target(st: &St) -> (Option<String>, Option<String>) {
    if moves(&st.cause) && st.relaunched_at != 0 && st.relaunched_at >= st.moved_at {
        own_move_target(st)
    } else {
        (st.moved_login.clone(), st.moved_left_model.clone())
    }
}

/// Whether a restart of `cause` MOVES the conversation: onto a bucket's
/// fallback model, back from it, or under another account.
fn moves(cause: &str) -> bool {
    cause.starts_with(CAUSE_MODEL)
        || cause.starts_with(CAUSE_MODEL_BACK)
        || cause.starts_with(CAUSE_ACCOUNT)
}

/// THE LAST MOVE record `st` knows of ([`super::upgrade_drive::St::moved_at`]),
/// unix seconds, `0` for none: its own restart's relaunch line where it is a
/// move whose line was typed, else the move it kept. A move whose line was
/// never typed moved nothing. PURE.
pub(super) fn last_move_at(st: &St) -> u64 {
    if moves(&st.cause) && st.relaunched_at != 0 {
        st.relaunched_at.max(st.moved_at)
    } else {
        st.moved_at
    }
}

/// WHETHER THE AGENT A RELAUNCH OF CONVERSATION `session` IN TAB `tab`
/// ADOPTED IS GONE (the residual review's R5): its record in flight
/// ([`St::in_flight`]) in that tab names the process its relaunch adopted
/// ([`St::relaunched_pid`], kept until a relaunch line is typed) —
/// `Some(true)` where that process is no more ([`St::relaunched_gone`], the
/// rule [`after_exit_as`] closes such a record and relaunches afresh by),
/// `Some(false)` where it lives. `None`: no record, one not in flight
/// (carried on, stopped), none adopted — or ANOTHER TAB'S (F1-R9-G6): a
/// record the previous aterm left in flight in the tab a cold restore's
/// layout named, whose adopted process ended with it and whose pid
/// `kill(0)` may now find another process holding, says nothing of the
/// reopened tab (its relaunch closes that record, `host-ended`). Read-only.
#[must_use]
pub fn adopted_gone(opts: &Opts, tab: &str, session: &str) -> Option<bool> {
    let st = load(opts, session).filter(|st| st.in_flight() && st.tab == tab)?;
    (st.relaunched_pid != 0).then(|| st.relaunched_gone(alive))
}

/// WHETHER A RELAUNCH OF CONVERSATION `session` IS IN FLIGHT IN TAB `tab`
/// ([`St::in_flight`]): a relaunch saves its record so before it types its
/// line ([`after_exit_as`]), so a step that ended without saying how — it
/// panicked — may have typed it. Another tab's record says nothing of this
/// one (F1-R9-G6). Read-only.
#[must_use]
pub fn in_flight_here(opts: &Opts, tab: &str, session: &str) -> bool {
    load(opts, session).is_some_and(|st| st.in_flight() && st.tab == tab)
}

/// Whether a restart of `agent`'s lane is carried by this module's own rules
/// ([`carry_restart`], [`carry_own_step`]: the stale-exit bound counted from
/// the carry, the adopted process, the relaunched agent gone, the age limit)
/// — Claude Code's — or by its lane's, which keeps all of them itself (a
/// Codex restart: [`super::upgrade_drive::codex_carry_in_flight`], its age
/// counted from its typed `/exit`).
fn carried_here(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        // Muse Code, agy and Gemini CLI: each lane keeps its own rules for
        // the relaunch it files (`upgrade_drive`'s `muse::carry_in_flight`,
        // `agy::carry_in_flight` and `gemini::carry_in_flight`: the stale
        // bound counted from its exit, the adopted process, its age), and
        // Claude Code's rules are never theirs.
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

/// The word of a step asked of a process no lane relaunches (a vendor
/// `codex`, an image no lane reads; [`super::lanes::lanes`]): nothing read,
/// typed or relaunched.
pub const NO_LANE: &str = "no-lane";

/// The continuation owed to tab `opts.only_sid`: an adopted relaunch not yet
/// carried on ([`Opts::hand_back`]), or a typed continuation whose model is
/// still to be read ([`confirm`]). The host parks the tab's loop for it at its
/// next idle point regardless of `[harness] upgrade`. Read-only.
#[must_use]
pub fn owed(opts: &Opts) -> bool {
    owed_of(opts, None)
}

/// [`owed`], of `only`'s record alone where it names an agent: the window's
/// host asks so for the worker of a relaunch-only lane (Muse Code, agy,
/// Gemini CLI), which
/// carries its own agent's relaunch and never one the sweep carries — a
/// codex-alab's left owed in the same tab is the sweep's alone
/// ([`super::lanes::sweep_carries_relaunch`]). `None`: any agent's record.
/// Read-only.
#[must_use]
pub fn owed_of(opts: &Opts, only: Option<upgrade::Agent>) -> bool {
    owed_record(opts, only).is_some()
}

/// The conversation and record [`owed_of`] finds for tab `opts.only_sid`:
/// `only`'s record alone where it names an agent.
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

/// Carry on the relaunched agent at its loop's idle point, where the loop
/// parked for an [`owed`] continuation: find the new process again
/// ([`await_new`]) and tell it to carry on ([`carry_on`]), or read a typed
/// continuation's model ([`confirm`], `wait:model` while it may come). Under
/// the upgrade state lock.
#[must_use]
pub fn resume(opts: &Opts) -> Report {
    resume_as(opts, false, None)
}

/// [`resume`] at a point the loop read as a TASK LANDED UNDER LAND
/// (`done_under_land`, `crate::supervise::IdlePoint::done_under_land`): the
/// session's word is Land and the policy read the task landed. Its
/// carry-on — "Continue where you left off" — would start again the work
/// that landed, after a cold restore, a crash's or a relaunch on exit
/// alike, so the relaunched agent is found and adopted as a loop's
/// ([`await_new`] under `hand_back`, the same look that precedes every
/// carry-on), and its record is done with NOTHING TYPED ([`carried_done`],
/// `done:said-done`). The loop then decides the point as any other: a
/// landed task gets nothing until someone else's turn. A model a typed continuation owes is read as
/// ever ([`confirm`]); a relaunch whose agent is not found yet waits, owed.
/// `only`: the one agent whose record is carried ([`owed_of`]); another
/// agent's owed in the tab is `current` here.
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
        // A quiet restart's look — or a carry-on's that typed nothing — types
        // its one line here, at the tab's idle point, where a sweep's types
        // nothing.
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
    // A task landed under Land is adopted, never carried on: the look stops
    // where a loop's relaunch stops (`adopted`), before anything is typed.
    let carry = Opts {
        hand_back: done_under_land,
        background: false,
        ..opts.clone()
    };
    // A Codex restart's continuation is the Codex branch's to type: the same
    // step, over the TUI its relaunch line brought back.
    let r = match st.agent {
        upgrade::Agent::Codex => {
            super::upgrade_drive::codex_carry_in_flight(&carry, r, st, &session)
        }
        upgrade::Agent::Claude => carry_in_flight(&carry, r, st, &session, &tab),
        // A Muse Code relaunch's continuation is its lane's to type, over the
        // TUI its relaunch line brought back.
        upgrade::Agent::Muse => super::upgrade_drive::muse_carry_in_flight(&carry, r, st, &session),
        // An agy relaunch's continuation is its lane's to type, the same way,
        // and so is a Gemini CLI's.
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

/// The word of a relaunch carried on with nothing because its task landed
/// under Land ([`resume_as`]): a last word that typed and ended nothing
/// (`upgrade_drive::moved`), so the loop decides the point after it. The
/// word is an older build's, kept for the ledgers that hold it.
pub const CARRIED_DONE: &str = "done:said-done";

/// THE RELAUNCH OF A TASK LANDED UNDER LAND, CARRIED ON WITH NOTHING
/// ([`resume_as`]): its agent adopted (`r`), the record of `session` is done
/// — no continuation typed, none owed, no model to read after — and the
/// owner's outcome line says why. A record no longer in flight (another
/// actor moved it meanwhile) is left as it is.
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

/// THE CARRY-ON OWED TO TAB `opts.only_sid` SETTLED WITH NOTHING TYPED, off
/// the host's idle point: where the relaunched agent rests at a point of
/// the tab's loop that a guard withheld from the host — before and after
/// the loop's own act there, its task's end
/// (`crate::supervise::IdleHost::at_withheld_point`) — the relaunch's last
/// step is taken as [`resume`] would take it there whenever that types
/// nothing ([`carried_quietly`]'s `done:resumed`, [`nothing_owed`]'s
/// `done:taskless`, `done:finished`, `done:left`, `done:carried`), and
/// its one look at what the resumed build reports did not come back is
/// kept open for its whole window ([`RESTORE_LOOK`]), as there, and read
/// typing nothing ([`settle_look`]): now, at the loop's next withheld
/// point or wake ([`settles_again`]), and at the tab's idle point. A warning in a finished or taskless
/// conversation is recorded as soon as it is read (`done:lost`); one in a
/// conversation whose task is open keeps the look owed for the tab's next
/// idle point (`wait:owed` once read: never settled again, F1-R3-B1), where
/// its line is typed under every guard, or recorded once
/// [`RESTORE_TYPE_WITHIN_S`] has passed with a guard standing. A look whose
/// window ends with no warning ends on the carry-on's own word.
///
/// F1 OF THE LIVE LAB OF 2026-10-16 (Claude Code 2.1.301, the lab's L7): a
/// crash relaunched the agent into a usage limit; its loop's limit episode
/// withheld every idle point from the host until the limit's reset plus
/// `upgrade_drive::LIMIT_GRACE_S` (an hour; for good where the limit names
/// no reset), while the loop typed its act and said `DONE`. The
/// record stayed `relaunched` (owed) through it all, and an owed relaunch
/// holds the tab's automatic update — its notice at a break and its step
/// at an idle point are the carry-on's first, and the upgrade's own record
/// of the conversation reads `in-flight` — and its restore look, never
/// begun. Nothing wrong was typed: the step, taken late, ends at
/// `done:carried`.
///
/// Every other carry-on — one that would type (the continuation, the Cruise
/// prompt after a done claim, an open task's restore line), a Codex's, a
/// typed continuation's model — is left to the tab's idle point
/// (`wait:owed`). `wait:resume`: the relaunch
/// has not adopted its new process yet (its own step does, and a settle
/// after it can settle). `wait:changed`: the adopted process no longer
/// holds the conversation (its exit is a relaunch's own business,
/// [`St::relaunched_gone`]). Under the upgrade state lock: `busy:<why>`
/// where another sweep holds it.
///
/// A SETTLE MAY MISS AT THE LOOP'S LAST POINT (F1-R2, the second
/// verification of F1's fix, 2026-10-16): a limit or a wall may withhold
/// every later point. What a later settle can still settle
/// ([`settles_again`]: a lock another sweep held — a cold restore's next
/// relaunch, or this one's own wait — a relaunch not adopted yet, a look
/// still open) is settled again at the later wakes of the loop's wait at
/// that point, typing nothing, while the agent rests there: the loop's own
/// cadence (F1-R4, the fourth verification: a timer of the host's thread
/// that did it, with its arm, its bound and its catch, was wrong three
/// rounds running). The agent busy again ends the guard (its busy read
/// closes a limit's episode), and the turn's end is the idle point.
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
    // A look left open (a quiet restart's, a carry-on's that typed
    // nothing, an earlier settle's): read again, typing nothing.
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
    // Not adopted yet: the relaunch's own step finds the new process.
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
    // The look stays open for its whole window, as the idle point's step
    // leaves it: read now for a warning already written, and again at the
    // loop's next withheld point or wake, or at the tab's idle point. The
    // step's own word stands unless the look ended it: an open task's
    // warning read now is the idle point's line, which the next settle
    // finds.
    settle_look(opts, &r, &mut st, &session, now_s())
        .filter(|look| look.step.starts_with("done"))
        .unwrap_or(r)
}

/// WHETHER A [`settle`] THAT SAID `step` LEFT SOMETHING A LATER ONE CAN
/// SETTLE, typing nothing: another sweep held the lock
/// (`busy:another-sweep`; a state directory that cannot hold it is no
/// sweep's, and settles nothing later), the relaunch had not adopted its
/// new process yet (`wait:resume`), or the step's one look is still open
/// with no warning read — its window not passed (`wait:restore`, and every
/// `done:<word>` but `done:lost`, which ends it). The tab's host settles
/// again on it at the later wakes of the loop's withheld point, while
/// [`owed`] holds (F1-R2, F1-R4). Never a
/// carry-on that would type (`wait:owed`: an open task's restore line among
/// them, F1-R3-B1), a Codex's, a model still to be read (`wait:model`),
/// an adopted process gone (`wait:changed`), or nothing owed (`current`):
/// those are the idle point's, or nobody's.
#[must_use]
pub fn settles_again(step: &str) -> bool {
    matches!(step, "busy:another-sweep" | "wait:resume" | "wait:restore")
        || (step.starts_with("done:") && step != "done:lost")
}

/// [`settle`]'s read of the one look a carry-on that typed nothing keeps
/// open ([`restore_look`]), typing nothing, at `now`: a warning the resumed
/// build wrote in a finished or taskless conversation is recorded at once
/// (`done:lost`, the fact the tab's host raises), as at the tab's idle
/// point; one in a conversation whose task is open is the idle point's line
/// (`wait:owed`); and a look with no warning ends on the carry-on's own word
/// once its window has passed. `None` while the look stays open with no
/// warning.
///
/// AN OPEN TASK'S WARNING IS NEVER DECIDED OFF THE POINT (F1-R3-B1, the
/// third verification of F1's fix, 2026-10-16): it was read as a sweep reads
/// it, owed until [`RESTORE_TYPE_WITHIN_S`] past the window and then
/// recorded (`done:lost`, "no idle point came to say so at"). Once a
/// settle was asked again every [`BUSY`] off the points (F1-R2), a relaunched agent
/// merely busy for five minutes after the loop's act — a coding turn after
/// a wall's retry, a turn after a limit's resume — had its restore line
/// replaced by that record and an alert, and the idle point at the turn's
/// end, which would have typed the line, found nothing owed. The line is a
/// carry-on that types, and so the idle point's ([`resume`]'s look, under
/// every guard, bounded there): a later settle settles nothing of it
/// ([`settles_again`]).
///
/// THE WINDOW IS NEVER CUT SHORT HERE (F1-V1, the verification of F1's fix
/// of 2026-10-16): the resumed build writes its warning as it resumes — a
/// second after its first idle point in the one measured case — and the
/// loop may act at that very point, so a look read once and closed at the
/// act lost the warning: no restore line for an open task, no `done:lost`
/// for a finished one. Meanwhile the record reads owed ([`owed`]), as
/// after the idle point's step: the look's next read — the loop's next
/// withheld point or wake, the tab's idle point — ends it once its window
/// has passed.
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

/// The host's word for an exit the explicit restart's stamp names whose process still runs: a
/// stamped program stopped from elsewhere inside the act's grace, its shell taking the terminal
/// back. The act's watch ends it once it leads again (or it finishes the shutdown its `SIGTERM`
/// started at its `SIGCONT`), and whose end that is is decided then (`aterm-gui`'s
/// `on_agent_left`). No relaunch is attempted: it stays owed, its snapshot kept, and is looked at
/// again after [`BUSY`], uncounted ([`Outcome::Busy`]), while the stamp stands. Read as
/// `wait:not-exited`, the snapshot would be forgotten and the act's end never relaunched.
pub const WAIT_ACT_ENDS: &str = "wait:not-exited:act-ends";

/// THE WORD FOR AN EXIT THE HARNESS MADE, CARRIED THROUGH A HAND THAT WAITS:
/// the explicit restart's (ruling 20 of 2026-10-14, the host's word,
/// `aterm-gui`'s `on_agent_left`: an exit the act's stamp names, read while a
/// hold or any hand the relaunch line waits on holds the tab
/// ([`hand_on_tab`]), is the act's still — [`on_exit`] answers
/// [`OnExit::Asked`] for it) — and its own restart's, the upgrade's or a
/// restart in place's, whose line is still to type ([`hand_waits_out`]: the
/// carry, [`carry_restart`], and every other carrier of its record; LC-7 of
/// the 2026-10-15 sweep). Its relaunch is typed only once that hand lifts.
/// Until then no relaunch is attempted — this word is answered in its
/// place, and rings nobody — and it is looked at again after [`BUSY`],
/// uncounted and unsaid ([`Outcome::Busy`]): a hand is not a relaunch that
/// keeps failing, a hold of minutes must not climb the back-off to ten (the
/// relaunch line's own `wait:held`, a counted miss, is what an attempt under
/// it would have answered), and the restart's record does not go stale under
/// it ([`exit_clock`]).
pub const WAIT_HAND_LIFTS: &str = "wait:held:act-carried";

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
    /// Another aterm actor holds the upgrade state — or the explicit
    /// restart's act still holds the end of the program its standing stamp
    /// names ([`WAIT_ACT_ENDS`]), or another hand holds the tab an exit the
    /// harness made is carried through ([`WAIT_HAND_LIFTS`]: the explicit
    /// restart's, or its own restart's carry): no miss; the host looks again
    /// after [`BUSY`] without moving the back-off or attention.
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
    // agent is gone already, [`carry_restart`]) — or `done:resumed`, the
    // crash rule's relaunch of a proven self-claimed codex-alab
    // ([`super::upgrade_drive::self_claimed_after_exit`]: its new TUI leads
    // the tab, and codex-alab carries its own thread on, nothing typed
    // after). That word was `adopted` until the codex-alab carve
    // (3999d9adc6); read as a miss since, the host's relaunch loop never read
    // the agent it had just started and tried the crash again on its back-off
    // with the CRASHED process's snapshot and line — `ended:handled`, its
    // witness already relaunched — so a relaunched codex-alab that crashed
    // before the host's next pass named it was never relaunched (the second
    // crash of the codex-alab proof, 2026-10-17).
    // The host hands every relaunched agent to its loop
    // ([`Opts::hand_back`]), so no other word lands — and a carry-on that
    // typed nothing (`done:no-continue`) is never one.
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
        // `refused:shell-gone` included: the one word a gone shell gives.
        // `told:` is a restart that can never land whose own row asks the
        // owner ([`told_by_its_row`]): said there, as the host reads it.
        Outcome::Cannot(why.to_string())
    } else if matches!(step, "busy:another-sweep" | WAIT_ACT_ENDS | WAIT_HAND_LIFTS) {
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
    /// The session is held: halted (`hold=1`), a drive lease, or a turn a named driver typed
    /// ([`held_by_someone`]). The exit is the holder's, save one the explicit restart made, which
    /// is carried through any hand its relaunch line waits on ([`hand_on_tab`],
    /// [`WAIT_HAND_LIFTS`]). The host also answers it for an exit another supervisor's claim holds
    /// (`aterm-gui`'s `claimed_by_another`).
    Held,
    /// Nobody owns the exit but `[harness] relaunch = false`; said once on the
    /// session's attention.
    Limited,
    /// Nobody asked: relaunch it — a crash, on the crash back-off
    /// ([`Relaunches`]).
    Relaunch,
    /// The explicit restart ended it (its stamp names the process: a
    /// person's `restart`, the stall's remedy): relaunched on its
    /// conversation whoever typed just before, carried through a hand its
    /// line waits on ([`hand_on_tab`]) — and no crash: the crash back-off
    /// neither paces nor counts it ([`Relaunches`]; LC-6 of the 2026-10-15
    /// sweep: three restarts a person asked for made the next crash wait
    /// ten minutes, the tab saying nothing).
    Asked,
    /// The harness's own restart ended the agent and left its relaunch in
    /// flight ([`restarted`]): carried ([`carry_restart`]) until it lands or is
    /// said. Typing still waits on a person via the relaunch line's own look;
    /// a hold or any hand the line waits on is waited out, uncounted
    /// ([`WAIT_HAND_LIFTS`]).
    Restarted,
}

/// Decide whose exit this was. `allowed` is `[harness] relaunch`; `status` the
/// session's `status` reply (`None`: unreadable, read as nobody); `grace_s` is
/// `[harness] human_grace_s`; `asked`: the explicit restart ended it (its
/// stamp names the process: the stall's remedy, or a person's own
/// `restart`), so a keystroke just before was no `/exit`, and a hold, a lease
/// or a turn on the tab is waited out, never handed the exit (ruling 20 of
/// 2026-10-14: the host types its relaunch only once that hand lifts,
/// [`hand_on_tab`], [`WAIT_HAND_LIFTS`], as the harness's own restart's
/// relaunch line waits on `held`); `restarted`: the harness's own restart ended it
/// ([`restarted`]), asked first because that exit is the restart's whoever is
/// at the tab ([`OnExit::Restarted`]). A person or holder owns any other exit
/// before the configuration limit is asked, which holds an asked exit too;
/// an asked exit it allows is [`OnExit::Asked`], any other [`OnExit::Relaunch`].
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

/// Whether one `status` reply says the session is held by someone the exit
/// belongs to: `hold=1`, a drive lease (`hand=lease:…`), or a named driver's
/// turn (`hand=turn:<id>:<driver>`). An unnamed turn is an owner-class caller's
/// (this host's loop), and a `driving:` hand concerns sessions it drives.
/// [`super::upgrade_drive`]'s `held` is stricter: its halt and hands are
/// [`hand_on_tab`]'s, which an asked exit is carried through.
#[must_use]
pub fn held_by_someone(status: &str) -> bool {
    status.split_whitespace().any(|t| t == "hold=1") || driver_hand(status)
}

/// The hands the relaunch line waits on, from one `status` reply: a halt (`hold=1`), or any hand
/// on the tab but this process's own (another driver's lease, any turn, a `driving:` hand). The
/// relaunch line's own look ([`super::upgrade_drive::held_by_status`]) also holds on a person's
/// keystroke and unread input, which are short and suit its counted waits. An exit the explicit
/// restart made is carried through these hands without an attempt ([`WAIT_HAND_LIFTS`]), so a turn
/// or lease of minutes does not climb the back-off. Wider than [`held_by_someone`], which names the
/// hands an unasked exit is left to.
#[must_use]
pub fn hand_on_tab(status: &str) -> bool {
    status.split_whitespace().any(|t| {
        t == "hold=1"
            || t.strip_prefix("hand=")
                .is_some_and(|hand| hand != "-" && !super::upgrade_drive::our_hand(hand))
    })
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

/// THE PERSON'S GRACE STILL TO RUN by one `status` reply (`human_ms=`):
/// how long until their last input is `grace_s` old, which a relaunch line
/// typed for them waits out (`upgrade_drive::held_by_status`, a counted wait
/// at each try). Zero with no input within it, none ever, or no stamp read.
#[must_use]
pub fn grace_left(status: &str, grace_s: u32) -> Duration {
    HumanInput::of_status(status)
        .ago()
        .map_or(Duration::ZERO, |age| {
            Duration::from_secs(u64::from(grace_s)).saturating_sub(age)
        })
}

/// The pauses before consecutive relaunches of a crash: a repeating crash is
/// not relaunched at once, and one that keeps repeating is retried every ten
/// minutes, never given up on. An exit someone asked for ([`OnExit::Asked`],
/// [`OnExit::Restarted`]) is no step of it: its own retries climb the same
/// pauses from the first ([`Relaunches`]).
pub const BACKOFF: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(10),
    Duration::from_secs(60),
    Duration::from_secs(600),
];

/// The look again after an [`Outcome::Busy`] step ([`WAIT_ACT_ENDS`], [`WAIT_HAND_LIFTS`], or
/// another actor on the upgrade state): a retry while that holds, never counted.
pub const BUSY: Duration = Duration::from_secs(10);

/// An agent that ran this long since its last relaunch of a crash starts the
/// back-off over: it crashed once, not in a loop. A streak climbed by misses
/// alone, which no landing of a crash dates, counts from the first agent seen
/// running since ([`Relaunches::running`], an asked exit's landing).
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
    /// The attention this raised is cleared: an agent runs in the tab again,
    /// or the relaunch whose word said aterm still tries is tried no more.
    Clear,
    /// The relaunch keeps failing ([`BADGE_AFTER`] in a row); it is still
    /// tried, and the attention says so.
    Failing,
    /// The relaunch can never be made: the one case a person is asked about.
    Cannot,
    /// The relaunch is limited ([`OnExit::Limited`]): the agent is not relaunched, and the
    /// attention says why.
    Limited,
}

/// One session's relaunch state as the host keeps it across workers: the
/// back-off ([`BACKOFF`], reset by a [`HEALTHY`] run), whether a relaunch is
/// due, whether the tab was left, and what the attention says. Modelled by
/// `HarnessRelaunchOnExit` in `aterm-spec`; the Tier-1 test drives this type
/// through every action.
///
/// THE BACK-OFF IS A CRASH'S (LC-6 of ATERM-KI-1015-1): only the attempts at
/// an exit nobody asked for climb it ([`Self::exited_as`]'s `crash`). An exit
/// the explicit restart made ([`OnExit::Asked`]: a person's `aterm ctl
/// restart`, the stall's remedy) or the harness's own restart
/// ([`OnExit::Restarted`]) is no crash: its attempts wait on their own count
/// ([`Self::pause`]), and its landing leaves the back-off where it was —
/// before, three restarts made a later crash wait ten minutes. An asked exit
/// whose stamp lapsed before its program ended is decided again as
/// [`OnExit::Relaunch`]: from there its end is the program's own, a crash's
/// ([`Self::decide`], the model's `Lapse`). Nor does an asked exit's landing
/// move the healthy run that forgives the crash back-off — save to start one
/// a streak of misses alone never had ([`Self::start_healthy_clock`]).
///
/// A WORD THAT ASKS FOR A CONVERSATION STANDS UNTIL IT RUNS (LC-4): the
/// tab's mark naming the command that resumes conversation `<id>`
/// ([`Self::ask`]) is answered only by an agent holding it
/// ([`Self::conversation_runs`]) or a relaunch that lands; another agent in
/// the tab — another conversation, or the told command fallen into Claude
/// Code's first-run screens, which holds none — leaves it ([`Self::running`]).
///
/// SAVE THE WORD OF A RELAUNCH THAT KEEPS FAILING, WHICH ENDS WITH ITS TRYING
/// (the review of the r20 relaunch lane, 2026-10-16; [`Self::failing`]): it
/// says aterm still tries, so the launch's own end, a person or a holder
/// back at the tab and another agent running there end it ([`Say::Clear`]),
/// and a limit or a refusal says its own word in its place, asking nothing
/// until the host names its conversation again. It stood after the trying
/// ended, until the conversation it named ran — for an upgrade's fresh
/// restart, `claude --resume` of a conversation Claude refuses, which never
/// does. So too a failing said BESIDE a word that asks for a conversation,
/// which keeps the attention ([`Self::failing_said`], the review of the
/// harness landing's eighth merge): its line of the digest ("aterm still
/// tries") ends with the trying, that word, its mark and its ask left
/// standing; it stood while nothing tried. Modelled:
/// `HarnessRelaunchOnExit`'s `TriesOnlyWhileDue` (`failing`, `said`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Relaunches {
    /// Relaunch attempts at CRASHES since the agent last ran [`HEALTHY`]ly.
    streak: u32,
    /// The exit being handled is a crash ([`Self::exited_as`]).
    crash: bool,
    /// Attempts at the exit being handled, no crash, that did not land: its
    /// pauses grow by them ([`Self::pause`]), the crash back-off does not.
    tries: u32,
    /// The conversation the session's standing word asks a person to resume
    /// ([`Self::ask`]).
    asks: Option<String>,
    /// The standing word is THIS relaunch's that keeps failing ([`Say::Failing`]:
    /// aterm still tries): it ends with the trying.
    failing: bool,
    /// This relaunch's [`Say::Failing`] was said BESIDE a standing word that
    /// asks for a conversation ([`Self::ask`]: an agent before this one that
    /// cannot be brought back), which keeps the attention: what said it (the
    /// digest's line) ends with the trying too ([`Self::tried_no_more`]),
    /// and that word, its mark and its ask stand.
    failing_said: bool,
    /// Consecutive attempts that did not relaunch.
    misses: u32,
    /// When the healthy run that forgives the crash back-off began: the last
    /// relaunch of a crash landed — or, for a streak no landing of a crash
    /// started a clock for (climbed by misses alone), when an agent was
    /// first seen running since: one someone else started ([`Self::running`])
    /// or an asked exit's landing. Neither moves a clock that runs already.
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
    /// A crash unless `decision` names an exit someone asked for
    /// ([`OnExit::Asked`], [`OnExit::Restarted`]).
    ///
    /// # Errors
    /// No relaunch is due: the [`Say`] for the session's attention.
    pub fn exited(&mut self, decision: OnExit, now: Instant) -> Result<Duration, Say> {
        let crash = !matches!(decision, OnExit::Asked | OnExit::Restarted);
        self.exited_as(decision, crash, now)
    }

    /// [`Self::exited`] of an exit that is a `crash` or not: an exit the
    /// explicit restart made, or the harness's own restart, is none, and its
    /// relaunch does not climb the crash back-off (LC-6).
    ///
    /// # Errors
    /// No relaunch is due: the [`Say`] for the session's attention.
    pub fn exited_as(
        &mut self,
        decision: OnExit,
        crash: bool,
        now: Instant,
    ) -> Result<Duration, Say> {
        self.forgive(now);
        self.crash = crash;
        self.tries = 0;
        // A word standing now is an earlier exit's, never this relaunch's.
        self.failing = false;
        self.failing_said = false;
        match self.decided(decision) {
            None => Ok(self.pause()),
            Some(say) => Err(say),
        }
    }

    /// [`on_exit`] asked again during the pause (a person or holder may be
    /// back, or configuration changed): `None` while still due, else the
    /// attention's word. An asked exit decided again as nobody's
    /// ([`OnExit::Relaunch`]: its stamp lapsed before its program ended) is a
    /// crash's from here (the model's `Lapse`).
    pub fn decide(&mut self, decision: OnExit) -> Option<Say> {
        if decision == OnExit::Relaunch && self.pending && !self.crash && !self.restarted {
            self.crash = true;
        }
        self.decided(decision)
    }

    /// The exit being handled read as `decision`, the first time
    /// ([`Self::exited_as`]) or again ([`Self::decide`]).
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

    /// A crash streak whose agent has run [`HEALTHY`]ly by `now` (since the
    /// healthy run began, [`Self::last`]) starts the back-off over: it
    /// crashed once, not in a loop.
    pub fn forgive(&mut self, now: Instant) {
        if self
            .last
            .is_some_and(|at| now.saturating_duration_since(at) >= HEALTHY)
        {
            self.streak = 0;
        }
    }

    /// The pause before the next attempt: a crash's on the crash back-off
    /// (past it by the misses an asked exit took before its stamp lapsed),
    /// an exit someone asked for on its own steps, from the first — a crash
    /// history never paces a person's restart (LC-6 of the 2026-10-15
    /// sweep: a missed first try waited ten minutes).
    #[must_use]
    pub fn pause(&self) -> Duration {
        let step = if self.crash {
            self.streak.saturating_add(self.tries)
        } else {
            self.tries
        };
        crate::supervise::ladder::Ladder(&BACKOFF).step(usize::try_from(step).unwrap_or(usize::MAX))
    }

    /// One attempt came to `outcome` at `now`: what the attention says.
    pub fn attempted(&mut self, outcome: &Outcome, now: Instant) -> Say {
        match outcome {
            Outcome::Relaunched => {
                // A landing of an exit someone asked for is no crash's: it
                // neither climbs the crash back-off nor restarts the healthy
                // run that forgives it — but it starts one a streak of misses
                // alone never had.
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
                // Its own conversation runs again, or the word's agent is
                // the one relaunched: nothing is asked of a person.
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
                    // A word asking for a conversation holds the attention
                    // (an agent before this one that cannot be brought
                    // back): this relaunch keeps failing all the same, said
                    // once beside it, and the word stays the ask's (LC-4);
                    // what said it ends with the trying ([`Self::failing_said`]).
                    self.failing_said = true;
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

    /// An agent runs in the tab again at `now`, started by someone else:
    /// nothing is due, and a word this raised is cleared — save one that asks
    /// a person to resume a conversation ([`Self::ask`]): which conversation
    /// this agent holds is not known yet, and only that one answers it
    /// ([`Self::conversation_runs`]; LC-4). A relaunch that kept failing is
    /// tried no more, and its word goes with it ([`Self::failing`]), or what
    /// said it beside such a word ([`Self::failing_said`]). A crash
    /// streak no landing started a healthy clock for starts one here
    /// ([`Self::start_healthy_clock`]).
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

    /// The session's standing word — its tab's mark — asks a person to
    /// resume `conversation` (the command it names): it stands until that
    /// conversation runs in the tab ([`Self::conversation_runs`]) or a
    /// relaunch lands, whatever other agent runs there meanwhile — save a
    /// failing relaunch's, which ends with its trying ([`Self::failing`]).
    /// Nothing is asked where no word stands.
    pub fn ask(&mut self, conversation: &str) {
        if self.badged {
            self.asks = Some(conversation.to_string());
        }
    }

    /// The agent in the tab, read, holds `conversation`: a word asking for
    /// it is answered and cleared ([`Say::Clear`]); any other word, or none,
    /// is left as it is.
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

    /// Whether the standing word is this relaunch's that keeps failing — it
    /// says aterm still tries — which ends with the trying.
    #[must_use]
    pub fn failing(&self) -> bool {
        self.failing
    }

    /// Whether this relaunch's failing was said beside a standing word that
    /// asks for a conversation ([`Self::failing_said`]): the model's `said`.
    #[must_use]
    pub fn failing_said(&self) -> bool {
        self.failing_said
    }

    /// THE RELAUNCH IS TRIED NO MORE, and nothing says so in its place (its
    /// launch's own end, a person or a holder back, another agent running):
    /// the word of its trying, where it stands, is cleared ([`Say::Clear`]) —
    /// and so is what said it beside a word that asks for a conversation
    /// ([`Self::failing_said`]), that word, its mark and its ask kept: the
    /// host keeps a mark such a word holds through a `Clear`.
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

    /// A word of a limit or a refusal takes the place of the word of a
    /// relaunch that kept failing, where it stands: that word asks for no
    /// conversation any more (the host names one again where the new word
    /// does, [`Self::ask`]). Its failing said beside another word is written
    /// over too: the window's limit and refusal end that line.
    fn replaced(&mut self) {
        self.failing_said = false;
        if std::mem::take(&mut self.failing) {
            self.asks = None;
        }
    }

    /// Whether the exit being handled is a crash ([`Self::exited_as`]).
    #[must_use]
    pub fn crash(&self) -> bool {
        self.crash
    }

    /// An agent runs at `now` after a crash streak that no landing of a
    /// crash started a healthy clock for — climbed by misses alone (a
    /// relaunch that never landed, a `Cannot`, the person resuming by hand):
    /// its healthy run starts here, so it is forgiven [`HEALTHY`] later.
    /// A clock that runs already is never moved: an asked exit's landing
    /// never pushes it later (LC-6 of the 2026-10-15 sweep).
    fn start_healthy_clock(&mut self, now: Instant) {
        if self.last.is_none() && self.streak > 0 {
            self.last = Some(now);
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
