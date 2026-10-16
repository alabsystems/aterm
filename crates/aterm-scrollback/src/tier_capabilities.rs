// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Build-time tier capabilities of this scrollback crate.
//!
//! Every build keeps hot (RAM) + warm (LZ4) + LZ4 cold pages; native builds may
//! add the mmap disk spill. Hosts embedding several engine builds (wasm renderer
//! panes, native daemon sessions) read this to size budgets and describe
//! retention per build.

/// Codec used for cold-tier pages in this build.
///
/// A single codec (docs/AUDIT-third-party-2026-10-15.md, step ZC.2); the enum
/// stays so the wire shape (`coldCodec`) is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdTierCodec {
    /// Cold pages use the warm tier's size-prepended LZ4 codec.
    Lz4,
}

/// The tier shape this crate was built with (cargo features, not runtime state).
///
/// Compile-time constants: the answer must not depend on which store instance
/// is asked, or a misconfigured pane could misreport the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierCapabilities {
    /// Cold-tier page codec.
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
            cold_codec: ColdTierCodec::Lz4,
            disk_spill: cfg!(feature = "disk-tier"),
        }
    }
}
