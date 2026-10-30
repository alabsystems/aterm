// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the beam family's streak over text on a dark ground through [`crate::cursor_fx::CursorFx::tick`].

use super::*;
use crate::cursor_fx::{CursorFx, CursorFxInput};
use crate::cursor_momentum::MOMENTUM_GLOW_TAU_S;
use aterm_core::terminal::CursorStyle;

const FG: u32 = CursorGlow::STREAK_REF_INK;
const BG: u32 = CursorGlow::STREAK_REF_GROUND;
const CURSOR: u32 = 0x0054_F97A;
const LINE: &str = "echo the quick brown fox";
const ROW: u16 = 3;
const START: u16 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
    None,
    Unbudgeted,
    Unsolved,
    NoAccount,
}

struct Shot {
    name: &'static str,
    newest: f32,
    bed: f32,
}

fn style_cfg(style: GlowStyle, mono: bool) -> (GlowConfig, TrailConfig) {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    let glow = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: crate::cursor_fx::default_trail_intensity(Some(style), mono),
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(style),
            beam: !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty),
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: mono,
        },
        CURSOR,
        true,
        FG,
        BG,
        0.5,
    );
    let trail = TrailConfig {
        enabled: matches!(style, GlowStyle::Comet)
            && !crate::cursor_fx::comet_tail_is_its_streak(&glow),
        duration: Duration::from_millis(260),
        max_len: 24,
        color: glow.color,
        intensity: 0.0,
        warmth: 0.0,
    };
    (glow, trail)
}

fn composite(fx: &CursorFx, tint: u32, g: Geom, typed: usize, light: bool) -> Glass {
    let mut glass = Glass::new(g, BG);
    for cell in &fx.trail_scratch {
        let (x0, y0, x1, y1) = glass.cell_rect(cell.row as u16, cell.col as u16);
        for y in y0.max(0)..y1.min(glass.h as i32) {
            for x in x0.max(0)..x1.min(glass.w as i32) {
                let slot = &mut glass.px[y as usize * glass.w + x as usize];
                *slot = aterm_render::blend_rgb(*slot, tint, cell.alpha);
            }
        }
    }
    glass.add_quads(fx.glow.under_quads());
    glass.add_fire_patches(fx.glow.patches());
    for (i, ch) in LINE.chars().take(typed).enumerate() {
        if ch == ' ' {
            continue;
        }
        for (y, x) in glyph(g, ROW, START + i as u16).0 {
            glass.px[y as usize * glass.w + x as usize] = FG;
        }
    }
    if light {
        glass.add_quads(&fx.glow_scratch);
        glass.add_halos(fx.glow.halos());
    }
    glass
}

fn burst(style: GlowStyle, control: Control) -> Vec<Shot> {
    burst_face(style, false, control)
}

fn burst_face(style: GlowStyle, mono: bool, control: Control) -> Vec<Shot> {
    let g = tall_geom();
    let (glow_cfg, trail_cfg) = style_cfg(style, mono);
    let mut fx = CursorFx::default();
    fx.glow.streak_unbudgeted = control == Control::Unbudgeted;
    fx.glow.classic.mono_unbedded = control == Control::Unbudgeted;
    fx.glow.bed_unsolved = control == Control::Unsolved;
    fx.glow.over_ink_idle = control == Control::NoAccount;
    let t0 = Instant::now();
    let input = |now: Instant, col: u16| CursorFxInput {
        now,
        cur: Some((ROW, col)),
        live_viewport: true,
        cursor_style: CursorStyle::SteadyBlock,
        blink_phase: true,
        live_cursor: CURSOR,
        cursor_color_pinned: false,
        default_bg: BG,
        cursor_cell_bg: None,
        geom: g,
        focused: true,
        shed_envelope: 1.0,
        trail_brightness: 1.0,
        body_allowed: true,
        momentum_glow: true,
        user_tinted: false,
        accent_pinned: false,
    };
    let probe = |typed: usize| {
        let mut cols = vec![' '; g.cols];
        cols[0] = '$';
        for (i, ch) in LINE.chars().take(typed).enumerate() {
            cols[usize::from(START) + i] = ch;
        }
        cols
    };
    let blank = vec![' '; g.cols];
    let frame = |fx: &mut CursorFx, now: Instant, typed: usize| {
        let caret = START + typed as u16;
        fx.begin(true, true);
        fx.glow.observe_row(ROW, caret, &probe(typed), now);
        fx.glow.observe_neighbor_rows(Some(&blank), Some(&blank));
        fx.tick(&input(now, caret), &glow_cfg, trail_cfg)
            .trail_color
    };
    let shoot = |fx: &CursorFx, tint: u32, name: &'static str, typed: usize| {
        let caret = START + typed as u16;
        let lit = composite(fx, tint, g, typed, true);
        let bed = composite(fx, tint, g, typed, false);
        let mut newest = f32::INFINITY;
        let mut under = f32::INFINITY;
        for col in caret.saturating_sub(3)..caret {
            let i = usize::from(col - START);
            if LINE.chars().nth(i) == Some(' ') {
                continue;
            }
            let (ink, counters) = glyph(g, ROW, col);
            newest = newest.min(ratio(&lit, &ink, &counters));
            under = under.min(ratio(&bed, &ink, &counters));
        }
        Shot {
            name,
            newest,
            bed: under,
        }
    };
    let mut shots = Vec::new();
    frame(&mut fx, t0, 0);
    let mut t = t0;
    let n = LINE.chars().count();
    let mut tint = 0;
    for typed in 1..=n {
        t += Duration::from_millis(60);
        fx.glow.note_synthetic_typed(t, 1);
        fx.trail.note_synthetic_typed(t);
        fx.cadence.on_keystroke(t);
        fx.momentum.on_key(t, MOMENTUM_GLOW_TAU_S);
        tint = frame(&mut fx, t, typed);
        if typed == 13 {
            shots.push(shoot(&fx, tint, "mid", typed));
        }
    }
    shots.push(shoot(&fx, tint, "t0", n));
    for _ in 0..4 {
        t += Duration::from_millis(30);
        tint = frame(&mut fx, t, n);
    }
    shots.push(shoot(&fx, tint, "t120", n));
    shots
}

fn bedded() -> [GlowStyle; 5] {
    [
        GlowStyle::Lumen,
        GlowStyle::Comet,
        GlowStyle::Laser,
        GlowStyle::Phaser,
        GlowStyle::Beam,
    ]
}

#[test]
fn each_dark_streak_lays_the_newest_letters_on_a_bed_at_the_bar() {
    let clean = crate::color_math::contrast_ratio(FG, BG);
    let mut failures = Vec::new();
    let mut sunk = false;
    for style in bedded() {
        let shots = burst(style, Control::None);
        for s in &shots {
            if s.bed < 4.5 {
                failures.push(format!("{style:?} {}: over the bed {:.2}:1", s.name, s.bed));
            }
            if s.newest < 3.0 {
                failures.push(format!(
                    "{style:?} {}: every light on {:.2}:1",
                    s.name, s.newest
                ));
            }
        }
        let deepest = shots.iter().map(|s| s.bed).fold(f32::INFINITY, f32::min);
        if deepest > 0.75 * clean {
            failures.push(format!(
                "{style:?}: no bed under the letters ({deepest:.2}:1 against {clean:.2}:1 clean)"
            ));
        }
        let unsolved = burst(style, Control::Unsolved);
        let lifted = unsolved.iter().map(|s| s.bed).fold(f32::INFINITY, f32::min);
        if (lifted - deepest).abs() <= 0.05 * deepest {
            failures.push(format!(
                "{style:?}: control: an unsolved bed leaves {lifted:.2}:1, as the solved {deepest:.2}:1"
            ));
        }
        sunk |= lifted < 4.5;
        let unbudgeted = burst(style, Control::Unbudgeted);
        if unbudgeted.iter().all(|s| s.newest >= 3.0) {
            failures.push(format!(
                "{style:?}: control: the light over the letters holds 3:1"
            ));
        }
    }
    for style in [GlowStyle::Fire, GlowStyle::Sparkle, GlowStyle::Water] {
        for s in burst(style, Control::None) {
            if s.newest < 3.0 {
                failures.push(format!(
                    "{style:?} {}: every light on {:.2}:1",
                    s.name, s.newest
                ));
            }
        }
    }
    let mono = burst_face(GlowStyle::Classic, true, Control::None);
    for s in &mono {
        if s.bed < 4.5 || s.newest < 3.0 {
            failures.push(format!(
                "classic mono {}: {:.2}:1 over the bed, {:.2}:1 every light on",
                s.name, s.bed, s.newest
            ));
        }
    }
    if mono.iter().map(|s| s.bed).fold(f32::INFINITY, f32::min) > 0.75 * clean {
        failures.push("classic mono: no bed under the letters".into());
    }
    let unbedded = burst_face(GlowStyle::Classic, true, Control::Unbudgeted);
    if unbedded.iter().all(|s| s.newest >= 3.0) {
        failures.push("classic mono: control: its light over the letters holds 3:1".into());
    }
    if !sunk {
        failures.push("control: no unsolved bed sinks a letter under 4.5:1".into());
    }
    let sinks = |shots: Vec<Shot>| shots.iter().any(|s| s.newest < 3.0);
    let unaccounted = bedded()
        .into_iter()
        .any(|style| sinks(burst(style, Control::NoAccount)))
        || [GlowStyle::Fire, GlowStyle::Water]
            .into_iter()
            .any(|style| sinks(burst(style, Control::NoAccount)))
        || sinks(burst_face(GlowStyle::Classic, true, Control::NoAccount));
    if !unaccounted {
        failures
            .push("control: with no over-ink account every accounted style still holds 3:1".into());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn bed_alpha_at(under: &[GlowQuad], x: u16) -> u8 {
    under
        .iter()
        .filter(|q| q.alpha > 0 && q.x <= x && x < q.x + q.w)
        .map(|q| q.alpha.max(q.alpha2))
        .max()
        .unwrap_or(0)
}

fn one_bed_over_its_life(style: GlowStyle) -> (Vec<(u8, u8)>, bool) {
    let g = tall_geom();
    let mut c = cfg(style, true);
    c.intensity = 1.0;
    c.theme_fg = FG;
    c.theme_bg = BG;
    let mut glow = CursorGlow::default();
    let (row, start) = (3u16, 4u16);
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((row, start)), t, &c, g, &mut out);
    for k in 1..=8u16 {
        t += Duration::from_millis(57);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, start + k)), t, &c, g, &mut out);
    }
    let col = start + 7;
    let (cx, cy) = g.cell_center(row, col);
    let cov_at = |glow: &CursorGlow| {
        glow.comet_runs
            .iter()
            .flatten()
            .filter(|s| (s.x - cx).abs() < 0.5 && (s.y - cy).abs() < 0.5)
            .map(|s| s.cov)
            .max()
            .unwrap_or(0)
    };
    let mut life = Vec::new();
    for _ in 0..400 {
        t += Duration::from_millis(8);
        glow.tick(Some((row, start + 8)), t, &c, g, &mut out);
        let (alpha, cov) = (bed_alpha_at(glow.under_quads(), cx as u16), cov_at(&glow));
        if cov == 0 && alpha == 0 && !life.is_empty() {
            return (life, true);
        }
        life.push((alpha, cov));
    }
    (life, false)
}

fn tracks_the_fade(life: &[(u8, u8)], full: f32) -> Result<(), String> {
    for &(alpha, cov) in life {
        let want = crate::cursor_glow::bed::DARK_BED_ALPHA_CAP * (f32::from(cov) / full).min(1.0);
        if f32::from(alpha) > want + 3.0 {
            return Err(format!(
                "bed {alpha} where its coverage {cov} asks {want:.0}"
            ));
        }
    }
    Ok(())
}

