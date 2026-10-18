// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `classic` cursor style: a thin four-layer anti-aliased comet along the
//! swept cell path under a three-rectangle box-stack bloom crown, with an
//! expanding square ring on a jump. It spawns on observed cursor motion, so a
//! screen-crossing jump draws a long diagonal comet.
//!
//! A separate engine rather than a style arm of [`crate::cursor_glow`], whose
//! momentum, admission, ribbon and thermal layers would each have to be
//! defeated to reproduce it. Driven from one branch in
//! [`crate::cursor_glow::CursorGlow::tick`]; on a dark ground every quad is
//! window-absolute additive light (`alpha: 0`).
//!
//! Additive light cannot show on a light ground, so a light theme takes the
//! beam family's light arm instead: the comet becomes darkened source-over
//! veils hugging the row's lower edge ([`ClassicWake::halos`]), the crown is
//! skipped, and the landing ring is a veil ring.

use std::time::Duration;

use aterm_time::Instant;

use aterm_render::{CometSample, GlowQuad, RainHalo, comet_glow_quads, premul_rgb};

use crate::cursor_glow::{Geom, GlowConfig, hsv2rgb, push_halo_over};
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
    /// observed inter-key cadence during sustained typing, the full comet
    /// `duration` for a jump.
    life: f32,
    /// Typing spark (else jump): picks the fade curve in `emit_comet`.
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

/// The classic wake's animation state; no other style touches it.
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
    /// The crown window the last move applied: [`Self::CROWN_TYPING_MS`] for a
    /// single-cell advance, [`Self::CROWN_MS`] for a jump.
    crown_window_ms: u64,
    /// Rolling hue phase (turns), advanced per spawn.
    hue: f32,
    /// Typing-cadence heat 0..1, decayed lazily from `heat_at`, so idle gaps
    /// cost nothing and never keep the animation timer armed.
    heat: f32,
    heat_at: Option<Instant>,
    /// Last typing advance, for the inter-key cadence.
    last_type: Option<Instant>,
    /// Resident run-builder pool, so the comet reuses its nested buffers every
    /// redraw. The pool is never `clear`ed and its slots are never moved out:
    /// clearing a `Vec<Vec<_>>` drops every inner buffer and `mem::take` leaves
    /// a builder with no capacity, either of which reallocates every frame.
    comet_runs: Vec<Vec<CometSample>>,
    /// Index of the run being filled. `comet_live + 1` slots are live once
    /// anything is pushed; a split only leaves a non-empty run behind, so no
    /// live slot is empty.
    comet_live: usize,
    /// This frame's light-theme veils (source-over [`RainHalo`]s), empty on a
    /// dark ground. The host splices them beside the glow quads.
    halos: Vec<RainHalo>,
    /// The move this frame's tick spawned on, `(from, to)`, for the host's
    /// diagnosis ring ([`Self::take_spawn`]). `None` on a frame that spawned
    /// nothing.
    spawned: Option<((u16, u16), (u16, u16))>,
}

impl ClassicWake {
    /// Hard cap on emitted quads (defends the renderer + the per-frame upload).
    const MAX_QUADS: usize = 8192;
    /// Hard cap on live swept-path samples. `cfg.length` bounds one move; this
    /// bounds an accumulation of moves at the same instant.
    const MAX_SPARKS: usize = 512;
    /// Bloom-crown fade window (the crown follows the head this long post-move).
    const CROWN_MS: u64 = 200;
    /// Typing crown window: at human cadence (≤~350 ms between keys) the crown
    /// never lapses mid-sentence.
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
    /// observed cadence, so the streak is continuous at any human rhythm.
    const CHAIN_GAP_MAX: f32 = 0.40;
    /// Chained life = observed gap + this margin…
    const CHAIN_MARGIN: f32 = 0.10;
    /// …capped here so the post-stop tail stays crisp.
    const CHAIN_LIFE_MAX: f32 = 0.50;

