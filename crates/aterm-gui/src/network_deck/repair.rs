// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The retained result of one repair attempt, shared by the lens and Lab page.

use aterm_types::network_state::{RepairObservation, RepairOutcome, RepairStage};

#[cfg(test)]
mod tests;

/// A repair outcome in a person's words, for the lens and the Local lens's observed rows.
pub(super) const fn outcome_words(outcome: RepairOutcome) -> &'static str {
    match outcome {
        RepairOutcome::Failed(_) => "Failed",
        RepairOutcome::Cancelled => "Stopped before completion",
        RepairOutcome::Deferred(_) => "Work deferred",
        RepairOutcome::NotObserved => "Application exchange not observed",
        RepairOutcome::NoCommonTable => "No common table selected",
        RepairOutcome::Exchanged => "One application exchange completed",
    }
}

pub(super) fn details(observation: Option<&RepairObservation>) -> Vec<(String, String)> {
    let Some(observation) = observation else {
        return vec![("Last repair".into(), "Not reported by this observer".into())];
    };
    let outcome = outcome_words(observation.outcome);
    // The cause in the same word `aterm db commons-status` and its JSON use.
    let outcome = match observation.cause() {
        Some(cause) => format!("{outcome} ({})", cause.word()),
        None => outcome.to_owned(),
    };
    let stage = observation
        .stage
        .map_or("stage not reported", |stage| match stage {
            RepairStage::Transport => "transport",
            RepairStage::Membership => "membership",
            RepairStage::Control => "shared control history",
            RepairStage::Application => "application data",
            RepairStage::Credentials => "credential handoff",
        });
    let count = |value: Option<u64>| value.map_or_else(|| "unknown".into(), |n| n.to_string());
    let mut rows = vec![
        ("Last repair".into(), format!("{outcome} · {stage}")),
        (
            "Repair time".into(),
            format!(
                "{} · {}",
                super::observation_time(observation.completed_at_ms),
                if observation.outbound { "outgoing" } else { "incoming" }
            ),
        ),
        (
            "Record receipts".into(),
            format!(
                "Sent {} · received {} · deferred {}",
                count(observation.sent),
                count(observation.received),
                count(observation.deferred)
            ),
        ),
        (
            "Repair scope".into(),
            "Historical attempt; receipts may include retries. Current table agreement is not established by this report.".into(),
        ),
    ];
    if let Some(table) = observation.table {
        rows.push((
            "Repair table".into(),
            aterm_codec::hex::encode_lower(&table),
        ));
    }
    rows
}
