// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    network_state::{self, Publication},
    replica::{InventoryObservation, InventorySummary, MatchingInventory, Report},
    runtime::RepairRound,
    table::Scope,
};
use aterm_secrets::commons::journal::Trust;
use aterm_spec::interp::State;
use aterm_spec::{derive::Model, verify};
use aterm_types::network_state::{MachineIdentity, NetworkObservation};
use std::time::Duration;

fn observation(id: u8) -> AccountsExchangeObservation {
    AccountsExchangeObservation {
        scope_digest: [id; 32],
        local_actor: [id + 10; 48],
        control_observation: [id + 20; 64],
        inventory_count: u64::from(id),
        inventory_fingerprint: [id + 30; 32],
        peer: MachineIdentity {
            machine_id: [id + 40; 16],
            incarnation: [id + 50; 16],
            label: None,
        },
        // Later monotonic evidence may have an earlier wall-clock timestamp.
        sampled_at_ms: if id == 1 { 200 } else { 100 },
    }
}

fn candidate(id: u8, at: Instant, sample: u64) -> Candidate {
    Candidate {
        observation: observation(id),
        sampled_at: at + Duration::from_secs(sample),
    }
}

fn payload_id(value: &AccountsExchangeObservation) -> i64 {
    let mut value = value.clone();
    value.sampled_at_ms = 0;
    for id in 1..=2 {
        let mut expected = observation(id);
        expected.sampled_at_ms = 0;
        if value == expected {
            return i64::from(id);
        }
    }
    3
}

fn transition_admitted(model: &Model, before: &State, after: &State, action: &str) -> bool {
    let (accepted, why) = verify::validate_transition_tiered(
        model,
        &[],
        before,
        after,
        Some(action),
        "Account inventory exchange conformance",
    );
    eprintln!("{} {action}: {why}", model.name);
    accepted
}

fn check_model(model: &Model) {
    let covered = verify::prove_and_catch_scalar(model, "Account inventory exchange model");
    eprintln!("{}: {covered:?}", model.name);
    let faults = verify::sweep_faults(std::slice::from_ref(model));
    assert!(faults.is_empty(), "{}: {faults:?}", model.name);
    assert_eq!(
        aterm_spec::interp::fired_actions(model).len(),
        model.actions.len()
    );
}

pub(crate) fn retention_transition(before: &State, action: &str) -> State {
    let at = Instant::now();
    let mut last = None;
    let mut sample = None;
    let (scenario, accepted) = match action {
        "First" => (1, retain(&mut last, &mut sample, candidate(1, at, 2))),
        "Newer" | "SameSample" | "OlderSample" => {
            assert!(retain(&mut last, &mut sample, candidate(1, at, 2)));
            let (scenario, next_sample) = match action {
                "Newer" => (2, 3),
                "SameSample" => (3, 2),
                _ => (4, 1),
            };
            (
                scenario,
                retain(&mut last, &mut sample, candidate(2, at, next_sample)),
            )
        }
        "Read" => {
            assert!(retain(&mut last, &mut sample, candidate(1, at, 2)));
            let publication = Publication::new(
                NetworkObservation {
                    accounts_exchange: last.clone(),
                    ..NetworkObservation::default()
                },
                at,
            );
            last = publication
                .current_at(at + Duration::from_secs(120), Some(500_000))
                .accounts_exchange;
            (5, false)
        }
        "HistoryRead" | "FirstAfterHistory" => {
            let stored = NetworkObservation {
                accounts_exchange: Some(observation(1)),
                ..NetworkObservation::default()
            };
            last = network_state::decode(&network_state::encode(&stored, None).unwrap())
                .unwrap()
                .accounts_exchange;
            // The decoded history has no process-local Instant to restore.
            assert!(sample.is_none());
            if action == "FirstAfterHistory" {
                (7, retain(&mut last, &mut sample, candidate(2, at, 1)))
            } else {
                (6, false)
            }
        }
        _ => panic!("unknown retention scenario"),
    };
    let mut next = before.clone();
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next.insert("payload", last.as_ref().map_or(0, payload_id));
    next.insert(
        "stamp",
        last.as_ref().map_or(0, |value| match value.sampled_at_ms {
            200 => 1,
            100 => 2,
            _ => 3,
        }),
    );
    next.insert(
        "sample",
        sample.map_or(0, |sample| sample.duration_since(at).as_secs() as i64),
    );
    next
}

