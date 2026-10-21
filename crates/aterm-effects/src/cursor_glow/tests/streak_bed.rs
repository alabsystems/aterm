// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The beam family's streak over text on a dark ground, through the whole
//! cursor frame step ([`crate::cursor_fx::CursorFx::tick`]): the under-ink
//! bed the letters sit on, and every additive light that still lands on them
//! (the crown, the particles, the caret's body and the momentum glow).

use super::*;
use crate::cursor_fx::{CursorFx, CursorFxInput};
use crate::cursor_momentum::MOMENTUM_GLOW_TAU_S;
use aterm_core::terminal::CursorStyle;

/// The shipped dark theme's pair, which the gallery captures on.
const FG: u32 = CursorGlow::STREAK_REF_INK;
const BG: u32 = CursorGlow::STREAK_REF_GROUND;
/// The shipped dark theme's cursor colour.
const CURSOR: u32 = 0x0054_F97A;
/// The gallery's typing line, after its `$ ` prompt.
const LINE: &str = "echo the quick brown fox";
const ROW: u16 = 3;
const START: u16 = 2;

/// Which negative control a burst runs under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
    /// The shipped engine.
    None,
    /// The streak as light over the letters, with no bed and no account.
    Unbudgeted,
    /// The bed in each stop at full value, not solved to the bar.
    Unsolved,
    /// The bed, but no over-ink account.
    NoAccount,
}

/// One captured frame of a burst.
struct Shot {
    name: &'static str,
    /// The worst of the three glyphs left of the caret, every light on.
    newest: f32,
    /// The same glyphs over the under-ink stack alone (bed, cell tint).
    bed: f32,
}

/// The shipped knobs and spelling of `style` (`mono` picks `classic mono`
/// for [`GlowStyle::Classic`]) on the dark default page, and the comet
/// body's config.
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
        // As a native window runs it: the comet's tail is its own streak.
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

/// The frame as the renderer composites it: the page, the comet's cell
/// tint, the under-ink stream and fire's field, the glyph ink, then (with
/// `light`) the additive quads and halos.
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

/// Type the gallery's line at its cadence (60 ms a key) through the whole
/// frame step with the momentum glow on, the row probe fed as the GUI feeds
/// it, and capture the gallery's frames: `mid` (13 keys), `t0` (the last
/// key), `t120`.
fn burst(style: GlowStyle, control: Control) -> Vec<Shot> {
    burst_face(style, false, control)
}

/// [`burst`] with the classic wake's face named ([`style_cfg`]).
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

/// The styles whose dark streak lies under the letters as a solved bed: the
/// beam family but sparkle, whose gold rides its rail, its bridge and its
/// crown with nothing under its letters.
fn bedded() -> [GlowStyle; 5] {
    [
        GlowStyle::Lumen,
        GlowStyle::Comet,
        GlowStyle::Laser,
        GlowStyle::Phaser,
        GlowStyle::Beam,
    ]
}

/// The newest letters of a typed burst on the dark default page, at the
/// gallery's frames, through the whole frame step with the momentum glow
/// on: over the bed alone they read 4.5:1 or better, and with every light
/// laid (the crown, the grains, the caret's body, the momentum glow) 3:1 or
/// better. The bed is there: it takes the letters well under their clean
/// contrast. Fire, sparkle and water keep the 3:1 too (fire and sparkle with
/// no bed; water over its own wake), and the classic wake's mono face keeps both bars over its
/// own bed. Negative controls, each the same bursts: the bed in each
/// stop at its full value moves every style's letters off the solved bed's
/// contrast (the solve is what sets it), and sinks some under 4.5:1 over the
/// bed alone; the streak as light
/// over the letters with no bed and no account sinks one of every style's
/// under 3:1 (the mono face's too, drawn as the spectrum face draws); and
/// with no account the lights alone sink one under 3:1 for some style the
/// account holds, so the account is what holds the bar.
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
    // The classic wake's mono face lays its tracer the same way (its
    // spectrum face keeps the original's light, byte for byte).
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

