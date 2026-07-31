// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fast path scanning for the parser with explicit SIMD intrinsics.
//!
//! ## Performance
//!
//! The ground state fast path finds the next byte that requires state
//! machine processing. With explicit SIMD intrinsics:
//! - AVX2 (x86_64, runtime-detected): Processes 32 bytes per iteration
//! - SSE2 (x86_64 baseline): Processes 16 bytes per iteration; SSE2 is
//!   architecturally guaranteed on x86_64, so it needs no runtime detection
//!   and serves both pre-AVX2 CPUs and inputs shorter than an AVX2 chunk
//! - NEON (aarch64): Processes 16 bytes per iteration
//! - Scalar fallback: other architectures
//!
//! Explicit SIMD provides better throughput than the scalar loops (early-exit
//! `position` searches do not auto-vectorize) for the predicate
//! `byte < 0x20 || byte > 0x7E` because we can use optimized SIMD comparisons.
//!
//! ## Special Bytes
//!
//! Non-printable bytes that exit the printable-ASCII fast path
//! (`find_non_printable`):
//! - C0 controls: 0x00-0x1F (including ESC at 0x1B)
//! - DEL: 0x7F
//! - High bytes: >= 0x80 (includes C1 controls and bytes >= 0xA0)

// =============================================================================
// x86_64 SIMD (AVX2 + SSE2)
// =============================================================================

/// AVX2 + SSE2 implementations for x86_64.
/// AVX2 (runtime-detected) processes 32 bytes per iteration; SSE2 (baseline,
/// no detection needed) processes 16 bytes per iteration and also serves as
/// the tail handler for the AVX2 loops.
#[cfg(all(target_arch = "x86_64", not(kani)))]
mod x86_simd {
    use std::arch::x86_64::*;

    /// Check if AVX2 is available, detected once and cached.
    ///
    /// `is_x86_feature_detected!` caches CPUID results but still pays an
    /// atomic load plus two bit tests per call; the `OnceLock` keeps the
    /// per-call cost to a single load + branch on the hot scan paths.
    #[inline]
    pub(crate) fn has_avx2() -> bool {
        static HAS_AVX2: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *HAS_AVX2.get_or_init(|| std::arch::is_x86_feature_detected!("avx2"))
    }

    /// Find first C0 control byte (< 0x20) using SSE2 (16 bytes/iteration).
    ///
    /// SSE2 is baseline on x86_64, so this is safe to call without runtime
    /// detection and can inline into the dispatcher.
    #[inline]
    pub(crate) fn find_c0_control_sse2(input: &[u8]) -> Option<usize> {
        let len = input.len();
        let ptr = input.as_ptr();
        let mut offset = 0usize;
        while offset + 16 <= len {
            // SAFETY: offset + 16 <= len; SSE2 is baseline on x86_64.
            let found = unsafe {
                // `_mm_loadu_si128` is an explicitly UNALIGNED load, so the
                // 1 -> 16 byte alignment increase in the pointer cast is fine.
                #[allow(clippy::cast_ptr_alignment)]
                let chunk = _mm_loadu_si128(ptr.add(offset).cast::<__m128i>());
                let threshold = _mm_set1_epi8(0x20i8);
                let bias = _mm_set1_epi8(-128i8);
                let biased_chunk = _mm_add_epi8(chunk, bias);
                let biased_threshold = _mm_add_epi8(threshold, bias);
                let below = _mm_cmplt_epi8(biased_chunk, biased_threshold);
                _mm_movemask_epi8(below).cast_unsigned()
            };
            if found != 0 {
                return Some(offset + found.trailing_zeros() as usize);
            }
            offset += 16;
        }
        for (i, &byte) in input[offset..].iter().enumerate() {
            if byte < 0x20 {
                return Some(offset + i);
            }
        }
        None
    }

    /// Find the first byte equal to any of `needles` using SSE2
    /// (16 bytes/iteration).
    ///
    /// Mirrors [`find_any_of_avx2`] at 128-bit width: one OR-reduced
    /// `_mm_cmpeq_epi8` per needle, sign-agnostic so high needles like 0x9C
    /// (ST) match correctly. SSE2 is baseline on x86_64.
    #[inline]
    pub(crate) fn find_any_of_sse2<const N: usize>(
        input: &[u8],
        needles: [u8; N],
    ) -> Option<usize> {
        let len = input.len();
        let ptr = input.as_ptr();
        let mut offset = 0usize;
        while offset + 16 <= len {
            // SAFETY: offset + 16 <= len; SSE2 is baseline on x86_64.
            let found = unsafe {
                // `_mm_loadu_si128` is an explicitly UNALIGNED load, so the
                // 1 -> 16 byte alignment increase in the pointer cast is fine.
                #[allow(clippy::cast_ptr_alignment)]
                let chunk = _mm_loadu_si128(ptr.add(offset).cast::<__m128i>());
                let mut eq = _mm_setzero_si128();
                for needle in needles {
                    let needle_vec = _mm_set1_epi8(i8::from_ne_bytes([needle]));
                    eq = _mm_or_si128(eq, _mm_cmpeq_epi8(chunk, needle_vec));
                }
                _mm_movemask_epi8(eq).cast_unsigned()
            };
            if found != 0 {
                return Some(offset + found.trailing_zeros() as usize);
            }
            offset += 16;
        }
        for (i, &byte) in input[offset..].iter().enumerate() {
            if needles.contains(&byte) {
                return Some(offset + i);
            }
        }
        None
    }

