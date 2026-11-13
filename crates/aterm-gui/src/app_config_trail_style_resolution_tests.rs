// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_effects::cursor_glow::{GlowConfig, GlowStyle};

fn glow(raw: &str, enabled: bool) -> GlowConfig {
    resolve_cursor_glow(
        CursorGlowInputs {
            enabled,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: 0.7,
            radius: 0.6,
            ring: true,
        },
        resolve_trail_presentation(raw, "off"),
        0x00FF_FFFF,
        true,
        0x00C8_D3F5,
        0x001A_1B26,
        0.5,
    )
}

fn warning_for(raw: &str) -> Option<String> {
    Config {
        cursor_trail_style: Some(raw.to_string()),
        ..Config::default()
    }
    .cursor_trail_style_warning()
}

#[test]
fn trail_sound_defaults_agree_with_the_window_policy() {
    use aterm_effects::sound_policy::{fold_window_audibility, trail_sound_gain};

    let app = crate::App::headless_for_test();
    let cold = app.glow_config();
    assert_eq!(cold.audible, GlowConfig::default().audible);
    assert!(!cold.audible, "a cold config has no window focus evidence");
    assert!(app.config.trail_sounds_or_default());
    for focused in [false, true] {
        let mut frame = cold;
        fold_window_audibility(&mut frame, 1.0, 1.0, focused);
        assert_eq!(frame.audible, focused);
        for configured in [None, Some(false), Some(true)] {
            let config = Config {
                trail_sounds: configured,
                ..Config::default()
            };
            assert_eq!(
                trail_sound_gain(frame.audible, config.trail_sounds_or_default(), 0.4),
                (focused && configured != Some(false)).then_some(0.4),
                "focus={focused}, preference={configured:?}"
            );
        }
    }
}

#[test]
fn the_default_glow_config_is_the_shipped_resolve() {
    // A window whose trail is drawn by default: where decorative effects are off by default
    // (Windows) a window with no config draws none, and it takes the drawn default by name,
    // as `trail_drawn` writes it.
    let drawn = || {
        let mut app = crate::App::headless_for_test();
        if !DEFAULT_DECORATIVE_EFFECTS {
            app.config.cursor_trail_style =
                Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE.to_string());
        }
        app
    };
    // The engine's default config still names the tall ribbon body, which no style a person
    // can choose lays: the shipped rainbow is the underline. Every other field is the default.
    let shipped = GlowConfig {
        ribbon_tall: false,
        ..GlowConfig::default()
    };
    let app = drawn();
    assert_eq!(
        app.glow_config(),
        shipped,
        "a window with no config resolves the engine's defaults, on the underline"
    );
    let mut tinted = drawn();
    tinted.config.cursor_trail_color = Some("#d03a8c".to_string());
    assert_ne!(
        tinted.glow_config(),
        shipped,
        "control: a set trail colour resolves away from the default"
    );
}

#[test]
fn classic_receives_the_shipped_knobs() {
    let mut app = crate::App::headless_for_test();
    app.config.cursor_trail_style = Some("classic".to_string());
    let cfg = app.glow_config();
    assert_eq!(
        (
            cfg.style,
            cfg.enabled,
            cfg.duration,
            cfg.length,
            cfg.intensity,
            cfg.radius,
            cfg.ring,
            cfg.beam,
            cfg.classic_mono,
            cfg.head_dx,
        ),
        (
            GlowStyle::Classic,
            true,
            std::time::Duration::from_millis(260),
            24,
            aterm_effects::cursor_fx::CLASSIC_DEFAULT_INTENSITY,
            0.6,
            true,
            true,
            false,
            0.5,
        ),
        "the window hands Classic other knobs than the dark wake is pinned on"
    );
    assert_eq!(
        aterm_effects::cursor_fx::CLASSIC_DEFAULT_INTENSITY,
        0.7,
        "Classic's unset brightness is the v0.28 release's"
    );
    app.config.cursor_trail_length = Some(18);
    assert_eq!(
        app.glow_config().length,
        18,
        "control: a set length is read"
    );
    app.config.cursor_trail_intensity = Some(1.0);
    assert_eq!(
        app.glow_config().intensity,
        1.0,
        "control: a set brightness is read"
    );
    app.config.cursor_trail_intensity = None;
    app.config.cursor_trail_color = Some("#d03a8c".to_string());
    assert_eq!(
        app.glow_config().intensity,
        1.0,
        "control: Classic's one-colour face (a trail colour) draws at full brightness"
    );
    app.config.cursor_trail_color = None;
    app.config.cursor_trail_style = Some("lumen".to_string());
    assert_eq!(
        app.glow_config().intensity,
        1.0,
        "control: another style's unset brightness is full"
    );
}

