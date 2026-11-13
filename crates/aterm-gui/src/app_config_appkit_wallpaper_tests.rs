// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::decode_wallpaper_appkit;

fn two_by_two_bmp() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"BM");
    v.extend_from_slice(&70_u32.to_le_bytes());
    v.extend_from_slice(&0_u32.to_le_bytes());
    v.extend_from_slice(&54_u32.to_le_bytes());
    v.extend_from_slice(&40_u32.to_le_bytes());
    v.extend_from_slice(&2_i32.to_le_bytes());
    v.extend_from_slice(&2_i32.to_le_bytes());
    v.extend_from_slice(&1_u16.to_le_bytes());
    v.extend_from_slice(&24_u16.to_le_bytes());
    v.extend_from_slice(&0_u32.to_le_bytes());
    v.extend_from_slice(&16_u32.to_le_bytes());
    v.extend_from_slice(&2835_i32.to_le_bytes());
    v.extend_from_slice(&2835_i32.to_le_bytes());
    v.extend_from_slice(&0_u32.to_le_bytes());
    v.extend_from_slice(&0_u32.to_le_bytes());
    v.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]);
    v.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
    v
}

fn dominant(px: &[u8]) -> (usize, u8) {
    let (mut best, mut best_v) = (0, px[0]);
    for (i, &v) in px.iter().take(3).enumerate() {
        if v > best_v {
            best = i;
            best_v = v;
        }
    }
    (best, px[3])
}

#[test]
fn appkit_decodes_a_bmp_with_the_right_geometry_and_channel_order() {
    let (rgba, w, h) = decode_wallpaper_appkit(&two_by_two_bmp()).expect("AppKit decoded the BMP");
    assert_eq!((w, h), (2, 2));
    assert_eq!(rgba.len(), 2 * 2 * 4);
    let px = |i: usize| &rgba[i * 4..i * 4 + 4];
    assert_eq!(dominant(px(0)).0, 0, "top-left is red");
    assert_eq!(dominant(px(1)).0, 1, "top-right is green");
    assert_eq!(dominant(px(2)).0, 2, "bottom-left is blue");
    for i in 0..4 {
        assert_eq!(px(i)[3], 255, "pixel {i} lost its alpha");
    }
    let white = px(3);
    assert!(
        white[0] > 200 && white[1] > 200 && white[2] > 200,
        "bottom-right should be near-white, got {white:?}"
    );
}
