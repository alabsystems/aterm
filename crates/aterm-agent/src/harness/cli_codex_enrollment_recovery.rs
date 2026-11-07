// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Publication-only recovery of an exact retired native enrollment attempt.

use super::*;

pub(super) struct Publication {
    pub admission: crate::harness::codex_enrollment::Staging,
    pub digest: String,
    pub deadline: crate::harness::wake_deadline::WakeDeadline,
    pub claim: u64,
}

pub(super) fn require_current(
    admission: &crate::harness::codex_enrollment::Staging,
    claim: Option<u64>,
    store: &accounts::Store,
    raw: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
) -> Result<(), String> {
    logins.enrollment.require_running(&admission.request)?;
    if let Some(claim) = claim {
        logins
            .enrollment
            .require_recovery(&admission.request, claim)?;
    } else if !logins.enrollment.is_retired(&admission.request)? {
        return Err("enrollment resources remain owned".into());
    }
    require_binding(admission, store, raw, state, logins)
}

pub(super) fn require_binding(
    admission: &crate::harness::codex_enrollment::Staging,
    store: &accounts::Store,
    raw: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
) -> Result<(), String> {
    logins.enrollment.require_running(&admission.request)?;
    if !admission.directories_current()
        || logins.enrollment.staged(&admission.request)? != Some(admission)
        || state
            .pending_pin
            .as_ref()
            .map(accounts::PendingSelection::identity)
            != admission.selection
        || accounts::pool_authority_revision(raw, logins)? != admission.policy
        || std::fs::canonicalize(&store.dir).ok().as_ref() != Some(&admission.store)
        || std::fs::canonicalize(&admission.directory).ok().as_ref() != Some(&admission.directory)
        || std::fs::canonicalize(store.logins_dir()).ok().as_deref() != admission.directory.parent()
    {
        return Err("enrollment recovery authority changed".into());
    }
    admission
        .parent
        .require_current(logins, &admission.request.label)?;
    let row = raw
        .row(&admission.request.label)
        .ok_or("enrollment row removed")?;
    if accounts::current_workflow(row, logins)
        .is_none_or(|token| token.value() != admission.workflow)
    {
        return Err("enrollment workflow changed".into());
    }
    Ok(())
}

pub(super) fn checkpoint_matches(
    admission: &crate::harness::codex_enrollment::Staging,
    checkpoint: &accounts::RenewalCheckpoint,
    email: &str,
) -> bool {
    checkpoint.enrollment.as_ref() == Some(&admission.request)
        && checkpoint.label == admission.request.label
        && checkpoint.email.eq_ignore_ascii_case(email)
        && Some(checkpoint.dir.as_str()) == admission.directory.to_str()
        && checkpoint.parent == admission.parent
        && checkpoint.proved_by == Some(accounts::Proof::Status)
        && checkpoint.region.is_none()
        && checkpoint.subscription.is_none()
        && checkpoint.google_principal.is_none()
        && checkpoint.google_grant_digest.is_none()
        && checkpoint.google_binding_scope.is_none()
}

fn checkpoint_available(
    admission: &crate::harness::codex_enrollment::Staging,
    email: &str,
    now: i64,
) -> bool {
    match std::fs::symlink_metadata(admission.directory.join(accounts::RENEWAL_CHECKPOINT_FILE)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(_) => false,
        Ok(_) => {
            matches!(accounts::read_renewal_checkpoint_checked_for(Cli::Codex, &admission.directory, &admission.directory, now), Ok(Some(checkpoint)) if checkpoint_matches(admission, &checkpoint, email))
        }
    }
}

pub(in crate::harness) fn cleanup_codex_enrollments(
    store: &accounts::Store,
    roster: &Path,
    now: i64,
    target: &Path,
) {
    // Only this trusted Store's fenced metadata supplies locations. A credential journal
    // never supplies a Store or roster path, and no account writer is taken under AuthLock.
    let Ok(records) = store.peek_account_records(roster, now) else {
        return;
    };
    let Ok(actual_store) = std::fs::canonicalize(&store.dir) else {
        return;
    };
    let Ok(logins) = std::fs::canonicalize(store.logins_dir()) else {
        return;
    };
    let home = target;
    let Some(applied) = records
        .logins
        .enrollment
        .applied_artifact(home, &records.logins.labels)
    else {
        return;
    };
    let admission = applied.admission();
    if admission.store == actual_store
        && admission.directory.parent() == Some(logins.as_path())
        && std::fs::canonicalize(home).ok().as_deref() == Some(home)
        && admission.directories_current()
    {
        let _ = crate::harness::codex_app_login::cleanup_applied_enrollment(&applied);
    }
}

