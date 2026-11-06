// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A second durable client slot for an unconfirmed account successor. This
//! preserves the predecessor until a typed issuer receipt can authorize promotion.
//! No wire request, promotion or abandonment is implemented here.

use super::native_act::{Binding, ImagePhase, NativePlan, Prepare, Request};
use super::upgrade_drive::{self, Opts, St};
use aterm_json::Value;

/// Preserve even an unreadable future record. Its presence keeps the old debt
/// held; failure to decode it must not look like an absent candidate.
#[derive(Clone)]
pub(super) struct Pending(Value);

// JSON Value's f64 arm lacks Eq. Slots compare their persisted representation,
// not floating-point arithmetic; nonserializable values cannot pass checked().
impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        aterm_json::to_string(&self.0).ok() == aterm_json::to_string(&other.0).ok()
    }
}

impl Eq for Pending {}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingAccountSuccessor(redacted)")
    }
}

impl Pending {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountClientSlot",
            action = "Opaque",
            reason = "The reader encounters an unknown or damaged candidate schema"
        )
    )]
    pub(super) fn read(value: &Value) -> Self {
        Self(value.clone())
    }

    #[cfg(all(test, unix))]
    fn value(&self) -> &Value {
        &self.0
    }

    pub(super) fn write_into(&self, record: &mut aterm_json::Map) {
        record.insert("native_account_candidate".into(), self.0.clone());
    }

    fn new(predecessor: &Binding, phase: ImagePhase, prepare: &Prepare) -> Option<Self> {
        let mut record = aterm_json::Map::new();
        record.insert("version".into(), Value::from(1));
        record.insert("predecessor_id".into(), Value::from(predecessor.id.clone()));
        record.insert(
            "predecessor_cap".into(),
            Value::from(predecessor.capability.clone()),
        );
        record.insert(
            "phase".into(),
            Value::from(match phase {
                ImagePhase::Forward => "forward",
                ImagePhase::Rollback => "rollback",
            }),
        );
        record.insert(
            "prepare".into(),
            Value::from(Request::prepare(prepare.clone()).encode()?),
        );
        Some(Self(Value::Object(record)))
    }

    fn checked(&self, st: &St) -> Option<SavedCandidate> {
        let record = self.0.as_object()?;
        if record.len() != 5 || record.get("version")?.as_u64()? != 1 {
            return None;
        }
        let phase = match record.get("phase")?.as_str()? {
            "forward" => ImagePhase::Forward,
            "rollback" => ImagePhase::Rollback,
            _ => return None,
        };
        let (predecessor, original) = source(st, phase)?;
        if record.get("predecessor_id")?.as_str()? != predecessor.id
            || record.get("predecessor_cap")?.as_str()? != predecessor.capability
        {
            return None;
        }
        let encoded = record.get("prepare")?.as_str()?;
        let Request::Prepare(prepare) = Request::parse(encoded)? else {
            return None;
        };
        if Request::Prepare(prepare.clone()).encode().as_deref() != Some(encoded)
            || !original.account_successor_recipe(&prepare, phase)
        {
            return None;
        }
        Some(SavedCandidate {
            predecessor: predecessor.clone(),
            phase,
            prepare,
        })
    }
}

/// Only returned after this attempt's successful durability barrier. This is
/// data for a future typed account request, not a native execution capability.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(
    not(all(test, unix)),
    allow(
        dead_code,
        reason = "Candidate fields await the typed issuer transport; their fixtures are Unix-only."
    )
)]
pub(super) struct SavedCandidate {
    predecessor: Binding,
    phase: ImagePhase,
    prepare: Box<Prepare>,
}

/// A stale writer cannot erase a candidate or change the debt it depends on.
/// Wait/status metadata may still progress while the immutable slots stay held.
pub(super) fn preserves_pending(previous: &St, next: &St) -> bool {
    if !previous.native_retired_unissued
        && previous.native_account_candidate.is_none()
        && next.native_account_candidate.is_none()
    {
        return true;
    }
    (previous.native_account_candidate.is_none()
        || previous.native_account_candidate == next.native_account_candidate)
        && next.native_retired_unissued
        && previous.native == next.native
        && previous.native_prepare == next.native_prepare
        && previous.native_rollback == next.native_rollback
        && previous.line == next.line
        && previous.back_line == next.back_line
        && previous.account_intent == next.account_intent
        && previous.back_account_intent == next.back_account_intent
        && previous.phase == next.phase
        && previous.pending_since == next.pending_since
        && previous.salt == next.salt
        && previous.from == next.from
        && previous.to == next.to
        && previous.source == next.source
}

