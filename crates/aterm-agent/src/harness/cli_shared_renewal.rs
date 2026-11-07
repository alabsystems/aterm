// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One explicit, root-checked renewal of an existing local Claude row. Root
//! resolution is supplied by the composing executable, not by provider I/O.

use super::*;
use accounts::shared::{self, Source};

/// Terms supplied only after the composing adapter has checked its root record
/// and owned configuration. These fields alone are not authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub source: Source,
    pub decision: [u8; 32],
    pub generation: u64,
    pub configuration: [u8; 32],
    pub identity: String,
    pub expected_principal: Option<[u8; 32]>,
    /// A hard profile requirement from the approved configuration, distinct
    /// from an optional local preference. It must match the selected route.
    pub required_browser_profile: Option<String>,
}

/// The root/application check, independent of provider and filesystem I/O.
pub trait Authority {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String>;
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String>;
    /// Ask the issuer to consider a distinct preparation. This never permits
    /// repeating the consumed provider action or replacing its unknown result.
    fn request_recovery(
        &self,
        _grant: &Grant,
        _row: &accounts::Row,
        _intent: &shared::Intent,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Called under the account writer immediately before checkpoint publication.
/// Retained custody identifies the attempt; only the current root grants its
/// publication. A successor grant cannot adopt an older preparation's result.
pub(in crate::harness) fn check_checkpoint(
    authority: &dyn Authority,
    source: Source,
    row: &accounts::Row,
    logins: &accounts::Logins,
    principal: Option<[u8; 32]>,
) -> Result<(), String> {
    let grant = authority.prepare(row)?;
    let attempt = logins
        .shared
        .latest(source)
        .ok_or_else(|| "shared checkpoint has no retained preparation".to_owned())?;
    let intent = &attempt.intent;
    if attempt.outcome != shared::Outcome::Pending
        || grant.source != source
        || grant.decision != intent.decision
        || grant.generation != intent.generation
        || grant.configuration != intent.configuration
        || intent.label != row.label
        || !accounts::email_matches(&grant.identity, &intent.identity)
        || !row
            .expected_identity_email()
            .is_some_and(|email| accounts::email_matches(email, &intent.identity))
        || intent.before_generation
            != logins
                .get(&row.label)
                .map(|binding| binding.renewal_generation)
        || !principal.is_some_and(|principal| {
            logins.shared.principal_allowed(source, principal)
                && grant
                    .expected_principal
                    .is_none_or(|expected| expected == principal)
        })
    {
        return Err("shared checkpoint differs from current root preparation".into());
    }
    authority.current(&grant, row)
}

/// A completed local login has priority over asking for another preparation.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "Checkpoint",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "MissingCredential",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "NewerSignout",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "QuotaCheckpoint",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "ReferencedCheckpoint",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "SupersededCheckpoint",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
pub(in crate::harness) fn pending_checkpoint(
    cli: Cli,
    row: &accounts::Row,
    grant: &Grant,
    roster: &accounts::Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> bool {
    // Shared Codex and Kimi results require their typed provider proof and
    // current root grant. Their adoption path reconciles retained Pending data.
    if cli != Cli::Claude {
        return false;
    }
    let Ok(Some(source)) = logins.shared.required(row) else {
        return false;
    };
    let Some(attempt) = logins.shared.latest(source) else {
        return false;
    };
    if source != grant.source
        || attempt.outcome != shared::Outcome::Pending
        || attempt.intent.label != row.label
        || attempt.intent.decision != grant.decision
        || attempt.intent.generation != grant.generation
        || attempt.intent.configuration != grant.configuration
        || !accounts::email_matches(&attempt.intent.identity, &grant.identity)
    {
        return false;
    }
    let staging = Path::new(&attempt.intent.staging);
    let Ok(actual) = std::fs::canonicalize(staging) else {
        return false;
    };
    accounts::read_renewal_checkpoint_for(cli, staging, &actual, now).is_some_and(|checkpoint| {
        checkpoint.label == row.label
            && accounts::email_matches(&checkpoint.email, &attempt.intent.identity)
            && accounts::checkpoint_identity_matches(cli, row, &checkpoint)
            && accounts::checkpoint_unreferenced(roster, logins, staging, &actual)
            && accounts::checkpoint_current(row, state, logins, &checkpoint, None)
            && {
                let identity = super::super::profile::identity_in_home(staging, &checkpoint.email);
                identity.email_matches != Some(false)
                    && identity.principal.is_some_and(|principal| {
                        logins.shared.principal_allowed(source, principal)
                            && grant
                                .expected_principal
                                .is_none_or(|expected| expected == principal)
                    })
                    && logins.shared.checkpoint_allowed(
                        row,
                        identity.principal,
                        &checkpoint.dir,
                        &checkpoint.parent,
                    )
            }
    })
}

/// Advertise an uncertain consumed attempt without refunding its preparation.
/// The issuer independently checks this advisory request before issuing a new
/// decision. No account writer is held while publishing the request.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "Unknown",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "Pending",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "Published",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "Held",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "NotStarted",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "ChangedBinding",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "ChangedDecision",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "Stopped",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "Unavailable",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "Retry",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "ChangedRoster",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "WrongIdentity",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "WrongConfiguration",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "WrongGeneration",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedProgress",
        action = "Fail",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::shared_progress_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedProgress",
        action = "Retry",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::shared_progress_transition"
    )
)]
pub(in crate::harness) fn recover_consumed(
    authority: &dyn Authority,
    grant: &Grant,
    store: &accounts::Store,
    policy: accounts::RenewalPolicy<'_>,
    _signin: &accounts::Lock,
    now: i64,
) -> Result<(), String> {
    let records = store
        .read_account_records(Some(policy.roster_path), now)
        .map_err(|error| match error {
            accounts::RosterReadError::Roster(why) | accounts::RosterReadError::Records(why) => why,
        })?;
    let row = policy.row;
    if &records.raw_roster != policy.roster || records.roster.row(&row.label) != Some(row) {
        return Ok(());
    }
    let logins = &records.logins;
    let Some(attempt) = logins.shared.latest(grant.source) else {
        return Ok(());
    };
    let intent = &attempt.intent;
    if pending_checkpoint(
        Cli::Claude,
        row,
        grant,
        &records.raw_roster,
        &records.state,
        logins,
        now,
    ) {
        return Ok(());
    }
    if !matches!(
        attempt.outcome,
        shared::Outcome::Pending | shared::Outcome::Unknown
    ) || intent.decision != grant.decision
        || intent.generation != grant.generation
        || intent.configuration != grant.configuration
        || intent.label != row.label
        || !accounts::email_matches(&intent.identity, &grant.identity)
        || !row
            .expected_identity_email()
            .is_some_and(|email| accounts::email_matches(email, &intent.identity))
        || logins.shared.required(row)? != Some(grant.source)
        || intent.before_generation
            != logins
                .get(&row.label)
                .map(|binding| binding.renewal_generation)
    {
        return Ok(());
    }
    authority.current(grant, row)?;
    authority.request_recovery(grant, row, intent)
}

