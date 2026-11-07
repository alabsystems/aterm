// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Join an issuer-authenticated response to its original enrollment journal.
//! No response is inferred from a surviving credential or a matching principal.

use super::*;

#[cfg(unix)]
pub(in crate::harness) fn reconcile_retained_enrollment(
    env: &super::super::cli::Env,
    admission: &super::super::codex_enrollment::Staging,
    grant: &super::super::codex_grant_owner::RetainedGrant,
    deadline: WakeDeadline,
    work: Option<&super::super::account_work::Scope>,
    store: &super::super::accounts::Store,
    signer: &super::super::accounts::SigninTurn,
) -> Result<String, String> {
    super::super::codex_grant_owner::with_retained_response(
        env,
        &admission.request,
        grant,
        |_identity, document, witness, held, predecessor| {
            reconcile_original_held(
                admission,
                (document, witness, held, predecessor),
                deadline,
                work,
                &EnrollmentPublicationGuard::Legacy { store, signer },
            )
        },
    )
}

#[cfg(unix)]
fn reconcile_original_held(
    admission: &super::super::codex_enrollment::Staging,
    (document, witness, held, predecessor): (
        &[u8],
        &super::super::codex_app_login::StagedWitness,
        &AuthLock,
        Option<Option<&str>>,
    ),
    deadline: WakeDeadline,
    work: Option<&super::super::account_work::Scope>,
    guard: &EnrollmentPublicationGuard<'_>,
) -> Result<String, String> {
    let home = &admission.directory;
    let auth = auth_path_in_home(home);
    let check = || -> Result<(), String> {
        if !admission.directories_current()
            || witness.coordinates()?.0 != auth
            || !witness.matches_auth_file(&auth)
        {
            return Err("retained enrollment authority changed".into());
        }
        guard.check(admission)?;
        if work.is_some_and(super::super::account_work::Scope::cancelled) || deadline.passed() {
            return Err("retained enrollment work stopped".into());
        }
        Ok(())
    };
    check()?;
    let digest = credential_digest(document);
    let current = match aterm_uds::private_file::read_private(home, AUTH_FILE, AUTH_FILE_MAX) {
        Ok(bytes) => Some(aterm_secrets::SecretBytes::new(bytes)),
        Err(aterm_uds::private_file::PrivateFileError::Missing) => None,
        Err(_) => return Err("retained enrollment credential unavailable".into()),
    };
    let current_digest = current
        .as_ref()
        .map(|bytes| credential_digest(bytes.as_slice()));
    let trusted_predecessor = predecessor.map(|digest| digest.map(str::to_owned));
    let exact = current
        .as_ref()
        .is_some_and(|bytes| bytes.as_slice() == document);
    if current.is_some() && !exact && trusted_predecessor.as_ref() != Some(&current_digest) {
        return Err("retained enrollment predecessor differs".into());
    }
    let journal =
        match RefreshJournal::read(home).map_err(|_| "retained enrollment journal unavailable")? {
            Some(journal @ RefreshJournal::EnrollmentResponse { .. }) => {
                let RefreshJournal::EnrollmentResponse {
                    admission: recorded,
                    candidate,
                    digest: recorded_digest,
                    predecessor,
                } = &journal
                else {
                    unreachable!()
                };
                if recorded.as_ref() != admission
                    || candidate.0.as_bytes() != document
                    || recorded_digest != &digest
                    || trusted_predecessor
                        .as_ref()
                        .is_some_and(|expected| expected != predecessor)
                    || (!exact && current.is_some() && trusted_predecessor.is_none())
                {
                    return Err("retained enrollment response differs".into());
                }
                journal
            }
            None => RefreshJournal::EnrollmentResponse {
                admission: Box::new(admission.clone()),
                predecessor: trusted_predecessor.clone().unwrap_or(current_digest),
                candidate: NativeCredential(
                    std::str::from_utf8(document)
                        .map_err(|_| "retained enrollment encoding invalid")?
                        .to_owned(),
                ),
                digest: digest.clone(),
            },
            Some(_) => return Err("retained enrollment journal has another owner".into()),
        };
    // Retain the exact existing predecessor, and make the journal
    // durable before materialization under the same native AuthLock.
    journal
        .write_before_publish(home, || check().map_err(std::io::Error::other))
        .map_err(|_| "retained enrollment journal publication unavailable")?;
    check()?;
    // A missing stage can be restored from the authenticated response
    // even when its original predecessor was present before the POST.
    if current.is_none()
        && !aterm_uds::private_file::write_private_new(home, AUTH_FILE, document, |_| {
            check().map_err(std::io::Error::other)
        })
        .map_err(|_| "retained enrollment restoration unavailable")?
    {
        return Err("retained enrollment restoration raced".into());
    }
    let materialized = materialize_enrollment_guarded(admission, deadline, work, guard, held)?;
    let actual = aterm_secrets::SecretBytes::new(
        aterm_uds::private_file::read_private(home, AUTH_FILE, AUTH_FILE_MAX)
            .map_err(|_| "retained enrollment readback unavailable")?,
    );
    if materialized != digest || actual.as_slice() != document {
        return Err("retained enrollment readback differs".into());
    }
    check()?;
    Ok(materialized)
}

/// The caller owns the exact canonical recovery claim, native resource role,
/// issuer and already-open AuthLock throughout this restoration.
#[cfg(unix)]
pub(in crate::harness) fn reconcile_native_recovery(
    current: &super::super::codex_native_row::CurrentNativeRow<'_>,
    store: &super::super::accounts::Store,
    turn: &super::super::native_enrollment_admission::NativeRecoveryTurn,
) -> Result<String, String> {
    let admission = turn.admission().staging();
    let guard = EnrollmentPublicationGuard::Native {
        store,
        turn,
        current,
    };
    guard.check(admission)?;
    current.with_recovery_stage(|document, witness, held, predecessor| {
        reconcile_original_held(
            admission,
            (document, witness, held, predecessor),
            turn.deadline(),
            Some(turn.scope()),
            &guard,
        )
    })
}

#[cfg(not(unix))]
pub(in crate::harness) fn reconcile_native_recovery(
    _current: &super::super::codex_native_row::CurrentNativeRow<'_>,
    _store: &super::super::accounts::Store,
    _turn: &super::super::native_enrollment_admission::NativeRecoveryTurn,
) -> Result<String, String> {
    Err("native credential custody is unsupported on this platform".into())
}

#[cfg(not(unix))]
pub(in crate::harness) fn reconcile_retained_enrollment(
    _env: &super::super::cli::Env,
    _admission: &super::super::codex_enrollment::Staging,
    _grant: &super::super::codex_grant_owner::RetainedGrant,
    _deadline: WakeDeadline,
    _work: Option<&super::super::account_work::Scope>,
    _store: &super::super::accounts::Store,
    _signer: &super::super::accounts::SigninTurn,
) -> Result<String, String> {
    Err("native credential custody is unsupported on this platform".into())
}