#[test]
fn the_bed_keeps_each_streaks_fade() {
    let mut failures = Vec::new();
    for style in [
        GlowStyle::Comet,
        GlowStyle::Lumen,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Phaser,
    ] {
        let full = crate::cursor_glow::bed::BedShape::of(style, 1.0)
            .expect("a bedded style")
            .full;
        let (life, out) = one_bed_over_its_life(style);
        let cap = crate::cursor_glow::bed::DARK_BED_ALPHA_CAP;
        if life.len() < 10 || f32::from(life[0].0) < 0.6 * cap || !out {
            failures.push(format!(
                "{style:?}: the cell did not fade from the cap: {life:?}"
            ));
            continue;
        }
        if let Err(e) = tracks_the_fade(&life, full) {
            failures.push(format!("{style:?}: {e}"));
        }
        if !matches!(style, GlowStyle::Laser)
            && let Some(w) = life.windows(2).find(|w| w[1].0 > w[0].0)
        {
            failures.push(format!("{style:?}: the bed re-brightened {w:?}"));
        }
        let held: Vec<(u8, u8)> = life.iter().map(|&(_, cov)| (life[0].0, cov)).collect();
        if tracks_the_fade(&held, full).is_ok() {
            failures.push(format!("{style:?}: control: a bed held at its cap passes"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_dark_beam_keeps_full_strength_over_air_and_onto_the_caret() {
    let g = tall_geom();
    let blank = vec![' '; g.cols];
    for style in [
        GlowStyle::Comet,
        GlowStyle::Lumen,
        GlowStyle::Sparkle,
        GlowStyle::Laser,
        GlowStyle::Beam,
    ] {
        let mut c = cfg(style, true);
        c.intensity = 1.0;
        let leap = |unbudgeted: bool| {
            let mut glow = CursorGlow {
                streak_unbudgeted: unbudgeted,
                ..CursorGlow::default()
            };
            let (mut out, t0) = (Vec::new(), Instant::now());
            glow.observe_row(3, 2, &blank, t0);
            glow.observe_neighbor_rows(Some(&blank), Some(&blank));
            glow.tick(Some((3, 2)), t0, &c, g, &mut out);
            let t = t0 + Duration::from_millis(16);
            glow.note_synthetic_move(t);
            glow.observe_row(3, 30, &blank, t);
            glow.observe_neighbor_rows(Some(&blank), Some(&blank));
            glow.tick(Some((3, 30)), t, &c, g, &mut out);
            let mut lit = Glass::new(g, c.theme_bg);
            lit.add_quads(&out);
            lit.add_halos(glow.halos());
            let (x0, y0, _, _) = lit.cell_rect(2, 4);
            let (_, _, x1, y1) = lit.cell_rect(4, 27);
            let px: Vec<u32> = (y0..y1)
                .flat_map(|y| (x0..x1).map(move |x| (y, x)))
                .map(|(y, x)| lit.at(y, x))
                .collect();
            (px, glow.under_quads().len())
        };
        let ((bedded, bed), (bare, _)) = (leap(false), leap(true));
        assert!(
            bare.iter().any(|&p| p != c.theme_bg),
            "{style:?}: control: the leap draws a beam"
        );
        assert!(
            bedded == bare || style == GlowStyle::Beam,
            "{style:?}: the beam over air is the full beam"
        );
        assert!(bed > 0, "{style:?}: the bed lies under the leap too");

        let head = |unbudgeted: bool| {
            let mut glow = CursorGlow {
                streak_unbudgeted: unbudgeted,
                ..CursorGlow::default()
            };
            let (mut out, mut t) = (Vec::new(), Instant::now());
            let mut text = blank.clone();
            glow.tick(Some((3, 4)), t, &c, g, &mut out);
            for k in 1..=12u16 {
                t += Duration::from_millis(57);
                text[usize::from(3 + k)] = 'm';
                glow.note_synthetic_typed(t, 1);
                glow.observe_row(3, 4 + k, &text, t);
                glow.tick(Some((3, 4 + k)), t, &c, g, &mut out);
            }
            let mut lit = Glass::new(g, c.theme_bg);
            lit.add_quads(&out);
            let (x0, y0, x1, y1) = lit.cell_rect(3, 16);
            let mut peak = 0;
            for y in y0..y1 {
                for x in x0..x1 {
                    peak = peak.max(lit.level(y, x));
                }
            }
            peak
        };
        let (kept, full) = (head(false), head(true));
        assert!(
            kept >= full,
            "{style:?}: the head over the caret's cell fell from {full} to {kept}"
        );
    }
}

fn t0_frame(style: GlowStyle, set: &dyn Fn(&mut CursorFx)) -> (Glass, u16) {
    let (lit, _, caret, _) = typed_frame(style, set, LINE.chars().count());
    (lit, caret)
}

fn typed_frame(
    style: GlowStyle,
    set: &dyn Fn(&mut CursorFx),
    typed: usize,
) -> (Glass, Glass, u16, CursorFx) {
    let g = tall_geom();
    let (glow_cfg, trail_cfg) = style_cfg(style, false);
    let mut fx = CursorFx::default();
    set(&mut fx);
    let t0 = Instant::now();
    let input = |now: Instant, col: u16| CursorFxInput {
        now,
        cur: Some((ROW, col)),
        live_viewport: true,
        cursor_style: CursorStyle::SteadyBlock,
        blink_phase: true,
        live_cursor: CURSOR,
        cursor_color_pinned: false,
        default_bg: BG,
        cursor_cell_bg: None,
        geom: g,
        focused: true,
        shed_envelope: 1.0,
        trail_brightness: 1.0,
        body_allowed: true,
        momentum_glow: true,
        user_tinted: false,
        accent_pinned: false,
    };
    let blank = vec![' '; g.cols];
    let mut tint = 0;
    let mut t = t0;
    for k in 0..=typed {
        if k > 0 {
            t += Duration::from_millis(60);
            fx.glow.note_synthetic_typed(t, 1);
            fx.cadence.on_keystroke(t);
            fx.momentum.on_key(t, MOMENTUM_GLOW_TAU_S);
        }
        let mut cols = vec![' '; g.cols];
        cols[0] = '$';
        for (i, ch) in LINE.chars().take(k).enumerate() {
            cols[usize::from(START) + i] = ch;
        }
        let caret = START + k as u16;
        fx.begin(true, true);
        fx.glow.observe_row(ROW, caret, &cols, t);
        fx.glow.observe_neighbor_rows(Some(&blank), Some(&blank));
        tint = fx.tick(&input(t, caret), &glow_cfg, trail_cfg).trail_color;
    }
    (
        composite(&fx, tint, g, typed, true),
        composite(&fx, tint, g, typed, false),
        START + typed as u16,
        fx,
    )
}

fn hsv(rgb: u32) -> (f32, f32, f32) {
    crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF)
}

fn straight(premul: u32, alpha: u8) -> u32 {
    let a = u32::from(alpha.max(1));
    let ch = |sh: u32| ((((premul >> sh) & 0xff) * 255 + a / 2) / a).min(255);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

#[test]
fn the_sparkle_comet_burns_gold_with_nothing_under_its_letters() {
    use crate::cursor_glow::bed::{BedScratch, BedShape, lay_dark_bed};
    let g = tall_geom();
    let mut wheel_off_gold = false;
    let (row_top, row_bot) = {
        let (_, y0, _, y1) = Glass::new(g, BG).cell_rect(ROW, 0);
        (y0, y1)
    };
    let gold = |rgb: u32| {
        let (h, sat, val) = hsv(rgb);
        (32.0..=62.0).contains(&h) && sat >= 0.5 && val >= 0.2
    };
    for typed in [13usize, LINE.chars().count()] {
        let (lit, bed, caret, fx) = typed_frame(GlowStyle::Sparkle, &|_| {}, typed);
        let newest = caret - 1;
        for back in [0u16, 2] {
            let (x0, _, _, _) = lit.cell_rect(ROW, newest - back);
            let rail = lit.at(row_bot + 1, x0 + g.cw as i32 / 2);
            assert!(
                gold(rail),
                "key {typed}, {back} behind the head: the rail is not gold: {rail:06x} {:?}",
                hsv(rail)
            );
        }
        let wheel = style_comet_color(GlowStyle::Sparkle, 0, 0, fx.glow.hue, 1.0);
        let (wh, _, _) = hsv(wheel);
        let off = ((wh - 45.0 + 540.0) % 360.0 - 180.0).abs();
        wheel_off_gold |= off > 40.0;
        let (x0, _, _, _) = bed.cell_rect(ROW, newest);
        let under = bed.at(row_top + g.ch as i32 / 2, x0 + 4) & 0x00FF_FFFF;
        assert_eq!(under, BG, "key {typed}: a colour under the newest letter");
    }
    assert!(
        wheel_off_gold,
        "control: the wheel stood near gold at both frames, so the gold proves nothing"
    );
    let shape = BedShape::of(GlowStyle::Sparkle, 1.0).expect("sparkle lays a rail");
    let run: Vec<CometSample> = (0..6u16)
        .map(|k| {
            let (x, y) = g.cell_center(ROW, START + k);
            CometSample {
                x,
                y,
                cov: 215,
                pos: 1.0,
            }
        })
        .collect();
    let lay = |shape: BedShape| {
        let mut scratch = BedScratch::default();
        scratch.inks.resize(run.len(), 0x0040_3A80);
        scratch.rails.resize(run.len(), 0x00FF_C040);
        let mut under = Vec::new();
        let mut ledger = InkLedger::default();
        let blank = TextSpans::default();
        let frame = FrameText {
            lo: 0,
            rows: (0..g.rows as u16)
                .map(|r| (r == ROW).then_some((i32::from(START), i32::from(START) + 6)))
                .collect(),
            ..FrameText::default()
        };
        lay_dark_bed(
            std::slice::from_ref(&run),
            g,
            BG,
            shape,
            &TextView::new(blank, Some(&frame)),
            &mut scratch,
            &mut ledger,
            &mut crate::cursor_glow::glow_sink(
                &mut under,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
        );
        let mut glass = Glass::new(g, BG);
        glass.add_quads(&under);
        glass
    };
    let x = g.cell_center(ROW, START + 3).0 as i32;
    let page = crate::color_math::relative_luminance(BG);
    let top_step = |glass: &Glass| {
        let profile: Vec<f32> = (row_bot - 4..row_bot + 3)
            .map(|y| crate::color_math::relative_luminance(glass.at(y, x) & 0x00FF_FFFF) - page)
            .collect();
        let peak = profile.iter().copied().fold(0.0f32, f32::max);
        profile
            .windows(2)
            .map(|w| w[1] - w[0])
            .fold(0.0f32, f32::max)
            / peak.max(1e-6)
    };
    let soft = top_step(&lay(shape));
    let crisp = top_step(&lay(BedShape {
        rail_rise: 0.0,
        ..shape
    }));
    assert!(
        soft <= 0.6 && crisp > soft + 0.3,
        "the rail's top: {soft:.2} of its peak in one pixel (a crisp rule: {crisp:.2})"
    );
    let dusk = lay(BedShape { dim: 0.6, ..shape });
    assert_ne!(
        dusk.at(row_top + g.ch as i32 / 2, x) & 0x00FF_FFFF,
        BG,
        "control: a shape with a share of the bar lays nothing under the letters"
    );
}

#[test]
fn the_phaser_bed_keeps_the_night_arc() {
    let g = tall_geom();
    let (_, y0, _, _) = Glass::new(g, BG).cell_rect(ROW, 0);
    let ch = g.ch as i32;
    let mut phaser = Vec::new();
    for typed in [6usize, 13, 20, LINE.chars().count()] {
        let (_, _, _, fx) = typed_frame(GlowStyle::Phaser, &|_| {}, typed);
        for q in fx.glow.under_quads() {
            let a = q.alpha.max(q.alpha2);
            let mid = i32::from(q.y) + i32::from(q.h) / 2;
            if a < 24 || mid < y0 + ch / 4 || mid > y0 + 3 * ch / 4 {
                continue;
            }
            let (h, s, _) = hsv(straight(q.color, q.alpha.max(1)));
            if s > 0.3 {
                phaser.push((h, a));
            }
        }
    }
    let night = |h: f32| (185.0..=340.0).contains(&h);
    assert!(!phaser.is_empty(), "the phaser lays a bed");
    assert!(
        phaser.iter().all(|&(h, _)| night(h)),
        "the phaser's bed leaves the night arc: {phaser:?}"
    );
    assert!(
        phaser.iter().all(|&(h, _)| !(30.0..=95.0).contains(&h)),
        "the phaser's bed lays an olive or a brown: {phaser:?}"
    );
}

#[test]
fn the_bloom_stays_whole_over_the_blank_cells_around_the_caret() {
    let mut failures = Vec::new();
    let mut cut_bites = false;
    for style in bedded() {
        let (kept, caret) = t0_frame(style, &|_| {});
        let (bare, _) = t0_frame(style, &|fx| fx.glow.over_ink_idle = true);
        let (cut, _) = t0_frame(style, &|fx| fx.glow.over_ink_uniform = true);
        let energy = |glass: &Glass| {
            let mut sum = 0i64;
            let mut worst = 0i32;
            for row in [ROW - 1, ROW, ROW + 1] {
                for col in caret + 1..=caret + 2 {
                    let (x0, y0, x1, y1) = glass.cell_rect(row, col);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            sum += i64::from(glass.level(y, x));
                            worst = worst.max((glass.level(y, x) - bare.level(y, x)).abs());
                        }
                    }
                }
            }
            (sum, worst)
        };
        let (lit, drift) = energy(&kept);
        let (whole, _) = energy(&bare);
        let (dimmed, _) = energy(&cut);
        let page = {
            let (x0, y0, x1, y1) = kept.cell_rect(ROW, caret + 1);
            i64::from((x1 - x0) * (y1 - y0) * 6) * i64::from(kept.level(0, 0))
        };
        if drift > 2 {
            failures.push(format!(
                "{style:?}: the light over air moved {drift} levels"
            ));
        }
        if lit <= page + 2000 {
            failures.push(format!(
                "{style:?}: no bloom over the blank cells ({lit} vs page {page})"
            ));
        }
        cut_bites |= dimmed * 10 <= whole * 9;
    }
    if !cut_bites {
        failures
            .push("control: no style's whole light cut to one share dims the blank cells".into());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_leap_over_rows_the_frame_shows_blank_is_light() {
    let g = tall_geom();
    let cells: Vec<Vec<aterm_core::terminal::RenderCell>> =
        vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    let mut probe = CursorGlow {
        last: Some((10, 3)),
        ..CursorGlow::default()
    };
    let mut at = Instant::now();
    for cur in [None, Some((10, 3))] {
        for frame in [&cells[..0], &cells[..2]] {
            probe.observe_frame_text(Some((0, 3)), &cells, at);
            assert!(probe.frame_text.pair(0, 3).is_some());
            assert!(TextView::new(TextSpans::default(), Some(&probe.frame_text)).knows_row(0));
            at += Duration::from_millis(1);
            probe.observe_frame_text(cur, frame, at);
            assert_eq!(probe.frame_text.at, Some(at));
            assert!(probe.frame_text.rows.is_empty());
            assert!(probe.frame_text.pairs.is_empty());
            assert_eq!(probe.frame_text.cols, 0);
            assert!(probe.frame_text.pair(0, 3).is_none());
            let text = TextView::new(TextSpans::default(), Some(&probe.frame_text));
            assert!(!text.knows_row(0));
            assert!(!text.knows_row(10));
        }
    }
    for (style, mono) in [
        (GlowStyle::Lumen, false),
        (GlowStyle::Classic, true),
        (GlowStyle::Comet, false),
    ] {
        let (c, _) = style_cfg(style, mono);
        let leap = |hand_cells: bool| {
            let mut glow = CursorGlow::default();
            let blank = vec![' '; g.cols];
            let (mut out, t0) = (Vec::new(), Instant::now());
            glow.observe_row(6, 3, &blank, t0);
            glow.tick(Some((6, 3)), t0, &c, g, &mut out);
            let t = t0 + Duration::from_millis(16);
            glow.note_synthetic_move(t);
            glow.observe_row(0, 3, &blank, t);
            if hand_cells {
                glow.observe_frame_text(Some((0, 3)), &cells, t);
            }
            glow.tick(Some((0, 3)), t, &c, g, &mut out);
            let mut lit = Glass::new(g, c.theme_bg);
            lit.add_quads(glow.under_quads());
            lit.add_quads(&out);
            lit.add_halos(glow.halos());
            let x = g.cell_center(3, 3).0 as i32;
            let mut column: Vec<f32> = (2 * g.ch as i32..5 * g.ch as i32)
                .map(|y| rel_luminance(lit.at(y, x) & 0x00FF_FFFF))
                .collect();
            column.sort_by(f32::total_cmp);
            column[column.len() / 2]
        };
        let (light, bed) = (leap(true), leap(false));
        assert!(
            light >= 0.45,
            "{style:?} (mono {mono}): the leap over blank rows peaks at {light:.3}"
        );
        assert!(
            bed <= 0.16,
            "{style:?}: control: unknown rows lit to {bed:.3} against {light:.3}"
        );
    }
}

#[test]
fn the_rail_lies_in_the_leading_under_the_run() {
    let g = tall_geom();
    let bottom = (i32::from(ROW) + 1) * g.ch as i32;
    let blank = vec![' '; g.cols];
    let text: Vec<char> = (0..g.cols).map(|_| 'm').collect();
    for (style, rail) in [
        (GlowStyle::Comet, true),
        (GlowStyle::Phaser, true),
        (GlowStyle::Sparkle, true),
        (GlowStyle::Lumen, false),
    ] {
        let (c, _) = style_cfg(style, false);
        let under_run = |below: &[char]| {
            let mut glow = CursorGlow::default();
            let (mut out, mut t) = (Vec::new(), Instant::now());
            let mut line = blank.clone();
            glow.tick(Some((ROW, START)), t, &c, g, &mut out);
            for k in 1..=6u16 {
                t += Duration::from_millis(60);
                line[usize::from(START + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
                glow.observe_row(ROW, START + k, &line, t);
                glow.observe_neighbor_rows(Some(&blank), Some(below));
                glow.tick(Some((ROW, START + k)), t, &c, g, &mut out);
            }
            glow.under_quads().to_vec()
        };
        let under = under_run(&blank);
        let below: Vec<&GlowQuad> = under.iter().filter(|q| i32::from(q.y) >= bottom).collect();
        assert!(
            under
                .iter()
                .all(|q| i32::from(q.y) >= i32::from(ROW) * g.ch as i32),
            "{style:?}: the bed leaves its row upward"
        );
        let over_text = under_run(&text);
        assert!(
            over_text.iter().all(|q| i32::from(q.y) < bottom),
            "{style:?}: control: a rail over the letters of the row below"
        );
        if !rail {
            assert!(
                below.is_empty(),
                "{style:?}: control: a rail under a style with none"
            );
            continue;
        }
        assert!(!below.is_empty(), "{style:?}: no rail under the run");
        // Over this bare row Comet's icy tail takes its depth, to the icy
        // rail's floor (looks9 round 2's Q2,
        // `comet_s_icy_tail_takes_its_depth_over_a_bare_row`).
        let reach = if matches!(style, GlowStyle::Comet) {
            crate::cursor_glow::bed::icy_rail_floor(g.ch as f32).ceil() as i32
        } else {
            (g.ch as f32 * 0.2).ceil() as i32
        };
        let mut brightest = 0.0f32;
        for q in &below {
            assert!(
                i32::from(q.y) + i32::from(q.h) <= bottom + reach,
                "{style:?}: the rail reaches past the leading: {q:?}"
            );
            if q.alpha >= 64 {
                let straight =
                    |sh: u32| (((q.color >> sh) & 0xFF) * 255 / u32::from(q.alpha)).min(255);
                let rgb = (straight(16) << 16) | (straight(8) << 8) | straight(0);
                brightest = brightest.max(rel_luminance(rgb));
            }
        }
        assert!(
            brightest >= crate::rainbow_kitty::ribbon::RAIL_LUMA_FLOOR - 0.02,
            "{style:?}: the rail's brightest ink is at {brightest:.3}"
        );
    }
}

#[test]
fn the_night_arc_keeps_the_spectrum_beds_on_blue_violet_and_magenta() {
    use crate::cursor_glow::bed::{night_hue, night_rgb};
    let mut prev = night_hue(0.0);
    for d in 0..=360 {
        let h = night_hue(d as f32);
        assert!((195.0..=330.0).contains(&h), "{d}° lands on {h}°");
        assert!((h - prev).abs() <= 3.0, "{d}° jumps from {prev}° to {h}°");
        prev = h;
    }
    let mut outside = false;
    for d in (0..360).step_by(15) {
        let raw = crate::color_math::hsv2rgb(d as f32, 0.95, 1.0);
        let (h, s, _) = crate::color_math::rgb2hsv(night_rgb(raw));
        assert!(
            s > 0.5 && (190.0..=335.0).contains(&h),
            "{d}°: night ink at {h}°"
        );
        let (rh, _, _) = crate::color_math::rgb2hsv(raw);
        outside |= !(190.0..=335.0).contains(&rh);
    }
    assert!(outside, "control: the raw spectrum never leaves the arc");
}

#[test]
fn the_comet_bed_is_a_wedge_and_every_run_rises_from_its_tail() {
    use crate::cursor_glow::bed::{BedScratch, BedShape, lay_dark_bed};
    let g = tall_geom();
    let run: Vec<CometSample> = (0..8u16)
        .map(|k| {
            let (x, y) = g.cell_center(ROW, START + k);
            CometSample {
                x,
                y,
                cov: 255,
                pos: 1.0,
            }
        })
        .collect();
    let alpha_at = |shape: BedShape, col: u16| {
        let mut scratch = BedScratch::default();
        scratch.inks.resize(run.len(), 0x0020_4060);
        let mut ledger = InkLedger::default();
        let mut under = Vec::new();
        let text =
            crate::cursor_glow::TextView::new(crate::cursor_glow::TextSpans::default(), None);
        lay_dark_bed(
            std::slice::from_ref(&run),
            g,
            BG,
            shape,
            &text,
            &mut scratch,
            &mut ledger,
            &mut crate::cursor_glow::glow_sink(
                &mut under,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
        );
        bed_alpha_at(&under, g.cell_center(ROW, col).0 as u16)
    };
    let comet = BedShape::of(GlowStyle::Comet, 1.0).expect("comet lays a bed");
    let head = START + 7;
    let (at_head, one, two, four) = (
        alpha_at(comet, head),
        alpha_at(comet, head - 1),
        alpha_at(comet, head - 2),
        alpha_at(comet, head - 4),
    );
    assert!(
        at_head > one && one > two && two > four,
        "{at_head} {one} {two} {four}"
    );
    assert!(four == 0, "the wedge is gone four cells back ({four})");
    let flat = alpha_at(
        BedShape {
            wedge: 0.0,
            ..comet
        },
        head - 3,
    );
    assert!(
        flat > 200,
        "control: a wedgeless bed three cells back is {flat}"
    );
    let lumen = BedShape::of(GlowStyle::Lumen, 1.0).expect("lumen lays a bed");
    let tail_edge = (g.cell_center(ROW, START).0 - g.cw as f32 * 0.5) as u16;
    let edge = bed_alpha_at_column(&lumen_under(&run, g, lumen), tail_edge);
    let inside = alpha_at(lumen, START + 2);
    assert!(
        edge < 20 && inside > 200,
        "lumen's tail: {edge} at its edge, {inside} inside"
    );
}

fn bed_alpha_at_column(under: &[GlowQuad], x: u16) -> u8 {
    bed_alpha_at(under, x)
}

fn lumen_under(
    run: &[CometSample],
    g: Geom,
    shape: crate::cursor_glow::bed::BedShape,
) -> Vec<GlowQuad> {
    let mut scratch = crate::cursor_glow::bed::BedScratch::default();
    scratch.inks.resize(run.len(), 0x0020_6020);
    let mut ledger = InkLedger::default();
    let mut under = Vec::new();
    let text = crate::cursor_glow::TextView::new(crate::cursor_glow::TextSpans::default(), None);
    crate::cursor_glow::bed::lay_dark_bed(
        &[run.to_vec()],
        g,
        BG,
        shape,
        &text,
        &mut scratch,
        &mut ledger,
        &mut crate::cursor_glow::glow_sink(
            &mut under,
            &mut aterm_render::GlowRamps::default(),
            true,
        ),
    );
    under
}

#[test]
fn the_bridge_onto_the_caret_stays_in_its_cell() {
    let g = tall_geom();
    let mut spills = false;
    for style in [
        GlowStyle::Beam,
        GlowStyle::Lumen,
        GlowStyle::Comet,
        GlowStyle::Phaser,
    ] {
        let (c, _) = style_cfg(style, false);
        let streak = |unbudgeted: bool| {
            let mut glow = CursorGlow {
                streak_unbudgeted: unbudgeted,
                ..CursorGlow::default()
            };
            let (mut out, mut t) = (Vec::new(), Instant::now());
            let mut text = vec![' '; g.cols];
            glow.tick(Some((ROW, START)), t, &c, g, &mut out);
            for k in 1..=8u16 {
                t += Duration::from_millis(60);
                text[usize::from(START + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
                glow.observe_row(ROW, START + k, &text, t);
                glow.tick(Some((ROW, START + k)), t, &c, g, &mut out);
            }
            let (mut light, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
            glow.emit_comet(
                t,
                &c,
                g,
                Some((ROW, START + 8)),
                &mut crate::cursor_glow::glow_sink(
                    &mut light,
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
            light
        };
        let (top, bot) = (
            i32::from(ROW) * g.ch as i32,
            (i32::from(ROW) + 1) * g.ch as i32,
        );
        let out_of_row = |qs: &[GlowQuad]| {
            qs.iter()
                .filter(|q| q.color & 0x00FF_FFFF != 0)
                .any(|q| i32::from(q.y) < top || i32::from(q.y) + i32::from(q.h) > bot)
        };
        assert!(
            !out_of_row(&streak(false)),
            "{style:?}: the streak's light leaves the typed row"
        );
        spills |= out_of_row(&streak(true));
    }
    assert!(spills, "control: no whole streak leaves its row");
}

#[test]
fn the_lines_around_the_caret_keep_the_text_bar() {
    let g = tall_geom();
    let (c, _) = style_cfg(GlowStyle::Lumen, false);
    let mut glow = CursorGlow::default();
    let line: Vec<char> = (0..g.cols).map(|_| 'm').collect();
    let t = Instant::now();
    glow.observe_row(ROW, 30, &line, t);
    glow.observe_neighbor_rows(Some(&line), Some(&vec![' '; g.cols]));
    glow.rotate_row_probe();
    glow.begin_ink_ledger(&c, g, Some((ROW, 30)));
    let mut light: Vec<GlowQuad> = [ROW - 1, ROW]
        .iter()
        .map(|&row| {
            let (x0, y0, _, _) = Glass::new(g, BG).cell_rect(row, 8);
            GlowQuad::flat(
                row,
                x0 as u16,
                y0 as u16,
                4 * g.cw as u16,
                g.ch as u16,
                0x00C0_C0C0,
                0,
            )
        })
        .collect();
    let n = light.len();
    glow.ink_ledger
        .admit_masked(&mut light, 0..n, InkLedger::MOMENTUM_FLOOR);
    let mut glass = Glass::new(g, BG);
    for row in [ROW - 1, ROW] {
        for col in 8..12 {
            for (y, x) in glyph(g, row, col).0 {
                glass.px[y as usize * glass.w + x as usize] = FG;
            }
        }
    }
    glass.add_quads(&light);
    let worst = |row: u16| {
        (8..12)
            .map(|col| {
                let (ink, counters) = glyph(g, row, col);
                ratio(&glass, &ink, &counters)
            })
            .fold(f32::INFINITY, f32::min)
    };
    let (above, caret_line) = (worst(ROW - 1), worst(ROW));
    assert!(
        above >= 4.5,
        "a letter on the line above reads {above:.2}:1"
    );
    assert!(
        (3.0..4.5).contains(&caret_line),
        "control: the caret line's letter reads {caret_line:.2}:1"
    );
}

#[test]
fn the_account_holds_a_letter_on_its_own_ground() {
    const SELECTION: u32 = 0x0033_415E;
    let g = tall_geom();
    let (c, _) = style_cfg(GlowStyle::Lumen, false);
    let line: Vec<char> = (0..g.cols).map(|_| 'm').collect();
    let mut cells = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    for (row, bg) in [(ROW - 1, SELECTION), (ROW, BG)] {
        for cell in &mut cells[usize::from(row)] {
            cell.ch = 'm';
            cell.fg = rgb(FG);
            cell.bg = rgb(bg);
        }
    }
    cells[0].clear();
    let above = |hand_cells: bool| {
        let mut glow = CursorGlow::default();
        let t = Instant::now();
        glow.observe_row(ROW, 30, &line, t);
        glow.observe_neighbor_rows(Some(&line), Some(&vec![' '; g.cols]));
        if hand_cells {
            glow.observe_frame_text(Some((ROW, 30)), &cells, t);
        }
        glow.rotate_row_probe();
        glow.begin_ink_ledger(&c, g, Some((ROW, 30)));
        let (x0, y0, _, _) = Glass::new(g, BG).cell_rect(ROW - 1, 8);
        let mut light = vec![GlowQuad::flat(
            ROW - 1,
            x0 as u16,
            y0 as u16,
            4 * g.cw as u16,
            g.ch as u16,
            0x00C0_C0C0,
            0,
        )];
        glow.ink_ledger
            .admit_masked(&mut light, 0..1, InkLedger::MOMENTUM_FLOOR);
        let mut glass = Glass::new(g, BG);
        let (sx0, sy0, _, sy1) = glass.cell_rect(ROW - 1, 0);
        for y in sy0..sy1 {
            for x in sx0..glass.w as i32 {
                glass.px[y as usize * glass.w + x as usize] = SELECTION;
            }
        }
        for col in 8..12 {
            for (y, x) in glyph(g, ROW - 1, col).0 {
                glass.px[y as usize * glass.w + x as usize] = FG;
            }
        }
        glass.add_quads(&light);
        (8..12)
            .map(|col| {
                let (ink, counters) = glyph(g, ROW - 1, col);
                ratio(&glass, &ink, &counters)
            })
            .fold(f32::INFINITY, f32::min)
    };
    let (held, page) = (above(true), above(false));
    assert!(
        held >= 4.5,
        "a selected letter reads {held:.2}:1 on its own ground"
    );
    assert!(
        page < 4.5,
        "control: held against the page it reads {page:.2}:1"
    );
}

#[test]
fn a_leap_bed_is_one_tube_across_a_short_gap() {
    use crate::cursor_glow::bed::{BedScratch, BedShape, lay_dark_bed};
    let g = tall_geom();
    let shape = BedShape::of(GlowStyle::Phaser, 1.0).expect("the phaser lays a bed");
    assert!(
        !shape.leap_air,
        "fixture: the phaser's leap leaves the air to its light"
    );
    let tube_at_gap = |gap: u16| -> u32 {
        let end3 = 11 - gap;
        let cells: Vec<(u16, u16)> = (11..=14u16)
            .rev()
            .map(|c| (2, c))
            .chain((2..=10u16).rev().map(|c| (3, c)))
            .collect();
        let run: Vec<CometSample> = cells
            .iter()
            .map(|&(r, c)| {
                let (x, y) = g.cell_center(r, c);
                CometSample {
                    x,
                    y,
                    cov: 215,
                    pos: 1.0,
                }
            })
            .collect();
        let frame = FrameText {
            lo: 0,
            rows: (0..g.rows as u16)
                .map(|r| match r {
                    2 => Some((0, 15)),
                    3 => Some((0, i32::from(end3))),
                    _ => None,
                })
                .collect(),
            ..FrameText::default()
        };
        let mut scratch = BedScratch::default();
        scratch.inks.resize(run.len(), 0x0040_3A80);
        let mut under = Vec::new();
        let mut ledger = InkLedger::default();
        lay_dark_bed(
            std::slice::from_ref(&run),
            g,
            BG,
            BedShape {
                wedge: 0.0,
                ..shape
            },
            &TextView::new(TextSpans::default(), Some(&frame)),
            &mut scratch,
            &mut ledger,
            &mut crate::cursor_glow::glow_sink(
                &mut under,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
        );
        let mut glass = Glass::new(g, BG);
        glass.add_quads(&under);
        let mid = f32::from(end3) + f32::from(gap) * 0.5;
        let x = (mid * g.cw as f32) as i32;
        let (y0, y1) = (2 * g.ch as i32, 4 * g.ch as i32);
        (y0..y1)
            .map(|y| (glass.level(y, x) - glass.level(0, 0)).max(0) as u32)
            .max()
            .unwrap_or(0)
    };
    assert!(
        tube_at_gap(2) > 8,
        "the leap's bed breaks over two cells of air"
    );
    assert_eq!(
        tube_at_gap(6),
        0,
        "control: across six cells of air the bed still lays a tube"
    );
}

/// The ink a Comet lays under the row after `typed` keys and one completion of `jump` cells at
/// Trail `length`, on the dark (or light) reference ground: before the completion, and on the
/// frame after it, one alpha a column.
fn comet_completion_beds(
    dark: bool,
    typed: usize,
    jump: usize,
    length: usize,
) -> (Vec<u8>, Vec<u8>) {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    let g = tall_geom();
    let line: Vec<char> = LINE.chars().collect();
    let (fg, bg) = if dark {
        (FG, BG)
    } else {
        (0x001F_2328, 0x00FF_FFFF)
    };
    let glow_cfg = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length,
            intensity: crate::cursor_fx::default_trail_intensity(Some(GlowStyle::Comet), false),
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(GlowStyle::Comet),
            beam: true,
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
        },
        CURSOR,
        dark,
        fg,
        bg,
        0.5,
    );
    let trail_cfg = TrailConfig {
        enabled: !crate::cursor_fx::comet_tail_is_its_streak(&glow_cfg),
        duration: Duration::from_millis(260),
        max_len: length,
        color: glow_cfg.color,
        intensity: 0.0,
        warmth: 0.0,
    };
    let mut fx = CursorFx::default();
    let t0 = Instant::now();
    let input = |now: Instant, col: u16| CursorFxInput {
        now,
        cur: Some((ROW, col)),
        live_viewport: true,
        cursor_style: CursorStyle::SteadyBlock,
        blink_phase: true,
        live_cursor: CURSOR,
        cursor_color_pinned: false,
        default_bg: bg,
        cursor_cell_bg: None,
        geom: g,
        focused: true,
        shed_envelope: 1.0,
        trail_brightness: 1.0,
        body_allowed: true,
        momentum_glow: true,
        user_tinted: false,
        accent_pinned: false,
    };
    let blank = vec![' '; g.cols];
    let row_with = |shown: usize| {
        let mut cols = vec![' '; g.cols];
        cols[0] = '$';
        for (i, ch) in line.iter().take(shown).enumerate() {
            cols[usize::from(START) + i] = *ch;
        }
        cols
    };
    let bed_row = |fx: &CursorFx| -> Vec<u8> {
        (0..g.cols as u16)
            .map(|col| bed_alpha_at(fx.glow.under_quads(), g.cell_center(ROW, col).0 as u16))
            .collect()
    };
    let mut t = t0;
    for k in 0..=typed {
        if k > 0 {
            t += Duration::from_millis(60);
            fx.glow.note_synthetic_typed(t, 1);
            fx.cadence.on_keystroke(t);
            fx.momentum.on_key(t, MOMENTUM_GLOW_TAU_S);
        }
        let caret = START + k as u16;
        fx.begin(true, true);
        fx.glow.observe_row(ROW, caret, &row_with(k), t);
        fx.glow.observe_neighbor_rows(Some(&blank), Some(&blank));
        fx.tick(&input(t, caret), &glow_cfg, trail_cfg);
    }
    let before = bed_row(&fx);
    t += Duration::from_millis(60);
    fx.cadence.on_keystroke(t);
    fx.glow.note_user_gesture(t);
    fx.trail.note_user_gesture(t);
    let caret = START + (typed + jump) as u16;
    fx.begin(true, true);
    fx.glow.observe_row(ROW, caret, &row_with(typed + jump), t);
    fx.glow.observe_neighbor_rows(Some(&blank), Some(&blank));
    fx.tick(&input(t, caret), &glow_cfg, trail_cfg);
    t += Duration::from_millis(16);
    fx.begin(true, true);
    fx.glow.observe_row(ROW, caret, &row_with(typed + jump), t);
    fx.glow.observe_neighbor_rows(Some(&blank), Some(&blank));
    fx.tick(&input(t, caret), &glow_cfg, trail_cfg);
    (before, bed_row(&fx))
}

#[test]
fn a_comet_completion_lays_the_path_length_cuts_under_its_letters() {
    let (typed, jump) = (10_usize, 6_usize);
    let (short_before, short_after) = comet_completion_beds(true, typed, jump, 2);
    let (long_before, long_after) = comet_completion_beds(true, typed, jump, 24);
    assert_eq!(
        short_before, long_before,
        "control: typing alone lays one bed at every length"
    );
    let path = usize::from(START) + typed..usize::from(START) + typed + jump;
    let apart: u32 = path
        .clone()
        .map(|col| u32::from(long_after[col].abs_diff(short_after[col])))
        .sum();
    let longer = path
        .clone()
        .filter(|&col| long_after[col] > short_after[col] + 40)
        .count();
    assert!(
        longer >= 1 && apart > 60,
        "Length cuts the completion's bed: {:?} at 24 against {:?} at 2",
        &long_after[path.clone()],
        &short_after[path]
    );
}

/// On a light ground a Comet completion's ink is as long as the path it laid, the path Trail
/// length cut, as the dark bed's is: before, the light Comet drew every run as the typing
/// wedge, 3.5 cells behind its head, so a completion looked the same at every length (only its
/// faint trail cells moved, at most 12 levels apart in the preview).
#[test]
fn a_light_comet_completion_lays_the_path_length_cuts() {
    let (typed, jump) = (10_usize, 8_usize);
    let (short_before, short_after) = comet_completion_beds(false, typed, jump, 2);
    let (long_before, long_after) = comet_completion_beds(false, typed, jump, 24);
    assert_eq!(
        short_before, long_before,
        "control: typing alone lays one ink at every length"
    );
    // Past the typing wedge's reach behind the head: only a jump's own path inks there.
    let head = usize::from(START) + typed + jump;
    let far = head - jump..head - 4;
    let longer = far
        .clone()
        .filter(|&col| long_after[col] > short_after[col] + 8)
        .count();
    assert!(
        longer >= 2,
        "Length cuts the completion's ink: {:?} at 24 against {:?} at 2",
        &long_after[far.clone()],
        &short_after[far]
    );
}

fn enter_leap(style: GlowStyle, split: bool) -> (Glass, Glass, usize, (f32, f32), (f32, f32)) {
    let g = tall_geom();
    let (c, _) = style_cfg(style, false);
    let mut cells = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for (row, text) in [
        (2, "$ echo the quick brown fox"),
        (3, "the quick brown fox"),
        (4, "$ "),
    ] {
        for (cell, ch) in cells[row].iter_mut().zip(text.chars()) {
            cell.ch = ch;
        }
    }
    let (from, to) = ((2u16, 26u16), (4u16, 2u16));
    let mut glow = CursorGlow {
        leap_split: split,
        ..CursorGlow::default()
    };
    let (mut out, t0) = (Vec::new(), Instant::now());
    glow.observe_frame_text(Some(from), &cells, t0);
    glow.tick(Some(from), t0, &c, g, &mut out);
    let t = t0 + Duration::from_millis(16);
    glow.note_synthetic_move(t);
    glow.observe_frame_text(Some(to), &cells, t);
    glow.tick(Some(to), t, &c, g, &mut out);
    let mut lit = Glass::new(g, c.theme_bg);
    lit.add_quads(glow.under_quads());
    lit.add_quads(&out);
    lit.add_halos(glow.halos());
    let mut under = Glass::new(g, c.theme_bg);
    under.add_quads(glow.under_quads());
    (
        lit,
        under,
        glow.particles.len(),
        g.cell_center(from.0, from.1),
        g.cell_center(to.0, to.1),
    )
}

fn along(a: (f32, f32), b: (f32, f32), skip_a: f32, skip_b: f32) -> Vec<(f32, f32)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let n = (len - skip_a - skip_b).max(0.0) as usize;
    (0..=n)
        .map(|i| {
            let d = skip_a + i as f32;
            (a.0 + dx * d / len, a.1 + dy * d / len)
        })
        .collect()
}

fn lift_across(glass: &Glass, (x, y): (f32, f32), bg: i32) -> i32 {
    (-5..=5)
        .map(|k| glass.level(y as i32 + k, x as i32) - bg)
        .max()
        .unwrap_or(0)
}

fn whiteness(p: u32) -> u32 {
    ((p >> 16) & 0xff).min((p >> 8) & 0xff).min(p & 0xff)
}

#[test]
fn a_beam_leap_is_its_ice_tube_and_only_the_comet_burns_a_head() {
    let bg = Glass::new(tall_geom(), BG).level(0, 0);
    let (lit, under, dust, a, b) = enter_leap(GlowStyle::Beam, false);
    let cell = tall_geom().cw as f32;
    let path = along(a, b, 2.0 * cell, 2.5 * cell);
    for &(x, y) in &path {
        for k in -6..=6 {
            let (y, x) = (y as i32 + k, x as i32);
            assert_eq!(
                lit.at(y, x),
                under.at(y, x),
                "the beam lights its leap at ({x}, {y}): {:06x} over its tube's {:06x}",
                lit.at(y, x),
                under.at(y, x)
            );
        }
    }
    let white = path
        .iter()
        .map(|&(x, y)| whiteness(lit.at(y as i32, x as i32)))
        .max()
        .unwrap_or(0);
    assert!(white < 160, "the beam's leap burns white ({white})");
    let thin = path
        .iter()
        .map(|&p| lift_across(&under, p, bg))
        .min()
        .unwrap_or(0);
    assert!(
        thin > 8,
        "the beam's tube breaks along its leap (lift {thin})"
    );
    assert_eq!(dust, 0, "the beam's leap sheds dust");

    let (lit, _, dust, a, b) = enter_leap(GlowStyle::Comet, false);
    let white = along(a, b, 0.0, 1.5 * cell)
        .iter()
        .map(|&(x, y)| whiteness(lit.at(y as i32, x as i32)))
        .max()
        .unwrap_or(0);
    assert!(
        white >= 200,
        "control: the comet burns no white head ({white})"
    );
    assert!(dust > 0, "control: the comet's leap sheds no dust");
}

#[test]
fn an_enter_leap_is_one_piece_from_its_tail_to_its_head() {
    const DIP: f32 = 0.6;
    let bg = Glass::new(tall_geom(), BG).level(0, 0);
    let cell = tall_geom().cw as f32;
    for style in [
        GlowStyle::Comet,
        GlowStyle::Beam,
        GlowStyle::Lumen,
        GlowStyle::Laser,
    ] {
        let worst = |split: bool| {
            let (lit, _, _, a, b) = enter_leap(style, split);
            let raw: Vec<i32> = along(a, b, 2.0 * cell, 0.5 * cell)
                .iter()
                .map(|&p| lift_across(&lit, p, bg))
                .collect();
            let lifts: Vec<i32> = (0..raw.len())
                .map(|i| {
                    let mut w = raw[i.saturating_sub(3)..(i + 4).min(raw.len())].to_vec();
                    w.sort_unstable();
                    w[w.len() / 2]
                })
                .collect();
            let behind = (1.5 * cell) as usize;
            (1..lifts.len())
                .map(|i| {
                    let most = lifts[i.saturating_sub(behind)..i]
                        .iter()
                        .copied()
                        .max()
                        .unwrap_or(0);
                    lifts[i] as f32 / most.max(1) as f32
                })
                .fold(f32::INFINITY, f32::min)
        };

        let (whole, split) = (worst(false), worst(true));
        assert!(
            whole >= DIP,
            "{style:?}: the leap dips to {whole:.2} of itself between its pieces"
        );
        assert!(
            split < DIP,
            "{style:?}: control: the split leap keeps {split:.2} of itself"
        );
    }
}

fn left_head(style: GlowStyle, dark: bool, square: bool, after_ms: u64) -> (Glass, u32) {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    let g = tall_geom();
    let (fg, bg) = if dark {
        (FG, BG)
    } else {
        (0x001F_2328, 0x00FF_FFFF)
    };
    let c = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: crate::cursor_fx::default_trail_intensity(Some(style), false),
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(style),
            beam: true,
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
        },
        CURSOR,
        dark,
        fg,
        bg,
        0.5,
    );
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let frame = |lines: &[(u16, &[char])]| {
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(fg), rgb(bg));
            }
        }
        for &(row, text) in lines {
            for (col, &ch) in text.iter().enumerate() {
                rows[usize::from(row)][col].ch = ch;
            }
        }
        rows
    };
    let mut glow = CursorGlow {
        heads_square: square,
        ..CursorGlow::default()
    };
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    text[0] = '$';
    let mut t = Instant::now();
    for (k, ch) in std::iter::once(' ').chain(LINE.chars()).enumerate() {
        if k > 0 {
            t += Duration::from_millis(60);
            text[START as usize + k - 1] = ch;
            glow.note_synthetic_typed(t, 1);
        }
        let caret = (2u16, START + k as u16);
        glow.observe_row(caret.0, caret.1, &text, t);
        glow.observe_frame_text(Some(caret), &frame(&[(2, &text)]), t);
        glow.tick(Some(caret), t, &c, g, &mut out);
    }
    t += Duration::from_millis(30);
    let mut prompt = vec![' '; g.cols];
    prompt[0] = '$';
    let output: Vec<char> = "the quick brown fox"
        .chars()
        .chain(std::iter::repeat(' '))
        .take(g.cols)
        .collect();
    let rows = frame(&[(2, &text), (3, &output), (4, &prompt)]);
    glow.note_synthetic_move(t);
    let landing = (4u16, START);
    let mut at = 0u64;
    loop {
        let now = t + Duration::from_millis(at);
        glow.observe_row(landing.0, landing.1, &prompt, now);
        glow.observe_frame_text(Some(landing), &rows, now);
        glow.tick(Some(landing), now, &c, g, &mut out);
        if at >= after_ms {
            break;
        }
        at = (at + 16).min(after_ms);
    }
    let mut glass = Glass::new(g, bg);
    glass.add_quads(glow.under_quads());
    (glass, bg)
}

#[test]
fn a_run_the_hand_has_left_melts_out_over_its_last_letter() {
    let g = tall_geom();
    let cw = g.cw as i32;
    let last = i32::from(START) + LINE.len() as i32 - 1;
    let (y0, y1) = (2 * g.ch as i32, 3 * g.ch as i32);
    for dark in [true, false] {
        let profile = |glass: &Glass, page: u32| -> Vec<i32> {
            (0..g.win_w as i32)
                .map(|x| {
                    (y0..y1)
                        .map(|y| {
                            let p = glass.at(y, x);
                            [16, 8, 0]
                                .iter()
                                .map(|&sh| {
                                    (((p >> sh) & 0xff) as i32 - ((page >> sh) & 0xff) as i32).abs()
                                })
                                .max()
                                .unwrap_or(0)
                        })
                        .max()
                        .unwrap_or(0)
                })
                .collect()
        };
        let worst = |square: bool| -> (f32, i32) {
            let (glass, page) = left_head(GlowStyle::Phaser, dark, square, 40);
            let col = profile(&glass, page);
            let level = col[((last - 1) * cw + cw / 2) as usize];
            assert!(
                level > 8,
                "fixture: the bar lies under the line ({dark}, {level})"
            );
            let step = ((last * cw - 2)..((last + 2) * cw))
                .map(|x| (col[x as usize] - col[x as usize + 1]).max(0))
                .max()
                .unwrap_or(0);
            let past = ((last + 1) * cw + 1..(last + 2) * cw)
                .map(|x| col[x as usize])
                .max()
                .unwrap_or(0);
            (step as f32 / level as f32, past)
        };
        let (eased, past) = worst(false);
        assert!(
            eased <= 0.3,
            "dark {dark}: the bar drops by {eased:.2} of itself in a column over its last letter"
        );
        assert!(
            past <= 2,
            "dark {dark}: the cell the caret left holds {past} of the bar"
        );
        let (square, _) = worst(true);
        assert!(
            square >= 0.6,
            "control, dark {dark}: cut square, the bar drops by only {square:.2}"
        );
    }
}

/// A stream's lights laid since a mark, clipped to a box
/// ([`clip_stream_to`]), are its legacy quads clipped ([`clip_quads_to`]):
/// every ramp row re-ramped at a horizontal cut as its quad is, rows and
/// quads outside dropped, each quad among the ramps clipped in its place,
/// and the lights before the mark untouched. A lit row the cut leaves no
/// light keeps its count as the legacy quad does, as a dark quad, and a
/// ramp's run is clipped as the one quad it stands for.
#[test]
fn a_clipped_stream_is_its_quads_clipped() {
    use aterm_render::{GlowRamps, RampSink, glow_end};
    let quad = |x: u16, y: u16, w: u16, (c, a): (u32, u8), (c2, a2): (u32, u8)| GlowQuad {
        row: 0,
        x,
        y,
        w,
        h: 1,
        color: c,
        alpha: a,
        color2: c2,
        alpha2: a2,
    };
    let rect = (22, 3, 30, 9);
    let mut quads = vec![quad(0, 0, 40, (0x0010_2030, 0x40), (0x0030_2010, 0x40))];
    let mut ramps = GlowRamps::default();
    let from = (quads.len(), ramps.mark());
    // An over ramp the box cuts on both sides and at its top, a quad laid
    // after it, then an additive ramp whose faint row the cut darkens.
    let over = |k: u32| {
        (
            glow_end(0x0040_6080 + k * 0x0001_0101, 0xA0),
            glow_end(0x0002_0408, 0x10 + k as u8),
        )
    };
    ramps.open(quads.len(), 0, (4, 1, 40), over(0));
    for k in 1..7 {
        ramps.extend_last(0, over(k));
    }
    quads.push(quad(2, 5, 32, (0x0080_4020, 0), (0x0000_0000, 0)));
    ramps.open(quads.len(), 0, (0, 4, 40), (glow_end(0x0001_0000, 0), 0));
    ramps.extend_last(1, (glow_end(0x0060_6060, 0), glow_end(0x0020_2020, 0)));
    quads.push(quad(31, 4, 5, (0x0050_5050, 0), (0x0050_5050, 0)));
    // A column's ramp: an edge row above the box, a run the box's top cuts
    // into and its left side re-ramps, an edge row.
    let edge = (glow_end(0x0010_1010, 0), glow_end(0x0010_1010, 0));
    ramps.open_run(quads.len(), 0, (20, 1, 8), 1, edge);
    assert!(ramps.extend_run(0, 4, (glow_end(0x0060_2000, 0), glow_end(0x0000_4060, 0))));
    assert!(ramps.extend_run(0, 1, edge));
    let mut want = Vec::new();
    ramps.expand_into(&quads, &mut want);
    clip_quads_to(&mut want, from.0, rect);
    assert!(
        want.iter().any(|q| (q.x, q.y, q.w, q.h) == (22, 3, 6, 3)),
        "premise: the box cuts the run's quad"
    );
    let mut sink = RampSink::new(&mut quads, &mut ramps);
    clip_stream_to(&mut sink, from, rect);
    drop(sink);
    let mut got = Vec::new();
    ramps.expand_into(&quads, &mut got);
    let dark = |q: &GlowQuad| q.color | q.color2 == 0 && q.alpha | q.alpha2 == 0;
    assert_eq!(got.len(), want.len(), "the clip keeps the legacy count");
    assert!(want.iter().any(dark), "premise: the cut darkens a lit row");
    let lit = |v: &[GlowQuad]| v.iter().filter(|q| !dark(q)).copied().collect::<Vec<_>>();
    assert_eq!(
        lit(&got),
        lit(&want),
        "each light is its quad clipped, in order"
    );
    assert_eq!(
        got[0],
        quad(0, 0, 40, (0x0010_2030, 0x40), (0x0030_2010, 0x40))
    );
    let runs: Vec<_> = ramps.items.iter().filter(|r| r.joined > 0).collect();
    assert_eq!(runs.len(), 1, "the run stays one light");
    for r in runs {
        let head = r.row_ends(&ramps.ends, r.join);
        for k in r.join + 1..=r.join + r.joined {
            assert_eq!(
                r.row_ends(&ramps.ends, k),
                head,
                "every run row reads its cut ends"
            );
        }
    }
}

/// The chroma (most channel less least) of `rgb`.
fn chroma(rgb: u32) -> i32 {
    let ch = [16, 8, 0].map(|sh| ((rgb >> sh) & 0xff) as i32);
    ch.iter().max().unwrap_or(&0) - ch.iter().min().unwrap_or(&0)
}

/// looks7 M4: a word gap's bed takes the straight line between its letters'
/// inks. The knots are the gallery's `context` `mid` (Phaser's dark bar
/// between `the` and `quick`): the gap's own `#032BE7`, the most vivid blue
/// of the bar, stood a stripe between `#034591` and `#2F1DD3`. Every letter,
/// a run of three blanks and a blank with a letter on one side only keep
/// their own ink.
#[test]
fn a_word_gap_s_bed_takes_the_line_between_its_letters() {
    let g = tall_geom();
    let text = "the quic   x ";
    let knots: [u32; 13] = [
        0x001A_152D,
        0x0013_1A4C,
        0x0003_4591,
        0x0003_2BE7,
        0x002F_1DD3,
        0x0047_10B5,
        0x0056_08A9,
        0x005E_0682,
        0x0064_0779,
        0x006B_1464,
        0x0070_0E6F,
        0x006A_1060,
        0x0003_2BE7,
    ];
    let row = i32::from(ROW);
    let lettered = |r: i32, c: i32| {
        r == row
            && usize::try_from(c)
                .ok()
                .and_then(|c| text.chars().nth(c))
                .is_some_and(|ch| ch != ' ')
    };
    let sample = |c: usize| CometSample {
        x: f32::from(g.origin_x) + (c as f32 + 0.5) * g.cw as f32,
        y: f32::from(g.origin_y) + (row as f32 + 0.5) * g.ch as f32,
        cov: 255,
        pos: c as f32 / knots.len() as f32,
    };
    let runs = vec![(0..knots.len()).map(sample).collect::<Vec<_>>()];
    let mut inks = knots.to_vec();
    bed::bridge_word_gaps(&runs, g, lettered, &mut inks);
    let gap = 3;
    let vivid = |inks: &[u32]| chroma(inks[gap]) - chroma(inks[gap - 1]).max(chroma(inks[gap + 1]));
    assert!(
        vivid(&inks) <= 0,
        "the word gap's bed is {} more vivid than its letters' (#{:06X})",
        vivid(&inks),
        inks[gap]
    );
    assert_eq!(
        inks[gap], 0x0019_31B2,
        "the gap's ink is the line between its letters'"
    );
    for (c, (&got, &knot)) in inks.iter().zip(&knots).enumerate() {
        if c != gap {
            assert_eq!(got, knot, "column {c} keeps its own ink");
        }
    }
    assert!(
        vivid(&knots) >= 40,
        "control: unbridged, the gap is only {} more vivid than its letters",
        vivid(&knots)
    );
}

/// looks7 M4 through the bar itself: dark Phaser typed over a line the
/// frame's own cells hold, every word gap's bed (its middle pixel, the bed
/// alone) is no more vivid than the more vivid of its letters'; the gap
/// keeping its own knot (the negative control) stands more vivid than both.
#[test]
fn phaser_s_dark_bed_lays_no_vivid_knot_in_a_word_gap() {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    let g = tall_geom();
    let c = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: crate::cursor_fx::default_trail_intensity(Some(GlowStyle::Phaser), false),
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(GlowStyle::Phaser),
            beam: true,
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
        },
        CURSOR,
        true,
        FG,
        BG,
        0.5,
    );
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let worst = |bridged: bool| -> (i32, usize) {
        let mut glow = CursorGlow::default();
        glow.set_bed_word_gaps(bridged);
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        text[0] = '$';
        let mut t = Instant::now();
        let (mut worst, mut gaps) = (i32::MIN, 0usize);
        let y = i32::from(g.origin_y) + i32::from(ROW) * g.ch as i32 + g.ch as i32 / 2;
        for (k, ch) in std::iter::once(' ').chain(LINE.chars()).enumerate() {
            if k > 0 {
                t += Duration::from_millis(60);
                text[START as usize + k - 1] = ch;
                glow.note_synthetic_typed(t, 1);
            }
            let caret = (ROW, START + k as u16);
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for (col, cell) in rows[usize::from(ROW)].iter_mut().enumerate() {
                (cell.fg, cell.bg, cell.ch) = (rgb(FG), rgb(BG), text[col]);
            }
            glow.observe_row(caret.0, caret.1, &text, t);
            glow.observe_frame_text(Some(caret), &rows, t);
            glow.tick(Some(caret), t, &c, g, &mut out);
            let mut glass = Glass::new(g, BG);
            glass.add_quads(glow.under_quads());
            let mid = |col: usize| {
                let x = i32::from(g.origin_x) + col as i32 * g.cw as i32 + g.cw as i32 / 2;
                glass.at(y, x)
            };
            for col in 1..usize::from(caret.1).saturating_sub(1) {
                let side = |c: usize| text[c] != ' ' && text[c] != '$';
                if text[col] != ' ' || !side(col - 1) || !side(col + 1) {
                    continue;
                }
                if [col - 1, col, col + 1].iter().any(|&c| chroma(mid(c)) < 24) {
                    continue;
                }
                gaps += 1;
                let over = chroma(mid(col)) - chroma(mid(col - 1)).max(chroma(mid(col + 1)));
                worst = worst.max(over);
            }
        }
        (worst, gaps)
    };
    let (held, gaps) = worst(true);
    let (kept, _) = worst(false);
    assert!(
        gaps >= 6,
        "fixture: the bar lay across only {gaps} word gaps"
    );
    assert!(
        held <= 2,
        "a word gap's bed is {held} more vivid than its letters'"
    );
    assert!(
        kept >= 12,
        "control: keeping its own knot, a word gap is only {kept} more vivid than its letters'"
    );
}

/// looks7 round 2, A LIT PAGE'S BED: on Solarized Dark (its text reads 4.75:1
/// on `#002b36`, and a dim letter keeps 97% of its own reading under the
/// lights) no style's bed lays a shade. Typing the line, every frame, every
/// pixel the beds lay under the line's cells (the rails below the row, their
/// soft top in its last two pixel rows, are light in the leading, and over a
/// row below that is no air a rail kept in its own foot band
/// ([`crate::cursor_glow::bed::BedShape::row_rail`], looks10 L2) is light in
/// the row's last three) keeps seven tenths of the page's luminance (a soft
/// edge's sRGB mix of the page and a hue at its luminance) and none rises
/// over it by more than a byte's
/// rounding, and the bed wears its hue: some pixel stands well off the page.
/// Negative controls: held to the letters' budget alone, the bed solves to
/// black and lays the gallery's black smoke (Beam's tube, Phaser's word gaps,
/// Lumen's and Laser's tracers) under half the page's luminance; a sample in
/// the air by its own cell alone ([`crate::cursor_glow::air_lit`]), the one
/// past the line's last letter wears the air's light and the bed's colour,
/// drawn toward it, lights the last letter's ground past three times the
/// page's luminance.
#[test]
fn a_lit_page_s_beds_lay_no_shade() {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    const PAGE: u32 = 0x0000_2B36;
    const TEXT: u32 = 0x0083_9496;
    let g = tall_geom();
    let lum = crate::color_math::relative_luminance;
    let lp = lum(PAGE);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    // (least and most luminance under the line, its farthest OKLab off the
    // page)
    let lay = |style: GlowStyle, (unlit, alone): (bool, bool)| -> (f32, f32, f32) {
        let c = resolve_glow_config(
            GlowKnobs {
                enabled: true,
                color: None,
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: crate::cursor_fx::default_trail_intensity(Some(style), false),
                radius: 0.6,
                ring: true,
            },
            GlowSpelling {
                style: Some(style),
                beam: true,
                ribbon_tall: true,
                ribbon_flat: false,
                classic_mono: false,
            },
            CURSOR,
            true,
            TEXT,
            PAGE,
            0.5,
        );
        let mut glow = CursorGlow::default();
        glow.set_bed_page_lit(!unlit);
        glow.set_bed_air_beside_text(!alone);
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        text[0] = '$';
        let mut t = Instant::now();
        let (mut low, mut high, mut far) = (f32::INFINITY, 0.0f32, 0.0f32);
        // The foot band a rail may lie in, or a rail's soft top below it.
        let foot = ((crate::rainbow_kitty::ribbon::ROW_RAIL_CH * g.ch as f32) as usize).max(2);
        let (y0, y1) = (
            g.origin_y as usize + usize::from(ROW) * g.ch,
            g.origin_y as usize + usize::from(ROW + 1) * g.ch - foot,
        );
        for (k, ch) in std::iter::once(' ').chain(LINE.chars()).enumerate() {
            if k > 0 {
                t += Duration::from_millis(60);
                text[START as usize + k - 1] = ch;
                glow.note_synthetic_typed(t, 1);
            }
            let caret = (ROW, START + k as u16);
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for (col, cell) in rows[usize::from(ROW)].iter_mut().enumerate() {
                (cell.fg, cell.bg, cell.ch) = (rgb(TEXT), rgb(PAGE), text[col]);
            }
            glow.observe_row(caret.0, caret.1, &text, t);
            glow.observe_frame_text(Some(caret), &rows, t);
            glow.tick(Some(caret), t, &c, g, &mut out);
            let mut glass = Glass::new(g, PAGE);
            glass.add_quads(glow.under_quads());
            let line_end = g.origin_x as usize + usize::from(caret.1) * g.cw;
            for y in y0..y1 {
                for x in 0..glass.w {
                    let px = glass.px[y * glass.w + x];
                    if px == PAGE {
                        continue;
                    }
                    if x >= line_end {
                        continue;
                    }
                    let l = lum(px);
                    low = low.min(l);
                    high = high.max(l);
                    far = far.max(crate::color_math::oklab_distance(px, PAGE));
                }
            }
        }
        (low, high, far)
    };
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Comet,
        GlowStyle::Laser,
        GlowStyle::Phaser,
        GlowStyle::Beam,
    ] {
        let (low, high, far) = lay(style, (false, false));
        assert!(
            low >= 0.7 * lp,
            "{style:?}: the bed lays a shade at {low:.4}, the page at {lp:.4}"
        );
        assert!(
            high <= 1.06 * lp,
            "{style:?}: the bed lays light at {high:.4} over the page's {lp:.4}"
        );
        if style == GlowStyle::Phaser {
            assert!(
                far >= 0.06,
                "Phaser's bed lies on the page ({far:.3} OKLab at most)"
            );
        }
        let (unlit, _, _) = lay(style, (true, false));
        assert!(
            unlit < 0.5 * lp,
            "control: {style:?}'s bed held to its budget lays no shade ({unlit:.4} on the page's {lp:.4})"
        );
        let (_, alone, _) = lay(style, (false, true));
        assert!(
            alone > 3.0 * lp,
            "control: {style:?}'s air read by each sample's own cell lays no light under the line ({alone:.4} on the page's {lp:.4})"
        );
    }
}

/// looks10 L2, COMET'S TAIL IN ITS OWN ROW READS AS ITS TAIL: laid in the
/// typed row's foot band over a selection's fill ([`a_tail_with_no_air_below_lies_in_its_own_row`]),
/// Comet's icy tail was a soft line two pixels deep that fell off with its
/// taper, a hairline under the last word that did not read as a comet.
/// Laid whole down to the band's last pixel row and falling off with its
/// taper's square root ([`crate::cursor_glow::bed::BedScratch::row_tail_faint`]),
/// on the reference dark and on Solarized Dark its last frame lights the band
/// at least three cells back from the caret, its last pixel row too.
/// Negative control: laid as round 1 laid it (`comet_row_tail_faint`), it
/// lays under five sevenths of that light and leaves the band's last pixel
/// row dark.
#[test]
fn comet_s_tail_in_its_own_row_reads_as_its_tail() {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    const FILL: u32 = 0x0026_4F78;
    let g = tall_geom();
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let (cw, ch) = (g.cw, g.ch);
    let foot = (crate::rainbow_kitty::ribbon::ROW_RAIL_CH * ch as f32) as usize;
    // (cells behind the caret whose foot band reads lit, the band's light
    // over the line behind the caret)
    let lay = |(page, ink): (u32, u32), faint: bool| {
        let c = resolve_glow_config(
            GlowKnobs {
                enabled: true,
                color: None,
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: crate::cursor_fx::default_trail_intensity(Some(GlowStyle::Comet), false),
                radius: 0.6,
                ring: true,
            },
            GlowSpelling {
                style: Some(GlowStyle::Comet),
                beam: true,
                ribbon_tall: true,
                ribbon_flat: false,
                classic_mono: false,
            },
            CURSOR,
            true,
            ink,
            page,
            0.5,
        );
        let mut glow = CursorGlow {
            comet_row_tail_faint: faint,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        let mut caret = (ROW, START);
        for (k, letter) in std::iter::once(' ').chain(LINE.chars()).enumerate() {
            if k > 0 {
                t += Duration::from_millis(60);
                text[START as usize + k - 1] = letter;
                glow.note_synthetic_typed(t, 1);
            }
            caret = (ROW, START + k as u16);
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for (r, line) in rows.iter_mut().enumerate() {
                for (col, cell) in line.iter_mut().enumerate() {
                    let ground = if r == usize::from(ROW + 1) {
                        FILL
                    } else {
                        page
                    };
                    (cell.fg, cell.bg) = (rgb(ink), rgb(ground));
                    if r == usize::from(ROW) {
                        cell.ch = text[col];
                    }
                }
            }
            glow.observe_row(caret.0, caret.1, &text, t);
            glow.observe_frame_text(Some(caret), &rows, t);
            glow.tick(Some(caret), t, &c, g, &mut out);
        }
        let mut glass = Glass::new(g, page);
        glass.add_quads(glow.under_quads());
        let bottom = usize::from(ROW + 1) * ch;
        let head = usize::from(caret.1) * cw;
        let (mut cells, mut light) = (0usize, 0.0f32);
        for cell in 1..=8usize {
            let x0 = head - cell * cw;
            let mut deepest = 0.0f32;
            for y in bottom - foot..bottom {
                for x in x0..x0 + cw {
                    let d = crate::color_math::oklab_distance(glass.px[y * glass.w + x], page);
                    deepest = deepest.max(d);
                    light += d;
                }
            }
            if deepest > 0.12 {
                cells += 1;
            }
        }
        // The band's last pixel row at its deepest over the two cells
        // behind the one before the caret.
        let last_row = (head - 3 * cw..head - cw)
            .map(|x| crate::color_math::oklab_distance(glass.px[(bottom - 1) * glass.w + x], page))
            .fold(0.0f32, f32::max);
        (cells, light, last_row)
    };
    for theme in [(BG, FG), (0x0000_2B36, 0x0083_9496)] {
        let page = theme.0;
        let (cells, light, last_row) = lay(theme, false);
        assert!(
            cells >= 3 && last_row >= 0.3,
            "on {page:06x} the tail lights {cells} cells of its band, its last pixel row {last_row:.2}"
        );
        let (_, faint_light, faint_last_row) = lay(theme, true);
        assert!(
            light >= 1.4 * faint_light && faint_last_row < 0.1,
            "control: on {page:06x} round 1's tail laid {faint_light:.1} of the whole's {light:.1}, \
             its last pixel row {faint_last_row:.2}"
        );
    }
}

/// What [`lay_row_rail`] lights: (lit pixels, summed OKLab distance off the
/// page) in the foot band behind the caret, in the cell before it, under the
/// `q`, in the fill below, and in the three pixel rows over the band behind
/// the caret, where the letters' feet stand.
struct RowRailLit {
    behind: (usize, f32),
    before: (usize, f32),
    under_q: (usize, f32),
    below: (usize, f32),
    over_band: (usize, f32),
}

/// [`LINE`] typed in `style` on `(page, ink)` over a selection's fill
/// (`context`), its rail in its own row's foot band when `rails`
/// ([`CursorGlow::set_bed_row_rails`]), a swollen one lifted over the band
/// when `lifts` ([`CursorGlow::set_bed_row_rail_swell_lifts`]): what its last
/// frame's under stream lights.
fn lay_row_rail(style: GlowStyle, (page, ink): (u32, u32), rails: bool, lifts: bool) -> RowRailLit {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    const FILL: u32 = 0x0026_4F78;
    let g = tall_geom();
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let (cw, ch) = (g.cw, g.ch);
    let foot = (crate::rainbow_kitty::ribbon::ROW_RAIL_CH * ch as f32) as usize;
    let q = usize::from(START) + LINE.find('q').expect("the line has a q");
    {
        let c = resolve_glow_config(
            GlowKnobs {
                enabled: true,
                color: None,
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: crate::cursor_fx::default_trail_intensity(Some(style), false),
                radius: 0.6,
                ring: true,
            },
            GlowSpelling {
                style: Some(style),
                beam: true,
                ribbon_tall: true,
                ribbon_flat: false,
                classic_mono: style == GlowStyle::Classic,
            },
            CURSOR,
            true,
            ink,
            page,
            0.5,
        );
        let mut glow = CursorGlow::default();
        glow.set_bed_row_rails(rails);
        glow.set_bed_row_rail_swell_lifts(lifts);
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        let mut glass = Glass::new(g, page);
        let mut caret = (ROW, START);
        for (k, letter) in std::iter::once(' ').chain(LINE.chars()).enumerate() {
            if k > 0 {
                t += Duration::from_millis(60);
                text[START as usize + k - 1] = letter;
                glow.note_synthetic_typed(t, 1);
            }
            caret = (ROW, START + k as u16);
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for (r, line) in rows.iter_mut().enumerate() {
                for (col, cell) in line.iter_mut().enumerate() {
                    let ground = if r == usize::from(ROW + 1) {
                        FILL
                    } else {
                        page
                    };
                    (cell.fg, cell.bg) = (rgb(ink), rgb(ground));
                    if r == usize::from(ROW) {
                        cell.ch = text[col];
                    }
                }
            }
            glow.observe_row(caret.0, caret.1, &text, t);
            glow.observe_frame_text(Some(caret), &rows, t);
            glow.tick(Some(caret), t, &c, g, &mut out);
            glass = Glass::new(g, page);
            glass.add_quads(glow.under_quads());
        }
        let bottom = usize::from(ROW + 1) * ch;
        let lit = |ys: std::ops::Range<usize>, xs: std::ops::Range<usize>, ground: u32| {
            let mut n = 0usize;
            let mut sum = 0.0f32;
            for y in ys {
                for x in xs.clone() {
                    let px = glass.px[y * glass.w + x];
                    let d = crate::color_math::oklab_distance(px, ground);
                    if d > 0.03 {
                        n += 1;
                    }
                    sum += d;
                }
            }
            (n, sum)
        };
        let band = bottom - foot..bottom;
        let head = usize::from(caret.1) * cw;
        RowRailLit {
            behind: lit(band.clone(), usize::from(START) * cw..head - cw, page),
            before: lit(band.clone(), head - cw..head, page),
            under_q: lit(band.clone(), q * cw..(q + 1) * cw, page),
            below: lit(bottom..bottom + ch, usize::from(START) * cw..head, page),
            over_band: lit(
                band.start - 3..band.start,
                usize::from(START) * cw..head,
                page,
            ),
        }
    }
}

/// looks10 L2, A TAIL WITH NO AIR BELOW LIES IN ITS OWN ROW: in the trail
/// gallery's `context` (a selection's fill under the typed line, a powerline
/// bar over it) Comet's icy tail and Sparkle's gold rail found no air to lie
/// in and drew nothing behind the caret but its glow. Over a row below that
/// is no air, the rail lies in the typed row's own foot band
/// ([`crate::cursor_glow::bed::BedShape::row_rail`]); on Solarized Dark, whose
/// letters take no light, so does the rail of Beam, Lumen and Water (drawing
/// only their caret there), of Phaser (a faint wash) and of Classic's one
/// colour face (classic pink, its caret alone; looks10 round 2):
/// typing the line on the reference dark and on Solarized Dark over a selection's fill, its last
/// frame lights that band under the letters behind the caret and into the
/// cell before it, keeps it dark under the `q`'s tail, and lays nothing in
/// the fill below. Negative control: kept out of its row
/// ([`CursorGlow::set_bed_row_rails`]), the rail lies at the leading's depth in
/// the fill, as looks9 laid it, and Comet's foot band stays dark.
#[test]
fn a_tail_with_no_air_below_lies_in_its_own_row() {
    let cw = tall_geom().cw;
    const SOLARIZED: (u32, u32) = (0x0000_2B36, 0x0083_9496);
    let cases = [
        GlowStyle::Comet,
        GlowStyle::Sparkle,
        GlowStyle::Phaser,
        GlowStyle::Classic,
    ]
    .into_iter()
    .flat_map(|style| [(style, (BG, FG)), (style, SOLARIZED)])
    .chain(
        [GlowStyle::Beam, GlowStyle::Lumen, GlowStyle::Water]
            .into_iter()
            .map(|style| (style, SOLARIZED)),
    );
    for (style, theme) in cases {
        {
            let RowRailLit {
                behind,
                before,
                under_q,
                below,
                ..
            } = lay_row_rail(style, theme, true, false);
            let page = theme.0;
            // Water's rail is foam, broken over its swell's troughs: the cell
            // before the caret holds a piece of it, not a whole cell's run.
            let reaches = if style == GlowStyle::Water {
                before.0 > 0
            } else {
                before.0 >= cw
            };
            assert!(
                reaches && behind.0 + before.0 >= 3 * cw,
                "{style:?} on {page:06x}: the foot band behind the caret lights {behind:?} px, \
                 {before:?} in the cell before it"
            );
            assert_eq!(
                under_q.0, 0,
                "{style:?} on {page:06x}: the rail runs under the q's tail"
            );
            assert!(
                below.1 < 0.5,
                "{style:?} on {page:06x}: the rail spills into the fill below ({below:?})"
            );
            let RowRailLit {
                behind: off_behind,
                before: off_before,
                below: off_below,
                ..
            } = lay_row_rail(style, theme, false, false);
            assert!(
                off_below.0 >= 3 * cw,
                "control: {style:?} on {page:06x} kept out of its row lays its rail in the fill \
                 ({off_below:?})"
            );
            if style == GlowStyle::Comet {
                assert!(
                    off_behind.0 + off_before.0 < cw,
                    "control: Comet on {page:06x} kept out of its row lights its foot band \
                     ({off_behind:?}, {off_before:?})"
                );
            }
        }
    }
}

/// looks10 round 2, A RAIL IN ITS ROW KEEPS UNDER THE LETTERS: a rail laid in
/// its own row's foot band ([`crate::cursor_glow::bed::BedShape::row_rail`])
/// keeps to the band, Water's swollen foam included (its crests at the band's
/// top, the rail thinned by its swell's fall): over the three pixel rows over
/// the band, where the letters' feet stand, it lights no more than the same
/// line typed with its rail kept out of its row
/// ([`CursorGlow::set_bed_row_rails`]), on Solarized Dark or the reference
/// dark. Negative control: lifted over the band by its swell's fall
/// ([`CursorGlow::set_bed_row_rail_swell_lifts`]), Water's crests lay on the
/// letters' feet (in the round's gallery the `o` of `fox` read 1.39:1
/// against its own 4.28 in `context`).
#[test]
fn a_rail_in_its_row_keeps_under_the_letters() {
    const SOLARIZED: (u32, u32) = (0x0000_2B36, 0x0083_9496);
    let over_its_band = |style: GlowStyle, theme: (u32, u32), lifts: bool| {
        let laid = lay_row_rail(style, theme, true, lifts);
        let kept_out = lay_row_rail(style, theme, false, false);
        (
            laid.over_band.0.saturating_sub(kept_out.over_band.0),
            laid.over_band.1 - kept_out.over_band.1,
        )
    };
    for style in [
        GlowStyle::Comet,
        GlowStyle::Sparkle,
        GlowStyle::Phaser,
        GlowStyle::Classic,
        GlowStyle::Beam,
        GlowStyle::Lumen,
        GlowStyle::Water,
    ] {
        for theme in [(BG, FG), SOLARIZED] {
            let (px, light) = over_its_band(style, theme, false);
            assert!(
                px == 0 && light < 0.05,
                "{style:?} on {:06x}: its rail lights {px} px ({light:.2}) over its foot band, \
                 at the letters' feet",
                theme.0
            );
        }
    }
    let (px, _) = over_its_band(GlowStyle::Water, SOLARIZED, true);
    assert!(
        px >= 20,
        "control: Water's foam lifted over its band lights only {px} px more at the letters' feet"
    );
}

/// looks7 round 3, LUMEN KEEPS A GREY CURSOR GREY: Lumen's light is the cursor's, and its bed
/// floors the colour's saturation ([`BedShape::sat`]) to stay vivid; on Solarized Dark the
/// cursor is a grey `#93a1a1` (0.09), whose hue is noise, and floored its Return tracer read a
/// saturated cyan `#94edfb` the cursor does not have. A colour under
/// [`BedShape::own_chroma`] keeps its own chroma in the air's light and on its rail (on a lit
/// page, [`BedShape::on_lit_page`]); the dark gallery's green cursor (0.68) is floored as
/// before, and so is any style but Lumen's. Negative control: floored, the grey's rail and the
/// air's ink read past half saturation.
#[test]
fn lumen_keeps_a_grey_cursor_grey() {
    use crate::color_math::rgb2hsv;
    use crate::cursor_glow::bed::bed_rail_ink;
    const SOL_PAGE: u32 = 0x0000_2B36;
    const SOL_TEXT: u32 = 0x0083_9496;
    const GREY: u32 = 0x0093_A1A1;
    const GREEN: u32 = 0x0050_FA7B;
    let lumen = BedShape::of(GlowStyle::Lumen, 1.0).expect("Lumen lays a bed");
    let comet = BedShape::of(GlowStyle::Comet, 1.0).expect("Comet lays a bed");
    let sat = |c: u32| rgb2hsv(c).1;
    assert!(lumen.sat_for(GREY) < 0.1, "the grey keeps its own chroma");
    assert!(
        (lumen.sat_for(GREEN) - lumen.sat).abs() < 1e-6,
        "the green is floored"
    );
    assert!(
        (comet.sat_for(GREY) - comet.sat).abs() < 1e-6,
        "Comet floors the grey"
    );
    let rail = bed_rail_ink(GREY, lumen.on_lit_page());
    assert!(sat(rail) < 0.15, "Lumen's rail is {rail:06x}");
    let budget = crate::cursor_glow::dark_bed_budget(SOL_TEXT);
    let air = AirLight::of(SOL_PAGE, budget, lumen).expect("Solarized Dark's air is lit");
    let in_air = air.ink(GREY, SOL_PAGE, lumen.sat_for(GREY));
    assert!(
        sat(in_air) < 0.2,
        "Lumen's light in the air is {in_air:06x}"
    );
    let floored = crate::rainbow_kitty::ribbon::rail_ink(GREY);
    assert!(
        sat(floored) > 0.5,
        "control: the floored rail reads {floored:06x}"
    );
    let floored_air = air.ink(GREY, SOL_PAGE, lumen.sat);
    assert!(
        sat(floored_air) > 0.4,
        "control: the floored air's ink reads {floored_air:06x}"
    );
}

/// looks7 round 3 review, A CHOSEN COLOUR KEEPS ITS CHROMA: a trail colour the user chose
/// ([`ColourPins`]) is drawn as itself, never re-hued or given more chroma; the bed may spend
/// less light on it, never another chroma. On Solarized Dark the lit page's rail and the air's
/// light floored a chosen colour to the shape's saturation ([`BedShape::sat`]) and the rail's
/// ([`crate::rainbow_kitty::ribbon::rail_ink`]): Beam pinned to `#b0b0c8` (0.12) laid its
/// rail below the row in `#6464ff` (0.61), Lumen pinned to `#d09090` (0.31) in `#ff0000`.
/// Typing the line with each pinned, no lit quad below the caret's row stands more chroma than
/// the pin's, and some lie there; the shape's inks for the pin (its rail, the air's light and
/// the bed under the letters, on Solarized Dark and on the reference dark) keep it too.
/// Negative control: the same colour unpinned is floored, its rail past half saturation.
#[test]
fn a_chosen_colour_keeps_its_chroma_in_the_bed() {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    use crate::cursor_glow::bed::bed_rail_ink;
    const PAGE: u32 = 0x0000_2B36;
    const TEXT: u32 = 0x0083_9496;
    const EPS: f32 = 0.06;
    let g = tall_geom();
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let sat = |c: u32| hsv(c).1;
    let row_bot = (usize::from(g.origin_y) + usize::from(ROW + 1) * g.ch) as u16;
    // (the most chroma a lit quad below the caret's row stands, how many lie there)
    let lay = |style: GlowStyle, pin: u32, pinned: bool| -> (f32, usize) {
        let c = resolve_glow_config(
            GlowKnobs {
                enabled: true,
                color: Some(pin),
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: crate::cursor_fx::default_trail_intensity(Some(style), false),
                radius: 0.6,
                ring: true,
            },
            GlowSpelling {
                style: Some(style),
                beam: true,
                ribbon_tall: true,
                ribbon_flat: false,
                classic_mono: false,
            },
            CURSOR,
            true,
            TEXT,
            PAGE,
            0.5,
        );
        let mut glow = CursorGlow::default();
        glow.set_colour_pins(ColourPins {
            trail: pinned,
            accent: false,
        });
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        text[0] = '$';
        let mut t = Instant::now();
        let (mut most, mut lit) = (0.0f32, 0usize);
        for (k, ch) in std::iter::once(' ').chain(LINE.chars()).enumerate() {
            if k > 0 {
                t += Duration::from_millis(60);
                text[START as usize + k - 1] = ch;
                glow.note_synthetic_typed(t, 1);
            }
            let caret = (ROW, START + k as u16);
            // Every row on the page's ground: the row below is air, where the
            // rail lies ([`BedShape::row_rail`]).
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for line in &mut rows {
                for cell in line.iter_mut() {
                    (cell.fg, cell.bg) = (rgb(TEXT), rgb(PAGE));
                }
            }
            for (col, cell) in rows[usize::from(ROW)].iter_mut().enumerate() {
                cell.ch = text[col];
            }
            glow.observe_row(caret.0, caret.1, &text, t);
            glow.observe_frame_text(Some(caret), &rows, t);
            glow.tick(Some(caret), t, &c, g, &mut out);
            for q in glow.under_quads() {
                if q.y < row_bot {
                    continue;
                }
                for (color, alpha) in [(q.color, q.alpha), (q.color2, q.alpha2)] {
                    if alpha < 64 {
                        continue;
                    }
                    lit += 1;
                    most = most.max(sat(straight(color, alpha)));
                }
            }
        }
        (most, lit)
    };
    for (style, pin) in [
        (GlowStyle::Beam, 0x00B0_B0C8),
        (GlowStyle::Beam, 0x00C8_A0A0),
        (GlowStyle::Lumen, 0x00D0_9090),
        (GlowStyle::Lumen, 0x00A0_C0E0),
    ] {
        let own = sat(pin);
        let (most, lit) = lay(style, pin, true);
        let (most_u, _) = lay(style, pin, false);
        assert!(
            lit > 0,
            "{style:?} {pin:06x}: fixture: the bed lays nothing below the row"
        );
        assert!(
            most <= own + EPS,
            "{style:?} pinned to {pin:06x} ({own:.2}) lays {most:.2} of chroma below the row"
        );
        assert!(
            most_u > 0.5,
            "control: {style:?} unpinned in {pin:06x} keeps its chroma below the row ({most_u:.2})"
        );
        let shape = BedShape::of(style, 1.0)
            .expect("the style lays a bed")
            .pinned();
        let rail = bed_rail_ink(pin, shape.on_lit_page());
        assert!(
            (sat(rail) - own).abs() <= EPS,
            "{style:?}'s rail in {pin:06x} is {rail:06x}"
        );
        let budget = crate::cursor_glow::dark_bed_budget(TEXT);
        let air = AirLight::of(PAGE, budget, shape).expect("Solarized Dark's air is lit");
        let in_air = air.ink(pin, PAGE, shape.sat_for(pin));
        assert!(
            (sat(in_air) - own).abs() <= EPS,
            "{style:?}'s light in the air in {pin:06x} is {in_air:06x}"
        );
        let dark_budget = crate::cursor_glow::dark_bed_budget(FG);
        let under =
            crate::cursor_glow::dark_bed_ink(pin, BG, dark_budget, shape.under_sat_for(pin));
        assert!(
            sat(under) <= own + EPS,
            "{style:?}'s bed under the letters on the reference dark in {pin:06x} is {under:06x}"
        );
    }
}

/// The `$` the trail gallery's font draws in a 14×26 cell, as each pixel's
/// coverage (0–255) read off the gallery's clean classic-pink `enter-bottom`
/// frame, rows [`DOLLAR_TOP`].. of the cell: a stem of 60–67% coverage through
/// the S's solid strokes, whose rims are a share of the ink the lab still
/// judges as solid (a ring's bar beside a stroke lies by most of a thin row's
/// ink).
const DOLLAR: [[u8; 14]; 20] = [
    [0, 0, 0, 0, 0, 0, 3, 3, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 171, 154, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 1, 84, 140, 229, 223, 134, 65, 0, 0, 0, 0],
    [0, 0, 8, 167, 255, 255, 254, 254, 255, 255, 130, 1, 0, 0],
    [0, 0, 148, 255, 152, 45, 173, 157, 55, 194, 255, 103, 0, 0],
    [0, 0, 223, 212, 1, 0, 171, 154, 0, 43, 252, 189, 0, 0],
    [0, 23, 255, 179, 0, 0, 171, 154, 0, 0, 80, 98, 0, 0],
    [0, 3, 246, 250, 22, 0, 171, 154, 0, 0, 0, 0, 0, 0],
    [0, 0, 144, 255, 202, 67, 174, 154, 0, 0, 0, 0, 0, 0],
    [0, 0, 11, 158, 255, 255, 255, 213, 90, 12, 0, 0, 0, 0],
    [0, 0, 0, 0, 53, 143, 237, 255, 255, 246, 121, 4, 0, 0],
    [0, 0, 0, 0, 0, 0, 171, 177, 116, 231, 255, 144, 0, 0],
    [0, 0, 0, 0, 0, 0, 171, 154, 0, 23, 247, 254, 22, 0],
    [0, 11, 23, 4, 0, 0, 171, 154, 0, 0, 161, 255, 62, 0],
    [0, 84, 255, 96, 0, 0, 171, 154, 0, 0, 148, 255, 30, 0],
    [0, 11, 244, 219, 22, 0, 171, 154, 0, 36, 237, 213, 0, 0],
    [0, 0, 109, 254, 244, 175, 213, 205, 183, 250, 246, 78, 0, 0],
    [0, 0, 0, 72, 219, 255, 255, 255, 254, 208, 53, 0, 0, 0],
    [0, 0, 0, 0, 0, 16, 188, 171, 11, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 88, 80, 0, 0, 0, 0, 0, 0],
];

/// The cell row of [`DOLLAR`]'s first row.
const DOLLAR_TOP: i32 = 3;

/// Each pixel of the `$` in cell `(row, col)` of `g` (a 14×26 grid), with its
/// coverage; every other pixel of the cell is bare page.
fn dollar(g: Geom, row: u16, col: u16) -> Vec<((i32, i32), u8)> {
    let x0 = i32::from(g.origin_x) + i32::from(col) * g.cw as i32;
    let y0 = i32::from(g.origin_y) + i32::from(row) * g.ch as i32 + DOLLAR_TOP;
    (0i32..)
        .zip(DOLLAR)
        .flat_map(|(dy, line)| {
            (0i32..)
                .zip(line)
                .filter(|&(_, cov)| cov > 0)
                .map(move |(dx, cov)| ((y0 + dy, x0 + dx), cov))
        })
        .collect()
}

/// The worst reading of the new prompt's `$` through the one-colour classic's
/// landing on it, as the trail gallery's classic-pink `enter-bottom` lays it:
/// on the gallery's grid (14×26 cells under a 12 px pad, 14 rows) and page,
/// `$ echo the quick brown fox` typed on the last row at 60 ms a key, Enter
/// 32 ms after the last, the screen scrolled two rows (the command, its output,
/// the new prompt's `$` on the last row, the caret after it); the frame text
/// handed over as the host hands it, and judged every 2 ms for 300 ms (the
/// ring's bar sweeps a stroke's width in a few) as `aterm-lab trail-contrast`
/// judges it ([`lab_reading`]). `flying` admits the ring as it was.
fn pink_landing_dollar(flying: bool) -> f32 {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    const PAGE: (u32, u32) = (0x00D0_D0D0, 0x0011_1318);
    let g = Geom {
        cw: 14,
        ch: 26,
        rows: 14,
        cols: 72,
        origin_x: 12,
        origin_y: 2,
        win_w: 1032,
        win_h: 378,
        head: 0,
    };
    let last = g.rows as u16 - 1;
    let glow_cfg = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: Some(0x00D0_3A8C),
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: crate::cursor_fx::default_trail_intensity(Some(GlowStyle::Classic), true),
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(GlowStyle::Classic),
            beam: true,
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: true,
        },
        CURSOR,
        true,
        PAGE.0,
        PAGE.1,
        0.5,
    );
    let trail_cfg = TrailConfig {
        enabled: false,
        duration: Duration::from_millis(260),
        max_len: 24,
        color: glow_cfg.color,
        intensity: 0.0,
        warmth: 0.0,
    };
    let mut fx = CursorFx::default();
    fx.glow.classic.ring_flying = flying;
    let input = |now: Instant, cur: (u16, u16)| CursorFxInput {
        now,
        cur: Some(cur),
        live_viewport: true,
        cursor_style: CursorStyle::SteadyBlock,
        blink_phase: true,
        live_cursor: CURSOR,
        cursor_color_pinned: false,
        default_bg: PAGE.1,
        cursor_cell_bg: None,
        geom: g,
        focused: true,
        shed_envelope: 1.0,
        trail_brightness: 1.0,
        body_allowed: true,
        momentum_glow: true,
        user_tinted: true,
        accent_pinned: false,
    };
    let n = LINE.chars().count();
    let line = |text: &str| {
        let mut cols: Vec<char> = text.chars().collect();
        cols.resize(g.cols, ' ');
        cols
    };
    let typed_row = |typed: usize| {
        line(&format!(
            "$ {}",
            LINE.chars().take(typed).collect::<String>()
        ))
    };
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let frame = |lines: &[(u16, &[char])]| {
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(PAGE.0), rgb(PAGE.1));
            }
        }
        for &(row, text) in lines {
            for (col, &ch) in text.iter().enumerate() {
                rows[usize::from(row)][col].ch = ch;
            }
        }
        rows
    };
    let blank = vec![' '; g.cols];
    let mut t = Instant::now();
    for typed in 0..=n {
        t += Duration::from_millis(60);
        if typed > 0 {
            fx.glow.note_synthetic_typed(t, 1);
            fx.cadence.on_keystroke(t);
            fx.momentum.on_key(t, MOMENTUM_GLOW_TAU_S);
        }
        let caret = (last, START + typed as u16);
        let row = typed_row(typed);
        fx.begin(true, true);
        fx.glow.observe_row(caret.0, caret.1, &row, t);
        fx.glow.observe_neighbor_rows(Some(&blank), None);
        fx.glow
            .observe_frame_text(Some(caret), &frame(&[(last, &row)]), t);
        let _ = fx.tick(&input(t, caret), &glow_cfg, trail_cfg);
    }
    let (command, output, prompt) = (typed_row(n), line("the quick brown fox"), line("$"));
    let landed = frame(&[(last - 2, &command), (last - 1, &output), (last, &prompt)]);
    let enter = t + Duration::from_millis(32);
    fx.scroll_translate(2);
    let caret = (last, START);
    let ink = dollar(g, last, 0);
    let mut worst = f32::INFINITY;
    let mut lit = false;
    for k in 0..=150u64 {
        let now = enter + Duration::from_millis(2 * k);
        fx.begin(true, true);
        fx.glow.observe_row(caret.0, caret.1, &prompt, now);
        fx.glow.observe_neighbor_rows(Some(&output), None);
        fx.glow.observe_frame_text(Some(caret), &landed, now);
        let tint = fx
            .tick(&input(now, caret), &glow_cfg, trail_cfg)
            .trail_color;
        let mut glass = Glass::new(g, PAGE.1);
        for cell in &fx.trail_scratch {
            let (x0, y0, x1, y1) = glass.cell_rect(cell.row as u16, cell.col as u16);
            for y in y0.max(0)..y1.min(glass.h as i32) {
                for x in x0.max(0)..x1.min(glass.w as i32) {
                    let slot = &mut glass.px[y as usize * glass.w + x as usize];
                    *slot = aterm_render::blend_rgb(*slot, tint, cell.alpha);
                }
            }
        }
        glass.add_quads(fx.glow.under_quads());
        for &((y, x), cov) in &ink {
            let slot = &mut glass.px[y as usize * glass.w + x as usize];
            *slot = aterm_render::blend_rgb(*slot, PAGE.0, cov);
        }
        glass.add_quads(&fx.glow_scratch);
        glass.add_halos(fx.glow.halos());
        post_passes(&mut glass, &fx.glow_scratch, fx.bloom_held, PAGE.1);
        let (x0, y0, x1, y1) = glass.cell_rect(last, 0);
        lit |= (y0..y1).any(|y| (x0..x1).any(|x| glass.at(y, x) & 0x00FF_FFFF != PAGE.1));
        worst = worst.min(lab_reading(&glass, (x0, y0, x1, y1), &ink, PAGE));
    }
    assert!(lit, "fixture: the landing lays light on the prompt's `$`");
    worst
}

