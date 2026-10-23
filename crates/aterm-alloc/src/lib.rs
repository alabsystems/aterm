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

/// Return `true` when `len` fits within an inline capacity of `inline_cap`.
#[inline]
#[must_use]
pub const fn fits_inline(len: usize, inline_cap: usize) -> bool {
    len <= inline_cap
}

/// Compute the next heap capacity for `current_cap` to hold at least `min_needed` elements,
/// doubling `current_cap` (with a minimum of 4) and saturating on overflow.
#[inline]
#[must_use]
pub const fn next_capacity_for(current_cap: usize, min_needed: usize) -> usize {
    let doubled = current_cap.saturating_mul(2);
    let base = if doubled < 4 { 4 } else { doubled };
    if base < min_needed { min_needed } else { base }
}

/// Encode collection allocation counts into 16 LE bytes.
#[inline]
#[must_use]
pub const fn encode_alloc_counts_le(
    len: u32,
    inline_cap: u32,
    heap_cap: u32,
    elem_size: u32,
) -> [u8; 16] {
    [
        len as u8,
        (len >> 8) as u8,
        (len >> 16) as u8,
        (len >> 24) as u8,
        inline_cap as u8,
        (inline_cap >> 8) as u8,
        (inline_cap >> 16) as u8,
        (inline_cap >> 24) as u8,
        heap_cap as u8,
        (heap_cap >> 8) as u8,
        (heap_cap >> 16) as u8,
        (heap_cap >> 24) as u8,
        elem_size as u8,
        (elem_size >> 8) as u8,
        (elem_size >> 16) as u8,
        (elem_size >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into collection allocation counts.
#[inline]
#[must_use]
pub const fn decode_alloc_counts_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let len = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let inline_cap = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let heap_cap = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let elem_size = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (len, inline_cap, heap_cap, elem_size)
}

#[cfg(test)]
mod const_tests {
    use super::*;

    #[test]
    fn alloc_const_helpers_round_trip() {
        assert!(fits_inline(8, 8));
        assert!(!fits_inline(9, 8));
        assert_eq!(next_capacity_for(0, 1), 4);
        assert_eq!(next_capacity_for(8, 10), 16);
        assert_eq!(next_capacity_for(8, 24), 24);
        let enc = encode_alloc_counts_le(5, 8, 16, 4);
        assert_eq!(decode_alloc_counts_le(enc), (5, 8, 16, 4));
    }
}
