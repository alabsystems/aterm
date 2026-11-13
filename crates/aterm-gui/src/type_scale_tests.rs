// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn px_scales_linearly_and_clamps() {
    assert_eq!(TypeStep::Caption.px(10.0).get(), 8.0);
    assert_eq!(TypeStep::Display.px(10.0).get(), 16.0);
    // Clamp binds on both sides; inverted bounds pin to hi.max(0).
    assert_eq!(TypeStep::Caption.px_clamped(10.0, 9.0, 12.0).get(), 9.0);
    assert_eq!(TypeStep::Display.px_clamped(10.0, 2.0, 12.0).get(), 12.0);
    assert_eq!(TypeStep::Body.px_clamped(10.0, 8.0, 4.0).get(), 4.0);
    assert_eq!(TypeStep::Body.px_clamped(10.0, 8.0, -3.0).get(), 0.0);
    assert_eq!(TypeStep::Body.px(10.0).scaled(1.5).get(), 15.0);
    assert_eq!(TypeStep::Body.px(10.0).scaled(f32::NAN).get(), 10.0);
}
