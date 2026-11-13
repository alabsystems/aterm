// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{dim_toward_bg_with, faint_target, srgb_to_linear};
use aterm_types::Rgb;

fn dim_toward_bg(fg: Rgb, bg: Rgb, opacity: f32) -> Rgb {
    dim_toward_bg_with(fg, bg, opacity, true)
}

#[test]
fn the_platform_picks_its_faint_shape() {
    let (white, page) = (Rgb::new(255, 255, 255), Rgb::new(0x11, 0x13, 0x18));
    let t = aterm_types::DIM_FACTOR;
    assert_eq!(
        super::dim_toward_bg(white, page, t),
        dim_toward_bg_with(white, page, t, cfg!(windows))
    );
    assert_eq!(
        dim_toward_bg_with(white, page, t, false),
        blend(white, page, t)
    );
    assert_ne!(
        dim_toward_bg_with(white, page, t, true),
        blend(white, page, t)
    );
    let colors = lattice();
    for &fg in &colors {
        for &bg in &colors {
            let (lf, lb) = (luminance(fg), luminance(bg));
            let full = contrast(fg, bg);
            for t in INTERIOR {
                let dim = dim_toward_bg_with(fg, bg, t, false);
                let c = contrast(dim, bg);
                assert!(c <= full * 1.01 + 0.01, "fg={fg:?} bg={bg:?} t={t}");
                let ld = luminance(dim);
                assert!(
                    (ld - lb).abs() >= t * (lf - lb).abs() - 0.005,
                    "fg={fg:?} bg={bg:?} t={t}: the lerp shape mixes at least t"
                );
                assert!(
                    c >= faint_target(full, t) * 0.97 - 0.01,
                    "fg={fg:?} bg={bg:?} t={t}"
                );
            }
        }
    }
}

fn luminance(c: Rgb) -> f32 {
    0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b)
}

fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

const OPACITIES: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

const INTERIOR: [f32; 3] = [0.25, 0.5, 0.75];

fn lattice() -> Vec<Rgb> {
    let vals: Vec<u8> = (0..8u16).map(|i| (i * 255 / 7) as u8).collect();
    let mut colors = Vec::with_capacity(512);
    for &r in &vals {
        for &g in &vals {
            for &b in &vals {
                colors.push(Rgb { r, g, b });
            }
        }
    }
    colors
}

fn blend(fg: Rgb, bg: Rgb, m: f32) -> Rgb {
    let channel = |f: u8, b: u8| {
        let (lf, lb) = (srgb_to_linear(f), srgb_to_linear(b));
        super::linear_to_srgb(lb + (lf - lb) * m)
    };
    Rgb {
        r: channel(fg.r, bg.r),
        g: channel(fg.g, bg.g),
        b: channel(fg.b, bg.b),
    }
}

fn start_mix(fg: Rgb, bg: Rgb, t: f32) -> f32 {
    let (lf, lb) = (luminance(fg), luminance(bg));
    if lf > lb && t > 0.0 && t < 1.0 {
        super::srgb_blend_mix(super::transfer_tables(), fg, bg, (lf, lb), t).clamp(0.0, t)
    } else {
        t
    }
}

fn srgb_blend_luminance(fg: Rgb, bg: Rgb, t: f32) -> f32 {
    let channel = |f: u8, b: u8| {
        let (f, b) = (f32::from(f), f32::from(b));
        super::encoded_unit_to_linear((b + (f - b) * t) / 255.0)
    };
    0.2126 * channel(fg.r, bg.r) + 0.7152 * channel(fg.g, bg.g) + 0.0722 * channel(fg.b, bg.b)
}

fn faint_with(fg: Rgb, bg: Rgb, t: f32, target: impl Fn(f32, f32) -> f32) -> Rgb {
    let (lf, lb) = (luminance(fg), luminance(bg));
    let (hi, lo) = (lf.max(lb), lf.min(lb));
    if t <= 0.0 || t >= 1.0 || hi - lo <= f32::EPSILON {
        return blend(fg, bg, t);
    }
    let start = start_mix(fg, bg, t);
    let full = (hi + 0.05) / (lo + 0.05);
    let target = target(full, t);
    let at_start = if lf >= lb {
        1.0 + start * (full - 1.0)
    } else {
        (lb + 0.05) / (lb + start * (lf - lb) + 0.05)
    };
    if at_start >= target {
        return blend(fg, bg, start);
    }
    let needed = if lf < lb {
        (lb + 0.05) / target - 0.05
    } else {
        target * (lb + 0.05) - 0.05
    };
    blend(
        fg,
        bg,
        start.max(((needed - lb) / (lf - lb)).clamp(0.0, 1.0)),
    )
}