pub(super) enum Guard<'a> {
    Local,
    Shared {
        authority: &'a dyn Authority,
        request: [u8; 16],
        grant: Option<Box<Grant>>,
        token: Option<shared::Token>,
    },
}

impl Guard<'_> {
    pub(super) fn retains_staging(&self) -> bool {
        matches!(self, Self::Shared { .. })
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "OptionalDefault",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "RequiredDefault",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "OptionalRemote",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "RequiredRemote",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "OptionalProfile",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "WrongProfile",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "RequiredProfile",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    pub(super) fn preflight(
        &mut self,
        row: &accounts::Row,
        logins: &accounts::Logins,
        browser: &RenewBrowser,
    ) -> Result<(), String> {
        match self {
            Self::Local => unmanaged(logins, row, None, false),
            Self::Shared {
                authority, grant, ..
            } => {
                let approved = authority.prepare(row)?;
                let expected = row.expected_identity_email().ok_or_else(|| {
                    "shared renewal requires an explicit provider identity".to_owned()
                })?;
                if !accounts::email_matches(expected, &approved.identity)
                    || logins
                        .shared
                        .required(row)?
                        .is_some_and(|source| source != approved.source)
                {
                    return Err("shared renewal identity differs from its local custody".into());
                }
                if let Err(why) = logins.shared.check_preparation(
                    approved.source,
                    approved.generation,
                    approved.expected_principal,
                ) {
                    // Retain the checked terms for an advisory recovery request.
                    // Begin still refuses this consumed or conflicting preparation.
                    *grant = Some(Box::new(approved));
                    return Err(why);
                }
                if let Some(required) = approved.required_browser_profile.as_deref() {
                    match browser {
                        RenewBrowser::Profile { profile, .. }
                        | RenewBrowser::OwnProfile { profile, .. }
                            if !required.trim().is_empty() && profile == required => {}
                        RenewBrowser::Default => return Err("shared renewal requires a local browser profile, but its opener is unavailable".into()),
                        RenewBrowser::Remote { .. } => return Err("shared renewal cannot enforce the required browser profile over a remote route".into()),
                        RenewBrowser::Profile { .. } | RenewBrowser::OwnProfile { .. } => return Err("shared renewal selected a different browser profile than the approved requirement".into()),
                    }
                }
                *grant = Some(Box::new(approved));
                Ok(())
            }
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "Current",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "Stopped",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "ChangedHead",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "Unavailable",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "Unrelated",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    pub(super) fn begin(
        &mut self,
        store: &accounts::Store,
        policy: accounts::RenewalPolicy<'_>,
        staging: &str,
        now: i64,
    ) -> Result<accounts::Logins, String> {
        self.begin_with(store, policy, staging, now, || {})
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRenewalSnapshot",
            action = "Begin",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::snapshot_transition"
        )
    )]
    pub(super) fn begin_with(
        &mut self,
        store: &accounts::Store,
        policy: accounts::RenewalPolicy<'_>,
        staging: &str,
        now: i64,
        after_commit: impl FnOnce(),
    ) -> Result<accounts::Logins, String> {
        if matches!(self, Self::Shared { grant: None, .. }) {
            return Err("shared renewal was not checked".to_owned());
        }
        // Keep the committed generation through the returned read. A later writer
        // must not become this attempt's parent while its grant stays unchanged.
        let held = store
            .acquire_within(&accounts::LOCK_LADDER)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| accounts::LOCK_HELD.to_owned())?;
        match self {
            Self::Local => {
                store.transact_renewal_held(&held, now, &mut None, policy, |_, logins| {
                    unmanaged(logins, policy.row, None, false)?;
                    Ok(Vec::new())
                })?;
            }
            Self::Shared {
                authority,
                request,
                grant,
                token,
            } => {
                let approved = grant
                    .as_ref()
                    .ok_or_else(|| "shared renewal was not checked".to_owned())?;
                let mut begun = None;
                store.transact_renewal_held(&held, now, &mut None, policy, |_, logins| {
                    // Check after the account writer wait, before consuming the grant.
                    authority.current(approved, policy.row)?;
                    begun = Some(logins.shared.begin(
                        shared::Intent {
                            source: approved.source,
                            decision: approved.decision,
                            generation: approved.generation,
                            configuration: approved.configuration,
                            request: *request,
                            label: policy.row.label.clone(),
                            identity: approved.identity.clone(),
                            staging: staging.to_owned(),
                            before_generation:
                                logins.get(&policy.row.label).map(|b| b.renewal_generation),
                            begun_at: now,
                        },
                        approved.expected_principal,
                    )?);
                    Ok(vec![
                        accounts::Event::new(now, accounts::Kind::SignIn, &policy.row.label)
                            .because("shared preparation begun"),
                    ])
                })?;
                *token = begun;
            }
        }
        after_commit();
        store.load_logins(now).map(|(logins, _)| logins)
    }

    pub(super) fn check(&self, row: &accounts::Row) -> Result<(), String> {
        if let Self::Shared {
            authority, grant, ..
        } = self
        {
            authority.current(
                grant
                    .as_ref()
                    .ok_or_else(|| "shared preparation was not begun".to_owned())?,
                row,
            )?;
        }
        Ok(())
    }

    /// Observe the captured row, parent and authority after waiting for the writer.
    pub(super) fn check_policy(
        &self,
        store: &accounts::Store,
        policy: accounts::RenewalPolicy<'_>,
        now: i64,
    ) -> Result<(), String> {
        store
            .transact_renewal_within(&accounts::LOCK_LADDER, now, &mut None, policy, |_, _| {
                self.check(policy.row)?;
                Ok(Vec::new())
            })
            .map(|_| ())
    }

    pub(super) fn publish(
        &mut self,
        logins: &mut accounts::Logins,
        row: &accounts::Row,
        principal: Option<[u8; 32]>,
        now: i64,
    ) -> Result<(), String> {
        match self {
            Self::Local => unmanaged(logins, row, principal, true),
            Self::Shared {
                authority,
                grant,
                token,
                ..
            } => {
                let grant = grant
                    .as_ref()
                    .ok_or_else(|| "shared preparation was not begun".to_owned())?;
                // Recheck after acquiring the account writer as well: waiting
                // for that lock must not reuse a pre-wait authority observation.
                authority.current(grant, row)?;
                let principal = principal
                    .ok_or_else(|| "shared renewal has no full provider identity".to_owned())?;
                if grant
                    .expected_principal
                    .is_some_and(|expected| expected != principal)
                {
                    return Err("shared renewal did not prove the approved provider account".into());
                }
                let generation = logins
                    .generation_clock
                    .checked_add(1)
                    .ok_or_else(|| "local binding generation exhausted".to_owned())?;
                logins.shared.finish(
                    token
                        .take()
                        .ok_or_else(|| "shared preparation was already completed".to_owned())?,
                    shared::Outcome::Published { generation },
                    Some((&grant.identity, principal)),
                    now,
                )
            }
        }
    }

    fn failed(
        &mut self,
        store: &accounts::Store,
        policy: accounts::RenewalPolicy<'_>,
        signin: &accounts::Lock,
        now: i64,
        err: &mut dyn Write,
    ) {
        if let Self::Shared { token, .. } = self
            && let Some(token) = token.take()
            && let Err(why) =
                store.transact_logins_within(&accounts::LOCK_LADDER, now, &mut None, |_, logins| {
                    logins
                        .shared
                        .finish(token, shared::Outcome::Unknown, None, now)?;
                    Ok(Vec::new())
                })
        {
            let _ = writeln!(err, "shared renewal outcome remains unknown: {why}");
        }
        if let Self::Shared {
            authority,
            grant: Some(grant),
            ..
        } = self
        {
            let result = recover_consumed(*authority, grant, store, policy, signin, now);
            if let Err(why) = result {
                let _ = writeln!(err, "shared renewal recovery deferred: {why}");
            }
        }
    }
}

