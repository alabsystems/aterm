// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agents panel's glass laws, each with a negative control.

use super::*;
use crate::color_math::oklab;

const PALETTES: [CardPalette; 3] = [CardPalette::Cruise, CardPalette::Land, CardPalette::Off];

const GREY_DARK: u32 = 0x0099_9999;
const GREY_LIGHT: u32 = 0x0066_6666;

const SAT_STUCK_TEST: f32 = 0.10;
const SAT_90S: f32 = 1.0 - 0.85 * 70.0 / 130.0;

fn hex(c: aterm_types::Rgb) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

fn grounds() -> Vec<(&'static str, u32, u32, bool)> {
    ["Aterm Night", "Aterm Day", "Linen"]
        .into_iter()
        .map(|name| {
            let s = aterm_types::scheme::builtin(name).expect("built-in theme");
            let (bg, fg) = (hex(s.background), hex(s.foreground));
            (name, bg, fg, relative_luminance(bg) < 0.2)
        })
        .collect()
}

fn papers() -> Vec<(&'static str, u32)> {
    let mut papers: Vec<(&'static str, u32)> = grounds()
        .into_iter()
        .filter(|g| !g.3)
        .map(|(name, bg, _, _)| (name, bg))
        .collect();
    papers.push(("white", 0x00FF_FFFF));
    papers
}

fn ground(dark: bool) -> u32 {
    let name = if dark { "Aterm Night" } else { "Aterm Day" };
    hex(aterm_types::scheme::builtin(name).unwrap().background)
}

fn chroma(rgb: u32) -> f32 {
    chroma_hue(rgb).0
}

fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn look_on(
    palette: CardPalette,
    mood: DotMood,
    level: f32,
    sat: f32,
    k: usize,
    dark: bool,
) -> DotLook {
    dot_look(mood, level, sat, palette, k, dark, ground(dark), 1.0, false)
}

fn look(mood: DotMood, level: f32, sat: f32, k: usize, dark: bool) -> DotLook {
    look_on(CardPalette::Cruise, mood, level, sat, k, dark)
}

#[test]
fn the_rows_take_the_dot_hues_top_down() {
    let hue = |k: usize, dark: bool| {
        let ink = look(DotMood::Running, 0.5, 1.0, k, dark)
            .ink
            .expect("a running dot is inked");
        chroma_hue(ink).1
    };
    for dark in [true, false] {
        let (stripes, n) = dot_stripes(CardPalette::Cruise, dark);
        assert_eq!(n, 4, "dark={dark}: {:?}", &stripes[..n]);
        assert_eq!(
            &stripes[..2],
            &[1, 2],
            "orange, then yellow (gold on paper)"
        );
        assert_eq!(&stripes[2..4], &[4, 5], "blue, violet");
        let hues: Vec<f32> = (1..=4).map(|k| hue(k, dark)).collect();
        assert!(
            hues.windows(2).all(|w| w[1] > w[0]),
            "dark={dark}: the hues follow the palette {hues:?}"
        );
        let rows: Vec<f32> = (1..=4)
            .map(|k| {
                let art = CardPalette::Cruise.art(row_hue(CardPalette::Cruise, k, dark), dark);
                chroma_hue(art).1
            })
            .collect();
        for (i, &h) in hues.iter().enumerate() {
            let nearest = (0..4)
                .min_by(|&a, &b| (h - rows[a]).abs().total_cmp(&(h - rows[b]).abs()))
                .unwrap();
            assert_eq!(nearest, i, "dark={dark} row {}: {h} vs {rows:?}", i + 1);
        }
        assert_eq!(
            look(DotMood::Running, 0.5, 1.0, 5, dark),
            look(DotMood::Running, 0.5, 1.0, 1, dark)
        );
        assert_eq!(
            row_hue(CardPalette::Cruise, 0, dark),
            CardPalette::Cruise.stripe_t(0)
        );
        let reversed: Vec<f32> = hues.iter().rev().copied().collect();
        assert!(!reversed.windows(2).all(|w| w[1] > w[0]));
    }
    for palette in PALETTES {
        for dark in [true, false] {
            let (_, n) = dot_stripes(palette, dark);
            assert!(n >= 2, "{palette:?} dark={dark}: {n} dot hues");
            for k in 1..12 {
                assert_ne!(
                    row_hue(palette, k, dark),
                    row_hue(palette, k + 1, dark),
                    "{palette:?} dark={dark}: rows {k} and {}",
                    k + 1
                );
            }
        }
    }
}

#[test]
fn no_live_dot_wears_claudes_status_hues() {
    let mut worst = f32::MAX;
    for palette in PALETTES {
        for dark in [true, false] {
            for k in 1..=12 {
                let mut looks = Vec::new();
                for step in 0..=8 {
                    let level = step as f32 / 8.0;
                    for sat in [1.0, 0.8, SAT_90S, 0.3, 0.15] {
                        for mood in [DotMood::Running, DotMood::Quiet] {
                            looks.push(look_on(palette, mood, level, sat, k, dark));
                        }
                    }
                }
                for step in 0..BREATH_PERIOD_MS / BREATH_STEP_MS {
                    let b = breath(Some(step * BREATH_STEP_MS));
                    looks.push(dot_look(
                        DotMood::Stuck,
                        0.0,
                        SAT_STUCK_TEST,
                        palette,
                        k,
                        dark,
                        ground(dark),
                        b,
                        false,
                    ));
                }
                for l in looks {
                    let light = l.light.expect("lit");
                    for ink in [l.ink.expect("inked"), light.color] {
                        if let Some(gap) = status_gap(ink, dark) {
                            worst = worst.min(gap);
                            assert!(
                                gap >= STATUS_HUE_GUARD_DEG,
                                "{palette:?} dark={dark} k={k}: {ink:06x} lies {gap:.1}° \
                                 from Claude's status"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(
        worst < 40.0,
        "CONTROL: the guard is what binds ({worst:.1}°)"
    );
    for dark in [true, false] {
        let near: Vec<usize> = (0..6)
            .filter(|&k| {
                let art = CardPalette::Cruise.art(CardPalette::Cruise.stripe_t(k), dark);
                status_gap(art, dark).is_some_and(|g| g < STATUS_HUE_GUARD_DEG)
            })
            .collect();
        assert_eq!(near, vec![0, 3], "dark={dark}: red and green");
    }
}

#[test]
fn quiet_is_paler_than_running() {
    for palette in [CardPalette::Cruise, CardPalette::Land] {
        for dark in [true, false] {
            for k in 1..=6 {
                for level in [0.0, 0.5, 1.0] {
                    let run = look_on(palette, DotMood::Running, level, 1.0, k, dark);
                    for (sat, ink_most, light_most) in
                        [(1.0, 0.7, QUIET_KEEP + 0.02), (SAT_90S, 0.4, 0.32)]
                    {
                        let quiet = look_on(palette, DotMood::Quiet, level, sat, k, dark);
                        let pairs = [
                            (run.ink.unwrap(), quiet.ink.unwrap(), ink_most),
                            (
                                run.light.unwrap().color,
                                quiet.light.unwrap().color,
                                light_most,
                            ),
                        ];
                        for (r, q, most) in pairs {
                            assert!(
                                chroma(q) <= most * chroma(r),
                                "{palette:?} dark={dark} k={k} level={level} sat={sat}: \
                                 quiet {q:06x} ({:.3}) vs running {r:06x} ({:.3})",
                                chroma(q),
                                chroma(r)
                            );
                        }
                    }
                }
            }
        }
    }
    let hue = CardPalette::Cruise.art(row_hue(CardPalette::Cruise, 1, true), true);
    let old = |sat: f32| chroma(mix_rgb(hue, GREY_DARK, 1.0 - sat)) / chroma(hue);
    assert!(
        old(1.0) > 0.99 && old(SAT_90S) > 0.5,
        "{} {}",
        old(1.0),
        old(SAT_90S)
    );
}

#[test]
fn more_work_is_brighter_and_wider_light() {
    for dark in [true, false] {
        let levels = [0.0, 0.25, 0.5, 0.75, 1.0];
        let looks: Vec<DotLook> = levels
            .iter()
            .map(|&l| look(DotMood::Running, l, 1.0, 2, dark))
            .collect();
        let luma: Vec<f32> = looks
            .iter()
            .map(|l| relative_luminance(l.ink.unwrap()))
            .collect();
        let light: Vec<DotLight> = looks.iter().map(|l| l.light.unwrap()).collect();
        for w in light.windows(2) {
            assert!(w[1].radius >= w[0].radius && w[1].strength > w[0].strength);
            if dark {
                assert_eq!(w[1].fill, 0.0, "the dark halo fills the hollow");
            } else {
                assert!(
                    w[1].fill > w[0].fill,
                    "paper: the hollow fills with the work"
                );
            }
        }
        assert!(light[4].radius > light[0].radius);
        assert!(light[4].radius <= DOT_RADIUS_MAX);
        if dark {
            assert!(
                luma.windows(2).all(|w| w[1] >= w[0]) && luma[4] > luma[0] + 0.2,
                "dark: work lifts the dot {luma:?}"
            );
        } else {
            assert!(
                luma.windows(2).all(|w| w[1] <= w[0]) && luma[4] < luma[0],
                "light: work deepens the dot {luma:?}"
            );
        }
    }
    for level in [0.0, 0.4, 1.0] {
        assert_eq!(
            look(DotMood::Running, level, 1.0, 3, true)
                .light
                .unwrap()
                .radius,
            look(DotMood::Running, level, 1.0, 3, false)
                .light
                .unwrap()
                .radius
        );
    }
    for l in [0.0, 1.0] {
        assert_eq!(
            look(DotMood::Still, l, 1.0, 2, true),
            DotLook {
                ink: None,
                light: None
            }
        );
    }
}

#[test]
fn freshness_is_saturation() {
    for dark in [true, false] {
        let c: Vec<f32> = [1.0, 0.6, 0.3, 0.15]
            .iter()
            .map(|&s| chroma(look(DotMood::Quiet, 0.2, s, 1, dark).ink.unwrap()))
            .collect();
        assert!(
            c.windows(2).all(|w| w[1] < w[0]),
            "a staler dot is greyer ({dark}): {c:?}"
        );
        let l1 = look(DotMood::Quiet, 0.2, 1.0, 1, dark).light.unwrap();
        let l3 = look(DotMood::Quiet, 0.2, 0.3, 1, dark).light.unwrap();
        assert!(l3.strength < l1.strength);
        assert!(dark || l3.fill < l1.fill);
    }
    assert!(chroma(look(DotMood::Running, 0.2, 1.0, 1, true).ink.unwrap()) > 0.1);
}

#[test]
fn the_fade_and_the_lift_keep_the_hue() {
    for palette in PALETTES {
        for dark in [true, false] {
            let (stripes, n) = dot_stripes(palette, dark);
            for &k in &stripes[..n] {
                let art = palette.art(palette.stripe_t(k), dark);
                let (c0, h0) = chroma_hue(art);
                if c0 < HUE_CHROMA_GATE {
                    continue;
                }
                for keep in [0.8, QUIET_KEEP, 0.3, 0.1] {
                    let f = fade_chroma(art, keep);
                    let (c, h) = chroma_hue(f);
                    if c >= HUE_CHROMA_GATE {
                        assert!(
                            hue_gap(h, h0) <= STATUS_HUE_DRIFT_DEG,
                            "{art:06x}×{keep}: {f:06x}"
                        );
                    }
                    assert!((oklab(f)[0] - oklab(art)[0]).abs() < 0.02, "lightness held");
                }
                for lt in [0.1, 0.3, 0.475, 0.65] {
                    let l = at_luminance_hue(art, lt);
                    assert!(
                        (relative_luminance(l) - lt).abs() < 0.01,
                        "{art:06x}@{lt}: {l:06x}"
                    );
                    let (c, h) = chroma_hue(l);
                    if c >= HUE_CHROMA_GATE {
                        assert!(
                            hue_gap(h, h0) <= STATUS_HUE_DRIFT_DEG,
                            "{art:06x}@{lt}: {l:06x}"
                        );
                    }
                }
            }
        }
    }
    let orange = CardPalette::Cruise.art(CardPalette::Cruise.stripe_t(1), true);
    let (_, h0) = chroma_hue(orange);
    let (_, h1) = chroma_hue(at_luminance(orange, 0.65));
    assert!(hue_gap(h0, h1) > 10.0, "{h0} → {h1}");
}

#[test]
fn a_stuck_dot_is_grey_and_breathes_under_the_flash_bound() {
    for dark in [true, false] {
        let at = |b: f32| {
            dot_look(
                DotMood::Stuck,
                0.9,
                SAT_STUCK_TEST,
                CardPalette::Cruise,
                1,
                dark,
                ground(dark),
                b,
                false,
            )
        };
        let (lo, hi) = (at(breath(Some(0))), at(breath(Some(BREATH_PERIOD_MS / 2))));
        assert!(chroma(hi.ink.unwrap()) < 0.06, "stuck reads grey");
        let swing = contrast(lo.ink.unwrap(), hi.ink.unwrap());
        assert!(swing > 1.5, "dark={dark}: the ring breathes ({swing:.2}:1)");
        let (llo, lhi) = (lo.light.unwrap(), hi.light.unwrap());
        assert!(lhi.strength > llo.strength, "it swells");
        assert_eq!(llo.radius, STUCK_RADIUS);
        assert_eq!(lhi.fill, 0.0, "a stuck dot is hollow");
        let running = look(DotMood::Running, 0.9, 1.0, 1, dark).light.unwrap();
        assert!(lhi.strength < running.strength && lhi.radius < running.radius);
        let idlest = look(DotMood::Running, 0.0, 1.0, 1, dark).light.unwrap();
        assert!(
            lhi.strength <= idlest.strength + 1e-6,
            "dark={dark}: stuck {} over the idlest running {}",
            lhi.strength,
            idlest.strength
        );
        let serious = dot_look(
            DotMood::Stuck,
            0.9,
            SAT_STUCK_TEST,
            CardPalette::Cruise,
            1,
            dark,
            ground(dark),
            BREATH_REST,
            true,
        );
        assert!(serious.ink.is_some() && serious.light.is_none());
    }
    assert!((breath(Some(0)) - BREATH_FLOOR).abs() < 1e-6);
    assert!((breath(Some(BREATH_PERIOD_MS / 2)) - 1.0).abs() < 1e-6);
    assert_eq!(
        breath(Some(2 * BREATH_STEP_MS + BREATH_STEP_MS / 2)),
        breath(Some(2 * BREATH_STEP_MS))
    );
    assert_eq!(breath(Some(BREATH_PERIOD_MS + 500)), breath(Some(500)));
    assert_eq!(breath(None), BREATH_REST);
    assert_eq!(breath(Some(BREATH_FOR_MS)), BREATH_REST);
    let samples: Vec<f32> = (0..10_000 / BREATH_STEP_MS)
        .map(|i| breath(Some(i * BREATH_STEP_MS)))
        .collect();
    let peaks = samples
        .windows(3)
        .filter(|w| w[1] > w[0] && w[1] >= w[2])
        .count();
    assert!((peaks as f32) / 10.0 <= TWINKLE_FLASH_BOUND_HZ);
    assert!(peaks >= 4, "and it does breathe: {peaks} swells in 10 s");
    let r = |b| {
        dot_look(
            DotMood::Running,
            0.5,
            1.0,
            CardPalette::Cruise,
            1,
            true,
            ground(true),
            b,
            false,
        )
    };
    assert_eq!(r(BREATH_FLOOR), r(1.0));
}

#[test]
fn an_awaiting_dot_is_a_still_amber_ring() {
    for palette in PALETTES {
        for dark in [true, false] {
            let a = |b| {
                dot_look(
                    DotMood::Awaiting,
                    1.0,
                    1.0,
                    palette,
                    3,
                    dark,
                    ground(dark),
                    b,
                    false,
                )
            };
            assert_eq!(a(BREATH_FLOOR), a(1.0), "no breath");
            assert!(a(1.0).ink.is_some() && a(1.0).light.is_none(), "unlit");
            for k in 1..=12 {
                for step in 0..=8 {
                    let level = step as f32 / 8.0;
                    for mood in [DotMood::Running, DotMood::Tool] {
                        let look = look_on(palette, mood, level, 1.0, k, dark);
                        let light = look.light.expect("lit");
                        assert!(
                            alpha_byte(light.strength) >= VEIL_ALPHA_FLOOR
                                && (dark || alpha_byte(light.fill) >= VEIL_ALPHA_FLOOR),
                            "{palette:?} {mood:?} dark={dark} k={k} level={level}: {light:?}"
                        );
                    }
                }
            }
        }
    }
    let gold = look_on(CardPalette::Land, DotMood::Running, 0.3, 1.0, 3, true)
        .light
        .unwrap();
    let old_level = 0.3;
    let old = (
        (DOT_RADIUS_MIN + DOT_RADIUS_GAIN * old_level).min(DOT_RADIUS_MAX),
        HALO_PEAK_MIN + HALO_PEAK_GAIN * old_level,
    );
    assert_eq!(old, (gold.radius, gold.strength), "the same light");
    let gap = hue_gap(chroma_hue(AWAITING_AMBER).1, chroma_hue(gold.color).1);
    assert!(gap < 6.0, "Land's gold and the amber: {gap:.1}°");
    let print_gold = CardPalette::Cruise.art(row_hue(CardPalette::Cruise, 2, false), false);
    let gap = hue_gap(chroma_hue(AWAITING_AMBER).1, chroma_hue(print_gold).1);
    assert!(gap < 1.0, "Cruise's print gold and the amber: {gap:.1}°");
    let a = dot_look(
        DotMood::Awaiting,
        1.0,
        1.0,
        CardPalette::Off,
        3,
        true,
        ground(true),
        1.0,
        false,
    );
    assert_eq!(a.ink, Some(AWAITING_AMBER), "amber on every palette");
    let paper = look_on(CardPalette::Cruise, DotMood::Awaiting, 0.0, 1.0, 3, false);
    assert_eq!(
        paper.ink,
        Some(AWAITING_AMBER_PRINT),
        "Claude's own warning on paper"
    );
    let gold_tool = look_on(CardPalette::Cruise, DotMood::Tool, 0.0, 1.0, 2, false);
    let de = crate::color_math::oklab_distance(gold_tool.ink.unwrap(), AWAITING_AMBER_PRINT);
    assert!(
        de < 0.06,
        "Cruise's gold tool ring and the awaiting print: {de:.3}"
    );
}

#[test]
fn text_keeps_claudes_lightness_on_every_ground() {
    for (name, bg, fg, dark) in grounds() {
        let grey = if dark { GREY_DARK } else { GREY_LIGHT };
        for palette in PALETTES {
            for cell_fg in [fg, grey] {
                for i in 0..=20 {
                    let x = i as f32 / 20.0;
                    for sat in [1.0, 0.5] {
                        for mark in [PanelMark::Name, PanelMark::Pointer, PanelMark::BarEmpty] {
                            let ink = panel_ink(palette, mark, x, dark, cell_fg, sat, 0.0);
                            let (want, got) = (contrast(cell_fg, bg), contrast(ink, bg));
                            assert!(
                                (got - want).abs() / want <= 0.03,
                                "{name} {palette:?} {mark:?} x={x} fg={cell_fg:06x}: \
                                 {ink:06x} at {got:.2}:1 vs Claude's {want:.2}:1"
                            );
                        }
                    }
                }
            }
        }
    }
    let (_, bg, fg, _) = grounds()[0];
    let off = (0..=20).any(|i| {
        let raw = CardPalette::Cruise.letter(i as f32 / 20.0, true);
        (contrast(raw, bg) - contrast(fg, bg)).abs() / contrast(fg, bg) > 0.1
    });
    assert!(off, "matching is what keeps the lightness");
}

#[test]
fn match_luminance_keeps_the_hue() {
    for ink in [0x00FF_0000, 0x0000_88FF, 0x0040_C040] {
        for target in [0x0099_9999, 0x0020_2733, 0x00D8_E1EC] {
            let m = match_luminance(ink, target);
            let d = (relative_luminance(m) - relative_luminance(target)).abs();
            assert!(d < 0.01, "{ink:06x}→{target:06x}: {m:06x} off by {d}");
            let (h0, s0, _) = crate::color_math::rgb2hsv(ink);
            let (h1, s1, _) = crate::color_math::rgb2hsv(m);
            if s1 > 0.2 && s0 > 0.2 {
                let dh = (h0 - h1).abs().min(360.0 - (h0 - h1).abs());
                assert!(dh < 10.0, "{ink:06x}→{target:06x}: hue {h0} became {h1}");
            }
        }
    }
    assert_eq!(
        match_luminance(0, 0x0099_9999),
        0x0099_9999,
        "black takes the target"
    );
}

#[test]
fn off_reads_near_neutral() {
    for (_, _, fg, dark) in grounds() {
        for i in 0..=10 {
            let x = i as f32 / 10.0;
            let name = panel_ink(CardPalette::Off, PanelMark::Name, x, dark, fg, 1.0, 0.0);
            let fill = panel_ink(CardPalette::Off, PanelMark::BarFill, x, dark, fg, 1.0, 0.0);
            assert!(
                chroma(name) < 0.05 && chroma(fill) < 0.06,
                "{name:06x} {fill:06x}"
            );
        }
        for k in 0..6 {
            let l = look_on(CardPalette::Off, DotMood::Running, 1.0, 1.0, k, dark);
            let ink = l.ink.unwrap();
            assert!(chroma(ink) < 0.06, "Off's dot {k}: {ink:06x}");
            assert!(chroma(l.light.unwrap().color) < 0.06, "and its light");
        }
    }
    assert!(
        chroma(panel_ink(
            CardPalette::Cruise,
            PanelMark::BarFill,
            0.0,
            true,
            0,
            1.0,
            0.0
        )) > 0.15
    );
}

#[test]
fn only_the_bars_fill_crests() {
    for dark in [true, false] {
        let fg = if dark { GREY_DARK } else { GREY_LIGHT };
        for mark in [PanelMark::Name, PanelMark::Pointer, PanelMark::BarEmpty] {
            assert_eq!(
                panel_ink(CardPalette::Cruise, mark, 0.4, dark, fg, 1.0, 0.0),
                panel_ink(CardPalette::Cruise, mark, 0.4, dark, fg, 1.0, 1.0),
                "text never shimmers"
            );
        }
        let rest = panel_ink(
            CardPalette::Cruise,
            PanelMark::BarFill,
            0.4,
            dark,
            fg,
            1.0,
            0.0,
        );
        let lit = panel_ink(
            CardPalette::Cruise,
            PanelMark::BarFill,
            0.4,
            dark,
            fg,
            1.0,
            1.0,
        );
        assert_ne!(rest, lit, "the fill crests");
        if dark {
            assert!(relative_luminance(lit) > relative_luminance(rest));
        } else {
            assert!(
                relative_luminance(lit) <= relative_luminance(rest),
                "never paler on paper"
            );
        }
    }
    let field: Vec<f32> = (0..24).map(|d| crest(d as f32, 1000)).collect();
    assert!(field.iter().any(|&v| v > 0.0) && field.contains(&0.0));
}

#[test]
fn the_names_and_the_bar_walk_the_palette() {
    assert!((name_x(0.2, 0, 8) - 0.2).abs() < 1e-6);
    assert!((name_x(0.2, 7, 8) - (0.2 + NAME_SPAN)).abs() < 1e-6);
    assert!(
        (name_x(0.9, 7, 8) - (2.0 - 0.9 - NAME_SPAN)).abs() < 1e-6,
        "folds back"
    );
    assert!((name_x(0.4, 0, 1) - 0.4).abs() < 1e-6);
    assert_eq!(bar_x(0, 10), 0.0);
    assert_eq!(bar_x(9, 10), 1.0);
    assert_eq!(bar_x(0, 1), 0.0);
    let reach = |filled: usize| bar_x(filled - 1, 10);
    assert!(reach(8) > reach(3));
}

#[test]
fn a_dots_light_stays_on_its_bullet() {
    let place = DotPlace {
        cx: 105,
        cy: 409,
        cw: 10,
        ch: 18,
        row: 22,
        clip: (40, 418),
    };
    let check = |halos: &[RainHalo], mode: HaloMode| {
        for h in halos {
            assert_eq!((h.cx, h.cy, h.mode), (105, 409, mode));
            let band_top = i32::from(place.cy) - 9 + (i32::from(h.row) - 22) * 18;
            assert!(i32::from(h.y) >= band_top && i32::from(h.y + h.h) <= band_top + 18);
            assert!((21..=23).contains(&h.row));
            assert!(f32::from(h.rx) <= DOT_RADIUS_MAX * 10.0 + 0.5);
            assert!(h.y >= 40 && h.y + h.h <= 418);
            assert!(f32::from(h.ry) <= DOT_RISE_MAX * 18.0 + 0.5);
        }
        assert!(halos.iter().all(|h| h.row != 23), "clipped: {halos:?}");
    };
    let full = look(DotMood::Running, 1.0, 1.0, 1, true).light.unwrap();
    let mut halos = Vec::new();
    dot_light_quads(&full, true, place, &mut halos);
    assert!(!halos.is_empty());
    check(&halos, HaloMode::Add);
    let dark_rx = halos[0].rx;

    let paper = look(DotMood::Running, 1.0, 1.0, 1, false).light.unwrap();
    let mut halos = Vec::new();
    dot_light_quads(&paper, false, place, &mut halos);
    check(&halos, HaloMode::Over);
    let (tint, fill): (Vec<RainHalo>, Vec<RainHalo>) = halos.iter().partition(|h| h.rx == dark_rx);
    assert!(!tint.is_empty() && !fill.is_empty(), "{halos:?}");
    for h in &tint {
        assert_eq!(
            aterm_render::halo_over_cap(h.color),
            alpha_byte(paper.strength)
        );
    }
    for h in &fill {
        assert_eq!(aterm_render::halo_over_cap(h.color), alpha_byte(paper.fill));
        assert_eq!(h.rx, h.ry, "the fill is round");
        assert_eq!(f32::from(h.rx), (FILL_RADIUS * 10.0).round());
        assert_eq!(h.row, 22, "inside the bullet's band");
    }
    assert!(
        halos.iter().position(|h| h.rx != dark_rx) > halos.iter().position(|h| h.rx == dark_rx),
        "the fill over the tint"
    );
    let dim = look(DotMood::Running, 0.0, 1.0, 1, false).light.unwrap();
    let mut small = Vec::new();
    dot_light_quads(&dim, false, place, &mut small);
    let widest = |h: &[RainHalo]| h.iter().map(|q| q.rx).max().unwrap();
    assert!(widest(&small) < widest(&halos));
    for dark in [true, false] {
        let mut halos = Vec::new();
        let none = DotLight {
            strength: 0.0,
            fill: 0.0,
            ..full
        };
        dot_light_quads(&none, dark, place, &mut halos);
        assert!(halos.is_empty());
    }
}

#[test]
fn the_light_is_the_agents_hue_and_a_stuck_dots_is_grey() {
    for dark in [true, false] {
        for k in 1..=4 {
            let run = look(DotMood::Running, 1.0, 1.0, k, dark);
            let (ink, light) = (run.ink.unwrap(), run.light.unwrap());
            assert!(
                chroma(light.color) >= chroma(ink) - 1e-3 && chroma(light.color) > 0.08,
                "dark={dark} k={k}: the light {:06x} is the hue, ink {ink:06x}",
                light.color
            );
            if !dark {
                assert!(
                    relative_luminance(ink) <= PRINT_RING_LUMA + 0.01,
                    "k={k}: on paper the ring {ink:06x} is a print ink, never a pastel"
                );
            }
            let stuck = look(DotMood::Stuck, 0.0, SAT_STUCK_TEST, k, dark)
                .light
                .unwrap();
            assert!(
                chroma(stuck.color) < 0.4 * chroma(light.color),
                "dark={dark} k={k}: a stuck dot's light {:06x} is grey",
                stuck.color
            );
        }
        let waiting = look(DotMood::Awaiting, 0.0, 1.0, 1, dark);
        assert!(
            waiting.light.is_none(),
            "dark={dark}: an awaiting dot is unlit"
        );
    }
}

#[test]
fn every_dot_holds_three_to_one_on_its_ground() {
    let mut cases: Vec<(&str, u32, bool)> = grounds()
        .into_iter()
        .filter(|g| g.3)
        .map(|(name, bg, _, dark)| (name, bg, dark))
        .collect();
    cases.extend(papers().into_iter().map(|(name, bg)| (name, bg, false)));
    for (name, bg, dark) in cases {
        let under = |look: DotLook| {
            let ink = look.ink.expect("an inked dot");
            let (ink2, ground) = match look.light {
                Some(l) if !dark => {
                    let a = alpha_byte(l.strength);
                    (
                        over_rgb(ink, l.color, a) & 0x00FF_FFFF,
                        over_rgb(bg, l.color, a) & 0x00FF_FFFF,
                    )
                }
                _ => (ink, bg),
            };
            (ink, contrast(ink2, ground))
        };
        for palette in PALETTES {
            for k in 1..=6 {
                let at = |mood, level, sat, b| {
                    dot_look(mood, level, sat, palette, k, dark, bg, b, false)
                };
                let mut looks = Vec::new();
                for level in [0.0, 0.5, 1.0] {
                    for sat in [1.0, SAT_90S, 0.15] {
                        for mood in [DotMood::Running, DotMood::Quiet, DotMood::Tool] {
                            looks.push((at(mood, level, sat, 1.0), mood != DotMood::Quiet));
                        }
                    }
                }
                for step in 0..BREATH_PERIOD_MS / BREATH_STEP_MS {
                    let b = breath(Some(step * BREATH_STEP_MS));
                    looks.push((at(DotMood::Stuck, 0.0, SAT_STUCK_TEST, b), false));
                }
                looks.push((at(DotMood::Awaiting, 0.0, 1.0, 1.0), false));
                for (look, running) in looks {
                    let (ink, c) = under(look);
                    let floor = if running && !dark {
                        PRINT_RING_CONTRAST - 0.05
                    } else {
                        3.0
                    };
                    assert!(
                        c >= floor,
                        "{name} {palette:?} k={k}: {ink:06x} at {c:.2}:1 on {bg:06x}"
                    );
                }
            }
        }
    }
    let (_, linen) = papers()[1];
    let swell = dot_look(
        DotMood::Stuck,
        0.0,
        SAT_STUCK_TEST,
        CardPalette::Cruise,
        1,
        false,
        linen,
        1.0,
        false,
    )
    .ink
    .unwrap();
    let old = mix_rgb(swell, 0x00FF_FFFF, 1.0 - BREATH_FLOOR);
    assert!(contrast(old, linen) < 3.0, "{:.2}:1", contrast(old, linen));
    let gold = CardPalette::Cruise.art(CardPalette::Cruise.stripe_t(2), false);
    let top = at_luminance(gold, PRINT_RING_LUMA * (1.0 - DOT_DEEPEN_LIGHT));
    let veil = alpha_byte(TINT_ALPHA_MIN + TINT_ALPHA_GAIN);
    let c = contrast(
        over_rgb(top, gold, veil) & 0x00FF_FFFF,
        over_rgb(linen, gold, veil) & 0x00FF_FFFF,
    );
    assert!(c < 3.0, "the solve is what holds it: {c:.2}:1");
}

#[test]
fn a_tool_dot_is_fresh_still_and_dim() {
    let fresh_and_dim = |look: DotLook, running: DotLook, dark: bool| {
        let (ink, light) = (look.ink.unwrap(), look.light.unwrap());
        let (r_ink, r_light) = (running.ink.unwrap(), running.light.unwrap());
        let fresh = chroma(ink) >= 0.8 * chroma(r_ink) && chroma(ink) > 0.06;
        let dim = light.strength < r_light.strength && (dark || light.fill < r_light.fill);
        fresh && dim
    };
    for palette in PALETTES {
        for dark in [true, false] {
            for k in 1..=4 {
                let at = |mood, level, sat, b| {
                    dot_look(mood, level, sat, palette, k, dark, ground(dark), b, false)
                };
                for level in [0.0, 0.3, 1.0] {
                    let tool = at(DotMood::Tool, level, 1.0, 1.0);
                    let running = at(DotMood::Running, level, 1.0, 1.0);
                    if palette != CardPalette::Off {
                        assert!(
                            fresh_and_dim(tool, running, dark),
                            "{palette:?} dark={dark} k={k} level={level}"
                        );
                        let stuck = at(DotMood::Stuck, level, SAT_STUCK_TEST, 1.0);
                        let (ct, cs) = (chroma(tool.ink.unwrap()), chroma(stuck.ink.unwrap()));
                        assert!(
                            ct >= 3.0 * cs,
                            "{palette:?} dark={dark} k={k}: {ct:.3} vs {cs:.3}"
                        );
                        if level <= 0.3 {
                            assert!(
                                ct - cs >= 0.1,
                                "{palette:?} dark={dark} k={k}: {ct:.3} vs {cs:.3}"
                            );
                        }
                        assert!(!fresh_and_dim(stuck, running, dark));
                        assert!(!fresh_and_dim(running, running, dark));
                    }
                    let light = tool.light.unwrap();
                    let run = running.light.unwrap();
                    assert!((light.strength - TOOL_GLOW * run.strength).abs() < 1e-6);
                    assert!((light.fill - TOOL_GLOW * run.fill).abs() < 1e-6);
                    assert_eq!(light.radius, TOOL_RADIUS, "inside its ring");
                    assert!(light.radius < run.radius, "a running dot's spreads past it");
                    assert_eq!(at(DotMood::Tool, level, 1.0, BREATH_FLOOR), tool, "still");
                }
            }
        }
    }
    let serious = dot_look(
        DotMood::Tool,
        0.0,
        1.0,
        CardPalette::Cruise,
        1,
        true,
        ground(true),
        1.0,
        true,
    );
    assert!(serious.ink.is_some() && serious.light.is_none());
}
