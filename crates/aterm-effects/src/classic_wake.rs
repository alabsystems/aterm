// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `classic` cursor style: a thin four-layer anti-aliased comet along the swept cell path under a three-rectangle box-stack bloom crown, with an expanding square ring on a jump.

use std::time::Duration;

use aterm_time::Instant;

use aterm_render::{
    BeamVertex, CometSample, GlowQuad, HaloMode, RainHalo, comet_beam_over, comet_glow_quads,
    premul_rgb,
};

use crate::cursor_glow::{
    BedScratch, BedShape, CursorGlow, Geom, GlowConfig, InkLedger, LIGHT_BODY_ALPHA_CAP,
    LIGHT_MARK_QUADS, LightGround, TextView, clip_quads_to, hsv2rgb, lay_dark_bed, push_halo_rows,
    push_ink_ellipse, push_soft_veil, split_air_runs,
};
use crate::effect_util::{lerp_rgb, push_fx_rect as push_rect};
use crate::trail_sweep::line_cells_tail;

/// One comet cell, fading from `born`.
#[derive(Clone, Copy)]
struct Spark {
    row: u16,
    col: u16,
    /// Coverage at birth (head bright, tail faint).
    born_cov: u8,
    /// Position along the comet 0.0 (tail) .. 1.0 (head), for the hue sweep.
    pos: f32,
    /// Fade lifetime in seconds: short for a lone typing advance, chained to the
    life: f32,
    /// Typing spark (else jump): picks the fade curve in `emit_comet`.
    typing: bool,
    born: Instant,
}

/// One expanding landing ring, kept on its landing cell (not in pixels) so it
#[derive(Clone, Copy)]
struct Ring {
    row: u16,
    col: u16,
    born: Instant,
    life: f32,
}

/// The classic wake's animation state; no other style touches it.
#[derive(Default)]
pub struct ClassicWake {
    sparks: Vec<Spark>,
    /// Resident, bounded swept-cell scratch, built backward from the landing
    path_scratch: Vec<(i32, i32)>,
    ring: Option<Ring>,
    /// Last observed cursor cell, to detect a move.
    last: Option<(u16, u16)>,
    /// When the cursor last moved (drives the bloom-crown fade around the head).
    last_move: Option<Instant>,
    /// Deadline until which the crown is still emitted (cached so [`Self::is_active`]
    crown_until: Option<Instant>,
    /// The crown window the last move applied: [`Self::CROWN_TYPING_MS`] for a
    crown_window_ms: u64,
    /// Rolling hue phase (turns), advanced per spawn.
    hue: f32,
    /// Typing-cadence heat 0..1, decayed lazily from `heat_at`, so idle gaps
    heat: f32,
    heat_at: Option<Instant>,
    /// Last typing advance, for the inter-key cadence.
    last_type: Option<Instant>,
    /// Resident run-builder pool, so the comet reuses its nested buffers every
    comet_runs: Vec<Vec<CometSample>>,
    /// Index of the run being filled. `comet_live + 1` slots are live once
    comet_live: usize,
    /// Resident light-ground stroke scratch ([`Self::emit_light_comet`]), so
    light_verts: Vec<BeamVertex>,
    /// The move this frame's tick spawned on, `(from, to)`, for the host's
    spawned: Option<((u16, u16), (u16, u16))>,
    /// The mono face's dark bed's working buffers ([`lay_dark_bed`]).
    bed_scratch: BedScratch,
    /// The mono face's tracer runs over air ([`split_air_runs`]), pooled as
    air_runs: Vec<Vec<CometSample>>,
    /// Test-only: draw the mono face on a dark ground as the spectrum face draws, additive light over the letters, the negative control of its bed test.
    #[cfg(test)]
    pub(crate) mono_unbedded: bool,
    /// Test-only: lay the light-ground stroke and crown in their unsolved vivid ink (`LightGround::unsolved`), the legibility tests' negative control.
    #[cfg(test)]
    pub(crate) light_unsolved: bool,
}

/// What the mono face's dark bed reads of the frame ([`ClassicWake::tick`]): the probed text, which says where the tracer may be light, and the over-ink account its bed, crown and ring spend.
pub(crate) struct WakeInk<'a> {
    /// What the frame shows of text: the probe's lines and the frame's own cells for the rows it missed.
    pub(crate) text: TextView<'a>,
    /// The frame's over-ink account (idle when the host keeps none).
    pub(crate) ledger: &'a mut InkLedger,
}

/// The comet colour at path position `pos` (0 tail .. 1 head), in the face `mono` names: the one colour rule of the classic wake, shared with [`crate::cursor_glow::style_comet_color`]'s `Classic` arm.
#[must_use]
#[inline]
pub fn comet_color(mono: bool, color: u32, accent: u32, hue: f32, pos: f32) -> u32 {
    if mono {
        lerp_rgb(accent, color, pos)
    } else {
        hsv2rgb((hue + pos * 0.5).fract(), 0.95, 1.0)
    }
}

