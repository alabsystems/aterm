// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Style-switch crossfades, the non-fire crown and pooled bolts.

use super::*;

/// The visible LIGHT MASS of one ticked frame — every merged stream's
/// colour energy weighted coarsely by area. A crude photometer, but the
/// crossfade proofs only need MONOTONE-IN-AMPLITUDE: the ramp-down scales
/// every emitted colour linearly, so a falling envelope must read as a
/// falling mass.
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

/// STYLE SWITCH mid-animation CROSSFADES the foreign light instead of hard-
/// cutting to black: the old style's in-flight state moves into an outgoing
/// fade that keeps emitting under its OLD config with a DECREASING
/// amplitude, `is_active()` holds through the switch (the animation train
/// never disarms mid-fade), the NEW style's live state never inherits the
/// foreign light (the audited hot-swap artifact stays fixed), and the
/// typing WARMTH carries so the incoming style does not start cold.
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
    // HOT-SWAP to rainbow kitty on the next tick — SAME cursor cell (no move
    // to spawn from), so every emitted photon below is the OLD style's residue.
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
    // retired and the engine settles back to idle-zero — no leaked wakes.
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

/// The no-switch path is PROVABLY INERT for the crossfade machinery: a
/// single-style run never arms a fade or a ramp, so every built-in style's
/// output stays byte-identical (the whole-frame golden proof pins the
/// bytes; this pins the mechanism).
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

/// RAPID SEQUENTIAL SWITCHES chain gracefully: each outgoing style hands
/// off into its own fade (bounded at FADE_CAP, oldest shed), and NO frame
/// inside the chain shows zero cursor-effect output while any fade lives —
/// the product gesture is flipping through the style menu and watching the
/// trails morph live, never blink.
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
    // Three switches ~60 ms apart — well inside one 250 ms envelope.
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
    // Ride the tail out frame-by-frame: while ANY fade lives, the frame
    // shows output (a retained fade that painted nothing is a contract
    // violation — tick_fades retires such a fade the same frame).
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

/// SOUND does not cross styles: a cue recorded under the outgoing style is
/// dropped at the switch edge (the host maps cues -> sounds by the CURRENT
/// style when it drains, so a stale cue would play the wrong palette), and
/// the fade residue never records cues of its own.
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
    // Switch to fire on the next tick WITHOUT draining (a stalled host):
    // the water-forged cue must not survive to play fire's crackle.
    let fire = cfg(GlowStyle::Fire, true);
    let t1 = t0 + Duration::from_millis(8);
    glow.tick(Some((2, 1)), t1, &fire, g, &mut out);
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "cues forged under the old style are dropped at the switch edge"
    );
    // A fresh typed move under the NEW style records exactly its own cue.
    glow.note_synthetic_typed(t1, 1);
    let t2 = t1 + Duration::from_millis(40);
    glow.tick(Some((2, 2)), t2, &fire, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
}

/// The non-fire crown is RADIAL light now, not the retired box-stack of nested
/// rectangles: a hot run's crown lands as RainHalos around the head (falloff
/// radii ≥ 1) for every non-fire style — the audited laser posterized squares
/// (with a detached dim quad below) and the lumen/sparkle/water stepped banding
/// are gone. Both backends render the RainHalo identically.
#[test]
fn non_fire_crown_is_radial_halos_not_squares() {
    let g = geom();
    let t0 = Instant::now();
    for style in [
        GlowStyle::Laser,
        GlowStyle::Lumen,
        GlowStyle::Sparkle,
        GlowStyle::Water,
        GlowStyle::Comet,
        GlowStyle::Phaser,
    ] {
        let c = cfg(style, true);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = t0;
        glow.tick(Some((3, 5)), t, &c, g, &mut out);
        for i in 6..=12u16 {
            t += Duration::from_millis(35);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((3, i)), t, &c, g, &mut out);
        }
        assert!(
            !glow.halos().is_empty(),
            "{style:?}: the crown emits radial halos"
        );
        for h in glow.halos() {
            assert!(
                h.rx >= 1 && h.ry >= 1,
                "{style:?}: halo radii floored: {h:?}"
            );
        }
        let head_cx = (12 * g.cw + g.cw / 2) as i32;
        assert!(
            glow.halos()
                .iter()
                .any(|h| (h.cx as i32 - head_cx).abs() < 3 * g.cw as i32),
            "{style:?}: a radial crown halo sits on the cursor head"
        );
    }
}

/// "beam" is a first-class style now (the steady power-down TUBE), no longer
/// an alias folded into `Lumen` — while `comet`/`lumen` keep parsing to the
/// default and `laser` stays its own style.
/// **THE JAG'S REUSED BUFFERS CARRY NOTHING BETWEEN STRIKES.**
///
/// `spawn_bolt` used to build a fresh `Vec` per midpoint-displacement round
/// — `vec![from, to]` plus one `Vec::with_capacity` per round, two rounds
/// for a crackle and four for a jump — so every strike cost three or five
/// heap buffers to describe a polyline five points long, and hot typing
/// crackles several a second. They are pooled on the engine now, which
/// introduces exactly one hazard worth a test: a reused buffer that is not
/// cleared carries the PREVIOUS strike's points into this one.
///
/// So: the same seed must produce the same polyline, strike after strike.
/// Laser has no other behavioural coverage in this crate — the only other
/// mention is the style-name parse below — so this is also the only thing
/// standing between the pooling and a silent shape regression.
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
        // Intervening strikes with OTHER seeds, so the pooled buffers hold
        // someone else's points when the repeat runs.
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
