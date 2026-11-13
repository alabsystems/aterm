// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CURSOR_TRAIL_STYLES, Config, apply_prefs_edits};
use crate::cursor_glow::GlowStyle;
use aterm_effects::cursor_fx::GlowSpelling;
use aterm_effects::kitty_pet::PetSpecies;

fn set(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[test]
fn a_word_unit_is_singular_after_exactly_one() {
    use super::{
        EDIT_CURSOR_TRAIL_LENGTH, EDIT_CURSOR_TRAIL_MS, EDIT_CURSOR_TRAIL_RADIUS, editable_fields,
        integer_reading, numeric_reading,
    };
    assert_eq!(integer_reading(EDIT_CURSOR_TRAIL_LENGTH, 1), "1 cell");
    assert_eq!(integer_reading(EDIT_CURSOR_TRAIL_LENGTH, 24), "24 cells");
    assert_eq!(
        numeric_reading(EDIT_CURSOR_TRAIL_RADIUS, 1.0, "1").as_deref(),
        Some("1 cell")
    );
    assert_eq!(
        numeric_reading(EDIT_CURSOR_TRAIL_RADIUS, 0.6, "0.6").as_deref(),
        Some("0.6 cells")
    );
    assert_eq!(integer_reading(EDIT_CURSOR_TRAIL_MS, 1), "1 ms");
    let doc = "cursor_trail_length = 1\ncursor_trail_radius = 1.0\n";
    let fields = editable_fields(&Config::parse(doc).expect("parses"));
    let placeholder = |key: &str| {
        fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.placeholder.clone())
    };
    assert_eq!(
        placeholder(EDIT_CURSOR_TRAIL_LENGTH).as_deref(),
        Some("1 cell")
    );
    assert_eq!(
        placeholder(EDIT_CURSOR_TRAIL_RADIUS).as_deref(),
        Some("1 cell")
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Look {
    style: Option<GlowStyle>,
    ribbon_tall: bool,
    ribbon_flat: bool,
    classic_mono: bool,
    comet: bool,
    beam: bool,
}

const OFF: Look = Look {
    style: None,
    ribbon_tall: true,
    ribbon_flat: false,
    classic_mono: false,
    comet: false,
    beam: true,
};

const fn styled(style: GlowStyle) -> Look {
    Look {
        style: Some(style),
        ..OFF
    }
}

/// What each `cursor_trail_style` draws: one row a style, and Classic's second face.
const LOOKS: &[(&str, Look)] = &[
    ("off", OFF),
    (
        "rainbow",
        Look {
            ribbon_tall: false,
            beam: false,
            ..styled(GlowStyle::RainbowKitty)
        },
    ),
    ("phaser", styled(GlowStyle::Phaser)),
    ("fire", styled(GlowStyle::Fire)),
    (
        "water",
        Look {
            beam: false,
            ..styled(GlowStyle::Water)
        },
    ),
    ("laser", styled(GlowStyle::Laser)),
    (
        "comet",
        Look {
            comet: true,
            ..styled(GlowStyle::Comet)
        },
    ),
    ("sparkle", styled(GlowStyle::Sparkle)),
    ("lumen", styled(GlowStyle::Lumen)),
    ("beam", styled(GlowStyle::Beam)),
    ("classic", styled(GlowStyle::Classic)),
];

const CLASSIC_MONO: Look = Look {
    classic_mono: true,
    ..styled(GlowStyle::Classic)
};

fn native_look(raw: &str, pet: &str) -> (Look, Option<PetSpecies>, bool) {
    let p = crate::app_config::resolve_trail_presentation(raw, pet);
    let look = Look {
        style: p.style.style,
        ribbon_tall: p.ribbon_tall,
        ribbon_flat: p.ribbon_flat,
        classic_mono: p.classic_mono,
        comet: p.comet,
        beam: p.beam,
    };
    (look, p.pet_species, p.style.unknown)
}

fn web_look(raw: &str) -> Look {
    let s = GlowSpelling::parse(raw);
    Look {
        style: s.style,
        ribbon_tall: s.ribbon_tall,
        ribbon_flat: s.ribbon_flat,
        classic_mono: s.classic_mono,
        comet: s.style == Some(GlowStyle::Comet),
        beam: s.beam,
    }
}

/// The picker's rows are the table's, each draws its look natively and on the web however it
/// is cased or padded, and the pet beside it is the pet setting's and never the style's.
#[test]
fn every_style_draws_its_look_whatever_the_pet() {
    assert_eq!(
        CURSOR_TRAIL_STYLES,
        LOOKS.iter().map(|(name, _)| *name).collect::<Vec<_>>()
    );
    let spellings = LOOKS.iter().copied().chain(
        ["classic mono", "classic lumen", "mono classic"]
            .into_iter()
            .map(|raw| (raw, CLASSIC_MONO)),
    );
    for (raw, want) in spellings {
        for spelt in [
            raw.to_string(),
            raw.to_ascii_uppercase(),
            format!("  {raw} "),
        ] {
            for (pet, species) in [
                ("off", None),
                ("cat", Some(PetSpecies::Cat)),
                ("dog", Some(PetSpecies::Dog)),
            ] {
                assert_eq!(
                    native_look(&spelt, pet),
                    (want, species, false),
                    "native {spelt:?} beside {pet:?}"
                );
            }
            assert_eq!(web_look(&spelt), want, "web {spelt:?} agrees with native");
        }
    }
    let (typo, _, unknown) = native_look("rainbow kity", "off");
    assert!(unknown);
    let default = LOOKS
        .iter()
        .find(|(name, _)| *name == super::DEFAULT_CURSOR_TRAIL_STYLE)
        .expect("the default is a row");
    assert_eq!(typo, default.1);
    assert_eq!(web_look("rainbow kity").style, Some(GlowStyle::Lumen));
}

#[test]
fn validate_config_accepts_a_style_and_flags_a_retired_or_unknown_name() {
    let baseline = crate::diagnostics::validate_config_text("").expect("empty config");
    for style in ["classic mono", "rainbow", "laser", "OFF"] {
        let warnings =
            crate::diagnostics::validate_config_text(&format!("cursor_trail_style = {style:?}\n"))
                .expect("parses");
        assert_eq!(warnings, baseline, "{style:?}");
    }
    for (refused, says) in [
        ("lightnin", "unknown style"),
        ("kitty v2", "unknown style"),
        ("nyan rainbow", "no longer a style"),
        ("rainbow kitty pet", "no longer a style"),
        ("rainbow dog", "cursor_pet = \"dog\""),
        ("lightning", "cursor_trail_style = \"laser\""),
    ] {
        let warnings = crate::diagnostics::validate_config_text(&format!(
            "cursor_trail_style = {refused:?}\n"
        ))
        .expect("parses");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("cursor_trail_style") && w.contains(says)),
            "{refused:?} is flagged with {says:?}: {warnings:?}"
        );
    }
    let warnings =
        crate::diagnostics::validate_config_text("cursor_pet = \"kitty\"\n").expect("parses");
    assert!(
        warnings.iter().any(|w| w.contains("cursor_pet")),
        "{warnings:?}"
    );
}