fn ungated_target(full: f32, t: f32) -> f32 {
    full.powf(t)
        .max(9f32.powf(t).min(full.powf(f32::midpoint(1.0, t))))
}

fn share_target(full: f32, t: f32) -> f32 {
    full.powf(t)
}

fn naive_target(full: f32, t: f32) -> f32 {
    full.powf(t).max(full.min(3.0))
}

#[test]
fn dim_never_increases_contrast_grayscale_exhaustive() {
    let mut reduced = 0u32;
    for f in 0..=255u8 {
        for b in 0..=255u8 {
            let fg = Rgb { r: f, g: f, b: f };
            let bg = Rgb { r: b, g: b, b: b };
            let c0 = contrast(fg, bg);
            for t in OPACITIES {
                let c1 = contrast(dim_toward_bg(fg, bg, t), bg);
                assert!(
                    c1 <= c0,
                    "dim raised contrast: fg={f} bg={b} t={t}: {c1} > {c0}"
                );
                if c1 < c0 {
                    reduced += 1;
                }
            }
        }
    }
    assert!(reduced > 100_000, "non-vacuity: dim must actually reduce");
}

#[test]
fn dim_reduces_contrast_both_polarities_rgb_lattice() {
    let colors = lattice();
    let mut dark_on_light_checked = false;
    let mut light_on_dark_checked = false;
    let mut ungated_excess = 0u32;
    for &fg in &colors {
        for &bg in &colors {
            let c0 = contrast(fg, bg);
            if c0 < 1.25 && contrast(faint_with(fg, bg, 0.75, ungated_target), bg) > c0 + 0.005 {
                ungated_excess += 1;
            }
            for t in INTERIOR {
                let c1 = contrast(dim_toward_bg(fg, bg, t), bg);
                if c0 >= 1.05 {
                    assert!(c1 <= c0, "dim raised real contrast: {fg:?}/{bg:?} t={t}");
                } else {
                    assert!(
                        c1 <= c0 + 0.005,
                        "quantization excess out of bound: {fg:?}/{bg:?} t={t}"
                    );
                }
            }
            if luminance(fg) < luminance(bg) && c0 > 4.0 {
                dark_on_light_checked = true;
            }
            if luminance(fg) > luminance(bg) && c0 > 4.0 {
                light_on_dark_checked = true;
            }
        }
    }
    assert!(
        dark_on_light_checked && light_on_dark_checked,
        "non-vacuity"
    );
    assert!(
        ungated_excess > 0,
        "control: the ungated floor must break the quantization bound"
    );
}

#[test]
fn faint_keeps_its_share_of_contrast_on_light_themes() {
    use super::faint_mix;
    let t = aterm_types::DIM_FACTOR;
    let mut floored = 0u32;
    for name in ["Aterm Day", "Linen", "Default Light"] {
        let scheme = aterm_types::scheme::builtin(name).expect("built-in");
        let (fg, bg) = (scheme.foreground, scheme.background);
        assert!(
            contrast(blend(fg, bg, t), bg) < 2.0,
            "{name}: control, the bare lerp is what was weak"
        );
        for (slot, ink) in std::iter::once(fg).chain(scheme.ansi).enumerate() {
            let full = contrast(ink, bg);
            if full < 1.25 {
                continue;
            }
            let faint = contrast(dim_toward_bg(ink, bg, t), bg);
            let target = faint_target(full, t);
            assert!(faint >= target * 0.98, "{name} #{slot}: {faint} < {target}");
            assert!(faint < full, "{name} #{slot}: faint still recedes");
            if full >= 4.4 {
                assert!(faint >= 2.94, "{name} #{slot}: faint at {faint}:1");
            }
            if target > full.powf(t) + 0.01 {
                floored += 1;
            }
        }
    }
    assert!(
        floored > 10,
        "non-vacuity: the readable floor binds on palettes"
    );
    let day = aterm_types::scheme::builtin("Aterm Day").expect("built-in");
    let (seven, bg) = (day.ansi[7], day.background);
    assert!(
        contrast(faint_with(seven, bg, t, share_target), bg) < 2.5,
        "control: the log share alone leaves colour 7 under 2.5:1"
    );
    assert!(contrast(dim_toward_bg(seven, bg, t), bg) >= 2.94);
    for name in ["Aterm Day", "Linen", "Default Light"] {
        let scheme = aterm_types::scheme::builtin(name).expect("built-in");
        for ink in std::iter::once(scheme.foreground).chain(scheme.ansi) {
            if luminance(ink) < luminance(scheme.background) {
                assert!(
                    (start_mix(ink, scheme.background, t) - t).abs() < f32::EPSILON,
                    "{name}: dark ink starts from the lerp"
                );
            }
        }
    }
    assert!(faint_mix(0.02, 0.9, 0.0, 0.0).abs() < f32::EPSILON);
    assert!((faint_mix(0.02, 0.9, 1.0, 1.0) - 1.0).abs() < f32::EPSILON);
}

