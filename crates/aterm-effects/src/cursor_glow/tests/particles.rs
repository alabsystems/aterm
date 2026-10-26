// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor-glow particle tests across styles and dark/light backgrounds.

use super::*;

fn grain(x: f32, y: f32, hue: f32, born: Instant) -> Particle {
    Particle {
        x0: x,
        y0: y,
        vx: 0.0,
        vy: 0.0,
        gy: 0.0,
        life: 5.0,
        hue,
        cov_scale: 1.0,
        born,
    }
}

fn seed_hue(seed: u32) -> f32 {
    (seed as f32 + 0.5) / 4096.0
}

fn peak_age(hue: f32) -> f32 {
    let quarter = std::f32::consts::FRAC_PI_2 - hue * std::f32::consts::TAU;
    quarter.rem_euclid(std::f32::consts::PI) / crate::effect_util::TWINKLE_OMEGA
}

fn emit_one(c: &GlowConfig, g: Geom, p: Particle, age: f32) -> (Vec<GlowQuad>, Vec<RainHalo>) {
    let born = p.born;
    let mut glow = CursorGlow::default();
    glow.particles.push(p);
    let (mut out, mut halos) = (Vec::new(), Vec::new());
    glow.emit_particles(
        born + Duration::from_secs_f32(age),
        c,
        g,
        &mut out,
        &mut halos,
        &mut crate::cursor_glow::glow_sink(
            &mut Vec::new(),
            &mut aterm_render::GlowRamps::default(),
            true,
        ),
        CursorGlow::MAX_QUADS,
    );
    (out, halos)
}

fn covered(quads: &[GlowQuad]) -> std::collections::BTreeSet<(i32, i32)> {
    quads
        .iter()
        .flat_map(|q| {
            (q.y..q.y + q.h).flat_map(move |y| (q.x..q.x + q.w).map(move |x| (x as i32, y as i32)))
        })
        .collect()
}

#[test]
fn the_moon_is_a_crescent() {
    let g = retina_geom();
    let (cx, cy, r) = (100.5f32, 60.5f32, 8.0f32);
    let mut dark = Vec::new();
    push_crescent(
        &mut dark,
        g,
        (cx, cy),
        r,
        0x00E6_E6F2,
        200,
        CrescentInk::Add,
    );
    let lit = covered(&dark);
    let at = |dx: f32, dy: f32| lit.contains(&((cx + dx * r) as i32, (cy + dy * r) as i32));
    assert!(
        at(-0.8, 0.0) && at(-0.5, 0.6),
        "the limb is lit to the lower left"
    );
    assert!(
        !at(0.0, 0.0),
        "the disc's centre is in shadow: not a plain disc"
    );
    assert!(!at(0.6, 0.0) && !at(0.3, -0.5), "the shadow side is dark");
    assert!(
        at(-0.3, -0.85) && at(0.6, 0.65),
        "the horns reach up and to the right"
    );
    let west = |dy: f32| {
        let y = (cy + dy * r) as i32;
        lit.iter()
            .filter(|p| p.1 == y)
            .map(|p| p.0)
            .min()
            .expect("row lit")
    };
    assert!(
        west(0.0) + 2 < west(-0.75) && west(0.0) + 2 < west(0.75),
        "the limb curves: middle row starts at {}, the horn rows at {} and {}",
        west(0.0),
        west(-0.75),
        west(0.75)
    );
    assert!(
        dark.iter().all(|q| q.h == 1 && q.alpha == 0),
        "one pixel row a quad, additive on dark"
    );
    let mut light = Vec::new();
    push_crescent(
        &mut light,
        g,
        (cx, cy),
        r,
        0x0043_4347,
        180,
        CrescentInk::Over,
    );
    assert_eq!(
        covered(&light),
        lit,
        "the light moon has the dark one's footprint"
    );
    assert!(light.iter().all(|q| q.alpha > 0), "source-over on light");
}

