// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

type Hook = Box<dyn FnMut()>;

thread_local! {
    static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
}

/// Run `hook` after every later terminal hold on this thread.
pub(crate) fn install(hook: impl FnMut() + 'static) {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

pub(crate) fn clear() {
    HOOK.with(|h| *h.borrow_mut() = None);
}

pub(crate) fn after_terminal_hold() {
    HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook();
        }
    });
}
