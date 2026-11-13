// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::path::Path;

type Hook = Box<dyn FnMut(&Path)>;

thread_local! {
    static STAGING: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Call `then` with the entry's path each time a pass on this thread is about to rename an
/// entry aside, its intent on record.
pub(crate) fn on_staging(then: impl FnMut(&Path) + 'static) {
    STAGING.with(|h| *h.borrow_mut() = Some(Box::new(then)));
}

/// Forget the call of this thread.
pub(crate) fn clear() {
    STAGING.with(|h| *h.borrow_mut() = None);
}

pub(super) fn staging(entry: &Path) {
    let call = STAGING.with(|h| h.borrow_mut().take());
    if let Some(mut call) = call {
        call(entry);
        STAGING.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(call);
            }
        });
    }
}