/// looks7 round 3, SPARKLE'S GOLD ON A TINTED PAGE: added to Solarized Dark's teal
/// `#002b36`, the page's own green and blue turned the gold moons olive and sage (`#cbc165`,
/// `#4b6348`, hue 113). Laid over a tinted dark page by [`page_matched`], at every coverage
/// from a quarter up the moon's gold reads within eight degrees of the hue it reads added to
/// the reference dark page, no brighter than it lays added to the tinted page, and a full
/// moon reads as on the reference page. The reference page itself, and a grey one, are not tinted: the
/// moon is added there as before. Negative control: added to the teal page, a moon at half
/// its light reads more than twenty degrees off its gold.
#[test]
fn sparkle_s_gold_reads_gold_on_a_tinted_page() {
    use crate::color_math::{relative_luminance, rgb2hsv};
    const TEAL: u32 = 0x0000_2B36;
    const REF: u32 = 0x0011_1318;
    const GOLD: u32 = CursorGlow::MOON_GOLD;
    let ch = |c: u32, sh: u32| (c >> sh) & 0xff;
    let add = |page: u32, a: u32| {
        let c = |sh: u32| (ch(page, sh) + ch(GOLD, sh) * a / 255).min(255);
        (c(16) << 16) | (c(8) << 8) | c(0)
    };
    let over = |page: u32, (premul, alpha): (u32, u8)| {
        let c = |sh: u32| (ch(page, sh) * (255 - u32::from(alpha)) / 255 + ch(premul, sh)).min(255);
        (c(16) << 16) | (c(8) << 8) | c(0)
    };
    let hue_gap = |a: u32, b: u32| {
        let d = (rgb2hsv(a).0 - rgb2hsv(b).0).abs();
        d.min(360.0 - d)
    };
    assert_eq!(tinted_dark_page(TEAL), Some(TEAL));
    assert_eq!(
        tinted_dark_page(REF),
        None,
        "the reference page is not tinted"
    );
    assert_eq!(
        tinted_dark_page(0x0020_2020),
        None,
        "a grey page is not tinted"
    );
    for a in (64..=255u32).step_by(8) {
        let laid = over(TEAL, page_matched(GOLD, a as u8, TEAL));
        let reference = add(REF, a);
        let gap = hue_gap(laid, reference);
        assert!(
            gap <= 8.0,
            "at {a}: the moon reads {laid:06x}, {gap:.1} degrees off its {reference:06x} on the reference page"
        );
        assert!(
            relative_luminance(laid) <= relative_luminance(add(TEAL, a)) + 0.01,
            "at {a}: the moon lays more light than added: {laid:06x}"
        );
    }
    let full = over(TEAL, page_matched(GOLD, 255, TEAL));
    let near = |a: u32, b: u32| {
        [16, 8, 0]
            .iter()
            .all(|&sh| ch(a, sh).abs_diff(ch(b, sh)) <= 2)
    };
    assert!(
        near(full, add(REF, 255)),
        "a full moon reads {full:06x}, not its {:06x} on the reference page",
        add(REF, 255)
    );
    let added = add(TEAL, 128);
    assert!(
        hue_gap(added, add(REF, 128)) > 20.0,
        "control: added to the teal page, the half moon reads {added:06x}"
    );
}

