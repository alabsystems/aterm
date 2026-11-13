// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn restored_checkpoint_requires_original_peer_history_without_live_evidence() {
    let lost = super::super::tests::observation();
    let peer = &lost.peers[0].identity;
    let mut retained = lost.clone();
    retained.visibility = Visibility::Unavailable;
    retained.observer = None;
    retained.via_local_process = None;
    retained.peers[0].reachability = Reachability::Unknown;
    retained.peers[0].last_checked_ms = None;
    assert!(retained_matches(&retained, &lost, peer));
    for field in 0..12 {
        let mut wrong = retained.clone();
        match field {
            0 => wrong.history = HistoryState::Unavailable,
            1 => wrong.visibility = Visibility::Active,
            2 => wrong.observer = lost.observer.clone(),
            3 => wrong.via_local_process = lost.via_local_process,
            4 => wrong.revision -= 1,
            5 => wrong.observed_at_ms -= 1,
            6 => wrong.peers.clear(),
            7 => wrong.peers[0].identity.incarnation = [9; 16],
            8 => wrong.peers[0].slot += 1,
            9 => wrong.peers[0].first_seen_ms += 1,
            10 => wrong.peers[0].last_seen_ms += 1,
            _ => wrong.peers[0].last_checked_ms = Some(99_000),
        }
        assert!(!retained_matches(&wrong, &lost, peer), "field {field}");
    }
    retained.peers[0].reachability = Reachability::Reachable;
    assert!(!retained_matches(&retained, &lost, peer));
}