#[test]
fn the_unset_default_is_the_rainbow_beside_the_cat_all_the_way_to_the_draw_seam() {
    let unset = crate::app_config::Config::default();
    assert_eq!(
        unset.cursor_trail_style_raw(),
        crate::app_config::default_cursor_trail_style(),
        "Windows draws no trail until one is chosen"
    );
    assert_eq!(
        unset.cursor_pet_raw(),
        crate::app_config::default_cursor_pet(),
        "Windows draws no pet until one is chosen"
    );
    let raw = super::DEFAULT_CURSOR_TRAIL_STYLE;
    assert!(
        CURSOR_TRAIL_STYLES.contains(&raw),
        "the default must be a selectable option, got {raw:?}"
    );
    assert!(super::CURSOR_PETS.contains(&super::DEFAULT_CURSOR_PET));
    let (look, pet, unknown) = native_look(raw, super::DEFAULT_CURSOR_PET);
    assert!(!unknown);
    assert_eq!(look.style, Some(GlowStyle::RainbowKitty));
    assert!(
        !look.ribbon_tall && !look.ribbon_flat,
        "the rainbow is the line under what you type"
    );
    assert_eq!(
        pet,
        Some(PetSpecies::Cat),
        "an unset config draws the walking cat"
    );
}

#[test]
fn trail_sound_style_canonical_resolves_aliases_and_the_loader_agrees() {
    use aterm_effects::cursor_glow::GlowStyle;
    use aterm_effects::trail_sound::SoundVoice;
    for &s in super::TRAIL_SOUND_STYLES {
        assert_eq!(super::trail_sound_style_canonical(s), Some(s));
        assert_eq!(
            super::trail_sound_style_canonical(&format!(" {} ", s.to_ascii_uppercase())),
            Some(s)
        );
    }
    for &(alias, voice) in SoundVoice::ALIASES {
        assert_eq!(
            super::trail_sound_style_canonical(alias),
            Some(voice.name()),
            "{alias}"
        );
        let cfg = Config {
            trail_sound_style: Some(alias.to_string()),
            ..Config::default()
        };
        assert_eq!(cfg.trail_sound_voice(), voice, "loader: {alias}");
        if !super::TRAIL_SOUND_STYLES.contains(&alias) {
            assert!(
                apply_prefs_edits("", &[(super::EDIT_TRAIL_SOUND_STYLE, set(alias))]).is_err(),
                "writer must refuse alias {alias:?}"
            );
        }
    }
    for (raw, voice) in [
        ("mech", SoundVoice::Mech),
        ("mechanical", SoundVoice::Mech),
        ("water", SoundVoice::Of(GlowStyle::Water)),
        ("music box", SoundVoice::RainbowKittyV2),
        ("Glass Bell", SoundVoice::RainbowKittyV2),
        ("  felt ", SoundVoice::Felt),
        ("auto", SoundVoice::Style),
        ("garbage", SoundVoice::Style),
        ("", SoundVoice::Style),
    ] {
        let cfg = Config {
            trail_sound_style: Some(raw.to_string()),
            ..Config::default()
        };
        assert_eq!(cfg.trail_sound_voice(), voice, "{raw:?}");
    }
    assert_eq!(Config::default().trail_sound_voice(), SoundVoice::Style);
    assert_eq!(super::trail_sound_style_canonical("kazoo"), None);
    assert_eq!(super::trail_sound_style_canonical(""), None);
    for &o in super::TRAIL_SOUND_STYLES {
        let out = apply_prefs_edits("", &[(super::EDIT_TRAIL_SOUND_STYLE, set(o))]).unwrap();
        assert!(
            out.contains(&format!("trail_sound_style = \"{o}\"")),
            "{out}"
        );
        let up = apply_prefs_edits(
            "",
            &[(super::EDIT_TRAIL_SOUND_STYLE, set(&o.to_ascii_uppercase()))],
        )
        .unwrap();
        assert!(up.contains(&format!("trail_sound_style = \"{o}\"")), "{up}");
    }
}