#[test]
fn stardust_is_the_body_the_plus_is_the_accent() {
    let g = geom();
    let born = Instant::now();
    let (x, y) = (100.0, 40.0);
    let c = cfg(GlowStyle::Sparkle, true);
    for seed in [1u32, 2, 3, 8, 4, 5, 6, 7, 16] {
        let hue = seed_hue(seed);
        let p = Particle {
            vx: 30.0,
            vy: -20.0,
            ..grain(x, y, hue, born)
        };
        let (out, halos) = emit_one(&c, g, p, peak_age(hue));
        match seed {
            1..=3 | 8 => assert!(
                out.is_empty() && !halos.is_empty(),
                "sparkle dust (seed {seed}) is a round mote"
            ),
            4 | 5 => {
                assert!(out.is_empty(), "glitter (seed {seed}) lays no square");
                let centre = |h: &RainHalo| (h.cx, h.cy, h.rx, h.ry);
                assert!(
                    !halos.is_empty() && halos.iter().all(|h| centre(h) == centre(&halos[0])),
                    "glitter (seed {seed}) is one round grain"
                );
            }
            6 => assert!(
                halos.is_empty() && !out.is_empty() && out.iter().all(|q| q.h == 1),
                "the moon (seed {seed}) is the crescent"
            ),
            _ => assert!(!out.is_empty(), "the plus and the mini-comet lay quads"),
        }
    }
    let c = cfg(GlowStyle::Comet, true);
    let undealt = seed_hue(5);
    let (out, halos) = emit_one(&c, g, grain(x, y, undealt, born), peak_age(undealt));
    assert!(
        out.is_empty() && !halos.is_empty(),
        "an undealt comet grain is a round mote at its peak too, with no white core square"
    );
    let dealt = seed_hue(16);
    let (out, _) = emit_one(&c, g, grain(x, y, dealt, born), peak_age(dealt));
    assert!(!out.is_empty(), "the dealt comet grain glints at its peak");
    let (out, halos) = emit_one(&c, g, grain(x, y, dealt, born), peak_age(dealt) + 0.17);
    assert!(
        out.is_empty() && !halos.is_empty(),
        "off its peak the dealt grain is dust"
    );
}

