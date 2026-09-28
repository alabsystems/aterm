// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resident-state cap and its derived model.

use super::*;

/// Bounded resident-work model for the post-`spawn` state. Tier 0 keeps the
/// caps deliberately tiny so the complete reachable space is cheap to
/// exhaust. Tier 1 below rebinds the same constants to the shipping limits
/// and projects the real engine onto these exact saturating transitions.
/// `Buggy=1` models omitting both oldest-first clamps.
fn cursor_resident_cap_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        CursorGlowResidentCaps {
            const SparkCap = 4;
            const ParticleCap = 14;
            const SparkBurst = 1;
            const ParticleBurst = 7;
            const Buggy = 0;
            var sparks = 0;
            var particles = 0;
            action Spawn {
                sparks = if sparks + SparkBurst > SparkCap {
                    if Buggy == 1 { sparks + SparkBurst } else { SparkCap }
                } else {
                    sparks + SparkBurst
                };
                particles = if particles + ParticleBurst > ParticleCap {
                    if Buggy == 1 { particles + ParticleBurst } else { ParticleCap }
                } else {
                    particles + ParticleBurst
                };
            }
            invariant SparksBounded: sparks <= SparkCap;
            invariant ParticlesBounded: particles <= ParticleCap;
        }
    }
}

/// A hostile same-instant move flood cannot grow resident path or particle
/// work beyond the hard caps. This exercises the pre-decay worst case.
#[test]
fn resident_glow_state_is_hard_capped_at_spawn() {
    let g = geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.length = usize::MAX;
    c.intensity = 1.0;
    let mut glow = CursorGlow {
        rng: 0x9E37_79B9,
        ..CursorGlow::default()
    };
    let t = Instant::now();
    for i in 0..1_000u16 {
        let (from, to) = if i.is_multiple_of(2) { (1, 2) } else { (2, 1) };
        glow.last = Some((2, from));
        glow.note_synthetic_move(t);
        glow.spawn(2, from, 2, to, t, &c, g, SpawnLane::Visible);
        assert!(glow.sparks.len() <= CursorGlow::MAX_SPARKS);
        assert!(glow.particles.len() <= CursorGlow::MAX_PARTICLES);
    }
    assert_eq!(glow.sparks.len(), CursorGlow::MAX_SPARKS);
    assert_eq!(glow.particles.len(), CursorGlow::MAX_PARTICLES);

    let mut out = Vec::new();
    glow.last = Some((2, 1));
    glow.tick(Some((2, 1)), t, &c, g, &mut out);
    assert!(out.len() <= CursorGlow::MAX_QUADS);
    assert_invariants(&out, g);
}

/// Tier 0: the healthy saturating transition proves both resident bounds over
/// the full bounded state space, while the unclamped `Buggy=1` twin must yield
/// a counterexample. The latter prevents a vacuous green proof.
#[test]
fn cursor_resident_cap_derived_model_proves_and_catches_missing_clamp() {
    let model = cursor_resident_cap_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

/// Tier 1: drive the shipping Water spawn path at full heat, where every move
/// adds exactly one path sample and the maximum seven-particle burst. Every
/// real post-spawn count must equal the derived model's state through both
/// saturation points and beyond them.
#[test]
fn cursor_resident_cap_real_spawn_conforms_to_model() {
    let spark_cap = i64::try_from(CursorGlow::MAX_SPARKS).expect("spark cap fits i64");
    let particle_cap = i64::try_from(CursorGlow::MAX_PARTICLES).expect("particle cap fits i64");
    let base_model = cursor_resident_cap_model();
    let overrides = [("SparkCap", spark_cap), ("ParticleCap", particle_cap)];
    let model = aterm_spec::interp::with_consts(&base_model, &overrides);
    let mut state = model.init_state();

    let g = geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.length = usize::MAX;
    let mut glow = CursorGlow {
        rng: 0x9E37_79B9,
        ..CursorGlow::default()
    };
    let now = Instant::now();

    // Warm to full momentum, then clear only resident work. Keeping the
    // cadence state makes every measured spawn request Water's maximum
    // seven-particle burst, the hostile path the cap exists to bound.
    for step in 0usize..8 {
        let (from, to) = if step.is_multiple_of(2) {
            (1, 2)
        } else {
            (2, 1)
        };
        glow.last = Some((2, from));
        glow.note_synthetic_move(now);
        glow.spawn(2, from, 2, to, now, &c, g, SpawnLane::Visible);
    }
    assert_eq!(glow.heat, 1.0, "warm-up must reach maximum Water burst");
    glow.sparks.clear();
    glow.particles.clear();

    let mut saturated_transition = None;
    for step in 0..CursorGlow::MAX_SPARKS + 8 {
        let (from, to) = if step.is_multiple_of(2) {
            (1, 2)
        } else {
            (2, 1)
        };
        let previous = state.clone();
        glow.last = Some((2, from));
        glow.note_synthetic_move(now);
        glow.spawn(2, from, 2, to, now, &c, g, SpawnLane::Visible);
        assert!(model.fire("Spawn", &mut state));
        assert_eq!(
            i64::try_from(glow.sparks.len()).expect("spark count fits i64"),
            state["sparks"],
            "real spark count diverged at spawn {step}"
        );
        assert_eq!(
            i64::try_from(glow.particles.len()).expect("particle count fits i64"),
            state["particles"],
            "real particle count diverged at spawn {step}"
        );
        assert!(model.check_invariant("SparksBounded", &state));
        assert!(model.check_invariant("ParticlesBounded", &state));
        if step == CursorGlow::MAX_SPARKS {
            // The first post-cap spawn appends and evicts back to each limit,
            // so the projected shipping transition is a saturated self-loop.
            saturated_transition = Some((previous, state.clone()));
        }
    }
    assert_eq!(state["sparks"], spark_cap);
    assert_eq!(state["particles"], particle_cap);

    let (previous, saturated) = saturated_transition.expect("post-cap transition captured");
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &base_model,
        &overrides,
        &previous,
        &saturated,
        Some("Spawn"),
        "CursorGlow resident-cap saturation",
    );
    assert!(admitted, "real saturation transition rejected: {why}");

    // Tier-1 negative control: the same saturated source with one excess
    // spark is exactly the missing-clamp defect; no healthy action admits it.
    let mut overflow = saturated;
    overflow.insert("sparks", spark_cap + 1);
    let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
        &base_model,
        &overrides,
        &previous,
        &overflow,
        Some("Spawn"),
        "CursorGlow resident-cap negative control",
    );
    assert!(!admitted, "missing-clamp transition must be rejected");
}

#[test]
fn hsv_and_fire_are_sane() {
    assert_eq!(hsv2rgb(0.0, 1.0, 1.0), 0x00FF_0000); // red
    assert_eq!(hsv2rgb(1.0 / 3.0, 1.0, 1.0), 0x0000_FF00); // green
    assert_eq!(hsv2rgb(2.0 / 3.0, 1.0, 1.0), 0x0000_00FF); // blue
    let hot = fire_ramp(1.0);
    let cool = fire_ramp(0.0);
    assert!(
        (hot >> 16) & 0xff >= (cool >> 16) & 0xff,
        "hot end is brighter red+"
    );
}
