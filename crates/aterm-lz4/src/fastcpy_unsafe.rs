// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../LICENSE-MIT for the upstream MIT license.

//! # FastCpy
//!
//! The Rust Compiler calls `memcpy` for slices of unknown length.
//! This crate provides a faster implementation of `memcpy` for slices up to 32bytes (64bytes with `avx`).
//! If you know most of you copy operations are not too big you can use `fastcpy` to speed up your program.
//!
//! `fastcpy` is designed to contain not too much assembly, so the overhead is low.
//!
//! As fall back the standard `memcpy` is called
//!
//! ## Double Copy Trick
//! `fastcpy` employs a double copy trick to copy slices of length 4-32bytes (64bytes with `avx`).
//! E.g. Slice of length 6 can be copied with two uncoditional copy operations.
//!
//! /// [1, 2, 3, 4, 5, 6]
//! /// [1, 2, 3, 4]
//! ///       [3, 4, 5, 6]
//!

#[inline]
pub fn slice_copy(src: *const u8, dst: *mut u8, num_bytes: usize) {
    if num_bytes < 4 {
        short_copy(src, dst, num_bytes);
        return;
    }

    if num_bytes < 8 {
        double_copy_trick::<4>(src, dst, num_bytes);
        return;
    }

    if num_bytes <= 16 {
        double_copy_trick::<8>(src, dst, num_bytes);
        return;
    }

    //if num_bytes <= 32 {
    //double_copy_trick::<16>(src, dst, num_bytes);
    //return;
    //}

    // /// The code will use the vmovdqu instruction to copy 32 bytes at a time.
    //#[cfg(target_feature = "avx")]
    //{
    //if num_bytes <= 64 {
    //double_copy_trick::<32>(src, dst, num_bytes);
    //return;
    //}
    //}

    // For larger sizes we use the default, which calls memcpy
    // memcpy does some virtual memory tricks to copy large chunks of memory.
    //
    // The theory should be that the checks above don't cost much relative to the copy call for
    // larger copies.
    // The bounds checks in `copy_from_slice` are elided.

    //unsafe { core::ptr::copy_nonoverlapping(src, dst, num_bytes) }
    wild_copy_from_src::<16>(src, dst, num_bytes)
}

// Inline never because otherwise we get a call to memcpy -.-
#[inline]
fn wild_copy_from_src<const SIZE: usize>(
    mut source: *const u8,
    mut dst: *mut u8,
    num_bytes: usize,
) {
    // Note: if the compiler auto-vectorizes this it'll hurt performance!
    // It's not the case for 16 bytes stepsize, but for 8 bytes.
    // SAFETY: `wild_copy_from_src` is only reached from `slice_copy` on the
    // `num_bytes > 16` branch with `SIZE == 16`, so `num_bytes - SIZE` cannot
    // underflow. The caller (the lz4 decompressor via `fastcpy_unsafe::slice_copy`)
    // guarantees `source` points to at least `num_bytes` initialized bytes in
    // a single allocation, so `source.add(num_bytes - SIZE)` stays in-bounds
    // (offset <= num_bytes, which is the allocation size).
    let l_last = unsafe { source.add(num_bytes - SIZE) };
    // SAFETY: Mirrors the `source` computation above. The caller guarantees
    // `dst` points to a writable region of at least `num_bytes` bytes in a
    // single allocation, so `dst.add(num_bytes - SIZE)` stays in-bounds.
    let r_last = unsafe { dst.add(num_bytes - SIZE) };
    let num_bytes = (num_bytes / SIZE) * SIZE;

    // SAFETY: After the rounding above, `num_bytes` is the largest multiple
    // of `SIZE` that fits in the original byte count, so `dst.add(num_bytes)`
    // is in-bounds (one-past-the-end at most). Inside the loop each
    // `copy_nonoverlapping(source, dst, SIZE)` reads/writes exactly `SIZE`
    // bytes at offsets 0, SIZE, 2*SIZE, ... which all lie within the
    // `num_bytes`-byte source/destination regions the caller promised. The
    // loop exits as soon as `dst == dst_ptr_end`, so the pointer advances
    // never exceed the allocation. Source and destination are lz4 literal
    // copies from disjoint input/output buffers, so `copy_nonoverlapping`'s
    // non-overlap requirement holds. Reads/writes of `SIZE` `u8` bytes have
    // alignment 1, which any pointer trivially satisfies.
    unsafe {
        let dst_ptr_end = dst.add(num_bytes);
        loop {
            core::ptr::copy_nonoverlapping(source, dst, SIZE);
            source = source.add(SIZE);
            dst = dst.add(SIZE);
            if dst >= dst_ptr_end {
                break;
            }
        }
    }

    // SAFETY: `l_last`/`r_last` were computed above as `src/dst + (num_bytes
    // - SIZE)` and both point to `SIZE` valid bytes at the tail of the
    // caller's source/destination regions. The tail write overlaps the main
    // loop writes in the destination buffer, but source and destination
    // remain non-overlapping (literal copy between disjoint input/output
    // buffers), so `copy_nonoverlapping`'s precondition holds. Alignment is
    // satisfied trivially for byte-wise copies.
    unsafe {
        core::ptr::copy_nonoverlapping(l_last, r_last, SIZE);
    }
}

