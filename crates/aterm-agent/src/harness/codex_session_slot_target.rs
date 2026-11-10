// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Installed destination reads, distinct from settled/generic Source.
use super::*;

/// Destination read under a real installed factory role. Scope and immutable
/// metadata are checked before owner admission; there is no default/bootstrap
/// fallback and no role reconstructed from a signed Book or operation string.
pub(in crate::harness) fn preload_installed(
    env: &Env,
    permission: &crate::harness::codex_source_registry::InstalledRead<'_>,
) -> Result<(SourceHandle, MaterializationReceipt), String> {
    permission.current()?;
    let expected = permission.expected();
    if !env
        .home
        .as_deref()
        .is_some_and(|home| permission.gate().matches_home(home))
    {
        return Err("installed target source namespace changed".into());
    }
    authenticate_captured_namespaces(permission.caller(), expected, permission.gate())?;
    owner::with_current_published_grant(env, expected.binding(), permission.caller(), |grant| {
        permission.current()?;
        if grant.binding() != expected.binding()
            || grant.identity().account != expected.account()
            || grant.identity().user != expected.user()
            || grant.revision().generation < expected.credential_generation()
        {
            return Err("installed target grant changed".into());
        }
        permission.assignment().with_namespace(|book_namespace| {
            let namespaces = expected
                .namespaces()
                .ok_or("installed target namespace unavailable")?;
            if book_namespace != namespaces.book() {
                return Err("installed target Book changed".into());
            }
            let (opened, auth) =
                prepare_materialization(env, permission.caller(), expected.binding())?;
            let old = opened
                .record
                .as_ref()
                .ok_or("installed target Slot unavailable")?;
            use_guard::check_record(env, permission.caller(), old, expected)
                .map_err(|_| "installed target Slot changed")?;
            if opened.store.physical_identity().map_err(str::to_owned)? != namespaces.slot {
                return Err("installed target Slot namespace changed".into());
            }
            let desired = Record {
                schema: 2,
                caller: permission.caller().clone(),
                binding: grant.binding().clone(),
                assignment_generation: expected.assignment_generation(),
                source_authority: expected.source_authority().clone(),
                revision: grant.revision().clone(),
            };
            let result = if opened.record.as_ref() == Some(&desired) {
                let source = opened.readback(&grant, &desired, book_namespace)?;
                let receipt = MaterializationReceipt {
                    caller: desired.caller,
                    slot: opened.key,
                    revision: desired.revision,
                    _auth: auth,
                    _store: opened.store,
                };
                (source, receipt)
            } else {
                if !installation_allowed(
                    opened.record.as_ref(),
                    &desired,
                    permission.assignment().previous_assignment_generation(),
                ) {
                    return Err("installed target revision changed".into());
                }
                materialize(opened, auth, &grant, desired, book_namespace)?
            };
            permission.current()?;
            Ok(result)
        })
    })
}

/// Validate a destination's already loaded receipt without eagerly replacing
/// its observed revision. C may advance inside the same immutable grant.
pub(in crate::harness) fn validate_installed(
    env: &Env,
    permission: &crate::harness::codex_source_registry::InstalledRead<'_>,
) -> Result<(), String> {
    validate_observed(
        env,
        permission.caller(),
        permission.expected(),
        permission.gate(),
        permission.assignment(),
        || permission.current(),
    )
}

pub(in crate::harness) fn validate_target_snapshot(
    env: &Env,
    permission: &crate::harness::codex_source_registry::TargetSnapshotRead<'_>,
) -> Result<(), String> {
    validate_observed(
        env,
        permission.caller(),
        permission.expected(),
        permission.gate(),
        permission.assignment(),
        || permission.current(),
    )
}

pub(in crate::harness) fn validate_target_ack(
    env: &Env,
    permission: &crate::harness::codex_source_registry::TargetAckPermit<'_>,
) -> Result<(), String> {
    validate_observed(
        env,
        permission.assignment().caller(),
        permission.expected(),
        permission.gate(),
        permission.assignment(),
        || permission.current(),
    )
}

/// An explicitly normalized existing member has ordinary settled authority.
/// Its historical target context supplies no pending-assignment exception.
pub(in crate::harness) fn preload_normalized(
    env: &Env,
    permission: &crate::harness::codex_source_registry::NormalizedRead<'_>,
    operation: &str,
) -> Result<SourceProgress, String> {
    permission.current()?;
    if !env
        .home
        .as_deref()
        .is_some_and(|home| permission.gate().matches_home(home))
    {
        return Err("normalized source home changed".into());
    }
    let result = source_inner(
        env,
        permission.caller(),
        operation,
        Some(permission.expected()),
    )
    .map_err(|_| "normalized source unavailable")?;
    permission.current()?;
    Ok(result)
}
pub(in crate::harness) fn validate_normalized(
    env: &Env,
    permission: &crate::harness::codex_source_registry::NormalizedRead<'_>,
) -> Result<(), String> {
    validate_observed(
        env,
        permission.caller(),
        permission.expected(),
        permission.gate(),
        permission.assignment(),
        || permission.current(),
    )
}

fn validate_observed(
    env: &Env,
    caller: &Caller,
    expected: &SourceHandle,
    gate: &crate::harness::codex_source_gate::SourceGateIdentity,
    assignment: &codex_assignment::AssignmentCommit,
    current: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    current()?;
    if !env
        .home
        .as_deref()
        .is_some_and(|home| gate.matches_home(home))
    {
        return Err("installed target source namespace changed".into());
    }
    authenticate_captured_namespaces(caller, expected, gate)?;
    owner::with_current_published_grant(env, expected.binding(), caller, |grant| {
        if grant.identity().account != expected.account()
            || grant.identity().user != expected.user()
            || grant.revision().generation < expected.credential_generation()
        {
            return Err("installed target identity changed".into());
        }
        assignment.with_namespace(|namespace| {
            if expected
                .namespaces()
                .is_none_or(|expected| expected.book() != namespace)
            {
                return Err("installed target Book changed".into());
            }
            current()
        })
    })
}
