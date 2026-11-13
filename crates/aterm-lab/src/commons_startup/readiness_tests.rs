// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::{age::Identity, commons::binding::Request};
use aterm_types::network_state::{MachineIdentity, PeerObservation};

fn binding(n: u8) -> Binding {
    Request::create(
        [1; 16],
        [2; 32],
        [n; 16],
        [n; 16],
        &Identity::from_secret_bytes([n; 32]),
        &[n; 32],
    )
    .unwrap()
    .binding()
    .clone()
}

fn fixture(now: Instant) -> ([Observation; 2], [Binding; 2]) {
    let expected = [binding(3), binding(4)];
    let observed = [0, 1].map(|index| {
        let local = &expected[index];
        let peer = &expected[1 - index];
        Observation {
            trust: Trust {
                network: [1; 16],
                authority: [2; 32],
                genesis: [5; 32],
            },
            requested: local.clone(),
            sampled_at: now,
            admission: Some(Admission {
                current: true,
                local: local.clone(),
                role: Role::Writer,
                log_backed: true,
                head: [6; 32],
                history_head: [7; 32],
                basis: [8; 32],
                generation: 1,
                members: expected
                    .iter()
                    .map(|binding| (binding.clone(), Role::Writer))
                    .collect(),
            }),
            network: NetworkObservation {
                revision: 1,
                observed_at_ms: 100_000,
                via_local_process: Some(42 + index as u32),
                visibility: Visibility::Active,
                history: HistoryState::Ready,
                observer: Some(MachineIdentity {
                    machine_id: *local.machine(),
                    incarnation: *local.incarnation(),
                    label: None,
                }),
                peers: vec![PeerObservation {
                    slot: 1,
                    identity: MachineIdentity {
                        machine_id: *peer.machine(),
                        incarnation: *peer.incarnation(),
                        label: None,
                    },
                    first_seen_ms: 99_000,
                    last_seen_ms: 100_000,
                    last_checked_ms: Some(100_000),
                    last_authenticated_endpoint: None,
                    last_repair: None,
                    reachability: Reachability::Reachable,
                    instances: None,
                    samples: Vec::new(),
                    lease_ms: None,
                }],
                ..Default::default()
            },
        }
    });
    (observed, expected)
}

#[test]
fn active_transport_cannot_replace_current_shared_writer_admission() {
    let now = Instant::now();
    let (observed, expected) = fixture(now);
    let valid =
        |value: &[Observation; 2]| ready(value, &expected, [42, 43], true, now, Some(100_100));
    assert!(valid(&observed));
    for field in 0..13 {
        let mut pending = observed.clone();
        let local = &mut pending[0];
        match field {
            0 => local.admission = None,
            1 => local.admission.as_mut().unwrap().members.truncate(1),
            2 => local.admission.as_mut().unwrap().current = false,
            3 => local.trust.genesis = [9; 32],
            4 => local.admission.as_mut().unwrap().head = [9; 32],
            5 => local.admission.as_mut().unwrap().basis = [9; 32],
            6 => local.admission.as_mut().unwrap().generation = 2,
            7 => local.admission.as_mut().unwrap().role = Role::Reader,
            8 => local.admission.as_mut().unwrap().log_backed = false,
            9 => local.requested = binding(9),
            10 => local.admission.as_mut().unwrap().members[1].1 = Role::Reader,
            11 => local.admission.as_mut().unwrap().history_head = [9; 32],
            _ => {
                let peer = &expected[1];
                let other_writer = Request::create(
                    *peer.network(),
                    *peer.authority(),
                    *peer.machine(),
                    *peer.incarnation(),
                    &Identity::from_secret_bytes([9; 32]),
                    &[9; 32],
                )
                .unwrap()
                .binding()
                .clone();
                local.admission.as_mut().unwrap().members[1].0 = other_writer;
            }
        }
        assert!(!valid(&pending), "unready membership field {field}");
    }
}

#[test]
fn admitted_writers_still_need_exact_current_peer_contacts() {
    let now = Instant::now();
    let (observed, expected) = fixture(now);
    let valid =
        |value: &[Observation; 2]| ready(value, &expected, [42, 43], true, now, Some(100_100));
    for field in 0..7 {
        let mut pending = observed.clone();
        let network = &mut pending[0].network;
        match field {
            0 => network.peers.clear(),
            1 => network.peers[0].identity.incarnation = [9; 16],
            2 => network.peers[0].reachability = Reachability::LastSeen,
            3 => network.via_local_process = Some(99),
            4 => network.observed_at_ms = 100_101,
            5 => network.peers[0].last_seen_ms = 80_000,
            _ => network.peers[0].last_checked_ms = None,
        }
        assert!(!valid(&pending), "unready contact field {field}");
    }
    assert!(!ready(
        &observed,
        &expected,
        [42, 43],
        true,
        now + Duration::from_secs(16),
        Some(100_100)
    ));
    assert!(!ready(&observed, &expected, [42, 43], true, now, None));
}
