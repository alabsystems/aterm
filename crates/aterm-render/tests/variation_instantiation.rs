// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for variable-font instantiation (W9): the SHIPPING
//! `aterm_render::variation` policies are total, clamped, and consistently
//! applied — and the system SF Mono actually rescues to Regular.
//!
//! The sin: macOS ships SF Mono as `SFNSMono.ttf`, a variable font whose
//! `fvar` DEFAULT instance is "SF NS Mono Light" (`wght` ≈ 294.67). Loaded at
//! its default instance it reads faint, which is why the candidate order
//! once demoted it below Menlo (with this fix proven, SF Mono now leads the
//! candidates). W9 instantiates the face at load — `Regular`
//! named instance, else `wght=400` clamped — and routes the ONE resolved
//! coordinate list to the CoreText descriptor, the rustybuzz shaper and the
//! metrics derivation.
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `VfAxisClamp` and `VfNudgeGate` derived models
//!   (`aterm_spec::derive::{vf_axis_clamp_model, vf_nudge_gate_model}`) carry
//!   the axis-bounds and nudge-precondition invariants; `cargo test -p
//!   aterm-spec` proves each at `Buggy=0` and REQUIRES a counterexample at
//!   `Buggy=1` (the pre-fix behaviours: no clamping / an ungated nudge).
//! * **Tier-1 (concrete, this file)** — the same invariants checked on the
//!   real policy functions over exhaustive lattices that additionally cover
//!   what the integer models cannot: NaN/±∞ totality, float boundary
//!   behaviour (the ty Expr language has no floats — the procedural
//!   rounding-law precedent), plus the SF Mono acceptance binding and the
//!   coord single-source consistency binding on a live `Renderer`.

use aterm_render::variation::{
    self, BOLD_WGHT, DARK_NUDGE_ADVANCE_TOL_PX, REGULAR_WGHT, VfAxis, WGHT_TAG, clamp_axis,
};

/// A deliberate lattice of axis-bound magnitudes (design-space units),
/// including the SF Mono Light default (294.67) and the OpenType extremes.
const BOUNDS: &[f32] = &[-100.0, 0.0, 1.0, 294.67, 400.0, 700.0, 1000.0];

/// Requests: every lattice bound, off-by-epsilon probes, and the non-finite
/// values a config typo / failed measurement could produce.
const REQUESTS: &[f32] = &[
    f32::NAN,
    f32::NEG_INFINITY,
    f32::INFINITY,
    -1e30,
    -100.0,
    -1.0,
    0.0,
    1.0,
    294.66,
    294.67,
    294.68,
    400.0,
    700.0,
    999.99,
    1000.0,
    1000.01,
    1e30,
];

/// PROVE (1) — named-instance/axis resolution is TOTAL: for every axis on
/// the bounds lattice (min <= def <= max, ttf-parser's normalization) and
/// EVERY request including NaN/±∞, `clamp_axis` yields a finite value inside
/// `[min, max]`, exactly the request whenever the request is already
/// in-bounds. Exhaustive over the lattice; the integer half is the ty-checked
/// `VfAxisClamp` model.
#[test]
fn clamp_axis_total_and_exact() {
    let mut clamped_low = 0usize;
    let mut clamped_high = 0usize;
    for &min in BOUNDS {
        for &def in BOUNDS {
            for &max in BOUNDS {
                if !(min <= def && def <= max) {
                    continue; // fvar axes are normalized at parse
                }
                let axis = VfAxis {
                    tag: WGHT_TAG,
                    min,
                    def,
                    max,
                };
                for &req in REQUESTS {
                    let out = clamp_axis(&axis, req);
                    assert!(
                        out.is_finite(),
                        "clamp_axis must be total: {min}/{def}/{max} req {req} -> {out}"
                    );
                    assert!(
                        (min..=max).contains(&out),
                        "axis bounds violated: {min}/{def}/{max} req {req} -> {out}"
                    );
                    if req.is_finite() {
                        if (min..=max).contains(&req) {
                            assert_eq!(out, req, "in-bounds request must be exact");
                        } else if req < min {
                            assert_eq!(out, min);
                            clamped_low += 1;
                        } else {
                            assert_eq!(out, max);
                            clamped_high += 1;
                        }
                    } else {
                        assert_eq!(out, def, "non-finite request resolves to the default");
                    }
                }
            }
        }
    }
    // NON-VACUITY: both clamp directions were genuinely exercised.
    assert!(clamped_low > 0 && clamped_high > 0, "vacuous lattice");
}

