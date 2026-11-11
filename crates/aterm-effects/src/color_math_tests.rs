// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The page floor clears the ratio on both poles, moves lightness alone
/// (the HSV hue of the floored colour is the input's), and leaves a
/// colour that already clears it byte-identical.
#[test]
fn the_page_floor_clears_the_ratio_and_keeps_the_hue() {
    for (rgb, ground) in [
        (0x0051_0A00, 0x0011_1318),
        (0x000F_70B7, 0x0011_1318),
        (0x00B4_8CFF, 0x00FF_FFFF),
        (0x00FF_D9A0, 0x00FF_FFFF),
        (0x009E_D6FF, 0x00FD_F6E3),
        (0x0000_00FF, 0x0011_1318),
    ] {
        let before = contrast_ratio(rgb, ground);
        assert!(before < 4.5, "control: {rgb:06x} starts under the floor");
        let floored = floor_contrast_keeping_hue(rgb, ground, 4.5);
        assert!(
            contrast_ratio(floored, ground) >= 4.5,
            "{rgb:06x} on {ground:06x} floored to {floored:06x} at {:.2}:1",
            contrast_ratio(floored, ground)
        );
        let (h0, s0, _) = rgb2hsv(rgb);
        let (h1, _, _) = rgb2hsv(floored);
        let dh = (h0 - h1).abs().min(360.0 - (h0 - h1).abs());
        assert!(
            s0 < 0.05 || dh < 4.0,
            "{rgb:06x} → {floored:06x}: the hue moved {dh:.1}°"
        );
    }
    for (rgb, ground) in [(0x0009_69DA, 0x00FF_FFFF), (0x00FF_FFFF, 0x0011_1318)] {
        assert_eq!(floor_contrast_keeping_hue(rgb, ground, 4.5), rgb);
    }
    assert_eq!(
        floor_contrast_keeping_hue(0x0011_1318, 0x0011_1318, 1.0),
        0x0011_1318
    );
    assert_eq!(
        floor_contrast_keeping_hue(0x0011_1318, 0x0011_1318, f32::NAN),
        0x0011_1318
    );
}

/// The lift is the page floor's lightening path held to any test: on a near-black ground,
/// where the floor lightens, lifting to the floor's own ratio gives the floor's colour, and a
/// colour that already passes is kept. NEGATIVE CONTROL: on a mid grey the floor darkens the
/// blue while the lift lightens it past the grey, so the two are not one path everywhere.
#[test]
fn the_lift_is_the_floors_lightening_path() {
    let night = 0x0011_1318;
    for rgb in [0x0051_0A00, 0x0000_00FF, 0x004B_0082, 0x0094_00D3] {
        let floored = floor_contrast_keeping_hue(rgb, night, 4.5);
        assert_ne!(floored, rgb, "CONTROL: {rgb:06x} starts under the floor");
        assert_eq!(
            lift_keeping_hue(rgb, |c| contrast_ratio(c, night) >= 4.5),
            floored,
            "{rgb:06x} on {night:06x}"
        );
    }
    assert_eq!(lift_keeping_hue(0x0000_00FF, |_| true), 0x0000_00FF);
    let grey = 0x0092_9292;
    let lifted = lift_keeping_hue(0x0000_00FF, |c| contrast_ratio(c, grey) >= 3.0);
    assert!(
        lifted != floor_contrast_keeping_hue(0x0000_00FF, grey, 3.0)
            && relative_luminance(lifted) > relative_luminance(grey),
        "NEGATIVE CONTROL: on {grey:06x} the lift lightens the blue to {lifted:06x}"
    );
}

/// The light-page body fill deepens with heat (lower luminance, higher
/// saturation, the same hue), and a grey stays grey.
#[test]
fn the_light_body_fill_concentrates_the_hue_as_it_heats() {
    for rgb in [0x00B4_8CFF, 0x0009_69DA, 0x009E_D6FF, 0x00FF_D9A0] {
        let (h0, s0, _) = rgb2hsv(rgb);
        let rest = light_body_fill(rgb, 0.0);
        let hot = light_body_fill(rgb, 1.0);
        let (hr, sr, _) = rgb2hsv(rest);
        let (hh, sh, _) = rgb2hsv(hot);
        assert!(sr >= s0 && sh >= sr, "{rgb:06x}: saturation climbs");
        assert!(
            relative_luminance(hot) < relative_luminance(rest),
            "{rgb:06x}: heat deepens the fill ({rest:06x} → {hot:06x})"
        );
        for h in [hr, hh] {
            let dh = (h0 - h).abs().min(360.0 - (h0 - h).abs());
            assert!(dh < 4.0, "{rgb:06x}: the hue moved {dh:.1}°");
        }
    }
    let grey = light_body_fill(0x0080_8080, 0.5);
    let (_, s, _) = rgb2hsv(grey);
    assert!(s < 0.02, "a grey stays grey, got {grey:06x}");
}

#[test]
fn srgb_linear_table_matches_powf_within_one_ulp() {
    let table = relative_luminance_table();
    for (i, &got) in table.linear.iter().enumerate() {
        let n = i as f32 / 255.0;
        let expected = if n <= 0.03928 {
            n / 12.92
        } else {
            ((n + 0.055) / 1.055).powf(2.4)
        };
        let ulp = got.to_bits().abs_diff(expected.to_bits());
        assert!(
            ulp <= 1,
            "channel byte {i}: got {:08x} ({}), expected {:08x} ({expected}), ulp={ulp}",
            got.to_bits(),
            got,
            expected.to_bits(),
        );
    }
}
