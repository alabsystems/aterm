// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Why a vocabulary is malformed, or `Ok` when every retired spelling names
/// a row that draws, no spelling means two things, and no retired spelling
/// is also a row.
fn check_vocabulary(rows: &[TrailStyleRow], retired: &[RetiredTrailStyle]) -> Result<(), String> {
    let tidy = |s: &str| s == s.trim() && s == s.to_ascii_lowercase() && !s.contains("  ");
    let mut seen: Vec<&str> = Vec::new();
    for spelling in rows
        .iter()
        .map(|row| row.name)
        .chain(CLASSIC_MONO_SPELLINGS.iter().copied())
        .chain(retired.iter().map(|was| was.old))
    {
        if !tidy(spelling) {
            return Err(format!("{spelling:?} is not lower case and trimmed"));
        }
        if seen.iter().any(|s| s.eq_ignore_ascii_case(spelling)) {
            return Err(format!("{spelling:?} is spelled twice"));
        }
        seen.push(spelling);
    }
    for was in retired {
        if !rows
            .iter()
            .any(|row| row.name == was.style && row.style.is_some())
        {
            return Err(format!(
                "{:?} became {:?}, which is not a row that draws",
                was.old, was.style
            ));
        }
        if let Some(pet) = was.pet
            && crate::cursor_pet::species(pet).is_none()
            && !crate::cursor_pet::flies(pet)
        {
            return Err(format!(
                "{:?} became the pet {pet:?}, which is no animal",
                was.old
            ));
        }
    }
    Ok(())
}

#[test]
fn spellings_every_retired_name_became_a_row_and_none_is_still_one() {
    assert_eq!(check_vocabulary(TRAIL_STYLES, RETIRED_TRAIL_STYLES), Ok(()));
    let still_a_row = [RetiredTrailStyle {
        old: "lumen",
        style: "lumen",
        pet: None,
    }];
    assert!(check_vocabulary(TRAIL_STYLES, &still_a_row).is_err());
    let became_nothing = [RetiredTrailStyle {
        old: "plasma",
        style: "rainbow kitty pet",
        pet: None,
    }];
    assert!(check_vocabulary(TRAIL_STYLES, &became_nothing).is_err());
    let became_no_animal = [RetiredTrailStyle {
        old: "plasma",
        style: "rainbow",
        pet: Some("fox"),
    }];
    assert!(check_vocabulary(TRAIL_STYLES, &became_no_animal).is_err());
}

/// One rainbow row, and what each retired rainbow name became: the theme and
/// the animal it named. No retired spelling resolves to a style.
#[test]
fn spellings_one_rainbow_row_and_the_retired_names_resolve_to_none() {
    let rainbow: Vec<&str> = TRAIL_STYLES
        .iter()
        .filter(|row| row.style == Some(GlowStyle::RainbowKitty))
        .map(|row| row.name)
        .collect();
    assert_eq!(rainbow, ["rainbow"]);
    for (old, pet) in [
        ("rainbow kitty pet", "cat"),
        ("rainbow kitty underline", "cat"),
        ("rainbow dog pet", "dog"),
        ("rainbow kitty flying", "flying cat"),
        ("rainbow kitty flat", "cat"),
        ("\u{00a0}RAINBOW DOG\u{2003}", "dog"),
        ("Nyan", "flying cat"),
    ] {
        let was = retired(old).unwrap_or_else(|| panic!("{old:?} is retired"));
        assert_eq!((was.style, was.pet), ("rainbow", Some(pet)), "{old:?}");
    }
    for was in RETIRED_TRAIL_STYLES {
        assert_eq!(canonical(was.old), None, "{:?}", was.old);
        assert_eq!(row(was.old), None, "{:?}", was.old);
        assert_eq!(GlowStyle::parse(was.old), GlowStyle::Lumen, "{:?}", was.old);
        assert_eq!(
            was.pet.is_some(),
            was.style == "rainbow",
            "{:?}: a retired rainbow name names its animal, and no other name names one",
            was.old
        );
        assert!(
            !RETIRED_TRAIL_STYLES
                .iter()
                .any(|other| other.old == was.style),
            "{:?} became a retired name",
            was.old
        );
    }
    let nyan = RETIRED_TRAIL_STYLES
        .iter()
        .filter(|was| was.old.contains("nyan"))
        .count();
    assert_eq!(nyan, 5, "every nyan spelling is retired");
    for row in TRAIL_STYLES {
        assert_eq!(retired(row.name), None, "{:?}", row.name);
        assert!(!row.name.contains("nyan") && !row.label.to_ascii_lowercase().contains("nyan"));
    }
    for mono in CLASSIC_MONO_SPELLINGS {
        assert_eq!(retired(mono), None, "{mono:?}");
    }
}

#[test]
fn spellings_resolve_trimmed_and_case_insensitively() {
    const PADDED: Option<&str> = canonical("\u{00a0}RAINBOW\u{2003}");
    assert_eq!(PADDED, Some("rainbow"));
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
    for row in TRAIL_STYLES {
        // A label is what Settings shows, not a spelling: it resolves only where it is the name.
        if row.label.eq_ignore_ascii_case(row.name) {
            assert_eq!(canonical(row.label), Some(row.name), "{:?}", row.label);
        } else {
            assert_eq!(canonical(row.label), None, "{:?}", row.label);
        }
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
        "\u{200b}rainbow",
        "rainbow\u{feff}",
        "rainbow\u{00a0}kitty",
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
    use crate::cursor_fx::GlowSpelling;

    const OFF: GlowSpelling = GlowSpelling::parse("\u{00a0}OFF\u{2003}");
    const CLASSIC: GlowStyle = GlowStyle::parse("\u{2003}classic mono\u{00a0}");
    assert_eq!(OFF.style, None);
    assert_eq!(CLASSIC, GlowStyle::Classic);

    for row in TRAIL_STYLES {
        assert_eq!(
            GlowStyle::parse(row.name),
            row.style.unwrap_or(GlowStyle::Lumen),
            "{:?}",
            row.name
        );
    }
    assert_eq!(GlowStyle::parse("pack:synthwave"), GlowStyle::Lumen);
    assert_eq!(GlowStyle::parse("plasma"), GlowStyle::Lumen);
    assert_eq!(GlowSpelling::parse("\u{00a0}OFF\u{2003}").style, None);
    // The one rainbow theme is the tall band behind the words; no other style reads the shape.
    let rainbow = GlowSpelling::parse("\u{3000}Rainbow\u{202f}");
    assert_eq!(rainbow.style, Some(GlowStyle::RainbowKitty));
    assert!(rainbow.ribbon_tall && !rainbow.ribbon_flat);
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