/// PROVE (1), degenerate-axis totality: a hand-built axis with non-finite
/// bounds (unreachable from ttf-parser, reachable for a caller constructing
/// `VfAxis` directly) still never panics and never returns NaN.
#[test]
fn clamp_axis_never_panics_on_degenerate_axes() {
    for &bad in &[f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for &req in REQUESTS {
            let axis = VfAxis {
                tag: WGHT_TAG,
                min: bad,
                def: 400.0,
                max: bad,
            };
            let out = clamp_axis(&axis, req);
            assert!(out.is_finite(), "degenerate axis must yield the finite def");
            let axis_all_bad = VfAxis {
                tag: WGHT_TAG,
                min: bad,
                def: bad,
                max: bad,
            };
            let out = clamp_axis(&axis_all_bad, req);
            assert!(out.is_finite(), "fully-degenerate axis yields 0.0, not NaN");
        }
    }
}

/// Default resolution: no `Regular` instance ⇒ `wght` pulls to 400 (clamped),
/// every other axis stays at its default; a `Regular` instance wins and is
/// clamped per-axis; a length-mismatched instance list is IGNORED (falls back
/// to the wght rule) — the malformed-fvar degradation path.
#[test]
fn resolve_default_coords_regular_else_wght400() {
    let axes = [
        VfAxis {
            tag: WGHT_TAG,
            min: 294.67,
            def: 294.67,
            max: 700.0,
        },
        VfAxis {
            tag: u32::from_be_bytes(*b"opsz"),
            min: 6.0,
            def: 13.0,
            max: 28.0,
        },
    ];
    // No Regular instance: wght -> 400 (the SF Mono rescue), opsz stays def.
    assert_eq!(
        variation::resolve_default_coords(&axes, None),
        vec![400.0, 13.0]
    );
    // Regular instance present: its coords, clamped onto each axis.
    assert_eq!(
        variation::resolve_default_coords(&axes, Some(&[400.0, 14.0])),
        vec![400.0, 14.0]
    );
    assert_eq!(
        variation::resolve_default_coords(&axes, Some(&[1200.0, -50.0])),
        vec![700.0, 6.0],
        "a malformed Regular instance can never escape the axis bounds"
    );
    // Wrong-arity instance: ignored, falls back to the wght rule.
    assert_eq!(
        variation::resolve_default_coords(&axes, Some(&[400.0])),
        vec![400.0, 13.0]
    );
    // NEGATIVE CONTROL (the sin): WITHOUT resolution, the default instance
    // is the Light weight — what pre-W9 aterm rendered.
    assert_eq!(axes[0].def, 294.67);
    assert_ne!(axes[0].def, REGULAR_WGHT);
    // A wght axis whose range cannot reach 400 clamps to its max.
    let thin = [VfAxis {
        tag: WGHT_TAG,
        min: 100.0,
        def: 100.0,
        max: 300.0,
    }];
    assert_eq!(variation::resolve_default_coords(&thin, None), vec![300.0]);
}

/// Config overlay: known tags clamp onto their axis, unknown tags are
/// ignored, later requests win (so `font_weight` overrides a `wght=` entry).
#[test]
fn apply_requests_clamps_and_ignores_unknown() {
    let axes = [VfAxis {
        tag: WGHT_TAG,
        min: 100.0,
        def: 400.0,
        max: 900.0,
    }];
    let base = variation::resolve_default_coords(&axes, None);
    assert_eq!(
        variation::apply_requests(&axes, base.clone(), &[(WGHT_TAG, 450.0)]),
        vec![450.0]
    );
    assert_eq!(
        variation::apply_requests(&axes, base.clone(), &[(WGHT_TAG, 5000.0)]),
        vec![900.0],
        "requests are clamped, never trusted"
    );
    let slnt = u32::from_be_bytes(*b"slnt");
    assert_eq!(
        variation::apply_requests(&axes, base.clone(), &[(slnt, -10.0)]),
        vec![400.0],
        "a tag with no axis is ignored"
    );
    assert_eq!(
        variation::apply_requests(&axes, base, &[(WGHT_TAG, 450.0), (WGHT_TAG, 500.0)]),
        vec![500.0],
        "later request wins (font_weight over font_variation)"
    );
}

