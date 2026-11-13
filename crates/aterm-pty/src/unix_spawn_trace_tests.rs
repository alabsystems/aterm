// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::ffi::CString;

std::thread_local! {
    static FORKED_ONTO: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
}

/// In the parent, before the fork onto the pty of `master`.
pub(super) fn forking_onto(master: libc::c_int) {
    let name = super::pts_name(master).unwrap_or_default();
    FORKED_ONTO.with(|f| f.borrow_mut().push(name));
}

/// The ptys this thread's spawns forked onto since the last take.
pub(crate) fn take() -> Vec<CString> {
    FORKED_ONTO.with(|f| std::mem::take(&mut *f.borrow_mut()))
}
