// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::{derive::Model, interp::State, verify};
use aterm_types::network_state::AccountsExchangeObservation;

const WALL: u64 = 90_000;

fn identity(id: u8) -> MachineIdentity {
    MachineIdentity {
        machine_id: [id; 16],
        incarnation: [id + 1; 16],
        label: None,
    }
}

fn peer(slot: u64, at: u64, reachability: Reachability) -> PeerObservation {
    PeerObservation {
        slot,
        identity: identity(u8::try_from(slot).unwrap()),
        first_seen_ms: at,
        last_seen_ms: at,
        last_checked_ms: Some(at),
        last_repair: None,
        last_authenticated_endpoint: Some(AuthenticatedEndpoint {
            address: "127.0.0.1:41384".parse().unwrap(),
            observed_at_ms: at,
        }),
        reachability,
        instances: None,
        samples: vec![PresenceSample {
            at_ms: at,
            state: reachability,
        }],
        lease_ms: None,
    }
}

fn exercise(case: i64, historical_clock_guard: bool) -> [(&'static str, i64); 7] {
    let requested = Instant::now() + Duration::from_secs(60);
    let mut observation = NetworkObservation {
        observed_at_ms: WALL,
        visibility: Visibility::Active,
        history: HistoryState::Ready,
        observer: Some(identity(1)),
        peers: vec![
            peer(
                3,
                if case == 0 { 80_000 } else { 100_000 },
                Reachability::LastSeen,
            ),
            peer(5, WALL, Reachability::Reachable),
        ],
        accounts_exchange: Some(AccountsExchangeObservation {
            scope_digest: [1; 32],
            local_actor: [2; 48],
            control_observation: [3; 64],
            inventory_count: 7,
            inventory_fingerprint: [4; 32],
            peer: identity(7),
            sampled_at_ms: 90_002,
        }),
        ..NetworkObservation::default()
    };
    match case {
        2 => observation.peers[1].first_seen_ms = 100_000,
        3 => observation.observed_at_ms = WALL - 15_001,
        4 => observation.observed_at_ms = WALL + 1,
        9 => observation.peers[1].last_seen_ms = WALL + 1,
        10 => observation.peers[1].last_checked_ms = Some(WALL + 1),
        11 => observation.peers[1].reachability = Reachability::LastSeen,
        12 => observation.peers[0].last_seen_ms = WALL,
        14 => observation.peers[1].last_checked_ms = None,
        16 => observation.observed_at_ms = 0,
        17 => observation.visibility = Visibility::Offline,
        20 => {
            observation.peers[0].samples[0].at_ms = 200_000;
            observation.peers[0]
                .last_authenticated_endpoint
                .as_mut()
                .unwrap()
                .observed_at_ms = 200_000;
        }
        _ => {}
    }
    let mut source = Publication::new(observation.clone(), requested);
    if case != 13 {
        source
            .contact_ages
            .insert(5, Duration::from_millis(if case == 8 { 15_000 } else { 0 }));
    }
    let age_ms = match case {
        5 => 15_001,
        6 | 7 => 15_000,
        _ => 0,
    };
    let mut wire = live_encode_publication(&source, age_ms).unwrap();
    if case == 15 {
        let mut value: Value = aterm_json::from_slice(&wire).unwrap();
        value.as_object_mut().unwrap().remove("source_age_ms");
        wire = aterm_json::to_vec(&value).unwrap();
    }
    let before_wire = wire.clone();
    let decoded = decode_publication(&wire, WALL, requested).and_then(|publication| {
        if historical_clock_guard
            && publication.observation.peers.iter().any(|peer| {
                peer.first_seen_ms > peer.last_seen_ms
                    || peer.last_seen_ms > WALL
                    || peer.last_checked_ms.is_some_and(|at| at > WALL)
                    || peer.samples.iter().any(|sample| sample.at_ms > WALL)
            })
        {
            // The former admission policy rejected retained history as if every row were current.
            Err(refused_at("peer-clock"))
        } else {
            Ok(publication)
        }
    });
    let current_clock = if case == 19 {
        requested - Duration::from_millis(1)
    } else {
        requested + Duration::from_millis(if matches!(case, 7 | 8) { 1 } else { 0 })
    };
    let current_wall = if case == 18 {
        WALL - 1
    } else if matches!(case, 7 | 8) {
        WALL + 1
    } else {
        WALL
    };
    let current = decoded
        .as_ref()
        .ok()
        .map(|publication| publication.current_at(current_clock, Some(current_wall)));
    let original_times = current.as_ref().is_some_and(|state| {
        state.peers.len() == observation.peers.len()
            && state
                .peers
                .iter()
                .zip(&observation.peers)
                .all(|(after, before)| {
                    after.first_seen_ms == before.first_seen_ms
                        && after.last_seen_ms == before.last_seen_ms
                        && after.last_checked_ms == before.last_checked_ms
                        && after.last_authenticated_endpoint == before.last_authenticated_endpoint
                        && after.samples == before.samples
                })
    });
    let receipt = current
        .as_ref()
        .is_some_and(|state| state.accounts_exchange == observation.accounts_exchange);
    let original_sample = decoded.as_ref().is_ok_and(|publication| {
        publication.sampled_at() == requested - Duration::from_millis(age_ms)
            && publication.observation.observed_at_ms == observation.observed_at_ms
    });
    assert_eq!(wire, before_wire);
    assert_eq!(source.observation(), &observation);
    [
        ("phase", 1),
        ("accepted", i64::from(decoded.is_ok())),
        (
            "active",
            i64::from(
                current
                    .as_ref()
                    .is_some_and(|state| state.visibility == Visibility::Active),
            ),
        ),
        (
            "reachable",
            current.as_ref().map_or(0, |state| {
                i64::try_from(
                    state
                        .peers
                        .iter()
                        .filter(|peer| peer.reachability == Reachability::Reachable)
                        .count(),
                )
                .unwrap()
            }),
        ),
        ("history", i64::from(original_times)),
        ("receipt", i64::from(receipt)),
        ("sample", i64::from(original_sample)),
    ]
}

pub(crate) fn relay_transition(before: &State, action: &str) -> State {
    let mut after = before.clone();
    match action {
        "Case" => {
            after.insert("case", before["case"] + 1);
        }
        "Read" => after.extend(exercise(before["case"], false)),
        _ => unreachable!(),
    }
    after
}

fn admits(model: &Model, before: &State, after: &State, action: &str) -> bool {
    let (accepted, why) = verify::validate_transition_tiered(
        model,
        &[],
        before,
        after,
        Some(action),
        "relay historical clock separation",
    );
    eprintln!("{} case={} {action}: {why}", model.name, before["case"]);
    accepted
}

#[test]
fn live_relay_retains_history_while_current_evidence_stays_bounded() {
    let model = aterm_models_db::relay_history_model();
    let coverage = verify::prove_and_catch_scalar(&model, "relay historical clock separation");
    eprintln!("{}: {coverage:?}", model.name);
    if verify::checkers_required() {
        assert_eq!(coverage, verify::Covered::InterpreterAndTy);
    }
    let faults = verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    let mut reads = 0;
    let mut fixtures = 0;
    for before in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &before) {
                let after = relay_transition(&before, action.name);
                assert!(
                    admits(&model, &before, &after, action.name),
                    "{before:?} -> {after:?}"
                );
                reads += usize::from(action.name == "Read");
                fixtures += usize::from(action.name == "Case");
            }
        }
    }
    assert_eq!(reads, 21);
    assert_eq!(fixtures, 20);
    for case in [1, 2] {
        let mut before = model.init_state();
        before.insert("case", case);
        let mut obsolete = before.clone();
        obsolete.extend(exercise(case, true));
        assert!(!admits(&model, &before, &obsolete, "Read"));
        assert!(!model.check_invariant("FreshHistoryDoesNotRefuseRead", &obsolete));
        assert!(!model.check_invariant("FreshHistoryKeepsInventoryReceipt", &obsolete));
    }
    eprintln!(
        "{}: {reads} production reads, {fixtures} fixture transitions, 2 executable historical-clock negative controls; {coverage:?}",
        model.name
    );
}