pub(in crate::harness) fn unmanaged(
    logins: &accounts::Logins,
    row: &accounts::Row,
    principal: Option<[u8; 32]>,
    publishing: bool,
) -> Result<(), String> {
    if logins.shared.required(row)?.is_some()
        || principal.is_some_and(|p| logins.shared.principal_source(p).is_some())
    {
        return Err("this account requires a fresh root-authorized shared renewal".into());
    }
    if publishing && !logins.shared.is_empty() && principal.is_none() {
        return Err(
            "provider identity is unavailable in a store with shared account custody".into(),
        );
    }
    Ok(())
}

/// Run exactly one renewal. No fallback or retry re-enters a consumed grant.
/// The provider's own sign-in may still require a person at the terminal.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRenewalIdentity",
        action = "Replay",
        project = "aterm_agent::harness::cli::tests::renewal_identity::renewal_identity_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpoint",
        action = "Fail",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::checkpoint_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpoint",
        action = "Supersede",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::checkpoint_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "Good",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "StoppedBefore",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "StoppedDuring",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "WrongPrincipal",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "ChangedRow",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "Crash",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "ChangedHead",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "MissingPrincipal",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "Replay",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
pub fn run(
    env: &Env,
    host: &dyn AccountsHost,
    label: &str,
    request: [u8; 16],
    authority: &dyn Authority,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    if request == [0; 16] {
        let _ = writeln!(err, "shared renewal requires a nonzero request id");
        return ExitCode::from(2);
    }
    let Some(path) = accounts_roster_path_for(Cli::Claude, env, err) else {
        return ExitCode::from(1);
    };
    let store = env.accounts_store();
    super::super::accounts_sync::recover_staging_renewals_for(
        Cli::Claude,
        &store,
        &path,
        host.now().max(env.now),
        super::super::accounts_sync::shared_renewal::CheckpointAuthority::Explicit(authority),
    );
    let Some(records) = read_renewal_records(
        Cli::Claude,
        &store,
        &path,
        host.now().max(env.now),
        RenewalRecovery::None,
        env.home.as_deref(),
        err,
    ) else {
        return ExitCode::from(1);
    };
    let Some(row) = records.roster.row(label) else {
        let _ = writeln!(err, "shared renewal requires an existing local row");
        return ExitCode::from(1);
    };
    if row.email.is_none() || !host.stdin_is_terminal() {
        let _ = writeln!(
            err,
            "shared renewal requires a named local row and an interactive terminal"
        );
        return ExitCode::from(1);
    }
    if let Some(why) = host.unaskable() {
        let _ = writeln!(err, "shared renewal cannot check its login: {why}");
        return ExitCode::from(1);
    }
    let (lock, waited) = match signin_guard(&store, host, label, "shared-renew", out, err) {
        Ok(taken) => taken,
        Err(_) => return ExitCode::from(1),
    };
    let records = if waited {
        let Some(records) = read_renewal_records(
            Cli::Claude,
            &store,
            &path,
            host.now().max(env.now),
            RenewalRecovery::None,
            env.home.as_deref(),
            err,
        ) else {
            return ExitCode::from(1);
        };
        records
    } else {
        records
    };
    let Some(row) = records.roster.row(label) else {
        let _ = writeln!(err, "shared renewal requires an existing local row");
        return ExitCode::from(1);
    };
    let browser = if host.under_ssh() {
        RenewBrowser::Remote {
            opener: opener_link(&store, host).ok(),
        }
    } else {
        match row.browser.as_ref() {
            Some(profile) => match opener_link(&store, host) {
                Ok(opener) => RenewBrowser::Profile {
                    opener,
                    profile: profile.clone(),
                },
                Err(error) => {
                    let _ = writeln!(err, "shared renewal: {error}; nothing was run");
                    return ExitCode::from(1);
                }
            },
            None => RenewBrowser::Default,
        }
    };
    let policy = accounts::RenewalPolicy {
        roster_path: &path,
        roster: &records.raw_roster,
        row,
        logins: &records.logins,
    };
    let own_dir = records.raw_roster.row(label).and_then(|r| r.dir.as_deref());
    let mut guard = Guard::Shared {
        authority,
        request,
        grant: None,
        token: None,
    };
    let now = host.now().max(env.now);
    let Ok(begun) = super::begin_sign_in(
        super::person_sign_in(Cli::Claude, label),
        (Cli::Claude, label),
        (&store, lock),
        now,
        ("shared-renew", err),
    ) else {
        return ExitCode::from(1);
    };
    let result = renew_one_guarded(
        env, host, &store, &begun, policy, own_dir, &browser, out, err, &mut guard,
    );
    if result == Renewal::Renewed {
        ExitCode::SUCCESS
    } else {
        if let Some(lock) = begun.lock() {
            guard.failed(&store, policy, lock, now, err);
        }
        ExitCode::from(1)
    }
}