    /// Find first non-printable byte using SSE2 (16 bytes/iteration).
    /// Returns None if all bytes are printable ASCII (0x20-0x7E).
    ///
    /// Same signed-bias trick as [`find_non_printable_avx2`]. SSE2 is baseline
    /// on x86_64, so this needs no runtime detection and inlines into the
    /// dispatcher — which matters for the short (8-31 byte) printable runs
    /// that dominate line-oriented output.
    #[inline]
    pub(crate) fn find_non_printable_sse2(input: &[u8]) -> Option<usize> {
        let len = input.len();
        let ptr = input.as_ptr();
        let mut offset = 0usize;
        while offset + 16 <= len {
            // SAFETY: offset + 16 <= len; SSE2 is baseline on x86_64.
            let found = unsafe {
                // `_mm_loadu_si128` is an explicitly UNALIGNED load, so the
                // 1 -> 16 byte alignment increase in the pointer cast is fine.
                #[allow(clippy::cast_ptr_alignment)]
                let chunk = _mm_loadu_si128(ptr.add(offset).cast::<__m128i>());

                // Bias trick: shift to signed range so the printable range
                // [0x20, 0x7E] becomes [-0x60, -0x02] (see AVX2 version).
                let bias = _mm_set1_epi8(-128i8);
                let biased = _mm_add_epi8(chunk, bias);
                let biased_low = _mm_set1_epi8(-96i8);
                let biased_high = _mm_set1_epi8(-2i8);
                let too_low = _mm_cmplt_epi8(biased, biased_low);
                let too_high = _mm_cmpgt_epi8(biased, biased_high);
                let outside = _mm_or_si128(too_low, too_high);
                _mm_movemask_epi8(outside).cast_unsigned()
            };
            if found != 0 {
                return Some(offset + found.trailing_zeros() as usize);
            }
            offset += 16;
        }
        for (i, &byte) in input[offset..].iter().enumerate() {
            if !(0x20..=0x7E).contains(&byte) {
                return Some(offset + i);
            }
        }
        None
    }

    /// Find first C0 control byte (< 0x20) using AVX2.
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(crate) unsafe fn find_c0_control_avx2(input: &[u8]) -> Option<usize> {
        let len = input.len();
        if len == 0 {
            return None;
        }
        let ptr = input.as_ptr();
        let mut offset = 0usize;
        while offset + 32 <= len {
            // SAFETY: offset + 32 <= len; caller guarantees AVX2.
            let found = unsafe {
                // `_mm256_loadu_si256` is an explicitly UNALIGNED load, so the
                // 1 -> 32 byte alignment increase in the pointer cast is fine.
                #[allow(clippy::cast_ptr_alignment)]
                let chunk = _mm256_loadu_si256(ptr.add(offset).cast::<__m256i>());
                let threshold = _mm256_set1_epi8(0x20i8);
                let bias = _mm256_set1_epi8(-128i8);
                let biased_chunk = _mm256_add_epi8(chunk, bias);
                let biased_threshold = _mm256_add_epi8(threshold, bias);
                let below = _mm256_cmpgt_epi8(biased_threshold, biased_chunk);
                _mm256_movemask_epi8(below).cast_unsigned()
            };
            if found != 0 {
                return Some(offset + found.trailing_zeros() as usize);
            }
            offset += 32;
        }
        // Remainder is < 32 bytes: one SSE2 chunk + scalar tail.
        find_c0_control_sse2(&input[offset..]).map(|i| offset + i)
    }

    /// Find the first byte equal to any of `needles` using AVX2.
    ///
    /// Used by the DCS/APC passthrough bulk fast paths: the needle set is a
    /// small fixed terminator set, so we OR-reduce one `_mm256_cmpeq_epi8` per
    /// needle over each 32-byte chunk. Equality compares are sign-agnostic, so
    /// no signed-bias trick is needed and high needles like 0x9C (ST) match
    /// correctly. The per-needle broadcasts are loop-invariant and hoisted out
    /// of the chunk loop by the optimizer (mirroring `find_c0_control_avx2`).
    ///
    /// # Safety
    /// Caller must ensure AVX2 is available (use `has_avx2()` first).
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(crate) unsafe fn find_any_of_avx2<const N: usize>(
        input: &[u8],
        needles: [u8; N],
    ) -> Option<usize> {
        let len = input.len();
        if len == 0 {
            return None;
        }
        let ptr = input.as_ptr();
        let mut offset = 0usize;
        while offset + 32 <= len {
            // SAFETY: offset + 32 <= len; caller guarantees AVX2.
            let found = unsafe {
                // `_mm256_loadu_si256` is an explicitly UNALIGNED load, so the
                // 1 -> 32 byte alignment increase in the pointer cast is fine.
                #[allow(clippy::cast_ptr_alignment)]
                let chunk = _mm256_loadu_si256(ptr.add(offset).cast::<__m256i>());
                let mut eq = _mm256_setzero_si256();
                for needle in needles {
                    let needle_vec = _mm256_set1_epi8(i8::from_ne_bytes([needle]));
                    eq = _mm256_or_si256(eq, _mm256_cmpeq_epi8(chunk, needle_vec));
                }
                _mm256_movemask_epi8(eq).cast_unsigned()
            };
            if found != 0 {
                return Some(offset + found.trailing_zeros() as usize);
            }
            offset += 32;
        }
        // Remainder is < 32 bytes: one SSE2 chunk + scalar tail.
        find_any_of_sse2(&input[offset..], needles).map(|i| offset + i)
    }

