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
        latch: ParticleLatch::FRESH,
        edge_cut: false,
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

/// looks10 L11, A MOON THE GUARD CUTS STAYS GOLD: on Solarized Dark's teal a moon is added
/// to the night ground the frame lays after the letter guard ([`CursorGlow::ground_the_frame`]),
/// as the shower's grains and stars are, so cut to any share of its light (the guard scales an
/// added light's colour) no pixel of it reads more than twelve degrees past its gold's hue on
/// the reference page toward green, short of the page's own cyan (160 degrees; a faint edge
/// over a partial ground reads the page's slate, `#283331`), over its own ground. Negative control: laid over the teal as it reads on the reference page
/// ([`CrescentInk::Page`], where no night ground lies) and cut as the guard cuts a source-over
/// light, colour and opacity alike, the moon let the teal through and its horns read olive
/// beside the letters (`#68663d`, hue 56, in the replay's `split`).
#[test]
fn a_moon_the_guard_cuts_stays_gold_on_a_tinted_page() {
    use crate::color_math::rgb2hsv;
    const TEAL: u32 = 0x0000_2B36;
    let g = retina_geom();
    let at = Instant::now();
    let mut c = cfg(GlowStyle::Sparkle, true);
    c.theme_bg = TEAL;
    c.color = crate::cursor_glow::style::SPARKLE_DEFAULT_COLOR;
    let spot = (
        g.origin_x as f32 + 12.5 * g.cw as f32,
        g.origin_y as f32 + 0.5 * g.ch as f32,
    );
    let moon = |grounded: bool| {
        let mut glow = probed_glow(&c, g, at);
        glow.night_ground_off = !grounded;
        glow.particles.push(grain(spot.0, spot.1, seed_hue(6), at));
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
        (glow, out)
    };
    let gold_hue = {
        let ch = |sh: u32| {
            ((0x0011_1318u32 >> sh) & 0xff) + ((CursorGlow::MOON_GOLD >> sh) & 0xff) * 3 / 4
        };
        rgb2hsv((ch(16).min(255) << 16) | (ch(8).min(255) << 8) | ch(0).min(255)).0
    };
    // The pixels the cut moon moves off the teal whose hue is off its gold's.
    let off_gold = |glow: &mut CursorGlow, cut: &[GlowQuad]| {
        glow.under_out.clear();
        glow.ground_the_frame(&c, g, cut);
        let mut lit = Glass::new(g, TEAL);
        lit.add_quads(&glow.under_out);
        lit.add_quads(cut);
        let (mut moved, mut off) = (0usize, 0usize);
        for &p in &lit.px {
            let p = p & 0x00FF_FFFF;
            let gap = [16u32, 8, 0]
                .iter()
                .any(|&sh| ((p >> sh) & 0xff).abs_diff((TEAL >> sh) & 0xff) > 12);
            let (h, s, v) = rgb2hsv(p);
            if !gap || s < 0.2 || v < 0.12 {
                continue;
            }
            moved += 1;
            off += usize::from(h > gold_hue + 12.0 && h < 160.0);
        }
        (moved, off)
    };
    let sc = |c: u32, k: f32| {
        let f = |sh: u32| ((((c >> sh) & 0xff) as f32 * k) as u32) << sh;
        f(16) | f(8) | f(0)
    };
    let (mut glow, laid) = moon(true);
    assert!(
        !laid.is_empty() && laid.iter().all(|q| q.alpha == 0 && q.alpha2 == 0),
        "on the grounded teal the moon is an added light"
    );
    for share in [1.0f32, 0.7, 0.5, 0.3, 0.15] {
        let cut: Vec<GlowQuad> = laid
            .iter()
            .map(|q| GlowQuad {
                color: sc(q.color, share),
                color2: sc(q.color2, share),
                ..*q
            })
            .collect();
        let (moved, off) = off_gold(&mut glow, &cut);
        assert!(
            moved > 4,
            "fixture: the moon at {share} lights the page ({moved} px)"
        );
        assert_eq!(
            off, 0,
            "at {share} of its light {off} of the moon's {moved} px read off its gold"
        );
    }
    let (mut bare, page_laid) = moon(false);
    assert!(
        page_laid.iter().any(|q| q.alpha > 0),
        "control: with no night ground the moon is laid over the teal"
    );
    let cut: Vec<GlowQuad> = page_laid
        .iter()
        .map(|q| GlowQuad {
            color: sc(q.color, 0.4),
            color2: sc(q.color2, 0.4),
            alpha: (f32::from(q.alpha) * 0.4) as u8,
            alpha2: (f32::from(q.alpha2) * 0.4) as u8,
            ..*q
        })
        .collect();
    let (moved, off) = off_gold(&mut bare, &cut);
    assert!(
        off * 3 > moved,
        "control: cut over the teal only {off} of the moon's {moved} px read off its gold"
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
    // A grain the star accent deals is dust too: its white glint read as a
    // grey `+` (looks10 L9).
    let dealt = seed_hue(16);
    for age in [peak_age(dealt), peak_age(dealt) + 0.17] {
        let (out, halos) = emit_one(&c, g, grain(x, y, dealt, born), age);
        assert!(
            out.is_empty() && !halos.is_empty(),
            "the dealt comet grain is a round mote at {age:.2} s, with no glint"
        );
    }
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

/// Sparkle's paper marks keep off the typed line's letters behind the caret.
/// Two beads a typed space shed hovered in the blank cell between `&&` and
/// the caret on the paper Settings card, two stacked gold rings that read as
/// a typed `:` (composed r2's judges). A mark in the glyph band of the caret's
/// row, short of the caret, within two columns of a letter is not drawn on
/// paper; the air past the caret keeps its sparkle, and the dark shower (a
/// soft glow, not a rimmed bead) is as it was. Control:
/// `sparkle_typed_band_open` draws the mark among the letters again.
#[test]
fn a_paper_sparkle_mark_keeps_off_the_letters_just_typed() {
    let g = geom();
    let at = Instant::now();
    let caret = 24u16;
    let draws = |dark: bool, col: u16, seed: u32, open: bool| {
        let mut c = cfg(GlowStyle::Sparkle, true);
        c.dark_theme = dark;
        if !dark {
            c.theme_fg = 0x001F_2328;
            c.theme_bg = 0x00FF_FFFF;
        }
        let mut glow = CursorGlow::default();
        let mut row = [' '; 40];
        for (i, ch) in "md && ".chars().enumerate() {
            row[17 + i] = ch;
        }
        let blank = [' '; 40];
        glow.observe_row(2, caret, &row, at);
        glow.observe_neighbor_rows(Some(&blank), Some(&blank));
        let mut out = Vec::new();
        glow.tick(Some((2, caret)), at, &c, g, &mut out);
        glow.particles.clear();
        glow.sparkle_typed_band_open = open;
        let (x, y) = g.cell_center(2, col);
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
    // Column 22 is the blank cell after `&&` (columns 20 and 21), column 23
    // the typed space under the caret's reach, column 30 the air past it.
    for seed in [1u32, 4, 5, 7] {
        assert!(
            !draws(false, 22, seed, false),
            "seed {seed}: a paper mark among the letters just typed reads as a letter"
        );
        assert!(
            draws(false, 30, seed, false),
            "seed {seed}: a paper mark in the air past the caret draws"
        );
        assert!(
            draws(true, 22, seed, false),
            "seed {seed}: the dark shower's glow among the typed letters is as it was"
        );
        assert!(
            draws(false, 22, seed, true),
            "seed {seed}: control: with the typed band open the paper mark draws among the letters"
        );
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

/// A grain of Comet's dust is a mote of the tail's ice at its twinkle's
/// peak, on the dark page and on paper: its round body and no star (looks10
/// L9: its white four-point glint read as a grey `+`, the shower Sparkle
/// owns). On paper it is ink under the letters.
#[test]
fn a_dealt_comet_grain_is_a_mote_with_no_glint() {
    let g = geom();
    let born = Instant::now();
    let (x, y) = (100.0, 40.0);
    let dealt = seed_hue(16);
    let peak = peak_age(dealt);
    let c = cfg(GlowStyle::Comet, true);
    let (peak_out, peak_halos) = emit_one(&c, g, grain(x, y, dealt, born), peak);
    let body = |hs: &[RainHalo]| hs.iter().any(|h| (h.cx, h.cy) == (x as u16, y as u16));
    assert!(
        body(&peak_halos),
        "the grain keeps its round body at its peak"
    );
    assert!(peak_out.is_empty(), "the grain lays no glint at its peak");
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
        "control: the comet's dust over the printed line still draws on dark"
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

/// looks9 Q7, SPARKLE'S SHOWER IS GOLD: on a dark page every grain of the
/// shower (its dust, its stars, its mini comets) is the gold's family, gold,
/// amber or a warm white, on the default dark and on Solarized Dark; drawn
/// round the colour wheel, its dust threw green (`#7ae15a`) and pink specks
/// among the gold stars. Negative control: the wheel's shower
/// ([`CursorGlow::sparkle_wheel`]) throws grains off the gold.
#[test]
fn the_dark_sparkle_shower_is_the_gold_s_family() {
    let g = geom();
    let at = Instant::now();
    let caret = CursorGlow::caret_box(g, 2, 30);
    let clear = (
        caret.2 as f32 + 3.0 * g.cw as f32,
        (caret.1 + caret.3) as f32 * 0.5 - g.ch as f32,
    );
    // Whether a drawn colour is the gold's: a warm hue, or a warm white (a
    // premultiplied light too faint to hold a hue, under 8 of 255, is none).
    let golden = |rgb: u32| {
        let (h, s, v) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
        v < 8.0 / 255.0 || s < 0.10 || (18.0..=62.0).contains(&h)
    };
    let strays = |wheel: bool, page: u32| -> (usize, usize, u32) {
        let mut c = cfg(GlowStyle::Sparkle, true);
        (c.color, c.theme_bg) = (SPARKLE_DEFAULT_COLOR, page);
        let (mut drawn, mut off, mut first) = (0usize, 0usize, 0u32);
        for seed in (0u32..4096).step_by(13) {
            let mut glow = probed_glow(&c, g, at);
            glow.sparkle_wheel = wheel;
            let hue = seed_hue(seed);
            glow.particles.push(Particle {
                vx: 30.0,
                vy: -20.0,
                ..grain(clear.0, clear.1, hue, at)
            });
            let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
            let t = at + Duration::from_secs_f32(peak_age(hue).min(0.4));
            glow.emit_particles(
                t,
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
            // The light the shower adds (a tinted page's crescent and night
            // grounds are laid over it, darker than the page's light).
            let colours = out.iter().filter(|q| q.alpha == 0).map(|q| q.color).chain(
                halos
                    .iter()
                    .filter(|h| h.mode == aterm_render::HaloMode::Add)
                    .map(|h| h.color),
            );
            for rgb in colours {
                drawn += 1;
                if !golden(rgb) {
                    off += 1;
                    if first == 0 {
                        first = rgb & 0x00FF_FFFF;
                    }
                }
            }
        }
        (drawn, off, first)
    };
    for page in [0x0011_1318u32, 0x0000_2B36] {
        let (drawn, off, first) = strays(false, page);
        assert!(drawn > 100, "page {page:06x}: the shower draws ({drawn})");
        assert_eq!(
            off, 0,
            "page {page:06x}: {off} of {drawn} shower colours are off the gold (first {first:06x})"
        );
        let (_, wheel_off, _) = strays(true, page);
        assert!(
            wheel_off > 0,
            "control: page {page:06x}: the wheel's shower throws grains off the gold"
        );
    }
}

/// The hues (degrees) of every colour Sparkle's shower draws, one grain of
/// each sampled seed at its twinkle's peak: on the default dark page the
/// light it adds (the wheel's shower where `wheel`,
/// [`CursorGlow::sparkle_wheel`]), on paper every mark it inks. A colour too
/// faint (under 8 of 255) or too grey (under 0.10 saturation) to hold a hue,
/// a warm white among them, is none.
fn sparkle_shower_hues(dark: bool, wheel: bool) -> Vec<f32> {
    let g = geom();
    let at = Instant::now();
    let caret = CursorGlow::caret_box(g, 2, 30);
    let clear = (
        caret.2 as f32 + 3.0 * g.cw as f32,
        (caret.1 + caret.3) as f32 * 0.5 - g.ch as f32,
    );
    let mut c = cfg(GlowStyle::Sparkle, true);
    c.color = SPARKLE_DEFAULT_COLOR;
    if dark {
        c.theme_bg = 0x0011_1318;
    } else {
        c.dark_theme = false;
        c.theme_fg = 0x001F_2328;
        c.theme_bg = 0x00FF_FFFF;
    }
    let mut hues = Vec::new();
    for seed in (0u32..4096).step_by(13) {
        let mut glow = probed_glow(&c, g, at);
        glow.sparkle_wheel = wheel;
        let hue = seed_hue(seed);
        glow.particles.push(Particle {
            vx: 30.0,
            vy: -20.0,
            ..grain(clear.0, clear.1, hue, at)
        });
        let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
        let t = at + Duration::from_secs_f32(peak_age(hue).min(0.4));
        glow.emit_particles(
            t,
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
        let colours: Vec<u32> = if dark {
            out.iter()
                .filter(|q| q.alpha == 0)
                .map(|q| q.color)
                .chain(
                    halos
                        .iter()
                        .filter(|h| h.mode == aterm_render::HaloMode::Add)
                        .map(|h| h.color),
                )
                .collect()
        } else {
            out.iter()
                .chain(&under)
                .map(|q| q.color)
                .chain(halos.iter().map(|h| h.color))
                .collect()
        };
        for rgb in colours {
            let (h, s, v) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
            if v >= 8.0 / 255.0 && s >= 0.10 {
                hues.push(h);
            }
        }
    }
    hues
}

/// Whether a colour word holds of `hues` (degrees, [`sparkle_shower_hues`]):
/// a hue's name of every one, `rainbow` of hues round at least four of the
/// wheel's six sextants. `None` for a word that names no colour.
fn colour_word_holds(word: &str, hues: &[f32]) -> Option<bool> {
    let all_within = |lo: f32, hi: f32| hues.iter().all(|&h| (lo..=hi).contains(&h));
    Some(match word {
        "gold" | "golden" | "amber" => all_within(18.0, 62.0),
        "orange" => all_within(15.0, 45.0),
        "yellow" => all_within(45.0, 70.0),
        "green" => all_within(70.0, 170.0),
        "blue" => all_within(180.0, 260.0),
        "purple" | "violet" => all_within(260.0, 300.0),
        "pink" => all_within(300.0, 345.0),
        "red" => hues.iter().all(|&h| !(15.0..340.0).contains(&h)),
        "rainbow" | "colourful" | "colorful" | "multicoloured" | "multicolored" => {
            let sextants: std::collections::BTreeSet<u32> =
                hues.iter().map(|&h| (h / 60.0) as u32 % 6).collect();
            sextants.len() >= 4
        }
        _ => return None,
    })
}

/// The colour words of `blurb`'s shower (the words after its `shower`), each
/// checked against `hues`: how many it names, or the first that does not hold.
fn shower_words_hold(blurb: &str, hues: &[f32]) -> Result<usize, String> {
    let lower = blurb.to_lowercase();
    let Some((_, shower)) = lower.split_once("shower") else {
        return Err(format!("{blurb:?} names no shower"));
    };
    let mut named = 0;
    for word in shower.split(|c: char| !c.is_alphabetic()) {
        match colour_word_holds(word, hues) {
            Some(true) => named += 1,
            Some(false) => return Err(word.to_owned()),
            None => {}
        }
    }
    Ok(named)
}

/// Settings demo round 3's judges, S3: A STYLE'S BLURB SAYS WHAT IT DRAWS.
/// Sparkle's blurb, which Settings shows and speaks, names its shower's
/// colours as the shower draws them, gold (looks9 Q7) on the default dark
/// and on paper; it read "a shower of rainbow stars" over a shower of gold
/// stars and crescent moons. Negative controls: that blurb is refused on both
/// grounds, and the blurb as it stands is refused of the wheel's shower
/// ([`CursorGlow::sparkle_wheel`]).
#[test]
fn the_sparkle_blurb_names_the_colours_its_shower_draws() {
    let blurb = crate::cursor_glow::spellings::TRAIL_STYLES
        .iter()
        .find(|row| row.name == "sparkle")
        .map(|row| row.blurb)
        .expect("Sparkle has a row");
    let dark = sparkle_shower_hues(true, false);
    let paper = sparkle_shower_hues(false, false);
    let wheel = sparkle_shower_hues(true, true);
    let was = "A gold comet with a shower of rainbow stars";
    for (ground, hues) in [("dark", &dark), ("paper", &paper)] {
        assert!(
            hues.len() > 100,
            "{ground}: the shower draws ({})",
            hues.len()
        );
        let named = shower_words_hold(blurb, hues);
        assert!(
            named.as_ref().is_ok_and(|&n| n > 0),
            "{ground}: {blurb:?} names the shower's colours as drawn: {named:?}"
        );
        assert_eq!(
            shower_words_hold(was, hues),
            Err("rainbow".to_owned()),
            "control: {ground}: round 3's blurb is refused"
        );
    }
    assert!(
        shower_words_hold(blurb, &wheel).is_err(),
        "control: {blurb:?} is refused of the wheel's shower"
    );
}

/// looks9 round 3's review of Q7: A PINNED COLOUR'S SHOWER IS THE PIN'S. A
/// chosen trail colour is drawn as itself; brought to the gold's chroma, a
/// trail pinned to `#b0b0c8` (saturation 0.12) showered grains of 0.72
/// (`#2d2da2`). Pinned to `#b0b0c8` on the default dark and on Solarized
/// Dark, no colour the shower adds (bright enough to hold a hue) reads over
/// 0.16 saturation; Sparkle's moon (`MOON_GOLD`) is its own gold, not the
/// shower's, and is not read. Negative control: brought to the gold's chroma as Q7
/// laid it (`sparkle_pin_saturated`), the shower throws grains of 0.5 and
/// more.
#[test]
fn a_pinned_colour_s_dark_sparkle_shower_is_the_pin_s() {
    let g = geom();
    let at = Instant::now();
    let caret = CursorGlow::caret_box(g, 2, 30);
    let clear = (
        caret.2 as f32 + 3.0 * g.cw as f32,
        (caret.1 + caret.3) as f32 * 0.5 - g.ch as f32,
    );
    let pin = 0x00B0_B0C8u32;
    // `(drawn, most)`: the colours the shower adds, and the most saturated.
    let shower = |saturated: bool, page: u32| -> (usize, f32) {
        let mut c = cfg(GlowStyle::Sparkle, true);
        (c.color, c.theme_bg) = (pin, page);
        let (mut drawn, mut most) = (0usize, 0.0f32);
        for seed in (0u32..4096).step_by(13).filter(|s| s & 7 != 6) {
            let mut glow = probed_glow(&c, g, at);
            glow.set_colour_pins(ColourPins {
                trail: true,
                accent: false,
            });
            glow.sparkle_pin_saturated = saturated;
            let hue = seed_hue(seed);
            glow.particles.push(Particle {
                vx: 30.0,
                vy: -20.0,
                ..grain(clear.0, clear.1, hue, at)
            });
            let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
            let t = at + Duration::from_secs_f32(peak_age(hue).min(0.4));
            glow.emit_particles(
                t,
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
            let colours = out.iter().filter(|q| q.alpha == 0).map(|q| q.color).chain(
                halos
                    .iter()
                    .filter(|h| h.mode == aterm_render::HaloMode::Add)
                    .map(|h| h.color),
            );
            for rgb in colours {
                let (_, sat, v) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
                drawn += 1;
                if v >= 24.0 / 255.0 {
                    most = most.max(sat);
                }
            }
        }
        (drawn, most)
    };
    for page in [0x0011_1318u32, 0x0000_2B36] {
        let (drawn, most) = shower(false, page);
        assert!(drawn > 100, "page {page:06x}: the shower draws ({drawn})");
        assert!(
            most <= 0.16,
            "page {page:06x}: the pinned shower reaches saturation {most:.2}"
        );
        let (_, old) = shower(true, page);
        assert!(
            old >= 0.5,
            "control: page {page:06x}: at the gold's chroma the shower reaches only {old:.2}"
        );
    }
}

/// Sparkle's or Fire's particles born at once at a caret (row 2, column 20)
/// whose rows above and below are filled, on `page`, `seams` set on the
/// glow, emitted half a second on: every box a particle laid,
/// `(x0, y0, x1, y1)`.
fn dense_row_marks(
    style: GlowStyle,
    page: u32,
    seams: impl Fn(&mut CursorGlow),
) -> Vec<(i32, i32, i32, i32)> {
    dense_row_marks_in(geom(), style, page, seams)
}

/// [`dense_row_marks`] on the grid `g`.
fn dense_row_marks_in(
    g: Geom,
    style: GlowStyle,
    page: u32,
    seams: impl Fn(&mut CursorGlow),
) -> Vec<(i32, i32, i32, i32)> {
    let (out, halos, under) = dense_row_frame(g, style, page, seams, |k| seed_hue(k * 83 + 7));
    let boxed = |x: u16, y: u16, w: u16, h: u16| {
        (
            i32::from(x),
            i32::from(y),
            i32::from(x) + i32::from(w),
            i32::from(y) + i32::from(h),
        )
    };
    let mut boxes: Vec<_> = out.iter().map(|q| boxed(q.x, q.y, q.w, q.h)).collect();
    boxes.extend(halos.iter().map(|h| boxed(h.x, h.y, h.w, h.h)));
    boxes.extend(under.iter().map(|q| boxed(q.x, q.y, q.w, q.h)));
    boxes
}

/// The particles' frame [`dense_row_marks_in`] boxes: what they lay in `out`,
/// as halos and under the letters.
fn dense_row_frame(
    g: Geom,
    style: GlowStyle,
    page: u32,
    seams: impl Fn(&mut CursorGlow),
    hue: impl Fn(u32) -> f32,
) -> (Vec<GlowQuad>, Vec<RainHalo>, Vec<GlowQuad>) {
    use aterm_core::terminal::RenderCell;
    let (row, caret_col) = (2u16, 20u16);
    let ch = g.ch as f32;
    let caret = CursorGlow::caret_box(g, row, caret_col);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let mut c = cfg(style, true);
    c.theme_bg = page;
    if page == 0x00FF_FFFF {
        (c.dark_theme, c.theme_fg) = (false, 0x001F_2328);
    }
    let at = Instant::now();
    // One frame laid on its own: the air past the caret has held clear
    // before it ([`CursorGlow::DENSE_SEAT_HOLD_S`]).
    let mut glow = CursorGlow {
        dense_seat_unheld: true,
        ..CursorGlow::default()
    };
    seams(&mut glow);
    let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
    let mut typed = vec![' '; g.cols];
    for (r, line) in cells.iter_mut().enumerate() {
        for (col, cell) in line.iter_mut().enumerate() {
            cell.bg = rgb(page);
            if r == usize::from(row) - 1 || r == usize::from(row) + 1 {
                (cell.ch, cell.bg) = ('m', rgb(0x0026_4F78));
            } else if r == usize::from(row) && col < usize::from(caret_col) {
                cell.ch = 'm';
                typed[col] = 'm';
            }
        }
    }
    glow.observe_row(row, caret_col, &typed, at);
    glow.observe_frame_text(Some((row, caret_col)), &cells, at);
    let mut out = Vec::new();
    glow.tick(Some((row, caret_col)), at, &c, g, &mut out);
    glow.particles.clear();
    let (cx, cy) = (
        (caret.0 + caret.2) as f32 * 0.5,
        (caret.1 + caret.3) as f32 * 0.5,
    );
    for k in 0..48u32 {
        let a = k as f32 * 0.7;
        glow.particles.push(Particle {
            vx: a.cos() * 1.5 * ch,
            vy: -2.0 * ch,
            life: 2.0,
            ..grain(cx, cy, hue(k), at)
        });
    }
    let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
    glow.emit_particles(
        at + Duration::from_millis(500),
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
}

/// looks10 L2, A DENSE ROW SEATS ITS STARS AND EMBERS IN ITS OWN AIR: with a
/// powerline bar over the typed line and a selection under it (letters on
/// grounds of their own), Sparkle's stars and moons and Fire's embers fly
/// out of the row into rows that drop them, and in the gallery's `context`
/// the two drew a bare glowing caret. Seated in the blank air past the
/// caret ([`CursorGlow::DENSE_SEAT_CELLS`]), each particle the dense row
/// draws lies in the caret's row, past the caret's reach, and within the
/// seats' reach of it, on the reference dark, on Solarized Dark and on paper
/// (where the gallery's `context` showed only Sparkle's gold wash and Fire's
/// flame tips, looks10 round 2).
/// Negative control: flying as over air (`dense_seats_off`), none is drawn.
#[test]
fn a_dense_row_seats_its_stars_and_embers_in_its_own_air() {
    let g = geom();
    let (row, caret_col) = (2u16, 20u16);
    let (cw, ch) = (g.cw as f32, g.ch as f32);
    let caret = CursorGlow::caret_box(g, row, caret_col);
    // Every box a particle laid: (x0, y0, x1, y1).
    let seat = |style: GlowStyle, page: u32, off: bool| {
        dense_row_marks(style, page, |g| g.dense_seats_off = off)
    };
    let reach = (ch * 0.30).ceil() as i32;
    let far = caret.2 + reach + ((CursorGlow::DENSE_SEAT_CELLS + 2.0) * cw).ceil() as i32;
    for style in [GlowStyle::Sparkle, GlowStyle::Fire] {
        for page in [0x0011_1318u32, 0x0000_2B36, 0x00FF_FFFF] {
            let boxes = seat(style, page, false);
            assert!(
                boxes.len() >= 3,
                "{style:?} on {page:06x}: the dense row seats {} marks",
                boxes.len()
            );
            for &(x0, y0, x1, y1) in &boxes {
                assert!(
                    y0 >= caret.1 && y1 <= caret.3 && x0 >= caret.2 && x1 <= far,
                    "{style:?} on {page:06x}: a seated mark at {x0}..{x1} x {y0}..{y1} leaves \
                     the air past the caret ({}..{far} x {}..{})",
                    caret.2,
                    caret.1,
                    caret.3
                );
            }
            let flown = seat(style, page, true);
            assert!(
                flown.is_empty(),
                "control: {style:?} on {page:06x} flying as over air draws {} marks",
                flown.len()
            );
        }
    }
}

/// looks10 round 3, A DENSE ROW'S EMBERS CLIMB AWAY FROM THE CARET: Fire's
/// embers seated past the caret at a random depth lay level across the row,
/// and on paper three crisp dots at the letters' height past the caret read
/// as an ellipsis (the gallery's `context`, round 2's judges). On the
/// reference dark and on paper, the seated embers farther from the caret
/// (the outer half, by x) sit higher than the nearer half by at least an
/// eighth of the row (2.6 and 2.3 px of a 16 px row). Negative control:
/// seated as round 2 seated them (`dense_embers_level`), the outer half sits
/// 1.1 px higher.
#[test]
fn a_dense_row_s_embers_climb_away_from_the_caret() {
    let ch = geom().ch as f32;
    // How far the outer half's mean centre sits above the inner half's, px.
    let climb = |page: u32, level: bool| {
        let mut marks: Vec<(f32, f32)> =
            dense_row_marks(GlowStyle::Fire, page, |g| g.dense_embers_level = level)
                .iter()
                .map(|&(x0, y0, x1, y1)| (0.5 * (x0 + x1) as f32, 0.5 * (y0 + y1) as f32))
                .collect();
        assert!(
            marks.len() >= 4,
            "fixture: {} seated marks on {page:06x}",
            marks.len()
        );
        marks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (near, far) = marks.split_at(marks.len() / 2);
        let mean = |h: &[(f32, f32)]| h.iter().map(|m| m.1).sum::<f32>() / h.len() as f32;
        mean(near) - mean(far)
    };
    for page in [0x0011_1318u32, 0x00FF_FFFF] {
        let up = climb(page, false);
        assert!(
            up >= 0.12 * ch,
            "on {page:06x}: the outer embers sit {up:.1} px above the inner ones"
        );
        let level = climb(page, true);
        assert!(
            level < 0.12 * ch,
            "control: on {page:06x} round 2's embers climb {level:.1} px away from the caret"
        );
    }
}

/// after-land L3, A DENSE ROW'S EMBERS RISE IN A PLUME: seated past the caret
/// on one straight climb with an even scatter, Fire's embers stood on one
/// line, a straight, evenly spaced dotted diagonal right of the caret on
/// every theme (`fire-*-context.t0`, x468-505 y98-112). On the gallery's
/// 26 px row, on the reference dark and on Solarized Dark (paper's beads take
/// the same seats), with each particle's seed dealt as the engine deals one,
/// the outer half of the seated embers (a halo each, by x) fans out: their
/// heights scatter by a standard deviation of a twentieth of the row or
/// more, and they still climb away from the caret (their mean 0.06 of the
/// row or more above the inner half's). Negative control: seated on the
/// composed land's straight climb (`dense_embers_line`), the outer half
/// stands in one line along the band's top, scattering under 0.025 of the
/// row. And they taper: the outer half's embers are drawn at most 0.85 of the
/// inner half's mean size and light (after-land r1: of one size, the fanned
/// embers still read as a nearly level dotted line). Negative control: drawn
/// untapered (`dense_embers_untapered`), the outer half is 0.95 of the inner
/// half's size or more.
#[test]
fn a_dense_row_s_embers_rise_in_a_plume() {
    let g = tall_geom();
    let ch = g.ch as f32;
    // Each ember's centre: on a dark page an ember is one halo.
    let embers = |page: u32, line: bool| {
        // Each particle's seed as the engine deals one (`frand`), not a step:
        // seeds a step apart seat their embers in lockstep.
        let dealt = |k: u32| {
            let mut h = k.wrapping_add(0x9E37_79B9);
            h = (h ^ (h >> 16)).wrapping_mul(0x85EB_CA6B);
            h = (h ^ (h >> 13)).wrapping_mul(0xC2B2_AE35);
            (h ^ (h >> 16)) as f32 / u32::MAX as f32
        };
        let (_, halos, _) = dense_row_frame(
            g,
            GlowStyle::Fire,
            page,
            |glow| glow.dense_embers_line = line,
            dealt,
        );
        let mut centres: Vec<(f32, f32)> = halos
            .iter()
            .map(|h| {
                (
                    f32::from(h.x) + 0.5 * f32::from(h.w),
                    f32::from(h.y) + 0.5 * f32::from(h.h),
                )
            })
            .collect();
        centres.sort_by(|a, b| a.0.total_cmp(&b.0));
        centres
    };
    // How widely the outer half's embers (by x) scatter in height (their
    // standard deviation, px), and how far their mean centre sits above the
    // inner half's.
    let spread = |page: u32, line: bool| {
        let marks = embers(page, line);
        assert!(
            marks.len() >= 8,
            "fixture: {} seated embers on {page:06x}",
            marks.len()
        );
        let (near, far) = marks.split_at(marks.len() / 2);
        let mean = |h: &[(f32, f32)]| h.iter().map(|m| m.1).sum::<f32>() / h.len() as f32;
        let m = mean(far);
        let fan =
            (far.iter().map(|f| (f.1 - m) * (f.1 - m)).sum::<f32>() / far.len() as f32).sqrt();
        (fan, mean(near) - m)
    };
    // The outer half's mean halo size and light against the inner half's.
    let taper = |page: u32, untapered: bool| {
        let dealt = |k: u32| {
            let mut h = k.wrapping_add(0x9E37_79B9);
            h = (h ^ (h >> 16)).wrapping_mul(0x85EB_CA6B);
            h = (h ^ (h >> 13)).wrapping_mul(0xC2B2_AE35);
            (h ^ (h >> 16)) as f32 / u32::MAX as f32
        };
        let (_, halos, _) = dense_row_frame(
            g,
            GlowStyle::Fire,
            page,
            |glow| glow.dense_embers_untapered = untapered,
            dealt,
        );
        let mut marks: Vec<(f32, f32, f32)> = halos
            .iter()
            .map(|h| {
                let light = [16u32, 8, 0]
                    .map(|sh| (h.color >> sh) & 0xff)
                    .iter()
                    .sum::<u32>();
                (
                    f32::from(h.x) + 0.5 * f32::from(h.w),
                    f32::from(h.w),
                    light as f32,
                )
            })
            .collect();
        marks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (near, far) = marks.split_at(marks.len() / 2);
        let mean = |h: &[(f32, f32, f32)], f: fn(&(f32, f32, f32)) -> f32| {
            h.iter().map(f).sum::<f32>() / h.len().max(1) as f32
        };
        (
            mean(far, |m| m.1) / mean(near, |m| m.1),
            mean(far, |m| m.2) / mean(near, |m| m.2),
        )
    };
    for page in [0x0011_1318u32, 0x0000_2B36] {
        let (size, light) = taper(page, false);
        let (bare_size, bare_light) = taper(page, true);
        eprintln!(
            "on {page:06x}: the outer seated embers' size and light against the inner ones' {size:.2} {light:.2}, untapered {bare_size:.2} {bare_light:.2}"
        );
        assert!(
            size <= 0.85 && light <= 0.85,
            "on {page:06x}: the outer embers are {size:.2} of the inner ones' size and {light:.2} of their light"
        );
        assert!(
            bare_size >= 0.95,
            "control: on {page:06x} the untapered outer embers are {bare_size:.2} of the inner ones' size"
        );
    }
    for page in [0x0011_1318u32, 0x0000_2B36] {
        let (fan, up) = spread(page, false);
        let (line_fan, _) = spread(page, true);
        assert!(
            fan >= 0.05 * ch,
            "on {page:06x}: the outer embers scatter {fan:.2} px in height"
        );
        assert!(
            up >= 0.06 * ch,
            "on {page:06x}: the outer embers sit {up:.1} px above the inner ones"
        );
        assert!(
            line_fan < 0.025 * ch,
            "control: on {page:06x} the composed land's outer embers scatter {line_fan:.2} px"
        );
    }
}

/// One frame of `glow`'s particles at `now`: what it lays in `out`, as halos
/// and under the letters.
fn particle_marks(
    glow: &mut CursorGlow,
    now: Instant,
    c: &GlowConfig,
    g: Geom,
) -> (Vec<GlowQuad>, Vec<RainHalo>, Vec<GlowQuad>) {
    let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
    glow.emit_particles(
        now,
        c,
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
}

/// Whether one frame of `glow`'s particles draws any mark ([`particle_marks`]).
fn particles_drawn(glow: &mut CursorGlow, now: Instant, c: &GlowConfig, g: Geom) -> bool {
    let (out, halos, under) = particle_marks(glow, now, c, g);
    out.iter().any(|q| q.alpha > 0) || !halos.is_empty() || under.iter().any(|q| q.alpha > 0)
}

/// How often `shown` goes from not drawn back to drawn once first drawn: a
/// blink each.
fn returns(shown: &[bool]) -> usize {
    let first = shown.iter().position(|&on| on).unwrap_or(shown.len());
    shown[first..].windows(2).filter(|w| !w[0] && w[1]).count()
}

/// Settings demo round 3's review: A DROP THE EDGE CUT STAYS OUT THROUGH A
/// SCROLL. A drip falling into the grid's bottom edge is drawn, then left
/// out, and an Enter on the last row scrolls it a row up
/// (`translate_scroll_state`, which every full scroll runs) 0.64 s on: it
/// stays out, dark and on paper. Negative control: held by the drop's birth
/// and origin ([`CursorGlow::set_edge_cut_by_origin`]), which the scroll
/// rewrites, the latch let it back, drawn, gone, drawn.
#[test]
fn a_drop_the_edge_cut_stays_out_through_a_scroll() {
    let g = tall_geom();
    for dark in [true, false] {
        let mut c = cfg(GlowStyle::Water, true);
        c.intensity = 1.0;
        c.dark_theme = dark;
        let run = |by_origin: bool| -> Vec<bool> {
            let at = Instant::now();
            let mut glow = CursorGlow::default();
            glow.set_edge_cut_by_origin(by_origin);
            let (x, _) = g.cell_center(7, 20);
            glow.particles.push(Particle {
                x0: x,
                y0: g.fx_bot() as f32 - 16.0,
                vx: 0.0,
                vy: 4.0,
                gy: 60.0,
                life: 2.0,
                hue: 0.8,
                cov_scale: 1.0,
                born: at,
                latch: ParticleLatch::FRESH,
                edge_cut: false,
            });
            (0..=60_u64)
                .map(|frame| {
                    if frame == 40 {
                        glow.translate_scroll_state(1, g.ch as u16);
                    }
                    particles_drawn(&mut glow, at + ms(frame * 16), &c, g)
                })
                .collect()
        };
        let held = run(false);
        let cut_by = held.iter().position(|&on| !on);
        assert!(
            held[0] && cut_by.is_some_and(|f| f < 40),
            "dark={dark}: fixture: the drop is drawn, then the edge cuts it before the scroll: {held:?}"
        );
        assert_eq!(
            returns(&held),
            0,
            "dark={dark}: the drop the edge cut came back after the scroll: {held:?}"
        );
        let keyed = run(true);
        assert!(
            returns(&keyed) > 0,
            "negative control: dark={dark}: keyed on its origin, the scrolled drop comes back: {keyed:?}"
        );
    }
}

/// Settings demo round 3's review: A SIBLING THE EDGE CUTS HOLDS OUT ONLY
/// ITSELF. Water's spray on a leap and Laser's sparks are born together at
/// one point; of two such drops, one parked mid-grid and one thrown at the
/// right edge, the parked one is drawn every frame after its sibling meets
/// the edge. Negative control: held by birth and origin
/// ([`CursorGlow::set_edge_cut_by_origin`]), the sibling's cut took the
/// parked drop out with it.
#[test]
fn a_sibling_the_edge_cuts_holds_out_only_itself() {
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Water, true);
    c.intensity = 1.0;
    let (x, y) = g.cell_center(7, 20);
    // Per frame, whether the parked drop is drawn: a mark within a cell of
    // it (its sibling, at 600 px/s, is two cells on by the third frame).
    let run = |by_origin: bool| -> Vec<bool> {
        let at = Instant::now();
        let born = |vx: f32, hue: f32| Particle {
            x0: x,
            y0: y,
            vx,
            vy: 0.0,
            gy: 0.0,
            life: 30.0,
            hue,
            cov_scale: 1.0,
            born: at,
            latch: ParticleLatch::FRESH,
            edge_cut: false,
        };
        let mut glow = CursorGlow::default();
        glow.set_edge_cut_by_origin(by_origin);
        glow.particles.push(born(0.0, 0.6));
        glow.particles.push(born(600.0, 0.3));
        let near = |px: i32| (px - x as i32).abs() < g.cw as i32;
        (0..60_u64)
            .map(|frame| {
                let (out, halos, _) = particle_marks(&mut glow, at + ms(frame * 16), &c, g);
                halos.iter().any(|h| near(i32::from(h.cx)))
                    || out.iter().any(|q| q.alpha > 0 && near(i32::from(q.x)))
            })
            .collect()
    };
    let parked = run(false);
    assert!(
        parked.iter().all(|&on| on),
        "the parked drop went out with its sibling: {parked:?}"
    );
    let keyed = run(true);
    assert!(
        keyed.iter().any(|&on| !on),
        "negative control: keyed on their shared origin, the parked drop goes out with its sibling: {keyed:?}"
    );
}

/// Settings demo round 3's review: A PAPER COMET GRAIN IS DRAWN WHOLE OR NOT
/// AT ALL, as every particle is. Its ink is laid under the letters, past the
/// judge of the marks laid in `out` and as halos: a grain 1.5 px inside the
/// left edge of a pane 20 px in stood clipped flat there, its quads at the
/// box's first column the row's full height. Now no quad it lays meets an
/// edge, and the same grain two cells in is drawn (the fixture). Negative
/// control: drawn cut ([`CursorGlow::set_motes_cut`]), its ink meets the
/// edge.
#[test]
fn a_paper_comet_grain_at_a_side_edge_is_drawn_whole_or_not_at_all() {
    let mut g = tall_geom();
    g.origin_x = 20;
    g.win_w += 40;
    let mut c = cfg(GlowStyle::Comet, true);
    c.intensity = 1.0;
    c.dark_theme = false;
    c.theme_bg = 0x00FF_FFFF;
    c.theme_fg = 0x0020_2020;
    let (_, y) = g.cell_center(4, 0);
    let (left, top, right, bottom) = (g.fx_left(), g.fx_top(), g.fx_right(), g.fx_bot());
    let meets_edge = |quads: &[GlowQuad]| {
        quads.iter().any(|q| {
            let (x, y) = (i32::from(q.x), i32::from(q.y));
            x <= left || y <= top || x + i32::from(q.w) >= right || y + i32::from(q.h) >= bottom
        })
    };
    let ink = |x: f32, cut: bool| -> Vec<GlowQuad> {
        let at = Instant::now();
        let mut glow = CursorGlow::default();
        glow.set_motes_cut(cut);
        glow.particles.push(Particle {
            life: 30.0,
            ..grain(x, y, 0.6, at)
        });
        let (_, _, under) = particle_marks(&mut glow, at + ms(16), &c, g);
        under
            .into_iter()
            .filter(|q| q.alpha > 0 || q.alpha2 > 0 || q.color != 0)
            .collect()
    };
    let inside = ink(left as f32 + 2.0 * g.cw as f32, false);
    assert!(
        !inside.is_empty() && !meets_edge(&inside),
        "fixture: the grain two cells in is inked whole: {inside:?}"
    );
    let at_edge = ink(left as f32 + 1.5, false);
    assert!(
        !meets_edge(&at_edge),
        "a paper comet grain is inked meeting the box's left edge ({left}): {at_edge:?}"
    );
    let cut = ink(left as f32 + 1.5, true);
    assert!(
        meets_edge(&cut),
        "negative control: drawn cut, the grain's ink meets the edge: {cut:?}"
    );
}

/// A DENSE ROW'S SEAT IS EACH PARTICLE'S OWN: Sparkle's stars and moons and
/// Fire's embers born where neither row beside the caret's is air are seated
/// in the air past the caret ([`CursorGlow::DENSE_SEAT_CELLS`]), and the seat
/// is read at the column each was born in, so the caret moving on to a column
/// whose row above is air, and back, leaves every seated mark where it was:
/// on the reference dark, Solarized Dark and paper, the caret's row draws the
/// same marks, at least three, on every frame while the caret goes from their
/// birth column to a gap in the row above and back again (Fire's embers seated level, `dense_embers_level`,
/// since a climbing ember's height is its distance from the caret's,
/// [`CursorGlow::DENSE_EMBER_CLIMB`]).
/// Negative control (`dense_seat_at_caret`, the seat read at the caret's
/// column every frame as looks10 round 3 read it): at the gap the row draws
/// other marks than it did beside the letters.
#[test]
fn a_dense_row_s_seat_holds_while_the_caret_crosses_a_gap() {
    use aterm_core::terminal::RenderCell;
    let g = geom();
    let (row, born_col, gap_col) = (2u16, 16u16, 4u16);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    // The marks the caret's row draws on each frame, the caret at each of
    // `cols` in turn: each box inside the row.
    let marks = |style: GlowStyle, page: u32, at_caret: bool, cols: &[u16]| {
        let mut c = cfg(style, true);
        c.theme_bg = page;
        if page == 0x00FF_FFFF {
            (c.dark_theme, c.theme_fg) = (false, 0x001F_2328);
        }
        let at = Instant::now();
        // One instant for every frame: the air past the caret has held
        // clear before it ([`CursorGlow::DENSE_SEAT_HOLD_S`]).
        let mut glow = CursorGlow {
            dense_seat_at_caret: at_caret,
            dense_embers_level: true,
            dense_seat_unheld: true,
            ..CursorGlow::default()
        };
        // Row 1 lettered from column 10 on (air above the gap's column), row
        // 3 a filled bar across, the caret's row typed up to the caret.
        let frame = |glow: &mut CursorGlow, caret_col: u16, at: Instant| {
            let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
            let mut typed = vec![' '; g.cols];
            for (r, line) in cells.iter_mut().enumerate() {
                for (col, cell) in line.iter_mut().enumerate() {
                    cell.bg = rgb(page);
                    if r == usize::from(row) - 1 && col >= 10 {
                        cell.ch = 'm';
                    } else if r == usize::from(row) + 1 {
                        (cell.ch, cell.bg) = ('m', rgb(0x0026_4F78));
                    } else if r == usize::from(row) && col < usize::from(caret_col) {
                        cell.ch = 'm';
                        typed[col] = 'm';
                    }
                }
            }
            glow.observe_row(row, caret_col, &typed, at);
            glow.observe_frame_text(Some((row, caret_col)), &cells, at);
        };
        frame(&mut glow, born_col, at);
        let mut out = Vec::new();
        glow.tick(Some((row, born_col)), at, &c, g, &mut out);
        glow.particles.clear();
        let caret = CursorGlow::caret_box(g, row, born_col);
        let (cx, cy) = (
            (caret.0 + caret.2) as f32 * 0.5,
            (caret.1 + caret.3) as f32 * 0.5,
        );
        let ch = g.ch as f32;
        for k in 0..48u32 {
            let a = k as f32 * 0.7;
            glow.particles.push(Particle {
                vx: a.cos() * 1.5 * ch,
                vy: -2.0 * ch,
                life: 2.0,
                ..grain(cx, cy, seed_hue(k * 83 + 7), at)
            });
        }
        let (top, bottom) = (caret.1, caret.3);
        let mut seq = Vec::new();
        for &col in cols {
            // One instant for every frame, so a seat's own drift through its
            // life is not read as a move.
            let t = at + Duration::from_millis(300);
            frame(&mut glow, col, t);
            glow.last = Some((row, col));
            let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
            glow.emit_particles(
                t,
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
            let mut boxes: Vec<(i32, i32, i32, i32)> = out
                .iter()
                .map(|q| (q.x, q.y, q.w, q.h))
                .chain(halos.iter().map(|h| (h.x, h.y, h.w, h.h)))
                .chain(under.iter().map(|q| (q.x, q.y, q.w, q.h)))
                .map(|(x, y, w, h)| {
                    (
                        i32::from(x),
                        i32::from(y),
                        i32::from(x) + i32::from(w),
                        i32::from(y) + i32::from(h),
                    )
                })
                .filter(|&(_, y0, _, y1)| y0 >= top && y1 <= bottom)
                .collect();
            boxes.sort_unstable();
            seq.push(boxes);
        }
        seq
    };
    let path = [born_col, born_col, gap_col, gap_col, born_col];
    for style in [GlowStyle::Sparkle, GlowStyle::Fire] {
        for page in [0x0011_1318u32, 0x0000_2B36, 0x00FF_FFFF] {
            let seq = marks(style, page, false, &path);
            assert!(
                seq[0].len() >= 3,
                "{style:?} on {page:06x}: the dense row seats {} marks",
                seq[0].len()
            );
            for (k, frame) in seq.iter().enumerate() {
                assert_eq!(
                    frame,
                    &seq[0],
                    "{style:?} on {page:06x}: with the caret at column {}, the row draws {} \
                     marks where it drew {} at its birth column",
                    path[k],
                    frame.len(),
                    seq[0].len()
                );
            }
            let old = marks(style, page, true, &path);
            assert_ne!(
                old[2],
                old[0],
                "negative control: {style:?} on {page:06x} read at the caret's column kept \
                 the row's {} marks at the gap",
                old[0].len()
            );
        }
    }
}

/// Composed E2, A DENSE ROW'S SEATS HOLD BEFORE THEY LIGHT
/// ([`CursorGlow::DENSE_SEAT_HOLD_S`]): Sparkle's stars and moons and Fire's
/// embers seated in the air past the caret lie where a shell's ghost text
/// lands (fish draws its autosuggestion after the keystroke's echo). Typing
/// a dense row on the reference dark, 80 ms a key on a 16 ms frame grid,
/// each echo lands a frame before a hint past the caret: the air past the
/// caret shows no seated mark for a run of fewer than three frames that the
/// hint then puts out, so the seats do not flash on each key. Typing the
/// same row with no hint, seated marks show. Negative control: seats lit
/// the frame the air turns clear (`dense_seat_unheld`) flash under the hint
/// on the keys.
#[test]
fn a_dense_row_s_seated_marks_do_not_flash_before_a_late_hint() {
    use aterm_core::terminal::RenderCell;
    let g = geom();
    let page = 0x0011_1318u32;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let row = 2u16;
    // Whether a seated mark shows past the caret, frame by frame.
    let lay = |style: GlowStyle, hint: bool, unheld: bool| -> Vec<bool> {
        let mut c = cfg(style, true);
        c.theme_bg = page;
        let mut glow = CursorGlow {
            dense_seat_unheld: unheld,
            ..CursorGlow::default()
        };
        let mut text = vec![' '; g.cols];
        let t0 = Instant::now();
        let mut caret = 6u16;
        let mut lit = Vec::new();
        for f in 0..=60u64 {
            let t = t0 + Duration::from_millis(16 * f);
            if f > 0 && f % 5 == 0 {
                text[usize::from(caret)] = 'm';
                caret += 1;
                glow.note_synthetic_typed(t, 1);
            }
            let hinted = hint && f % 5 != 0;
            let shown: Vec<char> = (0..g.cols)
                .map(|col| {
                    let past = col >= usize::from(caret) && col < usize::from(caret) + 6;
                    if hinted && past { 's' } else { text[col] }
                })
                .collect();
            let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
            for (r, line) in cells.iter_mut().enumerate() {
                for (col, cell) in line.iter_mut().enumerate() {
                    cell.bg = rgb(page);
                    if r + 1 == usize::from(row) || r == usize::from(row) + 1 {
                        (cell.ch, cell.bg) = ('m', rgb(0x0026_4F78));
                    } else if r == usize::from(row) {
                        cell.ch = shown[col];
                    }
                }
            }
            glow.observe_row(row, caret, &shown, t);
            glow.observe_frame_text(Some((row, caret)), &cells, t);
            let mut out = Vec::new();
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
            let caret_box = CursorGlow::caret_box(g, row, caret);
            // A cell past the caret's own edge: the seats, not the caret's
            // glow the letter guard cuts at the hint.
            let from = caret_box.2 + g.cw as i32;
            let (top, bottom) = (caret_box.1, caret_box.3);
            let mark = |x: u16, y: u16, w: u16, h: u16| {
                let (x, y, w, h) = (i32::from(x), i32::from(y), i32::from(w), i32::from(h));
                x >= from && y >= top && y + h <= bottom && w <= g.ch as i32
            };
            lit.push(
                out.iter()
                    .chain(glow.under_quads())
                    .any(|q| mark(q.x, q.y, q.w, q.h))
                    || glow.halos().iter().any(|h| mark(h.x, h.y, h.w, h.h)),
            );
        }
        lit
    };
    let flashes = |lit: &[bool]| {
        let (mut n, mut run) = (0usize, 0usize);
        for &on in lit {
            if on {
                run += 1;
            } else {
                n += usize::from(run > 0 && run < 3);
                run = 0;
            }
        }
        n
    };
    let shown = |lit: &[bool]| {
        lit.iter()
            .map(|&on| if on { '#' } else { '.' })
            .collect::<String>()
    };
    for style in [GlowStyle::Sparkle, GlowStyle::Fire] {
        let hinted = lay(style, true, false);
        assert_eq!(
            flashes(&hinted),
            0,
            "{style:?}: the seats flash before the hint: {}",
            shown(&hinted)
        );
        let bare = lay(style, false, false);
        assert!(
            bare.iter().filter(|&&on| on).count() >= 20,
            "{style:?}: with no hint the seats show: {}",
            shown(&bare)
        );
        let unheld = lay(style, true, true);
        assert!(
            flashes(&unheld) >= 6,
            "control: {style:?}'s seats lit the frame the air turns clear flash {} times: {}",
            flashes(&unheld),
            shown(&unheld)
        );
    }
}

/// After-land lows W2: beside a bar or an underline caret, dark Water's drops keep off the
/// caret's cell and the cell either side of it. A block hid the beads a splash lays in its own
/// cell and hugged the rest; a bar stood among them, two or three bright beads about it that
/// read as a `:` or a `·`, and a lone bead in the blank cell before it as a `·` of its own
/// (shape-bar's `water-dark-enter-bottom.e120`). Drops in the caret's cell and seven tenths
/// of a cell before it draw nothing with a thin caret, still bead with a block, and a drop
/// past the reach beads either way. The control lays composed r3's beads about the bar.
#[test]
fn a_bar_caret_stands_among_no_water_beads() {
    let g = geom();
    let at = Instant::now();
    let c = cfg(GlowStyle::Water, true);
    let caret = CursorGlow::caret_box(g, 2, 30);
    let mid = (
        (caret.0 + caret.2) as f32 * 0.5,
        (caret.1 + caret.3) as f32 * 0.5,
    );
    let lit = |thin: bool, control: bool, (x, y): (f32, f32)| -> usize {
        let mut glow = probed_glow(&c, g, at);
        glow.caret_thin = thin;
        glow.water_drops_on_thin_caret = control;
        glow.particles.push(grain(x, y, seed_hue(7), at));
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_particles(
            at + Duration::from_secs_f32(0.05),
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
        out.iter().filter(|q| q.color != 0).count()
            + halos.iter().filter(|h| h.color & 0x00FF_FFFF != 0).count()
    };
    let (cw, ch) = (g.cw as f32, g.ch as f32);
    for (dx, dy) in [
        (0.0f32, 0.0f32),
        (-3.0, -4.0),
        (3.0, 5.0),
        (caret.0 as f32 - 0.7 * cw - mid.0, -0.2 * ch),
    ] {
        let at_bar = (mid.0 + dx, mid.1 + dy);
        assert_eq!(
            lit(true, false, at_bar),
            0,
            "a bead beside the bar at {at_bar:?}"
        );
        assert!(
            lit(false, false, at_bar) > 0,
            "a block's cell keeps its beads"
        );
        assert!(
            lit(true, true, at_bar) > 0,
            "control: composed r3's bead beside the bar at {at_bar:?}"
        );
    }
    let past = (caret.2 as f32 + 2.0 * g.cw as f32, mid.1 - g.ch as f32);
    assert!(
        lit(true, false, past) > 0,
        "a drop past the caret's reach still beads"
    );
}

/// The review of W2: A WATER BEAD BESIDE A BAR SHOWS ONCE ([`CaretBead`]). Judged afresh each
/// frame, nearly every bead was born hidden in the caret's padded box and popped in whole
/// mid-life as the caret stepped away, and a bead the caret came to dropped out and returned at
/// the next key. A real run (a bar caret, typed at 70 ms a key, then Backspaced at 90 ms a
/// key), each bead the run spawned replayed alone against the caret's own cells: no bead's
/// marks go out and come back, and one that first shows late starts under half its next
/// reading's light. A bead held still as the caret walks past and back goes out for good at
/// the caret's first reach. The control (`water_beads_unlatched`) judges each frame alone.
#[test]
fn a_water_bead_beside_a_moving_bar_shows_once() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let (mut cols, mut born) = (Vec::new(), Vec::<Particle>::new());
    let (mut glow, mut out) = (CursorGlow::default(), Vec::new());
    glow.set_thin_caret(true);
    let (mut col, mut key, mut typed) = (4u16, 200u64, 0);
    for at in (0..3_000).step_by(16) {
        let t = t0 + Duration::from_millis(at);
        if at >= key && typed < 18 {
            col += 1;
            typed += 1;
            glow.note_synthetic_typed(t, 1);
            key += 70;
        } else if at >= key && typed < 26 {
            col -= 1;
            typed += 1;
            key += 90;
        }
        glow.tick(Some((2, col)), t, &c, g, &mut out);
        for p in &glow.particles {
            if !born
                .iter()
                .any(|b| b.born == p.born && b.hue.to_bits() == p.hue.to_bits())
            {
                born.push(*p);
            }
        }
        cols.push((at, col));
    }
    assert!(born.len() > 40, "fixture: {} beads", born.len());
    // Each frame of bead `p`'s life: its marks' light, against the caret at that frame's cell.
    let lights = |p: &Particle, unlatched: bool| -> Vec<u32> {
        let mut one = probed_glow(&c, g, t0);
        one.caret_thin = true;
        one.water_beads_unlatched = unlatched;
        one.particles.push(Particle {
            latch: ParticleLatch::FRESH,
            ..*p
        });
        let mut read = Vec::new();
        for &(at, col) in &cols {
            let t = t0 + Duration::from_millis(at);
            if t < p.born || t.duration_since(p.born).as_secs_f32() > p.life {
                continue;
            }
            one.last = Some((2, col));
            let (mut o, mut h) = (Vec::new(), Vec::new());
            one.emit_particles(
                t,
                &c,
                g,
                &mut o,
                &mut h,
                &mut crate::cursor_glow::glow_sink(
                    &mut Vec::new(),
                    &mut aterm_render::GlowRamps::default(),
                    true,
                ),
                CursorGlow::MAX_QUADS,
            );
            read.push(
                o.iter()
                    .filter(|q| q.color != 0)
                    .map(|q| u32::from(q.alpha))
                    .sum::<u32>()
                    + h.iter()
                        .map(|h| (h.color >> 16 & 0xff) + (h.color >> 8 & 0xff) + (h.color & 0xff))
                        .sum::<u32>(),
            );
        }
        read
    };
    // A bead's holes (lit, dark, lit again), and whether it first showed late at more than half
    // its next reading.
    let judge = |read: &[u32]| -> (usize, bool) {
        let Some(first) = read.iter().position(|&l| l > 0) else {
            return (0, false);
        };
        let holes = read[first..]
            .windows(2)
            .enumerate()
            .filter(|(k, w)| w[0] > 0 && w[1] == 0 && read[first + k + 1..].iter().any(|&l| l > 0))
            .count();
        let next = read.get(first + 1).copied().unwrap_or(0);
        (holes, first > 0 && read[first] * 2 > next && next > 0)
    };
    let (mut holes, mut pops, mut shown) = (0, 0, 0);
    let (mut control_holes, mut control_pops) = (0, 0);
    for p in &born {
        let read = lights(p, false);
        let (h, pop) = judge(&read);
        holes += h;
        pops += usize::from(pop);
        shown += usize::from(read.iter().any(|&l| l > 0));
        let (h, pop) = judge(&lights(p, true));
        control_holes += h;
        control_pops += usize::from(pop);
    }
    assert!(
        shown * 2 > born.len(),
        "the bar keeps its beads: {shown} of {} show",
        born.len()
    );
    assert_eq!(holes, 0, "beads went dark and came back");
    assert_eq!(pops, 0, "beads popped in whole as the caret left");
    assert!(
        control_holes > 0 && control_pops > 0,
        "control: judged each frame alone, {control_holes} holes and {control_pops} pops"
    );
    // One bead held still, the caret walking past it and back.
    let walk = |unlatched: bool| -> Vec<bool> {
        let mut one = probed_glow(&c, g, t0);
        one.caret_thin = true;
        one.water_beads_unlatched = unlatched;
        let at_bar = CursorGlow::caret_box(g, 2, 30);
        let (x, y) = (
            at_bar.0 as f32 + 3.5 * g.cw as f32,
            (at_bar.1 + at_bar.3) as f32 * 0.5 - 0.2 * g.ch as f32,
        );
        one.particles.push(grain(x, y, seed_hue(7), t0));
        [30u16, 31, 32, 33, 34, 35, 36, 37, 36, 35, 34, 33, 32]
            .into_iter()
            .enumerate()
            .map(|(k, col)| {
                one.last = Some((2, col));
                let (mut o, mut h) = (Vec::new(), Vec::new());
                one.emit_particles(
                    t0 + Duration::from_millis(20 * k as u64),
                    &c,
                    g,
                    &mut o,
                    &mut h,
                    &mut crate::cursor_glow::glow_sink(
                        &mut Vec::new(),
                        &mut aterm_render::GlowRamps::default(),
                        true,
                    ),
                    CursorGlow::MAX_QUADS,
                );
                o.iter().any(|q| q.color != 0) || h.iter().any(|h| h.color & 0x00FF_FFFF != 0)
            })
            .collect()
    };
    let shown_lit = walk(false);
    let out_at = shown_lit
        .iter()
        .position(|&lit| !lit)
        .expect("the caret reaches the bead");
    assert!(out_at > 0, "fixture: the bead shows until the caret comes");
    assert!(
        shown_lit[out_at..].iter().all(|&lit| !lit),
        "once the caret came to it the bead stays out: {shown_lit:?}"
    );
    let control = walk(true);
    let dark = control.iter().position(|&lit| !lit).expect("fixture");
    assert!(
        control[dark..].iter().any(|&lit| lit),
        "control: judged each frame alone the bead returns: {control:?}"
    );
}

/// AFTER-LAND LOWS W9: SPARKLE'S CRESCENT IS NO DOT ([`CursorGlow::MOON_MIN_R`]). At twice its
/// grain's dust radius alone a low-seeded moon was 0.14 of a cell, about 6 px across at 1x, a
/// gold dot (composed r3's judges). The lowest-seeded moon stands at least seven tenths of a
/// half cell tall on paper and on a dark page. Control: composed r3's size (`moon_r3_size`).
#[test]
fn sparkle_s_least_moon_reads_as_a_crescent() {
    let g = tall_geom();
    let born = Instant::now();
    let hue = seed_hue(6);
    let tall = |dark: bool, r3: bool| {
        let mut c = cfg(GlowStyle::Sparkle, true);
        c.dark_theme = dark;
        if !dark {
            (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
        }
        let mut glow = CursorGlow {
            moon_r3_size: r3,
            ..CursorGlow::default()
        };
        glow.particles.push(grain(100.0, 40.0, hue, born));
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_particles(
            born + Duration::from_secs_f32(peak_age(hue)),
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
        assert!(
            !out.is_empty(),
            "fixture: the moon (dark {dark}) lays its crescent"
        );
        let top = out.iter().map(|q| i32::from(q.y)).min().unwrap_or(0);
        let bot = out
            .iter()
            .map(|q| i32::from(q.y) + i32::from(q.h))
            .max()
            .unwrap_or(0);
        (bot - top) as f32 / g.ch as f32
    };
    for dark in [true, false] {
        let h = tall(dark, false);
        assert!(
            h >= 0.35,
            "dark {dark}: the least moon stands {h:.2} of a cell"
        );
        let control = tall(dark, true);
        assert!(
            control < 0.35,
            "dark {dark}: control: composed r3's least moon stands {control:.2} of a cell"
        );
    }
}
