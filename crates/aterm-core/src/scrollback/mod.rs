// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Re-exports from the `aterm-scrollback` crate as `crate::scrollback::*`.

pub use aterm_scrollback::{
    CellAttrs, ColdTierCodec, HyperlinkSpan, Line, Rle, Scrollback, ScrollbackIter,
    ScrollbackStorage, TierCapabilities, WatermarkLevel,
};

pub use aterm_scrollback::DEFAULT_LINE_LIMIT;

pub use aterm_scrollback::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_strict_dropping_over_cap_links,
    deserialize_lines_tail_strict, max_linked_record_bytes, serialize_lines,
    serialize_lines_bounded,
};