impl ClassicWake {
    /// Hard cap on emitted quads (defends the renderer + the per-frame upload).
    const MAX_QUADS: usize = 8192;
    /// Hard cap on live swept-path samples. `cfg.length` bounds one move; this
    const MAX_SPARKS: usize = 512;
    /// Bloom-crown fade window (the crown follows the head this long post-move).
    const CROWN_MS: u64 = 200;
    /// Typing crown window: at human cadence (≤~350 ms between keys) the crown
    const CROWN_TYPING_MS: u64 = 350;
    /// Heat earned by one keystroke at full cadence (≈7 fast keys to full heat).
    const HEAT_GAIN: f32 = 0.16;
    /// Exponential heat cool-down time constant (seconds to ~37%).
    const HEAT_DECAY_TAU: f32 = 0.9;
    /// Inter-key gap (seconds) at or under which a keystroke earns full heat…
    const HEAT_GAP_FULL: f32 = 0.09;
    /// …and the gap beyond which it earns none (relaxed typing stays cool).
    const HEAT_GAP_ZERO: f32 = 0.40;
    /// Typing continuity: at or under this gap the spark's life chains to the
    const CHAIN_GAP_MAX: f32 = 0.40;
    /// Chained life = observed gap + this margin…
    const CHAIN_MARGIN: f32 = 0.10;
    /// …capped here so the post-stop tail stays crisp.
    const CHAIN_LIFE_MAX: f32 = 0.50;

