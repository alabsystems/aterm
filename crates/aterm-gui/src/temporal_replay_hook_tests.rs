// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

type Hook = Box<dyn FnMut()>;

thread_local! {
    static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
}

/// Clears the installed hook when dropped.
pub(crate) struct Installed;

impl Drop for Installed {
    fn drop(&mut self) {
        HOOK.with(|h| *h.borrow_mut() = None);
    }
}

/// Run `hook` at the start of every later replay on this thread, until the returned guard drops.
pub(crate) fn install(hook: impl FnMut() + 'static) -> Installed {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    Installed
}

pub(super) fn before_replay() {
    HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook();
        }
    });
}
