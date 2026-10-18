// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inline-optimized collections for aterm.
//!
//! Zero external dependencies, and `#![no_std]` (it needs only `core` and
//! `alloc`). Provides:
//!
//! - [`SmallVec<T, N>`] — inline storage for up to N elements, heap fallback.
//!   Replaces `smallvec::SmallVec<[T; N]>`.
//! - [`ArrayVec<T, N>`] — fixed-capacity inline-only storage, no heap allocation.
//!   Replaces `arrayvec::ArrayVec<T, N>`.
//!
//! ## `ArrayVec` is also republished as the `arrayvec` package
//!
//! `crates/aterm-arrayvec` is a shim whose `[package] name` is `arrayvec`; it
//! re-exports [`ArrayVec`] and nothing else of substance, so that
//! `[patch.crates-io]` can point `naga` / `wgpu` / `wgpu-core` / `wgpu-hal`
//! (and, until LW.5, `tiny-skia`) at this implementation. Anything you change in
//! `array_vec.rs` is therefore load-bearing for third-party GPU code as well as
//! for aterm's parser — read `crates/aterm-arrayvec/src/lib.rs` first.
//!
//! ## `SmallVec` is also the storage of the `smallvec` package
//!
//! `crates/aterm-smallvec` (package `smallvec`) wraps [`SmallVec<T, N>`] as
//! upstream's `SmallVec<[T; N]>` for parking_lot_core and the wgpu crates
//! (wayland-backend too, until linux-windowing LW.7). It adds no unsafe code:
//! every `MaybeUninit`
//! those consumers reach is in `small_vec.rs`, so a change there is checked by
//! that crate's tests too (`targo --unverified test -p smallvec`).

// `no_std` + `alloc`: `ArrayVec` names only `core`, and `SmallVec`'s heap
// fallback needs only `alloc::vec::Vec`. That keeps the `arrayvec` shim's
// `#![no_std]` true of its whole graph, not just of the shim. Tests link `std`
// for the harness and name what they use from it explicitly.
#![no_std]
// Trust tool-attribute plumbing (scrollback/lz4 pattern) for `#[trust::contract_panic]`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod array_vec;
mod small_vec;

// The ArrayVec iterators are re-exported under renamed names because
// `SmallVec`'s `IntoIter` owns the flat spelling here; `crates/aterm-arrayvec`
// renames them back to upstream's `IntoIter` / `Drain`.
pub use array_vec::{
    ArrayVec, CapacityError, Drain as ArrayVecDrain, IntoIter as ArrayVecIntoIter,
};
pub use small_vec::{IntoIter, SmallVec};

use core::mem::MaybeUninit;

/// Drop every value in `slots`, in order, with slice drop glue: if one
/// destructor panics, the rest are still dropped before the panic carries on
/// (a second panic aborts, as for any slice). Every bulk drop in this crate —
/// `clear`, `truncate`, the vectors' `Drop`, a by-value iterator's `Drop` —
/// goes through here, which is how std `Vec` and upstream `arrayvec` drop a
/// cut range (ruling 2026-10-10: a slot-by-slot loop stopped at the first
/// panic and leaked every later element). The caller lowers its length (or
/// cursor) first, so the unwind cannot reach these slots again.
///
/// # Safety
///
/// Every slot in `slots` holds an initialized value that nothing reads or
/// drops afterwards.
// Skip: the slot-to-`T` reinterpretation is the crate's init-tracking class
// (`ArrayVec::pop`'s); the precondition above is the audited invariant.
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn drop_initialized<T>(slots: &mut [MaybeUninit<T>]) {
    let values = core::ptr::slice_from_raw_parts_mut(slots.as_mut_ptr().cast::<T>(), slots.len());
    // SAFETY: `MaybeUninit<T>` has `T`'s layout, the pointer and length come
    // from the same live subslice, and the caller guarantees each slot is
    // initialized and dropped nowhere else.
    unsafe { core::ptr::drop_in_place(values) }
}
