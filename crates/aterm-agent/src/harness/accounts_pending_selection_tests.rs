// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

#[test]
fn pending_selection_preserves_account_wire_and_refuses_unknown_intents() {
    let request = PinRequest::new("work", "a".repeat(32)).unwrap();
    let state = State {
        pending_pin: Some(request.clone().into()),
        ..State::default()
    };
    let mut wire: Value = aterm_json::from_str(&state.to_json()).unwrap();
    let pending = wire.get("pending_pin").unwrap();
    assert_eq!(pending.as_object().unwrap().len(), 2);
    assert_eq!(pending.get("label").and_then(Value::as_str), Some("work"));
    assert_eq!(
        pending.get("nonce").and_then(Value::as_str),
        Some("a".repeat(32).as_str())
    );
    assert!(pending.get("kind").is_none());
    assert_eq!(
        State::from_json(&state.to_json(), NOW)
            .unwrap()
            .pending_pin
            .as_ref()
            .and_then(PendingSelection::account),
        Some(&request)
    );

    // Persisted intent decoding remains portable even where this process cannot
    // admit a new manual queue operation.
    let next = aterm_json::json!({"kind": "next", "nonce": "b".repeat(32)});
    wire.as_object_mut()
        .unwrap()
        .insert("pending_pin".into(), next.clone());
    let historical = State::from_json(&aterm_json::to_string(&wire).unwrap(), NOW).unwrap();
    assert!(
        historical
            .pending_pin
            .as_ref()
            .and_then(PendingSelection::next)
            .is_some()
    );
    let roundtrip: Value = aterm_json::from_str(&historical.to_json()).unwrap();
    assert_eq!(roundtrip.get("pending_pin"), Some(&next));

    for pending in [
        Value::Null,
        aterm_json::json!({"nonce": "a".repeat(32)}),
        aterm_json::json!({"kind": "unknown", "nonce": "a".repeat(32)}),
        aterm_json::json!({"kind": "next", "nonce": "bad"}),
        aterm_json::json!({"kind": "next", "nonce": "a".repeat(32), "label": "work"}),
        aterm_json::json!({"label": "work", "nonce": "a".repeat(32), "extra": true}),
    ] {
        wire.as_object_mut()
            .unwrap()
            .insert("pending_pin".into(), pending);
        assert!(State::from_json(&aterm_json::to_string(&wire).unwrap(), NOW).is_err());
    }
}

#[test]
fn next_selection_replaces_one_slot_without_stale_account_cancellation() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(root.path());
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = true;
            state.active = Some("preferred".into());
            Ok(Vec::new())
        })
        .unwrap();
    let old = store.request_pin("work", NOW).unwrap();
    if manual_signin_capability() == ManualSigninCapability::Unavailable {
        let before = std::fs::read(store.state_file()).unwrap();
        assert!(store.request_next(NOW).is_err());
        assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
        let state = store.load(NOW).unwrap().0;
        assert!(state.pinned);
        assert_eq!(state.active.as_deref(), Some("preferred"));
        assert_eq!(
            state
                .pending_pin
                .as_ref()
                .and_then(PendingSelection::account),
            Some(&old)
        );
        return;
    }
    let first = store.request_next(NOW).unwrap();
    let state = store.load(NOW).unwrap().0;
    assert!(!state.pinned);
    assert_eq!(state.active.as_deref(), Some("preferred"));
    assert_eq!(
        state.pending_pin.as_ref().and_then(PendingSelection::next),
        Some(&first)
    );
    assert!(
        state
            .pending_pin
            .as_ref()
            .and_then(PendingSelection::account)
            .is_none()
    );
    let wire: Value = aterm_json::from_str(&state.to_json()).unwrap();
    assert_eq!(
        wire.get("pending_pin")
            .unwrap()
            .get("kind")
            .and_then(Value::as_str),
        Some("next")
    );
    assert_eq!(
        State::from_json(&state.to_json(), NOW).unwrap().pending_pin,
        state.pending_pin
    );
    store.cancel_pin_request(&old, NOW).unwrap();
    assert_eq!(store.load(NOW).unwrap().0.pending_pin, state.pending_pin);
    let second = store.request_next(NOW).unwrap();
    assert_ne!(first, second);
    let latest = store.request_pin("other", NOW).unwrap();
    assert_eq!(
        store
            .load(NOW)
            .unwrap()
            .0
            .pending_pin
            .as_ref()
            .and_then(PendingSelection::account),
        Some(&latest)
    );
}
