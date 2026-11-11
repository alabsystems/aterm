// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn cursor_style_from_decscusr_param() {
    assert_eq!(CursorStyle::from_param(0), Some(CursorStyle::BlinkingBlock));
    assert_eq!(CursorStyle::from_param(1), Some(CursorStyle::BlinkingBlock));
    assert_eq!(CursorStyle::from_param(2), Some(CursorStyle::SteadyBlock));
    assert_eq!(
        CursorStyle::from_param(3),
        Some(CursorStyle::BlinkingUnderline)
    );
    assert_eq!(
        CursorStyle::from_param(4),
        Some(CursorStyle::SteadyUnderline)
    );
    assert_eq!(CursorStyle::from_param(5), Some(CursorStyle::BlinkingBar));
    assert_eq!(CursorStyle::from_param(6), Some(CursorStyle::SteadyBar));
    for invalid in [7, 255, u16::MAX] {
        assert_eq!(CursorStyle::from_param(invalid), None, "{invalid}");
    }
}

#[test]
fn luminance_direct_and_srgb_seam_continuity() {
    assert_eq!(Rgb::new(0, 0, 0).luminance(), 0.0);
    assert!((Rgb::new(255, 255, 255).luminance() - 1.0).abs() <= f64::EPSILON);

    let seam = 0.04045_f64;
    let low_branch = seam / 12.92;
    let high_branch = ((seam + 0.055) / 1.055).powf(2.4);
    assert!(
        (low_branch - high_branch).abs() < 1e-6,
        "IEC 61966-2-1 seam branches agree within 1e-6: {low_branch} vs {high_branch}"
    );
    let below = Rgb::new(10, 10, 10).luminance();
    let above = Rgb::new(11, 11, 11).luminance();
    assert!(
        below < low_branch && low_branch < above,
        "{below} < {low_branch} < {above}"
    );
}
