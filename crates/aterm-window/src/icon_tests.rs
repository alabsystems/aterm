// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/icon.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[test]
fn rgba_icon_rejects_overflowing_or_mismatched_dimensions() {
    assert!(matches!(
        RgbaIcon::from_rgba(Vec::new(), 65_536, 65_536),
        Err(BadIcon::DimensionsVsPixelCount { width: 65_536, height: 65_536, pixel_count: 0, .. })
    ));
    assert!(matches!(
        RgbaIcon::from_rgba(vec![0; 4], u32::MAX, u32::MAX),
        Err(BadIcon::DimensionsVsPixelCount {
            width: u32::MAX,
            height: u32::MAX,
            pixel_count: 1,
            ..
        })
    ));
    assert!(RgbaIcon::from_rgba(vec![0; 16], 2, 2).is_ok());
}
