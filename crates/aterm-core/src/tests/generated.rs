// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Generated-input tests over aterm-core's crate-private seams: inputs come
//! from `aterm_spec::rng::XorShift` at a fixed seed, so a failure reproduces
//! from its case index.

#[path = "../../test_support/generated/grapheme.rs"]
mod grapheme;

// SCR-1 absolute-content pinning: live output while scrolled back keeps the same
// absolute row at the viewport top (the engine invariant the renderer's
// scroll-restore relies on), over random volumes.
#[path = "../../test_support/generated/scroll_pin.rs"]
mod scroll_pin;

// Sixel decoder crash-safety + image invariants. Gated on the `sixel` feature
// that compiles the decoder.
#[cfg(feature = "sixel")]
#[path = "../../test_support/generated/sixel.rs"]
mod sixel;