pub(super) fn recover_codex_enrollment(
    env: &Env,
    host: &dyn AccountsHost,
    request: &crate::harness::codex_enrollment::Request,
    store: &accounts::Store,
    roster_path: &Path,
    admitted_work: &crate::harness::codex_enrollment::Admission<'_>,
) -> Result<(), String> {
    let deadline =
        crate::harness::wake_deadline::WakeDeadline::after(crate::harness::login_drive::HANG);
    if host.work_cancelled() {
        return Ok(());
    }
    let signer = match store.signin_now() {
        Ok(Some(signer)) => signer,
        _ => return Ok(()),
    };
    signer
        .check_store(store)
        .map_err(|_| "enrollment signer changed")?;
    let records = store
        .read_account_records(Some(roster_path), env.now)
        .map_err(|_| "enrollment records unavailable")?;
    records.logins.enrollment.require_running(request)?;
    if !records.logins.enrollment.is_retired(request)? {
        return Ok(());
    }
    let admission = records
        .logins
        .enrollment
        .staged(request)?
        .cloned()
        .ok_or("enrollment has no bound native attempt")?;
    require_current(
        &admission,
        None,
        store,
        &records.raw_roster,
        &records.state,
        &records.logins,
    )?;
    // Publication-only recovery must preserve the original admission's grant
    // requirement. The legacy Artifact journal cannot replace issuer-authenticated
    // custody, and a plain surviving candidate is never a grant constructor.
    let native_required = records.logins.enrollment.requires_native_grant(request)?;
    if !native_required
        && !crate::harness::codex_app_login::enrollment_response_available(&admission)
    {
        return Ok(());
    }
    let native_grant = if native_required {
        let recovered = match records.logins.enrollment.checkpoint_reference(request) {
            Ok(Some(reference)) => {
                crate::harness::codex_grant_owner::recover(env, request, &reference)
            }
            _ => crate::harness::codex_grant_owner::recover_pending(env, request),
        };
        let Ok(grant) = recovered else {
            return Ok(());
        };
        Some(grant)
    } else {
        None
    };
    let raw = records
        .raw_roster
        .row(&request.label)
        .ok_or("enrollment row removed")?;
    let row = records
        .roster
        .row(&request.label)
        .ok_or("enrollment row removed")?;
    if row.held_by.is_some() {
        return Err("enrollment workflow changed".into());
    }
    if !checkpoint_available(
        &admission,
        renewal_identity_email(Cli::Codex, row).unwrap_or(""),
        env.now,
    ) {
        return Ok(());
    }
    let work = host.work_scope();
    let mut claim = None;
    store.transact_roster_logins_within(
        &accounts::LOCK_LADDER,
        roster_path,
        env.now,
        &mut None,
        |raw, state, logins| {
            require_current(&admission, None, store, raw, state, logins)?;
            signer
                .check_store(store)
                .map_err(|_| "enrollment signer changed")?;
            if deadline.passed() || host.work_cancelled() {
                return Err("enrollment recovery work expired".into());
            }
            claim = Some(logins.enrollment.claim_recovery(request)?);
            Ok(Vec::new())
        },
    )?;
    let claim = claim.ok_or("enrollment recovery claim unavailable")?;
    admitted_work.recovery(claim)?;
    let mut digest = None;
    let materialized = store.transact_roster_logins_within(
        &accounts::LOCK_LADDER,
        roster_path,
        env.now,
        &mut None,
        |raw, state, logins| {
            require_current(&admission, Some(claim), store, raw, state, logins)?;
            if let Some(grant) = &native_grant {
                logins.enrollment.attach_grant(request, grant)?;
            }
            if deadline.passed() || host.work_cancelled() {
                return Err("enrollment recovery work expired".into());
            }
            digest = Some(match &native_grant {
                Some(grant) => crate::harness::codex_app_login::reconcile_retained_enrollment(
                    env,
                    &admission,
                    grant,
                    deadline,
                    work.as_ref(),
                    store,
                    &signer,
                )?,
                None => crate::harness::codex_app_login::materialize_enrollment(
                    &admission,
                    deadline,
                    work.as_ref(),
                    store,
                    &signer,
                )?,
            });
            Ok(Vec::new())
        },
    );
    if materialized.is_err() {
        return Ok(());
    }
    let digest = digest.ok_or("enrollment response unavailable")?;
    if host.work_cancelled() || deadline.passed() {
        return Ok(());
    }
    let email = crate::harness::codex_app_login::read_account_email(&admission.directory)
        .ok_or("enrollment identity unavailable")?;
    if row
        .expected_identity_email()
        .is_none_or(|expected| !accounts::email_matches(expected, &email))
    {
        return Err("enrollment identity differs".into());
    }
    let route = Cli::Codex
        .accounts()
        .signin
        .as_ref()
        .and_then(|signin| signin.route(row.flow(Cli::Codex)))
        .ok_or("enrollment route unavailable")?;
    let plan = SignInPlan {
        publication: crate::harness::codex_app_login::PublicationCustody::default(),
        enrollment: Some(request.clone()),
        native_required,
        native_grant,
        artifact: None,
        recovered: Some(Publication {
            admission: admission.clone(),
            digest,
            deadline,
            claim,
        }),
        route,
        hosts: route
            .hosts
            .of(row.vendor.region)
            .ok_or("enrollment hosts unavailable")?,
        staging: admission.directory.clone(),
        staging_text: admission
            .directory
            .to_str()
            .ok_or("enrollment staging unavailable")?
            .to_owned(),
        parent: admission.parent,
        workflow: accounts::current_workflow(raw, &records.logins)
            .ok_or("enrollment workflow changed")?,
        #[cfg(unix)]
        native_google: None,
        native_recovery: None,
        native_admission: None,
        #[cfg(unix)]
        native_parent: None,
        #[cfg(unix)]
        reauthentication: None,
        _begun: Begun::recovery(signer, store, &request.label)?,
    };
    let _ = bind_signed_in(
        Cli::Codex,
        env.now,
        host,
        store,
        (row, roster_path),
        raw.dir.as_deref(),
        &plan,
        accounts::Proof::Status,
        "recover",
        &mut io::sink(),
        &mut io::sink(),
    );
    cleanup_codex_enrollments(store, roster_path, env.now, &admission.directory);
    Ok(())
}