/// `font_variation` spec parsing: accepted forms and every malformed shape.
#[test]
fn parse_variation_spec_grammar() {
    assert_eq!(
        variation::parse_variation_spec("wght=450"),
        Some((WGHT_TAG, 450.0))
    );
    assert_eq!(
        variation::parse_variation_spec(" wght = 450.5 "),
        Some((WGHT_TAG, 450.5))
    );
    // Short tags are space-padded to 4 (the OpenType convention).
    assert_eq!(
        variation::parse_variation_spec("wd=1"),
        Some((u32::from_be_bytes(*b"wd  "), 1.0))
    );
    for bad in [
        "",
        "wght",
        "wght=",
        "=400",
        "toolong=1",
        "wght=abc",
        "wght=NaN",
        "wght=inf",
        "wg ht=1",
    ] {
        assert_eq!(
            variation::parse_variation_spec(bad),
            None,
            "must reject {bad:?}"
        );
    }
}

/// PROVE (3) — the dark-nudge SAFETY GATE: permitted iff BOTH advances are
/// finite AND they agree within 0.25px. Exhaustive over an advance lattice
/// spanning the tolerance boundary, plus the non-finite failed-measurement
/// cases. The pre-fix behaviour (an unconditional nudge) is the negative
/// control: it would "apply" on the >0.25px rows this test proves are
/// rejected.
#[test]
fn dark_nudge_gate_is_a_checked_precondition() {
    let advs = [
        f32::NAN,
        f32::INFINITY,
        0.0,
        7.0,
        7.2,
        7.24,
        7.25,
        7.2501,
        7.5,
        14.0,
    ];
    let mut permitted = 0usize;
    let mut rejected = 0usize;
    for &a in &advs {
        for &b in &advs {
            let ok = variation::dark_nudge_permitted(a, b);
            let expect = a.is_finite() && b.is_finite() && (a - b).abs() <= 0.25;
            assert_eq!(ok, expect, "gate({a}, {b})");
            if ok { permitted += 1 } else { rejected += 1 }
        }
    }
    // NON-VACUITY: both verdicts genuinely reachable.
    assert!(permitted > 0 && rejected > 0);
    // Boundary is INCLUSIVE at exactly 0.25px…
    assert!(variation::dark_nudge_permitted(7.25, 7.0));
    assert_eq!(DARK_NUDGE_ADVANCE_TOL_PX, 0.25);
    // …and a failed measurement (NaN) can never pass: the gate is a
    // precondition, not a heuristic (NaN comparisons would be false-y anyway,
    // but the explicit finite check is what the model pins).
    assert!(!variation::dark_nudge_permitted(f32::NAN, 7.0));
    assert!(!variation::dark_nudge_permitted(7.0, f32::NAN));
}

/// PROVE (4) — SF MONO ACCEPTANCE: the system `SFNSMono.ttf` (a) really does
/// default to the Light instance (the audit's sin 7, asserted so this test
/// can never go vacuous on a future macOS that fixes the default), and (b)
/// resolves to `wght` ≈ 400 under W9's default instantiation. Skips cleanly
/// on hosts without the file (Linux, stripped macOS).
#[test]
fn sf_mono_resolves_to_regular_not_light() {
    let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") else {
        eprintln!("SKIP: no system SFNSMono.ttf on this host");
        return;
    };
    let probe = variation::probe(&bytes, 0).expect("SFNSMono is a variable font");
    let wi = probe
        .axes
        .iter()
        .position(|a| a.tag == WGHT_TAG)
        .expect("SFNSMono has a wght axis");
    let axis = probe.axes[wi];
    // The sin exists: the fvar default is the Light weight, NOT Regular.
    assert!(
        axis.def < 350.0,
        "SFNSMono default weight expected Light (<350), got {} — if Apple fixed \
         the default, update this control",
        axis.def
    );
    assert!(
        axis.max >= 400.0,
        "wght axis must reach Regular, got max {}",
        axis.max
    );
    // The fix: default instantiation lands on Regular (named instance or
    // the clamped 400).
    let coords = variation::resolve_default_coords(&probe.axes, probe.regular_coords.as_deref());
    assert!(
        (399.0..=401.0).contains(&coords[wi]),
        "resolved wght must be ~400, got {}",
        coords[wi]
    );
}

