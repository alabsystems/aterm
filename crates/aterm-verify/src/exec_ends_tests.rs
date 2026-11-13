// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The end's flag, and the flag set once a look of this thread's clock finds a wait.
type Flags = (Arc<AtomicBool>, Arc<AtomicBool>);

thread_local! {
    static END: std::cell::RefCell<Option<Flags>> = const { std::cell::RefCell::new(None) };
}

/// End this thread's run once `flag` is set, setting `seen` once its clock finds a wait.
pub(crate) fn when(flag: Arc<AtomicBool>, seen: Arc<AtomicBool>) {
    END.with(|e| *e.borrow_mut() = Some((flag, seen)));
}

pub(super) fn now() -> bool {
    END.with(|e| {
        e.borrow()
            .as_ref()
            .is_some_and(|(f, _)| f.load(Ordering::SeqCst))
    })
}

/// A look of this thread's clock found a wait.
pub(super) fn saw() {
    END.with(|e| {
        if let Some((_, seen)) = e.borrow().as_ref() {
            seen.store(true, Ordering::SeqCst);
        }
    });
}
