// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_instant_word_is_relative_to_t0_and_none_is_apart() {
    let t0 = Instant::now();
    assert_eq!(instant_word(t0, t0), 0);
    assert_eq!(instant_word(t0, t0 + Duration::from_nanos(7)), 7);
    let later = t0 + Duration::from_secs(5);
    assert_eq!(
        instant_word(later, later + Duration::from_millis(3)),
        instant_word(t0, t0 + Duration::from_millis(3)),
        "the word reads the distance from t0, not the instant"
    );
    assert_eq!(instant_word(later, t0), (-5_000_000_000_i64) as u64);
    assert_ne!(instant_word(later, t0), NO_INSTANT);
}

#[test]
fn a_fold_reads_every_word_its_name_and_its_order() {
    let t0 = Instant::now();
    let fold = |words: &[Word]| {
        let mut f = Fold::new(t0);
        for &(n, v) in words {
            f.put(n, v);
        }
        f.finish()
    };
    let base = fold(&[("a", 1), ("b", 2)]);
    assert_eq!(base, fold(&[("a", 1), ("b", 2)]));
    assert_ne!(base, fold(&[("a", 1), ("b", 3)]), "a value");
    assert_ne!(base, fold(&[("a", 1), ("c", 2)]), "a name");
    assert_ne!(base, fold(&[("b", 2), ("a", 1)]), "the order");
    assert_ne!(base, fold(&[("a", 1)]), "a word dropped");
}

#[test]
fn an_absent_record_folds_apart_from_every_present_one() {
    let t0 = Instant::now();
    let word = |v: Option<u64>| {
        let mut w = StateWords::new(t0);
        w.opt("x", v, |f, v| f.put("v", v));
        w.get("x").expect("the word")
    };
    assert_ne!(word(None), word(Some(0)));
    assert_ne!(word(Some(0)), word(Some(1)));
}

#[test]
fn glow_cfg_words_hashes_every_field() {
    let t0 = Instant::now();
    // The sample names every field, so a field a later phase adds lands there and this test
    // still builds; each mutant moves one field off the sample's value, whatever it is.
    let base = GlowConfig::sample_for_test();
    let word = |cfg: &GlowConfig| {
        let mut w = StateWords::new(t0);
        w.group("cfg", |f| glow_cfg_words(f, cfg));
        w.get("cfg").expect("the word")
    };
    let other_style = if base.style == crate::cursor_glow::GlowStyle::Fire {
        crate::cursor_glow::GlowStyle::Lumen
    } else {
        crate::cursor_glow::GlowStyle::Fire
    };
    let mutants: [(&str, GlowConfig); 18] = [
        (
            "enabled",
            GlowConfig {
                enabled: !base.enabled,
                ..base
            },
        ),
        (
            "style",
            GlowConfig {
                style: other_style,
                ..base
            },
        ),
        (
            "color",
            GlowConfig {
                color: base.color ^ 1,
                ..base
            },
        ),
        (
            "accent",
            GlowConfig {
                accent: base.accent ^ 1,
                ..base
            },
        ),
        (
            "duration",
            GlowConfig {
                duration: base.duration + Duration::from_millis(1),
                ..base
            },
        ),
        (
            "length",
            GlowConfig {
                length: base.length + 1,
                ..base
            },
        ),
        (
            "intensity",
            GlowConfig {
                intensity: base.intensity + 0.125,
                ..base
            },
        ),
        (
            "audible",
            GlowConfig {
                audible: !base.audible,
                ..base
            },
        ),
        (
            "radius",
            GlowConfig {
                radius: base.radius + 0.125,
                ..base
            },
        ),
        (
            "ring",
            GlowConfig {
                ring: !base.ring,
                ..base
            },
        ),
        (
            "dark_theme",
            GlowConfig {
                dark_theme: !base.dark_theme,
                ..base
            },
        ),
        (
            "theme_fg",
            GlowConfig {
                theme_fg: base.theme_fg ^ 1,
                ..base
            },
        ),
        (
            "theme_bg",
            GlowConfig {
                theme_bg: base.theme_bg ^ 1,
                ..base
            },
        ),
        (
            "beam",
            GlowConfig {
                beam: !base.beam,
                ..base
            },
        ),
        (
            "head_dx",
            GlowConfig {
                head_dx: base.head_dx + 0.25,
                ..base
            },
        ),
        (
            "ribbon_tall",
            GlowConfig {
                ribbon_tall: !base.ribbon_tall,
                ..base
            },
        ),
        (
            "ribbon_flat",
            GlowConfig {
                ribbon_flat: !base.ribbon_flat,
                ..base
            },
        ),
        (
            "classic_mono",
            GlowConfig {
                classic_mono: !base.classic_mono,
                ..base
            },
        ),
    ];
    let base_word = word(&base);
    assert_eq!(
        base_word,
        word(&base),
        "the words are a function of the config"
    );
    for (field, cfg) in &mutants {
        assert_ne!(word(cfg), base_word, "{field} moves the config's words");
    }
}

