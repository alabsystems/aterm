// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};
use std::path::Path;

/// What runs with a copy's path before its status.
type Hook = Box<dyn FnMut(&Path)>;

std::thread_local! {
    static BEFORE: RefCell<Option<Hook>> = const { RefCell::new(None) };
    static ANSWERED: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Until the guard it returns drops, `hook` runs with each copy's path before its status.
pub(crate) fn set_before_status(hook: impl FnMut(&Path) + 'static) -> Reset {
    BEFORE.with(|b| *b.borrow_mut() = Some(Box::new(hook)));
    Reset(())
}

/// Whether the last [`super::work_status`] on this thread was answered by a copy (`None`:
/// none has asked since the thread began).
pub(crate) fn copy_answered() -> Option<bool> {
    ANSWERED.with(Cell::get)
}

/// No hook again once dropped.
pub(crate) struct Reset(());

impl Drop for Reset {
    fn drop(&mut self) {
        BEFORE.with(|b| *b.borrow_mut() = None);
    }
}

/// Run this thread's hook, if one is set, with the copy at `path`.
pub(super) fn before_status(path: &Path) {
    let hook = BEFORE.with(|b| b.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook(path);
        BEFORE.with(|b| {
            b.borrow_mut().get_or_insert(hook);
        });
    }
}

/// Note whether a copy answered this [`super::work_status`].
pub(super) fn answered(by_copy: bool) {
    ANSWERED.with(|a| a.set(Some(by_copy)));
}