    /// Wake brightness multiplier for typing light at the current heat: cool
    #[inline]
    const fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// Any light still alive → keep the animation timer armed.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        !self.sparks.is_empty() || self.ring.is_some() || self.crown_until.is_some()
    }

    /// The move the last tick spawned on, `(from, to)`, once: the classic wake lights every observed move, typed or not, and the host records it in the same spawn count and diagnosis ring the other styles fill.
    #[inline]
    pub const fn take_spawn(&mut self) -> Option<((u16, u16), (u16, u16))> {
        self.spawned.take()
    }

    /// Whether the engine remembers a cursor cell, from which its next tick would spawn a comet on a move.
    #[must_use]
    #[inline]
    pub const fn has_last(&self) -> bool {
        self.last.is_some()
    }

    /// Live swept-path samples, for `trail status`'s `sparks=` count.
    #[must_use]
    #[inline]
    pub const fn live_sparks(&self) -> usize {
        self.sparks.len()
    }

    /// The live sparks' cells, for the host-seam tests.
    #[cfg(test)]
    pub(crate) fn spark_cells(&self) -> Vec<(u16, u16)> {
        self.sparks.iter().map(|s| (s.row, s.col)).collect()
    }

    /// The rows the live sparks span, `(first, last)`, for the host's read of the frame's text ([`CursorGlow::observe_frame_text`]).
    #[inline]
    pub(crate) fn row_range(&self) -> Option<(u16, u16)> {
        let mut iter = self.sparks.iter().map(|s| s.row);
        let first = iter.next()?;
        Some(iter.fold((first, first), |(lo, hi), r| (lo.min(r), hi.max(r))))
    }

    /// The landing ring's cell, for the host-seam tests.
    #[cfg(test)]
    pub(crate) fn ring_cell(&self) -> Option<(u16, u16)> {
        self.ring.map(|r| (r.row, r.col))
    }

    /// Drop all in-flight light and forget the last cursor position, used when the cursor's coordinate space changes or the classic engine stops owning the frame (another style, the master switch), so no comet spawns from a stale position and no orphaned light keeps the host awake.
    pub fn reset(&mut self) {
        self.sparks.clear();
        self.ring = None;
        self.last = None;
        self.spawned = None;
        self.last_move = None;
        self.crown_until = None;
        self.crown_window_ms = Self::CROWN_MS;
        self.heat = 0.0;
        self.heat_at = None;
        self.last_type = None;
    }

    /// Carry every row-addressed member under `map` (an absolute row to its new row, `None` once carried off the grid or past a band's edge): the sparks, the ring's landing cell and the last observed cell ride the content, and whatever leaves is dropped, never clamped.
    pub fn translate_rows(&mut self, map: &impl Fn(u16) -> Option<u16>) {
        self.sparks.retain_mut(|s| match map(s.row) {
            Some(row) => {
                s.row = row;
                true
            }
            None => false,
        });
        self.ring = self
            .ring
            .and_then(|r| map(r.row).map(|row| Ring { row, ..r }));
        self.last = self
            .last
            .and_then(|(row, col)| map(row).map(|row| (row, col)));
    }

    /// Advance one frame: observe the cursor at `cur`, spawn on a move, decay, and emit the light into `out` (additive quads, dark ground) and `halos` (source-over veils, light ground).
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor, clock, config, geometry and the three streams the wake lands in"
    )]
    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry, cursor, the three streams and the over-text facts"
    )]
    pub(crate) fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
        under: &mut Vec<GlowQuad>,
        ink: WakeInk<'_>,
    ) -> u64 {
        out.clear();
        halos.clear();
        under.clear();
        self.spawned = None;
        if !cfg.enabled || cfg.intensity <= 0.0 || geom.cw == 0 || geom.ch == 0 {
            self.sparks.clear();
            self.ring = None;
            self.crown_until = None;
            self.last = cur;
            self.heat = 0.0;
            self.heat_at = None;
            self.last_type = None;
            return 0;
        }

        if let Some(t0) = self.heat_at
            && self.heat > 0.0
        {
            let dt = now.saturating_duration_since(t0).as_secs_f32();
            if dt > 0.0 {
                self.heat *= (-dt / Self::HEAT_DECAY_TAU).exp();
                if self.heat < 0.005 {
                    self.heat = 0.0;
                }
            }
        }
        self.heat_at = Some(now);

        if let (Some((pr, pc)), Some((cr, cc))) = (self.last, cur)
            && (pr != cr || pc != cc)
        {
            let (dr, dc) = (cr.abs_diff(pr), cc.abs_diff(pc));
            let typing = dr.max(dc) <= 1 || (dr == 0 && dc == 2);
            self.crown_window_ms = if typing {
                Self::CROWN_TYPING_MS
            } else {
                Self::CROWN_MS
            };
            self.spawn((pr, pc), (cr, cc), typing, now, cfg, geom);
            self.spawned = Some(((pr, pc), (cr, cc)));
            self.last_move = Some(now);
            self.crown_until = Some(now + Duration::from_millis(self.crown_window_ms));
        }
        self.last = cur;

        self.sparks
            .retain(|s| now.saturating_duration_since(s.born).as_secs_f32() < s.life);
        if self.sparks.len() > Self::MAX_SPARKS {
            let drop = self.sparks.len() - Self::MAX_SPARKS;
            self.sparks.drain(0..drop);
        }
        if let Some(r) = self.ring
            && now.saturating_duration_since(r.born).as_secs_f32() >= r.life
        {
            self.ring = None;
        }
        if self.crown_until.is_some_and(|t| now >= t) {
            self.crown_until = None;
        }

        if cfg.dark_theme && cfg.classic_mono && !self.mono_unbedded() {
            self.emit_mono_dark(now, cfg, geom, cur, out, under, ink.text, ink.ledger);
            out.truncate(Self::MAX_QUADS);
            under.truncate(Self::MAX_QUADS);
            let from = halos.len();
            self.emit_mono_crown(now, cfg, geom, cur, halos);
            ink.ledger
                .admit_halos_masked(halos, from, InkLedger::HEAD_FLOOR);
            if out.len() < Self::MAX_QUADS {
                let from = out.len();
                self.emit_ring(now, cfg, geom, out);
                let to = out.len();
                ink.ledger
                    .admit_masked(out, from..to, InkLedger::FLYING_FLOOR);
            }
            out.truncate(Self::MAX_QUADS);
        } else if cfg.dark_theme {
            self.emit_comet(now, cfg, geom, cur, out);
            out.truncate(Self::MAX_QUADS);
            if out.len() < Self::MAX_QUADS {
                self.emit_crown(now, cfg, geom, cur, out);
            }
            if out.len() < Self::MAX_QUADS {
                self.emit_ring(now, cfg, geom, out);
            }
            out.truncate(Self::MAX_QUADS);
        } else {
            self.emit_light_crown(now, cfg, geom, cur, under);
            self.emit_light_comet(now, cfg, geom, cur, under);
            under.truncate(Self::MAX_QUADS);
            self.emit_light_ring(now, cfg, geom, halos);
            halos.truncate(CursorGlow::MAX_HALOS);
        }

        let mut fp: u64 = 0;
        for q in out.iter() {
            fp = fp.wrapping_mul(1_000_003).wrapping_add(
                ((q.row as u64) << 40)
                    ^ ((q.x as u64) << 28)
                    ^ ((q.y as u64) << 16)
                    ^ ((q.w as u64) << 8)
                    ^ (q.h as u64)
                    ^ ((q.color as u64) << 20),
            );
        }
        for q in under.iter() {
            fp = fp.wrapping_mul(1_000_003).wrapping_add(
                ((q.row as u64) << 40)
                    ^ ((q.x as u64) << 28)
                    ^ ((q.y as u64) << 16)
                    ^ ((q.w as u64) << 8)
                    ^ (q.h as u64)
                    ^ ((q.color as u64) << 20)
                    ^ ((q.color2 as u64) << 4)
                    ^ (u64::from(q.alpha) << 56)
                    ^ (u64::from(q.alpha2) << 48),
            );
        }
        if !halos.is_empty() {
            for h in halos.iter() {
                for word in h.damage_words() {
                    fp = fp.wrapping_mul(1_000_003).wrapping_add(word);
                }
            }
        }
        if !under.is_empty() || !halos.is_empty() {
            fp = fp.max(1);
        }
        fp
    }

    fn spawn(
        &mut self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        typing: bool,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) {
        let gap = if typing {
            let gap = self
                .last_type
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(f32::MAX);
            let cadence = 1.0
                - ((gap - Self::HEAT_GAP_FULL) / (Self::HEAT_GAP_ZERO - Self::HEAT_GAP_FULL))
                    .clamp(0.0, 1.0);
            self.heat = (self.heat + cadence * Self::HEAT_GAIN).min(1.0);
            self.last_type = Some(now);
            gap
        } else {
            f32::MAX
        };
        let boost = if typing { self.typing_boost() } else { 1.0 };
        let full_life = cfg.duration.as_secs_f32().max(0.001);
        let spark_life = if typing {
            let heat_life = (full_life * 0.38).clamp(0.05, 0.11) * (1.0 + 1.3 * self.heat);
            if gap <= Self::CHAIN_GAP_MAX {
                heat_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
            } else {
                heat_life
            }
        } else {
            full_life
        };

        let max_len = cfg.length.clamp(1, Self::MAX_SPARKS);
        self.path_scratch.clear();
        line_cells_tail(
            &mut self.path_scratch,
            (pr as i32, pc as i32),
            (cr as i32, cc as i32),
            max_len,
            false,
        );
        if !self.path_scratch.is_empty() {
            let n = self.path_scratch.len() as f32;
            for (i, &(r, c)) in self.path_scratch.iter().enumerate() {
                if r < 0 || c < 0 || r as usize >= geom.rows || c as usize >= geom.cols {
                    continue;
                }
                let pos = (i as f32 + 1.0) / n;
                let born_cov = ((40.0 + 175.0 * pos) * boost).min(255.0) as u8;
                self.sparks.push(Spark {
                    row: r as u16,
                    col: c as u16,
                    born_cov,
                    pos,
                    life: spark_life,
                    typing,
                    born: now,
                });
            }
            if self.sparks.len() > Self::MAX_SPARKS {
                let drop = self.sparks.len() - Self::MAX_SPARKS;
                self.sparks.drain(0..drop);
            }
        }

        self.hue = (self.hue + 0.07).fract();

        if cfg.ring && !typing {
            self.ring = Some(Ring {
                row: cr,
                col: cc,
                born: now,
                life: 0.18,
            });
        }
    }

    /// A spark's coverage now: typing sparks hold full brightness for 55% of
    #[inline]
    fn spark_cov(s: &Spark, now: Instant, cfg: &GlowConfig) -> u8 {
        let age = now.saturating_duration_since(s.born).as_secs_f32();
        let frac = (age / s.life).clamp(0.0, 1.0);
        let tf = if s.typing {
            if frac < 0.55 {
                1.0
            } else {
                0.5 * (1.0 + (std::f32::consts::PI * (frac - 0.55) / 0.45).cos())
            }
        } else {
            1.0 - frac
        };
        ((s.born_cov as f32) * tf * cfg.intensity) as u8
    }

    /// Build this frame's comet as runs of adjacent swept cells in the
    fn build_runs(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
    ) -> usize {
        if self.sparks.is_empty() {
            return 0;
        }
        let dx = (cfg.head_dx - 0.5) * geom.cw as f32;
        if self.comet_runs.is_empty() {
            self.comet_runs.push(Vec::new());
        }
        self.comet_live = 0;
        self.comet_runs[0].clear();
        let mut prev: Option<(u16, u16)> = None;
        let mut head_cov = 0u8;
        for s in &self.sparks {
            let cov = Self::spark_cov(s, now, cfg);
            let here = (s.row, s.col);
            if cov == 0 {
                prev = Some(here);
                continue;
            }
            if let Some((pr, pc)) = prev {
                let far = (s.row as i32 - pr as i32)
                    .abs()
                    .max((s.col as i32 - pc as i32).abs())
                    > 1;
                if far && !self.comet_runs[self.comet_live].is_empty() {
                    self.comet_live += 1;
                    if self.comet_live == self.comet_runs.len() {
                        self.comet_runs.push(Vec::new());
                    }
                    self.comet_runs[self.comet_live].clear();
                }
            }
            let (x, y) = geom.cell_center(s.row, s.col);
            self.comet_runs[self.comet_live].push(CometSample {
                x: x + dx,
                y,
                cov,
                pos: s.pos,
            });
            prev = Some(here);
            head_cov = cov;
        }
        let live = if self.comet_runs[0].is_empty() {
            0
        } else {
            self.comet_live + 1
        };
        if live == 0 {
            return 0;
        }
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && let Some(last) = self.comet_runs[..live].last_mut()
        {
            let (x, y) = geom.cell_center(cr, cc);
            last.push(CometSample {
                x: x + dx,
                y,
                cov: head_cov,
                pos: 1.0,
            });
        }
        live
    }

    fn emit_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
    ) {
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let chf = geom.ch as f32;
        let core_thick = (chf * 0.13).max(2.0);
        let straighten = (geom.cw as f32).max(chf) * 0.8;
        let clip = geom.beam_clip();

        for r in &self.comet_runs[..live] {
            if r.len() < 2 {
                let s0 = r[0];
                let s = core_thick.max(2.0) as i32;
                push_rect(
                    out,
                    geom,
                    s0.x as i32 - s / 2,
                    s0.y as i32 - s / 2,
                    s,
                    s,
                    premul_rgb(self.comet_color(cfg, s0.pos), s0.cov),
                );
                continue;
            }
            comet_glow_quads(out, clip, r, core_thick, straighten, &|pos| {
                self.comet_color(cfg, pos)
            });
        }
    }

    /// The mono face on a dark ground: the runs the dark comet builds, laid
    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry, cursor, both streams and the over-text facts"
    )]
    fn emit_mono_dark(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
        text: TextView<'_>,
        ledger: &mut InkLedger,
    ) {
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let (ink, page) = CursorGlow::bed_pair(cfg);
        let budget = crate::cursor_glow::dark_bed_budget(ink);
        let shape = Self::MONO_BED;
        let mut scratch = std::mem::take(&mut self.bed_scratch);
        scratch.inks.clear();
        scratch.rails.clear();
        let mut last: Option<(u32, u32, u32)> = None;
        for run in &self.comet_runs[..live] {
            for s in run {
                let rgb = self.comet_color(cfg, s.pos);
                let (bed_c, rail_c) = match last {
                    Some((prev, b, r)) if prev == rgb => (b, r),
                    _ => {
                        let b = crate::cursor_glow::dark_bed_ink(rgb, page, budget, shape.sat);
                        let r = crate::rainbow_kitty::ribbon::rail_ink(rgb & 0x00FF_FFFF);
                        last = Some((rgb, b, r));
                        (b, r)
                    }
                };
                scratch.inks.push(bed_c);
                scratch.rails.push(rail_c);
            }
        }
        lay_dark_bed(
            &self.comet_runs[..live],
            geom,
            page,
            shape,
            &text,
            &mut scratch,
            ledger,
            under,
        );
        self.bed_scratch = scratch;
        let caret = cur.is_some_and(|(r, c)| (r as usize) < geom.rows && (c as usize) < geom.cols);
        let n = split_air_runs(
            &self.comet_runs[..live],
            geom,
            &text,
            caret,
            &mut self.air_runs,
        );
        let chf = geom.ch as f32;
        let core_thick = (chf * 0.13).max(2.0);
        let straighten = (geom.cw as f32).max(chf) * 0.8;
        let clip = geom.beam_clip();
        let caret_cell = cur.map(|(cr, cc)| CursorGlow::caret_box(geom, cr, cc));
        for (i, r) in self.air_runs[..n].iter().enumerate() {
            let stub = (caret && i + 1 == n && matches!(r.as_slice(), [knee, _] if knee.cov == 0))
                .then_some(caret_cell)
                .flatten();
            let from = out.len();
            if r.len() < 2 {
                let s0 = r[0];
                let s = core_thick.max(2.0) as i32;
                push_rect(
                    out,
                    geom,
                    s0.x as i32 - s / 2,
                    s0.y as i32 - s / 2,
                    s,
                    s,
                    premul_rgb(self.comet_color(cfg, s0.pos), s0.cov),
                );
                continue;
            }
            comet_glow_quads(out, clip, r, core_thick, straighten, &|pos| {
                self.comet_color(cfg, pos)
            });
            if let Some(cell) = stub {
                clip_quads_to(out, from, cell);
            }
        }
    }

    /// The mono face's dark bed: a thin line through the x-height in the
    const MONO_BED: BedShape = BedShape {
        reach: 0.10,
        thin: 0.45,
        sat: 0.90,
        full: 215.0,
        rail: 0.12,
        ..BedShape::BASE
    };

    /// Whether the mono face draws as the spectrum face does on a dark
    #[cfg(test)]
    const fn mono_unbedded(&self) -> bool {
        self.mono_unbedded
    }
    #[cfg(not(test))]
    #[inline(always)]
    const fn mono_unbedded(&self) -> bool {
        false
    }

    /// Where the light-ground stroke runs, as a share of the cell height
    const LIGHT_STROKE_DROP: f32 = 0.30;
    /// The light-ground stroke's thickness, as a share of the cell height.
    const LIGHT_STROKE_THICK: f32 = 0.12;
    /// A sample's coverage to the stroke's opacity: a fresh comet reaches the
    const LIGHT_STROKE_GAIN: f32 = 1.9;

    /// The light-ground comet: the same runs the dark comet draws, each as
    fn emit_light_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        under: &mut Vec<GlowQuad>,
    ) {
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let drop = chf * Self::LIGHT_STROKE_DROP;
        let thick = (chf * Self::LIGHT_STROKE_THICK).max(1.5);
        let clip = geom.beam_clip();
        let ground = LightGround::of(cfg);
        #[cfg(test)]
        let ground = LightGround {
            unsolved: self.light_unsolved,
            ..ground
        };
        let mut verts = std::mem::take(&mut self.light_verts);
        let mut last_ink: Option<(u32, u32)> = None;
        for run in &self.comet_runs[..live] {
            if under.len() >= Self::MAX_QUADS {
                break;
            }
            verts.clear();
            let mut ink = |s: &CometSample| {
                let raw = self.comet_color(cfg, s.pos);
                let color = match last_ink {
                    Some((prev, c)) if prev == raw => c,
                    _ => {
                        let c = ground.body_ink(raw);
                        last_ink = Some((raw, c));
                        c
                    }
                };
                BeamVertex {
                    x: s.x,
                    y: s.y + drop,
                    color,
                    cov: (f32::from(s.cov) * Self::LIGHT_STROKE_GAIN).min(LIGHT_BODY_ALPHA_CAP)
                        as u8,
                }
            };
            match run.as_slice() {
                [] => continue,
                [lone] => {
                    let v = ink(lone);
                    verts.push(BeamVertex {
                        x: v.x - cwf * 0.45,
                        ..v
                    });
                    verts.push(BeamVertex {
                        x: v.x + cwf * 0.45,
                        ..v
                    });
                }
                [first, .., last] => {
                    let (ax, ay, bx, by) = (first.x, first.y, last.x, last.y);
                    let (dx, dy) = (bx - ax, by - ay);
                    let len2 = dx * dx + dy * dy;
                    let across_rows = ay != by && len2 > 0.0;
                    for s in run {
                        let mut v = ink(s);
                        if across_rows {
                            let t = (((s.x - ax) * dx + (s.y - ay) * dy) / len2).clamp(0.0, 1.0);
                            v.x = ax + dx * t;
                            v.y = ay + dy * t + drop;
                        }
                        verts.push(v);
                    }
                }
            }
            comet_beam_over(under, clip, &verts, thick, 1, 0.0);
        }
        self.light_verts = verts;
    }

    /// The crown's caret cell and fade now (1 at the move, 0 at the end of
    #[inline]
    fn crown_fade(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
    ) -> Option<((u16, u16), f32)> {
        if cfg.radius <= 0.0 {
            return None;
        }
        let (cr, cc) = cur?;
        if cr as usize >= geom.rows || cc as usize >= geom.cols {
            return None;
        }
        let age_ms = now.saturating_duration_since(self.last_move?).as_millis() as u64;
        let window = self.crown_window_ms.max(1);
        (age_ms < window).then(|| ((cr, cc), 1.0 - age_ms as f32 / window as f32))
    }

    fn emit_crown(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
    ) {
        let Some(((cr, cc), fade)) = self.crown_fade(now, cfg, geom, cur) else {
            return;
        };
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = i32::from(geom.origin_x) + cc as i32 * cw;
        let cy = i32::from(geom.origin_y) + cr as i32 * ch;
        let r = (cfg.radius * chf_of(geom)) as i32;
        let (k, base_cov, color) = (
            3i32,
            50.0f32,
            if cfg.classic_mono {
                cfg.color
            } else {
                hsv2rgb(self.hue, 0.9, 1.0)
            },
        );
        let boost = self.typing_boost();
        for layer in 0..k {
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            let grow = r * (k - layer) / k;
            let cov = (base_cov * (layer as f32 + 1.0) / k as f32 * fade * cfg.intensity * boost)
                .min(255.0) as u8;
            if cov == 0 {
                continue;
            }
            push_rect(
                out,
                geom,
                cx - grow,
                cy - grow,
                cw + 2 * grow,
                ch + 2 * grow,
                premul_rgb(color, cov),
            );
        }
    }

    /// The mono face's crown on a dark ground: one smooth radial bloom on the
    fn emit_mono_crown(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        halos: &mut Vec<RainHalo>,
    ) {
        let Some(((cr, cc), fade)) = self.crown_fade(now, cfg, geom, cur) else {
            return;
        };
        let peak = (Self::MONO_CROWN_COV * fade * cfg.intensity * self.typing_boost()).min(210.0);
        if peak < 1.0 {
            return;
        }
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let r = cfg.radius * chf;
        let (cx, cy) = geom.cell_center(cr, cc);
        push_halo_rows(
            halos,
            geom,
            (cx, cy),
            (cwf * 0.5 + r * 0.9, chf * 0.5 + r * 0.55),
            premul_rgb(cfg.color, peak as u8),
            HaloMode::Add,
        );
    }

    /// The mono crown's peak coverage at full heat: the modern crown's
    const MONO_CROWN_COV: f32 = 95.0;

    /// The light crown's peak opacity at full heat and intensity: the box
    const LIGHT_CROWN_COV: f32 = 100.0 * Self::LIGHT_STROKE_GAIN;

    /// The crown on a light ground: the box crown's bloom as ink. One round
    fn emit_light_crown(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        under: &mut Vec<GlowQuad>,
    ) {
        let Some(((cr, cc), fade)) = self.crown_fade(now, cfg, geom, cur) else {
            return;
        };
        let peak = (Self::LIGHT_CROWN_COV * fade * cfg.intensity * self.typing_boost())
            .min(LIGHT_BODY_ALPHA_CAP);
        let color = if cfg.classic_mono {
            cfg.color
        } else {
            hsv2rgb(self.hue, 0.9, 1.0)
        };
        let ground = LightGround::of(cfg);
        #[cfg(test)]
        let ground = LightGround {
            unsolved: self.light_unsolved,
            ..ground
        };
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let reach = cwf * 0.5 + cfg.radius * chf * 0.9;
        let budget = under.len().saturating_add(LIGHT_MARK_QUADS);
        push_ink_ellipse(
            under,
            geom,
            geom.cell_center(cr, cc),
            (reach, reach),
            chf * 0.5,
            ground.body_ink(color),
            peak,
            budget,
        );
    }

    /// The ring's age fraction and coverage now, `None` when there is none or
    #[inline]
    fn ring_now(&self, now: Instant, cfg: &GlowConfig) -> Option<(Ring, f32, u8)> {
        let ring = self.ring?;
        let age = now.saturating_duration_since(ring.born).as_secs_f32();
        let t = (age / ring.life).clamp(0.0, 1.0);
        let cov = (200.0 * (1.0 - t) * cfg.intensity) as u8;
        (cov > 0).then_some((ring, t, cov))
    }

    fn emit_ring(&self, now: Instant, cfg: &GlowConfig, geom: Geom, out: &mut Vec<GlowQuad>) {
        let Some((ring, t, cov)) = self.ring_now(now, cfg) else {
            return;
        };
        let premul = premul_rgb(cfg.accent, cov);
        let s = ((0.6 + 1.0 * t) * chf_of(geom)) as i32;
        let th = (geom.ch as i32 / 8).max(2);
        let (cx, cy) = geom.cell_center(ring.row, ring.col);
        let (cx, cy) = (cx as i32, cy as i32);
        push_rect(out, geom, cx - s, cy - s, 2 * s, th, premul);
        push_rect(out, geom, cx - s, cy + s - th, 2 * s, th, premul);
        push_rect(out, geom, cx - s, cy - s, th, 2 * s, premul);
        push_rect(out, geom, cx + s - th, cy - s, th, 2 * s, premul);
    }

    /// The light-ground landing ring: the modern ring's light arm (an
    fn emit_light_ring(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        halos: &mut Vec<RainHalo>,
    ) {
        let Some((ring, t, cov)) = self.ring_now(now, cfg) else {
            return;
        };
        let grow = (0.7 + 1.0 * t) * chf_of(geom);
        let veil = lerp_rgb(cfg.accent, 0x0000_0000, 0.28);
        let cap = (u32::from(cov) / 2).clamp(1, 120) as u8;
        push_soft_veil(
            halos,
            geom,
            geom.cell_center(ring.row, ring.col),
            (grow, grow * 0.85),
            veil,
            cap,
        );
    }

    /// The comet colour at path position `pos` (0 tail .. 1 head) in this
    #[inline]
    fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        comet_color(cfg.classic_mono, cfg.color, cfg.accent, self.hue, pos)
    }
}

