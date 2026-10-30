// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing heat and the laser, sparkle and laser bursts.

use super::*;

/// Sustained fast typing leaves a brighter, longer wake than relaxed typing at
/// the same config (the heat model).
#[test]
fn sustained_typing_outshines_relaxed_typing() {
    let g = geom();
    let c = cfg(GlowStyle::Laser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    let mut slow = CursorGlow::default();
    let mut t = t0;
    slow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=10u16 {
        t += Duration::from_millis(500);
        slow.note_synthetic_typed(t, 1);
        slow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    let slow_lum = lum(&out);
    assert!(slow_lum > 0, "relaxed typing still glows (faintly)");

    let mut fast = CursorGlow::default();
    let mut t = t0;
    fast.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=10u16 {
        t += Duration::from_millis(50);
        fast.note_synthetic_typed(t, 1);
        fast.tick(Some((2, i)), t, &c, g, &mut out);
    }
    let fast_lum = lum(&out);
    assert!(
        fast_lum > slow_lum * 2,
        "sustained typing must overdrive the wake: fast {fast_lum} vs slow {slow_lum}"
    );
}

/// Heat builds under fast typing and cools across a pause (lazy decay).
#[test]
fn heat_cools_after_a_pause() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=10u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    assert!(
        glow.heat > 0.8,
        "sustained fast typing reaches high heat: {}",
        glow.heat
    );
    glow.tick(Some((2, 10)), t + Duration::from_secs(3), &c, g, &mut out);
    assert!(glow.heat < 0.1, "a pause cools the wake: {}", glow.heat);
}

/// A laser jump strikes lightning: a jagged main channel plus branch forks
/// spawn with the move (geometry frozen at spawn), keep emitting light while
/// alive, and are out well inside 120 ms (one flash), decaying to exactly
/// nothing.
#[test]
fn laser_jump_strikes_lightning() {
    let g = geom();
    let c = cfg(GlowStyle::Laser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((1, 38)), t0, &c, g, &mut out);
    assert!(
        glow.bolts.len() >= 3,
        "a jump forges a main channel + branch forks, got {}",
        glow.bolts.len()
    );
    assert!(
        glow.bolts.iter().any(|b| !b.branch),
        "the strike has a main channel"
    );
    assert!(
        glow.bolts.iter().any(|b| b.branch),
        "the strike throws branch forks"
    );
    let main = glow
        .bolts
        .iter()
        .find(|b| !b.branch)
        .expect("main channel exists");
    assert!(
        main.pts.len() > 8,
        "midpoint displacement kinks the channel into a real polyline: {} pts",
        main.pts.len()
    );
    glow.tick(
        Some((1, 38)),
        t0 + Duration::from_millis(40),
        &c,
        g,
        &mut out,
    );
    assert!(!out.is_empty(), "the strike is still lit at +40ms");
    assert!(!glow.bolts.is_empty(), "bolts outlive the first frames");
    glow.tick(
        Some((1, 38)),
        t0 + Duration::from_millis(105),
        &c,
        g,
        &mut out,
    );
    assert!(
        glow.bolts.is_empty(),
        "the strike is one flash, out by +105ms: {} bolts live",
        glow.bolts.len()
    );
    glow.tick(
        Some((1, 38)),
        t0 + Duration::from_millis(2000),
        &c,
        g,
        &mut out,
    );
    assert!(glow.bolts.is_empty(), "every bolt decays to nothing");
}

/// A raw Ctrl-A/Home timestamp forges no laser strike because it is not
/// movement provenance.
#[test]
fn laser_navigation_forges_no_strike() {
    let g = geom();
    let c = cfg(GlowStyle::Laser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut nav = CursorGlow::default();
    nav.tick(Some((2, 35)), t0, &c, g, &mut out);
    nav.note_navigation(t0 + Duration::from_millis(5));
    nav.tick(
        Some((2, 0)),
        t0 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert!(
        nav.bolts.is_empty(),
        "Ctrl-A forges no lightning strike, got {} bolts",
        nav.bolts.len()
    );
    assert!(
        nav.flare < 0.05,
        "and never slams the flare ({})",
        nav.flare
    );
    let mut jump = CursorGlow::default();
    jump.tick(Some((2, 35)), t0, &c, g, &mut out);
    jump.note_synthetic_move(t0 + Duration::from_millis(5));
    jump.tick(
        Some((2, 0)),
        t0 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert!(
        !jump.bolts.is_empty(),
        "a synthetic preview jump still strikes lightning"
    );
}

/// Sparkle on a light page: its grains are ink, not light. After a warm run
/// the particle pass lays no additive quad, and every grain it draws (motes,
/// the light star, the crescent and the mini-comet dash) is source-over ink
/// inside the over-text ceiling.
#[test]
fn sparkle_grains_on_a_light_page_are_ink_not_light() {
    let g = geom();
    for dark in [false, true] {
        let c = GlowConfig {
            dark_theme: dark,
            theme_fg: if dark { 0x00C8_D3F5 } else { 0x0024_292F },
            theme_bg: if dark { 0x001A_1B26 } else { 0x00FF_FFFF },
            ..cfg(GlowStyle::Sparkle, true)
        };
        let mut glow = CursorGlow::default();
        let mut t = Instant::now();
        let mut out = Vec::new();
        glow.tick(Some((2, 10)), t, &c, g, &mut out);
        for col in 11..=16u16 {
            t += Duration::from_millis(30);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((2, col)), t, &c, g, &mut out);
        }
        assert!(glow.particles.len() >= 4, "a warm run bursts sparkles");
        let (mut quads, mut halos) = (Vec::new(), Vec::new());
        glow.emit_particles(
            t,
            &c,
            g,
            &mut quads,
            &mut halos,
            &mut crate::cursor_glow::glow_sink(
                &mut Vec::new(),
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            CursorGlow::MAX_QUADS,
        );
        if dark {
            assert!(
                !quads.is_empty(),
                "control: on a dark page the shower is additive light"
            );
            continue;
        }
        for q in &quads {
            assert!(
                q.alpha > 0 && f32::from(q.alpha) <= LIGHT_INK_ALPHA_CAP,
                "a light grain's quad is source-over ink under the ceiling: {q:?}"
            );
        }
        assert!(
            !quads.is_empty() || !halos.is_empty(),
            "the grains still draw, as ink"
        );
        for h in &halos {
            assert_eq!(
                h.mode,
                aterm_render::HaloMode::Over,
                "a grain is a veil: {h:?}"
            );
            assert!(
                f32::from(aterm_render::halo_over_cap(h.color)) <= LIGHT_INK_ALPHA_CAP,
                "a grain over text stays under the over-text ceiling: {h:?}"
            );
        }
    }
}

/// One pixel's flashes, counted as WCAG 2.3.1 counts them: a flash is a rise
/// in relative luminance of 0.1 or more off a low under 0.8, completed by a
/// fall of 0.1 or more, so a ripple smaller than that is not one.
#[derive(Clone, Copy, PartialEq)]
struct FlashCounter {
    /// Rising toward a peak (since a counted rise) or falling toward a low.
    high: bool,
    /// The running extreme of the current phase.
    extreme: f32,
}

/// The flashes every pixel of a frame stream saw: per pixel, the times (s)
/// its flashes began.
struct FlashMeter {
    pixels: Vec<FlashCounter>,
    onsets: Vec<Vec<f32>>,
    /// The ground pixel, as [`Glass::new`] lays it, and the counter at rest
    /// on it.
    ground: (u32, FlashCounter),
    /// One row of bare ground.
    ground_row: Vec<u32>,
    /// Per row, the columns `lo..hi` whose counters are away from rest.
    stirred: Vec<Option<(usize, usize)>>,
    w: usize,
}

impl FlashMeter {
    fn new(g: Geom, ground: u32) -> Self {
        let (w, h) = (g.win_w as usize, g.win_h as usize);
        let rest = FlashCounter {
            high: false,
            extreme: crate::color_math::relative_luminance_with(
                crate::color_math::relative_luminance_table(),
                ground & 0x00FF_FFFF,
            ),
        };
        Self {
            pixels: vec![rest; w * h],
            onsets: vec![Vec::new(); w * h],
            ground: (ground, rest),
            ground_row: vec![ground; w],
            stirred: vec![None; h],
            w,
        }
    }

    /// Read one composited frame at `t` seconds.
    fn sample(&mut self, glass: &Glass, t: f32) {
        assert_eq!(glass.w, self.w, "the meter reads its own geometry's glass");
        let table = crate::color_math::relative_luminance_table();
        let (ground, rest) = self.ground;
        for (y, row) in glass.px.chunks_exact(self.w).enumerate() {
            if self.stirred[y].is_none() && row == self.ground_row.as_slice() {
                continue;
            }
            let off = |p: &u32| *p != ground;
            let lit = row
                .iter()
                .position(off)
                .map(|a| (a, row.iter().rposition(off).unwrap_or(a)));
            let (lo, hi) = match (lit, self.stirred[y]) {
                (None, None) => continue,
                (Some((a, b)), None) => (a, b + 1),
                (None, Some(s)) => s,
                (Some((a, b)), Some((lo, hi))) => (a.min(lo), (b + 1).max(hi)),
            };
            let mut stirred: Option<(usize, usize)> = None;
            for (x, &px) in row.iter().enumerate().take(hi).skip(lo) {
                let i = y * self.w + x;
                let l = crate::color_math::relative_luminance_with(table, px & 0x00FF_FFFF);
                let c = &mut self.pixels[i];
                if c.high {
                    if l > c.extreme {
                        c.extreme = l;
                    } else if c.extreme - l >= 0.1 {
                        *c = FlashCounter {
                            high: false,
                            extreme: l,
                        };
                    }
                } else if l < c.extreme {
                    c.extreme = l;
                } else if l - c.extreme >= 0.1 && c.extreme < 0.8 {
                    self.onsets[i].push(t);
                    *c = FlashCounter {
                        high: true,
                        extreme: l,
                    };
                }
                if *c != rest {
                    stirred = Some((stirred.map_or(x, |s| s.0), x + 1));
                }
            }
            self.stirred[y] = stirred;
        }
    }

    /// The most flashes any pixel began inside one closed second.
    fn worst_per_second(&self) -> usize {
        self.onsets
            .iter()
            .map(|on| {
                let mut lo = 0;
                let mut best = 0;
                for hi in 0..on.len() {
                    while on[hi] - on[lo] > 1.0 {
                        lo += 1;
                    }
                    best = best.max(hi + 1 - lo);
                }
                best
            })
            .max()
            .unwrap_or(0)
    }
}

/// Lightning on a dark ground, through the frames a 120 Hz display shows:
/// a storm of jumps between two cells five times a second, then a hot typing
/// run at seventeen keys a second.
fn lightning_flashes(unlimited: bool) -> (usize, usize) {
    let g = geom();
    let mut c = cfg(GlowStyle::Laser, true);
    c.intensity = 1.0;
    let mut glow = CursorGlow {
        bolts_unlimited: unlimited,
        ..CursorGlow::default()
    };
    let mut meter = FlashMeter::new(g, c.theme_bg);
    let (t0, mut out, mut verts) = (Instant::now(), Vec::new(), Vec::new());
    let mut forged = 0;
    let mut glass = Glass::new(g, c.theme_bg);
    let mut frame = |glow: &mut CursorGlow, at: Instant, caret: (u16, u16)| {
        glow.tick(Some(caret), at, &c, g, &mut out);
        let mut bolts = Vec::new();
        glow.emit_bolts(
            at,
            &c,
            g,
            &mut crate::cursor_glow::glow_sink(
                &mut bolts,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            &mut verts,
            &mut Vec::new(),
        );
        glass.px.fill(c.theme_bg);
        glass.add_quads(&bolts);
        meter.sample(&glass, at.saturating_duration_since(t0).as_secs_f32());
    };
    let mut events: Vec<(u64, (u16, u16), bool)> = Vec::new();
    for k in 0..13u64 {
        let caret = if k % 2 == 0 { (4, 34) } else { (1, 3) };
        events.push((200 * k, caret, true));
    }
    for k in 0..36u64 {
        events.push((2_700 + 55 * k, (2, 2 + k as u16), false));
    }
    frame(&mut glow, t0, (1, 3));
    for (i, &(at_ms, caret, jump)) in events.iter().enumerate() {
        let at = t0 + Duration::from_millis(at_ms + 10);
        if jump {
            glow.note_synthetic_move(at);
        } else {
            glow.note_synthetic_typed(at, 1);
        }
        let before = glow.bolts.len();
        frame(&mut glow, at, caret);
        forged += glow.bolts.len().saturating_sub(before);
        let next = events.get(i + 1).map_or(at_ms + 1_000, |e| e.0) + 10;
        let mut t = at + Duration::from_micros(8_333);
        while t < t0 + Duration::from_millis(next) {
            frame(&mut glow, t, caret);
            t += Duration::from_micros(8_333);
        }
    }
    (meter.worst_per_second(), forged)
}

/// Lightning keeps its flash and counts it (WCAG 2.3.1): a bolt is born at
/// full strength and only burns out, and strikes and crackles together are
/// born at most three a second, so no pixel flashes more than three times in
/// any second however the cursor jumps or however hot the typing.
#[test]
fn lightning_flashes_no_pixel_more_than_three_times_a_second() {
    let mut last = bolt_flash(0.0);
    assert_eq!(last, 1.0, "a bolt is born at full strength");
    for i in 1..=1000 {
        let e = bolt_flash(i as f32 / 1000.0);
        assert!(
            e <= last,
            "the envelope rises again at u = {}",
            i as f32 / 1000.0
        );
        last = e;
    }
    let (worst, forged) = lightning_flashes(false);
    assert!(
        forged > 6,
        "the storm still strikes and crackles: {forged} bolts"
    );
    assert!(
        worst as f32 <= crate::effect_util::TWINKLE_FLASH_BOUND_HZ,
        "a pixel flashed {worst} times in one second"
    );
    let (control, unlimited) = lightning_flashes(true);
    assert!(
        unlimited > forged && control as f32 > crate::effect_util::TWINKLE_FLASH_BOUND_HZ,
        "the control must strobe for the meter to mean anything: {control} flashes, \
         {unlimited} bolts"
    );
}

/// Lightning on a light ground is ink, never light: a jump strike and a hot
/// typing run with `dark_theme: false` emit no additive quad and no additive
/// halo in any stream, and the strike really paints (source-over quads in the
/// under-ink stream, where the letters it crosses draw over it).
#[test]
fn laser_on_a_light_ground_is_ink_never_light() {
    for color in [LASER_DEFAULT_COLOR, 0x0000_0000] {
        laser_on_light(color);
    }
}

/// One trail colour of [`laser_on_a_light_ground_is_ink_never_light`].
fn laser_on_light(color: u32) {
    let g = geom();
    let mut c = cfg(GlowStyle::Laser, true);
    c.color = color;
    c.dark_theme = false;
    c.theme_fg = 0x001F_2328;
    c.theme_bg = 0x00FF_FFFF;
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let table = crate::color_math::relative_luminance_table();
    let mut darkest = 1.0f32;
    let (mut painted_bolt, mut veils, mut frames) = (false, false, 0);
    let mut check = |glow: &CursorGlow, out: &[GlowQuad]| {
        let mut glass = Glass::new(g, c.theme_bg);
        glass.add_quads(glow.under_quads());
        for &px in &glass.px {
            darkest = darkest.min(crate::color_math::relative_luminance_with(table, px));
        }
        for q in out.iter().chain(glow.under_quads()) {
            assert!(q.alpha > 0, "an additive quad on a light ground: {q:?}");
        }
        for h in glow.halos() {
            assert_eq!(h.mode, HaloMode::Over, "an additive halo on light: {h:?}");
        }
        painted_bolt |= !glow.under_quads().is_empty();
        veils |= !glow.halos().is_empty();
        frames += 1;
    };
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let jump = t0 + Duration::from_millis(16);
    glow.note_synthetic_move(jump);
    glow.tick(Some((1, 38)), jump, &c, g, &mut out);
    assert!(!glow.bolts.is_empty(), "the jump strikes");
    check(&glow, &out);
    for ms in [40u64, 90, 150] {
        glow.tick(
            Some((1, 38)),
            jump + Duration::from_millis(ms),
            &c,
            g,
            &mut out,
        );
        check(&glow, &out);
    }
    let mut t = jump + Duration::from_millis(700);
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for col in 1..=16u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
        check(&glow, &out);
    }
    assert!(
        frames > 0 && painted_bolt,
        "{color:06x}: the strike paints on white"
    );
    assert!(veils, "the typing run lays its veil rails");
    assert!(
        darkest < 0.5,
        "{color:06x}: the strike visibly paints the paper: {darkest}"
    );
    let fg = crate::color_math::relative_luminance_with(table, c.theme_fg);
    let ratio = (darkest + 0.05) / (fg + 0.05);
    assert!(
        ratio >= 4.5,
        "{color:06x}: a letter over the strike's darkest pixel reads {ratio:.2}:1"
    );
}

/// A typing crackle crosses the newest letters, so on a dark ground it spends
/// the streak's over-ink budget: where a crackle runs straight through a
/// letter at full intensity, the stroke under its axis keeps 4.5:1 against
/// the counters beside it on the reference pair.
#[test]
fn a_crackle_keeps_the_letter_it_crosses_readable() {
    let g = geom();
    let mut c = cfg(GlowStyle::Laser, true);
    c.intensity = 1.0;
    c.theme_fg = CursorGlow::STREAK_REF_INK;
    c.theme_bg = CursorGlow::STREAK_REF_GROUND;
    let contrast = |unbudgeted: bool| {
        let born = Instant::now();
        let glow = CursorGlow {
            streak_unbudgeted: unbudgeted,
            bolts: vec![Bolt {
                pts: BoltPts::from_3((4.0, 40.0), (80.0, 40.0), (156.0, 40.0)),
                born,
                life: 0.2,
                branch: true,
                crackle: true,
            }],
            ..CursorGlow::default()
        };
        let (mut quads, mut verts) = (Vec::new(), Vec::new());
        glow.emit_bolts(
            born,
            &c,
            g,
            &mut crate::cursor_glow::glow_sink(
                &mut quads,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            &mut verts,
            &mut Vec::new(),
        );
        assert!(!quads.is_empty(), "the crackle draws");
        let mut glass = Glass::new(g, c.theme_bg);
        for y in 34..47 {
            glass.px[y * glass.w + 80] = c.theme_fg;
        }
        glass.add_quads(&quads);
        let l = |x: usize| crate::color_math::relative_luminance(glass.px[40 * glass.w + x]);
        let (ink, ground) = (l(80), l(84));
        (ink.max(ground) + 0.05) / (ink.min(ground) + 0.05)
    };
    let budgeted = contrast(false);
    assert!(
        budgeted >= CursorGlow::STREAK_INK_FLOOR - 0.05,
        "the crackle leaves the stroke {budgeted:.2}:1"
    );
    let control = contrast(true);
    assert!(
        control < CursorGlow::STREAK_INK_FLOOR - 0.5,
        "the control must bury the stroke for the probe to mean anything: {control:.2}:1"
    );
}

/// A crackle fades whole (looks10 L8, round 2): late in its life the light
/// left of a crackle runs from its root to well past its half (its tip
/// trails off), dimmer, or nothing of it shows; no stub of it stands apart
/// in the air. Negative controls: burnt back from its tip into its root
/// (`crackle_burns_back`, as round 1 burnt it), what is left is a dash at
/// its root, short of its half; burnt from its root out
/// (`crackle_burns_outward`, as a strike burns), its tip alone, past its
/// near quarter.
#[test]
fn a_crackle_fades_whole() {
    let g = geom();
    let c = cfg(GlowStyle::Laser, true);
    let (root, tip) = ((20.0f32, 40.0f32), (172.0f32, 40.0f32));
    let mid = (0.5 * (root.0 + tip.0), root.1);
    // (nearest lit x to the root, farthest lit x from it) at `at` ms of its
    // 200 ms life, or none when nothing of it shows
    let left = |back: bool, outward: bool, at: u64| -> Option<(i32, i32)> {
        let born = Instant::now();
        let glow = CursorGlow {
            crackle_burns_back: back,
            crackle_burns_outward: outward,
            bolts: vec![Bolt {
                pts: BoltPts::from_3(root, mid, tip),
                born,
                life: 0.2,
                branch: true,
                crackle: true,
            }],
            ..CursorGlow::default()
        };
        let (mut quads, mut verts) = (Vec::new(), Vec::new());
        glow.emit_bolts(
            born + Duration::from_millis(at),
            &c,
            g,
            &mut crate::cursor_glow::glow_sink(
                &mut quads,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            &mut verts,
            &mut Vec::new(),
        );
        let mut glass = Glass::new(g, c.theme_bg);
        glass.add_quads(&quads);
        let lit: Vec<i32> = (0..glass.w)
            .filter(|&x| {
                (30..51).any(|y| {
                    let px = glass.px[y * glass.w + x];
                    let ch = |s: u32| ((px >> s) & 0xff) as i32 - ((c.theme_bg >> s) & 0xff) as i32;
                    ch(16).abs().max(ch(8).abs()).max(ch(0).abs()) > 24
                })
            })
            .map(|x| x as i32)
            .collect();
        Some((*lit.first()?, *lit.last()?))
    };
    // Its three knots' light is laid between them, so its tip trails off
    // over its far half.
    let (near_q, half, far_q) = (
        (root.0 + 0.25 * (tip.0 - root.0)) as i32,
        (root.0 + 0.5 * (tip.0 - root.0)) as i32,
        (root.0 + 0.6 * (tip.0 - root.0)) as i32,
    );
    let mut shown = 0;
    for at in [150u64, 170, 190] {
        if let Some((near, far)) = left(false, false, at) {
            shown += 1;
            assert!(
                near <= root.0 as i32 + 4 && far >= far_q,
                "at {at} ms what is left of it runs {near}..{far}, not from its root {} past {far_q}",
                root.0
            );
        }
    }
    assert!(
        shown > 0,
        "fixture: the old crackle shows at none of the late frames"
    );
    let (_, far) = left(true, false, 150).expect("control: burnt back, its root still shows");
    assert!(
        far < half,
        "control: burnt back, what is left reaches {far}, past its half {half}"
    );
    let (near, _) = left(false, true, 150).expect("control: burnt outward, its tip still shows");
    assert!(
        near > near_q,
        "control: burnt outward, what is left starts at {near}, inside its near quarter {near_q}"
    );
}

/// A crackle kinks as a strike does (looks9's final look, looks10): every
/// crackle a key may forge, from its shortest to its longest, from twenty
/// seeds, runs in straight spans of at most three quarters of a cell, as
/// lightning does. Negative control: forged in two rounds whatever its
/// length (`crackle_two_rounds`, as it was), a long crackle is four spans of
/// more than a cell each, the bent wire Solarized Dark's `burst.t0` showed
/// in the air above the line.
#[test]
fn a_crackle_kinks_as_a_strike_does() {
    let g = geom();
    let cell = g.ch as f32;
    let longest_span = |two_rounds: bool| -> f32 {
        let mut worst = 0.0f32;
        for seed in 0..20u32 {
            for len_ch in [1.6f32, 2.4, 3.2, 4.4] {
                let mut glow = CursorGlow {
                    crackle_two_rounds: two_rounds,
                    ..CursorGlow::default()
                };
                for _ in 0..seed {
                    let _ = glow.frand();
                }
                let from = (200.0f32, 120.0f32);
                let a = 0.3 * seed as f32;
                let to = (
                    from.0 + a.cos() * len_ch * cell,
                    from.1 + a.sin() * len_ch * cell,
                );
                glow.spawn_bolt(from, to, cell, false, Instant::now());
                let bolt = glow.bolts.last().expect("a crackle is forged");
                assert!(bolt.crackle, "a small bolt is a crackle");
                let pts = bolt.pts.as_slice();
                for w in pts.windows(2) {
                    let span = crate::color_math::hypot_f32(w[1].0 - w[0].0, w[1].1 - w[0].1);
                    worst = worst.max(span / cell);
                }
            }
        }
        worst
    };
    let kinked = longest_span(false);
    assert!(
        kinked <= 0.75,
        "a crackle runs a straight span of {kinked:.2} cells"
    );
    let wire = longest_span(true);
    assert!(
        wire > 1.0,
        "control: in two rounds the longest span is only {wire:.2} cells"
    );
}

/// A crackle zigzags (looks10 round 2's L8): every crackle a key may forge,
/// from its shortest to its longest, from twenty seeds at as many angles,
/// turns by at least fifteen degrees at every knot, so no stretch of it reads
/// as a smooth arc. Negative control: its knots kicked by any amount either
/// way, the kick halving each round (`crackle_smooth_kicks`, as they were),
/// some knot bends by under five degrees, the bent-wire scratch Default
/// Light's `burst.t0` showed over `flies`.
#[test]
fn a_crackle_zigzags_at_every_knot() {
    let g = geom();
    let cell = g.ch as f32;
    let least_turn = |smooth: bool| -> f32 {
        let mut least = f32::MAX;
        for seed in 0..20u32 {
            for len_ch in [1.6f32, 2.4, 3.2, 4.4] {
                let mut glow = CursorGlow {
                    crackle_smooth_kicks: smooth,
                    ..CursorGlow::default()
                };
                for _ in 0..seed {
                    let _ = glow.frand();
                }
                let from = (200.0f32, 120.0f32);
                let a = 0.3 * seed as f32;
                let to = (
                    from.0 + a.cos() * len_ch * cell,
                    from.1 + a.sin() * len_ch * cell,
                );
                glow.spawn_bolt(from, to, cell, false, Instant::now());
                let bolt = glow.bolts.last().expect("a crackle is forged");
                assert!(bolt.crackle, "a small bolt is a crackle");
                for w in bolt.pts.as_slice().windows(3) {
                    let (u, v) = (
                        (w[1].0 - w[0].0, w[1].1 - w[0].1),
                        (w[2].0 - w[1].0, w[2].1 - w[1].1),
                    );
                    let turn = (u.0 * v.1 - u.1 * v.0).abs().atan2(u.0 * v.0 + u.1 * v.1);
                    least = least.min(turn.to_degrees());
                }
            }
        }
        least
    };
    let zigzag = least_turn(false);
    assert!(
        zigzag >= 15.0,
        "a crackle's knot bends by only {zigzag:.1} degrees"
    );
    let wire = least_turn(true);
    assert!(
        wire < 5.0,
        "control: kicked any amount, its least bend is {wire:.1} degrees"
    );
}

/// The typing streak on a dark ground through the frames a 120 Hz display
/// shows: forty keys along one row at seventeen a second, then the fade.
fn typing_streak_flashes(style: GlowStyle, chords: bool, text: bool, knobs: (u64, usize)) -> usize {
    let g = tall_geom();
    let mut c = cfg(style, true);
    c.intensity = 1.0;
    c.duration = Duration::from_millis(knobs.0);
    c.length = knobs.1;
    let mut glow = CursorGlow {
        streak_chords_rows: chords,
        ..CursorGlow::default()
    };
    let mut meter = FlashMeter::new(g, c.theme_bg);
    let (t0, mut out) = (Instant::now(), Vec::new());
    let (row, start) = (3u16, 2u16);
    let blank = vec![' '; g.cols];
    let mut glass = Glass::new(g, c.theme_bg);
    let mut frame = |glow: &mut CursorGlow, at: Instant, col: u16| {
        let caret = Some((row, col));
        let mut probe = blank.clone();
        if text {
            probe[usize::from(start)..usize::from(col)].fill('m');
        }
        glow.observe_row(row, col, &probe, at);
        glow.observe_neighbor_rows(Some(&blank), Some(&blank));
        glow.tick(caret, at, &c, g, &mut out);
        let (mut quads, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
        glow.begin_ink_ledger(&c, g, caret);
        glow.emit_comet(
            at,
            &c,
            g,
            caret,
            &mut crate::cursor_glow::glow_sink(
                &mut quads,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            &mut halos,
            &mut crate::cursor_glow::glow_sink(
                &mut under,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
        );
        glow.emit_crown(at, &c, g, caret, &mut halos, &mut under);
        glass.px.fill(c.theme_bg);
        glass.add_quads(&under);
        if text {
            for typed in start..col {
                for (y, x) in glyph(g, row, typed).0 {
                    glass.px[y as usize * glass.w + x as usize] = c.theme_fg;
                }
            }
        }
        glass.add_quads(&quads);
        glass.add_halos(&halos);
        meter.sample(&glass, at.saturating_duration_since(t0).as_secs_f32());
    };
    let frame_dt = Duration::from_micros(8_333);
    let mut t = t0;
    frame(&mut glow, t, start);
    for k in 0..40u16 {
        let key = t0 + Duration::from_millis(100 + 59 * u64::from(k));
        while t + frame_dt < key {
            t += frame_dt;
            frame(&mut glow, t, start + k);
        }
        glow.note_synthetic_typed(key, 1);
        t = key;
        frame(&mut glow, t, start + k + 1);
    }
    let end = t + Duration::from_millis(knobs.0 + 500);
    while t < end {
        t += frame_dt;
        frame(&mut glow, t, start + 40);
    }
    meter.worst_per_second()
}

/// The typing streak keeps the flash bound lightning keeps (WCAG 2.3.1, as
/// [`FlashMeter`] counts it): at seventeen keys a second, the streak and its
/// crown flash no pixel more than [`TWINKLE_FLASH_BOUND_HZ`] times in any
/// second, on bare ground and over the letters being typed, at the default
/// trail (260 ms, 24 cells) and the longest (2 s, 48 cells), for every
/// streaked style the bolt meter does not already cover.
#[test]
fn the_typing_streak_flashes_no_pixel_more_than_three_times_a_second() {
    use GlowStyle::{Comet, Fire, Laser, Lumen, Phaser, Sparkle};
    let bound = crate::effect_util::TWINKLE_FLASH_BOUND_HZ;
    let (default, longest) = ((260, 24), (2_000, 48));
    let mut cases = Vec::new();
    for style in [Lumen, Laser, Comet, Sparkle, Phaser, Fire] {
        for knobs in [default, longest] {
            for text in [false, true] {
                cases.push((style, false, text, knobs));
            }
        }
    }
    for style in [Lumen, Comet, Sparkle, Fire] {
        cases.push((style, true, false, default));
    }
    let worst = crate::test_pool::par_map(&cases, |&(style, chords, text, knobs)| {
        typing_streak_flashes(style, chords, text, knobs)
    });
    let flashes = |case| worst[cases.iter().position(|&c| c == case).expect("a run case")];
    let mut failures = Vec::new();
    for (&(style, chords, text, knobs), &worst) in cases.iter().zip(&worst) {
        if !chords && worst as f32 > bound {
            failures.push(format!(
                "{style:?} {knobs:?} text={text}: a pixel flashed {worst} times in a second"
            ));
        }
    }
    for style in [Lumen, Comet, Sparkle, Fire] {
        let chorded = flashes((style, true, false, default));
        let kept = flashes((style, false, false, default));
        if chorded <= kept {
            failures.push(format!(
                "{style:?}: control: the chorded run flashed {chorded} times a second, \
                 no more than the kept samples' {kept}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A JUMP STRIKE HOLDS ITS LIGHT OVER THE TEXT IT CROSSES: an Enter at the
/// bottom of a full screen strikes up through the lines it just printed.
#[test]
fn a_jump_strike_holds_its_light_over_the_text_it_crosses() {
    use aterm_core::terminal::RenderCell;
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Laser, true);
    c.intensity = 1.0;
    let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
    for row in [1usize, 2, 3] {
        for cell in cells[row].iter_mut().take(32) {
            cell.ch = 'm';
        }
    }
    let line: Vec<char> = (0..g.cols)
        .map(|c| if c < 32 { 'm' } else { ' ' })
        .collect();
    let blank = vec![' '; g.cols];
    let strike = |unbudgeted: bool| {
        let mut glow = CursorGlow {
            streak_unbudgeted: unbudgeted,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let t0 = Instant::now();
        glow.observe_row(1, 30, &line, t0);
        glow.observe_frame_text(Some((1, 30)), &cells, t0);
        glow.tick(Some((1, 30)), t0, &c, g, &mut out);
        let t = t0 + Duration::from_millis(16);
        glow.note_synthetic_move(t);
        glow.observe_row(5, 2, &blank, t);
        glow.observe_frame_text(Some((5, 2)), &cells, t);
        glow.tick(Some((5, 2)), t, &c, g, &mut out);
        let at = t + Duration::from_millis(8);
        let mut bolts = Vec::new();
        glow.emit_bolts(
            at,
            &c,
            g,
            &mut crate::cursor_glow::glow_sink(
                &mut bolts,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            &mut Vec::new(),
            &mut Vec::new(),
        );
        let (mut over_text, mut over_air, mut white_on_text) = (0u32, 0u32, false);
        for q in &bolts {
            let row = usize::from(q.y) / g.ch;
            let col = usize::from(q.x) / g.cw;
            let level = ((q.color >> 16) & 0xff)
                .max((q.color >> 8) & 0xff)
                .max(q.color & 0xff);
            let least = ((q.color >> 16) & 0xff)
                .min((q.color >> 8) & 0xff)
                .min(q.color & 0xff);
            if row == 2 && (2..30).contains(&col) {
                over_text = over_text.max(level);
                white_on_text |= level > 40 && least * 10 > level * 7;
            } else if row == 4 {
                over_air = over_air.max(level);
            }
        }
        (over_text, over_air, white_on_text)
    };
    let (held, air, white) = strike(false);
    let (bare, _, bare_white) = strike(true);
    assert!(
        air > 0 && held > 0,
        "fixture: the strike crosses both ({held}, {air})"
    );
    assert!(!white, "the strike lays its white core over the letters");
    assert!(
        held * 10 < bare * 8,
        "the strike over the letters is not held: {held} against {bare} unbudgeted"
    );
    assert!(
        bare_white,
        "control: the unbudgeted strike's white core never crosses the letters"
    );
}

/// looks10 round 2's L8: a crackle springs from its typed row's edge into a
/// blank row beside it, its every kink in that air, so no part of it lies
/// among the letters where the guard hid its root and left a lone violet
/// dash apart from the line (Solarized's and the dark page's `burst.t0`).
/// With the row above lettered it leaves by the row's foot; with both rows
/// beside it lettered none is forged. Negative control: rooted at its typed
/// cell's centre at any angle (`crackle_rooted_in_letters`, as it was), its
/// root and kinks lie on the letters.
#[test]
fn a_crackle_springs_from_its_row_s_edge_into_the_air() {
    use aterm_core::terminal::RenderCell;
    let g = tall_geom();
    let ch = g.ch as f32;
    let c = cfg(GlowStyle::Laser, true);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    // (crackles forged, their points on rows 1 or 2, the least y of a point)
    let forge = |rooted: bool, below_lettered: bool| -> (usize, usize, f32) {
        let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for cell in cells.iter_mut().flatten() {
            (cell.fg, cell.bg) = (rgb(c.theme_fg), rgb(c.theme_bg));
        }
        let lettered: &[usize] = if below_lettered { &[1, 3] } else { &[1] };
        for &row in lettered {
            for cell in cells[row].iter_mut().take(40) {
                cell.ch = 'm';
            }
        }
        let mut glow = CursorGlow {
            crackle_rooted_in_letters: rooted,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let t0 = Instant::now();
        let (mut forged, mut on_letters, mut least_y) = (0usize, 0usize, f32::INFINITY);
        let mut seen = Vec::new();
        for i in 0..80u16 {
            let t = t0 + Duration::from_millis(45 * u64::from(i));
            let col = 2 + i % 36;
            for cell in cells[2].iter_mut().take(usize::from(col)) {
                cell.ch = 'm';
            }
            let line: Vec<char> = cells[2].iter().map(|c| c.ch).collect();
            glow.note_synthetic_typed(t, 1);
            glow.observe_row(2, col, &line, t);
            glow.observe_frame_text(Some((2, col)), &cells, t);
            glow.tick(Some((2, col)), t, &c, g, &mut out);
            for b in glow.bolts.iter().filter(|b| b.crackle) {
                let key = (b.born, b.pts.as_slice()[0].0.to_bits());
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                forged += 1;
                for &(_, y) in b.pts.as_slice() {
                    least_y = least_y.min(y);
                    if y < 3.0 * ch && y >= ch {
                        on_letters += 1;
                    }
                }
            }
        }
        (forged, on_letters, least_y)
    };
    let (forged, on_letters, least_y) = forge(false, false);
    assert!(forged >= 3, "fixture: only {forged} crackles forged");
    assert_eq!(on_letters, 0, "crackle points lie on rows 1-2");
    assert!(
        least_y >= 3.0 * ch - 0.01,
        "a crackle reaches up to y {least_y} from the row's foot {}",
        3.0 * ch
    );
    let (walled, _, _) = forge(false, true);
    assert_eq!(
        walled, 0,
        "with no blank row beside it no crackle is forged"
    );
    let (rooted, rooted_on, _) = forge(true, false);
    assert!(
        rooted >= 3,
        "control fixture: only {rooted} crackles forged"
    );
    assert!(
        rooted_on > 0,
        "control: rooted at their cells' centres the crackles lie on the letters"
    );
}

/// The pieces of a Laser strike forged from the gallery's `enter-bottom`
/// Return (the echo's end to the new prompt, over the echo and its output)
/// that read alone, one count per seed. A piece is a run of pixels in the air
/// (off every line of text) the strike lights at all; it reads alone when it
/// touches neither caret's cell and a quarter or more of it is brighter than
/// half again the strike's light held over the letters (faint there, and on a
/// dim page cut by the guard to the letter's own share).
fn strike_pieces_apart(pair: (u32, u32), seams: impl Fn(&mut CursorGlow)) -> Vec<usize> {
    use aterm_core::terminal::RenderCell;
    let g = tall_geom();
    let (cw, ch) = (g.cw, g.ch);
    let mut c = cfg(GlowStyle::Laser, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = pair;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let lines = [
        "37",
        "38",
        "39",
        "40",
        "$ echo the quick brown fox",
        "the quick brown fox",
        "$",
    ];
    let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
    for cell in cells.iter_mut().flatten() {
        (cell.fg, cell.bg) = (rgb(pair.0), rgb(pair.1));
    }
    for (row, text) in lines.iter().enumerate() {
        for (col, letter) in text.chars().enumerate() {
            cells[row][col].ch = letter;
        }
    }
    let lettered = |x: usize, y: usize| {
        let (row, col) = (y / ch, x / cw);
        lines.get(row).is_some_and(|l| col < l.len())
    };
    let (from, to) = ((4u16, 26u16), (6u16, 2u16));
    let ends = [from, to];
    let in_end = |x: usize, y: usize| {
        ends.iter().any(|&(r, col)| {
            let (r, col) = (usize::from(r), usize::from(col));
            x + 2 >= col * cw && x < (col + 1) * cw + 2 && y + 2 >= r * ch && y < (r + 1) * ch + 2
        })
    };
    let diff = |px: u32| {
        [16u32, 8, 0]
            .iter()
            .map(|&s| (((px >> s) & 0xff) as i32 - ((pair.1 >> s) & 0xff) as i32).abs())
            .max()
            .unwrap_or(0)
    };
    let draw = |glow: &CursorGlow, now: Instant| {
        let mut quads = Vec::new();
        glow.emit_bolts(
            now,
            &c,
            g,
            &mut crate::cursor_glow::glow_sink(
                &mut quads,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            &mut Vec::new(),
            &mut Vec::new(),
        );
        let mut glass = Glass::new(g, pair.1);
        glass.add_quads(&quads);
        glass
    };
    // The strike's light held over the letters: a channel laid along the
    // echo's middle, all of it over them.
    let held = {
        let now = Instant::now();
        let mut glow = CursorGlow {
            last: Some(from),
            ..CursorGlow::default()
        };
        glow.observe_frame_text(Some(to), &cells, now);
        let (a, b) = (g.cell_center(4, 3), g.cell_center(4, 22));
        glow.bolts.push(Bolt {
            pts: BoltPts::from_3(a, (0.5 * (a.0 + b.0), a.1), b),
            born: now,
            life: 0.1,
            branch: false,
            crackle: false,
        });
        draw(&glow, now)
            .px
            .iter()
            .map(|&p| diff(p))
            .max()
            .unwrap_or(0)
    };
    assert!(
        held > 8,
        "fixture: the strike lays {held} levels over the letters"
    );
    let now = Instant::now();
    let mut apart = Vec::new();
    for seed in 0..300u32 {
        let mut glow = CursorGlow::default();
        seams(&mut glow);
        glow.last = Some(from);
        glow.observe_frame_text(Some(to), &cells, now);
        glow.rng = seed.wrapping_mul(2_654_435_761) | 1;
        glow.spawn_bolt(
            g.cell_center(from.0, from.1),
            g.cell_center(to.0, to.1),
            ch as f32,
            true,
            now,
        );
        glow.keep_strike_in_grid(g, pair.1, (from.0, to.0), now);
        let glass = draw(&glow, now);
        let bar = held * 3 / 2;
        let level: Vec<i32> = (0..glass.h)
            .flat_map(|y| (0..glass.w).map(move |x| (x, y)))
            .map(|(x, y)| {
                if lettered(x, y) {
                    0
                } else {
                    diff(glass.px[y * glass.w + x])
                }
            })
            .collect();
        let mut seen = vec![false; level.len()];
        let mut lone = 0usize;
        for start in 0..level.len() {
            if level[start] <= 24 || seen[start] {
                continue;
            }
            seen[start] = true;
            let (mut stack, mut anchored, mut bright, mut px) =
                (vec![start], false, 0usize, 0usize);
            while let Some(i) = stack.pop() {
                let (x, y) = (i % glass.w, i / glass.w);
                px += 1;
                anchored |= in_end(x, y);
                bright += usize::from(level[i] > bar);
                for (dx, dy) in [
                    (-1i32, -1i32),
                    (0, -1),
                    (1, -1),
                    (-1, 0),
                    (1, 0),
                    (-1, 1),
                    (0, 1),
                    (1, 1),
                ] {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx as usize >= glass.w || ny as usize >= glass.h {
                        continue;
                    }
                    let j = ny as usize * glass.w + nx as usize;
                    if level[j] > 24 && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
            lone += usize::from(!anchored && bright >= 3 && bright * 4 >= px);
        }
        apart.push(lone);
    }
    apart
}

/// looks10 round 3: A STRIKE SHOWS NO PIECE APART FROM ITS CARETS. On the
/// gallery's `enter-bottom` the Return's strike runs from the echo's end down
/// over its output to the new prompt; a fork forged before the output had
/// printed sprang from the channel down across `brown`, and the guard, which
/// on Solarized Dark leaves a letter its own reading, cut its run over the
/// letters and left its tip a lone violet tick in the prompt's row, 32 px
/// past the bolt (8 of 8 captures); the channel's jag swung up into the blank
/// rows over the echo and back, and the stretch between stood apart too.
/// Forged from 300 seeds over that page, on Solarized Dark and the dark page,
/// no strike leaves a piece in the air that reads alone
/// ([`strike_pieces_apart`]; 0 of 300 on each). Negative controls: forks
/// drawn over the letters too (`strike_forks_on_letters`; 221 and 189 of 300)
/// and the channel's stretches between two runs over the letters lit whole
/// (`strike_islands_lit`; 140 and 140) each leave one in a tenth of the
/// strikes or more.
#[test]
fn a_strike_shows_no_piece_apart_from_its_carets() {
    const SOLARIZED: (u32, u32) = (0x0083_9496, 0x0000_2B36);
    const DARK: (u32, u32) = (0x00D0_D0D0, 0x0011_1318);
    let count = |v: &[usize]| v.iter().filter(|&&n| n > 0).count();
    for (pair, name) in [(SOLARIZED, "Solarized Dark"), (DARK, "dark")] {
        let kept = count(&strike_pieces_apart(pair, |_| {}));
        assert_eq!(
            kept, 0,
            "{name}: {kept} of 300 strikes leave a piece apart from both carets"
        );
        let forks = count(&strike_pieces_apart(pair, |g| {
            g.strike_forks_on_letters = true
        }));
        assert!(
            forks >= 30,
            "{name} control: forks over the letters leave pieces apart in only {forks} of 300 strikes"
        );
        let islands = count(&strike_pieces_apart(pair, |g| g.strike_islands_lit = true));
        assert!(
            islands >= 30,
            "{name} control: the channel's stretches lit whole leave pieces apart in only {islands} of 300 strikes"
        );
    }
}

/// looks10 round 3, after main's card edge law (the settings card's
/// `no_light_stands_cut_at_the_cards_edge`): A STRIKE'S CORONA KEEPS INSIDE
/// ITS GRID. The Return's strike in the settings card's four rows runs from
/// the end of `git commit` on the top row down to the new prompt on the
/// third. Its channel was kept inside the grid by a fork's girth, but its
/// corona is twice a fork's, and where its jag swung into the blank last row
/// the corona stood cut at the grid's bottom edge (3 px of lavender along
/// the card's last pixel row, dark and light). Forged from 300 seeds in that
/// card's grid on the dark page, no strike lights the grid's last pixel row
/// or a side column. Negative controls, each alone: `strike_channel_fork_girth`
/// (the channel kept in by a fork's girth; 13 of 300 light the edge under a
/// keeper that read the girth across the height alone, 9 under the slabs'
/// own girth), and `bolt_ends_unread` (each segment read between its ends,
/// not the pixel past each its slabs' tiling lays; review 9 round 3's merge:
/// a steep turn in the last pixel row lit it).
#[test]
fn a_strike_s_corona_keeps_inside_its_grid() {
    use aterm_core::terminal::RenderCell;
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 4,
        cols: 48,
        origin_x: 0,
        origin_y: 0,
        win_w: 384,
        win_h: 64,
        head: 0,
    };
    let (fg, page) = (0x00D0_D0D0_u32, 0x0011_1318_u32);
    let mut c = cfg(GlowStyle::Laser, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (fg, page);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let lines = [
        "~/demo $ git add README.md && git commit",
        "[main 4f2c9e1] ship it",
        "~/demo $",
    ];
    let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
    for cell in cells.iter_mut().flatten() {
        (cell.fg, cell.bg) = (rgb(fg), rgb(page));
    }
    for (row, text) in lines.iter().enumerate() {
        for (col, letter) in text.chars().enumerate() {
            cells[row][col].ch = letter;
        }
    }
    let (from, to) = ((0u16, 40u16), (2u16, 9u16));
    let lit = |px: u32| {
        [16u32, 8, 0]
            .iter()
            .any(|&s| (((px >> s) & 0xff) as i32 - ((page >> s) & 0xff) as i32).abs() > 12)
    };
    let now = Instant::now();
    let cut = |seams: &dyn Fn(&mut CursorGlow)| {
        (0..300u32)
            .filter(|&seed| {
                let mut glow = CursorGlow::default();
                seams(&mut glow);
                glow.last = Some(from);
                glow.observe_frame_text(Some(to), &cells, now);
                glow.rng = seed.wrapping_mul(2_654_435_761) | 1;
                glow.spawn_bolt(
                    g.cell_center(from.0, from.1),
                    g.cell_center(to.0, to.1),
                    g.ch as f32,
                    true,
                    now,
                );
                glow.keep_strike_in_grid(g, page, (from.0, to.0), now);
                let mut quads = Vec::new();
                glow.emit_bolts(
                    now,
                    &c,
                    g,
                    &mut crate::cursor_glow::glow_sink(
                        &mut quads,
                        &mut aterm_render::GlowRamps::default(),
                        true,
                    ),
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                let mut glass = Glass::new(g, page);
                glass.add_quads(&quads);
                let (w, h) = (glass.w, glass.h);
                (0..w)
                    .map(|x| (x, h - 1))
                    .chain((0..h).flat_map(|y| [(0, y), (w - 1, y)]))
                    .any(|(x, y)| lit(glass.px[y * w + x]))
            })
            .count()
    };
    let kept = cut(&|_| {});
    assert_eq!(kept, 0, "{kept} of 300 strikes light the grid's edge");
    let control = cut(&|glow| glow.strike_channel_fork_girth = true);
    assert!(
        control > 0,
        "control: kept in by a fork's girth, no strike lights the grid's edge"
    );
    let ends = cut(&|glow| glow.bolt_ends_unread = true);
    assert!(
        ends > 0,
        "control: each segment read between its ends alone, no strike lights the grid's edge"
    );
}
