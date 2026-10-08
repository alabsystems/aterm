// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-search integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s. `index_release_memory` is
//! its own `[[test]]`: it installs a counting `#[global_allocator]` and asserts
//! the net bytes a drop returns, which any sibling test would pollute.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod conformance_streaming;
mod search_vs_bruteforce;
