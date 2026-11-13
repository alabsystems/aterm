// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

type Hook = Box<dyn FnMut()>;

thread_local! {
    static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
}

/// RAII guard that clears the serialization test hook on drop.
pub(super) struct Installed;

impl Drop for Installed {
    fn drop(&mut self) {
        HOOK.with(|h| *h.borrow_mut() = None);
    }
}

/// Installs `hook` to run at the start of each serialization on this thread.
pub(super) fn install(hook: impl FnMut() + 'static) -> Installed {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    Installed
}

pub(super) fn before_serialize() {
    HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook();
        }
    });
}
