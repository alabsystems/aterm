// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! No-op stubs for the engine's BiDi resolver/sync hooks.
//!
//! BiDi visual reordering itself lives in the `aterm-bidi` crate and is reached
//! through the off-by-default `bidi` feature (see `bidi_reorder.rs`). These
//! `Terminal`-level hooks are where a future stateful render-side resolver cache
//! would attach; until then they are intentionally no-ops, so the default build
//! carries no BiDi resolution state.

use super::Terminal;

#[allow(dead_code, reason = "stub methods for disabled bidi feature")]
#[allow(
    clippy::unused_self,
    reason = "stubs mirror the &mut self signatures of the real bidi methods so call sites are identical whether or not the feature is enabled"
)]
impl Terminal {
    /// No-op: BiDi feature is disabled.
    pub(crate) fn invalidate_bidi_all(&mut self) {}

    /// No-op: BiDi feature is disabled.
    pub(super) fn sync_bidi_resolver_from_config(&mut self) {}

    /// No-op: BiDi feature is disabled.
    pub(super) fn sync_bidi_from_damage(&mut self) {}
}