    /// Find first non-printable byte using AVX2.
    /// Returns None if all bytes are printable ASCII (0x20-0x7E).
    ///
    /// # Safety
    /// Caller must ensure AVX2 is available (use `has_avx2()` first).
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(crate) unsafe fn find_non_printable_avx2(input: &[u8]) -> Option<usize> {
        let len = input.len();
        if len == 0 {
            return None;
        }

        let ptr = input.as_ptr();
        let mut offset = 0usize;

        // Process 32 bytes at a time
        while offset + 32 <= len {
            // SAFETY: We've checked that offset + 32 <= len, so reading 32 bytes
            // from ptr.add(offset) is valid. Caller guarantees AVX2 is available.
            let found = unsafe {
                // `_mm256_loadu_si256` is an explicitly UNALIGNED load, so the
                // 1 -> 32 byte alignment increase in the pointer cast is fine.
                #[allow(clippy::cast_ptr_alignment)]
                let chunk = _mm256_loadu_si256(ptr.add(offset).cast::<__m256i>());

                // Check for bytes < 0x20 or > 0x7E
                // AVX2 doesn't have unsigned compare, so we use signed with bias
                //
                // Bias trick: subtract 0x80 from each byte to convert to signed range
                // Then printable range [0x20, 0x7E] becomes [-0x60, -0x02] in signed
                let bias = _mm256_set1_epi8(-128i8); // 0x80
                let biased = _mm256_add_epi8(chunk, bias);

                // Printable low (0x20 - 0x80 = -0x60 = -96 signed)
                let biased_low = _mm256_set1_epi8(-96i8);
                // Printable high (0x7E - 0x80 = -0x02 = -2 signed)
                let biased_high = _mm256_set1_epi8(-2i8);

                // Check if biased < biased_low (meaning original < 0x20)
                let too_low = _mm256_cmpgt_epi8(biased_low, biased);
                // Check if biased > biased_high (meaning original > 0x7E)
                let too_high = _mm256_cmpgt_epi8(biased, biased_high);

                // Combine: any byte outside [0x20, 0x7E]
                let outside = _mm256_or_si256(too_low, too_high);

                _mm256_movemask_epi8(outside).cast_unsigned()
            };

            if found != 0 {
                return Some(offset + found.trailing_zeros() as usize);
            }

            offset += 32;
        }

        // Remainder is < 32 bytes: one SSE2 chunk + scalar tail.
        find_non_printable_sse2(&input[offset..]).map(|i| offset + i)
    }
}

// This re-export exists solely for `crate::simd_csi`; every call site inside
// this module reaches `x86_simd::has_avx2()` directly. `simd_csi` became
// `#[cfg(test)]`-only when the speculative CSI parameter pre-parse was removed
// from production, so the `test` cfg here tracks it — without it this would be
// an unused import in an ordinary x86_64 build.
#[cfg(all(target_arch = "x86_64", not(kani), test))]
pub(crate) use x86_simd::has_avx2;

// =============================================================================
// aarch64 SIMD (NEON)
// =============================================================================

/// aarch64 implementation: 16-byte chunked scans expressed as safe code.
///
/// Each chunk is classified with a branch-free byte fold that LLVM
/// auto-vectorizes to NEON byte-compare instructions; only a matching chunk
/// is rescanned to locate the exact position. This replaces hand-written
/// `std::arch::aarch64` intrinsics: Trust's verifier fails closed on
/// unmodeled intrinsic calls, so the scans are expressed as safe,
/// obligation-free code with identical results. The saturating position
/// arithmetic is exact at runtime (`offset + i` never exceeds `input.len()`).
///
/// ## Codegen contract — the classifier shape is load-bearing
///
/// The chunk classifiers here MUST be written as a min/max reduction over a
/// `u8` accumulator, never as an OR-reduction over a `bool`. The `bool` form
/// (`let mut any = false; any |= byte < 0x20 || byte > 0x7E;`) reads as the
/// obvious branch-free fold, but it does not survive to the emitted code: an
/// i1 reduction over a short-circuiting `||` defeats LLVM's loop vectorizer,
/// which instead fully unrolls the 16 lanes into scalar code — 17 `ldrb` and
/// ZERO vector instructions per chunk (~62 scalar instructions per 16 bytes)
/// against the ~6 the reduce form emits (`ldr q` / `sub.16b` / `umaxv.16b` /
/// `fmov` / compare / branch).
///
/// Do NOT judge this by compiling the classifier standalone. Under this
/// repo's pinned compiler the old `bool` fold in `find_non_printable_neon`
/// DOES vectorize when the function is compiled on its own — and then LLVM
/// throws the vectorization away once the function inlines into
/// `advance_simd_loop`, which is the only place that matters. (The `bool`
/// fold in `find_c0_control_neon` never vectorized at all, standalone or
/// not.) A plain `u8` OR-accumulator fails the same way; only the min/max
/// reduce holds up after inlining.
///
/// This is the hottest scan in the parser — `find_non_printable` runs once
/// per ground-state printable run, i.e. per line, per SGR, per multibyte
/// lead — so the shape change is worth real throughput. Measured in-tree on
/// aarch64 (release profile, `advance_fast` + `NullSink`, 8 MiB corpora,
/// best-of-5), `bool` fold -> reduce: 80-col CRLF text 4,608 -> 7,898 MB/s;
/// 4 KiB printable flood 8,569 -> 27,691 MB/s; mixed international text
/// 1,193 -> 1,590 MB/s; `CUP` + text 2,857 -> 3,748 MB/s; 40 chars + SGR
/// 2,622 -> 2,951 MB/s.
///
/// LLVM's choice here is version-sensitive, so a toolchain bump that
/// de-vectorizes these folds would be a silent throughput regression: check
/// for `umaxv`/`uminv` in this module's `--emit asm` output if parser
/// throughput ever falls off a cliff.
///
/// [`find_any_of_neon`] keeps its `bool` OR-reduce, and that is deliberate:
/// equality compares over a fixed needle set DO vectorize (verified on this
/// toolchain — 13 `.16b` ops per chunk, the only `ldrb` being the sub-16-byte
/// tail), because the lane predicate is a chain of `==` with no
/// short-circuiting range test for the vectorizer to choke on.
#[cfg(all(target_arch = "aarch64", not(kani)))]
mod arm_simd {
    /// Find first C0 control byte (< 0x20) via chunked classification.
    #[inline]
    pub(crate) fn find_c0_control_neon(input: &[u8]) -> Option<usize> {
        let (chunks, rem) = input.as_chunks::<16>();
        let mut offset = 0usize;
        for chunk in chunks {
            // Min-reduce, not an OR-reduce over `bool`: LLVM lowers this to a
            // single `uminv.16b`. The `let mut any = false; any |= byte < 0x20`
            // form it replaced did NOT vectorize (an i1 reduction over a
            // short-circuiting `||` defeats the loop vectorizer) — it fully
            // unrolled to 16 `ldrb`/compare pairs per chunk. See the module
            // doc for the codegen contract.
            let mut lowest = 0xFFu8;
            for &byte in chunk {
                lowest = if byte < lowest { byte } else { lowest };
            }
            if lowest < 0x20 {
                for (i, &byte) in chunk.iter().enumerate() {
                    if byte < 0x20 {
                        return Some(offset.saturating_add(i));
                    }
                }
            }
            offset = offset.saturating_add(16);
        }
        for (i, &byte) in rem.iter().enumerate() {
            if byte < 0x20 {
                return Some(offset.saturating_add(i));
            }
        }
        None
    }