#[test]
fn the_one_rainbow_theme_is_the_underline() {
    for style in [
        "rainbow",
        " RAINBOW ",
        crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
    ] {
        let cfg = glow(style, true);
        assert_eq!(cfg.style, GlowStyle::RainbowKitty, "{style:?}");
        assert!(
            !cfg.ribbon_tall && !cfg.ribbon_flat,
            "{style:?} draws the line under what you type"
        );
    }
}

#[test]
fn immutable_catalog_caches_the_complete_trail_presentation() {
    let config = Config {
        cursor_trail_style: Some("rainbow".to_string()),
        ..Config::default()
    };
    let catalog = config.resolve_trail_style_cache();
    let cached = catalog.presentation_for(config.cursor_trail_style_raw(), "cat");
    assert_eq!(cached.style.style, Some(GlowStyle::RainbowKitty));
    assert!(!cached.ribbon_tall);
    assert_eq!(
        cached.pet_species,
        Some(aterm_effects::kitty_pet::PetSpecies::Cat)
    );

    // Another style, or another pet, than the cached pair resolves afresh.
    let fallback = catalog.presentation_for("laser", "dog");
    assert_eq!(fallback.style.style, Some(GlowStyle::Laser));
    assert_eq!(
        fallback.pet_species,
        Some(aterm_effects::kitty_pet::PetSpecies::Dog)
    );
    assert_eq!(catalog.presentation_for("rainbow", "off").pet_species, None);
    assert_eq!(
        catalog
            .presentation_for(config.cursor_trail_style_raw(), config.cursor_pet_raw())
            .pet_species,
        cached.pet_species
    );
}

fn engine_is_v2_for(raw: &str) -> bool {
    use aterm_effects::cursor_glow::{CursorGlow, Geom};
    let cfg = glow(raw, true);
    let mut engine = CursorGlow::default();
    let mut out = Vec::new();
    engine.tick(
        Some((2, 4)),
        std::time::Instant::now(),
        &cfg,
        Geom {
            cw: 8,
            ch: 16,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 640,
            win_h: 384,
            head: 0,
        },
        &mut out,
    );
    engine.v2_status().is_some()
}

