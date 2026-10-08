// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../LICENSE-MIT for the upstream MIT license.

//! Fast `memcpy` for short slices.
//!
//! The compiler calls `memcpy` for slices of unknown length. Up to 32 bytes
//! (64 on x86_64 or with `avx`) this copies inline with two overlapping
//! fixed-size copies, e.g. 6 bytes as `[1, 2, 3, 4]` then `[3, 4, 5, 6]`.
//! Longer slices fall back to `memcpy`.

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

    // The 33..=64-byte tier. Upstream gates it on `target_feature = "avx"`,
    // which no x86_64 lane of this tree sets (none passes `-C target-cpu` or
    // `target-feature`), so those copies all called `memcpy`. On x86_64 it is
    // taken unconditionally: call-free `movq`/SSE2 `movups` without AVX.
    //
    // Measured on an i7-7920HQ (macOS 13.7, Trust, opt-level 3, 65536 random
    // 33..=64-byte copies, min of 200 passes, 6 runs): memcpy 17.7..18.5
    // ns/op, this tier 12.1..12.7 (-29..-33%); >64-byte copies unchanged
    // within noise. A runtime-dispatched `#[target_feature(enable = "avx2")]`
    // copy measured 13.6..14.4 ns/op and needs `unsafe`. aarch64 keeps
    // upstream's gate. Linux and Windows x86_64 take the tier unmeasured.
    #[cfg(any(target_feature = "avx", target_arch = "x86_64"))]
    {
        if len <= 64 {
            double_copy_trick::<32>(src, dst);
            return;
        }
    }

    // Larger copies: `memcpy`, where the checks above cost little.
    dst.copy_from_slice(src);
}

#[inline(always)]
#[cfg_attr(trust_verify, trust::skip)] // precondition helper (1 <= src.len() <= dst.len()), enforced by slice_copy's contract
fn short_copy(src: &[u8], dst: &mut [u8]) {
    // length 1-3
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
