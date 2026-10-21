// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `cursor_trail_style` vocabulary: the one table of spellings every host
//! reads.
//!
//! [`TRAIL_STYLES`] lists the canonical styles in the order the Settings
//! Cursor Styles page lists them, each with the name and the one-line
//! description Settings shows. [`TRAIL_STYLE_ALIASES`] maps every other
//! accepted spelling to one of those rows in a single step. [`canonical`]
//! applies both tables.
//!
//! [`GlowStyle::parse`] and the `GlowStyle::style_names_*` predicates
//! canonicalise through this module first. So the native app, its Settings
//! picker and validator, and a web page calling `set_cursor_glow` all read a
//! spelling the same way. The one alias that draws differently from its
//! canonical is the old `classic mono` face: it resolves to `classic`, and
//! [`GlowStyle::style_names_classic_mono`] reads the authored spelling to
//! keep drawing it in the cursor's colour.

use super::GlowStyle;

/// One canonical `cursor_trail_style`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrailStyleRow {
    /// The canonical spelling: what Settings writes, what the picker offers
    /// and what every alias resolves to. Lower case, single spaces.
    pub name: &'static str,
    /// The engine style it draws. `None` for `off` alone.
    pub style: Option<GlowStyle>,
    /// The name Settings lists it by. Itself a spelling: [`canonical`] reads
    /// it back to [`Self::name`].
    pub label: &'static str,
    /// One line saying what the look is, for Settings and accessibility. True
    /// on a dark and a light ground alike.
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

/// Every canonical trail style, in the order the Settings Cursor Styles page
/// lists them: Off first, then the companions, then the trails. Each group
/// opens with the owner's top set (the rainbow kitty and its underline; the
/// phaser, fire, water and lightning); the flat rainbow, the A/B control, and
/// the classic restoration close their groups. Nothing is hidden.
///
/// The rainbow rows are one engine style ([`GlowStyle::RainbowKitty`]). Their
/// spellings choose only the companion and the ribbon body, through the
/// `GlowStyle::style_names_*` predicates.
pub const TRAIL_STYLES: &[TrailStyleRow] = &[
    // `off` turns off the trail and its companion only: the separate typing
    // momentum glow (`cursor_momentum_glow`) has its own switch.
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
    // The flat body (2026-09-13): the owner's A/B control against the
    // default's comet body and its vivid rail
    // (`docs/design/RAINBOW-KITTY-V2.md` §30), the same ribbon and pet with
    // the body as it was before.
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
    // Labelled "Lightning": it draws lightning, not a beam. The saved value
    // stays `laser`.
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
    // The v0.28 trail, restored as an option: a thin four-layer comet under a
    // soft square bloom, with the long diagonal jump comet the modern
    // engine's shape gates no longer mint. Last, because it is a
    // restoration. Its one-colour face is a colour choice, not a style: a
    // trail colour draws it.
    row_of(
        "classic",
        Some(GlowStyle::Classic),
        "Classic",
        "The original cursor glow, in shifting colors or your trail color",
    ),
];