#[test]
fn the_rainbow_is_v2_and_no_other_style_is() {
    use aterm_effects::trail_sound::SoundVoice;
    let default = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE;
    assert_eq!(
        ResolvedTrailPresentation::default(),
        resolve_trail_presentation(default, crate::prefs::DEFAULT_CURSOR_PET),
        "`Default` must be the resolver's answer for the default spelling"
    );
    let auto = Config::default();
    let marimba = Config {
        trail_sound_style: Some("marimba".to_string()),
        ..Config::default()
    };
    let spellings = crate::prefs::CURSOR_TRAIL_STYLES.iter().copied().chain(
        aterm_effects::cursor_glow::spellings::CLASSIC_MONO_SPELLINGS
            .iter()
            .copied(),
    );
    for raw in spellings {
        let p = resolve_trail_presentation(raw, "off");
        assert!(!p.style.unknown, "{raw:?} is a documented spelling");
        let is_kitty = p.style.style == Some(GlowStyle::RainbowKitty);
        assert_eq!(
            engine_is_v2_for(raw),
            is_kitty,
            "{raw:?}: the engine owns the frame as v2 iff the spelling is the rainbow kitty"
        );
        assert_eq!(
            auto.trail_sound_voice_for(p),
            if is_kitty {
                SoundVoice::RainbowKittyV2
            } else {
                SoundVoice::Style
            },
            "{raw:?}: `auto` is the music box iff the spelling is the rainbow kitty"
        );
        assert_eq!(
            marimba.trail_sound_voice_for(p),
            SoundVoice::Marimba,
            "{raw:?}: an explicit instrument is the user's"
        );
    }
    let fallback = resolve_trail_presentation(default, "off");
    for v1 in [
        "rainbow kitty v1",
        "RAINBOW KITTY V1 pet",
        "v2",
        "",
        "phasr",
        "rainbow kity tall",
        // A retired name is no style: it draws the default and says what it became.
        "nyan",
        "rainbow kitty flying",
        "rainbow dog pet",
    ] {
        let p = resolve_trail_presentation(v1, "off");
        assert!(p.style.unknown, "{v1:?} names no style and says so");
        assert_eq!(
            ResolvedTrailStyle {
                unknown: false,
                ..p.style
            },
            fallback.style,
            "{v1:?} falls back to the default"
        );
        assert!(
            engine_is_v2_for(v1),
            "{v1:?} draws the default, which is v2"
        );
        assert_eq!(auto.trail_sound_voice_for(p), SoundVoice::RainbowKittyV2);
        assert_eq!(effective_trail_style_token(v1), default, "{v1:?}");
        assert_eq!(
            glow(v1, true).ribbon_tall,
            glow(default, true).ribbon_tall,
            "{v1:?}"
        );
        assert!(warning_for(v1).is_some(), "{v1:?} must warn");
    }
    let config = Config {
        cursor_trail_style: Some("rainbow kitty v1".to_string()),
        ..Config::default()
    };
    let cached = config
        .resolve_trail_style_cache()
        .presentation_for(config.cursor_trail_style_raw(), config.cursor_pet_raw());
    assert!(cached.style.unknown);
    assert_eq!(cached.style.style, Some(GlowStyle::RainbowKitty));
    assert_eq!(
        config.trail_sound_voice_for(cached),
        SoundVoice::RainbowKittyV2
    );
}

#[test]
fn off_and_the_master_switch_still_disable_the_trail() {
    for spelling in ["off", "OFF", "  off  "] {
        assert_eq!(resolve_trail_style(spelling).style, None);
        assert!(!resolve_trail_style(spelling).unknown);
        assert!(!glow(spelling, true).enabled, "{spelling:?}");
        assert!(
            warning_for(spelling).is_none(),
            "{spelling:?} is not a typo"
        );
    }
    assert!(
        !glow(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE, false).enabled,
        "`cursor_trail = false` owns the whole effect"
    );
    for &s in crate::prefs::CURSOR_TRAIL_STYLES {
        assert_eq!(
            resolve_trail_style(s).style.is_none(),
            s == "off",
            "style {s:?}"
        );
    }
    for was in aterm_effects::cursor_glow::spellings::RETIRED_TRAIL_STYLES {
        assert!(
            resolve_trail_style(was.old).style.is_some(),
            "the retired {:?} must not disable the trail",
            was.old
        );
    }
}

#[test]
fn the_bloom_radius_is_per_style_with_the_knob_as_the_override() {
    fn radius(style: Option<&str>, knob: Option<f32>) -> f32 {
        let config = Config {
            cursor_trail_style: style.map(str::to_string),
            cursor_trail_bloom_radius: knob,
            ..Config::default()
        };
        let catalog = config.resolve_trail_style_cache();
        config.cursor_trail_bloom_radius_or_default(
            catalog.presentation_for(config.cursor_trail_style_raw(), config.cursor_pet_raw()),
        )
    }
    let kitty = radius(Some("rainbow"), None);
    assert!(kitty < radius(Some("laser"), None));
    assert_eq!(radius(Some("rainbow"), None), kitty);
    assert_eq!(
        radius(None, None),
        radius(Some(default_cursor_trail_style()), None)
    );
    assert_eq!(radius(Some("rainbow"), Some(3.0)), 3.0);
    assert_eq!(radius(Some("laser"), Some(3.0)), 3.0);
}