    /// Find the first byte equal to any of `needles` via chunked classification.
    ///
    /// Mirrors [`find_c0_control_neon`] but matches a small fixed needle set by
    /// OR-reducing one equality compare per needle over each 16-byte chunk.
    /// Equality compares match the exact byte, so high needles like 0x9C (ST)
    /// are handled correctly.
    #[inline]
    pub(crate) fn find_any_of_neon<const N: usize>(
        input: &[u8],
        needles: [u8; N],
    ) -> Option<usize> {
        let (chunks, rem) = input.as_chunks::<16>();
        let mut offset = 0usize;
        for chunk in chunks {
            let mut any = false;
            for &byte in chunk {
                let mut hit = false;
                for &needle in &needles {
                    hit |= byte == needle;
                }
                any |= hit;
            }
            if any {
                for (i, &byte) in chunk.iter().enumerate() {
                    if needles.contains(&byte) {
                        return Some(offset.saturating_add(i));
                    }
                }
            }
            offset = offset.saturating_add(16);
        }
        for (i, &byte) in rem.iter().enumerate() {
            if needles.contains(&byte) {
                return Some(offset.saturating_add(i));
            }
        }
        None
    }

    /// Find first non-printable byte via chunked classification.
    /// Returns None if all bytes are printable ASCII (0x20-0x7E).
    #[inline]
    #[allow(
        clippy::manual_range_contains,
        reason = "the locate-the-index rescan keeps the branch-free `byte < 0x20 || byte > 0x7E` spelling; the RangeInclusive::contains rewrite is not the lowerable form the strict Trust gate accepts. The CLASSIFIER above it must stay a max-reduce so it lowers to `umaxv.16b` — see the module doc"
    )]
    pub(crate) fn find_non_printable_neon(input: &[u8]) -> Option<usize> {
        let (chunks, rem) = input.as_chunks::<16>();
        let mut offset = 0usize;
        for chunk in chunks {
            // Max-reduce over `byte.wrapping_sub(0x20)`: LLVM lowers this to a
            // single `umaxv.16b`. `biased > 0x5E` is exactly
            // `byte < 0x20 || byte > 0x7E` over u8 — 0x20..=0x7E biases to
            // 0..=0x5E, 0x7F..=0xFF to 0x5F..=0xDF, and 0x00..=0x1F wraps to
            // 0xE0..=0xFF. The `bool` OR-reduce this replaced did NOT
            // vectorize; see the module doc for the codegen contract and the
            // measured cost.
            let mut worst = 0u8;
            for &byte in chunk {
                let biased = byte.wrapping_sub(0x20);
                worst = if biased > worst { biased } else { worst };
            }
            if worst > 0x5E {
                // First match is in this chunk; locate it with a short scan.
                for (i, &byte) in chunk.iter().enumerate() {
                    if byte < 0x20 || byte > 0x7E {
                        return Some(offset.saturating_add(i));
                    }
                }
            }
            offset = offset.saturating_add(16);
        }
        // Handle remaining bytes with scalar fallback
        for (i, &byte) in rem.iter().enumerate() {
            if !(0x20..=0x7E).contains(&byte) {
                return Some(offset.saturating_add(i));
            }
        }
        None
    }
}

// =============================================================================
// Public API with runtime dispatch
// =============================================================================

