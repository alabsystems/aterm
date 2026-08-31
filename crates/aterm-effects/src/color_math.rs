// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared `0x00RRGGBB` color math for the sparkle-words v2 effect stack
//! (docs/sparkle-words-v2-design.md §4.2/§4.3/§6.4/§6.5): the ink-pair hue
//! nudge, its HSV round-trip, and the WCAG relative luminance the legibility
//! guard and the §6.5 constant-luminance coupling both floor with.
//!
//! A LEAF module on purpose: `nova.rs` (and the §13 `sparkle_v2_demo`
//! filmstrip, which compiles `nova.rs` by `#[path]` — aterm-gui is bin-only)
//! consume these without pulling the host state machine in
//! `word_decorations.rs`, which re-exports them for its own call sites.

/// Rotate a `0x00RRGGBB` color's hue by the §4.2 nudge code (`0..=15` →
/// `-18°..=+18°`), preserving saturation and value.
pub fn hue_nudge(rgb: u32, code: u8) -> u32 {
    let (h, s, v) = rgb2hsv(rgb);
    hsv2rgb(h + (-18.0 + f32::from(code) * 2.4), s, v)
}

/// `0x00RRGGBB` → HSV (`h` degrees, `s`/`v` in `0..=1`) — the local inverse of
/// [`hsv2rgb`], used by the ink-pair hue nudge and the §6.5 luminance-matching
/// bisection. Deliberately NOT a consolidation of the three private `hsv2rgb`
/// copies (that cleanup is owned by the design's P1 note, §4.2).
pub fn rgb2hsv(rgb: u32) -> (f32, f32, f32) {
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let c = max - min;
    let h = if c == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / c).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / c + 2.0)
    } else {
        60.0 * ((r - g) / c + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { c / max };
    (h, s, max)
}

/// WCAG channel linearization and weights, precomputed. The input to the
/// transfer function is always a BYTE, so 256 entries cover every argument.
/// Each stored weight is produced by the same rounded `f32` multiply that the
/// scalar expression used, and evaluation retains its left-associative adds;
/// this is an exact cache, not an approximation. Built at runtime because
/// `powf` is not `const` and decimal transcription can lose the last ULP.
pub(crate) struct RelativeLuminanceTable {
    red: [f32; 256],
    green: [f32; 256],
    blue: [f32; 256],
}

static RELATIVE_LUMINANCE: std::sync::LazyLock<RelativeLuminanceTable> =
    std::sync::LazyLock::new(|| {
        let linear: [f32; 256] = std::array::from_fn(|i| {
            let n = i as f32 / 255.0;
            if n <= 0.03928 {
                n / 12.92
            } else {
                ((n + 0.055) / 1.055).powf(2.4)
            }
        });
        RelativeLuminanceTable {
            red: std::array::from_fn(|i| 0.2126 * linear[i]),
            green: std::array::from_fn(|i| 0.7152 * linear[i]),
            blue: std::array::from_fn(|i| 0.0722 * linear[i]),
        }
    });

#[inline]
pub(crate) fn relative_luminance_table() -> &'static RelativeLuminanceTable {
    &RELATIVE_LUMINANCE
}

#[inline]
pub(crate) fn relative_luminance_with(table: &RelativeLuminanceTable, rgb: u32) -> f32 {
    (table.red[((rgb >> 16) & 0xff) as usize] + table.green[((rgb >> 8) & 0xff) as usize])
        + table.blue[(rgb & 0xff) as usize]
}

/// sRGB relative luminance (WCAG) of a `0x00RRGGBB` colour. Host-side copy of the
/// renderer's private helper: the §4.3 guard runs BEFORE the bytes cross the
/// render boundary (the host resolves final ink), so it needs the same WCAG
/// definition the renderer floors with — bounded to ≤ 9 evaluations per ink word
/// per frame (the guard loop), never per pixel.
pub fn relative_luminance(rgb: u32) -> f32 {
    relative_luminance_with(relative_luminance_table(), rgb)
}

/// HSV (`h` degrees, `s`/`v` in `0..=1`) → `0x00RRGGBB`.
pub fn hsv2rgb(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let q = |f: f32| ((f + m).clamp(0.0, 1.0) * 255.0).round() as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}
