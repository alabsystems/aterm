// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First local custody of a root-approved account. A candidate is private until
//! its staged login proves the approved provider identity.

use super::super::accounts_sync::shared_renewal::{Candidate, resumes_unstarted};
use super::*;
use accounts::shared::{Intent, Outcome};
#[path = "cli_shared_admission.rs"]
mod admission;
use std::fs;

struct LocalPolicy<'a> {
    authority: &'a dyn shared_renewal::Authority,
    roster: &'a Path,
}
impl shared_renewal::Authority for LocalPolicy<'_> {
    fn prepare(&self, row: &accounts::Row) -> Result<shared_renewal::Grant, String> {
        if !super::super::accounts_sync::shared_adoption_on(self.roster) {
            return Err("automatic shared adoption is off".into());
        }
        self.authority.prepare(row)
    }
    fn current(&self, grant: &shared_renewal::Grant, row: &accounts::Row) -> Result<(), String> {
        if !super::super::accounts_sync::shared_adoption_on(self.roster) {
            return Err("automatic shared adoption is off".into());
        }
        self.authority.current(grant, row)
    }
    fn request_recovery(
        &self,
        grant: &shared_renewal::Grant,
        row: &accounts::Row,
        intent: &Intent,
    ) -> Result<(), String> {
        self.current(grant, row)?;
        self.authority.request_recovery(grant, row, intent)
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionProgress",
        action = "Unstarted",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::adoption_progress_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "WrongPrincipal",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "Retry",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "Reissue",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "WrongEmail",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "Stopped",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "Revoked",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "ChangedRoster",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "ChangedBinding",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "MissingProof",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "Unavailable",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "RetryUnavailable",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "ObservationError",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "Healthy",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "UnknownReply",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "UnknownCompleted",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "PriorCheckpoint",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "ClaimedPending",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "ClaimedUnstarted",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "ClaimedBeforeBegin",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "RetainedPrincipal",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "OtherPrincipal",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "ZeroPrincipal",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "Missing",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "Retry",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "Reissue",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "Unstarted",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "RetryUnstarted",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "ReissueUnstarted",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "Revoked",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "Disabled",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "RowChanged",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "BindingChanged",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "Claimed",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "PublicationUnavailable",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "RetryPublication",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "FileEntry",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "RevokedDuringCheck",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "UnstartedWithoutOpener",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedStagingRecovery",
        action = "MissingParent",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::staging::transition"
    )
)]
pub(in crate::harness) fn adopt(
    env: &Env,
    host: &dyn AccountsHost,
    store: &accounts::Store,
    _signin: &accounts::Lock,
    roster_path: &Path,
    candidate: Candidate,
) -> Result<bool, String> {
    let Candidate {
        mut row,
        cli,
        authority,
        provider,
        ordinary_renewal,
    } = candidate;
    let source_authority = authority.clone();
    let checked = LocalPolicy {
        authority: authority.as_ref(),
        roster: roster_path,
    };
    let authority: &dyn shared_renewal::Authority = &checked;
    if !super::super::accounts_sync::shared_adoption_on(roster_path) {
        return Err("automatic shared adoption is off".into());
    }
    let mut grant = authority.prepare(&row)?;
    let (target, mode) = roster_target(roster_path)?;
    store.recover_sync_within(&accounts::LOCK_LADDER, &target)?;
    let before = accounts::roster_text(&target)?;
    let raw = Roster::parse_for(cli, before.as_deref().unwrap_or(""))?;
    let (logins, _) = store.load_logins(env.now)?;
    let matches: Vec<_> = raw
        .rows
        .iter()
        .filter(|old| {
            row.email
                .iter()
                .chain(row.identity_email.iter())
                .any(|email| old.matches_email(email))
        })
        .collect();
    let is_new = matches.is_empty();
    if !is_new {
        if matches.len() != 1 || matches[0].held_by.is_some() {
            return Err("approved account has ambiguous or held local identity".into());
        }
        row = (*matches[0]).clone();
        grant = authority.prepare(&row)?;
        let effective = accounts::bound(&raw, &logins);
        let current = effective
            .row(&row.label)
            .ok_or("existing account disappeared")?;
        let (state, _) = store.load(env.now)?;
        if (ordinary_renewal
            && logins.shared.required(current)? == Some(grant.source)
            && !resumes_unstarted(&logins, &grant))
            || shared_renewal::pending_checkpoint(
                cli, current, &grant, &raw, &state, &logins, env.now,
            )
            || !accounts::needs_sign_in(
                current,
                &state,
                &logins,
                env.home.as_deref().is_some_and(|home| {
                    accounts::enrolled_in(cli, &effective, home).contains(&current.label)
                }),
                env.now,
            )
        {
            return Ok(false);
        }
    } else if raw.row(&row.label).is_some() {
        return Err("approved account label is already used by another local identity".into());
    }
    if row.browser.is_some() {
        return Err(super::super::account_catalog::NAMED_BROWSER_UNSUPPORTED.into());
    }
    let before_parent = accounts::RenewalParent::capture(&logins, &row.label);
    let email = row
        .expected_identity_email()
        .ok_or("approved account has no provider identity")?
        .to_owned();
    let pending = logins.shared.latest(grant.source).filter(|attempt| {
        attempt.intent.decision == grant.decision
            && matches!(attempt.outcome, Outcome::Pending | Outcome::NotStarted)
    });
    // New admissions need a usable opener before consuming authority. A
    // retained attempt first checks its staging home and recovery eligibility.
    let mut selected_browser = pending
        .is_none()
        .then(|| browser(store, host, &row, cli))
        .transpose()?;
    if pending.is_none()
        && logins
            .shared
            .check_preparation(grant.source, grant.generation, grant.expected_principal)
            .is_err()
    {
        return Ok(false);
    }
    let (staging, mut token, run_provider) = if let Some(pending) = pending {
        if pending.intent.label != row.label
            || pending.intent.identity != email.to_ascii_lowercase()
        {
            return Err("pending shared adoption differs from its approved identity".into());
        }
        let stage = PathBuf::from(&pending.intent.staging);
        if stage.parent() != Some(store.logins_dir().as_path()) {
            return Err("pending shared adoption has an unrelated staging home".into());
        }
        let token = logins
            .shared
            .pending_token(grant.source, grant.decision)
            .ok_or("pending shared adoption changed")?;
        (stage, Some(token), pending.outcome == Outcome::NotStarted)
    } else {
        logins.shared.check_preparation(
            grant.source,
            grant.generation,
            grant.expected_principal,
        )?;
        let staging = store
            .claim_staging_dir(&row.label)
            .map_err(|why| why.to_string())?;
        let staging_text = staging.to_str().ok_or("staging path is not UTF-8")?;
        if is_new {
            row.dir = Some(staging_text.to_owned());
            let reserved = logins.shared.required(&row)? == Some(grant.source)
                && logins
                    .get(&row.label)
                    .is_some_and(|binding| binding.dir.is_none());
            if !reserved {
                store.check_new_rows(std::slice::from_ref(&row), &raw, env.now)?;
            }
        }
        let mut request = [0; 16];
        aterm_uds::rand::fill(&mut request).map_err(|why| why.to_string())?;
        if request == [0; 16] {
            return Err("invalid shared adoption request".into());
        }
        let held = store
            .acquire_within(&accounts::LOCK_LADDER)
            .map_err(|why| why.to_string())?
            .ok_or(accounts::LOCK_HELD)?;
        let mut token = None;
        store.transact_sync_held(
            &held,
            env.now,
            &mut None,
            accounts::SyncTarget {
                path: &target,
                before: before.as_deref(),
                after: before.as_deref(),
                mode,
            },
            |_, current| {
                if !super::super::accounts_sync::shared_adoption_on(roster_path) {
                    return Err("automatic shared adoption is off".into());
                }
                authority.current(&grant, &row)?;
                if accounts::roster_text(&target)? != before {
                    return Err("local account roster changed before shared adoption".into());
                }
                before_parent.require_current(current, &row.label)?;
                if let Some(owner) = accounts::retained_directory_owner(
                    current,
                    &raw.rows,
                    &row.label,
                    staging_text,
                )? {
                    return Err(format!("shared adoption staging is retained by `{owner}`"));
                }
                token = Some(
                    current.shared.begin(
                        Intent {
                            source: grant.source,
                            decision: grant.decision,
                            generation: grant.generation,
                            configuration: grant.configuration,
                            request,
                            label: row.label.clone(),
                            identity: email.clone(),
                            staging: staging_text.into(),
                            before_generation: current
                                .get(&row.label)
                                .map(|binding| binding.renewal_generation),
                            begun_at: env.now,
                        },
                        grant.expected_principal,
                    )?,
                );
                Ok(vec![
                    accounts::Event::new(env.now, accounts::Kind::SignIn, &row.label)
                        .because("shared adoption begun"),
                ])
            },
            || Ok(()),
        )?;
        (staging, token, true)
    };
    let staging_text = staging.to_str().ok_or("staging path is not UTF-8")?;
    if is_new {
        row.dir = Some(staging_text.to_owned());
    }
    let own_dir = raw.row(&row.label).and_then(|old| old.dir.as_deref());
    // The ordinary local publication path assigned the pending attempt its
    // binding generation, even though no executable roster row exists yet.
    let begun = store.load_logins(env.now)?.0;
    let retained = begun
        .shared
        .latest(grant.source)
        .filter(|attempt| {
            attempt.intent.decision == grant.decision
                && attempt.intent.generation == grant.generation
                && attempt.intent.configuration == grant.configuration
                && matches!(attempt.outcome, Outcome::Pending | Outcome::NotStarted)
        })
        .ok_or("shared adoption has no current retained attempt")?;
    let parent = retained
        .intent
        .before_generation
        .ok_or("shared adoption has no pending binding fence")?;
    let custody_current = |current: &accounts::Logins| {
        if current
            .get(&row.label)
            .is_none_or(|binding| binding.renewal_generation != parent)
        {
            return Err("local shared account binding changed during adoption".to_owned());
        }
        if let Some(owner) =
            accounts::retained_directory_owner(current, &raw.rows, &row.label, staging_text)?
        {
            return Err(format!("shared adoption staging is retained by `{owner}`"));
        }
        Ok(())
    };
    let current = || {
        if !super::super::accounts_sync::shared_adoption_on(roster_path) {
            return Err("automatic shared adoption is off".into());
        }
        if accounts::roster_text(&target)? != before {
            return Err("local account roster changed during shared adoption".into());
        }
        custody_current(&store.load_logins(env.now)?.0)?;
        authority.current(&grant, &row)
    };
    let request_recovery = |outcome| {
        current()?;
        let latest = store.load_logins(host.now().max(env.now))?.0;
        custody_current(&latest)?;
        let attempt = latest
            .shared
            .latest(grant.source)
            .filter(|attempt| attempt.intent == retained.intent && attempt.outcome == outcome)
            .ok_or("shared recovery attempt changed")?;
        authority.request_recovery(&grant, &row, &attempt.intent)
    };
    current()?;
    if !private_staging_directory(&store.logins_dir())? || !private_staging_directory(&staging)? {
        // Lost local staging cannot supply a proof or safely resume an
        // unstarted flow. Keep its exact outcome and request new authority.
        request_recovery(retained.outcome)?;
        return Err(format!(
            "shared adoption staging is missing: {}",
            staging.display()
        ));
    }
    let native_admission = admission::Current {
        authority: source_authority,
        grant: grant.clone(),
        row: row.clone(),
        store: store.clone(),
        roster: roster_path.to_owned(),
        target: target.clone(),
        before: before.clone(),
        rows: raw.rows.clone(),
        intent: retained.intent.clone(),
        now: env.now,
    }
    .admit(run_provider);
    let context = shared_provider::Context {
        env,
        store,
        host,
        admission: Some(&native_admission),
    };
    let mut execution_error = None;
    if run_provider {
        if selected_browser.is_none() {
            selected_browser = Some(browser(store, host, &row, cli)?);
        }
        if pending.is_some() {
            store.transact_logins(env.now, &mut None, |_, current| {
                authority.current(&grant, &row)?;
                custody_current(current)?;
                if accounts::roster_text(&target)? != before {
                    return Err("local account roster changed before retry".into());
                }
                current
                    .shared
                    .resume_not_started(token.as_ref().ok_or("missing shared attempt")?)?;
                Ok(Vec::new())
            })?;
        }
        match provider.execute_attempt(
            &context,
            &row,
            &staging,
            selected_browser
                .as_ref()
                .ok_or("shared provider has no prepared opener")?,
        ) {
            shared_provider::Execution::Completed => {}
            shared_provider::Execution::Unknown(why) => execution_error = Some(why),
            shared_provider::Execution::NotStarted(why) => {
                store.transact_logins(host.now().max(env.now), &mut None, |_, current| {
                    custody_current(current)?;
                    if accounts::roster_text(&target)? != before {
                        return Err(
                            "local account roster changed before retaining no-effect proof".into(),
                        );
                    }
                    current.shared.not_started(
                        token.as_ref().ok_or("missing shared attempt")?,
                        host.now().max(env.now),
                    )?;
                    Ok(Vec::new())
                })?;
                return Err(why);
            }
        }
        current()?;
    }
    // Recovery observes only the exact retained attempt; no provider replay.
    let observed = provider.observe(&context, &row, &staging);
    let proof = match observed {
        Ok(Some(proof)) => proof,
        Err(shared_provider::ObservationError::Unavailable(why)) => {
            current()?;
            return Err(why);
        }
        missing => {
            request_recovery(Outcome::Pending)?;
            if let Some(why) = execution_error {
                return Err(why);
            }
            return missing.map(|_| false).map_err(|error| error.to_string());
        }
    };
    let principal = proof.principal();
    let identity_custody = store.load_logins(host.now().max(env.now))?.0;
    if !accounts::email_matches(proof.identity(), &email)
        || grant
            .expected_principal
            .is_some_and(|expected| principal != expected)
        || !identity_custody
            .shared
            .principal_allowed(grant.source, principal)
    {
        // A verified account can still contradict the root's pinned identity.
        // Keep its consumed attempt intact and ask for a distinct preparation.
        request_recovery(Outcome::Pending)?;
        return Err("staged shared login contradicts the approved identity".into());
    }
    current()?;
    let after = if is_new {
        accounts::append_rows_for(
            cli,
            before.as_deref().unwrap_or(""),
            std::slice::from_ref(&row),
        )?
    } else {
        before
            .clone()
            .ok_or("existing account roster disappeared")?
    };
    let now = host.now().max(env.now);
    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|why| why.to_string())?
        .ok_or(accounts::LOCK_HELD)?;
    store.transact_sync_held(
        &held,
        now,
        &mut None,
        accounts::SyncTarget {
            path: &target,
            before: before.as_deref(),
            after: Some(&after),
            mode,
        },
        |state, current| {
            authority.current(&grant, &row)?;
            custody_current(current)?;
            if accounts::roster_text(&target)? != before {
                return Err("local account roster changed before publication".into());
            }
            if !super::super::accounts_sync::shared_adoption_on(roster_path) {
                return Err("automatic shared adoption is off".into());
            }
            proof.check_current()?;
            // A bounded file or native credential read can outlive the previous
            // authority observation; it grants no permission to publish later.
            authority.current(&grant, &row)?;
            if !super::super::accounts_sync::shared_adoption_on(roster_path) {
                return Err("automatic shared adoption is off".into());
            }
            let generation = current
                .generation_clock
                .checked_add(1)
                .ok_or("local generation exhausted")?;
            current.shared.finish(
                token.take().ok_or("shared adoption already completed")?,
                Outcome::Published { generation },
                Some((&email, principal)),
                now,
            )?;
            // Pending custody retains this exact staging directory until the
            // atomic sync receipt commits. A separate renewal checkpoint here
            // would outlive a failed pre-receipt edit and obstruct its retry.
            let events = accounts::note_renewed(
                state,
                current,
                &row.label,
                own_dir,
                staging_text,
                proof.subscription(),
                now,
            );
            if events.is_empty() {
                return Err("local login capacity is exhausted".into());
            }
            if is_new {
                state.prepared_accounts.insert(row.label.clone());
                // This account never owned the process's default home.
                if let Some(binding) = current.labels.get_mut(&row.label) {
                    binding.was.clear();
                }
            }
            accounts::note_identity(current, &row.label, accounts::Proof::Status, Some(&email));
            Ok(events)
        },
        || Ok(()),
    )?;
    Ok(true)
}

/// Observe a private directory without creating it or following a leaf link.
fn private_staging_directory(path: &Path) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("shared adoption staging is not a private directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("shared adoption staging is not private".into());
        }
    }
    Ok(true)
}

fn browser(
    store: &accounts::Store,
    host: &dyn AccountsHost,
    row: &accounts::Row,
    cli: Cli,
) -> Result<RenewBrowser, String> {
    match row.browser.as_ref() {
        Some(_) => Err(super::super::account_catalog::NAMED_BROWSER_UNSUPPORTED.into()),
        None if cli == Cli::Claude => Ok(RenewBrowser::Remote {
            opener: Some(opener_link(store, host).map_err(|error| error.to_string())?),
        }),
        None => Ok(RenewBrowser::Default),
    }
}
