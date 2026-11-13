// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{bgra_to_rgba, decodable_extensions, decode_rgba8, split_extensions};

#[test]
fn a_codecs_extensions_are_bare_and_lower_case() {
    assert_eq!(
        split_extensions(".bmp,.DIB, .rle,,.bmp"),
        ["bmp", "dib", "rle"]
    );
    assert!(split_extensions("").is_empty());
}

#[test]
fn bgra_becomes_rgba() {
    let mut pixels = [1, 2, 3, 4, 5, 6, 7, 8];
    bgra_to_rgba(&mut pixels);
    assert_eq!(pixels, [3, 2, 1, 4, 7, 6, 5, 8]);
}

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

#[test]
fn wic_decodes_a_bmp_top_down_in_rgba() {
    let (rgba, w, h) = decode_rgba8(&two_by_two_bmp(), 8192).expect("WIC read the BMP");
    assert_eq!((w, h), (2, 2));
    assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "top left is red");
    assert_eq!(&rgba[4..8], &[0, 255, 0, 255], "top right is green");
    assert_eq!(&rgba[8..12], &[0, 0, 255, 255], "bottom left is blue");
    assert_eq!(
        &rgba[12..16],
        &[255, 255, 255, 255],
        "bottom right is white"
    );
    assert!(decodable_extensions().iter().any(|e| e == "bmp"));
    let refused = decode_rgba8(&two_by_two_bmp(), 1).expect_err("past the bound");
    assert!(refused.contains("larger than 1 px"), "{refused}");
    let refused = decode_rgba8(b"not an image", 8192).expect_err("no image");
    assert!(
        refused.starts_with("not an image Windows can decode"),
        "{refused}"
    );
}