/// [`glow_cfg_words_hashes_every_field`] moves every other field; the light, at every level,
/// moves nothing.
#[test]
fn glow_cfg_words_ignores_only_light() {
    use crate::cursor_glow::{Light, Live};
    let t0 = Instant::now();
    let base = GlowConfig::sample_for_test();
    let word = |cfg: &GlowConfig| {
        let mut w = StateWords::new(t0);
        w.group("cfg", |f| glow_cfg_words(f, cfg));
        w.get("cfg").expect("the word")
    };
    let base_word = word(&base);
    for live in [Live::Off, Live::Glass, Live::Lip, Live::Marks, Live::Vanish] {
        let lit = GlowConfig {
            light: Light::at(live),
            ..base
        };
        assert_ne!(lit, base, "{live:?} is a different config");
        assert_eq!(word(&lit), base_word, "{live:?} keeps the config's words");
    }
    assert_ne!(
        word(&GlowConfig {
            dark_theme: !base.dark_theme,
            ..base
        }),
        base_word,
        "a field beside it still moves them"
    );
}

#[test]
fn every_stream_is_one_bit_in_bit_order() {
    for (i, (stream, name)) in Streams::NAMED.into_iter().enumerate() {
        assert_eq!(stream.bits(), 1 << i, "{name} is bit {i}");
        assert_eq!(stream.names().collect::<Vec<_>>(), [name]);
    }
}

#[test]
fn sound_event_words_name_every_field() {
    use crate::trail_sound::{SoundEvent, SoundGesture, SoundKind, SoundVoice};
    let t0 = Instant::now();
    // The sample names every field, so a field added later lands there and this test still
    // builds; each mutant moves one field off the sample's value.
    let base = SoundEvent::sample_for_test();
    let word = |event: &SoundEvent| {
        let mut w = StateWords::new(t0);
        w.group("event", |f| sound_event_words(f, event));
        w.get("event").expect("the word")
    };
    let other_style = if base.style == crate::cursor_glow::GlowStyle::Fire {
        crate::cursor_glow::GlowStyle::Lumen
    } else {
        crate::cursor_glow::GlowStyle::Fire
    };
    let other_gesture = match base.kind {
        SoundGesture::Trail(SoundKind::Typed) => SoundGesture::Trail(SoundKind::Backspace),
        _ => SoundGesture::Trail(SoundKind::Typed),
    };
    let other_tone = if base.tone == crate::tone::Tone::Calm {
        crate::tone::Tone::Playful
    } else {
        crate::tone::Tone::Calm
    };
    let mutants: [(&str, SoundEvent); 10] = [
        (
            "style",
            SoundEvent {
                style: other_style,
                ..base
            },
        ),
        (
            "voice",
            SoundEvent {
                voice: SoundVoice::Of(base.style),
                ..base
            },
        ),
        (
            "kind",
            SoundEvent {
                kind: other_gesture,
                ..base
            },
        ),
        (
            "pan",
            SoundEvent {
                pan: base.pan + 0.125,
                ..base
            },
        ),
        (
            "heat",
            SoundEvent {
                heat: base.heat + 0.125,
                ..base
            },
        ),
        (
            "hue",
            SoundEvent {
                hue: base.hue + 0.125,
                ..base
            },
        ),
        (
            "gain",
            SoundEvent {
                gain: base.gain + 0.125,
                ..base
            },
        ),
        (
            "tone",
            SoundEvent {
                tone: other_tone,
                ..base
            },
        ),
        (
            "bed",
            SoundEvent {
                bed: !base.bed,
                ..base
            },
        ),
        (
            "shifted",
            SoundEvent {
                shifted: !base.shifted,
                ..base
            },
        ),
    ];
    let base_word = word(&base);
    assert_eq!(
        base_word,
        word(&base),
        "the words are a function of the event"
    );
    for (field, event) in &mutants {
        assert_ne!(word(event), base_word, "{field} moves the event's words");
    }
}

#[test]
fn arms_drawn_reports_each_arm_and_the_streams_it_touched() {
    assert!(ArmsDrawn::NONE.is_empty());
    assert_eq!(ArmsDrawn::NONE.iter().count(), 0);
    assert!(ArmsDrawn::NONE.streams().is_empty());
    let mut arms = ArmsDrawn::NONE;
    arms.note("glass", Streams::GLOW_UNDER);
    arms.note("lip", Streams::GLOW_UNDER_RAMPS);
    arms.note("glass", Streams::GLOW_HALO);
    assert!(!arms.is_empty());
    let drawn: Vec<_> = arms.iter().collect();
    assert_eq!(
        drawn,
        [
            ("glass", Streams::GLOW_UNDER.union(Streams::GLOW_HALO)),
            ("lip", Streams::GLOW_UNDER_RAMPS),
        ]
    );
    assert_eq!(
        arms.streams().names().collect::<Vec<_>>(),
        ["glow_under", "glow_under_ramps", "glow_halo"]
    );
}