/// PROVE (2) — COORD CONSISTENCY, bound on a live `Renderer` over the real
/// SF Mono: the renderer's single `variation_coords()` list (the one source
/// the CT descriptor and the shaper receive) is exactly the pure resolution
/// of the probe, and the CELL WIDTH equals the ttf-parser advance measured
/// at those SAME coords — i.e. metrics derivation consumed the identical
/// list, not the default instance (which would desync the grid). Also pins
/// the real-bold instance (wght 700 clamped) for PROVE-adjacent (c).
#[test]
fn renderer_binds_one_coord_list_everywhere() {
    let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") else {
        eprintln!("SKIP: no system SFNSMono.ttf on this host");
        return;
    };
    let px = 32.0;
    let r = aterm_render::Renderer::from_bytes(&bytes, px, aterm_render::Theme::default())
        .expect("renderer builds from SFNSMono");
    let probe = variation::probe(&bytes, 0).expect("variable");
    let expect = variation::resolve_default_coords(&probe.axes, probe.regular_coords.as_deref());
    let coords = r
        .variation_coords()
        .expect("SFNSMono must be instantiated (coords differ from fvar defaults)");
    assert_eq!(coords.len(), probe.axes.len(), "one value per axis");
    for ((tag, v), (axis, e)) in coords.iter().zip(probe.axes.iter().zip(&expect)) {
        assert_eq!(*tag, axis.tag);
        assert!((v - e).abs() < 1e-3, "coord for {tag:#x}: {v} vs pure {e}");
    }
    // Metrics received the SAME coords: cell_w == round(advance at coords).
    let m = variation::varied_metrics_px(&bytes, 0, coords, px).expect("varied metrics");
    assert_eq!(
        r.cell_size().0,
        (m.m_advance.round() as usize).max(1),
        "cell width must derive from the instantiated advance"
    );
    // Real bold instance: wght pulled to 700 clamped, other axes unchanged.
    let bold = r
        .debug_vf_bold_coords()
        .expect("SFNSMono's wght axis reaches bold");
    let wi = probe
        .axes
        .iter()
        .position(|a| a.tag == WGHT_TAG)
        .expect("wght");
    let bold_w = clamp_axis(&probe.axes[wi], BOLD_WGHT);
    assert!((bold[wi].1 - bold_w).abs() < 1e-3, "bold wght = clamp(700)");
    for (i, &(tag, v)) in bold.iter().enumerate() {
        if i != wi {
            assert_eq!((tag, v), coords[i], "non-wght axes identical in bold");
        }
    }
}

/// The dark-nudge end to end on the real SF Mono (a mono VF that holds
/// advances constant, so the gate PASSES): a dark theme gets `base + nudge`,
/// a light theme gets the un-nudged base — and the cell geometry is
/// IDENTICAL either way (the gate's grid-stability half), so a live theme
/// flip provably cannot re-grid. Default `nudge = 0` leaves coords untouched.
#[test]
fn dark_nudge_applies_gated_and_geometry_stable() {
    let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") else {
        eprintln!("SKIP: no system SFNSMono.ttf on this host");
        return;
    };
    let dark = aterm_render::Theme::default(); // bg 0x111318 — dark
    assert!(aterm_render::theme_is_dark(dark.bg));
    let light = aterm_render::Theme {
        bg: 0x00FF_FFFF,
        ..dark
    };
    assert!(!aterm_render::theme_is_dark(light.bg));

    let mut r = aterm_render::Renderer::from_bytes(&bytes, 32.0, dark).expect("renderer");
    let probe = variation::probe(&bytes, 0).expect("variable");
    let wi = probe.axes.iter().position(|a| a.tag == WGHT_TAG).unwrap();
    let base_w = r.variation_coords().unwrap()[wi].1;
    let base_cell = r.cell_size();
    let base_baseline = r.baseline();

    // Nudge on, dark theme: wght rises by the nudge (clamped), geometry holds.
    assert!(r.set_font_variations(&[], 50.0), "nudge change must apply");
    let nudged_w = r.variation_coords().unwrap()[wi].1;
    let expect = clamp_axis(&probe.axes[wi], base_w + 50.0);
    assert!(
        (nudged_w - expect).abs() < 1e-3,
        "dark theme nudges wght: {base_w} -> {nudged_w} (expect {expect})"
    );
    assert_eq!(r.cell_size(), base_cell, "nudge may never change the grid");
    assert_eq!(r.baseline(), base_baseline);

    // Theme flips to light: the nudge lifts, geometry still identical.
    r.set_theme(light);
    let light_w = r.variation_coords().unwrap()[wi].1;
    assert!(
        (light_w - base_w).abs() < 1e-3,
        "light theme must not nudge: {light_w} vs base {base_w}"
    );
    assert_eq!(r.cell_size(), base_cell);
    assert_eq!(r.baseline(), base_baseline);

    // Nudge off again (coords are ALREADY un-nudged on the light theme, so
    // the setter reports no raster change — the free-no-op contract).
    assert!(!r.set_font_variations(&[], 0.0));
    assert!((r.variation_coords().unwrap()[wi].1 - base_w).abs() < 1e-3);
    // And flipping back to dark with the nudge OFF stays un-nudged.
    r.set_theme(dark);
    assert!((r.variation_coords().unwrap()[wi].1 - base_w).abs() < 1e-3);
}

