// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A retained native use may continue only its original immutable source.
//! Expected metadata is checked before opening any owner or materializing a
//! credential; it is not reconstructed from an operation string.

use super::*;

/// Read/reconcile only the active native use's original source. This is a
/// private member continuation; it does not replace the legacy source ABI.
/// # Errors
/// The use, namespace, signed assignment, slot scope or current policy changed.
pub fn source_for_use(
    env: &Env,
    caller: &Caller,
    operation: &str,
    context: &super::super::codex_source_registry::SourceUse,
) -> Result<SourceProgress, SlotError> {
    let namespace = context
        .namespace(caller, env.home.as_deref().ok_or(SlotError::Unavailable)?)
        .map_err(|_| SlotError::Unavailable)?;
    let source = namespace.source();
    let progress = source_inner_using_namespace(
        env,
        caller,
        operation,
        Some(source),
        Some(SourceReadNamespace::ActiveUse(&namespace)),
    )?;
    namespace.current().map_err(|_| SlotError::Unavailable)?;
    check_source(&progress, source)?;
    Ok(progress)
}

/// Refresh under the original use scope. A completed response's owned
/// retention is preserved even if its caller later loses admission.
/// # Errors
/// The use, namespace, signed assignment, slot scope or current policy changed.
pub fn refresh_for_use(
    env: &Env,
    caller: &Caller,
    request: &RefreshRequest,
    context: &super::super::codex_source_registry::SourceUse,
) -> Result<RefreshProgress, SlotError> {
    let namespace =
        RefreshNamespace::for_use(env, caller, context).map_err(|_| SlotError::Unavailable)?;
    let source = namespace.expected();
    let progress = refresh_inner_using_namespace(
        env,
        caller,
        request,
        namespace.scope(),
        Some(source),
        Some(&namespace),
    )?;
    if !matches!(&progress, RefreshProgress::Retaining(_)) {
        namespace
            .with_current(
                env,
                caller,
                source.binding(),
                source.assignment_generation(),
                source.source_authority(),
                || Ok(()),
            )
            .map_err(|_| SlotError::Unavailable)?;
    }
    Ok(progress)
}

fn matches_record(record: &Record, expected: &SourceHandle, key: &str) -> bool {
    record.binding == *expected.binding()
        && key == expected.slot()
        && record.assignment_generation == expected.assignment_generation()
        && record.source_authority == *expected.source_authority()
        && record.revision.generation >= expected.credential_generation()
        && (record.revision.generation > expected.credential_generation()
            || record.revision.digest == expected.digest())
}

pub(super) fn check_record(
    env: &Env,
    caller: &Caller,
    record: &Record,
    expected: &SourceHandle,
) -> Result<(), SlotError> {
    let key = slot_key(caller, &record.binding).map_err(|_| SlotError::Unavailable)?;
    if record.caller != *caller || !matches_record(record, expected, &key) {
        return Err(SlotError::Unavailable);
    }
    let path = materialization_location(env, caller, &record.binding)
        .map_err(|_| SlotError::Unavailable)?
        .join("auth.json");
    let path = path.canonicalize().map_err(|_| SlotError::Unavailable)?;
    if path != expected.path() {
        return Err(SlotError::Unavailable);
    }
    Ok(())
}

pub(super) fn check_source(
    progress: &SourceProgress,
    expected: &SourceHandle,
) -> Result<(), SlotError> {
    let SourceProgress::Ready(source) = progress else {
        return Ok(());
    };
    if source.binding() != expected.binding()
        || source.slot() != expected.slot()
        || source.path() != expected.path()
        || source.assignment_generation() != expected.assignment_generation()
        || source.source_authority() != expected.source_authority()
        || source.account() != expected.account()
        || source.user() != expected.user()
        || source.credential_generation() < expected.credential_generation()
        || (source.credential_generation() == expected.credential_generation()
            && source.digest() != expected.digest())
    {
        return Err(SlotError::Unavailable);
    }
    Ok(())
}

#[cfg(test)]
std::thread_local! { static OWNER_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(super) fn note_owner_entry() {
    OWNER_ENTRIES.with(|count| count.set(count.get() + 1));
}

#[cfg(all(test, unix))]
#[path = "codex_session_slot_use_guard_tests.rs"]
mod tests;

/// Read only the source of an independently counted, already accepted PRELOAD.
/// The proof cannot be passed to the provider refresh engine.
pub(in crate::harness) fn source_for_preload(
    env: &Env,
    caller: &Caller,
    operation: &str,
    proof: &super::super::codex_source_registry::PreloadReadNamespace,
) -> Result<SourceProgress, crate::harness::codex_source_registry::Error> {
    use crate::harness::codex_source_registry::Error;
    // A contended occurrence check is retryable on this same owned command.
    // Do not relabel it as lost transport and discard its live connection.
    proof.current()?;
    let source = proof.source();
    let result = source_inner_using_namespace(
        env,
        caller,
        operation,
        Some(source),
        Some(SourceReadNamespace::CapturedPreload(proof)),
    )
    .map_err(|_| Error::Unavailable)?;
    proof.current()?;
    check_source(&result, source).map_err(|_| Error::Unavailable)?;
    Ok(result)
}
