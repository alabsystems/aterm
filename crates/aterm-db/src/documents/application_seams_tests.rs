// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

thread_local! {
    /// Run once a read on this thread has opened the selected home's custody, before it
    /// observes the home's predecessors.
    static OPENED: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    /// Run once a read on this thread has selected its generation, before its session opens.
    static SELECTED: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

/// Run `hook` once this thread's next read through [`super::observed_session`] has opened the
/// selected home's custody, before it observes that home's predecessors.
pub(crate) fn after_custody(hook: impl FnMut() + 'static) {
    OPENED.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

pub(super) fn opened() {
    let hook = OPENED.with(|slot| slot.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook();
    }
}

/// Run `hook` once this thread's next read through [`super::observed_session`] has selected
/// its generation, before it opens the selected home's session.
pub(crate) fn after_selection(hook: impl FnMut() + 'static) {
    SELECTED.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

pub(super) fn selected() {
    let hook = SELECTED.with(|slot| slot.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook();
    }
}
