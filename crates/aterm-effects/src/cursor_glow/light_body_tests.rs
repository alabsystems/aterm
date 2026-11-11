// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn contrast(a: u32, b: u32) -> f32 {
    let (x, y) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Every hue's body ink, at its strongest, leaves the theme's text at the
/// bar over it on every light theme that can meet the bar (a sweep of
/// foregrounds from black to mid grey on white and cream), and on one
/// that cannot (Solarized Light) it is still a colour the eye picks out.
#[test]
fn the_light_body_keeps_the_theme_text_at_the_bar_over_it() {
    let a = LIGHT_BODY_ALPHA_CAP as u8;
    let hues = [
        0x00FF_0000,
        0x00FF_7F00,
        0x00FF_FF00,
        0x0000_FF00,
        0x0000_00FF,
        0x004B_0082,
        0x0094_00D3,
        0x0009_69DA,
        0x009E_D6FF,
    ];
    for bg in [0x00FF_FFFF, 0x00FD_F6E3] {
        for grey in (0..=0x77u32).step_by(0x11) {
            let fg = (grey << 16) | (grey << 8) | grey;
            for rgb in hues {
                let ink = light_body_ink(rgb, fg, bg, a);
                let blend = over_at(bg, ink, a);
                assert!(
                    crate::rainbow_kitty::ribbon::seen(blend, bg),
                    "{rgb:06x} still reads as a colour on {bg:06x}"
                );
                if grey <= 0x33 {
                    assert!(
                        contrast(fg, blend) >= BODY_CONTRAST_BAR,
                        "{rgb:06x} under {fg:06x} on {bg:06x}: {:.2}:1",
                        contrast(fg, blend)
                    );
                }
            }
        }
    }
    let (fg, bg) = (0x0065_7B83, 0x00FD_F6E3);
    for rgb in hues {
        let blend = over_at(bg, light_body_ink(rgb, fg, bg, a), a);
        assert!(crate::rainbow_kitty::ribbon::seen(blend, bg), "{rgb:06x}");
    }
    let old = over_at(
        0x00FF_FFFF,
        light_ink(0x0000_00FF),
        LIGHT_INK_ALPHA_CAP as u8,
    );
    assert!(contrast(0x001F_2328, old) < 3.0, "the old blue body");
}