    /// Wake brightness multiplier for typing light at the current heat: cool
    /// typing ~0.28×, sustained fast typing ~1.2×. Jump comets and rings are
    /// navigation feedback and stay at 1×.
    fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// Any light still alive → keep the animation timer armed.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.sparks.is_empty() || self.ring.is_some() || self.crown_until.is_some()
    }

    /// The move the last tick spawned on, `(from, to)`, once: the classic
    /// wake lights every observed move, typed or not, and the host records it
    /// in the same spawn count and diagnosis ring the other styles fill.
    pub fn take_spawn(&mut self) -> Option<((u16, u16), (u16, u16))> {
        self.spawned.take()
    }

    /// This frame's light-theme veils, for `RenderInput.glow_halo`.
    #[must_use]
    pub fn halos(&self) -> &[RainHalo] {
        &self.halos
    }

    /// Drop all in-flight light and forget the last cursor position, used when
    /// the cursor's coordinate space changes, so no comet spawns from a stale
    /// cross-space position.
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

    /// Advance one frame: observe the cursor at `cur`, spawn on a move, decay,
    /// and emit the light into `out`. Returns a fingerprint that changes on
    /// every visible change (0 when empty).
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
    ) -> u64 {
        // `CursorGlow::tick` has already cleared `out` before the classic
        // branch; clearing here keeps the engine correct when driven directly
        // (its tests, a bench, an embedder).
        out.clear();
        self.halos.clear();
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
            self.spawned = Some(((pr, pc), (cr, cc)));
            self.last_move = Some(now);
            self.crown_until = Some(now + Duration::from_millis(self.crown_window_ms));
        }
        self.last = cur;

        // Decay everything to exactly empty. Each spark fades on its own
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

        self.emit_comet(now, cfg, geom, cur, out);
        out.truncate(Self::MAX_QUADS);
        if out.len() < Self::MAX_QUADS {
            self.emit_crown(now, cfg, geom, cur, out);
        }
        if out.len() < Self::MAX_QUADS {
            self.emit_ring(now, cfg, geom, out);
        }
        out.truncate(Self::MAX_QUADS);
        self.halos.truncate(Self::MAX_QUADS);

        // Fingerprint the emitted light (deterministic given the live state).
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
        for h in &self.halos {
            for word in h.damage_words() {
                fp = fp.wrapping_mul(1_000_003).wrapping_add(word);
            }
        }
        fp
    }

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
        // A single-cell advance is typing; a multi-cell delta is a jump.
        let dist = (cr as i32 - pr as i32)
            .abs()
            .max((cc as i32 - pc as i32).abs()) as f32;
        let typing = dist <= 1.0;
        // Typing cadence to heat: each keystroke earns credit by how quickly it
        // followed the previous one, so only sustained fast typing ramps up.
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
        // Typing gets a short, tight wake so the cursor reads as leading; a jump
        // gets the full comet duration. Heat stretches the typing wake, and in a
        // rhythm the life chains to the observed cadence so the streak does not
        // pulse per key.
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
        // Build the comet as runs of adjacent swept cells, so a stale older
        // group never connects across empty space. A faded tail cell breaks the
        // colour, not the run.
        if self.comet_runs.is_empty() {
            self.comet_runs.push(Vec::new());
        }
        self.comet_live = 0;
        self.comet_runs[0].clear();
        let mut prev: Option<(u16, u16)> = None;
        let mut head_cov = 0u8;
        for s in &self.sparks {
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            // Typing sparks hold full brightness for 55% of life then
            // cosine-fade, so the chained streak stays lit across the inter-key
            // gap; jump sparks fade linearly.
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
                if far && !self.comet_runs[self.comet_live].is_empty() {
                    // Step to the next pool slot, growing the pool only when
                    // this frame needs more runs than any frame before it.
                    self.comet_live += 1;
                    if self.comet_live == self.comet_runs.len() {
                        self.comet_runs.push(Vec::new());
                    }
                    self.comet_runs[self.comet_live].clear();
                }
            }
            let (x, y) = geom.cell_center(s.row, s.col);
            self.comet_runs[self.comet_live].push(CometSample {
                x,
                y,
                cov,
                pos: s.pos,
            });
            prev = Some(here);
            head_cov = cov;
        }
        // A split never leaves an empty run behind it, so slot 0 being empty is
        // the one and only "nothing was swept" case.
        let live = if self.comet_runs[0].is_empty() {
            0
        } else {
            self.comet_live + 1
        };
        if live == 0 {
            return;
        }
        // Connect the head run to the live cursor cell so the beam attaches to
        // the cursor with no trailing 1-cell gap.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && let Some(last) = self.comet_runs[..live].last_mut()
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
        if !cfg.dark_theme {
            // Light ground: a darkened veil per swept sample along the row's
            // lower edge, merging into one tracer that leaves the glyph band
            // clear (the beam family's light law, one rail for this thin look).
            let cwf = geom.cw as f32;
            let hue = self.hue;
            for run in &self.comet_runs[..live] {
                for s in run {
                    let peak = (f32::from(s.cov) * 1.9).min(230.0) as u8;
                    let ink = lerp_rgb(comet_color_at(hue, cfg, s.pos), 0x0000_0000, 0.38);
                    push_halo_over(
                        &mut self.halos,
                        geom,
                        s.x,
                        s.y + chf * 0.5,
                        cwf * 1.55,
                        chf * 0.34,
                        ink,
                        peak,
                    );
                }
            }
            return;
        }
        let core_thick = (chf * 0.13).max(2.0); // crisp thin core
        // Flatten the per-cell Bresenham path to the true straight move so the
        // beam is a clean diagonal, not a stair-stepped polyline of cell centres.
        let straighten = (geom.cw as f32).max(chf) * 0.8;
        let clip = geom.beam_clip();

        for r in &self.comet_runs[..live] {
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
            // The layered-bloom comet look is `aterm_render`'s, shared with the
            // modern styles.
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
        // Pure additive light: invisible on a light ground, and it would lift
        // fresh ink toward it. Skipped there, as the modern styles skip theirs.
        if !cfg.dark_theme {
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
        // Box-stack bloom: K concentric rectangles whose additive overlap
        // brightens the centre (a soft square halo, not the modern radial one).
        let (k, base_cov, color) = (
            3i32,
            50.0f32,
            if cfg.classic_mono {
                cfg.color
            } else {
                hsv2rgb(self.hue, 0.9, 1.0)
            },
        );
        // The crown follows typing heat: barely visible at rest, full only under
        // sustained fast typing, so a steady-state glow is not distracting.
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

    fn emit_ring(&mut self, now: Instant, cfg: &GlowConfig, geom: Geom, out: &mut Vec<GlowQuad>) {
        let Some(ring) = self.ring else { return };
        let age = now.saturating_duration_since(ring.born).as_secs_f32();
        let t = (age / ring.life).clamp(0.0, 1.0);
        let fade = 1.0 - t;
        let cov = (200.0 * fade * cfg.intensity) as u8;
        if cov == 0 {
            return;
        }
        if !cfg.dark_theme {
            // Light ground: an expanding darkened veil ring, its centre alpha
            // capped so it never buries text (the modern ring's light arm).
            let grow = (0.6 + 1.0 * t) * chf_of(geom);
            let veil = lerp_rgb(cfg.accent, 0x0000_0000, 0.28);
            let cap = (u32::from(cov) / 2).clamp(1, 120);
            push_halo_over(
                &mut self.halos,
                geom,
                ring.cx,
                ring.cy,
                grow,
                grow * 0.85,
                (veil & 0x00FF_FFFF) | (cap << 24),
                255,
            );
            return;
        }
        let premul = premul_rgb(cfg.accent, cov);
        // Expanding square outline: half-size grows ~0.6 → ~1.6 cells, emitted
        // as top/bottom and left/right bars (push_rect splits the verticals into
        // per-row slabs), ~2 px thick.
        let s = ((0.6 + 1.0 * t) * chf_of(geom)) as i32;
        let th = 2i32;
        let cx = ring.cx as i32;
        let cy = ring.cy as i32;
        push_rect(out, geom, cx - s, cy - s, 2 * s, th, premul);
        push_rect(out, geom, cx - s, cy + s - th, 2 * s, th, premul);
        push_rect(out, geom, cx - s, cy - s, th, 2 * s, premul);
        push_rect(out, geom, cx + s - th, cy - s, th, 2 * s, premul);
    }

    /// The comet colour at path position `pos` (0 tail .. 1 head): a rolling
    /// spectrum independent of the theme, or for `classic mono` a two-tone
    /// `accent → color` tracer that follows the cursor colour and OSC 12.
    fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        comet_color_at(self.hue, cfg, pos)
    }
}

