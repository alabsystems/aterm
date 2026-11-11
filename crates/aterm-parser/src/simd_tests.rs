// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
