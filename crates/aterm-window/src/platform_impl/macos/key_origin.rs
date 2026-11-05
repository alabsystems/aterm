// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Retain the application's source classification of a native key, click or scroll across
//! deferred window callbacks.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::OnceLock;

use crate::event::InputOrigin;
use crate::platform::macos::NativeInputEvent;

static CLASSIFIER: OnceLock<fn(NativeInputEvent) -> InputOrigin> = OnceLock::new();

thread_local! {
    /// The native input handler's decision, also restored while its callbacks run.
    static SOURCE: Cell<Option<Origin>> = const { Cell::new(None) };
    /// `None` outside a retained window callback, including a native input handler itself.
    static DELIVERY: Cell<Option<InputOrigin>> = const { Cell::new(None) };
}

#[derive(Clone, Copy)]
struct Origin {
    event: Option<NativeInputEvent>,
    origin: InputOrigin,
}

pub(crate) fn set_window_input_classifier(classifier: fn(NativeInputEvent) -> InputOrigin) {
    let _ = CLASSIFIER.set(classifier);
}

pub(crate) fn window_event_origin() -> Option<InputOrigin> {
    DELIVERY.with(Cell::get)
}

/// Restores a thread-local scope even when its handler unwinds; it cannot leave this thread.
#[must_use]
pub(super) struct Scope {
    prior: Option<Origin>,
    prior_delivery: Option<InputOrigin>,
    _thread: PhantomData<Rc<()>>,
}

impl Scope {
    fn enter(value: Origin) -> Self {
        Self {
            prior: SOURCE.with(|current| current.replace(Some(value))),
            prior_delivery: DELIVERY.with(|current| current.replace(None)),
            _thread: PhantomData,
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        SOURCE.with(|current| current.set(self.prior));
        DELIVERY.with(|current| current.set(self.prior_delivery));
    }
}

/// Classify once, before AppKit can generate or defer any callback from this native event. An
/// application that installed no classifier has every event read as a person's.
pub(super) fn source(event: NativeInputEvent) -> Scope {
    source_with(event, |event| {
        CLASSIFIER.get().map_or(InputOrigin::Person, |classify| classify(event))
    })
}

fn source_with(
    event: NativeInputEvent,
    classify: impl FnOnce(NativeInputEvent) -> InputOrigin,
) -> Scope {
    // cancelOperation: can be called from the key's interpretation. Its native identity
    // remains the same, even after a nested handler consumed another key's ledger entry.
    let retained = SOURCE.with(Cell::get).filter(|source| source.event == Some(event));
    Scope::enter(retained.unwrap_or_else(|| Origin { event: Some(event), origin: classify(event) }))
}

/// One callback's already decided source; no later event or classifier lookup can change it.
#[derive(Clone, Copy)]
pub(super) struct Captured(Origin);

pub(super) fn capture() -> Captured {
    Captured(SOURCE.with(Cell::get).unwrap_or(Origin { event: None, origin: InputOrigin::Person }))
}

impl Captured {
    pub(super) fn dispatch<R>(self, handler: impl FnOnce() -> R) -> R {
        struct RestoreDelivery(Option<InputOrigin>);
        impl Drop for RestoreDelivery {
            fn drop(&mut self) {
                DELIVERY.with(|current| current.set(self.0));
            }
        }
        let _source = Scope::enter(self.0);
        let _delivery =
            RestoreDelivery(DELIVERY.with(|current| current.replace(Some(self.0.origin))));
        handler()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERSON: InputOrigin = InputOrigin::Person;
    const POSTED: InputOrigin = InputOrigin::Posted;

    fn key(keycode: u16) -> NativeInputEvent {
        NativeInputEvent {
            kind: 10,
            keycode,
            timestamp: 1_000.0 + f64::from(keycode),
            source_pid: 0,
        }
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
            {
                let _physical = source_with(key(0), |_| PERSON);
                assert_eq!(
                    window_event_origin(),
                    None,
                    "the native handler is not a window callback"
                );
                capture().dispatch(|| assert_eq!(window_event_origin(), Some(PERSON)));
                assert_eq!(window_event_origin(), None);
            }
            assert_eq!(window_event_origin(), Some(POSTED));
        });
        assert_eq!(window_event_origin(), None);
    }
}
