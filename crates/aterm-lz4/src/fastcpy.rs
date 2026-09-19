// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../LICENSE-MIT for the upstream MIT license.

//! # FastCpy
//!
//! The Rust Compiler calls `memcpy` for slices of unknown length.
//! This crate provides a faster implementation of `memcpy` for slices up to 32bytes (64bytes with `avx`, and
//! always on x86_64 — see the tier comment in `slice_copy`).
//! If you know most of you copy operations are not too big you can use `fastcpy` to speed up your program.
//!
//! `fastcpy` is designed to contain not too much assembly, so the overhead is low.
//!
//! As fall back the standard `memcpy` is called
//!
//! ## Double Copy Trick
//! `fastcpy` employs a double copy trick to copy slices of length 4-32bytes (64bytes with `avx` or on x86_64).
//! E.g. Slice of length 6 can be copied with two uncoditional copy operations.
//!
//! /// [1, 2, 3, 4, 5, 6]
//! /// [1, 2, 3, 4]
//! ///       [3, 4, 5, 6]
//!

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
        len_mismatch_fail(src.len(), dst.len());
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

    // The 33..=64-byte tier. Upstream gates it on `cfg(target_feature = "avx")`
    // so each 32-byte half lowers to one `vmovdqu ymm`. No x86_64 lane of this
    // tree sets that cfg: `--print cfg` on the Trust pin (rustc 1.99.0-dev) and
    // on stock 1.97.1 and 1.98.1 stops at sse4.1 for x86_64-apple-darwin, sse3
    // for x86_64-pc-windows-gnu and sse2 for x86_64-unknown-linux-gnu, and none
    // of those lanes passes `-C target-cpu`/`target-feature`. So every
    // 33..=64-byte literal copy fell through to a `memcpy` call. On x86_64 the
    // tier is now taken unconditionally. Without AVX each 32-byte half is
    // call-free and branch-free, and only len >= 65 reaches `memcpy` (scratch
    // probe asm). The instructions follow the target CPU: four 8-byte `movq`
    // GPR moves on x86_64-apple-darwin's penryn baseline (Trust, stock 1.97.1
    // and 1.98.1 alike; no vector registers), two 16-byte SSE2 `movups` on the
    // generic x86-64 baseline (Linux, Windows).
    //
    // Measured on an i7-7920HQ (macOS 13.7, Trust, opt-level 3, 65536 random
    // 33..=64-byte copies, min of 200 passes, 6 runs): memcpy 17.7..18.5
    // ns/op, this tier 12.1..12.7 (-29..-33%). Copies over 64 bytes still take
    // the memcpy below, behind one more predictable compare; their delta
    // (+5.2..-3.5%) stayed inside the 2.4..5.8% that two byte-identical copies
    // of the old code differed by. A runtime-dispatched
    // `#[target_feature(enable = "avx2")]` copy was rejected: 13.6..14.4
    // ns/op, slower than this scalar tier (the AVX callee cannot inline into
    // a non-AVX caller, and the feature check is paid per call), and it needs
    // `unsafe`, which the default safe-encode + safe-decode build forbids.
    //
    // aarch64 keeps upstream's gate, so its codegen is untouched. The x86_64
    // Linux and Windows lanes take the tier too: the shipped Linux release
    // (tools/linux-auto-release.sh) and the x86_64-pc-windows-gnu
    // cfg-validation build now inline the SSE2 copies where they called
    // `memcpy`, and `xtask gate linux` (a `cargo check`) type-checks the tier.
    // The numbers above are from macOS on the i7-7920HQ only; the tier's speed
    // on those lanes is unmeasured.
    #[cfg(any(target_feature = "avx", target_arch = "x86_64"))]
    {
        if len <= 64 {
            double_copy_trick::<32>(src, dst);
            return;
        }
    }

    // For larger sizes we use the default, which calls memcpy
    // memcpy does some virtual memory tricks to copy large chunks of memory.
    //
    // The theory should be that the checks above don't cost much relative to the copy call for
    // larger copies.
    // The bounds checks in `copy_from_slice` are elided.
    dst.copy_from_slice(src);
}

#[inline(always)]
#[cfg_attr(trust_verify, trust::skip)] // precondition helper (1 <= src.len() <= dst.len()), enforced by slice_copy's contract
fn short_copy(src: &[u8], dst: &mut [u8]) {
    let len = src.len();

    // length 1-3
    dst[0] = src[0];
    if len >= 2 {
        double_copy_trick::<2>(src, dst);
    }
}

#[inline(always)]
/// [1, 2, 3, 4, 5, 6]
/// [1, 2, 3, 4]
///       [3, 4, 5, 6]
#[cfg_attr(trust_verify, trust::skip)] // precondition helper (SIZE <= src.len() <= dst.len()), enforced by slice_copy's contract
fn double_copy_trick<const SIZE: usize>(src: &[u8], dst: &mut [u8]) {
    dst[0..SIZE].copy_from_slice(&src[0..SIZE]);
    dst[src.len() - SIZE..].copy_from_slice(&src[src.len() - SIZE..]);
}

#[cfg(test)]
mod tests {
    use super::slice_copy;
    use alloc::vec::Vec;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn test_fast_short_slice_copy(left: Vec<u8>) {
            let mut right = vec![0u8; left.len()];
            slice_copy(&left, &mut right);
            prop_assert_eq!(&left, &right);
        }
    }

    #[test]
    fn test_fast_short_slice_copy_edge_cases() {
        for len in 0..(512 * 2) {
            let left = (0..len).map(|i| i as u8).collect::<Vec<_>>();
            let mut right = vec![0u8; len];
            slice_copy(&left, &mut right);
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
        slice_copy(&left, &mut right);
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
        slice_copy(&left, &mut right);
        assert_eq!(left, right);
    }
}