/// The instantiation reaches the actual CoreText RASTER (macOS): at the same
/// px, the W9-resolved Regular instance of SF Mono carries measurably MORE
/// ink than the fvar-default Light instance (pinned by steering the config
/// request back to the axis default, which collapses `variation_coords` to
/// `None` — the pre-W9 raster), and the W9 real-bold instance carries more
/// ink than Regular (replacing synthetic dilation). This is the end-to-end
/// proof that `CTFontDescriptorCreateCopyWithVariation` genuinely reshapes
/// the outlines — not just the metrics book-keeping.
#[cfg(target_os = "macos")]
#[test]
fn sf_mono_ct_raster_ink_tracks_the_instance() {
    if std::env::var_os("ATERM_RASTERIZER").is_some_and(|v| v == "fontdue") {
        eprintln!("SKIP: fontdue rasterizer forced (no variation support there by design)");
        return;
    }
    let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") else {
        eprintln!("SKIP: no system SFNSMono.ttf on this host");
        return;
    };
    let px = 32.0;
    let theme = aterm_render::Theme::default();
    let ink = |r: &mut aterm_render::Renderer, style: aterm_render::StyleBits| -> u64 {
        let key = r.glyph_key_styled('M', style);
        match r.glyph_image(key) {
            aterm_render::GlyphImage::Mono { bytes, .. } => {
                bytes.iter().map(|&b| u64::from(b)).sum()
            }
            _ => 0,
        }
    };
    let regular = aterm_render::StyleBits::REGULAR;
    let mut r = aterm_render::Renderer::from_bytes(&bytes, px, theme).expect("renderer");
    assert!(r.variation_coords().is_some(), "instantiated by default");
    let ink_regular = ink(&mut r, regular);
    let ink_bold = ink(&mut r, aterm_render::StyleBits::BOLD);

    // Steer the request to the axis default (Light): coords == defaults ⇒
    // `None` ⇒ the pre-W9 default-instance raster.
    let probe = variation::probe(&bytes, 0).expect("variable");
    let light: Vec<(u32, f32)> = probe.axes.iter().map(|a| (a.tag, a.def)).collect();
    let mut r_light = aterm_render::Renderer::from_bytes(&bytes, px, theme).expect("renderer");
    r_light.set_font_variations(&light, 0.0);
    assert!(
        r_light.variation_coords().is_none(),
        "default-instance request collapses to the unvaried path"
    );
    let ink_light = ink(&mut r_light, regular);

    assert!(ink_regular > 0 && ink_light > 0 && ink_bold > 0, "real ink");
    assert!(
        ink_regular > ink_light,
        "Regular (wght≈400) must out-ink the Light default: {ink_regular} vs {ink_light}"
    );
    assert!(
        ink_bold > ink_regular,
        "the real bold instance must out-ink Regular: {ink_bold} vs {ink_regular}"
    );
}

