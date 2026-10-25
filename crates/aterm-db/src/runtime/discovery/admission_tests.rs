// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn peer(state: i64, seen: Instant) -> Peer {
    Peer {
        seen,
        next: seen + Duration::from_secs(30),
        verified: matches!(state, 4 | 5 | 6 | 8),
        busy: matches!(state, 2 | 6 | 7 | 8),
        failed: matches!(state, 3 | 5 | 7 | 8),
        repair: Repair::default(),
    }
}

pub(crate) fn replacement_transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "AnotherLeft" => next.insert("left", state["left"] + 1),
        "AnotherRight" => next.insert("right", state["right"] + 1),
        "GlobalLimit" => next.insert("global", state["global"] + 1),
        "RightOlder" => next.insert("older", 2),
        "Choose" => next.insert("phase", 1),
        "Observe" => {
            let now = Instant::now();
            let mut hints = Hints::new([11; 32]);
            let global = state["global"] > 0;
            let separate = state["global"] == 1;
            let left = SocketAddr::from(([10, 1, 1, 1], 1001));
            let right = SocketAddr::from(([10, 1, 1, if separate { 2 } else { 1 }], 1002));
            for (i, (address, status)) in [(left, state["left"]), (right, state["right"])]
                .into_iter()
                .enumerate()
            {
                let older = i as i64 + 1 == state["older"];
                hints.peers.insert(
                    address,
                    peer(status, now + Duration::from_secs(u64::from(!older))),
                );
            }
            if global {
                for n in 0..MAX_PEERS - 2 {
                    hints.peers.insert(
                        SocketAddr::from(([10, 1, 2, n as u8 + 1], 1000)),
                        // At simultaneous limits, an older global failure must
                        // survive: first solve the candidate's own IP limit.
                        peer(if !separate && n == 0 { 3 } else { 4 }, now),
                    );
                }
            }
            let before: BTreeMap<_, _> = hints
                .peers
                .iter()
                .map(|(address, peer)| (*address, format!("{peer:?}")))
                .collect();
            let source = SocketAddr::from(([10, 1, 1, if separate { 3 } else { 1 }], 9));
            let candidate = SocketAddr::new(source.ip(), 1003);
            hints.observe(&Hints::new([12; 32]).wire(candidate.port()), source, now);
            let removed: Vec<_> = before
                .keys()
                .filter(|address| !hints.peers.contains_key(address))
                .copied()
                .collect();
            assert!(removed.len() <= 1, "one hint evicted multiple endpoints");
            for (address, peer) in &hints.peers {
                if let Some(old) = before.get(address) {
                    assert_eq!(old, &format!("{peer:?}"), "surviving endpoint changed");
                }
            }
            assert!(hints.count() <= if global { MAX_PEERS } else { PER_IP });
            let added = hints.peers.get(&candidate);
            if let Some(peer) = added {
                assert!(!peer.busy && !peer.failed);
                assert!(peer.repair.actor().is_none());
            }
            next.insert("phase", 2);
            next.insert(
                "victim",
                removed
                    .iter()
                    .find(|address| **address == left || **address == right)
                    .map_or(0, |address| if *address == left { 1 } else { 2 }),
            );
            next.insert("added", i64::from(added.is_some()));
            next.insert(
                "authority",
                i64::from(added.is_some_and(|peer| peer.verified)),
            );
            next.insert(
                "collateral",
                removed
                    .iter()
                    .filter(|address| **address != left && **address != right)
                    .count() as i64,
            );
            None
        }
        _ => panic!("unknown discovery action"),
    };
    next
}

#[test]
fn full_hint_cache_replacement_conforms_at_both_limits() {
    let model = aterm_models_db::discovery_replacement_model();
    assert_eq!(model.mutant_dials(), (1..=10).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut observed = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = replacement_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                observed += usize::from(action.name == "Observe");
            }
        }
    }
    assert_eq!(observed, 8 * 8 * 3 * 2);
    let mut state = model.init_state();
    state.insert("left", 5);
    state.insert("right", 5);
    state.insert("phase", 1);
    let mut stale = replacement_transition(&state, "Observe");
    stale.insert("added", 0);
    stale.insert("victim", 0);
    assert!(!model.check_invariant("RecoverFailedSlot", &stale));
    let mut protected = replacement_transition(&state, "Observe");
    protected.insert("left", 8);
    protected.insert("victim", 1);
    assert!(!model.check_invariant("ExistingProtected", &protected));
    state.insert("global", 1);
    let mut unrelated = replacement_transition(&state, "Observe");
    unrelated.insert("victim", 1);
    unrelated.insert("added", 1);
    assert!(!model.check_invariant("UnrelatedIPCannotDisplaceVerified", &unrelated));
}