/// The live terminal's post passes over `glass` on page `bg`, as the hosts
/// run them over the glow stream `glow` (the GUI's `post_process`): the bloom
/// unless the frame holds it, at the page's share of its strength, then the
/// SDR crown at the configuration's default boost.
fn post_passes(glass: &mut Glass, glow: &[aterm_render::GlowQuad], held: bool, bg: u32) {
    use aterm_render::{bloom, hdr};
    /// `cursor_glow_sdr_boost`'s default.
    const SDR_BOOST: f32 = 0.25;
    let mut frame = aterm_render::Frame {
        width: glass.w,
        height: glass.h,
        pixels: std::mem::take(&mut glass.px),
    };
    let rows = glass.g.rows;
    if !held {
        let params = bloom::BloomParams {
            strength: bloom::BLOOM_STRENGTH * hdr::light_page_bloom_share(bg),
            radius: bloom::BLOOM_RADIUS,
        };
        bloom::apply(&mut frame, glow, rows, params).expect("bloom");
    }
    let budget = hdr::sdr_glow_budget(hdr::packed_luma(bg), SDR_BOOST);
    bloom::sdr_crown(&mut frame, glow, rows, budget).expect("crown");
    glass.px = frame.pixels;
}

/// A glyph's reading as `aterm-lab trail-contrast` judges it: the lower of
/// its whole-cell contrast (the median of its core ink, within 70% of its
/// extreme's lift, against the median of its bare page) and its local one
/// (each solid ink pixel, [`SOLID_SHARE`] of the extreme's clean ratio,
/// against the per-channel median of the bare page on its nearest Chebyshev
/// ring, 1 then 2; each pixel row's median of those, the worst row). `ink`
/// is the glyph's coverage; the clean frame is the page under it.
fn lab_reading(
    glass: &Glass,
    (x0, y0, x1, y1): (i32, i32, i32, i32),
    ink: &[((i32, i32), u8)],
    (fg, bg): (u32, u32),
) -> f32 {
    /// `aterm-lab trail-contrast`'s share of the clean ratio that reads as
    /// solid ink.
    const SOLID_SHARE: f32 = 0.85;
    let cover: std::collections::HashMap<(i32, i32), u8> = ink.iter().copied().collect();
    let clean = |y: i32, x: i32| {
        cover
            .get(&(y, x))
            .map_or(bg, |&c| aterm_render::blend_rgb(bg, fg, c))
    };
    let at = |y: i32, x: i32| glass.at(y, x) & 0x00FF_FFFF;
    let ratio = |a: u32, b: u32| {
        let (la, lb) = (rel_luminance(a), rel_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    };
    let median = |px: &[u32], mid: usize| {
        let mut out = 0u32;
        for shift in [16u32, 8, 0] {
            let mut ch: Vec<u32> = px.iter().map(|p| (p >> shift) & 0xFF).collect();
            ch.sort_unstable();
            out |= ch[mid] << shift;
        }
        out
    };
    let solid = SOLID_SHARE * ratio(fg, bg);
    let reach = 0.70 * (rel_luminance(fg) - rel_luminance(bg));
    let (mut core, mut bed) = (Vec::new(), Vec::new());
    let mut worst_row = f32::INFINITY;
    for y in y0..y1 {
        let mut row = Vec::new();
        for x in x0..x1 {
            let c = clean(y, x);
            if c == bg {
                bed.push(at(y, x));
                continue;
            }
            if rel_luminance(c) - rel_luminance(bg) >= reach {
                core.push(at(y, x));
            }
            if ratio(c, bg) < solid {
                continue;
            }
            for d in 1..=2i32 {
                let ring: Vec<u32> = (-d..=d)
                    .flat_map(|dy| (-d..=d).map(move |dx| (dy, dx)))
                    .filter(|&(dy, dx)| dy.abs().max(dx.abs()) == d && clean(y + dy, x + dx) == bg)
                    .map(|(dy, dx)| at(y + dy, x + dx))
                    .collect();
                if !ring.is_empty() {
                    row.push(ratio(at(y, x), median(&ring, ring.len() / 2)));
                    break;
                }
            }
        }
        if !row.is_empty() {
            row.sort_by(f32::total_cmp);
            worst_row = worst_row.min(row[(row.len() - 1) / 2]);
        }
    }
    let whole = ratio(median(&core, core.len() / 2), median(&bed, bed.len() / 2));
    whole.min(worst_row)
}

/// THE ONE-COLOUR LANDING RING KEEPS THE NEW PROMPT'S `$` AT THE LETTER BAR
/// (looks8 K2): its left bar crosses the prompt's `$` (the trail gallery's
/// classic-pink `enter-bottom` e120 read it at 4.21–4.32:1, admitted at the
/// flying lights' floor). Negative control: admitted as it was, the same
/// landing leaves the `$` under the bar.
#[test]
fn the_one_colour_landing_ring_keeps_the_prompt_at_the_bar() {
    let kept = pink_landing_dollar(false);
    assert!(
        kept >= 4.5,
        "the new prompt's `$` reads {kept:.2}:1 under the pink landing"
    );
    let control = pink_landing_dollar(true);
    assert!(
        control < 4.5,
        "control: admitted at the flying floor, the `$` still reads {control:.2}:1"
    );
}

/// Each pixel row's lift over `page` (the largest channel step over it, the
/// most across three columns) from the typed row's bottom down half a cell,
/// in the columns `behind` cells behind the caret, once `style` typed
/// [`LINE`] on `page` in `text` (`even`: every rail as looks7 laid it).
fn rail_profile(style: GlowStyle, (text, page): (u32, u32), even: bool, behind: f32) -> Vec<i32> {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    let g = tall_geom();
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let c = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: crate::cursor_fx::default_trail_intensity(Some(style), false),
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(style),
            beam: true,
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
        },
        CURSOR,
        true,
        text,
        page,
        0.5,
    );
    let mut glow = CursorGlow {
        rail_even: even,
        ..CursorGlow::default()
    };
    let mut out = Vec::new();
    let mut line = vec![' '; g.cols];
    line[0] = '$';
    let mut t = Instant::now();
    let mut caret = (ROW, START);
    for (k, ch) in std::iter::once(' ').chain(LINE.chars()).enumerate() {
        if k > 0 {
            t += Duration::from_millis(60);
            line[START as usize + k - 1] = ch;
            glow.note_synthetic_typed(t, 1);
        }
        caret = (ROW, START + k as u16);
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for line_cells in &mut rows {
            for cell in line_cells.iter_mut() {
                (cell.fg, cell.bg) = (rgb(text), rgb(page));
            }
        }
        for (col, cell) in rows[usize::from(ROW)].iter_mut().enumerate() {
            cell.ch = line[col];
        }
        glow.observe_row(caret.0, caret.1, &line, t);
        glow.observe_frame_text(Some(caret), &rows, t);
        glow.tick(Some(caret), t, &c, g, &mut out);
    }
    let mut glass = Glass::new(g, page);
    glass.add_quads(glow.under_quads());
    let x = (f32::from(g.origin_x) + (f32::from(caret.1) - behind) * g.cw as f32) as i32;
    let bottom = i32::from(g.origin_y) + i32::from(ROW + 1) * g.ch as i32;
    let ch = |c: u32, sh: u32| ((c >> sh) & 0xFF) as i32;
    (bottom..bottom + g.ch as i32 / 2)
        .map(|y| {
            (x - 1..=x + 1)
                .map(|x| {
                    let p = glass.at(y, x) & 0x00FF_FFFF;
                    [16, 8, 0]
                        .iter()
                        .map(|&sh| ch(p, sh) - ch(page, sh))
                        .max()
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0)
        })
        .collect()
}

