// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the M5 true-vibrancy INK-OPACITY guarantee: the
//! background stream honours `background_opacity`, but the glyph / decoration /
//! image (INK) stream stays FULLY OPAQUE under every opacity setting — so the
//! desktop shows through the terminal's background without ever bleeding through
//! its text (the iTerm2-blur / ghostty-`background-blur` failure mode).
//!
//! ## The invariant (the PROVE bullet #2)
//!
//! For every `opacity` on a dense lattice over `[0, 1]`:
//!   * [`vibrancy::ink_quad_alpha`] `== 255` — ALWAYS opaque, independent of `o`.
//!   * [`vibrancy::bg_quad_alpha`] `== round(clamp(o) * 255)` — `< 255` iff the
//!     window is translucent, `== 255` at the solid default, monotone in `o`.
//!
//! The `o * 255` product is outside the `ty` `Expr` language (no `*`), so this is
//! the documented LATTICE waiver (mirroring the box-drawing rounding law); the
//! COMPANION ordering guarantee (translucency ⇒ WCAG-AA contrast floor) is the
//! `ty` model `aterm_spec::derive::vibrancy_contrast_model`. House style: a
//! NON-VACUITY control (the bg alpha genuinely drops) and a NEGATIVE control
//! (the pre-fix defect where ink would inherit the translucency) are asserted.
//!
//! Also binds the shipping `Renderer::set_background_opacity` / `background_opacity`
//! accessor pair to the same clamp domain (Tier-1 real-code conformance).

use aterm_render::vibrancy::{INK_ALPHA, bg_quad_alpha, ink_quad_alpha, is_translucent};

/// A dense opacity lattice over `[0, 1]` plus the out-of-domain guards.
fn opacity_lattice() -> Vec<f32> {
    let mut v: Vec<f32> = (0..=100).map(|i| i as f32 / 100.0).collect();
    // Out-of-domain / degenerate inputs the resolvers must fail safe on.
    v.extend_from_slice(&[-0.5, 1.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY]);
    v
}

#[test]
fn ink_stays_opaque_under_every_opacity() {
    let mut saw_translucent_bg = false;
    for &o in &opacity_lattice() {
        // THE INK INVARIANT: never translucent, for ANY opacity setting.
        assert_eq!(
            ink_quad_alpha(o),
            255,
            "ink quad went translucent at opacity={o} — text would sink into the desktop"
        );
        assert_eq!(INK_ALPHA, 255);

        let a = bg_quad_alpha(o);
        // The bg quad is opaque IFF the window is solid; translucent otherwise.
        if is_translucent(o) {
            assert!(
                a < 255,
                "translucent window (opacity={o}) must yield a bg alpha < 255, got {a}"
            );
            // And the INK is still strictly more opaque than the background it
            // sits over — the whole point of the split stream.
            assert!(
                ink_quad_alpha(o) > a,
                "ink must out-opaque the bg at opacity={o} (ink={}, bg={a})",
                ink_quad_alpha(o)
            );
            saw_translucent_bg = true;
        } else {
            assert_eq!(
                a, 255,
                "solid / out-of-domain opacity={o} must yield an opaque (255) bg, got {a}"
            );
        }
    }

    // NON-VACUOUS: the lattice actually reaches translucent backgrounds (the
    // guarantee is not trivially "everything is 255").
    assert!(
        saw_translucent_bg,
        "no translucent bg in the lattice — the ink/bg split would be vacuous"
    );
}

#[test]
fn bg_alpha_is_rounded_and_monotone() {
    // Exact endpoints + rounding at the boundaries.
    assert_eq!(bg_quad_alpha(1.0), 255, "solid");
    assert_eq!(bg_quad_alpha(0.0), 0, "fully transparent");
    assert_eq!(bg_quad_alpha(0.5), 128, "half → round(127.5)=128");
    // round(0.5*255+0.5)=128; the byte-exact rounding the present path uses.
    assert_eq!(bg_quad_alpha(0.25), 64, "quarter → round(63.75)=64");

    // Monotone non-decreasing across the in-domain lattice.
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
}

#[test]
fn negative_control_a_translucent_ink_would_be_caught() {
    // The pre-fix DEFECT reproduced: if the ink stream had (wrongly) inherited the
    // background opacity, its alpha at opacity=0.3 would be bg_quad_alpha(0.3),
    // NOT 255. Assert the shipping ink policy does NOT do that — the exact bug the
    // guarantee excludes.
    let buggy_translucent_ink = bg_quad_alpha(0.3);
    assert!(buggy_translucent_ink < 255, "control precondition");
    assert_ne!(
        ink_quad_alpha(0.3),
        buggy_translucent_ink,
        "ink must NOT inherit the background's translucency (the pre-fix defect)"
    );
    assert_eq!(ink_quad_alpha(0.3), 255);
}

#[test]
fn renderer_background_opacity_roundtrips_and_clamps() {
    // Tier-1: the SHIPPING renderer accessor clamps to the same [0,1] domain the
    // policy assumes, and defaults to solid.
    let Some(mut r) = aterm_render::Renderer::from_system(18.0, aterm_render::Theme::default())
    else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
