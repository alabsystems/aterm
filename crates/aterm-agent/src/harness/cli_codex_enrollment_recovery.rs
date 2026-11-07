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
        native_env: native_required.then(|| env.clone()),
        native_row_occurrence: None,
        native_row_admission: None,
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
        Some(env),
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

/// Recovery of an exact retired fenced response. This path has no provider,
/// browser or code-exchange call and cannot construct a legacy SigninTurn.
pub(super) fn recover_native_row_enrollment(
    env: &Env,
    host: &dyn AccountsHost,
    request: &crate::harness::codex_enrollment::Request,
    (store, roster): (&accounts::Store, &Path),
    admitted_work: &crate::harness::codex_enrollment::Admission<'_>,
    native: &crate::harness::codex_enrollment::NativeContext<'_>,
) -> Result<(), String> {
    if host.work_cancelled() || native.scope.cancelled() {
        return Ok(());
    }
    if native.env.home != env.home
        || native.env.config != env.config
        || native.env.state != env.state
    {
        return Err("native recovery environment changed".into());
    }
    let records = store
        .peek_account_records(roster, env.now)
        .map_err(|_| "native recovery records unavailable")?;
    records.logins.enrollment.require_running(request)?;
    if records
        .logins
        .enrollment
        .require_native_recovery_candidate(request)
        .is_err()
    {
        return Ok(());
    }
    let staging = records
        .logins
        .enrollment
        .staged(request)?
        .cloned()
        .ok_or("native recovery has no original staging")?;
    if staging.native_row.is_none() || !records.logins.enrollment.requires_native_grant(request)? {
        return Err("native recovery has no fenced issuer".into());
    }
    let fresh = match records
        .logins
        .enrollment
        .native_recovery_reference(request)?
    {
        Some(reference) => crate::harness::codex_grant_owner::recover(env, request, &reference),
        None => crate::harness::codex_grant_owner::recover_pending(env, request),
    };
    let Ok(fresh) = fresh else {
        return Ok(());
    };
    // A new publication-only admission owns this bounded deadline. It does
    // not extend a provider attempt or authorize another authorization POST.
    let deadline =
        crate::harness::wake_deadline::WakeDeadline::after(crate::harness::login_drive::HANG);
    let candidate = crate::harness::codex_native_row::recovery_candidate(
        env,
        (store, roster),
        request,
        &fresh,
    )?;
    let resources = crate::harness::codex_grant_owner::admit_native_recovery_publication(
        env,
        store,
        candidate,
        &fresh,
        native.scope,
        deadline,
    )?;
    // Waiting for kernel resources cannot mutate a canonical recovery claim.
    // Once acquired, these exact resources fence any superseded native writer.
    let turn = crate::harness::codex_native_row::claim_retired_recovery(
        env,
        (store, roster),
        resources,
        &fresh,
        admitted_work,
    )?;
    let claim = turn.admission().claim();
    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|_| "native recovery writer unavailable")?
        .ok_or("native recovery writer busy")?;
    let digest = crate::harness::codex_native_row::with_current_row(
        env,
        (store, &held),
        roster,
        env.now,
        request,
        &fresh,
        |current| {
            current.require_recovery(turn.admission())?;
            turn.check_store(store)?;
            // The held response lends its own exact bytes/witness/AuthLock;
            // the bridge never reopens the original owner or infers a grant.
            let digest =
                crate::harness::codex_app_login::reconcile_native_recovery(&current, store, &turn)?;
            store.transact_roster_logins_held(
                &held,
                roster,
                env.now,
                &mut None,
                |raw, state, logins| {
                    current.require_edit_image(raw, state, logins)?;
                    current.require_recovery(turn.admission())?;
                    logins.enrollment.require_native_publication_recovery(
                        request,
                        turn.admission().ownership(),
                    )?;
                    turn.check_store(store)?;
                    logins.enrollment.attach_grant(request, &fresh)?;
                    Ok(Vec::new())
                },
            )?;
            Ok(digest)
        },
    )?;
    drop(held);
    if host.work_cancelled() || native.scope.cancelled() || deadline.passed() {
        return Ok(());
    }
    let current = store
        .peek_account_records(roster, env.now)
        .map_err(|_| "native recovery records unavailable")?;
    let raw = current
        .raw_roster
        .row(&request.label)
        .ok_or("native recovery row removed")?;
    let row = current
        .roster
        .row(&request.label)
        .ok_or("native recovery row removed")?;
    if row.held_by.is_some() {
        return Err("native recovery row is held".into());
    }
    let email = crate::harness::codex_app_login::read_account_email(&staging.directory)
        .ok_or("native recovery identity unavailable")?;
    if row
        .expected_identity_email()
        .is_none_or(|expected| !accounts::email_matches(expected, &email))
    {
        return Err("native recovery identity differs".into());
    }
    let route = Cli::Codex
        .accounts()
        .signin
        .as_ref()
        .and_then(|signin| signin.route(row.flow(Cli::Codex)))
        .ok_or("native recovery route unavailable")?;
    let begun = Begun::native_recovery(turn, store, &request.label)?;
    let plan = SignInPlan {
        native_env: Some(env.clone()),
        native_row_occurrence: None,
        native_row_admission: None,
        publication: crate::harness::codex_app_login::PublicationCustody::default(),
        enrollment: Some(request.clone()),
        native_required: true,
        native_grant: Some(fresh),
        artifact: None,
        recovered: Some(Publication {
            admission: staging.clone(),
            digest,
            deadline,
            claim,
        }),
        route,
        hosts: route
            .hosts
            .of(row.vendor.region)
            .ok_or("native recovery hosts unavailable")?,
        staging: staging.directory.clone(),
        staging_text: staging
            .directory
            .to_str()
            .ok_or("native recovery staging invalid")?
            .to_owned(),
        parent: staging.parent,
        workflow: accounts::current_workflow(raw, &current.logins)
            .ok_or("native recovery workflow changed")?,
        #[cfg(unix)]
        native_google: None,
        native_recovery: None,
        native_admission: None,
        #[cfg(unix)]
        native_parent: None,
        #[cfg(unix)]
        reauthentication: None,
        _begun: begun,
    };
    let _ = bind_signed_in(
        Cli::Codex,
        env.now,
        host,
        Some(env),
        store,
        (row, roster),
        raw.dir.as_deref(),
        &plan,
        accounts::Proof::Status,
        "recover",
        &mut io::sink(),
        &mut io::sink(),
    );
    cleanup_codex_enrollments(store, roster, env.now, &staging.directory);
    Ok(())
}

