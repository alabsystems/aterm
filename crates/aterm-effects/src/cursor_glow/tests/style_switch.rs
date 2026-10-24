// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Style-switch crossfades, the non-fire crown and pooled bolts.

use super::*;

fn frame_mass(out: &[GlowQuad], glow: &CursorGlow) -> u64 {
    let rgb = |c: u32| ((c >> 16) & 0xff) as u64 + ((c >> 8) & 0xff) as u64 + (c & 0xff) as u64;
    let mut m: u64 = out.iter().map(|q| rgb(q.color)).sum();
    m += glow.halos().iter().map(|h| rgb(h.color)).sum::<u64>();
    m += glow.under_quads().iter().map(|q| rgb(q.color)).sum::<u64>();
    m += glow
        .patches()
        .iter()
        .map(|p| p.strength as u64)
        .sum::<u64>();
    m
}

#[test]
fn style_switch_crossfades_the_old_light_and_stays_active() {
    let g = geom();
    let t0 = Instant::now();
    let laser = cfg(GlowStyle::Laser, true);
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
    glow.tick(Some((0, 30)), t, &laser, g, &mut out); // a jump forges bolts + sparks
    assert!(!glow.bolts.is_empty(), "the laser jump forged live bolts");
    assert!(!glow.sparks.is_empty(), "the laser run left live sparks");
    assert!(glow.heat > 0.0, "the hot run charged the heat envelope");
    let heat_before = glow.heat;

    let rainbow_kitty = cfg(GlowStyle::RainbowKitty, true);
    t += Duration::from_millis(8);
    glow.tick(Some((0, 30)), t, &rainbow_kitty, g, &mut out);
    assert!(
        glow.bolts.is_empty() && glow.sparks.is_empty() && glow.particles.is_empty(),
        "the LIVE engine never inherits foreign light (it moved to the fade)"
    );
    assert_eq!(glow.fading.len(), 1, "the switch armed one outgoing fade");
    assert!(
        frame_has_output(&out, &glow),
        "the frame after the switch still shows the old style's light"
    );
    assert!(
        glow.is_active(),
        "is_active holds through the switch (the animation train stays armed)"
    );
    assert!(
        glow.heat > 0.5 * heat_before,
        "typing warmth CARRIES into the incoming style (no cold start)"
    );
    let mass_early = frame_mass(&out, &glow);

    t += Duration::from_millis(80);
    glow.tick(Some((0, 30)), t, &rainbow_kitty, g, &mut out);
    let mass_mid = frame_mass(&out, &glow);
    assert!(
        mass_mid > 0 && mass_mid < mass_early,
        "the fade's amplitude decreases ({mass_early} -> {mass_mid})"
    );
    assert!(glow.is_active(), "is_active holds while the fade lives");

    let fp = glow.tick(
        Some((0, 30)),
        t0 + Duration::from_secs(10),
        &rainbow_kitty,
        g,
        &mut out,
    );
    assert!(glow.fading.is_empty(), "the fade retires at envelope end");
    assert_eq!(fp, 0, "idle-zero after the crossfade completes");
    assert!(
        !glow.is_active(),
        "the timer disarms once everything settles"
    );
}

#[test]
fn no_switch_keeps_the_crossfade_machinery_inert() {
    let g = geom();
    let t0 = Instant::now();
    let c = cfg(GlowStyle::Fire, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=8u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
        assert!(glow.fading.is_empty(), "no switch -> no outgoing fade");
        assert!(glow.ramp_in_at.is_none(), "no switch -> no ramp-in scale");
    }
    t += Duration::from_millis(40);
    glow.note_synthetic_move(t);
    glow.tick(Some((0, 30)), t, &c, g, &mut out); // jump: still the same style
    assert!(glow.fading.is_empty() && glow.ramp_in_at.is_none());
}

