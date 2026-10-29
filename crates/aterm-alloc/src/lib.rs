// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inline-optimized collections for aterm.

#![no_std]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod array_vec;
mod small_vec;

pub use array_vec::{
    ArrayVec, CapacityError, Drain as ArrayVecDrain, IntoIter as ArrayVecIntoIter,
};
pub use small_vec::{IntoIter, SmallVec};

use core::mem::MaybeUninit;

/// Drop every value in `slots`, in order, with slice drop glue: if one
/// destructor panics, the rest are still dropped before the panic carries on
/// (a second panic aborts, as for any slice). Every bulk drop in this crate —
/// `clear`, `truncate`, the vectors' `Drop`, a by-value iterator's `Drop` —
/// goes through here, matching std `Vec` and upstream `arrayvec`. The caller
/// lowers its length (or cursor) first, so the unwind cannot reach these slots
/// again.
/// # Safety
///
/// Every slot in `slots` holds an initialized value that nothing reads or
/// drops afterwards.
// Skip: the slot-to-`T` reinterpretation is the crate's init-tracking class
// (`ArrayVec::pop`'s); the precondition above is the audited invariant.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn drop_initialized<T>(slots: &mut [MaybeUninit<T>]) {
    let values = core::ptr::slice_from_raw_parts_mut(slots.as_mut_ptr().cast::<T>(), slots.len());
    // SAFETY: `MaybeUninit<T>` has `T`'s layout, the pointer and length come from the same live
    // subslice, and the caller guarantees each slot is initialized and dropped nowhere else.
    unsafe { core::ptr::drop_in_place(values) }
}