/// `font_variation`/`font_weight` requests reach the renderer, clamped; a
/// non-variable primary (the embedded DejaVu) stays `None` forever — the
/// byte-identical default path.
#[test]
fn config_requests_apply_and_nonvariable_stays_default() {
    // Non-variable: the bundled DejaVu Sans Mono.
    let dejavu = include_bytes!("../assets/DejaVuSansMono.ttf");
    let mut r = aterm_render::Renderer::from_bytes(dejavu, 24.0, aterm_render::Theme::default())
        .expect("embedded face builds");
    assert!(r.variation_coords().is_none(), "non-VF: no instantiation");
    assert!(r.debug_vf_bold_coords().is_none());
    assert!(
        !r.set_font_variations(&[(WGHT_TAG, 700.0)], 0.0) || r.variation_coords().is_none(),
        "requests on a non-VF face must not fabricate coords"
    );
    assert!(r.variation_coords().is_none());

    // Variable: requests overlay the default resolution, clamped.
    let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") else {
        eprintln!("SKIP: no system SFNSMono.ttf on this host");
        return;
    };
    let mut r = aterm_render::Renderer::from_bytes(&bytes, 24.0, aterm_render::Theme::default())
        .expect("renderer");
    let probe = variation::probe(&bytes, 0).expect("variable");
    let wi = probe.axes.iter().position(|a| a.tag == WGHT_TAG).unwrap();
    assert!(r.set_font_variations(&[(WGHT_TAG, 510.0)], 0.0));
    let w = r.variation_coords().unwrap()[wi].1;
    assert!(
        (w - clamp_axis(&probe.axes[wi], 510.0)).abs() < 1e-3,
        "font_weight request must land clamped, got {w}"
    );
    // Same request again: a free no-op (hot-reload discipline).
    assert!(!r.set_font_variations(&[(WGHT_TAG, 510.0)], 0.0));
}

/// W9 perf regression: the resident `rustybuzz`-variation cache (borrowed by
/// the per-row-per-frame shaping path, NOT reallocated each row) stays in
/// lock-step with `variation_coords()` across every coord transition —
/// including the awkward variable→non-variable primary swap where
/// `refresh_variations` early-outs (`None == None`) and must NOT leave the old
/// instance's variations lingering. Guards the caching optimisation against a
/// stale-cache correctness regression.
#[test]
fn rb_variation_cache_tracks_coords() {
    // Non-variable primary: coords None ⇒ cache empty.
    let dejavu = include_bytes!("../assets/DejaVuSansMono.ttf");
    let r = aterm_render::Renderer::from_bytes(dejavu, 24.0, aterm_render::Theme::default())
        .expect("embedded face builds");
    assert!(r.variation_coords().is_none());
    assert!(
        r.debug_rb_variation_cache().is_empty(),
        "non-VF: the rb-variation cache must be empty"
    );

    // Variable primary (system SF Mono): coords Some ⇒ cache mirrors them 1:1.
    let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") else {
        eprintln!("SKIP: no system SFNSMono.ttf on this host");
        return;
    };
    let mut r = aterm_render::Renderer::from_bytes(&bytes, 24.0, aterm_render::Theme::default())
        .expect("renderer");
    assert!(r.set_font_variations(&[(WGHT_TAG, 510.0)], 0.0));
    let coords = r.variation_coords().expect("instantiated").to_vec();
    assert!(!coords.is_empty(), "instantiated VF has coords");
    assert_eq!(
        r.debug_rb_variation_cache(),
        coords,
        "cache must mirror the resolved coords 1:1 after a coord change"
    );

    // The smoking-gun transition: swap the primary to a NON-variable face.
    // `refresh_variations` early-outs on None==None, so unless the cache is
    // cleared eagerly it would keep SF Mono's variations on DejaVu.
    r.set_primary_font(dejavu).expect("swap to embedded face");
    assert!(r.variation_coords().is_none(), "non-VF after swap");
    assert!(
        r.debug_rb_variation_cache().is_empty(),
        "stale-cache regression: old instance's variations must be cleared"
    );
}

/// A variable MONO face installed on this host, or `None`. There is no variable
/// fixture committed to the tree — both committed faces (DejaVu Sans Mono and the
/// JetBrains Mono fixture) are static, which is exactly what makes them the anchor
/// for the `None` case in [`nonvariable_bold_still_takes_the_synthetic_path`]. So
/// the portable bold-instance proof has to borrow a real `wght` axis from the host
/// and skip when the host has none.
#[cfg(not(target_os = "macos"))]
fn host_variable_mono() -> Option<Vec<u8>> {
    // Cascadia Mono is aterm's Windows default primary and ships with Windows 11 as
    // a VARIABLE face (`wght` 200..400..700) — the whole reason this gap was worth
    // closing. The rest are the usual Linux placements of the same family.
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\CascadiaMono.ttf",
        r"C:\Windows\Fonts\CascadiaCode.ttf",
        "/usr/share/fonts/truetype/cascadia-code/CascadiaMono.ttf",
        "/usr/share/fonts/truetype/cascadia-code/CascadiaCode.ttf",
        "/usr/share/fonts/cascadia-code/CascadiaMono.ttf",
    ];
    CANDIDATES.iter().find_map(|p| {
        let bytes = std::fs::read(p).ok()?;
        // "Variable" is not enough: the file must actually carry a `wght` axis,
        // since that is the only axis a bold instance can be built on.
        variation::probe(&bytes, 0)?
            .axes
            .iter()
            .any(|a| a.tag == WGHT_TAG)
            .then_some(bytes)
    })
}