/// Find the first non-printable byte using the best available SIMD.
///
/// This function automatically selects the optimal implementation:
/// - AVX2 on x86_64 with AVX2 support and a full 32-byte chunk
/// - SSE2 on x86_64 otherwise (baseline, no detection)
/// - NEON on aarch64
/// - Scalar fallback on other platforms
#[inline]
#[allow(unreachable_code)]
fn find_non_printable_simd(input: &[u8]) -> Option<usize> {
    // Empty-run fast boundary: if the FIRST byte is already non-printable there is no
    // printable run to scan — return without the SIMD load + `has_avx2()` OnceLock. This
    // is the common case on CRLF-terminated output (one empty run per line) and dense
    // CSI/UTF-8 TUI redraws. Byte-identical to the SIMD/scalar result (which returns
    // `Some(0)` for a non-printable first byte and `None` for empty input), and the
    // predicate matches `find_non_printable_scalar` verbatim, so the Kani equivalence
    // proofs are preserved. On the pure-ASCII hot run this is one predicted-not-taken
    // branch before the existing dispatch.
    match input.first() {
        Some(&b0) if !(0x20..=0x7E).contains(&b0) => return Some(0),
        None => return None,
        _ => {}
    }
    #[cfg(all(target_arch = "x86_64", not(kani)))]
    {
        // AVX2 only pays off once a full 32-byte chunk exists; shorter inputs
        // go straight to the inlined SSE2 path with no detection branch.
        if input.len() >= 32 && x86_simd::has_avx2() {
            // SAFETY: We just checked that AVX2 is available
            return unsafe { x86_simd::find_non_printable_avx2(input) };
        }
        return x86_simd::find_non_printable_sse2(input);
    }

    #[cfg(all(target_arch = "aarch64", not(kani)))]
    {
        return arm_simd::find_non_printable_neon(input);
    }

    // Scalar fallback (other architectures; x86_64/aarch64 returned above)
    find_non_printable_scalar(input)
}

/// Scalar implementation (LLVM auto-vectorized).
/// Used as fallback when SIMD is not available.
#[cfg(not(kani))]
#[inline]
fn find_non_printable_scalar(input: &[u8]) -> Option<usize> {
    input.iter().position(|&b| !(0x20..=0x7E).contains(&b))
}