/// The opacity of the bed at one cell's middle column, over the row's spine
/// (the most any of its pieces lays there).
fn bed_alpha_at(under: &[GlowQuad], x: u16) -> u8 {
    under
        .iter()
        .filter(|q| q.alpha > 0 && q.x <= x && x < q.x + q.w)
        .map(|q| q.alpha.max(q.alpha2))
        .max()
        .unwrap_or(0)
}

/// One typed cell's bed over its life, beside its sample's coverage, from
/// the last key of a burst until the cell has faded: `(bed opacity, sample
/// coverage)` per 8 ms, and whether both reached nothing.
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
    // The newest letter: laid by the last key, so it starts fresh.
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

/// Whether a cell's bed follows its sample's fade: never above what its
/// coverage asks of the cap (within the bed's dither and a level of each
/// byte's truncation), so as the sample fades the bed comes down with it,
/// and out when it is out. A taper (the oldest cell's feather, a wedge's
/// fall behind its head) only lowers it further.
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

/// The bed keeps each streak's own fade over the text: one typed cell's bed
/// opacity never stands above its coverage's share of the bed's cap, frame
/// by frame, from fresh to gone, so the comet's taper, the laser's bright
/// fresh cell and the beam's power-down play out under the letters as they
/// did in light; outside the laser's ripple it never rises as the cell
/// ages; and the cell starts well up the cap (the newest letter, a cell
/// behind a comet's head, keeps most of its wedge) and fades to nothing,
/// or the probe proves nothing. Negative control: a bed held at its first
/// opacity while the cell fades fails the same probe.
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
        // The probe needs the whole fade, from well up the cap to nothing.
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

/// Over cells with no text the streak is light at full strength: a leap
/// along a row the probe shows blank lights the cells it crosses exactly as
/// the streak with no bed and no account does, pixel for pixel, with the bed
/// under it besides. Over a typed run the bridge onto the caret's cell keeps
/// the head as bright as it was: the bed takes the letters, not the caret's
/// light.
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
            // The cells the leap crosses, short of the caret's bridge, and
            // the rows either side its glow reaches.
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
            bedded == bare,
            "{style:?}: the beam over air is the full beam"
        );
        assert!(bed > 0, "{style:?}: the bed lies under the leap too");

        // Typed: the brightest level over the caret's own cell.
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

/// One burst's t0 frame (the last key) as [`burst_face`] types it, every
/// light on, with the control flags `set` applies to the engine; returns
/// the composite and the caret's column.
fn t0_frame(style: GlowStyle, set: &dyn Fn(&mut CursorFx)) -> (Glass, u16) {
    let (lit, _, caret, _) = typed_frame(style, set, LINE.chars().count());
    (lit, caret)
}

/// The burst typed to its `typed`th key: the frame with every light on,
/// the same frame with only the under-ink stack (the bed, the tint), the
/// caret's column, and the engine as it stands.
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

/// An `0x00RRGGBB` colour's HSV hue (degrees), saturation and value.
fn hsv(rgb: u32) -> (f32, f32, f32) {
    crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF)
}

