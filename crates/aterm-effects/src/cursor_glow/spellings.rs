// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical `cursor_trail_style` table and alias resolution.

use super::GlowStyle;

/// One canonical `cursor_trail_style`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrailStyleRow {
    /// Canonical lower-case spelling saved by Settings and returned by [`canonical`].
    pub name: &'static str,
    /// The engine style it draws, or `None` for `off`.
    pub style: Option<GlowStyle>,
    /// Display label shown in Settings.
    pub label: &'static str,
    /// One-line description for Settings and accessibility.
    pub blurb: &'static str,
}

#[inline]
const fn row_of(
    name: &'static str,
    style: Option<GlowStyle>,
    label: &'static str,
    blurb: &'static str,
) -> TrailStyleRow {
    TrailStyleRow {
        name,
        style,
        label,
        blurb,
    }
}

/// Canonical trail styles in Settings Cursor Styles display order.
pub const TRAIL_STYLES: &[TrailStyleRow] = &[
    row_of("off", None, "Off", "No trail; the cursor stays plain"),
    row_of(
        "rainbow kitty pet",
        Some(GlowStyle::RainbowKitty),
        "Rainbow kitty",
        "A rainbow ribbon with a cat that walks beside your cursor",
    ),
    row_of(
        "rainbow kitty underline",
        Some(GlowStyle::RainbowKitty),
        "Rainbow underline",
        "A thin rainbow line under what you type, with the walking cat; keeps text clearest",
    ),
    row_of(
        "rainbow dog pet",
        Some(GlowStyle::RainbowKitty),
        "Rainbow dog",
        "The rainbow ribbon with a dog that walks beside your cursor",
    ),
    row_of(
        "rainbow kitty flying",
        Some(GlowStyle::RainbowKitty),
        "Flying kitty",
        "The rainbow ribbon; a kitty flies in at its head once your typing gathers speed",
    ),
    row_of(
        "rainbow kitty flat",
        Some(GlowStyle::RainbowKitty),
        "Flat rainbow",
        "The rainbow kitty with an even ribbon, the same height end to end",
    ),
    row_of(
        "phaser",
        Some(GlowStyle::Phaser),
        "Phaser",
        "A short neon spectrum bar behind a glowing caret",
    ),
    row_of(
        "fire",
        Some(GlowStyle::Fire),
        "Fire",
        "Flames behind what you type and a red-hot ember caret",
    ),
    row_of(
        "water",
        Some(GlowStyle::Water),
        "Water",
        "A rolling blue wave with droplets and a water-drop caret",
    ),
    row_of(
        "laser",
        Some(GlowStyle::Laser),
        "Lightning",
        "Lightning on jumps and a bolt caret",
    ),
    row_of(
        "comet",
        Some(GlowStyle::Comet),
        "Comet",
        "An icy comet tail with a glowing nucleus caret",
    ),
    row_of(
        "sparkle",
        Some(GlowStyle::Sparkle),
        "Sparkle",
        "A gold comet with a shower of gold stars and moons",
    ),
    row_of(
        "lumen",
        Some(GlowStyle::Lumen),
        "Lumen",
        "A quiet tracer in your cursor color",
    ),
    row_of(
        "beam",
        Some(GlowStyle::Beam),
        "Beam",
        "A steady tube of cool light that powers down",
    ),
    row_of(
        "classic",
        Some(GlowStyle::Classic),
        "Classic",
        "The original cursor glow, in shifting colors or your trail color",
    ),
];

