// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `classic` cursor style: a thin four-layer anti-aliased comet along the swept cell path under a three-rectangle box-stack bloom crown, with an expanding square ring on a jump.

use std::time::Duration;

use aterm_time::Instant;

use aterm_render::{
    BeamRows, BeamVertex, CometSample, GlowQuad, HaloMode, RainHalo, RampSink, comet_beam_over,
    comet_glow_quads, premul_rgb,
};

use crate::cursor_glow::{
    BedScratch, BedShape, CursorGlow, Geom, GlowConfig, InkLedger, LIGHT_BODY_ALPHA_CAP,
    LIGHT_MARK_QUADS, LightGround, TextView, clip_stream_to, hsv2rgb, lay_dark_bed, push_halo_rows,
    push_ink_ellipse, push_soft_veil, split_air_runs,
};
use crate::effect_util::{lerp_rgb, push_fx_rect as push_rect};
use crate::trail_sweep::line_cells_tail;

#[derive(Clone, Copy)]
struct Spark {
    row: u16,
    col: u16,
    born_cov: u8,
    pos: f32,
    life: f32,
    typing: bool,
    born: Instant,
}

#[derive(Clone, Copy)]
struct Ring {
    row: u16,
    col: u16,
    born: Instant,
    life: f32,
    caret: (u16, u16),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct WakeLightKey {
    rgb: u32,
    fg: u32,
    bg: u32,
}

/// The classic wake's animation state.
pub struct ClassicWake {
    sparks: Vec<Spark>,
    path_scratch: Vec<(i32, i32)>,
    ring: Option<Ring>,
    last: Option<(u16, u16)>,
    last_move: Option<Instant>,
    crown_until: Option<Instant>,
    crown_window_ms: u64,
    hue: f32,
    heat: f32,
    heat_at: Option<Instant>,
    last_type: Option<Instant>,
    comet_runs: Vec<Vec<CometSample>>,
    comet_live: usize,
    light_verts: Vec<BeamVertex>,
    /// [`tracer_on_caret_row`]'s resident vertices.
    light_dense: Vec<BeamVertex>,
    spawned: Option<((u16, u16), (u16, u16))>,
    bed_scratch: BedScratch,
    air_runs: Vec<Vec<CometSample>>,
    crown_ink_memo: std::cell::Cell<Option<(WakeLightKey, u32)>>,
    veil_cap_memo: std::cell::Cell<Option<(WakeLightKey, u8)>>,
    /// Test-only: draw the mono face on a dark ground as additive light over the letters.
    #[cfg(test)]
    pub(crate) mono_unbedded: bool,
    /// Test-only: lay the light-ground stroke and crown in their unsolved vivid ink.
    #[cfg(test)]
    pub(crate) light_unsolved: bool,
    /// Test-only: lay the light-ground landing veil unbounded by the caret's cell and row.
    #[cfg(test)]
    pub(crate) veil_unbound: bool,
    /// Test-only: lay a paper Enter's tracer across the rows it crosses, as
    /// it was (looks7 M8's negative control).
    #[cfg(test)]
    pub(crate) tracer_over_letters: bool,
    /// Test-only: the span a paper Enter's tracer fades out over leaving the
    /// caret's row, px, in place of [`TRACER_LEAVE_SHARE`] of the row
    /// (looks7 round 2's control: a cut).
    #[cfg(test)]
    pub(crate) tracer_leave_px: Option<f32>,
    /// Test-only: a paper tracer kept to the caret's row eases in from the
    /// row's top whichever edge it enters by, as it did (looks7 round 3's
    /// review's control: a run from below is cut at the row's bottom).
    #[cfg(test)]
    pub(crate) tracer_top_only: bool,
}

impl Default for ClassicWake {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// Over-text state for the mono face's dark bed.
pub(crate) struct WakeInk<'a> {
    /// Probed and observed text spans for the frame.
    pub(crate) text: TextView<'a>,
    /// The frame's over-ink account.
    pub(crate) ledger: &'a mut InkLedger,
}

/// The share of a cell's height over which a paper Enter's tracer fades out
/// as it leaves the caret's row ([`tracer_on_caret_row`]).
const TRACER_LEAVE_SHARE: f32 = 0.6;

/// How near a lettered cell off the caret's row a paper tracer may pass,
/// px past its stroke's half width, and still be drawn whole.
const TRACER_CLEAR_PX: f32 = 3.0;

/// The longest step between a paper Enter's tracer's vertices once kept to
/// the caret's row, px: its fade eases over a few px, not over a cell's run
/// between two samples.
const TRACER_STEP_PX: f32 = 2.0;

/// A paper tracer across rows (`verts`, the run from the old caret to the
/// new one) that would pass a lettered cell off the caret's row `caret_row`
/// keeps to the caret's row. Laid straight across the rows at the
/// baseline's drop, an Enter's tracer struck through the printed line
/// (`brown fox` in the gallery's `enter-bottom`, looks7 M8); passed behind
/// each lettered row instead, it broke into a stub at the caret and a dash
/// floating past the printed line's end (looks7 round 2). Each vertex, the
/// line cut to [`TRACER_STEP_PX`] steps, keeps its light on the caret's row
/// only, its stroke's `half` width inside the row, easing in from the edge
/// the run enters the row by (its top for a run from above, as an Enter's;
/// its bottom for one from below, as a move up a page) over `leave` px, or
/// over the room between that edge and the caret's stroke where that is
/// less: one tail rising from the new caret toward the line it left, which
/// fades out before it leaves the row. Eased in from the top alone, a run
/// from below kept a full stub at the caret cut at the row's bottom (looks7
/// round 3's review). A run whose rows are clear of letters is drawn whole,
/// one stroke across them; without a caret, a run that crosses letters
/// keeps no light.
fn tracer_on_caret_row(
    (verts, dense): (&mut Vec<BeamVertex>, &mut Vec<BeamVertex>),
    text: TextView<'_>,
    geom: Geom,
    caret_row: Option<i32>,
    (half, leave, top_only): (f32, f32, bool),
) {
    if verts.len() < 2 {
        return;
    }
    let (ox, oy) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    dense.clear();
    for w in verts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let n = (crate::color_math::hypot_f32(b.x - a.x, b.y - a.y) * (1.0 / TRACER_STEP_PX))
            .ceil()
            .max(1.0) as u32;
        let inv_n = 1.0 / n as f32;
        for i in 0..n {
            let t = i as f32 * inv_n;
            dense.push(BeamVertex {
                x: a.x + (b.x - a.x) * t,
                y: a.y + (b.y - a.y) * t,
                color: lerp_rgb(a.color, b.color, t),
                cov: (f32::from(a.cov) + (f32::from(b.cov) - f32::from(a.cov)) * t).round() as u8,
            });
        }
    }
    if let Some(&last_v) = verts.last() {
        dense.push(last_v);
    }
    let reach = half + TRACER_CLEAR_PX;
    let reach2 = reach * reach;
    let (inv_cw, inv_ch) = (1.0 / cw, 1.0 / ch);
    let mut crosses = false;
    'verts: for v in dense.iter() {
        let (row, col) = (
            ((v.y - oy) * inv_ch).floor() as i32,
            ((v.x - ox) * inv_cw).floor() as i32,
        );
        for r in row - 1..=row + 1 {
            if Some(r) == caret_row {
                continue;
            }
            for c in col - 1..=col + 1 {
                if !(text.covers_cell(r, c) || text.covers_cell(r, c - 1)) {
                    continue;
                }
                let (x0, y0) = (ox + c as f32 * cw, oy + r as f32 * ch);
                let dx = (x0 - v.x).max(v.x - (x0 + cw)).max(0.0);
                let dy = (y0 - v.y).max(v.y - (y0 + ch)).max(0.0);
                if dx * dx + dy * dy < reach2 {
                    crosses = true;
                    break 'verts;
                }
            }
        }
    }
    if !crosses {
        return;
    }
    let Some(row) = caret_row else {
        for v in dense.iter_mut() {
            v.cov = 0;
        }
        std::mem::swap(verts, dense);
        return;
    };
    let (top, bottom) = (oy + row as f32 * ch, oy + (row + 1) as f32 * ch);
    // The edge the run enters the caret's row by: where it lies walking back
    // from the caret, the run's last vertex.
    let mut from_below = false;
    if !top_only {
        let mut idx = dense.len();
        while idx > 0 {
            idx -= 1;
            let v = &dense[idx];
            if v.y - half < top || v.y + half > bottom {
                from_below = v.y + half > bottom;
                break;
            }
        }
    }
    let room = match dense.last() {
        Some(v) if from_below => bottom - (v.y + half),
        Some(v) => v.y - half - top,
        None => leave,
    };
    let span = leave.min(room).max(1.0);
    let inv_span = 1.0 / span;
    for v in dense.iter_mut() {
        let k = if v.y - half < top || v.y + half > bottom {
            0.0
        } else {
            let inside = if from_below {
                bottom - (v.y + half)
            } else {
                v.y - half - top
            };
            let u = (inside * inv_span).clamp(0.0, 1.0);
            u * u * (3.0 - 2.0 * u)
        };
        v.cov = (f32::from(v.cov) * k).round() as u8;
    }
    std::mem::swap(verts, dense);
}

