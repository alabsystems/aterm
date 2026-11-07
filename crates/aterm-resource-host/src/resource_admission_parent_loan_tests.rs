// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn edge() -> Edge {
    Edge {
        parent: [1; 16],
        child: [2; 16],
        nonce: [3; 16],
        cpu: 4,
    }
}
fn failed() -> io::Result<()> {
    Err(io::Error::other("injected acknowledgment loss"))
}

#[test]
fn ambiguous_writes_never_unpark_or_change_the_original_edge() {
    let original = edge();
    let mut state = RunState::default();
    assert!(state.intent(original, failed).is_err());
    assert!(!state.runnable());
    assert!(state.edge() == Some(original));
    let changed = Edge {
        nonce: [9; 16],
        ..original
    };
    assert!(state.intent(changed, || Ok(())).is_err());
    assert!(state.confirm_intent(changed, || Ok(())).is_err());
    state.confirm_intent(original, || Ok(())).unwrap();
    assert!(state.lend(original, failed).is_err());
    assert!(!state.runnable());
    assert!(state.returned(changed, || Ok(())).is_err());
    assert!(state.returned(original, failed).is_err());
    assert!(!state.runnable());
    state.returned(original, || Ok(())).unwrap();
    assert!(state.runnable());
    assert!(state.returned(original, || Ok(())).is_err());
}

#[test]
fn same_edge_can_retry_binding_and_positive_no_child_return() {
    let original = edge();
    let mut state = RunState::default();
    assert!(state.lend(original, || Ok(())).is_err());
    state.intent(original, || Ok(())).unwrap();
    // A real NoChild cancellation may return directly from pending; this unit
    // exercises only ordering, and does not fabricate that native proof.
    state.returned(original, || Ok(())).unwrap();
    state.intent(original, || Ok(())).unwrap();
    state.lend(original, || Ok(())).unwrap();
    state.lend(original, || Ok(())).unwrap();
    assert!(!state.runnable());
    state.returned(original, || Ok(())).unwrap();
    assert!(state.runnable());
}
