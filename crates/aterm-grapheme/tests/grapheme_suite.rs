// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The aterm-grapheme integration files as modules of ONE test binary: each
//! file reads the UCD data it checks and nothing process-wide, so they share a
//! process. The files keep their paths, so `tests/<file>.rs` citations resolve.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod emoji_data;
mod uax29_conformance;
