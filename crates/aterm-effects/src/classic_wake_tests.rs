// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::color_math::relative_luminance;
use crate::cursor_glow::GlowStyle;
use aterm_render::{HaloMode, halo_over_cap, over_premul};

const fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    }
}

fn cfg() -> GlowConfig {
    GlowConfig {
        style: GlowStyle::Classic,
        accent: 0x0078_FFB9,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        theme_fg: 0x00C8_C8D2,
        theme_bg: 0x0010_1016,
        beam: true,
        ribbon_tall: false,
        ..Default::default()
    }
}

const PAPER: u32 = 0x00FF_FFFF;
const INK: u32 = 0x001F_2328;

fn light_cfg() -> GlowConfig {
    GlowConfig {
        dark_theme: false,
        theme_fg: INK,
        theme_bg: PAPER,
        color: 0x0009_69DA,
        accent: 0x0054_AEFF,
        ..cfg()
    }
}

#[derive(Default)]
struct Rig {
    w: ClassicWake,
    out: Vec<GlowQuad>,
    out_ramps: aterm_render::GlowRamps,
    halos: Vec<RainHalo>,
    under: Vec<GlowQuad>,
    under_ramps: aterm_render::GlowRamps,
    ledger: InkLedger,
    spans: crate::cursor_glow::TextSpans,
    /// The ground the one-colour bed mixes toward ([`WakeInk::bed_ground`]).
    bed_ground: Option<u32>,
}

impl Rig {
    /// One tick, its ramps laid as the quads they stand for.
    fn tick(&mut self, cur: Option<(u16, u16)>, now: Instant, c: &GlowConfig, g: Geom) -> u64 {
        self.w.tick(
            cur,
            now,
            c,
            g,
            &mut crate::cursor_glow::glow_sink(&mut self.out, &mut self.out_ramps, true),
            &mut self.halos,
            &mut crate::cursor_glow::glow_sink(&mut self.under, &mut self.under_ramps, true),
            WakeInk {
                text: crate::cursor_glow::TextView::new(self.spans, None),
                ledger: &mut self.ledger,
                chosen: false,
                feet: (&[], &[], [None; 2]),
                bed_ground: self.bed_ground,
            },
        )
    }
}

fn ms(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

fn assert_banded(out: &[GlowQuad], g: Geom) {
    for q in out {
        assert!(
            i32::from(q.x) >= g.fx_left() && i32::from(q.x) + i32::from(q.w) <= g.fx_right(),
            "quad escapes the effects box horizontally: {q:?}"
        );
        assert!(
            i32::from(q.y) >= g.fx_top() && i32::from(q.y) + i32::from(q.h) <= g.fx_bot(),
            "quad escapes the effects box vertically: {q:?}"
        );
        let band = i32::from(q.y) - i32::from(g.origin_y);
        let row = band.div_euclid(g.ch as i32);
        assert_eq!(
            row,
            i32::from(q.row),
            "quad's row tag disagrees with its pixels: {q:?}"
        );
        assert!(
            (band + i32::from(q.h) - 1).div_euclid(g.ch as i32) == row,
            "quad spans two cell rows: {q:?}"
        );
        assert_eq!(
            q.alpha, 0,
            "on a dark ground the classic wake is ADDITIVE light only"
        );
    }
}

/// The one-colour landing ring's quads each frame for 300 ms after a jump
/// from `(1, 30)` to `land`, the text `lines` probed, `cut` laying the ring
/// as it was ([`ClassicWake::ring_cut`]).
fn mono_ring_frames(land: (u16, u16), lines: &[(i32, i32, i32)], cut: bool) -> Vec<Vec<GlowQuad>> {
    let c = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        ..cfg()
    };
    ring_frames(&c, land, lines, (cut, false))
}

/// [`mono_ring_frames`] in the config `c`: on paper, the ring's rim.
fn ring_frames(
    c: &GlowConfig,
    land: (u16, u16),
    lines: &[(i32, i32, i32)],
    (cut, row_square): (bool, bool),
) -> Vec<Vec<GlowQuad>> {
    let (g, c) = (geom(), *c);
    let mut rig = Rig {
        spans: crate::cursor_glow::TextSpans::of_lines(lines),
        ..Rig::default()
    };
    rig.w.ring_cut = cut;
    rig.w.ring_row_square = row_square;
    let t0 = Instant::now();
    rig.tick(Some((1, 30)), t0, &c, g);
    let mut frames = Vec::new();
    for k in 1..=40u64 {
        rig.out.clear();
        rig.under.clear();
        rig.halos.clear();
        rig.tick(Some(land), ms(t0, 8 * k), &c, g);
        frames.push(rig.w.ring_seen.take());
    }
    frames
}

/// Whether `quads` are a whole square outline (the four bars of one square,
/// every pixel of its rim lit), and whether any lies on a cell `lines`
/// covers but `own`.
fn ring_reading(quads: &[GlowQuad], lines: &[(i32, i32, i32)], own: (u16, u16)) -> (bool, bool) {
    let (whole, on_text, (x0, x1, y0, y1)) = rim_reading(quads, lines, own);
    (whole && (x1 - x0) == (y1 - y0), on_text)
}

/// Whether `quads` are a whole rectangular outline (every pixel of its rim
/// lit), whether any lies on a cell `lines` covers but `own`, and its
/// bounds `(x0, x1, y0, y1)`, inclusive.
fn rim_reading(
    quads: &[GlowQuad],
    lines: &[(i32, i32, i32)],
    own: (u16, u16),
) -> (bool, bool, (i32, i32, i32, i32)) {
    let g = geom();
    let px: std::collections::BTreeSet<(i32, i32)> = quads
        .iter()
        .flat_map(|q| {
            (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).flat_map(move |y| {
                (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).map(move |x| (x, y))
            })
        })
        .collect();
    let on_text = px.iter().any(|&(x, y)| {
        let (r, c) = (y.div_euclid(g.ch as i32), x.div_euclid(g.cw as i32));
        (r, c) != (i32::from(own.0), i32::from(own.1))
            && lines
                .iter()
                .any(|&(lr, a, b)| lr == r && (a..b).contains(&c))
    });
    let (x0, x1) = (
        px.iter().map(|p| p.0).min().unwrap_or(0),
        px.iter().map(|p| p.0).max().unwrap_or(0),
    );
    let (y0, y1) = (
        px.iter().map(|p| p.1).min().unwrap_or(0),
        px.iter().map(|p| p.1).max().unwrap_or(0),
    );
    let rim = (x0..=x1).all(|x| px.contains(&(x, y0)) && px.contains(&(x, y1)))
        && (y0..=y1).all(|y| px.contains(&(x0, y)) && px.contains(&(x1, y)));
    (rim, on_text, (x0, x1, y0, y1))
}

/// looks9 Q4, THE ONE-COLOUR RING IS WHOLE OR NOT DRAWN: on a dark page the
/// one-colour face's landing ring grows while its four bars keep inside
/// the grid and off the text's cells, holds that size as it fades, and is
/// not drawn where its first size meets them. Cut where it crossed the
/// grid's edge and the letters, an Enter's ring left a lone dim bar right
/// of the caret on Solarized Dark and passed a bar through the `$` on dark
/// (the trail gallery's classic `#d03a8c` `enter-bottom` e120). Negative
/// control: the ring laid as it was ([`ClassicWake::ring_cut`]) breaks.
#[test]
fn the_one_colour_ring_is_whole_or_not_drawn() {
    // A landing beside a `$` with a line above it clear of the ring's first
    // size, and a landing on the grid's last row under a printed line.
    let mid: (u16, u16) = (2, 2);
    let mid_lines = [(2, 0, 1), (1, 10, 17)];
    let bottom: (u16, u16) = (5, 2);
    let bottom_lines = [(5, 0, 1), (4, 0, 19)];
    for (land, lines, wants_ring) in [(mid, &mid_lines, true), (bottom, &bottom_lines, false)] {
        let frames = mono_ring_frames(land, lines, false);
        let drawn = frames.iter().filter(|q| !q.is_empty()).count();
        for (k, quads) in frames.iter().enumerate().filter(|(_, q)| !q.is_empty()) {
            let (whole, on_text) = ring_reading(quads, lines, land);
            assert!(
                whole,
                "landing {land:?}, frame {k}: the ring is not whole: {quads:?}"
            );
            assert!(
                !on_text,
                "landing {land:?}, frame {k}: a bar lies on the text"
            );
        }
        if wants_ring {
            assert!(
                drawn >= 10,
                "landing {land:?}: the ring shows in {drawn} frames"
            );
        } else {
            assert_eq!(
                drawn, 0,
                "landing {land:?}: a ring that cannot be whole shows"
            );
        }
        let broken = mono_ring_frames(land, lines, true)
            .iter()
            .filter(|q| !q.is_empty())
            .filter(|q| {
                let (whole, on_text) = ring_reading(q, lines, land);
                !whole || on_text
            })
            .count();
        assert!(
            broken > 0,
            "control: landing {land:?}: the ring as it was stays whole and off the text"
        );
    }
}

/// The one-colour ring's quads each 8 ms frame after a jump from `(1, 30)`
/// to `(2, 2)` beside a `$`, the frame `k` (from 0) probing `lines(k)`;
/// `unlatched` judging the ring afresh each frame ([`ClassicWake::ring_unlatched`]).
fn mono_ring_frames_over(
    lines: &dyn Fn(u64) -> Vec<(i32, i32, i32)>,
    unlatched: bool,
) -> Vec<Vec<GlowQuad>> {
    let g = geom();
    let c = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        ..cfg()
    };
    let mut rig = Rig::default();
    rig.w.ring_unlatched = unlatched;
    let t0 = Instant::now();
    rig.spans = crate::cursor_glow::TextSpans::of_lines(&[(1, 28, 29)]);
    rig.tick(Some((1, 30)), t0, &c, g);
    (0..30u64)
        .map(|k| {
            rig.out.clear();
            rig.under.clear();
            rig.halos.clear();
            let mut now = vec![(2, 0, 1)];
            now.extend(lines(k));
            rig.spans = crate::cursor_glow::TextSpans::of_lines(&now);
            rig.tick(Some((2, 2)), ms(t0, 8 * (k + 1)), &c, g);
            rig.w.ring_seen.take()
        })
        .collect()
}

/// looks9's review of Q4: THE ONE-COLOUR RING'S VERDICT IS ITS LANDING'S.
/// Judged whole or not afresh from each frame's text, a ring whose bar a
/// line met for three frames hid for them and came back
/// (`#####...###############`): a blink. Landing beside a `$`, the ring
/// shows; met by a line over its top bar from its sixth frame for three
/// frames, or from then on, it is never drawn again once it goes out,
/// and no drawn frame lays a bar on the text. Negative control
/// ([`ClassicWake::ring_unlatched`]): judged afresh, the ring met for
/// three frames comes back.
#[test]
fn the_one_colour_ring_s_verdict_is_its_landing_s() {
    let line = (1, 1, 9);
    // `(drawn, came back, a bar on the text)`.
    let read = |frames: &[Vec<GlowQuad>], lines: &dyn Fn(u64) -> Vec<(i32, i32, i32)>| {
        let drawn = frames.iter().filter(|q| !q.is_empty()).count();
        let shown = frames.iter().position(|q| !q.is_empty()).unwrap_or(0);
        let out = frames[shown..]
            .iter()
            .position(Vec::is_empty)
            .map_or(frames.len(), |i| shown + i);
        let back = frames[out..].iter().any(|q| !q.is_empty());
        let on_text = frames.iter().enumerate().any(|(k, q)| {
            let mut now = vec![(2, 0, 1)];
            now.extend(lines(k as u64));
            !q.is_empty() && ring_reading(q, &now, (2, 2)).1
        });
        (drawn, back, on_text)
    };
    let still = |_: u64| Vec::new();
    let (drawn, back, _) = read(&mono_ring_frames_over(&still, false), &still);
    assert!(
        drawn >= 10 && !back,
        "fixture: on a still page the ring shows ({drawn} frames)"
    );
    let passing = move |k: u64| {
        if (6..9).contains(&k) {
            vec![line]
        } else {
            Vec::new()
        }
    };
    let staying = move |k: u64| if k >= 6 { vec![line] } else { Vec::new() };
    for (label, lines) in [
        (
            "met for three frames",
            &passing as &dyn Fn(u64) -> Vec<(i32, i32, i32)>,
        ),
        ("met from then on", &staying),
    ] {
        let (drawn, back, on_text) = read(&mono_ring_frames_over(lines, false), lines);
        assert!(
            drawn >= 3,
            "{label}: fixture: the ring shows first ({drawn})"
        );
        assert!(!back, "{label}: the ring goes out and is drawn again");
        assert!(!on_text, "{label}: a drawn frame lays a bar on the text");
    }
    let (_, old_back, _) = read(&mono_ring_frames_over(&passing, true), &passing);
    assert!(
        old_back,
        "control: judged afresh, the ring met for three frames never comes back"
    );
}

