// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::network_deck::{Target, details as peer_details};
use aterm_types::network_state::{
    MachineIdentity, NetworkStateSnapshot, PeerObservation, Reachability, ServiceAvailability,
    Visibility,
};

fn observation(outcome: RepairOutcome) -> RepairObservation {
    RepairObservation {
        cause: None,
        completed_at_ms: 3000,
        outbound: true,
        stage: Some(RepairStage::Application),
        outcome,
        table: Some([0x73; 32]),
        sent: Some(2),
        received: Some(3),
        deferred: Some(5),
    }
}

fn snapshot() -> NetworkStateSnapshot {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.peers.push(PeerObservation {
        slot: 2,
        identity: MachineIdentity {
            machine_id: [2; 16],
            incarnation: [1; 16],
            label: None,
        },
        first_seen_ms: 1000,
        last_seen_ms: 2000,
        last_checked_ms: None,
        last_repair: None,
        last_authenticated_endpoint: None,
        reachability: Reachability::LastSeen,
        instances: None,
        samples: Vec::new(),
    });
    snapshot.services.accounts.availability = ServiceAvailability::Cached;
    snapshot.services.secrets.availability = ServiceAvailability::Unknown;
    snapshot
}

fn row<'a>(rows: &'a [(String, String)], key: &str) -> &'a str {
    rows.iter()
        .find(|(name, _)| name == key)
        .unwrap()
        .1
        .as_str()
}

#[test]
fn retained_attempts_stay_distinct_without_changing_presence_or_service_access() {
    let mut snapshot = snapshot();
    let accounts = peer_details(&snapshot, Target::Accounts);
    let secrets = peer_details(&snapshot, Target::Secrets);
    let absent = peer_details(&snapshot, Target::Peer(2));
    let mut rendered_outcomes = vec![row(&absent, "Last repair").to_owned()];
    for outcome in [
        RepairOutcome::Failed,
        RepairOutcome::Deferred,
        RepairOutcome::Exchanged,
        RepairOutcome::Cancelled,
        RepairOutcome::NotObserved,
        RepairOutcome::NoCommonTable,
    ] {
        snapshot.network.peers[0].last_repair = Some(observation(outcome));
        let before = snapshot.clone();
        let rows = peer_details(&snapshot, Target::Peer(2));
        let rendered = row(&rows, "Last repair");
        assert!(!rendered_outcomes.iter().any(|prior| prior == rendered));
        rendered_outcomes.push(rendered.to_owned());
        assert_eq!(peer_details(&snapshot, Target::Accounts), accounts);
        assert_eq!(peer_details(&snapshot, Target::Secrets), secrets);
        assert_eq!(snapshot, before);
        for (visibility, sampled) in [(Visibility::Active, 4000), (Visibility::Unavailable, 0)] {
            let mut aged = before.clone();
            aged.network.visibility = visibility;
            aged.network.observed_at_ms = sampled;
            let projected = peer_details(&aged, Target::Peer(2));
            for key in [
                "Last repair",
                "Repair time",
                "Record receipts",
                "Repair table",
            ] {
                assert_eq!(row(&projected, key), row(&rows, key));
            }
        }
    }
    snapshot.network.peers[0].last_repair = None;
    assert_eq!(peer_details(&snapshot, Target::Peer(2)), absent);
}

#[test]
fn each_unobserved_receipt_is_distinct_from_an_observed_zero() {
    fn field(attempt: &mut RepairObservation, index: usize) -> &mut Option<u64> {
        match index {
            0 => &mut attempt.sent,
            1 => &mut attempt.received,
            _ => &mut attempt.deferred,
        }
    }
    for index in 0..3 {
        let mut unknown = observation(RepairOutcome::Failed);
        let mut zero = unknown.clone();
        *field(&mut unknown, index) = None;
        *field(&mut zero, index) = Some(0);
        assert_ne!(
            row(&details(Some(&unknown)), "Record receipts"),
            row(&details(Some(&zero)), "Record receipts")
        );
    }
}