/// Returns the comet colour at path position `pos` (`0` tail to `1` head).
#[must_use]
#[inline]
pub const fn comet_color(mono: bool, color: u32, accent: u32, hue: f32, pos: f32) -> u32 {
    if mono {
        lerp_rgb(accent, color, pos)
    } else {
        hsv2rgb((hue + pos * 0.5) % 1.0, 0.95, 1.0)
    }
}

impl ClassicWake {
    const MAX_QUADS: usize = 8192;
    const MAX_SPARKS: usize = 512;
    const CROWN_MS: u64 = 200;
    const CROWN_TYPING_MS: u64 = 350;
    const HEAT_GAIN: f32 = 0.16;
    const HEAT_DECAY_TAU: f32 = 0.9;
    const HEAT_GAP_FULL: f32 = 0.09;
    const HEAT_GAP_ZERO: f32 = 0.40;
    const CHAIN_GAP_MAX: f32 = 0.40;
    const CHAIN_MARGIN: f32 = 0.10;
    const CHAIN_LIFE_MAX: f32 = 0.50;

    /// Fresh dormant classic-wake animation state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sparks: Vec::new(),
            path_scratch: Vec::new(),
            ring: None,
            last: None,
            last_move: None,
            crown_until: None,
            crown_window_ms: 0,
            hue: 0.0,
            heat: 0.0,
            heat_at: None,
            last_type: None,
            comet_runs: Vec::new(),
            comet_live: 0,
            light_verts: Vec::new(),
            light_dense: Vec::new(),
            spawned: None,
            bed_scratch: BedScratch::EMPTY,
            air_runs: Vec::new(),
            crown_ink_memo: std::cell::Cell::new(None),
            veil_cap_memo: std::cell::Cell::new(None),
            #[cfg(test)]
            mono_unbedded: false,
            #[cfg(test)]
            light_unsolved: false,
            #[cfg(test)]
            veil_unbound: false,
            #[cfg(test)]
            tracer_over_letters: false,
            #[cfg(test)]
            tracer_leave_px: None,
            #[cfg(test)]
            tracer_top_only: false,
        }
    }

    #[inline]
    const fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// Returns whether any wake light is still alive.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        !self.sparks.is_empty() || self.ring.is_some() || self.crown_until.is_some()
    }

    /// Takes the `(from, to)` move the last tick spawned on.
    #[inline]
    pub const fn take_spawn(&mut self) -> Option<((u16, u16), (u16, u16))> {
        self.spawned.take()
    }

    /// Returns whether the engine remembers a cursor cell.
    #[must_use]
    #[inline]
    pub const fn has_last(&self) -> bool {
        self.last.is_some()
    }

    /// Returns the number of live swept-path samples.
    #[must_use]
    #[inline]
    pub const fn live_sparks(&self) -> usize {
        self.sparks.len()
    }

    /// Returns the live sparks' cells.
    #[cfg(test)]
    pub(crate) fn spark_cells(&self) -> Vec<(u16, u16)> {
        self.sparks.iter().map(|s| (s.row, s.col)).collect()
    }

    /// Returns the `(first, last)` row range spanned by live sparks.
    #[inline]
    pub(crate) const fn row_range(&self) -> Option<(u16, u16)> {
        let sparks = self.sparks.as_slice();
        if sparks.is_empty() {
            return None;
        }
        let mut lo = sparks[0].row;
        let mut hi = lo;
        let mut i = 1;
        while i < sparks.len() {
            let r = sparks[i].row;
            if r < lo {
                lo = r;
            }
            if r > hi {
                hi = r;
            }
            i += 1;
        }
        Some((lo, hi))
    }

    /// Returns the landing ring's cell.
    #[cfg(test)]
    pub(crate) const fn ring_cell(&self) -> Option<(u16, u16)> {
        match self.ring {
            Some(r) => Some((r.row, r.col)),
            None => None,
        }
    }

    /// Drops all in-flight light and forgets the last cursor position.
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

    /// Translates row-addressed members under `map`, dropping any that leave the grid.
    pub fn translate_rows(&mut self, map: &impl Fn(u16) -> Option<u16>) {
        self.sparks.retain_mut(|s| match map(s.row) {
            Some(row) => {
                s.row = row;
                true
            }
            None => false,
        });
        if let Some(r) = self.ring {
            self.ring = map(r.row).map(|row| Ring { row, ..r });
        }
        if let Some((row, col)) = self.last {
            self.last = map(row).map(|r| (r, col));
        }
    }

    /// Advances one frame and emits wake quads and halos.
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
        out: &mut RampSink<'_>,
        halos: &mut Vec<RainHalo>,
        under: &mut RampSink<'_>,
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
            const INV_HEAT_DECAY_TAU: f32 = 1.0 / ClassicWake::HEAT_DECAY_TAU;
            let dt = now.saturating_duration_since(t0).as_secs_f32();
            if dt > 0.0 {
                self.heat *= crate::color_math::exp_f32(-dt * INV_HEAT_DECAY_TAU);
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

        let mut last_alive: Option<((Instant, u32), bool)> = None;
        self.sparks.retain(|s| {
            let key = (s.born, s.life.to_bits());
            match last_alive {
                Some((prev_key, alive)) if prev_key == key => alive,
                _ => {
                    let alive = now.saturating_duration_since(s.born).as_secs_f32() < s.life;
                    last_alive = Some((key, alive));
                    alive
                }
            }
        });
        if self.sparks.len() > Self::MAX_SPARKS {
            let drop = self.sparks.len() - Self::MAX_SPARKS;
            self.sparks.drain(0..drop);
        }
        if let Some(r) = self.ring
            && now.saturating_duration_since(r.born).as_secs_f32() >= r.life
        {
            self.ring = None;
        }
        if matches!(self.crown_until, Some(t) if now >= t) {
            self.crown_until = None;
        }

        if cfg.dark_theme && cfg.classic_mono && !self.mono_unbedded() {
            self.emit_mono_dark(now, cfg, geom, cur, out, under, ink.text, ink.ledger);
            out.keep_laid(Self::MAX_QUADS);
            under.keep_laid(Self::MAX_QUADS);
            let from = halos.len();
            self.emit_mono_crown(now, cfg, geom, cur, halos);
            ink.ledger
                .admit_halos_masked(halos, from, InkLedger::HEAD_FLOOR);
            if out.laid() < Self::MAX_QUADS {
                let from = out.quads().len();
                self.emit_ring(now, cfg, geom, out.quads());
                let to = out.quads().len();
                ink.ledger
                    .admit_masked(out.quads(), from..to, InkLedger::FLYING_FLOOR);
            }
            out.keep_laid(Self::MAX_QUADS);
        } else if cfg.dark_theme {
            self.emit_comet(now, cfg, geom, cur, out);
            out.keep_laid(Self::MAX_QUADS);
            if out.laid() < Self::MAX_QUADS {
                self.emit_crown(now, cfg, geom, cur, out.quads());
            }
            if out.laid() < Self::MAX_QUADS {
                self.emit_ring(now, cfg, geom, out.quads());
            }
            out.keep_laid(Self::MAX_QUADS);
        } else {
            self.emit_light_crown(now, cfg, geom, cur, under.quads());
            self.emit_light_comet(now, cfg, geom, cur, under, ink.text);
            under.keep_laid(Self::MAX_QUADS);
            if let Some(ring) = &mut self.ring
                && let Some((r, c)) = cur
                && usize::from(r) < geom.rows
                && usize::from(c) < geom.cols
            {
                ring.caret = (r, c);
            }
            self.emit_light_ring(now, cfg, geom, cur, halos);
            halos.truncate(CursorGlow::MAX_HALOS);
        }

        let mut fp: u64 = 0;
        for q in out.quads().iter() {
            fp = fp.wrapping_mul(1_000_003).wrapping_add(
                ((q.row as u64) << 40)
                    ^ ((q.x as u64) << 28)
                    ^ ((q.y as u64) << 16)
                    ^ ((q.w as u64) << 8)
                    ^ (q.h as u64)
                    ^ ((q.color as u64) << 20),
            );
        }
        for q in under.quads().iter() {
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
        for ramps in [&*out.ramps(), &*under.ramps()] {
            for r in &ramps.items {
                fp = fp.wrapping_mul(1_000_003).wrapping_add(
                    (u64::from(r.at) << 32)
                        ^ (u64::from(r.row) << 40)
                        ^ (u64::from(r.x) << 28)
                        ^ (u64::from(r.y) << 16)
                        ^ (u64::from(r.w) << 8)
                        ^ u64::from(r.h)
                        ^ (u64::from(r.joined) << 48)
                        ^ (u64::from(r.join) << 56),
                );
                let ink = r.ink as usize;
                for e in ramps.ends[ink..ink + r.end_count()].as_chunks::<2>().0 {
                    fp = fp
                        .wrapping_mul(1_000_003)
                        .wrapping_add(u64::from(e[0]) << 32 | u64::from(e[1]));
                }
            }
        }
        if under.laid() > 0 || !halos.is_empty() {
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
            let gap = match self.last_type {
                Some(t) => now.saturating_duration_since(t).as_secs_f32(),
                None => f32::MAX,
            };
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
            let (rows_i32, cols_i32) = (geom.rows as i32, geom.cols as i32);
            for (i, &(r, c)) in self.path_scratch.iter().enumerate() {
                if r < 0 || c < 0 || r >= rows_i32 || c >= cols_i32 {
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
                caret: (cr, cc),
            });
        }
    }

    #[inline]
    fn spark_tf(s: &Spark, now: Instant) -> f32 {
        const PI_OVER_045: f32 = std::f32::consts::PI / 0.45;
        let age = now.saturating_duration_since(s.born).as_secs_f32();
        let frac = (age / s.life).clamp(0.0, 1.0);
        if s.typing {
            if frac < 0.55 {
                1.0
            } else {
                0.5 * (1.0 + crate::supernova::sin_cos_f32((frac - 0.55) * PI_OVER_045).1)
            }
        } else {
            1.0 - frac
        }
    }

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
        let mut last_tf: Option<((Instant, u32, bool), f32)> = None;
        for s in &self.sparks {
            let key = (s.born, s.life.to_bits(), s.typing);
            let tf = match last_tf {
                Some((prev_key, cached)) if prev_key == key => cached,
                _ => {
                    let cached = Self::spark_tf(s, now);
                    last_tf = Some((key, cached));
                    cached
                }
            };
            let cov = (f32::from(s.born_cov) * tf * cfg.intensity) as u8;
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
        out: &mut RampSink<'_>,
    ) {
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let chf = geom.ch as f32;
        let core_thick = (chf * 0.13).max(2.0);
        let s = core_thick as i32;
        let straighten = (geom.cw as f32).max(chf) * 0.8;
        let clip = geom.beam_clip();

        for r in &self.comet_runs[..live] {
            if r.len() < 2 {
                let s0 = r[0];
                push_rect(
                    out.quads(),
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
        out: &mut RampSink<'_>,
        under: &mut RampSink<'_>,
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
        let air = crate::cursor_glow::AirLight::of(page, budget, shape);
        let chf = geom.ch as f32;
        let lit_page = crate::cursor_glow::page_lit(page, budget);
        let shape = if lit_page { shape.on_lit_page() } else { shape };
        let mut scratch = std::mem::take(&mut self.bed_scratch);
        scratch.inks.clear();
        scratch.rails.clear();
        scratch.rail_drop = if lit_page {
            crate::cursor_glow::lit_rail_drop(chf)
        } else {
            0.0
        };
        let mut last: Option<(u32, bool, u32, u32)> = None;
        let mut lit = std::mem::take(&mut scratch.air);
        for run in &self.comet_runs[..live] {
            crate::cursor_glow::air_lit(run, geom, &text, (air.is_some(), false), &mut lit);
            for (s, &in_air) in run.iter().zip(&lit) {
                let rgb = self.comet_color(cfg, s.pos);
                let (bed_c, rail_c) = match last {
                    Some((prev, was_air, b, r)) if prev == rgb && was_air == in_air => (b, r),
                    _ => {
                        let b = match air {
                            Some(air) if in_air => air.ink(rgb, page, shape.sat),
                            _ => crate::cursor_glow::dark_bed_ink(rgb, page, budget, shape.sat),
                        };
                        let r = crate::rainbow_kitty::ribbon::rail_ink(rgb & 0x00FF_FFFF);
                        last = Some((rgb, in_air, b, r));
                        (b, r)
                    }
                };
                scratch.inks.push(bed_c);
                scratch.rails.push(rail_c);
            }
        }
        scratch.air = lit;
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
        let caret = matches!(
            cur,
            Some((r, c)) if (r as usize) < geom.rows && (c as usize) < geom.cols
        );
        let n = split_air_runs(
            &self.comet_runs[..live],
            geom,
            &text,
            caret,
            None,
            &mut self.air_runs,
        );
        let chf = geom.ch as f32;
        let core_thick = (chf * 0.13).max(2.0);
        let s = core_thick as i32;
        let straighten = (geom.cw as f32).max(chf) * 0.8;
        let clip = geom.beam_clip();
        let caret_cell = cur.map(|(cr, cc)| CursorGlow::caret_box(geom, cr, cc));
        for (i, r) in self.air_runs[..n].iter().enumerate() {
            let stub = if caret && i + 1 == n && matches!(r.as_slice(), [knee, _] if knee.cov == 0)
            {
                caret_cell
            } else {
                None
            };
            let from = out.mark();
            if r.len() < 2 {
                let s0 = r[0];
                push_rect(
                    out.quads(),
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
                clip_stream_to(out, from, cell);
            }
        }
    }

    const MONO_BED: BedShape = BedShape {
        reach: 0.10,
        thin: 0.45,
        sat: 0.90,
        full: 215.0,
        rail: 0.12,
        ..BedShape::BASE
    };

    #[cfg(test)]
    const fn mono_unbedded(&self) -> bool {
        self.mono_unbedded
    }
    #[cfg(not(test))]
    #[inline(always)]
    const fn mono_unbedded(&self) -> bool {
        false
    }

    const LIGHT_STROKE_DROP: f32 = 0.30;
    const LIGHT_STROKE_THICK: f32 = 0.12;
    const LIGHT_STROKE_GAIN: f32 = 1.9;

    fn emit_light_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        under: &mut RampSink<'_>,
        text: TextView<'_>,
    ) {
        #[cfg(test)]
        let kept = !self.tracer_over_letters;
        #[cfg(not(test))]
        let kept = true;
        let caret_row = cur.map(|(r, _)| i32::from(r));
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let mut dense = std::mem::take(&mut self.light_dense);
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        #[cfg(test)]
        let leave = self.tracer_leave_px.unwrap_or(chf * TRACER_LEAVE_SHARE);
        #[cfg(not(test))]
        let leave = chf * TRACER_LEAVE_SHARE;
        #[cfg(test)]
        let top_only = self.tracer_top_only;
        #[cfg(not(test))]
        let top_only = false;
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
            if under.laid() >= Self::MAX_QUADS {
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
                    let dx = cwf * 0.45;
                    verts.push(BeamVertex { x: v.x - dx, ..v });
                    verts.push(BeamVertex { x: v.x + dx, ..v });
                }
                [first, .., last] => {
                    let (ax, ay, bx, by) = (first.x, first.y, last.x, last.y);
                    let (dx, dy) = (bx - ax, by - ay);
                    let len2 = dx * dx + dy * dy;
                    let across_rows = ay != by && len2 > 0.0;
                    let inv_len2 = if across_rows { 1.0 / len2 } else { 0.0 };
                    for s in run {
                        let mut v = ink(s);
                        if across_rows {
                            let t =
                                (((s.x - ax) * dx + (s.y - ay) * dy) * inv_len2).clamp(0.0, 1.0);
                            v.x = ax + dx * t;
                            v.y = ay + dy * t + drop;
                        }
                        verts.push(v);
                    }
                    if across_rows && kept {
                        tracer_on_caret_row(
                            (&mut verts, &mut dense),
                            text,
                            geom,
                            caret_row,
                            (0.5 * thick, leave, top_only),
                        );
                    }
                }
            }
            comet_beam_over(under, clip, &verts, thick, 1, 0.0);
        }
        self.light_verts = verts;
        self.light_dense = dense;
    }

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
        if age_ms < window {
            Some(((cr, cc), 1.0 - age_ms as f32 / window as f32))
        } else {
            None
        }
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
        let k_f = k as f32;
        for layer in 0..k {
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            let grow = r * (k - layer) / k;
            let cov = (base_cov * (layer as f32 + 1.0) / k_f * fade * cfg.intensity * boost)
                .min(255.0) as u8;
            if cov == 0 {
                continue;
            }
            let two_grow = 2 * grow;
            push_rect(
                out,
                geom,
                cx - grow,
                cy - grow,
                cw + two_grow,
                ch + two_grow,
                premul_rgb(color, cov),
            );
        }
    }

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

    const MONO_CROWN_COV: f32 = 95.0;

    const LIGHT_CROWN_COV: f32 = 100.0 * Self::LIGHT_STROKE_GAIN;

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
        #[cfg(not(test))]
        let unsolved = false;
        #[cfg(test)]
        let unsolved = self.light_unsolved;
        let body_ink = if unsolved {
            color & 0x00FF_FFFF
        } else {
            let key = WakeLightKey {
                rgb: color,
                fg: cfg.theme_fg,
                bg: cfg.theme_bg,
            };
            match self.crown_ink_memo.get() {
                Some((k, v)) if k == key => v,
                _ => {
                    let v = LightGround::of(cfg).body_ink(color);
                    self.crown_ink_memo.set(Some((key, v)));
                    v
                }
            }
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
            body_ink,
            peak,
            budget,
        );
    }

    #[inline]
    fn ring_now(&self, now: Instant, cfg: &GlowConfig) -> Option<(Ring, f32, u8)> {
        let ring = self.ring?;
        let age = now.saturating_duration_since(ring.born).as_secs_f32();
        let t = (age / ring.life).clamp(0.0, 1.0);
        let cov = (200.0 * (1.0 - t) * cfg.intensity) as u8;
        if cov > 0 { Some((ring, t, cov)) } else { None }
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
        let (x0, y0) = (cx - s, cy - s);
        let two_s = 2 * s;
        push_rect(out, geom, x0, y0, two_s, th, premul);
        push_rect(out, geom, x0, cy + s - th, two_s, th, premul);
        push_rect(out, geom, x0, y0, th, two_s, premul);
        push_rect(out, geom, cx + s - th, y0, th, two_s, premul);
    }

    fn emit_light_ring(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        halos: &mut Vec<RainHalo>,
    ) {
        let Some((ring, t, cov)) = self.ring_now(now, cfg) else {
            return;
        };
        let grow = (0.7 + 1.0 * t) * chf_of(geom);
        let veil = lerp_rgb(cfg.accent, 0x0000_0000, 0.28);
        let cap = (u32::from(cov) / 2).clamp(1, 120) as u8;
        #[cfg(not(test))]
        let unbound = false;
        #[cfg(test)]
        let unbound = self.veil_unbound;
        let on_grid = match cur {
            Some((r, c)) if usize::from(r) < geom.rows && usize::from(c) < geom.cols => {
                Some((r, c))
            }
            _ if usize::from(ring.caret.0) < geom.rows && usize::from(ring.caret.1) < geom.cols => {
                Some(ring.caret)
            }
            _ => None,
        };
        let (centre, radii, cap) = match on_grid {
            Some((r, c)) if !unbound => {
                let (cw, ch) = (geom.cw as f32, chf_of(geom));
                let key = WakeLightKey {
                    rgb: veil,
                    fg: cfg.theme_fg,
                    bg: cfg.theme_bg,
                };
                let veil_cap = match self.veil_cap_memo.get() {
                    Some((k, v)) if k == key => v,
                    _ => {
                        let v = CursorGlow::light_veil_cap(veil, cfg.theme_fg, cfg.theme_bg);
                        self.veil_cap_memo.set(Some((key, v)));
                        v
                    }
                };
                (
                    geom.cell_center(r, c),
                    (
                        grow.min(CursorGlow::LIGHT_VEIL_REACH_CELLS * cw),
                        (grow * 0.85).min(0.5 * ch),
                    ),
                    cap.min(veil_cap),
                )
            }
            _ if unbound => (
                geom.cell_center(ring.row, ring.col),
                (grow, grow * 0.85),
                cap,
            ),
            _ => return,
        };
        push_soft_veil(halos, geom, centre, radii, veil, cap);
    }

    #[inline]
    const fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
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

    const fn cfg() -> GlowConfig {
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

    const PAPER: u32 = 0x00FF_FFFF;
    const INK: u32 = 0x001F_2328;

    const fn light_cfg() -> GlowConfig {
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
                    let w = ClassicWake {
                        ring: Some(Ring {
                            row: caret.0 - 1,
                            col: caret.1,
                            born,
                            life: 0.4,
                            caret,
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
                                    (((veil >> sh) & 0xff) * a
                                        + ((px >> sh) & 0xff) * (255 - a)
                                        + 127)
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
    /// (`a_vertical_jump_on_light_is_one_connected_stroke`).
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
}
