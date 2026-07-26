// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Build-time tier capabilities of this scrollback crate (audit E1).
//!
//! The tiered store is feature-shaped: the default build is the headless
//! hot(RAM)+warm(LZ4) store whose "cold" pages also encode with LZ4 (the wasm
//! renderer's shape), while native builds may opt into the zstd cold codec and
//! the mmap disk spill. Hosts embedding several engine builds (orc: wasm
//! renderer panes + native daemon sessions) need to DISTINGUISH those shapes to
//! size budgets and describe retention honestly — a "same API" surface that
//! silently means lz4-in-RAM on one host and zstd-on-disk on another is how
//! capability drift ships. This module is the single build-truth answer.

/// Codec used for cold-tier pages in THIS build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdTierCodec {
    /// Default build: cold pages fall back to the warm tier's LZ4 codec.
    Lz4,
    /// `zstd` feature: cold pages use the zstd codec (`COLD_ZSTD_LEVEL`).
    Zstd,
}

/// The tier shape this crate was BUILT with (cargo features, not runtime state).
///
/// Values are compile-time constants: capability answers must not depend on
/// which store instance is asked (a per-instance answer would let a
/// misconfigured pane misreport the build).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierCapabilities {
    /// Cold-tier page codec (`Lz4` default, `Zstd` with the `zstd` feature).
    pub cold_codec: ColdTierCodec,
    /// `disk-tier` feature: mmap-backed cold spill is available
    /// ([`DiskBackedScrollback`](crate::DiskBackedScrollback)).
    pub disk_spill: bool,
}

impl TierCapabilities {
    /// Capabilities of the running build. `const` so embedders can brand
    /// protocol/config structs with the answer at compile time.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            cold_codec: if cfg!(feature = "zstd") {
                ColdTierCodec::Zstd
            } else {
                ColdTierCodec::Lz4
            },
            disk_spill: cfg!(feature = "disk-tier"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_track_the_built_features() {
        let caps = TierCapabilities::current();
        assert_eq!(
            caps.cold_codec == ColdTierCodec::Zstd,
            cfg!(feature = "zstd")
        );
        assert_eq!(caps.disk_spill, cfg!(feature = "disk-tier"));
        // disk-tier implies zstd (the on-disk page format is zstd) — the
        // feature graph must keep that edge.
        if caps.disk_spill {
            assert_eq!(caps.cold_codec, ColdTierCodec::Zstd);
        }
    }
}