/// The upkeep already holds sign-in admission and has selected an unattended
/// route. It uses the same grant consumption and publication as explicit renewal.
pub(in crate::harness) struct BackgroundRenewal {
    pub renewal: Renewal,
    pub observation: Option<super::RenewalObservation>,
}

pub(in crate::harness) fn background(
    env: &Env,
    host: &dyn AccountsHost,
    store: &accounts::Store,
    signin: &super::Begun,
    policy: accounts::RenewalPolicy<'_>,
    browser: &RenewBrowser,
    authority: &dyn Authority,
) -> BackgroundRenewal {
    let mut request = [0; 16];
    if aterm_uds::rand::fill(&mut request).is_err() || request == [0; 16] {
        return BackgroundRenewal {
            renewal: Renewal::Failed,
            observation: None,
        };
    }
    let mut guard = Guard::Shared {
        authority,
        request,
        grant: None,
        token: None,
    };
    let own_dir = policy
        .roster
        .row(&policy.row.label)
        .and_then(|row| row.dir.as_deref());
    let mut observation = None;
    let result = super::renew_one_observed(
        env,
        host,
        store,
        signin,
        policy,
        own_dir,
        browser,
        &mut io::sink(),
        &mut io::sink(),
        &mut guard,
        &mut observation,
    );
    if result != Renewal::Renewed
        && !host.work_cancelled()
        && !observation
            .as_ref()
            .is_some_and(super::RenewalObservation::service_deferred)
        && let Some(lock) = signin.lock()
    {
        guard.failed(
            store,
            policy,
            lock,
            host.now().max(env.now),
            &mut io::sink(),
        );
    }
    BackgroundRenewal {
        renewal: result,
        observation,
    }
}
