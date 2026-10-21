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
//! [`crate::cursor_glow::CursorGlow::tick`]; it reads no other style's state.
//! On a dark ground the spectrum face's every quad is window-absolute additive
//! light (`alpha: 0`), exactly as the original drew it. The mono face lays its
//! tracer under the letters it crosses as a dark bed in the cursor colour,
//! solved so the letters read at the bar over it (the beam family's bed,
//! [`crate::cursor_glow::lay_dark_bed`]), keeps the additive tracer over air
//! and onto the caret, wears one round bloom for a crown (the spectrum
//! face's box crown read as a stepped square around a thin tracer), and its
//! crown and landing ring spend the frame's over-ink account where they
//! reach a letter: additive light over the newest letters left them at 2:1. On a light ground, which the original never
//! handled, it draws in ink: the
//! same comet path as one thin source-over stroke beneath the letters in the
//! under-ink stream, shifted down to the row's lower band (a typed run reads
//! as an underline in the comet's colours; a jump is one straight stroke
//! along the move), the crown as one round ink mark beneath the letters
//! inside the caret's row, and the landing ring as a darkened source-over
//! veil in the halo stream.
//!
//! Beyond the original: its rows ride `note_scroll` / `note_band_move`
//! ([`ClassicWake::translate_rows`]), so an Enter at the bottom of a full
//! screen sweeps the true diagonal; a same-row two-column step (one wide
//! glyph) is typing, not a jump; the comet's samples sit at `cfg.head_dx` of
//! their cells, so the head ends on a bar caret; and the ring's stroke is a
//! share of the cell height, at least 2 px. None of these moves the spectrum
//! face's dark output of a block caret on a cell under 24 px that never
//! scrolls or types a wide glyph (`the_dark_wake_is_byte_identical_to_its_base`).
//!
//! Additive light cannot show on a light ground, so a light theme draws the
//! wake as ink: each run one thin stroke under the letters in the row's
//! lower band, its ink solved against the theme's text
//! (`cursor_glow::LightGround::body_ink`), a jump one straight stroke, the
//! crown one round ink mark under the caret's row (the box crown's window,
//! fade, typing heat and reach, in the shape the mono face's crown has), and
//! the landing ring the shared veil ring.

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
    /// observed inter-key cadence during sustained typing, the full comet
    /// `duration` for a jump.
    life: f32,
    /// Typing spark (else jump): picks the fade curve in `emit_comet`.
    typing: bool,
    born: Instant,
}

/// One expanding landing ring, kept on its landing cell (not in pixels) so it
/// rides a scroll with the text and is centred from the live geometry.
#[derive(Clone, Copy)]
struct Ring {
    row: u16,
    col: u16,
    born: Instant,
    life: f32,
}

/// The classic wake's animation state, self-contained and touched by no other
/// style.
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
    /// typing step, [`Self::CROWN_MS`] for a jump.
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
    /// Resident light-ground stroke scratch ([`Self::emit_light_comet`]), so
    /// a light frame allocates nothing after the first growth.
    light_verts: Vec<BeamVertex>,
    /// The move this frame's tick spawned on, `(from, to)`, for the host's
    /// diagnosis ring ([`Self::take_spawn`]). `None` on a frame that spawned
    /// nothing.
    spawned: Option<((u16, u16), (u16, u16))>,
    /// The mono face's dark bed's working buffers ([`lay_dark_bed`]).
    bed_scratch: BedScratch,
    /// The mono face's tracer runs over air ([`split_air_runs`]), pooled as
    /// `comet_runs` is.
    air_runs: Vec<Vec<CometSample>>,
    /// Test-only: draw the mono face on a dark ground as the spectrum face
    /// draws, additive light over the letters, the negative control of its
    /// bed test.
    #[cfg(test)]
    pub(crate) mono_unbedded: bool,
    /// Test-only: lay the light-ground stroke and crown in their unsolved
    /// vivid ink (`LightGround::unsolved`), the legibility tests' negative
    /// control.
    #[cfg(test)]
    pub(crate) light_unsolved: bool,
}

/// What the mono face's dark bed reads of the frame
/// ([`ClassicWake::tick`]): the probed text, which says where the tracer
/// may be light, and the over-ink account its bed, crown and ring spend.
pub(crate) struct WakeInk<'a> {
    /// What the frame shows of text: the probe's lines and the frame's own
    /// cells for the rows it missed.
    pub(crate) text: TextView<'a>,
    /// The frame's over-ink account (idle when the host keeps none).
    pub(crate) ledger: &'a mut InkLedger,
}