/// looks10 L12, THE PAPER RING IS WHOLE AND OFF THE LETTERS: on paper the
/// landing ring's rim (the spectrum face's and the one-colour face's) grows
/// while its four bars keep inside the grid and off the text's cells; where
/// its first size meets them (an Enter's landing on the last row under a
/// printed line) it holds the caret's row, and where that meets them too (a
/// landing inside a word) it is not drawn. Grown over them, an Enter's box
/// ran its left bar down the `t` of the printed line and the `$` and its
/// top bar along that line (the trail gallery's classic `enter-bottom` e120
/// on GitHub Light and Default Light). Negative control: the rim laid as it
/// was ([`ClassicWake::ring_cut`]) lies on the text. Held to the caret's row,
/// it is a box whose top and bottom bars run past the caret's cell on both
/// sides, where a block caret leaves them seen (looks10 round 2's L12: as a
/// square as tall as the row it read as two side bars). Negative control:
/// held to the row as a square ([`ClassicWake::ring_row_square`]), its bars
/// barely pass the caret's cell.
#[test]
fn the_paper_ring_is_whole_and_off_the_letters() {
    let mid: (u16, u16) = (2, 2);
    let mid_lines = [(2, 0, 1), (1, 10, 17)];
    let bottom: (u16, u16) = (5, 2);
    let bottom_lines = [(5, 0, 1), (4, 0, 19)];
    let inside: (u16, u16) = (3, 6);
    let inside_lines = [(3, 0, 14)];
    let pink = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        ..light_cfg()
    };
    for c in [light_cfg(), pink] {
        let mono = c.classic_mono;
        for (land, lines, wants_ring) in [
            (mid, &mid_lines[..], true),
            (bottom, &bottom_lines[..], true),
            (inside, &inside_lines[..], false),
        ] {
            let frames = ring_frames(&c, land, lines, (false, false));
            let drawn = frames.iter().filter(|q| !q.is_empty()).count();
            for (k, quads) in frames.iter().enumerate().filter(|(_, q)| !q.is_empty()) {
                let (whole, on_text, _) = rim_reading(quads, lines, land);
                assert!(
                    whole && !on_text,
                    "mono {mono}, landing {land:?}, frame {k}: whole {whole}, on the \
                     text {on_text}: {quads:?}"
                );
            }
            if wants_ring {
                assert!(
                    drawn >= 10,
                    "mono {mono}, landing {land:?}: the ring shows in {drawn} frames"
                );
            } else {
                assert_eq!(
                    drawn, 0,
                    "mono {mono}, landing {land:?}: a ring that cannot be whole shows"
                );
            }
        }
        let on_text = ring_frames(&c, bottom, &bottom_lines, (true, false))
            .iter()
            .filter(|q| !q.is_empty() && ring_reading(q, &bottom_lines, bottom).1)
            .count();
        assert!(
            on_text > 0,
            "control: mono {mono}: the Enter's rim as it was keeps off the text"
        );
        // The row's box: its least reach past the caret's cell on either side.
        let g = geom();
        let th = (g.ch as i32 / 8).max(2);
        let (cell_x0, cell_x1) = (
            i32::from(bottom.1) * g.cw as i32,
            (i32::from(bottom.1) + 1) * g.cw as i32 - 1,
        );
        let past = |row_square: bool| {
            ring_frames(&c, bottom, &bottom_lines, (false, row_square))
                .iter()
                .filter(|q| !q.is_empty())
                .map(|q| {
                    let (_, _, (x0, x1, _, _)) = rim_reading(q, &bottom_lines, bottom);
                    (cell_x0 - x0).min(x1 - cell_x1)
                })
                .max()
                .unwrap_or(0)
        };
        let reach = past(false);
        assert!(
            reach > 2 * th,
            "mono {mono}: the Enter's box runs only {reach} px past the caret's cell"
        );
        let square = past(true);
        assert!(
            square <= 2 * th,
            "control: mono {mono}: as a square its bars run {square} px past the caret"
        );
    }
}

#[test]
fn the_comet_run_pool_keeps_its_buffers_between_frames() {
    let (g, c) = (geom(), cfg());
    let mut rig = Rig::default();
    let t0 = Instant::now();
    let frame = |rig: &mut Rig, k: u64| {
        let col = u16::try_from((k % 30) + 2).expect("column fits");
        let row = if k.is_multiple_of(7) { 4 } else { 2 };
        rig.tick(Some((row, col)), ms(t0, k * 8), &c, g);
    };
    for k in 0..120 {
        frame(&mut rig, k);
    }
    let pooled = |w: &ClassicWake| -> usize { w.comet_runs.iter().map(Vec::capacity).sum() };
    let warm = pooled(&rig.w);
    assert!(
        warm > 0,
        "fixture: the builder must have built something to pool"
    );
    assert!(
        !rig.w.comet_runs.is_empty(),
        "fixture: the pool holds at least one run buffer"
    );
    let mut slots_low = usize::MAX;
    let mut slots_high = 0usize;
    let mut cap_low = usize::MAX;
    for k in 120..600 {
        frame(&mut rig, k);
        slots_low = slots_low.min(rig.w.comet_runs.len());
        slots_high = slots_high.max(rig.w.comet_runs.len());
        cap_low = cap_low.min(pooled(&rig.w));
    }
    assert!(
        slots_high > 1,
        "fixture: the gesture must need more than one run, or the pool's own law is untested ({slots_high} high)"
    );
    assert_eq!(
        slots_low, slots_high,
        "the run pool SHRANK ({slots_low} low, {slots_high} high): its buffers are freed and rebuilt every redraw"
    );
    assert!(
        cap_low >= warm,
        "the run pool lost capacity between frames ({warm} warm, {cap_low} low)"
    );
}

#[test]
fn the_classic_wake_spawns_on_motion_alone_with_no_license() {
    let (g, c) = (geom(), cfg());
    let mut rig = Rig::default();
    let t0 = Instant::now();
    rig.tick(Some((2, 4)), t0, &c, g);
    let fp = rig.tick(Some((4, 34)), ms(t0, 16), &c, g);
    assert!(!rig.out.is_empty(), "a licence-free jump must still light");
    assert_ne!(fp, 0, "a lit frame fingerprints non-zero");
    assert_banded(&rig.out, g);
}

#[test]
fn a_wide_glyph_is_typing_and_a_longer_step_is_a_jump() {
    let (g, c) = (geom(), cfg());
    let t0 = Instant::now();
    let mut rig = Rig::default();
    rig.tick(Some((2, 4)), t0, &c, g);
    rig.tick(Some((2, 6)), ms(t0, 60), &c, g);
    rig.tick(Some((2, 8)), ms(t0, 120), &c, g);
    assert!(rig.w.heat > 0.0, "two fast wide glyphs earn typing heat");
    assert!(rig.w.ring.is_none(), "a wide glyph lays no landing ring");
    assert_eq!(rig.w.crown_window_ms, ClassicWake::CROWN_TYPING_MS);
    rig.tick(Some((2, 6)), ms(t0, 180), &c, g);
    assert!(rig.w.ring.is_none(), "erasing a wide glyph lays no ring");
    let heat = rig.w.heat;
    rig.tick(Some((3, 6)), ms(t0, 240), &c, g);
    assert!(rig.w.ring.is_none(), "a one-row step lays no ring");
    assert!(rig.w.heat > heat, "a one-row step is typing and earns heat");

    let mut jump = Rig::default();
    jump.tick(Some((2, 4)), t0, &c, g);
    jump.tick(Some((2, 7)), ms(t0, 60), &c, g);
    assert!(jump.w.ring.is_some(), "a three-column step is a jump");
    assert_eq!(jump.w.crown_window_ms, ClassicWake::CROWN_MS);
    assert_eq!(jump.w.heat, 0.0, "a jump earns no typing heat");
}

