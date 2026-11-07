// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Agent relaunch primitive: resumes an exited agent in its own tab on its existing conversation.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::Value;

use super::accounts;
use super::token_route;
use super::upgrade::{self, Dialect, SessionFile, Version};
#[cfg(test)]
use super::upgrade_drive::state_dir;
use super::upgrade_drive::{
    Client, Hand, Job, Kernel, Live, Opts, Records, Report, SAME_PROGRAM, Screen,
    SessionRosterCache, St, TAIL_BYTES, Targets, Terminated, Typed, WhereWritten, alive,
    composer_clear, composer_empty, connect, continuation_composer_empty, conversation_live,
    conversation_typed_by, credits_billed_for, cwd_of, earlier_tabs, exe_of,
    finished_where_written, first_word, foreground_shell, held, home_carried, host_roster, ids,
    is_our_ancestor, kept_move, kernel_start, launch_kept, ledger, left_to_person, line_model_of,
    live_background, load_record, model_and_mark, native_root, note_stuck, now_s, owned_by_aterm,
    process_in_tab, read_upgrade_dir, record_asked, restart_models, restore, roster, said, save,
    save_checked, screen, since, squash, sweep_lock, table, tail_to_end, take_back_ask, terminate,
    turn_fenced, type_line, typing_fence, under_hand, unique_tab_for_group,
};
use crate::supervise::approvals;
use crate::supervise::limit::one_line;
use crate::supervise::policy::turn_end::RestartClaim;
use crate::supervise::policy::turn_end::TurnEndReading;
pub use crate::supervise::policy::turn_end::{Restart, RestartAsk};
use crate::supervise::screen::HumanInput;

#[cfg(all(test, unix))]
#[path = "relaunch_native_gate_tests.rs"]
mod native_gate_tests;

/// A relaunch planned from the live process.
pub(super) struct Plan {
    pub(super) shell: u32,
    pub(super) dialect: Dialect,
    pub(super) line: String,
    /// Why [`Self::line`] does not carry the usage tap its gate offered it ([`line_tap_kept`]):
    /// recorded when the line is typed ([`note_dropped`]).
    pub(super) tap_dropped: Option<String>,
    /// Why [`Self::line`] does not carry the token wrapper its judgment offered it
    /// ([`token_route::judge`]; the tap went first): recorded when the line is typed
    /// ([`note_dropped`]).
    pub(super) token_dropped: Option<String>,
    /// The directory [`Self::line`] runs the agent in when it carries no `cd` of its own: the
    /// agent's, where the shell stood when the line was planned (`None`: the line carries its own
    /// `cd`, or no directory was read). The record keeps it ([`St::line_dir`]), and the line typed
    /// once the shell stands elsewhere carries a `cd` back to it ([`line_at_shell`]).
    pub(super) line_dir: Option<String>,
    /// The exact flags/environment selected while rendering `line`, before native admission.
    pub(super) command: Option<PlannedCommand>,
    pub(super) cold: Option<super::native_act::ColdIntent>,
}

#[derive(Clone, Debug)]
pub(super) struct PlannedCommand {
    pub(super) dialect: Dialect,
    pub(super) heal: Option<PathBuf>,
    pub(super) cwd: Option<String>,
    pub(super) target: PathBuf,
    pub(super) argv: Vec<String>,
    pub(super) env: Vec<(String, String)>,
    pub(super) unset: Option<Vec<String>>,
    pub(super) account_intent: Option<super::accounts_pool::ExecutionIntent>,
}

impl PlannedCommand {
    /// The program a native restart of this command expects it to execute
    /// (`native_restart::begin`'s target): aterm's token wrapper where the command serves the
    /// account's vault token ([`super::accounts_pool::ExecutionIntent::Token`]), else `program`,
    /// the build the caller planned it for.
    pub(super) fn expects<'a>(command: Option<&'a Self>, program: &'a Path) -> &'a Path {
        match command {
            Some(command)
                if matches!(
                    command.account_intent,
                    Some(super::accounts_pool::ExecutionIntent::Token { .. })
                ) =>
            {
                &command.target
            }
            _ => program,
        }
    }

    pub(super) fn capture(&self) -> Option<super::native_act::RestartCommand> {
        // A token command's program is aterm's wrapper, which execs the build its line names in
        // the same pid: the host holds the child to that build, captured here with the wrapper.
        let settles = match self.account_intent {
            Some(super::accounts_pool::ExecutionIntent::Token { .. }) => Some(
                super::native_act::Image::at(TokenServe::served_build(&self.argv)?)?,
            ),
            Some(_) | None => None,
        };
        Some(super::native_act::RestartCommand {
            image: super::native_act::Image::at(&self.target)?,
            program: self.target.to_str()?.to_string(),
            argv: self.argv.clone(),
            env: self.env.clone(),
            unset: self.unset.clone(),
            cwd: self.cwd.clone(),
            heal: match &self.heal {
                None => None,
                Some(path) => Some(path.to_str()?.to_string()),
            },
            dialect: self.dialect,
            settles,
            account_intent: self.account_intent.clone().map(Box::new),
        })
    }
}

struct SelectedCommand {
    line: String,
    /// What `line` executes: the launch's `exe`, the build past its twin ([`managed_twin_build`]),
    /// or aterm's token wrapper where the line serves the vault token ([`TokenServe`]).
    exe: PathBuf,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    tap_dropped: Option<String>,
    token_dropped: Option<String>,
    /// The token entry the line serves through the wrapper, where it carries it.
    served: Option<accounts::TokenEntry>,
}

/// What a relaunch line types for a token decision ([`token_route::Decision::Token`]):
/// `<aterm> harness accounts token-run <named> -- <build> <flags…>` (§5.4).
#[derive(Clone, Debug)]
pub(super) struct TokenServe {
    /// The aterm the line runs.
    pub(super) aterm: PathBuf,
    /// The row's label, or a row-less account's email.
    pub(super) named: String,
    /// The store build, as the twin names it ([`super::upgrade_drive::twin_build`]).
    pub(super) build: PathBuf,
    /// The account's token entry.
    pub(super) entry: accounts::TokenEntry,
    /// The account has a stored login on this Mac (its login record is here): a line that drops
    /// the wrapper for want of room falls back to it, and one with none is no plan.
    pub(super) stored: bool,
}

impl TokenServe {
    /// The wrapper's arguments around `flags`.
    fn args(&self, flags: &[String]) -> Vec<String> {
        token_route::VERB
            .iter()
            .map(|word| (*word).to_owned())
            .chain([
                self.named.clone(),
                "--".to_owned(),
                self.build.to_string_lossy().into_owned(),
            ])
            .chain(flags.iter().cloned())
            .collect()
    }

    /// The build a wrapper line's arguments ([`Self::args`]) serve: the word after the `--` that
    /// follows the verb and the account. `None` for any other argv.
    fn served_build(argv: &[String]) -> Option<&Path> {
        let verb = token_route::VERB.len();
        let named = argv.get(..verb)?.iter().eq(token_route::VERB.iter());
        (named && argv.get(verb + 1).map(String::as_str) == Some("--"))
            .then(|| argv.get(verb + 2).map(Path::new))
            .flatten()
    }
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
    /// What the line executes, but for the managed twin on a line it would make yolo
    /// ([`past_unmarked_twin`]) or in a cold plan ([`plan_with_cold`]), where the line executes
    /// the twin's build.
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
    /// How the process it replaces authenticated ([`Snapshot::route`]).
    pub(super) route: accounts::AuthRoute,
    /// The authentication route was positively observed, rather than omitted by a legacy record.
    pub(super) auth_observed: bool,
    /// What the replaced agent's own exec environment recorded of its shell's, where it was
    /// recorded ([`Snapshot::launch_env`]).
    pub(super) launch_env: Option<&'a accounts::LaunchEnv>,
    /// Whether the line moves the model: without usage credits, a move needs a named model and a
    /// captured Standard billing context equal to the target's.
    pub(super) billing: ModelBilling<'a>,
    /// What the line does with its account's vault token ([`Serve`]).
    pub(super) serve: Serve,
}

/// What a relaunch line does with its account's vault token
/// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.4, §5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Serve {
    /// The one judgment decides ([`token_route::judge`]): the wrapper where it says token.
    Judge,
    /// Never: the line is on the row's stored login (a move back off a token that cannot serve,
    /// an upgrade's rollback, which a native restart holds to the replaced process's own image).
    Never,
    /// The line serves the token, else there is no plan (a move onto it, [`Restart::Route`]).
    Must,
}

/// What a plan answers where a move onto the token finds no line that serves it ([`Serve::Must`]).
pub(super) const ROUTE_UNSERVED: &str = "route-unserved";

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

/// Reuse the original model-billing rule for a deferred login selection.
#[must_use]
pub fn cold_profile_allows(
    intent: &super::native_act::ColdIntent,
    seed: &super::native_act::RestartCommand,
    resolved: &super::native_act::RestartCommand,
    user_home: &Path,
    cwd: &str,
) -> bool {
    if !seed.cold_resolution_agrees(resolved) {
        return false;
    }
    if intent.billing != super::native_act::ColdBilling::PreserveProfile {
        return true;
    }
    let required = resolved
        .env
        .iter()
        .filter_map(|(key, value)| {
            let key = match key.as_str() {
                accounts::LOGIN_ENV => accounts::LOGIN_ENV,
                accounts::HOME_ENV => accounts::HOME_ENV,
                accounts::HOMEDIR_ENV => accounts::HOMEDIR_ENV,
                _ => return None,
            };
            Some((key, value.as_str()))
        })
        .collect::<Vec<_>>();
    let target = intent
        .profile
        .as_ref()
        .and_then(|from| from.for_launch(user_home, cwd, &required));
    cold_profile_allows_context(
        intent,
        seed,
        resolved,
        target.as_ref().map(|env| &env.context),
    )
}

/// Apply the existing billing law to the actual helper's resolved profile context.
#[must_use]
pub fn cold_profile_allows_context(
    intent: &super::native_act::ColdIntent,
    seed: &super::native_act::RestartCommand,
    resolved: &super::native_act::RestartCommand,
    target: Option<&super::profile::Context>,
) -> bool {
    if !seed.cold_resolution_agrees(resolved) {
        return false;
    }
    if intent.billing != super::native_act::ColdBilling::PreserveProfile {
        return true;
    }
    let model = seed
        .argv
        .windows(2)
        .find(|words| words[0] == "--model")
        .map(|words| words[1].as_str());
    billing_allows_launch(
        false,
        ModelBilling::Move(intent.profile.as_ref()),
        model,
        target,
    )
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
    plan_with_cold(opts, launch, shell, t, None)
}

fn plan_with_cold(
    opts: &Opts,
    launch: &Launch<'_>,
    shell: u32,
    t: &[super::upgrade_drive::ProcRow],
    cold: Option<super::native_act::ColdIntent>,
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
    let cd = (!want.is_empty() && !same_dir(cwd_of(shell).as_deref(), &want)).then(|| want.clone());
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
    // The one judgment (§5.3): a token decision types the wrapper before the store build. A cold
    // plan's route is resolved by its own helper, after this.
    let token = match launch.serve {
        Serve::Judge | Serve::Must if cold.is_none() => token_serve(opts, launch, &want),
        Serve::Judge | Serve::Must | Serve::Never => None,
    };
    // No inherited token state where the replaced process ran on its vault token: a line on the
    // stored login carries neither the descriptor nor the marker (§5.4). A line that serves the
    // token unsets neither (`served_unset`), since the wrapper makes both afresh, and any line
    // after another route keeps the length it had, so its usage tap still fits Darwin's cap.
    let served_unset: Option<Vec<&str>> = exact.as_ref().map(|env| env.unset.clone());
    let unset: Option<Vec<&str>> = match launch.route {
        accounts::AuthRoute::VaultToken { .. } => {
            let mut names = served_unset.clone().unwrap_or_default();
            names.extend(token_route::TOKEN_STATE_ENV);
            Some(names)
        }
        accounts::AuthRoute::Stored | accounts::AuthRoute::Outside => served_unset.clone(),
    };
    // A cold restart execs what its command names and holds the child to that image, and a script
    // twin's exec maps its build: named the twin, the agent runs and its outcome never matches.
    let cold_build = cold.as_ref().and_then(|_| managed_twin_build(launch.exe));
    let selected = line_tap_selected(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        cold_build.as_deref().unwrap_or(launch.exe),
        launch.argv,
        launch.resume.then_some(launch.session),
        model,
        launch.mode,
        if exact.is_some() {
            &exact_values
        } else {
            &required
        },
        unset.as_deref(),
        served_unset.as_deref(),
        tap.as_deref(),
        token.as_ref(),
    )?;
    // What the selected line unsets, which the command's capture renders again.
    let unset = if selected.served.is_some() {
        served_unset
    } else {
        unset
    };
    if launch.serve == Serve::Must && selected.served.is_none() {
        return Err(NoPlan::Refused {
            why: ROUTE_UNSERVED.to_string(),
            what: ROUTE_UNSERVED.to_string(),
            detail: "the account's vault token does not serve this launch here, or its line has \
                     no room for aterm's wrapper"
                .to_string(),
        });
    }
    // Profile reconstruction can supply a login absent from the original argv.
    // Bind the environment actually retained by this command, after selection.
    let selected_value = |name: &str| {
        selected
            .env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    // A line that serves the token carries a token intent; one that does not is on the stored
    // login, whatever the replaced process ran on (§5.2).
    let route = match selected.served {
        Some(entry) => accounts::AuthRoute::VaultToken { entry },
        None => match launch.route {
            accounts::AuthRoute::VaultToken { .. } => accounts::AuthRoute::Stored,
            route @ (accounts::AuthRoute::Stored | accounts::AuthRoute::Outside) => route,
        },
    };
    let selected_launch = Launch {
        home: selected_value(accounts::HOME_ENV).or(launch.home),
        homedir: selected_value(accounts::HOMEDIR_ENV).or(launch.homedir),
        login: selected_value(accounts::LOGIN_ENV).or(launch.login),
        route,
        ..*launch
    };
    let unset = unset.map(|names| {
        names
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>()
    });
    let account_intent = if cold.is_some() {
        // This is only the immutable recipe. The exact owned helper supplies the
        // observed route before Resolve selects and captures its account intent.
        None
    } else {
        let mut intent =
            command_account(opts, &selected_launch).map_err(|_| NoPlan::Wait("account"))?;
        intent.bind_external_environment(&selected.env, unset.as_deref(), launch.launch_env);
        Some(intent)
    };
    Ok(Plan {
        cold,
        shell,
        dialect,
        line: selected.line,
        tap_dropped: selected.tap_dropped,
        token_dropped: selected.token_dropped,
        line_dir: (cd.is_none() && !want.is_empty()).then_some(want),
        command: Some(PlannedCommand {
            dialect,
            heal,
            cwd: cd,
            target: selected.exe,
            argv: selected.argv,
            env: selected.env,
            unset,
            account_intent,
        }),
    })
}

/// The store build a token line types for `exe`, the launch's program: the managed twin's
/// build ([`super::upgrade_drive::twin_build`], so the line never types the twin), else `exe`
/// itself; only a Claude Code build in atpkg's own store ([`token_route::store_build_in`] of
/// [`token_builds`]), the one gate the wrapper judges the same build by at exec. `None` where it
/// cannot be read or lies in no such store: the plan is then the stored login's, typed as today
/// (through the twin, whose `--auto` judges at exec), and a move onto the token has no plan.
fn token_build(exe: &Path) -> Option<PathBuf> {
    let build = match super::upgrade_drive::managed_agent_twin(token_route::PROGRAM) {
        Some(twin) if twin == exe => super::upgrade_drive::twin_build(exe, token_route::PROGRAM)?,
        _ => exe.to_path_buf(),
    };
    token_route::store_build_in(&token_builds()?, &build).map(|_| build)
}

/// atpkg's own store of Claude Code builds ([`token_route::configured_builds`]); a test's, where
/// it set one ([`TOKEN_BUILDS`]).
fn token_builds() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(builds) = TOKEN_BUILDS.with(|seam| seam.borrow().clone()) {
        return Some(builds);
    }
    token_route::configured_builds()
}

/// The plan's half of the one judgment ([`token_route::judge`]): what a relaunch of `launch` in
/// `cwd` types the wrapper for, where its account's token is in use here. The account is the one
/// the replaced process's login resolves; the claims ring is admission's, under
/// `accounts.lock`, before the line is typed.
fn token_serve(opts: &Opts, launch: &Launch<'_>, cwd: &str) -> Option<TokenServe> {
    let build = token_build(launch.exe);
    let facts = token_route::Facts {
        macos: cfg!(target_os = "macos"),
        no_harness: false,
        argv: launch.argv,
        store_build: build.is_some(),
    };
    let asked = PlanAsk { opts, launch, cwd };
    match token_route::judge(&facts, &asked) {
        token_route::Decision::Token { account, entry } => Some(TokenServe {
            aterm: token_aterm()?,
            named: account.named().to_owned(),
            build: build?,
            entry,
            stored: super::profile::login_record(
                &opts.home,
                launch.login.filter(|login| !login.is_empty()),
            )
            .is_some(),
        }),
        token_route::Decision::Stored(_) | token_route::Decision::AsTyped(_) => None,
    }
}

/// The aterm a token line runs: this process's lasting one ([`super::tap::stable_exe`]).
fn token_aterm() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some((aterm, _)) = TOKEN_SEAM.with(|seam| seam.borrow().clone()) {
        return Some(aterm);
    }
    super::tap::stable_exe()
}

#[cfg(test)]
thread_local! {
    /// A test's aterm for token lines and the token's standing it reads, in place of this
    /// process's and the vault's.
    pub(super) static TOKEN_SEAM: std::cell::RefCell<Option<(PathBuf, token_route::TokenFact)>> =
        const { std::cell::RefCell::new(None) };
    /// A test's store of Claude Code builds, in place of atpkg's configured one.
    pub(super) static TOKEN_BUILDS: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// [`token_route::Ask`] for a relaunch plan.
struct PlanAsk<'a> {
    opts: &'a Opts,
    launch: &'a Launch<'a>,
    cwd: &'a str,
}

impl token_route::Ask for PlanAsk<'_> {
    fn account(&self) -> Result<(token_route::Account, Option<accounts::TokenEntry>), String> {
        let launch = self.launch;
        let settings = super::upgrade_drive::tap_settings(self.opts, launch.argv, self.cwd);
        let profile = super::upgrade_drive::tap_profile_store(self.opts);
        let own_home = launch.home.is_none_or(str::is_empty)
            && launch
                .homedir
                .is_none_or(|home| same_dir(Some(home), &self.opts.home.to_string_lossy()));
        let who = accounts::ProcessLogin {
            login: launch
                .login
                .filter(|login| !login.is_empty())
                .map(str::to_owned),
            default_home: own_home,
            route: launch.route,
        }
        .with_settings(&settings)
        .with_profile_store(profile);
        let roster = self
            .opts
            .account_roster
            .as_deref()
            .filter(|path| path.exists())
            .and_then(|path| accounts::Roster::load_for(accounts::Cli::Claude, path).ok());
        token_route::account_of(
            &who,
            roster.as_ref(),
            token_route::Named::Auto,
            &|login| {
                super::profile::login_record(&self.opts.home, login).map(|record| record.email)
            },
            &|| None,
        )
    }

    fn claims(&self, _account: &token_route::Account) -> Option<token_route::Fallback> {
        None
    }

    fn token(
        &self,
        entry: accounts::TokenEntry,
        account: &token_route::Account,
    ) -> token_route::TokenFact {
        #[cfg(test)]
        if let Some((_, fact)) = TOKEN_SEAM.with(|seam| seam.borrow().clone()) {
            return fact;
        }
        token_fact(self.opts, entry, account)
    }
}

/// The token's standing for a plan, as admission reads it: this Mac's vault under the user's
/// home and its table.
#[cfg(unix)]
fn token_fact(
    opts: &Opts,
    entry: accounts::TokenEntry,
    account: &token_route::Account,
) -> token_route::TokenFact {
    let Ok(seat) = account_seat(opts) else {
        return token_route::TokenFact::Busy;
    };
    let store = seat.store(accounts::Cli::Claude);
    token_route::standing_fact(
        &aterm_secrets::client::Paths::for_home(&opts.home),
        &store,
        entry,
        account,
        u64::try_from(seat.now).unwrap_or(0),
        super::accounts_token_run::LABELLED_WAIT,
    )
    .0
}

#[cfg(not(unix))]
fn token_fact(
    _opts: &Opts,
    _entry: accounts::TokenEntry,
    _account: &token_route::Account,
) -> token_route::TokenFact {
    token_route::TokenFact::Absent
}

fn account_seat(opts: &Opts) -> Result<super::accounts_pool::Seat<'_>, &'static str> {
    let roster = opts
        .account_roster
        .as_deref()
        .ok_or("account namespace unavailable")?;
    let config_dir = roster
        .parent()
        .filter(|_| {
            roster
                .file_name()
                .is_some_and(|name| name == accounts::Cli::Claude.accounts().roster_file)
        })
        .ok_or("account namespace invalid")?;
    Ok(super::accounts_pool::Seat {
        config_dir,
        state: &opts.state,
        home: &opts.home,
        now: i64::try_from(now_s()).map_err(|_| "account clock unavailable")?,
    })
}

fn command_account(
    opts: &Opts,
    launch: &Launch<'_>,
) -> Result<super::accounts_pool::ExecutionIntent, String> {
    if !launch.auth_observed {
        return Err("command authentication attribution is unknown".into());
    }
    // A launch path carries the route: a token session's relaunch keeps a `Token` intent, and
    // admission checks its row's custody, claims and token (§5.2).
    let route = match launch.route {
        accounts::AuthRoute::Outside => {
            return Ok(super::accounts_pool::ExecutionIntent::external());
        }
        route @ (accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. }) => route,
    };
    if launch.home.is_some_and(|home| !home.is_empty())
        || launch
            .homedir
            .is_some_and(|home| !same_dir(Some(home), &opts.home.to_string_lossy()))
    {
        {
            let mut directories = Vec::new();
            if let Some(home) = launch.home.filter(|home| !home.is_empty()) {
                directories.push(PathBuf::from(home));
            } else if let Some(home) = launch.homedir {
                directories
                    .push(Path::new(home).join(accounts::Cli::Claude.accounts().default_home));
            }
            if let Some(login) = launch.login.filter(|login| !login.is_empty()) {
                directories.push(PathBuf::from(login));
            }
            for directory in &mut directories {
                if directory.is_relative() {
                    *directory = Path::new(launch.cwd).join(&*directory);
                }
            }
            super::accounts_pool::admit_external_execution(
                &account_seat(opts)?,
                accounts::Cli::Claude,
                &directories,
            )?;
        }
        return Ok(super::accounts_pool::ExecutionIntent::external());
    }
    super::accounts_pool::capture_execution(
        &account_seat(opts)?,
        accounts::Cli::Claude,
        &accounts::ProcessLogin {
            login: launch
                .login
                .filter(|login| !login.is_empty())
                .map(str::to_owned),
            default_home: true,
            route,
        },
    )
}

fn account_execution(
    opts: &Opts,
    intent: Option<&super::accounts_pool::ExecutionIntent>,
) -> Result<Option<super::accounts_pool::ExecutionGuard>, String> {
    let intent = intent.ok_or("saved command authentication attribution is unknown")?;
    if matches!(
        intent,
        super::accounts_pool::ExecutionIntent::External { .. }
    ) {
        return Ok(None);
    }
    super::accounts_pool::admit_execution(&account_seat(opts)?, accounts::Cli::Claude, intent)
        .map(Some)
}

