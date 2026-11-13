// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::path::Path;

type Hook = Box<dyn FnMut(&Path)>;

thread_local! {
    static RECHECK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Call `then` with the link's path at each re-check on this thread.
pub(super) fn on_recheck(then: impl FnMut(&Path) + 'static) {
    RECHECK.with(|slot| *slot.borrow_mut() = Some(Box::new(then)));
}

/// Forget this thread's call.
pub(super) fn clear() {
    RECHECK.with(|slot| *slot.borrow_mut() = None);
}

pub(super) fn rechecking(link: &Path) {
    let call = RECHECK.with(|slot| slot.borrow_mut().take());
    if let Some(mut call) = call {
        call(link);
        RECHECK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(call);
            }
        });
    }
}
