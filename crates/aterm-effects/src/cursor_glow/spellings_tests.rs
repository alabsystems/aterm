// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
