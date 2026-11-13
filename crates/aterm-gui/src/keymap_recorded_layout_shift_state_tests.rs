// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

/// A layout's `VkKeyScanExW` shift-state answer for a character.
pub(crate) type ShiftState = fn(char) -> Option<u8>;

thread_local! {
    static LAYOUT: Cell<Option<ShiftState>> = const { Cell::new(None) };
}

/// Answer `VkKeyScanExW`'s shift state from `layout` on this thread (`None`: from Windows).
pub(crate) fn set(layout: Option<ShiftState>) {
    LAYOUT.with(|cell| cell.set(layout));
}

pub(super) fn get() -> Option<ShiftState> {
    LAYOUT.with(Cell::get)
}