#[test]
fn the_comet_ends_on_a_bar_caret() {
    let g = geom();
    let t0 = Instant::now();
    let centroid = |head_dx: f32| -> f32 {
        let c = GlowConfig {
            head_dx,
            radius: 0.0,
            ring: false,
            ..cfg()
        };
        let mut rig = Rig::default();
        rig.tick(Some((1, 5)), t0, &c, g);
        rig.tick(Some((5, 5)), ms(t0, 16), &c, g);
        assert!(!rig.out.is_empty(), "fixture: the jump lit");
        let (mut sum, mut weight) = (0.0f64, 0.0f64);
        for q in &rig.out {
            let lum =
                f64::from(((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff));
            let wgt = lum * f64::from(q.w) * f64::from(q.h);
            sum += wgt * (f64::from(q.x) + f64::from(q.w) / 2.0);
            weight += wgt;
        }
        (sum / weight) as f32
    };
    let cw = g.cw as f32;
    for head_dx in [0.08f32, 0.5] {
        let want = (5.0 + head_dx) * cw;
        let got = centroid(head_dx);
        assert!(
            (got - want).abs() <= 1.0,
            "head_dx {head_dx}: the streak runs at x {got}, not on the caret at {want}"
        );
    }
}

/// THE BOX BLOOM IS THE BAR'S BEHIND A BAR (composed r1's judges, O4(a)): the dark spectrum
/// face's box bloom (`emit_crown`) spanned the caret's whole cell, so behind a bar caret a box
/// sized to a block cell framed the 2 px bar with the bar at its left third. It now blooms
/// round the caret's body: the cell under a block, the bar (twice `head_dx` across from the
/// cell's left edge) under a bar. The crown's own light (the frame laid with the crown less
/// the frame laid without it, `radius` 0) after a keystroke: behind a bar its light-weighted
/// centre lies within a pixel of the bar's and none of it reaches past the bar by more than
/// the bloom's grow; under a block its centre is the cell's. Negative control
/// (`crown_cell_wide` alone): behind the bar the centre is the cell's again.
#[test]
fn the_box_bloom_is_the_bar_s_behind_a_bar() {
    let g = geom();
    let t0 = Instant::now();
    let crown = |head_dx: f32, wide: bool| -> Vec<u32> {
        let lay = |radius: f32| -> Vec<u32> {
            let c = GlowConfig {
                head_dx,
                radius,
                ring: false,
                ..cfg()
            };
            let mut rig = Rig::default();
            rig.w.crown_cell_wide = wide;
            rig.tick(Some((2, 4)), t0, &c, g);
            rig.tick(Some((2, 5)), ms(t0, 60), &c, g);
            rig.tick(Some((2, 5)), ms(t0, 76), &c, g);
            let mut px = vec![0u32; g.win_w as usize * g.win_h as usize];
            for q in &rig.out {
                for y in q.y..q.y + q.h {
                    for x in q.x..q.x + q.w {
                        let p = &mut px[usize::from(y) * g.win_w as usize + usize::from(x)];
                        *p = over_premul(*p, q.color, q.alpha);
                    }
                }
            }
            px
        };
        let (with, without) = (lay(0.6), lay(0.0));
        with.iter()
            .zip(&without)
            .map(|(&a, &b)| {
                [16u32, 8, 0]
                    .iter()
                    .map(|&sh| ((a >> sh) & 0xff).saturating_sub((b >> sh) & 0xff))
                    .sum()
            })
            .collect()
    };
    let stats = |light: &[u32]| -> (f64, i32) {
        let (mut sum, mut weight, mut right) = (0.0f64, 0.0f64, i32::MIN);
        for (i, &v) in light.iter().enumerate() {
            if v == 0 {
                continue;
            }
            let x = (i % g.win_w as usize) as i32;
            sum += f64::from(v) * (f64::from(x) + 0.5);
            weight += f64::from(v);
            right = right.max(x);
        }
        assert!(weight > 0.0, "fixture: the crown lit");
        (sum / weight, right)
    };
    let (x0, cw) = (5 * g.cw as i32, g.cw as i32);
    let grow = (0.6 * g.ch as f32) as i32;
    let bw = 2;
    let (centre, right) = stats(&crown(0.08, false));
    assert!(
        (centre - (f64::from(x0) + f64::from(bw) / 2.0)).abs() <= 1.0,
        "behind a bar the box blooms at x {centre:.1}, not round the bar at {}",
        x0 + bw / 2
    );
    assert!(
        right < x0 + bw + grow,
        "behind a bar the box's light reaches x {right}, past the bar's bloom at {}",
        x0 + bw + grow
    );
    let (centre, _) = stats(&crown(0.5, false));
    assert!(
        (centre - f64::from(x0 + cw / 2)).abs() <= 1.0,
        "under a block the box blooms at x {centre:.1}, not round the cell at {}",
        x0 + cw / 2
    );
    let (centre, _) = stats(&crown(0.08, true));
    assert!(
        (centre - f64::from(x0 + cw / 2)).abs() <= 1.0,
        "negative control: the cell-wide box blooms at x {centre:.1}, not the cell's centre"
    );
}

#[test]
fn the_landing_ring_stroke_scales_with_the_cell() {
    let t0 = Instant::now();
    for (ch, th) in [(16usize, 2u16), (23, 2), (32, 4)] {
        let g = Geom {
            ch,
            win_h: (6 * ch) as u16,
            ..geom()
        };
        let c = GlowConfig {
            radius: 0.0,
            ..cfg()
        };
        let mut rig = Rig::default();
        rig.tick(Some((1, 4)), t0, &c, g);
        rig.tick(Some((4, 30)), ms(t0, 16), &c, g);
        let (cx, _) = g.cell_center(4, 30);
        let s = (0.6 * ch as f32) as i32;
        let left = (cx as i32 - s) as u16;
        assert!(
            rig.out.iter().any(|q| q.x == left && q.w == th),
            "ch {ch}: the ring's left bar is {th} px wide"
        );
    }
}

#[test]
fn the_wake_decays_to_exactly_empty_and_disarms() {
    let t0 = Instant::now();
    for c in [cfg(), light_cfg()] {
        let g = geom();
        let mut rig = Rig::default();
        rig.tick(Some((2, 4)), t0, &c, g);
        rig.tick(Some((4, 34)), ms(t0, 16), &c, g);
        assert!(rig.w.is_active());
        let fp = rig.tick(Some((4, 34)), t0 + Duration::from_secs(4), &c, g);
        assert!(
            rig.out.is_empty() && rig.halos.is_empty(),
            "the wake decays to EXACTLY empty"
        );
        assert_eq!(fp, 0, "an empty frame fingerprints zero");
        assert!(!rig.w.is_active(), "and the animation timer disarms");
    }
}

#[test]
fn disabled_returns_the_engine_to_rest() {
    let (g, mut c) = (geom(), cfg());
    let mut rig = Rig::default();
    let t0 = Instant::now();
    rig.tick(Some((2, 4)), t0, &c, g);
    rig.tick(Some((4, 34)), ms(t0, 16), &c, g);
    assert!(rig.w.is_active());
    c.enabled = false;
    let fp = rig.tick(Some((4, 34)), ms(t0, 32), &c, g);
    assert_eq!(fp, 0);
    assert!(rig.out.is_empty());
    assert!(!rig.w.is_active(), "disabled leaves nothing in flight");
}

#[test]
fn the_mono_face_follows_the_configured_colour_and_the_spectrum_does_not() {
    let g = geom();
    let typed_on = |base: GlowConfig, mono: bool, color: u32, accent: u32| {
        let c = GlowConfig {
            classic_mono: mono,
            color,
            accent,
            ..base
        };
        let mut rig = Rig::default();
        let t0 = Instant::now();
        rig.tick(Some((2, 4)), t0, &c, g);
        rig.tick(Some((2, 5)), ms(t0, 16), &c, g);
        rig.out
            .iter()
            .chain(&rig.under)
            .map(|q| q.color)
            .collect::<Vec<_>>()
    };
    let typed = |mono: bool, color: u32, accent: u32| typed_on(cfg(), mono, color, accent);
    const GREEN: (u32, u32) = (0x0050_FA7B, 0x0078_FFB9);
    const AMBER: (u32, u32) = (0x00FF_A500, 0x00FF_D080);

    let mono_green = typed(true, GREEN.0, GREEN.1);
    assert_ne!(
        mono_green,
        typed(true, AMBER.0, AMBER.1),
        "the mono face follows the configured colour"
    );
    assert_eq!(
        typed(false, GREEN.0, GREEN.1),
        typed(false, AMBER.0, AMBER.1),
        "the spectrum face's comet and crown owe nothing to the theme"
    );
    assert_eq!(
        typed_on(light_cfg(), true, GREEN.0, GREEN.1).len(),
        typed_on(light_cfg(), false, GREEN.0, GREEN.1).len(),
        "the two faces differ in colour alone on a light ground"
    );
}

#[test]
fn the_shared_ramp_speaks_the_engines_spectrum() {
    for i in 0..=8 {
        let (hue, pos) = (i as f32 * 0.11, i as f32 / 8.0);
        assert_eq!(
            crate::cursor_glow::style_comet_color(GlowStyle::Classic, 1, 2, hue, pos),
            comet_color(false, 1, 2, hue, pos)
        );
    }
    assert_eq!(
        comet_color(true, 0x00FF_0000, 0x0000_00FF, 0.3, 0.0),
        0x0000_00FF
    );
    assert_eq!(
        comet_color(true, 0x00FF_0000, 0x0000_00FF, 0.3, 1.0),
        0x00FF_0000
    );
}

fn dark_session_fold(c: &GlowConfig, g: Geom) -> u64 {
    dark_session_fold_as(c, g, |_| {})
}

/// [`dark_session_fold`] of a wake its test seams set first.
fn dark_session_fold_as(c: &GlowConfig, g: Geom, seams: impl FnOnce(&mut ClassicWake)) -> u64 {
    use aterm_hash::fnv_mix_u64;
    let mut rig = Rig::default();
    seams(&mut rig.w);
    let t0 = Instant::now();
    let mut acc: u64 = 0;
    let mut frame = |rig: &mut Rig, cur: Option<(u16, u16)>, at: u64| {
        let fp = rig.tick(cur, ms(t0, at), c, g);
        assert!(
            rig.halos.iter().all(|h| h.mode == HaloMode::Add),
            "a dark ground lays no veils"
        );
        acc = fnv_mix_u64(acc, fp);
        acc = fnv_mix_u64(acc, rig.out.len() as u64);
        for q in &rig.out {
            for &word in q.damage_words().as_slice() {
                acc = fnv_mix_u64(acc, word);
            }
        }
        for h in &rig.halos {
            for word in h.damage_words() {
                acc = fnv_mix_u64(acc, word);
            }
        }
    };
    let mut at = 0;
    frame(&mut rig, Some((2, 4)), at);
    for k in 1..=8u16 {
        at += 60;
        frame(&mut rig, Some((2, 4 + k)), at);
        frame(&mut rig, Some((2, 4 + k)), at + 24);
    }
    at += 500;
    frame(&mut rig, Some((2, 13)), at);
    at += 80;
    frame(&mut rig, Some((2, 12)), at);
    at += 100;
    frame(&mut rig, Some((3, 12)), at);
    at += 150;
    frame(&mut rig, Some((5, 30)), at);
    for dt in [16u64, 90, 200] {
        frame(&mut rig, Some((5, 30)), at + dt);
    }
    at += 400;
    frame(&mut rig, Some((5, 3)), at);
    frame(&mut rig, Some((5, 3)), at + 40);
    at += 300;
    frame(&mut rig, Some((0, 20)), at);
    frame(&mut rig, Some((0, 20)), at + 70);
    at += 100;
    frame(&mut rig, None, at);
    at += 30;
    frame(&mut rig, Some((0, 21)), at);
    frame(&mut rig, Some((0, 21)), at + 4000);
    acc
}

#[test]
fn the_mono_crown_is_one_smooth_round_bloom() {
    let g = geom();
    let mono = GlowConfig {
        classic_mono: true,
        ..cfg()
    };
    let column = |c: &GlowConfig| -> (Vec<i32>, usize) {
        let mut rig = Rig::default();
        let t0 = Instant::now();
        for k in 0..=8u16 {
            rig.tick(Some((2, 4 + k)), ms(t0, 50 * u64::from(k)), c, g);
        }
        rig.tick(Some((2, 12)), ms(t0, 420), c, g);
        let mut px = vec![0u32; g.win_w as usize * g.win_h as usize];
        for q in &rig.out {
            for y in q.y..q.y + q.h {
                for x in q.x..q.x + q.w {
                    let p = &mut px[usize::from(y) * g.win_w as usize + usize::from(x)];
                    *p = over_premul(*p, q.color, q.alpha);
                }
            }
        }
        for h in &rig.halos {
            let (cx, cy) = (i32::from(h.cx), i32::from(h.cy));
            let (rx2, ry2) = (i32::from(h.rx).pow(2), i32::from(h.ry).pow(2));
            for y in h.y..h.y + h.h {
                let ny = aterm_render::halo_row_ny(i32::from(y) - cy, ry2);
                if ny >= 256 {
                    continue;
                }
                for x in h.x..h.x + h.w {
                    let wt = aterm_render::halo_weight(i32::from(x) - cx, ny, rx2).min(255);
                    let p = &mut px[usize::from(y) * g.win_w as usize + usize::from(x)];
                    *p = aterm_render::add_sat(*p, aterm_render::premul_rgb(h.color, wt as u8));
                }
            }
        }
        let x = 12 * g.cw + g.cw / 2;
        let bottom = 3 * g.ch;
        let line = (bottom..bottom + 2 * g.ch)
            .map(|y| {
                let p = px[y * g.win_w as usize + x];
                (((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff)) as i32
            })
            .collect();
        (line, rig.halos.len())
    };
    let steepest = |l: &[i32]| {
        let peak = l.iter().copied().max().unwrap_or(0).max(1);
        let step = l.windows(2).map(|w| (w[1] - w[0]).abs()).max().unwrap_or(0);
        step as f32 / peak as f32
    };
    let (round, halos) = column(&mono);
    assert!(halos > 0, "the mono crown is radial halos");
    assert!(
        round.iter().any(|&v| v > 0),
        "the bloom reaches past the caret's cell"
    );
    assert!(
        steepest(&round) <= 0.35,
        "the bloom steps by {:.2} of its light in one pixel: {round:?}",
        steepest(&round)
    );
    let (boxed, _) = column(&cfg());
    assert!(
        steepest(&boxed) > 0.35,
        "control: the box crown steps by only {:.2}: {boxed:?}",
        steepest(&boxed)
    );
}

/// The SPECTRUM wake on dark is v0.28's, byte for byte
/// at the shipped duration
/// (`the_host_resolved_dark_wake_is_byte_identical_to_its_base`). The
/// one-colour wake's fold was re-baked deliberately by looks8 K2, whose
/// landing ring lies under the letters, again by looks9 (Q4's whole ring,
/// Q11's one stroke up a page), and by looks10 (L6: a stroke up a page
/// runs from the old caret to the new); and all three folds by access r3
/// (composed O12(a)): this fixture's 240 ms is off the default, and a
/// typed spark now lives its tuned life times the duration's share of
/// the default, chained keys too. With access r3's seam set back the
/// three fold to their bases before it, with L6's seam set back too the
/// one-colour wake folds to looks9's value, and with looks9's two set
/// back too to looks8's, so each re-bake moves nothing else.
#[test]
fn the_dark_wake_is_byte_identical_to_its_base() {
    let mono = GlowConfig {
        classic_mono: true,
        ..cfg()
    };
    let padded = Geom {
        cw: 11,
        ch: 23,
        rows: 6,
        cols: 40,
        origin_x: 8,
        origin_y: 56,
        win_w: 456,
        win_h: 202,
        head: 48,
    };
    let got = [
        dark_session_fold(&cfg(), geom()),
        dark_session_fold(&mono, geom()),
        dark_session_fold(&cfg(), padded),
    ];
    assert_eq!(
        got,
        [
            2_703_624_080_531_407_634,
            5_821_303_035_924_665_918,
            1_226_402_589_320_791_053,
        ],
        "the dark classic wake drifted from its base"
    );
    let unscaled = |w: &mut ClassicWake| w.typed_life_unscaled = true;
    assert_eq!(
        [
            dark_session_fold_as(&cfg(), geom(), unscaled),
            dark_session_fold_as(&mono, geom(), unscaled),
            dark_session_fold_as(&cfg(), padded, unscaled),
        ],
        [
            11_293_809_440_103_835_944,
            2_735_246_743_558_469_609,
            8_579_084_186_203_313_172,
        ],
        "control: the wake with access r3's seam set back is its base before it"
    );
    let looks9 = dark_session_fold_as(&mono, geom(), |w| {
        w.typed_life_unscaled = true;
        w.tracer_up_stretch = true;
    });
    assert_eq!(
        looks9, 15_572_736_309_831_782_819,
        "control: the one-colour wake with looks10's seam set back is looks9's"
    );
    let looks8 = dark_session_fold_as(&mono, geom(), |w| {
        w.typed_life_unscaled = true;
        w.ring_cut = true;
        w.tracer_up_kept = true;
        w.tracer_up_stretch = true;
    });
    assert_eq!(
        looks8, 11_956_472_567_039_891_853,
        "control: the one-colour wake with looks9's seams set back is looks8's"
    );
    let bar = GlowConfig {
        head_dx: 0.08,
        ..cfg()
    };
    assert_ne!(
        dark_session_fold(&bar, geom()),
        got[0],
        "control: the pin must be able to see the comet move"
    );
}

const fn shipped_dark_at(length: usize, intensity: f32) -> GlowConfig {
    crate::cursor_fx::resolve_glow_config(
        crate::cursor_fx::GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length,
            intensity,
            radius: 0.6,
            ring: true,
        },
        crate::cursor_fx::GlowSpelling::parse("classic"),
        0x0050_FA7B,
        true,
        0x00D0_D0D0,
        0x0011_1318,
        0.5,
    )
}

const fn shipped_dark(length: usize) -> GlowConfig {
    shipped_dark_at(
        length,
        crate::cursor_fx::default_trail_intensity(Some(GlowStyle::Classic), false),
    )
}

#[test]
fn the_host_resolved_dark_wake_is_byte_identical_to_its_base() {
    let shipped = shipped_dark(24);
    assert_eq!(
        (
            shipped.style,
            shipped.enabled,
            shipped.duration,
            shipped.length,
            shipped.intensity,
            shipped.radius,
            shipped.ring,
            shipped.classic_mono,
            shipped.dark_theme,
        ),
        (
            GlowStyle::Classic,
            true,
            Duration::from_millis(260),
            24,
            cfg().intensity,
            0.6,
            true,
            false,
            true,
        ),
        "the config law changed a knob Classic reads"
    );
    let padded = Geom {
        cw: 11,
        ch: 23,
        rows: 6,
        cols: 40,
        origin_x: 8,
        origin_y: 56,
        win_w: 456,
        win_h: 202,
        head: 48,
    };
    let got = [
        dark_session_fold(&shipped, geom()),
        dark_session_fold(&shipped, padded),
    ];
    assert_eq!(
        got,
        [8_976_404_549_136_510_798, 10_432_365_287_462_349_185],
        "the host-resolved dark classic wake drifted from its base"
    );
    assert_ne!(
        dark_session_fold(&shipped_dark(18), geom()),
        got[0],
        "control: a resized comet must fold to another print"
    );
    assert_ne!(
        dark_session_fold(&shipped_dark_at(24, 1.0), geom()),
        got[0],
        "control: the full brightness must fold to another print"
    );
}

/// THE PAPER SPECTRUM LAYS NO OLIVE (looks8 K10): Classic's spectrum face
/// on paper darkens each colour to the paper's bar, and its yellows
/// darkened read olive and khaki (the `enter-bottom` Return's tracer).
/// Through a jump at every twelfth of the hue wheel, no stroke or crown
/// quad on paper wears a hue between 45 and 80 degrees. Negative
/// control: walking the whole wheel, some do.
#[test]
fn the_paper_spectrum_lays_no_olive() {
    let (g, c) = (geom(), light_cfg());
    let olive = |muddy: bool| {
        let mut n = 0;
        for k in 0..12 {
            let t0 = Instant::now();
            let mut rig = Rig::default();
            rig.w.paper_muddy = muddy;
            rig.tick(Some((1, 4)), t0, &c, g);
            rig.w.hue = k as f32 / 12.0;
            rig.tick(Some((4, 30)), ms(t0, 16), &c, g);
            // Each end's colour, its premultiplied light over its opacity.
            let own = |rgb: u32, a: u8| {
                let ch = |sh: u32| ((((rgb >> sh) & 0xFF) * 255) / u32::from(a.max(1))).min(255);
                (ch(16) << 16) | (ch(8) << 8) | ch(0)
            };
            n += rig
                .under
                .iter()
                .flat_map(|q| [(q.color, q.alpha), (q.color2, q.alpha2)])
                .filter(|&(rgb, a)| {
                    let (h, s, _) = crate::color_math::rgb2hsv(own(rgb, a));
                    a >= 24 && s > 0.3 && (45.0..80.0).contains(&h)
                })
                .count();
        }
        n
    };
    assert!(
        olive(true) > 0,
        "control: the whole wheel lays olive on paper"
    );
    assert_eq!(olive(false), 0, "the paper spectrum lays olive");
}

/// A CHOSEN COLOUR'S BED KEEPS NEAR A LIT PAGE UNDER ITS LETTERS (looks8
/// round 2's K10): on Solarized Dark the one-colour face's bed under the
/// letters lies at the page's luminance in its own hue, and a colour far
/// round the wheel from the page's (`#d03a8c` on the teal) laid a maroon
/// smear the letters' edges blended over and read under their bar (`f`
/// at 3.93:1 of its own 4.06). Under a lettered row a leap's bed keeps
/// within half the distance from the page the whole hue took. Negative
/// control: the whole hue.
#[test]
fn a_chosen_colour_s_bed_keeps_near_a_lit_page_under_its_letters() {
    let g = geom();
    let c = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        theme_fg: 0x0083_9496,
        theme_bg: 0x0000_2B36,
        ..cfg()
    };
    let lines = [(2, 0, 30)];
    let ch = g.ch as i32;
    let reach = |whole: bool| {
        let t0 = Instant::now();
        let mut rig = Rig {
            spans: crate::cursor_glow::TextSpans::of_lines(&lines),
            ..Rig::default()
        };
        rig.w.mono_lit_whole = whole;
        rig.tick(Some((2, 2)), t0, &c, g);
        rig.tick(Some((2, 28)), ms(t0, 16), &c, g);
        let own = |rgb: u32, a: u8| {
            let ch = |sh: u32| ((((rgb >> sh) & 0xFF) * 255) / u32::from(a.max(1))).min(255);
            (ch(16) << 16) | (ch(8) << 8) | ch(0)
        };
        let far = |rgb: u32| {
            [16u32, 8, 0]
                .iter()
                .map(|&sh| (((rgb >> sh) & 0xFF) as i32 - ((c.theme_bg >> sh) & 0xFF) as i32).abs())
                .max()
                .unwrap_or(0)
        };
        rig.under
            .iter()
            .filter(|q| {
                let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
                y0 >= 2 * ch + ch / 4 && y1 <= 3 * ch - ch / 4
            })
            .flat_map(|q| [(q.color, q.alpha), (q.color2, q.alpha2)])
            .filter(|&(_, a)| a >= 64)
            .map(|(rgb, a)| far(own(rgb, a)))
            .max()
            .unwrap_or(0)
    };
    let (kept, whole) = (reach(false), reach(true));
    assert!(
        whole >= 20,
        "control: the whole hue lies {whole} from the page"
    );
    assert!(
        2 * kept <= whole,
        "under the letters the bed lies {kept} from the page, the whole hue {whole}"
    );
}