/// The particles stop where their stream's lights fill it, the lit rows of
/// its ramps counted: given the room those leave (`cap`), a shower lays over
/// an empty stream exactly the quads and halos it lays after as many legacy
/// quads, each lit ramp row one, as the cap cuts it short.
#[test]
fn particles_stop_where_the_stream_fills_its_ramp_lights_counted() {
    let g = geom();
    let c = cfg(GlowStyle::Sparkle, true);
    let now = Instant::now() + Duration::from_secs(10);
    let shower = |held: usize, cap: usize| {
        let mut glow = CursorGlow::default();
        for k in 0..96u32 {
            let hue = seed_hue(k % 17);
            let (x, y) = (12.0 + 3.0 * k as f32, 20.0 + (k % 5) as f32 * 12.0);
            let born = now - Duration::from_secs_f32(peak_age(hue));
            glow.particles.push(grain(x, y, hue, born));
        }
        let (mut out, mut halos) = (vec![GlowQuad::default(); held], Vec::new());
        glow.emit_particles(
            now,
            &c,
            g,
            &mut out,
            &mut halos,
            &mut crate::cursor_glow::glow_sink(
                &mut Vec::new(),
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            cap,
        );
        (out.split_off(held), halos)
    };
    let all = shower(0, CursorGlow::MAX_QUADS);
    assert!(
        all.0.len() > 8 && all.1.len() > 8,
        "the shower lays stars and halos: {} quads, {} halos",
        all.0.len(),
        all.1.len()
    );
    for room in [0, 1, 5, all.0.len() / 2] {
        let legacy = shower(CursorGlow::MAX_QUADS - room, CursorGlow::MAX_QUADS);
        assert!(
            legacy.1.len() < all.1.len(),
            "room {room}: the cap cut the shower short"
        );
        assert_eq!(
            shower(0, room),
            legacy,
            "room {room}: the ramps' lights cap the particles as their quads do"
        );
    }
}

fn probed_glow(c: &GlowConfig, g: Geom, at: Instant) -> CursorGlow {
    let mut glow = CursorGlow::default();
    let mut row = [' '; 40];
    row[5] = 'w';
    row[10] = 'x';
    let mut above = [' '; 40];
    above[20] = 'y';
    let blank = [' '; 40];
    glow.observe_row(2, 30, &row, at);
    glow.observe_neighbor_rows(Some(&above), Some(&blank));
    let mut out = Vec::new();
    glow.tick(Some((2, 30)), at, c, g, &mut out);
    glow.particles.clear();
    glow
}

#[test]
fn light_grains_are_ink_and_keep_off_the_letters() {
    let g = geom();
    let at = Instant::now();
    for style in [
        GlowStyle::Sparkle,
        GlowStyle::Comet,
        GlowStyle::Water,
        GlowStyle::Laser,
    ] {
        let mut c = cfg(style, true);
        c.dark_theme = false;
        c.theme_fg = 0x001F_2328;
        c.theme_bg = 0x00FF_FFFF;
        let draw = |x: f32, y: f32, hue: f32| {
            let mut glow = probed_glow(&c, g, at);
            glow.particles.push(Particle {
                vx: 30.0,
                vy: -20.0,
                ..grain(x, y, hue, at)
            });
            let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
            glow.emit_particles(
                at,
                &c,
                g,
                &mut out,
                &mut halos,
                &mut crate::cursor_glow::glow_sink(
                    &mut under,
                    &mut aterm_render::GlowRamps::default(),
                    true,
                ),
                CursorGlow::MAX_QUADS,
            );
            (out, halos, under)
        };
        for seed in [1u32, 4, 6, 7, 16] {
            let hue = seed_hue(seed);
            let (out, halos, under) = draw(g.cell_center(2, 14).0, 40.0, hue);
            assert!(
                !out.is_empty() || !halos.is_empty() || !under.is_empty(),
                "{style:?} seed {seed}: a grain past the end of the line draws"
            );
            assert!(
                out.iter().chain(&under).all(|q| q.alpha > 0)
                    && halos.iter().all(|h| h.mode == HaloMode::Over),
                "{style:?} seed {seed}: a light grain is ink"
            );
            assert!(
                style == GlowStyle::Comet || under.is_empty(),
                "{style:?} seed {seed}: only the comet's debris lies under the letters"
            );
            for (row, col) in [(2u16, 10u16), (2, 7), (1, 20)] {
                let (x, y) = g.cell_center(row, col);
                let (out, halos, under) = draw(x, y, hue);
                assert!(
                    out.is_empty() && halos.is_empty() && under.is_empty(),
                    "{style:?} seed {seed}: a grain over the text at {row},{col} is not drawn"
                );
            }
        }
    }
    for dark in [false, true] {
        let mut c = cfg(GlowStyle::Beam, true);
        c.dark_theme = dark;
        if !dark {
            c.theme_fg = 0x001F_2328;
            c.theme_bg = 0x00FF_FFFF;
        }
        for seed in [1u32, 4, 6, 7, 16] {
            let mut glow = probed_glow(&c, g, at);
            glow.particles
                .push(grain(g.cell_center(2, 14).0, 40.0, seed_hue(seed), at));
            let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
            glow.emit_particles(
                at,
                &c,
                g,
                &mut out,
                &mut halos,
                &mut crate::cursor_glow::glow_sink(
                    &mut under,
                    &mut aterm_render::GlowRamps::default(),
                    true,
                ),
                CursorGlow::MAX_QUADS,
            );
            let drew = !out.is_empty() || !halos.is_empty() || !under.is_empty();
            assert_eq!(
                drew, dark,
                "Beam seed {seed}: a grain draws only on dark (dark={dark})"
            );
        }
    }
    {
        let c = cfg(GlowStyle::Comet, true);
        for seed in [1u32, 4, 6, 7, 16, 23] {
            for col in [10u16, 7] {
                let mut glow = probed_glow(&c, g, at);
                glow.particles
                    .push(grain(g.cell_center(2, col).0, 40.0, seed_hue(seed), at));
                let (mut out, mut halos) = (Vec::new(), Vec::new());
                glow.emit_particles(
                    at,
                    &c,
                    g,
                    &mut out,
                    &mut halos,
                    &mut crate::cursor_glow::glow_sink(
                        &mut Vec::new(),
                        &mut aterm_render::GlowRamps::default(),
                        true,
                    ),
                    CursorGlow::MAX_QUADS,
                );
                let top = 2.0 * g.ch as f32;
                for h in &halos {
                    let k = (f32::from(h.cy) - top) / g.ch as f32;
                    assert!(
                        !(0.2..0.8).contains(&k),
                        "Comet seed {seed}: over the text at column {col} a grain stands at the letters' height ({k:.2} of the row)"
                    );
                }
            }
        }
    }
    for style in [GlowStyle::Sparkle] {
        let c = cfg(style, true);
        let hue = seed_hue(5);
        let peak = |x: f32| {
            let mut glow = probed_glow(&c, g, at);
            glow.particles.push(grain(x, 40.0, hue, at));
            let (mut out, mut halos) = (Vec::new(), Vec::new());
            glow.emit_particles(
                at,
                &c,
                g,
                &mut out,
                &mut halos,
                &mut crate::cursor_glow::glow_sink(
                    &mut Vec::new(),
                    &mut aterm_render::GlowRamps::default(),
                    true,
                ),
                CursorGlow::MAX_QUADS,
            );
            halos
                .iter()
                .map(|h| (h.color >> 8) & 0xff)
                .max()
                .unwrap_or(0) as f32
        };
        let open = peak(g.cell_center(2, 14).0);
        assert!(
            open > 0.0,
            "{style:?}: the grain glows past the end of the line"
        );
        for col in [10u16, 7] {
            let over = peak(g.cell_center(2, col).0);
            assert!(
                over <= open * CursorGlow::GRAIN_OVER_GLYPH_DIM + 2.0,
                "{style:?}: over the text at column {col} the grain dims: {over} vs {open}"
            );
        }
    }
}

#[test]
fn a_full_pool_sheds_the_oldest_grains_never_the_head() {
    let g = geom();
    let c = cfg(GlowStyle::Sparkle, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    let jump = t0 + Duration::from_millis(16);
    glow.note_synthetic_move(jump);
    glow.tick(Some((4, 30)), jump, &c, g, &mut out);
    let at = jump + Duration::from_millis(20);
    glow.particles.clear();
    glow.tick(Some((4, 30)), at, &c, g, &mut out);
    let head: Vec<RainHalo> = glow.halos().to_vec();
    assert!(
        !head.is_empty(),
        "fixture: the jump lands a crown and a ring"
    );
    for i in 0..CursorGlow::MAX_PARTICLES {
        let (x, y) = (8.0 + (i % 36) as f32 * 8.0, 6.0 + (i / 36) as f32 * 6.0);
        glow.particles
            .push(grain(x, y, seed_hue(1 + 8 * (i as u32 % 64)), at));
    }
    let newest = glow
        .particles
        .last()
        .map(|p| (p.x0 as u16, p.y0 as u16))
        .unwrap();
    let mut pool_halos = Vec::new();
    glow.emit_particles(
        at,
        &c,
        g,
        &mut out,
        &mut pool_halos,
        &mut crate::cursor_glow::glow_sink(
            &mut Vec::new(),
            &mut aterm_render::GlowRamps::default(),
            true,
        ),
        CursorGlow::MAX_QUADS,
    );
    assert!(
        head.len() + pool_halos.len() > CursorGlow::MAX_HALOS,
        "fixture: the pool alone overflows the budget ({} halos)",
        pool_halos.len()
    );
    glow.tick(Some((4, 30)), at, &c, g, &mut out);
    let halos = glow.halos();
    assert!(halos.len() <= CursorGlow::MAX_HALOS, "the stream is capped");
    assert!(
        halos.iter().any(|h| (h.cx, h.cy) == newest),
        "the newest grain survives the cap"
    );
    let crown_ring = |h: &RainHalo| head.iter().any(|k| k == h);
    assert_eq!(
        halos.iter().filter(|h| crown_ring(h)).count(),
        head.len(),
        "the head marks drawn before the particles survive the cap"
    );
}

#[test]
fn a_dealt_comet_grain_glints_over_its_body() {
    let g = geom();
    let born = Instant::now();
    let (x, y) = (100.0, 40.0);
    let dealt = seed_hue(16);
    let peak = peak_age(dealt);
    let past = peak + 0.88f32.acos() / crate::effect_util::TWINKLE_OMEGA;
    let c = cfg(GlowStyle::Comet, true);
    let ink = |age: f32| {
        let (out, halos) = emit_one(&c, g, grain(x, y, dealt, born), age);
        let mut glass = Glass::new(g, c.theme_bg);
        glass.add_halos(&halos);
        glass.add_quads(&out);
        let off_ground = |px: u32| -> i64 {
            [16u32, 8, 0]
                .iter()
                .map(|&sh| {
                    (i64::from((px >> sh) & 0xff) - i64::from((c.theme_bg >> sh) & 0xff)).abs()
                })
                .sum()
        };
        let total: i64 = glass.px.iter().map(|&px| off_ground(px)).sum();
        (total, off_ground(glass.at(y as i32, x as i32)), out, halos)
    };
    let (at_peak, peak_centre, peak_out, peak_halos) = ink(peak);
    let (just_past, past_centre, past_out, _) = ink(past);
    let body = |hs: &[RainHalo]| hs.iter().any(|h| (h.cx, h.cy) == (x as u16, y as u16));
    assert!(
        body(&peak_halos),
        "the grain keeps its round body at its peak"
    );
    assert!(
        !peak_out.is_empty() && past_out.is_empty(),
        "the glint fires at the peak only"
    );
    assert!(
        at_peak > just_past && peak_centre >= past_centre,
        "the glint flares, never dips: {at_peak} (centre {peak_centre}) at the peak vs \
         {just_past} (centre {past_centre}) just past it"
    );
    let mut light = cfg(GlowStyle::Comet, true);
    light.dark_theme = false;
    light.theme_fg = 0x001F_2328;
    light.theme_bg = 0x00FF_FFFF;
    let mut glow = CursorGlow::default();
    glow.particles.push(grain(x, y, dealt, born));
    let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
    glow.emit_particles(
        born + Duration::from_secs_f32(peak),
        &light,
        g,
        &mut out,
        &mut halos,
        &mut crate::cursor_glow::glow_sink(
            &mut under,
            &mut aterm_render::GlowRamps::default(),
            true,
        ),
        CursorGlow::MAX_QUADS,
    );
    assert!(
        out.is_empty() && halos.is_empty(),
        "no glint over the text on paper"
    );
    assert!(
        !under.is_empty() && under.iter().all(|q| q.alpha > 0),
        "the mote is ink under the letters"
    );
}

#[test]
fn the_dark_sparkle_shower_keeps_off_the_caret_and_its_moon_is_gold() {
    let g = geom();
    let at = Instant::now();
    let caret = CursorGlow::caret_box(g, 2, 30);
    let edge = (caret.2 as f32 + 1.0, (caret.1 + caret.3) as f32 * 0.5);
    let clear = (
        caret.2 as f32 + 2.0 * g.cw as f32 + 4.0,
        edge.1 - g.ch as f32,
    );
    let draws = |style: GlowStyle, (x, y): (f32, f32), seed: u32| {
        let c = cfg(style, true);
        let mut glow = probed_glow(&c, g, at);
        glow.particles.push(Particle {
            vx: 30.0,
            vy: -20.0,
            ..grain(x, y, seed_hue(seed), at)
        });
        let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
        glow.emit_particles(
            at,
            &c,
            g,
            &mut out,
            &mut halos,
            &mut crate::cursor_glow::glow_sink(
                &mut under,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            CursorGlow::MAX_QUADS,
        );
        (out, halos)
    };
    for seed in [1u32, 4, 6, 7] {
        let (out, halos) = draws(GlowStyle::Sparkle, edge, seed);
        assert!(
            out.is_empty() && halos.is_empty(),
            "seed {seed}: a grain the block would cut is not drawn"
        );
        let (out, halos) = draws(GlowStyle::Sparkle, clear, seed);
        assert!(
            !out.is_empty() || !halos.is_empty(),
            "seed {seed}: a grain clear of the caret draws"
        );
    }
    let (moon, _) = draws(GlowStyle::Sparkle, clear, 6);
    let lit = moon
        .iter()
        .max_by_key(|q| q.color & 0xff_ff_ff)
        .expect("a moon");
    let (hue, sat, _) = crate::color_math::rgb2hsv(lit.color & 0x00FF_FFFF);
    assert!(
        (25.0..=55.0).contains(&hue) && sat >= 0.6,
        "the moon is warm gold: {:06x} (hue {hue:.0}, saturation {sat:.2})",
        lit.color & 0x00FF_FFFF
    );
    let (out, halos) = draws(GlowStyle::Lumen, edge, 1);
    assert!(
        !out.is_empty() || !halos.is_empty(),
        "control: Lumen's grain there still draws on dark"
    );
}

#[test]
fn the_light_sparkle_shower_is_gold_and_registers_on_white() {
    use crate::color_math::{contrast_ratio, rgb2hsv};
    let g = geom();
    let at = Instant::now();
    let mut c = cfg(GlowStyle::Sparkle, true);
    c.dark_theme = false;
    (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
    c.intensity = 1.0;
    let gold = |ink: u32| {
        let (h, s, _) = rgb2hsv(ink);
        (20.0..=58.0).contains(&h) && s >= 0.6
    };
    let mut pastel_strays = 0;
    for seed in [1u32, 4, 6, 7, 16, 400, 900, 2047] {
        let hue = seed_hue(seed);
        let mut glow = probed_glow(&c, g, at);
        glow.particles.push(Particle {
            vx: 30.0,
            vy: -20.0,
            ..grain(g.cell_center(2, 14).0, 40.0, hue, at)
        });
        let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
        glow.emit_particles(
            at + Duration::from_secs_f32(peak_age(hue)),
            &c,
            g,
            &mut out,
            &mut halos,
            &mut crate::cursor_glow::glow_sink(
                &mut under,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            CursorGlow::MAX_QUADS,
        );
        let inks: Vec<(u32, u8)> = out
            .iter()
            .map(|q| {
                let a = q.alpha.max(1);
                let un = |sh: u32| (((q.color >> sh) & 0xff) * 255 / u32::from(a)).min(255);
                ((un(16) << 16) | (un(8) << 8) | un(0), q.alpha)
            })
            .chain(
                halos
                    .iter()
                    .map(|h| (h.color & 0x00FF_FFFF, (h.color >> 24) as u8)),
            )
            .collect();
        assert!(!inks.is_empty(), "seed {seed}: the grain draws");
        let (deepest, ink) = inks
            .iter()
            .map(|&(ink, a)| {
                let seen =
                    aterm_render::over_premul(0x00FF_FFFF, aterm_render::premul_rgb(ink, a), a)
                        & 0x00FF_FFFF;
                (contrast_ratio(seen, 0x00FF_FFFF), ink)
            })
            .fold((0.0f32, 0u32), |a, b| if b.0 > a.0 { b } else { a });
        assert!(
            gold(ink),
            "seed {seed}: the grain's ink {ink:06x} is not gold"
        );
        assert!(
            deepest >= 3.0,
            "seed {seed}: the grain reads only {deepest:.2}:1 on white"
        );
        let pastel =
            InkRole::OverText.ink(style_particle_color(GlowStyle::Sparkle, c.color, hue, 1.0));
        pastel_strays += usize::from(!gold(pastel));
    }
    assert!(
        pastel_strays > 0,
        "control: the wheel's pastels inked for paper all stay gold"
    );
}

#[test]
fn the_shower_and_the_stardust_keep_off_the_lines_an_enter_printed() {
    use aterm_core::terminal::RenderCell;
    let g = geom();
    let at = Instant::now();
    let line = "the quick brown fox";
    // Each cell on its page's ground: a dark ground on paper is a filled
    // cell (a powerline bar), which no sparkle stands on.
    let cells_of = |text: &str, dark: bool| -> Vec<RenderCell> {
        let (fg, bg) = if dark {
            ([0xD0, 0xD0, 0xD0], [0x11, 0x13, 0x18])
        } else {
            ([0x1F, 0x23, 0x28], [0xFF, 0xFF, 0xFF])
        };
        (0..g.cols)
            .map(|i| RenderCell {
                ch: text.chars().nth(i).unwrap_or(' '),
                fg,
                bg,
                ..Default::default()
            })
            .collect()
    };
    let chars_of = |text: &str| -> Vec<char> {
        (0..g.cols)
            .map(|i| text.chars().nth(i).unwrap_or(' '))
            .collect()
    };
    let cells = |dark: bool| -> Vec<Vec<RenderCell>> {
        (0..g.rows)
            .map(|r| match r {
                3 | 4 => cells_of(line, dark),
                5 => cells_of("$", dark),
                _ => cells_of("", dark),
            })
            .collect()
    };
    let draws = |style: GlowStyle, dark: bool, (x, y): (f32, f32), seed: u32| {
        let mut c = cfg(style, true);
        if !dark {
            c.dark_theme = false;
            (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
        }
        let mut glow = CursorGlow::default();
        glow.observe_row(5, 2, &chars_of("$"), at);
        glow.observe_neighbor_rows(Some(&chars_of(line)), None);
        glow.observe_frame_text(Some((5, 2)), &cells(dark), at);
        let mut out = Vec::new();
        glow.tick(Some((5, 2)), at, &c, g, &mut out);
        glow.particles.clear();
        glow.particles.push(Particle {
            vx: 30.0,
            vy: -20.0,
            ..grain(x, y, seed_hue(seed), at)
        });
        let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
        glow.emit_particles(
            at,
            &c,
            g,
            &mut out,
            &mut halos,
            &mut crate::cursor_glow::glow_sink(
                &mut under,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            CursorGlow::MAX_QUADS,
        );
        !out.is_empty() || !halos.is_empty() || !under.is_empty()
    };
    let (letter, gap, air) = (
        g.cell_center(3, 4),
        g.cell_center(3, 9),
        g.cell_center(1, 9),
    );
    for seed in [1u32, 4, 6, 7] {
        for dark in [true, false] {
            for spot in [letter, gap] {
                assert!(
                    !draws(GlowStyle::Sparkle, dark, spot, seed),
                    "Sparkle seed {seed} (dark={dark}): a grain over the printed line draws"
                );
            }
            assert!(
                draws(GlowStyle::Sparkle, dark, air, seed),
                "Sparkle seed {seed} (dark={dark}): a grain over a blank row is not drawn"
            );
        }
        for spot in [letter, gap] {
            assert!(
                !draws(GlowStyle::Beam, true, spot, seed),
                "Beam seed {seed}: a speck over the printed line draws"
            );
        }
        assert!(
            draws(GlowStyle::Beam, true, air, seed),
            "Beam seed {seed}: a speck over a blank row is not drawn"
        );
    }
    assert!(
        draws(GlowStyle::Comet, true, letter, 5),
        "control: the comet's debris over the printed line still glints on dark"
    );
}

#[test]
fn a_dark_water_droplet_is_a_soft_round_bead() {
    let g = geom();
    let at = Instant::now();
    let c = cfg(GlowStyle::Water, true);
    for seed in [1u32, 4, 6, 7, 900, 2047] {
        for vy in [0.0f32, 140.0] {
            let p = Particle {
                vy,
                ..grain(60.0, 30.0, seed_hue(seed), at)
            };
            let (out, halos) = emit_one(&c, g, p, 0.05);
            assert!(
                out.is_empty(),
                "seed {seed}: a droplet lays no flat quad: {out:?}"
            );
            assert!(!halos.is_empty(), "seed {seed}: the droplet is lit");
            assert!(
                halos
                    .iter()
                    .all(|h| h.mode == HaloMode::Add && h.ry >= h.rx),
                "seed {seed}: a round bead, drawn out along its fall: {halos:?}"
            );
        }
    }
    let laser = cfg(GlowStyle::Laser, true);
    let (out, _) = emit_one(
        &laser,
        g,
        Particle {
            vx: 40.0,
            vy: 20.0,
            ..grain(60.0, 30.0, seed_hue(5), at)
        },
        0.05,
    );
    assert!(!out.is_empty(), "control: a laser spark lays quads");
}
