// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{clampf, mix_rgb, smoothstep};

/// NEGATIVE CONTROL: `clampf` propagates `NaN` as documented while `mix_rgb` treats `NaN`
/// and negative `t` as `0.0` (preserving `a & 0x00ff_ffff` instead of collapsing channels to
/// `0x000000`) and masks high alpha bytes from both packed inputs.
#[test]
fn clampf_smoothstep_and_mix_rgb_handle_bounds_and_nan() {
    assert_eq!(clampf(-2.0, 0.0, 1.0), 0.0);
    assert_eq!(clampf(0.25, 0.0, 1.0), 0.25);
    assert_eq!(clampf(3.0, 0.0, 1.0), 1.0);
    assert_eq!(clampf(0.5, 2.0, 1.0), 2.0);
    assert!(clampf(f32::NAN, 0.0, 1.0).is_nan());

    assert_eq!(smoothstep(-1.0), 0.0);
    assert_eq!(smoothstep(0.0), 0.0);
    assert_eq!(smoothstep(0.5), 0.5);
    assert_eq!(smoothstep(1.0), 1.0);
    assert_eq!(smoothstep(2.0), 1.0);
    assert!(smoothstep(f32::NAN).is_nan());

    let a = 0xFF20_4080;
    let b = 0xAA60_C000;
    assert_eq!(mix_rgb(a, b, 0.0), 0x0020_4080);
    assert_eq!(mix_rgb(a, b, -5.0), 0x0020_4080);
    assert_eq!(mix_rgb(a, b, f32::NEG_INFINITY), 0x0020_4080);
    assert_eq!(mix_rgb(a, b, f32::NAN), 0x0020_4080);
    assert_eq!(mix_rgb(a, b, 1.0), 0x0060_C000);
    assert_eq!(mix_rgb(a, b, 5.0), 0x0060_C000);
    assert_eq!(mix_rgb(a, b, f32::INFINITY), 0x0060_C000);
    assert_eq!(mix_rgb(a, b, 0.5), 0x0040_8040);
}
