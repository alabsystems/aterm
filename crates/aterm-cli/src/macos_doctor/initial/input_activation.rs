// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Attachment activation belongs to the exact accepted Caps setting, beyond stage completion.

use super::*;
use aterm_machines::init::KeyboardAttachmentActivation;

/// Current Caps authority, or a pending setting whose hardware facts may wake its worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSetupScope {
    /// No current Caps setting has work this observer may do.
    Inactive,
    /// Caps was verified or carried; only this revision admits attachment activation.
    Applied(u64),
    /// Caps is still owed; this revision permits observations and scoped coordinator resumption.
    Pending(u64),
}

/// One bounded, canonical set of native keyboard service identities.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputServiceSet(Vec<u64>);

impl InputServiceSet {
    /// Normalize one native census, refusing an ambiguous or unbounded observation.
    ///
    /// # Errors
    /// More than 4096 services, a zero identity, or a duplicate identity.
    pub fn new(mut ids: Vec<u64>) -> Result<Self, String> {
        if ids.len() > 4096 || ids.contains(&0) {
            return Err("invalid keyboard service identities".into());
        }
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate keyboard service identity".into());
        }
        Ok(Self(ids))
    }

    /// Observe current membership and the services whose complete native state is readable.
    /// Readability is a wake fact only; it never certifies a mapping or admits a setter.
    ///
    /// # Errors
    /// The observation has unbounded, zero or duplicate service identities.
    pub fn observed(
        rows: &[aterm_machines::port::KeyboardServiceObservation],
    ) -> Result<(Self, Self), String> {
        let present = Self::new(rows.iter().map(|row| row.registry()).collect())?;
        let readable = Self::new(
            rows.iter()
                .filter(|row| row.unread().is_none())
                .map(|row| row.registry())
                .collect(),
        )?;
        Ok((present, readable))
    }

    /// Keep previously observed positive facts only for services still present in this run.
    /// This scheduling history supplies no current mapping or mutation authority.
    pub fn retain_present_from(&mut self, before: &Self, present: &Self) {
        self.0.retain(|id| present.0.binary_search(id).is_ok());
        self.0.extend(
            before
                .0
                .iter()
                .filter(|id| present.0.binary_search(id).is_ok())
                .copied(),
        );
        self.0.sort_unstable();
        self.0.dedup();
    }

    /// Whether a service gained a positive observation; errors and lost reads add nothing.
    #[must_use]
    pub fn gained_from(&self, before: &Self) -> bool {
        self.0.iter().any(|id| before.0.binary_search(id).is_err())
    }

    /// Take the normalized identities without another allocation.
    #[must_use]
    pub fn into_vec(self) -> Vec<u64> {
        self.0
    }
}

fn scope_at(root: &Path) -> Result<InputSetupScope, String> {
    let records = observation_at(root).map_err(|why| why.to_string())?.records;
    Ok(if let Some(revision) = accepted(&records) {
        InputSetupScope::Applied(revision)
    } else if let Some(revision) = pending_inputs::caps_revision(root, &records) {
        InputSetupScope::Pending(revision)
    } else {
        InputSetupScope::Inactive
    })
}

/// Read the accepted or pending Caps scope from one canonical observation. Pending scope
/// authorizes a hardware observation only, never a setter or another setup stage.
///
/// # Errors
/// This account's scope or setup records cannot be read.
pub fn input_setup_scope() -> Result<InputSetupScope, String> {
    ensure_person_scope()?;
    scope_at(&state_root()?)
}

/// The same input-scope reader over inert scratch records, with no live account scope.
///
/// # Errors
/// The scratch setup records cannot be read.
#[cfg(any(test, debug_assertions))]
pub fn input_setup_scope_at(root: &Path) -> Result<InputSetupScope, String> {
    scope_at(root)
}

