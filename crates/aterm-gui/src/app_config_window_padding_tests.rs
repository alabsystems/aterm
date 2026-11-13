// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Config;

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn window_padding_clamps_and_rejects_non_finite() {
    type Read = fn(&Config) -> f32;
    let base: Read = Config::window_padding_or_default;
    let top: Read = Config::window_padding_top_or_default;
    for (source, read, expected) in [
        ("window_padding = 20.0", base, 20.0),
        ("window_padding = -3.0", base, 0.0),
        ("window_padding = 500.0", base, super::MAX_WINDOW_PADDING_PX),
        ("window_padding = nan", base, crate::PAD_LOGICAL_PX),
        ("window_padding_top = 6.0", top, 6.0),
        ("window_padding = 4.0\nwindow_padding_top = 9.0", top, 4.0),
        ("window_padding = 1.0", top, 1.0),
        ("window_padding_top = -2.0", top, 0.0),
        ("window_padding_top = inf", top, crate::PAD_TOP_LOGICAL_PX),
    ] {
        assert_eq!(read(&cfg(source)), expected, "{source}");
    }
}
