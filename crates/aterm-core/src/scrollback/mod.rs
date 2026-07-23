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
    CellAttrs, HyperlinkSpan, Line, Rle, Scrollback, ScrollbackIter, ScrollbackRevIter,
    ScrollbackStorage, WatermarkLevel,
};
// Disk cold-tier types are disk-tier-gated in aterm-scrollback (mmap + zstd-sys);
// dropped on wasm.
#[cfg(feature = "disk-tier")]
pub use aterm_scrollback::{DiskBackedScrollback, DiskBackedScrollbackConfig};

// Block codec, public for `TerminalCheckpoint` grid-body encode/decode (B.3.2).
pub use aterm_scrollback::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_tail_strict, serialize_lines,
};