/// LUMEN'S LIT RAIL IS SOFT (looks8 K10): on Solarized Dark Lumen's rail
/// was a crisp near-white 2 px hairline that read as a text underline; it
/// rises to its peak over a soft top and falls off below it with no
/// plateau, no pixel row stepping more than two thirds of its peak from the
/// next. Negative control: laid as looks7 laid it (full strength from its
/// top), the rail steps from the page to its peak in one pixel row.
#[test]
fn lumen_s_lit_rail_is_soft() {
    const SOLARIZED: (u32, u32) = (0x0083_9496, 0x0000_2B36);
    let steepest = |profile: &[i32]| -> (i32, i32) {
        let peak = profile.iter().copied().max().unwrap_or(0);
        let step = profile
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .chain(profile.first().copied())
            .max()
            .unwrap_or(0);
        (peak, step)
    };
    let (peak, step) = steepest(&rail_profile(GlowStyle::Lumen, SOLARIZED, false, 2.0));
    assert!(peak >= 24, "fixture: Lumen's rail lifts the page by {peak}");
    assert!(
        step * 3 <= peak * 2,
        "Lumen's rail steps {step} of its {peak} peak between two pixel rows"
    );
    let (peak, step) = steepest(&rail_profile(GlowStyle::Lumen, SOLARIZED, true, 2.0));
    assert!(
        step * 10 >= peak * 9,
        "control: laid as looks7 laid it, the rail steps only {step} of its {peak} peak"
    );
}

