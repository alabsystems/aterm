// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::theme_is_dark;
use aterm_messages::ink::bg_is_light;

fn sweep() -> impl Iterator<Item = [u8; 3]> {
    let greys = (0..=255u8).map(|v| [v, v, v]);
    let lattice = (0..=255u8).step_by(15).flat_map(|r| {
        (0..=255u8)
            .step_by(15)
            .flat_map(move |g| (0..=255u8).step_by(15).map(move |b| [r, g, b]))
    });
    greys.chain(lattice)
}

fn pack([r, g, b]: [u8; 3]) -> u32 {
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

#[test]
fn theme_is_dark_is_the_chrome_classifier_inverted() {
    for c in sweep() {
        let want = !bg_is_light(c);
        assert_eq!(theme_is_dark(pack(c)), want, "{c:02x?}");
        assert_eq!(
            theme_is_dark(0xFF00_0000 | pack(c)),
            want,
            "{c:02x?} with alpha"
        );
    }
}