/// Synthetic BOLD as `apply_synthetic_style` performs it: a horizontal max-dilation
/// by `e` px that widens the bitmap and leaves the advance alone. Reproduced here
/// rather than reached through the crate, so the assertions below are statements
/// about OBSERVABLE pixels — "the bold cell IS / IS NOT the dilation of the regular
/// one" — that stay honest if the private helper is renamed or re-tuned.
#[cfg(not(target_os = "macos"))]
fn dilate(cov: &[u8], w: usize, h: usize, e: usize) -> Vec<u8> {
    let nw = w + e;
    let mut out = vec![0u8; nw * h];
    for y in 0..h {
        for x in 0..nw {
            let mut m = 0u8;
            for k in 0..=e {
                if let Some(i) = x.checked_sub(k)
                    && i < w
                {
                    m = m.max(cov[y * w + i]);
                }
            }
            out[y * nw + x] = m;
        }
    }
    out
}

/// The portable twin of [`sf_mono_ct_raster_ink_tracks_the_instance`]: OFF macOS, a
/// BOLD cell on a variable primary draws the REAL `wght`≈700 instance through
/// [`variation::varied_glyph_raster`], not a synthetic dilation of the regular one.
///
/// This is the regression that shipped. The W9 resolver is platform-independent, so
/// `vf_bold_coords` was computed correctly on Windows and then thrown away by two
/// `#[cfg(not(target_os = "macos"))]` stubs — one in `vf_bold_mono_raster` (the
/// by-CHAR arm), one in the by-GLYPH-ID arm that plain primary text actually takes.
/// Every SGR-bold cell fell back to `embolden`, a horizontal max-dilation that keeps
/// the ADVANCE, so at a 7px Windows cell a 1px stem became a 2px stem bleeding into
/// the neighbouring column — "command" reading as "commmand".
///
/// Both arms are checked, because patching only one leaves bold broken for whatever
/// the other draws. Byte equality against a freshly-instantiated reference raster is
/// the proof (the stem LUT is the identity at the default `stem_gamma = 1.0`, so the
/// renderer's coverage is that raster verbatim); the `assert_ne!` against the
/// dilation then states the negative outright, so a revert fails loudly instead of
/// merely differing.
#[cfg(not(target_os = "macos"))]
#[test]
fn portable_bold_draws_the_real_instance_not_a_dilation() {
    let Some(bytes) = host_variable_mono() else {
        eprintln!("SKIP: no variable mono face installed on this host");
        return;
    };
    if std::env::var_os("ATERM_STEM_GAMMA").is_some() {
        // A non-identity LUT warps the antialiased fringe of the renderer's
        // coverage but not of the raw reference raster below, so byte equality
        // would be comparing two different transforms.
        eprintln!("SKIP: ATERM_STEM_GAMMA set (coverage is not the raw raster)");
        return;
    }
    let px = 16.0;
    let theme = aterm_render::Theme::default();
    let mut r = aterm_render::Renderer::from_bytes(&bytes, px, theme).expect("renderer");
    let bold_coords = r
        .debug_vf_bold_coords()
        .expect("a wght axis reaching 700 yields a bold instance")
        .to_vec();

    // The regular key is id-addressed, so it hands us the primary gid for free.
    let reg = r.glyph_key('M');
    assert_eq!(reg.glyph_class, aterm_render::GlyphClass::MonoGid);
    let gid = reg.ch_or_id as u16;
    let (ww, wh, wxmin, wymin, _wadv, want) =
        variation::varied_glyph_raster(&bytes, 0, &bold_coords, gid, px)
            .expect("the bold instance rasterizes");

    // ARM 1 — by CHAR (`vf_bold_mono_raster`): a styled primary glyph is
    // re-addressed by char so a real bold sibling FILE could serve it. This
    // renderer was built `from_bytes`, so it has no primary path and hence no
    // sibling — the instance is the only real bold available.
    let bold_char = r.glyph_key_styled('M', aterm_render::StyleBits::BOLD);
    assert_eq!(
        bold_char.glyph_class,
        aterm_render::GlyphClass::Mono,
        "a styled primary glyph is char-addressed"
    );
    let img_char = r.glyph_image(bold_char).clone();
    assert_eq!(
        (
            img_char.width(),
            img_char.height(),
            img_char.xmin(),
            img_char.ymin()
        ),
        (ww, wh, wxmin, wymin),
        "by-char bold must carry the bold INSTANCE's metrics"
    );
    assert_eq!(
        img_char.bytes(),
        &want[..],
        "by-char bold must be the bold instance's coverage, byte for byte"
    );

    // ARM 2 — by GLYPH ID, the arm plain primary text takes. `ligature_key` builds
    // exactly the `mono_gid` + style key the row planner emits for a bold run.
    let bold_gid = r.ligature_key(gid, aterm_render::StyleBits::BOLD);
    let img_gid = r.glyph_image(bold_gid).clone();
    assert_eq!(
        (img_gid.width(), img_gid.height()),
        (ww, wh),
        "by-gid bold must carry the bold INSTANCE's metrics"
    );
    assert_eq!(
        img_gid.bytes(),
        &want[..],
        "by-gid bold must be the bold instance's coverage, byte for byte"
    );

    // The negative: bold must not BE the dilation, must not exceed its footprint
    // (that widening under an unchanged advance is precisely the cell bleed), and
    // must still be genuinely heavier than regular — a "real instance" that lost
    // weight would be a different bug wearing this fix's clothes.
    let reg_img = r.glyph_image(reg).clone();
    let e = (px / 18.0).round().max(1.0) as usize;
    let synthetic = dilate(reg_img.bytes(), reg_img.width(), reg_img.height(), e);
    assert_ne!(
        img_char.bytes(),
        &synthetic[..],
        "bold must not be the synthetic dilation of regular"
    );
    assert!(
        img_char.width() <= reg_img.width() + e,
        "the real instance must not exceed the dilation's footprint: {} vs {}",
        img_char.width(),
        reg_img.width() + e
    );
    let ink = |b: &[u8]| -> u64 { b.iter().map(|&c| u64::from(c)).sum() };
    assert!(
        ink(img_char.bytes()) > ink(reg_img.bytes()),
        "a real 700-weight glyph must out-ink the 400: {} vs {}",
        ink(img_char.bytes()),
        ink(reg_img.bytes())
    );
}

