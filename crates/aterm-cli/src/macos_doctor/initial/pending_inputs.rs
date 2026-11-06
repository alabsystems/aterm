// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local input observations may wake an admitted settings pass, never authorize an effect.

use super::*;

pub(super) fn revision(records: &Records) -> Option<u64> {
    let (plan, true) = records.plan.as_ref()? else {
        return None;
    };
    let progress = records.progress?;
    if !progress.admitted
        || !progress.belongs_to(plan)
        || progress.completed
        || progress.stopped_by(records.stop_request)
        || progress.done & Stage::Workstation.bit() != 0
        || !matches!(
            records.decision,
            Decision::Pending | Decision::Applied(true)
        )
    {
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