#[inline]
const fn chf_of(geom: Geom) -> f32 {
    geom.ch as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_math::relative_luminance;
    use crate::cursor_glow::GlowStyle;
    use aterm_render::{HaloMode, halo_over_cap, over_premul};

    fn geom() -> Geom {
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

    /// The classic style's shipped defaults.
    fn cfg() -> GlowConfig {
        GlowConfig {
            enabled: true,
            style: GlowStyle::Classic,
            color: 0x0050_FA7B,
            accent: 0x0078_FFB9,
            duration: Duration::from_millis(240),
            length: 18,
            intensity: 0.7,
            audible: true,
            radius: 0.6,
            ring: true,
            dark_theme: true,
            theme_fg: 0x00C8_C8D2,
            theme_bg: 0x0010_1016,
            beam: true,
            head_dx: 0.5,
            ribbon_tall: false,
            ribbon_flat: false,
            classic_mono: false,
        }
    }

    /// GitHub Light's pair: the light ground the light arm is measured on.
    const PAPER: u32 = 0x00FF_FFFF;
    const INK: u32 = 0x001F_2328;

    /// The fixture on a light ground.
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

    /// The engine and its three output streams, ticked as `CursorGlow` does,
    #[derive(Default)]
    struct Rig {
        w: ClassicWake,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        under: Vec<GlowQuad>,
        ledger: InkLedger,
    }

    impl Rig {
        fn tick(&mut self, cur: Option<(u16, u16)>, now: Instant, c: &GlowConfig, g: Geom) -> u64 {
            self.w.tick(
                cur,
                now,
                c,
                g,
                &mut self.out,
                &mut self.halos,
                &mut self.under,
                WakeInk {
                    text: crate::cursor_glow::TextView::new(
                        crate::cursor_glow::TextSpans::default(),
                        None,
                    ),
                    ledger: &mut self.ledger,
                },
            )
        }
    }

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    /// Every emitted quad lands inside the effects box and inside one cell-row
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

    /// The run builder is resident: after warm-up the pooled slots keep their
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

    /// The classic wake spawns on cursor motion alone; an empty frame means the
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

    /// One wide glyph (CJK, emoji, a Hangul syllable) advances the caret two
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

    /// A bar caret stands at `head_dx` of its cell: the comet's samples sit
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
                let lum = f64::from(
                    ((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff),
                );
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

    /// The landing ring's stroke keeps its weight on a HiDPI cell: an eighth
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

    /// Idle-zero: the engine returns to exactly empty so the event loop can
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

    /// The master switch and a degenerate geometry both go dark at once, with
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

    /// The mono face follows the configured colour; the spectrum face does not.
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

    /// The shared colour ramp's `Classic` arm is the engine's own spectrum
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

    /// One scripted dark session, folded field-complete: every frame's
    fn dark_session_fold(c: &GlowConfig, g: Geom) -> u64 {
        use aterm_hash::fnv_mix_u64;
        let mut rig = Rig::default();
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

    /// The mono face's crown on a dark ground is one smooth round bloom: in a
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

    /// The dark wake is byte-identical to the salvage at 37b3368a2, the base
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
                11_293_809_440_103_835_944,
                12_176_010_846_753_476_077,
                8_579_084_186_203_313_172,
            ],
            "the dark classic wake drifted from its base"
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

    /// The knobs every host hands the classic wake when its user set none
    fn shipped_dark_at(length: usize, intensity: f32) -> GlowConfig {
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

    /// [`shipped_dark_at`] at the brightness a host hands Classic unset.
    fn shipped_dark(length: usize) -> GlowConfig {
        shipped_dark_at(
            length,
            crate::cursor_fx::default_trail_intensity(Some(GlowStyle::Classic), false),
        )
    }

    /// THE WAKE A HOST ACTUALLY DRIVES IS THE BASE'S: the config the one
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

    /// On a light ground the wake is ink: no additive quad at all (no box
    #[test]
    fn a_light_ground_draws_ink_under_the_letters_and_a_veil_ring_and_no_additive_light() {
        let (g, c) = (geom(), light_cfg());
        let t0 = Instant::now();
        let mut rig = Rig::default();
        rig.tick(Some((1, 4)), t0, &c, g);
        let fp = rig.tick(Some((4, 30)), ms(t0, 16), &c, g);
        assert!(rig.out.is_empty(), "a light ground emits no additive quad");
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
            rig.halos.iter().any(|h| (h.cx, h.cy)
                == (ring.0.round() as u16, ring.1.round() as u16)
                && halo_over_cap(h.color) <= 120),
            "the landing ring is a capped veil on the landing cell"
        );
    }

    /// The under-ink stream composited over `PAPER` in emission order (the
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

    /// The WCAG ratio between the mean luminance of two pixel sets.
    fn ratio(px: &[u32], a: &[usize], b: &[usize]) -> f32 {
        let mean = |set: &[usize]| {
            set.iter().map(|&i| relative_luminance(px[i])).sum::<f32>() / set.len() as f32
        };
        let (la, lb) = (mean(a), mean(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// A TYPED RUN ON A LIGHT GROUND IS AN UNDERLINE INSIDE ITS ROW: every
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
                    usize::from(q.y) >= top + g.ch / 2
                        && usize::from(q.y) + usize::from(q.h) <= bot,
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

    /// THE CROWN ON A LIGHT GROUND IS ONE ROUND INK MARK ON THE CARET'S ROW:
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

    /// A CTRL-L JUMP ON A LIGHT GROUND IS ONE CONNECTED STROKE: the caret
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

    /// The wake rides scrolled content: with the prompt on the bottom row of
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

    /// Light and memory carried off the grid's top are dropped, never
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

    /// After a coordinate-space change the next tick draws no comet from the old space.
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
}