/// Kani-specific scalar implementation.
/// Uses an explicit indexed loop to prevent LLVM auto-vectorization with
/// NEON intrinsics (simd_reduce_max) that Kani cannot model.
#[cfg(kani)]
fn find_non_printable_scalar(input: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b < 0x20 || b > 0x7E {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the next byte that's not in the printable ASCII range.
///
/// This is the primary fast path function, using explicit SIMD when available:
/// - AVX2 on x86_64 (32 bytes/iteration, runtime-detected)
/// - SSE2 on x86_64 (16 bytes/iteration, baseline — no detection)
/// - NEON on aarch64 (16 bytes/iteration)
/// - Scalar fallback on other architectures
#[inline]
pub(super) fn find_non_printable(input: &[u8]) -> Option<usize> {
    find_non_printable_simd(input)
}

/// Count the number of printable ASCII bytes at the start of input.
///
/// Returns the length of the prefix that's all printable ASCII.
#[inline]
pub(crate) fn count_printable(input: &[u8]) -> usize {
    find_non_printable(input).unwrap_or(input.len())
}

/// Optimized batch print: returns slice of printable ASCII at start.
///
/// This is used by the fast path to avoid per-byte dispatch for
/// long runs of printable text.
#[inline]
pub(crate) fn take_printable(input: &[u8]) -> (&[u8], &[u8]) {
    let n = count_printable(input);
    // `n` is the length of a prefix of `input`, so `n <= input.len()` always
    // and the fallback is unreachable at runtime; the checked spelling keeps
    // the split free of panic obligations.
    input.split_at_checked(n).unwrap_or((input, &[]))
}

/// Find the first C0 control byte (< 0x20) in input.
///
/// Used by the OSC/DCS fast paths to bulk-skip data bytes.
/// In OscString state, bytes 0x20-0xFF are all OscPut (data);
/// only bytes < 0x20 need state machine handling (BEL terminator,
/// ESC for ST, CAN, SUB, etc.).
///
/// Returns `None` if the entire input is >= 0x20.
#[inline]
pub(crate) fn find_c0_control(input: &[u8]) -> Option<usize> {
    find_c0_control_simd(input)
}

#[inline]
#[allow(unreachable_code)]
fn find_c0_control_simd(input: &[u8]) -> Option<usize> {
    #[cfg(all(target_arch = "x86_64", not(kani)))]
    {
        if input.len() >= 32 && x86_simd::has_avx2() {
            // SAFETY: We just checked that AVX2 is available
            return unsafe { x86_simd::find_c0_control_avx2(input) };
        }
        return x86_simd::find_c0_control_sse2(input);
    }

    #[cfg(all(target_arch = "aarch64", not(kani)))]
    {
        return arm_simd::find_c0_control_neon(input);
    }

    find_c0_control_scalar(input)
}

#[cfg(not(kani))]
#[inline]
fn find_c0_control_scalar(input: &[u8]) -> Option<usize> {
    input.iter().position(|&b| b < 0x20)
}

#[cfg(kani)]
fn find_c0_control_scalar(input: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < input.len() {
        if input[i] < 0x20 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the first byte in `input` that equals any of the `needles`.
///
/// Used by the DCS/APC passthrough bulk fast paths to bulk-skip data bytes:
/// `needles` is the small fixed terminator set ({0x18,0x1A,0x1B,0x9C}, plus
/// 0x7F for `DcsPassthrough`). Mirrors [`find_c0_control`] but matches a fixed
/// multi-byte set via OR-reduced equality compares instead of a single
/// threshold, so megabyte DCS/APC payloads (Sixel, Kitty graphics) get the
/// same SIMD throughput the OSC path already enjoys.
///
/// Returns `None` if no byte in `input` is in `needles`.
#[inline]
pub(crate) fn find_any_of<const N: usize>(input: &[u8], needles: [u8; N]) -> Option<usize> {
    find_any_of_simd(input, needles)
}

#[inline]
#[allow(unreachable_code)]
fn find_any_of_simd<const N: usize>(input: &[u8], needles: [u8; N]) -> Option<usize> {
    #[cfg(all(target_arch = "x86_64", not(kani)))]
    {
        if input.len() >= 32 && x86_simd::has_avx2() {
            // SAFETY: We just checked that AVX2 is available
            return unsafe { x86_simd::find_any_of_avx2(input, needles) };
        }
        return x86_simd::find_any_of_sse2(input, needles);
    }

    #[cfg(all(target_arch = "aarch64", not(kani)))]
    {
        return arm_simd::find_any_of_neon(input, needles);
    }

    find_any_of_scalar(input, needles)
}

#[cfg(not(kani))]
#[inline]
fn find_any_of_scalar<const N: usize>(input: &[u8], needles: [u8; N]) -> Option<usize> {
    input.iter().position(|&b| needles.contains(&b))
}

/// Kani-specific scalar implementation.
///
/// Explicit nested indexed loops (no iterators) so the proof models a plain
/// scan instead of an auto-vectorized one Kani cannot reason about, matching
/// [`find_c0_control_scalar`].
#[cfg(kani)]
fn find_any_of_scalar<const N: usize>(input: &[u8], needles: [u8; N]) -> Option<usize> {
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        let mut j = 0;
        while j < N {
            if b == needles[j] {
                return Some(i);
            }
            j += 1;
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_printable() {
        assert_eq!(count_printable(b"hello\x1bworld"), 5);
        assert_eq!(count_printable(b"hello world"), 11);
        assert_eq!(count_printable(b"\x1bhello"), 0);
    }

    #[test]
    fn test_take_printable() {
        let (printable, rest) = take_printable(b"hello\x1bworld");
        assert_eq!(printable, b"hello");
        assert_eq!(rest, b"\x1bworld");
    }

    // SIMD-specific tests
    #[test]
    fn test_find_non_printable_simd_empty() {
        assert_eq!(find_non_printable_simd(b""), None);
    }

    #[test]
    fn test_find_non_printable_simd_pure_ascii() {
        let data = b"Hello, World! This is a test of the terminal parser.";
        assert_eq!(find_non_printable_simd(data), None);
    }

    #[test]
    fn test_find_non_printable_simd_escape_at_start() {
        assert_eq!(find_non_printable_simd(b"\x1bhello"), Some(0));
    }

    #[test]
    fn test_find_non_printable_simd_escape_at_end() {
        let mut data = vec![b'A'; 100];
        data[99] = 0x1B;
        assert_eq!(find_non_printable_simd(&data), Some(99));
    }

    #[test]
    fn test_find_non_printable_simd_escape_middle() {
        let mut data = vec![b'A'; 100];
        data[50] = 0x1B;
        assert_eq!(find_non_printable_simd(&data), Some(50));
    }

    #[test]
    fn test_find_non_printable_simd_large_input() {
        // Test with >32 bytes to ensure SIMD path is exercised
        let mut data = vec![b'A'; 1024];
        data[512] = 0x1B;
        assert_eq!(find_non_printable_simd(&data), Some(512));
    }

    #[test]
    fn test_find_non_printable_simd_boundary_values() {
        // Test at exact boundaries
        assert_eq!(find_non_printable_simd(b"\x1F"), Some(0)); // Just below 0x20
        assert_eq!(find_non_printable_simd(b"\x20"), None); // Exactly 0x20
        assert_eq!(find_non_printable_simd(b"\x7E"), None); // Exactly 0x7E
        assert_eq!(find_non_printable_simd(b"\x7F"), Some(0)); // Just above 0x7E
    }

    #[test]
    fn test_find_non_printable_simd_all_printable_varying_sizes() {
        // Test various sizes to exercise both SIMD and scalar paths
        for size in [1usize, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129] {
            let data: Vec<u8> = (0..size)
                .map(|i| b'A' + u8::try_from(i % 26).unwrap())
                .collect();
            assert_eq!(
                find_non_printable_simd(&data),
                None,
                "Failed for size {}",
                size
            );
        }
    }

    #[test]
    fn test_find_non_printable_simd_high_bytes() {
        // Test high bytes (>= 0x80) which are non-printable for this fast path.
        // These include C1 controls and bytes >= 0xA0.
        assert_eq!(find_non_printable_simd(b"\x80"), Some(0)); // First C1 control
        assert_eq!(find_non_printable_simd(b"\x9B"), Some(0)); // CSI (C1 control)
        assert_eq!(find_non_printable_simd(b"\x9F"), Some(0)); // Last C1 control
        assert_eq!(find_non_printable_simd(b"\xA0"), Some(0)); // Non-breaking space (UTF-8)
        assert_eq!(find_non_printable_simd(b"\xFF"), Some(0)); // Maximum byte value

        // Test high bytes embedded in text
        let mut data = vec![b'A'; 100];
        data[50] = 0x80;
        assert_eq!(find_non_printable_simd(&data), Some(50));

        // Test high byte after SIMD boundary
        let mut data = vec![b'A'; 64];
        data[33] = 0xFF;
        assert_eq!(find_non_printable_simd(&data), Some(33));
    }

    #[test]
    fn test_find_non_printable_simd_scalar_equivalence() {
        // Verify SIMD and scalar implementations produce identical results
        // across various input patterns

        // Test all single-byte values
        for byte in 0u8..=255u8 {
            let input = [byte];
            let simd_result = find_non_printable_simd(&input);
            let scalar_result = find_non_printable_scalar(&input);
            assert_eq!(
                simd_result, scalar_result,
                "Mismatch for byte 0x{:02X}: SIMD={:?}, scalar={:?}",
                byte, simd_result, scalar_result
            );
        }

        // Test all boundary positions for various sizes
        for size in [16, 32, 48, 64, 100] {
            for pos in [0, 1, size / 2, size - 2, size - 1] {
                if pos < size {
                    let mut data = vec![b'A'; size];
                    data[pos] = 0x1B; // ESC
                    assert_eq!(
                        find_non_printable_simd(&data),
                        find_non_printable_scalar(&data),
                        "Mismatch for size {} pos {}",
                        size,
                        pos
                    );
                }
            }
        }
    }

    #[test]
    fn test_find_c0_control_empty() {
        assert_eq!(find_c0_control(b""), None);
    }

    #[test]
    fn test_find_c0_control_pure_data() {
        assert_eq!(find_c0_control(b"Hello, World!"), None);
        assert_eq!(find_c0_control(b"\x80\x90\xA0\xFF"), None);
        assert_eq!(find_c0_control(b"\x20\x7E\x7F\x80"), None);
    }

    #[test]
    fn test_find_c0_control_at_start() {
        assert_eq!(find_c0_control(b"\x07hello"), Some(0));
        assert_eq!(find_c0_control(b"\x1Bhello"), Some(0));
        assert_eq!(find_c0_control(b"\x00hello"), Some(0));
    }

    #[test]
    fn test_find_c0_control_embedded() {
        assert_eq!(find_c0_control(b"abc\x07def"), Some(3));
        assert_eq!(find_c0_control(b"abcdefghijklmnop\x1Bqrs"), Some(16));
    }

    #[test]
    fn test_find_c0_control_boundary() {
        assert_eq!(find_c0_control(b"\x1F"), Some(0));
        assert_eq!(find_c0_control(b"\x20"), None);
    }

    #[test]
    fn test_find_c0_control_varying_sizes() {
        for size in [1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 128] {
            let data: Vec<u8> = (0..size).map(|i| 0x20 + (i as u8 % 0x60)).collect();
            assert_eq!(find_c0_control(&data), None, "Failed for size {size}");

            for pos in [0, size / 2, size - 1] {
                let mut data2 = data.clone();
                data2[pos] = 0x07;
                assert_eq!(
                    find_c0_control(&data2),
                    Some(pos),
                    "Failed for size {size} pos {pos}"
                );
            }
        }
    }

    #[test]
    fn test_find_c0_control_scalar_equivalence() {
        for byte in 0u8..=255 {
            let input = [byte];
            assert_eq!(
                find_c0_control(&input),
                find_c0_control_scalar(&input),
                "Mismatch for byte 0x{byte:02X}"
            );
        }
    }

    // DCS terminator set: {CAN, SUB, ESC, ST}.
    const DCS_TERMINATORS: [u8; 4] = [0x18, 0x1A, 0x1B, 0x9C];
    // DCS-passthrough boundary set: terminators + DEL.
    const DCS_PASSTHROUGH: [u8; 5] = [0x18, 0x1A, 0x1B, 0x9C, 0x7F];

    #[test]
    fn test_find_any_of_empty() {
        assert_eq!(find_any_of(b"", DCS_TERMINATORS), None);
        assert_eq!(find_any_of(b"", DCS_PASSTHROUGH), None);
    }

    #[test]
    fn test_find_any_of_pure_data() {
        assert_eq!(find_any_of(b"Hello, World!", DCS_TERMINATORS), None);
        // High data bytes that are NOT needles must not match (no bias trick).
        assert_eq!(find_any_of(b"\x80\x90\xA0\xFF", DCS_TERMINATORS), None);
        // 0x7F is data for the terminator-only set but a boundary for passthrough.
        assert_eq!(find_any_of(b"abc\x7Fdef", DCS_TERMINATORS), None);
        assert_eq!(find_any_of(b"abc\x7Fdef", DCS_PASSTHROUGH), Some(3));
    }

    #[test]
    fn test_find_any_of_at_start() {
        assert_eq!(find_any_of(b"\x18hello", DCS_TERMINATORS), Some(0));
        assert_eq!(find_any_of(b"\x1Ahello", DCS_TERMINATORS), Some(0));
        assert_eq!(find_any_of(b"\x1Bhello", DCS_TERMINATORS), Some(0));
        // 0x9C (ST) is a high byte — equality compare must still find it.
        assert_eq!(find_any_of(b"\x9Chello", DCS_TERMINATORS), Some(0));
    }

    #[test]
    fn test_find_any_of_embedded() {
        assert_eq!(find_any_of(b"abc\x9Cdef", DCS_TERMINATORS), Some(3));
        // Needle just past the 32-byte AVX2 / 16-byte NEON SIMD boundary, so the
        // first full chunk is all-data and the match comes from a later chunk.
        let mut data = vec![b'A'; 40];
        data[33] = 0x1B;
        assert_eq!(find_any_of(&data, DCS_TERMINATORS), Some(33));
        data[33] = 0x7F;
        assert_eq!(find_any_of(&data, DCS_TERMINATORS), None);
        assert_eq!(find_any_of(&data, DCS_PASSTHROUGH), Some(33));
    }

    #[test]
    fn test_find_any_of_scalar_equivalence() {
        // SIMD vs scalar must agree for every single byte against both DCS sets.
        for byte in 0u8..=255 {
            let input = [byte];
            assert_eq!(
                find_any_of(&input, DCS_TERMINATORS),
                find_any_of_scalar(&input, DCS_TERMINATORS),
                "terminator-set mismatch for byte 0x{byte:02X}"
            );
            assert_eq!(
                find_any_of(&input, DCS_PASSTHROUGH),
                find_any_of_scalar(&input, DCS_PASSTHROUGH),
                "passthrough-set mismatch for byte 0x{byte:02X}"
            );
        }

        // Exercise both the SIMD body and the scalar tail at varied sizes, with a
        // needle planted at each position. b'A' (0x41) is in neither needle set.
        for size in [1usize, 15, 16, 17, 31, 32, 33, 48, 64, 100] {
            for needle in DCS_PASSTHROUGH {
                // saturating_sub keeps the small sizes (e.g. 1) from underflowing.
                for pos in [0, 1, size / 2, size.saturating_sub(2), size - 1] {
                    if pos < size {
                        let mut data = vec![b'A'; size];
                        data[pos] = needle;
                        assert_eq!(
                            find_any_of(&data, DCS_PASSTHROUGH),
                            find_any_of_scalar(&data, DCS_PASSTHROUGH),
                            "size {size} needle 0x{needle:02X} pos {pos}"
                        );
                    }
                }
            }
        }
    }

    // Differential tests for the x86_64 tiers: SSE2 and AVX2 (when the host
    // has it) must classify every byte exactly like the scalar reference, at
    // every position across the 16- and 32-byte chunk boundaries.
    #[cfg(all(target_arch = "x86_64", not(kani)))]
    #[test]
    fn test_x86_tiers_exhaustive_bytes_and_positions() {
        let avx2 = x86_simd::has_avx2();
        for size in [1usize, 7, 8, 15, 16, 17, 31, 32, 33, 47, 48, 63, 64, 65] {
            for pos in 0..size {
                for byte in 0u8..=255 {
                    let mut data = vec![b'A'; size];
                    data[pos] = byte;

                    let np = find_non_printable_scalar(&data);
                    assert_eq!(
                        x86_simd::find_non_printable_sse2(&data),
                        np,
                        "sse2 non_printable size {size} pos {pos} byte 0x{byte:02X}"
                    );
                    let c0 = find_c0_control_scalar(&data);
                    assert_eq!(
                        x86_simd::find_c0_control_sse2(&data),
                        c0,
                        "sse2 c0_control size {size} pos {pos} byte 0x{byte:02X}"
                    );
                    let any = find_any_of_scalar(&data, DCS_PASSTHROUGH);
                    assert_eq!(
                        x86_simd::find_any_of_sse2(&data, DCS_PASSTHROUGH),
                        any,
                        "sse2 any_of size {size} pos {pos} byte 0x{byte:02X}"
                    );

                    if avx2 {
                        // SAFETY: AVX2 availability checked above.
                        unsafe {
                            assert_eq!(
                                x86_simd::find_non_printable_avx2(&data),
                                np,
                                "avx2 non_printable size {size} pos {pos} byte 0x{byte:02X}"
                            );
                            assert_eq!(
                                x86_simd::find_c0_control_avx2(&data),
                                c0,
                                "avx2 c0_control size {size} pos {pos} byte 0x{byte:02X}"
                            );
                            assert_eq!(
                                x86_simd::find_any_of_avx2(&data, DCS_PASSTHROUGH),
                                any,
                                "avx2 any_of size {size} pos {pos} byte 0x{byte:02X}"
                            );
                        }
                    }
                }
            }
        }
    }

    // Random-buffer differential: all x86 tiers vs scalar over buffers whose
    // lengths span the 16/32-byte chunk boundaries, biased toward printable
    // bytes so the scans reach deep offsets and chunk-boundary tails.
    #[cfg(all(target_arch = "x86_64", not(kani)))]
    #[test]
    fn test_x86_tiers_random_buffers() {
        let avx2 = x86_simd::has_avx2();
        let mut state: u64 = 0x243F_6A88_85A3_08D3;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u32
        };
        for _ in 0..20_000 {
            let len = (next() % 130) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    let r = next();
                    if r % 8 == 0 {
                        (r >> 8) as u8 // arbitrary byte (may be a match)
                    } else {
                        0x20 + ((r >> 8) % 0x5F) as u8 // printable 0x20-0x7E
                    }
                })
                .collect();

            let np = find_non_printable_scalar(&data);
            assert_eq!(x86_simd::find_non_printable_sse2(&data), np);
            let c0 = find_c0_control_scalar(&data);
            assert_eq!(x86_simd::find_c0_control_sse2(&data), c0);
            let any = find_any_of_scalar(&data, DCS_PASSTHROUGH);
            assert_eq!(x86_simd::find_any_of_sse2(&data, DCS_PASSTHROUGH), any);
            // The public dispatch must agree regardless of which tier it picks.
            assert_eq!(find_non_printable_simd(&data), np);
            assert_eq!(find_c0_control_simd(&data), c0);
            assert_eq!(find_any_of_simd(&data, DCS_PASSTHROUGH), any);

            if avx2 {
                // SAFETY: AVX2 availability checked above.
                unsafe {
                    assert_eq!(x86_simd::find_non_printable_avx2(&data), np);
                    assert_eq!(x86_simd::find_c0_control_avx2(&data), c0);
                    assert_eq!(x86_simd::find_any_of_avx2(&data, DCS_PASSTHROUGH), any);
                }
            }
        }
    }
}
