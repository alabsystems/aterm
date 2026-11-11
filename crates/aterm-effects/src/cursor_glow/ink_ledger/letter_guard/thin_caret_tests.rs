// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn lum(c: u32) -> f32 {
    crate::color_math::relative_luminance_with(crate::color_math::relative_luminance_table(), c)
}

/// A caret's colour along its path keeps its hue while it lightens (dark) or darkens
/// (paper), each step at least as far from the page as the last, and ends at white or
/// black; a fill that reads already is left as it is.
#[test]
fn a_thin_caret_moves_in_lightness_alone() {
    for (fill, page) in [
        (0x0050_FA7B, 0x0011_1318),
        (0x0009_69DA, 0x00FF_FFFF),
        (0x00E8_6D18, 0x0011_1318),
    ] {
        let dark = aterm_render::theme_is_dark(page);
        let (h0, _, _) = crate::color_math::rgb2hsv(fill);
        let mut last = lum(fill);
        for k in 1..=40 {
            let c = lifted(fill, dark, 2.0 * k as f32 / 40.0);
            let l = lum(c);
            assert!(
                if dark {
                    l >= last - 1e-4
                } else {
                    l <= last + 1e-4
                },
                "{fill:06x} step {k}: {c:06x}"
            );
            last = l;
            let (h, s, _) = crate::color_math::rgb2hsv(c);
            if s > 0.15 {
                let d = (h - h0).abs();
                assert!(
                    d.min(360.0 - d) <= 6.0,
                    "{fill:06x} step {k}: hue {h:.0} of {h0:.0} ({c:06x})"
                );
            }
        }
        assert_eq!(lifted(fill, dark, 2.0), if dark { 0x00FF_FFFF } else { 0 });
    }
    assert_eq!(
        lifted_to(0x0050_FA7B, 0x0011_1318, 0x0011_1318, 2.0),
        Some(0x0050_FA7B)
    );
    // The theme's bar on a pink ribbon's paper tint is darkened to read it.
    let c = lifted_to(0x0009_69DA, 0x00FF_FFFF, 0x00EE_BADD, 2.0).expect("darkened, it reads");
    assert!(
        crate::color_math::contrast_ratio(c, 0x00EE_BADD) >= THIN_CARET_MIN,
        "{c:06x}"
    );
    // White on Laser's lavender head reads under the bar: no lift does it.
    assert_eq!(lifted_to(0x0050_FA7B, 0x0011_1318, 0x00CC_B3FF, 2.0), None);
}
