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
        "The rainbow ribbon; a kitty flies in after a few seconds of steady typing",
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
        "A gold comet with a shower of rainbow stars",
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
pub(crate) const fn trim_whitespace_bytes(raw: &str) -> &[u8] {
    // Each cut removes a whole scalar, leaving both edges on UTF-8 boundaries.
    let mut bytes = raw.as_bytes();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Why a vocabulary is malformed, or `Ok` when every alias reaches a
    /// canonical row in one step and no spelling means two things.
    fn check_vocabulary(rows: &[TrailStyleRow], aliases: &[(&str, &str)]) -> Result<(), String> {
        let tidy = |s: &str| s == s.trim() && s == s.to_ascii_lowercase() && !s.contains("  ");
        let mut seen: Vec<&str> = Vec::new();
        for spelling in rows
            .iter()
            .map(|row| row.name)
            .chain(aliases.iter().map(|&(alias, _)| alias))
        {
            if !tidy(spelling) {
                return Err(format!("{spelling:?} is not lower case and trimmed"));
            }
            if seen.iter().any(|s| s.eq_ignore_ascii_case(spelling)) {
                return Err(format!("{spelling:?} is spelled twice"));
            }
            seen.push(spelling);
        }
        for &(alias, target) in aliases {
            if !rows.iter().any(|row| row.name == target) {
                return Err(format!(
                    "{alias:?} points at {target:?}, which is not a canonical row"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn spellings_every_alias_lands_on_a_row_in_one_step() {
        assert_eq!(check_vocabulary(TRAIL_STYLES, TRAIL_STYLE_ALIASES), Ok(()));
        let chained = [
            ("kitty", "rainbow kitty"),
            ("rainbow kitty", "rainbow kitty pet"),
        ];
        assert!(check_vocabulary(TRAIL_STYLES, &chained).is_err());
        let duplicate = [("lumen", "lumen")];
        assert!(check_vocabulary(TRAIL_STYLES, &duplicate).is_err());
    }

    #[test]
    fn spellings_resolve_trimmed_and_case_insensitively() {
        const PADDED: Option<&str> = canonical("\u{00a0}RAINBOW DOG\u{2003}");
        assert_eq!(PADDED, Some("rainbow dog pet"));
        for raw in [
            "",
            "\u{00a0}\u{2003}\t\r\n\u{3000}",
            "\u{3000}é🦀\u{0085}",
            "\u{2003}\u{200b}x\u{feff}\u{00a0}",
            "\u{00a0}a\u{2003}b\u{3000}",
        ] {
            assert_eq!(trim_whitespace_bytes(raw), raw.trim().as_bytes());
        }
        let mut boundary = String::with_capacity(12);
        for code in 0..=char::MAX as u32 {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            boundary.clear();
            boundary.push(c);
            boundary.push('x');
            boundary.push(c);
            assert_eq!(
                trim_whitespace_bytes(&boundary),
                boundary.trim().as_bytes(),
                "U+{code:04X}"
            );
        }
        for row in TRAIL_STYLES {
            assert_eq!(canonical(row.name), Some(row.name));
            for padding in ["  ", "\u{00a0}", "\u{2003}\u{3000}", "\u{000b}\t\r\n"] {
                assert_eq!(
                    canonical(&format!(
                        "{padding}{}{padding}",
                        row.name.to_ascii_uppercase()
                    )),
                    Some(row.name)
                );
            }
        }
        for &(alias, target) in TRAIL_STYLE_ALIASES {
            assert_eq!(canonical(alias), Some(target), "{alias:?}");
            assert_eq!(
                canonical(&format!("\u{00a0}{}\u{2003}", alias.to_ascii_uppercase())),
                Some(target)
            );
            assert_eq!(canonical(target), Some(target), "{target:?} is canonical");
        }
        assert_eq!(canonical("Rainbow dog"), Some("rainbow dog pet"));
        assert_eq!(canonical("lightning"), Some("laser"));
        for row in TRAIL_STYLES {
            assert_eq!(canonical(row.label), Some(row.name), "{:?}", row.label);
        }
        for unknown in [
            "",
            "plasma",
            "rainbow kitty v1",
            "rainbow kitty v2",
            "v0.28",
            "retro",
            "pack:synthwave",
            "pack:",
            "\u{200b}rainbow kitty pet",
            "rainbow kitty pet\u{feff}",
            "rainbow\u{00a0}kitty pet",
        ] {
            assert_eq!(canonical(unknown), None, "{unknown:?}");
        }
    }

    /// The derived name lists are the table's rows, and `off` is the one row
    /// without a style.
    #[test]
    fn spellings_name_lists_follow_the_table() {
        let all: Vec<&str> = TRAIL_STYLES.iter().map(|row| row.name).collect();
        assert_eq!(TRAIL_STYLE_NAMES.as_slice(), all.as_slice());
        let styleless: Vec<&str> = TRAIL_STYLES
            .iter()
            .filter(|row| row.style.is_none())
            .map(|row| row.name)
            .collect();
        assert_eq!(styleless, ["off"]);
        for row in TRAIL_STYLES {
            assert_eq!(blurb(row.name), Some(row.blurb));
            assert_eq!(label(row.name), Some(row.label));
            assert!(!row.blurb.trim().is_empty(), "{:?}", row.name);
        }
    }

    /// `GlowStyle::parse` reads a row's style through this table and keeps
    /// `Lumen` for anything else; the old `classic mono` spellings resolve to
    /// `classic` and still name the mono face.
    #[test]
    fn spellings_drive_glow_style_parse() {
        use crate::{cursor_fx::GlowSpelling, kitty_pet::PetSpecies};

        const OFF: GlowSpelling = GlowSpelling::parse("\u{00a0}OFF\u{2003}");
        const DOG: Option<PetSpecies> = GlowSpelling::pet_of("\u{00a0}RAINBOW DOG\u{3000}");
        const CLASSIC: GlowStyle = GlowStyle::parse("\u{2003}classic mono\u{00a0}");
        assert_eq!(OFF.style, None);
        assert_eq!(DOG, Some(PetSpecies::Dog));
        assert_eq!(CLASSIC, GlowStyle::Classic);

        for row in TRAIL_STYLES {
            assert_eq!(
                GlowStyle::parse(row.name),
                row.style.unwrap_or(GlowStyle::Lumen),
                "{:?}",
                row.name
            );
        }
        for &(alias, target) in TRAIL_STYLE_ALIASES {
            assert_eq!(
                GlowStyle::parse(alias),
                GlowStyle::parse(target),
                "{alias:?}"
            );
        }
        assert_eq!(GlowStyle::parse("pack:synthwave"), GlowStyle::Lumen);
        assert_eq!(GlowStyle::parse("plasma"), GlowStyle::Lumen);
        assert_eq!(GlowSpelling::parse("\u{00a0}OFF\u{2003}").style, None);
        for (raw, species) in [
            ("\u{00a0}Rainbow dog\u{2003}", PetSpecies::Dog),
            ("\u{3000}rainbow kitty pet\u{202f}", PetSpecies::Cat),
        ] {
            assert_eq!(GlowStyle::parse(raw), GlowStyle::RainbowKitty);
            assert_eq!(
                GlowSpelling::parse(raw).style,
                Some(GlowStyle::RainbowKitty)
            );
            assert_eq!(GlowSpelling::pet_of(raw), Some(species));
        }
        for mono in [
            "classic mono",
            "Classic Lumen",
            " mono classic ",
            "\u{00a0}classic mono\u{3000}",
        ] {
            assert_eq!(canonical(mono), Some("classic"), "{mono:?}");
            assert!(GlowStyle::style_names_classic_mono(mono), "{mono:?}");
            assert!(GlowSpelling::parse(mono).classic_mono, "{mono:?}");
        }
        assert!(!GlowStyle::style_names_classic_mono("classic"));
    }
}