/// The `None` case, which the fix must PRESERVE: a face with no usable `wght` axis
/// (Consolas, Courier New, the bundled DejaVu — most static fonts) has no bold
/// instance to draw, so both arms must still fall through to the synthetic
/// dilation. The contract is "use the real instance WHEN one exists", never "assume
/// one exists".
///
/// Pinned on the committed DejaVu so it runs on every host, and on BOTH arms, since
/// each has its own `vf_bold_*` early-out to get wrong.
#[cfg(not(target_os = "macos"))]
#[test]
fn nonvariable_bold_still_takes_the_synthetic_path() {
    if std::env::var_os("ATERM_STEM_GAMMA").is_some() {
        eprintln!("SKIP: ATERM_STEM_GAMMA set (coverage is not the raw raster)");
        return;
    }
    let dejavu = include_bytes!("../assets/DejaVuSansMono.ttf");
    let px = 16.0;
    let mut r = aterm_render::Renderer::from_bytes(dejavu, px, aterm_render::Theme::default())
        .expect("embedded face builds");
    assert!(
        r.debug_vf_bold_coords().is_none(),
        "a static face offers no bold instance"
    );

    let reg = r.glyph_key('M');
    let reg_img = r.glyph_image(reg).clone();
    let e = (px / 18.0).round().max(1.0) as usize;
    let synthetic = dilate(reg_img.bytes(), reg_img.width(), reg_img.height(), e);

    let bold_char = r.glyph_key_styled('M', aterm_render::StyleBits::BOLD);
    let img_char = r.glyph_image(bold_char).clone();
    assert_eq!(
        img_char.width(),
        reg_img.width() + e,
        "by-char bold on a static face stays the dilation"
    );
    assert_eq!(img_char.bytes(), &synthetic[..], "…and IS that dilation");

    let bold_gid = r.ligature_key(reg.ch_or_id as u16, aterm_render::StyleBits::BOLD);
    let img_gid = r.glyph_image(bold_gid).clone();
    assert_eq!(
        img_gid.width(),
        reg_img.width() + e,
        "by-gid bold on a static face stays the dilation"
    );
    assert_eq!(img_gid.bytes(), &synthetic[..], "…and IS that dilation");
}