#[test]
fn faint_light_ink_recedes_as_an_alpha_blend() {
    let t = aterm_types::DIM_FACTOR;
    let page = Rgb::new(0x11, 0x13, 0x18);
    let (white, seven) = (Rgb::new(255, 255, 255), Rgb::new(229, 229, 229));
    let lerp_ghost = blend(white, page, t);
    assert_eq!(lerp_ghost, Rgb::new(0xBC, 0xBC, 0xBC), "control: the lerp");
    assert!(contrast(lerp_ghost, seven) < 1.6, "control: read as typed");
    let ghost = dim_toward_bg(white, page, t);
    let c = contrast(ghost, page);
    assert!((4.5..5.5).contains(&c), "{ghost:?}: {c}:1");
    assert!(contrast(ghost, seven) >= 2.5, "{ghost:?} beside colour 7");
    for name in ["Aterm Night", "Hearth", "Default"] {
        let scheme = aterm_types::scheme::builtin(name).expect("built-in");
        let (fg, bg) = (scheme.foreground, scheme.background);
        assert!(luminance(fg) > luminance(bg), "{name}");
        let faint = contrast(dim_toward_bg(fg, bg, t), bg);
        let alpha = (srgb_blend_luminance(fg, bg, t) + 0.05) / (luminance(bg) + 0.05);
        assert!(
            (faint - alpha).abs() <= alpha * 0.03,
            "{name}: {faint} vs the alpha blend's {alpha}"
        );
        assert!(
            faint < 0.6 * contrast(blend(fg, bg, t), bg),
            "{name}: {faint} recedes"
        );
        assert!(faint >= faint_target(contrast(fg, bg), t) * 0.98, "{name}");
    }
}

#[test]
fn faint_stays_fainter_than_its_colour() {
    let colors = lattice();
    let (mut floor_binds, mut naive_undimmed) = (0u32, 0u32);
    for &fg in &colors {
        for &bg in &colors {
            let full = contrast(fg, bg);
            if full < 1.25 {
                continue;
            }
            for t in INTERIOR {
                let faint = contrast(dim_toward_bg(fg, bg, t), bg);
                assert!(faint < full, "fg={fg:?} bg={bg:?} t={t}: {faint} >= {full}");
                let cap = contrast(blend(fg, bg, t), bg).max(full.powf(f32::midpoint(1.0, t)));
                assert!(
                    faint <= cap * 1.02,
                    "fg={fg:?} bg={bg:?} t={t}: {faint} > {cap}"
                );
                if faint_target(full, t) > full.powf(t) + 0.01 {
                    floor_binds += 1;
                }
                if full <= 3.0 && contrast(faint_with(fg, bg, t, naive_target), bg) >= full {
                    naive_undimmed += 1;
                }
            }
        }
    }
    assert!(floor_binds > 10_000, "non-vacuity: {floor_binds}");
    assert!(
        naive_undimmed > 0,
        "control: the ceiling must be load-bearing"
    );
}

