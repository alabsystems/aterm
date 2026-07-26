// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compatibility shim — re-exports from the `aterm-scrollback` crate.
//!
//! Domain logic now lives in `crates/aterm-scrollback/`. This module provides
//! backward-compatible `crate::scrollback::*` paths for the rest of aterm-core.
//!
//! Extracted as part of the monolith split (#2341).

// Explicit re-exports from aterm-scrollback (#2753).
// Keep this list explicit (no wildcard) so new public symbols in aterm-scrollback
// do not become part of aterm_core::scrollback::* without review.
pub use aterm_scrollback::{
    CellAttrs, ColdTierCodec, HyperlinkSpan, Line, Rle, Scrollback, ScrollbackIter,
    ScrollbackRevIter, ScrollbackStorage, TierCapabilities, WatermarkLevel,
};
// The construction-default TOTAL retention cap (audit E1): embedders that brand
// budgets/limits (wasm exports, the daemon builder) need the same number the
// tiered defaults were built from.
pub use aterm_scrollback::DEFAULT_LINE_LIMIT;
// Disk cold-tier types are disk-tier-gated in aterm-scrollback (mmap + zstd-sys);
// dropped on wasm.
#[cfg(feature = "disk-tier")]
pub use aterm_scrollback::{DiskBackedScrollback, DiskBackedScrollbackConfig};

// Block codec, public for `TerminalCheckpoint` grid-body encode/decode (B.3.2).
pub use aterm_scrollback::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_tail_strict, serialize_lines,
};