/// Accepted aliases mapped in one step to a [`TRAIL_STYLES`] canonical name.
pub const TRAIL_STYLE_ALIASES: &[(&str, &str)] = &[
    ("classic wake", "classic"),
    ("classic mono", "classic"),
    ("classic lumen", "classic"),
    ("mono classic", "classic"),
    ("nyan rainbow", "rainbow kitty flying"),
    ("nyan", "rainbow kitty flying"),
    ("rainbow", "rainbow kitty flying"),
    ("flying kitty", "rainbow kitty flying"),
    ("kitty flying", "rainbow kitty flying"),
    ("kitty", "rainbow kitty pet"),
    ("kitty pet", "rainbow kitty pet"),
    ("pet kitty", "rainbow kitty pet"),
    ("rainbow dog", "rainbow dog pet"),
    ("dog pet", "rainbow dog pet"),
    ("pet dog", "rainbow dog pet"),
    ("rainbow puppy pet", "rainbow dog pet"),
    ("rainbow underline", "rainbow kitty underline"),
    ("underline rainbow", "rainbow kitty underline"),
    ("nyan underline", "rainbow kitty underline"),
    ("rainbow kitty", "rainbow kitty pet"),
    ("rainbow kitty tall", "rainbow kitty pet"),
    ("rainbow tall", "rainbow kitty pet"),
    ("tall rainbow", "rainbow kitty pet"),
    ("nyan tall", "rainbow kitty pet"),
    ("rainbow flat", "rainbow kitty flat"),
    ("flat rainbow", "rainbow kitty flat"),
    ("nyan flat", "rainbow kitty flat"),
    ("sparkles", "sparkle"),
    ("phaser-sparkle", "sparkle"),
    ("rainbow-sparkle", "sparkle"),
    ("ember", "fire"),
    ("embers", "fire"),
    ("ocean", "water"),
    ("wave", "water"),
    ("lightning", "laser"),
    ("lightbeam", "beam"),
    ("light-beam", "beam"),
];

#[inline]
const fn row_names<const N: usize>() -> [&'static str; N] {
    let mut out = [""; N];
    let mut i = 0;
    while i < N {
        out[i] = TRAIL_STYLES[i].name;
        i += 1;
    }
    out
}

/// Canonical [`TRAIL_STYLES`] names in table order.
pub const TRAIL_STYLE_NAMES: [&str; TRAIL_STYLES.len()] = row_names();

/// Trim Unicode whitespace with the same property as [`str::trim`], including in const parsers.
#[inline]
pub(crate) const fn trim_whitespace_bytes(raw: &str) -> &[u8] {
    trim_whitespace_slice(raw.as_bytes())
}

/// Trim Unicode whitespace from a UTF-8 slice cut on scalar boundaries.
#[inline]
pub(crate) const fn trim_whitespace_slice(mut bytes: &[u8]) -> &[u8] {
    // Each cut removes a whole scalar, leaving both edges on UTF-8 boundaries.
    while !bytes.is_empty() {
        let (white, width) = scalar_whitespace(bytes);
        if !white {
            break;
        }
        bytes = bytes.split_at(width).1;
    }
    while !bytes.is_empty() {
        let mut start = bytes.len() - 1;
        while bytes[start] & 0xc0 == 0x80 {
            start -= 1;
        }
        let (white, _) = scalar_whitespace(bytes.split_at(start).1);
        if !white {
            break;
        }
        bytes = bytes.split_at(start).0;
    }
    bytes
}

/// Decode the first scalar of a nonempty slice cut at scalar boundaries from a valid `str`.
/// UTF-8's leading byte gives its width; the continuation bytes carry six bits each.
#[inline]
const fn scalar_whitespace(bytes: &[u8]) -> (bool, usize) {
    let lead = bytes[0];
    let width = if lead < 0x80 {
        1
    } else if lead < 0xe0 {
        2
    } else if lead < 0xf0 {
        3
    } else {
        4
    };
    let mut scalar = if width == 1 {
        lead as u32
    } else {
        (lead & (0x7f >> width)) as u32
    };
    let mut i = 1;
    while i < width {
        scalar = (scalar << 6) | (bytes[i] & 0x3f) as u32;
        i += 1;
    }
    let white = match char::from_u32(scalar) {
        Some(c) => c.is_whitespace(),
        None => false,
    };
    (white, width)
}

