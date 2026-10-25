// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The whole cursor-family frame step, read as a host reads it.

use super::*;
use crate::cursor_glow::{KnobContext, TrailKnob};
use aterm_render::HaloMode;
use std::hash::{Hash, Hasher};

const DARK: (u32, u32) = (0x00C8_D3F5, 0x001A_1B26);
const LIGHT: (u32, u32) = (0x001F_2328, 0x00FF_FFFF);
const THEME_CURSOR: u32 = 0x0009_69DA;

const fn geom() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 8,
        cols: 60,
        origin_x: 0,
        origin_y: 0,
        win_w: 600,
        win_h: 160,
        head: 0,
    }
}

const fn knobs() -> GlowKnobs {
    GlowKnobs {
        enabled: true,
        color: None,
        accent: None,
        duration_ms: 260,
        length: 24,
        intensity: 1.0,
        radius: 0.6,
        ring: true,
    }
}

struct Session {
    fx: CursorFx,
    glow: GlowConfig,
    trail: TrailConfig,
    shape: CursorStyle,
    ground: (u32, u32),
    tinted: bool,
    t0: Instant,
}

fn hash_quads(quads: &[aterm_render::GlowQuad], hash: &mut impl Hasher) {
    for q in quads {
        (
            q.row, q.x, q.y, q.w, q.h, q.color, q.color2, q.alpha, q.alpha2,
        )
            .hash(hash);
    }
}

impl Session {
    fn new(raw: &str, knobs: GlowKnobs, bar: bool, ground: (u32, u32)) -> Self {
        let spelling = GlowSpelling::parse(raw);
        let shape = if bar {
            CursorStyle::SteadyBar
        } else {
            CursorStyle::SteadyBlock
        };
        let dark = aterm_render::theme_is_dark(ground.1);
        let mut glow = resolve_glow_config(
            knobs,
            spelling,
            THEME_CURSOR,
            dark,
            ground.0,
            ground.1,
            head_dx_for(shape),
        );
        let mut trail = TrailConfig {
            enabled: spelling.style == Some(GlowStyle::Comet),
            duration: std::time::Duration::from_millis(knobs.duration_ms),
            max_len: knobs.length,
            color: knobs
                .color
                .unwrap_or(crate::cursor_glow::COMET_DEFAULT_COLOR),
            intensity: 0.0,
            warmth: 0.0,
        };
        follow_live_cursor(
            &mut glow,
            &mut trail,
            THEME_CURSOR,
            false,
            knobs
                .color
                .map_or(TrailColor::Automatic, TrailColor::Pinned),
            knobs.accent.is_some(),
            None,
        );
        Self {
            fx: CursorFx::default(),
            glow,
            trail,
            shape,
            ground,
            tinted: knobs.color.is_some(),
            t0: Instant::now(),
        }
    }

    fn frame(&mut self, ms: u64, cur: (u16, u16), hash: &mut impl Hasher) {
        let now = self.t0 + std::time::Duration::from_millis(ms);
        self.fx.begin(true, true);
        let blank = vec![' '; geom().cols];
        self.fx.glow.observe_row(cur.0, cur.1, &blank, now);
        self.fx
            .glow
            .observe_neighbor_rows(Some(&blank), Some(&blank));
        let frame = self.fx.tick(
            &CursorFxInput {
                now,
                cur: Some(cur),
                live_viewport: true,
                cursor_style: self.shape,
                blink_phase: true,
                live_cursor: THEME_CURSOR,
                cursor_color_pinned: false,
                default_bg: self.ground.1,
                geom: geom(),
                focused: true,
                cursor_cell_bg: None,
                trail_brightness: 1.0,
                shed_envelope: 1.0,
                body_allowed: true,
                momentum_glow: false,
                user_tinted: self.tinted,
                accent_pinned: false,
            },
            &self.glow,
            self.trail,
        );
        hash_quads(&self.fx.glow_scratch, hash);
        hash_quads(self.fx.glow.under_quads(), hash);
        for h in self.fx.glow.halos() {
            for word in h.damage_words() {
                word.hash(hash);
            }
        }
        self.fx.glow.patches().len().hash(hash);
        for cell in &self.fx.trail_scratch {
            (cell.row, cell.col, cell.alpha).hash(hash);
        }
        frame.block_fill.map(|f| f.fill).hash(hash);
    }

