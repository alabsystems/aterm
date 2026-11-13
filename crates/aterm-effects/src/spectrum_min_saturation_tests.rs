// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn hsv(rgb: u32) -> (f64, f64, f64) {
    spectrum_hsv(rgb)
}

/// Five desaturated witnesses around the hue circle reach the floor with
/// hue and value untouched.
#[test]
fn the_floor_is_reached_with_hue_and_value_untouched() {
    for (i, &under) in [
        0x006F_EBC2u32,
        0x006F_B1EB,
        0x00C0_8080,
        0x0080_C0A0,
        0x009A_9AD0,
    ]
    .iter()
    .enumerate()
    {
        let (h0, s0, v0) = hsv(under);
        assert!(
            s0 < 0.60,
            "witness {i} is not under S 0.60 ({s0:.3}) — this pin needs a colour the floor can move"
        );
        for floor in [0.65f32, 0.80, 0.90, 1.0] {
            let got = spectrum_with_min_saturation(under, floor);
            let (h1, s1, v1) = hsv(got);
            assert!(
                (h1 - h0).abs() < 1.0,
                "witness {i} at floor {floor}: hue moved {h0:.1}° → {h1:.1}° (#{got:06X})"
            );
            assert_eq!(
                (v1 * 255.0).round() as u32,
                (v0 * 255.0).round() as u32,
                "witness {i} at floor {floor}: value moved"
            );
            assert!(
                (s1 - f64::from(floor)).abs() <= 1.5 / 255.0 * 2.0,
                "witness {i} at floor {floor}: S landed at {s1:.3}, not on the floor"
            );
        }
    }
    assert_eq!(spectrum_with_min_saturation(0x006F_B1EB, 1.0), 0x0000_7DEB);
}

#[test]
fn a_colour_at_or_over_the_floor_and_every_grey_is_returned_bit_for_bit() {
    for &anchor in &SPECTRUM_ANCHORS {
        assert_eq!(
            spectrum_with_min_saturation(anchor, 1.0),
            anchor,
            "every anchor has a zero channel"
        );
    }
    for grey in [0u32, 0x0080_8080, 0x00FF_FFFF, 0x0011_1111] {
        assert_eq!(
            spectrum_with_min_saturation(grey, 1.0),
            grey,
            "a grey has no hue to keep"
        );
    }
    assert_eq!(
        spectrum_with_min_saturation(0x006F_EBC2, 0.50),
        0x006F_EBC2,
        "S 0.53 is over a 0.50 floor"
    );
    assert_eq!(
        spectrum_with_min_saturation(0x006F_EBC2, f32::NAN),
        0x006F_EBC2,
        "a non-finite floor is no floor"
    );
    assert_eq!(spectrum_with_min_saturation(0x006F_EBC2, 0.0), 0x006F_EBC2);
}
