// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../LICENSE-MIT for the upstream MIT license.

//! Fast `memcpy` for short slices using overlapping fixed-size copies.

#[inline]
#[cfg_attr(trust_verify, trust::skip)] // len-contract copy family (see len_mismatch_fail); native lowering gap: typed-TrustIr does not complete on this body
pub fn slice_copy(src: &[u8], dst: &mut [u8]) {
    #[inline(never)]
    #[cold]
    #[track_caller]
    #[cfg_attr(trust_verify, trust::skip)] // deliberate contract-violation panic (the assert slice_copy documents)
    fn len_mismatch_fail(dst_len: usize, src_len: usize) -> ! {
        panic!(
            "source slice length ({}) does not match destination slice length ({})",
            src_len, dst_len,
        );
    }

    if src.len() != dst.len() {
        len_mismatch_fail(dst.len(), src.len());
    }
    let len = src.len();

    if src.is_empty() {
        return;
    }

    if len < 4 {
        short_copy(src, dst);
        return;
    }

    if len < 8 {
        double_copy_trick::<4>(src, dst);
        return;
    }

    if len <= 16 {
        double_copy_trick::<8>(src, dst);
        return;
    }

    if len <= 32 {
        double_copy_trick::<16>(src, dst);
        return;
    }

    #[cfg(any(target_feature = "avx", target_arch = "x86_64"))]
    {
        if len <= 64 {
            double_copy_trick::<32>(src, dst);
            return;
        }
    }

    dst.copy_from_slice(src);
}

#[inline(always)]
#[cfg_attr(trust_verify, trust::skip)] // precondition helper (1 <= src.len() <= dst.len()), enforced by slice_copy's contract
fn short_copy(src: &[u8], dst: &mut [u8]) {
    dst[0] = src[0];
    if src.len() >= 2 {
        double_copy_trick::<2>(src, dst);
    }
}

/// Copies the first and last `SIZE` bytes; the two copies overlap to cover
/// any length in `SIZE..=2 * SIZE`.
#[inline(always)]
#[cfg_attr(trust_verify, trust::skip)] // precondition helper (SIZE <= src.len() <= dst.len()), enforced by slice_copy's contract
fn double_copy_trick<const SIZE: usize>(src: &[u8], dst: &mut [u8]) {
    dst[0..SIZE].copy_from_slice(&src[0..SIZE]);
    dst[src.len() - SIZE..].copy_from_slice(&src[src.len() - SIZE..]);
}

#[cfg(test)]
mod tests {
    use super::slice_copy;
    use alloc::vec::Vec;

    #[test]
    fn test_fast_short_slice_copy_edge_cases() {
        for len in 0..(512 * 2) {
            let left = (0..len).map(|i| i as u8).collect::<Vec<_>>();
            let mut right = vec![0u8; len];
            slice_copy(&left, &mut right);
            assert_eq!(left, right);
        }
    }
}
