// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use astream_wire::sync::Header;
use aterm_db::{
    accounts::proposals::Operation,
    database::{Candidate, Cell},
    table::{Scope, store::Receipt},
};
use aterm_secrets::commons::journal::Trust;
use aterm_types::network_state::{PeerObservation, Visibility};

fn identity(n: u8) -> MachineIdentity {
    MachineIdentity {
        machine_id: [n; 16],
        incarnation: [n; 16],
        label: None,
    }
}

pub(super) fn observation() -> NetworkObservation {
    NetworkObservation {
        revision: 10,
        observed_at_ms: 100_000,
        via_local_process: Some(42),
        observer: Some(identity(1)),
        visibility: Visibility::Active,
        history: HistoryState::Ready,
        peers: vec![PeerObservation {
            slot: 7,
            identity: identity(2),
            first_seen_ms: 90_000,
            last_seen_ms: 99_000,
            last_checked_ms: Some(99_000),
            last_authenticated_endpoint: None,
            last_repair: None,
            reachability: Reachability::Reachable,
            instances: None,
            samples: Vec::new(),
            lease_ms: None,
        }],
        ..Default::default()
    }
}

#[test]
fn peer_cycle_requires_a_continuing_observer_and_fresh_contact_after_loss() {
    let before = observation();
    let peer = identity(2);
    assert!(matches_stage(&before, 42, &peer, Stage::Before, None));
    let mut lost = before.clone();
    lost.revision += 1;
    lost.observed_at_ms = 116_000;
    lost.peers[0].reachability = Reachability::LastSeen;
    assert!(matches_stage(&lost, 42, &peer, Stage::Lost, Some(&before)));
    let mut reconnected = lost.clone();
    reconnected.revision += 1;
    reconnected.observed_at_ms = 117_000;
    reconnected.peers[0].reachability = Reachability::Reachable;
    // A replay with a newer publication revision cannot prove a new exchange.
    assert!(!matches_stage(
        &reconnected,
        42,
        &peer,
        Stage::Reconnected,
        Some(&lost)
    ));
    reconnected.peers[0].last_seen_ms = 116_500;
    reconnected.peers[0].last_checked_ms = Some(116_500);
    assert!(matches_stage(
        &reconnected,
        42,
        &peer,
        Stage::Reconnected,
        Some(&lost)
    ));
    for field in 0..7 {
        let mut wrong = reconnected.clone();
        match field {
            0 => wrong.via_local_process = Some(43),
            1 => wrong.observer = Some(identity(3)),
            2 => wrong.peers.clear(),
            3 => wrong.peers[0].identity.machine_id = [3; 16],
            4 => wrong.peers[0].identity.incarnation = [3; 16],
            5 => wrong.peers[0].slot += 1,
            _ => wrong.peers[0].first_seen_ms += 1,
        }
        assert!(!matches_stage(
            &wrong,
            42,
            &peer,
            Stage::Reconnected,
            Some(&lost)
        ));
    }
    for visibility in [Visibility::Offline, Visibility::Unavailable] {
        let mut unknown = lost.clone();
        unknown.visibility = visibility;
        unknown.peers[0].reachability = Reachability::Unknown;
        assert!(!matches_stage(
            &unknown,
            42,
            &peer,
            Stage::Lost,
            Some(&before)
        ));
    }
}

#[test]
fn restarted_observer_must_retain_absent_peer_without_current_lifetime_contact() {
    let mut lost = observation();
    lost.peers[0].reachability = Reachability::LastSeen;
    let peer = &lost.peers[0].identity;
    let mut restored = lost.clone();
    restored.revision += 1;
    restored.observed_at_ms += 1_000;
    restored.via_local_process = Some(43);
    restored.peers[0].last_checked_ms = None;
    assert!(matches_stage(
        &restored,
        43,
        peer,
        Stage::Restored,
        Some(&lost)
    ));
    assert!(!matches_stage(
        &restored,
        42,
        peer,
        Stage::Restored,
        Some(&lost)
    ));
    for field in 0..9 {
        let mut wrong = restored.clone();
        match field {
            0 => wrong.history = HistoryState::Unavailable,
            1 => wrong.observer = Some(identity(3)),
            2 => wrong.peers.clear(),
            3 => wrong.peers[0].slot += 1,
            4 => wrong.peers[0].first_seen_ms += 1,
            5 => wrong.peers[0].last_seen_ms += 1,
            6 => wrong.peers[0].last_checked_ms = Some(100_500),
            7 => wrong.peers[0].reachability = Reachability::Reachable,
            _ => wrong.revision = lost.revision,
        }
        assert!(
            !matches_stage(&wrong, 43, peer, Stage::Restored, Some(&lost)),
            "field {field}"
        );
    }
    let mut reconnected = restored.clone();
    reconnected.revision += 1;
    reconnected.observed_at_ms += 1_000;
    reconnected.peers[0].reachability = Reachability::Reachable;
    reconnected.peers[0].last_checked_ms = Some(reconnected.observed_at_ms);
    assert!(!matches_stage(
        &reconnected,
        43,
        peer,
        Stage::Reconnected,
        Some(&restored)
    ));
    reconnected.peers[0].last_seen_ms = reconnected.observed_at_ms;
    assert!(matches_stage(
        &reconnected,
        43,
        peer,
        Stage::Reconnected,
        Some(&restored)
    ));
}

#[test]
fn cached_availability_cannot_replace_an_exact_eligible_account_value() {
    let id = [1; 32];
    let value = json!({"provider":"example","method":"oauth","label":"synthetic"});
    let expected = [(id, "one", &value)];
    let mut snapshot = Snapshot {
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
        retained: 1,
        withheld: 0,
        cells: [(
            ("accounts".into(), "one".into()),
            Cell {
                stopped: false,
                candidates: vec![Candidate {
                    operation: id,
                    value: value.clone(),
                }],
            },
        )]
        .into(),
        operations: vec![Operation {
            header: Header {
                id,
                actor: [5; 48],
                sequence: 1,
                previous: None,
                parents: Vec::new(),
            },
            roster: [6; 32],
            epoch: 1,
            authored_ms: 10,
            receipt: Receipt {
                id,
                received_ms: 10,
                offset: 1,
            },
            request: None,
            reverts: None,
            available: true,
            catalog_eligible: true,
            eligible: true,
            duplicate_of: None,
            refusal: None,
        }],
    };
    assert!(cache_matches(&snapshot, &expected));
    let wrong = json!({"provider":"example","method":"oauth","label":"different"});
    assert!(!cache_matches(&snapshot, &[(id, "one", &wrong)]));
    assert!(!cache_matches(&snapshot, &[([9; 32], "one", &value)]));
    for field in 0..4 {
        let mut missing = snapshot.clone();
        match field {
            0 => missing.cells.clear(),
            1 => missing.operations.clear(),
            2 => missing.operations[0].eligible = false,
            _ => missing.withheld = 1,
        }
        assert!(!cache_matches(&missing, &expected));
    }
    snapshot.operations[0].available = false;
    assert!(!cache_matches(&snapshot, &expected));
}
