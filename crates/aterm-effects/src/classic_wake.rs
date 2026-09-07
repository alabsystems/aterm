// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLASSIC WAKE — the v0.28 cursor aurora, salvaged.
//!
//! This is the trail aterm shipped as its DEFAULT at tag `v0.28`
//! (`8e7fe4f6f`, 2026-07-12): a thin four-layer anti-aliased comet along the
//! swept cell path under a three-rectangle box-stack bloom crown, with an
//! expanding square landing ring on a jump. It reads as *restraint* — a fine
//! spectrum-swept tracer that leads the caret and gets out of the way — and
//! that is exactly what the grown-up [`crate::cursor_glow`] engine no longer
//! draws: today's `phaser` is a fat saturated band, and today's shape gates
//! mint NO geometry at all from a screen-crossing jump (`off-shape`), so the
//! long diagonal comet that made the old trail feel fast is simply gone.
//!
//! **WHY A SEPARATE ENGINE RATHER THAN A STYLE ARM.** The v0.28 look is not a
//! parameterization of today's emitter; it is the emitter aterm had before
//! ~64k lines of momentum, admission, ribbon, scintillation and thermal work
//! grew around it. Reproducing it by configuring the modern path would mean
//! defeating each of those in turn. So the salvage keeps its OWN state and its
//! OWN emit, driven from one branch in [`crate::cursor_glow::CursorGlow::tick`]
//! — which is also what makes the other ten built-in styles provably untouched
//! (they never reach this file, and this file reads none of their state).
//!
//! **WHAT IS FAITHFUL AND WHAT IS ADAPTED.** The laws are v0.28's, constant for
//! constant: the heat accumulator (gain 0.16, τ 0.9 s, the 0.09→0.40 s cadence
//! window), the per-spark lifetime with its typing/chaining/jump cases, the
//! `40 + 175·pos` birth-coverage ramp, the 3-layer crown at 350 ms (typing) /
//! 200 ms (jump), the 0.18 s ring, and the shared `comet_glow_quads` layer
//! stack — whose `COMET_LAYERS` values are byte-identical to v0.28's, so the
//! beam rasterizes exactly as it did. Three things are ADAPTED, because the
//! surrounding contract changed and fidelity to a dead convention would be a
//! bug: quads are emitted in WINDOW-ABSOLUTE pixels through
//! [`push_fx_rect`]/[`Geom::beam_clip`] (v0.28 emitted grid-interior, before
//! the effects box existed), every quad carries `alpha: 0` (the additive mode
//! selector [`aterm_render::GlowQuad::alpha`] postdates v0.28), and the clock
//! is [`aterm_time::Instant`] (the workspace retired `web_time`).

use std::time::Duration;

use aterm_time::Instant;

use aterm_render::{CometSample, GlowQuad, comet_glow_quads, premul_rgb};

use crate::cursor_glow::{Geom, GlowConfig};
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
    /// Fade lifetime in seconds — short for a lone typing advance, CHAINED to
    /// the observed inter-key cadence during sustained typing, and the full
    /// comet `duration` for a real jump.
    life: f32,
    /// TYPING spark? Typing sparks hold full brightness for 55% of life then
    /// cosine-fade; jump sparks keep the classic linear fade.
    typing: bool,
    born: Instant,
}

/// One expanding landing ring.
#[derive(Clone, Copy)]
struct Ring {
    cx: f32,
    cy: f32,
    born: Instant,
    life: f32,
}