#[test]
fn retention_conforms_and_renewing_replayed_history_is_detected() {
    let model = aterm_models_db::account_exchange_retention_model();
    check_model(&model);
    let initial = model.init_state();
    for action in &model.actions {
        let next = retention_transition(&initial, action.name);
        assert!(
            transition_admitted(&model, &initial, &next, action.name),
            "{}: {next:?}",
            action.name
        );
    }
    // Executable unsafe policy: replacing an equal original sample with a
    // different payload is exactly what strict comparison must reject.
    let at = Instant::now();
    let mut last = None;
    let mut sample = None;
    assert!(retain(&mut last, &mut sample, candidate(1, at, 2)));
    let replay = candidate(2, at, 2);
    if sample.is_none_or(|old| replay.sampled_at >= old) {
        last = Some(replay.observation);
    }
    let mut unsafe_next = retention_transition(&initial, "SameSample");
    unsafe_next.insert("accepted", 1);
    unsafe_next.insert("payload", payload_id(last.as_ref().unwrap()));
    unsafe_next.insert("stamp", 2);
    assert!(!transition_admitted(
        &model,
        &initial,
        &unsafe_next,
        "SameSample"
    ));
    assert!(!model.check_invariant("StrictSampleOrder", &unsafe_next));
}

const ADMISSION_ACTIONS: &[&str] = &[
    "Valid",
    "DifferentHash",
    "DifferentCount",
    "MissingLocal",
    "MissingPeer",
    "WrongScope",
    "LocalChanged",
    "PeerChanged",
    "ControlChanged",
    "FinalInventoryChanged",
    "FinalScopeChanged",
    "ZeroStamp",
    "BackwardsSample",
    "Deferred",
    "NoActor",
    "ForeignActor",
    "NotStarted",
    "Cancelled",
    "ControlError",
    "MembershipError",
    "MissingApplication",
    "ApplicationError",
    "Interrupted",
    "InventoryDeferral",
    "BatchDeferral",
    "HandoffError",
    "MissingWitness",
    "CompletedScopeChanged",
    "CompletedInventoryChanged",
    "CompletedDeferred",
    "OfferedSubset",
    "LaterWallRollback",
    "CompletedPeerInventoryChanged",
    "CompletedPeerInventoryMissing",
];

fn inventory_context(at: Instant) -> InventoryObservation {
    InventoryObservation {
        scope: Scope::new(
            Trust {
                network: [1; 16],
                authority: [2; 32],
                genesis: [3; 32],
            },
            [4; 16],
            1,
        )
        .unwrap(),
        local_actor: [5; 48],
        peer_actor: [6; 48],
        control_observation: [7; 64],
        inventory: InventorySummary {
            count: 2,
            fingerprint: [8; 32],
        },
        observed_at_ms: 200,
        sampled_at: at,
    }
}

fn inventory_report(before: InventoryObservation) -> Report {
    Report {
        table: Some(before.scope.digest()),
        local_known: Some(before.inventory),
        peer_known: Some(before.inventory),
        local_offered: Some(before.inventory),
        ..Report::default()
    }
}

fn completed(report: Report, peer: [u8; 48]) -> RepairRound {
    RepairRound {
        target: None,
        actor: Some(peer),
        connection: None,
        exchange_started: true,
        cancelled: false,
        phase: Some(crate::control::Phase::ControlComplete),
        control: Ok(crate::control::Report::default()),
        membership: None,
        membership_error: None,
        application: Some(report),
        application_error: None,
        application_interrupted: None,
        application_deferral: None,
        application_batch_deferral: None,
        handoff: None,
        completed_at: Instant::now(),
        completed_at_ms: 500,
        dropped_before: 0,
    }
}