fn external_at_shell(
    opts: &Opts,
    intent: Option<&super::accounts_pool::ExecutionIntent>,
    shell: u32,
) -> Result<Option<super::accounts_pool::ExecutionGuard>, String> {
    let Some(intent) = intent else {
        return Ok(None);
    };
    if !matches!(
        intent,
        super::accounts_pool::ExecutionIntent::External { .. }
    ) {
        return Ok(None);
    }
    let inherited = match atpkg::caller_shell::process_args(shell) {
        Some(args) => args.env,
        // An unread environment reads as an empty one only where the agent recorded its
        // launch environment, the base the intent then reads instead.
        None if matches!(
            intent,
            super::accounts_pool::ExecutionIntent::External {
                launch: Some(_),
                ..
            }
        ) =>
        {
            Vec::new()
        }
        None => return Err("shell authentication environment is unavailable".into()),
    };
    external_from_env(opts, intent, &inherited, cwd_of(shell).as_deref())
}

fn external_from_env(
    opts: &Opts,
    intent: &super::accounts_pool::ExecutionIntent,
    inherited: &[String],
    cwd: Option<&str>,
) -> Result<Option<super::accounts_pool::ExecutionGuard>, String> {
    let env = intent
        .external_environment(inherited)
        .ok_or("external authentication context is unavailable")?;
    let who = accounts::ProcessLogin::of_exec_env(&env)
        .ok_or("shell authentication attribution is unavailable")?;
    let value = |name: &str| {
        env.iter()
            .filter_map(|entry| entry.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
    };
    let home = value(accounts::HOMEDIR_ENV).ok_or("shell home is unavailable")?;
    let same_home = same_dir(Some(home), &opts.home.to_string_lossy());
    // An external intent names no account, so it names no token entry.
    if !intent.matches_process(&who, same_home, None) {
        return Err("external authentication route is no longer present".into());
    }
    match who.route {
        accounts::AuthRoute::Outside => return Ok(None),
        accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. } => {}
    }
    let mut directories = Vec::new();
    if let Some(custom) = value(accounts::HOME_ENV).filter(|path| !path.is_empty()) {
        directories.push(PathBuf::from(custom));
    } else if !same_home {
        directories.push(Path::new(home).join(accounts::Cli::Claude.accounts().default_home));
    }
    if let Some(login) = who.login {
        directories.push(PathBuf::from(login));
    }
    for directory in &mut directories {
        if directory.is_relative() {
            *directory = Path::new(cwd.ok_or("shell directory is unavailable")?).join(&*directory);
        }
    }
    super::accounts_pool::admit_external_execution(
        &account_seat(opts)?,
        accounts::Cli::Claude,
        &directories,
    )
    .map(Some)
}

fn checks_account_execution(agent: &upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex | upgrade::Agent::Agy | upgrade::Agent::Muse => false,
    }
}

/// Whether the shell's directory `shell` (the kernel's spelling) is `want` (the agent's, often a
/// session file's logical `$PWD`), by [`super::upgrade_drive::same_dir`]: one directory under two
/// spellings (`/tmp` and `/private/tmp`, a symlinked home) is one directory, and a relaunch into
/// it needs no `cd`. Every lane's relaunch plan asks this.
pub(super) fn same_dir(shell: Option<&str>, want: &str) -> bool {
    shell.is_some_and(|shell| super::upgrade_drive::same_dir(Path::new(shell), Path::new(want)))
}