#[test]
fn chained_switches_cap_fades_and_never_go_dark() {
    let g = geom();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    let phaser = cfg(GlowStyle::Phaser, true);
    glow.tick(Some((2, 0)), t, &phaser, g, &mut out);
    for i in 1..=8u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &phaser, g, &mut out);
    }
    t += Duration::from_millis(40);
    glow.note_synthetic_move(t);
    glow.tick(Some((0, 30)), t, &phaser, g, &mut out);
    for style in [GlowStyle::Sparkle, GlowStyle::Comet, GlowStyle::Lumen] {
        let c = cfg(style, true);
        t += Duration::from_millis(60);
        glow.tick(Some((0, 30)), t, &c, g, &mut out);
        assert!(
            glow.fading.len() <= CursorGlow::FADE_CAP,
            "concurrent fades stay capped at {}",
            CursorGlow::FADE_CAP
        );
        assert!(
            frame_has_output(&out, &glow),
            "no dark frame right after a chained switch to {style:?}"
        );
    }
    let lumen = cfg(GlowStyle::Lumen, true);
    for _ in 0..30 {
        t += Duration::from_millis(16);
        glow.tick(Some((0, 30)), t, &lumen, g, &mut out);
        if !glow.fading.is_empty() {
            assert!(
                frame_has_output(&out, &glow),
                "a live fade always contributes visible output"
            );
        }
    }
    assert!(
        glow.fading.is_empty(),
        "every fade retires within its envelope"
    );
}

#[test]
fn style_switch_drops_pending_sound_cues() {
    use crate::trail_sound::SoundKind;
    let g = geom();
    let t0 = Instant::now();
    let water = cfg(GlowStyle::Water, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &water, g, &mut out); // seed
    glow.note_synthetic_typed(t0, 1);
    glow.tick(Some((2, 1)), t0, &water, g, &mut out); // typed step -> cue recorded
    assert_eq!(glow.sound_cues.len(), 1, "the typed move recorded its cue");
    let fire = cfg(GlowStyle::Fire, true);
    let t1 = t0 + Duration::from_millis(8);
    glow.tick(Some((2, 1)), t1, &fire, g, &mut out);
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "cues forged under the old style are dropped at the switch edge"
    );
    glow.note_synthetic_typed(t1, 1);
    let t2 = t1 + Duration::from_millis(40);
    glow.tick(Some((2, 2)), t2, &fire, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
}

#[test]
fn a_pooled_bolt_jag_is_deterministic_across_strikes() {
    let mut g = CursorGlow::default();
    let now = Instant::now();
    let strike = |g: &mut CursorGlow, seed: u32, big: bool| -> Vec<(f32, f32)> {
        g.rng = seed;
        g.bolts.clear();
        g.spawn_bolt((10.0, 10.0), (90.0, 46.0), 16.0, big, now);
        g.bolts
            .last()
            .expect("a strike longer than one pixel pushes a bolt")
            .pts
            .to_vec()
    };
    for big in [false, true] {
        let first = strike(&mut g, 0x1234_5678, big);
        assert!(
            first.len() >= 5,
            "fixture: a {} strike is a real polyline ({} points)",
            if big { "jump" } else { "crackle" },
            first.len()
        );
        for seed in [0xAAAA_5555, 0x0F0F_F0F0] {
            strike(&mut g, seed, !big);
        }
        let again = strike(&mut g, 0x1234_5678, big);
        assert_eq!(
            first, again,
            "the reused jag buffer carried a previous strike's points in"
        );
    }
}

#[test]
fn empty_outgoing_style_does_not_evict_live_fades() {
    let g = geom();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;

    let laser = cfg(GlowStyle::Laser, true);
    glow.tick(Some((2, 0)), t, &laser, g, &mut out);
    t += Duration::from_millis(20);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 1)), t, &laser, g, &mut out);

    let sparkle = cfg(GlowStyle::Sparkle, true);
    t += Duration::from_millis(20);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 2)), t, &sparkle, g, &mut out);
    assert_eq!(glow.fading.len(), 1);

    let rk_cfg = cfg(GlowStyle::RainbowKitty, true);
    t += Duration::from_millis(20);
    glow.tick(Some((2, 2)), t, &rk_cfg, g, &mut out);
    assert_eq!(glow.fading.len(), 2);

    let comet = cfg(GlowStyle::Comet, true);
    t += Duration::from_millis(20);
    glow.tick(Some((2, 2)), t, &comet, g, &mut out);
    assert_eq!(
        glow.fading.len(),
        2,
        "switching away from an empty outgoing style must not evict live fades"
    );
    assert_eq!(glow.fading[0].cfg.style, GlowStyle::Laser);
    assert_eq!(glow.fading[1].cfg.style, GlowStyle::Sparkle);
}
