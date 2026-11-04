// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compose automatic login scheduling with the executable's checked authority.
//! A relay or account proposal never supplies this callback.

use super::super::cli::{
    AccountsHost, RenewBrowser,
    shared_renewal::{Authority, Grant},
};
use super::*;
use std::sync::{Arc, OnceLock};

pub type CheckedAuthority = Arc<dyn Authority + Send + Sync>;
pub type Factory =
    dyn Fn(&Path, accounts::shared::Source, &Row) -> Result<CheckedAuthority, String> + Send + Sync;

static AUTHORITY: OnceLock<Box<Factory>> = OnceLock::new();
static CHECKPOINT_AUTHORITY: OnceLock<Box<Factory>> = OnceLock::new();
pub(super) type Discovery = dyn Fn(&Path) -> Result<Vec<Candidate>, String> + Send + Sync;
static DISCOVERY: OnceLock<Box<Discovery>> = OnceLock::new();

/// A supported candidate obtained from a current root decision, never from
/// the display catalog. The authority is re-read before every external effect.
pub struct Candidate {
    pub row: Row,
    pub cli: Cli,
    pub authority: CheckedAuthority,
    pub provider: Arc<dyn super::super::cli::shared_provider::Provider>,
    /// An already-owned row uses the ordinary scheduler when no prepared inputs are needed.
    pub ordinary_renewal: bool,
}

pub fn install_discovery(
    discover: impl Fn(&Path) -> Result<Vec<Candidate>, String> + Send + Sync + 'static,
) {
    let _ = DISCOVERY.set(Box::new(discover));
}

/// The composing executable installs its root adapter before starting upkeep.
/// There is no fallback from a refused shared approval to ordinary renewal.
pub fn install_authority(
    factory: impl Fn(&Path, accounts::shared::Source, &Row) -> Result<CheckedAuthority, String>
    + Send
    + Sync
    + 'static,
) {
    let _ = AUTHORITY.set(Box::new(factory));
}

/// Install the root reader for completing retained Claude checkpoints. Prepared
/// provider inputs may be recovered here without enabling ordinary renewal.
pub fn install_checkpoint_authority(
    factory: impl Fn(&Path, accounts::shared::Source, &Row) -> Result<CheckedAuthority, String>
    + Send
    + Sync
    + 'static,
) {
    let _ = CHECKPOINT_AUTHORITY.set(Box::new(factory));
}

#[derive(Clone, Default)]
pub(super) struct Effects<'a> {
    pub authority: Option<&'a Factory>,
    pub checkpoint_authority: Option<&'a Factory>,
    pub host: Option<Arc<dyn AccountsHost + Send + Sync>>,
    pub discover: Option<&'a Discovery>,
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionProgress",
        action = "Healthy",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::adoption_progress_transition"
    )
)]
fn needs_adoption(
    candidate: &Candidate,
    grant: &Grant,
    records: &accounts::AccountRecords,
    home: &Path,
    now: i64,
) -> bool {
    if candidate.row.browser.is_some() {
        return false;
    }
    let mut matching = records.roster.rows.iter().filter(|row| {
        candidate
            .row
            .email
            .iter()
            .chain(candidate.row.identity_email.iter())
            .any(|email| row.matches_email(email))
    });
    let Some(row) = matching.next() else {
        return true;
    };
    if matching.next().is_some()
        || row.browser.is_some()
        || (candidate.ordinary_renewal
            && records.logins.shared.required(row) == Ok(Some(grant.source))
            && !resumes_unstarted(&records.logins, grant))
        || super::super::cli::shared_renewal::pending_checkpoint(
            candidate.cli,
            row,
            grant,
            &records.raw_roster,
            &records.state,
            &records.logins,
            now,
        )
    {
        return false;
    }
    accounts::needs_sign_in(
        row,
        &records.state,
        &records.logins,
        accounts::enrolled_in(candidate.cli, &records.roster, home).contains(&row.label),
        now,
    )
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionProgress",
        action = "Resume",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::adoption_progress_transition"
    )
)]
pub(in crate::harness) fn resumes_unstarted(logins: &accounts::Logins, grant: &Grant) -> bool {
    logins.shared.latest(grant.source).is_some_and(|attempt| {
        attempt.intent.decision == grant.decision
            && attempt.outcome == accounts::shared::Outcome::NotStarted
    })
}