fn admitted(
    action: &str,
    unsafe_counts: bool,
    unsafe_completion: bool,
) -> (
    InventoryObservation,
    Option<MatchingInventory>,
    Option<MatchingInventory>,
) {
    let at = Instant::now();
    let mut before = inventory_context(at);
    let mut after = InventoryObservation {
        sampled_at: at + Duration::from_secs(1),
        ..before
    };
    let mut report = inventory_report(before);
    let original = MatchingInventory::from_observations(&report, before, after).unwrap();
    match action {
        "DifferentHash" => report.peer_known.as_mut().unwrap().fingerprint = [9; 32],
        "DifferentCount" => report.peer_known.as_mut().unwrap().count += 1,
        "MissingLocal" => report.local_known = None,
        "MissingPeer" => report.peer_known = None,
        "WrongScope" => report.table = Some([9; 32]),
        "LocalChanged" => after.local_actor = [9; 48],
        "PeerChanged" => after.peer_actor = [9; 48],
        "ControlChanged" => after.control_observation = [9; 64],
        "FinalInventoryChanged" => after.inventory.fingerprint = [9; 32],
        "FinalScopeChanged" => after.scope = Scope::new(before.scope.trust(), [9; 16], 1).unwrap(),
        "ZeroStamp" => before.observed_at_ms = 0,
        "BackwardsSample" => after.sampled_at = at - Duration::from_secs(1),
        "Deferred" => report.deferred = 1,
        "OfferedSubset" => report.local_offered = None,
        "LaterWallRollback" => after.observed_at_ms = 100,
        _ => {}
    }
    report.matching_inventory = if unsafe_counts {
        // Negative control: equal counts incorrectly substitute for exact IDs.
        (report.local_known.map(|value| value.count) == report.peer_known.map(|value| value.count))
            .then_some(original)
    } else {
        MatchingInventory::from_observations(&report, before, after)
    };
    let constructed = report.matching_inventory;
    let mut round = completed(report, before.peer_actor);
    match action {
        "NoActor" => round.actor = None,
        "ForeignActor" => round.actor = Some([9; 48]),
        "NotStarted" => round.exchange_started = false,
        "Cancelled" => round.cancelled = true,
        "ControlError" => round.control = Err("fixed fixture failure".into()),
        "MembershipError" => round.membership_error = Some("fixed fixture failure".into()),
        "MissingApplication" => round.application = None,
        "ApplicationError" => round.application_error = Some("fixed fixture failure".into()),
        "Interrupted" => round.application_interrupted = Some(Report::default()),
        "InventoryDeferral" => {
            round.application_deferral = Some(crate::replica::InventoryDeferral {
                inventory: "operations",
                local_error: None,
                retaining_primary_conflicts: false,
                peer_unavailable: true,
            })
        }
        "BatchDeferral" => {
            round.application_batch_deferral = Some(crate::replica::BatchDeferral {
                batch: "operations",
                stage: "ingest",
                reason: "fixed fixture failure".into(),
            })
        }
        "HandoffError" => round.handoff = Some(Err("fixed fixture failure".into())),
        "MissingWitness" => round.application.as_mut().unwrap().matching_inventory = None,
        "CompletedScopeChanged" => round.application.as_mut().unwrap().table = Some([9; 32]),
        "CompletedInventoryChanged" => {
            round
                .application
                .as_mut()
                .unwrap()
                .local_known
                .as_mut()
                .unwrap()
                .fingerprint = [9; 32]
        }
        "CompletedDeferred" => round.application.as_mut().unwrap().deferred = 1,
        "CompletedPeerInventoryChanged" => {
            round
                .application
                .as_mut()
                .unwrap()
                .peer_known
                .as_mut()
                .unwrap()
                .fingerprint = [9; 32];
        }
        "CompletedPeerInventoryMissing" => round.application.as_mut().unwrap().peer_known = None,
        _ => {}
    }
    let witness = if unsafe_completion {
        // Negative control: publishing the optional application evidence
        // directly skips the authenticated completed-round admission.
        round.application.and_then(|value| value.matching_inventory)
    } else {
        round.matching_inventory()
    };
    (before, constructed, witness)
}

fn original(witness: MatchingInventory, before: InventoryObservation) -> bool {
    witness.scope() == before.scope
        && witness.local_actor() == before.local_actor
        && witness.peer_actor() == before.peer_actor
        && witness.control_observation() == before.control_observation
        && witness.inventory() == before.inventory
        && witness.observed_at_ms() == before.observed_at_ms
        && witness.sampled_at() == before.sampled_at
}

pub(crate) fn admission_transition(before: &State, action: &str) -> State {
    admission_transition_with(before, action, false, false)
}

fn admission_transition_with(
    state: &State,
    action: &str,
    unsafe_counts: bool,
    unsafe_completion: bool,
) -> State {
    let scenario = ADMISSION_ACTIONS
        .iter()
        .position(|name| *name == action)
        .unwrap()
        + 1;
    let (before, constructed, witness) = admitted(action, unsafe_counts, unsafe_completion);
    let mut next = state.clone();
    next.insert("scenario", scenario as i64);
    next.insert("constructed", i64::from(constructed.is_some()));
    next.insert("issued", i64::from(witness.is_some()));
    next.insert(
        "original",
        i64::from(constructed.is_some_and(|value| original(value, before))),
    );
    next
}

#[test]
fn admission_conforms_and_counts_only_or_uncompleted_publication_is_detected() {
    let model = aterm_models_db::account_exchange_admission_model();
    check_model(&model);
    let initial = model.init_state();
    for action in &model.actions {
        let next = admission_transition(&initial, action.name);
        assert!(
            transition_admitted(&model, &initial, &next, action.name),
            "{}: {next:?}",
            action.name
        );
    }
    let bad_hash = admission_transition_with(&initial, "DifferentHash", true, false);
    assert_eq!(bad_hash["constructed"], 1);
    assert_eq!(bad_hash["issued"], 0);
    assert!(!transition_admitted(
        &model,
        &initial,
        &bad_hash,
        "DifferentHash"
    ));
    assert!(!model.check_invariant("ExactAdmission", &bad_hash));
    for action in [
        "Cancelled",
        "CompletedPeerInventoryChanged",
        "CompletedPeerInventoryMissing",
        "NoActor",
        "ForeignActor",
        "ApplicationError",
        "Deferred",
    ] {
        let unsafe_next = admission_transition_with(&initial, action, action == "Deferred", true);
        assert!(!transition_admitted(&model, &initial, &unsafe_next, action));
        assert!(!model.check_invariant("ExactAdmission", &unsafe_next));
    }
}
