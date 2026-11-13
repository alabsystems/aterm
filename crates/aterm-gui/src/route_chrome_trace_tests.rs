// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::ChromeStep;
use std::cell::{Cell, RefCell};

thread_local! {
    static COMPOSITIONS: Cell<usize> = const { Cell::new(0) };
    static STEPS: RefCell<Vec<ChromeStep>> = const { RefCell::new(Vec::new()) };
    static MASK: RefCell<Vec<ChromeStep>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn composition() {
    COMPOSITIONS.with(|c| c.set(c.get() + 1));
}

/// Whether `step` runs; a step that runs is recorded.
pub(super) fn admit(step: ChromeStep) -> bool {
    if MASK.with(|mask| mask.borrow().contains(&step)) {
        return false;
    }
    STEPS.with(|steps| steps.borrow_mut().push(step));
    true
}

/// The compositions and steps recorded since the last take, which resets
/// both.
pub(crate) fn take() -> (usize, Vec<ChromeStep>) {
    (
        COMPOSITIONS.with(|c| c.replace(0)),
        STEPS.with(|steps| std::mem::take(&mut *steps.borrow_mut())),
    )
}

/// Skip `steps` in every composition on this thread until the guard
/// drops.
#[must_use]
pub(crate) fn mask(steps: &[ChromeStep]) -> MaskGuard {
    MASK.with(|mask| *mask.borrow_mut() = steps.to_vec());
    MaskGuard
}

pub(crate) struct MaskGuard;

impl Drop for MaskGuard {
    fn drop(&mut self) {
        MASK.with(|mask| mask.borrow_mut().clear());
    }
}