fn storage_enabled(opts: &Opts) -> Result<(), &'static str> {
    if opts.dry_run {
        Err("native-dry-run")
    } else if !cfg!(unix) {
        // The portable legacy writer does not acknowledge a directory barrier.
        Err("native-account-durability-unavailable")
    } else {
        Ok(())
    }
}

fn source(st: &St, phase: ImagePhase) -> Option<(&Binding, Box<Prepare>)> {
    if !st.native_retired_unissued || st.native_rollback != (phase == ImagePhase::Rollback) {
        return None;
    }
    let predecessor = st.native.as_ref()?;
    if !aterm_codec::hex::is_lower(&predecessor.id, 32)
        || !aterm_codec::hex::is_lower(&predecessor.capability, 64)
    {
        return None;
    }
    let Request::Prepare(original) = Request::parse(&st.native_prepare)? else {
        return None;
    };
    if original.id != predecessor.id
        || !matches!(
            original.plan,
            NativePlan::Restart {
                cold: None,
                observation: None,
                ..
            }
        )
    {
        return None;
    }
    let command = super::native_account_recipe::active_command(&original.plan, phase)?;
    if command.legacy_line().as_deref() != Some(st.line.as_str())
        || command.account_intent.as_deref() != st.account_intent.as_ref()
    {
        return None;
    }
    Some((predecessor, original))
}

/// Select at most one exact candidate. A failed save retains it in memory;
/// retry it rather than rerunning selection and replacing a possibly sent ID.
#[cfg_attr(
    not(all(test, unix)),
    allow(
        dead_code,
        reason = "The typed issuer claim envelope is not enabled; candidate fixtures are Unix-only."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeAccountClientSlot",
        action = "Rival",
        project = "aterm_agent::harness::native_account_handoff::tests::transition"
    )
)]
pub(super) fn stage(
    opts: &Opts,
    session: &str,
    st: &mut St,
    candidate: &Prepare,
    phase: ImagePhase,
) -> Result<SavedCandidate, &'static str> {
    storage_enabled(opts)?;
    let (predecessor, original) = source(st, phase).ok_or("native-account-predecessor")?;
    if !original.account_successor_recipe(candidate, phase) {
        return Err("native-account-recipe");
    }
    let pending = Pending::new(predecessor, phase, candidate).ok_or("native-account-recipe")?;
    match &st.native_account_candidate {
        Some(existing) if existing != &pending => return Err("native-account-candidate-pending"),
        Some(_) => {}
        None => st.native_account_candidate = Some(pending),
    }
    retry(opts, session, st)
}

/// A loaded candidate is never evidence of a completed directory barrier.
/// Every exposure repeats the exact save, including after a lost save reply.
#[cfg_attr(
    not(all(test, unix)),
    allow(
        dead_code,
        reason = "The typed issuer claim envelope is not enabled; candidate fixtures are Unix-only."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeAccountClientSlot",
        action = "FailBarrier",
        reason = "The filesystem refuses to acknowledge a directory durability barrier"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeAccountClientSlot",
        action = "RepairBarrier",
        reason = "The filesystem resumes acknowledging directory durability barriers"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeAccountClientSlot",
        action = "DryRun",
        reason = "The caller forbids persistent work and candidate exposure"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeAccountClientSlot",
        action = "LiveRun",
        reason = "The caller supplies ordinary execution options"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeAccountClientSlot",
        action = "Attempt",
        project = "aterm_agent::harness::native_account_handoff::tests::transition"
    )
)]
pub(super) fn retry(opts: &Opts, session: &str, st: &St) -> Result<SavedCandidate, &'static str> {
    storage_enabled(opts)?;
    let candidate = st
        .native_account_candidate
        .as_ref()
        .and_then(|pending| pending.checked(st))
        .ok_or("native-account-candidate")?;
    upgrade_drive::save_checked(opts, session, st).map_err(|_| "native-unsaved")?;
    Ok(candidate)
}

#[cfg(all(test, unix))]
#[path = "native_account_handoff_tests.rs"]
mod tests;
