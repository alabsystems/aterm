// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND MIT
//
// This crate is a modified, block-mode-only subset of upstream `lz4_flex`
// (https://github.com/pseitz/lz4_flex). The upstream code is MIT-licensed;
// see LICENSE-MIT in this crate's root for the original notice.
//
// We need LZ4 block compression for warm-tier scrollback storage but not the
// frame format and its additional dependency. The upstream-derived files in
// this crate carry local compatibility, lint, and verification changes.

// Trust tool-attribute registration (scrollback-style): a stable build
// ignores all of this; under Trust it enables the `#[cfg_attr(trust_verify,
// trust::skip)]` opt-outs on the deliberate-panic API-contract items below
// (Sink capacity contracts, fastcpy precondition helpers, allocation panics)
// — the documented strict-by-default escape hatch where the panic IS the
// documented behavior and cannot be a Level-0 theorem.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Pure Rust, high-performance LZ4 **block-format** compression.
//!
//! This is a vendored, block-mode-only subset of the upstream `lz4_flex`
//! crate. Only the LZ4 block format is supported; the frame format (which
//! requires an `xxhash` dependency) is intentionally omitted. The on-wire
//! format is identical to upstream `lz4_flex` block mode, so data compressed
//! with either codebase decompresses with the other.
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
//! Only upstream's safe paths are carried: the safe-only encoder and the
//! bounds-checked decoder (upstream's `safe-encode` / `safe-decode` /
//! `checked-decode` defaults, the only configuration any consumer ever built).
//! The crate forbids `unsafe` code.

#![forbid(unsafe_code)]
// The files under `src/block/`, `src/sink.rs`, and `src/fastcpy.rs`
// derive from upstream `lz4_flex` 0.11.5. Upstream
// does not currently enforce the stricter clippy lints the rest of the aterm
// workspace enables, so we relax them here at the crate boundary. Any
// locally-authored code in this crate (see `lib.rs` and `tests/`) is
// expected to meet workspace defaults; the lint relaxations only affect
// the upstream-derived surface.
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

// Convenience re-exports at the crate root: these match the two entry points
// used by every in-tree consumer of lz4 block mode.
pub use block::{
    DecompressError, compress, compress_prepend_size, decompress, decompress_into,
    decompress_size_prepended, uncompressed_size,
};
