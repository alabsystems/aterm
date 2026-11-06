// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renew a rejected direct credential without giving native auth its live home.

use super::*;

#[cfg(test)]
#[path = "agy_signin_direct_refresh_tests.rs"]
mod tests;

pub(super) fn run_with(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    deadline: Instant,
    mut read: impl FnMut(
        &str,
        &str,
        Duration,
    ) -> Result<
        super::super::super::google_profile::GoogleProfile,
        super::super::super::google_profile::Error,
    >,
    fresh: impl FnOnce(
        &Path,
        &Path,
        &Path,
        &str,
        Option<StagedGoogleLogin<'_>>,
        Instant,
    ) -> Result<UnverifiedGoogleGrant, GoogleLoginError>,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    let failure = || GoogleLoginError::Native(Error::Profile);
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    if !state.is_absolute() || !home.is_absolute() {
        return Err(GoogleLoginError::Configuration);
    }
    let registry = registry(state).ok_or(GoogleLoginError::Configuration)?;
    let (identity, binding) = registry
        .resolve("agy", account)
        .ok_or(GoogleLoginError::Configuration)?;
    let scope = current_checkpoint_scope(state, account, binding.expected_email())
        .ok_or(GoogleLoginError::Configuration)?;
    if !program.is_absolute() || !program.is_file() {
        return Err(GoogleLoginError::Native(Error::Profile));
    }
    let mut key = aterm_digest::Sha256::new();
    key.update(b"aterm.agy-direct-recovery.v1\0");
    let (mut journal, _) = Journal::open(
        state,
        home,
        account,
        scope,
        RecoveryKey(key.finalize()),
        "pending-native-google-direct",
    )
    .map_err(GoogleLoginError::Recovery)?;
    let profile = native_profile(state, account, identity);
    aterm_secrets::fsx::ensure_private_dir(&profile).map_err(|_| failure())?;
    let claim =
        super::super::super::browser_window::ProfileLock::claim(&profile).map_err(|_| failure())?;
    let path = profile.join(".gemini/antigravity-cli/antigravity-oauth-token");
    let original = aterm_secrets::fsx::read_private(&path, LIMIT).map_err(|_| failure())?;
    let access = original
        .as_ref()
        .and_then(|bytes| native_access_token(bytes.as_slice()).ok());
    let rejected = if let Some(access) = access {
        let verdict = read(
            access.as_str().ok_or_else(failure)?,
            binding.expected_email(),
            remaining(deadline)
                .map_err(GoogleLoginError::Native)?
                .min(Duration::from_secs(30)),
        );
        remaining(deadline).map_err(GoogleLoginError::Native)?;
        journal.unchanged().map_err(GoogleLoginError::Recovery)?;
        if current_checkpoint_scope(state, account, binding.expected_email()) != Some(scope) {
            return Err(GoogleLoginError::Configuration);
        }
        match verdict {
            Ok(_) => false,
            Err(super::super::super::google_profile::Error::CredentialRejected) => true,
            Err(_) => {
                return Err(GoogleLoginError::Recovery(
                    "Direct Google verification is unavailable or refused",
                ));
            }
        }
    } else {
        false
    };
    if !rejected {
        drop(claim);
        drop(journal);
        return super::direct_with(program, state, home, account, deadline, fresh);
    }
    let original = original.ok_or_else(failure)?;
    let baseline = grant_digest(original.as_slice());
    let current =
        || current_checkpoint_scope(state, account, binding.expected_email()) == Some(scope);
    let unchanged = || -> Result<(), GoogleLoginError> {
        if !current() {
            return Err(GoogleLoginError::Configuration);
        }
        if grant_digest(grant_bytes(&path).map_err(|_| failure())?.as_slice()) != baseline {
            return Err(failure());
        }
        Ok(())
    };
    unchanged()?;
    let seed = aterm_tempfile::tempdir().map_err(|_| failure())?;
    let _seed_claim = stage_profile(seed.path(), None).map_err(GoogleLoginError::Native)?;
    let seed_path = seed
        .path()
        .join(".gemini/antigravity-cli/antigravity-oauth-token");
    aterm_secrets::fsx::ensure_private_dir(seed_path.parent().ok_or_else(failure)?)
        .map_err(|_| failure())?;
    aterm_secrets::fsx::write_atomic(&seed_path, original.as_slice(), 0o600)
        .map_err(|_| failure())?;
    prepare_rejected_staging(
        seed.path(),
        &StagedRecovery {
            expected_email: binding.expected_email(),
            current: &current,
        },
        deadline,
        |_, _| Err(super::super::super::google_profile::Error::CredentialRejected),
    )
    .map_err(GoogleLoginError::Native)?;
    let staging = aterm_tempfile::tempdir().map_err(|_| failure())?;
    let mut key = aterm_digest::Sha256::new();
    key.update(b"aterm.agy-direct-renewal.v1\0");
    key.update(baseline);
    let staged = run_with_journal(
        program,
        state,
        home,
        account,
        StagedGoogleLogin {
            profile: staging.path(),
            previous: seed_path.is_file().then_some(seed.path()),
            expected_email: binding.expected_email(),
            recovery: Some(RecoveryKey(key.finalize())),
        },
        deadline,
        |token, expected, wait| read(token, expected, wait).map(drop),
        fresh,
        ("pending-native-google-direct-renewal", None),
    )?;
    let mut verified = staged
        .verify_with_retry(deadline, &mut read, std::thread::sleep, Instant::now)
        .map_err(|_| {
            GoogleLoginError::Recovery("Renewed direct Google credential could not be verified")
        })?;
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    unchanged()?;
    journal.unchanged().map_err(GoogleLoginError::Recovery)?;
    verified
        .check_current()
        .map_err(GoogleLoginError::Recovery)?;
    let replacement = grant_bytes(verified.grant.path()).map_err(GoogleLoginError::Recovery)?;
    // Keep both encrypted completions before publishing. A crash before publication
    // resumes the renewal under the exact old live digest; one after it resumes direct.
    journal.capture(&verified.grant, identity, home)?;
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    unchanged()?;
    verified
        .check_current()
        .map_err(GoogleLoginError::Recovery)?;
    journal.unchanged().map_err(GoogleLoginError::Recovery)?;
    aterm_secrets::fsx::write_atomic(&path, replacement.as_slice(), 0o600)
        .map_err(|_| failure())?;
    verified.grant.native.path = path;
    verified.grant.native._claim = claim;
    verified
        .finish_recovery()
        .map_err(GoogleLoginError::Recovery)?;
    verified.grant.recovery = Some(journal);
    verified
        .check_current()
        .map_err(GoogleLoginError::Recovery)?;
    Ok(verified.grant)
}