/// [`ClassicWake::comet_color`] for a given rolling `hue`, so an emitter can
/// borrow the engine's buffers while it colours.
fn comet_color_at(hue: f32, cfg: &GlowConfig, pos: f32) -> u32 {
    if cfg.classic_mono {
        lerp_rgb(cfg.accent, cfg.color, pos)
    } else {
        hsv2rgb((hue + pos * 0.5).fract(), 0.95, 1.0)
    }
}

#[inline]
fn chf_of(geom: Geom) -> f32 {
    geom.ch as f32
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

    /// Every emitted quad lands inside the effects box and inside one cell-row
    /// band, as the row-scoped damage gate and GPU scissor require.
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

    /// The run builder is resident: after warm-up the pooled slots keep their
    /// capacity at the top of every frame. Asserting capacity rather than
    /// allocation count needs no global allocator in the lib test binary.
    #[test]
    fn the_comet_run_pool_keeps_its_buffers_between_frames() {
        let (g, c) = (geom(), cfg());
        let mut w = ClassicWake::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        // A typing run across a row split by a jump, so more than one run is
        // live: the builder's worst case.
        let frame = |w: &mut ClassicWake, out: &mut Vec<GlowQuad>, k: u64| {
            let now = t0 + Duration::from_millis(k * 8);
            // The fixture grid is 6 rows x 40 cols; stay inside it or nothing
            // spawns at all.
            let col = u16::try_from((k % 30) + 2).expect("column fits");
            // Alternate rows so the comet breaks into adjacent runs.
            let row = if k.is_multiple_of(7) { 4 } else { 2 };
            w.tick(Some((row, col)), now, &c, g, out);
        };
        for k in 0..120 {
            frame(&mut w, &mut out, k);
        }
        let pooled = |w: &ClassicWake| -> usize { w.comet_runs.iter().map(Vec::capacity).sum() };
        let warm = pooled(&w);
        assert!(
            warm > 0,
            "fixture: the builder must have built something to pool"
        );
        assert!(
            !w.comet_runs.is_empty(),
            "fixture: the pool holds at least one run buffer"
        );
        // Stated on `len()`, not capacity alone: a pool never shrinks, so its
        // slot count is the high-water run count, while a clear-and-rebuild
        // shrinks it on a frame with fewer runs.
        let mut slots_low = usize::MAX;
        let mut slots_high = 0usize;
        let mut cap_low = usize::MAX;
        for k in 120..600 {
            frame(&mut w, &mut out, k);
            slots_low = slots_low.min(w.comet_runs.len());
            slots_high = slots_high.max(w.comet_runs.len());
            cap_low = cap_low.min(pooled(&w));
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
    /// classic branch went through the admission seam.
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

    /// Idle-zero: the engine returns to exactly empty so the event loop can
    /// park and the animation timer disarm.
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

    /// The master switch and a degenerate geometry both go dark at once, with
    /// no residue left to re-light the next frame.
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

    /// The mono face follows the configured colour; the spectrum face does not.
    /// Asserted over a typing advance: the landing ring is `cfg.accent` in both.
    #[test]
    fn the_mono_face_follows_the_configured_colour_and_the_spectrum_does_not() {
        let g = geom();
        // A typing advance: comet + crown, and deliberately no landing ring.
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
        // Both faces are the same geometry: only the colours moved.
        assert_eq!(
            mono_green.len(),
            typed(false, GREEN.0, GREEN.1).len(),
            "the two faces differ in colour alone"
        );
    }

    /// Additive light vanishes on white, so a light ground lays the wake as
    /// source-over veils instead: a typing run and a jump both leave veils and
    /// no glow quads, in both faces, and the frame still fingerprints.
    #[test]
    fn a_light_ground_lays_source_over_veils_instead_of_light() {
        let g = geom();
        let mut c = cfg();
        c.dark_theme = false;
        c.theme_fg = 0x0024_292F;
        c.theme_bg = 0x00FF_FFFF;
        for mono in [false, true] {
            c.classic_mono = mono;
            let mut w = ClassicWake::default();
            let mut out = Vec::new();
            let t0 = Instant::now();
            let mut fp = 0;
            for k in 0..=8u16 {
                fp = w.tick(
                    Some((2, 4 + k)),
                    t0 + Duration::from_millis(70 * u64::from(k)),
                    &c,
                    g,
                    &mut out,
                );
            }
            assert!(out.is_empty(), "no additive light on a light ground");
            assert!(
                !w.halos().is_empty(),
                "a typing run leaves a veil (mono={mono})"
            );
            assert!(
                w.halos()
                    .iter()
                    .all(|h| h.mode == aterm_render::HaloMode::Over),
                "every light-ground mark is source-over"
            );
            assert_ne!(fp, 0, "a veiled frame fingerprints non-zero");
            w.tick(
                Some((4, 34)),
                t0 + Duration::from_millis(640),
                &c,
                g,
                &mut out,
            );
            assert!(out.is_empty());
            assert!(!w.halos().is_empty(), "a jump leaves its veil and ring");
        }
        // Negative control: the same run on a dark ground is additive light.
        let dark = cfg();
        let mut w = ClassicWake::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        w.tick(Some((2, 4)), t0, &dark, g, &mut out);
        w.tick(
            Some((4, 34)),
            t0 + Duration::from_millis(16),
            &dark,
            g,
            &mut out,
        );
        assert!(!out.is_empty() && w.halos().is_empty());
    }

    /// After a coordinate-space change the next tick draws no comet from the old space.
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
