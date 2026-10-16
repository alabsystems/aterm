// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Generated-input tests over aterm-core's crate-private seams, from
//! `aterm_spec::rng::XorShift` at fixed seeds so a failure reproduces from its case index.

#[path = "../../test_support/generated/grapheme.rs"]
mod grapheme;

#[path = "../../test_support/generated/scroll_pin.rs"]
mod scroll_pin;

#[cfg(feature = "sixel")]
#[path = "../../test_support/generated/sixel.rs"]
mod sixel;