/// A premultiplied quad colour at opacity `alpha` back to its straight
/// colour.
fn straight(premul: u32, alpha: u8) -> u32 {
    let a = u32::from(alpha.max(1));
    let ch = |sh: u32| ((((premul >> sh) & 0xff) * 255 + a / 2) / a).min(255);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Sparkle on a dark ground is a gold comet with nothing under its letters.
/// At the mid and t0 frames: the rail in the leading under the newest letter
/// is a warm gold, bright and saturated, and so is it two cells back along
/// its tail (a gold comet, not a sunset running to violet and crimson), though
/// the wheel the grains are dealt from stands far from gold at one of the
/// two; under the newest letter lies only the page (no lavender dusk); and
/// the rail's top melts into the row over more than a pixel rather than
/// standing on its edge as a crisp rule. Negative controls: the same rail
/// stood on the row's edge (`rail_rise` 0) shows the crisp top the probe
/// looks for, and the shape with a share of the bar (the old dusk) lays a
/// colour under the letter.
#[test]
fn the_sparkle_comet_burns_gold_with_nothing_under_its_letters() {
    use crate::cursor_glow::bed::{BedScratch, BedShape, lay_dark_bed};
    let g = tall_geom();
    let mut wheel_off_gold = false;
    let (row_top, row_bot) = {
        let (_, y0, _, y1) = Glass::new(g, BG).cell_rect(ROW, 0);
        (y0, y1)
    };
    // Gold in hue and saturation; the tail fades by its opacity, so its
    // value falls with it.
    let gold = |rgb: u32| {
        let (h, sat, val) = hsv(rgb);
        (32.0..=62.0).contains(&h) && sat >= 0.5 && val >= 0.2
    };
    for typed in [13usize, LINE.chars().count()] {
        let (lit, bed, caret, fx) = typed_frame(GlowStyle::Sparkle, &|_| {}, typed);
        let newest = caret - 1;
        for back in [0u16, 2] {
            let (x0, _, _, _) = lit.cell_rect(ROW, newest - back);
            // The rail, a pixel into the leading under the cell.
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
        // Nothing under the newest letter: a counter column's middle.
        let (x0, _, _, _) = bed.cell_rect(ROW, newest);
        let under = bed.at(row_top + g.ch as i32 / 2, x0 + 4) & 0x00FF_FFFF;
        assert_eq!(under, BG, "key {typed}: a colour under the newest letter");
    }
    assert!(
        wheel_off_gold,
        "control: the wheel stood near gold at both frames, so the gold proves nothing"
    );
    // The rail's top: the largest one-pixel step of its lift down the
    // column through its top, against its peak.
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
        // Every row read and blank but the typed one, so the rail's leading
        // under it is known air.
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
            &mut under,
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

/// The phaser's dark bed keeps the night arc: every saturated colour it lays
/// under the letters of the typed row, over every frame of the burst, is a
/// blue, a violet or a magenta, never the olive or brown a warm or green
/// stop becomes at the bar's luminance. (The raw spectrum's negative control
/// is `the_night_arc_keeps_the_spectrum_beds_on_blue_violet_and_magenta`.)
#[test]
fn the_phaser_bed_keeps_the_night_arc() {
    let g = tall_geom();
    let (_, y0, _, _) = Glass::new(g, BG).cell_rect(ROW, 0);
    let ch = g.ch as i32;
    // The bed's straight colours in the x-height band of the typed row, over
    // every frame of the burst.
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

/// The over-ink account holds back only the light over the letters: over
/// the blank cells around the caret (the two cells ahead of it, and the
/// blank rows above and below them) the caret's bloom, its body and the
/// momentum glow land exactly as they do with no account at all, and they
/// are there to see. Negative control: the account that cut a whole light
/// to the share of its least-room letter dims those cells too, for a style
/// whose light over the letters leaves them little room (the beam's
/// stardust, which spent it, keeps off the letters now).
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

/// A leap over rows the frame shows blank (a clear homes the caret) is the
/// style's light, not its bed: handed the frame's cells, the leap down the
/// column lights it bright (its median luminance well over the bed's bar), for bedded
/// styles and the classic wake's mono face alike. Negative control: with no
/// cells handed, those rows are unknown and the leap is only its bed, at the
/// bar's luminance.
#[test]
fn a_leap_over_rows_the_frame_shows_blank_is_light() {
    let g = tall_geom();
    let cells: Vec<Vec<aterm_core::terminal::RenderCell>> =
        vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    // A smaller frame can leave the remembered caret entirely off-grid,
    // with or without a current caret. Its unread rows stay unknown.
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
            // The column's median (a grain shed along the leap is a point).
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

/// A style's vivid rail lies in the leading under the run, where no letter
/// of the row stands: over a blank row below, every under-ink quad below the
/// typed row's bottom is within a fifth of a cell of it and brighter than
/// the bar's bed (its ink at the rail's luminance floor), and some is
/// there; nothing under the ink reaches above the row. A style with no rail
/// (Lumen) lays nothing under the row, and over a line of text below (a
/// selection, earlier output, whose letters' tops would sit in it) no
/// style lays its rail: the controls.
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
        let reach = (g.ch as f32 * 0.2).ceil() as i32;
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

/// The night arc holds every hue on blue, violet and magenta, where a bed at
/// the bar keeps its colour, and rolls through them without a jump: each
/// whole degree of the wheel lands in [195°, 330°], a degree on moves it at
/// most a few degrees, and the arc is the bed's hue for the spectrum styles.
/// Negative control: the raw spectrum the phaser rolls through leaves the
/// arc (its greens and yellows).
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

/// A comet's bed is a wedge and every typed run's bed rises from nothing at
/// its tail: laid under a run of fresh cells, the comet's bed is whole
/// under the caret, thins with each cell behind it and is gone three and a
/// half cells back, and a lumen run's bed is nothing at its tail's outer
/// edge and whole a cell and a half in. Negative control: the comet's shape
/// with no wedge keeps its bed whole three cells back.
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
            &mut under,
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

/// The bed opacity at pixel column `x` (any quad over it).
fn bed_alpha_at_column(under: &[GlowQuad], x: u16) -> u8 {
    bed_alpha_at(under, x)
}

/// Lumen's bed under `run`, alone.
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
        &mut under,
    );
    under
}

/// The head's bridge onto the caret's cell is the caret's light, kept in
/// that cell: typed over text, a streak's own additive light (no crown, no
/// body) lies inside the typed row, never a stub above and below the
/// caret. Negative control: the streak drawn whole over the letters (no
/// bed) spills its halo out of the row for some style (the beam's).
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
                &mut light,
                &mut halos,
                &mut under,
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

/// The account holds the lines around the caret's to the text bar: one
/// bright light laid over the caret's line and the line of output above
/// it, admitted at the momentum glow's tier, leaves a letter above at 4.5:1
/// or better while it washes the caret line's letter down toward its own
/// tier's floor, under 4.5:1 (the control: the same light, the same ledger,
/// only the line differs).
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
    // A flat white light over cols 8..12 of the line above and the caret's.
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

/// A letter is held against the colours it is really drawn in: with the
/// frame's own cells handed over (its first row empty, as a host may hand a
/// blank row), one bright light over a selected line
/// above the caret's (its ground the selection's, `#33415E`) leaves its
/// letters 4.5:1 or better on that ground. Negative control: without the
/// cells the account takes the page for their ground and lets the same
/// light sink them under 4.5:1.
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
    // A host's frame need not carry every column of every row (a blank row
    // may come empty): the rows read keep their own colours.
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

/// A LEAP'S BED IS ONE TUBE ACROSS A SHORT STRETCH OF AIR: an Enter-like
/// leap down from the end of one line to the start of the next crosses a
/// cell or two past the lower line's end before it reaches its letters; the
/// bed lays one tube through them (two tubes with a gap between read as two
/// offset slabs). Negative control: across a long stretch of air the bed
/// still leaves the air to the beam (no tube in the middle of the gap).
#[test]
fn a_leap_bed_is_one_tube_across_a_short_gap() {
    use crate::cursor_glow::bed::{BedScratch, BedShape, lay_dark_bed};
    let g = tall_geom();
    let shape = BedShape::of(GlowStyle::Beam, 1.0).expect("the beam lays a bed");
    // The leap's cells: row 2 from col 14 left, then row 3; the lower line's
    // text ends `gap` cells before the leap reaches it.
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
            &mut under,
        );
        let mut glass = Glass::new(g, BG);
        glass.add_quads(&under);
        // The middle of the air the leap crosses on row 3, its whole height.
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