/// A CHOSEN COLOUR'S BED KEEPS ITS HUE ON A TINTED PAGE (composed r2, O4(b); the pin
/// ruling): under a lit page's letters the one-colour face's bed keeps
/// [`ClassicWake::MONO_LIT_BED_SHARE`] of its hue, and the rest was Solarized Dark's teal, so
/// the pink pin `#d03a8c` (hue 327) laid its bed blue (hue 222). On a tinted page whose
/// light lays a night ground the rest is that ground's ink, the page's neutral
/// ([`WakeInk::bed_ground`], [`crate::cursor_glow::GroundRule::NeutralCore`], composed r3):
/// every bed mark under the lettered row wears the pin's hue within 25 degrees, at the page's
/// luminance within 1.2:1. Negative control (no ground, the page): some mark lies past 60
/// degrees off.
#[test]
fn a_chosen_colour_s_bed_keeps_its_hue_on_a_tinted_page() {
    let g = geom();
    let c = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        theme_fg: 0x0083_9496,
        theme_bg: 0x0000_2B36,
        ..cfg()
    };
    let ground = crate::cursor_glow::night_ground_holding(
        c.theme_bg,
        &[c.color, c.accent, 0x00FF_FFFF],
        crate::cursor_glow::GroundRule::NeutralCore,
    )
    .expect("fixture: Solarized Dark solves a ground for the pink pin")
    .ink;
    let ch = g.ch as i32;
    let worst = |bed_ground: Option<u32>| -> (f32, f32) {
        let t0 = Instant::now();
        let mut rig = Rig {
            spans: crate::cursor_glow::TextSpans::of_lines(&[(2, 0, 30)]),
            bed_ground,
            ..Rig::default()
        };
        rig.tick(Some((2, 2)), t0, &c, g);
        rig.tick(Some((2, 28)), ms(t0, 16), &c, g);
        let own = |rgb: u32, a: u8| {
            let ch = |sh: u32| ((((rgb >> sh) & 0xFF) * 255) / u32::from(a.max(1))).min(255);
            (ch(16) << 16) | (ch(8) << 8) | ch(0)
        };
        let (want, _, _) = crate::color_math::rgb2hsv(c.color);
        let page = crate::color_math::relative_luminance(c.theme_bg);
        let marks: Vec<u32> = rig
            .under
            .iter()
            .filter(|q| {
                let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
                q.alpha > 0 && y0 >= 2 * ch + ch / 4 && y1 <= 3 * ch - ch / 4
            })
            .flat_map(|q| [(q.color, q.alpha), (q.color2, q.alpha2)])
            .filter(|&(_, a)| a >= 64)
            .map(|(rgb, a)| own(rgb, a))
            .collect();
        assert!(
            !marks.is_empty(),
            "fixture: a leap lays its bed under the row"
        );
        marks.iter().fold((0.0f32, 1.0f32), |(gap, ratio), &m| {
            let (h, _, _) = crate::color_math::rgb2hsv(m);
            let d = (h - want).abs();
            let l = crate::color_math::relative_luminance(m);
            (
                gap.max(d.min(360.0 - d)),
                ratio.max((l.max(page) + 0.05) / (l.min(page) + 0.05)),
            )
        })
    };
    let (gap, ratio) = worst(Some(ground));
    assert!(
        gap <= 25.0 && ratio <= 1.2,
        "the bed under the letters lies {gap:.0} degrees off the pin, {ratio:.2}:1 off the page"
    );
    let (gap, _) = worst(None);
    assert!(
        gap > 60.0,
        "control: mixed toward the teal, the bed lies only {gap:.0} degrees off"
    );
}

