// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The frame's [`Light`] (`docs/design/PRISM-GLASS.md` §15.2 1.1): its levels, and the one place
//! the glow drops it, an outgoing style's fade.

use super::*;

/// [`Light::OFF`] draws no arm and is not on; a latched scheme is on at every level, and each
/// level draws its own arm and every arm before it, never one after.
#[test]
fn a_light_draws_the_arms_of_its_level_and_those_before_it() {
    let arms = |l: Light| (l.on(), l.glass(), l.lip(), l.marks(), l.vanish());
    assert_eq!(Light::default(), Light::OFF);
    assert_eq!(arms(Light::OFF), (false, false, false, false, false));
    assert_eq!(
        arms(Light::at(Live::Off)),
        (true, false, false, false, false)
    );
    assert_eq!(
        arms(Light::at(Live::Glass)),
        (true, true, false, false, false)
    );
    assert_eq!(arms(Light::at(Live::Lip)), (true, true, true, false, false));
    assert_eq!(
        arms(Light::at(Live::Marks)),
        (true, true, true, true, false)
    );
    assert_eq!(
        arms(Light::at(Live::Vanish)),
        (true, true, true, true, true)
    );
    assert_eq!(
        GlowConfig::default().light,
        Light::OFF,
        "the shipped resolve is dark"
    );
}

/// A latch's verdict is the frame's light: an admitted frame is lit at the live level, and one
/// the latch refused is [`Light::OFF`] at every level (§15.2 1.3).
#[test]
fn an_admitted_frame_is_lit_at_the_live_level_and_a_refused_one_is_off() {
    for live in [Live::Off, Live::Glass, Live::Lip, Live::Marks, Live::Vanish] {
        assert_eq!(Light::admitted(true, live), Light::at(live));
        assert_eq!(Light::admitted(false, live), Light::OFF);
    }
    assert_eq!(
        Light::admitted(true, crate::rainbow_kitty::prism_glass::LIVE),
        Light::at(Live::Off),
        "until 1.12 a latched scheme draws no arm"
    );
}

/// A style switch hands the outgoing engine its last config with the light dropped, whatever the
/// frame's light was: a fade draws main's arm.
#[test]
fn a_style_fade_draws_by_mains_arm() {
    let g = geom();
    let t0 = Instant::now();
    let laser = GlowConfig {
        light: Light::at(Live::Vanish),
        ..cfg(GlowStyle::Laser, true)
    };
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &laser, g, &mut out);
    for i in 1..=8u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &laser, g, &mut out);
    }
    t += Duration::from_millis(40);
    glow.note_synthetic_move(t);
    glow.tick(Some((0, 30)), t, &laser, g, &mut out);
    assert!(
        !glow.sparks.is_empty(),
        "the laser left live sparks to fade"
    );
    assert_eq!(
        glow.last_cfg.map(|c| c.light),
        Some(Light::at(Live::Vanish)),
        "the live engine keeps the frame's light"
    );
    t += Duration::from_millis(8);
    glow.tick(
        Some((0, 30)),
        t,
        &cfg(GlowStyle::RainbowKitty, true),
        g,
        &mut out,
    );
    let fade = glow
        .fading
        .first()
        .expect("the switch armed an outgoing fade");
    assert_eq!(fade.cfg.light, Light::OFF, "the fade's config");
    assert_eq!(
        fade.engine.last_cfg.map(|c| c.light),
        Some(Light::OFF),
        "the outgoing engine's config"
    );
}

/// The rainbow kitty's config carries the frame's light as the glow config hands it.
#[test]
fn the_rainbow_kittys_config_carries_the_frames_light() {
    for light in [Light::OFF, Light::at(Live::Off), Light::at(Live::Glass)] {
        let glow = GlowConfig {
            light,
            ..cfg(GlowStyle::RainbowKitty, true)
        };
        for reduced_motion in [false, true] {
            assert_eq!(
                crate::rainbow_kitty::Config::from_glow(&glow, reduced_motion).light,
                light
            );
        }
    }
}
