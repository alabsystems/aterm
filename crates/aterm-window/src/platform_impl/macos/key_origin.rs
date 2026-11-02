// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Retain the application's source-key classification across deferred window callbacks.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::OnceLock;

use crate::platform::macos::NativeKeyEvent;

static CLASSIFIER: OnceLock<fn(NativeKeyEvent) -> bool> = OnceLock::new();

thread_local! {
    /// The native key handler's decision, also restored while its callbacks run.
    static SOURCE: Cell<Option<Origin>> = const { Cell::new(None) };
    /// `None` outside a retained window callback, including a native key handler itself.
    static DELIVERY: Cell<Option<bool>> = const { Cell::new(None) };
}

#[derive(Clone, Copy)]
struct Origin {
    key: Option<NativeKeyEvent>,
    posted: bool,
}

pub(crate) fn set_window_key_classifier(classifier: fn(NativeKeyEvent) -> bool) {
    let _ = CLASSIFIER.set(classifier);
}

pub(crate) fn window_event_posted() -> Option<bool> {
    DELIVERY.with(Cell::get)
}

/// Restores a thread-local scope even when its handler unwinds; it cannot leave this thread.
#[must_use]
pub(super) struct Scope {
    prior: Option<Origin>,
    prior_delivery: Option<bool>,
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

/// Classify once, before AppKit can generate or defer any callback from this native key.
pub(super) fn source(key: NativeKeyEvent) -> Scope {
    source_with(key, |key| CLASSIFIER.get().is_some_and(|classify| classify(key)))
}

fn source_with(key: NativeKeyEvent, classify: impl FnOnce(NativeKeyEvent) -> bool) -> Scope {
    // cancelOperation: can be called from the key's interpretation. Its native identity
    // remains the same, even after a nested handler consumed another key's ledger entry.
    let retained = SOURCE.with(Cell::get).filter(|source| source.key == Some(key));
    Scope::enter(retained.unwrap_or_else(|| Origin { key: Some(key), posted: classify(key) }))
}

/// One callback's already decided source; no later key or classifier lookup can change it.
#[derive(Clone, Copy)]
pub(super) struct Captured(Origin);

pub(super) fn capture() -> Captured {
    Captured(SOURCE.with(Cell::get).unwrap_or(Origin { key: None, posted: false }))
}

impl Captured {
    pub(super) fn dispatch<R>(self, handler: impl FnOnce() -> R) -> R {
        struct RestoreDelivery(Option<bool>);
        impl Drop for RestoreDelivery {
            fn drop(&mut self) {
                DELIVERY.with(|current| current.set(self.0));
            }
        }
        let _source = Scope::enter(self.0);
        let _delivery =
            RestoreDelivery(DELIVERY.with(|current| current.replace(Some(self.0.posted))));
        handler()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(keycode: u16) -> NativeKeyEvent {
        NativeKeyEvent { kind: 10, keycode, timestamp: 1_000.0 + f64::from(keycode) }
    }

    #[test]
    fn queued_callbacks_keep_their_one_source_classification() {
        let calls = Cell::new(0);
        let decision = Cell::new(true);
        let pending = {
            let _source = source_with(key(14), |seen| {
                assert_eq!(seen, key(14));
                calls.set(calls.get() + 1);
                decision.get()
            });
            [capture(), capture()]
        };
        assert_eq!(calls.get(), 1);
        decision.set(false);
        let _later = source_with(key(0), |_| false);
        for callback in pending {
            callback.dispatch(|| {
                assert_eq!(window_event_posted(), Some(true));
                let _same = source_with(key(14), |_| {
                    panic!("the retained native key cannot be classified again")
                });
                capture().dispatch(|| assert_eq!(window_event_posted(), Some(true)));
            });
        }
        assert_eq!(calls.get(), 1, "delivery never consults the classifier again");
        assert_eq!(window_event_posted(), None);
        capture().dispatch(|| assert_eq!(window_event_posted(), Some(false)));
    }

    #[test]
    fn a_physical_callback_does_not_borrow_a_later_posted_keys_mark() {
        let physical = {
            let _source = source_with(key(0), |_| false);
            capture()
        };
        let _later = source_with(key(14), |_| true);
        physical.dispatch(|| assert_eq!(window_event_posted(), Some(false)));
        capture().dispatch(|| assert_eq!(window_event_posted(), Some(true)));
        assert_eq!(window_event_posted(), None);
    }

    #[test]
    fn the_same_native_key_reuses_its_decision_after_a_different_nested_key() {
        let calls = Cell::new(0);
        let _first = source_with(key(14), |_| {
            calls.set(calls.get() + 1);
            true
        });
        {
            let _physical = source_with(key(0), |_| {
                calls.set(calls.get() + 1);
                false
            });
            capture().dispatch(|| assert_eq!(window_event_posted(), Some(false)));
        }
        let _same = source_with(key(14), |_| panic!("the enclosing key was already classified"));
        capture().dispatch(|| assert_eq!(window_event_posted(), Some(true)));
        assert_eq!(calls.get(), 2, "only the two distinct native keys were classified");
    }

    #[test]
    fn keyless_nested_callbacks_mask_and_restore_both_scopes() {
        let keyless = capture();
        let posted = {
            let _source = source_with(key(14), |_| true);
            capture()
        };
        posted.dispatch(|| {
            assert_eq!(window_event_posted(), Some(true));
            keyless.dispatch(|| {
                assert_eq!(window_event_posted(), Some(false));
                capture().dispatch(|| assert_eq!(window_event_posted(), Some(false)));
            });
            assert_eq!(window_event_posted(), Some(true));
            capture().dispatch(|| assert_eq!(window_event_posted(), Some(true)));
        });
        assert_eq!(window_event_posted(), None);
        capture().dispatch(|| assert_eq!(window_event_posted(), Some(false)));
    }

    #[test]
    fn a_nested_native_key_does_not_inherit_the_outer_callback_verdict() {
        let posted = {
            let _source = source_with(key(14), |_| true);
            capture()
        };
        posted.dispatch(|| {
            assert_eq!(window_event_posted(), Some(true));
            {
                let _physical = source_with(key(0), |_| false);
                assert_eq!(
                    window_event_posted(),
                    None,
                    "the native handler is not a window callback"
                );
                capture().dispatch(|| assert_eq!(window_event_posted(), Some(false)));
                assert_eq!(window_event_posted(), None);
            }
            assert_eq!(window_event_posted(), Some(true));
        });
        assert_eq!(window_event_posted(), None);
    }
}
