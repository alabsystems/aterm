// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Original canonical cancellation custody, not row/provider readiness. This
//! role never reads or writes a credential file or lends response bytes.

use super::{accounts, cli::Env, codex_enrollment, codex_grant_owner};
use std::path::{Path, PathBuf};

/// Pure canonical observation. This has not authenticated an owner response
/// and cannot acquire native resources or mutate a recovery claim.
pub(super) struct RetirementObservation {
    request: codex_enrollment::Request,
    staging: codex_enrollment::Staging,
    reference: Option<codex_grant_owner::GrantReference>,
}
pub(super) fn observe(
    env: &Env,
    request: &codex_enrollment::Request,
) -> Result<RetirementObservation, String> {
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("native retirement Store unavailable")?;
    let (logins, note) = store.load_logins(env.now)?;
    if note.is_some() {
        return Err("native retirement records are uncertain".into());
    }
    logins
        .enrollment
        .require_native_retirement_candidate(request)?;
    let staging = logins
        .enrollment
        .staged(request)?
        .cloned()
        .ok_or("native retirement stage unavailable")?;
    if std::fs::canonicalize(&store.dir).ok().as_ref() != Some(&staging.store)
        || !staging.store_current()
    {
        return Err("native retirement Store changed".into());
    }
    Ok(RetirementObservation {
        request: request.clone(),
        staging,
        reference: logins.enrollment.native_retirement_reference(request)?,
    })
}
/// Called only after a Pending release operation has been counted. Historical
/// authentication alone is not permission to publish or use the credential.
pub(super) fn authenticate_observation(
    env: &Env,
    observation: &RetirementObservation,
) -> Result<(NativeRetirementCandidate, codex_grant_owner::RetainedGrant), String> {
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("native retirement Store unavailable")?;
    let fresh = match &observation.reference {
        Some(reference) => codex_grant_owner::recover(env, &observation.request, reference)?,
        None => codex_grant_owner::recover_pending(env, &observation.request)?,
    };
    let candidate = candidate(env, &store, &observation.request, &fresh)?;
    if candidate.staging() != &observation.staging
        || observation
            .reference
            .as_ref()
            .is_some_and(|reference| reference != candidate.reference())
    {
        return Err("native retirement observation changed".into());
    }
    Ok((candidate, fresh))
}

pub(super) struct NativeRetirementCandidate {
    source_home: PathBuf,
    request: codex_enrollment::Request,
    reference: codex_grant_owner::GrantReference,
    staging: codex_enrollment::Staging,
}
impl std::fmt::Debug for NativeRetirementCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRetirementCandidate(release only, redacted)")
    }
}
impl NativeRetirementCandidate {
    pub(super) fn source_home(&self) -> &Path {
        &self.source_home
    }
    pub(super) fn request(&self) -> &codex_enrollment::Request {
        &self.request
    }
    pub(super) fn reference(&self) -> &codex_grant_owner::GrantReference {
        &self.reference
    }
    pub(super) fn staging(&self) -> &codex_enrollment::Staging {
        &self.staging
    }
    pub(super) fn check_store(&self, store: &accounts::Store) -> Result<(), String> {
        if store.cli() != accounts::Cli::Codex
            || std::fs::canonicalize(&store.dir).ok().as_ref() != Some(&self.staging.store)
            || !self.staging.store_current()
        {
            return Err("native retirement Store changed".into());
        }
        Ok(())
    }
    fn require_image(&self, logins: &accounts::Logins) -> Result<(), String> {
        logins
            .enrollment
            .require_native_retirement_candidate(&self.request)?;
        if logins.enrollment.staged(&self.request)? != Some(&self.staging)
            || logins
                .enrollment
                .native_retirement_reference(&self.request)?
                .as_ref()
                .is_some_and(|reference| reference != &self.reference)
        {
            return Err("native retirement original custody changed".into());
        }
        Ok(())
    }
}

pub(super) fn candidate(
    env: &Env,
    store: &accounts::Store,
    request: &codex_enrollment::Request,
    fresh: &codex_grant_owner::RetainedGrant,
) -> Result<NativeRetirementCandidate, String> {
    if !fresh.matches_request(request) {
        return Err("native retirement response changed".into());
    }
    let configured = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("native retirement Store unavailable")?;
    if std::fs::canonicalize(&configured.dir).ok() != std::fs::canonicalize(&store.dir).ok() {
        return Err("native retirement Store is not the admitted source".into());
    }

    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|_| "native retirement writer unavailable")?
        .ok_or("native retirement writer busy")?;
    let snapshot = store.native_retirement_snapshot_held(&held, env.now)?;
    let logins = snapshot.retained_image();
    logins
        .enrollment
        .require_native_retirement_candidate(request)?;
    let candidate = NativeRetirementCandidate {
        source_home: std::fs::canonicalize(
            env.home
                .as_deref()
                .ok_or("native retirement home unavailable")?,
        )
        .map_err(|_| "native retirement home unavailable")?,
        request: request.clone(),
        reference: fresh.reference(),
        staging: logins
            .enrollment
            .staged(request)?
            .cloned()
            .ok_or("native retirement original stage unavailable")?,
    };
    candidate.check_store(store)?;
    candidate.require_image(logins)?;
    snapshot.current()?;
    Ok(candidate)
}

pub(super) fn claim(
    env: &Env,
    store: &accounts::Store,
    resources: super::native_enrollment_admission::NativeRetirementResources,
    admitted_work: &codex_enrollment::Admission<'_>,
) -> Result<super::account_work::ReleaseTurn, String> {
    claim_record(env, store, &resources, |receipt| {
        admitted_work.native_recovery(receipt)
    })?;
    resources
        .release_after_retirement()
        .ok_or_else(|| "native retirement custody changed".into())
}
pub(super) fn claim_released(
    env: &Env,
    store: &accounts::Store,
    resources: super::native_enrollment_admission::NativeRetirementResources,
    admitted: &codex_enrollment::release::ReleaseAdmission<'_>,
) -> Result<(), String> {
    claim_record(env, store, &resources, |receipt| {
        admitted.native_recovery(receipt)
    })?;
    if resources.release_after_retirement().is_some() {
        return Err("native retirement custody changed".into());
    }
    Ok(())
}
fn claim_record(
    env: &Env,
    store: &accounts::Store,
    resources: &super::native_enrollment_admission::NativeRetirementResources,
    track: impl FnOnce(&codex_enrollment::RecoveryOwnership) -> Result<(), String>,
) -> Result<(), String> {
    resources.check_store(store)?;
    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|_| "native retirement writer unavailable")?
        .ok_or("native retirement writer busy")?;
    let snapshot = store.native_retirement_snapshot_held(&held, env.now)?;
    resources
        .candidate()
        .require_image(snapshot.retained_image())?;
    store.transact_logins_held(&held, env.now, &mut None, |_, logins| {
        snapshot.current()?;
        resources.check_store(store)?;
        resources.candidate().require_image(logins)?;
        let receipt = logins
            .enrollment
            .claim_native_retirement_recovery(resources)?;
        track(&receipt)?;
        Ok(Vec::new())
    })?;
    resources.check_store(store)?;
    Ok(())
}
