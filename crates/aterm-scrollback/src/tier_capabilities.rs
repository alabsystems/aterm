// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The scrollback tiers of every build: hot (RAM), warm (LZ4) and cold (LZ4)
//! pages, all in memory. A host reads them to describe retention (the wasm
//! module's `tier_capabilities_json`).

/// Codec of the cold-tier pages. It has one variant; the enum keeps the wire
/// field `coldCodec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdTierCodec {
    /// Cold pages use the warm tier's size-prepended LZ4 codec.
    Lz4,
}

/// The tier shape of this build: a compile-time constant, so the answer does not
/// depend on which store is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierCapabilities {
    /// Cold-tier page codec.
    pub cold_codec: ColdTierCodec,
    /// Whether cold pages can spill to disk. Always `false` (there is no disk
    /// tier); the field keeps the wire field `diskSpill`.
    pub disk_spill: bool,
}

impl TierCapabilities {
    /// Capabilities of this build; `const` so an embedder can use the answer at
    /// compile time.
    #[must_use]
    #[inline]
    pub const fn current() -> Self {
        Self {
            cold_codec: ColdTierCodec::Lz4,
            disk_spill: false,
        }
    }
}
