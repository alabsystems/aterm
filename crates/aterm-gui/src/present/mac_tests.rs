// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{BITMAP_INFO, BITS_PER_COMPONENT, BITS_PER_PIXEL, BYTES_PER_PIXEL, release_pixel_box};

#[test]
fn bitmap_descriptor_matches_the_retired_backend() {
    let none_skip_first = 6_u32;
    let integer_components = 0_u32;
    let order_32_little = 2_u32 << 12;
    let packed = 0_u32;
    assert_eq!(
        BITMAP_INFO,
        none_skip_first | integer_components | order_32_little | packed,
    );
    assert_eq!(BITS_PER_COMPONENT * 4, BITS_PER_PIXEL);
    assert_eq!(BITS_PER_PIXEL / 8, BYTES_PER_PIXEL);
}

#[test]
fn release_callback_reclaims_the_box_and_ignores_null() {
    // SAFETY: the documented degenerate input: the callback must not touch a NULL `data`.
    unsafe { release_pixel_box(std::ptr::null_mut(), std::ptr::null(), 64) };

    let pixels = vec![0x0011_2233_u32; 32];
    let byte_len = pixels.len() * std::mem::size_of::<u32>();
    let raw: *mut [u32] = Box::into_raw(pixels.into_boxed_slice());
    // SAFETY: exactly the pairing `commit` sets up (the thin pointer and byte length of one
    // leaked `Box<[u32]>`), passed once.
    unsafe { release_pixel_box(std::ptr::null_mut(), raw.cast(), byte_len) };
}
