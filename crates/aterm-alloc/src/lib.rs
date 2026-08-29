// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inline-optimized collections for aterm.
//!
//! Zero external dependencies. Provides:
//!
//! - [`SmallVec<T, N>`] — inline storage for up to N elements, heap fallback.
//!   Replaces `smallvec::SmallVec<[T; N]>`.
//! - [`ArrayVec<T, N>`] — fixed-capacity inline-only storage, no heap allocation.
//!   Replaces `arrayvec::ArrayVec<T, N>`.
//!
//! ## Capacities used in aterm
//!
//! **SmallVec:** 2 (combining chars, hyperlinks, placements), 4 (deferred cols,
//! combining marks), 8 (extra collection keys), 16 (color palette overrides),
//! 96 (cell vertices).
//!
//! **ArrayVec:** 4 (intermediates), 16 (CSI params), 32 (OSC params).
//!
//! ## `ArrayVec` is also republished as the `arrayvec` package
//!
//! `crates/aterm-arrayvec` is a shim whose `[package] name` is `arrayvec`; it
//! re-exports [`ArrayVec`] and nothing else of substance, so that
//! `[patch.crates-io]` can point `naga` / `wgpu` / `wgpu-core` / `wgpu-hal` /
//! `tiny-skia` / `vte` at this implementation. Anything you change in
//! `array_vec.rs` is therefore load-bearing for third-party GPU code as well as
//! for aterm's parser — read `crates/aterm-arrayvec/src/lib.rs` first.

#![deny(clippy::all)]
#![deny(unsafe_op_in_unsafe_fn)]
// Trust tool-attribute plumbing (scrollback/lz4 pattern) for `#[trust::contract_panic]`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

mod array_vec;
mod small_vec;

// The ArrayVec iterators are re-exported UNDER RENAMED NAMES because
// `SmallVec`'s by-value iterator already owns the flat `IntoIter` spelling in
// this crate's root. `crates/aterm-arrayvec` renames them back to upstream
// `arrayvec`'s `IntoIter` / `Drain` on the way out, which is the only place the
// names have to match somebody else's API.
pub use array_vec::{
    ArrayVec, CapacityError, Drain as ArrayVecDrain, IntoIter as ArrayVecIntoIter,
};
pub use small_vec::{IntoIter, SmallVec};