#[test]
fn a_light_ground_draws_ink_under_the_letters_and_a_veil_ring_and_no_additive_light() {
    let (g, c) = (geom(), light_cfg());
    let t0 = Instant::now();
    let mut rig = Rig::default();
    rig.tick(Some((1, 4)), t0, &c, g);
    let fp = rig.tick(Some((4, 30)), ms(t0, 16), &c, g);
    // The ring's own square, laid in ink over the paper (r17 CA4: the veil alone changed
    // some 20 px of a card by more than 32 levels).
    assert!(!rig.out.is_empty(), "a light-ground landing lays its ring");
    let (rx, ry) = g.cell_center(4, 30);
    let s = (0.6 * g.ch as f32) as i32 + 1;
    for q in &rig.out {
        assert!(
            q.alpha > 0 && f32::from(q.alpha) <= LIGHT_INK_ALPHA_CAP,
            "a light ground emits no additive quad, and its ring stays under the over-text \
             ceiling: {q:?}"
        );
        let (x0, y0) = (i32::from(q.x), i32::from(q.y));
        let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
        assert!(
            x0 >= rx as i32 - s - 1
                && x1 <= rx as i32 + s + 1
                && y0 >= ry as i32 - s - 1
                && y1 <= ry as i32 + s + 1,
            "the ring's square stands about the landing: {q:?}"
        );
    }
    assert!(!rig.under.is_empty(), "a light-ground jump is visible");
    assert_ne!(fp, 0, "a frame of ink and veils alone still fingerprints");
    for q in &rig.under {
        assert!(
            q.alpha > 0 && q.alpha2 > 0,
            "the stroke is source-over: {q:?}"
        );
        assert!(
            f32::from(q.alpha.max(q.alpha2)) <= LIGHT_BODY_ALPHA_CAP,
            "the stroke stays under the body cap: {q:?}"
        );
    }
    let ring = g.cell_center(4, 30);
    assert!(
        rig.halos.iter().all(|h| h.mode == HaloMode::Over),
        "every veil is source-over"
    );
    assert!(
        rig.halos.iter().any(
            |h| (h.cx, h.cy) == (ring.0.round() as u16, ring.1.round() as u16)
                && halo_over_cap(h.color) <= 120
        ),
        "the landing ring is a capped veil on the landing cell"
    );
}

#[test]
fn the_paper_landing_veil_stays_in_the_caret_row_at_the_text_bar() {
    let g = geom();
    let caret = (4u16, 12u16);
    let (ccx, _) = g.cell_center(caret.0, caret.1);
    let reach = CursorGlow::LIGHT_VEIL_REACH_CELLS * g.cw as f32 + 1.0;
    let (row_top, row_bot) = (
        i32::from(caret.0) * g.ch as i32,
        i32::from(caret.0 + 1) * g.ch as i32,
    );
    let (mut escapes, mut drawn) = (0usize, 0usize);
    for (fg, bg) in [(INK, PAPER), (0x0033_3333, 0x00FA_F8F5)] {
        for accent in [light_cfg().accent, 0x00D0_3A8C] {
            let c = GlowConfig {
                theme_fg: fg,
                theme_bg: bg,
                accent,
                color: accent,
                ..light_cfg()
            };
            for unbound in [false, true] {
                let born = Instant::now();
                let mut w = ClassicWake {
                    ring: Some(Ring {
                        row: caret.0 - 1,
                        col: caret.1,
                        dx: 0.0,
                        born,
                        life: 0.4,
                        caret,
                        mono: MonoRing::Unjudged,
                    }),
                    veil_unbound: unbound,
                    ..ClassicWake::default()
                };
                for (at, cur) in [(30u64, Some(caret)), (120, Some(caret)), (60, None)] {
                    let mut halos = Vec::new();
                    w.emit_light_ring(ms(born, at), &c, g, cur, &mut halos);
                    for h in &halos {
                        let inside = i32::from(h.y) >= row_top
                            && i32::from(h.y) + i32::from(h.h) <= row_bot
                            && (f32::from(h.cx) - ccx).abs() <= 1.0
                            && f32::from(h.rx) <= reach;
                        if unbound {
                            escapes += usize::from(!inside);
                            continue;
                        }
                        drawn += 1;
                        assert!(
                            inside,
                            "{accent:06x} on {bg:06x}, {at} ms, caret {cur:?}: the veil \
                             {h:?} leaves the caret's cell and row"
                        );
                    }
                    if unbound {
                        continue;
                    }
                    let (cx, cy) = (halos[0].cx, halos[0].cy);
                    let under = halos
                        .iter()
                        .filter(|h| {
                            (h.x..h.x + h.w).contains(&cx) && (h.y..h.y + h.h).contains(&cy)
                        })
                        .fold(bg, |px, h| {
                            let a = u32::from(halo_over_cap(h.color));
                            let veil = h.color & 0x00FF_FFFF;
                            let mix = |sh: u32| {
                                (((veil >> sh) & 0xff) * a + ((px >> sh) & 0xff) * (255 - a) + 127)
                                    / 255
                            };
                            (mix(16) << 16) | (mix(8) << 8) | mix(0)
                        });
                    let k = crate::color_math::contrast_ratio(fg, under);
                    assert!(
                        k >= 4.5,
                        "{accent:06x} on {bg:06x}, {at} ms: the text over the veil's centre \
                         reads {k:.2}:1"
                    );
                }
            }
        }
    }
    assert!(drawn > 0, "the paper landing lays no veil");
    assert!(
        escapes > 0,
        "control: the unbound veil stays in the caret's row"
    );
}

#[allow(clippy::type_complexity)]
fn light_frame(
    under: &[GlowQuad],
    g: Geom,
    row: u16,
    cols: std::ops::Range<u16>,
    times: usize,
) -> (Vec<u32>, Vec<(Vec<usize>, Vec<usize>)>) {
    let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
    let mut px = vec![PAPER; w * h];
    for _ in 0..times {
        for q in under {
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                    px[y * w + x] = over_premul(px[y * w + x], q.color, q.alpha) & 0x00FF_FFFF;
                }
            }
        }
    }
    let mut cells = Vec::new();
    for col in cols {
        let (x0, y0) = (usize::from(col) * g.cw, usize::from(row) * g.ch);
        for y in y0 + 1..y0 + g.ch - 1 {
            let (mut stems, mut counters) = (Vec::new(), Vec::new());
            for x in x0 + 1..x0 + g.cw - 1 {
                if matches!(x - x0, 2 | 5) {
                    stems.push(y * w + x);
                } else {
                    counters.push(y * w + x);
                }
            }
            for &i in &stems {
                px[i] = INK;
            }
            cells.push((stems, counters));
        }
    }
    (px, cells)
}