/// The comet colour at path position `pos` (0 tail .. 1 head), in the face
/// `mono` names: the one colour rule of the classic wake, shared with
/// [`crate::cursor_glow::style_comet_color`]'s `Classic` arm.
///
/// Two faces: the default is a rolling spectrum (`hue` + half a turn along the
/// comet) independent of the theme; `classic mono` is a two-tone tracer fading
/// `accent → color`, following the theme's cursor colour,
/// `cursor_trail_color` and live OSC 12.
#[must_use]
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

    /// Whether the engine remembers a cursor cell, from which its next tick
    /// would spawn a comet on a move.
    #[must_use]
    pub fn has_last(&self) -> bool {
        self.last.is_some()
    }

    /// Live swept-path samples, for `trail status`'s `sparks=` count.
    #[must_use]
    pub fn live_sparks(&self) -> usize {
        self.sparks.len()
    }

    /// The live sparks' cells, for the host-seam tests.
    #[cfg(test)]
    pub(crate) fn spark_cells(&self) -> Vec<(u16, u16)> {
        self.sparks.iter().map(|s| (s.row, s.col)).collect()
    }

    /// The rows the live sparks span, `(first, last)`, for the host's read
    /// of the frame's text ([`CursorGlow::observe_frame_text`]).
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

    /// Drop all in-flight light and forget the last cursor position, used when
    /// the cursor's coordinate space changes or the classic engine stops
    /// owning the frame (another style, the master switch), so no comet spawns
    /// from a stale position and no orphaned light keeps the host awake.
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

    /// Carry every row-addressed member under `map` (an absolute row to its
    /// new row, `None` once carried off the grid or past a band's edge): the
    /// sparks, the ring's landing cell and the last observed cell ride the
    /// content, and whatever leaves is dropped, never clamped. A `last` carried
    /// off is forgotten, so the next sighting only re-observes. Called from
    /// `CursorGlow`'s shared scroll and band-move translation.
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

    /// Advance one frame: observe the cursor at `cur`, spawn on a move, decay,
    /// and emit the light into `out` (additive quads, dark ground) and `halos`
    /// (source-over veils, light ground). Returns a fingerprint that changes on
    /// every visible change in either stream (0 when both are empty).
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
        // `CursorGlow::tick` has already cleared every stream before the
        // classic branch; clearing here keeps the engine correct when driven
        // directly (its tests, a bench, an embedder).
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

        // Cool the typing heat lazily (correct across arbitrary idle gaps, and
        // free while the animator is disarmed).
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
            // A single-cell step is TYPING, and so is a same-row step of two
            // columns: one wide glyph (CJK, emoji, a Hangul syllable) typed or
            // erased. Anything farther is a real JUMP. Judged once here and
            // handed to the spawn, so the crown window, the spark life and
            // the landing ring cannot disagree.
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

        // ---- emit the light layers ----
        if cfg.dark_theme && cfg.classic_mono && !self.mono_unbedded() {
            // The mono face: the tracer's bed under the letters, its light
            // over air, and the crown and ring through the over-ink account.
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
            // The light law: the crown as ink beneath the letters, the comet's
            // stroke over it (so the underline keeps its colours through the
            // crown), the ring as a veil, and nothing additive.
            self.emit_light_crown(now, cfg, geom, cur, under);
            self.emit_light_comet(now, cfg, geom, cur, under);
            under.truncate(Self::MAX_QUADS);
            self.emit_light_ring(now, cfg, geom, halos);
            halos.truncate(CursorGlow::MAX_HALOS);
        }

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
        // …and the light ground's ink and veils and the mono face's dark bed,
        // so a frame where only they change still redraws. The spectrum
        // face's dark frame has neither, so its print is the original's.
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

    // ----- spawning -----

    fn spawn(
        &mut self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        typing: bool,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) {
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

        // Landing ring on a real jump, if enabled.
        if cfg.ring && !typing {
            self.ring = Some(Ring {
                row: cr,
                col: cc,
                born: now,
                life: 0.18,
            });
        }
    }

    // ----- emitting -----

    /// A spark's coverage now: typing sparks hold full brightness for 55% of
    /// life then cosine-fade, so the chained streak stays lit across the
    /// inter-key gap; jump sparks fade linearly.
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
    /// resident pool (`comet_runs[..live]`), the head run attached to the live
    /// cursor cell, and return `live` (0 when nothing is lit). The one path
    /// both grounds draw.
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
        // A bar caret stands at `head_dx` of its cell, not its centre, so
        // every sample sits there too and the head ends on the bar (exactly
        // 0 for a block).
        let dx = (cfg.head_dx - 0.5) * geom.cw as f32;
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
            let cov = Self::spark_cov(s, now, cfg);
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
                x: x + dx,
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
            return 0;
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

    /// The mono face on a dark ground: the runs the dark comet builds, laid
    /// as a bed under the letters in the tracer's own colour
    /// ([`lay_dark_bed`], a thin tracer's shape), and drawn as the comet's
    /// additive light only over air and onto the caret's cell
    /// ([`split_air_runs`]), so no tracer light lands on a letter.
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
        for run in &self.comet_runs[..live] {
            for s in run {
                let rgb = self.comet_color(cfg, s.pos);
                scratch.inks.push(crate::cursor_glow::dark_bed_ink(
                    rgb, page, budget, shape.sat,
                ));
                scratch
                    .rails
                    .push(crate::rainbow_kitty::ribbon::rail_ink(rgb & 0x00FF_FFFF));
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
        // `build_runs` ends the head run on the caret's cell whenever the
        // caret is on the grid.
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
            // The head's bridge over the caret's own cell is the caret's
            // light: kept inside that cell.
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
    /// cursor colour, thinning as each cell fades, full at a head cell's
    /// birth coverage, over the tracer's bright core as a thin rail in the
    /// leading under the row, so the face stays a thin bright tracer.
    const MONO_BED: BedShape = BedShape {
        reach: 0.10,
        thin: 0.45,
        sat: 0.90,
        full: 215.0,
        rail: 0.12,
        ..BedShape::BASE
    };

    /// Whether the mono face draws as the spectrum face does on a dark
    /// ground: never in a build, only for a test's control.
    #[cfg(test)]
    fn mono_unbedded(&self) -> bool {
        self.mono_unbedded
    }
    #[cfg(not(test))]
    #[inline(always)]
    fn mono_unbedded(&self) -> bool {
        false
    }

    /// Where the light-ground stroke runs, as a share of the cell height
    /// below the row's centre: the row's lower band, under the letters'
    /// x-height and clear of the row below.
    const LIGHT_STROKE_DROP: f32 = 0.30;
    /// The light-ground stroke's thickness, as a share of the cell height.
    const LIGHT_STROKE_THICK: f32 = 0.12;
    /// A sample's coverage to the stroke's opacity: a fresh comet reaches the
    /// body cap ([`LIGHT_BODY_ALPHA_CAP`]); a fading one thins with it.
    const LIGHT_STROKE_GAIN: f32 = 1.9;

    /// The light-ground comet: the same runs the dark comet draws, each as
    /// one thin source-over stroke beneath the letters ([`comet_beam_over`]
    /// into the under-ink stream), shifted down to the row's lower band and
    /// inked in the light body's ink of the comet's colour, solved against
    /// the theme's text ([`LightGround::body_ink`]). A typed
    /// run reads as an underline in the comet's colours inside its own row;
    /// a jump across rows is one straight stroke along the move, its samples
    /// laid on the line from the run's tail to its head so the Bresenham
    /// cells leave no staircase and every row it crosses is joined.
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
        for run in &self.comet_runs[..live] {
            if under.len() >= Self::MAX_QUADS {
                break;
            }
            verts.clear();
            let ink = |s: &CometSample| BeamVertex {
                x: s.x,
                y: s.y + drop,
                color: ground.body_ink(self.comet_color(cfg, s.pos)),
                cov: (f32::from(s.cov) * Self::LIGHT_STROKE_GAIN).min(LIGHT_BODY_ALPHA_CAP) as u8,
            };
            match run.as_slice() {
                [] => continue,
                [lone] => {
                    // A lone swept cell: a short dash across its cell.
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
                        // A jump across rows: on the straight move.
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
    /// its window), `None` when there is no crown: the Glow size knob at 0,
    /// no caret on the grid, or the last move's window over. One window for
    /// every face and ground.
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

    /// The mono face's crown on a dark ground: one smooth radial bloom on the
    /// caret's cell in the tracer's colour, the shape every modern style's
    /// crown has, reaching as far as the box crown did (the radius knob).
    /// The spectrum face's stack of concentric rectangles
    /// ([`Self::emit_crown`], its signature) read around a thin mono tracer as
    /// a stepped square box, and spilled square onto the lines above and
    /// below. Same window, fade and typing heat as the box crown, at the
    /// modern crown's peak.
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
        // As far past the cell as the box crown reached (its radius knob),
        // flattened a little toward the row so it keeps off the lines above
        // and below.
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
    /// (`CursorGlow::emit_crown`, 50 × 1.9).
    const MONO_CROWN_COV: f32 = 95.0;

    /// The light crown's peak opacity at full heat and intensity: the box
    /// crown's stacked centre (50 × (1 + 2 + 3) / 3 = 100) at the light
    /// stroke's gain ([`Self::LIGHT_STROKE_GAIN`]), capped at the body cap.
    const LIGHT_CROWN_COV: f32 = 100.0 * Self::LIGHT_STROKE_GAIN;

    /// The crown on a light ground: the box crown's bloom as ink. One round
    /// mark beneath the letters on the caret's cell ([`push_ink_ellipse`],
    /// held inside the caret's row so the lines above and below stay paper),
    /// as far past the cell as the box crown reaches (the Glow size knob),
    /// with its window, fade and typing heat, in the box crown's colour (the
    /// rolling spectrum, or the one-colour face's colour) as the light body's
    /// ink, solved against the theme's text ([`LightGround::body_ink`]).
    /// Additive light cannot show on paper, and without its crown the wake
    /// at a relaxed typing pace is a dash a pixel or two deep under the
    /// letter just typed: on a dark ground the crown is what a typist sees.
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
    /// it has faded out.
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
        // Expanding square outline: half-size grows ~0.6 → ~1.6 cells, emitted
        // as top/bottom and left/right bars (push_rect splits the verticals into
        // per-row slabs). The stroke is an eighth of the cell height, never
        // under the original's 2 px, so a HiDPI cell keeps its weight.
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
    /// expanding source-over veil of the ring colour mixed 28% toward black,
    /// its centre alpha fading with the ring and capped at 120), as
    /// `CursorGlow::emit_ring` draws it for the other styles.
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
        // Falling off from its centre, never a flat disc of ink (a stain).
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
    /// config's face ([`comet_color`]).
    fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        comet_color(cfg.classic_mono, cfg.color, cfg.accent, self.hue, pos)
    }
}

#[inline]
fn chf_of(geom: Geom) -> f32 {
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
    /// with no probed text and no over-ink account (an idle one).
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
            assert_eq!(
                q.alpha, 0,
                "on a dark ground the classic wake is ADDITIVE light only"
            );
        }
    }

    /// The run builder is resident: after warm-up the pooled slots keep their
    /// capacity at the top of every frame. Asserting capacity rather than
    /// allocation count needs no global allocator in the lib test binary.
    #[test]
    fn the_comet_run_pool_keeps_its_buffers_between_frames() {
        let (g, c) = (geom(), cfg());
        let mut rig = Rig::default();
        let t0 = Instant::now();
        // A typing run across a row split by a jump, so more than one run is
        // live: the builder's worst case.
        let frame = |rig: &mut Rig, k: u64| {
            // The fixture grid is 6 rows x 40 cols; stay inside it or nothing
            // spawns at all.
            let col = u16::try_from((k % 30) + 2).expect("column fits");
            // Alternate rows so the comet breaks into adjacent runs.
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
        // Stated on `len()`, not capacity alone: a pool never shrinks, so its
        // slot count is the high-water run count, while a clear-and-rebuild
        // shrinks it on a frame with fewer runs.
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
    /// classic branch went through the admission seam.
    #[test]
    fn the_classic_wake_spawns_on_motion_alone_with_no_license() {
        let (g, c) = (geom(), cfg());
        let mut rig = Rig::default();
        let t0 = Instant::now();
        rig.tick(Some((2, 4)), t0, &c, g);
        // No `note_typed`, no hint, no license of any kind — just a move.
        let fp = rig.tick(Some((4, 34)), ms(t0, 16), &c, g);
        assert!(!rig.out.is_empty(), "a licence-free jump must still light");
        assert_ne!(fp, 0, "a lit frame fingerprints non-zero");
        assert_banded(&rig.out, g);
    }

    /// One wide glyph (CJK, emoji, a Hangul syllable) advances the caret two
    /// columns; typed at speed it earns heat and the typing crown, and no
    /// landing ring. A one-row vertical step stays typing too, and a
    /// three-column step is still a jump (the control).
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
        // Backspace over a wide glyph is the same step leftward.
        rig.tick(Some((2, 6)), ms(t0, 180), &c, g);
        assert!(rig.w.ring.is_none(), "erasing a wide glyph lays no ring");
        // A one-row vertical step (arrow down, `j`) stays typing.
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
    /// there, so a vertical jump's streak runs down the bar, not half a cell
    /// to its right. The block caret (0.5) is the same law at the cell's
    /// centre.
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
    /// of the cell height, never under the original's 2 px.
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
    /// park and the animation timer disarm.
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
    /// no residue left to re-light the next frame.
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
    /// Asserted over a typing advance: the landing ring is `cfg.accent` in both.
    #[test]
    fn the_mono_face_follows_the_configured_colour_and_the_spectrum_does_not() {
        let g = geom();
        // A typing advance: comet + crown, and deliberately no landing ring.
        // On a dark ground the mono face's tracer is its bed under the
        // letters, so its colours are in both streams.
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
        // On a light ground both faces are the same ink geometry: only the
        // colours moved.
        assert_eq!(
            typed_on(light_cfg(), true, GREEN.0, GREEN.1).len(),
            typed_on(light_cfg(), false, GREEN.0, GREEN.1).len(),
            "the two faces differ in colour alone on a light ground"
        );
    }

    /// The shared colour ramp's `Classic` arm is the engine's own spectrum
    /// rule, so the two cannot drift.
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
    /// fingerprint, quad count and every quad's damage words. The script stays
    /// inside what the light, wide-glyph, bar-caret and scroll adaptations
    /// leave alone (a block caret, no 2-column same-row step, no scroll, cells
    /// under 24 px), so it pins the dark look itself.
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
            // The mono face's round crown (the spectrum face lays no halo,
            // so its prints fold nothing here).
            for h in &rig.halos {
                for word in h.damage_words() {
                    acc = fnv_mix_u64(acc, word);
                }
            }
        };
        let mut at = 0;
        frame(&mut rig, Some((2, 4)), at);
        // Fast typing builds heat; the chained wake and hot crown follow.
        for k in 1..=8u16 {
            at += 60;
            frame(&mut rig, Some((2, 4 + k)), at);
            frame(&mut rig, Some((2, 4 + k)), at + 24);
        }
        // A lone key after the heat cools, then an erase.
        at += 500;
        frame(&mut rig, Some((2, 13)), at);
        at += 80;
        frame(&mut rig, Some((2, 12)), at);
        // A one-row vertical step is typing too.
        at += 100;
        frame(&mut rig, Some((3, 12)), at);
        // A diagonal jump with its ring, animated through its decay.
        at += 150;
        frame(&mut rig, Some((5, 30)), at);
        for dt in [16u64, 90, 200] {
            frame(&mut rig, Some((5, 30)), at + dt);
        }
        // A long leftward jump and an upward jump to the top row.
        at += 400;
        frame(&mut rig, Some((5, 3)), at);
        frame(&mut rig, Some((5, 3)), at + 40);
        at += 300;
        frame(&mut rig, Some((0, 20)), at);
        frame(&mut rig, Some((0, 20)), at + 70);
        // Hidden, then seen again: the gap only re-observes.
        at += 100;
        frame(&mut rig, None, at);
        at += 30;
        frame(&mut rig, Some((0, 21)), at);
        frame(&mut rig, Some((0, 21)), at + 4000);
        acc
    }

    /// The mono face's crown on a dark ground is one smooth round bloom: in a
    /// hot typed run its light is radial halos, and below the caret's cell,
    /// down the column through its middle, it falls off to nothing a little
    /// at a time. Negative control: the spectrum face's box crown (its
    /// signature, held byte for byte) steps down by whole layers there.
    #[test]
    fn the_mono_crown_is_one_smooth_round_bloom() {
        let g = geom();
        let mono = GlowConfig {
            classic_mono: true,
            ..cfg()
        };
        // The additive light down the caret's column, from its cell's
        // bottom edge to two cells below, after a hot run.
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
    /// the light, wide-glyph, bar-caret and scroll adaptations were made on
    /// (the v0.28 A/B harness that once proved it against the release is
    /// gone): the spectrum face on the identity layout and on a padded window
    /// with a head band. The mono face's print is its own since its tracer
    /// lies under the letters on a dark ground (its bed, recaptured on
    /// purpose), since its crown is one round bloom (recaptured on purpose
    /// again) and since a leap's bed is one soft straight tube, not a stair
    /// of slabs (recaptured on purpose a third time); the spectrum face's two
    /// prints held through all three. A
    /// deliberate change to the dark look recaptures these on purpose; any
    /// other drift is the bug.
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
        // The fold sees a sub-cell move: the same session on a bar caret
        // (one of the adaptations) does not fold to the block's print.
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
    /// (aterm-gui's `cursor_trail_*_or_default` and `cursor_trail_intensity_for`:
    /// 260 ms, 24 cells and a 0.6-cell crown with the ring on, at the v0.28
    /// release's own brightness, [`crate::cursor_fx::default_trail_intensity`];
    /// aterm-gui's `classic_receives_the_shipped_knobs` pins that side),
    /// resolved for the dark default theme through the one config law every
    /// host runs ([`crate::cursor_fx::resolve_glow_config`], the knob law
    /// included), `length` and `intensity` aside.
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
    /// config law resolves from the shipped knobs (not only the fixture the
    /// base was pinned on) reaches the engine whole, so its dark session
    /// folds to its own print, on the identity layout and the padded one.
    /// The knob law folds nothing Classic reads: the length, the crown's
    /// reach, the ring and the duration arrive as set, and an unset
    /// brightness is the v0.28 release's 0.7, the fixture's own (at full
    /// brightness the tracer's core left the newest letter at 3:1).
    /// Negative controls: the same knobs with the length the fixture uses (a
    /// law that resized Classic's comet), or at the full brightness every
    /// other style defaults to, fold to other prints.
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
    /// crown of light), the comet and the crown as source-over ink in the
    /// under-ink stream under the body cap, and the landing ring as a capped
    /// source-over veil on the landing cell.
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
    /// renderer's source-over law), `times` over, then `INK` stems: two 1 px
    /// stems per cell from the ascender line to the descender line on `row`
    /// over `cols`. Returns the frame and, per cell and pixel row, the stems
    /// and the counters beside them, so a stroke a few pixels tall is judged
    /// where it lies.
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
    /// stroke pixel lies in the typed row (the rows above and below stay
    /// paper), the stroke lies in the row's lower band (measured with the
    /// crown off, Glow size 0), and every stem of the letters typed over the
    /// whole wake, the stroke and the ink crown on the caret's row, keeps
    /// 4.5:1 against its counters on GitHub Light, across a session that
    /// rolls the spectrum, even laid three deep (a run that compounded its
    /// revisits): the inks are solved against the theme's text, so a stack
    /// of them composites toward the inks. Negative control: the same session
    /// in the unsolved vivid ink drops a letter under the bar.
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
    /// typed at speed, the wake lays the crown first and then exactly the
    /// stroke a crownless twin (Glow size 0) lays, so the crown is the
    /// difference; it is source-over ink under the body cap, inside the
    /// caret's row, reaching past the caret's cell on both sides by the same
    /// amount, deepest at the caret and fading toward its rim. It follows
    /// typing heat (a lone key after a pause inks it fainter than the hot
    /// run) and ends with its window. Negative control: the twin lays none.
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
        // A lone key after a pause long enough to cool the run.
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
    /// leaps from the bottom row to the top, and the comet's ink covers every
    /// pixel row between the two ends along the move, with no per-row gap.
    /// Negative control: the per-cell rails the classic wake drew before (a
    /// veil along each swept cell's top and bottom edge) leave the same
    /// column with gaps between rows.
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
        // Each cell's rails: a veil along its row's top and bottom edges,
        // 0.22 of the cell deep, clipped to the cell.
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
    /// a full screen, an Enter scrolls the command up and lands the caret at
    /// the new prompt's start. Carried with the text, the old caret cell is
    /// now two rows up, so the comet is the true diagonal and lays nothing
    /// along the new prompt row past the diagonal's own last cells. The twin
    /// is the same move expressed as a band move; the control is the same
    /// move with the scroll untold, which draws the false horizontal comet.
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
    /// clamped onto row 0, and a forgotten `last` spawns nothing. (No crown,
    /// which follows the live caret and is no row-addressed state.)
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

    /// A coordinate-space change (a split-pane transition) must not let the
    /// next tick draw a comet from a position in the space that just died.
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
