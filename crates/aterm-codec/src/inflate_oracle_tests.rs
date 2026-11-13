// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::Cell;

thread_local! {
    static FORCE_BITWISE: Cell<bool> = const { Cell::new(false) };
    static TABLES_BUILT: Cell<u64> = const { Cell::new(0) };
}

pub(super) fn force_bitwise() -> bool {
    FORCE_BITWISE.with(Cell::get)
}

/// Count one [`super::Huffman::new`] on this thread.
pub(super) fn note_table_built() {
    TABLES_BUILT.with(|c| c.set(c.get().saturating_add(1)));
}

/// How many Huffman tables this thread has built.
pub(super) fn tables_built() -> u64 {
    TABLES_BUILT.with(Cell::get)
}

/// Run `f` with the root table disabled.
pub(super) fn without_root_table<T>(f: impl FnOnce() -> T) -> T {
    FORCE_BITWISE.with(|c| c.set(true));
    let out = f();
    FORCE_BITWISE.with(|c| c.set(false));
    out
}
