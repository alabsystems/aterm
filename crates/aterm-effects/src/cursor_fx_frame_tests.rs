// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The whole cursor-family frame step, read as a host reads it: the knob
//! law against what the frame draws, and the light ground's bodies.

use super::*;
use crate::cursor_glow::{KnobContext, TrailKnob};
use aterm_render::HaloMode;
use std::hash::{Hash, Hasher};

const DARK: (u32, u32) = (0x00C8_D3F5, 0x001A_1B26);
const LIGHT: (u32, u32) = (0x001F_2328, 0x00FF_FFFF);
const THEME_CURSOR: u32 = 0x0009_69DA;

fn geom() -> Geom {
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

fn knobs() -> GlowKnobs {
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

/// One host frame of the scripted session.
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
            knobs.color.is_some(),
            knobs.accent.is_some(),
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
                amplitude: 1.0,
                shed_envelope: 1.0,
                body_allowed: true,
                momentum_glow: false,
                user_tinted: self.tinted,
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

    /// Type eight keys, hop six cells forward (a paste), type two more, then
    /// drop to another row: every move a host's trail meets.
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

/// THE KNOB LAW IS WHAT THE FRAME READS: for every built-in style, on a
/// dark and a light ground and with a block and a bar cursor, changing one
/// knob changes the scripted session's frames exactly when
/// `GlowStyle::reads_knob` says the style reads it. Both directions are the
/// check: a knob the law lists must show (no dead row in Settings), and a
/// knob it does not must leave every frame byte-identical (no hidden path).
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
                for ring in [true, false] {
                    for (knob, turn) in turns {
                        if knob == TrailKnob::Ring && !ring {
                            continue;
                        }
                        let session = |on: bool| {
                            let mut k = knobs();
                            k.ring = ring;
                            turn(&mut k, on);
                            Session::new(raw, k, bar, ground).script()
                        };
                        let shows = session(true) != session(false);
                        let ctx = KnobContext {
                            dark: ground == DARK,
                            bar,
                            ring,
                            classic_tinted: false,
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
    // Negative control: a knob the frame plainly reads is caught, so the
    // harness is not blind (Lumen's colour on a dark block).
    let mut red = knobs();
    red.color = Some(0x00FF_2020);
    assert_ne!(
        Session::new("lumen", red, false, DARK).script(),
        Session::new("lumen", knobs(), false, DARK).script()
    );
}

/// The ring knob reaches the accent only while the ring is on: Phaser's and
/// Sparkle's accent colours nothing else, so with the ring off it is dead,
/// as the law's context says.
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

/// THE BODIES ON PAPER: on a light ground the comet's nucleus, the light
/// rod (bar and emitter block) and every crown are ink under the letters:
/// the frame carries no additive light at all, and the under-ink stream
/// holds source-over ink around the caret. Negative control: the same
/// frames on a dark ground carry the bodies' additive light.
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

/// How dark the under-ink makes the white paper, summed over pixel columns
/// `x0..x1` (composited in emission order).
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

/// THE LIGHT COMET HAS A BODY AND A HEAD: typed on paper, the comet lays at
/// least three quarters of the beam's ink behind the caret (it was about a
/// third: a tail that tapered twice and a head dropped), and at rest it
/// keeps a round head of ink past both edges of its block, as the dark coma
/// is always there. Negative control: the beam's block has no round head
/// (its aura hugs the block, full height to its edge).
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
    // At rest, 1.5 s on: the comet's round head.
    let rest = |raw: &str| {
        let mut s = typed(raw);
        let mut hash = aterm_hash::FxHasher::default();
        s.frame(60 * 6 + 1500, (2, 10), &mut hash);
        s
    };
    // The ink's vertical extent in one pixel column.
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
    assert!(
        beam_edge == 0 || beam_edge >= g.ch as u16 - 1,
        "the beam's aura hugs its block square, never a round head: {beam_edge}"
    );
}