impl Effects<'static> {
    pub(super) fn live() -> Self {
        Self {
            authority: AUTHORITY.get().map(|f| &**f),
            checkpoint_authority: CHECKPOINT_AUTHORITY.get().map(|f| &**f),
            host: None,
            discover: DISCOVERY.get().map(|f| &**f),
        }
    }
}

/// Publication of a retained checkpoint still needs current root authority.
/// Explicit renewal supplies its own adapter; background recovery also obeys
/// the local sign-in switch. Neither form starts another provider operation.
pub(in crate::harness) enum CheckpointAuthority<'a> {
    Unavailable,
    Background {
        home: &'a Path,
        factory: &'a Factory,
        config: PathBuf,
    },
    Explicit(&'a dyn Authority),
}

impl<'a> CheckpointAuthority<'a> {
    pub(in crate::harness) fn background(
        home: Option<&'a Path>,
        factory: Option<&'a Factory>,
        roster: &Path,
    ) -> Self {
        match (home, factory) {
            (Some(home), Some(factory)) => Self::Background {
                home,
                factory,
                config: config_beside(roster),
            },
            _ => Self::Unavailable,
        }
    }

    pub(in crate::harness) fn live(home: Option<&'a Path>, roster: &Path) -> Self {
        Self::background(
            home,
            CHECKPOINT_AUTHORITY.get().map(|factory| &**factory),
            roster,
        )
    }

    pub(in crate::harness) fn check(
        &self,
        source: accounts::shared::Source,
        row: &Row,
        logins: &accounts::Logins,
        principal: Option<[u8; 32]>,
    ) -> Result<(), String> {
        match self {
            Self::Unavailable => Err("shared checkpoint requires current root authority".into()),
            Self::Explicit(authority) => super::super::cli::shared_renewal::check_checkpoint(
                *authority, source, row, logins, principal,
            ),
            Self::Background {
                home,
                factory,
                config,
            } => {
                let authority = automatic(factory(home, source, row)?, config.clone());
                super::super::cli::shared_renewal::check_checkpoint(
                    authority.as_ref(),
                    source,
                    row,
                    logins,
                    principal,
                )
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PreparationKind {
    #[default]
    Native,
    Shared,
}

impl PreparationKind {
    fn other(self) -> Self {
        match self {
            Self::Native => Self::Shared,
            Self::Shared => Self::Native,
        }
    }
}

/// Priority belongs to admitted work, while an exclusive discovery pass keeps
/// overlapping upkeep calls from acting on an obsolete priority.
#[derive(Default)]
pub(super) struct PreparationOrder {
    next: PreparationKind,
    active: bool,
}

impl PreparationOrder {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationOrder",
            action = "Begin",
            project = "aterm_agent::harness::accounts_sync::shared_renewal::order_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationOrder",
            action = "Busy",
            project = "aterm_agent::harness::accounts_sync::shared_renewal::order_tests::transition"
        )
    )]
    fn begin(&mut self) -> Option<PreparationKind> {
        if self.active {
            return None;
        }
        self.active = true;
        Some(self.next)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationOrder",
            action = "Native",
            project = "aterm_agent::harness::accounts_sync::shared_renewal::order_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationOrder",
            action = "Shared",
            project = "aterm_agent::harness::accounts_sync::shared_renewal::order_tests::transition"
        )
    )]
    fn admitted(&mut self, kind: PreparationKind) {
        assert!(
            self.active,
            "preparation admission needs its discovery pass"
        );
        self.next = kind.other();
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationOrder",
            action = "End",
            project = "aterm_agent::harness::accounts_sync::shared_renewal::order_tests::transition"
        )
    )]
    fn end(&mut self) {
        self.active = false;
    }
}

