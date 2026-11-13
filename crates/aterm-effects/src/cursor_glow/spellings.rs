// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `cursor_trail_style` table, and the spellings retired from it.

use super::GlowStyle;

/// One canonical `cursor_trail_style`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrailStyleRow {
    /// The lower-case spelling aterm.toml holds, returned by [`canonical`].
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
    row_of("off", None, "Off", "No trail behind the cursor"),
    row_of(
        "rainbow",
        Some(GlowStyle::RainbowKitty),
        "Rainbow kitty",
        "A thin rainbow line under what you type; keeps text clearest",
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

/// The spellings of `classic` that draw its theme-following face
/// ([`GlowStyle::style_names_classic_mono`]). They name a face of one row, so they are spellings
/// of that row and not retired names.
pub const CLASSIC_MONO_SPELLINGS: &[&str] = &["classic mono", "classic lumen", "mono classic"];

/// One `cursor_trail_style` spelling this build no longer accepts, with what it became: the
/// style that draws it now and, for a spelling that also named an animal, the `cursor_pet`
/// ([`crate::cursor_pet`]) that keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetiredTrailStyle {
    /// The retired lower-case spelling.
    pub old: &'static str,
    /// The [`TRAIL_STYLES`] name it became.
    pub style: &'static str,
    /// The `cursor_pet` it named, where it named one.
    pub pet: Option<&'static str>,
}

#[inline]
const fn was(
    old: &'static str,
    style: &'static str,
    pet: Option<&'static str>,
) -> RetiredTrailStyle {
    RetiredTrailStyle { old, style, pet }
}

/// Every retired `cursor_trail_style` spelling. A host rewrites a stored one once
/// ([`retired`]) and refuses it afterwards; nothing else reads this table, so no retired
/// spelling resolves to a style.
///
/// The five rainbow rows became the one `rainbow` theme, and the animal each named became the
/// `cursor_pet` setting: the tall, underline and flat ribbons kept the cat, the dog row the dog.
/// The flying row had no walker (a head flew in at speed): it became the flying cat, a pet of
/// its own. A row that named no animal drew none, so its file keeps `cursor_pet = "off"`.
pub const RETIRED_TRAIL_STYLES: &[RetiredTrailStyle] = &[
    was("rainbow kitty pet", "rainbow", Some("cat")),
    was("rainbow kitty", "rainbow", Some("cat")),
    was("rainbow kitty tall", "rainbow", Some("cat")),
    was("rainbow tall", "rainbow", Some("cat")),
    was("tall rainbow", "rainbow", Some("cat")),
    was("nyan tall", "rainbow", Some("cat")),
    was("kitty", "rainbow", Some("cat")),
    was("kitty pet", "rainbow", Some("cat")),
    was("pet kitty", "rainbow", Some("cat")),
    was("rainbow kitty underline", "rainbow", Some("cat")),
    was("rainbow underline", "rainbow", Some("cat")),
    was("underline rainbow", "rainbow", Some("cat")),
    was("nyan underline", "rainbow", Some("cat")),
    was("rainbow dog pet", "rainbow", Some("dog")),
    was("rainbow dog", "rainbow", Some("dog")),
    was("dog pet", "rainbow", Some("dog")),
    was("pet dog", "rainbow", Some("dog")),
    was("rainbow puppy pet", "rainbow", Some("dog")),
    was("rainbow kitty flying", "rainbow", Some("flying cat")),
    was("flying kitty", "rainbow", Some("flying cat")),
    was("kitty flying", "rainbow", Some("flying cat")),
    was("nyan rainbow", "rainbow", Some("flying cat")),
    was("nyan", "rainbow", Some("flying cat")),
    was("rainbow kitty flat", "rainbow", Some("cat")),
    was("rainbow flat", "rainbow", Some("cat")),
    was("flat rainbow", "rainbow", Some("cat")),
    was("nyan flat", "rainbow", Some("cat")),
    was("classic wake", "classic", None),
    was("sparkles", "sparkle", None),
    was("phaser-sparkle", "sparkle", None),
    was("rainbow-sparkle", "sparkle", None),
    was("ember", "fire", None),
    was("embers", "fire", None),
    was("ocean", "water", None),
    was("wave", "water", None),
    was("lightning", "laser", None),
    was("lightbeam", "beam", None),
    was("light-beam", "beam", None),
];

/// The retired spelling `raw` is (trimmed, ASCII case-insensitive), with what it became.
#[inline]
#[must_use]
pub const fn retired(raw: &str) -> Option<&'static RetiredTrailStyle> {
    let raw = trim_whitespace_bytes(raw);
    let mut i = 0;
    while i < RETIRED_TRAIL_STYLES.len() {
        if eq_ascii_ci(raw, RETIRED_TRAIL_STYLES[i].old) {
            return Some(&RETIRED_TRAIL_STYLES[i]);
        }
        i += 1;
    }
    None
}

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

/// Resolves `raw` (trimmed, ASCII case-insensitive) to its [`TRAIL_STYLES`] name. A retired
/// spelling ([`retired`]) resolves to none.
#[inline]
#[must_use]
pub const fn canonical(raw: &str) -> Option<&'static str> {
    match row(raw) {
        Some(r) => Some(r.name),
        None => None,
    }
}

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
    let mut mi = 0;
    while mi < CLASSIC_MONO_SPELLINGS.len() {
        if eq_ascii_ci(raw, CLASSIC_MONO_SPELLINGS[mi]) {
            return row("classic");
        }
        mi += 1;
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
