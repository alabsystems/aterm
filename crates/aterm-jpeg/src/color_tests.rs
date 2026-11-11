// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ColorModel, convert_row, orient, ycbcr_to_rgb};

#[test]
fn ycbcr_converts_as_jfif_defines() {
    assert_eq!(ycbcr_to_rgb(100, 128, 128), [100, 100, 100]);
    let red = ycbcr_to_rgb(76, 85, 255);
    assert!(red[0] > 250 && red[1] < 5 && red[2] < 5, "{red:?}");
    let blue = ycbcr_to_rgb(29, 255, 107);
    assert!(blue[2] > 250 && blue[0] < 5 && blue[1] < 5, "{blue:?}");
}

#[test]
fn adobe_cmyk_is_read_inverted() {
    let rows = vec![vec![255], vec![255], vec![0], vec![255]];
    let mut out = [0u8; 4];
    convert_row(ColorModel::Cmyk, &rows, 1, &mut out);
    assert_eq!(out, [255, 255, 0, 255], "full yellow ink");
}

#[test]
fn orientation_six_turns_the_image_clockwise() {
    let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255];
    let (out, w, h) = orient(rgba, 2, 1, 6);
    assert_eq!((w, h), (1, 2));
    assert_eq!(&out[..4], &[255, 0, 0, 255], "red on top");
    assert_eq!(&out[4..], &[0, 255, 0, 255], "green below");
}