#[test]
fn faint_mix_never_drops_below_its_start() {
    let colors = lattice();
    let (mut raised, mut receded) = (0u32, 0u32);
    for &fg in &colors {
        for &bg in &colors {
            let (lf, lb) = (luminance(fg), luminance(bg));
            for t in INTERIOR {
                let start = start_mix(fg, bg, t);
                if lf > lb {
                    let unclamped =
                        super::srgb_blend_mix(super::transfer_tables(), fg, bg, (lf, lb), t);
                    assert!(unclamped <= t + 1e-5, "fg={fg:?} bg={bg:?} t={t}");
                    if start < t - 0.05 {
                        receded += 1;
                    }
                } else {
                    assert!((start - t).abs() < f32::EPSILON);
                }
                let ld = luminance(dim_toward_bg(fg, bg, t));
                assert!(
                    (ld - lb).abs() >= start * (lf - lb).abs() - 0.005,
                    "fg={fg:?} bg={bg:?} t={t}"
                );
                if (ld - lb).abs() > start * (lf - lb).abs() + 0.01 {
                    raised += 1;
                }
            }
        }
    }
    assert!(raised > 10_000, "non-vacuity: the floors raise the mix");
    assert!(receded > 10_000, "non-vacuity: the alpha blend recedes");
}

#[test]
fn faint_keeps_its_reference_bytes() {
    let colors = lattice();
    let fg = Rgb {
        r: 109,
        g: 255,
        b: 36,
    };
    let bg = Rgb {
        r: 0,
        g: 255,
        b: 182,
    };
    assert!(luminance(fg) > luminance(bg));
    assert_eq!(
        dim_toward_bg(fg, bg, 0.75),
        faint_with(fg, bg, 0.75, faint_target)
    );
    let (mut light_ink, mut dark_ink, mut light_floored) = (0, 0, 0);
    for &fg in &colors {
        for &bg in &colors {
            assert_eq!(dim_toward_bg(fg, bg, 0.0), bg);
            assert_eq!(dim_toward_bg(fg, bg, 1.0), fg);
            let (lf, lb) = (luminance(fg), luminance(bg));
            let lighter = lf >= lb;
            let full = (lf.max(lb) + 0.05) / (lf.min(lb) + 0.05);
            for t in INTERIOR {
                let start = start_mix(fg, bg, t);
                let at_start = if lighter {
                    1.0 + start * (full - 1.0)
                } else {
                    (lb + 0.05) / (lb + start * (lf - lb) + 0.05)
                };
                let plain = at_start >= faint_target(full, t) || (lf - lb).abs() <= f32::EPSILON;
                let expected = faint_with(fg, bg, t, faint_target);
                if plain {
                    let kept = if (lf - lb).abs() <= f32::EPSILON {
                        t
                    } else {
                        start
                    };
                    assert_eq!(expected, blend(fg, bg, kept), "fg={fg:?} bg={bg:?}");
                }
                assert_eq!(
                    dim_toward_bg(fg, bg, t),
                    expected,
                    "fg={fg:?} bg={bg:?} t={t}"
                );
                if lighter && !plain {
                    light_floored += 1;
                }
            }
            if lighter {
                light_ink += 1;
            } else {
                dark_ink += 1;
            }
        }
    }
    assert!(
        light_ink > 100_000 && dark_ink > 100_000,
        "non-vacuity: both full RGB lattices"
    );
    assert!(light_floored > 0, "non-vacuity: light ink is floored");
}

#[test]
fn faint_target_square_roots_are_its_powers() {
    let rule = |full: f32, t: f32| {
        if full < 1.25 {
            return full.powf(t);
        }
        full.powf(t)
            .max(9f32.powf(t).min(full.powf(f32::midpoint(1.0, t))))
    };
    for i in 0..=2000u16 {
        let full = 1.0 + f32::from(i) * 0.01;
        let (got, want) = (faint_target(full, 0.5), rule(full, 0.5));
        assert!((got - want).abs() <= want * 1e-6, "{full}: {got} vs {want}");
        assert_eq!(faint_target(full, 0.25), rule(full, 0.25), "{full}");
    }
}

#[test]
fn old_gamma_multiply_violates_the_law() {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn old_dim(c: Rgb) -> Rgb {
        Rgb {
            r: (f32::from(c.r) * 0.5) as u8,
            g: (f32::from(c.g) * 0.5) as u8,
            b: (f32::from(c.b) * 0.5) as u8,
        }
    }
    let fg = Rgb {
        r: 60,
        g: 60,
        b: 60,
    };
    let bg = Rgb {
        r: 240,
        g: 240,
        b: 240,
    };
    assert!(
        contrast(old_dim(fg), bg) > contrast(fg, bg),
        "control: the old multiply must exhibit the polarity inversion"
    );
    assert!(
        contrast(dim_toward_bg(fg, bg, 0.5), bg) < contrast(fg, bg),
        "the fix reduces contrast on the same pair"
    );
}
