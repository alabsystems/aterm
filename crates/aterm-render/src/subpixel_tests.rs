// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::hinted::{HintBank, unicode_gid};

fn dejavu() -> &'static [u8] {
    crate::embedded_font()
}

fn lcd_instance(px: f32) -> std::sync::Arc<HintingInstance> {
    let bytes = dejavu();
    HintBank::default()
        .instance_with(
            bytes.as_ptr() as usize,
            bytes,
            0,
            px,
            Some(lcd_hint_options()),
            &[],
            0,
        )
        .expect("LCD instance builds for the embedded face")
}

/// A space has no outline: the raster declines (`None`).
#[test]
fn space_declines() {
    let hint = lcd_instance(12.0);
    let gid = unicode_gid(dejavu(), 0, ' ').unwrap();
    assert!(subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), false).is_none());
}

/// Stem edges carry per-channel structure (some texel has R != B), where a grayscale
/// raster is R == G == B by construction.
#[test]
fn stem_edges_carry_channel_structure() {
    let hint = lcd_instance(15.0);
    let gid = unicode_gid(dejavu(), 0, 'l').unwrap();
    let (w, h, .., bytes) =
        subpixel_glyph_raster(dejavu(), 0, gid, 15.0, Some(&hint), false).unwrap();
    let fringed = (0..w * h)
        .filter(|i| {
            let (r, b) = (bytes[3 * i], bytes[3 * i + 2]);
            r.abs_diff(b) > 16
        })
        .count();
    assert!(
        fringed > 0,
        "an 'l' stem at 15px must have R!=B fringe texels ({w}x{h}: {bytes:?})"
    );
}

/// Solid interior texels remain 255 across all three channels after FIR5 filtering.
#[test]
fn solid_interior_is_full_in_every_channel() {
    let hint = lcd_instance(32.0);
    let gid = unicode_gid(dejavu(), 0, '0').unwrap();
    let (w, h, .., bytes) =
        subpixel_glyph_raster(dejavu(), 0, gid, 32.0, Some(&hint), false).unwrap();
    let solid = (0..w * h)
        .filter(|i| bytes[3 * i..3 * i + 3].iter().all(|&c| c == 255))
        .count();
    assert!(
        solid > 0,
        "'0' at 32px must keep fully-covered texels ({w}x{h})"
    );
}

/// BGR swaps the R and B channels of the RGB coverage.
#[test]
fn bgr_swaps_r_and_b() {
    let hint = lcd_instance(12.0);
    let gid = unicode_gid(dejavu(), 0, 'g').unwrap();
    let rgb = subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), false).unwrap();
    let bgr = subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), true).unwrap();
    assert_eq!((rgb.0, rgb.1, rgb.2, rgb.3), (bgr.0, bgr.1, bgr.2, bgr.3));
    for i in 0..rgb.0 * rgb.1 {
        assert_eq!(rgb.4[3 * i], bgr.4[3 * i + 2]);
        assert_eq!(rgb.4[3 * i + 1], bgr.4[3 * i + 1]);
        assert_eq!(rgb.4[3 * i + 2], bgr.4[3 * i]);
    }
}

/// Unhinted LCD total ink matches the grayscale unhinted outline's ink within rounding.
#[test]
fn unhinted_subpixel_ink_is_the_unhinted_outline() {
    for px in [12.0f32, 24.0] {
        for ch in ['x', 'M', '0', 'g', '@', 'i', '.', '_'] {
            let gid = unicode_gid(dejavu(), 0, ch).unwrap();
            let (.., lcd) = subpixel_glyph_raster(dejavu(), 0, gid, px, None, false).unwrap();
            let (.., gray) =
                crate::variation::varied_glyph_raster(dejavu(), 0, &[], gid, px).unwrap();
            let lcd_ink = lcd.iter().map(|&v| f32::from(v)).sum::<f32>() / (3.0 * 255.0);
            let gray_ink = gray.iter().map(|&v| f32::from(v)).sum::<f32>() / 255.0;
            assert!(
                (lcd_ink - gray_ink).abs() <= 0.01 * gray_ink + 0.05,
                "{ch:?} @{px}px: unhinted LCD ink {lcd_ink} vs grayscale {gray_ink}"
            );
        }
    }
}

/// Spelling table: env/config parse and the canonical round-trip.
#[test]
fn mode_spellings() {
    use SubpixelMode::*;
    for (s, m) in [
        ("rgb", Rgb),
        ("on", Rgb),
        ("1", Rgb),
        ("true", Rgb),
        ("bgr", Bgr),
        ("off", Off),
        ("0", Off),
        ("", Off),
        ("vrgb", Off), // vertical geometries are NOT stage 1
        ("anything", Off),
    ] {
        assert_eq!(SubpixelMode::parse(s), m, "spelling {s:?}");
    }
    assert_eq!(Rgb.as_str(), "rgb");
    assert_eq!(Bgr.as_str(), "bgr");
    assert_eq!(Off.as_str(), "off");
}