fn ratio(px: &[u32], a: &[usize], b: &[usize]) -> f32 {
    let mean = |set: &[usize]| {
        set.iter().map(|&i| relative_luminance(px[i])).sum::<f32>() / set.len() as f32
    };
    let (la, lb) = (mean(a), mean(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[test]
fn a_typed_run_on_light_is_an_underline_inside_its_row_that_keeps_the_letters_legible() {
    let (g, c) = (geom(), light_cfg());
    let crownless = GlowConfig { radius: 0.0, ..c };
    let t0 = Instant::now();
    let mut rig = Rig::default();
    let mut stroke = Rig::default();
    let row = 2u16;
    let (mut worst, mut worst_stacked, mut drawn) = (f32::MAX, f32::MAX, 0usize);
    let mut vivid = Rig::default();
    vivid.w.light_unsolved = true;
    let mut worst_vivid = f32::MAX;
    vivid.tick(Some((row, 3)), t0, &c, g);
    rig.tick(Some((row, 3)), t0, &c, g);
    stroke.tick(Some((row, 3)), t0, &crownless, g);
    for col in 4..=30u16 {
        rig.tick(Some((row, col)), ms(t0, 40 * u64::from(col)), &c, g);
        assert!(
            rig.out.is_empty() && !rig.under.is_empty(),
            "col {col}: ink only"
        );
        drawn += rig.under.len();
        stroke.tick(Some((row, col)), ms(t0, 40 * u64::from(col)), &crownless, g);
        let (top, bot) = (usize::from(row) * g.ch, usize::from(row + 1) * g.ch);
        for q in &rig.under {
            assert!(
                usize::from(q.y) >= top && usize::from(q.y) + usize::from(q.h) <= bot,
                "col {col}: the wake leaves the typed row: {q:?}"
            );
        }
        for q in &stroke.under {
            assert!(
                usize::from(q.y) >= top + g.ch / 2 && usize::from(q.y) + usize::from(q.h) <= bot,
                "col {col}: the stroke leaves the row's lower band: {q:?}"
            );
        }
        let typed = 3..col;
        let (px, cells) = light_frame(&rig.under, g, row, typed.clone(), 1);
        for (stems, counters) in &cells {
            worst = worst.min(ratio(&px, stems, counters));
        }
        let (px, cells) = light_frame(&rig.under, g, row, typed.clone(), 3);
        for (stems, counters) in &cells {
            worst_stacked = worst_stacked.min(ratio(&px, stems, counters));
        }
        vivid.tick(Some((row, col)), ms(t0, 40 * u64::from(col)), &c, g);
        let (px, cells) = light_frame(&vivid.under, g, row, typed, 1);
        for (stems, counters) in &cells {
            worst_vivid = worst_vivid.min(ratio(&px, stems, counters));
        }
    }
    assert!(drawn > 50, "fixture: the session drew strokes ({drawn})");
    assert!(worst >= 4.5, "the underline buries a letter: {worst:.2}:1");
    assert!(
        worst_stacked >= 4.5,
        "laid three deep the underline buries a letter: {worst_stacked:.2}:1"
    );
    assert!(
        worst_vivid < 4.5,
        "control: the unsolved ink must fail the bar ({worst_vivid:.2}:1)"
    );
}

#[test]
fn the_light_crown_is_one_round_ink_mark_on_the_caret_row() {
    let (g, c) = (geom(), light_cfg());
    let crownless = GlowConfig { radius: 0.0, ..c };
    let t0 = Instant::now();
    let (mut rig, mut twin) = (Rig::default(), Rig::default());
    let row = 2u16;
    let mut at = 0;
    let crown_of = |rig: &mut Rig, twin: &mut Rig, col: u16, at: u64| {
        rig.tick(Some((row, col)), ms(t0, at), &c, g);
        twin.tick(Some((row, col)), ms(t0, at), &crownless, g);
        let n = rig.under.len() - twin.under.len();
        assert_eq!(
            rig.under[n..],
            twin.under[..],
            "col {col}: the stroke over the crown is the crownless stroke"
        );
        rig.under[..n].to_vec()
    };
    crown_of(&mut rig, &mut twin, 3, at);
    let mut hot = Vec::new();
    for col in 4..=14u16 {
        at += 40;
        hot = crown_of(&mut rig, &mut twin, col, at);
    }
    assert!(!hot.is_empty(), "a hot typed run wears its crown");
    let (cx, _) = g.cell_center(row, 14);
    let (top, bot) = (row * g.ch as u16, (row + 1) * g.ch as u16);
    assert!(
        !twin.under.is_empty() && twin.under.iter().all(|q| q.y >= top + g.ch as u16 / 2),
        "control: at Glow size 0 the wake lays its stroke and no crown"
    );
    let (mut left, mut right, mut peak_at, mut peak) = (f32::MAX, f32::MIN, 0.0, 0u8);
    for q in &hot {
        assert!(q.alpha > 0 && q.alpha2 > 0, "source-over ink: {q:?}");
        assert!(
            f32::from(q.alpha.max(q.alpha2)) <= LIGHT_BODY_ALPHA_CAP,
            "under the body cap: {q:?}"
        );
        assert!(
            q.y >= top && q.y + q.h <= bot,
            "the crown stays on the caret's row: {q:?}"
        );
        left = left.min(f32::from(q.x));
        right = right.max(f32::from(q.x + q.w));
        if q.alpha.max(q.alpha2) > peak {
            peak = q.alpha.max(q.alpha2);
            peak_at = f32::from(q.x) + f32::from(q.w) * 0.5;
        }
    }
    let half = g.cw as f32 * 0.5;
    let reach = half + c.radius * g.ch as f32 * 0.9;
    for (side, edge) in [("left", cx - left), ("right", right - cx)] {
        assert!(
            edge > half + 0.5 * (reach - half) && edge <= reach + 2.0,
            "the {side} edge sits {edge:.1} px from the caret's centre, reach {reach:.1}"
        );
    }
    assert!(
        ((cx - left) - (right - cx)).abs() <= 2.0,
        "round about the caret: {left:.1}..{right:.1} around {cx:.1}"
    );
    assert!(
        (peak_at - cx).abs() <= g.cw as f32 * 0.5,
        "deepest on the caret's cell, at {peak_at:.1} (caret {cx:.1})"
    );
    at += 4_000;
    crown_of(&mut rig, &mut twin, 14, at);
    at += 40;
    let cool = crown_of(&mut rig, &mut twin, 15, at);
    let deepest = |quads: &[GlowQuad]| quads.iter().map(|q| q.alpha.max(q.alpha2)).max();
    assert!(
        deepest(&cool).is_some_and(|cool| cool < peak),
        "a cool key inks a fainter crown: {:?} vs {peak}",
        deepest(&cool)
    );
    at += ClassicWake::CROWN_TYPING_MS + 1;
    assert!(
        crown_of(&mut rig, &mut twin, 15, at).is_empty(),
        "the crown ends with its window"
    );
}

#[test]
fn a_vertical_jump_on_light_is_one_connected_stroke() {
    let (g, c) = (geom(), light_cfg());
    let t0 = Instant::now();
    let mut rig = Rig::default();
    let (from, to) = ((5u16, 6u16), (0u16, 6u16));
    rig.tick(Some(from), t0, &c, g);
    rig.tick(Some(to), ms(t0, 16), &c, g);
    assert!(!rig.under.is_empty(), "the jump is drawn");
    let x = g.cell_center(to.0, to.1).0.round() as u16;
    let drop = g.ch as f32 * ClassicWake::LIGHT_STROKE_DROP;
    let y_top = (g.cell_center(to.0, to.1).1 + drop).round() as u16;
    let y_bot = (g.cell_center(from.0, from.1).1 + drop).round() as u16 - 1;
    let gaps = |quads: &[GlowQuad]| {
        (y_top..y_bot)
            .filter(|&y| {
                !quads
                    .iter()
                    .any(|q| q.y <= y && y < q.y + q.h && q.x <= x && x < q.x + q.w)
            })
            .count()
    };
    assert_eq!(gaps(&rig.under), 0, "the stroke joins every row it crosses");
    let (cw, ch) = (g.cw as u16, g.ch as u16);
    let reach = (f32::from(ch) * 0.22) as u16;
    let rail_quads: Vec<GlowQuad> = (to.0..=from.0)
        .flat_map(|row| {
            let (x0, y0) = (to.1 * cw, row * ch);
            [y0, y0 + ch - reach].map(|y| GlowQuad::flat(row, x0, y, cw, reach, c.color, 200))
        })
        .collect();
    assert!(
        gaps(&rail_quads) > 0,
        "control: per-cell rails leave gaps between rows"
    );
}

/// looks7 M8: on paper an Enter's tracer ran straight from the old
/// line's end to the new line's start at the baseline's drop, across the
/// printed line's letters: a strike-through through `brown fox` in the
/// gallery's `enter-bottom`; passed behind each lettered row, it broke
/// into a stub at the caret and a dash floating past the printed line's
/// end (round 2). It keeps to the caret's row, for an Enter (a run from
/// above) and for a move up a page across lettered rows (a run from
/// below, round 3's review: eased in from the row's top alone it was a
/// full stub cut at the row's bottom): no pixel of another row takes its
/// ink, the caret's row shows it, and it fades in from the edge it
/// enters by, no row of pixels lifting by more than half its brightest
/// toward the caret, and walking its tail away from the caret no column
/// drops by more than a quarter of its brightest. Laid across the rows
/// (the first negative control) it strikes the lines it crosses; faded
/// over a pixel (the second) it is cut at the row's edge. Across rows
/// clear of letters it stays one stroke
/// (`a_vertical_jump_on_light_is_one_connected_stroke`). A move up now
/// keeps its longest clear stretch instead
/// (`a_paper_move_up_draws_one_stroke_off_the_letters`); kept to the
/// caret's row (`tracer_up_kept`), it holds these laws.
#[test]
fn a_paper_enter_s_tracer_keeps_to_the_caret_s_row() {
    let (g, c) = (geom(), light_cfg());
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    // The echo on row 2 to column 26, the printed line on row 3 to 19.
    let lines = [(2, 0, 26), (3, 0, 19)];
    let held = |x: i32, y: i32| {
        let (row, col) = (y.div_euclid(ch), x.div_euclid(cw));
        lines
            .iter()
            .any(|&(r, first, end)| r == row && col >= first && col <= end)
    };
    struct Laid {
        struck: usize,
        off_row: usize,
        shown: usize,
        lift: f32,
        drop: f32,
    }
    // An Enter from the echo's end to the next prompt (from above), and
    // a move up from below the printed line across both lines.
    let enter = ((2u16, 26u16), (4u16, 2u16));
    let up = ((4u16, 20u16), (1u16, 2u16));
    let laid = |(from, to): ((u16, u16), (u16, u16)),
                over: bool,
                leave: Option<f32>,
                top_only: bool|
     -> Laid {
        let t0 = Instant::now();
        let mut rig = Rig {
            spans: crate::cursor_glow::TextSpans::of_lines(&lines),
            ..Rig::default()
        };
        rig.w.tracer_over_letters = over;
        rig.w.tracer_leave_px = leave;
        rig.w.tracer_top_only = top_only;
        rig.w.tracer_up_kept = true;
        rig.tick(Some(from), t0, &c, g);
        let ww = usize::from(g.win_w);
        let caret_row = i32::from(to.0);
        let mut most = vec![0u32; ch as usize];
        let mut cols = vec![0u32; ww];
        let (mut struck, mut off_row, mut shown) = (0usize, 0usize, 0usize);
        for at in [16u64, 40, 80, 140] {
            rig.tick(Some(to), ms(t0, at), &c, g);
            let mut ink = vec![0u32; ww * usize::from(g.win_h)];
            for q in &rig.under {
                let w = u32::from(q.w.max(1));
                for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                    for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                        let i = (x - i32::from(q.x)) as u32;
                        let a = (u32::from(q.alpha) * (2 * w - (2 * i + 1))
                            + u32::from(q.alpha2) * (2 * i + 1)
                            + w)
                            / (2 * w);
                        let slot = &mut ink[y as usize * ww + x as usize];
                        *slot = (*slot).max(a);
                    }
                }
            }
            for (i, &a) in ink.iter().enumerate() {
                let (x, y) = ((i % ww) as i32, (i / ww) as i32);
                let row = y.div_euclid(ch);
                if row == caret_row {
                    let m = &mut most[y.rem_euclid(ch) as usize];
                    *m = (*m).max(a);
                    let m = &mut cols[x as usize];
                    *m = (*m).max(a);
                }
                if a <= 16 {
                    continue;
                }
                if held(x, y) {
                    struck += 1;
                }
                if row == caret_row {
                    shown += 1;
                } else {
                    off_row += 1;
                }
            }
        }
        let peak = most.iter().copied().max().unwrap_or(0).max(1) as f32;
        // Toward the caret from the edge the run enters by.
        if from.0 > to.0 {
            most.reverse();
        }
        let lift = most
            .windows(2)
            .map(|w| w[1].saturating_sub(w[0]) as f32 / peak)
            .fold(0.0, f32::max);
        // Along the tail, from the caret's column toward the one it left.
        let caret_x = (i32::from(to.1) * cw + cw / 2) as usize;
        let tail: Vec<u32> = if from.1 > to.1 {
            cols[caret_x..].to_vec()
        } else {
            cols[..=caret_x].iter().rev().copied().collect()
        };
        let drop = tail
            .windows(2)
            .map(|w| w[0].saturating_sub(w[1]) as f32 / peak)
            .fold(0.0, f32::max);
        Laid {
            struck,
            off_row,
            shown,
            lift,
            drop,
        }
    };
    for (name, run) in [("Enter", enter), ("up", up)] {
        let kept = laid(run, false, None, false);
        assert_eq!(
            (kept.struck, kept.off_row),
            (0, 0),
            "{name}: pixels of the lines the tracer crosses, and of the rows off the caret's, take its ink"
        );
        assert!(
            kept.shown > 0,
            "{name}: fixture: the tracer is drawn on the caret's row"
        );
        assert!(
            kept.drop <= 0.25,
            "{name}: the tracer's tail drops by {:.2} of its brightest from one column to the next",
            kept.drop
        );
        let over = laid(run, true, None, false);
        assert!(
            over.struck > 0 && over.off_row > 0,
            "{name}: control: laid across the rows, the tracer strikes nothing"
        );
    }
    // An Enter crosses most of the caret's row from its top: the rise is
    // gentle down the row too. A run from below crosses only the room
    // between the row's bottom and the caret's baseline, a few px, in
    // which its taper rises faster, so it is judged along its tail.
    let kept = laid(enter, false, None, false);
    assert!(
        kept.lift <= 0.5,
        "Enter: the tracer lifts by {:.2} of its brightest from one row of pixels to the next",
        kept.lift
    );
    let cut = laid(enter, false, Some(0.0), false);
    assert!(
        cut.lift > 0.5,
        "control: faded over a pixel, the Enter's tracer lifts by only {:.2}",
        cut.lift
    );
    let top_only = laid(up, false, None, true);
    assert!(
        top_only.drop > 0.25,
        "control: eased in from the row's top alone, the tail from below drops by only {:.2}",
        top_only.drop
    );
}

/// looks10 L6 (after looks9 Q11): on paper Classic and Classic pink drew a move up a page
/// that lands among letters as its longest stretch clear of them, a dash floating between
/// the old caret and the new, anchored to neither (`classic-light-move-up.up`). It runs from
/// the old caret to the new, through the lettered rows at a share of its light
/// ([`TRACER_UNDER_LETTERS_PAPER`]): each frame it is one stroke (one run of lit pixel columns),
/// it shows, and its light reaches within two px of the landing marks round the caret
/// (the letters it passes keep their bar by the letter guard). Negative control: kept to
/// its longest stretch, it ends six px or more short of the landing.
#[test]
fn a_paper_move_up_runs_to_the_caret() {
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    for c in [
        light_cfg(),
        GlowConfig {
            classic_mono: true,
            ..light_cfg()
        },
    ] {
        // The echo on row 2 to column 26, the printed line on row 3 to 19.
        let lines = [(2, 0, 26), (3, 0, 19)];
        // (lit px, most stroke runs in a frame, least gap to the landing in px)
        let laid = |stretch: bool| -> (usize, usize, i32) {
            let t0 = Instant::now();
            let mut rig = Rig {
                spans: crate::cursor_glow::TextSpans::of_lines(&lines),
                ..Rig::default()
            };
            rig.w.tracer_up_stretch = stretch;
            rig.tick(Some((4, 20)), t0, &c, g);
            let ww = usize::from(g.win_w);
            let (mut shown, mut runs, mut gap) = (0usize, 0usize, i32::MAX);
            for at in [16u64, 40, 80, 140] {
                rig.tick(Some((1, 2)), ms(t0, at), &c, g);
                let mut ink = vec![0u32; ww * usize::from(g.win_h)];
                for q in &rig.under {
                    for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                        for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                            let slot = &mut ink[y as usize * ww + x as usize];
                            *slot = (*slot).max(u32::from(q.alpha.max(q.alpha2)));
                        }
                    }
                }
                // The caret's own cell and the landing marks round it
                // are the landing's, not the tracer's.
                let (cx0, cy0) = (2 * cw - cw, ch - ch / 2);
                let (cx1, cy1) = (cx0 + 3 * cw - 1, cy0 + 2 * ch - 1);
                let landing = |x: i32, y: i32| (cx0..=cx1).contains(&x) && (cy0..=cy1).contains(&y);
                let mut lit_cols = vec![false; ww];
                for (i, &a) in ink.iter().enumerate() {
                    let (x, y) = ((i % ww) as i32, (i / ww) as i32);
                    if a <= 16 || landing(x, y) {
                        continue;
                    }
                    shown += 1;
                    gap = gap.min((cx0 - x).max(x - cx1).max(cy0 - y).max(y - cy1));
                    lit_cols[x as usize] = true;
                }
                let frame_runs = lit_cols.windows(2).filter(|w| w[1] && !w[0]).count()
                    + usize::from(lit_cols[0]);
                runs = runs.max(frame_runs);
            }
            (shown, runs, gap)
        };
        let mono = c.classic_mono;
        let (shown, runs, gap) = laid(false);
        assert!(shown >= 60, "mono {mono}: the tracer shows only {shown} px");
        assert_eq!(runs, 1, "mono {mono}: the tracer falls into {runs} strokes");
        assert!(
            gap <= 2,
            "mono {mono}: the tracer ends {gap} px short of the landing"
        );
        let (_, _, stretch_gap) = laid(true);
        assert!(
            stretch_gap >= 6,
            "mono {mono}: control: kept to its longest stretch the tracer ends {stretch_gap} px short"
        );
    }
}