#[cfg(test)]
#[path = "accounts_preparation_order_tests.rs"]
mod order_tests;

struct PreparationPass {
    guard: Arc<std::sync::Mutex<AutoRenewGuard>>,
    first: PreparationKind,
}

impl PreparationPass {
    fn claim(guard: &Arc<std::sync::Mutex<AutoRenewGuard>>) -> Option<Self> {
        let first = {
            let mut state = guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.in_flight {
                return None;
            }
            state.preparation_order.begin()?
        };
        Some(Self {
            guard: Arc::clone(guard),
            first,
        })
    }

    fn admitted(&self, kind: PreparationKind) {
        self.guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .preparation_order
            .admitted(kind);
    }
}

impl Drop for PreparationPass {
    fn drop(&mut self) {
        self.guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .preparation_order
            .end();
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedProgress",
        action = "Healthy",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::shared_progress_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionProgress",
        action = "Busy",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::adoption_progress_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionProgress",
        action = "Cooldown",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::adoption_progress_transition"
    )
)]
pub(super) fn adopt_first(
    home: &Path,
    state_dir: &Path,
    roster: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) {
    let Some(pass) = PreparationPass::claim(fx.renew_guard) else {
        return;
    };
    for kind in [pass.first, pass.first.other()] {
        let admitted = match kind {
            PreparationKind::Native => prepare_native(
                home,
                state_dir,
                &config_beside(roster),
                fx.renew_guard,
                fx.spawn_renewal,
                &fx.log,
            ),
            PreparationKind::Shared => prepare_shared(home, state_dir, roster, now, fx),
        };
        if admitted {
            pass.admitted(kind);
            return;
        }
    }
}

