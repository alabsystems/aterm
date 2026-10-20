// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-zstd integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s. The shared input
//! generators are `support`, compiled once.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod support;

mod cli_decode;
mod cli_frames;
mod decode_frames;
mod sweep;
mod xxh64_recorded;
