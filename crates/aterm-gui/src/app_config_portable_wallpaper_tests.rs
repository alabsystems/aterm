// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{decode_wallpaper_bytes, decode_wallpaper_portable, wallpaper_image_filter};

const QUADRANTS: &[u8] = include_bytes!("../../aterm-jpeg/tests/fixtures/b420.jpg");
const PROGRESSIVE: &[u8] = include_bytes!("../../aterm-jpeg/tests/fixtures/p420.jpg");

fn dominant(rgba: &[u8], width: usize, x: usize, y: usize) -> char {
    let i = (y * width + x) * 4;
    let (r, g, b) = (rgba[i], rgba[i + 1], rgba[i + 2]);
    if r > g && r > b {
        'r'
    } else if g > r && g > b {
        'g'
    } else {
        'b'
    }
}

#[test]
fn wallpaper_decodes_a_jpeg_on_every_platform() {
    for bytes in [QUADRANTS, PROGRESSIVE] {
        for (path, decoded) in [
            (
                "portable",
                decode_wallpaper_portable(bytes).ok_or("refused"),
            ),
            (
                "platform",
                decode_wallpaper_bytes(bytes).map_err(|_| "refused"),
            ),
        ] {
            let (rgba, width, height) = decoded.unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!((width, height), (33, 21), "{path}");
            assert_eq!(rgba.len(), 33 * 21 * 4, "{path}");
            assert_eq!(dominant(&rgba, width, 4, 4), 'r', "{path}: top left");
            assert_eq!(dominant(&rgba, width, 28, 4), 'g', "{path}: top right");
            assert_eq!(dominant(&rgba, width, 4, 17), 'b', "{path}: bottom left");
        }
    }
    assert!(decode_wallpaper_portable(b"GIF89a....").is_none());
    assert!(decode_wallpaper_bytes(b"not an image").is_err());
}

#[test]
fn the_image_picker_row_lists_what_the_decoder_reads() {
    let (name, patterns) = wallpaper_image_filter(&[]);
    assert_eq!(name, "Images");
    assert_eq!(patterns, "*.png;*.jpg;*.jpeg;*.jpe;*.jfif");
    let platform = ["bmp", "JPG", "heic", "bmp"].map(str::to_string);
    let (_, patterns) = wallpaper_image_filter(&platform);
    assert_eq!(patterns, "*.png;*.jpg;*.jpeg;*.jpe;*.jfif;*.bmp;*.heic");
}