/// Cancelled native publication recovery is retirement-only. It cannot restore
/// the Main response, write a credential, initialize C1 or publish an account.
pub(super) fn retire_cancelled_native_row(
    env: &Env,
    request: &crate::harness::codex_enrollment::Request,
    store: &accounts::Store,
    admitted_work: &crate::harness::codex_enrollment::Admission<'_>,
    native: &crate::harness::codex_enrollment::NativeContext<'_>,
) -> Result<(), String> {
    if native.env.home != env.home
        || native.env.config != env.config
        || native.env.state != env.state
    {
        return Err("native retirement environment changed".into());
    }
    let release = native
        .scope
        .release_turn()
        .ok_or("native retirement cleanup is sealed")?;
    let (logins, note) = store
        .load_logins(env.now)
        .map_err(|_| "native retirement records unavailable")?;
    if note.is_some() {
        return Err("native retirement records are uncertain".into());
    }
    logins
        .enrollment
        .require_native_retirement_candidate(request)?;
    let fresh = match logins.enrollment.native_retirement_reference(request)? {
        Some(reference) => crate::harness::codex_grant_owner::recover(env, request, &reference)?,
        None => crate::harness::codex_grant_owner::recover_pending(env, request)?,
    };
    let deadline =
        crate::harness::wake_deadline::WakeDeadline::after(crate::harness::login_drive::HANG);
    let candidate =
        crate::harness::codex_native_retirement::candidate(env, store, request, &fresh)?;
    let resources = crate::harness::codex_grant_owner::admit_native_retirement(
        env,
        store,
        candidate,
        &fresh,
        native.scope,
        release,
        deadline,
    )?;
    let release =
        crate::harness::codex_native_retirement::claim(env, store, resources, admitted_work)?;
    let mut retained = native
        .retirement
        .lock()
        .map_err(|_| "native retirement cleanup custody unavailable")?;
    if retained.is_some() {
        return Err("native retirement already owns cleanup".into());
    }
    *retained = Some(release);
    Ok(())
}
