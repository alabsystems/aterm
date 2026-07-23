// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Property-based tests for aterm-core.
//!
//! Physical files relocated to test_support/proptest/ (Part of #6814).
//! These tests still exercise crate-private seams and remain owned by aterm-core.

#[path = "../../../test_support/proptest/scrollback.rs"]
mod scrollback;

// SCR-1 absolute-content pinning: live output while scrolled back keeps the same
// absolute row at the viewport top (the engine invariant the renderer's scroll-restore
// relies on). Generalizes the single processing.rs unit case over random volumes.
#[path = "../../../test_support/proptest/scroll_pin.rs"]
mod scroll_pin;

// Sixel decoder crash-safety + image-invariant proptests. Gated on the same
// off-by-default `sixel` feature that compiles the decoder: with the feature
// off there is no `crate::sixel` to exercise, so the module stays compiled out
// (mirroring the consume-only build). Run with `--features sixel`.
#[cfg(feature = "sixel")]
#[path = "../../../test_support/proptest/sixel.rs"]
mod sixel;