/// looks10 L6 (after looks9 Q11): on a dark page Classic pink's tracer for a move up across
/// lettered rows kept its longest piece in the air, a dash floating between the old caret
/// and the new, anchored to neither (`classic-pink/classic-dark-move-up.up`). It runs from
/// the old caret to the new, through the lettered rows at a share of its light
/// ([`TRACER_UNDER_LETTERS`]): each frame its light past the caret's cell is one run of lit
/// pixel columns and it reaches within two px of the caret's cell (the letters it passes
/// keep their bar by the letter guard). Negative control: kept to its longest piece, it
/// ends six px or more short of the caret's cell.
#[test]
fn a_one_colour_move_up_on_a_dark_page_runs_to_the_caret() {
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let c = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        ..cfg()
    };
    // The old caret's line, a line it rises past with letters in its
    // middle, and the line it lands in (the probe knows three rows).
    let lines = [(4, 0, 1), (3, 10, 18), (2, 0, 34)];
    let (from, to) = ((4u16, 2u16), (2u16, 30u16));
    // (lit px, most runs in a frame, least gap to the caret's cell)
    let laid = |stretch: bool| -> (usize, usize, i32) {
        let t0 = Instant::now();
        let mut rig = Rig {
            spans: crate::cursor_glow::TextSpans::of_lines(&lines),
            ..Rig::default()
        };
        rig.w.tracer_up_stretch = stretch;
        rig.tick(Some(from), t0, &c, g);
        let ww = usize::from(g.win_w);
        let (mut shown, mut runs, mut gap) = (0usize, 0usize, i32::MAX);
        for at in [16u64, 40, 80] {
            rig.tick(Some(to), ms(t0, at), &c, g);
            // Both layers add premultiplied light: a pixel shows when the
            // light summed on it from the air strokes and the bed under
            // the letters reaches a visible step in any channel.
            let mut sum = vec![[0u32; 3]; ww * usize::from(g.win_h)];
            for q in rig.out.iter().chain(rig.under.iter()) {
                for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                    for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                        let px = &mut sum[y as usize * ww + x as usize];
                        for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                            px[k] += (q.color >> sh) & 0xFF;
                        }
                    }
                }
            }
            let (cx0, cy0) = (i32::from(to.1) * cw - 2, i32::from(to.0) * ch - 2);
            let (cx1, cy1) = (cx0 + cw + 3, cy0 + ch + 3);
            let mut cols = vec![false; ww];
            for (i, px) in sum.iter().enumerate() {
                let light = px.iter().copied().max().unwrap_or(0);
                if light < 24 {
                    continue;
                }
                let (x, y) = ((i % ww) as i32, (i / ww) as i32);
                if (cx0..=cx1).contains(&x) && (cy0..=cy1).contains(&y) {
                    continue;
                }
                shown += 1;
                gap = gap.min((cx0 - x).max(x - cx1).max(cy0 - y).max(y - cy1));
                cols[x as usize] = true;
            }
            let n = cols.windows(2).filter(|w| w[1] && !w[0]).count() + usize::from(cols[0]);
            runs = runs.max(n);
        }
        (shown, runs, gap)
    };
    let (shown, runs, gap) = laid(false);
    assert!(shown >= 200, "fixture: the tracer shows only {shown} px");
    assert_eq!(runs, 1, "the one-colour tracer falls into {runs} strokes");
    assert!(
        gap <= 2,
        "the one-colour tracer ends {gap} px short of the caret"
    );
    let (_, _, stretch_gap) = laid(true);
    assert!(
        stretch_gap >= 6,
        "control: kept to its longest piece the tracer ends {stretch_gap} px short of the caret"
    );
}

/// looks10 L6, round 1: the one-colour tracer up a dark page, run whole to the caret, was cut
/// finer through the swept cells' centres as they stood, so it climbed in stairs, a flat in
/// each row it crossed and a step between (`classic-pink/solarized-dark/classic-dark-move-up.up`),
/// where looks9's straightened beam drew one slant. It is the line its straightened beam
/// draws: each frame the core of each column it crosses between the two carets (the centre
/// of the pixels at half its brightest or more) lies within two px of one straight line
/// fitted through them all. Negative control: laid through the cells' centres, its columns
/// stray four px or more from that line.
#[test]
fn a_one_colour_move_up_on_a_dark_page_is_one_slant() {
    let g = geom();
    let (cw, ch) = (g.cw, g.ch);
    let c = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        ..cfg()
    };
    let lines = [(4, 0, 1), (3, 10, 18), (2, 0, 34)];
    let (from, to) = ((4u16, 2u16), (2u16, 30u16));
    // (the least columns read in a frame, the most a column's core strays, px)
    let stray = |stairs: bool| -> (usize, f32) {
        let t0 = Instant::now();
        let mut rig = Rig {
            spans: crate::cursor_glow::TextSpans::of_lines(&lines),
            ..Rig::default()
        };
        rig.w.tracer_up_stairs = stairs;
        rig.tick(Some(from), t0, &c, g);
        let ww = usize::from(g.win_w);
        let (mut least, mut worst) = (usize::MAX, 0.0f32);
        for at in [16u64, 40, 80] {
            rig.tick(Some(to), ms(t0, at), &c, g);
            let mut light = vec![0u32; ww * usize::from(g.win_h)];
            for q in &rig.out {
                for y in usize::from(q.y)..usize::from(q.y) + usize::from(q.h) {
                    for x in usize::from(q.x)..usize::from(q.x) + usize::from(q.w) {
                        let px = &mut light[y * ww + x];
                        *px += [16u32, 8, 0]
                            .into_iter()
                            .map(|sh| (q.color >> sh) & 0xFF)
                            .max()
                            .unwrap_or(0);
                    }
                }
            }
            // The columns clear of both carets' cells and of the
            // landing round the new one, over the rows the move rises.
            let xs = (usize::from(from.1) + 2) * cw..(usize::from(to.1) - 3) * cw;
            let ys = usize::from(to.0) * ch..(usize::from(from.0) + 1) * ch;
            let mut cores = Vec::new();
            for x in xs {
                let top = ys.clone().map(|y| light[y * ww + x]).max().unwrap_or(0);
                if top < 24 {
                    continue;
                }
                let (mut sum, mut weight) = (0.0f32, 0.0f32);
                for y in ys.clone() {
                    let l = light[y * ww + x];
                    if 2 * l >= top {
                        sum += y as f32 * l as f32;
                        weight += l as f32;
                    }
                }
                cores.push((x as f32, sum / weight));
            }
            least = least.min(cores.len());
            let k = cores.len() as f32;
            let (mx, my) = cores
                .iter()
                .fold((0.0, 0.0), |(a, b), &(x, y)| (a + x / k, b + y / k));
            let (sxy, sxx) = cores.iter().fold((0.0, 0.0), |(a, b), &(x, y)| {
                (a + (x - mx) * (y - my), b + (x - mx) * (x - mx))
            });
            let slope = sxy / sxx;
            for &(x, y) in &cores {
                worst = worst.max((y - (my + slope * (x - mx))).abs());
            }
        }
        (least, worst)
    };
    let (read, straight) = stray(false);
    assert!(read >= 24, "fixture: a frame reads only {read} columns");
    assert!(
        straight <= 2.0,
        "the one-colour tracer up a page strays {straight:.1} px from a line"
    );
    let (read, stairs) = stray(true);
    assert!(
        read >= 24,
        "control fixture: a frame reads only {read} columns"
    );
    assert!(
        stairs >= 4.0,
        "control: laid through the cells' centres the tracer strays only {stairs:.1} px"
    );
}