fn tap_of(
    opts: &Opts,
    launch: &Launch<'_>,
    armed: &super::tap::Armed,
    cwd: &str,
) -> Option<String> {
    let lone = armed.lone_for(launch.login);
    // The tap reports a token session's reading as a stored login's; its consumer credits the
    // row whose entry the marker names (§5.7).
    let own = match launch.route {
        accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. } => true,
        accounts::AuthRoute::Outside => false,
    } && launch.homedir.is_none()
        && super::tap::own_login(launch.login, launch.home, lone);
    if !own || super::tap::settings_value(&armed.exe).is_none() {
        return None;
    }
    let settings = super::upgrade_drive::tap_settings(opts, launch.argv, cwd);
    let user = super::upgrade_drive::tap_user_settings(&opts.home);
    super::tap::gate(&super::tap::GateFacts {
        exe: Some(&armed.exe),
        route: launch.route,
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
    line_tap_kept(
        dialect, heal, cd, exe, argv, session, model, mode, required, unset, tap,
    )
    .map(|(line, _)| line)
}

/// [`line_tapped_clear`]'s line, and why it does not carry the usage tap `tap` it was offered
/// (`None`: it carries it, or none was offered): the tty's line cap had no room for it even
/// without the launch's own environment ([`CLAUDE_LAUNCH_ENV`]), so the line falls back to no tap
/// rather than to no relaunch. The plan reports it ([`Plan::tap_dropped`]), so the drop is
/// recorded ([`note_dropped`]), never silent.
#[allow(clippy::too_many_arguments)]
pub(super) fn line_tap_kept(
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
) -> Result<(String, Option<String>), NoPlan> {
    line_tap_selected(
        dialect, heal, cd, exe, argv, session, model, mode, required, unset, unset, tap, None,
    )
    .map(|selected| (selected.line, selected.tap_dropped))
}

/// The build the twin `exe` execs, where `exe` is Claude Code's managed twin and the build can be
/// read.
fn managed_twin_build(exe: &Path) -> Option<PathBuf> {
    let twin = super::upgrade_drive::managed_agent_twin("claude")?;
    if twin != exe {
        return None;
    }
    super::upgrade_drive::twin_build(exe, "claude")
}

/// The build to type in place of `exe` ([`managed_twin_build`]), where `flags` hold nothing the
/// twin reads as typed: it would add yolo, and a relaunch never enters bypass
/// ([`with_permission_mode`]). A typed line with `--resume` or a mode written on it stays the
/// twin's. [`Plan::command`] names what the line types, so a cold restart execs the same file.
fn past_unmarked_twin(exe: &Path, flags: &[String]) -> Option<PathBuf> {
    let atpkg::yolo::Launch::Default(_) = atpkg::yolo::decide("claude", flags) else {
        return None;
    };
    managed_twin_build(exe)
}

#[allow(clippy::too_many_arguments)]
fn line_tap_selected(
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
    served_unset: Option<&[&str]>,
    tap: Option<&str>,
    token: Option<&TokenServe>,
) -> Result<SelectedCommand, NoPlan> {
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
    let build = past_unmarked_twin(exe, &flags);
    let exe = build.as_deref().unwrap_or(exe);
    let both: Vec<(&str, &str)> = required.iter().copied().chain(CLAUDE_LAUNCH_ENV).collect();
    let tapped = tap.map(|value| with_tap(flags.clone(), value));
    let line = |env: &[(&str, &str)], flags: &[String], served: Option<&TokenServe>| {
        // A line that serves the token unsets `served_unset`, what the wrapper does not make
        // afresh; any other line `unset`.
        let (exe, shim_of, args, unset) = match served {
            None => (exe, exe, flags.to_vec(), unset),
            Some(serve) => (
                serve.aterm.as_path(),
                serve.build.as_path(),
                serve.args(flags),
                served_unset,
            ),
        };
        upgrade::relaunch_line_selected_for(dialect, heal, cd, env, unset, shim_of, exe, &args).map(
            |selected| SelectedCommand {
                line: selected.line,
                exe: exe.to_path_buf(),
                argv: args,
                env: selected.env,
                tap_dropped: None,
                token_dropped: None,
                served: served.map(|serve| serve.entry),
            },
        )
    };
    let fit = |flags: &[String], served: Option<&TokenServe>| {
        line(&both, flags, served).or_else(|_| line(required, flags, served))
    };
    let left = |what: &str, why: String, kept: &SelectedCommand| {
        format!(
            "{what} left this relaunch line: with it, {why}; without it, {} bytes",
            kept.line.len()
        )
    };
    // The line budget (§5.4): the tap goes first, then the wrapper, and the line is the stored
    // login's.
    let tap_dropped = match tapped.as_deref() {
        None => None,
        Some(t) => match fit(t, token) {
            Ok(tapped) => return Ok(tapped),
            Err(why) => Some(why),
        },
    };
    let token_dropped = match token {
        None => None,
        Some(serve) => match fit(&flags, Some(serve)) {
            Ok(mut served) => {
                served.tap_dropped = tap_dropped.map(|why| left("the usage tap", why, &served));
                return Ok(served);
            }
            Err(why) if serve.stored => Some(why),
            Err(why) => {
                return Err(NoPlan::Refused {
                    why: format!("line:{why}"),
                    what: "line".to_string(),
                    detail: format!(
                        "the token wrapper left this relaunch line ({why}), and {} has no \
                         stored login on this Mac to start on without it",
                        serve.named
                    ),
                });
            }
        },
    };
    fit(&flags, None)
        .map(|mut plain| {
            plain.tap_dropped = tap_dropped.map(|why| left("the usage tap", why, &plain));
            plain.token_dropped = token_dropped.map(|why| left("the token wrapper", why, &plain));
            plain
        })
        .map_err(|e| NoPlan::Refused {
            why: format!("line:{e}"),
            what: "line".to_string(),
            detail: e,
        })
}

/// The ledger's step for a relaunch typed without the usage tap its plan offered it, for want of
/// room on the tty's line ([`line_tap_kept`]).
pub(super) const TAP_DROPPED: &str = "tap-dropped";

/// The ledger's step for a relaunch typed without the token wrapper its judgment offered it, for
/// want of room on the tty's line once the tap went ([`line_tap_selected`]).
pub(super) const TOKEN_DROPPED: &str = "token-dropped";

/// Record, beside the relaunch `r` whose line is about to be typed, that its line dropped the
/// usage tap, or the token wrapper, where it did ([`Plan::tap_dropped`],
/// [`Plan::token_dropped`]).
pub(super) fn note_dropped(opts: &Opts, r: &Report, tap: Option<&str>, token: Option<&str>) {
    if let Some(why) = tap {
        ledger(opts, &said(r.clone(), TAP_DROPPED), why);
    }
    if let Some(why) = token {
        ledger(opts, &said(r.clone(), TOKEN_DROPPED), why);
    }
}

/// `flags` with the usage tap's pair, `--settings <value>`, before the trailing `--resume` pair (at the end of a fresh start's flags).
pub(crate) fn with_tap(flags: Vec<String>, value: &str) -> Vec<String> {
    let spans = upgrade::parse_flag_spans(&flags);
    let resume_at = spans
        .iter()
        .rposition(|(name, _)| *name == Some("--resume"));
    let mut out = Vec::with_capacity(flags.len() + 2);
    for (idx, (_, tokens)) in spans.into_iter().enumerate() {
        if Some(idx) == resume_at {
            out.push("--settings".to_string());
            out.push(value.to_string());
        }
        out.extend(tokens);
    }
    if resume_at.is_none() {
        out.push("--settings".to_string());
        out.push(value.to_string());
    }
    out
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
    let spans = upgrade::parse_flag_spans(&flags);
    let mut out = Vec::with_capacity(flags.len() + 2);
    let mut tail = Vec::new();
    let mut in_tail = false;
    for (name, tokens) in spans {
        if in_tail {
            tail.extend(tokens);
        } else if name == Some("--model") {
        } else if name == Some("--resume") {
            in_tail = true;
            tail.extend(tokens);
        } else {
            out.extend(tokens);
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
pub(crate) fn screen_mode(rows: &[String]) -> Option<String> {
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
pub(crate) fn with_permission_mode(flags: Vec<String>, mode: Option<&str>) -> Vec<String> {
    let Some(mode) = mode.filter(|m| *m != "bypassPermissions") else {
        return flags;
    };
    let mode = if mode == "manual" { "default" } else { mode };
    let spans = upgrade::parse_flag_spans(&flags);
    let mut out = Vec::with_capacity(flags.len() + 3);
    let mut tail = Vec::new();
    let mut in_tail = false;
    let mut bypass_in_cycle = false;
    for (name, tokens) in spans {
        if in_tail {
            tail.extend(tokens);
            continue;
        }
        match name {
            Some("--dangerously-skip-permissions" | "--allow-dangerously-skip-permissions") => {
                bypass_in_cycle = true;
            }
            Some("--permission-mode") => {
                bypass_in_cycle |= match tokens.as_slice() {
                    [one] => one == "--permission-mode=bypassPermissions",
                    [_, m] => m == "bypassPermissions",
                    _ => false,
                };
            }
            Some("--resume") => {
                in_tail = true;
                tail.extend(tokens);
            }
            _ => out.extend(tokens),
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
    if st.native_retired_unissued {
        return None;
    }
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
            if st.found_gone {
                REGISTERED_AND_ENDED
            } else {
                "the relaunched agent never registered the conversation"
            }
            .to_string(),
        )),
        _ => None,
    }
}

/// What the ledger says of a relaunch found on the conversation and gone before it came up.
const REGISTERED_AND_ENDED: &str =
    "the relaunched agent registered the conversation and ended before it came up";

/// The word a Claude restart in flight stops with once another conversation holds its tab
/// ([`overtaken`]).
pub const TAB_TAKEN: &str = "tab-taken";

/// Whether a Claude restart in flight can be overtaken: it has no live agent of its own (it is
/// exiting, or relaunched with no relaunched agent or one that is gone) and its agent is gone.
/// Once relaunched afresh ([`starts_afresh`]) it never is: the agent its line started holds a new
/// conversation from a launch that resumes none, which [`overtaken`] cannot tell from another
/// conversation's agent, so its held wait ([`await_new`]) adopts that agent (`done:fresh`).
pub(super) fn overtakable(st: &St, alive: impl Fn(u32) -> bool) -> bool {
    let unheld = match st.phase {
        upgrade::Phase::Exiting { .. } => true,
        upgrade::Phase::Relaunched { .. } if starts_afresh(&st.cause) => false,
        upgrade::Phase::Relaunched { .. } => {
            st.relaunched_pid == 0 || st.relaunched_gone(&alive, kernel_start)
        }
        _ => false,
    };
    // The tab's holder is read from Claude Code's own session files.
    let claude = match st.agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex | upgrade::Agent::Muse | upgrade::Agent::Agy => false,
    };
    claude
        && unheld
        && (st.exited_at != 0 || st.agent_exit_by(&alive, kernel_start) == AgentExit::Gone)
}

/// Whether process `pid`, holding conversation `held` in the foreground of the tab of the restart
/// in flight of conversation `session` by the registry that records its birth as `born`, overtook
/// that restart: the restart is [`overtakable`] and `pid` is another live process, the one that
/// wrote that registry ([`wrote_registry`]), on another conversation, whose launch did not resume
/// `session`. [`STALE_S`] stays the backstop of a restart nothing overtook.
pub(super) fn overtaken(
    st: &St,
    session: &str,
    pid: u32,
    born: &str,
    held: &str,
    alive: impl Fn(u32) -> bool,
) -> bool {
    overtakable(st, &alive)
        && pid != 0
        && pid != st.pid
        && pid != st.relaunched_pid
        && held != session
        && upgrade::is_session_id(held)
        && alive(pid)
        && wrote_registry(pid, born)
        && !launched_to_resume(pid, session)
}

/// Whether live process `pid` is the one that wrote the registry recording its birth as `born`:
/// the kernel reads its start as `born` does. A crashed agent's session file stays while any
/// process holds its number, so a program that took that number in front of the tab is no agent.
fn wrote_registry(pid: u32, born: &str) -> bool {
    kernel_start(pid).as_deref() == Some(squash(born).as_str())
}

/// Whether process `pid`'s launch resumes `session`, or its arguments could not be read.
fn launched_to_resume(pid: u32, session: &str) -> bool {
    atpkg::caller_shell::process_args(pid)
        .is_none_or(|args| upgrade::launch_resume(&args.argv).flatten().as_deref() == Some(session))
}

/// Tab `tab`'s foreground process group: the host roster's (`tabs`), or with no roster the group
/// in front of `shell`'s terminal when it is not the shell's own.
pub(super) fn tab_front(
    tabs: Option<&[super::upgrade_drive::LiveTab]>,
    tab: &str,
    shell: u32,
) -> Option<i64> {
    match tabs {
        Some(tabs) => tabs.iter().find(|t| t.sid == tab).and_then(|t| t.fgpgid),
        None if shell == NO_SHELL => None,
        None => {
            ids(shell).and_then(|(_, group, front)| (front > 0 && front != group).then_some(front))
        }
    }
}

/// The process in `files` that leads `front`, its tab's foreground group, and overtook the restart
/// in flight `st` of conversation `session` ([`overtaken`]).
pub(super) fn overtaken_by<'a>(
    st: &St,
    session: &str,
    files: &'a [SessionFile],
    front: Option<i64>,
) -> Option<&'a SessionFile> {
    let front = front.filter(|&group| group > 0)?;
    files
        .iter()
        .filter(|sf| i64::from(sf.pid) == front && sf.kind == "interactive")
        .find(|sf| overtaken(st, session, sf.pid, &sf.proc_start, &sf.session_id, alive))
}

/// The ledger line of a restart [`overtaken`] by process `pid` on conversation `held`.
pub(super) fn overtaken_line(pid: u32, held: &str) -> String {
    format!(
        "claude (pid {pid}) holds conversation {held} in its tab since the agent's end: nothing \
         typed"
    )
}

/// Close the restart in flight `st` of conversation `session` as [`TAB_TAKEN`] when the agent in
/// front of its tab `tab` overtook it ([`overtaken_by`]), the tab's group read from its host's
/// roster on `c` (or else from its shell's terminal) and the agent from the record's own registry.
fn close_if_taken(
    opts: &Opts,
    r: &Report,
    st: &mut St,
    session: &str,
    c: &mut Client,
    tab: &str,
) -> Option<Report> {
    let retired = match super::native_restart::refresh_retirement_hold(c, tab, st) {
        Ok(retired) => retired,
        Err(why) => return Some(said(r.clone(), format!("wait:{why}"))),
    };
    if retired {
        save(opts, session, st);
        return Some(said(
            r.clone(),
            super::native_restart::WAIT_RETIRED_UNISSUED,
        ));
    }
    if !overtakable(st, alive) {
        return None;
    }
    let files = st.agent_records(&opts.home).session_files()?;
    let tabs = host_roster(c);
    let front = tab_front(tabs.as_deref(), tab, st.shell);
    let sf = overtaken_by(st, session, &files, front)?;
    if let Some(wait) = native_retirement_wait(opts, r, st, session, c) {
        return Some(wait);
    }
    let line = overtaken_line(sf.pid, &sf.session_id);
    st.close_overtaken(TAB_TAKEN, now_s());
    let r = said(r.clone(), format!("failed:{TAB_TAKEN}"));
    ledger(opts, &r, &line);
    save(opts, session, st);
    Some(r)
}

pub(super) fn native_retirement_wait(
    opts: &Opts,
    r: &Report,
    st: &mut St,
    session: &str,
    c: &mut Client,
) -> Option<Report> {
    if st.native_retired_unissued {
        return Some(said(
            r.clone(),
            super::native_restart::WAIT_RETIRED_UNISSUED,
        ));
    }
    if st.native.is_none() && st.native_prepare.is_empty() {
        return None;
    }
    let tab = st.tab.clone();
    let retired = super::native_restart::retire(opts, session, c, &tab, st);
    save(opts, session, st);
    (!retired).then(|| {
        said(
            r.clone(),
            if st.native_retired_unissued {
                super::native_restart::WAIT_RETIRED_UNISSUED
            } else {
                "wait:native-retirement"
            },
        )
    })
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
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = relaunch_held(opts, r, st, c, &mut hand, session, k);
    if st.native_retired_unissued || !matches!(st.phase, upgrade::Phase::Exiting { .. }) {
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
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    let has_private_constructor = match st.agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex | upgrade::Agent::Agy | upgrade::Agent::Muse => false,
    };
    if !st.host_ended_fresh
        && (super::native_restart::coordinated(st)
            || opts.token.is_some() && has_private_constructor)
        && st.native.is_none()
        && st.native_prepare.is_empty()
    {
        // An old or already completed native act is never an untyped launch grant. A new
        // crash needs its own retained live/death observation and a newly admitted plan.
        match super::native_restart::begin_completed_rollback(opts, c, st, session) {
            Ok(super::native_restart::Recovery::Admitted) => {}
            Ok(super::native_restart::Recovery::Completed) => {
                return await_held(opts, r, st, c, hand, session, k);
            }
            Ok(super::native_restart::Recovery::Retired) => {
                return said(r, "failed:native-cohort-retired");
            }
            Ok(super::native_restart::Recovery::Held) => {
                return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
            }
            Ok(super::native_restart::Recovery::Legacy) => {}
            Ok(super::native_restart::Recovery::Cancelled) => {
                return said(r, "wait:native-crash-observation");
            }
            Err(why) => return said(r, format!("wait:{why}")),
        }
    }
    let old = st.pid;
    if !st.native_prepare.is_empty() {
        let tab = st.tab.clone();
        match super::native_restart::recover(opts, session, c, &tab, st) {
            Ok(super::native_restart::Recovery::Completed) => {
                save(opts, session, st);
                return await_held(opts, r, st, c, hand, session, k);
            }
            Ok(super::native_restart::Recovery::Cancelled) => {
                st.stop("native-cancelled-before-mutation", now_s());
                save(opts, session, st);
                return said(r, "wait:native-cancelled-before-mutation");
            }
            Ok(super::native_restart::Recovery::Retired) => {
                save(opts, session, st);
                return said(r, "failed:native-cohort-retired");
            }
            Ok(super::native_restart::Recovery::Held) => {
                save(opts, session, st);
                return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
            }
            Ok(
                super::native_restart::Recovery::Admitted | super::native_restart::Recovery::Legacy,
            ) => {}
            Err(_) => return said(r, "wait:native-recovery"),
        }
        save(opts, session, st);
    }
    let tab = st.tab.clone();
    let resumed = match super::native_restart::resume_observed(c, &tab, st) {
        Ok(resumed) => resumed,
        Err(why) => return said(r, format!("wait:{why}")),
    };
    if resumed {
        // The client may have ended after the server submitted the fixed line but before its
        // local typed record landed. Kernel proof of that same operation resumes observation;
        // the old line is never sent again while waiting for the shell to return.
        st.relaunched(now_s());
        save(opts, session, st);
        return await_held(opts, r, st, c, hand, session, k);
    }
    if st.native_retired_unissued {
        save(opts, session, st);
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    if !st.native_prepare.is_empty() && st.agent_exit() != AgentExit::Gone {
        let tab = st.tab.clone();
        // A prepare/signal reply may have been lost before mutation. Prove an explicit
        // pre-mutation abort before returning to a fresh at-rest constructor; never signal
        // later from this recovery/observation lane using an old screen observation.
        if super::native_restart::abort(opts, session, c, &tab, st) {
            st.stop("native-aborted-before-signal", now_s());
            save(opts, session, st);
            return said(r, "wait:native-aborted-before-signal");
        }
        if st.native_retired_unissued {
            save(opts, session, st);
            return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
        }
    }
    // The relaunch waits only on the process the record names by pid and kernel start
    // ([`St::agent_exit`]), never on a pid alone, which may name another process since (a reboot
    // reuses them); a record that already saw its agent gone ([`St::exited_at`]) waits on none.
    if st.exited_at == 0 {
        match st.agent_exit() {
            AgentExit::Gone => {}
            AgentExit::Runs => {
                if !super::upgrade_wake::wait_exit_as(old, &st.pid_start, opts.timings.exited) {
                    return said(r, "wait:exiting");
                }
            }
            AgentExit::Unproven => return said(r, WAIT_EXIT_UNPROVEN),
        }
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
    let dialect = shell_dialect(shell, &table());
    let line = match owed_line_at_shell(&st.line, &st.line_dir, cwd_of(shell).as_deref(), dialect) {
        Ok(line) => line,
        Err(NoPlan::Wait(why @ "shell-moved")) => {
            st.note_wait(why, now_s());
            return said(r, format!("wait:{why}"));
        }
        Err(no) => return unplanned(opts, r, st, no),
    };
    if st.native.is_some() && line != st.line {
        return said(r, "wait:native-continuation-changed");
    }
    let native = st.native.clone();
    if native.is_none() && !st.host_ended_fresh && super::native_restart::coordinated(st) {
        return said(r, "wait:native-recovery");
    }
    let native_phase = super::native_restart::phase(st);
    let deferred_cold = native.is_some()
        && matches!(super::native_act::Request::parse(&st.native_prepare),
            Some(super::native_act::Request::Prepare(prepare))
                if matches!(prepare.plan, super::native_act::NativePlan::Restart { cold: Some(_), .. }));
    // Observe already dispatched work above before consulting current policy. This
    // admission grants only the saved command; it never chooses a replacement login.
    // A deferred native recipe has no selected account until the exact helper resolves it.
    // Its Execute takes canonical account admission after that point; legacy recipes cannot
    // enter this branch and keep their existing dispatch guard below.
    let account_guard = if checks_account_execution(&st.agent) && !deferred_cold {
        match account_execution(opts, st.account_intent.as_ref()) {
            Ok(guard) => guard,
            Err(_) => return said(r, WAIT_ACCOUNT),
        }
    } else {
        None
    };
    // The native helper takes the same writer at Execute. Keeping the client lock
    // over Resume would deadlock it; the raw legacy write retains it through dispatch.
    let legacy_account_guard = if native.is_some() {
        drop(account_guard);
        None
    } else {
        if checks_account_execution(&st.agent) {
            match external_at_shell(opts, st.account_intent.as_ref(), shell) {
                Ok(guard) => guard.or(account_guard),
                Err(_) => return said(r, WAIT_ACCOUNT),
            }
        } else {
            account_guard
        }
    };
    let asked = record_asked(opts, session, &st.model_list);
    let block = line_block(c, &tab);
    let typed = type_relaunch_line_admitted(
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
        native.as_ref().map(|binding| (binding, native_phase)),
    );
    drop(legacy_account_guard);
    if typed
        .as_ref()
        .is_err_and(|e| *e != RelaunchLineError::Unanswered)
    {
        take_back_ask(opts, session, asked);
    }
    match typed {
        Ok(()) => {
            st.prompt = None;
            st.line = line;
        }
        Err(RelaunchLineError::Unanswered) => {
            return line_unanswered(opts, r, st, session, block, line);
        }
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
            ledger(opts, &r, &line);
            return r;
        }
    }
    typed_recorded(opts, st, session, block, |st| {
        ledger(opts, &said(r.clone(), "relaunched"), &st.line);
    });
    await_held(opts, r, st, c, hand, session, k)
}

/// A relaunch line whose request went unanswered ([`RelaunchLineError::Unanswered`]) may have
/// run: it is recorded as typed (its ask kept), and the next step, on a new connection, reads the
/// agent it started or the shell's prompt back ([`await_held`]).
pub(super) fn line_unanswered(
    opts: &Opts,
    r: Report,
    st: &mut St,
    session: &str,
    block: Option<u64>,
    line: String,
) -> Report {
    st.prompt = None;
    st.line = line;
    typed_recorded(opts, st, session, block, |st| {
        ledger(opts, &said(r.clone(), "relaunched"), &st.line);
    });
    said(r, "wait:unanswered")
}

pub(super) fn typed_recorded(
    opts: &Opts,
    st: &mut St,
    session: &str,
    block: Option<u64>,
    after: impl FnOnce(&St),
) {
    st.relaunched(now_s());
    st.line_block = block;
    st.relaunched_pid = 0;
    st.found_gone = false;
    save(opts, session, st);
    after(st);
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RelaunchLineError {
    Wait(&'static str),
    /// A hand the line waits on stands on the tab ([`hand_on_tab`]: a hold, another driver's lease, a turn).
    Hand,
    Turn(String),
    /// The line's request went unanswered within its wait ([`Typed::Unanswered`]): it may have run.
    Unanswered,
}

/// Type the relaunch line. Only `upgrade_drive`'s tests call it, and they run on unix.
#[cfg(all(test, unix))]
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
    type_relaunch_line_admitted(
        opts, records, c, shell, tab, session, line, dialect, look, tab_probe, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn type_relaunch_line_admitted(
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
    native: Option<(&super::native_act::Binding, super::native_act::ImagePhase)>,
) -> Result<(), RelaunchLineError> {
    type_relaunch_line_admitted_with(
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
        native,
    )
}

/// What one relaunch attempt judges the returned prompt by.
pub(super) struct PromptLook<'a> {
    pub(super) mark: &'a mut Option<PromptMark>,
    pub(super) ended_at_s: u64,
    /// Where a hand held the line ([`hand_held_since`]).
    pub(super) held_since_s: u64,
    /// The grace the record gives a person's keys over the line ([`St::line_grace_s`],
    /// [`line_grace`]).
    pub(super) line_grace_s: Option<u32>,
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
            line_grace_s: st.line_grace_s,
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
    tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<(), RelaunchLineError> {
    type_relaunch_line_admitted_with(
        opts,
        c,
        shell,
        tab,
        line,
        dialect,
        look,
        owner_clear,
        tab_probe,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn type_relaunch_line_admitted_with(
    opts: &Opts,
    c: &mut Client,
    shell: u32,
    tab: &str,
    line: &str,
    dialect: Option<Dialect>,
    look: PromptLook<'_>,
    owner_clear: impl FnOnce() -> bool,
    mut tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
    native: Option<(&super::native_act::Binding, super::native_act::ImagePhase)>,
) -> Result<(), RelaunchLineError> {
    let PromptLook {
        mark,
        ended_at_s,
        held_since_s,
        line_grace_s,
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
    let checked_status = super::upgrade_drive::status_line(c, tab);
    if let Some(held) = line_held_by_status(
        checked_status.as_deref(),
        line_grace(opts.human_grace_s, ended_at_s, line_grace_s),
        &super::upgrade_drive::hand_name(),
    ) {
        return Err(held);
    }
    let native = match native {
        None => None,
        Some((binding, phase)) => Some((
            binding,
            phase,
            checked_status
                .as_deref()
                .and_then(super::native_restart::human_seq)
                .ok_or(RelaunchLineError::Wait("native-person-unread"))?,
        )),
    };
    if mark.is_none()
        && let Some(scr) = screen(c, tab)
    {
        *mark = first_mark(c, tab, &scr, ended_at_s, held_since_s)?;
    }
    let healed = native
        .is_none()
        .then(|| dialect.and_then(|d| healed_line(c, tab, shell, d, line)))
        .flatten();
    let typed = type_at_prompt(
        c,
        shell,
        tab,
        healed.as_deref().unwrap_or(line),
        mark,
        &mut tab_probe,
        opts.timings.fence_every,
        native,
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

#[allow(clippy::too_many_arguments)]
fn type_at_prompt(
    c: &mut Client,
    shell: u32,
    tab: &str,
    line: &str,
    mark: &mut Option<PromptMark>,
    tab_probe: &mut impl FnMut(&mut Client, u32, &str) -> bool,
    fence_every: Duration,
    native: Option<(
        &super::native_act::Binding,
        super::native_act::ImagePhase,
        u64,
    )>,
) -> Result<(), RelaunchLineError> {
    if let Some((binding, phase, human_seq)) = native {
        let scr = screen(c, tab);
        at_mark(mark, scr.as_ref())?;
        if !tab_probe(c, shell, tab) {
            return Err(RelaunchLineError::Wait("tab-ownership-changed"));
        }
        let generation = scr
            .and_then(|s| s.generation)
            .ok_or(RelaunchLineError::Wait("native-screen-unread"))?;
        return super::native_restart::resume(c, tab, binding, phase, &generation, human_seq)
            .map_err(|error| match error {
                Typed::Unanswered => RelaunchLineError::Unanswered,
                _ => RelaunchLineError::Wait("native-continuation"),
            });
    }
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
            let Some((generation, human)) = scr.and_then(|s| Some((s.generation?, s.human_seq)))
            else {
                break;
            };
            match turn_fenced(c, tab, line, Some(&generation), human) {
                Ok(()) => return Ok(()),
                Err(Typed::Changed) => pause(fence_every),
                Err(Typed::Yielded) => return Err(RelaunchLineError::Wait("yield")),
                Err(Typed::Person) => return Err(RelaunchLineError::Wait("held")),
                Err(Typed::Cut(_)) => return Err(RelaunchLineError::Wait("cut")),
                Err(Typed::Refused(e)) => return Err(refused_line(e)),
                Err(Typed::Unanswered) => return Err(RelaunchLineError::Unanswered),
            }
        }
    }
    let last = screen(c, tab);
    at_mark(mark, last.as_ref())?;
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    let typed = if bash {
        type_line(c, tab, line)
    } else {
        turn_fenced(c, tab, line, None, last.and_then(|s| s.human_seq))
    };
    match typed {
        Ok(()) => Ok(()),
        Err(Typed::Yielded | Typed::Changed) => Err(RelaunchLineError::Wait("yield")),
        Err(Typed::Person) => Err(RelaunchLineError::Wait("held")),
        Err(Typed::Cut(_)) => Err(RelaunchLineError::Wait("cut")),
        Err(Typed::Refused(e)) => Err(refused_line(e)),
        Err(Typed::Unanswered) => Err(RelaunchLineError::Unanswered),
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
            row: crate::parse_uint(parts.next()?)?,
            col: crate::parse_uint(parts.next()?)?,
            right: match parts.next()? {
                "-" => None,
                r => Some(crate::parse_uint(r)?),
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
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
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
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    let tab = st.tab.clone();
    let unissued = if checks_account_execution(&st.agent) {
        match super::native_restart::launch_unissued(c, &tab, st) {
            Ok(unissued) => unissued,
            Err(why) => return said(r, format!("wait:{why}")),
        }
    } else {
        false
    };
    if unissued {
        retain_unissued(st);
        save(opts, session, st);
        return said(r, WAIT_NATIVE_UNISSUED);
    }
    if st.native_retired_unissued {
        save(opts, session, st);
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    let records = st.agent_records(&opts.home);
    let (old, shell) = (st.pid, st.shell);
    let fresh = starts_afresh(&st.cause);
    let mut new: Option<SessionFile> = None;
    let mut roster = SessionRosterCache::default();
    let mut back = ShellBack::after(st.line_block, &opts.timings);
    let mut found_gone = std::mem::take(&mut st.found_gone);
    let mut ended = found_gone;
    let mut unread = false;
    let look_back = ends_at_once(&st.cause);
    let tab = st.tab.clone();
    let limit = held_wait(&opts.timings, &st.cause);
    wait_every(limit, poll_gap(limit).min(opts.timings.shell_look), || {
        hand.keep(c);
        let resumed = match super::native_restart::resume_observed(c, &tab, st) {
            Ok(resumed) => resumed,
            Err(_) => {
                unread = true;
                return true;
            }
        };
        if resumed {
            let _completed = super::native_restart::complete(opts, session, c, &tab, st);
            if st.native_retired_unissued {
                st.found_gone = found_gone;
            }
            save(opts, session, st);
            if st.native_retired_unissued {
                return true;
            }
        }
        if st.native_retired_unissued {
            st.found_gone = found_gone;
            save(opts, session, st);
            return true;
        }
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
        if !ended && look_back && new.is_none() && back.due(now) {
            let over = back.block.and_then(|block| line_over(c, &tab, block));
            ended = back.saw(k.shell_holds_tab(shell), over, now);
        }
        new.is_some() || ended
    });
    if unread {
        st.found_gone = found_gone;
        save(opts, session, st);
        return said(r, "wait:native-recovery");
    }
    if st.native_retired_unissued {
        st.found_gone = found_gone;
        save(opts, session, st);
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    if let Some(sf) = &new {
        first_idle(c, hand, &st.tab, opts.timings.first_idle, || {
            records
                .session_file(sf.pid)
                .is_some_and(|sf| sf.status == "idle")
        });
    }
    // A process found on the conversation and gone by the end of this wait is read as one that
    // never came up, unless an earlier step adopted it, whose end is its own. After a restart that
    // ended its own agent, adopting it would let the carry of the old agent's exit close the move
    // as one that landed, with nothing said. The on-exit lanes and the restored lane retry an
    // adopted process that is gone. One limit: a process up but not adopted yet, its step having
    // answered a wait, and quit during this wait reads the same.
    if new.as_ref().is_some_and(|sf| {
        ends_its_agent(&st.cause) && st.relaunched_pid != sf.pid && !alive(sf.pid)
    }) {
        new = None;
        ended = true;
        found_gone = true;
    }
    let how_ended = if found_gone {
        REGISTERED_AND_ENDED
    } else {
        "the relaunched agent ended without registering the conversation: its shell has the tab \
         back"
    };
    // The helper can be refused and exit during the observation wait, not just
    // before it. Preserve that positively unissued attempt before ordinary
    // no-resume retirement or rollback can consume the owed conversation.
    let unissued = if new.is_none() && checks_account_execution(&st.agent) {
        match super::native_restart::launch_unissued(c, &tab, st) {
            Ok(unissued) => unissued,
            Err(why) => {
                st.found_gone = found_gone;
                save(opts, session, st);
                return said(r, format!("wait:{why}"));
            }
        }
    } else {
        false
    };
    if unissued {
        retain_unissued(st);
        save(opts, session, st);
        return said(r, WAIT_NATIVE_UNISSUED);
    }
    if st.native_retired_unissued {
        st.found_gone = found_gone;
        save(opts, session, st);
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    if new.is_some() && !super::native_restart::complete(opts, session, c, &st.tab.clone(), st) {
        if st.native_retired_unissued {
            st.found_gone = found_gone;
        }
        save(opts, session, st);
        return said(
            r,
            if st.native_retired_unissued {
                super::native_restart::WAIT_RETIRED_UNISSUED
            } else {
                "wait:native-outcome"
            },
        );
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
            let tab = st.tab.clone();
            let none_owed = if resumes_quietly(st) {
                None
            } else {
                let ours = match conversation_typed_by(opts, &tab, session, st) {
                    Ok(ours) => ours,
                    Err(_) => return said(r, WAIT_LEDGER_EVIDENCE),
                };
                let written = WhereWritten::new(opts, &records, session, &ours);
                match nothing_owed(opts, &written, &tab, st) {
                    Ok(nothing) => nothing.map(|(_, why)| why),
                    Err(_) => return said(r, WAIT_LEDGER_EVIDENCE),
                }
            };
            st.relaunched_pid = sf.pid;
            st.relaunched_start = squash(&sf.proc_start);
            let promise = Adopted::of(st, || none_owed);
            ledger(opts, &r, &promise.detail());
            save(opts, session, st);
            r
        }
        Some(sf) => carry_on(opts, r, st, c, session, &sf, Some(hand)),
        None if ended && st.roll_back(now_s(), opts.human_grace_s) => {
            let failed = said(r.clone(), "failed:no-resume");
            ledger(
                opts,
                &failed,
                &format!(
                    "{how_ended}; resumed on {}, the build it ran",
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
            if st.native.is_some() && !super::native_restart::retire(opts, session, c, &tab, st) {
                st.found_gone = found_gone;
                save(opts, session, st);
                if st.native_retired_unissued {
                    return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
                }
                return said(r, "wait:native-retirement");
            }
            st.stop("no-resume", now_s());
            let r = said(r, "failed:no-resume");
            ledger(opts, &r, how_ended);
            r
        }
        None => {
            if let upgrade::Phase::Relaunched { at_s } = st.phase
                && now_s().saturating_sub(at_s) > STALE_S
            {
                st.found_gone = found_gone;
                if let Some(wait) = native_retirement_wait(opts, &r, st, session, c) {
                    return wait;
                }
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

fn retain_unissued(st: &mut St) {
    let at_s = if st.exited_at != 0 {
        st.exited_at
    } else {
        match &st.phase {
            upgrade::Phase::Relaunched { at_s } | upgrade::Phase::Exiting { at_s } => *at_s,
            _ => now_s(),
        }
    };
    st.exited_at = at_s;
    st.phase = upgrade::Phase::Exiting { at_s };
}

/// What an `adopted` row promises of the relaunch's last step, which the tab's loop takes at its
/// first idle point ([`resume`]), ranked as [`carry_on_once`] ranks it there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Adopted {
    /// A quiet restart ([`resumes_quietly`]), resumed with nothing typed (`done:resumed`).
    Quiet,
    /// The conversation's own records owe nothing, for the why [`nothing_owed`] names.
    NothingOwed(&'static str),
    /// Any other move owes the continuation, under Cruise and under Land alike, which the tab's
    /// supervisor types at the agent's first idle point.
    Owed,
}

impl Adopted {
    fn of(st: &St, none_owed: impl FnOnce() -> Option<&'static str>) -> Self {
        if resumes_quietly(st) {
            Self::Quiet
        } else if let Some(why) = none_owed() {
            Self::NothingOwed(why)
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
            Self::Owed => "the relaunched agent holds the conversation; its supervisor types \
                           the continuation at its first idle point"
                .to_string(),
        }
    }
}

/// Whether the session's Autopilot is Land (the host's [`Opts::land`], or tab `tab`'s status): a
/// relaunch is carried on there too, with the restart's facts and no done check of Cruise's, the
/// loop's own Land acts deciding every turn end after it.
pub(super) fn carries_under_land(opts: &Opts, c: &mut Client, tab: &str) -> bool {
    opts.land || tab_under_land(c, tab)
}

/// Whether tab `tab`'s own status says its Autopilot is off: nothing is typed there, and a relaunch
/// stays owed in case the person turns it on again (`wait:off`).
pub(super) fn tab_off(c: &mut Client, tab: &str) -> bool {
    super::upgrade_drive::status_line(c, tab)
        .is_some_and(|line| autopilot_by_status(&line) == Some("off"))
}

/// The unix second from which a prompt in the relaunched agent's own log reached it first: the
/// relaunch line's ([`reached_first`]).
pub(super) fn prompts_from(st: &St) -> u64 {
    relaunched_line_at(st)
}

/// Whether `prompt`, as a relaunched agent's own log wrote it, is one of the harness's own lines.
pub(super) fn harness_prompt(prompt: &str) -> bool {
    prompt
        .trim_start()
        .starts_with(crate::harness::upgrade::HARNESS_MARK)
}

/// A relaunch whose agent's own log shows `prompts` written since its line, settled on `new` with
/// nothing typed, or `None` where nobody has spoken to it and the carry-on still goes: a person's
/// own message wins ([`PERSON_FIRST`]), and a line of the harness's that landed (the loop's act past
/// the hold, or the carry-on's own, late answered) has already told it to go on ([`HARNESS_FIRST`]).
/// The reading of a lane with no transcript of Claude Code's to read ([`nothing_owed`]): a key alone
/// is no message, so a click, a scroll or a cleared draft ends nothing here.
pub(super) fn reached_first(
    opts: &Opts,
    r: &Report,
    st: &mut St,
    new: u32,
    prompts: &[String],
) -> Option<Report> {
    let (word, why) = if prompts.iter().any(|p| !harness_prompt(p)) {
        ("done:left", PERSON_FIRST)
    } else if prompts.is_empty() {
        return None;
    } else {
        ("done:carried", HARNESS_FIRST)
    };
    st.resumed_pid = new;
    st.resumed_on.clone_from(&st.to);
    Some(settled_with_nothing(opts, r.clone(), st, word, why))
}

/// A relaunch settled with nothing typed, on `word` for the reason `why`; the caller saves `st`.
pub(super) fn settled_with_nothing(
    opts: &Opts,
    r: Report,
    st: &mut St,
    word: &str,
    why: &str,
) -> Report {
    st.phase = upgrade::Phase::Done;
    st.done_at = now_s();
    st.outcome = nothing_typed(st.agent, &st.to, &format!("nothing typed: {why}"));
    let r = said(r, word);
    ledger(opts, &r, &st.outcome);
    r
}

/// The word of a relaunch under Land whose agent's last reply defends the work or says it could not
/// finish: nothing is typed, and the tab's loop reads that reply at this point ([`DEFENDED`]).
pub const LANDING: &str = "done:landing";

/// Why [`LANDING`] types nothing.
pub const DEFENDED: &str = "its last reply defends its work as landed or says it could not finish, and the tab's loop \
     reads it";

/// Whether the last reply on the screen `rows` draws for `program` ends the question of more typing
/// ([`crate::supervise::policy::turn_end::reply_for_the_loop`]), the reading a relaunch under Land
/// with no transcript of Claude Code's has.
pub(super) fn defended_on_screen(program: aterm_phase::Program, rows: &[String]) -> bool {
    crate::supervise::policy::turn_end::reply_for_the_loop(
        program.reader().said_tail(rows).as_deref(),
    )
}

/// [`LANDING`] for a lane that reads the screen ([`defended_on_screen`]); the caller saves `st`.
pub(super) fn landing(opts: &Opts, r: Report, st: &mut St, new: u32) -> Report {
    st.resumed_pid = new;
    st.resumed_on.clone_from(&st.to);
    settled_with_nothing(opts, r, st, LANDING, DEFENDED)
}

/// How many looks in a row at a draft or a box in the composer end a relaunch's wait for its
/// composer: a person's draft stands for as long as they keep it, and a look later is all the
/// carry-on needs ([`settled_for_carry`]).
pub(super) const DRAFT_LOOKS: u32 = 3;

/// Whether the screen settled for a carry-on within `within` by the lane's own `fence`, keeping the
/// harness's hand with `keep`: `Err` with the word of what stands in the composer, which keeps the
/// carry-on owed and looks again; never a reason to give it up, as nothing may be typed over unsent
/// input.
pub(super) fn settled_for_carry(
    within: Duration,
    c: &mut Client,
    mut fence: impl FnMut(&mut Client) -> Result<Option<String>, &'static str>,
    mut keep: impl FnMut(&mut Client),
) -> Result<bool, &'static str> {
    let (mut stood, mut word) = (0, "wait:draft");
    let settled = wait_until(within, || {
        keep(c);
        if c.reply_unread() {
            return true;
        }
        match fence(c) {
            Ok(_) => {
                stood = 0;
                true
            }
            Err(held @ ("draft" | "box")) => {
                word = if held == "box" {
                    "wait:box"
                } else {
                    "wait:draft"
                };
                stood += 1;
                stood >= DRAFT_LOOKS
            }
            Err(_) => {
                stood = 0;
                false
            }
        }
    });
    if stood >= DRAFT_LOOKS {
        return Err(word);
    }
    Ok(settled && !c.reply_unread())
}

fn nothing_typed(agent: upgrade::Agent, version: &str, why: &str) -> String {
    let version = one_line(version);
    match agent {
        upgrade::Agent::Claude => format!("claude restarted on {version} · {why}"),
        upgrade::Agent::Codex => {
            format!("codex on {version} · the same conversation resumed; {why}")
        }
        upgrade::Agent::Muse => format!("muse on {version} · the same conversation resumed; {why}"),
        upgrade::Agent::Agy => format!("agy on {version} · the same conversation resumed; {why}"),
    }
}

/// What the last reply `program` draws on `rows` claims ([`upgrade::done_said`]): a resumed agent
/// whose transcript is its screen (Muse Code, agy) is read there.
pub(super) fn said_on_screen(program: aterm_phase::Program, rows: &[String]) -> (bool, bool) {
    program
        .reader()
        .said_tail(rows)
        .map_or((false, false), |words| upgrade::done_said(&words))
}

/// The relaunch's prompt `text`, its carry-on swapped for Cruise's done check where the agent's
/// last words claim the work done (`said`, [`upgrade::checking`]), the check's sentence that the
/// go-ahead is no need cut where the person's hour `held` it ([`go_ahead_held_by_status`]).
pub(super) fn carried_text(
    text: String,
    (said_done, irreversible): (bool, bool),
    held: bool,
) -> String {
    if said_done {
        upgrade::checking(&text, irreversible, held)
    } else {
        text
    }
}

/// Whether one `status` reply says the person's last key holds the go-ahead now, as the loop's
/// hour does ([`crate::supervise::policy::turn_end::GO_AHEAD_HELD`]): their typing
/// (`human_typed_ms=`, else `human_ms=` from a window that reports no typing apart) within the
/// hour. A reply with no readable key holds it, so a check typed after a restart says no go-ahead
/// word where the hour cannot be read; `-` (no typing since the host began serving the tab) holds
/// nothing. The loop's hour also runs from the end of the turn the key came in, which one reply
/// does not show.
#[must_use]
pub fn go_ahead_held_by_status(status: &str) -> bool {
    let Some(line) = status.lines().find(|l| crate::is_ok_reply(l)) else {
        return true;
    };
    let field = |name: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(name)?.strip_prefix('='))
    };
    match field("human_typed_ms").or_else(|| field("human_ms")) {
        Some("-") => false,
        Some(ms) => crate::parse_uint::<u64>(ms).is_none_or(|ms| {
            Duration::from_millis(ms) < crate::supervise::policy::turn_end::GO_AHEAD_HELD
        }),
        None => true,
    }
}

/// The word of a carry-on typed: the done check's where the agent's last words claimed the work done.
pub(super) const fn continued_word(said_done: bool) -> &'static str {
    if said_done {
        "continued:check"
    } else {
        "continued"
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

/// Wait at most `within` for the relaunched agent's first rest, renewing the harness's `hand`
/// first: its idle, or a screen no idle will come to until someone answers it. A resume onto a
/// last reply that asked something reads as a `question`; an opening box (`prompt`) and a `wall`
/// are the supervisor's to answer once the step returns, so waiting on past them only holds the
/// tab for `within`.
pub(super) fn first_idle(
    c: &mut Client,
    hand: &mut Hand,
    tab: &str,
    within: Duration,
    record_idle: impl FnMut() -> bool,
) {
    hand.keep(c);
    let awaited = c.request_line(&format!(
        "@{tab} await agent idle,question,prompt,wall timeout={}",
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
    let mut draft_looks = 0;
    let settled = wait_until(opts.timings.carry_settle, || {
        // This wait (up to 120 s) outlasts the hand's 60 s lease: renew it throughout.
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        // A reply left unread ends the wait: the connection asks nothing more.
        if c.reply_unread() {
            return true;
        }
        let Some(sf) = records.session_file(new.pid) else {
            return false;
        };
        if sf.session_id != key {
            conversation_changed = true;
            return true;
        }
        if sf.status != "idle" {
            draft_looks = 0;
            return false;
        }
        if continuation_composer_empty(c, tab, &mut tail_available) {
            draft_looks = 0;
            return true;
        }
        draft_looks += 1;
        draft_looks >= DRAFT_LOOKS
    }) && !c.reply_unread()
        && draft_looks < DRAFT_LOOKS;
    if conversation_changed {
        return said(r, "wait:conversation-changed");
    }
    let Some(current) = continuation_session(&records, new.pid, key) else {
        return said(r, "wait:conversation-changed");
    };
    if resumes_quietly(st) {
        return done_quietly(opts, r, st, key, &current.version, new.pid);
    }
    // NOTHING IS TYPED OVER UNSENT INPUT, AND THE CARRY-ON IS NOT GIVEN UP FOR IT: a person's draft
    // keeps it owed for a later look, as an agent still busy past the wait does.
    if draft_looks >= DRAFT_LOOKS {
        return said(r, "wait:draft");
    }
    if !settled && !c.reply_unread() {
        return said(r, "wait:unsettled");
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
    let ours = match conversation_typed_by(opts, tab, key, st) {
        Ok(ours) => ours,
        Err(_) => return said(r, WAIT_LEDGER_EVIDENCE),
    };
    let written = WhereWritten::new(opts, &records, key, &ours);
    if settled {
        match nothing_owed(opts, &written, tab, st) {
            Ok(Some((word, why))) => {
                return done_with_nothing(opts, r, st, key, word, why, &latest.version, new.pid);
            }
            Ok(None) => {}
            Err(_) => return said(r, WAIT_LEDGER_EVIDENCE),
        }
    }
    // UNDER LAND THE RESTART'S FACTS ARE TYPED AS UNDER CRUISE, without Cruise's done check: a task
    // Land finished stopped at `nothing_owed` above, a reply that defends its work stops here for
    // the loop's land check at this point, and every later turn end is its loop's.
    let under_land = settled && carries_under_land(opts, c, tab);
    if under_land && written.defended() == Some(true) {
        return done_with_nothing(
            opts,
            r,
            st,
            key,
            LANDING,
            DEFENDED,
            &latest.version,
            new.pid,
        );
    }
    let claim = if settled && !under_land {
        written.said_done().unwrap_or_default()
    } else {
        (false, false)
    };
    let said_done = claim.0;
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
                if tab_off(c, tab) {
                    return said(r, "wait:off");
                }
                let Some(hour) = super::upgrade_drive::unheld_hour(c, tab, opts.human_grace_s)
                else {
                    return said(r, "wait:held");
                };
                let text = carried_text(text, claim, hour);
                let Some(typed) = under_hand(c, tab, hand, |c| {
                    type_continuation(c, tab, &text, opts.timings.fence_every)
                }) else {
                    return said(r, "wait:held");
                };
                match typed {
                    Continued::Typed => true,
                    Continued::Not => false,
                    Continued::Stands(word) => return said(r, word),
                    Continued::Unanswered
                        if took_it(&records, &latest, opts.timings.first_idle) =>
                    {
                        true
                    }
                    Continued::Unanswered => return said(r, "wait:unanswered"),
                }
            }
            None => false,
        };
    // A carry that typed nothing because the socket left a reply unread is asked again at the
    // next step, on a new connection: the silence is never the carry's end.
    if !typed && c.reply_unread() {
        return said(r, "wait:no-socket");
    }
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
    let r = said(r, continued_word(said_done));
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
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
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

/// An external continuation remains owed while its books cannot distinguish standing holds,
/// already typed acts and the person's own turns from an empty ledger.
pub const WAIT_LEDGER_EVIDENCE: &str = "wait:ledger-evidence-unreadable";

/// Why a relaunch a person's own message reached first is carried on with nothing: the message
/// wins, and the turn end after it is the loop's to decide.
pub const PERSON_FIRST: &str = "a person's own message reached it first";

/// Why a relaunch a line of the harness's reached first is carried on with nothing more
/// ([`reached_first`]).
pub const HARNESS_FIRST: &str = "a line of the harness's reached it first";

fn nothing_owed(
    opts: &Opts,
    written: &WhereWritten<'_>,
    tab: &str,
    st: &St,
) -> std::io::Result<Option<(&'static str, &'static str)>> {
    let (records, key, ours) = (written.records(), written.session(), written.ours());
    if records.tasked(key, ours) == Some(false) {
        return Ok(Some(("done:taskless", NOBODY_ASKED)));
    }
    if written.finished() == Some(true) {
        return Ok(Some(("done:finished", RECORD_LANDED)));
    }
    if left_to_person(opts, records, tab, key, st)? {
        return Ok(Some((
            "done:left",
            "its last turn was a person's own exchange",
        )));
    }
    if written.asked_since(relaunched_line_at(st)) == Some(true) {
        return Ok(Some(("done:left", PERSON_FIRST)));
    }
    if loop_typed_since(opts, tab, relaunched_line_at(st))? {
        return Ok(Some(("done:carried", "the tab's loop resumed it first")));
    }
    Ok(None)
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
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
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

fn loop_typed_since(opts: &Opts, tab: &str, since: u64) -> std::io::Result<bool> {
    let Some(root) = opts.aterm_state.as_deref() else {
        return Ok(false);
    };
    let Ok(since) = i64::try_from(since) else {
        return Ok(false);
    };
    Ok(since != 0
        && approvals::read_typed_texts(&approvals::ledger_under(root, Some(tab)), Some(tab))?
            .iter()
            .any(|t| {
                t.at_ms.div_euclid(1000) > since
                    && !t.text.starts_with("relaunch ")
                    && t.text != super::usage::USAGE_COMMAND
            }))
}

/// What [`type_continuation`] did, or a lane's [`type_carry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Continued {
    Typed,
    Not,
    /// A person's draft or a box stands in the composer, or the screen moved under every try, by
    /// this word: nothing is typed over it, and the carry-on stays owed.
    Stands(&'static str),
    /// Its turn went unanswered within its wait ([`Typed::Unanswered`]): it may have landed.
    Unanswered,
}

fn type_continuation(c: &mut Client, tab: &str, text: &str, fence_every: Duration) -> Continued {
    for _ in 0..CONTINUE_FENCE_TRIES {
        let (generation, human) = match typing_fence(c, tab) {
            Ok(fenced) => fenced,
            Err("changed") => {
                pause(fence_every);
                continue;
            }
            Err("draft") => return Continued::Stands("wait:draft"),
            Err("box") => return Continued::Stands("wait:box"),
            Err(_) => return Continued::Not,
        };
        match turn_fenced(c, tab, text, generation.as_deref(), human) {
            Ok(()) => return Continued::Typed,
            Err(Typed::Changed | Typed::Yielded) => pause(fence_every),
            // The person's gesture since the read: their grace is the carry's next step's to wait
            // out, never a retry's a moment on.
            Err(Typed::Person) => return Continued::Stands("wait:held"),
            Err(Typed::Cut(_) | Typed::Refused(_)) => return Continued::Not,
            Err(Typed::Unanswered) => return Continued::Unanswered,
        }
    }
    let Some(clear) = composer_clear(c, tab) else {
        return Continued::Stands("wait:draft");
    };
    match turn_fenced(c, tab, text, None, clear.human_seq) {
        Ok(()) => Continued::Typed,
        Err(Typed::Person) => Continued::Stands("wait:held"),
        Err(Typed::Unanswered) => Continued::Unanswered,
        Err(_) => Continued::Not,
    }
}

/// [`type_continuation`] for a lane with its own `fence` and `type_text` (Muse Code, agy): what
/// stands at the fence keeps the carry-on owed by its word, as the screen moving under every try
/// does, and a line unanswered is read for what it did before it is typed again. Only a refusal or
/// a cut gives it up, as on Claude Code's lane.
pub(super) fn type_carry(
    c: &mut Client,
    mut fence: impl FnMut(&mut Client) -> Result<Option<String>, &'static str>,
    mut type_text: impl FnMut(&mut Client, Option<&str>) -> Result<(), Typed>,
    fence_every: Duration,
) -> Continued {
    for _ in 0..CONTINUE_FENCE_TRIES {
        let generation = match fence(c) {
            Ok(generation) => generation,
            Err("draft") => return Continued::Stands("wait:draft"),
            Err("box") => return Continued::Stands("wait:box"),
            Err(_) => return Continued::Stands("wait:unsettled"),
        };
        match type_text(c, generation.as_deref()) {
            Ok(()) => return Continued::Typed,
            Err(Typed::Changed | Typed::Yielded) => pause(fence_every),
            Err(Typed::Person) => return Continued::Stands("wait:held"),
            Err(Typed::Cut(_) | Typed::Refused(_)) => return Continued::Not,
            Err(Typed::Unanswered) => return Continued::Unanswered,
        }
    }
    Continued::Stands("wait:unsettled")
}

/// Whether the agent `before` was read as, within `within`, shows it took a continuation whose
/// turn went unanswered: its own record leaves idle, or says its status moved since.
fn took_it(records: &Records, before: &SessionFile, within: Duration) -> bool {
    wait_until(within, || {
        records.session_file(before.pid).is_some_and(|sf| {
            sf.status != "idle" || sf.status_updated_at_ms > before.status_updated_at_ms
        })
    })
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
    let agent_alive = st.resumed_pid == 0 || alive(st.resumed_pid);
    if after.is_none() && st.mark != 0 && now < st.confirm_by && agent_alive {
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

fn told_nothing(
    opts: &Opts,
    records: &Records,
    key: &str,
    st: &St,
) -> std::io::Result<Option<&'static str>> {
    let ours = conversation_typed_by(opts, &st.tab, key, st)?;
    if finished_where_written(opts, records, key, &ours) == Some(true) {
        Ok(Some("its worker finished its task"))
    } else if records.tasked(key, &ours) == Some(false) {
        Ok(Some("nobody has asked it anything"))
    } else {
        Ok(None)
    }
}

const RESTORE_LOST: &str =
    "Claude Code reported that something this session ran did not come back after the restart";

/// The one look after a quiet restart or a carry-on that typed nothing, open [`RESTORE_LOOK`]: if
/// the resumed agent warned that something did not come back, a tasked agent is told to restore
/// what it relies on. A look whose resumed agent is gone closes at once.
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
    if !alive(st.resumed_pid) {
        // The look is its resumed agent's, and that agent is gone: a look that warned is lost
        // now, and one that did not closes saying nothing, since `done:resumed` was said when
        // the restart resumed it ([`done_quietly`]). While the look is open its agent ended
        // before the look; once `confirm_by` passed, no idle point came during it to say so.
        if look_warned(&records, key, st) {
            let why = if now < st.confirm_by {
                "its agent ended before the look"
            } else {
                "no idle point came to say so at"
            };
            return Some(restore_lost(opts, r, st, key, why));
        }
        st.confirm_by = 0;
        st.look_word.clear();
        save(opts, key, st);
        return None;
    }
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
    let quiet = match told_nothing(opts, &records, key, st) {
        Ok(quiet) => quiet,
        Err(_) => return Some(said(r.clone(), WAIT_LEDGER_EVIDENCE)),
    };
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
    let idle = continuation_session(records, st.resumed_pid, key).filter(|sf| {
        sf.status == "idle"
            && kernel_start(sf.pid).as_deref() == Some(squash(&sf.proc_start).as_str())
    });
    let Some(idle) = idle else {
        return Err("wait:changed");
    };
    if !tab_probe(c, st.resumed_pid, tab) {
        return Err("wait:tab-ownership-changed");
    }
    if held(c, tab, opts.human_grace_s) {
        return Err("wait:held");
    }
    let text = upgrade::restore_prompt(Version::parse(&st.resumed_on).as_ref(), st.is_upgrade());
    let typed = match under_hand(c, tab, None, |c| {
        type_continuation(c, tab, &text, opts.timings.fence_every)
    }) {
        None => return Err("wait:held"),
        Some(Continued::Typed) => true,
        Some(Continued::Stands(word)) => return Err(word),
        Some(Continued::Unanswered) if took_it(records, &idle, opts.timings.first_idle) => true,
        Some(Continued::Unanswered) => return Err("wait:unanswered"),
        Some(Continued::Not) if c.reply_unread() => return Err("wait:no-socket"),
        Some(Continued::Not) => false,
    };
    if !typed {
        return Ok(restore_lost(
            opts,
            r,
            st,
            key,
            "its line could not be typed",
        ));
    }
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

/// The [`St::cause`] of a restart the host made because the worker read the line the loop wrote to
/// it and drew none of it a minute on ([`restart_here`], [`Restart::InputLost`]): the process that
/// may hold it ends, and its relaunch resumes the conversation as a memory restart's does.
pub(super) const CAUSE_INPUT_LOST: &str = "input-lost";

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

/// The [`St::cause`] of a restart onto another credential of the session's own account,
/// `route:` then `token` or `stored` ([`Restart::Route`]).
pub(super) const CAUSE_ROUTE: &str = "route:";

/// What an account move answers where a restart of another kind was in flight for the conversation ([`restart_as`], [`carried_word`]).
pub const IN_FLIGHT_OTHER: &str = "wait:restart-in-flight";

/// What a restart in place answers where it would supersede the conversation's record
/// ([`supersedes`]) but for the native restart's operation still in it (`St::native`,
/// `native_prepare`): the record was carried, so its operation is observed and completed
/// (`native_restart::complete`), and the restart asked is no part of that carry. A deferral, so
/// the loop asks it again under the same ask (on its back-off, or a move put off to a quiet moment
/// at a later turn end), when the record carries no native move and is superseded
/// ([`native_carried`]).
pub const NATIVE_CARRIED: &str = "wait:native-carried";

/// What a restart in place answers where the conversation's record is the asked restart's own,
/// its relaunch already carried on in the very process asking ([`already_made`]): the restart was
/// made, and nothing is signalled again.
pub const ALREADY_MADE: &str = "done:made";

/// What a restart in place answers where it was asked of another process than the tab's agent
/// now ([`asked_of`]): the process it was asked of no longer holds the tab, and no other process
/// is restarted in its place.
pub const AGENT_CHANGED: &str = "wait:agent-changed";

/// Whether `snap`, the tab's agent now, is the process the restart `why` was asked of: a restart
/// for a lost line ([`Restart::InputLost`]) names the process the line was written to and is made
/// of it alone (its pid, and its start where both are known), so a host whose word lags the tab
/// never has a process restarted that holds nothing of the line; every other restart, and one for
/// a line written where the host named no process, is of the tab's agent (that one, asked again,
/// is kept from ending the process its own relaunch started by [`its_own`]: the record any of its
/// asks claimed is its own).
pub(super) fn asked_of(why: &Restart, snap: &Snapshot) -> bool {
    let Restart::InputLost {
        of: (pid, started), ..
    } = why
    else {
        return true;
    };
    let start =
        super::upgrade_models::parse_lstart(&snap.start).and_then(|s| i64::try_from(s).ok());
    pid.is_none_or(|pid| pid == snap.pid) && started.zip(start).is_none_or(|(was, is)| was == is)
}

/// WHETHER THE RESTART `why`, ASKED BY THE AGENT `sf` NAMES, SUPERSEDES THE CONVERSATION'S RECORD
/// `st` IN FLIGHT rather than being answered by carrying it ([`restart_as`]): the record is
/// `Relaunched` and its relaunch already landed in the asker (adopted, `relaunched_pid`, or the
/// process its line started by its parent, `k`'s read), so carrying it ends nothing of the asker
/// (its held wait finds the asker and answers `adopted` at once) and would answer the restart
/// asked with a word for a restart never made. The asked restart is made of the asker instead,
/// its own record the conversation's from its claim; the record's carry-on, owed the asker and
/// not booked typed (a carry-on booked typed leaves the record `Done`), goes with it, and the
/// relaunch after the restart is owed its own. Where the record still holds a native restart's
/// operation (`St::native`, `native_prepare`), whose host operation only its carry observes and
/// completes (`native_restart::complete`), [`restart_as`] carries it first and answers
/// [`NATIVE_CARRIED`] ([`native_carried`]): asked again, the restart supersedes the record the
/// carry left with no native move in it. Never for an account move, whose record of the same
/// account is the move asked ([`carried_word`]), nor where the record is the asked restart's own
/// ([`its_own`], by its `ask`): asked again after its relaunch answered a wait (the process it
/// started not yet registered, its ledger's evidence unread), carrying it adopts the process that
/// restart started, which a restart made over it would end.
pub(super) fn supersedes(
    why: &Restart,
    ask: &RestartAsk,
    st: &St,
    sf: &SessionFile,
    session: &str,
    k: &dyn Kernel,
) -> bool {
    !matches!(why, Restart::Account { .. })
        && !its_own(why, ask, st)
        && matches!(st.phase, upgrade::Phase::Relaunched { .. })
        && landed_in(st, sf, session, k)
}

/// Whether the relaunch the record `st` holds landed in the agent `sf` names: adopted there
/// (`relaunched_pid`), or the process its line started, by its parent (`k`'s read,
/// [`relaunch_of`]).
fn landed_in(st: &St, sf: &SessionFile, session: &str, k: &dyn Kernel) -> bool {
    st.relaunched_pid == sf.pid || relaunch_of(k, sf, session, st.pid, st.shell) == Relaunch::Ours
}

/// WHETHER THE RESTART `why`, ASKED AGAIN AS `ask` BY THE AGENT `sf` NAMES IN TAB `tab`, IS ALREADY
/// MADE ([`ALREADY_MADE`]): the conversation's record `st` is its own ([`its_own`]: an ask of that
/// restart claimed it, of its cause), its relaunch carried on (`Done`) in that tab and landed in
/// the asker ([`carried_on_in`]). A restart whose relaunch answered a wait once its signal had gone
/// (`wait:resume`, `wait:exiting`, `wait:shell-prompt`) is asked again under the same ask, and the
/// sweep may carry its relaunch on first: made again over that record, it would end the process
/// its own relaunch started, a second relaunch for one restart (a model's way back, put off to a
/// quiet moment, the loop asks until it books it made). A record another ask claimed is another
/// restart's, and one landed in another process says nothing of the asker: either is restarted
/// over as any record that is not in flight.
pub(super) fn already_made(
    why: &Restart,
    ask: &RestartAsk,
    st: &St,
    tab: &str,
    (sf, session): (&SessionFile, &str),
    k: &dyn Kernel,
) -> bool {
    matches!(st.phase, upgrade::Phase::Done)
        && st.tab == tab
        && its_own(why, ask, st)
        && carried_on_in(st, sf, session, k)
}

/// How long after a relaunched agent's start its record may be carried on (`St::done_at`) and
/// still be read as that relaunch's ([`carried_on_in`]): the whole seconds both clocks round to.
const CARRIED_ON_SLACK_S: u64 = 5;

/// Whether the relaunch the carried-on record `st` holds landed in the agent `sf` names: adopted
/// there (`relaunched_pid`), or the process its line started by its parent (`k`'s read,
/// [`relaunch_of`]) where that process was up by the time the record was carried on (`done_at`,
/// within [`CARRIED_ON_SLACK_S`]; a start or carry-on unread is no evidence against it). The
/// relaunch's own process was up before its record was carried on; one started after that in the
/// record's shell on the same conversation (`claude -c` typed once the relaunch ended) is another
/// launch, whatever model it runs, which a re-ask restarts over.
fn carried_on_in(st: &St, sf: &SessionFile, session: &str, k: &dyn Kernel) -> bool {
    let up_by_carry_on = || {
        st.done_at == 0
            || super::upgrade_models::parse_lstart(&squash(&sf.proc_start))
                .is_none_or(|began| began <= st.done_at.saturating_add(CARRIED_ON_SLACK_S))
    };
    st.relaunched_pid == sf.pid
        || (relaunch_of(k, sf, session, st.pid, st.shell) == Relaunch::Ours && up_by_carry_on())
}

/// The word [`restart_as`] answers after carrying a record the restart asked supersedes but for
/// its native move (`step`, the carry's word): the carry's own deferral or dry run as it stands,
/// and [`NATIVE_CARRIED`] for anything else it answered, the record's relaunch adopted or ended,
/// since none of that is the restart asked.
pub(super) fn native_carried(step: String) -> String {
    if ["wait:", "busy:", "would-"]
        .iter()
        .any(|head| step.starts_with(head))
    {
        step
    } else {
        NATIVE_CARRIED.to_string()
    }
}

/// Whether the record `st` is the restart `why`'s own, asked again as `ask` ([`supersedes`]): the
/// record an ask of that restart claimed ([`St::ask`], the [`RestartAsk`] every ask of one
/// restart carries: a lost line's its line's, any other restart's its run's), of its cause
/// ([`model_restart`]: a memory restart's, a model's move onto the same model, its way back, a lost
/// line's). Asked again after a deferral, a restart meets its own record wherever the relaunch of
/// the ask that claimed it landed, its first ask's or a later one's: carried, it adopts the process
/// that relaunch started, which superseding would end, a second relaunch for one restart; carried
/// on already (`Done`), it is made ([`already_made`]). Any other
/// record of its cause is another restart's, made before this one was asked and landed in the
/// process now asking (an earlier memory restart's, a model's earlier move, an earlier line's), and
/// so is one met where this restart's first ask was deferred before it claimed anything
/// (`busy:another-sweep`, `wait:changed`, `wait:no-session-record`): carrying it would answer the
/// restart asked with a word for a restart never made, the process it is asked for left running.
/// The cause alone never says whose a record is. A lost line's restart that names its process is of
/// that process alone ([`asked_of`]), so asked again after its relaunch landed it answers
/// [`AGENT_CHANGED`] before any record is read. An account move's is [`carried_word`]'s to answer.
pub(super) fn its_own(why: &Restart, ask: &RestartAsk, st: &St) -> bool {
    let cause = st.cause.as_str();
    let of_cause = match why {
        Restart::Memory => cause == CAUSE_MEMORY,
        Restart::Model { to } => cause.strip_prefix(CAUSE_MODEL) == Some(to.as_str()),
        Restart::ModelBack { .. } => cause.starts_with(CAUSE_MODEL_BACK),
        Restart::InputLost { .. } => cause == CAUSE_INPUT_LOST,
        Restart::Account { .. } => false,
        Restart::Route { to } => cause.strip_prefix(CAUSE_ROUTE) == Some(to.word()),
    };
    of_cause && st.ask == ask.word()
}

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
        Restart::Memory | Restart::InputLost { .. } => ModelRestart {
            fallback_from: fallback_carried(origin, riding.as_deref(), hand.as_deref()),
            model: riding.or(hand),
            cause: if matches!(why, Restart::Memory) {
                CAUSE_MEMORY
            } else {
                CAUSE_INPUT_LOST
            }
            .to_string(),
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
        // The same account on another credential: the model it runs rides, as a memory
        // restart's does, and a fallback it rides stays one.
        Restart::Route { to } => ModelRestart {
            fallback_from: fallback_carried(origin, riding.as_deref(), hand.as_deref()),
            model: riding.or(hand),
            cause: format!("{CAUSE_ROUTE}{}", to.word()),
            launch_model: launched,
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
        || cause == CAUSE_INPUT_LOST
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
        || cause == CAUSE_INPUT_LOST
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
    } else if cause == CAUSE_INPUT_LOST {
        format!(
            "Restarted: {agent} read the line aterm wrote to it and drew none of it a minute on, \
             so this session was ended and restarted"
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

/// The restart in place `why`, asked as `ask` ([`RestartAsk`]): its report and its claim
/// ([`Restarted`]).
#[must_use]
pub fn restart_here(opts: &Opts, why: &Restart, ask: &RestartAsk) -> Restarted {
    restart_from(opts, why, ask, snapshot(opts))
}

/// [`restart_here`] for a host that keeps the owner's roster of Claude Code accounts.
#[must_use]
pub fn restart_under(
    opts: &Opts,
    why: &Restart,
    ask: &RestartAsk,
    roster: &accounts::Roster,
    selected: &AccountRestart,
) -> Restarted {
    restart_as(
        opts,
        why,
        ask,
        roster,
        snapshot(opts),
        Some(selected),
        &Live,
    )
}

/// The process, target row and reason chosen by an account decision.
/// `current` fences the caller's generation and pending move; it must perform no I/O.
pub struct AccountRestart {
    source: Snapshot,
    target: accounts::Row,
    why: Restart,
    current: Box<dyn Fn() -> bool + Send + Sync>,
}

impl AccountRestart {
    /// Capture a selection and its caller-owned cancellation check.
    #[must_use]
    pub fn new(
        why: &Restart,
        source: &Snapshot,
        target: &accounts::Row,
        current: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            source: source.clone(),
            target: target.clone(),
            why: why.clone(),
            current: Box::new(current),
        }
    }

    /// Whether fresh observations still describe this exact account selection.
    #[must_use]
    pub fn matches(&self, why: &Restart, roster: &accounts::Roster, observed: &Snapshot) -> bool {
        matches!(why, Restart::Account { to, .. }
            if self.why == *why && self.target.label == *to
                && roster.row(to) == Some(&self.target))
            && self.source.same_auth_context(observed)
            && self.is_current()
    }

    fn is_current(&self) -> bool {
        (self.current)()
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRestartSource",
        action = "AnotherCase",
        reason = "Select a source, target, reason or retirement change"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRestartSource",
        action = "Choose",
        reason = "Capture the selected restart before fresh observations"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRestartSource",
        action = "Observe",
        project = "harness::relaunch::tests::account_restart_source::selection_transition"
    )
)]
fn restart_admitted(
    why: &Restart,
    roster: &accounts::Roster,
    observed: &Result<Snapshot, &'static str>,
    selected: Option<&AccountRestart>,
) -> bool {
    match selected {
        Some(selected) => observed
            .as_ref()
            .is_ok_and(|snap| selected.matches(why, roster, snap)),
        None => !matches!(why, Restart::Account { .. }),
    }
}

/// Whether the process `snap` was read from runs under the account the roster labels `label`.
#[must_use]
pub fn runs_under(snap: &Snapshot, roster: &accounts::Roster, label: &str) -> bool {
    roster
        .row_for_login(snap.login.as_deref())
        .is_some_and(|row| row.label == label)
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
    // field) at the login file `CODEX_AUTH_PATH` (or `$CODEX_HOME/auth.json`) names. It is no Codex
    // variable: tools the agent runs read it (Codex's bundled image skill's CLI fallback "Requires
    // `OPENAI_API_KEY`"), and aterm passes it to the shells it opens, so removing it would take it
    // from those tools.
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
    if accounts::admissible_label(label) {
        let named = home.join(format!("leader-{label}-{tag:08x}.sock"));
        if fits(&named) {
            return Ok(named);
        }
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
    /// carried in place of a stored login ([`ahead_env`]), the login seam for a default row,
    /// and for Grok Build on a row of its
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
    if env.value.is_none() {
        unset.insert(0, env.var);
    }
    if cli != accounts::Cli::Grok {
        return Ok(PoolArgs {
            unset,
            flags: flags.to_vec(),
        });
    }
    let drops = |path: &str| env.value.is_some() || names_row_socket(path, home);
    let mut kept = Vec::with_capacity(flags.len() + 2);
    let mut after_dashdash = Vec::new();
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        if flag == "--" {
            after_dashdash.push(flag.clone());
            after_dashdash.extend(it.cloned());
            break;
        } else if flag == LEADER_SOCKET_FLAG {
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
    kept.extend(after_dashdash);
    Ok(PoolArgs { unset, flags: kept })
}

/// The argv that resumes session `id` of `cli`'s program, as each CLI's `--help` spells it.
#[must_use]
pub fn resume_argv(cli: accounts::Cli, id: &str) -> [String; 2] {
    let word = match cli {
        accounts::Cli::Codex => "resume",
        accounts::Cli::Claude | accounts::Cli::Grok => "--resume",
        accounts::Cli::Kimi => "--session",
        accounts::Cli::Agy => "--conversation",
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
/// types a pooled Codex line yet: [`super::accounts_pool::session_plan`] prepares it, and
/// the host asks it of no Codex session (aterm-gui's `harness_host` supervises no Codex: its
/// worker only watches) and keeps no line it returns.
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
    let PoolArgs { unset, flags } = pool_args(cli, env, how.home, how.flags)?;
    let args: Vec<String> = match cli {
        accounts::Cli::Codex => {
            let argv: Vec<String> = std::iter::once(String::new()).chain(flags).collect();
            let kept = super::codex_alab::kept_flags(&argv, |_, _| true)
                .map_err(|why| format!("Codex's carried flags cannot resume a thread: {why}"))?;
            super::codex_alab::resume_words(kept, how.resume)
        }
        accounts::Cli::Claude | accounts::Cli::Grok | accounts::Cli::Kimi | accounts::Cli::Agy => {
            let resume = how.resume.map(|id| resume_argv(cli, id));
            flags
                .into_iter()
                .chain(resume.into_iter().flatten())
                .collect()
        }
    };
    let set: Vec<(&str, &str)> = match env.value.as_deref() {
        Some(value) => vec![(env.var, value)],
        None => Vec::new(),
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
    ask: &RestartAsk,
    snap: Result<Snapshot, &'static str>,
) -> Restarted {
    restart_with(opts, why, ask, snap, &Live)
}

/// [`restart_from`] with process facts from `k` ([`Kernel`]), the seam a test scripts the agent's job and terminal through.
pub(super) fn restart_with(
    opts: &Opts,
    why: &Restart,
    ask: &RestartAsk,
    snap: Result<Snapshot, &'static str>,
    k: &dyn Kernel,
) -> Restarted {
    restart_as(opts, why, ask, &accounts::Roster::default(), snap, None, k)
}

/// The login an account move's relaunch line carries.
pub(super) fn account_login(roster: &accounts::Roster, label: &str) -> Option<String> {
    roster.row(label).map(|row| accounts::login_env(row).1)
}

/// WHAT A RESTART IN PLACE ANSWERED ([`restart_as`]): its report, and what it read of the
/// conversation's record once it answered ([`RestartClaim`], the loop's
/// [`crate::supervise::policy::turn_end::RestartAnswer::claim`]), read back from the record as the
/// restart left it, under the sweep's lock it held ([`claim_of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restarted {
    /// The restart's report: its step, and the agent's pid it names.
    pub report: Report,
    /// What the restart read of the conversation's record once it answered.
    pub claim: RestartClaim,
}

impl Restarted {
    /// `report`, where the restart read no record ([`RestartClaim::Unread`]).
    fn unread(report: Report) -> Self {
        Self {
            report,
            claim: RestartClaim::Unread,
        }
    }
}

/// HOW FAR A RESTART IN PLACE WENT WITH THE CONVERSATION'S RECORD ([`restart_locked`]), by which
/// [`restart_as`] reads its claim back ([`claim_of`]).
#[derive(Debug)]
enum Met {
    /// No record read: no session record of the asker named.
    Unread,
    /// The record read and left as it stood: another tab's in flight, or an account move's
    /// deferred over one.
    Stood,
    /// The record in flight carried.
    Carried,
    /// The asked restart's own record found carried on in the asker ([`already_made`]).
    Made,
    /// A claim of the restart's own asked for, over the record that stood before (`None`: none),
    /// boxed so the other variants stay small.
    Claiming(Option<Box<St>>),
}

/// THE CLAIM [`restart_as`] SAYS ([`RestartClaim`]), read back from the conversation's record as
/// the restart left it (`report` names its session) by how far it went (`met`). The record holds
/// the asked restart's claim live where it is that restart's own ([`its_own`]) and its relaunch
/// is in flight or carried on ([`claim_live`]): held where the restart carried that record (in
/// the asker's tab, as only such a record is carried) or found it made, taken where the restart
/// asked a claim of its own and the record is no longer the one that stood before (a record this
/// restart writes is the asker's tab's). Anything else read holds no live claim of the restart's:
/// another's record, another tab's in flight (left as it stood), its own stopped or failed unmade,
/// its own retired unissued (as the restart found it or left it), its own claim unwritten or
/// restored, its own carried on in another process than the asker (left as it stood).
fn claim_of(
    opts: &Opts,
    why: &Restart,
    ask: &RestartAsk,
    report: &Report,
    met: Met,
) -> RestartClaim {
    let live = || {
        load_record(opts, &report.session)
            .map(|record| record.filter(|st| its_own(why, ask, st) && claim_live(st)))
    };
    match met {
        Met::Unread => RestartClaim::Unread,
        Met::Stood => RestartClaim::Unclaimed,
        Met::Made => RestartClaim::Held,
        Met::Carried => match live() {
            Err(_) => RestartClaim::Unread,
            Ok(Some(_)) => RestartClaim::Held,
            Ok(None) => RestartClaim::Unclaimed,
        },
        Met::Claiming(prior) => match live() {
            Err(_) => RestartClaim::Unread,
            Ok(Some(st)) if prior.as_deref() != Some(&st) => RestartClaim::Taken,
            Ok(_) => RestartClaim::Unclaimed,
        },
    }
}

/// Whether the claim the record `st` holds is live: its relaunch in flight ([`St::in_flight`]),
/// which a re-ask under its ask carries, or carried on (`Done`), which a re-ask answers made where
/// it landed in the asker ([`already_made`]). A relaunch stopped or failed (`Failed`) made nothing:
/// a re-ask over its record restarts afresh. One retired unissued (`native_retired_unissued`: its
/// native command positively not granted, the record held durably so the conversation is kept)
/// is in flight by [`St::in_flight`] for good, yet nothing of it is under way or ever will be:
/// every re-ask over it waits ([`native_restart::WAIT_RETIRED_UNISSUED`]), so it holds no live
/// claim.
///
/// [`native_restart::WAIT_RETIRED_UNISSUED`]: super::native_restart::WAIT_RETIRED_UNISSUED
fn claim_live(st: &St) -> bool {
    !st.native_retired_unissued && (st.in_flight() || st.phase == upgrade::Phase::Done)
}

/// [`restart_with`] under `roster` ([`restart_under`]). A restart asked of another process than
/// the tab's agent makes none ([`asked_of`]); the conversation's record in flight answers the
/// restart asked by being carried ([`carried_word`]) unless the restart supersedes it
/// ([`supersedes`]), and one it supersedes that still holds a native restart's operation is
/// carried first and answered by [`native_carried`], superseded at the restart's re-ask; its own
/// record already carried on in the asker answers [`ALREADY_MADE`] ([`already_made`]); the record
/// it claims keeps its `ask` ([`St::ask`]). THE CLAIM IS SAID HERE, ONCE ([`Restarted::claim`],
/// [`claim_of`]): with the sweep's lock still held, the conversation's record as the restart left
/// it is read back. An answer given before any record is read (an account move not listed or not
/// admitted, another sweep's lock, a snapshot of another tab or none, one of a process the
/// restart was not asked of or with no shell job, one that names no session record) is
/// [`RestartClaim::Unread`], whoever's the record is. Past that, the claim is
/// [`RestartClaim::Taken`] where this answer wrote the record as the asked restart's own and its
/// claim is live (a relaunch landed or in flight, its signal gone; a native move's record saved),
/// [`RestartClaim::Held`] where this answer carried the restart's own record in flight and it is
/// live still, or found it made, and [`RestartClaim::Unclaimed`] otherwise. No account move's
/// record is its own, so no account move's answer holds or takes a claim.
pub(super) fn restart_as(
    opts: &Opts,
    why: &Restart,
    ask: &RestartAsk,
    roster: &accounts::Roster,
    snap: Result<Snapshot, &'static str>,
    selected: Option<&AccountRestart>,
    k: &dyn Kernel,
) -> Restarted {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let r = Report {
        pid: 0,
        tab,
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: String::new(),
    };
    let target = match why {
        Restart::Account { to, .. } => match account_login(roster, to) {
            Some(login) => Some(login),
            None => return Restarted::unread(said(r, "refused:account-not-listed")),
        },
        _ => None,
    };
    if !restart_admitted(why, roster, &snap, selected) {
        return Restarted::unread(said(r, "wait:account-changed"));
    }
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return Restarted::unread(said(r, format!("busy:{why}"))),
    };
    let mut met = Met::Unread;
    let report = restart_locked(opts, why, ask, snap, selected, k, r, target, &mut met);
    let claim = claim_of(opts, why, ask, &report, met);
    Restarted { report, claim }
}

/// [`restart_as`] past its admission, under the sweep's lock: `r` the report so far, `target` the
/// login an account move's line carries, `met` set to how far it went with the conversation's
/// record ([`Met`]).
#[allow(clippy::too_many_arguments)]
fn restart_locked(
    opts: &Opts,
    why: &Restart,
    ask: &RestartAsk,
    snap: Result<Snapshot, &'static str>,
    selected: Option<&AccountRestart>,
    k: &dyn Kernel,
    mut r: Report,
    target: Option<String>,
    met: &mut Met,
) -> Report {
    let tab = r.tab.clone();
    let snap = match snap {
        Ok(snap) if snap.tab == tab => snap,
        Ok(_) => return said(r, "wait:tab-identity-conflict"),
        Err(why) => return said(r, format!("wait:{why}")),
    };
    r.pid = snap.pid;
    if !asked_of(why, &snap) {
        return said(r, AGENT_CHANGED);
    }
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
    let prior = match load_record(opts, &session) {
        Ok(record) => record,
        Err(_) => {
            return said(
                r,
                format!("wait:{}", super::upgrade_drive::UNREAD_SAVED_RECORD),
            );
        }
    };
    *met = Met::Stood;
    // A record an earlier build wrote, naming its agent by pid alone, takes no start from this
    // live snapshot: a process born since at that pid would read as its agent. Its carry judges
    // it unproven while that pid lives (`wait:exit-unproven`), never waiting on the bare pid.
    if let Some(st) = prior.clone().filter(St::in_flight) {
        if st.native_retired_unissued {
            return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
        }
        if st.tab != tab {
            return said(r, "wait:conversation-in-other-tab");
        }
        if selected.is_some_and(|s| !s.is_current()) {
            return said(r, "wait:account-changed");
        }
        let supersedes = supersedes(why, ask, &st, &sf, &session, k);
        let native = st.native.is_some() || !st.native_prepare.is_empty();
        if !supersedes || native {
            let cause = st.cause.clone();
            *met = Met::Carried;
            let carried = carry_in_flight(opts, r, st, &session, &tab);
            let step = if supersedes {
                native_carried(carried.step.clone())
            } else {
                carried_word(why, &cause, carried.step.clone())
            };
            return said(carried, step);
        }
    }
    if prior
        .as_ref()
        .is_some_and(|st| already_made(why, ask, st, &tab, (&sf, &session), k))
    {
        *met = Met::Made;
        return said(r, ALREADY_MADE);
    }
    *met = Met::Claiming(prior.clone().map(Box::new));
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
        matches!(
            why,
            Restart::Memory
                | Restart::InputLost { .. }
                | Restart::Account { .. }
                | Restart::Route { .. }
        ),
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
    let moves_account = target.is_some();
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
        route: moved_route(moves_account, snap.route),
        auth_observed: snap.auth_observed,
        launch_env: snap.launch_env.as_ref(),
        billing: if riding_move || matches!(why, Restart::Model { .. } | Restart::ModelBack { .. })
        {
            ModelBilling::Move(snap.profile.as_ref())
        } else {
            ModelBilling::Keep
        },
        serve: serve_for(why),
    };
    let mut st = St {
        from: sf.version.clone(),
        to: sf.version.clone(),
        source: SAME_PROGRAM.to_string(),
        cause,
        ask: ask.word().to_string(),
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
    let Plan {
        shell,
        line,
        tap_dropped,
        token_dropped,
        line_dir,
        command,
        ..
    } = match planned {
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
    if selected.is_some_and(|s| !s.is_current()) {
        return said(r, "wait:account-changed");
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
                && k.process_start(snap.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        }) && foreground_shell(k.job(snap.pid)) == Ok(shell)
            && k.terminal(snap.pid)
                .as_ref()
                .is_some_and(|(p, name)| owned_by_aterm((*p, name.as_str())))
            && screen(&mut c, &tab).is_some_and(|scr| at_rest_for(why, &mut c, &tab, &scr))
            && k.restart_background(snap.pid).is_empty()
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
        if selected.is_some_and(|s| !s.is_current()) {
            break 'signal Some(said(r.clone(), "wait:account-changed"));
        }
        (st.model_before, st.mark) = model_and_mark(recent.as_ref());
        st.bind(snap.pid, &snap.start);
        st.shell = shell;
        st.tab.clone_from(&tab);
        st.line = line;
        st.account_intent = command
            .as_ref()
            .and_then(|command| command.account_intent.clone());
        st.line_dir = line_dir.unwrap_or_default();
        st.line_grace_s = Some(opts.human_grace_s);
        st.phase = upgrade::Phase::Exiting { at_s: now_s() };
        if save_checked(opts, &session, &st).is_err() {
            break 'signal Some(said(r.clone(), "wait:claim-unwritten"));
        }
        super::upgrade_drive::note_moved_writes(opts, &snap, &session, &sf.cwd);
        let admission = super::native_restart::begin(
            opts,
            &mut c,
            &mut st,
            &session,
            super::contract::Scope::Relaunch,
            PlannedCommand::expects(command.as_ref(), std::path::Path::new(&snap.program)),
            command.as_ref(),
            None,
        );
        if matches!(admission, Ok(super::native_restart::Recovery::Completed)) {
            break 'signal Some(said(r.clone(), "wait:native-completed"));
        }
        if matches!(admission, Ok(super::native_restart::Recovery::Retired)) {
            break 'signal Some(said(r.clone(), "failed:native-cohort-retired"));
        }
        if matches!(admission, Ok(super::native_restart::Recovery::Held)) {
            break 'signal Some(said(
                r.clone(),
                super::native_restart::WAIT_RETIRED_UNISSUED,
            ));
        }
        if let Err(why) = admission {
            if st.native_prepare.is_empty()
                || super::native_restart::abort(opts, &session, &mut c, &tab, &mut st)
            {
                if super::native_restart::coordinated(&st) {
                    let _ = super::native_record::restore_inactive(
                        opts,
                        &session,
                        &mut st,
                        prior.as_ref(),
                    );
                } else {
                    restore(opts, &session, prior.as_ref());
                }
            } else {
                save(opts, &session, &st);
            }
            if st.native_retired_unissued {
                break 'signal Some(said(
                    r.clone(),
                    super::native_restart::WAIT_RETIRED_UNISSUED,
                ));
            }
            break 'signal Some(said(r.clone(), format!("wait:{why}")));
        }
        if st.native.is_some()
            && (selected.is_some_and(|s| !s.is_current())
                || foreground_shell(k.job(snap.pid)) != Ok(shell)
                || !screen(&mut c, &tab).is_some_and(|scr| at_rest_for(why, &mut c, &tab, &scr))
                || !k.restart_background(snap.pid).is_empty()
                || held(&mut c, &tab, opts.human_grace_s)
                || records.unique_owner(&sf).is_err())
        {
            if super::native_restart::abort(opts, &session, &mut c, &tab, &mut st) {
                let _ =
                    super::native_record::restore_inactive(opts, &session, &mut st, prior.as_ref());
            } else {
                save(opts, &session, &st);
            }
            if st.native_retired_unissued {
                break 'signal Some(said(
                    r.clone(),
                    super::native_restart::WAIT_RETIRED_UNISSUED,
                ));
            }
            break 'signal Some(said(r.clone(), "wait:changed-after-native-admission"));
        }
        let signal = if let Some(binding) = st.native.as_ref() {
            super::native_restart::signal(&mut c, &tab, binding)
        } else {
            terminate(&mut c, &tab, pid, opts.human_grace_s)
        };
        match signal {
            Terminated::Sent => None,
            Terminated::Refused(no) => {
                if st.native.is_some() {
                    save(opts, &session, &st);
                    break 'signal Some(said(r.clone(), format!("wait:signal-{no}")));
                }
                if super::native_restart::coordinated(&st) {
                    let _ = super::native_record::restore_inactive(
                        opts,
                        &session,
                        &mut st,
                        prior.as_ref(),
                    );
                } else {
                    restore(opts, &session, prior.as_ref());
                }
                Some(said(r.clone(), format!("wait:signal-{no}")))
            }
            Terminated::Failed => {
                if st.native.is_some() || !st.native_prepare.is_empty() {
                    save(opts, &session, &st);
                } else if super::native_restart::coordinated(&st) {
                    let _ = super::native_record::restore_inactive(
                        opts,
                        &session,
                        &mut st,
                        prior.as_ref(),
                    );
                } else {
                    restore(opts, &session, prior.as_ref());
                }
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
    note_dropped(opts, &r, tap_dropped.as_deref(), token_dropped.as_deref());
    let r = relaunch(opts, r, &mut st, &mut c, &session, k);
    save(opts, &session, &st);
    r
}

/// The route an account move's line is judged from (§5.2: "a move's target row starts on its own
/// route"): a session on its own account's credential, its stored login or its vault token, moves
/// onto the target row's stored login, which the one judgment then puts on that row's token where
/// it is in use here ([`token_route::judge`]); a session off it stays off it. Any other restart
/// keeps the route the replaced process ran on.
pub(super) const fn moved_route(
    moves_account: bool,
    route: accounts::AuthRoute,
) -> accounts::AuthRoute {
    match route {
        accounts::AuthRoute::VaultToken { .. } if moves_account => accounts::AuthRoute::Stored,
        route => route,
    }
}

/// What a restart's line does with the account's vault token ([`Serve`]): a route move's is its
/// target's, every other restart's the one judgment's.
pub(super) const fn serve_for(why: &Restart) -> Serve {
    match why {
        Restart::Route {
            to: crate::supervise::policy::turn_end::RouteTo::Token,
        } => Serve::Must,
        Restart::Route {
            to: crate::supervise::policy::turn_end::RouteTo::Stored,
        } => Serve::Never,
        Restart::Memory
        | Restart::InputLost { .. }
        | Restart::Model { .. }
        | Restart::ModelBack { .. }
        | Restart::Account { .. } => Serve::Judge,
    }
}

/// Whether the tab's screen lets [`restart_as`] signal its agent: none of the agent's own work
/// running under its turn's end as the screen names it ([`aterm_phase::phase::work_running`],
/// read as Claude Code's, the one agent restarted in place), unless the restart is made over it
/// ([`over_own_work`]); and the composer empty, or, for an account move, a sign-in menu drawn in
/// the composer's place ([`TurnEndReading::sign_in_menu_drawn`]), where nothing typed stands to
/// lose and the login the move leaves is the menu's own. The processes under the agent (a shell,
/// a monitor) are the process table's to say too ([`live_background`]); this reads the work the
/// screen names, among it the work no process row names ([`aterm_phase::phase::unlisted_work`]:
/// the work Claude Code runs in its own process, any other work its footer's pill counts).
fn at_rest_for(why: &Restart, c: &mut Client, tab: &str, scr: &Screen) -> bool {
    (over_own_work(why) || aterm_phase::phase::work_running(&scr.rows).is_none())
        && (composer_empty(c, tab, scr)
            || (matches!(why, Restart::Account { .. })
                && TurnEndReading::sign_in_menu_drawn(
                    Some(aterm_phase::Program::Claude.name()),
                    &scr.rows,
                    scr.cursor.map(|(_, col)| col),
                )))
}

/// WHETHER THE RESTART `why` IS MADE OVER THE AGENT'S OWN WORK ITS SCREEN NAMES RUNNING
/// ([`at_rest_for`]): a memory restart alone, the remedy for that very process's memory (Claude
/// Code's critical-memory banner, or the Mac short of memory with it the largest past aterm's
/// ceiling), whose in-process work lives in that memory and ends with the process's crash, which
/// waiting risks; the work's processes still hold it, as every restart's do ([`live_background`]).
/// Every other restart waits for the work to end, as the upgrade's automatic restart does
/// ([`super::upgrade_drive`]'s `turn_over_shown`): a lost line's restart, for a process that runs
/// on and whose line the loop holds meanwhile (`custody::Rest::working`); a model's move onto its
/// fallback, or back at its reset, and an account move, each a move a later turn's end makes as
/// well, where the work it would end costs the session more than the wait.
const fn over_own_work(why: &Restart) -> bool {
    match why {
        Restart::Memory => true,
        Restart::InputLost { .. }
        | Restart::Model { .. }
        | Restart::ModelBack { .. }
        | Restart::Account { .. }
        | Restart::Route { .. } => false,
    }
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
    /// How it authenticates, by its own exec environment ([`accounts::ProcessLogin::of_exec_env`]'s
    /// route; an unread one is outside).
    pub route: accounts::AuthRoute,
    /// Whether Claude's authentication attribution was captured, rather than omitted by a legacy restore.
    /// Other providers carry their authentication provenance in their own run fields.
    pub auth_observed: bool,
    /// The exact profile paths and filename selector captured from this process's own exec environment and resolved records.
    pub profile: Option<super::profile::Context>,
    /// What its cold account admission reads of the environment its shell passed it, read from
    /// its own exec environment with the rest of this snapshot ([`accounts::LaunchEnv`]): the
    /// base that admission reads where the shell's own environment cannot be read.
    pub launch_env: Option<accounts::LaunchEnv>,
}

impl Snapshot {
    /// The process's start in Unix seconds, only while its kernel identity still
    /// matches this snapshot. Reads the process table; call from a background worker.
    #[must_use]
    pub fn current_start_seconds(&self) -> Option<u64> {
        let current = kernel_start(self.pid);
        bound_start_seconds(&self.start, current.as_deref(), || {
            #[cfg(windows)]
            {
                let birth = aterm_uds::process::read_process_birth(i32::try_from(self.pid).ok()?)?;
                (kernel_start(self.pid).as_deref() == Some(self.start.as_str()))
                    .then_some(birth.seconds)
            }
            #[cfg(not(windows))]
            {
                super::upgrade_models::parse_lstart(&self.start)
            }
        })
    }

    /// Match launch and authentication identity while allowing live record metadata to advance.
    #[must_use]
    pub fn same_auth_context(&self, other: &Self) -> bool {
        // Classify every field: metadata and job-control observations may advance independently.
        let Self {
            tab,
            pid,
            start,
            program,
            argv,
            run,
            login,
            home,
            claude_dir,
            route,
            auth_observed,
            profile,
            cwd: _,
            session: _,
            version: _,
            shell: _,
            dialect: _,
            // Read from the same exec environment as `login`, `home` and `route`, which
            // are compared: a record carried from an older layout without it is the same context.
            launch_env: _,
        } = self;
        (
            tab, pid, start, program, argv, run, login, home, claude_dir, route, profile,
        ) == (
            &other.tab,
            &other.pid,
            &other.start,
            &other.program,
            &other.argv,
            &other.run,
            &other.login,
            &other.home,
            &other.claude_dir,
            &other.route,
            &other.profile,
        ) && (!matches!(run, AgentRun::Claude) || auth_observed == &other.auth_observed)
    }

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
            AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) => None,
        }
    }

    /// [`Snapshot::codex`], to change.
    pub const fn codex_mut(&mut self) -> Option<&mut CodexRun> {
        match &mut self.run {
            AgentRun::Codex(run) => Some(run),
            AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) => None,
        }
    }

    /// A Muse Code's run, `None` for any other agent's.
    #[must_use]
    pub const fn muse(&self) -> Option<&MuseRun> {
        match &self.run {
            AgentRun::Muse(run) => Some(run),
            AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Agy(_) => None,
        }
    }

    /// An agy's run, `None` for any other agent's.
    #[must_use]
    pub const fn agy(&self) -> Option<&AgyRun> {
        match &self.run {
            AgentRun::Agy(run) => Some(run),
            AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Muse(_) => None,
        }
    }
}

fn bound_start_seconds(
    recorded: &str,
    current: Option<&str>,
    read_seconds: impl FnOnce() -> Option<u64>,
) -> Option<u64> {
    if current != Some(recorded) {
        return None;
    }
    read_seconds()
}

#[cfg(test)]
mod snapshot_start_tests {
    use super::bound_start_seconds;

    #[test]
    fn seconds_require_the_snapshots_current_kernel_start() {
        for current in [None, Some("replacement-process")] {
            assert_eq!(
                bound_start_seconds("recorded-process", current, || {
                    panic!("a different process must not supply its birth")
                }),
                None
            );
        }
        assert_eq!(
            bound_start_seconds("recorded-process", Some("recorded-process"), || Some(42)),
            Some(42)
        );
        assert_eq!(
            bound_start_seconds("recorded-process", Some("recorded-process"), || None),
            None
        );
    }
}

/// The agent a [`Snapshot`] is of, with the run only that agent's relaunch reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRun {
    /// Claude Code: its run is the snapshot's own fields (`login`, `home`, `claude_dir`, `route`).
    Claude,
    /// A Codex TUI ([`CodexRun`]).
    Codex(CodexRun),
    /// A Muse Code TUI ([`MuseRun`]).
    Muse(MuseRun),
    /// An agy TUI ([`AgyRun`]).
    Agy(AgyRun),
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
        AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) => {
            return None;
        }
    }
    let dialect = snap.dialect?;
    let session = snap
        .session
        .as_deref()
        .filter(|s| upgrade::is_session_id(s))?;
    if upgrade::one_shot(&snap.argv)
        || snap
            .cwd
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return None;
    }
    let homedir = home_carried(
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
        &required_env(
            snap.home.as_deref(),
            homedir.as_deref(),
            snap.login.as_deref(),
        ),
    )
    .ok()
}

/// The longest model id [`tap_room`] keeps room for: a relaunch may name the conversation's model
/// (`--model`), which only its records say.
const TAP_ROOM_MODEL: usize = 64;

/// The permission mode [`tap_room`] keeps room for: the longest one a relaunch names
/// ([`with_permission_mode`] names none for `bypassPermissions`).
const TAP_ROOM_MODE: &str = "acceptEdits";

/// WHETHER THE RELAUNCH LINE aterm would type for `snap`'s Claude Code HAS ROOM FOR THE USAGE TAP
/// `tap`, as far as a live read can tell: the one judgment the Claude Code footer's promise
/// (`weekly – · until relaunch`) makes beside its settings' ([`super::upgrade_drive::tap_settings_admit`]),
/// so it never promises a tap the line would drop ([`line_tap_kept`]). The line is the one
/// [`plan`] builds: the shell's dialect, atpkg's heal hook under `user_home`, a `cd` unless the
/// shell is read standing in the agent's directory, its program, argv, conversation, home and
/// login, and room besides for a `--model` of [`TAP_ROOM_MODEL`] bytes and a `--permission-mode`
/// of [`TAP_ROOM_MODE`], which only the conversation's records say. `false` where no such line
/// can be built at all.
#[must_use]
pub fn tap_room(user_home: &Path, snap: &Snapshot, tap: &str) -> bool {
    let (Some(dialect), Some(session)) = (
        snap.dialect,
        snap.session
            .as_deref()
            .filter(|s| upgrade::is_session_id(s)),
    ) else {
        return false;
    };
    if !matches!(snap.run, AgentRun::Claude) {
        return false;
    }
    let Ok(homedir) = home_carried(
        user_home,
        snap.home.as_deref(),
        &snap.cwd,
        &Records::of_snapshot(user_home, snap),
    ) else {
        return false;
    };
    let heal = atpkg::hooks::hook_file(user_home, dialect.hook_ext()).map(|(p, _)| p);
    let cd = (!snap.cwd.is_empty()
        && !same_dir(
            super::upgrade_drive::cwd_of(snap.shell).as_deref(),
            &snap.cwd,
        ))
    .then_some(snap.cwd.as_str());
    let model = "m".repeat(TAP_ROOM_MODEL);
    line_tap_kept(
        dialect,
        heal.as_deref(),
        cd,
        &snap.program,
        &snap.argv,
        Some(session),
        Some(&model),
        Some(TAP_ROOM_MODE),
        &required_env(
            snap.home.as_deref(),
            homedir.as_deref(),
            snap.login.as_deref(),
        ),
        None,
        Some(tap),
    )
    .is_ok_and(|(_, dropped)| dropped.is_none())
}

/// The line a person is told to run to resume the agent `snap` describes by hand, for a user whose home is `user_home`.
#[must_use]
pub fn told_command(user_home: &Path, state: Option<&Path>, snap: &Snapshot) -> Option<String> {
    match &snap.run {
        AgentRun::Claude => {}
        AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) => {
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
    let places = super::agent_image::Places {
        claude_versions: Some(native),
        roots: None,
    };
    let laid = super::agent_image::in_vendor_layout(
        aterm_phase::Program::Claude,
        &super::agent_image::Image::at(image),
        &places,
    );
    if !(is_claude(image) || laid || registered) {
        return None;
    }
    let mut out = vec![image.to_string_lossy().into_owned()];
    out.extend(argv.iter().skip(1).cloned());
    Some((image.to_path_buf(), out))
}

/// Read the [`Snapshot`] of the Claude Code (or codex-alab, Muse Code or agy) in tab `opts.only_sid` on `opts.sock`.
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

/// Retain passive native crash evidence only after the host published this snapshot.
/// The caller must release its publication locks before this control exchange; the observer
/// still verifies that the exact vendor and shell are live before registering either one.
pub fn observe_published(opts: &Opts, snap: &Snapshot) {
    super::native_restart::observe_live(opts, snap);
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
    /// No lane relaunches it.
    None,
}

/// The lane a foreground process is read by: the basename of its mapped image (`image`; `None`:
/// unreadable), a Codex's lane only for an ALab build, and none for a retired agent's
/// ([`aterm_phase::retired_agent`]: Gemini CLI, which agy replaces).
pub(super) fn snapshot_lane(image: Option<&Path>) -> SnapshotLane {
    let name = image.and_then(Path::file_name).and_then(|n| n.to_str());
    if name.is_some_and(aterm_phase::retired_agent) {
        return SnapshotLane::None;
    }
    match name.and_then(aterm_phase::program_of) {
        Some(aterm_phase::Program::Codex)
            if image.is_some_and(|exe| {
                super::agent_image::in_vendor_layout(
                    aterm_phase::Program::Codex,
                    &super::agent_image::Image::at(exe),
                    &super::agent_image::Places::default(),
                )
            }) =>
        {
            SnapshotLane::CodexAlab
        }
        Some(aterm_phase::Program::Muse) => SnapshotLane::Muse,
        Some(aterm_phase::Program::Agy) => SnapshotLane::Agy,
        Some(
            aterm_phase::Program::Codex | aterm_phase::Program::Grok | aterm_phase::Program::Kimi,
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
        route: crate::harness::accounts::AuthRoute::Outside,
        auth_observed: false,
        profile: None,
        launch_env: None,
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
    match snapshot_lane(image.as_deref()) {
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
        route: accounts::ProcessLogin::of_exec_env(&args.env)
            .map_or(accounts::AuthRoute::Outside, |who| who.route),
        auth_observed: !args.env.is_empty(),
        profile: records.billing_context(&opts.home, &args),
        launch_env: accounts::LaunchEnv::of_exec_env(&args.env),
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
        AgentRun::Codex(_) | AgentRun::Muse(_) | AgentRun::Agy(_) => {
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
}

impl ExitRecord {
    /// The exit was a crash, read at the exit.
    #[must_use]
    pub const fn crashed(&self) -> bool {
        matches!(self, Self::Survived(_) | Self::Crashed)
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
        }
    }
}

/// The exit is its launch's own end, by what it left as it was seen ([`exit_record`]).
#[must_use]
pub fn launch_ended(snap: &Snapshot, left: &ExitRecord) -> Option<&'static str> {
    let one_shot = match &snap.run {
        AgentRun::Claude => upgrade::one_shot(&snap.argv),
        AgentRun::Codex(_) => !super::codex_alab::is_tui_argv(&snap.argv),
        AgentRun::Muse(_) => !super::upgrade_muse::is_tui_argv(&snap.argv),
        AgentRun::Agy(_) => !super::upgrade_agy::is_tui_argv(&snap.argv),
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
    }
}

/// Follow a Muse Code's snapshot between the loop's idle points.
pub fn follow_muse(snap: &mut Snapshot) -> bool {
    let muse = match &snap.run {
        AgentRun::Muse(_) => true,
        AgentRun::Claude | AgentRun::Codex(_) | AgentRun::Agy(_) => false,
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
                upgrade::Agent::Muse | upgrade::Agent::Agy => match now.crashed {
                    Some(true) => return ExitRecord::Crashed,
                    Some(false) if now.refused => return ExitRecord::Refused,
                    Some(false) => return ExitRecord::Removed,
                    None if waited >= EXIT_GONE => return ExitRecord::Unread,
                    None => {}
                },
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
    after_exit_as(opts, snap, left, upgrade, cause, None, None, false)
}

/// Admission for a newly planned execution, after ended exits and existing durable plans are resolved.
pub enum ColdAdmission {
    Immediate(Option<Box<Snapshot>>),
    Deferred(Box<super::native_act::ColdIntent>),
}

impl ColdAdmission {
    /// Retain a resolved snapshot without enlarging every deferred admission.
    pub fn immediate(snapshot: Option<Snapshot>) -> Self {
        Self::Immediate(snapshot.map(Box::new))
    }
}

pub type Admission<'a> = dyn Fn(&Snapshot) -> Result<ColdAdmission, String> + 'a;

/// [`after_exit`] with account admission only where a new command will be planned.
/// Existing in-flight commands retain their already-recorded execution context.
#[must_use]
pub fn after_exit_admitting(
    opts: &Opts,
    snap: &Snapshot,
    left: &ExitRecord,
    upgrade: bool,
    asked: Option<super::restart_stamp::Cause>,
    carried: bool,
    admit: &Admission<'_>,
) -> Report {
    let cause = if carried {
        ExitCause::Carried
    } else {
        match asked {
            Some(super::restart_stamp::Cause::Stall) => ExitCause::Stall,
            Some(super::restart_stamp::Cause::Asked) => ExitCause::Asked,
            None => ExitCause::Exit,
        }
    };
    after_exit_as(opts, snap, left, upgrade, cause, None, Some(admit), false)
}

/// [`after_exit`] of an exit an Autopilot carries ([`Carried::Yes`]): relaunched on its
/// conversation as a crash is, whatever the exit left of its agent's own record, and said to the
/// owner when it cannot be made (a `refused:` step where [`after_exit`] says `ended:`).
#[must_use]
pub fn after_carried_exit(
    opts: &Opts,
    snap: &Snapshot,
    left: &ExitRecord,
    upgrade: bool,
) -> Report {
    after_exit_as(
        opts,
        snap,
        left,
        upgrade,
        ExitCause::Carried,
        None,
        None,
        false,
    )
}

/// Whose an exit [`on_exit`] found nobody owns is, under the session's Autopilot ([`carried`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carried {
    /// Not the Autopilot's to carry: decided as any exit is ([`after_exit`]).
    No,
    /// Nobody's: it comes back whatever it left of its agent's own record
    /// ([`after_carried_exit`]), once a healthy run ([`Relaunches::carry`]).
    Yes,
    /// Whose it is could not be read (no `status` at the exit, so neither a person's keys nor a
    /// hand could be seen): decided as any exit is, so an end the crash rule would not bring
    /// back is left, and the host records whose it could not read.
    Unread,
}

/// THE EXIT AN AUTOPILOT CARRIES: whose an exit [`on_exit`] found nobody owns (`decision`
/// [`OnExit::Relaunch`]: no person's key within the grace, no hold, lease or driver's turn, no
/// restart stamp, and, as the host reads it first, no end asked through aterm's `signal` verb)
/// is, under an `autopilot` that carries the session's work (Cruise, Land; Land's only until its
/// work landed, `landed`: the task's face the session's loop last told, which the host keeps
/// whatever the tab publishes once its agent is gone). An agent ends itself in order on a
/// `SIGTERM` no one in aterm sent (Claude Code removes its record, and its shell reads 143), so a
/// graceful end alone says nothing of who wanted it. Such an exit is
///
/// - not the Autopilot's ([`Carried::No`]) under Land once its work landed;
/// - unread when the tab's `status` could not be read, so no person's keys or hand could be seen
///   ([`Carried::Unread`]);
/// - a driver's ([`Carried::No`]) when a control client other than aterm's own typed into the
///   session within the person's grace `grace_s` (`driven_ms`, the age of that typing: a
///   driver's `/exit` is the driver's, as a person's is the person's);
/// - else nobody's: [`Carried::Yes`].
#[must_use]
pub fn carried(
    decision: OnExit,
    autopilot: crate::supervise::Autopilot,
    status: Option<&str>,
    driven_ms: Option<u64>,
    landed: bool,
    grace_s: u32,
) -> Carried {
    use crate::supervise::Autopilot;
    let carries = match autopilot {
        Autopilot::Cruise => true,
        Autopilot::Land => !landed,
        Autopilot::Off => false,
    };
    if decision != OnExit::Relaunch || !carries {
        return Carried::No;
    }
    if !status.is_some_and(crate::is_ok_reply) {
        return Carried::Unread;
    }
    if driven_ms.is_some_and(|ms| ms < u64::from(grace_s).saturating_mul(1000)) {
        Carried::No
    } else {
        Carried::Yes
    }
}

/// The step of an exit whose relaunch cannot be made for `why`: `ended:<why>`, left as its
/// launch's own end and said to nobody, for every cause but [`ExitCause::Carried`], whose exit
/// nobody owns: `refused:<why>`, which the host says to the owner ([`outcome`]'s `Cannot`).
pub(super) fn cannot_step(cause: ExitCause, why: &str) -> String {
    if cause == ExitCause::Carried {
        format!("refused:{why}")
    } else {
        format!("ended:{why}")
    }
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
    /// Its own exit, which nobody owns, under an Autopilot that carries the work
    /// ([`Carried::Yes`]): relaunched as a crash is, whatever it left of its agent's record.
    Carried,
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
        None,
        false,
    )
}

/// Host-ended relaunch with explicit account admission for a new plan.
#[must_use]
pub fn after_host_ended_admitting(
    opts: &Opts,
    snap: &Snapshot,
    upgrade: bool,
    was: Option<&str>,
    admit: &Admission<'_>,
) -> Report {
    after_exit_as(
        opts,
        snap,
        &ExitRecord::Unread,
        upgrade,
        ExitCause::HostEnded,
        was,
        Some(admit),
        false,
    )
}

/// [`after_host_ended_admitting`] of an agent its host knows ended under an earlier aterm
/// process (a tab restored from the layout, after a reboot, a kill or an update's owed
/// relaunch), `asked` the restart stamp its layout carried (its restart or a stall's remedy ended
/// it, so the relaunch is that one's), `admit` its admission where a new plan is made. Every native act of that
/// process died with its epoch, so a record's kept native coordination gates no fresh,
/// admitted relaunch of it ([`host_ended_fresh`]), on this pass or a later one.
#[must_use]
pub fn after_earlier_host_ended(
    opts: &Opts,
    snap: &Snapshot,
    upgrade: bool,
    was: Option<&str>,
    asked: Option<super::restart_stamp::Cause>,
    admit: Option<&Admission<'_>>,
) -> Report {
    let cause = match asked {
        Some(super::restart_stamp::Cause::Stall) => ExitCause::Stall,
        Some(super::restart_stamp::Cause::Asked) => ExitCause::Asked,
        None => ExitCause::HostEnded,
    };
    after_exit_as(
        opts,
        snap,
        &ExitRecord::Unread,
        upgrade,
        cause,
        was,
        admit,
        true,
    )
}

/// The line the crash rule validated for `snap`'s codex-alab ([`CodexRun::self_claimed`]), when its snapshot carries one.
#[must_use]
pub const fn self_claimed_line(snap: &Snapshot) -> Option<&super::self_claimed::Resume> {
    match &snap.run {
        AgentRun::Codex(run) => run.self_claimed.as_ref(),
        AgentRun::Claude | AgentRun::Muse(_) | AgentRun::Agy(_) => None,
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

#[allow(clippy::too_many_arguments)]
fn after_exit_as(
    opts: &Opts,
    snap: &Snapshot,
    left: &ExitRecord,
    upgrade: bool,
    cause: ExitCause,
    was: Option<&str>,
    admit: Option<&Admission<'_>>,
    earlier: bool,
) -> Report {
    match &snap.run {
        AgentRun::Codex(_) => {
            if !matches!(cause, ExitCause::Exit | ExitCause::Carried)
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
        return said(r, cannot_step(cause, "no-conversation"));
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
    let prior = match load_record(opts, &session) {
        Ok(record) => record,
        Err(_) => {
            return said(
                r,
                format!("wait:{}", super::upgrade_drive::UNREAD_SAVED_RECORD),
            );
        }
    };
    // An agent that ended under an earlier aterm process is that host's end whatever stamp its
    // layout carried: a restart that process had in flight is carried below, by identity. A
    // stamped one whose restart already failed keeps that refusal, as it did under its host.
    let host_ended = cause == ExitCause::HostEnded || earlier;
    if let Some(st) = prior
        .clone()
        .filter(|st| ended_by_restart(st, upgrade::Agent::Claude, &snap.tab, snap.pid, now_s()))
        && (!host_ended
            || (cause != ExitCause::HostEnded && matches!(st.phase, upgrade::Phase::Failed(_))))
    {
        r.to = format!("{}({})", st.to, st.source);
        return carry_own(opts, r, st, &session, &snap.tab);
    }
    let mut relaunched_before = false;
    if let Some(mut st) = prior.filter(St::in_flight) {
        if st.native_retired_unissued {
            return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
        }
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != snap.tab && !host_ended {
            return said(r, "wait:conversation-in-other-tab");
        }
        // A record an earlier build wrote names its agent by pid alone: it takes the start the
        // snapshot of that pid carries (the layout's, after a restore), as RS1 carries an
        // earlier host's restart by identity.
        st.adopt_start(snap.pid, &snap.start);
        let overtaken = if host_ended && st.tab != snap.tab {
            Some((
                "host-ended",
                "aterm ended while this relaunch was in flight; its tab was reopened".to_string(),
            ))
        } else {
            (!st.names(snap.pid, &snap.start) || st.relaunched_gone(alive, kernel_start)).then(
                || {
                    match st.phase {
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
            }
                },
            )
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
            if in_flight_exit_proven(&st, alive(st.pid), kernel_start(st.pid).as_deref()) {
                // A restart in flight when aterm ended (a stall's or an asked one) waits on
                // its agent's exit: proven here by its kernel start, never by a pid that may
                // name another process since (a reboot reuses them).
                st.exited_at = now_s();
            }
            if host_ended && snap.shell != NO_SHELL && alive(snap.shell) {
                // Aterm ended: the restart's line goes to the shell the restored tab holds
                // now, never to the pid its earlier shell had.
                st.shell = snap.shell;
            }
            // Recomputed on every pass: the record keeps the mark, never this exemption.
            st.host_ended_fresh = host_ended_fresh_now(opts, earlier, false, &st);
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
    let (admitted, mut cold) = match admit {
        Some(admit) => match admit(snap) {
            Ok(ColdAdmission::Immediate(chosen)) => (chosen, None),
            Ok(ColdAdmission::Deferred(intent)) => (None, Some(*intent)),
            Err(why) => {
                aterm_log::info!("harness @{}: live account admission waits: {why}", snap.tab);
                return said(r, "wait:account");
            }
        },
        None => (None, None),
    };
    let snap = admitted.as_deref().unwrap_or(snap);
    let running = version.as_deref().and_then(Version::parse);
    let candidates = if upgrade {
        let native = native_root(&opts.home);
        Targets::read(opts).for_session(snap.program.starts_with(&native))
    } else {
        Vec::new()
    };
    let prior = match load_record(opts, &session) {
        Ok(record) => record,
        Err(_) => {
            return said(
                r,
                format!("wait:{}", super::upgrade_drive::UNREAD_SAVED_RECORD),
            );
        }
    };
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
        route: snap.route,
        auth_observed: snap.auth_observed,
        launch_env: snap.launch_env.as_ref(),
        billing: if riding.is_some() {
            ModelBilling::Move(snap.profile.as_ref())
        } else {
            ModelBilling::Keep
        },
        serve: Serve::Judge,
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
            (true, ExitCause::Exit | ExitCause::Carried) => CAUSE_EXIT,
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
    if let Some(intent) = &mut cold {
        intent.billing = match launch.billing {
            ModelBilling::Keep => super::native_act::ColdBilling::Keep,
            ModelBilling::Move(_) if opts.usage_credits => {
                super::native_act::ColdBilling::UsageCredits
            }
            ModelBilling::Move(_) => super::native_act::ColdBilling::PreserveProfile,
        };
    }
    let planned =
        home_carried(&opts.home, snap.home.as_deref(), &cwd, &records).and_then(|homedir| {
            let launch = Launch {
                homedir: homedir.as_deref(),
                ..launch
            };
            plan_with_cold(opts, &launch, snap.shell, &table(), cold)
        });
    let plan = match planned {
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
    launch_after_exit(opts, snap, cause, earlier, r, st, plan, c)
}

/// The real exit lane's already-selected immutable plan, current connection and state. The
/// caller has performed record/target/roster selection; the held relaunch retains every
/// person, shell and process-exit guard before any legacy or typed continuation.
#[allow(clippy::too_many_arguments)]
fn launch_after_exit(
    opts: &Opts,
    snap: &Snapshot,
    cause: ExitCause,
    earlier: bool,
    r: Report,
    mut st: St,
    plan: Plan,
    mut c: Client,
) -> Report {
    let Plan {
        shell,
        line,
        tap_dropped,
        token_dropped,
        line_dir,
        command,
        cold,
        ..
    } = plan;
    let session = r.session.clone();
    // The agent this relaunch waits on, by its pid and kernel start: a prior record's exit
    // stamp, kept by a restart that carried it, spoke of another agent and goes ([`St::bind`]).
    st.bind(snap.pid, &snap.start);
    st.shell = shell;
    st.tab.clone_from(&snap.tab);
    st.line = line;
    st.account_intent = command
        .as_ref()
        .and_then(|command| command.account_intent.clone());
    st.line_dir = line_dir.unwrap_or_default();
    st.phase = upgrade::Phase::Exiting { at_s: now_s() };
    if st.exited_at == 0 && st.agent_exit() == AgentExit::Gone {
        // The process that ran as `snap.pid` is gone, by its kernel start: a live pid here
        // is another process (pids are reused, as after a reboot), never a guard to wait on.
        st.exited_at = now_s();
    }
    st.host_ended_fresh = host_ended_fresh_now(opts, earlier, cold.is_some(), &st);
    if !st.host_ended_fresh && (opts.token.is_some() || super::native_restart::coordinated(&st)) {
        match super::native_restart::begin_crash(
            opts,
            &mut c,
            &mut st,
            &session,
            snap,
            command.as_ref(),
            cold.as_ref(),
        ) {
            Ok(super::native_restart::Recovery::Admitted) => {}
            Ok(super::native_restart::Recovery::Completed) => return said(r, "wait:relaunched"),
            Ok(super::native_restart::Recovery::Retired) => {
                return said(r, "failed:native-cohort-retired");
            }
            Ok(super::native_restart::Recovery::Held) => {
                return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
            }
            Ok(super::native_restart::Recovery::Legacy) => {}
            Ok(super::native_restart::Recovery::Cancelled) => {
                return said(r, "wait:native-crash-observation");
            }
            Err(why) => return said(r, format!("wait:{why}")),
        }
    }
    save(opts, &session, &st);
    consume_stamp(opts, snap, cause);
    ledger(opts, &said(r.clone(), "exited"), &st.line);
    note_dropped(opts, &r, tap_dropped.as_deref(), token_dropped.as_deref());
    let r = relaunch(opts, r, &mut st, &mut c, &session, &Live);
    save(opts, &session, &st);
    r
}

/// Whether the in-flight restart `st` was waiting on its agent's exit and that agent is proven
/// gone by the identity the record names ([`St::pid_start`], adopted from the snapshot of its
/// pid where an earlier build recorded none): its pid `alive`, born `start_now`, as
/// [`agent_exit`] judges. Only an Exiting record not yet stamped.
pub(super) fn in_flight_exit_proven(st: &St, alive: bool, start_now: Option<&str>) -> bool {
    matches!(st.phase, upgrade::Phase::Exiting { .. })
        && st.exited_at == 0
        && agent_exit(&st.pid_start, alive, start_now) == AgentExit::Gone
}

/// Whether an exit's relaunch is a typed plan no native authority gates
/// ([`St::host_ended_fresh`]): its agent ended under an earlier aterm process (`earlier`, as
/// its host knows it: a tab restored from the layout, never a stranded or rolled-back relaunch
/// of this one), its record coordinated (an uncoordinated one takes its gates as before), the
/// plan carries no deferred cold intent (it was admitted now), its exit is proven
/// ([`St::exited_at`], by [`agent_exit`]), no native operation or rollback is left
/// unresolved (one is reconciled or held as it is, never replaced by a line), and the record
/// holds no native act of this process (`own`, [`native_of_this_host`]). Every native act of
/// the ended process died with its epoch, so the record's kept coordination is no grant to
/// wait on.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Own",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Earlier",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Coordinated",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Deferred",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Unresolved",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HostEndedRelaunch",
        action = "Decide",
        project = "aterm_agent::harness::relaunch::native_gate_tests::host_ended_transition"
    )
)]
pub(super) fn host_ended_fresh(earlier: bool, deferred: bool, own: bool, st: &St) -> bool {
    earlier
        && super::native_restart::coordinated(st)
        && !deferred
        && st.exited_at != 0
        && st.native.is_none()
        && st.native_prepare.is_empty()
        && !st.native_retired_unissued
        && st.native_account_candidate.is_none()
        && !st.native_rollback
        && !own
}

/// [`host_ended_fresh`] of `st` now, asking its host whether a native act the record holds
/// is this process's only where nothing else already refuses it.
fn host_ended_fresh_now(opts: &Opts, earlier: bool, deferred: bool, st: &St) -> bool {
    let own = host_ended_fresh(earlier, deferred, false, st)
        && native_of_this_host(st, || {
            let mut c = connect(opts, &st.tab).ok()?;
            match super::native_act::exchange(&mut c, &st.tab, &super::native_act::Request::Info) {
                Ok(super::native_act::Reply::Info { epoch, .. }) => Some(epoch),
                _ => None,
            }
        });
    host_ended_fresh(earlier, deferred, own, st)
}

/// Whether `st` holds a native act of this aterm process: a completed preparation or a death
/// witness stamped with the epoch its host reports now (`epoch`, read only when one is
/// stamped). One whose host's epoch cannot be read counts as this process's, so its gates
/// stay.
pub(super) fn native_of_this_host(st: &St, epoch: impl FnOnce() -> Option<String>) -> bool {
    let stamped: Vec<&str> = st
        .native_completed_prepare
        .iter()
        .map(|p| p.host_epoch.as_str())
        .chain(
            st.native_observation
                .iter()
                .map(|o| o.registration.host_epoch.as_str()),
        )
        .collect();
    !stamped.is_empty() && epoch().is_none_or(|now| stamped.contains(&now.as_str()))
}

/// What the kernel says of the agent a record names by its pid and the kernel start it was
/// born at ([`agent_exit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentExit {
    /// Proven gone: no process holds its pid, or one born at another time does (a reused pid,
    /// as after a reboot).
    Gone,
    /// The process born at its start still holds its pid: the one exit a relaunch waits on.
    Runs,
    /// A live pid the record cannot name: it recorded no start (an earlier build's record no
    /// snapshot gave one), or the live process's start cannot be read. Never waited on by its
    /// pid alone, and never proven gone.
    Unproven,
}

/// The step word of a relaunch whose agent's exit is [`AgentExit::Unproven`]: it neither waits
/// on the pid nor types a line over a process that may be its agent.
pub(super) const WAIT_EXIT_UNPROVEN: &str = "wait:exit-unproven";

/// What the kernel's answer for a record's pid now proves of the agent it names, born at
/// `start` (`kernel_start`'s spelling; empty when the record recorded none): `alive`, and the
/// live process's start `now` (`None`: unreadable). A free pid proves the exit whatever the
/// record knows (a process keeps its pid until it is reaped); a live one proves it only by
/// another start, and holds the relaunch only by the same one.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Recorded",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Snapshot",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Same",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Reborn",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HostEndedRelaunch",
        action = "Unread",
        reason = "Select one input of the relaunch's fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HostEndedRelaunch",
        action = "Judge",
        project = "aterm_agent::harness::relaunch::native_gate_tests::host_ended_transition"
    )
)]
pub(super) fn agent_exit(start: &str, alive: bool, now: Option<&str>) -> AgentExit {
    if !alive {
        return AgentExit::Gone;
    }
    match now {
        _ if start.is_empty() => AgentExit::Unproven,
        Some(now) if now == start => AgentExit::Runs,
        Some(_) => AgentExit::Gone,
        None => AgentExit::Unproven,
    }
}

/// The step a relaunch answers while the agent its record names is not proven gone
/// ([`AgentExit`], read by `exit`), or `None` once it is: an unproven one is never waited on,
/// and a running one is waited on within `wait` (`wait_until` over the lane's limit), which only
/// a proof ends. A start unread mid-wait (a slow kernel read) is no exit: the agent may run on.
pub(super) fn exit_wait(
    exit: impl Fn() -> AgentExit,
    wait: impl FnOnce(&mut dyn FnMut() -> bool) -> bool,
) -> Option<&'static str> {
    match exit() {
        AgentExit::Gone => None,
        AgentExit::Unproven => Some(WAIT_EXIT_UNPROVEN),
        AgentExit::Runs => {
            if wait(&mut || exit() == AgentExit::Gone) {
                return None;
            }
            match exit() {
                AgentExit::Gone => None,
                AgentExit::Runs => Some("wait:exiting"),
                AgentExit::Unproven => Some(WAIT_EXIT_UNPROVEN),
            }
        }
    }
}

/// The fallback origin an exit's relaunch of `session` records ([`fallback_carried`] of the one its last record keeps).
pub(super) fn exit_fallback(
    opts: &Opts,
    session: &str,
    riding: Option<&str>,
    hand: Option<&str>,
) -> Option<String> {
    let origin = load_record(opts, session)
        .ok()
        .flatten()
        .as_ref()
        .and_then(fallback_origin);
    fallback_carried(origin, riding, hand)
}

fn carry_in_flight(opts: &Opts, r: Report, mut st: St, session: &str, tab: &str) -> Report {
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    if opts.dry_run {
        return said(r, format!("would-resume:{}", st.phase.word()));
    }
    let Ok(mut c) = connect(opts, tab) else {
        return said(r, "wait:no-socket");
    };
    if let Some(r) = close_if_taken(opts, &r, &mut st, session, &mut c, tab) {
        return r;
    }
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
        }
}

fn restart_record(opts: &Opts, agent: upgrade::Agent, pid: Option<u32>) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let pid = pid?;
    let now = now_s();
    record_where(opts, |st| ended_by_restart(st, agent, tab, pid, now))
}

fn record_where(opts: &Opts, ours: impl Fn(&St) -> bool) -> Option<(String, St)> {
    let dir = read_upgrade_dir(&opts.state).ok()?;
    dir.flatten().find_map(|e| {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            return None;
        }
        let session = path.file_stem()?.to_string_lossy().into_owned();
        let st = load_record(opts, &session).ok()??;
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

/// How long a person's keys hold the relaunch line of the restart [`restarted`] finds for the
/// agent `pid` of tab `opts.only_sid`: its record's own ([`line_grace`]), or the whole grace when
/// no record is found.
#[must_use]
pub fn carry_line_grace(opts: &Opts, agent: upgrade::Agent, pid: Option<u32>) -> u32 {
    restart_record(opts, agent, pid).map_or(opts.human_grace_s, |(_, st)| {
        line_grace(opts.human_grace_s, end_asked_at(&st), st.line_grace_s)
    })
}

/// Whether the agent `pid`'s own background work holds its restart in place: a shell of its job
/// still runs, or the process table reads empty, so none can be ruled out. [`restart_as`]'s
/// signal refuses the restart then (`wait:changed`), and an account move asks this first, so it
/// takes no row from another session for a relaunch this would refuse.
#[must_use]
pub fn background_holds(pid: u32) -> bool {
    !live_background(pid).is_empty()
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
    if stamped && !opts.dry_run {
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
    let raised = load_record(opts, session).ok().flatten().is_some_and(|st| {
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
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
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
    if carried_here(st.agent) && st.relaunched_gone(alive, kernel_start) {
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
    let record = load_record(opts, session).ok().flatten().is_some_and(|st| {
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
    let st = load_record(opts, session)
        .ok()
        .flatten()
        .filter(|st| st.in_flight() && st.tab == tab)?;
    (st.relaunched_pid != 0).then(|| st.relaunched_gone(alive, kernel_start))
}

/// Whether a relaunch of conversation `session` is in flight in tab `tab` ([`St::in_flight`]).
#[must_use]
pub fn in_flight_here(opts: &Opts, tab: &str, session: &str) -> bool {
    load_record(opts, session)
        .ok()
        .flatten()
        .is_some_and(|st| st.in_flight() && st.tab == tab)
}

const fn carried_here(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex | upgrade::Agent::Muse | upgrade::Agent::Agy => false,
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

/// Whether a relaunch's carry-on still stands to be typed in tab `opts.only_sid`, `only`'s record
/// alone where it names an agent: its relaunch is in flight and speaks. Narrower than [`owed_of`],
/// whose confirmation looks come after the carry-on and hold nothing of the loop's.
#[must_use]
pub fn carry_owed_of(opts: &Opts, only: Option<upgrade::Agent>) -> bool {
    let Some(tab) = opts.only_sid.as_deref() else {
        return false;
    };
    record_where(opts, |st| {
        st.tab == tab
            && !st.native_retired_unissued
            && matches!(st.phase, upgrade::Phase::Relaunched { .. })
            && !resumes_quietly(st)
            && only.is_none_or(|agent| st.agent == agent)
    })
    .is_some()
}

/// Whether a carry-on's `step` waits on what a person or their switch holds (a draft or a box in
/// the composer, their grace, Off): the loop holds for the same, so the hold that keeps its act
/// from overtaking the carry-on runs on from this look.
#[must_use]
pub fn carry_stands(step: &str) -> bool {
    matches!(step, "wait:draft" | "wait:box" | "wait:held" | "wait:off")
}

fn owed_record(opts: &Opts, only: Option<upgrade::Agent>) -> Option<(String, St)> {
    let tab = opts.only_sid.as_deref()?;
    let dir = read_upgrade_dir(&opts.state).ok()?;
    dir.flatten().find_map(|e| {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            return None;
        }
        let session = path.file_stem()?.to_string_lossy().into_owned();
        let st = load_record(opts, &session).ok()??;
        if st.native_retired_unissued {
            return None;
        }
        // A look is its resumed agent's: once that agent is gone, whatever runs next in the tab
        // is owed nothing of it, and a sweep's confirmations close it ([`restore_look`]).
        let due = matches!(st.phase, upgrade::Phase::Relaunched { .. })
            || (st.confirming() && st.resumed_pid != 0 && alive(st.resumed_pid));
        let its = only.is_none_or(|agent| st.agent == agent);
        (st.tab == tab && due && its).then_some((session, st))
    })
}

/// Whether the foreground agent `snap` resumes a conversation: its own arguments resume one,
/// whoever ran it, or aterm's relaunch resumed it ([`agent_resumed`], read with `opts`). An agy's
/// own arguments prove no resume where the conversation it holds now is another than the one they
/// name (a `/clear` since), or where a relaunch record of its tab, read with `opts`, failed that
/// very process [`STARTED_FRESH`](super::upgrade_drive::STARTED_FRESH): aterm's own relaunch line
/// names the conversation it asked for, whatever the agy opened.
#[must_use]
pub fn resumes_conversation(opts: Option<&Opts>, snap: &Snapshot) -> bool {
    resumes_conversation_with(opts, snap, &super::upgrade_drive::agy_conversation_held)
}

/// [`resumes_conversation`], reading the conversation an agy holds now through `agy_held`, at most
/// once ([`agent_resumed_with`]).
pub(in crate::harness) fn resumes_conversation_with(
    opts: Option<&Opts>,
    snap: &Snapshot,
    agy_held: &dyn Fn(&Path, u32) -> Option<Option<String>>,
) -> bool {
    let read = std::cell::OnceCell::new();
    let held = |config: &Path, pid: u32| read.get_or_init(|| agy_held(config, pid)).clone();
    (launch_resumes(snap, &held) && !opts.is_some_and(|o| relaunch_found_fresh(o, snap)))
        || opts.is_some_and(|o| agent_resumed_with(o, snap, &held))
}

fn launch_resumes(
    snap: &Snapshot,
    agy_held: &dyn Fn(&Path, u32) -> Option<Option<String>>,
) -> bool {
    match snap.agent() {
        upgrade::Agent::Claude => upgrade::launch_resume(&snap.argv).is_some(),
        upgrade::Agent::Codex => super::codex_alab::resumed_thread(&snap.argv).is_some(),
        upgrade::Agent::Muse => super::upgrade_muse::resumes(&snap.argv),
        upgrade::Agent::Agy => {
            super::upgrade_agy::resumes(&snap.argv)
                && super::upgrade_agy::resumed_conversation(&snap.argv).is_none_or(|named| {
                    snap.agy()
                        .and_then(|run| agy_held(&run.config, snap.pid))
                        .unwrap_or_else(|| snap.session.clone())
                        .is_none_or(|cur| cur == named)
                })
        }
    }
}

/// Whether a relaunch record of `snap`'s tab failed `snap`'s own process
/// [`STARTED_FRESH`](super::upgrade_drive::STARTED_FRESH) (its relaunched pid, of the record's
/// start where both are read). A record that cannot be read proves nothing.
fn relaunch_found_fresh(opts: &Opts, snap: &Snapshot) -> bool {
    if snap.pid == 0 {
        return false;
    }
    let Ok(dir) = read_upgrade_dir(&opts.state) else {
        return false;
    };
    let snap_start = squash(&snap.start);
    dir.flatten().any(|e| {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            return false;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            return false;
        };
        let Ok(Some(st)) = load_record(opts, &session) else {
            return false;
        };
        st.tab == snap.tab
            && st.agent == snap.agent()
            && st.relaunched_pid == snap.pid
            && matches!(&st.phase, upgrade::Phase::Failed(why)
                if why == super::upgrade_drive::STARTED_FRESH)
            && (st.relaunched_start.is_empty()
                || snap_start.is_empty()
                || snap_start == squash(&st.relaunched_start))
    })
}

/// Whether `snap` is a foreground agent resuming the conversation of one of tab `snap.tab`'s
/// relaunch records (and `opts.only_sid` when set), rather than a program started afresh: its argv
/// resumes the record's conversation, which the conversation it holds, where read, does not
/// contradict, and it is the record's own process, started no earlier than the record's line
/// ([`St::relaunched_at`]). That is the record's relaunched or resumed process (of the record's
/// start where it has one, else started by [`St::done_at`] where that is set), or any process
/// while the record is [`Relaunched`](upgrade::Phase::Relaunched) and claims neither: a person's
/// own resume counts where the record names it (`upgrade_drive::resumed_by_another`) or came
/// first. No record counts that starts afresh (`starts_afresh`), has no line, was failed
/// [`STARTED_FRESH`](super::upgrade_drive::STARTED_FRESH), or names `snap` as the process that
/// exited (save a cold restore's pid that a process started since its line reused); nor, for an
/// agy, one whose conversation it holds now is another ([`agent_resumed_with`]).
#[must_use]
pub fn agent_resumed(opts: &Opts, snap: &Snapshot) -> bool {
    agent_resumed_with(opts, snap, &super::upgrade_drive::agy_conversation_held)
}

/// [`agent_resumed`], reading the conversation an agy holds now through `agy_held` (`None`: its
/// files are unread, and the snapshot's own conversation stands; `Some(None)`: no one
/// conversation, as before its lock is taken).
pub(in crate::harness) fn agent_resumed_with(
    opts: &Opts,
    snap: &Snapshot,
    agy_held: &dyn Fn(&Path, u32) -> Option<Option<String>>,
) -> bool {
    if snap.pid == 0 {
        return false;
    }
    let tab = opts.only_sid.as_deref().unwrap_or(&snap.tab);
    if snap.tab != tab {
        return false;
    }
    let snap_start = squash(&snap.start);
    let snap_began = super::upgrade_models::parse_lstart(&snap_start);
    let Ok(dir) = read_upgrade_dir(&opts.state) else {
        return false;
    };
    dir.flatten().any(|e| {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            return false;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            return false;
        };
        let Ok(Some(st)) = load_record(opts, &session) else {
            return false;
        };
        record_resumed_snapshot(snap, &snap_start, snap_began, &session, &st, agy_held)
    })
}

fn record_resumed_snapshot(
    snap: &Snapshot,
    snap_start: &str,
    snap_began: Option<u64>,
    session: &str,
    st: &St,
    agy_held: &dyn Fn(&Path, u32) -> Option<Option<String>>,
) -> bool {
    if st.tab != snap.tab
        || st.agent != snap.agent()
        || starts_afresh(&st.cause)
        || st.relaunched_at == 0
        || matches!(&st.phase, upgrade::Phase::Failed(why)
            if why == super::upgrade_drive::STARTED_FRESH)
    {
        return false;
    }
    if snap.pid == st.pid
        && !(st.cause == CAUSE_HOST
            && !snap_start.is_empty()
            && snap_start != squash(&st.proc_start)
            && snap_began.is_some_and(|began| began.saturating_add(2) >= st.relaunched_at))
    {
        return false;
    }
    snap_resumes_record(snap, session, st, agy_held)
        && snap_matches_relaunch(snap.pid, snap_start, snap_began, st)
}

fn snap_resumes_record(
    snap: &Snapshot,
    session: &str,
    st: &St,
    agy_held: &dyn Fn(&Path, u32) -> Option<Option<String>>,
) -> bool {
    match snap.agent() {
        upgrade::Agent::Claude => {
            upgrade::launch_resume(&snap.argv).flatten().as_deref() == Some(session)
                && snap.session.as_deref().is_none_or(|cur| cur == session)
        }
        upgrade::Agent::Codex => {
            !st.thread.is_empty()
                && (super::codex_alab::resumed_thread(&snap.argv).as_deref()
                    == Some(st.thread.as_str())
                    || (!st.resume_argv.is_empty() && snap.argv == st.resume_argv))
                && snap.session.as_deref().is_none_or(|cur| cur == st.thread)
        }
        upgrade::Agent::Muse => {
            !st.thread.is_empty()
                && super::upgrade_muse::resumed_ref(&snap.argv).as_deref()
                    == Some(st.thread.as_str())
                && snap.session.as_deref().is_none_or(|cur| cur == st.thread)
        }
        upgrade::Agent::Agy => {
            !st.thread.is_empty()
                && super::upgrade_agy::resumed_conversation(&snap.argv).as_deref()
                    == Some(st.thread.as_str())
                && snap
                    .agy()
                    .and_then(|run| agy_held(&run.config, snap.pid))
                    .unwrap_or_else(|| snap.session.clone())
                    .as_deref()
                    .is_none_or(|cur| cur == st.thread)
        }
    }
}

fn snap_matches_relaunch(pid: u32, snap_start: &str, snap_began: Option<u64>, st: &St) -> bool {
    if snap_began.is_some_and(|began| began.saturating_add(2) < st.relaunched_at) {
        return false;
    }
    if st.relaunched_pid == pid || st.resumed_pid == pid {
        if !st.relaunched_start.is_empty() && !snap_start.is_empty() {
            snap_start == squash(&st.relaunched_start)
        } else if st.done_at != 0 {
            snap_began.is_none_or(|began| began <= st.done_at.saturating_add(5))
        } else {
            true
        }
    } else {
        st.relaunched_pid == 0
            && st.resumed_pid == 0
            && matches!(st.phase, upgrade::Phase::Relaunched { .. })
    }
}

/// Carry on the relaunched agent at its loop's idle point, where the loop parked for an [`owed`] continuation.
#[must_use]
pub fn resume(opts: &Opts) -> Report {
    resume_as(opts, false, None)
}

/// [`resume`] at a point the loop read as a task landed under land (`done_under_land`, `crate::supervise::IdlePoint::done_under_land`).
#[must_use]
pub fn resume_as(opts: &Opts, done_under_land: bool, only: Option<upgrade::Agent>) -> Report {
    resume_adopting(opts, done_under_land.then_some(END_LANDED), only).0
}

/// [`resume`] at a point where the stop for replies that repeat with no work holds the loop's
/// typing (`crate::supervise::IdlePoint::replies_held`): the relaunch is settled with nothing typed.
#[must_use]
pub fn resume_held(opts: &Opts, only: Option<upgrade::Agent>) -> Report {
    resume_adopting(opts, Some(END_HELD), only).0
}

/// What a relaunch settles on with nothing typed: its step's word and why ([`carried_done`]).
type Ending = (&'static str, &'static str);

const END_LANDED: Ending = (
    CARRIED_DONE,
    "nothing typed: under Land, its task concluded",
);

const END_HELD: Ending = (
    CARRIED_HELD,
    "nothing typed: Cruise stopped for replies that repeat with no work",
);

/// [`resume_as`], and the kind of the row owed to an agent it adopted that no step had adopted
/// ([`unadopted`]): `upgrade` for an upgrade, else `carry-on`; `None` when it adopted no such
/// agent. Under an `ended` the report is then that [`Ending`]'s, the adoption under it.
fn resume_adopting(
    opts: &Opts,
    ended: Option<Ending>,
    only: Option<upgrade::Agent>,
) -> (Report, Option<&'static str>) {
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
        Err(why) => return (said(r, format!("busy:{why}")), None),
    };
    let Some((session, mut st)) = owed_record(opts, only) else {
        return (said(r, "current"), None);
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
            let looked = restore_look(
                opts,
                &r,
                &mut st,
                &session,
                now_s(),
                Some(&tab),
                |c, pid, tab| process_in_tab(c, pid, tab, None, None),
            )
            .unwrap_or_else(|| said(r, "wait:restore"));
            return (looked, None);
        }
        let confirmed =
            confirm(opts, &r, &mut st, &session, now_s()).unwrap_or_else(|| said(r, "wait:model"));
        return (confirmed, None);
    }
    let owed = unadopted(&st).then_some(if st.is_upgrade() {
        "upgrade"
    } else {
        "carry-on"
    });
    let carry = at_point(opts, &st, ended.is_some());
    let r = match st.agent {
        upgrade::Agent::Codex => {
            super::upgrade_drive::codex_carry_in_flight(&carry, r, st, &session)
        }
        upgrade::Agent::Claude => carry_in_flight(&carry, r, st, &session, &tab),
        upgrade::Agent::Muse => super::upgrade_drive::muse_carry_in_flight(&carry, r, st, &session),
        upgrade::Agent::Agy => super::upgrade_drive::agy_carry_in_flight(&carry, r, st, &session),
    };
    let adopted = owed.filter(|_| r.step == "adopted");
    if let Some(end) = ended
        && r.step == "adopted"
    {
        return (carried_done(opts, r, &session, end), adopted);
    }
    (r, adopted)
}

/// The options [`resume_as`] carries `st` on with at the tab's loop's idle point. It hands the
/// agent back (adopts it and types nothing) where the relaunch `ends` with nothing typed (a task
/// done under Land, the stop), and for a relaunch no step adopted ([`unadopted`]): its own
/// step (`wait:resume`, `wait:unanswered`, …) ended before the agent registered the conversation,
/// so this idle point is the first step to see that agent, and it adopts it as that step would
/// have ([`resume_at_point`] carries on at once).
pub(in crate::harness) fn at_point(opts: &Opts, st: &St, ends: bool) -> Opts {
    Opts {
        hand_back: ends || unadopted(st),
        background: false,
        ..opts.clone()
    }
}

/// Whether `st` is a relaunch in flight of a lane carried here ([`carried_here`]: Claude Code's)
/// whose agent no step adopted yet: its line was typed ([`typed_recorded`] zeroes
/// `relaunched_pid`), only an adoption sets it ([`await_held`]), and the step that typed it ended
/// before the agent registered the conversation.
fn unadopted(st: &St) -> bool {
    carried_here(st.agent)
        && matches!(st.phase, upgrade::Phase::Relaunched { .. })
        && st.relaunched_pid == 0
}

/// [`resume_as`] at the tab's loop's idle point, each act's row said by `row` under its kind. A
/// relaunch it adopts there ([`unadopted`]) gets the row its own step would have written (kind
/// `upgrade` for an upgrade, else `carry-on`): an `adopted` row, which lifts the failure the
/// relaunch followed and labels the restart. It is then carried on at once, at the same point,
/// under kind `carry-on`, whose step is the one returned; at a task done under Land, that carry-on
/// is the adoption's own [`CARRIED_DONE`], said after it.
pub fn resume_at_point(
    opts: &Opts,
    done_under_land: bool,
    only: Option<upgrade::Agent>,
    row: impl FnMut(&str, &Report),
) -> Report {
    resume_ending_at_point(opts, done_under_land.then_some(END_LANDED), only, row)
}

/// [`resume_at_point`] where the stop holds the loop's typing ([`resume_held`]).
pub fn resume_held_at_point(
    opts: &Opts,
    only: Option<upgrade::Agent>,
    row: impl FnMut(&str, &Report),
) -> Report {
    resume_ending_at_point(opts, Some(END_HELD), only, row)
}

fn resume_ending_at_point(
    opts: &Opts,
    ended: Option<Ending>,
    only: Option<upgrade::Agent>,
    mut row: impl FnMut(&str, &Report),
) -> Report {
    let (r, adopted) = resume_adopting(opts, ended, only);
    let Some(kind) = adopted else {
        if r.is_act() {
            row("carry-on", &r);
        }
        return r;
    };
    row(kind, &said(r.clone(), "adopted"));
    let r = if ended.is_some() {
        r
    } else {
        resume_as(opts, false, only)
    };
    if r.is_act() && r.step != "adopted" {
        row("carry-on", &r);
    }
    r
}

/// The word of a relaunch carried on with nothing because its task landed under Land ([`resume_as`]).
pub const CARRIED_DONE: &str = "done:said-done";

/// The word of a relaunch carried on with nothing because the stop holds ([`resume_held`]).
pub const CARRIED_HELD: &str = "done:replies-held";

fn carried_done(opts: &Opts, r: Report, session: &str, (word, why): (&str, &str)) -> Report {
    let record = match load_record(opts, session) {
        Ok(record) => record,
        Err(_) => {
            return said(
                r,
                format!("wait:{}", super::upgrade_drive::UNREAD_SAVED_RECORD),
            );
        }
    };
    let Some(mut st) = record.filter(|st| matches!(st.phase, upgrade::Phase::Relaunched { .. }))
    else {
        return r;
    };
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    st.phase = upgrade::Phase::Done;
    st.done_at = now_s();
    st.outcome = nothing_typed(st.agent, &st.to, why);
    let r = said(r, word);
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
        let ours = match conversation_typed_by(opts, &tab, &session, &st) {
            Ok(ours) => ours,
            Err(_) => return said(r, WAIT_LEDGER_EVIDENCE),
        };
        let written = WhereWritten::new(opts, &records, &session, &ours);
        let (word, why) = match nothing_owed(opts, &written, &tab, &st) {
            Ok(Some(nothing)) => nothing,
            Ok(None) => return said(r, "wait:owed"),
            Err(_) => return said(r, WAIT_LEDGER_EVIDENCE),
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

pub(super) fn settle_look(
    opts: &Opts,
    r: &Report,
    st: &mut St,
    key: &str,
    now: u64,
) -> Option<Report> {
    if st.native_retired_unissued {
        return Some(said(
            r.clone(),
            super::native_restart::WAIT_RETIRED_UNISSUED,
        ));
    }
    let records = st.agent_records(&opts.home);
    if alive(st.resumed_pid) && look_warned(&records, key, st) {
        return Some(match told_nothing(opts, &records, key, st) {
            Ok(Some(why)) => restore_lost(opts, r, st, key, why),
            Ok(None) => said(r.clone(), "wait:owed"),
            Err(_) => said(r.clone(), WAIT_LEDGER_EVIDENCE),
        });
    }
    restore_look(opts, r, st, key, now, None, |_, _, _| false)
}

/// The host's word for an exit the explicit restart's stamp names whose process still runs.
pub const WAIT_ACT_ENDS: &str = "wait:not-exited:act-ends";

/// The word for an exit the harness made, carried through a hand that waits.
pub const WAIT_HAND_LIFTS: &str = "wait:held:act-carried";

/// The word for a relaunch line something at its tab holds: a person's keys, input unread, a hand
/// ([`line_held_by_status`]).
pub const WAIT_HELD: &str = "wait:held";

/// What one relaunch step came to, for the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The new process holds the conversation.
    Relaunched,
    /// Nothing to relaunch or say, and why.
    Left(String),
    /// Not now, and why (a busy tab, a shell not back yet, a relaunch that did not register).
    NotYet(String),
    /// Account admission, a hand or another aterm actor holds the attempt before launch.
    Busy,
    /// Never, and why: an interactive conversation that cannot come back (an uncarryable launch, a gone shell).
    Cannot(String),
}

/// [`Outcome`] from one [`after_exit`] step's word.
#[must_use]
pub fn outcome(step: &str) -> Outcome {
    // The host hands every relaunched agent to its loop ([`Opts::hand_back`]), so no other word
    // lands, and a carry-on that typed nothing (`done:no-continue`) is never one.
    if matches!(
        step,
        "adopted"
            | "done:fresh"
            | "landed"
            | "done:resumed"
            | super::upgrade_drive::RESUMED_BY_ANOTHER
    ) {
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
    } else if matches!(
        step,
        "busy:another-sweep"
            | WAIT_ACT_ENDS
            | WAIT_HAND_LIFTS
            | WAIT_ACCOUNT
            | WAIT_NATIVE_UNISSUED
            | super::native_restart::WAIT_RETIRED_UNISSUED
    ) {
        Outcome::Busy
    } else if let Some(why) = step.strip_prefix("busy:") {
        // State directory cannot hold the lock.
        Outcome::Cannot(why.to_string())
    } else {
        Outcome::NotYet(step.to_string())
    }
}

/// Account admission held an attempt before planning or executing its launch.
/// This wait neither spends a restart attempt nor erases an earlier failure.
/// Other account waits may follow a dispatched restart and keep their own outcome.
pub const WAIT_ACCOUNT: &str = "wait:account";

/// Positive proof that the last native helper received no exec grant; the same
/// owed command remains eligible without consuming launch-failure history.
pub const WAIT_NATIVE_UNISSUED: &str = "wait:native-launch-unissued";

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
    hand_on_tab_as(status, &super::upgrade_drive::hand_name())
}

/// [`hand_on_tab`] for the host whose hand is named `ours` ([`super::upgrade_drive::hand_name`]):
/// a hold, or any hand but that host's own lease.
#[must_use]
pub(super) fn hand_on_tab_as(status: &str, ours: &str) -> bool {
    status.split_whitespace().any(|t| {
        t == "hold=1"
            || t.strip_prefix("hand=")
                .is_some_and(|hand| hand != "-" && hand.strip_prefix("lease:") != Some(ours))
    })
}

/// How long a person's keys hold the relaunch line a record owes, under the configured `grace_s`.
///
/// After an end aterm made (`ended_at_s`, [`end_asked_at`]) it is the grace the record names
/// (`recorded`, [`St::line_grace_s`]: the one the gate that let that end go waited out, or the
/// whole grace after a relaunched build ended on its own, [`St::roll_back`]). That gate let the end
/// go only once every key was older than its grace, so no key from before the end holds the line,
/// and one typed after it holds the line exactly as long as one would have held the end. A record
/// that names none (an earlier build's, carried across an update) keeps the whole `grace_s`, as the
/// line of the build that wrote it did: its end's gate is unknown, and a line typed late costs a
/// wait while one typed early lands on a person at the prompt. Any other exit (`ended_at_s == 0`: a
/// crash, an end aterm did not ask) keeps the whole `grace_s` too.
#[must_use]
pub(super) fn line_grace(grace_s: u32, ended_at_s: u64, recorded: Option<u32>) -> u32 {
    match recorded {
        Some(recorded) if ended_at_s != 0 => recorded.min(grace_s),
        _ => grace_s,
    }
}

/// What holds the relaunch line by one `status` reply of its tab (`None`: none could be read),
/// for the host whose hand is named `ours`; `None` when nothing does.
///
/// A reply that is not `OK` holds it, as do a hand ([`hand_on_tab_as`]), input the tab's program
/// has not read (`input=`), and a person's keys within `grace_s` (the line's own, [`line_grace`]).
/// Only the server's own age of the last key (`human_ms=`) is read, never the wall clock.
pub(super) fn line_held_by_status(
    status: Option<&str>,
    grace_s: u32,
    ours: &str,
) -> Option<RelaunchLineError> {
    let Some(line) = status.filter(|l| crate::is_ok_reply(l)) else {
        return Some(RelaunchLineError::Wait("held"));
    };
    if hand_on_tab_as(line, ours) {
        return Some(RelaunchLineError::Hand);
    }
    let keyed = HumanInput::of_status(line).within(grace_s) == Some(true);
    (super::upgrade_drive::input_pending(line) || keyed).then_some(RelaunchLineError::Wait("held"))
}

/// The relaunch line a record owes (`line`, planned to run the agent in `line_dir`,
/// [`St::line_dir`]; empty: it carries its own `cd`) as typed at its shell, which stands in
/// `shell_now` (`None`: unread, no sign it moved) and speaks `dialect` (`None`: unread).
///
/// While the shell stands where the line was planned, the line as planned; once the person moved
/// it, that line with a `cd` back to `line_dir` ([`upgrade::with_cd`]), so the conversation resumes
/// where it ran, never in the shell's new directory. A line that cannot carry the `cd` (a fish
/// shell's, or one too long for the terminal with it) is `Refused`, and its caller waits for the
/// shell to come back (`wait:shell-moved`); an unread dialect waits.
/// The line a relaunch owes as typed at its shell ([`line_at_shell`]), where a moved shell whose
/// line cannot carry the `cd` back waits for the shell to come back (`wait:shell-moved`), as the
/// line did before it could follow one: refusing would end, for good, a relaunch the person's own
/// `cd -` could still let go.
pub(super) fn owed_line_at_shell(
    line: &str,
    line_dir: &str,
    shell_now: Option<&str>,
    dialect: Option<Dialect>,
) -> Result<String, NoPlan> {
    match line_at_shell(line, line_dir, shell_now, dialect) {
        Ok(line) => Ok(line.into_owned()),
        Err(NoPlan::Refused { .. }) => Err(NoPlan::Wait("shell-moved")),
        Err(no) => Err(no),
    }
}

pub(super) fn line_at_shell<'a>(
    line: &'a str,
    line_dir: &str,
    shell_now: Option<&str>,
    dialect: Option<Dialect>,
) -> Result<std::borrow::Cow<'a, str>, NoPlan> {
    match shell_now {
        Some(now) if !line_dir.is_empty() && !same_dir(Some(now), line_dir) => {
            let dialect = dialect.ok_or(NoPlan::Wait("shell-dialect"))?;
            upgrade::with_cd(dialect, line, line_dir)
                .map(std::borrow::Cow::Owned)
                .map_err(|e| NoPlan::Refused {
                    why: format!("line:{e}"),
                    what: "line".to_string(),
                    detail: e,
                })
        }
        _ => Ok(std::borrow::Cow::Borrowed(line)),
    }
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

/// Whether one `status` reply names a hand [`driver_hand`] counts whose holder (percent-decoded) is
/// no window host's ([`super::upgrade_drive::is_host_holder`]): a person's drive or another
/// program's, which another `aterm harness` process's own lease (`aterm harness upgrade`'s) is not.
#[must_use]
pub fn outside_hand(status: &str) -> bool {
    status.split_whitespace().any(|t| {
        t.strip_prefix("hand=").is_some_and(|hand| {
            hand.strip_prefix("lease:")
                .or_else(|| {
                    hand.strip_prefix("turn:")
                        .and_then(|turn| turn.split_once(':'))
                        .map(|(_, holder)| holder)
                })
                .is_some_and(|holder| {
                    !super::upgrade_drive::is_host_holder(&aterm_codec::pct::decode(holder))
                })
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

/// The look again after an [`Outcome::Busy`] step: account admission, a hand or
/// another actor on the upgrade state ([`WAIT_ACCOUNT`], [`WAIT_ACT_ENDS`], [`WAIT_HAND_LIFTS`]).
pub const BUSY: Duration = Duration::from_secs(10);

/// An agent that ran this long since its last relaunch of a crash starts the back-off over.
pub const HEALTHY: Duration = Duration::from_secs(600);

/// The longest pause before the next attempt at a harness restart ([`OnExit::Restarted`]).
pub const CARRY_EVERY: Duration = Duration::from_secs(60);

/// When the next attempt at a harness restart's line ([`OnExit::Restarted`]) comes after one that
/// did not land ([`carry_pause`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarryPause {
    /// The back-off's step ([`Relaunches::pause`]), never past [`CARRY_EVERY`].
    Ladder(Duration),
    /// One second after a person's keys let the line go: no attempt before it could land, so none
    /// is made, and one made then that its line still holds does not climb the back-off
    /// ([`Relaunches::attempted_at_keys`]).
    Keys(Duration),
}

impl CarryPause {
    /// The pause itself.
    #[must_use]
    pub const fn pause(self) -> Duration {
        match self {
            Self::Ladder(pause) | Self::Keys(pause) => pause,
        }
    }
}

/// The next attempt at a harness restart's line after one that did not land, by the tab's
/// `status`: while a person's keys hold the line, under `line_grace_s` (the record's own,
/// [`carry_line_grace`]), one second after they let it go ([`grace_left`]; never past
/// [`STALE_S`], the life of the record that owes the line), else the back-off's `ladder`, never
/// past [`CARRY_EVERY`].
#[must_use]
pub fn carry_pause(ladder: Duration, status: Option<&str>, line_grace_s: u32) -> CarryPause {
    match status.map(|s| grace_left(s, line_grace_s)) {
        Some(left) if !left.is_zero() => CarryPause::Keys(
            left.saturating_add(Duration::from_secs(1))
                .min(Duration::from_secs(STALE_S)),
        ),
        _ => CarryPause::Ladder(ladder.min(CARRY_EVERY)),
    }
}

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
    /// The exit relaunched now is one an Autopilot carries ([`Self::carry`]).
    carrying: bool,
    /// When the relaunch of the last exit an Autopilot carried landed.
    carried_landed: Option<Instant>,
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
        self.carrying = false;
        self.tries = 0;
        self.failing = false;
        self.failing_said = false;
        match self.decided(decision) {
            None => Ok(self.pause()),
            Some(say) => Err(say),
        }
    }

    /// THE AUTOPILOT CARRIES AN EXIT ONCE A HEALTHY RUN: whether the exit [`Self::exited_as`] was
    /// last told of, one [`carried`] found nobody's at `now` and the crash rule would not bring
    /// back anyway (a crash is relaunched under any Autopilot, and asks nothing of this), comes
    /// back. It does unless the relaunch of the last exit an Autopilot carried landed less than
    /// [`HEALTHY`] before: an agent that ends again so soon, on nobody's word aterm can see, is
    /// being ended on purpose (a person's `kill` from outside aterm, Activity Monitor's Quit) or
    /// ends itself, and coming back again would fight either, so it is left and recorded instead.
    /// The host asks again before every attempt at a carried exit's relaunch, so one that landed
    /// and is gone again before its landing was read is not carried twice. One that comes back
    /// starts that clock as its relaunch lands ([`Self::attempted`]), or as an agent is read back
    /// attached in the tab while that relaunch is still due ([`Self::running`]: its line's agent
    /// registered late, or one a person started meanwhile; either way an agent came back), and
    /// nothing but the next such landing starts it over: an attach with no carried relaunch due
    /// (a person's own start later on) starts no clock.
    pub fn carry(&mut self, now: Instant) -> bool {
        self.carrying = self
            .carried_landed
            .is_none_or(|at| now.saturating_duration_since(at) >= HEALTHY);
        self.carrying
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
        self.attempted_as(outcome, now, true)
    }

    /// [`Self::attempted`] of an attempt made as a person's keys let its line go
    /// ([`CarryPause::Keys`]): one its line still held (`wait:held`) does not climb the back-off,
    /// since the back-off did not time it.
    pub fn attempted_at_keys(&mut self, outcome: &Outcome, now: Instant) -> Say {
        let held = matches!(outcome, Outcome::NotYet(step) if step == WAIT_HELD);
        self.attempted_as(outcome, now, !held)
    }

    /// [`Self::attempted`], a miss climbing the back-off only when `climbs`.
    fn attempted_as(&mut self, outcome: &Outcome, now: Instant, climbs: bool) -> Say {
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
                if std::mem::take(&mut self.carrying) {
                    self.carried_landed = Some(now);
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
                if climbs {
                    if self.crash {
                        self.streak = self.streak.saturating_add(1);
                    } else {
                        self.tries = self.tries.saturating_add(1);
                    }
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

    /// An agent runs in the tab again at `now`, read back attached rather than landed by an
    /// attempt (started by someone else, or by an attempt whose agent registered after its own
    /// wait gave up). One read back while a carried exit's relaunch is due ([`Self::carry`]) counts
    /// as that relaunch's landing: the clock the next carry reads starts.
    pub fn running(&mut self, now: Instant) -> Say {
        self.start_healthy_clock(now);
        if std::mem::take(&mut self.carrying) && self.pending {
            self.carried_landed = Some(now);
        }
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
