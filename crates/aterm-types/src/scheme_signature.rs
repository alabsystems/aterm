// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Measurements aterm's signature themes clear (`OkLCh` hue/chroma and WCAG contrast ratios).

use crate::Rgb;
use crate::scheme::ColorScheme;

/// One bar of the signature set that a theme misses.
#[derive(Debug, Clone, PartialEq)]
pub struct SignatureDefect {
    /// Stable snake-case bar code (e.g., `text_contrast`, `accent_contrast`, `hue_spacing`).
    pub code: &'static str,
    /// The measurement and the bar, in words.
    pub message: String,
}

/// `OkLab` (Björn Ottosson's published matrices) of an sRGB colour.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "the published OkLab matrices, digit for digit"
)]
#[inline]
#[must_use]
// Skip: f64 arithmetic — the float-interval lane.
#[cfg_attr(trust_verify, trust::skip)]
pub fn oklab(c: Rgb) -> (f64, f64, f64) {
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    (
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    )
}

/// `OkLCh` chroma and hue (degrees in `[0, 360)`) of an sRGB colour.
#[inline]
#[must_use]
// Skip: f64 arithmetic — the float-interval lane.
#[cfg_attr(trust_verify, trust::skip)]
pub fn chroma_hue(c: Rgb) -> (f64, f64) {
    let (_, a, b) = oklab(c);
    (a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

/// The Euclidean `OkLab` distance between two colours.
#[inline]
#[must_use]
// Skip: f64 arithmetic — the float-interval lane.
#[cfg_attr(trust_verify, trust::skip)]
pub fn oklab_distance(x: Rgb, y: Rgb) -> f64 {
    let (p, q) = (oklab(x), oklab(y));
    ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2) + (p.2 - q.2).powi(2)).sqrt()
}

// Skip: f64 arithmetic — the float-interval lane.
/// The angle between two hues, in `[0, 180]`.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn hue_gap(x: f64, y: f64) -> f64 {
    let d = (x - y).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// Every signature bar `s` misses; empty when the theme clears them all.
#[must_use]
// Skip: f64 arithmetic — the float-interval lane.
#[cfg_attr(trust_verify, trust::skip)]
pub fn signature_report(s: &ColorScheme) -> Vec<SignatureDefect> {
    let mut defects = Vec::new();
    let mut need = |ok: bool, code: &'static str, message: String| {
        if !ok {
            defects.push(SignatureDefect { code, message });
        }
    };
    let bg = s.background;
    let a = &s.ansi;
    let tiers = [[1, 2, 3, 4, 5, 6], [9, 10, 11, 12, 13, 14]];
    let text = s.foreground.contrast(bg);
    need(
        text >= 7.0,
        "text_contrast",
        format!("text {text:.2}:1 < 7:1"),
    );
    for &i in tiers.iter().flatten() {
        let r = a[i].contrast(bg);
        need(
            r >= 4.5,
            "accent_contrast",
            format!("color{i} {r:.2}:1 < 4.5:1"),
        );
    }
    for tier in tiers {
        for (n, &i) in tier.iter().enumerate() {
            for &j in &tier[n + 1..] {
                let gap = hue_gap(chroma_hue(a[i]).1, chroma_hue(a[j]).1);
                need(
                    gap >= 25.0,
                    "hue_spacing",
                    format!("color{i}/color{j} hues {gap:.0} deg apart, < 25"),
                );
            }
        }
    }
    for i in 1..=6 {
        let d = oklab_distance(a[i], a[i + 8]);
        need(
            d >= 0.05,
            "bright_distance",
            format!("color{} {d:.3} OkLab from color{i}, < 0.05", i + 8),
        );
    }
    let grey = a[8].contrast(bg);
    need(
        (3.0..4.5).contains(&grey),
        "color8_band",
        format!("color8 {grey:.2}:1 outside [3, 4.5)"),
    );
    let black = a[0].contrast(bg);
    need(
        black >= 1.15,
        "color0_visible",
        format!("color0 {black:.2}:1 < 1.15:1"),
    );
    for i in [7, 15] {
        let r = a[i].contrast(bg);
        need(
            r >= 4.5,
            "white_contrast",
            format!("color{i} {r:.2}:1 < 4.5:1"),
        );
    }
    let band = s.selection_band();
    let r = band.contrast(bg);
    need(r >= 1.5, "selection_band", format!("band {r:.2}:1 < 1.5:1"));
    let r = s.foreground.contrast(band);
    need(
        r >= 4.5,
        "text_on_band",
        format!("text on the band {r:.2}:1 < 4.5:1"),
    );
    let cursor = s.cursor.unwrap_or(s.foreground);
    let r = cursor.contrast(bg);
    need(
        r >= 3.0,
        "cursor_contrast",
        format!("cursor {r:.2}:1 < 3:1"),
    );
    let (cursor_chroma, cursor_hue) = chroma_hue(cursor);
    need(
        cursor_chroma >= 0.08,
        "cursor_chroma",
        format!("cursor chroma {cursor_chroma:.3} < 0.08"),
    );
    let accent = tiers[0]
        .iter()
        .map(|&i| hue_gap(cursor_hue, chroma_hue(a[i]).1))
        .fold(f64::INFINITY, f64::min);
    need(
        accent <= 15.0,
        "cursor_hue",
        format!("cursor hue {accent:.0} deg from the nearest accent, > 15"),
    );
    let (lo, hi) = tiers
        .iter()
        .flatten()
        .map(|&i| chroma_hue(a[i]).0)
        .fold((f64::INFINITY, 0.0f64), |(lo, hi), c| {
            (lo.min(c), hi.max(c))
        });
    need(
        lo >= 0.07 && hi <= 0.17 && hi / lo <= 1.75,
        "chroma_balance",
        format!("accent chroma {lo:.3}..{hi:.3} outside 0.07-0.17 with max/min <= 1.75"),
    );
    defects
}
