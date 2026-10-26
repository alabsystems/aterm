// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance tests for variable-font instantiation and SF Mono Regular rescue.

use aterm_render::variation::{self, VfAxis, WGHT_TAG, clamp_axis};

const BOUNDS: &[f32] = &[-100.0, 0.0, 1.0, 294.67, 400.0, 700.0, 1000.0];

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

#[test]
fn clamp_axis_total_and_exact() {
    let mut clamped_low = 0usize;
    let mut clamped_high = 0usize;
    for &min in BOUNDS {
        for &def in BOUNDS {
            for &max in BOUNDS {
                if !(min <= def && def <= max) {
                    continue;
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
    assert!(clamped_low > 0 && clamped_high > 0, "vacuous lattice");
}

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
    assert_eq!(
        variation::resolve_default_coords(&axes, None),
        vec![400.0, 13.0]
    );
    assert_eq!(
        variation::resolve_default_coords(&axes, Some(&[400.0, 14.0])),
        vec![400.0, 14.0]
    );
    assert_eq!(
        variation::resolve_default_coords(&axes, Some(&[1200.0, -50.0])),
        vec![700.0, 6.0],
        "a malformed Regular instance can never escape the axis bounds"
    );
    assert_eq!(
        variation::resolve_default_coords(&axes, Some(&[400.0])),
        vec![400.0, 13.0]
    );
    let thin = [VfAxis {
        tag: WGHT_TAG,
        min: 100.0,
        def: 100.0,
        max: 300.0,
    }];
    assert_eq!(variation::resolve_default_coords(&thin, None), vec![300.0]);
}

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
    assert!(permitted > 0 && rejected > 0);
    assert!(variation::dark_nudge_permitted(7.25, 7.0));
    assert!(!variation::dark_nudge_permitted(f32::NAN, 7.0));
    assert!(!variation::dark_nudge_permitted(7.0, f32::NAN));
}

/// The system SF Mono, a variable face every macOS ships.
#[cfg(target_os = "macos")]
fn sf_mono() -> Vec<u8> {
    std::fs::read("/System/Library/Fonts/SFNSMono.ttf").expect("macOS ships SFNSMono.ttf")
}

fn variable_face() -> Vec<u8> {
    include_bytes!("../assets/game/Cinzel-var.ttf").to_vec()
}

/// Coord consistency, bound on a live `Renderer` over the real
/// SF Mono: the renderer's single `variation_coords()` list (the one source
/// the CT descriptor and the shaper receive) is exactly the pure resolution
/// of the probe, and the cell width equals the aterm-font advance measured
/// at those same coords — i.e. metrics derivation consumed the identical
/// list, not the default instance (which would desync the grid).
#[cfg(target_os = "macos")]
#[test]
fn renderer_binds_one_coord_list_everywhere() {
    let bytes = sf_mono();
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
    let m = variation::varied_metrics_px(&bytes, 0, coords, px).expect("varied metrics");
    assert_eq!(
        r.cell_size().0,
        (m.m_advance.round() as usize).max(1),
        "cell width must derive from the instantiated advance"
    );
    // Its bold is the Bold its designer drew (`wght` 683 on its own `YAXS`), the face Settings
    // names "SF Mono Bold", not `wght` 700 on the Regular's other axes.
    let bold = r
        .debug_vf_bold_coords()
        .expect("SFNSMono's wght axis reaches bold");
    let designed = probe
        .bold_coords
        .as_deref()
        .expect("SF Mono designs a Bold named instance");
    assert_eq!(bold.len(), probe.axes.len(), "one value per axis in bold");
    for ((tag, v), (axis, want)) in bold.iter().zip(probe.axes.iter().zip(designed)) {
        assert_eq!(*tag, axis.tag);
        assert!(
            (v - want).abs() < 1e-3,
            "bold draws SF Mono's Bold: {tag:#x} {v} vs {want}"
        );
    }
}

/// The dark-nudge end to end on the real SF Mono (a mono VF that holds
/// advances constant, so the gate PASSES): a dark theme gets `base + nudge`,
/// a light theme gets the un-nudged base — and the cell geometry is
/// IDENTICAL either way (the gate's grid-stability half), so a live theme
/// flip provably cannot re-grid.
#[cfg(target_os = "macos")]
#[test]
fn dark_nudge_applies_gated_and_geometry_stable() {
    let bytes = sf_mono();
    let dark = aterm_render::Theme::default();
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

    assert!(r.set_font_variations(&[], 50.0), "nudge change must apply");
    let nudged_w = r.variation_coords().unwrap()[wi].1;
    let expect = clamp_axis(&probe.axes[wi], base_w + 50.0);
    assert!(
        (nudged_w - expect).abs() < 1e-3,
        "dark theme nudges wght: {base_w} -> {nudged_w} (expect {expect})"
    );
    assert_eq!(r.cell_size(), base_cell, "nudge may never change the grid");
    assert_eq!(r.baseline(), base_baseline);

    r.set_theme(light);
    let light_w = r.variation_coords().unwrap()[wi].1;
    assert!(
        (light_w - base_w).abs() < 1e-3,
        "light theme must not nudge: {light_w} vs base {base_w}"
    );
    assert_eq!(r.cell_size(), base_cell);
    assert_eq!(r.baseline(), base_baseline);

    assert!(!r.set_font_variations(&[], 0.0));
    assert!((r.variation_coords().unwrap()[wi].1 - base_w).abs() < 1e-3);
    r.set_theme(dark);
    assert!((r.variation_coords().unwrap()[wi].1 - base_w).abs() < 1e-3);
}

/// The instantiation reaches the actual CoreText raster (macOS): at the same
/// px, the resolved Regular instance of SF Mono carries measurably more
/// ink than the fvar-default Light instance (pinned by steering the config
/// request back to the axis default, which collapses `variation_coords` to
/// `None`), and the real-bold instance carries more
/// ink than Regular (replacing synthetic dilation).
#[cfg(target_os = "macos")]
#[test]
fn sf_mono_ct_raster_ink_tracks_the_instance() {
    // A developer who pins the portable rasterizer has no CoreText raster for
    // this test to read, by their own choice.
    if std::env::var_os("ATERM_RASTERIZER").is_some_and(|v| v == "portable" || v == "fontdue") {
        aterm_spec::host::could_not_run("ATERM_RASTERIZER pins the portable rasterizer here");
    }
    let bytes = sf_mono();
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

#[test]
fn config_requests_apply_and_nonvariable_stays_default() {
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

    let bytes = variable_face();
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
    assert!(!r.set_font_variations(&[(WGHT_TAG, 510.0)], 0.0));
}

#[test]
fn shaping_variation_cache_tracks_coords() {
    let dejavu = include_bytes!("../assets/DejaVuSansMono.ttf");
    let r = aterm_render::Renderer::from_bytes(dejavu, 24.0, aterm_render::Theme::default())
        .expect("embedded face builds");
    assert!(r.variation_coords().is_none());
    assert!(
        r.debug_shaping_variations().is_empty(),
        "non-VF: the shaping-variation cache must be empty"
    );

    let bytes = variable_face();
    let mut r = aterm_render::Renderer::from_bytes(&bytes, 24.0, aterm_render::Theme::default())
        .expect("renderer");
    assert!(r.set_font_variations(&[(WGHT_TAG, 510.0)], 0.0));
    let coords = r.variation_coords().expect("instantiated").to_vec();
    assert!(!coords.is_empty(), "instantiated VF has coords");
    assert_eq!(
        r.debug_shaping_variations(),
        coords,
        "cache must mirror the resolved coords 1:1 after a coord change"
    );

    r.set_primary_font(dejavu).expect("swap to embedded face");
    assert!(r.variation_coords().is_none(), "non-VF after swap");
    assert!(
        r.debug_shaping_variations().is_empty(),
        "stale-cache regression: old instance's variations must be cleared"
    );
}

/// A variable MONO face, checked in: Cascadia Mono 2407.24's variable build
/// (`ttf/CascadiaMono.ttf` of the release's `CascadiaCode-2407.24.zip`, sha256
/// `08026f61…59fa2`; `wght` 200..400..700, OFL-1.1 beside it in `assets/test/`),
/// the face `font_family = "Cascadia Mono"` names on Windows 11.
#[cfg(not(target_os = "macos"))]
const VARIABLE_MONO: &[u8] = include_bytes!("../assets/test/CascadiaMono-Variable.ttf");

/// Synthetic BOLD as `apply_synthetic_style` performs it: a horizontal max-dilation
/// by `e` px that widens the bitmap and leaves the advance alone.
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

/// The portable twin of [`sf_mono_ct_raster_ink_tracks_the_instance`]: off macOS, a
/// bold cell on a variable primary draws the real `wght`≈700 instance through
/// [`variation::varied_glyph_raster`], not a synthetic dilation of the regular one.
#[cfg(not(target_os = "macos"))]
#[test]
fn portable_bold_draws_the_real_instance_not_a_dilation() {
    let bytes = VARIABLE_MONO.to_vec();
    assert!(
        variation::probe(&bytes, 0).is_some_and(|p| p.axes.iter().any(|a| a.tag == WGHT_TAG)),
        "the checked-in variable mono face carries a `wght` axis"
    );
    let px = 16.0;
    let theme = aterm_render::Theme::default();
    let mut r = aterm_render::Renderer::from_bytes(&bytes, px, theme).expect("renderer");
    r.set_font_hinting("off");
    let bold_coords = r
        .debug_vf_bold_coords()
        .expect("a wght axis reaching 700 yields a bold instance")
        .to_vec();

    let reg = r.glyph_key('M');
    assert_eq!(reg.glyph_class, aterm_render::GlyphClass::MonoGid);
    let gid = reg.ch_or_id as u16;
    let (ww, wh, wxmin, wymin, _wadv, want) =
        variation::varied_glyph_raster(&bytes, 0, &bold_coords, gid, px)
            .expect("the bold instance rasterizes");

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

/// A face with no usable `wght` axis
/// (Consolas, Courier New, the bundled DejaVu — most static fonts) has no bold
/// instance to draw, so both arms must still fall through to the synthetic
/// dilation.
#[cfg(not(target_os = "macos"))]
#[test]
fn nonvariable_bold_still_takes_the_synthetic_path() {
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
