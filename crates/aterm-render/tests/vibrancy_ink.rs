// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background opacity alpha rounding, monotonicity, and clamping tests.

use aterm_render::vibrancy::{bg_quad_alpha, is_translucent};

#[test]
fn bg_alpha_is_rounded_and_monotone() {
    assert_eq!(bg_quad_alpha(1.0), 255, "solid");
    assert_eq!(bg_quad_alpha(0.0), 0, "fully transparent");
    assert_eq!(bg_quad_alpha(0.5), 128, "half → round(127.5)=128");
    assert_eq!(bg_quad_alpha(0.25), 64, "quarter → round(63.75)=64");

    let mut prev = 0u8;
    for i in 0..=100 {
        let a = bg_quad_alpha(i as f32 / 100.0);
        assert!(
            a >= prev,
            "bg alpha must be monotone; dipped at {i}%: {a} < {prev}"
        );
        prev = a;
    }
    assert_eq!(prev, 255, "the top of the lattice is opaque");

    for o in [1.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(bg_quad_alpha(o), 255, "opacity={o} must stay solid");
        assert!(!is_translucent(o), "opacity={o} is not translucent");
    }
    assert_eq!(bg_quad_alpha(-0.5), 0, "under-range clamps to transparent");
    for i in 0..100 {
        let o = i as f32 / 100.0;
        assert!(is_translucent(o) && bg_quad_alpha(o) < 255, "opacity={o}");
    }
    let below_one = f32::from_bits(1.0_f32.to_bits() - 1);
    for o in [0.9985, 0.999, below_one] {
        assert!(is_translucent(o), "opacity={o} below 1.0 is translucent");
        assert_eq!(
            bg_quad_alpha(o),
            254,
            "translucent opacity={o} must stay below solid 255"
        );
    }
}

#[test]
fn renderer_background_opacity_roundtrips_and_clamps() {
    let mut r = aterm_render::Renderer::from_system(18.0, aterm_render::Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    assert_eq!(
        r.background_opacity(),
        1.0,
        "default is solid (byte-identical)"
    );
    r.set_background_opacity(0.4);
    assert!((r.background_opacity() - 0.4).abs() < 1e-6);
    r.set_background_opacity(2.0);
    assert_eq!(r.background_opacity(), 1.0, "over-range clamps to solid");
    r.set_background_opacity(-1.0);
    assert_eq!(
        r.background_opacity(),
        0.0,
        "under-range clamps to transparent"
    );
    r.set_background_opacity(f32::NAN);
    assert_eq!(r.background_opacity(), 1.0, "NaN fails safe to solid");
}
