// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::streak_is_dark;

/// With no override the streak takes the native classifier's side, mid-greys
/// included: `#A0A0A0` is light to the chrome, so the streak must not paint
/// additive light on it.
#[test]
fn streak_polarity_follows_the_theme_background_by_default() {
    for v in 0..=255u32 {
        let bg = v * 0x0001_0101;
        assert_eq!(
            streak_is_dark(None, bg),
            aterm_render::theme_is_dark(bg),
            "{bg:06x}"
        );
    }
    assert!(!streak_is_dark(None, 0x00A0_A0A0));
    assert!(streak_is_dark(None, 0x0011_1318));
}

/// A host-supplied bool overrides the derived side either way.
#[test]
fn a_host_override_wins_over_the_derived_side() {
    assert!(streak_is_dark(Some(true), 0x00FF_FFFF));
    assert!(!streak_is_dark(Some(false), 0x0000_0000));
}
