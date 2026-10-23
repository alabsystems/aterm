// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SIMD-accelerated byte scans for printable ASCII runs, C0 controls, and DCS/APC terminators.

#[cfg(target_arch = "x86_64")]
mod x86_simd {
    use std::arch::x86_64::*;

    /// Returns whether AVX2 is supported on the current CPU.
    #[inline]
    pub(crate) fn has_avx2() -> bool {
        static HAS_AVX2: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *HAS_AVX2.get_or_init(|| std::arch::is_x86_feature_detected!("avx2"))
    }

    /// Finds the first C0 control byte (`< 0x20`) using SSE2.
    #[inline]
    pub(crate) fn find_c0_control_sse2(input: &[u8]) -> Option<usize> {
        let len = input.len();
        let ptr = input.as_ptr();
        let mut offset = 0usize;
        while offset + 16 <= len {
            // SAFETY: offset + 16 <= len; SSE2 is baseline on x86_64.
            let found = unsafe {
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

    /// Finds the first byte matching any element of `needles` using SSE2.
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

    /// Finds the first byte outside `0x20..=0x7E` using SSE2.
    #[inline]
    pub(crate) fn find_non_printable_sse2(input: &[u8]) -> Option<usize> {
        let len = input.len();
        let ptr = input.as_ptr();
        let mut offset = 0usize;
        while offset + 16 <= len {
            // SAFETY: offset + 16 <= len; SSE2 is baseline on x86_64.
            let found = unsafe {
                #[allow(clippy::cast_ptr_alignment)]
                let chunk = _mm_loadu_si128(ptr.add(offset).cast::<__m128i>());

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

    /// Finds the first C0 control byte (`< 0x20`) using AVX2.
    ///
    /// # Safety
    /// Caller must ensure AVX2 is available.
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
        find_c0_control_sse2(&input[offset..]).map(|i| offset + i)
    }

    /// Finds the first byte matching any element of `needles` using AVX2.
    ///
    /// # Safety
    /// Caller must ensure AVX2 is available.
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
        find_any_of_sse2(&input[offset..], needles).map(|i| offset + i)
    }

    /// Finds the first byte outside `0x20..=0x7E` using AVX2.
    ///
    /// # Safety
    /// Caller must ensure AVX2 is available.
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(crate) unsafe fn find_non_printable_avx2(input: &[u8]) -> Option<usize> {
        let len = input.len();
        if len == 0 {
            return None;
        }

        let ptr = input.as_ptr();
        let mut offset = 0usize;

        while offset + 32 <= len {
            // SAFETY: offset + 32 <= len; caller guarantees AVX2.
            let found = unsafe {
                #[allow(clippy::cast_ptr_alignment)]
                let chunk = _mm256_loadu_si256(ptr.add(offset).cast::<__m256i>());

                let bias = _mm256_set1_epi8(-128i8);
                let biased = _mm256_add_epi8(chunk, bias);

                let biased_low = _mm256_set1_epi8(-96i8);
                let biased_high = _mm256_set1_epi8(-2i8);

                let too_low = _mm256_cmpgt_epi8(biased_low, biased);
                let too_high = _mm256_cmpgt_epi8(biased, biased_high);

                let outside = _mm256_or_si256(too_low, too_high);

                _mm256_movemask_epi8(outside).cast_unsigned()
            };

            if found != 0 {
                return Some(offset + found.trailing_zeros() as usize);
            }

            offset += 32;
        }

        find_non_printable_sse2(&input[offset..]).map(|i| offset + i)
    }
}

/// aarch64 scans: 16-byte chunks in safe code that LLVM vectorizes to NEON,
/// rescanning only a matching chunk for the exact position.
#[cfg(any(target_arch = "aarch64", test))]
mod arm_simd {
    /// Finds the first C0 control byte (`< 0x20`) via 16-byte chunked reduction.
    #[inline]
    pub(crate) fn find_c0_control_neon(input: &[u8]) -> Option<usize> {
        let (chunks, rem) = input.as_chunks::<16>();
        let mut offset = 0usize;
        for chunk in chunks {
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

    /// Finds the first byte matching any element of `needles` via 16-byte chunked reduction.
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

    /// Finds the first byte outside `0x20..=0x7E` via 16-byte chunked reduction.
    #[inline]
    #[allow(
        clippy::manual_range_contains,
        reason = "the locate-the-index rescan keeps the branch-free `byte < 0x20 || byte > 0x7E` spelling; the RangeInclusive::contains rewrite is not the lowerable form the Trust lane (`ship trust run`) accepts. The CLASSIFIER above it must stay a max-reduce so it lowers to `umaxv.16b` — see the module doc"
    )]
    pub(crate) fn find_non_printable_neon(input: &[u8]) -> Option<usize> {
        let (chunks, rem) = input.as_chunks::<16>();
        let mut offset = 0usize;
        for chunk in chunks {
            let mut worst = 0u8;
            for &byte in chunk {
                let biased = byte.wrapping_sub(0x20);
                worst = if biased > worst { biased } else { worst };
            }
            if worst > 0x5E {
                for (i, &byte) in chunk.iter().enumerate() {
                    if byte < 0x20 || byte > 0x7E {
                        return Some(offset.saturating_add(i));
                    }
                }
            }
            offset = offset.saturating_add(16);
        }
        for (i, &byte) in rem.iter().enumerate() {
            if !(0x20..=0x7E).contains(&byte) {
                return Some(offset.saturating_add(i));
            }
        }
        None
    }
}

/// Find the first non-printable byte with the best tier: AVX2 on x86_64 when
/// available and the input holds a full 32-byte chunk, else SSE2; NEON on
/// aarch64; scalar elsewhere.
#[inline]
#[allow(unreachable_code)]
fn find_non_printable_simd(input: &[u8]) -> Option<usize> {
    match input.first() {
        Some(&b0) if !(0x20..=0x7E).contains(&b0) => return Some(0),
        None => return None,
        _ => {}
    }
    #[cfg(target_arch = "x86_64")]
    {
        // go straight to the inlined SSE2 path with no detection branch.
        if input.len() >= 32 && x86_simd::has_avx2() {
            // SAFETY: AVX2 availability was just checked.
            return unsafe { x86_simd::find_non_printable_avx2(input) };
        }
        return x86_simd::find_non_printable_sse2(input);
    }

    #[cfg(target_arch = "aarch64")]
    {
        return arm_simd::find_non_printable_neon(input);
    }

    find_non_printable_scalar(input)
}

/// The scalar reference and fallback.
#[inline]
fn find_non_printable_scalar(input: &[u8]) -> Option<usize> {
    input.iter().position(|&b| !(0x20..=0x7E).contains(&b))
}

/// Finds the first byte outside printable ASCII (`0x20..=0x7E`).
#[inline]
pub(super) fn find_non_printable(input: &[u8]) -> Option<usize> {
    find_non_printable_simd(input)
}

/// Returns the length of `input`'s printable-ASCII prefix.
#[inline]
pub(crate) fn count_printable(input: &[u8]) -> usize {
    find_non_printable(input).unwrap_or(input.len())
}

/// Splits `input` into its printable-ASCII prefix and remainder.
#[inline]
pub(crate) fn take_printable(input: &[u8]) -> (&[u8], &[u8]) {
    let n = count_printable(input);
    input.split_at_checked(n).unwrap_or((input, &[]))
}

/// Finds the first C0 control byte (`< 0x20`).
#[inline]
pub(crate) fn find_c0_control(input: &[u8]) -> Option<usize> {
    find_c0_control_simd(input)
}

#[inline]
#[allow(unreachable_code)]
fn find_c0_control_simd(input: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if input.len() >= 32 && x86_simd::has_avx2() {
            // SAFETY: AVX2 availability was just checked.
            return unsafe { x86_simd::find_c0_control_avx2(input) };
        }
        return x86_simd::find_c0_control_sse2(input);
    }

    #[cfg(target_arch = "aarch64")]
    {
        return arm_simd::find_c0_control_neon(input);
    }

    find_c0_control_scalar(input)
}

#[inline]
fn find_c0_control_scalar(input: &[u8]) -> Option<usize> {
    input.iter().position(|&b| b < 0x20)
}

/// Finds the first byte in `input` equal to any byte in `needles`.
#[inline]
pub(crate) fn find_any_of<const N: usize>(input: &[u8], needles: [u8; N]) -> Option<usize> {
    find_any_of_simd(input, needles)
}

#[inline]
#[allow(unreachable_code)]
fn find_any_of_simd<const N: usize>(input: &[u8], needles: [u8; N]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if input.len() >= 32 && x86_simd::has_avx2() {
            // SAFETY: AVX2 availability was just checked.
            return unsafe { x86_simd::find_any_of_avx2(input, needles) };
        }
        return x86_simd::find_any_of_sse2(input, needles);
    }

    #[cfg(target_arch = "aarch64")]
    {
        return arm_simd::find_any_of_neon(input, needles);
    }

    find_any_of_scalar(input, needles)
}

#[inline]
fn find_any_of_scalar<const N: usize>(input: &[u8], needles: [u8; N]) -> Option<usize> {
    input.iter().position(|&b| needles.contains(&b))
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

    #[test]
    fn test_find_c0_control_boundary() {
        assert_eq!(find_c0_control(b"\x1F"), Some(0));
        assert_eq!(find_c0_control(b"\x20"), None);
        assert_eq!(find_c0_control(b"\x7E\x7F\x80\x9F\xFF"), None);
    }

    const DCS_TERMINATORS: [u8; 4] = [0x18, 0x1A, 0x1B, 0x9C];
    const DCS_PASSTHROUGH: [u8; 5] = [0x18, 0x1A, 0x1B, 0x9C, 0x7F];

    #[test]
    fn test_find_any_of_pure_data() {
        assert_eq!(find_any_of(b"Hello, World!", DCS_TERMINATORS), None);
        assert_eq!(find_any_of(b"\x80\x90\xA0\xFF", DCS_TERMINATORS), None);
        assert_eq!(find_any_of(b"abc\x7Fdef", DCS_TERMINATORS), None);
        assert_eq!(find_any_of(b"abc\x7Fdef", DCS_PASSTHROUGH), Some(3));
    }

    #[test]
    fn test_find_any_of_at_start() {
        assert_eq!(find_any_of(b"\x18hello", DCS_TERMINATORS), Some(0));
        assert_eq!(find_any_of(b"\x1Ahello", DCS_TERMINATORS), Some(0));
        assert_eq!(find_any_of(b"\x1Bhello", DCS_TERMINATORS), Some(0));
        assert_eq!(find_any_of(b"\x9Chello", DCS_TERMINATORS), Some(0));
    }

    #[test]
    fn test_tiers_exhaustive_bytes_and_positions() {
        #[cfg(target_arch = "x86_64")]
        let avx2 = x86_simd::has_avx2();
        for size in [1usize, 7, 8, 15, 16, 17, 31, 32, 33, 47, 48, 63, 64, 65] {
            for pos in 0..size {
                for byte in 0u8..=255 {
                    let mut data = vec![b'A'; size];
                    data[pos] = byte;

                    let np = find_non_printable_scalar(&data);
                    let c0 = find_c0_control_scalar(&data);
                    let any = find_any_of_scalar(&data, DCS_PASSTHROUGH);

                    assert_eq!(
                        find_non_printable_simd(&data),
                        np,
                        "dispatch non_printable size {size} pos {pos} byte 0x{byte:02X}"
                    );
                    assert_eq!(
                        find_c0_control_simd(&data),
                        c0,
                        "dispatch c0_control size {size} pos {pos} byte 0x{byte:02X}"
                    );
                    assert_eq!(
                        find_any_of_simd(&data, DCS_PASSTHROUGH),
                        any,
                        "dispatch any_of size {size} pos {pos} byte 0x{byte:02X}"
                    );

                    assert_eq!(
                        arm_simd::find_non_printable_neon(&data),
                        np,
                        "neon non_printable size {size} pos {pos} byte 0x{byte:02X}"
                    );
                    assert_eq!(
                        arm_simd::find_c0_control_neon(&data),
                        c0,
                        "neon c0_control size {size} pos {pos} byte 0x{byte:02X}"
                    );
                    assert_eq!(
                        arm_simd::find_any_of_neon(&data, DCS_PASSTHROUGH),
                        any,
                        "neon any_of size {size} pos {pos} byte 0x{byte:02X}"
                    );

                    #[cfg(target_arch = "x86_64")]
                    {
                        assert_eq!(
                            x86_simd::find_non_printable_sse2(&data),
                            np,
                            "sse2 non_printable size {size} pos {pos} byte 0x{byte:02X}"
                        );
                        assert_eq!(
                            x86_simd::find_c0_control_sse2(&data),
                            c0,
                            "sse2 c0_control size {size} pos {pos} byte 0x{byte:02X}"
                        );
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
    }

    #[test]
    fn test_tiers_random_buffers() {
        #[cfg(target_arch = "x86_64")]
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
                        // arbitrary byte (may be a match)
                        (r >> 8) as u8
                    } else {
                        // printable 0x20-0x7E
                        0x20 + ((r >> 8) % 0x5F) as u8
                    }
                })
                .collect();

            let np = find_non_printable_scalar(&data);
            let c0 = find_c0_control_scalar(&data);
            let any = find_any_of_scalar(&data, DCS_PASSTHROUGH);

            assert_eq!(
                find_non_printable_simd(&data),
                np,
                "dispatch non_printable {data:02X?}"
            );
            assert_eq!(
                find_c0_control_simd(&data),
                c0,
                "dispatch c0_control {data:02X?}"
            );
            assert_eq!(
                find_any_of_simd(&data, DCS_PASSTHROUGH),
                any,
                "dispatch any_of {data:02X?}"
            );

            assert_eq!(
                arm_simd::find_non_printable_neon(&data),
                np,
                "neon non_printable {data:02X?}"
            );
            assert_eq!(
                arm_simd::find_c0_control_neon(&data),
                c0,
                "neon c0_control {data:02X?}"
            );
            assert_eq!(
                arm_simd::find_any_of_neon(&data, DCS_PASSTHROUGH),
                any,
                "neon any_of {data:02X?}"
            );

            #[cfg(target_arch = "x86_64")]
            {
                assert_eq!(x86_simd::find_non_printable_sse2(&data), np);
                assert_eq!(x86_simd::find_c0_control_sse2(&data), c0);
                assert_eq!(x86_simd::find_any_of_sse2(&data, DCS_PASSTHROUGH), any);

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
}