/// looks10 L6, round 2: on Solarized Dark a letter keeps 97% of its own reading under the
/// lights, so the guard cut the one-colour tracer up a page off every lettered row it rose
/// past, and only its stretch in the blank row showed, a glaring slash attached to neither
/// caret (`classic-pink/solarized-dark/classic-dark-move-up.up`, 56-59 px short). Through the
/// letters it is laid under them at the page's own luminance, one stroke on to the caret's
/// cell, and its light in the air is a share of its whole, not a white-hot core. Negative
/// controls: laid as round 1 laid it, nothing lies under the letters and the trace stops
/// short; kept whole in the air, its core burns brighter than the share.
#[test]
fn a_one_colour_move_up_on_a_dim_page_runs_under_the_letters_to_the_caret() {
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let page = 0x0000_2B36;
    let c = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        theme_bg: page,
        theme_fg: 0x0083_9496,
        ..cfg()
    };
    assert!(crate::rainbow_kitty::ribbon::dim_theme(c.theme_fg, page));
    // From a short prompt up past a blank row into a long line's letters.
    let lines = [(4, 0, 5), (2, 0, 34)];
    let (from, to) = ((4u16, 6u16), (2u16, 30u16));
    let page_lum = relative_luminance(page);
    // (under-letter quads at the page's luminance on row 2's letters, their least gap to the
    // caret's cell, the brightest channel the air light reaches)
    let laid = |unshaded: bool, whole: bool| -> (usize, i32, u32) {
        let t0 = Instant::now();
        let mut rig = Rig {
            spans: crate::cursor_glow::TextSpans::of_lines(&lines).with_blank_row(3),
            ..Rig::default()
        };
        rig.w.tracer_up_unshaded = unshaded;
        rig.w.tracer_up_whole = whole;
        rig.tick(Some(from), t0, &c, g);
        let (mut on_letters, mut gap, mut air_peak) = (0usize, i32::MAX, 0u32);
        let (cx0, cy0) = (i32::from(to.1) * cw, i32::from(to.0) * ch);
        let (cx1, cy1) = (cx0 + cw, cy0 + ch);
        for at in [16u64, 40, 80] {
            rig.tick(Some(to), ms(t0, at), &c, g);
            for q in &rig.under {
                if q.alpha < 48 {
                    continue;
                }
                let k = 255.0 / f32::from(q.alpha);
                let un = |sh: u32| ((((q.color >> sh) & 0xFF) as f32 * k).round() as u32).min(255);
                let rgb = (un(16) << 16) | (un(8) << 8) | un(0);
                let lum = relative_luminance(rgb);
                if (lum - page_lum).abs() > 0.25 * page_lum {
                    continue;
                }
                let (x0, y0) = (i32::from(q.x), i32::from(q.y));
                let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
                if y0 >= 2 * ch && y1 <= 3 * ch && x1 > 20 * cw && x0 < 30 * cw {
                    on_letters += 1;
                }
                gap = gap.min((cx0 - x1).max(x0 - cx1).max(cy0 - y1).max(y0 - cy1).max(0));
            }
            for q in &rig.out {
                let (x0, y0) = (i32::from(q.x), i32::from(q.y));
                let x1 = x0 + i32::from(q.w);
                // The blank row's air.
                if y0 >= 3 * ch && y0 < 4 * ch && x1 > 12 * cw && x0 < 24 * cw {
                    let top = [16u32, 8, 0]
                        .into_iter()
                        .map(|sh| (q.color >> sh) & 0xFF)
                        .max()
                        .unwrap_or(0);
                    air_peak = air_peak.max(top);
                }
            }
        }
        (on_letters, gap, air_peak)
    };
    let (on_letters, gap, air) = laid(false, false);
    assert!(
        on_letters >= 3,
        "the tracer lays only {on_letters} quads under row 2's letters"
    );
    assert!(
        gap <= 2,
        "the tracer stops {gap} px short of the caret's cell"
    );
    let (bare, bare_gap, _) = laid(true, false);
    assert_eq!(bare, 0, "control: round 1 laid nothing under the letters");
    assert!(
        bare_gap > 2,
        "control: round 1's trace reached within {bare_gap} px of the caret"
    );
    let (_, _, hot) = laid(false, true);
    assert!(air > 0, "fixture: the tracer lights the blank row's air");
    assert!(
        (air as f32) < 0.75 * hot as f32,
        "the air keeps {air} of the whole {hot}: a share, not the white-hot core"
    );
}

/// looks10 L6, round 2: on a dim page the one-colour tracer up a page lays none of its light
/// over the letters it rises past; they are its shade's. Its fifth there took no letter's
/// light, and the letter guard cuts every light over a cell to the one share its letter
/// keeps, so it cut the shade with it to a trace of 5 bytes on the page and the slash in the
/// blank row stood alone again (`classic-pink/solarized-dark/classic-dark-move-up.up`).
/// Negative control: its fifth kept over the letters (`tracer_up_lit_letters`), the light
/// lies on row 2's letters.
#[test]
fn a_one_colour_move_up_on_a_dim_page_leaves_its_letters_to_the_shade() {
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let page = 0x0000_2B36;
    let c = GlowConfig {
        classic_mono: true,
        color: 0x00D0_3A8C,
        accent: 0x00D0_3A8C,
        theme_bg: page,
        theme_fg: 0x0083_9496,
        ..cfg()
    };
    let lines = [(4, 0, 5), (2, 0, 34)];
    let (from, to) = ((4u16, 6u16), (2u16, 30u16));
    // (the light quads on row 2's letters off the caret's cell, the shade's quads there)
    let laid = |lit_letters: bool| -> (usize, usize) {
        let t0 = Instant::now();
        let mut rig = Rig {
            spans: crate::cursor_glow::TextSpans::of_lines(&lines).with_blank_row(3),
            ..Rig::default()
        };
        rig.w.tracer_up_lit_letters = lit_letters;
        rig.tick(Some(from), t0, &c, g);
        let (mut light, mut shade) = (0usize, 0usize);
        let on_letters = |q: &GlowQuad| {
            let (x0, y0) = (i32::from(q.x), i32::from(q.y));
            let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
            y0 >= 2 * ch && y1 <= 3 * ch && x1 > 18 * cw && x0 < 29 * cw
        };
        for at in [16u64, 40, 80] {
            rig.tick(Some(to), ms(t0, at), &c, g);
            light += rig
                .out
                .iter()
                .filter(|q| on_letters(q))
                .filter(|q| {
                    [16u32, 8, 0]
                        .into_iter()
                        .any(|sh| ((q.color | q.color2) >> sh) & 0xFF > 4)
                })
                .count();
            shade += rig
                .under
                .iter()
                .filter(|q| on_letters(q) && q.alpha >= 48)
                .count();
        }
        (light, shade)
    };
    let (light, shade) = laid(false);
    assert!(
        shade >= 3,
        "fixture: the shade lays only {shade} quads on row 2's letters"
    );
    assert_eq!(
        light, 0,
        "the tracer lays {light} quads of light on row 2's letters"
    );
    let (lit, _) = laid(true);
    assert!(
        lit > 0,
        "control: with its fifth kept the light lies on the letters"
    );
}

#[test]
fn the_wake_rides_a_scroll_and_a_band_move() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 16,
        ..geom()
    };
    let c = cfg();
    let t0 = Instant::now();
    let on_prompt_row = |rig: &Rig| {
        rig.w
            .sparks
            .iter()
            .filter(|s| s.row == 13 && (6..=15).contains(&s.col))
            .count()
    };
    let enter = |carry: &dyn Fn(&mut ClassicWake)| {
        let mut rig = Rig::default();
        rig.tick(Some((13, 15)), t0, &c, g);
        carry(&mut rig.w);
        rig.tick(Some((13, 2)), ms(t0, 16), &c, g);
        rig
    };
    let scroll = |w: &mut ClassicWake| w.translate_rows(&|row: u16| row.checked_sub(2));
    let band = |w: &mut ClassicWake| {
        w.translate_rows(&|row| crate::cursor_glow::band_row(row, 0, 13, -2));
    };
    for (name, carry) in [
        ("scroll", &scroll as &dyn Fn(&mut ClassicWake)),
        ("band", &band),
    ] {
        let rig = enter(carry);
        assert_eq!(
            on_prompt_row(&rig),
            0,
            "{name}: a comet along the new prompt row"
        );
        assert!(
            rig.w.sparks.iter().any(|s| s.row == 11),
            "{name}: the comet starts where the command's text now is"
        );
        let ring = rig.w.ring.expect("the Enter is a jump");
        assert_eq!(
            (ring.row, ring.col),
            (13, 2),
            "{name}: the ring sits on the landing cell"
        );
    }
    let untold = enter(&|_: &mut ClassicWake| {});
    assert!(
        on_prompt_row(&untold) > 0,
        "control: untold, the move reads as a horizontal jump on the prompt row"
    );
}

#[test]
fn a_scroll_drops_what_it_carries_off_the_top() {
    let g = geom();
    let c = GlowConfig {
        radius: 0.0,
        ..cfg()
    };
    let t0 = Instant::now();
    let mut rig = Rig::default();
    rig.tick(Some((0, 4)), t0, &c, g);
    rig.tick(Some((1, 30)), ms(t0, 16), &c, g);
    assert!(
        rig.w.ring.is_some() && !rig.w.sparks.is_empty(),
        "fixture: lit"
    );
    rig.w.translate_rows(&|row: u16| row.checked_sub(2));
    assert!(
        rig.w.sparks.is_empty(),
        "sparks carried off the top are gone"
    );
    assert!(rig.w.ring.is_none(), "so is the ring");
    assert!(!rig.w.has_last(), "and the caret memory");
    let fp = rig.tick(Some((3, 10)), ms(t0, 32), &c, g);
    assert!(rig.w.sparks.is_empty(), "no comet from a forgotten cell");
    assert_eq!(fp, 0, "nothing in flight, nothing spawned");
}

#[test]
fn reset_forgets_the_cursor_so_no_cross_space_comet_spawns() {
    let (g, c) = (geom(), cfg());
    let mut rig = Rig::default();
    let t0 = Instant::now();
    rig.tick(Some((2, 4)), t0, &c, g);
    rig.w.reset();
    assert!(!rig.w.is_active());
    let fp = rig.tick(Some((5, 30)), ms(t0, 16), &c, g);
    assert_eq!(fp, 0, "the first tick after a reset only re-observes");
    assert!(rig.out.is_empty());
}

#[test]
fn fingerprint_remaps_zero_fold_on_additive_quads_to_one() {
    let mut out_q = Vec::new();
    let mut out_r = aterm_render::GlowRamps::default();
    let mut under_q = Vec::new();
    let mut under_r = aterm_render::GlowRamps::default();
    assert_eq!(
        ClassicWake::fingerprint(
            &mut crate::cursor_glow::glow_sink(&mut out_q, &mut out_r, true),
            &mut crate::cursor_glow::glow_sink(&mut under_q, &mut under_r, true),
            &[],
        ),
        0,
        "all streams empty -> 0"
    );
    // (1 << 40) ^ (0x0010_0000 << 20) == 0 and (1 << 8) ^ 256 == 0, so the
    // raw polynomial term is 0 even though `out` holds a visible quad.
    out_q.push(GlowQuad {
        row: 1,
        x: 0,
        y: 0,
        w: 1,
        h: 256,
        color: 0x0010_0000,
        alpha: 0,
        color2: 0x0010_0000,
        alpha2: 0,
    });
    assert_eq!(
        ClassicWake::fingerprint(
            &mut crate::cursor_glow::glow_sink(&mut out_q, &mut out_r, true),
            &mut crate::cursor_glow::glow_sink(&mut under_q, &mut under_r, true),
            &[],
        ),
        1,
        "any laid additive quad remaps a zero fold to 1"
    );
}
