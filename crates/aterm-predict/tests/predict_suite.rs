// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-predict integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s. None of them touches
//! the process environment or a global.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! THE WORLD (`common/world.rs`) is mounted here, once, because two files run
//! it: the safety theorem (`reconciled_view_safety`) and the measurement of
//! what predictive editing buys (`editing_win`). Mounted inside either, the
//! other could not reach it; mounted inside both, it would compile twice.

#[path = "common/world.rs"]
mod world;

mod conformance_visibility;
mod editing_win;
mod reconciled_view_safety;
