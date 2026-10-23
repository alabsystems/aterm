// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm_render::vibrancy::effective_minimum_contrast` and `VibrancyContrast` model conformance.

use aterm_render::vibrancy::{CONTRAST_FLOOR, effective_minimum_contrast, is_translucent};

fn opacities() -> Vec<f32> {
    let mut o: Vec<f32> = (0..=100).map(|i| i as f32 / 100.0).collect();
    o.extend([-0.5, 1.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY]);
    o
}

const CONTRASTS: [f32; 8] = [1.0, 2.0, 3.0, 4.5, 4.6, 7.0, 21.0, f32::NAN];

#[test]
fn effective_floor_never_illegible_and_never_weakens() {
    let (mut raised, mut kept) = (0usize, 0usize);
    for o in opacities() {
        for c in CONTRASTS {
            let eff = effective_minimum_contrast(c, o);
            let at = format!("opacity={o} configured={c}");
            if is_translucent(o) {
                assert!(eff >= CONTRAST_FLOOR, "{at}: glass shipped {eff} < 4.5");
                if c.is_finite() {
                    assert!(eff >= c, "{at}: the floor weakened the configured value");
                }
                if c.is_nan() || c < CONTRAST_FLOOR {
                    raised += 1;
                }
            } else {
                assert_eq!(
                    eff.to_bits(),
                    c.to_bits(),
                    "{at}: opaque must keep c exactly"
                );
                kept += 1;
            }
            assert_eq!(
                effective_minimum_contrast(eff, o).to_bits(),
                eff.to_bits(),
                "{at}: not idempotent"
            );
        }
    }
    assert!(raised > 0 && kept > 0, "both branches exercised");

    assert_eq!(effective_minimum_contrast(1.0, 0.5), CONTRAST_FLOOR);
    assert_ne!(effective_minimum_contrast(1.0, 0.5), 1.0);
}

/// The model's contrast lattice: index `k` ↔ ratio, with `Floor = 3` ↔ 4.5.
const LATTICE: [f32; 7] = [1.0, 2.0, 3.0, 4.5, 7.0, 14.0, 21.0];

fn index_of(ratio: f32) -> i64 {
    LATTICE
        .iter()
        .position(|&r| r.to_bits() == ratio.to_bits())
        .unwrap_or_else(|| panic!("{ratio} is off the lattice")) as i64
}

/// Tier-1 conformance for `VibrancyContrast`: for every `Pick` (translucent × base),
/// the model's one `Fire` successor's `effective` is the lattice index of the real
/// resolver's answer, with `translucent ∈ {0, 1}` ↔ opacity `{1.0, 0.6}`.
#[test]
fn resolver_conforms_to_the_vibrancy_contrast_model() {
    let model = aterm_models_render::vibrancy_contrast_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let picks = model.successors("Pick", &model.init_state());
    assert_eq!(
        picks.len(),
        2 * LATTICE.len(),
        "every translucent × base pick"
    );
    let (mut checked, mut separating) = (0usize, 0usize);
    for picked in picks {
        let translucent = picked["translucent"];
        let base = picked["base"];
        let opacity = if translucent == 1 { 0.6 } else { 1.0 };
        let real = index_of(effective_minimum_contrast(
            LATTICE[usize::try_from(base).expect("lattice index")],
            opacity,
        ));
        let fired = model.successors("Fire", &picked);
        assert_eq!(fired.len(), 1, "Fire is deterministic");
        assert_eq!(
            fired[0]["effective"], real,
            "translucent={translucent} base={base}: the real floor is not the model's"
        );
        let pre_fix = buggy.successors("Fire", &picked);
        if pre_fix.iter().any(|s| s["effective"] != real) {
            assert_eq!(translucent, 1, "Buggy differs only on glass");
            assert!(base < 3, "Buggy differs only below Floor");
            separating += 1;
        }
        checked += 1;
    }
    assert_eq!(checked, 14);
    assert!(
        separating > 0,
        "the Buggy = 1 model must separate from the real resolver somewhere"
    );
}
