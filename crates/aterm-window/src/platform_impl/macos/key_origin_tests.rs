// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

const PERSON: InputOrigin = InputOrigin::Person;
const POSTED: InputOrigin = InputOrigin::Posted;

fn key(keycode: u16) -> NativeInputEvent {
    NativeInputEvent { kind: 10, keycode, timestamp: 1_000.0 + f64::from(keycode), source_pid: 0 }
}

#[test]
fn queued_callbacks_keep_their_one_source_classification() {
    let calls = Cell::new(0);
    let decision = Cell::new(POSTED);
    let pending = {
        let _source = source_with(key(14), |seen| {
            assert_eq!(seen, key(14));
            calls.set(calls.get() + 1);
            decision.get()
        });
        [capture(), capture()]
    };
    assert_eq!(calls.get(), 1);
    decision.set(PERSON);
    let _later = source_with(key(0), |_| PERSON);
    for callback in pending {
        callback.dispatch(|| {
            assert_eq!(window_event_origin(), Some(POSTED));
            let _same = source_with(key(14), |_| {
                panic!("the retained native key cannot be classified again")
            });
            capture().dispatch(|| assert_eq!(window_event_origin(), Some(POSTED)));
        });
    }
    assert_eq!(calls.get(), 1, "delivery never consults the classifier again");
    assert_eq!(window_event_origin(), None);
    capture().dispatch(|| assert_eq!(window_event_origin(), Some(PERSON)));
}

#[test]
fn queued_callbacks_keep_their_original_native_event_beside_a_later_key() {
    let first = key(14);
    let later = key(0);
    let pending = {
        let _source = source_with(first, |_| POSTED);
        assert_eq!(window_event_native_input(), None, "not in callback delivery");
        capture()
    };
    let _later = source_with(later, |_| PERSON);
    pending.dispatch(|| {
        assert_eq!(window_event_native_input(), Some(first));
        assert_ne!(window_event_native_input(), Some(later));
        assert_eq!(window_event_origin(), Some(POSTED));
    });
    assert_eq!(window_event_native_input(), None);
    capture().dispatch(|| {
        assert_eq!(window_event_native_input(), Some(later));
        assert_eq!(window_event_origin(), Some(PERSON));
    });
}

#[test]
fn native_event_clock_is_masked_by_keyless_callbacks_and_restored_on_unwind() {
    let keyless = capture();
    let first = key(14);
    let pending = {
        let _source = source_with(first, |_| POSTED);
        capture()
    };
    pending.dispatch(|| {
        assert_eq!(window_event_native_input(), Some(first));
        let result = std::panic::catch_unwind(|| {
            keyless.dispatch(|| {
                assert_eq!(window_event_native_input(), None);
                assert_eq!(window_event_origin(), Some(PERSON));
                panic!("leave the keyless callback");
            });
        });
        assert!(result.is_err());
        assert_eq!(window_event_native_input(), Some(first));
        assert_eq!(window_event_origin(), Some(POSTED));
        std::thread::spawn(|| {
            assert_eq!(window_event_native_input(), None);
            assert_eq!(window_event_origin(), None);
        })
        .join()
        .unwrap();
    });
    assert_eq!(window_event_native_input(), None);
    assert_eq!(window_event_origin(), None);
    let result = std::panic::catch_unwind(|| {
        pending.dispatch(|| panic!("leave the native callback"));
    });
    assert!(result.is_err());
    assert_eq!(window_event_native_input(), None);
    assert_eq!(window_event_origin(), None);
}

#[test]
fn a_physical_callback_does_not_borrow_a_later_posted_keys_mark() {
    let physical = {
        let _source = source_with(key(0), |_| PERSON);
        capture()
    };
    let _later = source_with(key(14), |_| POSTED);
    physical.dispatch(|| assert_eq!(window_event_origin(), Some(PERSON)));
    capture().dispatch(|| assert_eq!(window_event_origin(), Some(POSTED)));
    assert_eq!(window_event_origin(), None);
}

/// A click's callback keeps the verdict of its own native event, a third one beside a
/// posted key's and a person's.
#[test]
fn a_clicks_callback_keeps_its_own_verdict_beside_a_keys() {
    let click = NativeInputEvent { kind: 2, keycode: 0, timestamp: 1_000.5, source_pid: 7 };
    let relayed = {
        let _source = source_with(click, |seen| {
            assert_eq!(seen, click);
            InputOrigin::Relayed
        });
        capture()
    };
    let _later = source_with(key(14), |_| POSTED);
    relayed.dispatch(|| assert_eq!(window_event_origin(), Some(InputOrigin::Relayed)));
    capture().dispatch(|| assert_eq!(window_event_origin(), Some(POSTED)));
}

#[test]
fn the_same_native_key_reuses_its_decision_after_a_different_nested_key() {
    let calls = Cell::new(0);
    let _first = source_with(key(14), |_| {
        calls.set(calls.get() + 1);
        POSTED
    });
    {
        let _physical = source_with(key(0), |_| {
            calls.set(calls.get() + 1);
            PERSON
        });
        capture().dispatch(|| assert_eq!(window_event_origin(), Some(PERSON)));
    }
    let _same = source_with(key(14), |_| panic!("the enclosing key was already classified"));
    capture().dispatch(|| assert_eq!(window_event_origin(), Some(POSTED)));
    assert_eq!(calls.get(), 2, "only the two distinct native keys were classified");
}

#[test]
fn keyless_nested_callbacks_mask_and_restore_both_scopes() {
    let keyless = capture();
    let posted = {
        let _source = source_with(key(14), |_| POSTED);
        capture()
    };
    posted.dispatch(|| {
        assert_eq!(window_event_origin(), Some(POSTED));
        keyless.dispatch(|| {
            assert_eq!(window_event_origin(), Some(PERSON));
            capture().dispatch(|| assert_eq!(window_event_origin(), Some(PERSON)));
        });
        assert_eq!(window_event_origin(), Some(POSTED));
        capture().dispatch(|| assert_eq!(window_event_origin(), Some(POSTED)));
    });
    assert_eq!(window_event_origin(), None);
    capture().dispatch(|| assert_eq!(window_event_origin(), Some(PERSON)));
}

#[test]
fn a_nested_native_key_does_not_inherit_the_outer_callback_verdict() {
    let posted = {
        let _source = source_with(key(14), |_| POSTED);
        capture()
    };
    posted.dispatch(|| {
        assert_eq!(window_event_origin(), Some(POSTED));
        assert_eq!(window_event_native_input(), Some(key(14)));
        {
            let _physical = source_with(key(0), |_| PERSON);
            assert_eq!(window_event_origin(), None, "the native handler is not a window callback");
            assert_eq!(window_event_native_input(), None);
            capture().dispatch(|| {
                assert_eq!(window_event_origin(), Some(PERSON));
                assert_eq!(window_event_native_input(), Some(key(0)));
            });
            assert_eq!(window_event_origin(), None);
            assert_eq!(window_event_native_input(), None);
        }
        assert_eq!(window_event_origin(), Some(POSTED));
        assert_eq!(window_event_native_input(), Some(key(14)));
    });
    assert_eq!(window_event_origin(), None);
    assert_eq!(window_event_native_input(), None);
}
