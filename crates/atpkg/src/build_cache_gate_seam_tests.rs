// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::path::Path;

type Hook = Box<dyn FnMut(&Path)>;

thread_local! {
    static WAITING: RefCell<Option<Hook>> = const { RefCell::new(None) };
    static SETTLING: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

/// Call `hook` at each wait of this thread until the guard returned is dropped.
pub(in super::super) fn on_waiting(hook: impl FnMut(&Path) + 'static) -> Reset {
    WAITING.with(|waiting| *waiting.borrow_mut() = Some(Box::new(hook)));
    Reset
}

/// Call `hook` at each pause of this thread's reader riding out a hold of the whole gate, until
/// the guard returned is dropped.
pub(in super::super) fn on_settling(hook: impl FnMut() + 'static) -> Reset {
    SETTLING.with(|settling| *settling.borrow_mut() = Some(Box::new(hook)));
    Reset
}

/// Forgets this thread's calls as it drops.
pub(in super::super) struct Reset;

impl Drop for Reset {
    fn drop(&mut self) {
        WAITING.with(|waiting| *waiting.borrow_mut() = None);
        SETTLING.with(|settling| *settling.borrow_mut() = None);
    }
}

pub(in super::super) fn waiting(gate: &Path) {
    WAITING.with(|waiting| {
        if let Some(hook) = waiting.borrow_mut().as_mut() {
            hook(gate);
        }
    });
}

pub(in super::super) fn settling() {
    SETTLING.with(|settling| {
        if let Some(hook) = settling.borrow_mut().as_mut() {
            hook();
        }
    });
}
