// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND MIT
// Derived from upstream `lz4_flex` (https://github.com/pseitz/lz4_flex, MIT; see LICENSE-MIT).

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Pure Rust, high-performance LZ4 **block-format** compression.

#![forbid(unsafe_code)]
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