    fn key(&mut self, ms: u64) {
        let now = self.t0 + std::time::Duration::from_millis(ms);
        self.fx.cadence.on_keystroke(now);
        self.fx.glow.note_synthetic_typed(now, 1);
        self.fx.trail.note_synthetic_typed(now);
    }

    fn jump(&mut self, ms: u64) {
        let now = self.t0 + std::time::Duration::from_millis(ms);
        self.fx.glow.note_synthetic_move(now);
        self.fx.trail.note_synthetic_move(now);
    }

    fn script(&mut self) -> u64 {
        let mut hash = aterm_hash::FxHasher::default();
        let mut ms = 0;
        let mut col = 4;
        self.frame(ms, (2, col), &mut hash);
        for _ in 0..8 {
            for _ in 0..2 {
                ms += 40;
                self.frame(ms, (2, col), &mut hash);
            }
            ms += 40;
            self.key(ms);
            col += 1;
            self.frame(ms, (2, col), &mut hash);
        }
        ms += 60;
        self.jump(ms);
        col += 6;
        self.frame(ms, (2, col), &mut hash);
        for _ in 0..4 {
            ms += 40;
            self.frame(ms, (2, col), &mut hash);
        }
        for _ in 0..2 {
            ms += 120;
            self.key(ms);
            col += 1;
            self.frame(ms, (2, col), &mut hash);
        }
        ms += 80;
        self.jump(ms);
        self.frame(ms, (5, 10), &mut hash);
        for _ in 0..5 {
            ms += 40;
            self.frame(ms, (5, 10), &mut hash);
        }
        hash.finish()
    }
}

