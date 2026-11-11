// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/dark_mode.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[test]
fn an_activating_window_takes_its_menus_back_only_when_they_moved() {
    let light = PreferredAppMode::ForceLight as i32;
    let dark_window = Some((Some(Theme::Dark), true));
    assert_eq!(menu_side_to_reassert(light, dark_window), dark_window);
    let light_window = Some((Some(Theme::Light), false));
    assert_eq!(menu_side_to_reassert(light, light_window), None);
    assert_eq!(menu_side_to_reassert(0, light_window), light_window, "none set yet");
    assert_eq!(menu_side_to_reassert(light, None), None, "never themed");
    let follows_os = Some((None, false));
    assert_eq!(menu_side_to_reassert(light, follows_os), follows_os);
    let allow = PreferredAppMode::AllowDark as i32;
    assert_eq!(menu_side_to_reassert(allow, follows_os), None);
}

#[test]
fn menus_take_the_side_the_caption_was_given() {
    assert_eq!(preferred_app_mode(Some(Theme::Dark)), PreferredAppMode::ForceDark);
    assert_eq!(preferred_app_mode(Some(Theme::Light)), PreferredAppMode::ForceLight);
    assert_eq!(preferred_app_mode(None), PreferredAppMode::AllowDark);
}
