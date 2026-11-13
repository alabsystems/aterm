// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Config, DEFAULT_DECORATIVE_EFFECTS, default_cursor_trail_style};

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn the_trail_master_is_platform_split_and_its_two_neighbours_are_not() {
    let fresh = Config::default();
    assert_eq!(
        fresh.cursor_trail_drawn(),
        DEFAULT_DECORATIVE_EFFECTS,
        "a fresh Windows config stands no resident cat on live output; \
         elsewhere the delight stays on"
    );
    assert_eq!(
        fresh.cursor_momentum_glow_or_default(),
        DEFAULT_DECORATIVE_EFFECTS
    );
    assert_eq!(
        cfg("font_px = 14.0").cursor_trail_drawn(),
        DEFAULT_DECORATIVE_EFFECTS
    );
    assert!(fresh.cursor_trail_bloom_or_default());
    assert!(fresh.sparkle_words_enabled_or_default());
    assert!(cfg("cursor_trail_style = \"comet\"").cursor_trail_drawn());
    assert!(!cfg("cursor_trail_style = \"off\"").cursor_trail_drawn());
    assert!(!cfg("cursor_trail_bloom = false").cursor_trail_bloom_or_default());
    assert!(!cfg("[sparkle_words]\nenabled = false").sparkle_words_enabled_or_default());
}

#[test]
fn the_retired_cursor_master_folds_into_its_successors() {
    let mut naive_disagrees = false;
    for master in [None, Some(true), Some(false)] {
        for style in [None, Some("off"), Some("fire")] {
            for glow in [None, Some(true), Some(false)] {
                let mut toml = String::new();
                if let Some(m) = master {
                    toml.push_str(&format!("cursor_trail = {m}\n"));
                }
                if let Some(s) = style {
                    toml.push_str(&format!("cursor_trail_style = \"{s}\"\n"));
                }
                if let Some(g) = glow {
                    toml.push_str(&format!("cursor_momentum_glow = {g}\n"));
                }
                let c = cfg(&toml);
                let on = master.unwrap_or(DEFAULT_DECORATIVE_EFFECTS);
                let old_style = style.unwrap_or(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE);
                let old_drawn = on && old_style != "off";
                let old_glow = on && glow.unwrap_or(true);
                let (want_drawn, want_glow) = if master.is_some() || DEFAULT_DECORATIVE_EFFECTS {
                    (old_drawn, old_glow)
                } else {
                    (style.is_some_and(|s| s != "off"), glow.unwrap_or(false))
                };
                assert_eq!(c.cursor_trail_drawn(), want_drawn, "{toml}");
                assert_eq!(c.cursor_momentum_glow_or_default(), want_glow, "{toml}");
                let naive_drawn = style.unwrap_or(default_cursor_trail_style()).ne("off");
                naive_disagrees |= naive_drawn != want_drawn;
            }
        }
    }
    assert!(
        naive_disagrees,
        "the matrix catches a fold that ignores the old switch"
    );
}
