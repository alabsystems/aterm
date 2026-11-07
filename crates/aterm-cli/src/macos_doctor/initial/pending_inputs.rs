// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local input observations may wake an admitted settings pass, never authorize an effect.

use super::*;

fn pending(records: &Records) -> Option<(&Plan, Progress)> {
    let (plan, true) = records.plan.as_ref()? else {
        return None;
    };
    let progress = records.progress?;
    if !progress.admitted
        || !progress.belongs_to(plan)
        || progress.completed
        || progress.stopped_by(records.stop_request)
        || !matches!(
            records.decision,
            Decision::Pending | Decision::Applied(true)
        )
    {
        return None;
    }
    Some((plan, progress))
}

pub(super) fn revision(records: &Records) -> Option<u64> {
    let (plan, progress) = pending(records)?;
    if progress.done & Stage::Workstation.bit() != 0 {
        return None;
    }
    let held = holds(records);
    Stage::Workstation
        .settings()
        .iter()
        .any(|setting| {
            plan.selection.contains(*setting)
                && progress.only.is_none_or(|only| only.contains(*setting))
                && !held.holds(*setting)
        })
        .then_some(progress.revision)
}

pub(super) fn repositories_revision(root: &Path, records: &Records) -> Option<u64> {
    let (plan, progress) = pending(records)?;
    if !plan.selection.contains(Setting::Repositories)
        || progress.done & Stage::Repositories.bit() != 0
        || holds(records).holds(Setting::Repositories)
    {
        return None;
    }
    let ledger = ledger::load(root, progress.revision);
    (!ledger.frozen && ledger.ledger.parked(Stage::Repositories).is_none())
        .then_some(progress.revision)
}

pub(super) fn caps_revision(root: &Path, records: &Records) -> Option<u64> {
    let revision = revision(records)?;
    let (plan, _) = records.plan.as_ref()?;
    let progress = records.progress?;
    if !plan.selection.contains(Setting::CapsLock)
        || progress
            .only
            .is_some_and(|only| !only.contains(Setting::CapsLock))
        || holds(records).holds(Setting::CapsLock)
        || records.restored.is_some_and(|restored| {
            restored.left.contains(Setting::CapsLock)
                && !restored
                    .queue(Some(progress), Some(plan))
                    .contains(Setting::CapsLock)
        })
    {
        return None;
    }
    let ledger = ledger::load(root, revision);
    (!ledger.frozen && ledger.ledger.parked(Stage::Workstation).is_none()).then_some(revision)
}

/// The current admitted revision with unheld preferences still to run. A native console
/// transition can wake its existing worker; this supplies no authority to change settings.
///
/// # Errors
/// This account's scope or its setup records cannot be read.
pub fn initial_setup_pending_workstation_revision() -> Result<Option<u64>, String> {
    ensure_person_scope()?;
    let observation = observation_at(&state_root()?).map_err(|error| error.to_string())?;
    Ok(revision(&observation.records))
}

/// The current admitted, unheld Repositories revision with an unfinished and unparked stage.
/// A background child may request an ordinary guarded observation for it, never infer that
/// the stage completed from the child's exit or authorize another effect.
///
/// # Errors
/// This account's scope or its setup records cannot be read.
pub fn initial_setup_pending_repositories() -> Result<Option<InitialSetupAdmission>, String> {
    ensure_person_scope()?;
    let root = state_root()?;
    let observation = observation_at(&root).map_err(|error| error.to_string())?;
    Ok(
        repositories_revision(&root, &observation.records).and_then(|revision| {
            observation
                .records
                .plan
                .map(|(plan, _)| InitialSetupAdmission {
                    plan,
                    revision,
                    completed: false,
                })
        }),
    )
}
