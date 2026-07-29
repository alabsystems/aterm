// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! No-op stubs for the engine's BiDi resolver/sync hooks.
//!
//! BiDi visual reordering itself lives in the `aterm-bidi` crate and is reached
//! through the off-by-default `bidi` feature (see `bidi_reorder.rs`). These
//! `Terminal`-level hooks are where a future stateful render-side resolver cache
//! would attach. Today invalidation still dirties the presented grid because
//! BiDi policy changes reorder already-stored rows at snapshot time.

use super::Terminal;

#[allow(dead_code, reason = "stub methods for disabled bidi feature")]
impl Terminal {
    /// Invalidate the render-time BiDi projection of all stored rows.
    pub(crate) fn invalidate_bidi_all(&mut self) {
        self.grid.damage_mut().mark_full();
    }

    /// No-op: BiDi feature is disabled.
    pub(super) fn sync_bidi_resolver_from_config(&mut self) {}

    /// No-op: BiDi feature is disabled.
    pub(super) fn sync_bidi_from_damage(&mut self) {}
}
