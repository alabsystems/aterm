// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Historical repair metadata. None of these observations refresh peer presence.

use super::*;
use aterm_types::network_state::{RepairCause, RepairObservation, RepairOutcome, RepairStage};

pub(super) fn value(observation: &RepairObservation) -> Value {
    json!({
        "completed_at_ms": observation.completed_at_ms,
        "outbound": observation.outbound,
        "stage": observation.stage.map(stage),
        "outcome": outcome(observation.outcome),
        "cause": observation.cause().map(RepairCause::word),
        "needs_update": observation.cause().map(|cause| matches!(cause, RepairCause::LegacyPastCrossing | RepairCause::NeedsUpdate)),
        "table": observation.table.map(|id| aterm_codec::hex::encode_lower(&id)),
        "sent": observation.sent,
        "received": observation.received,
        "deferred": observation.deferred,
    })
}

pub(super) fn read(row: &Value) -> Result<Option<RepairObservation>> {
    let Some(value) = row.get("last_repair").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let completed_at_ms = number(value, "completed_at_ms")?;
    if completed_at_ms == 0 {
        return Err(bad());
    }
    let stage = match value.get("stage").and_then(Value::as_str) {
        Some("transport") => Some(RepairStage::Transport),
        Some("membership") => Some(RepairStage::Membership),
        Some("control") => Some(RepairStage::Control),
        Some("application") => Some(RepairStage::Application),
        Some("credentials") => Some(RepairStage::Credentials),
        None if value.get("stage").is_some_and(Value::is_null) => None,
        _ => return Err(bad()),
    };
    // A word a newer aterm added reads as `other`: its writer's record stays readable here, and
    // nothing beyond this closed vocabulary is kept.
    let cause = match value.get("cause") {
        None | Some(Value::Null) => None,
        Some(Value::String(word)) => {
            Some(RepairCause::from_word(word).unwrap_or(RepairCause::Other))
        }
        Some(_) => return Err(bad()),
    };
    let outcome = match (value.get("outcome").and_then(Value::as_str), cause) {
        (Some("failed"), Some(cause)) => RepairOutcome::Failed(cause),
        // A failure an older aterm recorded without its cause (before causes were kept, or a
        // control, membership or credential failure whose cause the round's report dropped)
        // names no more specific category: `other`, its stage, table and counts kept, so what it
        // received or deferred still holds an import back (`alab::command::empty_view_wait`).
        (Some("failed"), None) => RepairOutcome::Failed(RepairCause::Other),
        (Some("deferred"), cause) => RepairOutcome::Deferred(cause),
        // An older aterm kept an application refusal's cause on an attempt the owner stopped.
        (Some("cancelled"), _) => RepairOutcome::Cancelled,
        (Some("not-observed"), None) => RepairOutcome::NotObserved,
        (Some("no-common-table"), None) => RepairOutcome::NoCommonTable,
        (Some("exchanged"), None) => RepairOutcome::Exchanged,
        _ => return Err(bad()),
    };
    let count = |key: &str| match value.get(key) {
        Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(bad),
        None => Err(bad()),
    };
    let sent = count("sent")?;
    let received = count("received")?;
    let deferred = count("deferred")?;
    if sent.is_some() != received.is_some() || sent.is_some() != deferred.is_some() {
        return Err(bad());
    }
    Ok(Some(RepairObservation {
        completed_at_ms,
        outbound: value
            .get("outbound")
            .and_then(Value::as_bool)
            .ok_or_else(bad)?,
        stage,
        outcome,
        table: value
            .get("table")
            .filter(|value| !value.is_null())
            .map(|_| bytes(value, "table"))
            .transpose()?,
        sent,
        received,
        deferred,
    }))
}

fn stage(stage: RepairStage) -> &'static str {
    match stage {
        RepairStage::Transport => "transport",
        RepairStage::Membership => "membership",
        RepairStage::Control => "control",
        RepairStage::Application => "application",
        RepairStage::Credentials => "credentials",
    }
}

fn outcome(outcome: RepairOutcome) -> &'static str {
    match outcome {
        RepairOutcome::Failed(_) => "failed",
        RepairOutcome::Cancelled => "cancelled",
        RepairOutcome::Deferred(_) => "deferred",
        RepairOutcome::NotObserved => "not-observed",
        RepairOutcome::NoCommonTable => "no-common-table",
        RepairOutcome::Exchanged => "exchanged",
    }
}

impl Observer {
    /// Only current membership may associate an attempt with an already observed incarnation.
    pub(crate) fn repair(
        &mut self,
        identity: &MachineIdentity,
        observation: RepairObservation,
        sampled_at: Instant,
    ) {
        if !self.accepting || !self.local_verified || observation.completed_at_ms == 0 {
            return;
        }
        let Some(peer) = self
            .state
            .peers
            .iter_mut()
            .find(|peer| peer.identity == *identity)
        else {
            return;
        };
        if self
            .repair_sampled
            .get(&identity.machine_id)
            .is_some_and(|old| *old > sampled_at)
        {
            return;
        }
        self.repair_sampled.insert(identity.machine_id, sampled_at);
        if peer.last_repair.as_ref() == Some(&observation) {
            return;
        }
        // An outcome change is a transition, written with the next checkpoint (`SETTLE`).
        // Counts and table scans are published live and retained by the periodic checkpoint,
        // like continuing authenticated contacts. A reader that needs the latest round takes
        // the live observation, or a history its observer closed (`read_closed_history`).
        self.dirty |= peer
            .last_repair
            .as_ref()
            .is_none_or(|old| old.stage != observation.stage || old.outcome != observation.outcome);
        peer.last_repair = Some(observation);
        self.changed = true;
    }
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;
