// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Style-switch crossfades, the non-fire crown and pooled bolts.

use super::*;

/// The visible light mass of one ticked frame: every merged stream's colour
/// energy weighted coarsely by area. Crude, but the crossfade proofs only need
/// it monotone in amplitude.
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

/// A style switch mid-animation crossfades the foreign light: the old style's
/// in-flight state moves into an outgoing fade emitting under its old config
/// with decreasing amplitude, `is_active()` holds through the switch, the new
/// style's live state never inherits the foreign light, and typing warmth
/// carries over.
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
    // Switch to rainbow kitty on the next tick at the same cell (no move to spawn from), so every
    // photon below is the old style's residue.
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
    // Two later frames inside the ramp-down: strictly falling amplitude.
    t += Duration::from_millis(80);
    glow.tick(Some((0, 30)), t, &rainbow_kitty, g, &mut out);
    let mass_mid = frame_mass(&out, &glow);
    assert!(
        mass_mid > 0 && mass_mid < mass_early,
        "the fade's amplitude decreases ({mass_early} -> {mass_mid})"
    );
    assert!(glow.is_active(), "is_active holds while the fade lives");
    // Past the whole envelope (and every spark/bolt lifetime): the fade is
    // retired and the engine settles to idle-zero.
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

/// A single-style run never arms a crossfade or a ramp.
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

/// Rapid sequential switches chain: each outgoing style hands off into its own fade (at most
/// `FADE_CAP`, oldest shed), and no frame in the chain is dark while any fade lives.
#[test]
fn chained_switches_cap_fades_and_never_go_dark() {
    let g = geom();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;
    // A hot phaser run + jump: plenty of in-flight light to hand off.
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
    // Three switches 60 ms apart, inside one `FADE_OUT_S` envelope.
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
    // While any fade lives, the frame shows output (`tick_fades` retires a fade
    // that painted nothing in the same frame).
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

/// Sound does not cross styles: a cue recorded under the outgoing style is
/// dropped at the switch edge (the host maps cues to sounds by the current
/// style), and the fade residue records no cues.
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
    // Switch to fire on the next tick without draining (a stalled host): the water-forged cue
    // must not survive to play fire's crackle.
    let fire = cfg(GlowStyle::Fire, true);
    let t1 = t0 + Duration::from_millis(8);
    glow.tick(Some((2, 1)), t1, &fire, g, &mut out);
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "cues forged under the old style are dropped at the switch edge"
    );
    // A fresh typed move under the new style records exactly its own cue.
    glow.note_synthetic_typed(t1, 1);
    let t2 = t1 + Duration::from_millis(40);
    glow.tick(Some((2, 2)), t2, &fire, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
}

/// The pooled bolt-jag buffers carry nothing between strikes: the same seed produces the same
/// polyline strike after strike.
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
            .clone()
    };
    for big in [false, true] {
        let first = strike(&mut g, 0x1234_5678, big);
        assert!(
            first.len() >= 5,
            "fixture: a {} strike is a real polyline ({} points)",
            if big { "jump" } else { "crackle" },
            first.len()
        );
        // Intervening strikes with other seeds, so the pooled buffers hold someone else's points
        // when the repeat runs.
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

/// Switching through an idle or v2-owned style with no v1 residue must not
/// evict an earlier live outgoing fade from `fading`.
#[test]
fn empty_outgoing_style_does_not_evict_live_fades() {
    let g = geom();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = t0;

    // 1. Forge live light under Laser.
    let laser = cfg(GlowStyle::Laser, true);
    glow.tick(Some((2, 0)), t, &laser, g, &mut out);
    t += Duration::from_millis(20);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 1)), t, &laser, g, &mut out);

    // 2. Switch to Sparkle and forge live light there too -> 1 fade (Laser).
    let sparkle = cfg(GlowStyle::Sparkle, true);
    t += Duration::from_millis(20);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 2)), t, &sparkle, g, &mut out);
    assert_eq!(glow.fading.len(), 1);

    // 3. Switch to RainbowKitty (both Laser and Sparkle are now in `fading`, len == 2 == FADE_CAP).
    let rk_cfg = cfg(GlowStyle::RainbowKitty, true);
    t += Duration::from_millis(20);
    glow.tick(Some((2, 2)), t, &rk_cfg, g, &mut out);
    assert_eq!(glow.fading.len(), 2);

    // 4. Switch from RainbowKitty (which has no v1 residue) to Comet: neither
    // the Laser fade nor the Sparkle fade may be evicted.
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