/// COMET'S RAIL TAPERS BEHIND ITS NUCLEUS (looks8 K10): on Solarized Dark
/// Beam's and Comet's rails both read as one cyan-blue underline band;
/// Comet's is deepest at its nucleus and thins behind it with its bed's
/// taper, while Beam's stays an even tube. Measured as the pixel rows lit
/// to a third of the column's own peak or more, half a cell and two and a
/// half cells behind the caret. Negative control: laid even, Comet's rail
/// is as deep behind its head as at it.
#[test]
fn comet_s_rail_tapers_behind_its_nucleus() {
    const SOLARIZED: (u32, u32) = (0x0083_9496, 0x0000_2B36);
    let depth = |style: GlowStyle, even: bool, behind: f32| -> usize {
        let profile = rail_profile(style, SOLARIZED, even, behind);
        let peak = profile.iter().copied().max().unwrap_or(0);
        assert!(
            peak >= 12,
            "fixture: {style:?}'s rail lifts {peak} at {behind} cells behind"
        );
        profile.iter().filter(|&&lift| lift * 3 >= peak).count()
    };
    let (head, tail) = (
        depth(GlowStyle::Comet, false, 0.5),
        depth(GlowStyle::Comet, false, 2.5),
    );
    let (bh, bt) = (
        depth(GlowStyle::Beam, false, 0.5),
        depth(GlowStyle::Beam, false, 2.5),
    );
    let (eh, et) = (
        depth(GlowStyle::Comet, true, 0.5),
        depth(GlowStyle::Comet, true, 2.5),
    );
    assert!(
        head * 2 >= tail * 3,
        "Comet's rail is {head} px deep at its nucleus and {tail} px behind it"
    );
    assert!(
        bt * 5 >= bh * 4,
        "Beam's tube is {bh} px deep at its head and {bt} px behind it"
    );
    assert!(
        et * 5 >= eh * 4,
        "control: laid even, Comet's rail is {eh} px deep at its head and {et} px behind it"
    );
}

