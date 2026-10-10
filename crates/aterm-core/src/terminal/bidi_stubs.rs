// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The engine's BiDi invalidation hooks.
//!
//! BiDi reordering runs at snapshot time (`bidi_reorder.rs`, feature `bidi`),
//! so a policy change dirties the whole presented grid. `sync_bidi_from_damage`
//! is where a render-side resolver cache would attach.

use super::Terminal;

#[allow(
    clippy::unused_self,
    reason = "a hook with no state yet keeps the &mut self signature its callers use"
)]
impl Terminal {
    /// Invalidate the render-time BiDi projection of all stored rows.
    pub(crate) fn invalidate_bidi_all(&mut self) {
        self.grid.damage_mut().mark_full();
    }

    /// No-op: no resolver cache to sync.
    pub(super) fn sync_bidi_from_damage(&mut self) {}

    /// Without the `bidi` feature no row is reordered, so a pointer's viewport
    /// cell is its logical cell (the feature's version maps it through the
    /// row's permutation).
    #[cfg(not(feature = "bidi"))]
    #[must_use]
    pub fn viewport_cell_logical(&self, _row: u16, col: u16) -> (u16, bool) {
        (col, false)
    }
}
