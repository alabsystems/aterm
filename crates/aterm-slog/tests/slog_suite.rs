// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-slog integration files as modules of ONE test binary.
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-slog --test slog_suite -- <file>::`.

mod support;

mod brief_budget;
mod conformance_ring;
mod conformance_turn;
mod discipline;
mod fold_laws;
mod line_codec;
mod ring_bounds;
mod turn_replays;
mod vocab_mirror;
