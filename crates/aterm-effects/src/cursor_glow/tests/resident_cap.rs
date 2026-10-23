// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resident-state cap and its derived model.

use super::*;

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

#[test]
fn cursor_resident_cap_derived_model_proves_and_catches_missing_clamp() {
    let model = cursor_resident_cap_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

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