fn prepare_shared(
    home: &Path,
    state_dir: &Path,
    roster: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> bool {
    prepare_shared_with_clock(home, state_dir, roster, now, fx, Instant::now)
}

pub(super) fn prepare_shared_with_clock(
    home: &Path,
    state_dir: &Path,
    roster: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
    clock: impl Fn() -> Instant,
) -> bool {
    let Some(discover) = fx.shared.discover else {
        return false;
    };
    let Ok(candidates) = discover(home) else {
        return false;
    };
    for candidate in ordered_shared_candidates(candidates, fx.renew_guard) {
        let cli = candidate.cli;
        let store = accounts::Store::at_for(state_dir, cli, home);
        let Some(target_roster) = accounts::roster_beside_for(&config_beside(roster), cli) else {
            continue;
        };
        let Ok(records) = store.read_account_records(Some(&target_roster), now) else {
            continue;
        };
        let logins = &records.logins;
        let Ok(grant) = candidate.authority.prepare(&candidate.row) else {
            continue;
        };
        if !needs_adoption(&candidate, &grant, &records, home, now) {
            continue;
        }
        let pending = logins
            .shared
            .pending_token(grant.source, grant.decision)
            .is_some();
        if !pending
            && logins
                .shared
                .check_preparation(grant.source, grant.generation, grant.expected_principal)
                .is_err()
        {
            continue;
        }
        let key = shared_key(grant.source);
        let Ok(Some(signin)) = store.signin_now() else {
            return false;
        };
        let Some(permit) =
            AutoRenewPermit::claim_preparation(fx.renew_guard, &key, clock(), &grant)
        else {
            continue;
        };
        let home = home.to_owned();
        let state_dir = state_dir.to_owned();
        let roster = target_roster;
        let host = fx.shared.host.clone();
        let log = fx.log.clone();
        let _ = start_auto_renew(
            permit,
            move || {
                let store = accounts::Store::at_for(&state_dir, cli, &home);
                let env = quiet_env(&home, &state_dir, &config_beside(&roster), now);
                let background = BackgroundHost {
                    home: &home,
                    mailbox: None,
                    headless: true,
                };
                let host = host
                    .as_deref()
                    .map_or(&background as &dyn AccountsHost, |host| host);
                if let Err(why) = super::super::cli::shared_adoption::adopt(
                    &env, host, &store, &signin, &roster, candidate,
                ) {
                    log(&format!("shared account adoption: {why}"));
                }
            },
            fx.spawn_renewal,
        );
        return true;
    }
    false
}

pub(super) fn shared_key(source: accounts::shared::Source) -> String {
    format!(
        "shared:{}:{}",
        aterm_codec::hex::encode_lower(&source.network),
        aterm_codec::hex::encode_lower(&source.account)
    )
}

/// Current root identity determines which attempt history belongs to a
/// candidate. Ranking observes that history without consuming or resetting it.
fn shared_priority(
    state: &AutoRenewGuard,
    grant: &Grant,
) -> (Option<Instant>, accounts::shared::Source) {
    let key = shared_key(grant.source);
    let attempted = if state.shared_preparations.get(&key) == Some(&(grant.source, grant.decision))
    {
        state.last_attempt.get(&key).copied()
    } else {
        None
    };
    (attempted, grant.source)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidatePriority",
        action = "Tie",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidatePriority",
        action = "Unseen",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidatePriority",
        action = "Oldest",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidatePriority",
        action = "Reissue",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidatePriority",
        action = "Unattributed",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidatePriority",
        action = "Refused",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidatePriority",
        action = "Context",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidatePriority",
        action = "ContextHistory",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidateProgress",
        action = "A",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::progress_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedCandidateProgress",
        action = "B",
        project = "aterm_agent::harness::accounts_sync::shared_renewal::candidate_tests::progress_transition"
    )
)]
fn ordered_shared_candidates(
    candidates: Vec<Candidate>,
    guard: &Arc<std::sync::Mutex<AutoRenewGuard>>,
) -> Vec<Candidate> {
    let mut prepared: Vec<_> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let grant = candidate.authority.prepare(&candidate.row).ok()?;
            Some((candidate, grant))
        })
        .collect();
    {
        let state = guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prepared.sort_by_key(|(_, grant)| shared_priority(&state, grant));
    }
    prepared
        .into_iter()
        .map(|(candidate, _)| candidate)
        .collect()
}

#[cfg(test)]
#[path = "accounts_shared_candidate_tests.rs"]
mod candidate_tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreparationRevision",
        action = "Replace",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::native_revision_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreparationRevision",
        action = "Person",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::native_revision_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreparationRevision",
        action = "Bot",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::native_revision_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreparationRevision",
        action = "Blocked",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::native_revision_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreparationRevision",
        action = "Allow",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::native_revision_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreparationRevision",
        action = "Future",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::native_revision_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreparationRevision",
        action = "Try",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::native_revision_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreparationRevision",
        action = "Repeat",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::native_revision_transition"
    )
)]
pub(super) fn prepare_native(
    home: &Path,
    state: &Path,
    config: &Path,
    guard: &Arc<std::sync::Mutex<AutoRenewGuard>>,
    spawn: &SpawnRenewal,
    log: &SignInLog,
) -> bool {
    let mut permit = None;
    let task = super::super::native_keys::preparation(home, state, config, &mut |id| {
        if permit.is_some() {
            return false;
        }
        permit = AutoRenewPermit::claim_native_preparation(guard, Instant::now(), id)
            .map(|permit| (permit, id.clone()));
        permit.is_some()
    });
    match (task, permit) {
        (Ok(Some(task)), Some((permit, admitted))) if task.preparation == admitted => {
            let log = log.clone();
            let _ = start_auto_renew(
                permit,
                move || {
                    if let Err(why) = (task.run)() {
                        log(&format!("native account preparation: {why}"));
                    }
                },
                spawn,
            );
            true
        }
        (Ok(Some(_)), _) => {
            log("native account preparation: task has no matching admission");
            false
        }
        (Err(why), _) => {
            log(&format!("native account preparation: {why}"));
            false
        }
        _ => false,
    }
}

