// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

thread_local! { static ARMED: Cell<bool> = const { Cell::new(false) }; }

pub(crate) struct Scope {
    previous: bool,
    _same_thread: PhantomData<Rc<()>>,
}
impl Scope {
    pub(crate) fn arm() -> Self {
        Self {
            previous: ARMED.with(|armed| armed.replace(true)),
            _same_thread: PhantomData,
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.set(self.previous));
    }
}
pub(super) fn armed() -> bool {
    ARMED.with(Cell::get)
}