/// Sparkle with its trail colour `pin` typed along [`LINE`] on row [`ROW`] of
/// `tall_geom` at 80 ms a key through [`CursorGlow::tick`], the trail pin set
/// (`saturated`: read nowhere, [`CursorGlow::sparkle_pin_saturated`]), on
/// `page` with `ink` text. Every light it lays, each as its own colour and
/// bright enough to hold a hue (the additive quads and halos; on paper the
/// inks too, opaque enough that their colour reads within two levels a
/// channel; on a dark page the inks are the night grounds that cancel a
/// tinted page's cast, [`CursorGlow::ground_the_frame`], not lights):
/// `(the most saturated, how many in Sparkle's gold hues at a gold's
/// chroma, how many)`.
fn sparkle_pinned_lights(
    pin: u32,
    (page, ink, dark): (u32, u32, bool),
    saturated: bool,
) -> (f32, usize, usize) {
    use crate::cursor_fx::{GlowKnobs, GlowSpelling, resolve_glow_config};
    let g = tall_geom();
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let c = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: Some(pin),
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: crate::cursor_fx::default_trail_intensity(Some(GlowStyle::Sparkle), false),
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(GlowStyle::Sparkle),
            beam: true,
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
        },
        CURSOR,
        dark,
        ink,
        page,
        0.5,
    );
    assert_eq!(
        c.color & 0x00FF_FFFF,
        pin,
        "fixture: the trail colour is the pin"
    );
    let mut glow = CursorGlow::default();
    glow.set_colour_pins(ColourPins {
        trail: true,
        accent: false,
    });
    glow.sparkle_pin_saturated = saturated;
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    text[0] = '$';
    let mut t = Instant::now();
    let (mut most, mut gold, mut lit) = (0.0f32, 0usize, 0usize);
    let mut judge = |light: u32| {
        let (h, s, v) = hsv(light);
        if v < 24.0 / 255.0 {
            return;
        }
        lit += 1;
        most = most.max(s);
        gold += usize::from((18.0..=60.0).contains(&h) && s > 0.4);
    };
    for (k, ch) in std::iter::once(' ').chain(LINE.chars()).enumerate() {
        if k > 0 {
            t += Duration::from_millis(80);
            text[START as usize + k - 1] = ch;
            glow.note_synthetic_typed(t, 1);
        }
        let caret = (ROW, START + k as u16);
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for row in rows.iter_mut() {
            for cell in row.iter_mut() {
                (cell.fg, cell.bg) = (rgb(ink), rgb(page));
            }
        }
        for (col, cell) in rows[usize::from(ROW)].iter_mut().enumerate() {
            cell.ch = text[col];
        }
        glow.observe_row(caret.0, caret.1, &text, t);
        glow.observe_frame_text(Some(caret), &rows, t);
        out.clear();
        glow.tick(Some(caret), t, &c, g, &mut out);
        for q in out.iter().chain(glow.under_quads()) {
            for (color, alpha) in [(q.color, q.alpha), (q.color2, q.alpha2)] {
                if alpha == 0 && color != 0 {
                    judge(color & 0x00FF_FFFF);
                } else if !dark && alpha >= 64 {
                    judge(straight(color, alpha));
                }
            }
        }
        // A light laid over the page (paper's stars) is its straight colour.
        for h in glow.halos().iter().filter(|h| {
            h.mode == aterm_render::HaloMode::Add
                || (!dark && h.mode == aterm_render::HaloMode::Over)
        }) {
            judge(h.color & 0x00FF_FFFF);
        }
    }
    (most, gold, lit)
}

