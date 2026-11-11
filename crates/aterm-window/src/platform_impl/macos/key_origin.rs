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
#[path = "key_origin_tests.rs"]
mod tests;
