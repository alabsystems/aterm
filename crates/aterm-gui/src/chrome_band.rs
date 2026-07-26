// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared themed cells for compact in-grid chrome such as the find bar, config
//! notices, and native-tab backing rows. This is deliberately independent of any
//! status feature: callers provide the content and own its lifecycle.

use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_render::Theme;

use crate::tab_bar::bg_is_light;

/// On-theme tones for compact chrome bands.
#[derive(Clone, Copy)]
pub(crate) struct BandColors {
    pub bar_bg: [u8; 3],
    pub label: [u8; 3],
    pub value: [u8; 3],
    pub warn: [u8; 3],
    /// Background of an editable WELL inset in the band (the find bar's query
    /// field). The terminal's own background, so the band reads as a raised panel
    /// with a recessed input in it — and so `value` text in the well keeps the
    /// terminal's own fg/bg contrast rather than the band's smaller one.
    pub field_bg: [u8; 3],
    /// Text caret drawn in that well — the theme's CURSOR colour, contrast-floored
    /// against `field_bg` so it stays visible on a recoloured background.
    pub caret: [u8; 3],
}

fn rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

fn blend(a: u32, b: u32, t: f32) -> [u8; 3] {
    mix3(rgb(a), rgb(b), t)
}

fn mix3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let mix = |x: u8, y: u8| (f32::from(x).mul_add(1.0 - t, f32::from(y) * t)).round() as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    aterm_types::Rgb::new(a[0], a[1], a[2]).contrast(aterm_types::Rgb::new(b[0], b[1], b[2]))
}

fn ensure_contrast(c: [u8; 3], bg: [u8; 3], target: f64) -> [u8; 3] {
    if contrast(c, bg) >= target {
        return c;
    }
    let anchor = if bg_is_light(bg) {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    };
    let mut best = c;
    let mut best_ratio = contrast(c, bg);
    let mut step = 1u8;
    while step <= 10 {
        let mixed = mix3(c, anchor, f32::from(step) / 10.0);
        let ratio = contrast(mixed, bg);
        if ratio > best_ratio {
            best = mixed;
            best_ratio = ratio;
        }
        if ratio >= target {
            return mixed;
        }
        step += 1;
    }
    best
}

/// Appearance-aware, theme-derived band tones with WCAG-AA text contrast.
pub(crate) fn band_colors(theme: Theme) -> BandColors {
    let light = bg_is_light(rgb(theme.bg));
    let bar_bg = blend(theme.bg, theme.fg, if light { 0.10 } else { 0.16 });
    let warn_base = if light {
        rgb(0x009A_6700)
    } else {
        rgb(0x00F1_FA8C)
    };
    const AA: f64 = 4.5;
    let field_bg = rgb(theme.bg);
    BandColors {
        bar_bg,
        // `label` is the SECONDARY tone, not an optional one: it carries the find
        // panel's whole hint row, its placeholder, and every inactive toggle. Held to
        // the same AA floor as `value` — a dim role still has to be readable, and
        // `value` (bold, full contrast) keeps the hierarchy on its own.
        label: ensure_contrast(
            blend(theme.fg, theme.bg, if light { 0.40 } else { 0.48 }),
            bar_bg,
            AA,
        ),
        value: ensure_contrast(rgb(theme.fg), bar_bg, AA),
        warn: ensure_contrast(warn_base, bar_bg, AA),
        field_bg,
        caret: ensure_contrast(rgb(theme.cursor), field_bg, AA),
    }
}

/// Build one render cell for compact chrome.
pub(crate) fn cell(ch: char, fg: [u8; 3], bg: [u8; 3], bold: bool, seam: bool) -> RenderCell {
    RenderCell {
        ch,
        fg,
        bg,
        wide: false,
        emoji_presentation: false,
        bold,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: seam,
        underline_color: None,
    }
}

/// A theme-derived blank band cell with a top seam.
#[must_use]
pub(crate) fn blank_cell(theme: Theme) -> RenderCell {
    let colors = band_colors(theme);
    cell(' ', colors.label, colors.bar_bg, false, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_colors_meet_wcag_aa_on_every_builtin_scheme() {
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
            let parts = scheme.to_theme_parts();
            let theme = Theme {
                fg: parts.fg,
                bg: parts.bg,
                cursor: parts.cursor,
                selection: parts.selection,
            };
            let colors = band_colors(theme);
            for (role, value) in [
                ("value", colors.value),
                ("warn", colors.warn),
                ("label", colors.label),
            ] {
                assert!(
                    contrast(value, colors.bar_bg) >= 4.5,
                    "{name} {role} must meet WCAG-AA"
                );
            }
            // The inset well carries the find query + its caret: both must clear AA
            // against the WELL's background, not the band's.
            assert!(
                contrast(colors.caret, colors.field_bg) >= 4.5,
                "{name} caret must meet WCAG-AA in the well"
            );
        }
    }
}
