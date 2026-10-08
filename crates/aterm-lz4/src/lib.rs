// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND MIT
//
// A modified, block-mode-only subset of upstream `lz4_flex`
// (https://github.com/pseitz/lz4_flex), MIT-licensed; see LICENSE-MIT.
// Warm-tier scrollback needs LZ4 blocks, not the frame format.

// Registers the `trust` tool so `#[cfg_attr(trust_verify, trust::skip)]` can
// mark items whose panic IS the documented contract (Sink capacity, fastcpy
// preconditions, allocation). A stable build ignores this.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Pure Rust, high-performance LZ4 **block-format** compression.
//!
//! Block format only (no frame format, so no `xxhash`). The wire format is
//! upstream `lz4_flex` block mode's: either side decompresses the other's data.
//!
//! ```
//! use aterm_lz4::block::{compress_prepend_size, decompress_size_prepended};
//! let input: &[u8] = b"Hello people, what's up?";
//! let compressed = compress_prepend_size(input);
//! let uncompressed = decompress_size_prepended(&compressed).unwrap();
//! assert_eq!(input, uncompressed);
//! ```
//!
//! # Safety
//!
//! Only upstream's safe encoder and bounds-checked decoder are carried
//! (`safe-encode` / `safe-decode` / `checked-decode`). The crate forbids
//! `unsafe` code.

#![forbid(unsafe_code)]
// `src/block/`, `src/sink.rs` and `src/fastcpy.rs` derive from upstream
// `lz4_flex` 0.11.5, which does not meet the workspace's stricter clippy
// lints; these relaxations exist for that code only.
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_range_contains)]
#![allow(clippy::needless_lifetimes)]
#![allow(clippy::len_zero)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::useless_conversion)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::identity_op)]

extern crate alloc;

pub mod block;

mod fastcpy;

pub(crate) mod sink;

pub use block::{
    DecompressError, compress, compress_prepend_size, decompress, decompress_into,
    decompress_size_prepended, uncompressed_size,
};