/// The v0.28 wake's animation state — self-contained, and touched by no other
/// style. Every field is v0.28's, minus the ones that served effects this
/// engine does not draw (particles, the water wake, the nyan ribbon).
#[derive(Default)]
pub struct ClassicWake {
    sparks: Vec<Spark>,
    /// Resident, bounded swept-cell scratch, built backward from the landing
    /// point so an outlier jump never walks or allocates the discarded prefix.
    path_scratch: Vec<(i32, i32)>,
    ring: Option<Ring>,
    /// Last observed cursor cell, to detect a move.
    last: Option<(u16, u16)>,
    /// When the cursor last moved (drives the bloom-crown fade around the head).
    last_move: Option<Instant>,
    /// Deadline until which the crown is still emitted (cached so [`Self::is_active`]
    /// needs no clock); keeps the timer armed past comet decay.
    crown_until: Option<Instant>,
    /// The crown window the LAST move applied: [`Self::CROWN_TYPING_MS`] for a
    /// single-cell advance, [`Self::CROWN_MS`] for a jump.
    crown_window_ms: u64,
    /// Rolling hue phase (turns), advanced per spawn.
    hue: f32,
    /// Typing-cadence HEAT 0..1, decayed lazily from `heat_at` so idle gaps cost
    /// nothing and never keep the animation timer armed.
    heat: f32,
    heat_at: Option<Instant>,
    /// Last TYPING advance, for the inter-key cadence.
    last_type: Option<Instant>,
    /// Resident run-builder scratch, so the animated comet reuses one nested
    /// Vec instead of allocating a fresh one every redraw.
    comet_runs: Vec<Vec<CometSample>>,
    comet_run: Vec<CometSample>,
}

impl ClassicWake {
    /// Hard cap on emitted quads (defends the renderer + the per-frame upload).
    const MAX_QUADS: usize = 8192;
    /// Hard cap on live swept-path samples. `cfg.length` bounds one move; this
    /// bounds an accumulation of moves at the same instant.
    const MAX_SPARKS: usize = 512;
    /// Bloom-crown fade window (the crown follows the head this long post-move).
    const CROWN_MS: u64 = 200;
    /// TYPING crown window: at human cadence (≤~350 ms between keys) the crown
    /// never lapses mid-sentence, so the stream stays non-empty across the gap.
    const CROWN_TYPING_MS: u64 = 350;
    /// Heat earned by one keystroke at full cadence (≈7 fast keys to full heat).
    const HEAT_GAIN: f32 = 0.16;
    /// Exponential heat cool-down time constant (seconds to ~37%).
    const HEAT_DECAY_TAU: f32 = 0.9;
    /// Inter-key gap (seconds) at or under which a keystroke earns full heat…
    const HEAT_GAP_FULL: f32 = 0.09;
    /// …and the gap beyond which it earns none (relaxed typing stays cool).
    const HEAT_GAP_ZERO: f32 = 0.40;
    /// TYPING CONTINUITY: at or under this gap the spark's life chains to the
    /// observed cadence, so the streak is continuous at any human rhythm.
    const CHAIN_GAP_MAX: f32 = 0.40;
    /// Chained life = observed gap + this margin…
    const CHAIN_MARGIN: f32 = 0.10;
    /// …capped here so the post-stop tail stays crisp.
    const CHAIN_LIFE_MAX: f32 = 0.50;