/// looks9's review of Q7: A PINNED COLOUR IS SPARKLE'S WHOLE TRAIL. Only the
/// dark shower read the pin: pinned to `#b0b0c8` (saturation 0.12), the body
/// and tail still burned the pin brought to the gold's chroma (saturation
/// 0.73 on dark), and on paper the beads and moon were the fixed golds (951
/// gold lights, pinned or not), and the stars stayed gold after them (the
/// gallery's paper frames, review9fix round 1). Typed along a row pinned to `#b0b0c8` on the
/// reference dark, Solarized Dark and paper, no light Sparkle lays reads
/// over the pin's saturation by more than 0.04, and none is a gold. Negative
/// control (`sparkle_pin_saturated`, the pin read nowhere): the trail
/// reaches saturation 0.5 and more, and on paper lays golds.
#[test]
fn a_pinned_colour_is_sparkle_s_whole_trail() {
    let pin = 0x00B0_B0C8u32;
    let (_, pin_sat, _) = hsv(pin);
    for (name, page) in [
        ("dark", (0x0011_1318u32, 0x00C5_C8C6u32, true)),
        ("solarized dark", (0x0000_2B36, 0x0083_9496, true)),
        ("paper", (0x00FF_FFFF, 0x0022_2222, false)),
    ] {
        let (most, gold, lit) = sparkle_pinned_lights(pin, page, false);
        assert!(lit > 100, "{name}: fixture: the trail lights ({lit})");
        assert!(
            most <= pin_sat + 0.04,
            "{name}: a light reaches saturation {most:.2}, the pin's {pin_sat:.2}"
        );
        assert_eq!(gold, 0, "{name}: {gold} of {lit} lights are golds");
        let (old_most, old_gold, _) = sparkle_pinned_lights(pin, page, true);
        assert!(
            old_most >= 0.5 && (page.2 || old_gold > 0),
            "control: {name}: the pin read nowhere, the trail reaches only {old_most:.2} with {old_gold} golds"
        );
    }
}