/// Every other accepted spelling, mapped in ONE step to a [`TRAIL_STYLES`]
/// name. No alias names another alias.
pub const TRAIL_STYLE_ALIASES: &[(&str, &str)] = &[
    ("classic wake", "classic"),
    // Classic in one colour is Classic with a trail colour; the old
    // spellings still draw it in the cursor's colour
    // (`GlowStyle::style_names_classic_mono` reads the authored spelling).
    ("classic mono", "classic"),
    ("classic lumen", "classic"),
    ("mono classic", "classic"),
    // The historical `nyan` / `rainbow` spellings keep the flying head.
    ("nyan rainbow", "rainbow kitty flying"),
    ("nyan", "rainbow kitty flying"),
    ("rainbow", "rainbow kitty flying"),
    ("flying kitty", "rainbow kitty flying"),
    ("kitty flying", "rainbow kitty flying"),
    ("kitty", "rainbow kitty pet"),
    ("kitty pet", "rainbow kitty pet"),
    ("pet kitty", "rainbow kitty pet"),
    // The Cursor Styles list's name for the row.
    ("rainbow dog", "rainbow dog pet"),
    ("dog pet", "rainbow dog pet"),
    ("pet dog", "rainbow dog pet"),
    ("rainbow puppy pet", "rainbow dog pet"),
    ("rainbow underline", "rainbow kitty underline"),
    ("underline rainbow", "rainbow kitty underline"),
    ("nyan underline", "rainbow kitty underline"),
    // The bare name and the `… tall` spellings draw exactly what the default
    // draws (the tall body is the default geometry), so they are spellings of
    // it rather than entries of their own.
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
    // What the laser row draws, and what Settings calls it.
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

/// Every [`TRAIL_STYLES`] name in table order: the save-time domain of
/// `cursor_trail_style` and the Settings picker's options.
pub const TRAIL_STYLE_NAMES: [&str; TRAIL_STYLES.len()] = row_names();

/// The canonical [`TRAIL_STYLES`] name a spelling resolves to (a canonical
/// name or an alias; trimmed, ASCII case-insensitive), or `None` for a
/// spelling nothing recognises.
#[must_use]
pub fn canonical(raw: &str) -> Option<&'static str> {
    let raw = raw.trim();
    TRAIL_STYLES
        .iter()
        .map(|row| row.name)
        .find(|name| raw.eq_ignore_ascii_case(name))
        .or_else(|| {
            TRAIL_STYLE_ALIASES
                .iter()
                .find(|(alias, _)| raw.eq_ignore_ascii_case(alias))
                .map(|&(_, name)| name)
        })
}

/// The [`TRAIL_STYLES`] row a spelling resolves to.
#[must_use]
pub fn row(raw: &str) -> Option<&'static TrailStyleRow> {
    let name = canonical(raw)?;
    TRAIL_STYLES.iter().find(|row| row.name == name)
}

/// The Settings name of the style a spelling resolves to.
#[must_use]
pub fn label(raw: &str) -> Option<&'static str> {
    row(raw).map(|row| row.label)
}

/// The one-line description of the style a spelling resolves to.
#[must_use]
pub fn blurb(raw: &str) -> Option<&'static str> {
    row(raw).map(|row| row.blurb)
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
        // Negative controls: a chained alias and a duplicate are both caught.
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
        for row in TRAIL_STYLES {
            assert_eq!(canonical(row.name), Some(row.name));
            assert_eq!(
                canonical(&format!("  {}  ", row.name.to_ascii_uppercase())),
                Some(row.name)
            );
        }
        for &(alias, target) in TRAIL_STYLE_ALIASES {
            assert_eq!(canonical(alias), Some(target), "{alias:?}");
            assert_eq!(canonical(target), Some(target), "{target:?} is canonical");
        }
        // The Cursor Styles list's own names resolve to their rows.
        assert_eq!(canonical("Rainbow dog"), Some("rainbow dog pet"));
        assert_eq!(canonical("lightning"), Some("laser"));
        for row in TRAIL_STYLES {
            assert_eq!(canonical(row.label), Some(row.name), "{:?}", row.label);
        }
        // The retired spellings (the seam-era `v2` word, the release names of
        // the classic wake, Trail Packs) are unknown words.
        for unknown in [
            "",
            "plasma",
            "rainbow kitty v1",
            "rainbow kitty v2",
            "v0.28",
            "retro",
            "pack:synthwave",
            "pack:",
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
        for mono in ["classic mono", "Classic Lumen", " mono classic "] {
            assert_eq!(canonical(mono), Some("classic"), "{mono:?}");
            assert!(GlowStyle::style_names_classic_mono(mono), "{mono:?}");
        }
        assert!(!GlowStyle::style_names_classic_mono("classic"));
    }
}