    /// The wake brightness multiplier for TYPING light at the current heat: cool
    /// typing is a whisper (~0.28×), sustained fast typing overdrives past the
    /// configured intensity (~1.2×). Jump comets and rings are navigation
    /// feedback, not typing, so they stay at 1×.
    fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// Any light still alive → keep the animation timer armed.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.sparks.is_empty() || self.ring.is_some() || self.crown_until.is_some()
    }

    /// Drop all in-flight light and forget the last cursor position — used when
    /// the cursor's coordinate space changes out from under the animator, so the
    /// next tick cannot spawn a comet from a stale cross-space position.
    pub fn reset(&mut self) {
        self.sparks.clear();
        self.ring = None;
        self.last = None;
        self.last_move = None;
        self.crown_until = None;
        self.crown_window_ms = Self::CROWN_MS;
        self.heat = 0.0;
        self.heat_at = None;
        self.last_type = None;
    }

    /// Advance one frame: observe the cursor at `cur`, spawn on a move, decay,
    /// and emit the current light into `out`, returning a fingerprint that
    /// changes on every visible change (0 when empty).
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
    ) -> u64 {
        // v0.28's tick opened by clearing the frame, and so does this one. In
        // production [`crate::cursor_glow::CursorGlow::tick`] has already
        // cleared `out` before it reaches the classic branch, so this is a
        // no-op there — but it is what makes the engine correct when driven
        // directly (its own tests, a bench, an embedder), which a salvaged
        // module that is the SINGLE definition of a shipped look ought to be.
        out.clear();
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

        // Cool the typing heat lazily (correct across arbitrary idle gaps, and
        // free while the animator is disarmed).
        if let Some(t0) = self.heat_at {
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
            let dist = (cr.abs_diff(pr)).max(cc.abs_diff(pc));
            self.crown_window_ms = if dist <= 1 {
                Self::CROWN_TYPING_MS
            } else {
                Self::CROWN_MS
            };
            self.spawn(pr, pc, cr, cc, now, cfg, geom);
            self.last_move = Some(now);
            self.crown_until = Some(now + Duration::from_millis(self.crown_window_ms));
        }
        self.last = cur;

        // Decay everything to EXACTLY empty. Each spark fades on its OWN
        // lifetime, so a fast typing wake never lingers as a smear.
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

        // ---- emit the light layers ----
        self.emit_comet(now, cfg, geom, cur, out);
        out.truncate(Self::MAX_QUADS);
        if out.len() < Self::MAX_QUADS {
            self.emit_crown(now, cfg, geom, cur, out);
        }
        if out.len() < Self::MAX_QUADS {
            self.emit_ring(now, cfg, geom, out);
        }
        out.truncate(Self::MAX_QUADS);

        // Fingerprint the emitted quads (deterministic given the live state).
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
        fp
    }

    // ----- spawning -----

    #[allow(
        clippy::too_many_arguments,
        reason = "from/to cursor cells + clock + config + geometry; packing them into a struct would only obscure a single internal call site"
    )]
    fn spawn(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) {
        // A single-cell advance is TYPING; a multi-cell delta is a real JUMP.
        let dist = (cr as i32 - pr as i32)
            .abs()
            .max((cc as i32 - pc as i32).abs()) as f32;
        let typing = dist <= 1.0;
        // Typing cadence → HEAT: each keystroke earns credit by how quickly it
        // followed the previous one, so only SUSTAINED fast typing ramps the
        // wake up and the ramp reads as acceleration.
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
        // Typing → a short, tight wake so the cursor reads as LEADING; a real
        // jump → the full comet duration. Heat stretches the typing wake, and
        // during a rhythm the life CHAINS to the observed cadence so the streak
        // is continuous instead of pulsing per key with dark rests.
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

        // Comet path: swept cells from origin toward the destination.
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
                let pos = (i as f32 + 1.0) / n; // tail→head 0..1
                // Faint tail → bright head, scaled by the typing heat (jump = 1×);
                // heat can overdrive past the static curve, so clamp into u8.
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

        // Advance the rolling hue a little each move.
        self.hue = (self.hue + 0.07).fract();

        // Landing ring on a real jump (>1 cell), if enabled.
        if cfg.ring && dist >= 2.0 {
            let (cx, cy) = geom.cell_center(cr, cc);
            self.ring = Some(Ring {
                cx,
                cy,
                born: now,
                life: 0.18,
            });
        }
    }

    // ----- emitting -----

    fn emit_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
    ) {
        if self.sparks.is_empty() {
            return;
        }
        // Build the comet as RUNS of ADJACENT swept cells, so a stale older
        // group never connects across empty space. A fully-faded tail cell
        // breaks the colour, not the run.
        self.comet_runs.clear();
        self.comet_run.clear();
        let mut prev: Option<(u16, u16)> = None;
        let mut head_cov = 0u8;
        for s in &self.sparks {
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            // TYPING sparks hold full brightness for 55% of life then
            // cosine-fade, so the chained streak stays luminous across the
            // inter-key gap; jump sparks keep the classic linear fade.
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
            let cov = ((s.born_cov as f32) * tf * cfg.intensity) as u8;
            let here = (s.row, s.col);
            if cov == 0 {
                prev = Some(here); // keep continuity so the next cell isn't a false split
                continue;
            }
            if let Some((pr, pc)) = prev {
                let far = (s.row as i32 - pr as i32)
                    .abs()
                    .max((s.col as i32 - pc as i32).abs())
                    > 1;
                if far && !self.comet_run.is_empty() {
                    self.comet_runs.push(std::mem::take(&mut self.comet_run));
                }
            }
            let (x, y) = geom.cell_center(s.row, s.col);
            self.comet_run.push(CometSample {
                x,
                y,
                cov,
                pos: s.pos,
            });
            prev = Some(here);
            head_cov = cov;
        }
        if !self.comet_run.is_empty() {
            self.comet_runs.push(std::mem::take(&mut self.comet_run));
        }
        if self.comet_runs.is_empty() {
            return;
        }
        // Connect the head run to the LIVE cursor cell so the beam visibly
        // attaches to the cursor (no trailing 1-cell gap → reads as responsive).
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && let Some(last) = self.comet_runs.last_mut()
        {
            let (x, y) = geom.cell_center(cr, cc);
            last.push(CometSample {
                x,
                y,
                cov: head_cov,
                pos: 1.0,
            });
        }

        let chf = geom.ch as f32;
        let core_thick = (chf * 0.13).max(2.0); // crisp thin core
        // Flatten the per-cell Bresenham path to the true straight move so the
        // beam is a clean diagonal, not a stair-stepped polyline of cell centres.
        let straighten = (geom.cw as f32).max(chf) * 0.8;
        let clip = geom.beam_clip();

        for r in &self.comet_runs {
            if r.len() < 2 {
                // A lone swept cell: a small soft dot, not a full blocky cell.
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
            // The layered-bloom comet look lives ONCE in `aterm_render`, shared
            // with the modern styles and the render demos.
            comet_glow_quads(out, clip, r, core_thick, straighten, &|pos| {
                self.comet_color(cfg, pos)
            });
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
        if cfg.radius <= 0.0 {
            return;
        }
        let Some((cr, cc)) = cur else { return };
        if cr as usize >= geom.rows || cc as usize >= geom.cols {
            return;
        }
        let Some(t0) = self.last_move else { return };
        let age_ms = now.saturating_duration_since(t0).as_millis() as u64;
        let window = self.crown_window_ms.max(1);
        if age_ms >= window {
            return;
        }
        let fade = 1.0 - age_ms as f32 / window as f32;
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = i32::from(geom.origin_x) + cc as i32 * cw;
        let cy = i32::from(geom.origin_y) + cr as i32 * ch;
        let r = (cfg.radius * chf_of(geom)) as i32;
        // Box-stack bloom: K concentric rectangles, additive overlap brightens
        // the centre. This is the v0.28 crown — a soft square halo, not the
        // modern radial one.
        let (k, base_cov, color) = (
            3i32,
            50.0f32,
            if cfg.classic_mono {
                cfg.color
            } else {
                hsv2rgb(self.hue, 0.9, 1.0)
            },
        );
        // The crown follows the TYPING heat: barely-there at rest, full-bright
        // only under sustained fast typing — a steady-state glow around the
        // cursor is what reads as "distracting", so it earns its brightness.
        let boost = self.typing_boost();
        for layer in 0..k {
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            // layer 0 = widest/faintest, k-1 = tightest/strongest.
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

    fn emit_ring(&self, now: Instant, cfg: &GlowConfig, geom: Geom, out: &mut Vec<GlowQuad>) {
        let Some(ring) = self.ring else { return };
        let age = now.saturating_duration_since(ring.born).as_secs_f32();
        let t = (age / ring.life).clamp(0.0, 1.0);
        let fade = 1.0 - t;
        let cov = (200.0 * fade * cfg.intensity) as u8;
        if cov == 0 {
            return;
        }
        let premul = premul_rgb(cfg.accent, cov);
        // Expanding square outline: half-size grows from ~0.6 to ~1.6 cells;
        // emitted as top/bottom bars + left/right bars (push_rect splits the
        // verticals into per-row slabs). Thickness ~2px.
        let s = ((0.6 + 1.0 * t) * chf_of(geom)) as i32;
        let th = 2i32;
        let cx = ring.cx as i32;
        let cy = ring.cy as i32;
        push_rect(out, geom, cx - s, cy - s, 2 * s, th, premul);
        push_rect(out, geom, cx - s, cy + s - th, 2 * s, th, premul);
        push_rect(out, geom, cx - s, cy - s, th, 2 * s, premul);
        push_rect(out, geom, cx + s - th, cy - s, th, 2 * s, premul);
    }

    /// The comet colour at path position `pos` (0 tail .. 1 head).
    ///
    /// Two faces, both v0.28's. The default is its PHASER ramp — a rolling
    /// spectrum that owes nothing to the theme. `classic mono` is its LUMEN
    /// ramp, a two-tone tracer fading `accent → color`, which is the face that
    /// follows the theme's cursor colour, `cursor_trail_color`, and live
    /// OSC 12. Same geometry, same laws; only this closure differs.
    fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        if cfg.classic_mono {
            lerp_rgb(cfg.accent, cfg.color, pos)
        } else {
            hsv2rgb((self.hue + pos * 0.5).fract(), 0.95, 1.0)
        }
    }
}

#[inline]
fn chf_of(geom: Geom) -> f32 {
    geom.ch as f32
}

/// v0.28's HSV→packed-RGB, carried here verbatim so the salvaged palette cannot
/// drift with the modern engine's colour work.
fn hsv2rgb(h: f32, s: f32, v: f32) -> u32 {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let u = |c: f32| ((c.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32;
    (u(r) << 16) | (u(g) << 8) | u(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::GlowStyle;

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

    /// v0.28's own fixture: the shipped defaults of the release being restored.
    fn cfg() -> GlowConfig {
        GlowConfig {
            enabled: true,
            style: GlowStyle::Classic,
            color: 0x0050_FA7B,
            accent: 0x0078_FFB9,
            duration: Duration::from_millis(240),
            length: 18,
            intensity: 0.7,
            radius: 0.6,
            ring: true,
            dark_theme: true,
            theme_fg: 0x00C8_C8D2,
            theme_bg: 0x0010_1016,
            beam: true,
            head_dx: 0.5,
            pack: None,
            wake_persist_s: 0.0,
            ribbon_tall: false,
            classic_mono: false,
        }
    }

    /// Every emitted quad lands inside the effects box and inside ONE cell-row
    /// band — the debt every emitter owes the row-scoped damage gate and the
    /// GPU scissor. A separate engine is exactly the kind that drifts out of
    /// this unnoticed, so it is asserted here as well as in the shared sweep.
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
            assert_eq!(q.alpha, 0, "the classic wake is ADDITIVE light only");
        }
    }

    /// THE DEFINING PROPERTY, stated as a test. Every modern style draws
    /// nothing for a move no keystroke licensed; the salvage draws the comet,
    /// because v0.28 spawned on observed cursor MOTION and that is what the
    /// restoration is for. If this ever starts returning an empty frame, the
    /// classic branch has been routed back through the admission seam and the
    /// screen-crossing jump comet is gone again.
    #[test]
    fn the_classic_wake_spawns_on_motion_alone_with_no_license() {
        let (g, c) = (geom(), cfg());
        let mut w = ClassicWake::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        w.tick(Some((2, 4)), t0, &c, g, &mut out);
        // No `note_typed`, no hint, no license of any kind — just a move.
        let fp = w.tick(
            Some((4, 34)),
            t0 + Duration::from_millis(16),
            &c,
            g,
            &mut out,
        );
        assert!(!out.is_empty(), "a licence-free jump must still light");
        assert_ne!(fp, 0, "a lit frame fingerprints non-zero");
        assert_banded(&out, g);
    }

    /// A single-cell advance is TYPING: a short, tight wake that reads as the
    /// caret LEADING rather than dragging.
    #[test]
    fn a_typing_advance_lights_a_short_wake() {
        let (g, c) = (geom(), cfg());
        let mut w = ClassicWake::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        w.tick(Some((2, 4)), t0, &c, g, &mut out);
        w.tick(
            Some((2, 5)),
            t0 + Duration::from_millis(16),
            &c,
            g,
            &mut out,
        );
        assert!(!out.is_empty(), "a typed advance lights");
        assert_banded(&out, g);
    }

    /// IDLE-ZERO. The whole engine must return to EXACTLY empty so the event
    /// loop can park at 0% — the property that lets the animation timer disarm.
    #[test]
    fn the_wake_decays_to_exactly_empty_and_disarms() {
        let (g, c) = (geom(), cfg());
        let mut w = ClassicWake::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        w.tick(Some((2, 4)), t0, &c, g, &mut out);
        w.tick(
            Some((4, 34)),
            t0 + Duration::from_millis(16),
            &c,
            g,
            &mut out,
        );
        assert!(w.is_active());
        let fp = w.tick(Some((4, 34)), t0 + Duration::from_secs(4), &c, g, &mut out);
        assert!(out.is_empty(), "the wake decays to EXACTLY empty");
        assert_eq!(fp, 0, "an empty frame fingerprints zero");
        assert!(!w.is_active(), "and the animation timer disarms");
    }

    /// The engine is clockless: the same relative schedule must fold to the
    /// same frame every run, or the whole-frame golden that pins this style
    /// against the shipped v0.28 build would be meaningless.
    #[test]
    fn the_same_schedule_folds_identically() {
        let (g, c) = (geom(), cfg());
        let run = || {
            let mut w = ClassicWake::default();
            let mut out = Vec::new();
            let t0 = Instant::now();
            let mut acc = Vec::new();
            acc.push(w.tick(Some((2, 4)), t0, &c, g, &mut out));
            for k in 1..=6u64 {
                acc.push(w.tick(
                    Some((2, 4 + k as u16)),
                    t0 + Duration::from_millis(70 * k),
                    &c,
                    g,
                    &mut out,
                ));
            }
            acc.push(w.tick(
                Some((4, 34)),
                t0 + Duration::from_millis(600),
                &c,
                g,
                &mut out,
            ));
            acc
        };
        assert_eq!(run(), run(), "the classic wake is deterministic");
    }

    /// The master switch and a degenerate geometry both mean DARK NOW, with no
    /// residue left to re-light the next frame.
    #[test]
    fn disabled_returns_the_engine_to_rest() {
        let (g, mut c) = (geom(), cfg());
        let mut w = ClassicWake::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        w.tick(Some((2, 4)), t0, &c, g, &mut out);
        w.tick(
            Some((4, 34)),
            t0 + Duration::from_millis(16),
            &c,
            g,
            &mut out,
        );
        assert!(w.is_active());
        c.enabled = false;
        let fp = w.tick(
            Some((4, 34)),
            t0 + Duration::from_millis(32),
            &c,
            g,
            &mut out,
        );
        assert_eq!(fp, 0);
        assert!(out.is_empty());
        assert!(!w.is_active(), "disabled leaves nothing in flight");
    }

    /// THE TWO FACES. Same engine, same geometry, same quad count — only the
    /// colour closure differs. The mono face must actually FOLLOW the
    /// configured colour, which is the thing the spectrum cannot do and the
    /// whole reason the face is offered.
    ///
    /// The spectrum's independence is asserted over a TYPING advance, not a
    /// jump, and that is not a convenience: v0.28's landing RING is drawn in
    /// `cfg.accent` for every style, so even the spectrum face's ring follows
    /// the theme. Faithful, and worth pinning as a fact rather than
    /// discovering again — the first version of this test claimed the whole
    /// spectrum frame owed nothing to the theme, and the ring proved it wrong.
    #[test]
    fn the_mono_face_follows_the_configured_colour_and_the_spectrum_does_not() {
        let g = geom();
        // A typing advance: comet + crown, and deliberately NO landing ring.
        let typed = |mono: bool, color: u32, accent: u32| {
            let mut c = cfg();
            c.classic_mono = mono;
            c.color = color;
            c.accent = accent;
            let mut w = ClassicWake::default();
            let mut out = Vec::new();
            let t0 = Instant::now();
            w.tick(Some((2, 4)), t0, &c, g, &mut out);
            w.tick(
                Some((2, 5)),
                t0 + Duration::from_millis(16),
                &c,
                g,
                &mut out,
            );
            out.iter().map(|q| q.color).collect::<Vec<_>>()
        };
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
        // Both faces are the SAME geometry: only the colours moved.
        assert_eq!(
            mono_green.len(),
            typed(false, GREEN.0, GREEN.1).len(),
            "the two faces differ in colour alone"
        );
    }

    /// A coordinate-space change (a split-pane transition) must not let the
    /// next tick draw a comet from a position in the space that just died.
    #[test]
    fn reset_forgets_the_cursor_so_no_cross_space_comet_spawns() {
        let (g, c) = (geom(), cfg());
        let mut w = ClassicWake::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        w.tick(Some((2, 4)), t0, &c, g, &mut out);
        w.reset();
        assert!(!w.is_active());
        let fp = w.tick(
            Some((5, 30)),
            t0 + Duration::from_millis(16),
            &c,
            g,
            &mut out,
        );
        assert_eq!(fp, 0, "the first tick after a reset only re-observes");
        assert!(out.is_empty());
    }
}