/// ASCII case-insensitive byte comparison against a lower-case `target`.
#[inline]
pub(crate) const fn eq_ascii_ci(bytes: &[u8], target: &str) -> bool {
    let t = target.as_bytes();
    if bytes.len() != t.len() {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].to_ascii_lowercase() != t[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Resolves `raw` (trimmed, ASCII case-insensitive) to its canonical [`TRAIL_STYLES`] name.
#[inline]
#[must_use]
pub const fn canonical(raw: &str) -> Option<&'static str> {
    match row(raw) {
        Some(r) => Some(r.name),
        None => None,
    }
}

#[inline]
const fn alias_row_indices<const N: usize>() -> [usize; N] {
    let mut out = [0; N];
    let mut ai = 0;
    while ai < N {
        let (_, target) = TRAIL_STYLE_ALIASES[ai];
        let mut ri = 0;
        let mut found = None;
        while ri < TRAIL_STYLES.len() {
            if eq_ascii_ci(target.as_bytes(), TRAIL_STYLES[ri].name) {
                found = Some(ri);
                break;
            }
            ri += 1;
        }
        out[ai] = match found {
            Some(ri) => ri,
            None => panic!("alias target not in TRAIL_STYLES"),
        };
        ai += 1;
    }
    out
}

const TRAIL_STYLE_ALIAS_ROWS: [usize; TRAIL_STYLE_ALIASES.len()] = alias_row_indices();

/// The [`TRAIL_STYLES`] row a spelling resolves to.
#[inline]
#[must_use]
pub const fn row(raw: &str) -> Option<&'static TrailStyleRow> {
    let raw = trim_whitespace_bytes(raw);
    if raw.is_empty() {
        return None;
    }
    let mut i = 0;
    while i < TRAIL_STYLES.len() {
        if eq_ascii_ci(raw, TRAIL_STYLES[i].name) {
            return Some(&TRAIL_STYLES[i]);
        }
        i += 1;
    }
    let mut ai = 0;
    while ai < TRAIL_STYLE_ALIASES.len() {
        let (alias, _) = TRAIL_STYLE_ALIASES[ai];
        if eq_ascii_ci(raw, alias) {
            return Some(&TRAIL_STYLES[TRAIL_STYLE_ALIAS_ROWS[ai]]);
        }
        ai += 1;
    }
    None
}

/// The Settings name of the style a spelling resolves to.
#[inline]
#[must_use]
pub const fn label(raw: &str) -> Option<&'static str> {
    match row(raw) {
        Some(r) => Some(r.label),
        None => None,
    }
}

/// The one-line description of the style a spelling resolves to.
#[inline]
#[must_use]
pub const fn blurb(raw: &str) -> Option<&'static str> {
    match row(raw) {
        Some(r) => Some(r.blurb),
        None => None,
    }
}

/// What a style's Settings row says with a bar caret, where its own blurb promises a caret
/// a bar does not draw, as `(canonical name, blurb)`. A bar caret is a thin rod in the trail's
/// colour in every style, glowing in it: Lightning's bolt, Fire's ember, Water's drop and
/// Comet's nucleus are block carets, and with a bar the rows promised what the caret did not
/// show (composed r3's judges; the after-land lows' W7).
pub const TRAIL_STYLE_BAR_BLURBS: &[(&str, &str)] = &[
    (
        "fire",
        "Flames behind what you type; a bar caret glows ember",
    ),
    (
        "water",
        "A rolling blue wave with droplets; a bar caret glows sea blue",
    ),
    ("laser", "Lightning on jumps; a bar caret glows violet"),
    ("comet", "An icy comet tail; a bar caret glows at its head"),
];

/// The one-line description of the style a spelling resolves to, as the Settings row says it
/// for a bar caret when `bar` ([`TRAIL_STYLE_BAR_BLURBS`]).
#[inline]
#[must_use]
pub const fn blurb_for(raw: &str, bar: bool) -> Option<&'static str> {
    let Some(r) = row(raw) else {
        return None;
    };
    if bar {
        let mut i = 0;
        while i < TRAIL_STYLE_BAR_BLURBS.len() {
            let (name, said) = TRAIL_STYLE_BAR_BLURBS[i];
            if eq_ascii_ci(name.as_bytes(), r.name) {
                return Some(said);
            }
            i += 1;
        }
    }
    Some(r.blurb)
}

#[cfg(test)]
#[path = "spellings_tests.rs"]
mod tests;