struct Automatic {
    inner: CheckedAuthority,
    config: PathBuf,
}

impl Automatic {
    fn check_policy(&self) -> Result<(), String> {
        if sign_ins_on(&self.config, Cli::Claude) {
            Ok(())
        } else {
            Err("automatic account sign-ins are off".into())
        }
    }
}

impl Authority for Automatic {
    fn prepare(&self, row: &Row) -> Result<Grant, String> {
        self.check_policy()?;
        self.inner.prepare(row)
    }
    fn current(&self, grant: &Grant, row: &Row) -> Result<(), String> {
        self.check_policy()?;
        self.inner.current(grant, row)
    }
    fn request_recovery(
        &self,
        grant: &Grant,
        row: &Row,
        intent: &accounts::shared::Intent,
    ) -> Result<(), String> {
        self.current(grant, row)?;
        self.inner.request_recovery(grant, row, intent)
    }
}

pub(super) fn automatic(inner: CheckedAuthority, config: PathBuf) -> CheckedAuthority {
    Arc::new(Automatic { inner, config })
}

fn environment(home: &Path, name: &str) -> Option<std::ffi::OsString> {
    if name == "HOME" {
        return Some(home.as_os_str().to_owned());
    }
    std::env::var_os(name)
}

/// One bounded background provider flow, shared with ordinary automatic renewal.
pub(super) fn sign_in(
    home: &Path,
    staging: &str,
    email: &str,
    browser: &RenewBrowser,
    headless: bool,
    mailbox: Option<&Path>,
) -> Result<
    (
        super::super::align::Captured,
        Option<super::super::open_url::Met>,
    ),
    super::super::cli::RenewalFailure,
> {
    sign_in_with_inputs(home, staging, email, browser, headless, mailbox, None)
}

pub(crate) fn sign_in_with_inputs(
    home: &Path,
    staging: &str,
    email: &str,
    browser: &RenewBrowser,
    headless: bool,
    mailbox: Option<&Path>,
    inputs: Option<&Path>,
) -> Result<
    (
        super::super::align::Captured,
        Option<super::super::open_url::Met>,
    ),
    super::super::cli::RenewalFailure,
> {
    let pass_env =
        super::super::cli::unattended_renewal_environment(&|name| environment(home, name))?;
    let prog = resolve_claude_program(home);
    let mut cmd = super::super::cli::renew_command(prog.as_os_str(), staging, email, browser);
    cmd.env("HOME", home)
        .env(super::super::open_url::EMAIL_ENV, email)
        .envs(pass_env);
    if std::env::var_os(super::super::open_url::MAILBOX_ENV).is_none_or(|v| v.is_empty())
        && let Some(mailbox) = mailbox
    {
        cmd.env(super::super::open_url::MAILBOX_ENV, mailbox);
    }
    if let Some(inputs) = inputs {
        cmd.env(super::super::open_url::INPUT_ENV, inputs);
    }
    if headless || inputs.is_some() {
        cmd.env(super::super::headless_oauth::HEADLESS_ENV, "1");
    }
    super::super::cli::capture_unattended_renewal(cmd, super::super::cli::UNATTENDED_RENEWAL_BUDGET)
}

pub(super) struct BackgroundHost<'a> {
    pub home: &'a Path,
    pub mailbox: Option<&'a Path>,
    pub headless: bool,
}

