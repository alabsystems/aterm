// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{AppRt as _, AppRtMacOS, headless_fixture_apprt};
use crate::native_appearance::AppearancePreferences;

#[test]
fn a_headless_fixture_closes_the_native_preferences_arm() {
    let inert = headless_fixture_apprt(AppRtMacOS {
        probe_preferences: false,
    });
    assert!(!inert.probe_preferences);
    assert_eq!(
        inert.native_appearance_preferences(),
        AppearancePreferences::default()
    );
    assert!(!inert.reduce_motion());

    let fixture = headless_fixture_apprt(AppRtMacOS {
        probe_preferences: true,
    });
    assert!(
        !fixture.probe_preferences,
        "a headless fixture must not inherit the live native-preference arm"
    );
    assert_eq!(
        fixture.native_appearance_preferences(),
        AppearancePreferences::default()
    );
    assert!(!fixture.reduce_motion());
}