#[inline]
fn short_copy(src: *const u8, dst: *mut u8, len: usize) {
    // SAFETY: `short_copy` is only reached from `slice_copy` on the
    // `num_bytes < 4` branch, where the caller guarantees `num_bytes >= 1`
    // at this point (`slice_copy`'s callers never pass zero-length copies
    // through `fastcpy`). Thus `src` points to at least one readable byte
    // and `dst` points to at least one writable byte, both within their
    // respective allocations. `u8` has alignment 1, so the single-byte
    // read/write through raw pointers is aligned. Source and destination
    // are disjoint literal buffers in the lz4 decompressor.
    unsafe {
        *dst = *src;
    }
    if len >= 2 {
        double_copy_trick::<2>(src, dst, len);
    }
}

#[inline(always)]
/// [1, 2, 3, 4, 5, 6]
/// [1, 2, 3, 4]
///       [3, 4, 5, 6]
fn double_copy_trick<const SIZE: usize>(src: *const u8, dst: *mut u8, len: usize) {
    // SAFETY: `double_copy_trick` is invoked from `slice_copy`/`short_copy`
    // only when `len >= SIZE` (SIZE is 2, 4, or 8 at the call sites, and each
    // call site gates on `num_bytes >= SIZE` before dispatching here). Thus
    // `len - SIZE` does not underflow. The caller's contract on `slice_copy`
    // guarantees `src` points to at least `len` initialized bytes and `dst`
    // points to at least `len` writable bytes in their respective allocations,
    // so `src.add(len - SIZE)` and `dst.add(len - SIZE)` stay in-bounds.
    let l_end = unsafe { src.add(len - SIZE) };
    // SAFETY: See the comment on `l_end`; the same `len >= SIZE` invariant
    // and destination allocation-size guarantee apply to `dst`.
    let r_end = unsafe { dst.add(len - SIZE) };

    // SAFETY: Both copies move exactly `SIZE` bytes within the `len`-byte
    // source/destination regions promised by the caller, so each access is
    // in-bounds. The two copies overlap within the destination buffer on
    // purpose (the "double copy trick" that fills middle bytes twice), but
    // `copy_nonoverlapping` only forbids src/dst overlap: source and
    // destination are disjoint literal buffers in the lz4 decompressor, so
    // the non-overlap requirement holds for each individual call.
    // Byte-wise copies are aligned for any pointer value.
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, SIZE);
        core::ptr::copy_nonoverlapping(l_end, r_end, SIZE);
    }
}

#[cfg(test)]
mod tests {
    use super::slice_copy;
    use alloc::vec::Vec;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn test_fast_short_slice_copy(left: Vec<u8>) {
            if left.is_empty() {
                return Ok(());
            }
            let mut right = vec![0u8; left.len()];
            slice_copy(left.as_ptr(), right.as_mut_ptr(), left.len());
            prop_assert_eq!(&left, &right);
        }
    }

    #[test]
    fn test_fast_short_slice_copy_edge_cases() {
        for len in 1..(512 * 2) {
            let left = (0..len).map(|i| i as u8).collect::<Vec<_>>();
            let mut right = vec![0u8; len];
            slice_copy(left.as_ptr(), right.as_mut_ptr(), left.len());
            assert_eq!(left, right);
        }
    }

    #[test]
    fn test_fail2() {
        let left = vec![
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31, 32,
        ];
        let mut right = vec![0u8; left.len()];
        slice_copy(left.as_ptr(), right.as_mut_ptr(), left.len());
        assert_eq!(left, right);
    }

    #[test]
    fn test_fail() {
        let left = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut right = vec![0u8; left.len()];
        slice_copy(left.as_ptr(), right.as_mut_ptr(), left.len());
        assert_eq!(left, right);
    }
}