impl AccountsHost for BackgroundHost<'_> {
    fn automatic_sign_in(&self) -> bool {
        true
    }
    fn native_credential_setup(&self) -> bool {
        true
    }
    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
    fn renew_flow_with_inputs(
        &self,
        staging: &str,
        email: &str,
        browser: &RenewBrowser,
        inputs: &Path,
    ) -> Result<i32, String> {
        self.renew_outcome_with_inputs(staging, email, browser, inputs)
            .map(|exit| exit.code)
            .map_err(|failure| failure.to_string())
    }
    fn renew_outcome_with_inputs(
        &self,
        staging: &str,
        email: &str,
        browser: &RenewBrowser,
        inputs: &Path,
    ) -> Result<super::super::cli::RenewalExit, super::super::cli::RenewalFailure> {
        super::super::cli::RenewalExit::from_capture(sign_in_with_inputs(
            self.home,
            staging,
            email,
            browser,
            true,
            None,
            Some(inputs),
        )?)
    }
    fn shared_claude_profile(
        &self,
        access_token: &str,
        wait: std::time::Duration,
    ) -> Result<
        super::super::cli::shared_provider::ClaudeProfile,
        super::super::cli::shared_provider::ObservationError,
    > {
        super::super::cli::shared_provider::read_claude_profile(access_token, wait)
    }
    fn machine_var(&self, name: &str) -> Option<std::ffi::OsString> {
        environment(self.home, name)
    }
    fn vendor_sign_in(
        &self,
        request: &super::super::login_drive::Request<'_>,
    ) -> super::super::login_drive::Drove {
        super::super::login_drive::sign_in_live(request, self.home, &|name| {
            environment(self.home, name)
        })
    }
    fn vendor_sign_in_with_mailbox(
        &self,
        request: &super::super::login_drive::Request<'_>,
        home: &Path,
        mailbox: super::super::headless_oauth::MailboxConfig,
    ) -> super::super::login_drive::Drove {
        super::super::cli::vendor_mailbox_sign_in(self, request, home, mailbox)
    }
    fn codex_launch_program(&self) -> Option<PathBuf> {
        Some(super::super::login_drive::program_of(Cli::Codex, self.home))
    }
    fn codex_sign_in_program(&self) -> Option<PathBuf> {
        Some(super::super::login_drive::program_of(Cli::Codex, self.home))
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<super::super::align::Captured, String> {
        Err("background shared renewal checks only its staging login".into())
    }
    fn unaskable(&self) -> Option<String> {
        super::super::cli::auth_check_env(&|name| environment(self.home, name)).err()
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("background shared renewal requires its approved named flow".into())
    }
    fn now(&self) -> i64 {
        crate::supervise::limit::unix_now()
    }
    fn renew_flow(
        &self,
        staging: &str,
        email: &str,
        browser: &RenewBrowser,
    ) -> Result<i32, String> {
        self.renew_outcome(staging, email, browser)
            .map(|exit| exit.code)
            .map_err(|failure| failure.to_string())
    }
    fn renew_outcome(
        &self,
        staging: &str,
        email: &str,
        browser: &RenewBrowser,
    ) -> Result<super::super::cli::RenewalExit, super::super::cli::RenewalFailure> {
        super::super::cli::RenewalExit::from_capture(sign_in(
            self.home,
            staging,
            email,
            browser,
            self.headless,
            self.mailbox,
        )?)
    }
    fn status_in_home(&self, staging: &str) -> Result<super::super::align::Captured, String> {
        super::super::cli::status_in_home_within(
            resolve_claude_program(self.home).as_os_str(),
            staging,
            &|name| environment(self.home, name),
            super::super::cli::AUTH_CHECK_BUDGET,
        )
    }
    fn claude_principal_in_home(&self, staging: &str) -> Option<[u8; 32]> {
        super::super::profile::principal_in_home(Path::new(staging))
    }

    fn claude_identity_in_home(
        &self,
        staging: &str,
        email: &str,
    ) -> super::super::profile::IdentityObservation {
        super::super::profile::identity_in_home(Path::new(staging), email)
    }
}
