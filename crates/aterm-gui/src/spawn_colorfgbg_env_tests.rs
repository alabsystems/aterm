// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn colorfgbg_names_the_themes_background() {
    use aterm_types::Appearance;
    let pair = |a| super::colorfgbg_env(a);
    assert_eq!(pair(None), ("COLORFGBG".to_string(), "15;0".to_string()));
    assert_eq!(pair(Some(Appearance::Dark)).1, "15;0");
    assert_eq!(pair(Some(Appearance::Light)).1, "0;15");
}