fn accepted(records: &Records) -> Option<u64> {
    let (plan, _) = records.plan.as_ref()?;
    let held = holds(records);
    if !plan.selection.contains(Setting::CapsLock)
        || held.holds(Setting::CapsLock)
        || matches!(records.decision, Decision::Stopped(_))
        || records.restored.is_some_and(|restored| {
            restored.left.contains(Setting::CapsLock)
                || restored
                    .queue(records.progress, Some(plan))
                    .contains(Setting::CapsLock)
        })
    {
        return None;
    }
    let Some(progress) = records.progress else {
        // The older selection-only writer kept no progress. Its applied decision already
        // proves the workstation pass ran (the same evidence a later choice carries with
        // older_run). Keep that run's existing revision zero without admitting another pass.
        // With no physical run to disambiguate a Stop, any request withholds this authority.
        if records.stop_request.is_some() {
            return None;
        }
        let run = older_run(records.plan.as_ref(), records.decision)?;
        return (run.done & Stage::Workstation.bit() != 0).then_some(run.revision);
    };
    // A choice can carry Caps while its Workstation stage owes only other preferences.
    // `only` names what still has to apply; without that explicit scope, an unfinished
    // stage supplies no per-setting completion evidence.
    let caps_applied = progress.done & Stage::Workstation.bit() != 0
        || progress
            .only
            .is_some_and(|only| !only.contains(Setting::CapsLock));
    (progress.belongs_to(plan)
        && progress.admitted
        && !progress.stopped
        && !progress.named_by(records.stop_request)
        && caps_applied)
        .then_some(progress.revision)
}

pub(super) fn revision_at(root: &Path) -> Result<Option<u64>, String> {
    Ok(accepted(
        &observation_at(root).map_err(|why| why.to_string())?.records,
    ))
}

/// Activate new or changed services under their original accepted Caps setting. The
/// profile lock orders the operation against choices and Restore, while both the durable and
/// this process's immediate Stop are checked before each whole item.
/// `invalidate` retires an earlier whole-Caps reading before a mutation or on uncertain
/// attachment coverage; this path never supplies a positive whole-Caps observation.
///
/// # Errors
/// Setup authority, console identity, input facts or live activation cannot be read safely.
pub fn activate_input_attachments(
    expected: u64,
    registries: &[u64],
    stopped: impl Fn(u64) -> bool,
    invalidate: &dyn Fn(),
) -> Result<Option<KeyboardAttachmentActivation>, String> {
    let invalidated = std::cell::Cell::new(false);
    let invalidate_once = || {
        if !invalidated.replace(true) {
            invalidate();
        }
    };
    let result = (|| {
        ensure_person_scope()?;
        let root = state_root()?;
        activate_at(
            &root,
            expected,
            &stopped,
            &invalidate_once,
            |kept, continues| {
                aterm_machines::init::activate_keyboard_attachments(
                    &aterm_machines::port::Live::new(),
                    registries,
                    &|item| kept.keeps(item),
                    continues,
                    &invalidate_once,
                )
            },
        )
    })();
    if matches!(
        result,
        Err(_) | Ok(Some(KeyboardAttachmentActivation::Unfinished))
    ) {
        invalidate_once();
    }
    result
}

fn activate_at<T>(
    root: &Path,
    expected: u64,
    stopped: &dyn Fn(u64) -> bool,
    invalidate: &dyn Fn(),
    work: impl FnOnce(&kept::Kept, &dyn Fn() -> bool) -> Result<T, String>,
) -> Result<Option<T>, String> {
    with_current_at(root, expected, stopped, invalidate, |continues| {
        let kept = kept::Kept::load(root)?;
        work(&kept, continues)
    })
}

/// Publish an already-paid input diagnostic under the same profile/current-revision guard
/// as attachment activation. This reads only setup authority, never preferences or HID.
///
/// # Errors
/// This account's scope, setup lock or current records cannot be resolved.
pub fn with_current_input_scope<T>(
    expected: u64,
    stopped: impl Fn(u64) -> bool,
    work: impl FnOnce() -> T,
) -> Result<Option<T>, String> {
    ensure_person_scope()?;
    with_current_at(&state_root()?, expected, &stopped, &|| {}, |_| {
        // The authority read can block; honor an immediate Stop again after it.
        Ok((!stopped(expected)).then(work))
    })
    .map(Option::flatten)
}

fn with_current_at<T>(
    root: &Path,
    expected: u64,
    stopped: &dyn Fn(u64) -> bool,
    invalidate: &dyn Fn(),
    work: impl FnOnce(&dyn Fn() -> bool) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let Some(_lock) = HeldLock::try_take(root)? else {
        return Ok(None);
    };
    let continues = || {
        let current =
            !stopped(expected) && revision_at(root).is_ok_and(|now| now == Some(expected));
        if !current {
            invalidate();
        }
        current
    };
    if !continues() {
        return Ok(None);
    }
    work(&continues).map(Some)
}

#[cfg(test)]
#[path = "input_activation_tests.rs"]
mod tests;