#[test]
fn the_knob_law_is_what_the_frame_reads() {
    let styles = [
        "lumen",
        "phaser",
        "sparkle",
        "fire",
        "laser",
        "beam",
        "water",
        "comet",
        "classic",
        "rainbow kitty pet",
    ];
    type Turn = fn(&mut GlowKnobs, bool);
    let turns: [(TrailKnob, Turn); 5] = [
        (TrailKnob::Length, |k, on| {
            k.length = if on { 3 } else { 30 }
        }),
        (TrailKnob::Radius, |k, on| {
            k.radius = if on { 0.3 } else { 1.2 }
        }),
        (TrailKnob::Ring, |k, on| k.ring = on),
        (TrailKnob::Color, |k, on| {
            k.color = on.then_some(0x00FF_2020);
        }),
        (TrailKnob::Accent, |k, on| {
            k.accent = on.then_some(0x0020_FFFF);
        }),
    ];
    let mut wrong = Vec::new();
    for raw in styles {
        let style = GlowSpelling::parse(raw).style.expect("a built-in style");
        for ground in [DARK, LIGHT] {
            for bar in [false, true] {
                let mut scripted: Vec<(String, u64)> = Vec::new();
                let mut session = |k: GlowKnobs| {
                    let key = format!("{k:?}");
                    if let Some(&(_, hash)) = scripted.iter().find(|(seen, _)| *seen == key) {
                        return hash;
                    }
                    let hash = Session::new(raw, k, bar, ground).script();
                    scripted.push((key, hash));
                    hash
                };
                for ring in [true, false] {
                    for (knob, turn) in turns {
                        if knob == TrailKnob::Ring && !ring {
                            continue;
                        }
                        let turned = |on: bool| {
                            let mut k = knobs();
                            k.ring = ring;
                            turn(&mut k, on);
                            k
                        };
                        let shows = session(turned(true)) != session(turned(false));
                        let ctx = KnobContext {
                            dark: ground == DARK,
                            bar,
                            ring,
                            classic_tinted: false,
                            over_text: false,
                        };
                        let law = style.reads_knob(knob, ctx);
                        if shows != law {
                            let side = if ground == DARK { "dark" } else { "light" };
                            let shape = if bar { "bar" } else { "block" };
                            wrong.push(format!(
                                "{raw} {side} {shape} ring={ring} {knob:?}: frame {shows}, law {law}"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "knob law vs frame:\n{}", wrong.join("\n"));
    let mut red = knobs();
    red.color = Some(0x00FF_2020);
    assert_ne!(
        Session::new("lumen", red, false, DARK).script(),
        Session::new("lumen", knobs(), false, DARK).script()
    );
}

/// One licensed jump of `rows` rows and `cols` columns after a few typed keys, and the frames
/// its light lives through, folded into one hash.
fn jump_frames(raw: &str, length: usize, ground: (u32, u32), rows: u16, cols: u16) -> u64 {
    let mut k = knobs();
    k.length = length;
    let mut session = Session::new(raw, k, false, ground);
    let mut hash = aterm_hash::FxHasher::default();
    let (mut ms, mut col) = (0, 4);
    session.frame(ms, (2, col), &mut hash);
    for _ in 0..6 {
        ms += 120;
        session.key(ms);
        col += 1;
        session.frame(ms, (2, col), &mut hash);
    }
    ms += 200;
    session.jump(ms);
    for _ in 0..12 {
        session.frame(ms, (2 + rows, col + cols), &mut hash);
        ms += 40;
    }
    hash.finish()
}

/// [`GlowStyle::jump_reach`] is where Trail length stops changing a jump, read off the frames
/// the engine draws: at its reach a jump draws as it does at the longest length, a cell short of
/// it it draws otherwise, and where it says Length shapes nothing, no length changes anything.
#[test]
fn the_jump_reach_is_where_trail_length_stops_changing_a_jump() {
    let styles = [
        "lumen",
        "phaser",
        "sparkle",
        "fire",
        "laser",
        "beam",
        "water",
        "comet",
        "classic",
        "rainbow kitty pet",
    ];
    let aspect = geom().ch as f32 / geom().cw as f32;
    let mut wrong = Vec::new();
    for raw in styles {
        let style = GlowSpelling::parse(raw).style.expect("a built-in style");
        for ground in [DARK, LIGHT] {
            let ctx = KnobContext {
                dark: ground == DARK,
                ring: true,
                ..KnobContext::EMPTY
            };
            // Hops and leaps along a row, and Enters down to the next rows: the moves the
            // reach describes (a short move off its row on paper keeps only its head's row).
            for (rows, cols) in [(0, 3), (0, 6), (0, 8), (0, 12), (1, 30), (2, 40)] {
                let side = if ground == DARK { "dark" } else { "light" };
                let longest = jump_frames(raw, 512, ground, rows, cols);
                let reach = style.jump_reach(rows, cols, aspect, ctx);
                match reach {
                    Some(reach) => {
                        let at = reach.ceil() as usize;
                        if jump_frames(raw, at, ground, rows, cols) != longest {
                            wrong.push(format!(
                                "{raw} {side} ({rows},{cols}): Length {at} still changes it"
                            ));
                        }
                        let short = (reach.floor() as usize).saturating_sub(1).max(1);
                        if short < at && jump_frames(raw, short, ground, rows, cols) == longest {
                            wrong.push(format!(
                                "{raw} {side} ({rows},{cols}): Length {short} draws as {at}"
                            ));
                        }
                    }
                    None => {
                        if jump_frames(raw, 1, ground, rows, cols) != longest {
                            wrong.push(format!(
                                "{raw} {side} ({rows},{cols}): no reach, yet Length 1 changes it"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "jump reach vs frame:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn the_accent_follows_the_ring_where_the_ring_is_its_only_mark() {
    for raw in ["phaser", "sparkle"] {
        let session = |accent: Option<u32>| {
            let mut k = knobs();
            k.ring = false;
            k.accent = accent;
            Session::new(raw, k, false, DARK).script()
        };
        assert_eq!(session(None), session(Some(0x0020_FFFF)), "{raw}");
        let style = GlowSpelling::parse(raw).style.expect("built-in");
        assert!(!style.reads_knob(TrailKnob::Accent, KnobContext::default()));
    }
}

#[test]
fn light_bodies_are_ink_under_the_letters() {
    for (raw, bar) in [
        ("comet", false),
        ("beam", true),
        ("lumen", false),
        ("sparkle", true),
    ] {
        let mut s = Session::new(raw, knobs(), bar, LIGHT);
        let mut hash = aterm_hash::FxHasher::default();
        s.frame(0, (2, 4), &mut hash);
        s.key(40);
        s.frame(40, (2, 5), &mut hash);
        s.key(80);
        s.frame(80, (2, 6), &mut hash);
        assert!(
            s.fx.glow_scratch.iter().all(|q| q.alpha > 0),
            "{raw}: no additive light on paper"
        );
        assert!(
            s.fx.glow.halos().iter().all(|h| h.mode == HaloMode::Over),
            "{raw}: no additive halo on paper"
        );
        let g = geom();
        let near =
            s.fx.glow
                .under_quads()
                .iter()
                .filter(|q| q.alpha > 0)
                .filter(|q| {
                    let mid = i32::from(q.x) + i32::from(q.w) / 2;
                    (mid - 6 * g.cw as i32 - g.cw as i32 / 2).abs() <= 2 * g.cw as i32
                })
                .count();
        assert!(near > 0, "{raw}: ink around the caret");
        let mut d = Session::new(raw, knobs(), bar, DARK);
        d.frame(0, (2, 4), &mut hash);
        d.key(40);
        d.frame(40, (2, 5), &mut hash);
        assert!(
            d.fx.glow_scratch.iter().any(|q| q.alpha == 0),
            "{raw}: a dark ground keeps its light"
        );
    }
}

fn ink_mass(under: &[aterm_render::GlowQuad], x0: u16, x1: u16) -> f32 {
    let mut px = std::collections::HashMap::<(u16, u16), [f32; 3]>::new();
    for q in under.iter().filter(|q| q.alpha > 0) {
        let keep = 1.0 - f32::from(q.alpha) / 255.0;
        let src = [
            ((q.color >> 16) & 0xFF) as f32,
            ((q.color >> 8) & 0xFF) as f32,
            (q.color & 0xFF) as f32,
        ];
        for x in q.x.max(x0)..(q.x + q.w).min(x1) {
            for y in q.y..q.y + q.h {
                let dst = px.entry((x, y)).or_insert([255.0; 3]);
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = s + *d * keep;
                }
            }
        }
    }
    px.values()
        .map(|[r, g, b]| 255.0 - (0.2126 * r + 0.7152 * g + 0.0722 * b))
        .sum()
}

#[test]
fn the_light_comet_carries_a_body_and_a_round_head() {
    let g = geom();
    let typed = |raw: &str| {
        let mut s = Session::new(raw, knobs(), false, LIGHT);
        let mut hash = aterm_hash::FxHasher::default();
        let mut ms = 0;
        s.frame(ms, (2, 4), &mut hash);
        for col in 5..=10 {
            ms += 60;
            s.key(ms);
            s.frame(ms, (2, col), &mut hash);
        }
        s
    };
    let behind = |s: &Session| ink_mass(s.fx.glow.under_quads(), 7 * g.cw as u16, 10 * g.cw as u16);
    let comet = behind(&typed("comet"));
    let beam = behind(&typed("beam"));
    assert!(
        comet >= 0.75 * beam,
        "the comet's body: {comet:.0} vs the beam's {beam:.0}"
    );
    let rest = |raw: &str| {
        let mut s = typed(raw);
        let mut hash = aterm_hash::FxHasher::default();
        s.frame(60 * 6 + 1500, (2, 10), &mut hash);
        s
    };
    let column_height = |s: &Session, x: u16| -> u16 {
        let covering = || {
            s.fx.glow
                .under_quads()
                .iter()
                .filter(move |q| q.alpha > 0 && (q.x..q.x + q.w).contains(&x))
        };
        let top = covering().map(|q| q.y).min();
        let bottom = covering().map(|q| q.y + q.h).max();
        top.zip(bottom).map_or(0, |(t, b)| b - t)
    };
    let comet = rest("comet");
    let centre = 10 * g.cw as u16 + g.cw as u16 / 2;
    let past = |side: i32| (i32::from(centre) + side * (g.cw as i32 / 2 + 2)) as u16;
    for side in [-1, 1] {
        let h = column_height(&comet, past(side));
        assert!(h > 0, "the head reaches past the block's edge ({side})");
        assert!(
            h < column_height(&comet, centre),
            "the head is round: shorter past the edge ({side})"
        );
    }
    let beam = rest("beam");
    let beam_edge = column_height(&beam, past(1));
    assert_eq!(
        beam_edge, 0,
        "control: the beam's block lays no head of ink beside it"
    );
}

/// SPARKLE'S PAPER CARET IS GOLD: the rod that fills Sparkle's block on
/// paper wore its dark-theme peach, which the page floor deepened to a
/// mustard brown (`#8f5602`, hue 36) beside the gold its puff wears. It
/// wears the paper gold, deepened in its hue to the caret's page bar, on
/// GitHub Light and Default Light. A trail colour the user chose keeps its
/// own hue (the control: the gold is Sparkle's own, never laid over a
/// choice).
#[test]
fn sparkle_s_paper_caret_is_gold_deepened_in_its_hue() {
    let fill_of = |color: Option<u32>, ground: (u32, u32)| -> u32 {
        let mut k = knobs();
        k.color = color;
        let mut s = Session::new("sparkle", k, false, ground);
        let now = s.t0;
        s.fx.begin(true, true);
        let blank = vec![' '; geom().cols];
        s.fx.glow.observe_row(2, 4, &blank, now);
        let frame = s.fx.tick(
            &CursorFxInput {
                now,
                cur: Some((2, 4)),
                live_viewport: true,
                cursor_style: s.shape,
                blink_phase: true,
                live_cursor: THEME_CURSOR,
                cursor_color_pinned: false,
                default_bg: ground.1,
                geom: geom(),
                focused: true,
                cursor_cell_bg: None,
                trail_brightness: 1.0,
                shed_envelope: 1.0,
                body_allowed: true,
                momentum_glow: false,
                user_tinted: s.tinted,
                accent_pinned: false,
            },
            &s.glow,
            s.trail,
        );
        frame.block_fill.expect("sparkle fills its block").fill & 0x00FF_FFFF
    };
    for ground in [LIGHT, (0x002B_2D33, 0x00F6_F7F9)] {
        let fill = fill_of(None, ground);
        let (hue, sat, _) = crate::color_math::rgb2hsv(fill);
        let ratio = crate::color_math::contrast_ratio(fill, ground.1);
        assert!(
            (41.0..=47.0).contains(&hue) && sat > 0.8,
            "sparkle's paper caret {fill:06x} on {:06x} stands at hue {hue:.1}, saturation {sat:.2}: not gold",
            ground.1
        );
        assert!(
            ratio >= CARET_PAGE_MIN_CONTRAST,
            "sparkle's paper caret {fill:06x} reads {ratio:.2}:1 on {:06x}",
            ground.1
        );
    }
    let chosen = fill_of(Some(0x00FF_3399), LIGHT);
    let (hue, _, _) = crate::color_math::rgb2hsv(chosen);
    let (own, _, _) = crate::color_math::rgb2hsv(0x00FF_3399);
    assert!(
        (hue - own).abs() <= 6.0,
        "control: a chosen trail colour keeps its hue ({chosen:06x} at {hue:.1}, its own {own:.1})"
    );
}
