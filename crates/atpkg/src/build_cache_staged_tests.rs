// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::path::Path;

type Hook = Box<dyn FnMut(&Path)>;

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Call `then` with each stage just filled on this thread, until [`clear`].
pub(crate) fn on(then: impl FnMut(&Path) + 'static) {
    HOOK.with(|hook| *hook.borrow_mut() = Some(Box::new(then)));
}

/// No hook on this thread.
pub(crate) fn clear() {
    HOOK.with(|hook| *hook.borrow_mut() = None);
}

pub(super) fn at(stage: &Path) {
    HOOK.with(|hook| {
        if let Some(then) = hook.borrow_mut().as_mut() {
            then(stage);
        }
    });
}
