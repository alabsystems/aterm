// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Config;
use aterm_render::vibrancy::{CONTRAST_FLOOR, effective_minimum_contrast};

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn effective_minimum_contrast_is_the_renderers_law() {
    let opacities: Vec<f32> = (0..=20).map(|i| i as f32 / 20.0).collect();
    let contrasts = [1.0_f32, 2.0, 3.0, 4.5, 4.6, 7.0, 21.0];
    for &o in &opacities {
        for &c in &contrasts {
            let config = cfg(&format!("background_opacity = {o}\nminimum_contrast = {c}"));
            let base = config.minimum_contrast_or_default();
            let opacity = config.background_opacity_or_default();
            assert_eq!(
                config.effective_minimum_contrast().to_bits(),
                effective_minimum_contrast(base, opacity).to_bits(),
                "opacity={o} contrast={c}"
            );
        }
    }

    let default_glass = cfg("background_opacity = 0.5");
    assert_eq!(default_glass.minimum_contrast_or_default(), 1.0);
    assert_eq!(default_glass.effective_minimum_contrast(), CONTRAST_FLOOR);
}

#[test]
fn an_unset_minimum_contrast_floors_theme_pairs_on_windows_only() {
    let unset = cfg("");
    assert_eq!(unset.effective_minimum_contrast(), 1.0);
    assert_eq!(
        unset.applied_terminal_config().indexed_minimum_contrast,
        if cfg!(windows) { 4.5 } else { 1.0 }
    );
    for set in ["minimum_contrast = 1.0", "minimum_contrast = 7.0"] {
        let config = cfg(set);
        assert_eq!(
            config.applied_terminal_config().indexed_minimum_contrast,
            1.0,
            "{set}"
        );
    }
}
