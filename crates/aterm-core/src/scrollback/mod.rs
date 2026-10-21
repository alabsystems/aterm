// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Re-exports from the `aterm-scrollback` crate as `crate::scrollback::*`.

// Explicit list (no wildcard) so new public symbols in aterm-scrollback do not
// join aterm_core::scrollback::* without review.
pub use aterm_scrollback::{
    CellAttrs, ColdTierCodec, HyperlinkSpan, Line, Rle, Scrollback, ScrollbackIter,
    ScrollbackStorage, TierCapabilities, WatermarkLevel,
};
// The construction-default total retention cap, for embedders that report
// budgets and limits (wasm exports, the daemon builder).
pub use aterm_scrollback::DEFAULT_LINE_LIMIT;

// Block codec, public for `TerminalCheckpoint` grid-body encode/decode.
pub use aterm_scrollback::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_strict_dropping_over_cap_links,
    deserialize_lines_tail_strict, max_linked_record_bytes, serialize_lines,
    serialize_lines_bounded,
};
